//! Run many [`Reactor`]s with an [`Orchestrator`] and merge their [`Report`]s
//! into an [`OrchestratorReport`] which includes all the necessary state to
//! resume where [`Agent`]s left off.

use std::collections::BTreeMap;

use crate::ids::ReactorId;

#[allow(unused_imports)] // for docs
use super::{Agent, Reactor, Report, Run, RunError};

// FIXME(mdegans): the orchestrator does not yet route agents across reactors.
// Intra-reactor capability negotiation now lives in `Reactor::run` (it
// partitions its own cohort into the batch/sequential paths and rejects agents
// whose requested `ModelInfo` the endpoint can't satisfy, surfaced per reactor
// in `Report::rejected`). Cross-reactor routing — picking the right endpoint for
// an agent — and returning *live* rejected agents (not just snapshots) for
// re-routing is the next step, likely an `Orchestratable: Run` supertrait
// exposing each reactor's offered `Models`.

/// [`Report`]s from every [`Reactor`](crate::reactor::Reactor)
///
/// Deliberately a plain data bag: presentation and policy (pretty-printing,
/// re-routing, retry decisions) belong to the caller. The only conveniences
/// here are the ones a caller *acts* on: iteration, and [`rejected`] — the
/// cross-reactor view of agents that need re-routing.
///
/// [`rejected`]: Self::rejected
#[derive(Debug, Default)]
pub struct OrchestratorReport {
    /// Per-[`Reactor`] [`Report`]s or [`RunError`] if the [`run`](Run::run)
    /// failed entirely
    pub report: BTreeMap<ReactorId, Result<Report, RunError>>,
}

impl OrchestratorReport {
    /// Every agent some reactor rejected (see [`Report::rejected`]),
    /// flattened across reactors: `(which reactor, agent, state snapshot)`.
    /// These agents ran nowhere — the caller re-routes them (an endpoint
    /// that satisfies their requested model) or persists the snapshots.
    pub fn rejected(
        &self,
    ) -> impl Iterator<Item = (ReactorId, crate::ids::AgentId, &serde_json::Value)>
    {
        self.report.iter().flat_map(|(&reactor, result)| {
            result
                .iter()
                .flat_map(|report| &report.rejected)
                .map(move |(&agent, state)| (reactor, agent, state))
        })
    }
}

impl IntoIterator for OrchestratorReport {
    type Item = (ReactorId, Result<Report, RunError>);
    type IntoIter = std::collections::btree_map::IntoIter<
        ReactorId,
        Result<Report, RunError>,
    >;

    fn into_iter(self) -> Self::IntoIter {
        self.report.into_iter()
    }
}

impl<'a> IntoIterator for &'a OrchestratorReport {
    type Item = (&'a ReactorId, &'a Result<Report, RunError>);
    type IntoIter = std::collections::btree_map::Iter<
        'a,
        ReactorId,
        Result<Report, RunError>,
    >;

    fn into_iter(self) -> Self::IntoIter {
        self.report.iter()
    }
}
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
