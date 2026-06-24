//! Reactor tests against a mock transport and an in-memory store. They exercise
//! the scheduling contracts without any network:
//!
//! - **A** round-major lockstep: one batch per round, sized to the live cohort,
//!   shrinking as agents finish.
//! - **B** budget-bounded retry: a failing agent gives up after its budget, not
//!   forever.
//! - **C** error containment: one agent's failure doesn't abort the cohort.
//! - **D** `PauseTurn` continuation: a paused turn continues (and isn't a fail).
//! - **E** turn-order invariant: after `handle`, the prompt ends in a user turn.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use misanthropic::prompt::Prompt;
use misanthropic::prompt::message::Role;
use misanthropic::response::{self, StopReason};

use super::backend::{BatchInference, Inference, Storage};
use super::{Agent, BatchReactor, Outcome, Reactor, Run, State};
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
}

// ---------------------------------------------------------------------------
// Agent
// ---------------------------------------------------------------------------

/// How a `TestAgent` behaves each time it is handed a response.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
enum Behavior {
    /// Finish cleanly after `turns_left` responses.
    Complete,
    /// `handle` returns `Err` on the first response (a drive error).
    ErrHandle,
    /// Charge the budget every response; fail when it reaches zero.
    Budget,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct TestState {
    behavior: Behavior,
    turns_left: usize,
    budget: usize,
    /// `Some(true)` = complete, `Some(false)` = failed, `None` = still running.
    done: Option<bool>,
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

    fn outcome(&self) -> Option<Outcome> {
        match self.state.done {
            None => None,
            Some(true) => Some(Outcome::Complete),
            Some(false) => Some(Outcome::Failed),
        }
    }

    fn prompt(&self) -> &Prompt {
        &self.prompt
    }

    fn parts(&mut self) -> (&mut misanthropic::tool::ToolBox, &mut Prompt) {
        (&mut self.tools, &mut self.prompt)
    }

    async fn handle(&mut self, response: response::Message) -> Result<(), TestError> {
        // A paused turn continues: seat the content, stay running, no budget
        // charge — and re-establish a user tail so the next infer is legal.
        if matches!(response.stop_reason, Some(StopReason::PauseTurn)) {
            self.prompt
                .push_message(response.inner)
                .map_err(|e| TestError::Msg(e.to_string()))?;
            self.push_user("continue")?;
            return Ok(());
        }

        match self.state.behavior {
            Behavior::ErrHandle => return Err(TestError::Msg("boom".into())),
            Behavior::Budget => {
                if self.state.budget == 0 {
                    self.state.done = Some(false);
                } else {
                    self.state.budget -= 1;
                    // Re-seat to end in a user turn so it re-batches legally.
                    self.prompt
                        .push_message(response.inner)
                        .map_err(|e| TestError::Msg(e.to_string()))?;
                    self.push_user("retry")?;
                }
            }
            Behavior::Complete => {
                self.prompt
                    .push_message(response.inner)
                    .map_err(|e| TestError::Msg(e.to_string()))?;
                self.push_user("next")?;
                if self.state.turns_left <= 1 {
                    self.state.done = Some(true);
                } else {
                    self.state.turns_left -= 1;
                }
            }
        }
        Ok(())
    }
}

fn agent(behavior: Behavior, turns_left: usize, budget: usize) -> TestAgent {
    TestAgent::new(
        AgentId::new(),
        TestState {
            behavior,
            turns_left,
            budget,
            done: None,
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

// ---------------------------------------------------------------------------
// Mock transport: scripts a stop_reason per round and records call shapes.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct MockInference {
    /// Single-`infer` call count (sequential reactor).
    infer_calls: AtomicUsize,
    /// Size of each `infer_batch` submission, in order (batch reactor).
    batch_sizes: Mutex<Vec<usize>>,
    /// `stop_reason` per round; rounds past the end default to `EndTurn`.
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
        let round = {
            let mut sizes = self.batch_sizes.lock().unwrap();
            sizes.push(prompts.len());
            sizes.len() - 1
        };
        let stop = self
            .script
            .get(round)
            .copied()
            .unwrap_or(StopReason::EndTurn);
        Ok(prompts.iter().map(|_| Ok(message(stop))).collect())
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
        agent(Behavior::Complete, 1, 0),
        agent(Behavior::Complete, 2, 0),
        agent(Behavior::Complete, 3, 0),
    ];
    let mut reactor = BatchReactor::new(transport, MemStore::default(), agents);
    let report = reactor.run().await.unwrap();

    assert_eq!(report.done, 3);
    assert_eq!(sizes.get(), vec![3, 2, 1], "one batch per round, shrinking");
}

/// B — budget-bounded retry: a Budget agent fails after exactly `budget + 1`
/// responses (not forever), and the exhausted budget is visible in its state.
#[tokio::test]
async fn budget_bounds_retry() {
    let inference = MockInference::default();
    let store = MemStore::default();
    let only = agent(Behavior::Budget, 0, 3);
    let id = only.id();
    let mut reactor = Reactor::new(inference, store.clone(), vec![only]);
    let report = reactor.run().await.unwrap();

    assert_eq!(report.failed, 1);
    assert_eq!(report.done, 0);
    // Budget exhausted, persisted.
    let saved: TestState =
        serde_json::from_value(store.map.lock().unwrap().get(&id).cloned().unwrap()).unwrap();
    assert_eq!(saved.budget, 0);
    assert_eq!(saved.done, Some(false));
}

/// C — error containment (sequential): one agent's `handle` error doesn't abort
/// the cohort; the others complete and the failure is recorded.
#[tokio::test]
async fn sequential_contains_one_failure() {
    let inference = MockInference::default();
    let bad = agent(Behavior::ErrHandle, 1, 0);
    let bad_id = bad.id();
    let agents = vec![
        agent(Behavior::Complete, 1, 0),
        bad,
        agent(Behavior::Complete, 1, 0),
    ];
    let mut reactor = Reactor::new(inference, MemStore::default(), agents);
    let report = reactor.run().await.unwrap();

    assert_eq!(report.done, 2);
    assert_eq!(report.failed, 1);
    assert!(report.errors.contains_key(&bad_id));
}

/// C (batch) — same containment guarantee in the round-major reactor.
#[tokio::test]
async fn batch_contains_one_failure() {
    let inference = MockInference::default();
    let bad = agent(Behavior::ErrHandle, 1, 0);
    let bad_id = bad.id();
    let agents = vec![
        agent(Behavior::Complete, 1, 0),
        bad,
        agent(Behavior::Complete, 1, 0),
    ];
    let mut reactor = BatchReactor::new(inference, MemStore::default(), agents);
    let report = reactor.run().await.unwrap();

    assert_eq!(report.done, 2);
    assert_eq!(report.failed, 1);
    assert!(report.errors.contains_key(&bad_id));
}

/// D — a paused turn continues: scripting PauseTurn then EndTurn, the agent
/// finishes in two rounds and is not failed.
#[tokio::test]
async fn pause_turn_continues() {
    let inference = MockInference {
        script: vec![StopReason::PauseTurn, StopReason::EndTurn],
        ..Default::default()
    };
    let mut reactor = Reactor::new(
        inference,
        MemStore::default(),
        vec![agent(Behavior::Complete, 1, 0)],
    );
    let report = reactor.run().await.unwrap();

    assert_eq!(report.done, 1);
    assert_eq!(report.failed, 0);
}

/// E — turn-order invariant: after `handle`, the prompt ends in a user turn.
#[tokio::test]
async fn handle_keeps_user_tail() {
    let mut a = agent(Behavior::Complete, 2, 0);
    a.handle(message(StopReason::EndTurn)).await.unwrap();
    let last = a.prompt().messages.last().expect("non-empty prompt");
    assert_eq!(last.role, Role::User);
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
