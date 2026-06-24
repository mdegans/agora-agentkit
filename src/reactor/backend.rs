use serde::{Serialize, de::DeserializeOwned};

use crate::ids::AgentId;

/// A model transport: one prompt in, one assistant response out. The agent that
/// calls this never learns whether it spoke to the Messages API or rode a
/// batch — batching is an *additional* capability (see [`BatchInference`]),
/// never visible here, so every agent and reactor step is written against this
/// one method, identical across transports.
///
/// Construction is the orchestrator's concern (inherent constructors on the
/// concrete transport), deliberately *not* on this trait.
#[async_trait::async_trait]
pub trait Inference: Send + Sync {
    type Error: super::Error;

    /// The prompt representation this transport consumes. It lives on the
    /// trait, not on `infer`, because a batch submission can only pack a
    /// *single* prompt type — a per-method `<P>` would make batching
    /// impossible. The common choice is [`misanthropic::prompt::Prompt`].
    type Prompt: Serialize + Send + Sync;

    /// Run one prompt to a single assistant response (full
    /// [`response::Message`] — the reactor needs `stop_reason` for quiescence
    /// and `usage` for cache accounting). Takes `&Self::Prompt` so the agent
    /// keeps ownership of its prompt across the call.
    ///
    /// [`response::Message`]: misanthropic::response::Message
    async fn infer(
        &self,
        prompt: &Self::Prompt,
    ) -> Result<misanthropic::response::Message, Self::Error>;

    /// The models this transport can serve, for routing agents by model. Mirrors
    /// [`misanthropic::Client::models`].
    async fn models(&self) -> Result<misanthropic::model::Models, Self::Error>;

    /// How many agents this transport will run at once. `Some(1)` forces
    /// serial-to-completion execution (Ollama, whose single KV slot thrashes
    /// under concurrency); `None` means unbounded (Anthropic and blallama are
    /// breakpoint-cached, so concurrency is free).
    fn max_concurrency(&self) -> Option<usize> {
        None
    }
}

/// A transport that can additionally run a *cohort* of prompts as one batch
/// submission — much cheaper per token on Anthropic. The round-major batch
/// reactor requires this; the agent never sees it.
#[async_trait::async_trait]
pub trait BatchInference: Inference {
    /// Submit every prompt as one batch and return the results **aligned to
    /// input order**. The outer `Err` means the whole submission failed (the
    /// transport is dead); a per-item `Err` (canceled / expired / errored)
    /// leaves that agent un-advanced so it re-batches next round — retry for
    /// free, bounded by the agent's own round budget. Implementations chunk
    /// against the provider's batch-size cap internally.
    ///
    /// Takes an [`ExactSizeIterator`] rather than a slice so callers can pass a
    /// lazy `iter().map(..)` over their live cohort without materializing a
    /// `Vec<&Prompt>` first; the exact length lets implementations size their
    /// output buffer up front.
    async fn infer_batch<'p, I>(
        &self,
        prompts: I,
    ) -> Result<Vec<Result<misanthropic::response::Message, Self::Error>>, Self::Error>
    where
        I: ExactSizeIterator<Item = &'p Self::Prompt> + Send,
        Self::Prompt: 'p;
}

/// Persistence as an opaque key-value store over agent ids. It deals only in
/// serializable bytes — it never imports `Agent`, `ToolBox`, or an agent's
/// error type, so one `Storage` can back many different kinds of agent. The
/// reactor is the only place a stored payload is turned back into an `Agent`.
///
/// Implementations write just the two `*_raw` methods; the typed `save`/`load`
/// convenience is provided.
#[async_trait::async_trait]
pub trait Storage: Sized + Send + Sync {
    type Error: super::Error + From<serde_json::Error>;

    /// Persist an opaque JSON value under `id`, overwriting any prior value.
    async fn save_raw(&mut self, id: AgentId, value: serde_json::Value) -> Result<(), Self::Error>;

    /// Load the JSON value stored under `id`, or `None` if there is none.
    async fn load_raw(&self, id: AgentId) -> Result<Option<serde_json::Value>, Self::Error>;

    /// Serialize and store [`Agent::State`]
    ///
    /// [`Agent::State`]: crate::reactor::Agent::State
    async fn save<T: Serialize + Sync>(
        &mut self,
        id: AgentId,
        value: &T,
    ) -> Result<(), Self::Error> {
        self.save_raw(id, serde_json::to_value(value)?).await
    }

    /// Load and deserialize `Agent::State` or `None` if nothing is stored for `id`.
    async fn load<T: DeserializeOwned>(&self, id: AgentId) -> Result<Option<T>, Self::Error> {
        Ok(self
            .load_raw(id)
            .await?
            .map(serde_json::from_value)
            .transpose()?)
    }

    /// Persist many values at once. **Atomic**: implementations are expected to
    /// commit the whole batch or none of it, so this returns a single result
    /// for the cohort rather than per-item. The default loops [`save_raw`];
    /// override it to do one query (e.g. a multi-row SQL upsert).
    ///
    /// [`save_raw`]: Storage::save_raw
    async fn save_all_raw(
        &mut self,
        items: Vec<(AgentId, serde_json::Value)>,
    ) -> Result<(), Self::Error> {
        for (id, value) in items {
            self.save_raw(id, value).await?;
        }
        Ok(())
    }

    /// Load many values at once, skipping ids with nothing stored. The default
    /// loops [`load_raw`]; override it to do one query (e.g. `WHERE id IN …`).
    ///
    /// [`load_raw`]: Storage::load_raw
    async fn load_all_raw(
        &self,
        ids: &[AgentId],
    ) -> Result<Vec<(AgentId, serde_json::Value)>, Self::Error> {
        let mut out = Vec::with_capacity(ids.len());
        for &id in ids {
            if let Some(value) = self.load_raw(id).await? {
                out.push((id, value));
            }
        }
        Ok(out)
    }

    /// Serialize and store many [`Agent::State`](crate::reactor::Agent::State)s
    /// in one (atomic) batch.
    async fn save_all<T: Serialize + Send>(
        &mut self,
        items: Vec<(AgentId, T)>,
    ) -> Result<(), Self::Error> {
        let mut raw = Vec::with_capacity(items.len());
        for (id, value) in &items {
            raw.push((*id, serde_json::to_value(value)?));
        }
        self.save_all_raw(raw).await
    }

    /// Load and deserialize many payloads, skipping ids with nothing stored.
    async fn load_all<T: DeserializeOwned>(
        &self,
        ids: &[AgentId],
    ) -> Result<Vec<(AgentId, T)>, Self::Error> {
        let raw = self.load_all_raw(ids).await?;
        let mut out = Vec::with_capacity(raw.len());
        for (id, value) in raw {
            out.push((id, serde_json::from_value(value)?));
        }
        Ok(out)
    }
}
