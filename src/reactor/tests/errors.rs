//! Error containment (one agent's failure never aborts the cohort) and per-item
//! retry classification on the round-major path.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

/// C — error containment (sequential): one agent's drive error doesn't abort
/// the cohort; the others complete and the failure is recorded.
#[tokio::test]
async fn sequential_contains_one_failure() {
    let bad = agent(Behavior::ErrHandle, 1);
    let bad_id = bad.id();
    let agents = vec![
        agent(Behavior::Complete, 1),
        bad,
        agent(Behavior::Complete, 1),
    ];
    // Three sequential agents, one `infer` each (the bad one errors in `handle`
    // after its response lands).
    let mut reactor: Reactor<_, _, TestAgent> =
        Reactor::new(MockInference::end_turns(3), MemStore::default(), agents);
    let report = reactor.run().await.unwrap();

    assert_eq!(report.done, 2);
    assert_eq!(report.failed, 1);
    assert!(report.errors.contains_key(&bad_id));
}

/// C (batch) — same containment guarantee in the round-major reactor.
#[tokio::test]
async fn batch_contains_one_failure() {
    let bad = batch_agent(Behavior::ErrHandle, 1);
    let bad_id = bad.id();
    let agents = vec![
        batch_agent(Behavior::Complete, 1),
        bad,
        batch_agent(Behavior::Complete, 1),
    ];
    let mut reactor: Reactor<_, _, TestAgent> =
        Reactor::new(MockInference::default(), MemStore::default(), agents);
    let report = reactor.run().await.unwrap();

    assert_eq!(report.done, 2);
    assert_eq!(report.failed, 1);
    assert!(report.errors.contains_key(&bad_id));
}

/// `on_teardown` runs even when the drive errors — a stateful tool may hold
/// real resources — on BOTH paths, and the drive's own error wins over any
/// teardown outcome.
#[tokio::test]
async fn teardown_runs_on_drive_error_both_paths() {
    let seq = agent(Behavior::ErrHandle, 1);
    let bat = batch_agent(Behavior::ErrHandle, 1);
    let (seq_td, bat_td) = (seq.teardowns.clone(), bat.teardowns.clone());
    let (seq_id, bat_id) = (seq.id(), bat.id());

    let mut reactor: Reactor<_, _, TestAgent> = Reactor::new(
        // One scripted infer for the sequential agent; the batch agent rides
        // the mock's unscripted `infer_batch`.
        MockInference::end_turns(1),
        MemStore::default(),
        vec![seq, bat],
    );
    let report = reactor.run().await.unwrap();

    assert_eq!(report.failed, 2);
    assert_eq!(seq_td.load(Ordering::SeqCst), 1, "agent-major tore down");
    assert_eq!(bat_td.load(Ordering::SeqCst), 1, "round-major tore down");
    assert_eq!(
        report.errors[&seq_id].kind,
        ErrorKind::Agent,
        "drive error wins"
    );
    assert_eq!(report.errors[&bat_id].kind, ErrorKind::Agent);
}

/// A failed `models()` probe fails the run but must not cost the cohort: the
/// agents stay seated in the reactor, so a retry against the recovered
/// endpoint drives them.
#[tokio::test]
async fn failed_models_probe_retains_cohort() {
    let mut reactor: Reactor<_, _, TestAgent> = Reactor::new(
        FlakyModels::failing(1),
        MemStore::default(),
        vec![agent(Behavior::Complete, 1), agent(Behavior::Complete, 1)],
    );

    let err = reactor.run().await.expect_err("first probe fails the run");
    assert!(matches!(err, RunError::InferenceError(_)));

    let report = reactor.run().await.unwrap();
    assert_eq!(report.done, 2, "cohort survived the failed probe");
}

/// A fatal per-item inference error (`retry_after() == None`) fails the agent on
/// the first round instead of burning the whole retry cap.
#[tokio::test]
async fn fatal_item_fails_without_burning_retries() {
    let calls = Arc::new(AtomicUsize::new(0));
    let transport = FailingBatch {
        calls: calls.clone(),
        fatal: true,
    };
    let mut reactor: Reactor<_, _, TestAgent> = Reactor::new(
        transport,
        MemStore::default(),
        vec![batch_agent(Behavior::Complete, 1)],
    );
    let report = reactor.run().await.unwrap();

    assert_eq!(report.failed, 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1, "failed on the first round");
}

/// A transient per-item error (`retry_after() == Some(..)`) re-batches up to the
/// retry cap before the agent is failed.
#[tokio::test]
async fn transient_item_retries_to_cap() {
    let calls = Arc::new(AtomicUsize::new(0));
    let transport = FailingBatch {
        calls: calls.clone(),
        fatal: false,
    };
    let mut reactor: Reactor<_, _, TestAgent> = Reactor::new(
        transport,
        MemStore::default(),
        vec![batch_agent(Behavior::Complete, 1)],
    );
    let report = reactor.run().await.unwrap();

    assert_eq!(report.failed, 1);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        3,
        "re-batched up to MAX_BATCH_ITEM_RETRIES"
    );
}

/// A rejected agent whose state fails to serialize is not silently dropped:
/// there's nothing to snapshot into `rejected`, so the failure is recorded as
/// a `Storage` error against its id — mirroring the persist path.
#[tokio::test]
async fn rejected_agent_serialize_failure_is_recorded() {
    // Rejected (requests more input context than the offered model serves),
    // with a state that refuses to serialize.
    let mut greedy = agent(Behavior::Complete, 1);
    greedy.model.max_input_tokens = 1;
    greedy.state.poison = Some(());
    let greedy_id = greedy.id();

    let mut reactor: Reactor<_, _, TestAgent> = Reactor::new(
        // Only the admitted agent ever infers.
        MockInference::end_turns(1),
        MemStore::default(),
        vec![agent(Behavior::Complete, 1), greedy],
    );
    let report = reactor.run().await.unwrap();

    assert_eq!(report.done, 1, "the satisfiable agent still ran");
    assert!(report.rejected.is_empty(), "no snapshot could be taken");
    let err = report.errors.get(&greedy_id).expect("never silent");
    assert_eq!(err.kind, ErrorKind::Storage);
}
