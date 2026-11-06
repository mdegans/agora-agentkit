//! Asking an agent a question and taking its answer as JSON: one copy, for
//! every question a session closes with (memory, soul, evolution, survey)
//! and every question a runner adds (the seed runner's trial review and
//! offers).
//!
//! **The contract.** A question is an [`Ask`]: its words, its token budget,
//! its schema and its parser. Whoever takes the answer implements
//! [`Answer<A>`] for it. The host (an [`Agent`] implementing [`AskHost`])
//! keeps a queue of questions ([`Asks`]), and the driver ([`seat_next`],
//! [`handle`]) asks them one per turn, in order.
//!
//! **Constrained only where it's free** (Steward, 2026-10-01: never a cache
//! miss to constrain an answer). The schema goes out as `output_config` only
//! where [`Quirks::output_config_cache_safe`] holds (blallama). On the
//! Anthropic API `output_config` is part of what the cache keys on, so the
//! question's words and [`Ask::parse`] carry the contract alone, and the
//! request changes nothing but `max_tokens`. The session's effort is kept
//! either way.
//!
//! **Schemas** (CLAUDE.md, "Never ship a `$ref` schema"): inline, no
//! `$ref`/`$defs`, no `pattern`, `additionalProperties: false`, no field that
//! asks for reasoning. Build them with [`ask_schema`] and, for a question
//! whose answer may be `null`, [`nullable`]; [`assert_constrainable`] is the
//! test every `Ask` gets. Never use the derived schema of `Option<T>`:
//! schemars renders it as `"type": ["object", "null"]`, which drama_llama
//! collapses to `"object"` on purpose, so `null` could never be written.
//!
//! **Required and optional.** A required question (the memory rewrite) is
//! what the session is for: a failed attempt is [`Control::Stalled`], the
//! reactor's stall cap is its budget, and running out fails the session, as
//! before. Every other question is optional (Steward, 2026-11-06): a failed
//! attempt is [`Control::Continue`] against the question's own
//! [`Ask::attempts`], and running out is [`Unanswered::Missed`], logged
//! loudly (`event_type = "ask_unanswered"`, ERROR) so the operator hears of
//! it, and the agent is asked again in a later session. It never fails the
//! session.
//!
//! **Refusals.** An API refusal (`stop_reason: refusal`) of an optional
//! question is [`Unanswered::Refused`]: logged loudly, that question only;
//! the questions after it are still asked. Of a required one, it is a failed
//! attempt. An agent that chooses not to answer answers: each question that
//! may be declined has a way to say so.
//!
//! **Pauses** (a server tool's `pause_turn`) are resumed, under the session's
//! pause budget, through [`Agent::on_pause`]; a pause is never a miss.
//!
//! **Room.** Every reply's usage is recorded on the host's [`ContextGauge`],
//! and every question's `max_tokens` is fitted to the room left. A question
//! that doesn't fit is not asked: [`Unanswered::NoRoom`] (the memory rewrite
//! then fails the session; an offer is simply not put, and not recorded).
//!
//! [`Agent`]: super::Agent
//! [`Agent::on_pause`]: super::Agent::on_pause
//! [`Control::Stalled`]: super::Control::Stalled
//! [`Control::Continue`]: super::Control::Continue
//! [`Quirks::output_config_cache_safe`]: super::inference::Quirks::output_config_cache_safe

use std::collections::VecDeque;

use misanthropic::prompt::message::Content;
use misanthropic::response;
use serde::de::DeserializeOwned;

use super::gauge::ContextGauge;
use super::{Agent, Control, Outcome};

/// A question put to an agent, answered as JSON.
pub trait Ask: Send + Sync + 'static {
    /// The answer, as parsed. For a question that may be answered `null`
    /// (no change, no feedback), an `Option`; its [`schema`](Self::schema)
    /// is then built with [`nullable`].
    type Answer: DeserializeOwned + Send;

    /// A short name for logs, alerts and `stall_reason` (`"memory"`,
    /// `"offer:cadence"`).
    fn name(&self) -> &'static str;

    /// The question, seated as a user turn.
    fn question(&self) -> Content;

    /// The answer's `max_tokens`, before it is fitted to the room left.
    fn max_tokens(&self) -> u32;

    /// Attempts before an optional question is [`Unanswered::Missed`].
    /// Unused for a required one (the reactor's stall cap is its budget).
    fn attempts(&self) -> u32 {
        3
    }

    /// Whether the session fails without an answer (the memory rewrite).
    fn required(&self) -> bool {
        false
    }

    /// This question's schema: per instance, since an answer's options may
    /// be shown in a per-agent order (the schema's `enum` follows it) or
    /// narrowed (the cadence re-ask). Built with [`ask_schema`] /
    /// [`nullable`].
    fn schema(&self) -> serde_json::Value;

    /// Parse the answer's text. `last` is true on the last attempt, where an
    /// over-length field may be clipped at a sentence boundary rather than
    /// the answer lost (the `parse_X_clipped` pairs this replaces). An
    /// `Err` is the message the agent is shown before it tries again
    /// (`serde_path_to_error`'s path and serde's words, no line or column).
    fn parse(&self, text: &str, last: bool) -> Result<Self::Answer, String> {
        let _ = last;
        parse_lenient(text)
    }
}

