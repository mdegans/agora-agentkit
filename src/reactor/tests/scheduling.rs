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

/// An agent whose requested model the endpoint can't satisfy is rejected —
/// never run, never downgraded — and its snapshot lands in `Report::rejected`
/// for the caller to re-route.
#[tokio::test]
async fn unsatisfiable_agent_is_rejected_not_run() {
    // Requests more input context than the offered model serves (the offered
    // fixture advertises 0), so no offered model `satisfies` it.
    let mut greedy = agent(Behavior::Complete, 1);
    greedy.model.max_input_tokens = 1;
    let greedy_id = greedy.id();

    let mut reactor: Reactor<_, _, TestAgent> = Reactor::new(
        // Only the admitted agent ever infers.
        MockInference::end_turns(1),
        MemStore::default(),
        vec![agent(Behavior::Complete, 1), greedy],
    );
    let report = reactor.run().await.unwrap();

    assert_eq!(report.done, 1, "the satisfiable agent ran");
    assert_eq!(report.failed, 0, "rejection is not failure");
    let snapshot = report
        .rejected
        .get(&greedy_id)
        .expect("rejected agent's snapshot kept");
    let state: TestState = serde_json::from_value(snapshot.clone()).unwrap();
    assert_eq!(state.behavior, Behavior::Complete, "snapshot round-trips");
}

/// Admission completes the negotiation handshake: every admitted agent —
/// both run-paths — receives the endpoint's quirks via `on_admit` before any
/// inference; a rejected agent never sees it.
#[tokio::test]
async fn admission_hands_quirks_to_admitted_agents_only() {
    let quirks = Quirks {
        tool_choice_not_respected: true,
        ..Default::default()
    };
    let seq = agent(Behavior::Complete, 1);
    let bat = batch_agent(Behavior::Complete, 1);
    // Rejected: requests more input context than the offered model serves.
    let mut greedy = agent(Behavior::Complete, 1);
    greedy.model.max_input_tokens = 1;
    let (seq_admit, bat_admit, greedy_admit) = (
        seq.admitted.clone(),
        bat.admitted.clone(),
        greedy.admitted.clone(),
    );

    let mut reactor: Reactor<_, _, TestAgent> = Reactor::new(
        MockInference {
            // One scripted `infer` for the sequential agent; the batch agent
            // rides the mock's `infer_batch`.
            script: Mutex::new([message(StopReason::EndTurn)].into()),
            quirks,
        },
        MemStore::default(),
        vec![seq, bat, greedy],
    );
    let report = reactor.run().await.unwrap();

    assert_eq!(report.done, 2);
    assert_eq!(*seq_admit.lock().unwrap(), Some(quirks), "sequential path");
    assert_eq!(*bat_admit.lock().unwrap(), Some(quirks), "batch path");
    assert_eq!(
        *greedy_admit.lock().unwrap(),
        None,
        "rejected agents get no handshake"
    );
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

/// The orchestrator-level conveniences: iterating an [`OrchestratorReport`]
/// yields each reactor's result, and [`rejected`] flattens every reactor's
/// rejected snapshots into one re-routable view.
///
/// [`rejected`]: OrchestratorReport::rejected
#[tokio::test]
async fn orchestrator_report_iterates_and_flattens_rejected() {
    use super::super::Orchestrator;

    let mut greedy = agent(Behavior::Complete, 1);
    greedy.model.max_input_tokens = 1;
    let greedy_id = greedy.id();

    let satisfiable: Reactor<_, _, TestAgent> = Reactor::new(
        MockInference::end_turns(1),
        MemStore::default(),
        vec![agent(Behavior::Complete, 1)],
    );
    let mixed: Reactor<_, _, TestAgent> = Reactor::new(
        MockInference::end_turns(1),
        MemStore::default(),
        vec![agent(Behavior::Complete, 1), greedy],
    );
    let mixed_id = Run::id(&mixed);

    let mut orchestrator = Orchestrator::new();
    orchestrator.push(satisfiable);
    orchestrator.push(mixed);
    let report = orchestrator.run().await;

    // Borrowing iteration: one entry per reactor, all Ok here.
    assert_eq!((&report).into_iter().count(), 2);
    assert!((&report).into_iter().all(|(_, r)| r.is_ok()));

    // The flattened rejected view names the reactor, the agent, and a
    // snapshot that round-trips.
    let rejected: Vec<_> = report.rejected().collect();
    let (reactor, agent_id, snapshot) = match rejected.as_slice() {
        [one] => *one,
        other => panic!("expected exactly one rejected agent: {other:?}"),
    };
    assert_eq!(reactor, mixed_id);
    assert_eq!(agent_id, greedy_id);
    let state: TestState = serde_json::from_value(snapshot.clone()).unwrap();
    assert_eq!(state.behavior, Behavior::Complete);

    // Consuming iteration hands the results out by value.
    assert_eq!(report.into_iter().count(), 2);
}

/// The round-major path primes the shared cache prefix once per distinct
/// model — a sequential `infer` before any batch round — so the first
/// batch reads the prefix instead of writing it N times.
#[tokio::test]
async fn batch_primes_shared_prefix_once_per_model() {
    let transport = ModelRecorder::offering([
        model_info_named("model-a", true),
        model_info_named("model-b", true),
    ]);
    let mut agents = vec![
        named_batch_agent("model-a", Behavior::Complete, 1),
        named_batch_agent("model-a", Behavior::Complete, 1),
        named_batch_agent("model-b", Behavior::Complete, 1),
    ];
    for a in &mut agents {
        // The default prime is the tools+system prefix; seat a system so
        // there is one to prime.
        a.prompt = std::mem::take(&mut a.prompt).system("shared prefix");
    }
    let mut reactor: Reactor<_, _, TestAgent> =
        Reactor::new(transport.clone(), MemStore::default(), agents);
    let report = reactor.run().await.unwrap();

    assert_eq!(report.done, 3, "all agents complete");
    assert_eq!(
        transport.seq_models(),
        vec!["model-a", "model-b"],
        "exactly one prime per distinct model, in cohort order"
    );
    assert_eq!(
        transport.round_models(),
        vec![vec!["model-a", "model-a", "model-b"]],
        "one batch round over the whole cohort"
    );
}

/// Without a seated system there is no shared prefix to prime: the batch
/// path makes no sequential calls at all.
#[tokio::test]
async fn no_system_means_no_prime() {
    let transport =
        ModelRecorder::offering([model_info_named("model-a", true)]);
    let agents = vec![named_batch_agent("model-a", Behavior::Complete, 1)];
    let mut reactor: Reactor<_, _, TestAgent> =
        Reactor::new(transport.clone(), MemStore::default(), agents);
    let report = reactor.run().await.unwrap();

    assert_eq!(report.done, 1);
    assert!(
        transport.seq_models().is_empty(),
        "no prime without a system"
    );
}
