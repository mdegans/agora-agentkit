//! The questions a seed session closes with, as [`Ask`]s: the memory
//! rewrite (required), a rare deep soul mutation, an evolution-log entry and
//! the anonymous survey (optional, each answered `null` when there is
//! nothing to say, but the mutation). [`SeedAgent`] takes their answers.
//!
//! Each parse target is also the question's schema, sent as `output_config`
//! where that is cache-safe ([`crate::reactor::ask`]); elsewhere the
//! question's JSON-shape block and the parser carry the contract alone.

use chrono::Utc;
use misanthropic::prompt::message::Content;
use serde::Deserialize;

use super::output;
use super::soul::{ITEM_MAX, PROSE_MAX, SoulDraft};
use super::{
    EvolutionRequest, Feedback, Memory, Phase, SeedAgent, SeedError,
    ShortString, Soul,
};
use crate::reactor::ask::{
    Answer, Ask, Asked, Mark, Next, Rejection, Unanswered, ask_schema,
    nullable, parse_lenient,
};
use crate::reactor::{Control, Outcome, SessionFailure};
use crate::requests::SubmitFeedbackPayload;

/// The parts of a seed session, by name: acting, then the closing
/// questions. One set, for [`Ask::name`](crate::reactor::ask::Ask::name),
/// the `during` of a [`SessionFailure`] and [`SeedAgent::unanswered_asks`],
/// so a runner matches on these rather than on strings
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionPart {
    /// The tool rounds
    Acting,
    /// The memory rewrite (required)
    Memory,
    /// A deep soul mutation
    Soul,
    /// An evolution-log entry
    Evolution,
    /// The anonymous survey
    Survey,
}

impl SessionPart {
    /// Every part, in session order
    pub const ALL: [Self; 5] = [
        Self::Acting,
        Self::Memory,
        Self::Soul,
        Self::Evolution,
        Self::Survey,
    ];

    /// The name logs, `during` and [`Ask::name`] use
    ///
    /// [`Ask::name`]: crate::reactor::ask::Ask::name
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Acting => "acting",
            Self::Memory => "memory",
            Self::Soul => "soul",
            Self::Evolution => "evolution",
            Self::Survey => "survey",
        }
    }

    /// The part named `name`, if it is one
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|part| part.as_str() == name)
    }
}

impl std::fmt::Display for SessionPart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Replace a schema's descriptions with ones written for the model: its
/// root's, and each named property's. The types' Rust docs stay for Rust
/// (beta round 20: the survey's schema spoke of `output_config` and
/// teardown to the model)
fn describe(
    mut schema: serde_json::Value,
    root: &str,
    fields: &[(&str, &str)],
) -> serde_json::Value {
    schema["description"] = root.into();
    for (field, text) in fields {
        if let Some(prop) = schema
            .get_mut("properties")
            .and_then(|p| p.get_mut(*field))
            .and_then(|p| p.as_object_mut())
        {
            prop.insert("description".into(), (*text).into());
        }
    }
    schema
}

// The soul question's answer on the wire: a `Soul` without its evolution
// log, which the system keeps (a schema declaring it would have a
// constrained model write one, only for it to be thrown away). `Soul`
// itself is persisted state and stays as it is. Plain comments, not doc
// comments: the descriptions below are what the model reads.
#[derive(Deserialize, schemars::JsonSchema)]
#[schemars(description = "Your rewritten SOUL: who you are now.")]
struct SoulRewrite {
    #[schemars(
        description = "Your name as it appears on Agora. Lowercase, hyphenated. Unchanged."
    )]
    name: ShortString<64>,
    #[schemars(
        description = "Who you are, in your own voice. A few sentences."
    )]
    identity: ShortString<PROSE_MAX>,
    #[schemars(
        description = "What you care about: one value per entry, 3 to 5."
    )]
    values: Vec<ShortString<ITEM_MAX>>,
    #[schemars(description = "What you want to talk about, and where.")]
    interests: super::Interests,
    #[schemars(
        description = "How you write: tone, register, characteristic phrases."
    )]
    voice: ShortString<PROSE_MAX>,
    #[schemars(
        description = "Hard limits on what you do. Optional: leave it out for none."
    )]
    #[serde(default)]
    boundaries: Option<ShortString<PROSE_MAX>>,
}

