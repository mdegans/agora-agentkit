//! Persistence: each agent saved as its session ends, a bulk load, and
//! partial-save recovery (unsaved snapshots + error attribution), plus the
//! `Report` serde round-trip it all crosses the `dyn Run` erasure as.

use std::sync::atomic::Ordering;
use std::time::Duration;

use super::*;

/// Each agent is saved as its session ends, so the final bulk save has nothing
/// left to do.
#[tokio::test]
async fn reactor_saves_each_agent_as_it_finishes() {
    let store = BulkStore::default();
    let agents = vec![
        batch_agent(Behavior::Complete, 1),
        batch_agent(Behavior::Complete, 2),
        agent(Behavior::Complete, 1),
    ];
    let mut reactor: Reactor<_, _, TestAgent> =
        Reactor::new(MockInference::end_turns(1), store.clone(), agents);
    let report = reactor.run().await.unwrap();

    assert_eq!(report.done, 3);
    assert_eq!(store.map.lock().unwrap().len(), 3, "all three saved");
    assert_eq!(store.bulk_calls.load(Ordering::SeqCst), 0, "nothing left");
}

/// A run that dies mid-cohort (here a panic in the second agent's session)
/// keeps the first agent's finished session — agent-major path.
#[tokio::test]
async fn finished_agent_survives_a_later_crash_agent_major() {
    let store = MemStore::default();
    let first = agent(Behavior::Complete, 1);
    let first_id = first.id();
    let agents = vec![first, agent(Behavior::Panic, 1)];
    let mut reactor: Reactor<_, _, TestAgent> =
        Reactor::new(MockInference::end_turns(2), store.clone(), agents);

    let crashed = tokio::spawn(async move { reactor.run().await }).await;
    assert!(crashed.unwrap_err().is_panic(), "the run went down");

    let saved = store.map.lock().unwrap();
    assert_eq!(saved.len(), 1, "only the finished agent was saved");
    assert!(saved.contains_key(&first_id));
}

/// The round-major counterpart: the first agent finishes in round one, the
/// second panics in round two.
#[tokio::test]
async fn finished_agent_survives_a_later_crash_round_major() {
    let store = MemStore::default();
    let first = batch_agent(Behavior::Complete, 1);
    let first_id = first.id();
    let agents = vec![first, batch_agent(Behavior::Panic, 2)];
    let mut reactor: Reactor<_, _, TestAgent> =
        Reactor::new(MockInference::default(), store.clone(), agents);

    let crashed = tokio::spawn(async move { reactor.run().await }).await;
    assert!(crashed.unwrap_err().is_panic(), "the run went down");

    let saved = store.map.lock().unwrap();
    assert_eq!(saved.len(), 1, "only the finished agent was saved");
    assert!(saved.contains_key(&first_id));
}

/// Bulk load: `load_agents` reconstructs from one query, skipping ids with
/// nothing stored.
#[tokio::test]
async fn load_agents_round_trips() {
    let mut store = MemStore::default();
    let id1 = AgentId::new();
    let id2 = AgentId::new();
    store
        .save::<TestAgent>(
            id1,
            &TestState {
                behavior: Behavior::Complete,
                turns_left: 1,
                poison: None,
            },
            None,
        )
        .await
        .unwrap();
    store
        .save::<TestAgent>(
            id2,
            &TestState {
                behavior: Behavior::Stall,
                turns_left: 0,
                poison: None,
            },
            None,
        )
        .await
        .unwrap();

    let LoadedAgents {
        agents,
        bases,
        failures,
    }: LoadedAgents<TestAgent, u64> =
        load_agents(&store, "shared", [id1, id2, AgentId::new()].into_iter())
            .await
            .unwrap();
    assert_eq!(bases.len(), 2, "each loaded agent's base");

    assert_eq!(failures.len(), 1); // for the AgentId::new
    assert_eq!(agents.len(), 2);
    let ids: Vec<_> = agents.iter().map(|a| a.id()).collect();
    assert!(ids.contains(&id1) && ids.contains(&id2));
    assert!(
        agents.iter().all(|a| a.ctx == "shared"),
        "the context reached every construction"
    );
}

