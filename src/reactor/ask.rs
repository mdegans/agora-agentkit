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
//! **Refusals.** An API refusal (`stop_reason: refusal`) of any question
//! ends the session as failed (Steward, 2026-11-06: whatever is in the
//! context may set the classifier off again). It is logged (`event_type =
//! "api_refusal"`, ERROR, no content), handed to the host as
//! [`Unanswered::Refused`], and the refused turn is never seated. An agent
//! that chooses not to answer answers: each question that may be declined
//! has a way to say so.
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
use std::num::NonZeroU32;

use misanthropic::prompt::Prompt;
use misanthropic::prompt::message::{Block, Content, Role};
use misanthropic::prompt::output::OutputConfig;
use misanthropic::response::{self, StopReason};
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::gauge::{self, ContextGauge, Fit};
use super::{
    Agent, Control, MAX_STALLS, Outcome, seat_unused_reply, seat_user,
};
use crate::ids::AgentId;

/// Said after a reply cut off at `max_tokens`, in an acting turn or a
/// question alike. Worded to make clear only the *attempt* was dropped:
/// implying a post or comment was thrown out invites duplicates. One copy,
/// for agentkit and the runner.
pub const TRUNCATION_WARNING: &str = "Your previous attempt at this turn \
exceeded the max_tokens limit and was cut off, so nothing in it was used. \
Respond again, more briefly.";

/// The error result a tool call gets in a turn clipped at `max_tokens`
pub const NOT_RUN_CLIPPED: &str =
    "Not run: this turn was cut off at the length limit. Nothing was done.";

/// The error result a tool call gets in a reply to a question, which is
/// answered in JSON text, not with a tool
pub const NOT_RUN_NOW: &str =
    "Not run: no tools can be used in this turn. Nothing was done.";

/// Said after a reply to a question that called a tool
pub const NO_TOOLS_NOW: &str = "Do NOT use tools right now. Respond in JSON \
only, per the instructions above.";

/// Said after an answer that is the question's own example: a placeholder
/// copied as it was shown
pub const TEMPLATE_NOTE: &str = "That is the example shape from the \
instructions above, with its placeholder left in, not your answer. Write \
your own in that shape: JSON only.";

/// The most of a reply's text kept in an [`Attempt`]
const RAW_CAP: usize = 4_000;

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

    /// Whether an answer is anonymous (the survey): the driver then keeps no
    /// [`Attempt::raw`] text of its unusable replies either. (No question's
    /// failure message is ever kept or logged: see [`Attempt::failure`].)
    fn anonymous(&self) -> bool {
        false
    }

    /// The JSON example the question shows, if any. An answer holding one
    /// of its string values verbatim is that example copied, and a retry
    /// ([`TEMPLATE_NOTE`]); so is one holding a `<…>` placeholder taken
    /// from the question's text, whatever this returns.
    fn template(&self) -> Option<serde_json::Value> {
        None
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
    /// The API refused the turn (`stop_reason: refusal`): the session ends
    /// as failed, whatever the question (Steward, 2026-11-06: what is in the
    /// context may keep setting the classifier off). The host records it;
    /// the driver ends the session whatever [`Answer::unanswered`] returns.
    Refused {
        /// The refused response's id
        response_id: String,
    },
    /// Its attempts were spent; the last failure's kind ([`Attempt::failure`]).
    Missed(String),
    /// No room left in the context to ask it. Not asked at all.
    NoRoom,
}

impl Unanswered {
    /// A short word for logs: `refused`, `missed`, `no_room`
    pub fn kind(&self) -> &'static str {
        match self {
            Unanswered::Refused { .. } => "refused",
            Unanswered::Missed(_) => "missed",
            Unanswered::NoRoom => "no_room",
        }
    }
}

/// One unusable reply, for the record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attempt {
    /// The reply's text blocks, capped; empty for an
    /// [anonymous](Ask::anonymous) answer. Never logged by the driver.
    pub raw: String,
    /// Why it couldn't be used, as a kind only: `parse` (with the field's
    /// path where there is one: ``parse at `content` ``), `clipped`,
    /// `tool_call`, `rejected`, `template`, `refused`, `paused`. Never
    /// serde's message or the host's: they quote what the agent wrote (a
    /// bare-string memory reply's message is the whole memory; "We Don't
    /// Read Agent Memory"). The agent itself is shown the whole note. This
    /// is what logs, [`Unanswered::Missed`] and
    /// [`Asks::stall_reason`] carry.
    pub failure: String,
}

/// A failure as [`Attempt::failure`] keeps it: its `kind`, and for a parse
/// error the field path [`crate::field_path`] put in the note
fn failure_kind(kind: &'static str, note: &str) -> String {
    if kind == "parse"
        && let Some(rest) = note.strip_prefix("Invalid JSON at `")
        && let Some(end) = rest.find('`')
    {
        return format!("parse at `{}`", &rest[..end]);
    }
    kind.to_string()
}

