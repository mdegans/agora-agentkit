//! Mixed-model negotiation: one `Reactor`, an endpoint offering several
//! models, agents requesting different ids. Negotiation is per-agent — each
//! agent lands on the offered model matching its requested id (and on the
//! run-path its requested capabilities pick), and an unoffered id is rejected
//! without collateral to the rest of the cohort.

use super::*;

/// Two offered (non-batch) models, four agents split across them: every agent
/// is admitted onto the sequential path, all complete, none are rejected, and
/// each submitted prompt carries its requesting agent's model id.
#[tokio::test]
async fn mixed_cohort_routes_per_agent() {
    let transport = ModelRecorder::offering([
        model_info_named("model-a", false),
        model_info_named("model-b", false),
    ]);
    let agents = vec![
        named_agent("model-a", Behavior::Complete, 1),
        named_agent("model-b", Behavior::Complete, 1),
        named_agent("model-a", Behavior::Complete, 1),
        named_agent("model-b", Behavior::Complete, 1),
    ];
    let mut reactor: Reactor<_, _, TestAgent> =
        Reactor::new(transport.clone(), MemStore::default(), agents);
    let report = reactor.run().await.unwrap();

    assert_eq!(report.done, 4, "every agent completes");
    assert_eq!(report.failed, 0);
    assert!(report.rejected.is_empty(), "both ids are offered");

    // One request per agent, each under the requesting agent's id. The
    // sequential path may interleave agents, so compare as a multiset.
    let mut models = transport.seq_models();
    models.sort();
    assert_eq!(models, ["model-a", "model-a", "model-b", "model-b"]);
    assert!(
        transport.round_models().is_empty(),
        "no batch path involved"
    );
}

/// An agent requesting an id the endpoint doesn't offer is rejected — never
/// run, never downgraded onto an offered model — and its snapshot lands in
/// `Report::rejected` for the caller to re-route, while the agents whose ids
/// *are* offered run to completion.
#[tokio::test]
async fn unoffered_model_is_rejected_others_run() {
    let transport = ModelRecorder::offering([
        model_info_named("model-a", false),
        model_info_named("model-b", false),
    ]);
    let stray = named_agent("model-c", Behavior::Complete, 1);
    let stray_id = stray.id();

    let mut reactor: Reactor<_, _, TestAgent> = Reactor::new(
        transport.clone(),
        MemStore::default(),
        vec![
            named_agent("model-a", Behavior::Complete, 1),
            stray,
            named_agent("model-b", Behavior::Complete, 1),
        ],
    );
    let report = reactor.run().await.unwrap();

    assert_eq!(report.done, 2, "the offered-id agents ran");
    assert_eq!(report.failed, 0, "rejection is not failure");
    let snapshot = report
        .rejected
        .get(&stray_id)
        .expect("rejected agent's snapshot kept");
    let state: TestState = serde_json::from_value(snapshot.clone()).unwrap();
    assert_eq!(state.behavior, Behavior::Complete, "snapshot round-trips");

    let mut models = transport.seq_models();
    models.sort();
    assert_eq!(
        models,
        ["model-a", "model-b"],
        "the unoffered id never hit the wire"
    );
}

/// One offered model with the batch capability, one without, an agent on each:
/// the batch-model agent negotiates onto the round-major path and the other
/// onto the agent-major path — in one reactor, each under its own id.
#[tokio::test]
async fn mixed_models_split_across_both_paths() {
    let transport = ModelRecorder::offering([
        model_info_named("model-a", true),
        model_info_named("model-b", false),
    ]);
    let agents = vec![
        named_batch_agent("model-a", Behavior::Complete, 1),
        named_agent("model-b", Behavior::Complete, 1),
    ];
    let mut reactor: Reactor<_, _, TestAgent> =
        Reactor::new(transport.clone(), MemStore::default(), agents);
    let report = reactor.run().await.unwrap();

    assert_eq!(report.done, 2, "both agents complete");
    assert!(report.rejected.is_empty());
    assert_eq!(
        transport.round_models(),
        vec![vec!["model-a"]],
        "one round-major batch: the batch agent alone, under its id"
    );
    assert_eq!(
        transport.seq_models(),
        ["model-b"],
        "the sequential agent made one infer call under its id"
    );
}