/// Partial save: a store that commits a prefix then fails. The committed,
/// completed agents are `done`; the rest are `failed`, their snapshots survive
/// in `unsaved` (deserializable back to state), and the lone store error lands
/// on the unsaved agent, classified `Storage`.
#[tokio::test]
async fn partial_save_surfaces_unsaved_snapshots() {
    let store = PartialStore::commit(2); // commits the first two, fails on the third
    let agents = vec![
        batch_agent(Behavior::Complete, 1),
        batch_agent(Behavior::Complete, 1),
        batch_agent(Behavior::Complete, 1),
    ];
    let ids: Vec<AgentId> = agents.iter().map(|a| a.id()).collect();
    let mut reactor: Reactor<_, _, TestAgent> =
        Reactor::new(MockInference::default(), store, agents);
    let report = reactor.run().await.unwrap();

    assert_eq!(report.done, 2, "committed + completed agents are done");
    assert_eq!(report.failed, 1, "the un-persisted agent is failed");

    assert_eq!(report.unsaved.len(), 1);
    let snapshot = report
        .unsaved
        .get(&ids[2])
        .expect("third agent's snapshot kept");
    let recovered: TestState =
        serde_json::from_value(snapshot.clone()).unwrap();
    assert_eq!(
        recovered.behavior,
        Behavior::Complete,
        "snapshot round-trips"
    );

    let err = report.errors.get(&ids[2]).expect("store error attributed");
    assert_eq!(err.kind, ErrorKind::Storage);
    assert!(err.retry_after.is_none(), "a plain store error is fatal");
}

/// A successful run leaves nothing to recover and no errors.
#[tokio::test]
async fn successful_run_leaves_nothing_unsaved() {
    let agents = vec![
        batch_agent(Behavior::Complete, 1),
        batch_agent(Behavior::Complete, 1),
    ];
    let mut reactor: Reactor<_, _, TestAgent> =
        Reactor::new(MockInference::default(), MemStore::default(), agents);
    let report = reactor.run().await.unwrap();

    assert_eq!(report.done, 2);
    assert!(report.unsaved.is_empty());
    assert!(report.errors.is_empty());
}

/// The `Report` (errors + unsaved snapshots) survives a serde round-trip — it
/// has to, since it crosses the orchestrator's `dyn Run` erasure as data.
#[test]
fn report_serde_round_trips() {
    let id = AgentId::new();
    let mut report = Report {
        done: 1,
        failed: 1,
        ..Default::default()
    };
    report.errors.insert(
        id,
        ErrorReport {
            kind: ErrorKind::Storage,
            retry_after: Some(Duration::from_secs(5)),
            message: "disk full".into(),
        },
    );
    report.unsaved.insert(
        id,
        serde_json::json!({ "behavior": "Complete", "turns_left": 1 }),
    );

    let json = serde_json::to_string(&report).unwrap();
    let back: Report = serde_json::from_str(&json).unwrap();

    assert_eq!(back.done, 1);
    assert_eq!(back.failed, 1);
    assert_eq!(back.errors[&id].kind, ErrorKind::Storage);
    assert_eq!(back.errors[&id].retry_after, Some(Duration::from_secs(5)));
    assert_eq!(back.unsaved[&id]["turns_left"], 1);
}

/// `session_finished`'s outcome: an error wins, then the stall cap, then the
/// agent's own outcome.
#[test]
fn session_outcome_names_how_it_ended() {
    use super::super::Ending;
    let err: Result<Outcome, &str> = Err("boom");
    assert_eq!(Ending::of(&err, true).as_str(), "error");
    assert_eq!(
        Ending::of(&Ok::<_, ()>(Outcome::Failed), true).as_str(),
        "stalled"
    );
    assert_eq!(
        Ending::of(&Ok::<_, ()>(Outcome::Failed), false).as_str(),
        "failed"
    );
    assert_eq!(
        Ending::of(&Ok::<_, ()>(Outcome::Complete), false).as_str(),
        "complete"
    );
}

/// A store whose first `n` saves fail, then every save commits: the
/// session-end save fails and the end-of-run retry commits.
#[derive(Default, Clone)]
struct FailFirst {
    inner: MemStore,
    left: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl Storage for FailFirst {
    type Error = TestError;
    type Version = u64;

    #[allow(deprecated)]
    async fn save_raw(
        &mut self,
        id: AgentId,
        value: serde_json::Value,
        base: Option<&u64>,
    ) -> Result<Option<u64>, TestError> {
        if self
            .left
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                n.checked_sub(1)
            })
            .is_ok()
        {
            return Err(TestError::Msg("disk hiccup".into()));
        }
        self.inner.save_raw(id, value, base).await
    }

    async fn load_raw(
        &self,
        id: AgentId,
    ) -> Result<Loaded<serde_json::Value, u64>, TestError> {
        self.inner.load_raw(id).await
    }
}