/// Where a question was seated: the transcript's length, and, when the
/// question joined a trailing user turn, that turn's block count. The survey
/// redacts from here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mark {
    pub messages: usize,
    pub blocks: Option<usize>,
}

impl Mark {
    /// The end of `prompt` as it stands: where a question seated next
    /// begins
    pub fn at(prompt: &Prompt) -> Self {
        let blocks = prompt
            .messages
            .last()
            .filter(|last| last.role == Role::User)
            .map(|last| last.content.len());
        Self {
            messages: prompt.messages.len(),
            blocks,
        }
    }

    /// Cut everything from this mark on out of `prompt`
    pub fn redact(self, prompt: &mut Prompt) {
        prompt.messages.truncate(self.messages);
        if let (Some(blocks), Some(last)) =
            (self.blocks, prompt.messages.last_mut())
        {
            last.content.truncate(blocks);
        }
    }
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
///
/// The reply that answered is already seated when [`answer`](Self::answer)
/// runs; a [`Rejection::Retry`] seats the retry note after it.
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

    /// The question was seated at `mark`, before any reply: for what must
    /// hold even if the session ends with the question unanswered (the
    /// survey's redaction point). The default does nothing.
    fn seated(&mut self, ask: &A, mark: Mark) {
        let _ = (ask, mark);
    }
}

/// An [`Agent`] that asks questions.
#[async_trait::async_trait]
pub trait AskHost: Agent {
    /// The host's queue.
    fn asks(&mut self) -> &mut Asks<Self>;

    /// The host's context gauge, if it keeps one: each reply's usage is
    /// recorded on it, and each question fitted to its room.
    fn gauge(&self) -> Option<ContextGauge>;

    /// The agent's name, for the driver's log lines (`agent`), beside its
    /// id. `None` (the default) logs the id alone.
    fn agent_name(&self) -> Option<String> {
        None
    }

    /// The queue ran out: what the session does next (the memory turn after
    /// a trial review, the survey after the offers, the end).
    /// `last` is the reply that emptied the queue, or `None` when it
    /// emptied with no reply (every question left skipped for room).
    async fn asks_done(
        &mut self,
        last: Option<&response::Message>,
    ) -> Result<Control, Self::Error>;
}

/// A queued question, its type erased. Built by [`Asks::push`]; the driver
/// is its only caller.
#[async_trait::async_trait]
pub trait Turn<H>: Send {
    /// [`Ask::name`].
    fn name(&self) -> &'static str;

    /// The unusable replies so far, for [`Asks::stall_reason`].
    fn attempts(&self) -> &[Attempt];

    /// Seat the question (constrained where cache-safe, fitted to the room
    /// left, its [`Mark`] taken).
    async fn seat(self: Box<Self>, host: &mut H) -> Result<Seated<H>, H::Error>
    where
        H: AskHost;

    /// Take a reply: classify it (tool call, refusal, pause, clip, text),
    /// parse, hand the outcome to [`Answer`], or seat the retry note.
    async fn take(
        self: Box<Self>,
        host: &mut H,
        reply: response::Message,
    ) -> Result<Taken<H>, H::Error>
    where
        H: AskHost;
}

/// What seating a question came to.
pub enum Seated<H> {
    /// Asked: waiting for the reply.
    Asked(Box<dyn Turn<H>>),
    /// Not asked, for want of room; [`Answer::unanswered`] has run.
    Skipped(Next),
}

/// What a reply came to.
pub enum Taken<H> {
    /// Another attempt is seated: the turn goes back to the queue's front.
    Again(Box<dyn Turn<H>>, Control),
    /// The question is done, answered or not.
    Done(Next),
}

/// A host's questions, in the order they are asked; the head is the one
/// seated, if any (out of the queue while its reply is taken).
pub struct Asks<H> {
    queue: VecDeque<Box<dyn Turn<H>>>,
    /// The question seated, from its seating until it is done, its reply's
    /// taking included
    active: Option<&'static str>,
}

impl<H> Default for Asks<H> {
    fn default() -> Self {
        Self {
            queue: VecDeque::new(),
            active: None,
        }
    }
}

impl<H> Asks<H> {
    /// Queue `ask` behind the others.
    pub fn push<A: Ask>(&mut self, ask: A)
    where
        H: Answer<A>,
    {
        self.queue.push_back(Box::new(AskTurn {
            ask,
            attempts: Vec::new(),
            constrained: false,
            mark: None,
            model: None,
        }));
    }

    /// Whether a question is in progress: from its seating until it is
    /// done, including while [`handle`] takes its reply (so a host's
    /// [`Agent::on_pause`](super::Agent::on_pause), called from there,
    /// sees it). The host routes its responses to [`handle`] while this
    /// holds.
    pub fn is_active(&self) -> bool {
        self.active.is_some()
    }

