//! Round-major (batch) reactor.
//!
//! Where the sequential [`Reactor`](super::Reactor) runs each agent to
//! completion, this drives the whole cohort in lockstep: every round it
//! collects the next prompt from each live agent, submits them as **one batch**
//! via [`BatchInference::infer_batch`], and scatters the responses back. Agents
//! that finish drop out; the cohort shrinks until everyone is done.
//!
//! Only the inference is batched. Per-agent lifecycle (`on_init`/`on_turn`/
//! `handle`/`on_teardown`) runs sequentially here for v1 simplicity — the
//! expensive, latency-bound step is the batched model call, which is what we
//! parallelize. (Parallelizing tool dispatch across agents within a round is a
//! future optimization.)
//!
//! Not yet done: explicit shared-prefix *priming*. The first batch round warms
//! the cache correctly on its own; priming with a separate one-token request
//! would need the agent to expose its cacheable prefix, which the trait does
//! not yet offer.

use std::collections::{BTreeMap, HashMap};

use misanthropic::prompt::Prompt;

use super::backend::{BatchInference, Storage};
use super::{Agent, Control, MAX_STALLS, Outcome, ReactorError, Report, Run, RunError};
use crate::ids::AgentId;

/// How many consecutive *batch-item* failures (canceled / expired / errored
/// results, which never reach the agent's `handle` and so never charge its own
/// budget) a single agent may accrue before the reactor gives up on it. Without
/// this, an item that always errors would re-batch forever.
const MAX_BATCH_ITEM_RETRIES: usize = 3;

/// A round-major reactor over a cohort sharing a [`BatchInference`] transport.
pub struct BatchReactor<I: BatchInference, S: Storage, A: Agent> {
    inference: I,
    storage: S,
    agents: Vec<A>,
    done: BTreeMap<AgentId, A>,
    failed: BTreeMap<AgentId, A>,
    errors: BTreeMap<AgentId, ReactorError<I, S, A>>,
}

impl<I: BatchInference<Prompt = Prompt>, S: Storage, A: Agent> BatchReactor<I, S, A> {
    /// Build a batch reactor over already-constructed transports and a cohort.
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

    /// Summarize the run: how many agents finished, how many failed, and the
    /// rendered error for each that errored.
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
}

#[async_trait::async_trait]
impl<I: BatchInference<Prompt = Prompt>, S: Storage, A: Agent> Run for BatchReactor<I, S, A> {
    async fn run(&mut self) -> Result<Report, RunError> {
        let mut agents = std::mem::take(&mut self.agents);
        // All keyed by the agent's index in `agents` (which stays full-length).
        let mut errors: HashMap<usize, ReactorError<I, S, A>> = HashMap::new();
        // Agents that reached an outcome (Done, or stall-capped to Failed).
        let mut finished: HashMap<usize, Outcome> = HashMap::new();
        // Consecutive `Control::Stalled` rounds, and consecutive transport-level
        // per-item failures, per agent.
        let mut stalls: HashMap<usize, usize> = HashMap::new();
        let mut item_failures: HashMap<usize, usize> = HashMap::new();

        // Install tools + run each agent's on_init.
        for (i, agent) in agents.iter_mut().enumerate() {
            if let Err(e) = agent.on_init().await {
                errors.insert(i, ReactorError::AgentError(e));
            }
        }

        // Lockstep rounds: one batch per round over all still-live agents.
        loop {
            let live: Vec<usize> = (0..agents.len())
                .filter(|i| !errors.contains_key(i) && !finished.contains_key(i))
                .collect();
            if live.is_empty() {
                break;
            }

            // Refresh per-turn tool context.
            for &i in &live {
                if let Err(e) = agents[i].on_turn().await {
                    errors.insert(i, ReactorError::AgentError(e));
                }
            }
            let live: Vec<usize> = live
                .into_iter()
                .filter(|i| !errors.contains_key(i))
                .collect();
            if live.is_empty() {
                continue;
            }

            // Collect prompts and submit them as one batch. The immutable
            // borrows in `prompts` end before we hand responses back (mutable).
            let resps = {
                let prompts: Vec<&Prompt> = live.iter().map(|&i| agents[i].prompt()).collect();
                match self.inference.infer_batch(&prompts).await {
                    Ok(resps) => resps,
                    Err(e) => {
                        // Whole submission failed — the transport is dead. Record
                        // the error against one live agent and stop; the final
                        // pass persists everyone (still-live agents fail out).
                        if let Some(&i) = live.first() {
                            errors.insert(i, ReactorError::InferenceError(e));
                        }
                        break;
                    }
                }
            };

            // Scatter: hand each response back to its agent (`resps` is aligned
            // to `live` by input order).
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
                                if *n >= MAX_STALLS {
                                    finished.insert(i, Outcome::Failed);
                                }
                            }
                        }
                    }
                    Err(e) => {
                        // Transient per-item failure: leave the agent un-advanced
                        // so it re-batches next round, but cap the retries.
                        let n = item_failures.entry(i).or_insert(0);
                        *n += 1;
                        if *n >= MAX_BATCH_ITEM_RETRIES {
                            errors.insert(i, ReactorError::InferenceError(e));
                        }
                    }
                }
            }
        }

        // Teardown every agent (best-effort).
        for (i, agent) in agents.iter_mut().enumerate() {
            if let Err(e) = agent.on_teardown().await {
                errors.entry(i).or_insert(ReactorError::AgentError(e));
            }
        }

        // Persist all snapshots in one (atomic) bulk save.
        let snapshots: Vec<(AgentId, A::State)> = agents
            .iter()
            .map(|agent| (agent.id(), agent.snapshot()))
            .collect();
        let mut save_err = self.storage.save_all(snapshots).await.err();
        let saved_ok = save_err.is_none();

        // Bucket every agent. "Failed" = drive error, bulk-save failure, or it
        // didn't finish with `Outcome::Complete`. The single atomic save error
        // is attributed to one clean agent.
        for (i, agent) in agents.into_iter().enumerate() {
            let id = agent.id();
            let drive_err = errors.remove(&i);
            let failed = drive_err.is_some()
                || !saved_ok
                || !matches!(finished.get(&i), Some(Outcome::Complete));
            if let Some(e) = drive_err {
                self.errors.insert(id, e);
            } else if let Some(e) = save_err.take() {
                self.errors.insert(id, ReactorError::StorageError(e));
            }
            if failed {
                self.failed.insert(id, agent);
            } else {
                self.done.insert(id, agent);
            }
        }
        Ok(self.report())
    }
}