/// `on_saved` runs once for each complete session whose save committed, on
/// both paths, and never for a session that stalled or errored.
#[tokio::test]
async fn on_saved_runs_once_only_after_a_committed_complete_session() {
    let agents = vec![
        agent(Behavior::Complete, 1),
        batch_agent(Behavior::Complete, 1),
        agent(Behavior::Stall, 1),
        batch_agent(Behavior::Stall, 1),
        agent(Behavior::ErrHandle, 1),
        batch_agent(Behavior::ErrHandle, 1),
    ];
    let saves: Vec<_> = agents.iter().map(|a| a.saves.clone()).collect();
    let mut reactor: Reactor<_, _, TestAgent> =
        Reactor::new(MockInference::end_turns(64), MemStore::default(), agents);
    reactor.run().await.unwrap();

    let calls: Vec<usize> =
        saves.iter().map(|s| s.load(Ordering::SeqCst)).collect();
    assert_eq!(calls, [1, 1, 0, 0, 0, 0]);
}

/// A complete session whose state never committed gets no `on_saved`; one
/// whose session-end save failed but the end-of-run retry committed gets it
/// then, once.
#[tokio::test]
async fn on_saved_waits_for_the_save_that_commits() {
    let never = agent(Behavior::Complete, 1);
    let never_saves = never.saves.clone();
    let mut reactor: Reactor<_, _, TestAgent> = Reactor::new(
        MockInference::end_turns(1),
        PartialStore::commit(0),
        vec![never],
    );
    let report = reactor.run().await.unwrap();
    assert_eq!(report.done, 0);
    assert_eq!(never_saves.load(Ordering::SeqCst), 0);

    let retried = agent(Behavior::Complete, 1);
    let retried_saves = retried.saves.clone();
    let store = FailFirst {
        left: Arc::new(AtomicUsize::new(1)),
        ..Default::default()
    };
    let mut reactor: Reactor<_, _, TestAgent> =
        Reactor::new(MockInference::end_turns(1), store, vec![retried]);
    let report = reactor.run().await.unwrap();
    assert_eq!(report.done, 1);
    assert_eq!(retried_saves.load(Ordering::SeqCst), 1);
}

/// An agent whose `on_init` fails is saved by neither path, at the session's
/// end or in the end-of-run retry, and is not left in `unsaved` either
#[tokio::test]
async fn a_failed_start_saves_nothing() {
    for batch in [false, true] {
        let make = if batch { batch_agent } else { agent };
        let store = MemStore::default();
        let refused = make(Behavior::ErrInit, 1);
        let fine = make(Behavior::Complete, 1);
        let (refused_id, fine_id) = (refused.id(), fine.id());
        let mut reactor: Reactor<_, _, TestAgent> = Reactor::new(
            MockInference::end_turns(4),
            store.clone(),
            vec![refused, fine],
        );
        let report = reactor.run().await.unwrap();

        let map = store.map.lock().unwrap();
        assert!(!map.contains_key(&refused_id), "batch={batch}");
        assert!(map.contains_key(&fine_id), "batch={batch}");
        assert!(report.unsaved.is_empty(), "batch={batch}");
        assert_eq!(report.failed, 1);
        assert_eq!(report.errors[&refused_id].kind, ErrorKind::Agent);
    }
}

/// Each agent's base goes from its load to its save, and the next save
/// names the version the last one made
#[tokio::test]
async fn the_base_goes_from_load_to_save() {
    let mut store = MemStore::default();
    let id = AgentId::new();
    let state = TestState {
        behavior: Behavior::Complete,
        turns_left: 1,
        poison: None,
    };
    let first = store.save::<TestAgent>(id, &state, None).await.unwrap();
    assert_eq!(first, Some(1));

    for round in 1..=2u64 {
        let LoadedAgents { agents, bases, .. }: LoadedAgents<TestAgent, u64> =
            load_agents(&store, "", [id].into_iter()).await.unwrap();
        assert_eq!(bases[&id], round);
        let mut reactor: Reactor<_, _, TestAgent> =
            Reactor::new(MockInference::end_turns(2), store.clone(), agents)
                .with_bases(bases);
        let report = reactor.run().await.unwrap();
        assert_eq!(report.done, 1);
        assert_eq!(report.bases[&id], serde_json::json!(round + 1));
    }
    let seen = store.bases_seen.lock().unwrap();
    assert_eq!(*seen, vec![(id, None), (id, Some(1)), (id, Some(2)),]);
}
