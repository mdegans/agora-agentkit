//! Shared harness for the reactor tests: a mock transport ([`MockInference`] and
//! friends), an in-memory [`Storage`], the [`TestAgent`], and the fixtures that
//! build them. The tests themselves live in per-concern submodules that pull
//! this in with `use super::*`:
//!
//! - [`scheduling`] — round-major lockstep, the stall cap, `PauseTurn`
//!   continuation, the turn-order invariant, and mixed-cohort routing.
//! - [`errors`] — one agent's failure doesn't abort the cohort, and per-item
//!   retry classification.
//! - [`persistence`] — one bulk save/load, and partial-save recovery.
//! - [`tools`] — the tool-dispatch half of the default `handle`.
//! - [`truncation`] — the `MaxTokens` path of the default `handle`: budget
//!   bump, ceiling clamp, and stall.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use misanthropic::model::ModelInfo;
use misanthropic::prompt::Prompt;
use misanthropic::prompt::message::Role;
use misanthropic::response::{self, StopReason};
use misanthropic::tool::Tool;
use serde::Serialize;

use super::backend::{Inference, SaveError, Storage};
use super::{Agent, AgentNotFound, Control, Outcome, RetryAfter, State};
use crate::ids::AgentId;

// Reactor types the test submodules reach through `use super::*` but the harness
// itself doesn't touch — re-exported so the submodules stay a bare glob import.
pub(crate) use super::{
    ErrorKind, ErrorReport, Reactor, Report, Run, RunError, load_agents,
};

mod errors;
mod persistence;
mod scheduling;
// The tool tests build a `#[tool]`-macro tool, which needs its argument struct's
// `schemars::JsonSchema` — gated on that feature (`--all-features` covers it).
#[cfg(feature = "schemars")]
mod tools;
mod truncation;

// ---------------------------------------------------------------------------
// Shared error: serves as Agent::Error, Storage::Error, and Inference::Error.
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
enum TestError {
    #[error("tool: {0}")]
    Tool(#[from] Box<dyn std::error::Error + Send + Sync>),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("not found: {0}")]
    NotFound(#[from] AgentNotFound),
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

#[derive(
    Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize,
)]
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
    /// `Some` makes `Serialize` fail — for proving serialize failures are
    /// never silent.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "poisoned"
    )]
    poison: Option<()>,
}

/// Only reached when the field is `Some` (`skip_serializing_if` skips `None`).
fn poisoned<S: serde::Serializer>(
    _: &Option<()>,
    _: S,
) -> Result<S::Ok, S::Error> {
    Err(serde::ser::Error::custom("poisoned state"))
}

impl State for TestState {}

struct TestAgent {
    id: AgentId,
    state: TestState,
    prompt: Prompt,
    tools: misanthropic::tool::ToolBox,
    model: ModelInfo,
    /// Times `on_teardown` ran. The contract is exactly once, however the
    /// drive ends.
    teardowns: Arc<AtomicUsize>,
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
            model: model_info(false),
            teardowns: Arc::new(AtomicUsize::new(0)),
        };
        // Establish the "ends in a user turn" invariant.
        agent.push_user("start")?;
        Ok(agent)
    }

    fn id(&self) -> AgentId {
        self.id
    }

    fn state(&self) -> &TestState {
        &self.state
    }

    fn prompt(&self) -> &Prompt {
        &self.prompt
    }

    fn parts(&mut self) -> (&mut misanthropic::tool::ToolBox, &mut Prompt) {
        (&mut self.tools, &mut self.prompt)
    }

    fn model(&self) -> ModelInfo {
        self.model.clone()
    }

    /// The default, plus the counter — so a test can prove teardown ran.
    async fn on_teardown(&mut self) -> Result<(), TestError> {
        self.teardowns.fetch_add(1, Ordering::SeqCst);
        let (tools, prompt) = self.parts();
        tools.on_teardown(prompt).await?;
        Ok(())
    }

    async fn on_quiesce(
        &mut self,
        _response: &response::Message,
    ) -> Result<Control, TestError> {
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
            poison: None,
        },
    )
    .unwrap()
}

/// A [`TestAgent`] requesting a batch-capable model, so it negotiates onto the
/// round-major path.
fn batch_agent(behavior: Behavior, turns_left: usize) -> TestAgent {
    let mut a = agent(behavior, turns_left);
    a.model = model_info(true);
    a
}

