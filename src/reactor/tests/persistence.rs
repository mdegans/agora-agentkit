//! Persistence: one bulk save/load for the whole cohort, and partial-save
//! recovery (unsaved snapshots + error attribution), plus the `Report` serde
//! round-trip it all crosses the `dyn Run` erasure as.

use std::sync::atomic::Ordering;
use std::time::Duration;

use super::*;

/// Bulk save: the reactor persists the whole cohort in a single `save_all_raw`
/// call (the override a SQL backend would do as one query), not per-agent.
#[tokio::test]
async fn reactor_persists_in_one_bulk_save() {
    let store = BulkStore::default();
    let agents = vec![
        batch_agent(Behavior::Complete, 1),
        batch_agent(Behavior::Complete, 1),
        batch_agent(Behavior::Complete, 1),
    ];
    let mut reactor: Reactor<_, _, TestAgent> =
        Reactor::new(MockInference::default(), store.clone(), agents);
    reactor.run().await.unwrap();

    assert_eq!(store.bulk_calls.load(Ordering::SeqCst), 1, "one bulk save");
    assert_eq!(*store.last_batch.lock().unwrap(), 3, "all three in it");
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
        )
        .await
        .unwrap();

    let (agents, failures): (Vec<TestAgent>, _) =
        load_agents(&store, [id1, id2, AgentId::new()].into_iter())
            .await
            .unwrap();

    assert_eq!(failures.len(), 1); // for the AgentId::new
    assert_eq!(agents.len(), 2);
    let ids: Vec<_> = agents.iter().map(|a| a.id()).collect();
    assert!(ids.contains(&id1) && ids.contains(&id2));
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
