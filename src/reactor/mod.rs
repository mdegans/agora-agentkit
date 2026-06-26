//! A [`Reactor`] runs [`Agent`]s to completion by scheduling agentic tasks and
//! submitting prompts to [`Inference`] engines. Post-[`Run`] agents [`Persist`]
//! in a [`Storage`] implementation (disk, sql, etc).
//!
//! All traits, [`Agent`], [`Inference`] and [`Storage`] all have an associated
//! [`Error`] type requiring [`RetryAfter`] be implemented so 429, 529 and more
//! can be handled.
//!
//! The top-level to handle this, but not required, is an [`Orchestrator`] that
//! runs all the reactors concurrently, collecting their [`Report`]s into an
//! [`OrchestratorReport`] keyed by [`ReactorId`].

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

mod agent;
pub use agent::{Affinity, Agent, Control, Outcome, State};

mod backend;
pub use backend::{AgentNotFound, Inference, SaveError, Storage};

mod batch_reactor;
pub use batch_reactor::BatchReactor;

mod orchestrator;
pub use orchestrator::{Orchestrator, OrchestratorReport};

#[cfg(feature = "client")]
pub mod anthropic;
#[cfg(feature = "client")]
pub use anthropic::Client;

use crate::ids::{AgentId, ReactorId};

#[cfg(test)]
mod tests;

/// Crate `Error` trait used for all [`Reactor`] *child* related errors. See
/// also [`ReactorError`].
pub trait Error:
    std::error::Error + Send + Sync + RetryAfter + 'static
{
}
impl<T: std::error::Error + Send + Sync + RetryAfter + 'static> Error for T {}

/// Retry classification for an [`Error`]
pub trait RetryAfter {
    /// - `None` = fatal / no retry
    /// - `Some(ZERO)` = retry immediately
    /// - `Some(d)` = retry after d
    fn retry_after(&self) -> Option<std::time::Duration> {
        None
    }

    /// Returns `true` if the [`Error`] is fatal ([`retry_after`] is `None`)
    ///
    /// [`retry_after`]: Self::retry_after
    fn is_fatal(&self) -> bool {
        self.retry_after().is_none()
    }
}

/// Something went wrong in the [`Agent`] [`Reactor`]
#[derive(Debug, thiserror::Error)]
pub enum ReactorError<I: Inference, S: Storage, A: Agent> {
    #[error("inference: {0}")]
    InferenceError(I::Error),
    #[error("agent: {0}")]
    AgentError(A::Error),
    #[error("storage: {0}")]
    StorageError(S::Error),
}

impl<I: Inference, S: Storage, A: Agent> RetryAfter for ReactorError<I, S, A> {
    fn retry_after(&self) -> Option<std::time::Duration> {
        match self {
            ReactorError::InferenceError(e) => e.retry_after(),
            ReactorError::AgentError(e) => e.retry_after(),
            ReactorError::StorageError(e) => e.retry_after(),
        }
    }
}

/// An [`Agent`] ready to [`persist_all`](Reactor::persist_all) to [`Storage`]
type Persist<I, S, A> = (A, Result<Outcome, ReactorError<I, S, A>>);

/// An `Reactor` is an engine in an [`Orchestrator`] that drives [`Agent`]s
///
/// - A `Reactor` is Default when I and S are Default
/// - A `Reactor` implementing Default can be collected from an iterable of
///   anything that converts Into<A>. A From implementation also exists in this
///   case.
pub struct Reactor<I: Inference, S: Storage, A: Agent> {
    id: ReactorId,
    inference: I,
    storage: S,
    agents: VecDeque<A>,
    done: BTreeMap<AgentId, A>,
    failed: BTreeMap<AgentId, A>,
    errors: BTreeMap<AgentId, ReactorError<I, S, A>>,
    /// [`State`] of [`Agent`]s whose state didn't persist. Drained into
    /// [`Report::unsaved`].
    unsaved: BTreeMap<AgentId, serde_json::Value>,
}

/// A [`Reactor`] using an [`anthropic::Client`] for [`Inference`].
#[cfg(feature = "client")]
pub type AnthropicReactor<S, A> = Reactor<anthropic::Client, S, A>;