    /// Whether any question is queued.
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// The seated question's name, if one is seated.
    pub fn seated(&self) -> Option<&'static str> {
        self.active
    }

    /// For [`Agent::stall_reason`]: the seated
    /// question's name, how many of its replies were unusable, and the last
    /// failure.
    pub fn stall_reason(&self) -> Option<String> {
        let name = self.active?;
        // Out of the queue while its reply is taken: the name alone
        let Some(turn) = self.queue.front().filter(|t| t.name() == name) else {
            return Some(format!("the {name} question"));
        };
        let attempts = turn.attempts();
        Some(match attempts.last() {
            Some(last) => format!(
                "the {} question failed {} times in a row; last: {}",
                turn.name(),
                attempts.len(),
                last.failure
            ),
            None => format!("the {} question", turn.name()),
        })
    }
}

/// The driver's own errors, boxed into the host's
#[derive(Debug, thiserror::Error)]
#[error("ask: {0}")]
struct AskError(String);

fn error<E: From<Box<dyn std::error::Error + Send + Sync>>>(
    e: impl std::fmt::Display,
) -> E {
    E::from(Box::new(AskError(e.to_string())))
}

/// Seat the next question: [`Control::Continue`] once one is asked. Questions
/// skipped for room are settled on the way; with none left,
/// [`AskHost::asks_done`] decides (`last` is the reply that emptied the queue,
/// if one did).
pub async fn seat_next<H: AskHost>(
    host: &mut H,
    last: Option<&response::Message>,
) -> Result<Control, H::Error> {
    if host.asks().active.is_some() {
        return Err(error("a question is already seated"));
    }
    loop {
        let Some(turn) = host.asks().queue.pop_front() else {
            return host.asks_done(last).await;
        };
        match turn.seat(host).await? {
            Seated::Asked(turn) => {
                let asks = host.asks();
                asks.active = Some(turn.name());
                asks.queue.push_front(turn);
                return Ok(Control::Continue);
            }
            Seated::Skipped(Next::Continue) => continue,
            Seated::Skipped(Next::Done(outcome)) => {
                return Ok(Control::Done(outcome));
            }
        }
    }
}

/// A reply while [`Asks::is_active`]: record its usage, take it, and seat
/// whatever comes next.
pub async fn handle<H: AskHost>(
    host: &mut H,
    reply: response::Message,
) -> Result<Control, H::Error> {
    if let Some(gauge) = host.gauge() {
        gauge.record(&reply.usage);
    }
    let asks = host.asks();
    if asks.active.is_none() {
        return Err(error("a reply with no question seated"));
    }
    // Still active while its reply is taken
    let turn = asks.queue.pop_front().expect("active implies a head");
    let last = reply.clone();
    match turn.take(host, reply).await? {
        Taken::Again(turn, control) => {
            host.asks().queue.push_front(turn);
            Ok(control)
        }
        Taken::Done(next) => {
            host.asks().active = None;
            // A question's format goes with it: a later unconstrained turn
            // must not be bound to a stale schema (the effort stays)
            clear_format(host.parts().1);
            match next {
                Next::Continue => seat_next(host, Some(&last)).await,
                Next::Done(outcome) => Ok(Control::Done(outcome)),
            }
        }
    }
}

/// Drop `prompt`'s output format, keeping its effort
fn clear_format(prompt: &mut Prompt) {
    let effort = prompt.output_config.take().and_then(|c| c.effort);
    prompt.output_config = effort.map(OutputConfig::effort);
}

/// Take a reply that is not seated back off the gauge: the request after
/// it holds none of its output
fn unrecord<H: AskHost>(host: &H, reply: &response::Message) {
    if let Some(gauge) = host.gauge() {
        gauge.set(gauge.get().saturating_sub(reply.usage.output_tokens));
    }
}

/// Who a log line is about
struct Who {
    agent: Option<String>,
    agent_id: AgentId,
}

impl Who {
    fn of<H: AskHost>(host: &H) -> Self {
        Self {
            agent: host.agent_name(),
            agent_id: host.id(),
        }
    }
}

/// A queued [`Ask`] and its attempt state
struct AskTurn<A: Ask> {
    ask: A,
    attempts: Vec<Attempt>,
    constrained: bool,
    mark: Option<Mark>,
    model: Option<String>,
}

impl<A: Ask> AskTurn<A> {
    fn failures(&self) -> u32 {
        u32::try_from(self.attempts.len()).unwrap_or(u32::MAX)
    }

    /// Whether the reply being taken is the question's last try: for a
    /// required one, before the reactor's stall cap ends the session
    fn last(&self) -> bool {
        let budget = if self.ask.required() {
            u32::try_from(MAX_STALLS).unwrap_or(u32::MAX)
        } else {
            self.ask.attempts()
        };
        self.failures() + 1 >= budget
    }

