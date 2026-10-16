//! [`ContextGauge`] — how full an agent's context is — and [`Gauged`], which
//! checks every tool result against it before the result is seated.
//!
//! Results are gated *before* they join the prompt, so the prompt stays
//! append-only: an oversized result is replaced by a short note, and nothing
//! already seated is touched (see agora `memory/project_context_overflow_plan.md`).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use misanthropic::prompt::Prompt;
use misanthropic::tool::{self, MethodDef, Notifications, Tool, Use};

use crate::ids::{AgentId, ContentRef};
use crate::requests::GetContentInput;

/// Tokens held back from the context window when deciding whether a tool
/// result fits, on a gauge built with [`ContextGauge::default`]. A
/// [`SeedAgent`](super::SeedAgent) sizes its reserve from its own budgets
/// instead: see [`ContextGauge::reserve_for`].
pub const CONTEXT_BUFFER_TOKENS: u64 = 16_000;

/// Room for a closing-phase instruction (the memory rewrite, say), and the
/// "not run" answers seated before it, on top of the phase's `max_tokens`
pub const PHASE_PROMPT_TOKENS: u64 = 2_000;

/// The least a session keeps back for its end, whatever its window: the
/// closing turn, reflect and the memory rewrite, and an interview if the
/// agent asks to be contacted (Steward, 2026-10-14)
pub const CLOSING_RESERVE_MIN: u64 = 32_768;

/// The share of the window kept back for the end of a session: a quarter
/// (Steward, 2026-10-14; agreed 2026-10-09, agentkit#148)
pub const CLOSING_RESERVE_DIVISOR: u64 = 4;

/// The least room an acting round needs for its tool results. With less,
/// acting ends, since a round could do nothing (see [`ContextGauge::room`]).
/// The memory rewrite is kept by the reserve ([`ContextGauge::reserve`]),
/// not by this.
///
/// Small: the reserve already keeps the end of the session, and the gauge
/// refuses any one result that would not fit, with its size and cheaper
/// reads. At 8,000 a session with ~5k of room ended at turn 7 of 10 rather
/// than refusing its one large read, and the agent never learned the read's
/// size (beta-harness, 2026-10-14; Steward: refuse the one read). This is
/// room for a small read or a few refusal notes.
pub const MIN_ROUND_ROOM: u64 = 2_000;

/// Tokens in an agent's context as of its last response, plus the tool
/// results counted since, against the agent's context window. Shared
/// between the [`SeedAgent`](super::SeedAgent), which reads each response's
/// usage and sets the window from the admitted model, and the tools, which
/// decide whether a result fits.
#[derive(Debug, Clone)]
pub struct ContextGauge(Arc<Inner>);

#[derive(Debug)]
struct Inner {
    held: AtomicU64,
    window: AtomicU64,
    /// Results left out since the last response: after the first, a
    /// refusal's note is one line (see [`Gauged::admit`])
    refused: AtomicU64,
    /// The least reserve, from the session's own budgets; see
    /// [`ContextGauge::reserve`]
    floor: u64,
}

impl Default for ContextGauge {
    fn default() -> Self {
        Self::new(super::DEFAULT_CONTEXT_WINDOW, CONTEXT_BUFFER_TOKENS)
    }
}

impl ContextGauge {
    /// A gauge for a `window`-token context, holding at least `floor`
    /// tokens back from every tool result (see [`ContextGauge::reserve`])
    pub fn new(window: u64, floor: u64) -> Self {
        Self(Arc::new(Inner {
            held: AtomicU64::new(0),
            window: AtomicU64::new(window),
            refused: AtomicU64::new(0),
            floor,
        }))
    }

    /// The reserve that keeps a session able to close: one more acting
    /// turn's `max_tokens`, then the reflect instruction and its
    /// `max_tokens`. Every result admitted under it leaves room for the
    /// next request, and for reflect after that one.
    pub fn reserve_for(act_max_tokens: u32, phase_max_tokens: u32) -> u64 {
        u64::from(act_max_tokens)
            + u64::from(phase_max_tokens)
            + PHASE_PROMPT_TOKENS
    }

    pub fn get(&self) -> u64 {
        self.0.held.load(Ordering::Relaxed)
    }

    pub fn set(&self, tokens: u64) {
        self.0.held.store(tokens, Ordering::Relaxed);
    }

    /// Count a tool result about to join the context, so a second result in
    /// the same turn sees the first
    pub fn add(&self, tokens: u64) {
        self.0.held.fetch_add(tokens, Ordering::Relaxed);
    }