/// A [`ModelInfo`] requesting (or not) the batch capability — the only field the
/// interim negotiation reads.
fn model_info(batch: bool) -> ModelInfo {
    use misanthropic::model::{Capabilities, Kind, Model};
    ModelInfo {
        id: Model::default(),
        display_name: "test-model".into(),
        capabilities: Capabilities {
            batch: batch.into(),
            ..Default::default()
        },
        max_input_tokens: 0,
        max_tokens: 0,
        kind: Kind::Model,
        created_at: chrono::DateTime::from_timestamp(0, 0).unwrap(),
    }
}

/// The [`Models`](misanthropic::model::Models) every mock transport offers: one
/// batch-capable model that [`satisfies`](ModelInfo::satisfies) both the plain
/// and batch [`model_info`] an agent requests, so negotiation routes rather than
/// rejects.
fn offered_models() -> misanthropic::model::Models {
    [model_info(true)].into_iter().collect()
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

    async fn save_raw(
        &mut self,
        id: AgentId,
        value: serde_json::Value,
    ) -> Result<(), TestError> {
        self.map.lock().unwrap().insert(id, value);
        Ok(())
    }

    async fn load_raw(
        &self,
        id: AgentId,
    ) -> Result<serde_json::Value, TestError> {
        match self.map.lock().unwrap().get(&id) {
            Some(value) => Ok(serde_json::from_value(value.clone())?),
            None => Err(AgentNotFound(id).into()),
        }
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

    async fn save_raw(
        &mut self,
        id: AgentId,
        value: serde_json::Value,
    ) -> Result<(), TestError> {
        self.map.lock().unwrap().insert(id, value);
        Ok(())
    }

    async fn load_raw(
        &self,
        id: AgentId,
    ) -> Result<serde_json::Value, TestError> {
        match self.map.lock().unwrap().get(&id) {
            Some(value) => Ok(serde_json::from_value(value.clone())?),
            None => Err(AgentNotFound(id).into()),
        }
    }

    async fn save_all_raw<It>(
        &mut self,
        items: It,
    ) -> Result<(), SaveError<TestError>>
    where
        It: ExactSizeIterator<Item = (AgentId, serde_json::Value)> + Send,
    {
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
// Mock transport: hands out a scripted response per `infer`, popping in order.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct MockInference {
    /// Responses handed out one per `infer`, in order. Draining it dry panics —
    /// an over-long drive loop should fail loudly, not silently `EndTurn`.
    script: Mutex<VecDeque<response::Message>>,
}

impl MockInference {
    /// A mock whose `infer` returns `messages` in order, one per call.
    fn scripted(messages: impl IntoIterator<Item = response::Message>) -> Self {
        Self {
            script: Mutex::new(messages.into_iter().collect()),
        }
    }

    /// `n` plain `EndTurn` responses — for drives whose content doesn't matter,
    /// only that inference is available for `n` rounds.
    fn end_turns(n: usize) -> Self {
        Self::scripted((0..n).map(|_| message(StopReason::EndTurn)))
    }
}

/// The wire string for a [`StopReason`], for building response fixtures.
fn stop_str(stop: StopReason) -> &'static str {
    match stop {
        StopReason::EndTurn => "end_turn",
        StopReason::PauseTurn => "pause_turn",
        StopReason::ToolUse => "tool_use",
        StopReason::MaxTokens => "max_tokens",
        StopReason::StopSequence => "stop_sequence",
        StopReason::Refusal => "refusal",
    }
}

fn message(stop: StopReason) -> response::Message {
    let stop = stop_str(stop);
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

    async fn infer<P>(&self, _prompt: P) -> Result<response::Message, TestError>
    where
        P: Serialize + Send,
    {
        Ok(self.script.lock().unwrap().pop_front().expect(
            "mock inference script exhausted: more infer calls than scripted",
        ))
    }

    async fn infer_batch<P>(
        &self,
        prompts: &[&P],
    ) -> Result<Vec<Result<response::Message, TestError>>, TestError>
    where
        P: Serialize + Send + Sync,
    {
        Ok(prompts
            .iter()
            .map(|_| Ok(message(StopReason::EndTurn)))
            .collect())
    }

    async fn models(&self) -> Result<misanthropic::model::Models, TestError> {
        Ok(offered_models())
    }
}

// ---------------------------------------------------------------------------
// A recording batch transport, so the round-major test can read per-round batch
// sizes after the cohort is moved into the reactor.
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

    async fn infer<P>(&self, _prompt: P) -> Result<response::Message, TestError>
    where
        P: Serialize + Send,
    {
        Ok(message(StopReason::EndTurn))
    }

    async fn infer_batch<P>(
        &self,
        prompts: &[&P],
    ) -> Result<Vec<Result<response::Message, TestError>>, TestError>
    where
        P: Serialize + Send + Sync,
    {
        self.sizes.0.lock().unwrap().push(prompts.len());
        Ok(prompts
            .iter()
            .map(|_| Ok(message(StopReason::EndTurn)))
            .collect())
    }

    async fn models(&self) -> Result<misanthropic::model::Models, TestError> {
        Ok(offered_models())
    }
}

