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
}

impl NoteAsk {
    fn optional() -> Self {
        Self {
            required: false,
            max_tokens: 2_000,
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
        "Say a note, or null.".into()
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
        _last: &response::Message,
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
        [("note", Unanswered::Missed(why))] if why.contains("Invalid JSON")
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
            && reason.contains("Invalid JSON"),
        "{reason}"
    );
}

#[tokio::test]
async fn an_api_refusal_of_an_optional_question_skips_only_it() {
    let mut host = host();
    asked(&mut host, [NoteAsk::optional(), NoteAsk::optional()]).await;
    let refusal = reply(
        serde_json::json!([{"type": "text", "text": "No."}]),
        "refusal",
    );
    let control = handle(&mut host, refusal).await.unwrap();
    assert_eq!(control, Control::Continue);
    assert_eq!(host.unanswered, [("note", Unanswered::Refused)]);
    assert!(host.asks.is_active(), "the next question is asked");
    assert!(last_text(&host).contains("Say a note"));
}

#[tokio::test]
async fn an_api_refusal_of_a_required_question_is_a_failed_attempt() {
    let mut host = host();
    asked(&mut host, [NoteAsk::required()]).await;
    let refusal = reply(
        serde_json::json!([{"type": "text", "text": "No."}]),
        "refusal",
    );
    let control = handle(&mut host, refusal).await.unwrap();
    assert_eq!(control, Control::Stalled);
    assert!(host.unanswered.is_empty());
    assert!(last_text(&host).contains("ended as a refusal"));
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
