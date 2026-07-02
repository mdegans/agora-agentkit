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
use misanthropic::model::{ModelInfo, Models};
use misanthropic::prompt::Prompt;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

mod agent;
pub use agent::{Agent, Control, Outcome, State};

mod backend;
pub use backend::{AgentNotFound, Inference, SaveError, Storage};

pub mod inference;

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
    /// One error's data attributed to many agents at once (a whole batch
    /// submission failing) — pre-projected to [`ErrorReport`] because source
    /// errors aren't `Clone`
    #[error("{}", .0.message)]
    Shared(ErrorReport),
}

impl<I: Inference, S: Storage, A: Agent> ReactorError<I, S, A> {
    /// Which reactor operation it came from — the [`ErrorKind`] projection
    pub fn kind(&self) -> ErrorKind {
        match self {
            ReactorError::InferenceError(_) => ErrorKind::Inference,
            ReactorError::AgentError(_) => ErrorKind::Agent,
            ReactorError::StorageError(_) => ErrorKind::Storage,
            ReactorError::Shared(report) => report.kind,
        }
    }
}

impl<I: Inference, S: Storage, A: Agent> RetryAfter for ReactorError<I, S, A> {
    fn retry_after(&self) -> Option<std::time::Duration> {
        match self {
            ReactorError::InferenceError(e) => e.retry_after(),
            ReactorError::AgentError(e) => e.retry_after(),
            ReactorError::StorageError(e) => e.retry_after(),
            ReactorError::Shared(report) => report.retry_after,
        }
    }
}

/// An [`Agent`] ready to [`persist_all`](Reactor::persist_all) to [`Storage`]
type Persist<I, S, A> = (A, Result<Outcome, ReactorError<I, S, A>>);

/// How many consecutive batch-item failures (canceled / expired / errored
/// results, which never reach the agent's `handle` and so never charge its own
/// budget) one agent may accrue on the round-major path before the reactor gives
/// up on it. Without this, an item that always errors would re-batch forever.
const MAX_BATCH_ITEM_RETRIES: usize = 3;

