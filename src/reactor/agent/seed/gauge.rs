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

/// Tokens held back from the context window when deciding whether a tool
/// result fits, on a gauge built with [`ContextGauge::default`]. A
/// [`SeedAgent`](super::SeedAgent) sizes its reserve from its own budgets
/// instead: see [`ContextGauge::reserve_for`].
pub const CONTEXT_BUFFER_TOKENS: u64 = 16_000;

/// Room for a closing-phase instruction (the memory rewrite, say), and the
/// "not run" answers seated before it, on top of the phase's `max_tokens`
pub const PHASE_PROMPT_TOKENS: u64 = 2_000;

/// The least room an acting round needs for its tool results. With less,
/// acting ends and the session goes on to reflect while there is still
/// room to write memory (see [`ContextGauge::room`]).
pub const MIN_ROUND_ROOM: u64 = 8_000;

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
    reserve: u64,
}

impl Default for ContextGauge {
    fn default() -> Self {
        Self::new(super::DEFAULT_CONTEXT_WINDOW, CONTEXT_BUFFER_TOKENS)
    }
}

impl ContextGauge {
    /// A gauge for a `window`-token context, holding `reserve` tokens back
    /// from every tool result
    pub fn new(window: u64, reserve: u64) -> Self {
        Self(Arc::new(Inner {
            held: AtomicU64::new(0),
            window: AtomicU64::new(window),
            reserve,
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

    /// The tokens held back from every tool result
    pub fn reserve(&self) -> u64 {
        self.0.reserve
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
    }
}

/// A rough token count for `text`: a byte for every three.
// TODO: count with the endpoint (`BatchBackend::count_tokens` /
// misanthropic `Client::count_tokens`), which needs the inference backend
// plumbed into the tools.
pub(super) fn estimate_tokens(text: &str) -> u64 {
    text.len() as u64 / 3
}

/// A [`Tool`] whose every result is counted into a [`ContextGauge`], and
/// replaced by a short note when it would not fit in the context window
pub struct Gauged<T> {
    inner: T,
    context: ContextGauge,
}

impl<T> Gauged<T> {
    pub fn new(inner: T, context: ContextGauge) -> Self {
        Self { inner, context }
    }

    /// Count `result`, or stand a note in for it when it would not fit
    fn admit(&self, call: &str, mut result: tool::Result) -> tool::Result {
        let text = result.content.to_string();
        let tokens = estimate_tokens(&text);
        if self.context.fits(tokens) {
            self.context.add(tokens);
            return result;
        }
        let held = self.context.get();
        tracing::warn!(
            tool = call,
            tokens,
            held,
            window = self.context.window(),
            "tool result would not fit in context; replaced by a note"
        );
        let note = format!(
            "This result is about {tokens} tokens ({} KB); with about {held} \
             already in your context it would not fit in your {} token \
             window, so it was left out. {}",
            text.len() / 1024,
            self.context.window(),
            smaller(call),
        );
        self.context.add(estimate_tokens(&note));
        result.content = note.into();
        result.is_error = true;
        result
    }
}

/// How to ask `call` for less
fn smaller(call: &str) -> &'static str {
    match call {
        "get_content" => {
            "Read a post with detail=\"summary\" for it without its comments, or \
             read one comment by its id."
        }
        "search" | "get_feed" | "get_governance_log" | "get_proposals" => {
            "Ask for fewer with a smaller limit."
        }
        _ => "Ask for something smaller.",
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
        let result = self.inner.call(call).await;
        self.admit(&name, result)
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
        let gauge = ContextGauge::new(40_000, CONTEXT_BUFFER_TOKENS);
        gauge.set(10_000);
        // 30 KB is 10k tokens: 10k + 10k + 16k fits in 40k, twice does not.
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
        assert!(note.contains("smaller limit"), "{note}");
        assert!(gauge.get() > 20_000 && gauge.get() < 20_200, "the note");
    }
}