/// What to do after an answer was taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Next {
    /// On to the next question, or, the queue empty, [`AskHost::asks_done`].
    Continue,
    /// End the session now.
    Done(Outcome),
}

/// Why an answer that parsed was not taken.
#[derive(Debug)]
pub enum Rejection<E> {
    /// The agent is shown this and tries again (an attempt is counted):
    /// "Memory rejected: …", an invalid community, an option this offer
    /// doesn't have.
    Retry(String),
    /// Not the agent's doing: the session errs.
    Error(E),
}

/// Why a question ended without an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unanswered {
    /// The API refused the turn (`stop_reason: refusal`). Optional questions
    /// only: of a required one it is a failed attempt.
    Refused,
    /// Its attempts were spent; the last failure, as the agent was shown it.
    Missed(String),
    /// No room left in the context to ask it. Not asked at all.
    NoRoom,
}

/// One unusable reply, kept verbatim (capped) for the record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attempt {
    /// The reply's text blocks, capped for a log line or a ledger.
    pub raw: String,
    /// Why it couldn't be used, as the agent was shown it.
    pub failure: String,
}

/// Where a question was seated: the transcript's length, and, when the
/// question joined a trailing user turn, that turn's block count. The survey
/// redacts from here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mark {
    pub messages: usize,
    pub blocks: Option<usize>,
}

/// How a question went, handed to [`Answer`] with the outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asked {
    /// Unusable replies before this outcome, in order.
    pub attempts: Vec<Attempt>,
    /// Whether the schema went out as `output_config` (grammar-constrained
    /// where the endpoint enforces it).
    pub constrained: bool,
    /// Where the question was seated.
    pub mark: Mark,
    /// The model that answered (the last reply's).
    pub model: Option<String>,
}

/// Taking `A`'s outcome. Implemented by the host once per question type, so
/// the compiler checks that every queued question has a taker.
#[async_trait::async_trait]
pub trait Answer<A: Ask>: AskHost {
    /// Take a parsed answer: apply it, record it, queue what follows.
    async fn answer(
        &mut self,
        ask: &A,
        answer: A::Answer,
        asked: &Asked,
    ) -> Result<Next, Rejection<Self::Error>>;

    /// The question ended without an answer. The driver has already logged
    /// `ask_unanswered`; record it (an offer's ledger), alert, or end the
    /// session (a required question).
    async fn unanswered(
        &mut self,
        ask: &A,
        why: Unanswered,
        asked: &Asked,
    ) -> Result<Next, Self::Error>;
}

/// An [`Agent`] that asks questions.
#[async_trait::async_trait]
pub trait AskHost: Agent {
    /// The host's queue.
    fn asks(&mut self) -> &mut Asks<Self>;

    /// The host's context gauge, if it keeps one: each reply's usage is
    /// recorded on it, and each question fitted to its room.
    fn gauge(&self) -> Option<ContextGauge>;

    /// The queue ran out: what the session does next (the memory turn after
    /// a trial review, the survey after the offers, the end).
    async fn asks_done(
        &mut self,
        last: &response::Message,
    ) -> Result<Control, Self::Error>;
}

/// A queued question, its type erased. Built by [`Asks::push`]; the driver
/// is its only caller.
#[async_trait::async_trait]
pub trait Turn<H: AskHost>: Send {
    /// [`Ask::name`].
    fn name(&self) -> &'static str;

    /// Seat the question (constrained where cache-safe, fitted to the room
    /// left, its [`Mark`] taken).
    async fn seat(self: Box<Self>, host: &mut H)
    -> Result<Seated<H>, H::Error>;

    /// Take a reply: classify it (tool call, refusal, pause, clip, text),
    /// parse, hand the outcome to [`Answer`], or seat the retry note.
    async fn take(
        self: Box<Self>,
        host: &mut H,
        reply: response::Message,
    ) -> Result<Taken<H>, H::Error>;
}