impl From<SoulRewrite> for Soul {
    fn from(rewrite: SoulRewrite) -> Self {
        Soul {
            name: rewrite.name,
            identity: rewrite.identity,
            values: rewrite.values,
            interests: rewrite.interests,
            voice: rewrite.voice,
            boundaries: rewrite.boundaries,
            // The system re-attaches the prior log (`apply_mutation`)
            evolution_log: Vec::new(),
        }
    }
}

/// Log the fields a last attempt had clipped to fit
fn log_clipped(agent: &str, ask: &'static str, fields: &[String]) {
    if !fields.is_empty() {
        tracing::warn!(
            event_type = "phase_output_clipped",
            agent,
            ask,
            fields = ?fields,
            "last attempt over length: clipped at a sentence boundary \
             rather than lose the answer"
        );
    }
}

/// The memory rewrite: what the session is for. Without it the session
/// fails
pub(crate) struct MemoryAsk {
    pub max_tokens: u32,
}

impl Ask for MemoryAsk {
    type Answer = Memory;

    fn name(&self) -> &'static str {
        SessionPart::Memory.as_str()
    }

    fn question(&self) -> Content {
        output::MEMORY_REWRITE_MESSAGE.into()
    }

    fn max_tokens(&self) -> u32 {
        self.max_tokens
    }

    fn required(&self) -> bool {
        true
    }

    fn schema(&self) -> serde_json::Value {
        describe(
            ask_schema::<Memory>(),
            "Your rewritten memory.",
            &[(
                "content",
                "Your whole memory, as markdown, under 2000 words. Other \
                 agents can't see it; your operator can.",
            )],
        )
    }

    fn template(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "content": "<your full memory as freeform markdown text>"
        }))
    }
}

/// A deep soul mutation: the whole SOUL rewritten. The system re-attaches
/// the prior evolution log and stamps an entry
pub(crate) struct SoulAsk {
    pub question: String,
    pub max_tokens: u32,
    /// For the log, when a last attempt is clipped
    pub agent: String,
}

impl Ask for SoulAsk {
    type Answer = Soul;

    fn name(&self) -> &'static str {
        SessionPart::Soul.as_str()
    }

    fn question(&self) -> Content {
        self.question.clone().into()
    }

    fn max_tokens(&self) -> u32 {
        self.max_tokens
    }

    fn schema(&self) -> serde_json::Value {
        ask_schema::<SoulRewrite>()
    }

    /// The example the question shows; its `"..."`s are the placeholders
    fn template(&self) -> Option<serde_json::Value> {
        Some(output::soul_example(&self.agent))
    }

    /// The last try: an over-length field is clipped at a sentence
    /// boundary rather than the whole rewrite lost (tango-aether,
    /// 2026-10-01)
    fn parse(&self, text: &str, last: bool) -> Result<Soul, String> {
        parse_lenient::<SoulRewrite>(text)
            .map(Soul::from)
            .or_else(|e| {
                if !last {
                    return Err(e);
                }
                let draft: SoulDraft = parse_lenient(text).map_err(|_| e)?;
                let (soul, clipped) = draft.into_clipped();
                log_clipped(&self.agent, self.name(), &clipped);
                Ok(soul)
            })
    }
}

/// An evolution-log entry, or `null` for no change
pub(crate) struct EvolutionAsk {
    pub max_tokens: u32,
    pub agent: String,
}

impl Ask for EvolutionAsk {
    type Answer = Option<EvolutionRequest>;

    fn name(&self) -> &'static str {
        SessionPart::Evolution.as_str()
    }

    fn question(&self) -> Content {
        output::EVOLUTION_MESSAGE.into()
    }

    fn max_tokens(&self) -> u32 {
        self.max_tokens
    }

    fn schema(&self) -> serde_json::Value {
        nullable(describe(
            ask_schema::<EvolutionRequest>(),
            "An entry for your evolution log, or null if nothing changed.",
            &[("note", "How you changed, in a sentence or two.")],
        ))
    }

    fn template(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({"note": "<your change here>"}))
    }

    /// The last try: an over-length note is clipped at a sentence boundary
    fn parse(
        &self,
        text: &str,
        last: bool,
    ) -> Result<Option<EvolutionRequest>, String> {
        #[derive(Deserialize)]
        struct Draft {
            note: String,
        }
        parse_lenient(text).or_else(|e| {
            if !last {
                return Err(e);
            }
            let draft: Option<Draft> = parse_lenient(text).map_err(|_| e)?;
            Ok(draft.map(|draft| {
                let (note, cut) =
                    ShortString::<ITEM_MAX>::clipped(draft.note.trim());
                if cut {
                    log_clipped(&self.agent, self.name(), &["note".into()]);
                }
                EvolutionRequest { note }
            }))
        })
    }
}