impl<I, S, A> Default for Reactor<I, S, A>
where
    I: Inference + Default,
    S: Storage + Default,
    A: Agent,
{
    fn default() -> Self {
        Self::new(I::default(), S::default(), Vec::<A>::new())
    }
}

impl<I, S, A, Ai, As> From<As> for Reactor<I, S, A>
where
    I: Inference,
    S: Storage,
    A: Agent,
    Self: Default + FromIterator<Ai>,
    As: IntoIterator<Item = Ai>,
{
    fn from(value: As) -> Self {
        value.into_iter().collect()
    }
}

impl<I, S, A, Ai> FromIterator<Ai> for Reactor<I, S, A>
where
    I: Inference,
    S: Storage,
    Self: Default,
    Ai: Into<A>,
    A: Agent,
{
    fn from_iter<T: IntoIterator<Item = Ai>>(iter: T) -> Self {
        Self::default().with_agents(iter)
    }
}

impl<I: Inference, S: Storage, A: Agent> Reactor<I, S, A> {
    /// Consecutive [`Stalled`](Control::Stalled) rounds before the [`Reactor`]
    /// aborts the [`Agent`]
    pub const MAX_STALLS: usize = 3;

    /// Build a [`Reactor`] from [`Inference`], [`Storage`], and [`Agent`]s
    pub fn new<Ai>(
        inference: I,
        storage: S,
        agents: impl IntoIterator<Item = Ai>,
    ) -> Self
    where
        Ai: Into<A>,
    {
        Self {
            inference,
            storage,
            id: ReactorId::new(),
            agents: VecDeque::new(),
            done: BTreeMap::new(),
            failed: BTreeMap::new(),
            errors: BTreeMap::new(),
            unsaved: BTreeMap::new(),
        }
        .with_agents(agents)
    }

    /// Add [`Agent`]s to self.
    pub fn with_agents<Ai>(
        mut self,
        agents: impl IntoIterator<Item = Ai>,
    ) -> Self
    where
        Ai: Into<A>,
    {
        self.extend(agents);
        self
    }

    /// Extend [`Agent`]s onto self.
    pub fn extend<Ai>(&mut self, agents: impl IntoIterator<Item = Ai>)
    where
        Ai: Into<A>,
    {
        self.agents.extend(agents.into_iter().map(Into::into));
    }

    /// Generate a [`Report`] of a [`Reactor`] [`Run`] (done, failed, etc.)
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

