//! Concrete [`Inference`] transports.
//!
//! - [`Direct`]: the Messages API — one `Client::message` call per `infer`.
//! - [`Batch`]: the Anthropic Batch API — a whole cohort packed into one (or a
//!   few chunked) submissions, each polled to completion.
//!
//! Both consume [`misanthropic::prompt::Prompt`] and use [`misanthropic::client::Error`]
//! as their error (which already absorbs per-item batch errors via its
//! `Anthropic(AnthropicError)` variant). Construction is inherent — the
//! orchestrator builds these and hands them to a reactor.

use std::collections::HashMap;
use std::time::Duration;

use misanthropic::{Client, batch, prompt::Prompt, response};

use super::RetryAfter;
use super::backend::{BatchInference, Inference};

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

/// The Messages-API transport: each `infer` is one `Client::message` call.
pub struct Direct {
    client: Client,
    concurrency: Option<usize>,
}

impl Direct {
    /// A concurrency-unbounded transport, for breakpoint-cached backends
    /// (Anthropic, blallama) that run agents in parallel for free.
    pub fn new(client: Client) -> Self {
        Self {
            client,
            concurrency: None,
        }
    }

    /// A serial-to-completion transport, for Ollama — whose single KV slot
    /// thrashes under concurrency, so agents must run one at a time.
    pub fn serial(client: Client) -> Self {
        Self {
            client,
            concurrency: Some(1),
        }
    }
}

#[async_trait::async_trait]
impl Inference for Direct {
    type Error = misanthropic::client::Error;
    type Prompt = Prompt;

    async fn infer(
        &self,
        prompt: &Prompt,
    ) -> Result<response::Message, Self::Error> {
        self.client.message(prompt).await
    }

    async fn models(&self) -> Result<misanthropic::model::Models, Self::Error> {
        self.client.models().await
    }

    fn max_concurrency(&self) -> Option<usize> {
        self.concurrency
    }
}

/// The Batch-API transport. `infer` falls back to a single Messages call;
/// [`infer_batch`](BatchInference::infer_batch) packs the cohort into chunked
/// submissions, polling each to completion.
pub struct Batch {
    client: Client,
    /// Maximum prompts per submission; larger cohorts are chunked.
    max_batch: usize,
    /// How long to wait between `batch_poll`s.
    poll_period: Duration,
}

impl Batch {
    pub fn new(
        client: Client,
        max_batch: usize,
        poll_period: Duration,
    ) -> Self {
        Self {
            client,
            max_batch,
            poll_period,
        }
    }
}

#[async_trait::async_trait]
impl Inference for Batch {
    type Error = misanthropic::client::Error;
    type Prompt = Prompt;

    async fn infer(
        &self,
        prompt: &Prompt,
    ) -> Result<response::Message, Self::Error> {
        self.client.message(prompt).await
    }

    async fn models(&self) -> Result<misanthropic::model::Models, Self::Error> {
        self.client.models().await
    }
}

#[async_trait::async_trait]
impl BatchInference for Batch {
    async fn infer_batch(
        &self,
        prompts: &[&Prompt],
    ) -> Result<Vec<Result<response::Message, Self::Error>>, Self::Error> {
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
            let items: Vec<(batch::Id, &Prompt)> = (start..end)
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
}