    /// Whether an optional question's attempts are spent
    fn spent(&self) -> bool {
        !self.ask.required() && self.failures() >= self.ask.attempts()
    }

    /// What a failed attempt that leaves the question seated returns: a
    /// stall for a required question (the reactor's cap is its budget),
    /// progress for an optional one (its own budget bounds it)
    fn retry_control(&self) -> Control {
        if self.ask.required() {
            Control::Stalled
        } else {
            Control::Continue
        }
    }

    fn asked(&self, prompt: &Prompt) -> Asked {
        Asked {
            attempts: self.attempts.clone(),
            constrained: self.constrained,
            mark: self.mark.unwrap_or_else(|| Mark::at(prompt)),
            model: self.model.clone(),
        }
    }

    /// Record an unusable reply: verbatim (capped) with the message the
    /// agent was shown, or, for an anonymous answer, its `kind` alone
    fn push_attempt(
        &mut self,
        reply: &response::Message,
        note: &str,
        kind: &'static str,
    ) {
        self.attempts.push(Attempt {
            raw: if self.ask.anonymous() {
                String::new()
            } else {
                cap(raw_text(reply))
            },
            failure: failure_kind(kind, note),
        });
    }

    /// The strings an answer that copied the question's example would hold
    fn placeholders(&self) -> Vec<String> {
        let mut out = Vec::new();
        for block in self.ask.question().iter() {
            if let Block::Text { text, .. } = block {
                out.extend(angle_placeholders(text));
            }
        }
        if let Some(template) = self.ask.template() {
            string_leaves(&template, &mut out);
        }
        out.retain(|p| !p.trim().is_empty());
        out
    }

    /// Log `why`, then hand it to the host
    async fn settle<H: Answer<A>>(
        self: Box<Self>,
        host: &mut H,
        why: Unanswered,
    ) -> Result<Next, H::Error> {
        let who = Who::of(host);
        let failure = match &why {
            Unanswered::Missed(failure) => Some(failure.as_str()),
            _ => None,
        };
        let quiet = matches!(why, Unanswered::NoRoom) && !self.ask.required();
        macro_rules! unanswered {
            ($level:ident) => {
                tracing::$level!(
                    event_type = "ask_unanswered",
                    agent = who.agent.as_deref(),
                    agent_id = %who.agent_id,
                    ask = self.ask.name(),
                    why = why.kind(),
                    failure,
                    attempts = self.attempts.len(),
                    required = self.ask.required(),
                    constrained = self.constrained,
                    model = self.model.as_deref(),
                    "a question ended without an answer"
                )
            };
        }
        // An optional question with no room is routine near the edge
        // (`phase_skipped_for_context` says so); everything else is loud.
        if quiet {
            unanswered!(warn);
        } else {
            unanswered!(error);
        }
        let asked = self.asked(host.prompt());
        host.unanswered(&self.ask, why, &asked).await
    }

    /// Fit the request's `max_tokens` to the room left. False when nothing
    /// worth sending fits (logged as `context_exhausted`)
    fn fit(
        &self,
        who: &Who,
        gauge: &ContextGauge,
        prompt: &mut Prompt,
    ) -> bool {
        match gauge::fit_max_tokens(gauge, prompt) {
            Fit::Whole => true,
            Fit::Clamped { from, room } => {
                tracing::warn!(
                    event_type = "phase_budget_clamped",
                    agent = who.agent.as_deref(),
                    agent_id = %who.agent_id,
                    ask = self.ask.name(),
                    max_tokens = from,
                    room,
                    held = gauge.get(),
                    window = gauge.window(),
                    "a question's max_tokens lowered to fit the context"
                );
                true
            }
            Fit::NoRoom { room, floor } => {
                tracing::error!(
                    event_type = "context_exhausted",
                    agent = who.agent.as_deref(),
                    agent_id = %who.agent_id,
                    ask = self.ask.name(),
                    room,
                    floor,
                    held = gauge.get(),
                    window = gauge.window(),
                    "no room left to send a question"
                );
                false
            }
        }
    }

