//! The driver on a minimal host: one question type ([`NoteAsk`]), a
//! transcript, and a record of what the host was handed.

use misanthropic::model::{Kind, Model, ModelInfo};
use misanthropic::prompt::output::Effort;
use misanthropic::tool::ToolBox;
use serde::Deserialize;

use super::*;
use crate::reactor::inference::Quirks;
use crate::reactor::{RetryAfter, State};

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct HostError(#[from] Box<dyn std::error::Error + Send + Sync>);

impl RetryAfter for HostError {}

#[derive(serde::Serialize, serde::Deserialize)]
struct NoState;

impl State for NoState {}

#[derive(Debug, PartialEq, Deserialize, schemars::JsonSchema)]
struct Note {
    note: String,
}

/// "Say a note, or null"
struct NoteAsk {
    required: bool,
    max_tokens: u32,
    anonymous: bool,
}

impl NoteAsk {
    fn optional() -> Self {
        Self {
            required: false,
            max_tokens: 2_000,
            anonymous: false,
        }
    }

    fn required() -> Self {
        Self {
            required: true,
            ..Self::optional()
        }
    }
}

impl Ask for NoteAsk {
    type Answer = Option<Note>;

    fn name(&self) -> &'static str {
        if self.required { "required" } else { "note" }
    }

    fn question(&self) -> Content {
        "Say a note, or null: {\"note\": \"<your note>\"}".into()
    }

    fn anonymous(&self) -> bool {
        self.anonymous
    }

    fn template(&self) -> Option<Value> {
        Some(serde_json::json!({"note": "An example note."}))
    }

    fn max_tokens(&self) -> u32 {
        self.max_tokens
    }

    fn required(&self) -> bool {
        self.required
    }

    fn schema(&self) -> Value {
        nullable(ask_schema::<Note>())
    }
}

struct Host {
    id: AgentId,
    prompt: Prompt,
    tools: ToolBox,
    asks: Asks<Host>,
    gauge: Option<ContextGauge>,
    quirks: Quirks,
    answers: Vec<Option<Note>>,
    asked: Vec<Asked>,
    unanswered: Vec<(&'static str, Unanswered)>,
    /// Answers to reject (as a host's own check would) before taking one
    reject: usize,
    pauses: usize,
    max_pauses: usize,
    done: bool,
    /// What `asks.is_active()` said inside each `on_pause`
    active_in_pause: Vec<bool>,
    /// `answer` errs (not the agent's doing)
    fail_answer: bool,
}

#[async_trait::async_trait]
impl Agent for Host {
    type State = NoState;
    type Context = ();
    type Error = HostError;

    fn new(id: AgentId, _: NoState, _: ()) -> Result<Self, HostError> {
        let mut prompt = Prompt::default();
        prompt.push_message((Role::User, "act")).unwrap();
        prompt
            .push_message((Role::Assistant, "Done acting."))
            .unwrap();
        Ok(Self {
            id,
            prompt,
            tools: ToolBox::new(),
            asks: Asks::default(),
            gauge: None,
            quirks: Quirks::default(),
            answers: Vec::new(),
            asked: Vec::new(),
            unanswered: Vec::new(),
            reject: 0,
            pauses: 0,
            max_pauses: 5,
            done: false,
            active_in_pause: Vec::new(),
            fail_answer: false,
        })
    }

    fn id(&self) -> AgentId {
        self.id
    }

    fn state(&self) -> &NoState {
        &NoState
    }

    fn prompt(&self) -> &Prompt {
        &self.prompt
    }

    fn parts(&mut self) -> (&mut ToolBox, &mut Prompt) {
        (&mut self.tools, &mut self.prompt)
    }

    fn model(&self) -> ModelInfo {
        ModelInfo {
            id: Model::from("claude-haiku-4-5"),
            display_name: "Test Haiku".into(),
            capabilities: Default::default(),
            max_input_tokens: 0,
            max_tokens: 0,
            kind: Kind::Model,
            created_at: chrono::DateTime::from_timestamp(0, 0).unwrap(),
        }
    }

    fn quirks(&self) -> Option<Quirks> {
        Some(self.quirks)
    }

