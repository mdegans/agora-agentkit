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

use std::collections::{BTreeMap, BTreeSet, HashMap};

use misanthropic::prompt::Prompt;

use super::RetryAfter;
use super::backend::{BatchInference, SaveError, Storage};
use super::{
    Agent, Control, ErrorReport, MAX_STALLS, Outcome, ReactorError, Report,
    Run, RunError,
};
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
    /// Serialized snapshots of agents whose state didn't persist — populated by
    /// the persist path on a save failure, drained into [`Report::unsaved`].
    unsaved: BTreeMap<AgentId, serde_json::Value>,
}

impl<I: BatchInference<Prompt = Prompt>, S: Storage, A: Agent>
    BatchReactor<I, S, A>
{
    /// Build a batch reactor over already-constructed transports and a cohort.
    pub fn new(
        inference: I,
        storage: S,
        agents: impl IntoIterator<Item = A>,
    ) -> Self {
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

    /// Summarize the run: counts, the flattened error for each agent that
    /// errored, and the snapshots of any agents that didn't persist.
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
}

#[async_trait::async_trait]
impl<I: BatchInference<Prompt = Prompt>, S: Storage, A: Agent> Run
    for BatchReactor<I, S, A>
{
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

        // The live cohort for a round, reused across rounds: cleared and
        // refilled each iteration so its allocation is paid once, not per round.
        let mut live: Vec<usize> = Vec::new();

        // Lockstep rounds: one batch per round over all still-live agents.
        loop {
            live.clear();
            live.extend((0..agents.len()).filter(|i| {
                !errors.contains_key(i) && !finished.contains_key(i)
            }));
            if live.is_empty() {
                break;
            }

            // Refresh per-turn tool context.
            for &i in &live {
                if let Err(e) = agents[i].on_turn().await {
                    errors.insert(i, ReactorError::AgentError(e));
                }
            }
            // Drop any agent that just errored in `on_turn`, in place.
            live.retain(|i| !errors.contains_key(i));
            if live.is_empty() {
                continue;
            }

            // Collect prompts and submit them as one batch. A lazy `iter().map`
            // can't be used here: async_trait boxes `infer_batch` as a `Send`
            // future, and an iterator borrowing `agents` is only `Send` if
            // `A: Sync` — which no agent is (its `ToolBox` holds
            // `Box<dyn Tool + Send>`). The `Vec<&Prompt>` sidesteps that; its
            // immutable borrows end before we hand responses back (mutable).
            let resps = {
                let prompts: Vec<&Prompt> =
                    live.iter().map(|&i| agents[i].prompt()).collect();
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
                    Err(e) if e.retry_after().is_none() => {
                        // Fatal per-item failure (no retry hint): it will never
                        // succeed, so fail the agent now rather than burning the
                        // whole retry cap re-batching a doomed item.
                        errors.insert(i, ReactorError::InferenceError(e));
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

        // Persist all snapshots in one bulk save. Serialize each once; a
        // serialize failure is itself a storage error (that agent can be neither
        // persisted nor recovered).
        let mut values: Vec<(AgentId, serde_json::Value)> =
            Vec::with_capacity(agents.len());
        for (i, agent) in agents.iter().enumerate() {
            match serde_json::to_value(agent.snapshot()) {
                Ok(v) => values.push((agent.id(), v)),
                Err(e) => {
                    errors.entry(i).or_insert(ReactorError::StorageError(
                        S::Error::from(e),
                    ));
                }
            }
        }
        let attempted: BTreeSet<AgentId> =
            values.iter().map(|(id, _)| *id).collect();

        // Persist, learning exactly which ids committed. The clone feeds the
        // save; the original is drained below into `unsaved`.
        let (saved, mut save_err) =
            match self.storage.save_all_raw(values.clone()).await {
                Ok(()) => (attempted.clone(), None),
                Err(SaveError { saved, inner }) => (saved, Some(inner)),
            };
        // Keep the only in-memory copy of every attempted-but-uncommitted snapshot.
        for (id, value) in values {
            if !saved.contains(&id) {
                self.unsaved.insert(id, value);
            }
        }

        // Bucket every agent. `done` = no drive error, finished `Complete`, *and*
        // persisted. The lone store error (not `Clone`) is pinned on one clean
        // agent that didn't commit; `unsaved` is the authoritative un-saved set.
        for (i, agent) in agents.into_iter().enumerate() {
            let id = agent.id();
            let drive_err = errors.remove(&i);
            let done = drive_err.is_none()
                && matches!(finished.get(&i), Some(Outcome::Complete))
                && saved.contains(&id);
            if let Some(e) = drive_err {
                self.errors.insert(id, e);
            } else if attempted.contains(&id)
                && !saved.contains(&id)
                && let Some(e) = save_err.take()
            {
                self.errors.insert(id, ReactorError::StorageError(e));
            }
            if done {
                self.done.insert(id, agent);
            } else {
                self.failed.insert(id, agent);
            }
        }
        Ok(self.report())
    }
}