    /// A reply that couldn't be used: record it; with attempts left, seat it
    /// (or, near the edge, leave it out) and `note` after it, else settle the
    /// question as missed. `seated` says the reply is already in the
    /// transcript (an answer the host rejected).
    #[allow(clippy::too_many_arguments)]
    async fn fail<H: Answer<A>>(
        mut self: Box<Self>,
        host: &mut H,
        reply: &response::Message,
        note: &str,
        kind: &'static str,
        not_run: &str,
        seated: bool,
    ) -> Result<Taken<H>, H::Error> {
        let who = Who::of(host);
        self.push_attempt(reply, note, kind);
        let failure = self.attempts.last().expect("pushed").failure.clone();
        // Never the reply's text, and for an anonymous answer not even the
        // message: serde's quote the value they refused
        tracing::warn!(
            event_type = "ask_attempt_failed",
            agent = who.agent.as_deref(),
            agent_id = %who.agent_id,
            ask = self.ask.name(),
            attempt = self.attempts.len(),
            required = self.ask.required(),
            constrained = self.constrained,
            model = %reply.model,
            stop_reason = ?reply.stop_reason,
            output_tokens = reply.usage.output_tokens,
            kind,
            failure = %failure,
            "a reply to a question couldn't be used"
        );
        if self.spent() {
            if !seated {
                let (_, prompt) = host.parts();
                seat_unused_reply(prompt, reply, not_run).map_err(error)?;
            }
            let why = Unanswered::Missed(failure);
            return self.settle(host, why).await.map(Taken::Done);
        }
        let gauge = host.gauge();
        let budget = self.ask.max_tokens().max(1);
        let unseat = gauge.as_ref().is_some_and(|g| {
            gauge::unseat_for_retry(g, budget, reply.usage.output_tokens)
        });
        if let (true, Some(g)) = (unseat, &gauge) {
            tracing::info!(
                event_type = "phase_retry_unseated",
                agent = who.agent.as_deref(),
                agent_id = %who.agent_id,
                ask = self.ask.name(),
                held = g.get(),
                window = g.window(),
                "failed reply left out so the retry fits"
            );
        }
        let fits = {
            let (_, prompt) = host.parts();
            if unseat {
                if seated {
                    prompt.messages.pop();
                }
            } else if !seated {
                seat_unused_reply(prompt, reply, not_run).map_err(error)?;
            }
            prompt.max_tokens = NonZeroU32::new(budget).expect("max(1)");
            gauge.as_ref().is_none_or(|g| self.fit(&who, g, prompt))
        };
        if !fits {
            return self
                .settle(host, Unanswered::NoRoom)
                .await
                .map(Taken::Done);
        }
        let (_, prompt) = host.parts();
        seat_user(prompt, note.to_string()).map_err(error)?;
        let control = self.retry_control();
        Ok(Taken::Again(self, control))
    }
}

