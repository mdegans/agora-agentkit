mod agent;
use std::collections::{BTreeMap, VecDeque};

pub use agent::{Affinity, Agent, Control, Outcome, State};

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

/// Consecutive [`Control::Stalled`] rounds (a turn that made no successful tool
/// call, or an unparseable response) a single agent may accrue before the
/// reactor gives up and fails it — the "needs a fresh context" backstop.
const MAX_STALLS: usize = 3;

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
    /// `on_turn → infer → handle` until it reports [`Control::Done`] (or stalls
    /// past the cap), then teardown. Borrows only `&I` (shared), so callers may
    /// drive many agents concurrently; persistence is deferred to
    /// [`finish`](Self::finish).
    async fn drive_one(inference: &I, agent: &mut A) -> Result<Outcome, ReactorError<I, S, A>> {
        agent.on_init().await.map_err(ReactorError::AgentError)?;
        let mut stalls = 0usize;
        let outcome = loop {
            agent.on_turn().await.map_err(ReactorError::AgentError)?;
            let response = inference
                .infer(agent.prompt())
                .await
                .map_err(ReactorError::InferenceError)?;
            match agent
                .handle(response)
                .await
                .map_err(ReactorError::AgentError)?
            {
                Control::Done(outcome) => break outcome,
                Control::Continue => stalls = 0,
                Control::Stalled => {
                    stalls += 1;
                    if stalls >= MAX_STALLS {
                        break Outcome::Failed;
                    }
                }
            }
        };
        agent
            .on_teardown()
            .await
            .map_err(ReactorError::AgentError)?;
        Ok(outcome)
    }

    /// Persist every driven agent's snapshot in **one** (atomic) bulk save, then
    /// file each into done/failed. A drive error fails that agent and is
    /// recorded per-agent; a bulk-save failure fails the whole cohort's
    /// persistence (it's atomic) and is recorded once, against one clean agent.
    async fn persist_all(&mut self, driven: Vec<(A, Result<Outcome, ReactorError<I, S, A>>)>) {
        let snapshots: Vec<(AgentId, A::State)> = driven
            .iter()
            .map(|(agent, _)| (agent.id(), agent.snapshot()))
            .collect();
        let mut save_err = self.storage.save_all(snapshots).await.err();
        let saved_ok = save_err.is_none();

        for (agent, drive) in driven {
            let id = agent.id();
            let failed = !matches!(drive, Ok(Outcome::Complete)) || !saved_ok;
            match drive {
                Err(e) => {
                    self.errors.insert(id, e);
                }
                // The single atomic save error is attributed to one clean agent.
                Ok(_) => {
                    if let Some(e) = save_err.take() {
                        self.errors.insert(id, ReactorError::StorageError(e));
                    }
                }
            }
            if failed {
                self.failed.insert(id, agent);
            } else {
                self.done.insert(id, agent);
            }
        }
    }
}

/// Bulk-load agents: one [`load_all_raw`](Storage::load_all_raw) query for all
/// `ids`, then construct each from its state. Per-agent deserialize/construct
/// failures are returned rather than aborting the batch; ids with nothing stored
/// are skipped. Hand the constructed agents to a reactor's `new`.
pub async fn load_agents<S: Storage, A: Agent>(
    storage: &S,
    ids: &[AgentId],
) -> Result<(Vec<A>, Vec<(AgentId, A::Error)>), S::Error> {
    let raw = storage.load_all_raw(ids).await?;
    let mut agents = Vec::with_capacity(raw.len());
    let mut failures = Vec::new();
    for (id, value) in raw {
        match serde_json::from_value::<A::State>(value) {
            Ok(state) => match A::new(id, state) {
                Ok(agent) => agents.push(agent),
                Err(e) => failures.push((id, e)),
            },
            Err(e) => failures.push((
                id,
                A::Error::from(Box::new(e) as Box<dyn std::error::Error + Send + Sync>),
            )),
        }
    }
    Ok((agents, failures))
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
        let driven: Vec<(A, Result<Outcome, ReactorError<I, S, A>>)> =
            futures::stream::iter(agents)
                .map(|mut agent| async move {
                    let result = Self::drive_one(inference, &mut agent).await;
                    (agent, result)
                })
                .buffer_unordered(limit)
                .collect()
                .await;

        // Persist all snapshots in one bulk save, then bucket.
        self.persist_all(driven).await;
        Ok(self.report())
    }
}