    /// The context window, in tokens
    pub fn window(&self) -> u64 {
        self.0.window.load(Ordering::Relaxed)
    }

    /// Set the context window, as the admitted model reports it
    pub fn set_window(&self, window: u64) {
        self.0.window.store(window, Ordering::Relaxed);
    }

    /// The tokens held back from every tool result for the end of the
    /// session: a quarter of the window, at least [`CLOSING_RESERVE_MIN`],
    /// and never less than the session's own budgets need. It follows the
    /// window, which the admitted model can narrow mid-setup.
    pub fn reserve(&self) -> u64 {
        (self.window() / CLOSING_RESERVE_DIVISOR)
            .max(CLOSING_RESERVE_MIN)
            .max(self.0.floor)
    }

    /// Whether `tokens` more, plus the reserve, fit in the window
    pub fn fits(&self, tokens: u64) -> bool {
        self.get() + tokens + self.reserve() <= self.window()
    }

    /// Tokens left for tool results before the reserve: what the next
    /// acting round has to work with
    pub fn room(&self) -> u64 {
        self.window()
            .saturating_sub(self.get())
            .saturating_sub(self.reserve())
    }

    /// The most a closing phase can generate: the window less what is held
    /// and the phase's instruction ([`PHASE_PROMPT_TOKENS`])
    pub fn phase_room(&self) -> u64 {
        self.window()
            .saturating_sub(self.get())
            .saturating_sub(PHASE_PROMPT_TOKENS)
    }

    /// Everything a response says is now in context: its whole input and
    /// its output
    pub fn record(&self, usage: &misanthropic::response::Usage) {
        let input = usage.input_tokens
            + usage.cache_read_input_tokens.unwrap_or(0)
            + usage.cache_creation_input_tokens.unwrap_or(0);
        self.set(input + usage.output_tokens);
        self.0.refused.store(0, Ordering::Relaxed);
    }

    /// Count a result left out this turn; how many were before it
    fn refuse(&self) -> u64 {
        self.0.refused.fetch_add(1, Ordering::Relaxed)
    }
}

/// Bytes per token in [`estimate_tokens`], and in the sizes shown before a
/// read, so a size an agent is shown agrees with whether it will fit
pub const BYTES_PER_TOKEN: u64 = 3;

/// A rough token count for `text`: a token for every three bytes. Measured
/// on 174 fleet sessions (2026-10-14: each logged request against the
/// endpoint's exact count), whole requests ran 3.2 to 5.3 bytes a token
/// (median 3.7 for Qwen, 4.4 for gpt-oss), so three still errs on the side
/// of the window. Until 0.83 it was two, which over-counted results 1.6 to
/// 2.2 times and refused reads that would have fit.
// TODO: count with the endpoint (`BatchBackend::count_tokens` /
// misanthropic `Client::count_tokens`), which needs the inference backend
// plumbed into the tools.
pub(super) fn estimate_tokens(text: &str) -> u64 {
    text.len() as u64 / BYTES_PER_TOKEN
}

/// A [`Tool`] whose every result is counted into a [`ContextGauge`], and
/// replaced by a short note when it would not fit in the context window
pub struct Gauged<T> {
    inner: T,
    context: ContextGauge,
    /// Whose session this is, for the log when a result is left out
    agent_id: Option<AgentId>,
}

impl<T> Gauged<T> {
    pub fn new(inner: T, context: ContextGauge) -> Self {
        Self {
            inner,
            context,
            agent_id: None,
        }
    }

    /// Name the agent in the log when a result is left out (0.94: the
    /// WARN said only the tool and the sizes, so a morning of refused
    /// reads could not be traced to who read what)
    pub fn for_agent(mut self, agent_id: AgentId) -> Self {
        self.agent_id = Some(agent_id);
        self
    }

