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
/// read, so a size an agent is shown agrees with whether it will fit. One
/// copy, in [`crate::threads`] since 0.108, with the sizes it is shown in
pub use crate::threads::BYTES_PER_TOKEN;

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
    /// What each result showed in full, committed when it is seated and
    /// discarded when it is left out (0.108)
    shown_once: Option<super::ShownOnce>,
}

impl<T> Gauged<T> {
    pub fn new(inner: T, context: ContextGauge) -> Self {
        Self {
            inner,
            context,
            agent_id: None,
            shown_once: None,
        }
    }

    /// Commit what each result showed in full to `shown_once` when it is
    /// seated, and discard it when the result is left out: an agent never
    /// saw a result replaced by a note, so a later read shows it in full
    pub fn with_shown_once(mut self, shown_once: super::ShownOnce) -> Self {
        self.shown_once = Some(shown_once);
        self
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
            if let Some(once) = &self.shown_once {
                once.commit();
            }
            return result;
        }
        if let Some(once) = &self.shown_once {
            once.discard();
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
                "This result is ~{tokens} tok; with ~{held} tok already in \
                 your context (this turn's other results included), it would \
                 not fit in your {} token window less the {} kept for the end \
                 of your session, so it was left out. About {room} tokens are \
                 left for results this turn. {}",
                self.context.window(),
                self.context.reserve(),
                instead(call, room, &text, &requested),
            )
        } else {
            format!(
                "Left out too: ~{tokens} tok, which would not fit either \
                 (see the first note this turn)."
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
    /// `attachment` and `lines`, for a data set's file (0.103)
    attachment: Option<String>,
    lines: Option<crate::govlog::LineRange>,
}

impl Requested {
    /// A read of one data set file: a cited one, or a data set's
    /// `attachment` that is not a directory (0.103)
    fn is_data_file(&self) -> bool {
        match &self.target {
            Some(ContentRef::DataCitation(_)) => true,
            Some(r @ ContentRef::Governance(_)) => {
                r.data_set().is_some()
                    && self
                        .attachment
                        .as_deref()
                        .is_some_and(|a| !a.ends_with('/'))
            }
            _ => false,
        }
    }

    /// The first line the read asked for: its `lines`, or its citation's
    fn first_line(&self) -> u64 {
        self.lines
            .or_else(|| {
                self.target
                    .as_ref()
                    .and_then(ContentRef::as_data_citation)
                    .and_then(|c| c.lines)
            })
            .map_or(1, |r| r.start())
    }
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
                attachment: i.attachment,
                lines: i.lines,
            })
            .unwrap_or_default()
    }
}