    /// As the seed agent does: resumed under a budget, past which the
    /// partial turn is dropped and the turn stalls
    async fn on_pause(
        &mut self,
        reply: response::Message,
    ) -> Result<Control, HostError> {
        self.active_in_pause.push(self.asks.is_active());
        self.pauses += 1;
        if self.pauses > self.max_pauses {
            while self
                .prompt
                .messages
                .last()
                .is_some_and(|m| m.role == Role::Assistant)
            {
                self.prompt.messages.pop();
            }
            return Ok(Control::Stalled);
        }
        self.prompt
            .push_message(reply.inner)
            .map_err(error::<HostError>)?;
        Ok(Control::Continue)
    }
}

#[async_trait::async_trait]
impl AskHost for Host {
    fn asks(&mut self) -> &mut Asks<Self> {
        &mut self.asks
    }

    fn gauge(&self) -> Option<ContextGauge> {
        self.gauge.clone()
    }

    async fn asks_done(
        &mut self,
        _last: Option<&response::Message>,
    ) -> Result<Control, HostError> {
        self.done = true;
        Ok(Control::Done(Outcome::Complete))
    }
}

#[async_trait::async_trait]
impl Answer<NoteAsk> for Host {
    async fn answer(
        &mut self,
        _ask: &NoteAsk,
        answer: Option<Note>,
        asked: &Asked,
    ) -> Result<Next, Rejection<HostError>> {
        if self.fail_answer {
            return Err(Rejection::Error(error("the host failed")));
        }
        if self.reject > 0 {
            self.reject -= 1;
            return Err(Rejection::Retry("Rejected: try again.".into()));
        }
        self.answers.push(answer);
        self.asked.push(asked.clone());
        Ok(Next::Continue)
    }