/// What [`negotiate`] decided for an agent: which run-path (borrowing the
/// negotiated offered [`ModelInfo`], for [`Agent::on_admit`]), or rejection.
enum Admission<'a> {
    /// Run on the round-major batch path.
    Batch(&'a ModelInfo),
    /// Run on the agent-major sequential path.
    Sequential(&'a ModelInfo),
    /// The endpoint can't satisfy the agent's requested capabilities.
    Rejected,
}

/// Negotiate an agent's requested [`ModelInfo`] against what the endpoint
/// `offered` ([`Inference::models`]), deciding its run-path.
fn negotiate<'a>(offered: &'a Models, requested: &ModelInfo) -> Admission<'a> {
    let batch = requested.capabilities.batch.supported;

    for model in offered.iter() {
        if model.satisfies(requested) {
            if batch {
                return Admission::Batch(model);
            } else {
                return Admission::Sequential(model);
            }
        }
    }

    Admission::Rejected
}

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
    /// [`State`] of [`Agent`]s the endpoint couldn't satisfy (see [`negotiate`]).
    /// Drained into [`Report::rejected`] so the caller can re-route them.
    rejected: BTreeMap<AgentId, serde_json::Value>,
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
            rejected: BTreeMap::new(),
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
            rejected: self.rejected.clone(),
        }
    }

    /// `drive_one` [`Agent`] to completion using the supplied [`Inference`]
    /// engine. [`on_teardown`](Agent::on_teardown) runs however the drive
    /// ends — a stateful tool may hold real resources — and its error never
    /// clobbers the drive's own.
    async fn drive_one(
        inference: &I,
        agent: &mut A,
    ) -> Result<Outcome, ReactorError<I, S, A>> {
        let driven = Self::drive_inner(inference, agent).await;
        let teardown =
            agent.on_teardown().await.map_err(ReactorError::AgentError);
        driven.and_then(|outcome| teardown.map(|()| outcome))
    }

    /// The init + drive loop of [`drive_one`](Self::drive_one), split out so
    /// teardown can run no matter how it ends.
    async fn drive_inner(
        inference: &I,
        agent: &mut A,
    ) -> Result<Outcome, ReactorError<I, S, A>> {
        agent.on_init().await.map_err(ReactorError::AgentError)?;
        let mut stalls = 0usize;
        loop {
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
                Control::Done(outcome) => break Ok(outcome),
                Control::Continue => stalls = 0,
                Control::Stalled => {
                    stalls += 1;
                    if stalls >= Self::MAX_STALLS {
                        break Ok(Outcome::Failed);
                    }
                }
            }
        }
    }

    /// Agent-major path: drive each agent to completion, up to
    /// [`max_concurrency`](Inference::max_concurrency) in flight. One agent's
    /// failure never aborts the cohort (persistence happens after).
    async fn run_agent_major(
        inference: &I,
        agents: Vec<A>,
    ) -> Vec<Persist<I, S, A>> {
        let limit = inference.max_concurrency().get();
        futures::stream::iter(agents)
            .map(|mut agent| async move {
                let result = Self::drive_one(inference, &mut agent).await;
                (agent, result)
            })
            .buffer_unordered(limit)
            .collect()
            .await
    }

    /// Round-major path: drive the whole cohort in lockstep, collecting each live
    /// agent's next prompt into one [`infer_batch`](Inference::infer_batch) per
    /// round and scattering the responses back. Only inference is batched;
    /// per-agent lifecycle runs sequentially.
    async fn run_round_major(
        inference: &I,
        mut agents: Vec<A>,
    ) -> Vec<Persist<I, S, A>> {
        // All keyed by the agent's index in `agents` (which stays full-length).
        let mut errors: HashMap<usize, ReactorError<I, S, A>> = HashMap::new();
        // Agents that reached an outcome (Done, or stall-capped to Failed).
        let mut finished: HashMap<usize, Outcome> = HashMap::new();
        // Consecutive `Stalled` rounds, and consecutive per-item failures.
        let mut stalls: HashMap<usize, usize> = HashMap::new();
        let mut item_failures: HashMap<usize, usize> = HashMap::new();

        for (i, agent) in agents.iter_mut().enumerate() {
            if let Err(e) = agent.on_init().await {
                errors.insert(i, ReactorError::AgentError(e));
            }
        }

        // The live cohort for a round, reused across rounds.
        let mut live: Vec<usize> = Vec::new();
        loop {
            live.clear();
            live.extend((0..agents.len()).filter(|i| {
                !errors.contains_key(i) && !finished.contains_key(i)
            }));
            if live.is_empty() {
                break;
            }

            for &i in &live {
                if let Err(e) = agents[i].on_turn().await {
                    errors.insert(i, ReactorError::AgentError(e));
                }
            }
            live.retain(|i| !errors.contains_key(i));
            if live.is_empty() {
                continue;
            }

            // Collect prompts and submit them as one batch. A lazy `iter().map`
            // can't be used: `async_trait` boxes `infer_batch` as a `Send`
            // future, and an iterator borrowing `agents` is only `Send` if
            // `A: Sync` — which no agent is. The `Vec<&Prompt>` sidesteps that.
            let resps = {
                let prompts: Vec<&Prompt> =
                    live.iter().map(|&i| agents[i].prompt()).collect();
                match inference.infer_batch(&prompts).await {
                    Ok(resps) => resps,
                    Err(e) => {
                        // Whole submission failed — the transport is dead.
                        // Attribute it to *every* live agent (pre-projected;
                        // sources aren't `Clone`) so collateral of a dead
                        // transport stays distinguishable — and retryable —
                        // per agent.
                        let report = ErrorReport::from(
                            &ReactorError::<I, S, A>::InferenceError(e),
                        );
                        for &i in &live {
                            errors.insert(
                                i,
                                ReactorError::Shared(report.clone()),
                            );
                        }
                        break;
                    }
                }
            };

            // Scatter: `resps` is aligned to `live` by input order.
            for (&i, resp) in live.iter().zip(resps) {
                match resp {
                    Ok(message) => {
                        item_failures.remove(&i);
                        match agents[i].handle(message).await {
                            Err(e) => {
                                errors.insert(i, ReactorError::AgentError(e));
                            }
                            Ok(Control::Done(outcome)) => {
                                finished.insert(i, outcome);
                            }
                            Ok(Control::Continue) => {
                                stalls.remove(&i);
                            }
                            Ok(Control::Stalled) => {
                                let n = stalls.entry(i).or_insert(0);
                                *n += 1;
                                if *n >= Self::MAX_STALLS {
                                    finished.insert(i, Outcome::Failed);
                                }
                            }
                        }
                    }
                    Err(e) if e.is_fatal() => {
                        errors.insert(i, ReactorError::InferenceError(e));
                    }
                    Err(e) => {
                        // Transient per-item failure: leave the agent
                        // un-advanced so it re-batches, but cap the retries.
                        let n = item_failures.entry(i).or_insert(0);
                        *n += 1;
                        if *n >= MAX_BATCH_ITEM_RETRIES {
                            errors.insert(i, ReactorError::InferenceError(e));
                        }
                    }
                }
            }
        }

        // Tear down every agent (best-effort) without clobbering a prior error.
        for (i, agent) in agents.iter_mut().enumerate() {
            if let Err(e) = agent.on_teardown().await {
                errors.entry(i).or_insert(ReactorError::AgentError(e));
            }
        }

        // Flatten to `Persist`: errored → Err; finished → Ok(outcome).
        // `persist_all` buckets and serializes from here.
        agents
            .into_iter()
            .enumerate()
            .map(|(i, agent)| {
                let result = match errors.remove(&i) {
                    Some(e) => Err(e),
                    None => Ok(finished.remove(&i).unwrap_or(Outcome::Failed)),
                };
                (agent, result)
            })
            .collect()
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
            // `save_all_raw`, not `save_all`: per-agent serialize failures are
            // handled above (`save_all` aborts the whole batch — see its FIXME).
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

/// Bulk-load [`Agent`]s, cloning `context` into each construction (see
/// [`Agent::Context`]). A Deserialize failure will abort the entire batch
pub async fn load_agents<S: Storage, A: Agent>(
    storage: &S,
    context: A::Context,
    ids: impl ExactSizeIterator<Item = AgentId> + Send,
) -> Result<(Vec<A>, Vec<(AgentId, A::Error)>), S::Error> {
    let raw = storage.load_all::<_, A>(ids).await?;
    let mut agents = Vec::with_capacity(raw.len());
    let mut failures = Vec::new();
    for (id, result) in raw {
        match result {
            Ok(state) => match A::new(id, state, context.clone()) {
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
        // Already projected — pass it through.
        if let ReactorError::Shared(report) = e {
            return report.clone();
        }
        ErrorReport {
            kind: e.kind(),
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
    /// Snapshots of [`Agent`]s the endpoint couldn't satisfy (see [`negotiate`]),
    /// for the caller to re-route.
    #[serde(default)]
    pub rejected: BTreeMap<AgentId, serde_json::Value>,
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
        self.rejected.extend(rhs.rejected);
    }
}

/// A whole-[`run`](Run::run) failure, type-erased to cross the `dyn Run`
/// boundary. The [`RetryAfter`] classification is evaluated before the
/// erasure, while the concrete error types are still known.
#[derive(Debug, thiserror::Error)]
#[error("{kind:?} error: {inner}")]
pub struct RunError {
    pub kind: ErrorKind,
    /// `None` = fatal; `Some(d)` = retry after `d`. See [`RetryAfter`].
    pub retry_after: Option<std::time::Duration>,
    /// The erased source.
    pub inner: anyhow::Error,
}

impl RetryAfter for RunError {
    fn retry_after(&self) -> Option<std::time::Duration> {
        self.retry_after
    }
}

impl<I, S, A> From<ReactorError<I, S, A>> for RunError
where
    I: Inference,
    S: Storage,
    A: Agent,
{
    fn from(value: ReactorError<I, S, A>) -> Self {
        let kind = value.kind();
        let retry_after = value.retry_after();
        let inner = match value {
            ReactorError::InferenceError(e) => anyhow::Error::new(e),
            ReactorError::AgentError(e) => anyhow::Error::new(e),
            ReactorError::StorageError(e) => anyhow::Error::new(e),
            ReactorError::Shared(report) => anyhow::anyhow!(report.message),
        };
        RunError {
            kind,
            retry_after,
            inner,
        }
    }
}

#[async_trait::async_trait]
pub trait Run: Send {
    /// Return the [`ReactorId`] associated with the [`Run`]
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
        // Probe the endpoint before taking the cohort: a failed probe (e.g. a
        // transient 429) fails the run but must leave the agents seated in
        // `self.agents` so the caller can retry — some may exist nowhere else.
        let offered = self
            .inference
            .models()
            .await
            .map_err(ReactorError::<I, S, A>::InferenceError)?;

        // Negotiate each agent's requested capabilities against what the endpoint
        // offers, partitioning the cohort into the two run-paths (or rejecting).
        // Admission completes the handshake: the agent receives the negotiated
        // model and the endpoint's quirks before any inference.
        let quirks = self.inference.quirks();
        let agents = std::mem::take(&mut self.agents);
        let mut batch: Vec<A> = Vec::new();
        let mut sequential: Vec<A> = Vec::new();
        let mut rejected: Vec<A> = Vec::new();
        for mut agent in agents {
            match negotiate(&offered, &agent.model()) {
                Admission::Batch(model) => {
                    debug_assert_eq!(
                        agent.prompt().model.name(),
                        model.id.name(),
                        "prompt model diverges from the negotiated model"
                    );
                    agent.on_admit(model, &quirks);
                    batch.push(agent);
                }
                Admission::Sequential(model) => {
                    debug_assert_eq!(
                        agent.prompt().model.name(),
                        model.id.name(),
                        "prompt model diverges from the negotiated model"
                    );
                    agent.on_admit(model, &quirks);
                    sequential.push(agent);
                }
                Admission::Rejected => rejected.push(agent),
            }
        }

        // Snapshot rejected agents so the caller can re-route them — they cross
        // the `dyn Run` erasure as data, like `unsaved`.
        for agent in rejected {
            match serde_json::to_value(agent.state()) {
                Ok(value) => {
                    self.rejected.insert(agent.id(), value);
                }
                // Never silent: as on the persist path, a snapshot that can't
                // serialize is a storage error against the agent's id.
                Err(e) => {
                    self.errors.insert(
                        agent.id(),
                        ReactorError::StorageError(S::Error::from(e)),
                    );
                }
            }
        }

        // Run both paths concurrently; each only borrows `&self.inference`. One
        // agent's failure never aborts the cohort — persistence happens after.
        let inference = &self.inference;
        let (mut to_persist, seq_persist) = futures::join!(
            Self::run_round_major(inference, batch),
            Self::run_agent_major(inference, sequential),
        );
        to_persist.extend(seq_persist);

        // Persist all snapshots in one bulk save, then bucket.
        self.persist_all(to_persist).await;
        Ok(self.report())
    }
}