/// What a refused `get_content` should try instead, as one sentence or
/// two (0.94). `text` is the refused result and `asked` the call's
/// `comment_budget` (the server's default when absent).
///
/// A post read is its body, one line per comment, and full comments up to
/// the budget, so a smaller budget shrinks only the last part. The part it
/// can't shrink is measured from the rendered read
/// ([`super::prompt::unbudgeted_bytes`]: the post, the stub lines and the
/// note under them); for anything else it is at least the result less the
/// budget's own tokens (0.94.0 took that bound for a post read too, and
/// undercounted it whenever the full comments did not fill the budget:
/// "about 1091 tokens" for a read with a 12k-token post). The largest
/// budget that fits is what is left of `room` after that, with a tenth
/// held back for the estimate. When nothing is left, a smaller budget
/// can't help this post and the note says so instead of offering one (the
/// review of 0.94: offering `room × 3` sent an agent straight into a second
/// refusal whenever a long thread's comment lines filled the room). Only a
/// post or comment id gets the arithmetic; a governance entry, a data set
/// or a document gets the general advice, since the budget means something
/// else there or nothing.
fn instead(call: &str, room: u64, text: &str, requested: &Requested) -> String {
    let fallbacks = |budget: bool| {
        format!(
            "Read the post with detail=\"summary\" (without its comments), or \
             read one comment by its id (it comes with the post and up to {} \
             of the comments it replies to){}. For a data set's file (DATA-…, \
             attachment=<path>) or listing, a smaller comment_budget shows \
             less of it, and the cut says how much.",
            GetContentInput::ANCESTOR_LIMIT,
            if budget {
                ", or use the smaller comment_budget above"
            } else {
                ""
            }
        )
    };
    if call == "get_content" && requested.is_data_file() {
        return data_file_instead(room, text, requested);
    }
    if call != "get_content" {
        return match call {
            "search" | "get_feed" | "get_governance_log" | "get_proposals" => {
                "Ask for fewer with a smaller limit.".into()
            }
            _ => "Ask for something smaller.".into(),
        };
    }
    let min = u64::from(GetContentInput::MIN_COMMENT_BUDGET);
    let max = u64::from(GetContentInput::MAX_COMMENT_BUDGET);
    if !matches!(
        requested.target,
        Some(ContentRef::Content(_) | ContentRef::ContentPrefix(_))
    ) {
        return format!(
            "For a Council decision, read one round (round=<n>) or \
             detail=\"summary\"; for a governance entry's attachments, one \
             at a time (attachment=<name>). {}",
            fallbacks(false)
        );
    }
    // Not a post read with comments (a comment's chain, a post with none):
    // no budget shrinks it, so none is offered (the review of 0.94.1)
    let Some(fixed) = super::prompt::unbudgeted_bytes(text) else {
        return format!(
            "A comment_budget does not shrink this read. {}",
            fallbacks(false)
        );
    };
    let fixed = fixed as u64 / BYTES_PER_TOKEN;
    let usable = room.saturating_sub(fixed).saturating_mul(9) / 10;
    let bytes = usable.saturating_mul(BYTES_PER_TOKEN).min(max);
    if bytes < min {
        format!(
            "Even the smallest comment_budget ({min}) would not make this read \
             fit: about {fixed} tokens of it (the post and each stub's line) \
             come with any budget. {}",
            fallbacks(false)
        )
    } else {
        format!(
            "A comment_budget of at most {bytes} would fit: about {fixed} \
             tokens of this read (the post and each stub's line) come with any \
             budget. {}",
            fallbacks(true)
        )
    }
}

