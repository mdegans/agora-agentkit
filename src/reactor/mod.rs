//! A [`Reactor`] runs [`Agent`]s to completion by scheduling agentic tasks and
//! submitting prompts to [`Inference`] engines. Each agent persists in a
//! [`Storage`] implementation (disk, sql, etc) as its session ends.
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
pub use agent::cache;
#[cfg(feature = "seed")]
pub use agent::seed;
pub use agent::{Agent, Control, Outcome, State, default_handle};

mod backend;
pub use backend::{AgentNotFound, Inference, SaveError, Storage};

#[cfg(feature = "fs-storage")]
pub mod storage;
#[cfg(feature = "fs-storage")]
pub use storage::FsStorage;

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

// Anthropic error classification lives here rather than in
// `reactor::anthropic` because that module is gated on the `client`
// feature (misanthropic's HTTP client) while `crate::retry` — which
// delegates to these impls so the two cannot disagree — is gated on
// `misanthropic`. The impls need only the error *types*, which are
// available either way.
use std::time::Duration;

const COURTESY_BACKOFF: Duration = Duration::from_secs(10);

/// Is this HTTP status worth coming back for? 429 and 5xx are the
/// server telling us to wait; every other 4xx is our own request being
/// wrong, and repeating it verbatim cannot fix it.
fn status_is_transient(status: u16) -> bool {
    status == 429 || status >= 500
}

// Forward the `Retry-After` Anthropic sends on 429/529, and treat the
// transient *classes* as retryable even when no header arrives — the
// reactor has no other retry mechanism, so anything left `None` here is
// a per-agent failure with no reschedule and, on the batch path, no
// re-batch. That is how a single edge 503 failed 27 of 30 agents on
// 2026-08-21: `NonJsonResponse` was fatal, so nobody re-batched.
//
// Deliberately *not* blanket-retryable: a malformed request, a bad key,
// or an unparseable body will read the same on the tenth attempt.
// Implemented on `AnthropicError` too, not only the outer `Error`: an
// `anyhow` chain can surface either, and classifying them in two places is
// how they drift apart.
impl RetryAfter for misanthropic::client::AnthropicError {
    fn retry_after(&self) -> Option<Duration> {
        use misanthropic::client::AnthropicError as E;

        // The server's own hint wins wherever it sent one.
        self.retry_after().or(match self {
            E::Overloaded { .. }
            | E::RateLimit { .. }
            | E::API { .. }
            | E::Timeout { .. } => Some(COURTESY_BACKOFF),
            // Unknown shapes: 5xx is the server's problem, 4xx ours.
            E::Unknown { code, .. } => code
                .filter(|c| status_is_transient(c.get()))
                .map(|_| COURTESY_BACKOFF),
            E::InvalidRequest { .. }
            | E::Authentication { .. }
            | E::Billing { .. }
            | E::Permission { .. }
            | E::NotFound { .. }
            | E::RequestTooLarge { .. } => None,
        })
    }
}

