//! The concrete [`Inference`] transport: one [`Client`] wrapping a
//! [`misanthropic::Client`].
//!
//! [`infer`](Inference::infer) is one `Client::message`;
//! [`infer_batch`](Inference::infer_batch) packs the cohort into chunked
//! Anthropic [Batch API] submissions, each polled to completion. Construction is
//! inherent — the orchestrator builds it and hands it to a
//! [`Reactor`](super::Reactor).
//!
//! [Batch API]: misanthropic::Client::batch

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use misanthropic::model::{ModelInfo, Models};
use misanthropic::{batch, response};
use serde::{Deserialize, Serialize};

use crate::retry::client_error_recoverable;

use super::backend::Inference;
use super::inference::Quirks;

/// Default [`Client`] batch chunk size — larger cohorts are split across
/// submissions.
const DEFAULT_MAX_BATCH: usize = 1000;
/// Default [`Client`] period between batch polls.
const DEFAULT_POLL_PERIOD: Duration = Duration::from_secs(5);
/// A key of valid length that stands in for the real one on local variants,
/// so the real key can never leak to a localhost/LAN endpoint in the clear.
const DUMMY_KEY: &str = "sk-ant-api03-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";

/// Which `/v1/messages` implementation the [`Client`] points at. Converts to
/// the data-only [`Quirks`] that crosses to agents — behavioral lore stays
/// here, behind the `client` gate.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EndpointVariant {
    /// The real Anthropic API.
    #[default]
    Anthropic,
    /// ollama's Anthropic-compat layer.
    Ollama,
    /// The `drama_llama` server: Anthropic-conformant, deviations are bugs —
    /// except improvements.
    Blallama,
}

impl From<EndpointVariant> for Quirks {
    fn from(variant: EndpointVariant) -> Self {
        let mut quirks = Quirks::default();
        match variant {
            EndpointVariant::Anthropic => {}
            EndpointVariant::Ollama => {
                quirks.cache_markers_ignored = true;
                quirks.tool_choice_not_respected = true;
                quirks.cache_stats_unreported = true;
                quirks.web_search_unsupported = true;
                quirks.web_fetch_unsupported = true;
            }
            EndpointVariant::Blallama => {
                quirks.breakpoint_after_assistant = true;
                quirks.output_config_cache_safe = true;
                // No server-side tool runner yet. Anthropic-conformant
                // deviations are bugs — except improvements — so expect these
                // to flip to `false` one tool at a time rather than together.
                quirks.web_search_unsupported = true;
                quirks.web_fetch_unsupported = true;
            }
        }
        quirks
    }
}

/// An ollama/blallama `GET /api/tags` body — the subset [`models`]
/// synthesizes from.
///
/// [`models`]: Inference::models
#[derive(Deserialize)]
struct Tags {
    #[serde(default)]
    models: Vec<Tag>,
}

#[derive(Deserialize)]
struct Tag {
    name: String,
    #[serde(default)]
    modified_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Synthesize [`Models`] from a `/api/tags` body: custom ids, no
/// [`Capabilities`] (notably batch = false), unreported token ceilings.
///
/// [`Capabilities`]: misanthropic::model::Capabilities
fn models_from_tags(body: &str) -> Result<Models, misanthropic::client::Error> {
    let tags: Tags = serde_json::from_str(body)?;
    Ok(tags
        .models
        .into_iter()
        .map(|tag| ModelInfo {
            id: tag.name.clone().into(),
            display_name: tag.name.into(),
            capabilities: Default::default(),
            max_input_tokens: 0,
            max_tokens: 0,
            kind: Default::default(),
            created_at: tag.modified_at.unwrap_or_default(),
        })
        .collect())
}

/// Base wait for a header-less 529. The real API emits them (seen live
/// 2026-06-11) and blallama's "Session is busy" never carries the header;
/// without a courtesy backoff both read as fatal. Callers scale by attempt.
/// The Anthropic [`Inference`] transport: a thin wrapper over a
/// [`misanthropic::Client`]. [`infer`](Inference::infer) is one
/// `Client::message`; [`infer_batch`](Inference::infer_batch) uses the Batch API.
pub struct Client {
    client: misanthropic::Client,
    variant: EndpointVariant,
    concurrency: NonZeroUsize,
    /// Maximum prompts per batch submission; larger cohorts are chunked.
    max_batch: usize,
    /// How long to wait between `batch_poll`s.
    poll_period: Duration,
}

impl Client {
    /// Wrap a [`misanthropic::Client`]. Variant defaults to Anthropic;
    /// concurrency to 1; batches chunk at [`DEFAULT_MAX_BATCH`] and poll every
    /// [`DEFAULT_POLL_PERIOD`]. See [`with_variant`](Self::with_variant) /
    /// [`with_concurrency`](Self::with_concurrency) /
    /// [`with_batch`](Self::with_batch) to tune.
    pub fn new(client: misanthropic::Client) -> Self {
        Self {
            client,
            variant: EndpointVariant::default(),
            concurrency: 1.try_into().unwrap(),
            max_batch: DEFAULT_MAX_BATCH,
            poll_period: DEFAULT_POLL_PERIOD,
        }
    }