#[async_trait::async_trait]
impl<A: Ask, H: Answer<A>> Turn<H> for AskTurn<A> {
    fn name(&self) -> &'static str {
        self.ask.name()
    }

    fn attempts(&self) -> &[Attempt] {
        &self.attempts
    }

    async fn seat(
        mut self: Box<Self>,
        host: &mut H,
    ) -> Result<Seated<H>, H::Error>
    where
        H: AskHost,
    {
        let who = Who::of(host);
        let budget = self.ask.max_tokens().max(1);
        let gauge = host.gauge();
        // An optional question that would be clipped is not asked: its JSON
        // would not parse, and each retry would only fill the context more
        if let Some(g) = &gauge
            && !self.ask.required()
            && g.phase_room() < u64::from(budget)
        {
            tracing::info!(
                event_type = "phase_skipped_for_context",
                agent = who.agent.as_deref(),
                agent_id = %who.agent_id,
                ask = self.ask.name(),
                budget,
                room = g.phase_room(),
                "optional question skipped: no room in the context"
            );
            return self
                .settle(host, Unanswered::NoRoom)
                .await
                .map(Seated::Skipped);
        }
        let cache_safe =
            host.quirks().unwrap_or_default().output_config_cache_safe;
        let fits = {
            let (_, prompt) = host.parts();
            prompt.max_tokens = NonZeroU32::new(budget).expect("max(1)");
            gauge.as_ref().is_none_or(|g| self.fit(&who, g, prompt))
        };
        if !fits {
            return self
                .settle(host, Unanswered::NoRoom)
                .await
                .map(Seated::Skipped);
        }
        let (_, prompt) = host.parts();
        // The last question's format goes; the session's effort stays
        // (without it a question would think at the model's default)
        let effort = prompt.output_config.take().and_then(|c| c.effort);
        prompt.output_config = effort.map(OutputConfig::effort);
        // Constrained only where it's free (see the module docs)
        self.constrained = cache_safe;
        if cache_safe {
            let format = OutputConfig::json_schema(self.ask.schema()).format;
            prompt.output_config.get_or_insert_default().format = format;
        }
        let mark = Mark::at(prompt);
        self.mark = Some(mark);
        seat_user(prompt, self.ask.question()).map_err(error)?;
        host.seated(&self.ask, mark);
        Ok(Seated::Asked(self))
    }

    async fn take(
        mut self: Box<Self>,
        host: &mut H,
        reply: response::Message,
    ) -> Result<Taken<H>, H::Error>
    where
        H: AskHost,
    {
        self.model = Some(reply.model.to_string());
        match reply.stop_reason {
            // Resumed under the session's pause budget; never a miss
            Some(StopReason::PauseTurn) => {
                return match host.on_pause(reply).await? {
                    Control::Continue => {
                        Ok(Taken::Again(self, Control::Continue))
                    }
                    Control::Done(outcome) => {
                        Ok(Taken::Done(Next::Done(outcome)))
                    }
                    // The budget is spent and the paused turn abandoned:
                    // the question goes again as it stands
                    Control::Stalled => {
                        self.attempts.push(Attempt {
                            raw: String::new(),
                            failure: "paused".to_string(),
                        });
                        if self.spent() {
                            let why = Unanswered::Missed("paused".into());
                            return self
                                .settle(host, why)
                                .await
                                .map(Taken::Done);
                        }
                        let control = self.retry_control();
                        Ok(Taken::Again(self, control))
                    }
                };
            }
            // Any refusal ends the session as failed (Steward, 2026-11-06):
            // what is in the context may set the classifier off again. The
            // refused turn is never seated, nor any partial a pause left
            Some(StopReason::Refusal) => {
                unrecord(host, &reply);
                let (_, prompt) = host.parts();
                while prompt
                    .messages
                    .last()
                    .is_some_and(|m| m.role == Role::Assistant)
                {
                    prompt.messages.pop();
                }
                let who = Who::of(host);
                // No content: neither the reply nor the question
                tracing::error!(
                    event_type = "api_refusal",
                    agent = who.agent.as_deref(),
                    agent_id = %who.agent_id,
                    model = %reply.model,
                    during = self.ask.name(),
                    response_id = %reply.id,
                    "the API refused a reply; the session ends"
                );
                let why = Unanswered::Refused {
                    response_id: reply.id.to_string(),
                };
                self.settle(host, why).await?;
                return Ok(Taken::Done(Next::Done(Outcome::Failed)));
            }
            // Nothing from a clipped reply is used: a truncated answer can
            // parse yet be missing what the model never got to write
            Some(StopReason::MaxTokens) => {
                return self
                    .fail(
                        host,
                        &reply,
                        TRUNCATION_WARNING,
                        "clipped",
                        NOT_RUN_CLIPPED,
                        false,
                    )
                    .await;
            }
            _ => {}
        }
        // A tool call can't be dispatched (the question owns the turn)
        if reply
            .inner
            .content
            .iter()
            .any(|block| block.tool_use().is_some())
        {
            return self
                .fail(
                    host,
                    &reply,
                    NO_TOOLS_NOW,
                    "tool_call",
                    NOT_RUN_NOW,
                    false,
                )
                .await;
        }
        let text = raw_text(&reply);
        let answer = match self.ask.parse(&text, self.last()) {
            Ok(answer) => answer,
            Err(e) => {
                return self
                    .fail(host, &reply, &e, "parse", NOT_RUN_NOW, false)
                    .await;
            }
        };
        // The question's own example, copied: a placeholder is no answer
        if echoes_placeholder(&text, &self.placeholders()) {
            return self
                .fail(
                    host,
                    &reply,
                    TEMPLATE_NOTE,
                    "template",
                    NOT_RUN_NOW,
                    false,
                )
                .await;
        }
        // Seated before the host takes it, so whatever the host seats
        // follows it, and the transcript only ever grows
        {
            let (_, prompt) = host.parts();
            seat_unused_reply(prompt, &reply, NOT_RUN_NOW).map_err(error)?;
        }
        let asked = self.asked(host.prompt());
        match host.answer(&self.ask, answer, &asked).await {
            Ok(next) => Ok(Taken::Done(next)),
            Err(Rejection::Retry(note)) => {
                self.fail(host, &reply, &note, "rejected", NOT_RUN_NOW, true)
                    .await
            }
            Err(Rejection::Error(e)) => Err(e),
        }
    }
}

/// A reply's text blocks, joined
fn raw_text(reply: &response::Message) -> String {
    reply
        .inner
        .content
        .iter()
        .filter_map(|block| match block {
            Block::Text { text, .. } => Some(text.as_ref()),
            _ => None,
        })
        .collect::<Vec<&str>>()
        .join("\n\n")
}

/// `text`, cut to [`RAW_CAP`] bytes at a character boundary
fn cap(mut text: String) -> String {
    if text.len() > RAW_CAP {
        let mut end = RAW_CAP;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push('…');
    }
    text
}

/// The `<…>` placeholders in a question's text (`<feedback here>`): a
/// `<`, a letter, then up to 80 characters on one line to the `>`
fn angle_placeholders(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('<') {
        let after = &rest[open + 1..];
        let close = after.find(['>', '<', '\n']);
        match close {
            Some(close)
                if after[close..].starts_with('>')
                    && close <= 80
                    && after.starts_with(|c: char| c.is_alphabetic()) =>
            {
                out.push(format!("<{}>", &after[..close]));
                rest = &after[close + 1..];
            }
            _ => rest = after,
        }
    }
    out
}

