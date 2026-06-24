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

use super::backend::{BatchInference, Inference};

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

    async fn infer(&self, prompt: &Prompt) -> Result<response::Message, Self::Error> {
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
    pub fn new(client: Client, max_batch: usize, poll_period: Duration) -> Self {
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

    async fn infer(&self, prompt: &Prompt) -> Result<response::Message, Self::Error> {
        self.client.message(prompt).await
    }

    async fn models(&self) -> Result<misanthropic::model::Models, Self::Error> {
        self.client.models().await
    }
}

#[async_trait::async_trait]
impl BatchInference for Batch {
    async fn infer_batch<'p, I>(
        &self,
        mut prompts: I,
    ) -> Result<Vec<Result<response::Message, Self::Error>>, Self::Error>
    where
        I: ExactSizeIterator<Item = &'p Prompt> + Send,
        Prompt: 'p,
    {
        let chunk = self.max_batch.max(1);
        // Pre-size to the cohort's exact length (the lower `size_hint` bound is
        // exact for an `ExactSizeIterator`) so the per-chunk `extend`s below
        // never reallocate. We pull `chunk` prompts at a time straight off the
        // iterator, so the full `Vec<&Prompt>` is never materialized.
        let mut out: Vec<Result<response::Message, Self::Error>> =
            Vec::with_capacity(prompts.size_hint().0);

        loop {
            // Tag each prompt in this chunk with a fresh batch id and remember
            // its position *within the chunk*. `Item = &Prompt` — results route
            // by id, so we never need the prompts back and never clone them.
            let mut id_to_local: HashMap<batch::Id, usize> = HashMap::new();
            let mut items: Vec<(batch::Id, &Prompt)> = Vec::new();
            for _ in 0..chunk {
                let Some(prompt) = prompts.next() else { break };
                let id = batch::Id::default();
                id_to_local.insert(id, items.len());
                items.push((id, prompt));
            }
            if items.is_empty() {
                break;
            }
            let mut slots: Vec<Option<Result<response::Message, Self::Error>>> =
                (0..items.len()).map(|_| None).collect();

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
                if let Some(&local) = id_to_local.get(&id) {
                    // `BatchResult -> Result<Message, AnthropicError>`, then
                    // `AnthropicError -> misanthropic::client::Error`.
                    let r: Result<response::Message, misanthropic::client::AnthropicError> =
                        result.into();
                    slots[local] = Some(r.map_err(Into::into));
                }
            }

            // Chunks are consumed in order and each chunk's slots stay in input
            // order, so appending keeps `out` aligned to the input. A slot still
            // empty means the provider returned no result for that id; surface
            // it as an error so the agent re-batches next round.
            out.extend(slots.into_iter().map(|slot| {
                slot.unwrap_or(Err(misanthropic::client::Error::UnexpectedResponse {
                    message: "batch returned no result for prompt",
                }))
            }));
        }

        Ok(out)
    }
}
