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

use super::RetryAfter;
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
            }
            EndpointVariant::Blallama => {
                quirks.breakpoint_after_assistant = true;
                quirks.output_config_cache_safe = true;
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

// Forward the `Retry-After` that Anthropic sends on 429/529; everything else is
// fatal. `AnthropicError::retry_after` is the upstream accessor that turns the
// header's seconds into a `Duration`.
impl RetryAfter for misanthropic::client::Error {
    fn retry_after(&self) -> Option<Duration> {
        match self {
            misanthropic::client::Error::Anthropic(e) => e.retry_after(),
            _ => None,
        }
    }
}

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

            let mut pending = self.client.tagged_batch(items).await?;
            let ready = loop {
                match self.client.batch_poll(pending).await? {
                    batch::Batch::Ready(ready) => break ready,
                    batch::Batch::Pending(p) => {
                        pending = p;
                        tokio::time::sleep(self.poll_period).await;
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