    /// Count `result`, or stand a note in for it when it would not fit.
    /// `input` is the call's, for the log.
    fn admit(
        &self,
        call: &str,
        input: &serde_json::Value,
        mut result: tool::Result,
    ) -> tool::Result {
        let text = result.content.to_string();
        let tokens = estimate_tokens(&text);
        if self.context.fits(tokens) {
            self.context.add(tokens);
            return result;
        }
        let held = self.context.get();
        let room = self.context.room();
        let requested = Requested::of(call, input);
        tracing::warn!(
            agent_id = self.agent_id.as_ref().map(tracing::field::display),
            tool = call,
            target = requested.target.as_ref().map(tracing::field::display),
            comment_budget = requested.comment_budget,
            tokens,
            held,
            room,
            window = self.context.window(),
            "tool result would not fit in context; replaced by a note"
        );
        // Notes are counted but not refused, and a turn's calls have no cap:
        // after the first, a note is one line, so a wide turn of refusals
        // cannot crowd out the session's end (review of 0.89)
        let note = if self.context.refuse() == 0 {
            format!(
                "This result is about {tokens} tokens ({} KB); with about {held} \
                 already in your context (this turn's other results included), \
                 it would not fit in your {} token window less the {} kept for \
                 the end of your session, so it was left out. About {room} \
                 tokens are left for results this turn.{} {}",
                text.len() / 1024,
                self.context.window(),
                self.context.reserve(),
                budget_that_fits(call, room),
                smaller(call),
            )
        } else {
            format!(
                "Left out too: about {tokens} tokens ({} KB), which would not \
                 fit either (see the first note this turn).",
                text.len() / 1024
            )
        };
        self.context.add(estimate_tokens(&note));
        result.content = note.into();
        result.is_error = true;
        result
    }
}

/// What a left-out call asked for, for the log: `get_content`'s id (or
/// slug) and `comment_budget`. Nothing for other tools, or for an input
/// that does not parse (the call already answered, so it rarely won't).
#[derive(Debug, Default)]
struct Requested {
    target: Option<ContentRef>,
    comment_budget: Option<u32>,
}

impl Requested {
    fn of(call: &str, input: &serde_json::Value) -> Self {
        if call != "get_content" {
            return Self::default();
        }
        serde_json::from_value::<GetContentInput>(input.clone())
            .map(|i| Self {
                target: Some(i.id),
                comment_budget: i.comment_budget,
            })
            .unwrap_or_default()
    }
}

/// For a `get_content` refusal, the largest `comment_budget` the `room`
/// left would hold, at [`BYTES_PER_TOKEN`] and within the server's clamp,
/// or that none would (0.94). Empty for other tools.
fn budget_that_fits(call: &str, room: u64) -> String {
    if call != "get_content" {
        return String::new();
    }
    let min = u64::from(GetContentInput::MIN_COMMENT_BUDGET);
    let max = u64::from(GetContentInput::MAX_COMMENT_BUDGET);
    let bytes = room.saturating_mul(BYTES_PER_TOKEN);
    if bytes < min {
        format!(
            " That is less than the smallest comment_budget ({min}) holds, \
             so no read of a post with its comments fits this turn."
        )
    } else {
        format!(
            " For a post that is a comment_budget of at most {}, though the \
             post itself and the line listing each comment come on top of it.",
            bytes.min(max)
        )
    }
}

/// How to ask `call` for less
fn smaller(call: &str) -> std::borrow::Cow<'static, str> {
    match call {
        // Every comment is listed whatever the budget (paging, 0.86–0.89,
        // removed: each page cost a turn and the whole context again), so a
        // smaller budget shrinks a read but can't drop a long thread's lines
        "get_content" => format!(
            "Read a post with detail=\"summary\" (the post without its \
             comments), read one comment by its id (it comes with up to {} \
             of the comments above it), or use a smaller comment_budget \
             (down to {}), which shortens the full comments and previews \
             but still lists every comment. For a data set's file \
             (DATA-…, attachment=<path>) or listing, a smaller \
             comment_budget shows less of it, and the cut says how much.",
            crate::requests::GetContentInput::ANCESTOR_LIMIT,
            crate::requests::GetContentInput::MIN_COMMENT_BUDGET
        )
        .into(),
        "search" | "get_feed" | "get_governance_log" | "get_proposals" => {
            "Ask for fewer with a smaller limit.".into()
        }
        _ => "Ask for something smaller.".into(),
    }
}

#[async_trait::async_trait]
impl<T: Tool> Tool for Gauged<T> {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn definitions(&self) -> Vec<MethodDef> {
        self.inner.definitions()
    }

    async fn call(&mut self, call: Use) -> tool::Result {
        let name = call.name.clone();
        let input = call.input.clone();
        let result = self.inner.call(call).await;
        self.admit(&name, &input, result)
    }

    async fn save_json(&mut self) -> serde_json::Value {
        self.inner.save_json().await
    }

    async fn load_json(
        &mut self,
        json: serde_json::Value,
    ) -> Result<(), String> {
        self.inner.load_json(json).await
    }

    fn connect(&mut self, mailbox: tool::Mailbox) {
        self.inner.connect(mailbox)
    }