/// Every string in `value`, at any depth
fn string_leaves(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(s) => out.push(s.clone()),
        Value::Array(items) => items.iter().for_each(|v| string_leaves(v, out)),
        Value::Object(map) => map.values().for_each(|v| string_leaves(v, out)),
        _ => {}
    }
}

/// Whether any JSON in `text` holds a string that is, trimmed, one of
/// `placeholders`
fn echoes_placeholder(text: &str, placeholders: &[String]) -> bool {
    if placeholders.is_empty() {
        return false;
    }
    let text = text.trim();
    let mut values: Vec<Value> = embedded_objects(text);
    values.extend(serde_json::from_str::<Value>(text).ok());
    if let Some(body) = fenced(text) {
        values.extend(serde_json::from_str::<Value>(body).ok());
    }
    let mut strings = Vec::new();
    for value in &values {
        string_leaves(value, &mut strings);
    }
    strings
        .iter()
        .any(|s| placeholders.iter().any(|p| s.trim() == p.trim()))
}

/// A deserialization error as the agent is shown it
fn shown(e: serde_path_to_error::Error<serde_json::Error>) -> String {
    crate::field_path::FieldError::new(e).after("Invalid JSON")
}

fn from_text<T: DeserializeOwned>(text: &str) -> Result<T, String> {
    let mut de = serde_json::Deserializer::from_str(text);
    serde_path_to_error::deserialize(&mut de).map_err(shown)
}

fn from_value<T: DeserializeOwned>(value: Value) -> Result<T, String> {
    serde_path_to_error::deserialize(value).map_err(shown)
}

/// `T` from `value`, or from the object a one-key wrapper holds
/// (`{"answer_offer": {…}}`), tried only after `T` failed
fn from_value_or_unwrapped<T: DeserializeOwned>(
    value: Value,
) -> Result<T, String> {
    let inner = match &value {
        Value::Object(map) if map.len() == 1 => {
            map.values().next().filter(|v| v.is_object()).cloned()
        }
        _ => None,
    };
    match from_value(value) {
        Ok(answer) => Ok(answer),
        Err(e) => match inner.map(from_value) {
            Some(Ok(answer)) => Ok(answer),
            _ => Err(e),
        },
    }
}

/// The inside of the first code fence in `text`, whatever its case or
/// language tag; to the end when it is never closed
fn fenced(text: &str) -> Option<&str> {
    let open = text.find("```")?;
    let rest = &text[open + 3..];
    let tag = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || "_-+.".contains(c)))
        .unwrap_or(rest.len());
    let body = &rest[tag..];
    let body = match body.find("```") {
        Some(close) => &body[..close],
        None => body,
    };
    Some(body.trim())
}