    /// `drive_one` [`Agent`] to completion using the supplied [`Inference`]
    /// engine
    async fn drive_one(
        inference: &I,
        agent: &mut A,
    ) -> Result<Outcome, ReactorError<I, S, A>> {
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
                    if stalls >= Self::MAX_STALLS {
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

    /// Bulk save agents then calculate, done, failed, etc. for a [`Report`]. Of
    /// particular interest are the [`unsaved`](Self::unsaved).
    // TODO: We do this all at once, which is effecient but also means if there
    // is a power failure or whatever we lose everything. Instead we could
    // accept a Stream and map what's here now as an inner function onto chunks
    // of ~30. We simply remove the `collect` from the callsite in this case.
    async fn persist_all(&mut self, agent_results: Vec<Persist<I, S, A>>) {
        // Serialize each snapshot once. A serialize failure is itself a storage
        // error — that agent can be neither persisted nor recovered.
        let mut values: Vec<(AgentId, serde_json::Value)> =
            Vec::with_capacity(agent_results.len());
        for (agent, _) in &agent_results {
            let id = agent.id();
            match serde_json::to_value(agent.state()) {
                Ok(v) => values.push((id, v)),
                Err(e) => {
                    self.errors.insert(
                        id,
                        ReactorError::StorageError(S::Error::from(e)),
                    );
                }
            }
        }
        let attempted: BTreeSet<AgentId> =
            values.iter().map(|(id, _)| *id).collect();

        // Persist, learning exactly which ids committed. The clone feeds the
        // save; the original is drained below into `unsaved`.
        let (saved, mut save_err) =
            // `save_all_raw` for now because save_all
            match self.storage.save_all_raw(values.clone().into_iter()).await {
                Ok(()) => (attempted.clone(), None),
                Err(SaveError { saved, inner }) => (saved, Some(inner)),
            };

        // Keep the only in-memory copy of every attempted-but-uncommitted snapshot.
        for (id, value) in values {
            if !saved.contains(&id) {
                self.unsaved.insert(id, value);
            }
        }

        for (agent, result) in agent_results {
            let id = agent.id();
            let done =
                matches!(result, Ok(Outcome::Complete)) && saved.contains(&id);
            match result {
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

/// Bulk-load [`Agent`]s. A Deserialize failure will abort the entire batch
pub async fn load_agents<S: Storage, A: Agent>(
    storage: &S,
    ids: impl ExactSizeIterator<Item = AgentId> + Send,
) -> Result<(Vec<A>, Vec<(AgentId, A::Error)>), S::Error> {
    let raw = storage.load_all::<_, A>(ids).await?;
    let mut agents = Vec::with_capacity(raw.len());
    let mut failures = Vec::new();
    for (id, result) in raw {
        match result {
            Ok(state) => match A::new(id, state) {
                Ok(agent) => agents.push(agent),
                Err(e) => failures.push((id, e)),
            },
            Err(e) => failures.push((
                id,
                A::Error::from(
                    Box::new(e) as Box<dyn std::error::Error + Send + Sync>
                ),
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

impl<I: Inference, S: Storage, A: Agent> From<&ReactorError<I, S, A>>
    for ErrorReport
{
    fn from(e: &ReactorError<I, S, A>) -> Self {
        let kind = match e {
            ReactorError::InferenceError(_) => ErrorKind::Inference,
            ReactorError::AgentError(_) => ErrorKind::Agent,
            ReactorError::StorageError(_) => ErrorKind::Storage,
        };
        ErrorReport {
            kind,
            retry_after: e.retry_after(),
            message: e.to_string(),
        }
    }
}

/// A report on one [`Reactor`] [`Run`]. Can be added together to combine.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Report {
    pub done: usize,
    pub failed: usize,
    pub errors: BTreeMap<AgentId, ErrorReport>,
    pub unsaved: BTreeMap<AgentId, serde_json::Value>,
}

impl std::ops::Add<Report> for Report {
    type Output = Report;

    fn add(mut self, rhs: Report) -> Self::Output {
        self += rhs;
        self
    }
}

impl std::ops::AddAssign<Report> for Report {
    fn add_assign(&mut self, rhs: Report) {
        self.done += rhs.done;
        self.failed += rhs.failed;
        self.errors.extend(rhs.errors);
        self.unsaved.extend(rhs.unsaved);
    }
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
    /// Return the [`ReactorId`] assocaited with the [`Run`]
    fn id(&self) -> ReactorId;

    /// Run the reactor to completion.
    async fn run(&mut self) -> Result<Report, RunError>;
}

// The orchestrator drives reactors as `Box<dyn Run>`; keep that contract honest.
static_assertions::assert_obj_safe!(Run);

/// Agent-major (sequential) reactor: each agent runs to completion, with up to
/// [`max_concurrency`](Inference::max_concurrency) agents in flight.
#[async_trait::async_trait]
impl<I: Inference, S: Storage, A: Agent> Run for Reactor<I, S, A> {
    fn id(&self) -> ReactorId {
        self.id
    }

    async fn run(&mut self) -> Result<Report, RunError> {
        let agents = std::mem::take(&mut self.agents);
        let inference = &self.inference;
        let limit = inference.max_concurrency().get();

        // Drive every agent concurrently (bounded by the transport). Only the
        // shared `&inference` is borrowed here — persistence happens after, so
        // one agent's failure never aborts the cohort.
        let to_persist: Vec<Persist<I, S, A>> = futures::stream::iter(agents)
            .map(|mut agent| async move {
                let result = Self::drive_one(inference, &mut agent).await;
                (agent, result)
            })
            .buffer_unordered(limit)
            .collect()
            .await;

        // Persist all snapshots in one bulk save, then bucket.
        self.persist_all(to_persist).await;
        Ok(self.report())
    }
}
