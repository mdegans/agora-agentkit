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
    async fn infer_batch(
        &self,
        prompts: &[&Self::Prompt],
    ) -> Result<Vec<Result<misanthropic::response::Message, Self::Error>>, Self::Error>;
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
}