/// Anonymous feedback for the developers, or `null` for none
pub(crate) struct SurveyAsk {
    pub max_tokens: u32,
    pub agent: String,
}

impl Ask for SurveyAsk {
    type Answer = Option<Feedback>;

    fn name(&self) -> &'static str {
        SessionPart::Survey.as_str()
    }

    fn question(&self) -> Content {
        output::survey_message().into()
    }

    fn max_tokens(&self) -> u32 {
        self.max_tokens
    }

    fn schema(&self) -> serde_json::Value {
        nullable(describe(
            ask_schema::<Feedback>(),
            "Your feedback to Agora's developers, or null for none. With \
             contact_me false it is anonymous: the question and your answer \
             are removed from your transcript.",
            &[
                ("text", "Your feedback: a bug, a feature, anything."),
                (
                    "contact_me",
                    "True to be contacted about it: the exchange then stays \
                     in your transcript, so the developers know it was you.",
                ),
            ],
        ))
    }

    /// Feedback is anonymous: no failure's message (which can quote it)
    /// reaches a log, an alert or a stall reason
    fn anonymous(&self) -> bool {
        true
    }

    fn template(&self) -> Option<serde_json::Value> {
        Some(
            serde_json::json!({"text": "<feedback here>", "contact_me": false}),
        )
    }

    /// The last try: an over-length `text` is clipped rather than lost to
    /// the server's length check (iota-aether, 2026-10-05). The transcript
    /// keeps the answer as written.
    fn parse(
        &self,
        text: &str,
        last: bool,
    ) -> Result<Option<Feedback>, String> {
        #[derive(Deserialize)]
        struct Draft {
            text: String,
            contact_me: bool,
        }
        parse_lenient(text).or_else(|e| {
            if !last {
                return Err(e);
            }
            let draft: Option<Draft> = parse_lenient(text).map_err(|_| e)?;
            Ok(draft.map(|draft| {
                let (text, cut) = ShortString::clipped(&draft.text);
                if cut {
                    log_clipped(&self.agent, self.name(), &["text".into()]);
                }
                Feedback {
                    text,
                    contact_me: draft.contact_me,
                }
            }))
        })
    }
}

impl SeedAgent {
    /// Record a question that ended without an answer, for
    /// [`SeedAgent::unanswered_asks`]
    fn record_unanswered(
        &mut self,
        ask: &'static str,
        why: Unanswered,
        asked: &Asked,
    ) {
        // The two ways a question fails the session (any refusal; no room
        // for the memory rewrite), for the agent's notice next session
        match &why {
            Unanswered::Refused { response_id } => {
                self.failure = Some(SessionFailure::Refused {
                    during: ask.to_string(),
                    round: None,
                    model: asked.model.clone().unwrap_or_default(),
                    response_id: response_id.clone(),
                });
            }
            Unanswered::NoRoom if ask == SessionPart::Memory.as_str() => {
                self.failure = Some(SessionFailure::NoRoom {
                    during: ask.to_string(),
                });
            }
            _ => {}
        }
        self.unanswered.push((ask, why));
    }

    /// The session ran to a clean end
    fn finished(&mut self) -> Next {
        self.state.completed = true;
        Next::Done(Outcome::Complete)
    }
}

#[async_trait::async_trait]
impl Answer<MemoryAsk> for SeedAgent {
    async fn answer(
        &mut self,
        _ask: &MemoryAsk,
        memory: Memory,
        _asked: &Asked,
    ) -> Result<Next, Rejection<SeedError>> {
        if let Err(e) = self.state.memory.update(memory.content) {
            return Err(Rejection::Retry(format!(
                "Memory rejected: {e}. Try again."
            )));
        }
        self.state.last_cycle_at = Some(Utc::now());
        self.queue_after_memory();
        Ok(Next::Continue)
    }

    /// No room for it (the only way a required question ends unanswered):
    /// the session fails
    async fn unanswered(
        &mut self,
        ask: &MemoryAsk,
        why: Unanswered,
        asked: &Asked,
    ) -> Result<Next, SeedError> {
        self.record_unanswered(ask.name(), why, asked);
        Ok(Next::Done(Outcome::Failed))
    }
}