/// The top-level JSON objects embedded in `text`: each balanced `{…}`
/// (strings respected) that parses as an object
fn embedded_objects(text: &str) -> Vec<Value> {
    let mut found = Vec::new();
    let (mut depth, mut start) = (0usize, 0usize);
    let (mut in_string, mut escaped) = (false, false);
    for (i, c) in text.char_indices() {
        if in_string {
            match (escaped, c) {
                (true, _) => escaped = false,
                (false, '\\') => escaped = true,
                (false, '"') => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' if depth > 0 => in_string = true,
            '{' => {
                if depth == 0 {
                    start = i;
                }
                depth += 1;
            }
            '}' if depth > 0 => {
                depth -= 1;
                if depth == 0
                    && let Ok(value @ Value::Object(_)) =
                        serde_json::from_str::<Value>(&text[start..=i])
                {
                    found.push(value);
                }
            }
            _ => {}
        }
    }
    found
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
    let text = text.trim();
    if text.is_empty() {
        return from_value(Value::Null);
    }
    let mut shown = match from_text(text) {
        Ok(answer) => return Ok(answer),
        Err(e) => e,
    };
    let body = fenced(text);
    if let Some(body) = body {
        if body.is_empty() {
            return from_value(Value::Null);
        }
        match from_text(body) {
            Ok(answer) => return Ok(answer),
            Err(e) => shown = e,
        }
    }
    if let Ok(value @ Value::Object(_)) =
        serde_json::from_str::<Value>(body.unwrap_or(text))
        && let Ok(answer) = from_value_or_unwrapped(value)
    {
        return Ok(answer);
    }
    let objects = embedded_objects(text);
    let count = objects.len();
    let mut answers = Vec::new();
    for object in objects {
        match from_value_or_unwrapped(object) {
            Ok(answer) => answers.push(answer),
            // One object, of the wrong shape: its error names the field
            Err(e) if count == 1 => shown = e,
            Err(_) => {}
        }
    }
    match answers.len() {
        0 => Err(shown),
        1 => Ok(answers.pop().expect("one")),
        n => Err(format!(
            "Your reply holds {n} JSON objects that each answer the \
             question, so it can't be told which you meant. Respond with \
             exactly one."
        )),
    }
}

/// `T`'s schema for a question: inline, sanitized for the Anthropic API
/// (misanthropic's `schema_for`, the one generator for every `Ask`): no
/// `$ref`, no `pattern`, closed. The root's `$schema` and `title` go too:
/// noise to a model, and a `$schema` has no place inside [`nullable`]'s
/// `anyOf`.
pub fn ask_schema<T: schemars::JsonSchema>() -> serde_json::Value {
    let mut schema = misanthropic::prompt::output::schema_for::<T>();
    if let Some(root) = schema.as_object_mut() {
        // `retain`, not `remove`: under `preserve_order` a remove swaps the
        // last key into the gap
        root.retain(|key, _| key != "$schema" && key != "title");
    }
    strip_pattern(&mut schema);
    schema
}

/// Remove every `pattern` keyword (a property *named* `pattern` stays):
/// an enforced one invents values (agora CLAUDE.md, "No `pattern` in a
/// strict schema")
fn strip_pattern(schema: &mut Value) {
    match schema {
        Value::Object(map) => {
            map.retain(|key, value| key != "pattern" || !value.is_string());
            for (key, value) in map.iter_mut() {
                match key.as_str() {
                    "properties" => {
                        if let Some(props) = value.as_object_mut() {
                            props.values_mut().for_each(strip_pattern);
                        }
                    }
                    "enum" | "const" | "default" | "examples" => {}
                    _ => strip_pattern(value),
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(strip_pattern),
        _ => {}
    }
}

/// `schema`, or `null`: `{"anyOf": [schema, {"type": "null"}]}`, the form
/// drama_llama's grammar keeps both branches of. Never `Option<T>`'s derived
/// schema (see the [module docs](self)).
pub fn nullable(schema: serde_json::Value) -> serde_json::Value {
    serde_json::json!({ "anyOf": [schema, { "type": "null" }] })
}

/// Property names that ask a model for its reasoning (agora CLAUDE.md,
/// "No field asks a model for its reasoning")
const REASONING_FIELDS: &[&str] =
    &["reasoning", "thinking", "trace", "scratchpad", "reason"];

/// Panics unless `schema` is fit to constrain a question with: no `$ref` or
/// `$defs`, no `pattern`, every object closed (`additionalProperties:
/// false`), no property named for reasoning (`reasoning`, `thinking`,
/// `trace`, `scratchpad`, `reason`), and no root `type` array holding
/// `"null"`. Every `Ask` in this crate and the runner is tested with it.
pub fn assert_constrainable(schema: &serde_json::Value) {
    if let Some(Value::Array(types)) = schema.get("type") {
        assert!(
            !types.iter().any(|t| t == "null"),
            "a root type array holding null (drama_llama collapses it, so \
             null is unwritable; use `nullable`): {schema}"
        );
    }
    check_constrainable(schema, "#", schema);
}

fn check_constrainable(value: &Value, path: &str, root: &Value) {
    match value {
        Value::Object(map) => {
            for banned in ["$ref", "$defs", "definitions"] {
                assert!(
                    !map.contains_key(banned),
                    "{banned} at {path}: {root}"
                );
            }
            assert!(
                !map.get("pattern").is_some_and(Value::is_string),
                "pattern at {path}: {root}"
            );
            let is_object = map.contains_key("properties")
                || match map.get("type") {
                    Some(Value::String(t)) => t == "object",
                    Some(Value::Array(ts)) => ts.iter().any(|t| t == "object"),
                    _ => false,
                };
            if is_object {
                assert_eq!(
                    map.get("additionalProperties"),
                    Some(&Value::Bool(false)),
                    "an open object at {path}: {root}"
                );
            }
            for (key, value) in map {
                match key.as_str() {
                    "properties" => {
                        let Some(props) = value.as_object() else {
                            continue;
                        };
                        for (name, prop) in props {
                            assert!(
                                !REASONING_FIELDS
                                    .contains(&name.to_lowercase().as_str()),
                                "a property named for reasoning, `{name}`, \
                                 at {path}: {root}"
                            );
                            check_constrainable(
                                prop,
                                &format!("{path}/properties/{name}"),
                                root,
                            );
                        }
                    }
                    "enum" | "const" | "default" | "examples" | "required"
                    | "description" | "title" => {}
                    _ => check_constrainable(
                        value,
                        &format!("{path}/{key}"),
                        root,
                    ),
                }
            }
        }
        Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                check_constrainable(item, &format!("{path}/{i}"), root);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests;
