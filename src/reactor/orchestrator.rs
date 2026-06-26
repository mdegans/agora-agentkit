//! Run many [`Reactor`]s with an [`Orchestrator`] and merge their [`Report`]s
//! into an [`OrchestratorReport`] which includes all the necessary state to
//! resume where [`Agent`]s left off.

use std::collections::BTreeMap;

use crate::ids::ReactorId;

#[allow(unused_imports)] // for docs
use super::{Affinity, Agent, Reactor, Report, Run, RunError};

// Split agents by their declared [`Affinity`] into `(messages, batch)` groups,
// ready to hand to a [`Reactor`](super::Reactor) and a
// [`BatchReactor`](super::BatchReactor) respectively.
//
// FIXME(mdegans): This is actually unused and we need to partition not just by
// message/batch affinity but also by model, then sort by prefix and for batch
// endpoints setup prompt priming for common prefixes (breakpoints can be used
// to easily find these). A whole session should be devoted to this since it's
// very nuanced and we've screwed it up a whole bunch of times. I'm leaving this
// for the sake of the compiler warning -- to remind us to implement this
// properly. Very likely the orchestrator should be responsible for routing
// agents and this may mean more traits like an Orchestratable supertrait of Run
// with much more affinity information from the reactor parts. Does the
// inference endpoint support batch, for example. I don't like the idea of
// having a separate `BatchReactor`. It leads to too much drift when we can
// route this at runtime in a single Reactor with two run-paths based on what
// the inference engine is configured to do.
// fn partition_by_affinity<A: Agent>(
//     agents: impl IntoIterator<Item = A>,
// ) -> (Vec<A>, Vec<A>) {
//     let mut messages = Vec::new();
//     let mut batch = Vec::new();
//     for agent in agents {
//         match agent.affinity() {
//             Affinity::Messages => messages.push(agent),
//             Affinity::Batch => batch.push(agent),
//         }
//     }
//     (messages, batch)
// }

/// [`Report`]s from every [`Reactor`](crate::reactor::Reactor)
#[derive(Debug, Default)]
pub struct OrchestratorReport {
    /// Per-[`Reactor`] [`Report`]s or [`RunError`] if the [`run`](Run::run)
    /// failed entirely
    pub report: BTreeMap<ReactorId, Result<Report, RunError>>,
}
// TODO: OrchestratorReport impl. Let the downstream use define what this looks
// like.
/// [`run`](Self::run)s a set of [`Reactor`]s concurrently
#[derive(Default)]
pub struct Orchestrator {
    reactors: Vec<Box<dyn Run>>,
}

impl Orchestrator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Extend self with a [`Reactor`]
    pub fn push(&mut self, reactor: impl Run + 'static) -> &mut Self {
        self.reactors.push(Box::new(reactor));
        self
    }

    /// Run every [`Reactor`] concurrently and return a report.
    pub async fn run(&mut self) -> OrchestratorReport {
        let results = futures::future::join_all(
            self.reactors
                .iter_mut()
                .map(|r| async { (r.id(), r.run().await) }),
        )
        .await;

        OrchestratorReport {
            report: results.into_iter().collect(),
        }
    }
}