    /// Set the [`EndpointVariant`]. For non-Anthropic variants this also
    /// replaces the inner client's API key with [`DUMMY_KEY`] — misanthropic
    /// attaches the key to every request, and a real key must never reach a
    /// localhost/LAN endpoint in the clear.
    pub fn with_variant(mut self, variant: EndpointVariant) -> Self {
        self.variant = variant;
        if !matches!(variant, EndpointVariant::Anthropic) {
            self.client.key = Arc::new(
                DUMMY_KEY
                    .to_string()
                    .try_into()
                    .expect("DUMMY_KEY has a valid key length"),
            );
        }
        self
    }

    /// Change the concurrency limit. Beware rate limits.
    pub fn with_concurrency(mut self, n: NonZeroUsize) -> Self {
        self.set_concurrency(n);
        self
    }

    /// Set the concurrency limit. Beware rate limits.
    pub fn set_concurrency(&mut self, n: NonZeroUsize) {
        self.concurrency = n;
    }

    /// Change the batch chunk size and poll period.
    pub fn with_batch(
        mut self,
        max_batch: usize,
        poll_period: Duration,
    ) -> Self {
        self.max_batch = max_batch;
        self.poll_period = poll_period;
        self
    }
}

/// How many times a single batch submit or poll is retried before the chunk
/// is failed. Bounded on purpose: a batch that cannot be submitted after
/// this many attempts is not a blip, and the cohort should fail visibly
/// rather than spin.
const MAX_BATCH_RETRIES: usize = 5;

/// Exponential backoff for batch submit/poll: 1s, 2s, 4s, 8s, 16s, capped
/// at 30s. Total worst-case wait across [`MAX_BATCH_RETRIES`] is ~31s on
/// top of call latency.
async fn backoff(attempt: usize) {
    let secs = 1u64 << attempt.min(5);
    tokio::time::sleep(Duration::from_secs(secs.min(30))).await;
}

#[async_trait::async_trait]
impl Inference for Client {
    type Error = misanthropic::client::Error;

    async fn infer<P>(
        &self,
        prompt: P,
    ) -> Result<response::Message, Self::Error>
    where
        P: Serialize + Send,
    {
        self.client.message(prompt).await
    }