impl RetryAfter for misanthropic::client::Error {
    fn retry_after(&self) -> Option<Duration> {
        use misanthropic::client::Error;

        match self {
            Error::Anthropic(e) => RetryAfter::retry_after(e),
            // An edge or proxy answered instead of the API — HTML, a
            // plaintext gateway notice, a challenge page. The status is
            // still meaningful, so classify on it rather than guessing.
            Error::NonJsonResponse { status, .. } => {
                status_is_transient(*status).then_some(COURTESY_BACKOFF)
            }
            // Connection reset, DNS blip, TLS hiccup.
            Error::HTTP(_) => Some(COURTESY_BACKOFF),
            // Ours, or a server misbehaving in a way retrying won't fix.
            Error::Parse(_) | Error::UnexpectedResponse { .. } => None,
        }
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

/// An [`Agent`] whose session ended, for [`persist_all`](Reactor::persist_all)
struct Persist<I: Inference, S: Storage, A: Agent> {
    agent: A,
    result: Result<Outcome, ReactorError<I, S, A>>,
    /// Already committed by [`settle`](Reactor::settle); `persist_all` skips it
    saved: bool,
}

/// [`Storage`] shared by the two run-paths, so each agent saves as it finishes
type SharedStorage<'a, S> = futures::lock::Mutex<&'a mut S>;

/// How a session ended, for the `session_finished` event
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ending {
    Complete,
    Failed,
    /// Gave up after [`Reactor::MAX_STALLS`] rounds (an [`Outcome::Failed`])
    Stalled,
    Error,
}

impl Ending {
    fn of<E>(result: &Result<Outcome, E>, stalled: bool) -> Self {
        match result {
            Err(_) => Ending::Error,
            Ok(_) if stalled => Ending::Stalled,
            Ok(Outcome::Complete) => Ending::Complete,
            Ok(Outcome::Failed) => Ending::Failed,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Ending::Complete => "complete",
            Ending::Failed => "failed",
            Ending::Stalled => "stalled",
            Ending::Error => "error",
        }
    }
}

/// When a session started, for the `session_finished` event
#[derive(Debug, Clone, Copy)]
struct Started {
    at: chrono::DateTime<chrono::Utc>,
    instant: std::time::Instant,
}

impl Started {
    fn now() -> Self {
        Self {
            at: chrono::Utc::now(),
            instant: std::time::Instant::now(),
        }
    }
}

/// How many consecutive batch-item failures (canceled / expired / errored
/// results, which never reach the agent's `handle` and so never charge its own
/// budget) one agent may accrue on the round-major path before the reactor gives
/// up on it. Without this, an item that always errors would re-batch forever.
const MAX_BATCH_ITEM_RETRIES: usize = 3;

/// How many times one turn's inference may be retried on the agent-major
/// path when the error advertises a wait ([`RetryAfter`]) before the agent
/// fails. Waits scale linearly with the attempt, so a 10s hint costs at most
/// 10+20+30+40+50s before giving up.
pub(crate) const MAX_INFER_RETRIES: u32 = 5;

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
    ///
    /// Returns whether the drive ended on the stall cap, alongside the result.
    async fn drive_one(
        inference: &I,
        agent: &mut A,
    ) -> (Result<Outcome, ReactorError<I, S, A>>, bool) {
        let driven = Self::drive_inner(inference, agent).await;
        let stalled = matches!(driven, Ok(None));
        let driven = driven.map(|o| o.unwrap_or(Outcome::Failed));
        let teardown =
            agent.on_teardown().await.map_err(ReactorError::AgentError);
        (
            driven.and_then(|outcome| teardown.map(|()| outcome)),
            stalled,
        )
    }

    /// Save one finished agent's state right away, so a crash later in a long
    /// run loses nothing already done, and emit its `session_finished` event.
    /// Returns whether the save committed; if not,
    /// [`persist_all`](Self::persist_all) tries again and reports.
    async fn settle(
        storage: &SharedStorage<'_, S>,
        // `&mut` only because `&A` isn't `Send` across the save's await.
        agent: &mut A,
        result: &Result<Outcome, ReactorError<I, S, A>>,
        stalled: bool,
        started: Started,
    ) -> bool {
        let id = agent.id();
        let saved = match serde_json::to_value(agent.state()) {
            Ok(value) => match storage.lock().await.save_raw(id, value).await {
                Ok(()) => true,
                Err(e) => {
                    tracing::warn!(
                        agent_id = %id,
                        error = %e,
                        "session save failed; retrying at end of run"
                    );
                    false
                }
            },
            // `persist_all` serializes again and records the error.
            Err(_) => false,
        };
        let ending = Ending::of(result, stalled);
        let model = agent.prompt().model.to_string();
        let started_at = started.at.to_rfc3339();
        let duration_secs = started.instant.elapsed().as_secs_f64();
        // Anything short of complete is an ERROR the moment it happens,
        // with its cause, rather than only a line in the end-of-run report
        // (agora-agents#171: 19 sessions of a sweep died that way unseen).
        let error = match (ending, result) {
            (Ending::Complete, _) => None,
            (_, Err(e)) => Some(e.to_string()),
            (Ending::Stalled, _) => Some(format!(
                "no successful tool call in {} rounds",
                Self::MAX_STALLS
            )),
            _ => Some("the agent ended its session as failed".to_owned()),
        };
        match error {
            None => tracing::info!(
                event_type = "session_finished",
                agent_id = %id,
                model,
                outcome = ending.as_str(),
                started_at,
                duration_secs,
                saved,
                "session finished"
            ),
            Some(error) => tracing::error!(
                event_type = "session_finished",
                agent_id = %id,
                model,
                outcome = ending.as_str(),
                started_at,
                duration_secs,
                saved,
                error,
                "session finished without completing"
            ),
        }
        saved
    }

    /// The init + drive loop of [`drive_one`](Self::drive_one), split out so
    /// teardown can run no matter how it ends. `None` is the stall cap.
    async fn drive_inner(
        inference: &I,
        agent: &mut A,
    ) -> Result<Option<Outcome>, ReactorError<I, S, A>> {
        agent.on_init().await.map_err(ReactorError::AgentError)?;
        let mut stalls = 0usize;
        loop {
            agent.on_turn().await.map_err(ReactorError::AgentError)?;
            // Retry inference while the error advertises a wait (a 429/529
            // with — or courtesy-defaulted to — a backoff), scaling the wait
            // linearly per attempt. Fatal errors and exhausted budgets
            // surface immediately.
            let mut attempt: u32 = 0;
            let response = loop {
                match inference.infer(agent.prompt()).await {
                    Ok(response) => break response,
                    Err(e) => match e.retry_after() {
                        Some(wait) if attempt < MAX_INFER_RETRIES => {
                            attempt += 1;
                            let wait = wait * attempt;
                            tracing::warn!(
                                agent_id = %agent.id(),
                                attempt,
                                wait_secs = wait.as_secs(),
                                error = %e,
                                "retryable inference error"
                            );
                            tokio::time::sleep(wait).await;
                        }
                        _ => {
                            return Err(ReactorError::InferenceError(e));
                        }
                    },
                }
            };
            log_usage(agent.id(), &response, agent.prompt());
            let model = response.model.clone();
            match agent
                .handle(response)
                .await
                .map_err(ReactorError::AgentError)?
            {
                Control::Done(outcome) => break Ok(Some(outcome)),
                Control::Continue => stalls = 0,
                Control::Stalled => {
                    stalls += 1;
                    if stalls >= Self::MAX_STALLS {
                        log_stalled(agent.id(), &model, stalls);
                        break Ok(None);
                    }
                }
            }
        }
    }

    /// Agent-major path: drive each agent to completion, up to
    /// [`max_concurrency`](Inference::max_concurrency) in flight. One agent's
    /// failure never aborts the cohort; each agent is saved as it finishes.
    async fn run_agent_major(
        inference: &I,
        storage: &SharedStorage<'_, S>,
        agents: Vec<A>,
    ) -> Vec<Persist<I, S, A>> {
        let limit = inference.max_concurrency().get();
        futures::stream::iter(agents)
            .map(|mut agent| async move {
                let started = Started::now();
                let (result, stalled) =
                    Self::drive_one(inference, &mut agent).await;
                let saved = Self::settle(
                    storage, &mut agent, &result, stalled, started,
                )
                .await;
                Persist {
                    agent,
                    result,
                    saved,
                }
            })
            .buffer_unordered(limit)
            .collect()
            .await
    }

    /// Round-major path: drive the whole cohort in lockstep, collecting each live
    /// agent's next prompt into one [`infer_batch`](Inference::infer_batch) per
    /// round and scattering the responses back. Only inference is batched;
    /// per-agent lifecycle runs sequentially. Each agent is torn down and saved
    /// in the round it leaves the cohort.
    async fn run_round_major(
        inference: &I,
        storage: &SharedStorage<'_, S>,
        mut agents: Vec<A>,
    ) -> Vec<Persist<I, S, A>> {
        let started = Started::now();
        // All keyed by the agent's index in `agents` (which stays full-length).
        let mut errors: HashMap<usize, ReactorError<I, S, A>> = HashMap::new();
        // Agents that reached an outcome (Done, or stall-capped to Failed).
        let mut finished: HashMap<usize, Outcome> = HashMap::new();
        // Of those, the stall-capped ones.
        let mut stall_capped: BTreeSet<usize> = BTreeSet::new();
        // Agents torn down and saved (or save attempted), with the save result.
        let mut settled: HashMap<usize, bool> = HashMap::new();
        // Consecutive `Stalled` rounds, and consecutive per-item failures.
        let mut stalls: HashMap<usize, usize> = HashMap::new();
        let mut item_failures: HashMap<usize, usize> = HashMap::new();

        for (i, agent) in agents.iter_mut().enumerate() {
            if let Err(e) = agent.on_init().await {
                errors.insert(i, ReactorError::AgentError(e));
            }
        }

        // Prime the shared cache prefix once per distinct model, so the
        // very first batch reads it instead of writing it N times. Sent as
        // its own (single-submission) batch: batch prefill bills at half
        // price, and the docs' recommended batch-caching pattern is exactly
        // this — one shared-prefix request, then the rest once it lands.
        // Best-effort: a failed ping costs nothing — round 1 then writes
        // the prefix exactly as it would have without priming.
        // Collected first: holding `&A` across the await would require
        // `A: Sync` (same async_trait constraint as the batch prompt
        // collection below).
        let primes: Vec<Prompt> = {
            let mut primed: BTreeSet<String> = BTreeSet::new();
            agents
                .iter()
                .enumerate()
                .filter(|(i, _)| !errors.contains_key(i))
                .filter_map(|(_, agent)| agent.prime_prompt())
                .filter(|p| primed.insert(p.model.name().to_string()))
                .collect()
        };
        if !primes.is_empty() {
            let prompts: Vec<&Prompt> = primes.iter().collect();
            match inference.infer_batch(&prompts).await {
                Ok(_) => tracing::info!(models = primes.len(), "cache primed"),
                Err(e) => tracing::warn!(
                    error = %e,
                    "cache prime failed"
                ),
            }
        }

        // The live cohort for a round, reused across rounds.
        let mut live: Vec<usize> = Vec::new();
        loop {
            Self::settle_round_major(
                storage,
                &mut agents,
                &mut errors,
                &finished,
                &stall_capped,
                &mut settled,
                started,
            )
            .await;

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
                        log_usage(agents[i].id(), &message, agents[i].prompt());
                        let model = message.model.clone();
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
                                    log_stalled(agents[i].id(), &model, *n);
                                    finished.insert(i, Outcome::Failed);
                                    stall_capped.insert(i);
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

        // Whoever the loop left (a dead transport breaks out mid-round).
        Self::settle_round_major(
            storage,
            &mut agents,
            &mut errors,
            &finished,
            &stall_capped,
            &mut settled,
            started,
        )
        .await;

        // Flatten to `Persist`: errored → Err; finished → Ok(outcome).
        // `persist_all` buckets, and saves whatever didn't commit, from here.
        agents
            .into_iter()
            .enumerate()
            .map(|(i, agent)| Persist {
                result: match errors.remove(&i) {
                    Some(e) => Err(e),
                    None => Ok(finished.remove(&i).unwrap_or(Outcome::Failed)),
                },
                saved: settled.get(&i).copied().unwrap_or(false),
                agent,
            })
            .collect()
    }

    /// Tear down (best-effort, without clobbering a prior error) and
    /// [`settle`](Self::settle) every round-major agent that has left the
    /// cohort since the last call.
    async fn settle_round_major(
        storage: &SharedStorage<'_, S>,
        agents: &mut [A],
        errors: &mut HashMap<usize, ReactorError<I, S, A>>,
        finished: &HashMap<usize, Outcome>,
        stall_capped: &BTreeSet<usize>,
        settled: &mut HashMap<usize, bool>,
        started: Started,
    ) {
        for (i, agent) in agents.iter_mut().enumerate() {
            if settled.contains_key(&i)
                || !(errors.contains_key(&i) || finished.contains_key(&i))
            {
                continue;
            }
            if let Err(e) = agent.on_teardown().await {
                errors.entry(i).or_insert(ReactorError::AgentError(e));
            }
            let result = match errors.get(&i) {
                // Only the variant matters to `settle`; the error stays put.
                Some(e) => Err(ReactorError::Shared(ErrorReport::from(e))),
                None => Ok(finished[&i]),
            };
            let saved = Self::settle(
                storage,
                agent,
                &result,
                stall_capped.contains(&i),
                started,
            )
            .await;
            settled.insert(i, saved);
        }
    }

    /// Bulk save whatever [`settle`](Self::settle) didn't, then calculate done,
    /// failed, etc. for a [`Report`]. Of particular interest are the
    /// [`unsaved`](Self::unsaved).
    async fn persist_all(&mut self, agent_results: Vec<Persist<I, S, A>>) {
        // Serialize each snapshot once. A serialize failure is itself a storage
        // error — that agent can be neither persisted nor recovered.
        let mut values: Vec<(AgentId, serde_json::Value)> = Vec::new();
        let mut saved: BTreeSet<AgentId> = agent_results
            .iter()
            .filter(|p| p.saved)
            .map(|p| p.agent.id())
            .collect();
        for Persist { agent, .. } in agent_results.iter().filter(|p| !p.saved) {
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
        let mut save_err = None;
        if !values.is_empty() {
            // `save_all_raw`, not `save_all`: per-agent serialize failures are
            // handled above (`save_all` aborts the whole batch — see its FIXME).
            match self.storage.save_all_raw(values.clone().into_iter()).await {
                Ok(()) => saved.extend(attempted.iter().copied()),
                Err(SaveError { saved: some, inner }) => {
                    saved.extend(some);
                    save_err = Some(inner);
                }
            }
        }

        // Keep the only in-memory copy of every attempted-but-uncommitted snapshot.
        for (id, value) in values {
            if !saved.contains(&id) {
                self.unsaved.insert(id, value);
            }
        }

        for Persist { agent, result, .. } in agent_results {
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

        // Run both paths concurrently, sharing the inference and the storage.
        // One agent's failure never aborts the cohort, and each agent is saved
        // as soon as its session ends.
        let inference = &self.inference;
        let storage = futures::lock::Mutex::new(&mut self.storage);
        let (mut to_persist, seq_persist) = futures::join!(
            Self::run_round_major(inference, &storage, batch),
            Self::run_agent_major(inference, &storage, sequential),
        );
        drop(storage);
        to_persist.extend(seq_persist);

        // Save whatever didn't commit as it finished, then bucket.
        self.persist_all(to_persist).await;
        Ok(self.report())
    }
}

/// The last message of `prompt`, cut to its final ~2000 bytes: what the
/// model was answering when it refused.
fn prompt_tail(prompt: &Prompt) -> String {
    const CAP: usize = 2000;
    let Some(last) = prompt.messages.last() else {
        return String::new();
    };
    let text = last.to_string();
    if text.len() <= CAP {
        return text;
    }
    let mut start = text.len() - CAP;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!("[…] {}", &text[start..])
}

/// One `inference_usage` event per response, at info: token counts and
/// cache hits for every agent on every transport. Until 0.35 only blallama's
/// own log carried these.
///
/// A [`Refusal`] is also an `inference_refusal` error carrying the trigger:
/// repeated refusals can get an API account banned, and the operator needs
/// to know what caused one.
///
/// [`Refusal`]: misanthropic::response::StopReason::Refusal
fn log_usage(
    agent_id: AgentId,
    response: &misanthropic::response::Message,
    prompt: &Prompt,
) {
    if matches!(
        response.stop_reason,
        Some(misanthropic::response::StopReason::Refusal)
    ) {
        let details = response.stop_details.as_deref();
        tracing::error!(
            event_type = "inference_refusal",
            agent_id = %agent_id,
            model = %response.model,
            response_id = %response.id,
            category = details.and_then(|d| d.category.as_deref()),
            explanation = details.and_then(|d| d.explanation.as_deref()),
            prompt_tail = %prompt_tail(prompt),
            "model refused"
        );
    }
    let usage = &response.usage;
    tracing::info!(
        event_type = "inference_usage",
        agent_id = %agent_id,
        model = %response.model,
        stop_reason = stop_reason_str(response),
        input_tokens = usage.input_tokens,
        cache_read_input_tokens = usage.cache_read_input_tokens.unwrap_or(0),
        cache_creation_input_tokens =
            usage.cache_creation_input_tokens.unwrap_or(0),
        output_tokens = usage.output_tokens,
        "inference usage"
    );
}

/// The wire name of the response's stop reason (`tool_use`, `end_turn`, …),
/// or `none`.
fn stop_reason_str(response: &misanthropic::response::Message) -> String {
    serde_json::to_value(response.stop_reason)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "none".to_owned())
}

/// A `session_stalled` warning: the agent went [`MAX_STALLS`] rounds without
/// a successful tool call and is given up on, before any closing phase —
/// so no memory is written. Silent until 2026-09-24, when mangled ids
/// stalled ~37% of a night's sessions unnoticed.
///
/// [`MAX_STALLS`]: Reactor::MAX_STALLS
fn log_stalled(
    agent_id: AgentId,
    model: &misanthropic::model::Model,
    stalls: usize,
) {
    tracing::warn!(
        event_type = "session_stalled",
        agent_id = %agent_id,
        model = %model,
        stalls,
        "session abandoned: no successful tool call in MAX_STALLS rounds"
    );
}