#[async_trait::async_trait]
impl Answer<SoulAsk> for SeedAgent {
    async fn answer(
        &mut self,
        _ask: &SoulAsk,
        soul: Soul,
        _asked: &Asked,
    ) -> Result<Next, Rejection<SeedError>> {
        let warnings = soul.validate_communities(&self.communities);
        if !warnings.is_empty() {
            let bad: Vec<String> =
                warnings.iter().map(|w| w.message.clone()).collect();
            return Err(Rejection::Retry(format!(
                "Invalid communities: {}. Valid slugs: {:?}. Try again.",
                bad.join("; "),
                self.communities,
            )));
        }
        self.apply_mutation(soul);
        Ok(Next::Continue)
    }

    async fn unanswered(
        &mut self,
        ask: &SoulAsk,
        why: Unanswered,
        asked: &Asked,
    ) -> Result<Next, SeedError> {
        self.record_unanswered(ask.name(), why, asked);
        Ok(Next::Continue)
    }
}

#[async_trait::async_trait]
impl Answer<EvolutionAsk> for SeedAgent {
    async fn answer(
        &mut self,
        _ask: &EvolutionAsk,
        request: Option<EvolutionRequest>,
        _asked: &Asked,
    ) -> Result<Next, Rejection<SeedError>> {
        let note = request
            .map(|r| r.note.into_inner())
            .filter(|note| !note.trim().is_empty());
        if let Some(note) = note
            && let Err(e) = self.state.soul.push_evolution(note)
        {
            return Err(Rejection::Retry(format!(
                "Evolution note rejected: {e}. Try again."
            )));
        }
        Ok(Next::Continue)
    }

    async fn unanswered(
        &mut self,
        ask: &EvolutionAsk,
        why: Unanswered,
        asked: &Asked,
    ) -> Result<Next, SeedError> {
        self.record_unanswered(ask.name(), why, asked);
        Ok(Next::Continue)
    }
}

/// The survey is always the session's last question: answered or not, the
/// session is complete after it
#[async_trait::async_trait]
impl Answer<SurveyAsk> for SeedAgent {
    async fn answer(
        &mut self,
        _ask: &SurveyAsk,
        feedback: Option<Feedback>,
        asked: &Asked,
    ) -> Result<Next, Rejection<SeedError>> {
        // Seated either way, so the transcript only ever grows while the
        // session runs; an anonymous exchange is taken out at teardown,
        // after the last request.
        self.survey_mark = Some(asked.mark);
        self.contact_me = feedback.as_ref().is_some_and(|f| f.contact_me);
        if let Some(feedback) = feedback {
            let payload = SubmitFeedbackPayload {
                body: feedback.text.to_string(),
            };
            // Best-effort: a failed survey submission shouldn't fail a
            // session whose real work already landed.
            match self
                .ctx
                .client
                .submit_feedback(self.id, &payload, &self.key)
                .await
            {
                Ok(receipt) if self.contact_me => {
                    if receipt.id.is_none() {
                        tracing::warn!(
                            agent = %self.state.soul.name,
                            agent_id = %self.id,
                            status = %receipt.status,
                            "the server returned no feedback id, so no \
                             contact request can be filed"
                        );
                    }
                    self.contact_feedback = receipt.id;
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(
                    agent = %self.state.soul.name,
                    error = %e,
                    "feedback submission failed"
                ),
            }
        }
        Ok(self.finished())
    }

    /// Whatever was seated of it is redacted at teardown, failed attempts
    /// included. Set on every path (for `NoRoom` before seating, the mark
    /// is the transcript's end, and nothing follows it)
    async fn unanswered(
        &mut self,
        ask: &SurveyAsk,
        why: Unanswered,
        asked: &Asked,
    ) -> Result<Next, SeedError> {
        self.survey_mark = Some(asked.mark);
        // A refusal fails the session: not complete
        let refused = matches!(why, Unanswered::Refused { .. });
        self.record_unanswered(ask.name(), why, asked);
        if refused {
            return Ok(Next::Done(Outcome::Failed));
        }
        Ok(self.finished())
    }

    /// The redaction point is set the moment the question is seated, so a
    /// session that ends in an error with the survey open is redacted too
    fn seated(&mut self, _ask: &SurveyAsk, mark: Mark) {
        self.survey_mark = Some(mark);
    }
}

#[async_trait::async_trait]
impl crate::reactor::ask::AskHost for SeedAgent {
    fn asks(&mut self) -> &mut crate::reactor::ask::Asks<Self> {
        &mut self.asks
    }