    async fn infer_batch<P>(
        &self,
        prompts: &[&P],
    ) -> Result<Vec<Result<response::Message, Self::Error>>, Self::Error>
    where
        P: Serialize + Send + Sync,
    {
        let chunk = self.max_batch.max(1);
        // Results land here, indexed by the prompt's position in `prompts`.
        let mut out: Vec<Option<Result<response::Message, Self::Error>>> =
            (0..prompts.len()).map(|_| None).collect();

        for start in (0..prompts.len()).step_by(chunk) {
            let end = (start + chunk).min(prompts.len());

            // Tag each prompt with a fresh batch id and remember which position
            // it maps back to. `P = &Prompt` — results route by id, so we never
            // need the prompts back and never clone them.
            let mut id_to_idx: HashMap<batch::Id, usize> = HashMap::new();
            let items: Vec<(batch::Id, &P)> = (start..end)
                .map(|i| {
                    let id = batch::Id::default();
                    id_to_idx.insert(id, i);
                    (id, prompts[i])
                })
                .collect();

            // Submit with bounded backoff. A transient edge failure here
            // (gateway 503, reset connection) would otherwise drop the whole
            // chunk — and because one submission carries the entire cohort,
            // that means every agent in it fails at once. `items` is cheap to
            // rebuild: `P = &Prompt`, and the ids were minted above, so a
            // retried submission routes its results identically.
            let mut pending = {
                let mut attempt = 0usize;
                loop {
                    match self.client.tagged_batch(items.clone()).await {
                        Ok(pending) => break pending,
                        Err(e) => {
                            if attempt >= MAX_BATCH_RETRIES
                                || !client_error_recoverable(&e)
                            {
                                return Err(e);
                            }
                            tracing::warn!(
                                attempt = attempt + 1,
                                max = MAX_BATCH_RETRIES,
                                error = %e,
                                "batch submit failed, retrying"
                            );
                            backoff(attempt).await;
                            attempt += 1;
                        }
                    }
                }
            };

            let ready = {
                let mut attempt = 0usize;
                loop {
                    match self.client.batch_poll(pending).await {
                        Ok(batch::Batch::Ready(ready)) => break ready,
                        Ok(batch::Batch::Pending(p)) => {
                            pending = p;
                            // Progress: the batch is alive and answering, so
                            // the failure budget starts over. Otherwise a long
                            // batch with occasional blips would exhaust it.
                            attempt = 0;
                            tokio::time::sleep(self.poll_period).await;
                        }
                        // `batch::Error` hands the batch back rather than
                        // consuming it, so a failed poll is survivable: the
                        // work is already submitted and already billed.
                        Err(batch::Error {
                            client_error,
                            pending: p,
                        }) => {
                            if attempt >= MAX_BATCH_RETRIES
                                || !client_error_recoverable(&client_error)
                            {
                                return Err(client_error);
                            }
                            tracing::warn!(
                                attempt = attempt + 1,
                                max = MAX_BATCH_RETRIES,
                                batch_id = %p.meta().id,
                                error = %client_error,
                                "batch poll failed, retrying"
                            );
                            pending = p;
                            backoff(attempt).await;
                            attempt += 1;
                        }
                    }
                }
            };

            let (_, results) = ready.decompose();
            for (id, result) in results {
                if let Some(&idx) = id_to_idx.get(&id) {
                    // `BatchResult -> Result<Message, AnthropicError>`, then
                    // `AnthropicError -> misanthropic::client::Error`.
                    let r: Result<
                        response::Message,
                        misanthropic::client::AnthropicError,
                    > = result.into();
                    out[idx] = Some(r.map_err(Into::into));
                }
            }
        }

        // A slot still empty means the provider returned no result for that id;
        // surface it as an error so the agent re-batches next round.
        Ok(out
            .into_iter()
            .map(|slot| {
                slot.unwrap_or(Err(
                    misanthropic::client::Error::UnexpectedResponse {
                        message: "batch returned no result for prompt",
                    },
                ))
            })
            .collect())
    }

    async fn models(&self) -> Result<misanthropic::model::Models, Self::Error> {
        match self.variant {
            EndpointVariant::Anthropic => self.client.models().await,
            // ollama/blallama don't serve /v1/models; discover via /api/tags.
            // Deliberately through the bare `inner` and not a keyed helper
            // like `get_raw`: no API key may reach a local endpoint.
            EndpointVariant::Ollama | EndpointVariant::Blallama => {
                let url = self.client.messages_url.join("/api/tags").map_err(
                    |_| misanthropic::client::Error::UnexpectedResponse {
                        message: "cannot derive /api/tags from messages_url",
                    },
                )?;
                let body = self
                    .client
                    .inner
                    .get(url)
                    .send()
                    .await?
                    .error_for_status()?
                    .text()
                    .await?;
                models_from_tags(&body)
            }
        }
    }

    fn quirks(&self) -> Quirks {
        self.variant.into()
    }

