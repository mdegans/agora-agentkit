//! Composition layer: run many reactors concurrently and merge their reports.
//!
//! A reactor runs one cohort against one transport (one endpoint). The
//! orchestrator runs a *set* of reactors — typically one per inference endpoint
//! — at the same time and folds their per-agent results together.
//!
//! Routing agents to reactors is partly application-specific: grouping by model
//! needs the app's model→endpoint map and an agent's model (not part of the
//! [`Agent`] trait). What *is* reusable lives here: splitting a set of agents by
//! their declared [`Affinity`] ([`partition_by_affinity`]) and running the
//! resulting reactors together ([`Orchestrator`]). A typical wiring:
//!
//! ```ignore
//! let (messages, batch) = partition_by_affinity(agents);
//! let mut orch = Orchestrator::new();
//! orch.add(Reactor::new(Direct::new(client.clone()), store.clone(), messages));
//! orch.add(BatchReactor::new(Batch::new(client, 50, poll), store, batch));
//! let report = orch.run().await;
//! ```

use super::{Affinity, Agent, Report, Run, RunError};

/// Split agents by their declared [`Affinity`] into `(messages, batch)` groups,
/// ready to hand to a [`Reactor`](super::Reactor) and a
/// [`BatchReactor`](super::BatchReactor) respectively.
pub fn partition_by_affinity<A: Agent>(agents: impl IntoIterator<Item = A>) -> (Vec<A>, Vec<A>) {
    let mut messages = Vec::new();
    let mut batch = Vec::new();
    for agent in agents {
        match agent.affinity() {
            Affinity::Messages => messages.push(agent),
            Affinity::Batch => batch.push(agent),
        }
    }
    (messages, batch)
}

/// The merged outcome of running every reactor.
#[derive(Debug, Default)]
pub struct OrchestratorReport {
    /// Per-agent summary, merged across all reactors.
    pub report: Report,
    /// Reactor-level (catastrophic) failures, rendered. Normally empty — the
    /// reactors capture per-agent errors and otherwise return a report.
    pub reactor_errors: Vec<String>,
}

/// Runs a set of reactors concurrently, one future per reactor, merging their
/// reports. Reactors are type-erased through [`Run`], so a sequential
/// `Reactor<Direct, ..>` and a `BatchReactor<Batch, ..>` can run side by side.
#[derive(Default)]
pub struct Orchestrator {
    reactors: Vec<Box<dyn Run>>,
}

impl Orchestrator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a reactor (one endpoint's cohort) to run alongside the others.
    pub fn add(&mut self, reactor: impl Run + 'static) -> &mut Self {
        self.reactors.push(Box::new(reactor));
        self
    }

    /// Run every reactor concurrently and merge the results.
    pub async fn run(&mut self) -> OrchestratorReport {
        let results = futures::future::join_all(self.reactors.iter_mut().map(|r| r.run())).await;

        let mut out = OrchestratorReport::default();
        for result in results {
            match result {
                Ok(report) => {
                    out.report.done += report.done;
                    out.report.failed += report.failed;
                    out.report.errors.extend(report.errors);
                }
                Err(e) => out.reactor_errors.push(RunError::to_string(&e)),
            }
        }
        out
    }
}
