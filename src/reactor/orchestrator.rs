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
