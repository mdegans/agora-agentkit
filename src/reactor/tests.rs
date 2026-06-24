//! Reactor tests against a mock transport and an in-memory store. They exercise
//! the scheduling contracts without any network:
//!
//! - **A** round-major lockstep: one batch per round, sized to the live cohort,
//!   shrinking as agents finish.
//! - **B** stall-cap: an agent that never makes progress is failed after the
//!   cap, not looped forever.
//! - **C** error containment: one agent's failure doesn't abort the cohort.
//! - **D** `PauseTurn` continuation: a paused turn continues and isn't a stall.
//! - **E** turn-order invariant: after `handle`, the prompt ends in a user turn.

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use misanthropic::prompt::Prompt;
use misanthropic::prompt::message::Role;
use misanthropic::response::{self, StopReason};

use super::backend::{BatchInference, Inference, SaveError, Storage};
use super::{
    Agent, BatchReactor, Control, ErrorKind, ErrorReport, Outcome, Reactor, Report, RetryAfter,
    Run, State, load_agents,
};
use crate::ids::AgentId;

// ---------------------------------------------------------------------------
// Shared error: serves as Agent::Error, Storage::Error, and Inference::Error.
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
enum TestError {
    #[error("tool: {0}")]
    Tool(#[from] Box<dyn std::error::Error + Send + Sync>),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Msg(String),
    /// A transient error carrying a retry hint, for exercising classification.
    #[error("transient, retry after {0:?}")]
    Transient(Duration),
}

impl RetryAfter for TestError {
    fn retry_after(&self) -> Option<Duration> {
        match self {
            TestError::Transient(d) => Some(*d),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Agent — uses the DEFAULT `handle`; only `on_quiesce` is overridden (the test
// agents register no tools, so every response is quiescent).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
enum Behavior {
    /// Finish cleanly after `turns_left` quiescent responses.
    Complete,
    /// `on_quiesce` returns `Err` (a drive error) on the first response.
    ErrHandle,
    /// `on_quiesce` always stalls — the reactor should cap and fail it.
    Stall,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct TestState {
    behavior: Behavior,
    turns_left: usize,
}

impl State for TestState {}

struct TestAgent {
    id: AgentId,
    state: TestState,
    prompt: Prompt,
    tools: misanthropic::tool::ToolBox,
}

impl TestAgent {
    fn push_user(&mut self, text: &str) -> Result<(), TestError> {
        self.prompt
            .push_message((Role::User, text.to_string()))
            .map_err(|e| TestError::Msg(e.to_string()))?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl Agent for TestAgent {
    type State = TestState;
    type Error = TestError;

    fn new(id: AgentId, state: TestState) -> Result<Self, TestError> {
        let mut agent = Self {
            id,
            state,
            prompt: Prompt::default(),
            tools: misanthropic::tool::ToolBox::new(),
        };
        // Establish the "ends in a user turn" invariant.
        agent.push_user("start")?;
        Ok(agent)
    }

    fn id(&self) -> AgentId {
        self.id
    }

    fn snapshot(&self) -> TestState {
        self.state.clone()
    }

    fn prompt(&self) -> &Prompt {
        &self.prompt
    }

    fn parts(&mut self) -> (&mut misanthropic::tool::ToolBox, &mut Prompt) {
        (&mut self.tools, &mut self.prompt)
    }

    async fn on_quiesce(&mut self, _response: &response::Message) -> Result<Control, TestError> {
        match self.state.behavior {
            Behavior::ErrHandle => Err(TestError::Msg("boom".into())),
            Behavior::Stall => {
                // A real agent would re-ask; keep the prompt legal and stall.
                self.push_user("retry")?;
                Ok(Control::Stalled)
            }
            Behavior::Complete => {
                if self.state.turns_left <= 1 {
                    Ok(Control::Done(Outcome::Complete))
                } else {
                    self.state.turns_left -= 1;
                    self.push_user("next")?;
                    Ok(Control::Continue)
                }
            }
        }
    }
}

fn agent(behavior: Behavior, turns_left: usize) -> TestAgent {
    TestAgent::new(
        AgentId::new(),
        TestState {
            behavior,
            turns_left,
        },
    )
    .unwrap()
}

// ---------------------------------------------------------------------------
// In-memory store
// ---------------------------------------------------------------------------

#[derive(Default, Clone)]
struct MemStore {
    map: Arc<Mutex<HashMap<AgentId, serde_json::Value>>>,
}

#[async_trait::async_trait]
impl Storage for MemStore {
    type Error = TestError;

    async fn save_raw(&mut self, id: AgentId, value: serde_json::Value) -> Result<(), TestError> {
        self.map.lock().unwrap().insert(id, value);
        Ok(())
    }

    async fn load_raw(&self, id: AgentId) -> Result<Option<serde_json::Value>, TestError> {
        Ok(self.map.lock().unwrap().get(&id).cloned())
    }
}

/// A store that overrides `save_all_raw` (as a SQL backend would, with one
/// query) and records how it was called.
#[derive(Default, Clone)]
struct BulkStore {
    map: Arc<Mutex<HashMap<AgentId, serde_json::Value>>>,
    bulk_calls: Arc<AtomicUsize>,
    last_batch: Arc<Mutex<usize>>,
}

#[async_trait::async_trait]
impl Storage for BulkStore {
    type Error = TestError;

    async fn save_raw(&mut self, id: AgentId, value: serde_json::Value) -> Result<(), TestError> {
        self.map.lock().unwrap().insert(id, value);
        Ok(())
    }

    async fn load_raw(&self, id: AgentId) -> Result<Option<serde_json::Value>, TestError> {
        Ok(self.map.lock().unwrap().get(&id).cloned())
    }

    async fn save_all_raw(
        &mut self,
        items: Vec<(AgentId, serde_json::Value)>,
    ) -> Result<(), SaveError<TestError>> {
        self.bulk_calls.fetch_add(1, Ordering::SeqCst);
        *self.last_batch.lock().unwrap() = items.len();
        let mut map = self.map.lock().unwrap();
        for (id, value) in items {
            map.insert(id, value);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Mock transport: scripts a stop_reason per round.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct MockInference {
    infer_calls: AtomicUsize,
    script: Vec<StopReason>,
}

fn message(stop: StopReason) -> response::Message {
    let stop = match stop {
        StopReason::EndTurn => "end_turn",
        StopReason::PauseTurn => "pause_turn",
        StopReason::ToolUse => "tool_use",
        StopReason::MaxTokens => "max_tokens",
        StopReason::StopSequence => "stop_sequence",
        StopReason::Refusal => "refusal",
    };
    serde_json::from_value(serde_json::json!({
        "id": "msg_test",
        "role": "assistant",
        "content": [{ "type": "text", "text": "ok" }],
        "model": "claude-3-5-haiku-latest",
        "stop_reason": stop,
        "stop_sequence": null,
    }))
    .expect("valid response::Message fixture")
}

#[async_trait::async_trait]
impl Inference for MockInference {
    type Error = TestError;
    type Prompt = Prompt;

    async fn infer(&self, _prompt: &Prompt) -> Result<response::Message, TestError> {
        let round = self.infer_calls.fetch_add(1, Ordering::SeqCst);
        Ok(message(
            self.script
                .get(round)
                .copied()
                .unwrap_or(StopReason::EndTurn),
        ))
    }

    async fn models(&self) -> Result<misanthropic::model::Models, TestError> {
        Err(TestError::Msg("no models in mock".into()))
    }
}

#[async_trait::async_trait]
impl BatchInference for MockInference {
    async fn infer_batch(
        &self,
        prompts: &[&Prompt],
    ) -> Result<Vec<Result<response::Message, TestError>>, TestError> {
        Ok(prompts
            .iter()
            .map(|_| Ok(message(StopReason::EndTurn)))
            .collect())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// A — round-major lockstep: one batch per round, sized to the live cohort,
/// shrinking as agents finish (turns 1, 2, 3 → batch sizes 3, 2, 1).
#[tokio::test]
async fn batch_sizes_match_live_cohort_each_round() {
    let sizes = SharedSizes::default();
    let transport = RecordingBatch {
        sizes: sizes.clone(),
    };
    let agents = vec![
        agent(Behavior::Complete, 1),
        agent(Behavior::Complete, 2),
        agent(Behavior::Complete, 3),
    ];
    let mut reactor = BatchReactor::new(transport, MemStore::default(), agents);
    let report = reactor.run().await.unwrap();

    assert_eq!(report.done, 3);
    assert_eq!(sizes.get(), vec![3, 2, 1], "one batch per round, shrinking");
}

/// B — stall cap: an agent that never progresses is failed after the cap, not
/// looped forever (the test terminating at all is half the assertion).
#[tokio::test]
async fn stall_cap_bounds_retry() {
    let mut reactor = Reactor::new(
        MockInference::default(),
        MemStore::default(),
        vec![agent(Behavior::Stall, 0)],
    );
    let report = reactor.run().await.unwrap();

    assert_eq!(report.failed, 1);
    assert_eq!(report.done, 0);
}

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
    let mut reactor = Reactor::new(MockInference::default(), MemStore::default(), agents);
    let report = reactor.run().await.unwrap();

    assert_eq!(report.done, 2);
    assert_eq!(report.failed, 1);
    assert!(report.errors.contains_key(&bad_id));
}

/// C (batch) — same containment guarantee in the round-major reactor.
#[tokio::test]
async fn batch_contains_one_failure() {
    let bad = agent(Behavior::ErrHandle, 1);
    let bad_id = bad.id();
    let agents = vec![
        agent(Behavior::Complete, 1),
        bad,
        agent(Behavior::Complete, 1),
    ];
    let mut reactor = BatchReactor::new(MockInference::default(), MemStore::default(), agents);
    let report = reactor.run().await.unwrap();

    assert_eq!(report.done, 2);
    assert_eq!(report.failed, 1);
    assert!(report.errors.contains_key(&bad_id));
}

/// D — a paused turn continues (and is not a stall): scripting PauseTurn then
/// EndTurn, the agent finishes and is not failed.
#[tokio::test]
async fn pause_turn_continues() {
    let inference = MockInference {
        script: vec![StopReason::PauseTurn, StopReason::EndTurn],
        ..Default::default()
    };
    let mut reactor = Reactor::new(
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

/// Bulk save: the reactor persists the whole cohort in a single `save_all_raw`
/// call (the override a SQL backend would do as one query), not per-agent.
#[tokio::test]
async fn reactor_persists_in_one_bulk_save() {
    let store = BulkStore::default();
    let agents = vec![
        agent(Behavior::Complete, 1),
        agent(Behavior::Complete, 1),
        agent(Behavior::Complete, 1),
    ];
    let mut reactor = BatchReactor::new(MockInference::default(), store.clone(), agents);
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
        .save(
            id1,
            &TestState {
                behavior: Behavior::Complete,
                turns_left: 1,
            },
        )
        .await
        .unwrap();
    store
        .save(
            id2,
            &TestState {
                behavior: Behavior::Stall,
                turns_left: 0,
            },
        )
        .await
        .unwrap();

    let (agents, failures): (Vec<TestAgent>, _) = load_agents(&store, &[id1, id2, AgentId::new()])
        .await
        .unwrap();

    assert!(failures.is_empty());
    assert_eq!(agents.len(), 2, "missing id skipped");
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
        agent(Behavior::Complete, 1),
        agent(Behavior::Complete, 1),
        agent(Behavior::Complete, 1),
    ];
    let ids: Vec<AgentId> = agents.iter().map(|a| a.id()).collect();
    let mut reactor = BatchReactor::new(MockInference::default(), store, agents);
    let report = reactor.run().await.unwrap();

    assert_eq!(report.done, 2, "committed + completed agents are done");
    assert_eq!(report.failed, 1, "the un-persisted agent is failed");

    assert_eq!(report.unsaved.len(), 1);
    let snapshot = report
        .unsaved
        .get(&ids[2])
        .expect("third agent's snapshot kept");
    let recovered: TestState = serde_json::from_value(snapshot.clone()).unwrap();
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
    let agents = vec![agent(Behavior::Complete, 1), agent(Behavior::Complete, 1)];
    let mut reactor = BatchReactor::new(MockInference::default(), MemStore::default(), agents);
    let report = reactor.run().await.unwrap();

    assert_eq!(report.done, 2);
    assert!(report.unsaved.is_empty());
    assert!(report.errors.is_empty());
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
    let mut reactor = BatchReactor::new(
        transport,
        MemStore::default(),
        vec![agent(Behavior::Complete, 1)],
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
    let mut reactor = BatchReactor::new(
        transport,
        MemStore::default(),
        vec![agent(Behavior::Complete, 1)],
    );
    let report = reactor.run().await.unwrap();

    assert_eq!(report.failed, 1);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        3,
        "re-batched up to MAX_BATCH_ITEM_RETRIES"
    );
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

// ---------------------------------------------------------------------------
// A recording batch transport, so test A can read per-round batch sizes after
// the cohort is moved into the reactor.
// ---------------------------------------------------------------------------

#[derive(Default, Clone)]
struct SharedSizes(Arc<Mutex<Vec<usize>>>);

impl SharedSizes {
    fn get(&self) -> Vec<usize> {
        self.0.lock().unwrap().clone()
    }
}

struct RecordingBatch {
    sizes: SharedSizes,
}

#[async_trait::async_trait]
impl Inference for RecordingBatch {
    type Error = TestError;
    type Prompt = Prompt;

    async fn infer(&self, _prompt: &Prompt) -> Result<response::Message, TestError> {
        Ok(message(StopReason::EndTurn))
    }

    async fn models(&self) -> Result<misanthropic::model::Models, TestError> {
        Err(TestError::Msg("no models in mock".into()))
    }
}

#[async_trait::async_trait]
impl BatchInference for RecordingBatch {
    async fn infer_batch(
        &self,
        prompts: &[&Prompt],
    ) -> Result<Vec<Result<response::Message, TestError>>, TestError> {
        self.sizes.0.lock().unwrap().push(prompts.len());
        Ok(prompts
            .iter()
            .map(|_| Ok(message(StopReason::EndTurn)))
            .collect())
    }
}

// ---------------------------------------------------------------------------
// A store that commits a prefix of a bulk save and then fails — the streaming
// (non-transactional) case the partial-save reporting exists for.
// ---------------------------------------------------------------------------

#[derive(Default, Clone)]
struct PartialStore {
    map: Arc<Mutex<HashMap<AgentId, serde_json::Value>>>,
    commit: usize,
}

impl PartialStore {
    fn commit(commit: usize) -> Self {
        Self {
            commit,
            ..Default::default()
        }
    }
}

#[async_trait::async_trait]
impl Storage for PartialStore {
    type Error = TestError;

    async fn save_raw(&mut self, id: AgentId, value: serde_json::Value) -> Result<(), TestError> {
        self.map.lock().unwrap().insert(id, value);
        Ok(())
    }

    async fn load_raw(&self, id: AgentId) -> Result<Option<serde_json::Value>, TestError> {
        Ok(self.map.lock().unwrap().get(&id).cloned())
    }

    async fn save_all_raw(
        &mut self,
        items: Vec<(AgentId, serde_json::Value)>,
    ) -> Result<(), SaveError<TestError>> {
        let mut saved = BTreeSet::new();
        let mut map = self.map.lock().unwrap();
        for (i, (id, value)) in items.into_iter().enumerate() {
            if i >= self.commit {
                return Err(SaveError {
                    saved,
                    inner: TestError::Msg("out of space".into()),
                });
            }
            map.insert(id, value);
            saved.insert(id);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// A batch transport that fails every item, either fatally (no retry hint) or
// transiently (with one), to exercise per-item retry classification.
// ---------------------------------------------------------------------------

struct FailingBatch {
    calls: Arc<AtomicUsize>,
    fatal: bool,
}

#[async_trait::async_trait]
impl Inference for FailingBatch {
    type Error = TestError;
    type Prompt = Prompt;

    async fn infer(&self, _prompt: &Prompt) -> Result<response::Message, TestError> {
        Ok(message(StopReason::EndTurn))
    }

    async fn models(&self) -> Result<misanthropic::model::Models, TestError> {
        Err(TestError::Msg("no models in mock".into()))
    }
}

#[async_trait::async_trait]
impl BatchInference for FailingBatch {
    async fn infer_batch(
        &self,
        prompts: &[&Prompt],
    ) -> Result<Vec<Result<response::Message, TestError>>, TestError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(prompts
            .iter()
            .map(|_| {
                Err(if self.fatal {
                    TestError::Msg("fatal".into())
                } else {
                    TestError::Transient(Duration::from_millis(1))
                })
            })
            .collect())
    }
}
