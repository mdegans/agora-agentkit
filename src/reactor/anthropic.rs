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
// FIXME(mdegans): this still assumes an Anthropic endpoint. The next step
// (deferred) is an `endpoint_variant` so ollama/blallama — which serve
// /v1/messages but not /v1/models — route `models()` to /chat/tags and
// synthesize a `Models` (batch always false). When doing so, CLEAR the request
// headers first so the API key never reaches a localhost/LAN endpoint in the
// clear.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::time::Duration;

use misanthropic::{batch, response};
use serde::Serialize;

use super::RetryAfter;
use super::backend::Inference;

/// Default [`Client`] batch chunk size — larger cohorts are split across
/// submissions.
const DEFAULT_MAX_BATCH: usize = 1000;
/// Default [`Client`] period between batch polls.
const DEFAULT_POLL_PERIOD: Duration = Duration::from_secs(5);

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
    concurrency: NonZeroUsize,
    /// Maximum prompts per batch submission; larger cohorts are chunked.
    max_batch: usize,
    /// How long to wait between `batch_poll`s.
    poll_period: Duration,
}

impl Client {
    /// Wrap a [`misanthropic::Client`]. Concurrency defaults to 1; batches chunk
    /// at [`DEFAULT_MAX_BATCH`] and poll every [`DEFAULT_POLL_PERIOD`]. See
    /// [`with_concurrency`](Self::with_concurrency) /
    /// [`with_batch`](Self::with_batch) to tune.
    pub fn new(client: misanthropic::Client) -> Self {
        Self {
            client,
            concurrency: 1.try_into().unwrap(),
            max_batch: DEFAULT_MAX_BATCH,
            poll_period: DEFAULT_POLL_PERIOD,
        }
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
        self.client.models().await
    }

    fn max_concurrency(&self) -> NonZeroUsize {
        self.concurrency
    }
}