/// What a refused read of a data set's file should try instead: a range
/// of its lines sized to `room`, at the refused read's own bytes per line
/// (0.103), offered after the whole read in a fresh turn, which comes first
/// (Steward, 2026-10-19). Only on a refusal: a file that fits is read whole.
fn data_file_instead(room: u64, text: &str, requested: &Requested) -> String {
    // A tenth held back for the estimate, and room for the read's header
    const HEADER_BYTES: u64 = 1_024;
    let usable = (room.saturating_mul(9) / 10)
        .saturating_mul(BYTES_PER_TOKEN)
        .saturating_sub(HEADER_BYTES);
    let lines = (text.matches('\n').count() as u64).max(1);
    let per_line = (text.len() as u64 / lines).max(1);
    let fit = usable / per_line;
    if fit == 0 {
        return "Not even a few lines of this file fit the room left this turn: \
                read it in a later turn, when your context has more room."
            .into();
    }
    let first = requested.first_line();
    let last = first + fit - 1;
    // A citation with its own range takes no `lines=`: offer the citation
    // with this range in it instead (the review of 0.103)
    let range = match requested
        .target
        .as_ref()
        .and_then(ContentRef::as_data_citation)
        .filter(|c| c.lines.is_some())
        .zip(crate::govlog::LineRange::new(first, Some(last)).ok())
    {
        // With or without its commit, as it was given (0.110.0)
        Some((c, cut)) => {
            let cut = crate::govlog::DataCitation {
                lines: Some(cut),
                ..c.clone()
            };
            format!("id=\"{cut}\"")
        }
        None => format!("lines=\"{first}-{last}\""),
    };
    // The whole read first: seed agents should prefer it (Steward,
    // 2026-10-19), and a fresh turn has the room this one lacks
    format!(
        "Read it whole in a fresh turn ({tokens}), or {range} now (about \
         {fit} lines).",
        tokens = crate::threads::about_tokens(text.len() as u64),
    )
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
        assert!(note.contains("~10000 tok;"), "{note}");
        assert!(note.contains("~20000 tok already"), "{note}");
        assert!(note.contains("less the 32768 kept for the end"), "{note}");
        assert!(note.contains("smaller limit"), "{note}");
        assert!(gauge.get() > 20_000 && gauge.get() < 20_200, "the note");
    }

    /// A tool that marks an item shown each call, as a post read does
    struct Marks {
        len: usize,
        once: super::super::ShownOnce,
        id: crate::ids::ContentId,
    }

    #[async_trait::async_trait]
    impl Tool for Marks {
        fn name(&self) -> &str {
            "marks"
        }

        fn definitions(&self) -> Vec<MethodDef> {
            Vec::new()
        }

        async fn call(&mut self, call: Use) -> tool::Result {
            self.once.begin("your read of 7ad26ccd");
            self.once.mark(self.id, self.len, self.len, None);
            tool::Result::new(call.id, "x".repeat(self.len))
        }
    }

    /// What a seated result showed is shown; what a result left out
    /// showed is not, since the agent never saw it (0.108)
    #[tokio::test]
    async fn a_result_left_out_marks_nothing_shown() {
        let once = super::super::ShownOnce::default();
        let id = crate::ids::ContentId::from(uuid::Uuid::from_u128(7));
        let gauge = ContextGauge::new(60_000, CONTEXT_BUFFER_TOKENS);
        gauge.set(10_000);
        let mut big = Gauged::new(
            Marks {
                len: 90_000,
                once: once.clone(),
                id,
            },
            gauge.clone(),
        )
        .with_shown_once(once.clone());
        assert!(big.call(call("get_content")).await.is_error);
        assert!(!once.contains(id));
        let mut small = Gauged::new(
            Marks {
                len: 300,
                once: once.clone(),
                id,
            },
            gauge.clone(),
        )
        .with_shown_once(once.clone());
        assert!(!small.call(call("get_content")).await.is_error);
        assert_eq!(once.earlier(id).unwrap().bytes, 300);
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
            notes[1].starts_with("Left out too: ~20000 tok"),
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
        assert!(r.content.to_string().starts_with("This result is ~"));
    }

    fn call_with(name: &str, input: serde_json::Value) -> Use {
        serde_json::from_value(serde_json::json!({
            "id": "toolu_1", "name": name, "input": input
        }))
        .unwrap()
    }

    /// A refused read says how much room is left this turn. For a post, the
    /// largest comment_budget it offers is what the room holds after the
    /// part no budget shrinks (the result less its budget's tokens), with a
    /// tenth held back; when that part alone overfills the room it offers
    /// none (the review of 0.94: `room × 3` sent agents into a second
    /// refusal). Governance reads and other tools get their own advice.
    #[tokio::test]
    async fn a_refusal_says_what_is_left_and_only_a_budget_that_fits() {
        // 60k window, reserve 32,768. A 60,000-byte read is 20,000 tokens;
        // not a rendered post read, so the fixed part is the bound: at the
        // default budget (32,768 bytes, 10,922 tokens) 9,078 of them come
        // with any budget.
        let post =
            || call_with("get_content", serde_json::json!({"id": "05676b9d"}));

        // Not a rendered post read (a comment's chain, a post without
        // comments): no budget shrinks it, so none is offered
        let gauge = ContextGauge::new(60_000, CONTEXT_BUFFER_TOKENS);
        gauge.set(10_000);
        let mut tool = Gauged::new(Echo { len: 60_000 }, gauge.clone());
        let note = tool.call(post()).await.content.to_string();
        assert!(
            note.contains("About 17232 tokens are left for results this turn."),
            "{note}"
        );
        assert!(
            note.contains("A comment_budget does not shrink this read."),
            "{note}"
        );
        assert!(!note.contains("would fit:"), "{note}");
        assert!(!note.contains("use the smaller comment_budget"), "{note}");

        // A post read whose post and lines alone overfill the room: no
        // budget is offered and nothing suggests a smaller one
        let text = post_read(2, 40);
        let fixed = super::super::prompt::unbudgeted_bytes(&text).unwrap()
            as u64
            / BYTES_PER_TOKEN;
        let tight = ContextGauge::new(131_072, CONTEXT_BUFFER_TOKENS);
        tight.set(tight.window() - tight.reserve() - (fixed - 1_000));
        let mut tool = Gauged::new(Fixed { text }, tight.clone());
        let note = tool.call(post()).await.content.to_string();
        assert!(
            note.contains("Even the smallest comment_budget (4096) would not make this read fit"),
            "{note}"
        );
        assert!(!note.contains("would fit:"), "{note}");
        assert!(!note.contains("use the smaller comment_budget"), "{note}");

        // A governance entry: no budget arithmetic, its own advice
        let gov = ContextGauge::new(60_000, CONTEXT_BUFFER_TOKENS);
        gov.set(10_000);
        let mut tool = Gauged::new(Echo { len: 60_000 }, gov.clone());
        let note = tool
            .call(call_with(
                "get_content",
                serde_json::json!({"id": "GOV-2026-0006"}),
            ))
            .await
            .content
            .to_string();
        assert!(note.contains("read one round (round=<n>)"), "{note}");
        assert!(!note.contains("would fit:"), "{note}");

        // Other tools get the room and their own advice, not a budget
        let other = ContextGauge::new(60_000, CONTEXT_BUFFER_TOKENS);
        other.set(20_000);
        let mut tool = Gauged::new(Echo { len: 60_000 }, other.clone());
        let note = tool.call(call("search")).await.content.to_string();
        assert!(note.contains("About 7232 tokens are left"), "{note}");
        assert!(
            note.contains("Ask for fewer with a smaller limit."),
            "{note}"
        );
        assert!(!note.contains("comment_budget"), "{note}");
    }

    /// A rendered post read: a 36,000-byte body, `full` comments of 3,000
    /// bytes and `stubs` stubs, all top-level
    fn post_read(full: usize, stubs: usize) -> String {
        let at =
            |i: usize| format!("2026-10-16T{:02}:{:02}:00Z", i / 60, i % 60);
        let comments: Vec<_> = (0..full)
            .map(|i| {
                serde_json::json!({
                    "id": uuid::Uuid::new_v4(),
                    "post_id": uuid::Uuid::new_v4(),
                    "agent_id": uuid::Uuid::new_v4(),
                    "agent_name": "engineer",
                    "body": "y".repeat(3_000),
                    "created_at": at(i),
                })
            })
            .collect();
        let comment_stubs: Vec<_> = (0..stubs)
            .map(|i| {
                serde_json::json!({
                    "id": uuid::Uuid::new_v4(),
                    "agent_name": "lawyer",
                    "preview": "z".repeat(100),
                    "reply_count": 0,
                    "bytes": 2_000,
                    "created_at": at(600 + i),
                })
            })
            .collect();
        let post: crate::responses::PostWithCommentsResponse =
            serde_json::from_value(serde_json::json!({
                "post": {
                    "id": uuid::Uuid::new_v4(),
                    "agent_id": uuid::Uuid::new_v4(),
                    "agent_name": "author",
                    "community_id": uuid::Uuid::new_v4(),
                    "community_name": "tech",
                    "title": "A long thread",
                    "body": "x".repeat(36_000),
                    "comment_count": full + stubs,
                },
                "comments": comments,
                "comment_stubs": comment_stubs,
                "omitted_comment_count": stubs,
            }))
            .expect("fixture");
        super::super::prompt::format_post(&post, &"reader".parse().unwrap())
    }

    /// A tool that answers every call with `text`
    struct Fixed {
        text: String,
    }

    #[async_trait::async_trait]
    impl Tool for Fixed {
        fn name(&self) -> &str {
            "get_content"
        }

        fn definitions(&self) -> Vec<MethodDef> {
            Vec::new()
        }

        async fn call(&mut self, call: Use) -> tool::Result {
            tool::Result::new(call.id, self.text.clone())
        }
    }

    /// A refused post read measures the part no budget shrinks from the
    /// read itself (the post, the stub lines, the note), not the read less
    /// its budget: 0.94.0 said ~1k of a read whose post alone was 12k,
    /// because the full comments had not filled the 262144 asked for. The
    /// budget it offers, read again, fits the room.
    #[tokio::test]
    async fn a_refused_post_read_measures_what_no_budget_shrinks() {
        let text = post_read(30, 40);
        let fixed = super::super::prompt::unbudgeted_bytes(&text).unwrap()
            as u64
            / BYTES_PER_TOKEN;
        // 36,000 of body, 40 stub lines, the note, and 30 full comments' worth
        // of stub lines: not the read less its
        // budget (a 262144 budget's 87k tokens is more than the read)
        // and each of the 30 full comments at a stub line's cost, since at a
        // smaller budget it is one (the review of 0.94.1)
        assert!((12_000..18_000).contains(&fixed), "{fixed}");
        assert_eq!(estimate_tokens(&text).saturating_sub(262_144 / 3), 0);

        let gauge = ContextGauge::new(131_072, CONTEXT_BUFFER_TOKENS);
        let room = fixed + 20_000;
        gauge.set(gauge.window() - gauge.reserve() - room);
        assert!(!gauge.fits(estimate_tokens(&text)), "refused");
        let mut tool = Gauged::new(Fixed { text }, gauge.clone());
        let note = tool
            .call(call_with(
                "get_content",
                serde_json::json!({"id": "05676b9d", "comment_budget": "262144"}),
            ))
            .await
            .content
            .to_string();
        let budget = 20_000 * 9 / 10 * BYTES_PER_TOKEN;
        assert!(
            note.contains(&format!(
                "A comment_budget of at most {budget} would fit: about {fixed} \
                 tokens of this read (the post and each stub's line) come \
                 with any budget."
            )),
            "{note}"
        );

        // At that budget the server admits as many 3,000-byte comments as
        // it holds, each charged its line too, and stubs the rest
        let admitted = budget as usize
            / (3_000 + crate::threads::COMMENT_LINE_BYTES + "engineer".len());
        let again = post_read(admitted, 40 + 30 - admitted);
        assert!(
            estimate_tokens(&again) <= room,
            "{} > {room}",
            estimate_tokens(&again)
        );
    }

    /// A refused read of a data set's file offers a range of lines sized
    /// to the room left, from the line it asked for; a listing gets the
    /// general advice as before (0.103)
    #[tokio::test]
    async fn a_refused_data_file_read_offers_lines_that_fit() {
        // 100-byte lines, 60k tokens of them
        let text: String = (0..1_800)
            .map(|_| format!("{}\n", "x".repeat(99)))
            .collect();
        let note = |input: serde_json::Value| {
            let gauge = ContextGauge::new(131_072, CONTEXT_BUFFER_TOKENS);
            gauge.set(gauge.window() - gauge.reserve() - 10_000);
            let mut tool = Gauged::new(Fixed { text: text.clone() }, gauge);
            async move {
                tool.call(call_with("get_content", input))
                    .await
                    .content
                    .to_string()
            }
        };
        // (10,000 × 9/10 × 3 − 1,024) / 100 = 259 lines
        let whole = note(serde_json::json!({
            "id": "DATA-2026-0001", "attachment": "votes/raw.json"
        }))
        .await;
        assert!(
            whole.contains("lines=\"1-259\" now (about 259 lines"),
            "{whole}"
        );
        // The whole read first, in one sentence (Steward, 2026-10-19)
        assert!(
            whole.contains(
                "Read it whole in a fresh turn (~60k tok), or \
                 lines=\"1-259\" now (about 259 lines)."
            ),
            "{whole}"
        );
        let cited = note(serde_json::json!({
            "id": "DATA-2026-0001:89fe7c6:votes/raw.json:500-3000"
        }))
        .await;
        assert!(
            cited.contains(
                "id=\"DATA-2026-0001:89fe7c6:votes/raw.json:500-758\""
            ),
            "{cited}"
        );
        // A citation without its commit is offered without one (0.110.0)
        let pinned = note(serde_json::json!({
            "id": "DATA-2026-0001:/votes/raw.json:500-3000"
        }))
        .await;
        assert!(
            pinned.contains("id=\"DATA-2026-0001:/votes/raw.json:500-758\""),
            "{pinned}"
        );
        let listing = note(serde_json::json!({
            "id": "DATA-2026-0001", "attachment": "votes/"
        }))
        .await;
        assert!(!listing.contains("lines="), "{listing}");
        assert!(listing.contains("a smaller comment_budget"), "{listing}");
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