    fn max_concurrency(&self) -> NonZeroUsize {
        self.concurrency
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // The classification impls live in `reactor` (gated on
    // `misanthropic`, not `client`); these tests exercise them here
    // because they are Anthropic-transport behaviour.
    use super::super::{COURTESY_BACKOFF, RetryAfter};

    /// The variant → quirks lore: Anthropic is the all-`false` default;
    /// ollama and blallama each deviate exactly where documented.
    #[test]
    fn variant_quirks_mapping() {
        assert_eq!(Quirks::from(EndpointVariant::Anthropic), Quirks::default());

        let ollama = Quirks::from(EndpointVariant::Ollama);
        assert!(ollama.cache_markers_ignored);
        assert!(ollama.tool_choice_not_respected);
        assert!(ollama.cache_stats_unreported);
        assert!(!ollama.breakpoint_after_assistant);
        assert!(!ollama.output_config_cache_safe);

        let blallama = Quirks::from(EndpointVariant::Blallama);
        assert!(blallama.breakpoint_after_assistant);
        assert!(blallama.output_config_cache_safe);
        assert!(!blallama.cache_markers_ignored);
        assert!(!blallama.tool_choice_not_respected);
        assert!(!blallama.cache_stats_unreported);

        // Server tools: Anthropic runs them, the local endpoints don't.
        assert!(!Quirks::default().web_search_unsupported);
        assert!(!Quirks::default().web_fetch_unsupported);
        assert!(ollama.web_search_unsupported);
        assert!(ollama.web_fetch_unsupported);
        assert!(blallama.web_search_unsupported);
        assert!(blallama.web_fetch_unsupported);
    }

    /// The retry classification. A server-sent hint always wins; the
    /// transient *classes* get the courtesy backoff even with no header,
    /// because `retry_after` is the reactor's only retry mechanism — an
    /// error left fatal here gets no reschedule and, on the batch path,
    /// no re-batch.
    #[test]
    fn server_hints_win_over_the_courtesy_backoff() {
        use misanthropic::client::{AnthropicError, Error};

        let e = Error::Anthropic(AnthropicError::Overloaded {
            message: "overloaded".into(),
            retry_after: Some(3),
        });
        assert_eq!(
            e.retry_after(),
            Some(Duration::from_secs(3)),
            "an explicit Retry-After must not be replaced by the default"
        );

        let e = Error::Anthropic(AnthropicError::Overloaded {
            message: "Session is busy.".into(),
            retry_after: None,
        });
        assert_eq!(e.retry_after(), Some(COURTESY_BACKOFF));
    }

    /// Changed 2026-08-21: these were all fatal, which is why a single
    /// edge 503 failed 27 of 30 agents — `NonJsonResponse` meant no agent
    /// re-batched. Each is a class the server may recover from on its own.
    #[test]
    fn transient_classes_are_retryable_without_a_header() {
        use misanthropic::client::{AnthropicError, Error};
        use std::num::NonZeroU16;

        let cases: Vec<(&str, Error)> = vec![
            (
                "the 2026-08-21 gateway 503",
                Error::NonJsonResponse {
                    status: 503,
                    body: "upstream connect error".into(),
                },
            ),
            (
                "a non-JSON 429 challenge page",
                Error::NonJsonResponse {
                    status: 429,
                    body: "<html>slow down</html>".into(),
                },
            ),
            (
                "header-less 429",
                Error::Anthropic(AnthropicError::RateLimit {
                    message: "slow down".into(),
                    retry_after: None,
                }),
            ),
            (
                "5xx from the API itself",
                Error::Anthropic(AnthropicError::API {
                    message: "boom".into(),
                }),
            ),
            (
                "an unknown 5xx",
                Error::Anthropic(AnthropicError::Unknown {
                    code: Some(NonZeroU16::new(502).unwrap()),
                    message: "bad gateway".into(),
                }),
            ),
        ];

        for (what, e) in cases {
            assert_eq!(
                e.retry_after(),
                Some(COURTESY_BACKOFF),
                "{what} must be retryable"
            );
            assert!(!e.is_fatal(), "{what} must not be fatal");
        }
    }

    /// The other half of the same decision: repeating these verbatim
    /// cannot fix them, so they must stay fatal. A blanket "retry
    /// everything" would spin on a bad key or a malformed request.
    #[test]
    fn caller_side_failures_stay_fatal() {
        use misanthropic::client::{AnthropicError, Error};
        use std::num::NonZeroU16;

        let cases: Vec<(&str, Error)> = vec![
            (
                "a non-JSON 404 — wrong URL, not a blip",
                Error::NonJsonResponse {
                    status: 404,
                    body: "<html>not found</html>".into(),
                },
            ),
            (
                "bad request",
                Error::Anthropic(AnthropicError::InvalidRequest {
                    message: "malformed".into(),
                }),
            ),
            (
                "bad key",
                Error::Anthropic(AnthropicError::Authentication {
                    message: "nope".into(),
                }),
            ),
            (
                "an unknown 4xx",
                Error::Anthropic(AnthropicError::Unknown {
                    code: Some(NonZeroU16::new(418).unwrap()),
                    message: "teapot".into(),
                }),
            ),
            (
                "an unparseable success body",
                Error::UnexpectedResponse {
                    message: "stream where a message was expected",
                },
            ),
        ];

        for (what, e) in cases {
            assert_eq!(e.retry_after(), None, "{what} must stay fatal");
            assert!(e.is_fatal(), "{what} must be fatal");
        }
    }

    /// `retry::client_error_recoverable` delegates to this impl rather
    /// than classifying separately. Before it did, the two disagreed on
    /// five variants — including the 503 above, which one called
    /// retryable and the other fatal.
    #[test]
    fn the_retry_helper_agrees_with_retry_after() {
        use crate::retry::client_error_recoverable;
        use misanthropic::client::{AnthropicError, Error};

        let transient = Error::NonJsonResponse {
            status: 503,
            body: "upstream connect error".into(),
        };
        assert!(client_error_recoverable(&transient));
        assert!(!transient.is_fatal());

        let fatal = Error::Anthropic(AnthropicError::Authentication {
            message: "nope".into(),
        });
        assert!(!client_error_recoverable(&fatal));
        assert!(fatal.is_fatal());
    }

    /// The 2026-08-21 failure, reproduced: a gateway 503 with a non-JSON
    /// body on the batch *submission*. One submission carries the whole
    /// cohort, so before the retry loop this failed every agent in it at
    /// once — 27 of 30, in the same second.
    ///
    /// The mock answers 503 forever, so this asserts the two things that
    /// matter: that the submit is retried at all, and that it is *bounded* —
    /// exactly `MAX_BATCH_RETRIES` retries after the initial attempt, then
    /// the error surfaces. `start_paused` lets tokio fast-forward the
    /// backoff, so the ~31s of sleeps cost no wall-clock.
    #[tokio::test(start_paused = true)]
    async fn batch_submit_retries_a_transient_gateway_503() {
        use httpmock::prelude::*;
        use misanthropic::{Prompt, prompt::message::Role};

        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path("/v1/messages/batches/");
            then.status(503).body(
                "upstream connect error or disconnect/reset before headers. \
                 reset reason: connection termination",
            );
        });

