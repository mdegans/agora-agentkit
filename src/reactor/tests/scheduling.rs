//! Scheduling contracts: round-major lockstep, the stall cap, `PauseTurn`
//! continuation, the turn-order invariant, and mixed-cohort routing.

use std::sync::atomic::Ordering;

use misanthropic::prompt::message::Role;
use misanthropic::response::StopReason;

use super::*;

/// A — round-major lockstep: one batch per round, sized to the live cohort,
/// shrinking as agents finish (turns 1, 2, 3 → batch sizes 3, 2, 1).
#[tokio::test]
async fn batch_sizes_match_live_cohort_each_round() {
    let sizes = SharedSizes::default();
    let transport = RecordingBatch {
        sizes: sizes.clone(),
    };
    let agents = vec![
        batch_agent(Behavior::Complete, 1),
        batch_agent(Behavior::Complete, 2),
        batch_agent(Behavior::Complete, 3),
    ];
    let mut reactor: Reactor<_, _, TestAgent> =
        Reactor::new(transport, MemStore::default(), agents);
    let report = reactor.run().await.unwrap();

    assert_eq!(report.done, 3);
    assert_eq!(sizes.get(), vec![3, 2, 1], "one batch per round, shrinking");
}

/// B — stall cap: an agent that never progresses is failed after the cap, not
/// looped forever (the test terminating at all is half the assertion). It infers
/// exactly `MAX_STALLS` times before the cap fires — one response too few would
/// panic the mock, one too many would be left unconsumed.
#[tokio::test]
async fn stall_cap_bounds_retry() {
    let mut reactor: Reactor<_, _, TestAgent> = Reactor::new(
        MockInference::end_turns(
            Reactor::<MockInference, MemStore, TestAgent>::MAX_STALLS,
        ),
        MemStore::default(),
        [agent(Behavior::Stall, 0)],
    );
    let report = reactor.run().await.unwrap();

    assert_eq!(report.failed, 1);
    assert_eq!(report.done, 0);
}

/// The `Default`-based `.into()` shortcut collects a `Reactor` from an iterable
/// of agents (the `From` / `FromIterator` impls). Construction only: a `Default`
/// mock scripts no responses, so this can't drive inference.
#[test]
fn reactor_collects_from_agents_via_into() {
    let reactor: Reactor<MockInference, MemStore, TestAgent> =
        [agent(Behavior::Complete, 1), agent(Behavior::Complete, 1)].into();
    let report = reactor.report();
    assert_eq!((report.done, report.failed), (0, 0), "constructed, not run");
}

/// D — a paused turn continues (and is not a stall): scripting PauseTurn then
/// EndTurn, the agent finishes and is not failed.
#[tokio::test]
async fn pause_turn_continues() {
    let inference = MockInference::scripted([
        message(StopReason::PauseTurn),
        message(StopReason::EndTurn),
    ]);
    let mut reactor: Reactor<_, _, TestAgent> = Reactor::new(
        inference,
        MemStore::default(),
        vec![agent(Behavior::Complete, 1)],
    );
    let report = reactor.run().await.unwrap();

    assert_eq!(report.done, 1);
    assert_eq!(report.failed, 0);
}

/// E — turn-order invariant: after `handle`, the prompt ends in a user turn.
#[tokio::test]
async fn handle_keeps_user_tail() {
    let mut a = agent(Behavior::Complete, 2);
    a.handle(message(StopReason::EndTurn)).await.unwrap();
    let last = a.prompt().messages.last().expect("non-empty prompt");
    assert_eq!(last.role, Role::User);
}

/// A mixed cohort in one `Reactor` runs both paths concurrently: the
/// batch-capable agent negotiates onto the round-major path (`infer_batch`), the
/// other onto the agent-major path (`infer`). Both finish.
#[tokio::test]
async fn mixed_cohort_runs_both_paths() {
    let transport = MixedRecorder::default();
    let agents = vec![
        batch_agent(Behavior::Complete, 1),
        agent(Behavior::Complete, 1),
    ];
    let mut reactor: Reactor<_, _, TestAgent> =
        Reactor::new(transport.clone(), MemStore::default(), agents);
    let report = reactor.run().await.unwrap();

    assert_eq!(report.done, 2, "both agents complete");
    assert_eq!(
        transport.batch_sizes.get(),
        vec![1],
        "the batch agent ran one round-major batch of size 1"
    );
    assert_eq!(
        transport.infer_calls.load(Ordering::SeqCst),
        1,
        "the sequential agent made one infer call"
    );
}