    async fn unanswered(
        &mut self,
        ask: &NoteAsk,
        why: Unanswered,
        asked: &Asked,
    ) -> Result<Next, HostError> {
        self.unanswered.push((ask.name(), why));
        self.asked.push(asked.clone());
        Ok(if ask.required {
            Next::Done(Outcome::Failed)
        } else {
            Next::Continue
        })
    }
}

fn host() -> Host {
    Host::new(AgentId::new(), NoState, ()).unwrap()
}

fn reply(content: serde_json::Value, stop: &str) -> response::Message {
    serde_json::from_value(serde_json::json!({
        "id": "msg_test",
        "role": "assistant",
        "content": content,
        "model": "claude-haiku-4-5",
        "stop_reason": stop,
        "stop_sequence": null,
        "usage": {"input_tokens": 100, "output_tokens": 50},
    }))
    .expect("a valid reply fixture")
}

fn text(text: &str) -> response::Message {
    reply(
        serde_json::json!([{"type": "text", "text": text}]),
        "end_turn",
    )
}

fn paused() -> response::Message {
    reply(
        serde_json::json!([
            {"type": "text", "text": "Let me check."},
            {"type": "server_tool_use", "id": "srvtoolu_1",
             "name": "web_search", "input": {"query": "q"}},
        ]),
        "pause_turn",
    )
}

/// The last message's text, joined
fn last_text(host: &Host) -> String {
    let last = host.prompt.messages.last().unwrap();
    serde_json::to_string(last).unwrap()
}

async fn asked(host: &mut Host, asks: impl IntoIterator<Item = NoteAsk>) {
    for ask in asks {
        host.asks.push(ask);
    }
    let control = seat_next(host, None).await.unwrap();
    assert_eq!(control, Control::Continue);
    assert!(host.asks.is_active());
}

#[tokio::test]
async fn a_retry_then_an_answer() {
    let mut host = host();
    asked(&mut host, [NoteAsk::optional()]).await;
    assert!(last_text(&host).contains("Say a note"));
    assert_eq!(host.asks.seated(), Some("note"));

    let control = handle(&mut host, text("not json")).await.unwrap();
    assert_eq!(control, Control::Continue, "optional: no stall");
    assert!(
        last_text(&host).contains("Invalid JSON"),
        "{}",
        last_text(&host)
    );
    assert!(host.asks.is_active());

    let control = handle(&mut host, text(r#"{"note": "hi"}"#)).await.unwrap();
    assert_eq!(control, Control::Done(Outcome::Complete));
    assert!(host.done, "asks_done ran");
    assert_eq!(host.answers, [Some(Note { note: "hi".into() })]);
    let asked = &host.asked[0];
    assert_eq!(asked.attempts.len(), 1);
    assert_eq!(asked.attempts[0].raw, "not json");
    assert_eq!(asked.model.as_deref(), Some("claude-haiku-4-5"));
    assert!(!asked.constrained);
    // The answer is seated: the transcript ends on it
    assert_eq!(host.prompt.messages.last().unwrap().role, Role::Assistant);
}

#[tokio::test]
async fn null_is_an_answer() {
    let mut host = host();
    asked(&mut host, [NoteAsk::optional()]).await;
    handle(&mut host, text("null")).await.unwrap();
    assert_eq!(host.answers, [None]);
}

#[tokio::test]
async fn a_spent_optional_question_is_missed_and_the_next_is_asked() {
    let mut host = host();
    asked(&mut host, [NoteAsk::optional(), NoteAsk::optional()]).await;
    for _ in 0..2 {
        let control = handle(&mut host, text("nope")).await.unwrap();
        assert_eq!(control, Control::Continue);
    }
    let control = handle(&mut host, text("still nope")).await.unwrap();
    assert_eq!(control, Control::Continue, "on to the next question");
    assert!(matches!(
        host.unanswered.as_slice(),
        [("note", Unanswered::Missed(why))] if why == "parse"
    ));
    assert_eq!(host.asked[0].attempts.len(), 3);
    // The missed reply is seated, the next question after it
    let messages = &host.prompt.messages;
    assert_eq!(messages[messages.len() - 2].role, Role::Assistant);
    assert!(last_text(&host).contains("Say a note"));

    handle(&mut host, text(r#"{"note": "second"}"#))
        .await
        .unwrap();
    assert_eq!(
        host.answers,
        [Some(Note {
            note: "second".into()
        })]
    );
}

#[tokio::test]
async fn a_required_question_stalls_until_the_reactor_gives_up() {
    let mut host = host();
    asked(&mut host, [NoteAsk::required()]).await;
    for _ in 0..MAX_STALLS + 2 {
        let control = handle(&mut host, text("nope")).await.unwrap();
        assert_eq!(control, Control::Stalled);
    }
    assert!(host.unanswered.is_empty(), "never missed by the driver");
    let reason = host.asks.stall_reason().unwrap();
    assert!(
        reason.contains("the required question failed 5 times in a row")
            && reason.ends_with("last: parse"),
        "{reason}"
    );
}

#[tokio::test]
async fn a_pause_is_resumed_and_never_a_miss() {
    let mut host = host();
    asked(&mut host, [NoteAsk::optional()]).await;
    let before = host.prompt.messages.len();
    let control = handle(&mut host, paused()).await.unwrap();
    assert_eq!(control, Control::Continue);
    assert_eq!(host.prompt.messages.len(), before + 1, "the partial seated");
    assert!(host.asks.is_active());
    handle(&mut host, text(r#"{"note": "found it"}"#))
        .await
        .unwrap();
    assert_eq!(
        host.answers,
        [Some(Note {
            note: "found it".into()
        })]
    );
    assert!(host.asked[0].attempts.is_empty(), "a pause is no attempt");
}

#[tokio::test]
async fn a_pause_past_the_budget_is_a_failed_attempt() {
    let mut host = host();
    host.max_pauses = 0;
    asked(&mut host, [NoteAsk::required()]).await;
    let before = host.prompt.messages.len();
    let control = handle(&mut host, paused()).await.unwrap();
    assert_eq!(control, Control::Stalled);
    assert_eq!(
        host.prompt.messages.len(),
        before,
        "asked again as it stands"
    );
    assert!(last_text(&host).contains("Say a note"));
}

#[tokio::test]
async fn a_tool_call_is_answered_not_run_and_retried() {
    let mut host = host();
    asked(&mut host, [NoteAsk::optional()]).await;
    let call = reply(
        serde_json::json!([{"type": "tool_use", "id": "toolu_1",
                            "name": "agora", "input": {}}]),
        "tool_use",
    );
    handle(&mut host, call).await.unwrap();
    let messages = &host.prompt.messages;
    let results = serde_json::to_string(&messages[messages.len() - 1]).unwrap();
    assert!(results.contains(NOT_RUN_NOW), "{results}");
    assert!(results.contains("Do NOT use tools"), "{results}");
}

#[tokio::test]
async fn a_clipped_reply_is_retried_with_the_one_cut_off_note() {
    let mut host = host();
    asked(&mut host, [NoteAsk::optional()]).await;
    let clipped = reply(
        serde_json::json!([{"type": "text", "text": "{\"no"}]),
        "max_tokens",
    );
    handle(&mut host, clipped).await.unwrap();
    assert!(last_text(&host).contains("was cut off"));
}

#[tokio::test]
async fn a_rejected_answer_is_seated_then_the_note() {
    let mut host = host();
    host.reject = 1;
    asked(&mut host, [NoteAsk::optional()]).await;
    let before = host.prompt.messages.len();
    let control = handle(&mut host, text(r#"{"note": "bad"}"#)).await.unwrap();
    assert_eq!(control, Control::Continue);
    assert_eq!(host.prompt.messages.len(), before + 2);
    assert!(last_text(&host).contains("Rejected: try again."));
    handle(&mut host, text(r#"{"note": "good"}"#))
        .await
        .unwrap();
    assert_eq!(
        host.answers,
        [Some(Note {
            note: "good".into()
        })]
    );
}

/// A gauge with `room` tokens of phase room
fn gauge_with_room(room: u64) -> ContextGauge {
    let gauge = ContextGauge::new(100_000, 0);
    gauge.set(100_000 - gauge::PHASE_PROMPT_TOKENS - room);
    gauge
}

#[tokio::test]
async fn no_room_for_a_required_question_ends_the_session() {
    let mut host = host();
    host.gauge = Some(gauge_with_room(500));
    host.asks.push(NoteAsk::required());
    let before = host.prompt.messages.len();
    let control = seat_next(&mut host, None).await.unwrap();
    assert_eq!(control, Control::Done(Outcome::Failed));
    assert_eq!(host.unanswered, [("required", Unanswered::NoRoom)]);
    assert_eq!(host.prompt.messages.len(), before, "nothing seated");
}

#[tokio::test]
async fn a_required_question_is_clamped_to_the_room_left() {
    let mut host = host();
    host.gauge = Some(gauge_with_room(1_500));
    asked(&mut host, [NoteAsk::required()]).await;
    assert_eq!(host.prompt.max_tokens.get(), 1_500);
}

#[tokio::test]
async fn an_optional_question_that_would_not_fit_is_not_asked() {
    let mut host = host();
    host.gauge = Some(gauge_with_room(1_500));
    host.asks.push(NoteAsk::optional());
    host.asks.push(NoteAsk {
        max_tokens: 1_200,
        ..NoteAsk::optional()
    });
    let control = seat_next(&mut host, None).await.unwrap();
    assert_eq!(control, Control::Continue, "the second fits");
    assert_eq!(host.unanswered, [("note", Unanswered::NoRoom)]);
    assert_eq!(host.prompt.max_tokens.get(), 1_200);
}

#[tokio::test]
async fn near_the_edge_a_failed_reply_is_left_out() {
    let mut host = host();
    host.gauge = Some(gauge_with_room(2_500));
    asked(&mut host, [NoteAsk::required()]).await;
    let before = host.prompt.messages.len();
    // The reply's usage puts the gauge past the edge for a whole retry
    let gauge = host.gauge.clone().unwrap();
    let mut garbage = text("nope");
    garbage.usage.input_tokens = gauge.get() + 600;
    garbage.usage.output_tokens = 1_000;
    handle(&mut host, garbage).await.unwrap();
    assert_eq!(
        host.prompt.messages.len(),
        before,
        "the note joined the turn"
    );
    assert!(last_text(&host).contains("Invalid JSON"));
}

#[tokio::test]
async fn constrained_only_where_cache_safe_keeping_the_effort() {
    let mut host = host();
    host.prompt = std::mem::take(&mut host.prompt).effort(Effort::Medium);
    asked(&mut host, [NoteAsk::optional()]).await;
    let config = host.prompt.output_config.clone().unwrap();
    assert!(config.format.is_none(), "not on Anthropic");
    assert_eq!(config.effort, Some(Effort::Medium));

    let mut host = self::host();
    host.prompt = std::mem::take(&mut host.prompt).effort(Effort::Medium);
    host.quirks.output_config_cache_safe = true;
    asked(&mut host, [NoteAsk::optional()]).await;
    let config = host.prompt.output_config.clone().unwrap();
    let wire = serde_json::to_value(&config).unwrap();
    assert_eq!(wire["format"]["schema"], NoteAsk::optional().schema());
    assert_eq!(config.effort, Some(Effort::Medium));
    handle(&mut host, text("null")).await.unwrap();
    assert!(host.asked[0].constrained);
}

#[tokio::test]
async fn the_survey_mark_is_where_the_question_began() {
    let mut host = host();
    let before = Mark::at(&host.prompt);
    asked(&mut host, [NoteAsk::optional()]).await;
    handle(&mut host, text("null")).await.unwrap();
    let mark = host.asked[0].mark;
    assert_eq!(mark, before);
    mark.redact(&mut host.prompt);
    assert!(
        !serde_json::to_string(&host.prompt)
            .unwrap()
            .contains("Say a note")
    );
}

// --- parse_lenient ---

fn note(text: &str) -> Result<Option<Note>, String> {
    parse_lenient(text)
}

fn some(text: &str) -> Option<Note> {
    Some(Note { note: text.into() })
}

#[test]
fn fences_in_any_case_or_tag() {
    for fenced in [
        "```json\n{\"note\": \"a\"}\n```",
        "```JSON\n{\"note\": \"a\"}\n```",
        "```\n{\"note\": \"a\"}\n```",
        "```jsonc{\"note\": \"a\"}```",
        "Here it is:\n```Json\n{\"note\": \"a\"}\n```\nThanks.",
    ] {
        assert_eq!(note(fenced).unwrap(), some("a"), "{fenced}");
    }
    assert_eq!(note("```json\nnull\n```").unwrap(), None);
}

#[test]
fn a_one_key_wrapper_is_unwrapped() {
    assert_eq!(
        note(r#"{"answer_offer": {"note": "a"}}"#).unwrap(),
        some("a")
    );
    assert_eq!(
        note("```json\n{\"answer\": {\"note\": \"a\"}}\n```").unwrap(),
        some("a")
    );
}

#[test]
fn prose_around_one_object() {
    assert_eq!(
        note("Sure. {\"note\": \"a {b}\"} That's all.").unwrap(),
        some("a {b}")
    );
}

#[test]
fn two_candidate_objects_are_ambiguous() {
    let e = note(r#"First {"note": "a"} or {"note": "b"}"#).unwrap_err();
    assert!(e.contains("2 JSON objects"), "{e}");
}

#[test]
fn empty_is_null() {
    assert_eq!(note("").unwrap(), None);
    assert_eq!(note("  \n").unwrap(), None);
    assert!(parse_lenient::<Note>("").is_err(), "null is no Note");
}

#[test]
fn errors_name_the_field_without_a_position() {
    assert_eq!(
        parse_lenient::<Note>(r#"{"note": 42}"#).unwrap_err(),
        "Invalid JSON at `note`: invalid type: integer `42`, expected a string"
    );
    // One object in prose, of the wrong shape: its error, not the prose's
    let e = parse_lenient::<Note>(r#"Here: {"note": 42}"#).unwrap_err();
    assert!(e.starts_with("Invalid JSON at `note`"), "{e}");
    let e = parse_lenient::<Note>("not json at all").unwrap_err();
    assert!(e.starts_with("Invalid JSON"), "{e}");
    assert!(!e.contains("line"), "{e}");
}

#[test]
fn unquoted_keys_are_not_repaired() {
    assert!(parse_lenient::<Note>("{note: \"a\"}").is_err());
}

// --- schemas ---

#[test]
fn ask_schemas_are_constrainable() {
    assert_constrainable(&ask_schema::<Note>());
    assert_constrainable(&nullable(ask_schema::<Note>()));
    let schema = ask_schema::<Note>();
    assert!(schema.get("$schema").is_none() && schema.get("title").is_none());
}

#[test]
#[should_panic(expected = "a root type array holding null")]
fn an_option_derived_schema_is_not() {
    assert_constrainable(&misanthropic::prompt::output::schema_for::<
        Option<Note>,
    >());
}

#[test]
#[should_panic(expected = "named for reasoning")]
fn a_reasoning_field_is_not() {
    #[derive(schemars::JsonSchema)]
    #[allow(dead_code)]
    struct Bad {
        reasoning: String,
    }
    assert_constrainable(&ask_schema::<Bad>());
}

#[test]
fn pattern_is_stripped() {
    #[derive(schemars::JsonSchema)]
    #[allow(dead_code)]
    struct Patterned {
        #[schemars(regex(pattern = r"^GOV-\d{4}-\d{4}$"))]
        id: String,
    }
    let schema = ask_schema::<Patterned>();
    assert_constrainable(&schema);
    assert!(schema["properties"]["id"].get("pattern").is_none());
}

fn refusal() -> response::Message {
    reply(
        serde_json::json!([{"type": "text", "text": "REFUSED TURN"}]),
        "refusal",
    )
}

/// Any API refusal ends the session as failed (Steward, 2026-11-06), an
/// optional question's as much as a required one's: the host is told, the
/// refused turn is not seated, and nothing after it is asked
#[tokio::test]
async fn an_api_refusal_ends_the_session_failed() {
    for ask in [NoteAsk::optional(), NoteAsk::required()] {
        let mut host = host();
        asked(&mut host, [ask, NoteAsk::optional()]).await;
        let control = handle(&mut host, refusal()).await.unwrap();
        assert_eq!(control, Control::Done(Outcome::Failed));
        assert!(matches!(
            host.unanswered.as_slice(),
            [(_, Unanswered::Refused { response_id })] if response_id == "msg_test"
        ));
        assert!(!host.done, "asks_done never ran");
        let request = serde_json::to_string(&host.prompt).unwrap();
        assert!(!request.contains("REFUSED TURN"), "{request}");
    }
}

/// An anonymous answer's failures are kept, logged and handed on as a
/// category only: serde's messages quote the value they refused
#[tokio::test]
async fn an_anonymous_questions_failures_carry_no_words() {
    let mut host = host();
    let ask = NoteAsk {
        anonymous: true,
        ..NoteAsk::optional()
    };
    asked(&mut host, [ask]).await;
    for _ in 0..3 {
        handle(&mut host, text(r#"{"note": ["my", "words"]}"#))
            .await
            .unwrap();
    }
    // The agent was shown the whole note
    let seen = serde_json::to_string(&host.prompt).unwrap();
    assert!(seen.contains("Invalid JSON"), "{seen}");
    assert_eq!(
        host.unanswered,
        [("note", Unanswered::Missed("parse at `note`".into()))]
    );
    for attempt in &host.asked[0].attempts {
        assert_eq!(attempt.failure, "parse at `note`");
        assert!(attempt.raw.is_empty());
    }
}

#[tokio::test]
async fn an_anonymous_stall_reason_names_only_the_category() {
    let mut host = host();
    let ask = NoteAsk {
        anonymous: true,
        ..NoteAsk::required()
    };
    asked(&mut host, [ask]).await;
    handle(&mut host, text(r#"{"note": 42}"#)).await.unwrap();
    let reason = host.asks.stall_reason().unwrap();
    assert!(reason.ends_with("last: parse at `note`"), "{reason}");
}

/// The question's own example, copied, is a retry, whether its placeholder
/// is from the text (`<your note>`) or the template
#[tokio::test]
async fn the_questions_example_is_no_answer() {
    for copied in [
        r#"{"note": "<your note>"}"#,
        r#"Here: {"note": "An example note."}"#,
        "```json\n{\"note\": \" <your note> \"}\n```",
    ] {
        let mut host = host();
        asked(&mut host, [NoteAsk::optional()]).await;
        handle(&mut host, text(copied)).await.unwrap();
        assert!(host.answers.is_empty(), "{copied}");
        assert!(last_text(&host).contains("example shape"), "{copied}");
    }
    // Mentioning the placeholder inside a real answer is fine
    let mut host = host();
    asked(&mut host, [NoteAsk::optional()]).await;
    handle(&mut host, text(r#"{"note": "I won't write <your note>."}"#))
        .await
        .unwrap();
    assert_eq!(host.answers.len(), 1);
}

/// A question's format is cleared when it is done, its effort kept
#[tokio::test]
async fn the_format_goes_with_its_question() {
    let mut host = host();
    host.prompt = std::mem::take(&mut host.prompt).effort(Effort::Medium);
    host.quirks.output_config_cache_safe = true;
    asked(&mut host, [NoteAsk::optional()]).await;
    assert!(host.prompt.output_config.as_ref().unwrap().format.is_some());
    handle(&mut host, text("null")).await.unwrap();
    let config = host.prompt.output_config.clone().unwrap();
    assert!(config.format.is_none());
    assert_eq!(config.effort, Some(Effort::Medium));
}

/// Where `output_config` is not cache-safe, a retry's request is the
/// question's request extended: the same system, tools, output_config and
/// thinking, the same messages as a prefix, only the tail appended (and
/// `max_tokens`, which the cache doesn't key on)
#[tokio::test]
async fn a_retry_extends_the_questions_request() {
    use misanthropic::prompt::thinking::Thinking;
    let mut host = host();
    host.prompt = std::mem::take(&mut host.prompt)
        .system("You are a test.")
        .thinking(Thinking::adaptive())
        .effort(Effort::Medium);
    host.gauge = Some(gauge_with_room(50_000));
    asked(&mut host, [NoteAsk::required()]).await;
    let first = host.prompt.clone();
    for _ in 0..2 {
        handle(&mut host, text("not json")).await.unwrap();
        let retry = host.prompt.clone();
        assert_eq!(retry.system, first.system);
        assert_eq!(retry.tools, first.tools);
        assert_eq!(retry.output_config, first.output_config);
        assert_eq!(retry.thinking, first.thinking);
        assert_eq!(retry.tool_choice, first.tool_choice);
        assert_eq!(
            retry.messages[..first.messages.len()],
            first.messages[..],
            "the question's request is a prefix of the retry's"
        );
        assert!(retry.messages.len() > first.messages.len());
    }
}

/// A pause resumed through the default `Agent::on_pause`, whose partial turn
/// holds no server tool call, then answered: the answer continues the
/// partial turn rather than following it as a second assistant turn
/// (the runner's offers, 0.157 review)
#[tokio::test]
async fn a_pause_resumed_by_the_default_then_answered() {
    let mut host = host();
    host.max_pauses = 5;
    asked(&mut host, [NoteAsk::optional(), NoteAsk::optional()]).await;
    let partial = reply(
        serde_json::json!([{"type": "text", "text": "Thinking."}]),
        "pause_turn",
    );
    // Resumed as the default on_pause does: the partial is seated
    let control = handle(&mut host, partial).await.unwrap();
    assert_eq!(control, Control::Continue);
    let control = handle(&mut host, text(r#"{"note": "after"}"#))
        .await
        .unwrap();
    assert_eq!(control, Control::Continue, "the next question is asked");
    assert_eq!(
        host.answers,
        [Some(Note {
            note: "after".into()
        })]
    );
    // And a refusal after a pause leaves the partial out too
    let partial = reply(
        serde_json::json!([{"type": "text", "text": "PARTIAL TWO"}]),
        "pause_turn",
    );
    handle(&mut host, partial).await.unwrap();
    let control = handle(&mut host, refusal()).await.unwrap();
    let request = serde_json::to_string(&host.prompt).unwrap();
    assert!(!request.contains("PARTIAL TWO"), "{request}");
    assert_eq!(host.prompt.messages.last().unwrap().role, Role::User);
    assert_eq!(control, Control::Done(Outcome::Failed));
}

/// Every question skipped for room, with no reply: `asks_done(None)`
#[tokio::test]
async fn a_queue_skipped_whole_ends_cleanly() {
    let mut host = host();
    host.gauge = Some(gauge_with_room(500));
    host.asks.push(NoteAsk::optional());
    let control = seat_next(&mut host, None).await.unwrap();
    assert_eq!(control, Control::Done(Outcome::Complete));
    assert!(host.done);
}

/// A question is active from its seating until it is done, its reply's
/// taking included: the host's `on_pause`, called from inside `take`, sees
/// it (the runner needed its own flag for this)
#[tokio::test]
async fn a_question_is_active_while_its_reply_is_taken() {
    let mut host = host();
    asked(&mut host, [NoteAsk::optional()]).await;
    handle(&mut host, paused()).await.unwrap();
    assert_eq!(host.active_in_pause, [true]);
    assert_eq!(host.asks.seated(), Some("note"));
    handle(&mut host, text("null")).await.unwrap();
    assert!(!host.asks.is_active(), "done");
    assert_eq!(host.asks.seated(), None);
}

/// A take that errs leaves no question active: a later reply errs too,
/// rather than finding no head
#[tokio::test]
async fn an_error_while_taking_leaves_nothing_active() {
    let mut host = host();
    host.fail_answer = true;
    asked(&mut host, [NoteAsk::optional()]).await;
    assert!(handle(&mut host, text("null")).await.is_err());
    assert!(!host.asks.is_active());
    assert!(handle(&mut host, text("null")).await.is_err(), "no panic");
}

/// The `raw` of every `ask_attempt_raw` event, at any level, and the level
#[derive(Clone, Default)]
struct RawEvents(
    std::sync::Arc<std::sync::Mutex<Vec<(tracing::Level, String)>>>,
);

impl tracing::Subscriber for RawEvents {
    fn register_callsite(
        &self,
        _: &'static tracing::Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        tracing::subscriber::Interest::always()
    }
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(
        &self,
        _: &tracing::span::Id,
        _: &tracing::span::Id,
    ) {
    }
    fn event(&self, event: &tracing::Event<'_>) {
        #[derive(Default)]
        struct Visit {
            raw_event: bool,
            raw: Option<String>,
            all: String,
        }
        impl tracing::field::Visit for Visit {
            fn record_str(
                &mut self,
                field: &tracing::field::Field,
                value: &str,
            ) {
                if field.name() == "event_type" && value == "ask_attempt_raw" {
                    self.raw_event = true;
                }
                self.all.push_str(value);
            }
            fn record_debug(
                &mut self,
                field: &tracing::field::Field,
                value: &dyn std::fmt::Debug,
            ) {
                let value = format!("{value:?}");
                if field.name() == "raw" {
                    self.raw = Some(value.clone());
                }
                self.all.push_str(&value);
            }
        }
        let mut visit = Visit::default();
        event.record(&mut visit);
        let level = *event.metadata().level();
        if visit.raw_event {
            self.0
                .lock()
                .unwrap()
                .push((level, visit.raw.unwrap_or_default()));
        } else if level <= tracing::Level::INFO {
            // Above DEBUG, no event carries a reply's words
            assert!(!visit.all.contains("SECRET WORDS"), "{}", visit.all);
        }
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

/// An unusable reply's full text is logged at DEBUG only, as
/// `ask_attempt_raw`; an anonymous answer's (the survey's) never
#[tokio::test]
async fn an_unusable_replys_text_is_logged_at_debug_only() {
    let events = RawEvents::default();
    let _guard = tracing::subscriber::set_default(events.clone());
    tracing::callsite::rebuild_interest_cache();

    let mut host = host();
    asked(&mut host, [NoteAsk::optional()]).await;
    handle(&mut host, text("SECRET WORDS, not json"))
        .await
        .unwrap();
    {
        let raw = events.0.lock().unwrap();
        assert_eq!(raw.len(), 1);
        assert_eq!(raw[0].0, tracing::Level::DEBUG);
        assert!(raw[0].1.contains("SECRET WORDS, not json"), "{:?}", raw[0]);
    }

    let mut host = self::host();
    let ask = NoteAsk {
        anonymous: true,
        ..NoteAsk::optional()
    };
    asked(&mut host, [ask]).await;
    handle(&mut host, text("SECRET WORDS again")).await.unwrap();
    assert_eq!(events.0.lock().unwrap().len(), 1, "nothing for the survey");
}