        let transport = Client::new(
            misanthropic::Client::new("x".repeat(108))
                .unwrap()
                .base_url(server.base_url())
                .unwrap(),
        );

        let prompt = Prompt::default()
            .model(misanthropic::Id::Haiku45)
            .max_tokens(std::num::NonZeroU32::new(16).unwrap())
            .add_message((Role::User, "hi"))
            .unwrap();

        let error = transport
            .infer_batch(&[&prompt])
            .await
            .expect_err("a permanent 503 must eventually surface");

        assert!(
            matches!(
                error,
                misanthropic::client::Error::NonJsonResponse {
                    status: 503,
                    ..
                }
            ),
            "expected the edge 503 to surface unchanged, got: {error:?}"
        );
        mock.assert_hits(MAX_BATCH_RETRIES + 1);
    }

    /// A 4xx is the caller's fault and will not fix itself, so it must fail
    /// on the first attempt rather than burn the retry budget.
    #[tokio::test(start_paused = true)]
    async fn batch_submit_does_not_retry_a_400() {
        use httpmock::prelude::*;
        use misanthropic::{Prompt, prompt::message::Role};

        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path("/v1/messages/batches/");
            then.status(400).json_body(serde_json::json!({
                "type": "error",
                "error": { "type": "invalid_request_error", "message": "bad" }
            }));
        });

        let transport = Client::new(
            misanthropic::Client::new("x".repeat(108))
                .unwrap()
                .base_url(server.base_url())
                .unwrap(),
        );

        let prompt = Prompt::default()
            .model(misanthropic::Id::Haiku45)
            .max_tokens(std::num::NonZeroU32::new(16).unwrap())
            .add_message((Role::User, "hi"))
            .unwrap();

        transport
            .infer_batch(&[&prompt])
            .await
            .expect_err("a 400 must surface");

        mock.assert_hits(1);
    }

    /// Live: does the **Batch API** actually run server tools? Everything
    /// else about the web tools is settled by the docs; this one is not, and
    /// the seed cohort rides `infer_batch` exclusively — so a "batches don't
    /// do server tools" answer would invalidate the whole feature, and it
    /// would be found in production.
    ///
    /// Passes if the batch item comes back having searched (a
    /// `web_search_tool_result` block) or mid-search (`pause_turn`). Fails
    /// with the API's own words if the submission is rejected.
    ///
    /// `cargo test --all-features live_batch -- --ignored --nocapture`
    #[tokio::test]
    #[ignore = "hits the live Anthropic API (a search, so cents)"]
    async fn live_batch_runs_server_tools() {
        use misanthropic::prompt::message::Block;
        use misanthropic::tool::{ServerMethodDef, WebSearch};
        use misanthropic::{
            Prompt, prompt::message::Role, response::StopReason,
        };

        let key = std::env::var("ANTHROPIC_API_KEY").unwrap_or_else(|_| {
            let path = format!(
                "{}/Projects/agora/secrets/anthropic_api_key",
                std::env::var("HOME").expect("HOME")
            );
            std::fs::read_to_string(path)
                .expect("no ANTHROPIC_API_KEY and no key file")
                .trim()
                .to_string()
        });
        let transport = Client::new(misanthropic::Client::new(key).unwrap());

        let prompt = Prompt::default()
            .model(misanthropic::Id::Haiku45)
            .max_tokens(std::num::NonZeroU32::new(512).unwrap())
            // Not "search anthropic.com": `allowed_domains` is a server-side
            // filter the model can't see, and naming a domain it can't honor
            // makes it decline. The filter still scopes the results.
            .add_message((
                Role::User,
                "Search and name one product Anthropic makes.",
            ))
            .unwrap()
            .add_tool(ServerMethodDef::web_search(WebSearch {
                max_uses: Some(1),
                allowed_domains: Some(vec!["anthropic.com".into()]),
                ..Default::default()
            }));

        let results = transport
            .infer_batch(&[&prompt])
            .await
            .expect("batch submission accepted");
        let response = results
            .into_iter()
            .next()
            .expect("one prompt, one result")
            .expect("the batch item itself succeeded");

        let searched = response
            .inner
            .content
            .iter()
            .any(|b| matches!(b, Block::WebSearchToolResult { .. }));
        let paused =
            matches!(response.stop_reason, Some(StopReason::PauseTurn));
        println!(
            "batch server-tool run: stop={:?} searched={searched} \
             usage={:?}\n{}",
            response.stop_reason,
            response.usage.server_tool_use,
            response.inner.content
        );
        assert!(
            searched || paused,
            "the batch item ran no server tool: {:?}",
            response.inner.content
        );
    }

    /// `/api/tags` synthesis: custom ids, batch unsupported, ceilings
    /// unreported — and a missing `modified_at` doesn't fail the parse.
    #[test]
    fn models_from_tags_synthesizes() {
        let body = r#"{
            "models": [
                {"name": "llama3.3:70b", "modified_at": "2026-01-01T00:00:00Z"},
                {"name": "qwen3:32b"}
            ]
        }"#;
        let models = models_from_tags(body).unwrap();
        let infos: Vec<&ModelInfo> = models.iter().collect();
        assert_eq!(infos.len(), 2);
        assert_eq!(infos[0].id.name(), "llama3.3:70b");
        assert!(!infos[0].capabilities.batch.supported, "batch never");
        assert_eq!(infos[0].max_tokens, 0, "ceiling unreported");
        assert_eq!(infos[1].id.name(), "qwen3:32b");
    }
}
