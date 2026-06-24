mod agent;
use std::collections::{BTreeMap, VecDeque};

pub use agent::{Affinity, Agent, Outcome, State};

mod backend;
pub use backend::{BatchInference, Inference, Storage};
mod batch_reactor;
pub use batch_reactor::BatchReactor;
mod orchestrator;
pub use orchestrator::{Orchestrator, OrchestratorReport, partition_by_affinity};
#[cfg(test)]
mod tests;
pub mod transport;
use futures::StreamExt;
use misanthropic::prompt::Prompt;
use serde::{Deserialize, Serialize};
pub use transport::{Batch, Direct};

use crate::ids::AgentId;

pub trait Error: std::error::Error + Send + Sync + 'static {}
impl<T: std::error::Error + Send + Sync + 'static> Error for T {}

#[derive(Debug, thiserror::Error)]
pub enum ReactorError<I: Inference, S: Storage, A: Agent> {
    #[error("inference: {0}")]
    InferenceError(I::Error),
    #[error("agent: {0}")]
    AgentError(A::Error),
    #[error("storage: {0}")]
    StorageError(S::Error),
    #[error("no stored state for agent {0}")]
    NotFound(AgentId),
}

pub struct Reactor<I: Inference, S: Storage, A: Agent> {
    inference: I,
    storage: S,
    agents: VecDeque<A>,
    done: BTreeMap<AgentId, A>,
    failed: BTreeMap<AgentId, A>,
    errors: BTreeMap<AgentId, ReactorError<I, S, A>>,
}

impl<I: Inference<Prompt = Prompt>, S: Storage, A: Agent> Reactor<I, S, A> {
    /// Build a reactor over already-constructed transports and a set of agents.
    /// Construction of `I`/`S` is the orchestrator's concern, not this trait's.
    pub fn new(inference: I, storage: S, agents: impl IntoIterator<Item = A>) -> Self {
        Self {
            inference,
            storage,
            agents: agents.into_iter().collect(),
            done: BTreeMap::new(),
            failed: BTreeMap::new(),
            errors: BTreeMap::new(),
        }
    }

    pub fn report(&self) -> Report {
        Report {
            done: self.done.len(),
            failed: self.failed.len(),
            errors: self
                .errors
                .iter()
                .map(|(id, e)| (*id, e.to_string()))
                .collect(),
        }
    }

    /// Reconstruct an agent from storage: load its state and build it. Does
    /// *not* run `on_init` — the reactor does that as it drives the agent.
    pub async fn load_agent(&self, id: AgentId) -> Result<A, ReactorError<I, S, A>> {
        let state = self
            .storage
            .load::<A::State>(id)
            .await
            .map_err(ReactorError::StorageError)?
            .ok_or(ReactorError::NotFound(id))?;
        A::new(id, state).map_err(ReactorError::AgentError)
    }

    /// Drive one agent to completion against the transport: init, then
    /// `on_turn → infer → handle` until it reports an [`Outcome`], then
    /// teardown. Borrows only `&I` (shared), so callers may drive many agents
    /// concurrently; persistence is deferred to [`finish`](Self::finish).
    async fn drive_one(inference: &I, agent: &mut A) -> Result<(), ReactorError<I, S, A>> {
        agent.on_init().await.map_err(ReactorError::AgentError)?;
        while agent.outcome().is_none() {
            agent.on_turn().await.map_err(ReactorError::AgentError)?;
            let response = inference
                .infer(agent.prompt())
                .await
                .map_err(ReactorError::InferenceError)?;
            agent
                .handle(response)
                .await
                .map_err(ReactorError::AgentError)?;
        }
        agent
            .on_teardown()
            .await
            .map_err(ReactorError::AgentError)?;
        Ok(())
    }

    /// Persist `agent` and file it into done/failed. An agent is "failed" if it
    /// errored while driving, ended with [`Outcome::Failed`], or failed to
    /// save. The drive error takes precedence; a save error is recorded only
    /// when the agent otherwise ran clean.
    async fn finish(&mut self, agent: A, drive: Result<(), ReactorError<I, S, A>>) {
        let id = agent.id();
        let save_err = self.storage.save(id, &agent.snapshot()).await.err();
        let failed = drive.is_err()
            || save_err.is_some()
            || matches!(agent.outcome(), Some(Outcome::Failed));
        if let Err(e) = drive {
            self.errors.insert(id, e);
        } else if let Some(e) = save_err {
            self.errors.insert(id, ReactorError::StorageError(e));
        }
        if failed {
            self.failed.insert(id, agent);
        } else {
            self.done.insert(id, agent);
        }
    }
}

/// A summary of one reactor run: how many agents finished, how many failed, and
/// the rendered error for each agent that errored.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Report {
    pub done: usize,
    pub failed: usize,
    pub errors: BTreeMap<AgentId, String>,
}

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("Backend Error: {0}")]
    InferenceError(anyhow::Error),
    #[error("Agent Error: {0}")]
    AgentError(anyhow::Error),
    #[error("Storage Error: {0}")]
    StorageError(anyhow::Error),
}

#[async_trait::async_trait]
pub trait Run: Send {
    async fn run(&mut self) -> Result<Report, RunError>;
}

/// Agent-major (sequential) reactor: each agent runs to completion, with up to
/// [`max_concurrency`](Inference::max_concurrency) agents in flight. `Some(1)`
/// (Ollama) means strictly one-at-a-time, which keeps the KV cache local.
#[async_trait::async_trait]
impl<I: Inference<Prompt = Prompt>, S: Storage, A: Agent> Run for Reactor<I, S, A> {
    async fn run(&mut self) -> Result<Report, RunError> {
        let agents = std::mem::take(&mut self.agents);
        let inference = &self.inference;
        let limit = inference.max_concurrency().unwrap_or(usize::MAX).max(1);

        // Drive every agent concurrently (bounded by the transport). Only the
        // shared `&inference` is borrowed here — persistence happens after, so
        // one agent's failure never aborts the cohort.
        let driven: Vec<(A, Result<(), ReactorError<I, S, A>>)> = futures::stream::iter(agents)
            .map(|mut agent| async move {
                let result = Self::drive_one(inference, &mut agent).await;
                (agent, result)
            })
            .buffer_unordered(limit)
            .collect()
            .await;

        // Persist + bucket sequentially (needs `&mut self.storage`).
        for (agent, result) in driven {
            self.finish(agent, result).await;
        }
        Ok(self.report())
    }
}