/// What seating a question came to.
pub enum Seated<H: AskHost> {
    /// Asked: waiting for the reply.
    Asked(Box<dyn Turn<H>>),
    /// Not asked, for want of room; [`Answer::unanswered`] has run.
    Skipped(Next),
}

/// What a reply came to.
pub enum Taken<H: AskHost> {
    /// Another attempt is seated: the turn goes back to the queue's front.
    Again(Box<dyn Turn<H>>, Control),
    /// The question is done, answered or not.
    Done(Next),
}

/// A host's questions, in the order they are asked; the head is the one
/// seated, if any.
pub struct Asks<H: AskHost> {
    queue: VecDeque<Box<dyn Turn<H>>>,
    /// Whether the head has been seated and awaits its reply.
    seated: bool,
}

impl<H: AskHost> Default for Asks<H> {
    fn default() -> Self {
        Self {
            queue: VecDeque::new(),
            seated: false,
        }
    }
}

impl<H: AskHost> Asks<H> {
    /// Queue `ask` behind the others.
    pub fn push<A: Ask>(&mut self, ask: A)
    where
        H: Answer<A>,
    {
        let _ = ask;
        todo!("box an AskTurn<A> (the ask plus its attempt state) as a Turn<H>")
    }

    /// Whether a question is seated and awaiting its reply: the host routes
    /// its responses to [`handle`] while this holds.
    pub fn is_active(&self) -> bool {
        self.seated
    }

    /// Whether any question is queued.
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// For [`Agent::stall_reason`](super::Agent::stall_reason): the seated
    /// question's name.
    pub fn stall_reason(&self) -> Option<String> {
        self.seated
            .then(|| self.queue.front().map(|t| t.name().to_string()))
            .flatten()
    }
}

/// Seat the next question: [`Control::Continue`] once one is asked. Questions
/// skipped for room are settled on the way; with none left,
/// [`AskHost::asks_done`] decides (`last` is the reply that emptied the queue,
/// if one did).
pub async fn seat_next<H: AskHost>(
    host: &mut H,
    last: Option<&response::Message>,
) -> Result<Control, H::Error> {
    let _ = (host, last);
    todo!("pop, Turn::seat, push back the Asked turn; Skipped(Continue) → loop")
}

/// A reply while [`Asks::is_active`]: record its usage, take it, and seat
/// whatever comes next.
pub async fn handle<H: AskHost>(
    host: &mut H,
    reply: response::Message,
) -> Result<Control, H::Error> {
    let _ = (host, reply);
    todo!(
        "pop the seated turn, Turn::take, Again → push_front, Done → seat_next"
    )
}

/// The one lenient JSON reader, for every question. In order: the whole text
/// as `T`; inside a code fence (any case, any language tag); a lone
/// one-key wrapper object (`{"answer_offer": {…}}`) whose value is an object,
/// tried only after `T` failed; else the objects embedded in prose, taking
/// the one that parses as `T` (several that do is an error: ambiguous). An
/// `Err` names the field path and serde's message, without line or column
/// ([`crate::field_path`]). Unquoted keys and doubled quotes are not
/// repaired: they're a retry. Empty text is `null` (so an `Option` answer is
/// `None`, and any other answer an error).
pub fn parse_lenient<T: DeserializeOwned>(text: &str) -> Result<T, String> {
    let _ = text;
    todo!()
}

/// `T`'s schema for a question: inline, sanitized for the Anthropic API
/// (misanthropic's `schema_for`, the one generator for every `Ask`): no
/// `$ref`, no `pattern`, closed.
pub fn ask_schema<T: schemars::JsonSchema>() -> serde_json::Value {
    todo!()
}

/// `schema`, or `null`: `{"anyOf": [schema, {"type": "null"}]}`, the form
/// drama_llama's grammar keeps both branches of. Never `Option<T>`'s derived
/// schema (see the [module docs](self)).
pub fn nullable(schema: serde_json::Value) -> serde_json::Value {
    serde_json::json!({ "anyOf": [schema, { "type": "null" }] })
}

/// Panics unless `schema` is fit to constrain a question with: no `$ref` or
/// `$defs`, no `pattern`, every object closed (`additionalProperties:
/// false`), no property named for reasoning (`reasoning`, `thinking`,
/// `trace`, `scratchpad`, `reason`), and no root `type` array holding
/// `"null"`. Every `Ask` in this crate and the runner is tested with it.
pub fn assert_constrainable(schema: &serde_json::Value) {
    let _ = schema;
    todo!()
}