    async fn on_init(
        &mut self,
        prompt: &mut Prompt,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.inner.on_init(prompt).await
    }

    async fn on_turn(
        &mut self,
        prompt: &mut Prompt,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.inner.on_turn(prompt).await
    }

    async fn on_teardown(
        &mut self,
        prompt: &mut Prompt,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.inner.on_teardown(prompt).await
    }

    fn subscribe(&mut self) -> Option<Notifications> {
        self.inner.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cached input is still in context: a cache hit is not a small prompt
    #[test]
    fn the_gauge_counts_cached_input_and_output() {
        let gauge = ContextGauge::default();
        let usage: misanthropic::response::Usage = serde_json::from_str(
            r#"{
                "input_tokens": 10,
                "cache_read_input_tokens": 90000,
                "cache_creation_input_tokens": 1000,
                "output_tokens": 500
            }"#,
        )
        .unwrap();
        gauge.record(&usage);
        assert_eq!(gauge.get(), 91_510);
        gauge.add(10);
        assert_eq!(gauge.clone().get(), 91_520, "shared, not copied");
    }

    /// A tool that answers every call with `len` bytes
    struct Echo {
        len: usize,
    }

    #[async_trait::async_trait]
    impl Tool for Echo {
        fn name(&self) -> &str {
            "echo"
        }

        fn definitions(&self) -> Vec<MethodDef> {
            Vec::new()
        }

        async fn call(&mut self, call: Use) -> tool::Result {
            tool::Result::new(call.id, "x".repeat(self.len))
        }
    }

    fn call(name: &str) -> Use {
        serde_json::from_str(&format!(
            r#"{{"id": "toolu_1", "name": "{name}", "input": {{}}}}"#
        ))
        .unwrap()
    }

    /// A result that fits is passed through and counted, so the next one
    /// in the same turn sees it; one that doesn't is a note, counted too
    #[tokio::test]
    async fn every_result_is_counted_and_an_oversized_one_left_out() {
        let gauge = ContextGauge::new(60_000, CONTEXT_BUFFER_TOKENS);
        gauge.set(10_000);
        // 30 KB is 10k tokens; the reserve is CLOSING_RESERVE_MIN (a
        // quarter of 60k is less): 10k + 10k + 32,768 fits in 60k, twice
        // does not.
        let mut tool = Gauged::new(Echo { len: 30_000 }, gauge.clone());

        let first = tool.call(call("search")).await;
        assert!(!first.is_error);
        assert_eq!(first.content.to_string().len(), 30_000);
        assert_eq!(gauge.get(), 20_000);

        let second = tool.call(call("search")).await;
        assert!(second.is_error);
        assert_eq!(second.tool_use_id, "toolu_1", "still answers the call");
        let note = second.content.to_string();
        assert!(note.contains("about 10000 tokens (29 KB)"), "{note}");
        assert!(note.contains("about 20000 already"), "{note}");
        assert!(note.contains("less the 32768 kept for the end"), "{note}");
        assert!(note.contains("smaller limit"), "{note}");
        assert!(gauge.get() > 20_000 && gauge.get() < 20_200, "the note");
    }

    /// The fleet's settings (131k window, act and phase 16,384, so the
    /// reserve is its floor, 34,768) at the least room a round runs with: a
    /// wide turn of refused reads still leaves the next acting request and
    /// reflect inside the window, because after the first the notes are one
    /// line (review of 0.89: 90 full notes would have overrun it)
    #[tokio::test]
    async fn a_wide_turn_of_refusals_leaves_the_session_its_end() {
        let (act, phase) = (16_384u32, 16_384u32);
        let gauge =
            ContextGauge::new(131_072, ContextGauge::reserve_for(act, phase));
        assert_eq!(gauge.reserve(), 34_768);
        gauge.set(131_072 - 34_768 - MIN_ROUND_ROOM);
        assert_eq!(gauge.room(), MIN_ROUND_ROOM);
        let mut tool = Gauged::new(Echo { len: 60_000 }, gauge.clone());
        let mut notes = Vec::new();
        for _ in 0..100 {
            let r = tool.call(call("get_content")).await;
            assert!(r.is_error);
            notes.push(r.content.to_string());
        }
        assert!(notes[0].contains("would not fit in your 131072 token window"));
        assert!(
            notes[1].starts_with("Left out too: about 20000 tokens"),
            "{}",
            notes[1]
        );
        // The next acting request fits, and reflect keeps most of its budget
        assert!(
            gauge.get() + u64::from(act) <= gauge.window(),
            "{}",
            gauge.get()
        );
        let reflect =
            gauge.window() - gauge.get() - u64::from(act) - PHASE_PROMPT_TOKENS;
        assert!(reflect >= 12_000, "reflect keeps {reflect} of {phase}");
        // A new response resets the count: its first refusal is whole again
        gauge.record(&misanthropic::response::Usage::default());
        gauge.set(131_072 - 34_768 - MIN_ROUND_ROOM);
        let r = tool.call(call("get_content")).await;
        assert!(r.content.to_string().starts_with("This result is about"));
    }