    fn gauge(&self) -> Option<super::ContextGauge> {
        Some(self.context.clone())
    }

    fn agent_name(&self) -> Option<String> {
        Some(self.state.soul.name.to_string())
    }

    fn left_out(
        &mut self,
        ask: &'static str,
        attempt: usize,
        reply: &misanthropic::response::Message,
    ) {
        self.left_out.push((ask, attempt, reply.inner.clone()));
    }

    /// The tail is done: held for [`Epilogue`](crate::reactor::Epilogue),
    /// `Done` until the survey is begun; otherwise the session is complete
    async fn asks_done(
        &mut self,
        _last: Option<&misanthropic::response::Message>,
    ) -> Result<Control, SeedError> {
        if self.hold_survey {
            self.phase = Phase::Held;
            return Ok(Control::Done(Outcome::Complete));
        }
        Ok(self.finish())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reactor::ask::assert_constrainable;

    fn evolution() -> EvolutionAsk {
        EvolutionAsk {
            max_tokens: 4096,
            agent: "ada".into(),
        }
    }

    fn survey() -> SurveyAsk {
        SurveyAsk {
            max_tokens: 4096,
            agent: "ada".into(),
        }
    }

    #[test]
    fn session_parts_round_trip_by_name() {
        for part in SessionPart::ALL {
            assert_eq!(SessionPart::from_name(part.as_str()), Some(part));
        }
        assert_eq!(SessionPart::from_name("mutate"), None);
        let soul = SoulAsk {
            question: String::new(),
            max_tokens: 1,
            agent: String::new(),
        };
        let names = [
            MemoryAsk { max_tokens: 1 }.name(),
            soul.name(),
            evolution().name(),
            survey().name(),
        ];
        assert_eq!(names, ["memory", "soul", "evolution", "survey"]);
    }

    /// Each question shows, as its first JSON example, exactly the template
    /// its retry notes restate (one copy of the shape)
    #[test]
    fn each_question_shows_its_template() {
        fn first_example(question: &Content) -> serde_json::Value {
            let text: String = question
                .iter()
                .filter_map(|b| match b {
                    misanthropic::prompt::message::Block::Text {
                        text, ..
                    } => Some(text.to_string()),
                    _ => None,
                })
                .collect();
            let start = text.find("```json").expect("an example") + 7;
            let end = start + text[start..].find("```").unwrap();
            serde_json::from_str(text[start..end].trim()).unwrap()
        }
        let soul_ask = SoulAsk {
            question: output::build_soul_mutation_prompt(
                &serde_json::from_value(serde_json::json!({
                    "name": "ada", "identity": "x", "values": [],
                    "interests": {"communities": []}, "voice": "y"
                }))
                .unwrap(),
            ),
            max_tokens: 1,
            agent: "ada".into(),
        };
        let memory = MemoryAsk { max_tokens: 1 };
        for (question, template) in [
            (memory.question(), memory.template()),
            (soul_ask.question(), soul_ask.template()),
            (evolution().question(), evolution().template()),
            (survey().question(), survey().template()),
        ] {
            assert_eq!(first_example(&question), template.unwrap());
        }
    }

    /// Agent-private text never prints through `Debug` (CLAUDE.md rule
    /// 6): memory and feedback show their size only
    #[test]
    fn memory_and_feedback_debug_show_no_words() {
        let memory = Memory {
            content: "My private plans for tomorrow.".into(),
        };
        let shown = format!("{memory:?}");
        assert_eq!(shown, "Memory { content: [REDACTED, 30 chars] }");
        assert!(!shown.contains("private") && !shown.contains("plans"));
        let feedback = survey()
            .parse(r#"{"text": "Secret gripes.", "contact_me": false}"#, false)
            .unwrap()
            .unwrap();
        let shown = format!("{feedback:?}");
        assert_eq!(
            shown,
            "Feedback { text: [REDACTED, 14 chars], contact_me: false }"
        );
        let payload = crate::requests::SubmitFeedbackPayload {
            body: "Secret gripes.".into(),
        };
        assert!(!format!("{payload:?}").contains("gripes"));
        assert!(!format!("{:#?}", Some(memory)).contains("plans"));
    }

    #[test]
    fn memory_rewrite_parses() {
        let ask = MemoryAsk { max_tokens: 4096 };
        let m = ask.parse(r#"{"content": "I like tea."}"#, false).unwrap();
        assert_eq!(m.content, "I like tea.");
        let m = ask
            .parse("```json\n{\"content\": \"Fenced.\"}\n```", false)
            .unwrap();
        assert_eq!(m.content, "Fenced.");
    }

    #[test]
    fn memory_rewrite_error_names_the_path() {
        let err = MemoryAsk { max_tokens: 4096 }
            .parse(r#"{"content": 42}"#, false)
            .unwrap_err();
        // Its path and serde's words, no line or column (0.117.2: it ended
        // "at line 1 column 14")
        assert_eq!(
            err,
            "Invalid JSON at `content`: invalid type: integer `42`, expected \
             a string"
        );
    }

    #[test]
    fn evolution_null_is_no_change() {
        for text in ["null", "", "```json\nnull\n```"] {
            assert!(evolution().parse(text, false).unwrap().is_none());
        }
    }

    #[test]
    fn evolution_note_round_trips() {
        let note = evolution()
            .parse(r#"{"note": "I argue more."}"#, false)
            .unwrap()
            .unwrap();
        assert_eq!(note.note.as_str(), "I argue more.");
    }

    #[test]
    fn feedback_null_is_no_feedback() {
        assert!(survey().parse("null", false).unwrap().is_none());
    }

    #[test]
    fn feedback_parses() {
        let fb = survey()
            .parse(
                r#"{"text": "More cat pictures.", "contact_me": true}"#,
                false,
            )
            .unwrap()
            .unwrap();
        assert_eq!(fb.text.as_str(), "More cat pictures.");
        assert!(fb.contact_me);
    }

    #[test]
    fn a_last_attempt_feedback_is_clipped() {
        let max = crate::limits::feedback::BODY_MAX_CHARS;
        let long = format!("{} And more.", "Too long. ".repeat(max / 10));
        let json = format!(r#"{{"text": "{long}", "contact_me": true}}"#);
        assert!(survey().parse(&json, false).is_err());
        let feedback = survey().parse(&json, true).unwrap().unwrap();
        let text = feedback.text.as_str();
        assert!(text.chars().count() <= max && text.ends_with("long."));
        assert!(feedback.contact_me);

        let short = survey()
            .parse(r#"{"text": "Short.", "contact_me": false}"#, true)
            .unwrap()
            .unwrap();
        assert_eq!(short.text.as_str(), "Short.");
        assert!(survey().parse("null", true).unwrap().is_none());
    }

    #[test]
    fn a_last_attempt_note_is_clipped() {
        let long = format!("{} And more.", "I changed. ".repeat(200));
        let json = format!(r#"{{"note": "{long}"}}"#);
        assert!(evolution().parse(&json, false).is_err());
        let note = evolution().parse(&json, true).unwrap().unwrap();
        let note = note.note.as_str();
        assert!(note.chars().count() <= ITEM_MAX && note.ends_with("changed."));
        assert!(evolution().parse("null", true).unwrap().is_none());
    }

    /// Every question a seed session asks can be constrained with: inline,
    /// closed, no `pattern`, no reasoning field, and `null` writable where
    /// it is an answer (agora CLAUDE.md)
    #[test]
    fn every_seed_ask_schema_is_constrainable() {
        let soul = SoulAsk {
            question: String::new(),
            max_tokens: 4096,
            agent: "ada".into(),
        };
        for schema in [
            MemoryAsk { max_tokens: 1 }.schema(),
            soul.schema(),
            evolution().schema(),
            survey().schema(),
        ] {
            assert_constrainable(&schema);
        }
        // The soul's wire type leaves the evolution log out: the system
        // keeps it, and a grammar would make the model write one
        assert!(soul.schema()["properties"].get("evolution_log").is_none());
        assert_eq!(
            soul.schema()["properties"]
                .as_object()
                .unwrap()
                .keys()
                .collect::<std::collections::BTreeSet<_>>(),
            [
                "boundaries",
                "identity",
                "interests",
                "name",
                "values",
                "voice"
            ]
            .iter()
            .map(|k| k.to_string())
            .collect::<std::collections::BTreeSet<_>>()
            .iter()
            .collect()
        );
        for nullable in [evolution().schema(), survey().schema()] {
            assert_eq!(
                nullable["anyOf"][1],
                serde_json::json!({"type": "null"})
            );
        }
    }
}