// ---------------------------------------------------------------------------
// A transport that records *both* per-round batch sizes and single-prompt infer
// calls, so a mixed-cohort run can prove each path was exercised.
// ---------------------------------------------------------------------------

#[derive(Default, Clone)]
struct MixedRecorder {
    infer_calls: Arc<AtomicUsize>,
    batch_sizes: SharedSizes,
}

#[async_trait::async_trait]
impl Inference for MixedRecorder {
    type Error = TestError;

    async fn infer<P>(&self, _prompt: P) -> Result<response::Message, TestError>
    where
        P: Serialize + Send,
    {
        self.infer_calls.fetch_add(1, Ordering::SeqCst);
        Ok(message(StopReason::EndTurn))
    }

    async fn infer_batch<P>(
        &self,
        prompts: &[&P],
    ) -> Result<Vec<Result<response::Message, TestError>>, TestError>
    where
        P: Serialize + Send + Sync,
    {
        self.batch_sizes.0.lock().unwrap().push(prompts.len());
        Ok(prompts
            .iter()
            .map(|_| Ok(message(StopReason::EndTurn)))
            .collect())
    }

    async fn models(&self) -> Result<misanthropic::model::Models, TestError> {
        Ok(offered_models())
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

    async fn save_raw(
        &mut self,
        id: AgentId,
        value: serde_json::Value,
    ) -> Result<(), TestError> {
        self.map.lock().unwrap().insert(id, value);
        Ok(())
    }

    async fn load_raw(
        &self,
        id: AgentId,
    ) -> Result<serde_json::Value, TestError> {
        match self.map.lock().unwrap().get(&id) {
            Some(value) => Ok(serde_json::from_value(value.clone())?),
            None => Err(AgentNotFound(id).into()),
        }
    }

    async fn save_all_raw<It>(
        &mut self,
        items: It,
    ) -> Result<(), SaveError<TestError>>
    where
        It: ExactSizeIterator<Item = (AgentId, serde_json::Value)> + Send,
    {
        let mut saved = BTreeSet::new();
        let mut map = self.map.lock().unwrap();
        for (i, (id, value)) in items.enumerate() {
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
// A transport whose `models()` probe fails before succeeding — the flaky
// endpoint a run must survive without dropping the cohort.
// ---------------------------------------------------------------------------

struct FlakyModels {
    /// `models()` calls left to fail before the probe succeeds.
    failures_left: AtomicUsize,
}

impl FlakyModels {
    fn failing(n: usize) -> Self {
        Self {
            failures_left: AtomicUsize::new(n),
        }
    }
}

#[async_trait::async_trait]
impl Inference for FlakyModels {
    type Error = TestError;

    async fn infer<P>(&self, _prompt: P) -> Result<response::Message, TestError>
    where
        P: Serialize + Send,
    {
        Ok(message(StopReason::EndTurn))
    }

    async fn models(&self) -> Result<misanthropic::model::Models, TestError> {
        let failing = self
            .failures_left
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                n.checked_sub(1)
            })
            .is_ok();
        if failing {
            Err(TestError::Transient(Duration::from_millis(1)))
        } else {
            Ok(offered_models())
        }
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

    async fn infer<P>(&self, _prompt: P) -> Result<response::Message, TestError>
    where
        P: Serialize + Send,
    {
        Ok(message(StopReason::EndTurn))
    }

    async fn infer_batch<P>(
        &self,
        prompts: &[&P],
    ) -> Result<Vec<Result<response::Message, TestError>>, TestError>
    where
        P: Serialize + Send + Sync,
    {
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

    async fn models(&self) -> Result<misanthropic::model::Models, TestError> {
        Ok(offered_models())
    }
}