    /// A refused read says how much room is left this turn, and for
    /// get_content the largest comment_budget that holds: tokens × 3,
    /// within 4096..=262144, or that none does below 4096 (0.94)
    #[tokio::test]
    async fn a_refusal_says_what_is_left_and_the_budget_that_fits() {
        // 60k window, reserve 32,768: 20k held leaves 7,232 tokens of room,
        // 21,696 bytes of comment_budget
        let gauge = ContextGauge::new(60_000, CONTEXT_BUFFER_TOKENS);
        gauge.set(20_000);
        let mut tool = Gauged::new(Echo { len: 60_000 }, gauge.clone());
        let note = tool.call(call("get_content")).await.content.to_string();
        assert!(
            note.contains("About 7232 tokens are left for results this turn."),
            "{note}"
        );
        assert!(
            note.contains("a comment_budget of at most 21696,"),
            "{note}"
        );

        // Room past the clamp: the most the server takes
        let roomy = ContextGauge::new(1_000_000, CONTEXT_BUFFER_TOKENS);
        roomy.set(1_000_000 - 250_000 - 150_000);
        let mut tool = Gauged::new(Echo { len: 600_000 }, roomy.clone());
        let note = tool.call(call("get_content")).await.content.to_string();
        assert!(note.contains("About 150000 tokens are left"), "{note}");
        assert!(note.contains("comment_budget of at most 262144,"), "{note}");

        // Under the floor: says none fits
        let tight = ContextGauge::new(60_000, CONTEXT_BUFFER_TOKENS);
        tight.set(60_000 - 32_768 - 1_000);
        let mut tool = Gauged::new(Echo { len: 60_000 }, tight.clone());
        let note = tool.call(call("get_content")).await.content.to_string();
        assert!(note.contains("About 1000 tokens are left"), "{note}");
        assert!(
            note.contains(
                "less than the smallest comment_budget (4096) holds, so no \
                 read of a post with its comments fits this turn."
            ),
            "{note}"
        );

        // Other tools get the room, not a budget
        let other = ContextGauge::new(60_000, CONTEXT_BUFFER_TOKENS);
        other.set(20_000);
        let mut tool = Gauged::new(Echo { len: 60_000 }, other.clone());
        let note = tool.call(call("search")).await.content.to_string();
        assert!(note.contains("About 7232 tokens are left"), "{note}");
        assert!(!note.contains("comment_budget"), "{note}");
    }

    /// The log names what a refused get_content asked for
    #[test]
    fn a_refused_reads_target_and_budget_are_parsed_for_the_log() {
        let r = Requested::of(
            "get_content",
            &serde_json::json!({"id": "05676b9d", "comment_budget": "262144"}),
        );
        assert_eq!(r.target.unwrap().to_string(), "05676b9d");
        assert_eq!(r.comment_budget, Some(262_144));
        let r = Requested::of(
            "get_content",
            &serde_json::json!({"id": "protocol"}),
        );
        assert_eq!(r.target.unwrap().to_string(), "protocol");
        assert_eq!(r.comment_budget, None);
        let r = Requested::of("search", &serde_json::json!({"query": "x"}));
        assert!(r.target.is_none() && r.comment_budget.is_none());
    }

    /// A quarter of the window, at least CLOSING_RESERVE_MIN, at least the
    /// budgets; and it follows the window
    #[test]
    fn the_reserve_is_a_quarter_of_the_window_with_a_floor() {
        let gauge = ContextGauge::new(262_144, 10_000);
        assert_eq!(gauge.reserve(), 65_536);
        gauge.set_window(131_072);
        assert_eq!(gauge.reserve(), 32_768);
        gauge.set_window(64_000);
        assert_eq!(gauge.reserve(), CLOSING_RESERVE_MIN);
        let budgets = ContextGauge::new(64_000, 40_000);
        assert_eq!(budgets.reserve(), 40_000);
    }
}
