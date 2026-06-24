mod agent;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub use agent::{Affinity, Agent, Control, Outcome, State};

mod backend;
pub use backend::{BatchInference, Inference, SaveError, Storage};
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

/// Retry classification for an error: how long to wait before retrying the
/// operation that produced it, or `None` if it is fatal and not worth retrying.
///
/// `None` = fatal / no retry; `Some(ZERO)` = retry immediately; `Some(d)` =
/// retry after `d`. The default is fatal — a type opts *into* retryability by
/// overriding this. Anthropic surfaces a `Retry-After` on 429/529, which the
/// transport error forwards (see the impl in [`transport`]).
pub trait RetryAfter {
    fn retry_after(&self) -> Option<std::time::Duration> {
        None
    }
}

pub trait Error: std::error::Error + Send + Sync + RetryAfter + 'static {}
impl<T: std::error::Error + Send + Sync + RetryAfter + 'static> Error for T {}

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

// Retry classification delegates to the inner leaf error; a missing snapshot is
// fatal (there is nothing to retry).
impl<I: Inference, S: Storage, A: Agent> RetryAfter for ReactorError<I, S, A> {
    fn retry_after(&self) -> Option<std::time::Duration> {
        match self {
            ReactorError::InferenceError(e) => e.retry_after(),
            ReactorError::AgentError(e) => e.retry_after(),
            ReactorError::StorageError(e) => e.retry_after(),
            ReactorError::NotFound(_) => None,
        }
    }
}

/// A driven agent paired with how its drive ended — the unit the sequential
/// reactor collects concurrently and then hands to [`persist_all`](Reactor::persist_all).
type Driven<I, S, A> = (A, Result<Outcome, ReactorError<I, S, A>>);

pub struct Reactor<I: Inference, S: Storage, A: Agent> {
    inference: I,
    storage: S,
    agents: VecDeque<A>,
    done: BTreeMap<AgentId, A>,
    failed: BTreeMap<AgentId, A>,
    errors: BTreeMap<AgentId, ReactorError<I, S, A>>,
    /// Serialized snapshots of agents whose state didn't persist — populated by
    /// the persist path on a save failure, drained into [`Report::unsaved`].
    unsaved: BTreeMap<AgentId, serde_json::Value>,
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
            unsaved: BTreeMap::new(),
        }
    }

    pub fn report(&self) -> Report {
        Report {
            done: self.done.len(),
            failed: self.failed.len(),
            errors: self
                .errors
                .iter()
                .map(|(id, e)| (*id, ErrorReport::from(e)))
                .collect(),
            unsaved: self.unsaved.clone(),
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

    /// Persist every driven agent's snapshot in one bulk save, then file each
    /// into done/failed. A drive error fails that agent and is recorded per-agent.
    /// The save reports exactly which ids committed ([`SaveError::saved`]): an
    /// agent is `done` only if it both completed *and* persisted; the snapshots
    /// of those that didn't persist are kept in [`unsaved`](Self::unsaved) so the
    /// caller can recover them. The store error is attributed once (it isn't
    /// `Clone`, and `unsaved` is the authoritative set of who didn't save).
    async fn persist_all(&mut self, driven: Vec<Driven<I, S, A>>) {
        // Serialize each snapshot once. A serialize failure is itself a storage
        // error — that agent can be neither persisted nor recovered.
        let mut values: Vec<(AgentId, serde_json::Value)> = Vec::with_capacity(driven.len());
        for (agent, _) in &driven {
            let id = agent.id();
            match serde_json::to_value(agent.snapshot()) {
                Ok(v) => values.push((id, v)),
                Err(e) => {
                    self.errors
                        .insert(id, ReactorError::StorageError(S::Error::from(e)));
                }
            }
        }
        let attempted: BTreeSet<AgentId> = values.iter().map(|(id, _)| *id).collect();

        // Persist, learning exactly which ids committed. The clone feeds the
        // save; the original is drained below into `unsaved`.
        let (saved, mut save_err) = match self.storage.save_all_raw(values.clone()).await {
            Ok(()) => (attempted.clone(), None),
            Err(SaveError { saved, inner }) => (saved, Some(inner)),
        };
        // Keep the only in-memory copy of every attempted-but-uncommitted snapshot.
        for (id, value) in values {
            if !saved.contains(&id) {
                self.unsaved.insert(id, value);
            }
        }

        for (agent, drive) in driven {
            let id = agent.id();
            let done = matches!(drive, Ok(Outcome::Complete)) && saved.contains(&id);
            match drive {
                Err(e) => {
                    self.errors.insert(id, e);
                }
                // Pin the lone store error on one clean agent that didn't commit.
                Ok(_) => {
                    if attempted.contains(&id)
                        && !saved.contains(&id)
                        && let Some(e) = save_err.take()
                    {
                        self.errors.insert(id, ReactorError::StorageError(e));
                    }
                }
            }
            if done {
                self.done.insert(id, agent);
            } else {
                self.failed.insert(id, agent);
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

/// Which reactor operation an error came from, so a caller deciding whether to
/// retry knows *what* to retry (re-infer, re-drive, or re-save).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorKind {
    Inference,
    Agent,
    Storage,
    NotFound,
}

/// A serializable rendering of one [`ReactorError`], flattened so it crosses the
/// `dyn Run` erasure that the orchestrator runs reactors behind. We evaluate the
/// retry classification ([`RetryAfter`]) here, while the concrete error type is
/// still known, and keep only the resulting data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorReport {
    pub kind: ErrorKind,
    /// `None` = fatal; `Some(d)` = retry after `d`. See [`RetryAfter`].
    pub retry_after: Option<std::time::Duration>,
    pub message: String,
}

impl<I: Inference, S: Storage, A: Agent> From<&ReactorError<I, S, A>> for ErrorReport {
    fn from(e: &ReactorError<I, S, A>) -> Self {
        let kind = match e {
            ReactorError::InferenceError(_) => ErrorKind::Inference,
            ReactorError::AgentError(_) => ErrorKind::Agent,
            ReactorError::StorageError(_) => ErrorKind::Storage,
            ReactorError::NotFound(_) => ErrorKind::NotFound,
        };
        ErrorReport {
            kind,
            retry_after: e.retry_after(),
            message: e.to_string(),
        }
    }
}

/// A summary of one reactor run: how many agents finished, how many failed, the
/// flattened error for each agent that errored, and — crucially — the serialized
/// snapshots of any agents whose post-inference state did **not** persist.
///
/// `unsaved` is the recovery channel: on a save failure those snapshots are the
/// only surviving copy of expensive (paid-for) state, so they ride back here for
/// the caller to dump or re-save. It is empty on a fully successful run. Both
/// maps cross the `dyn Run` erasure as plain serializable data.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Report {
    pub done: usize,
    pub failed: usize,
    pub errors: BTreeMap<AgentId, ErrorReport>,
    pub unsaved: BTreeMap<AgentId, serde_json::Value>,
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

// The orchestrator drives reactors as `Box<dyn Run>`; keep that contract honest.
static_assertions::assert_obj_safe!(Run);

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
        let driven: Vec<Driven<I, S, A>> = futures::stream::iter(agents)
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
