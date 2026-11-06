//! [`ContextGauge`]: how full an agent's context is. Outside `seed` so the
//! [`ask`](super::ask) driver can fit its questions to it; seed's `Gauged`
//! checks every tool result against it before the result is seated.

use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use misanthropic::prompt::{Prompt, thinking::Thinking};

/// [`SeedConfig::context_window`](crate::reactor::seed::SeedConfig::context_window)'s default: the smallest window an agent
/// on the platform is expected to have
pub const DEFAULT_CONTEXT_WINDOW: u64 = 128_000;

/// Tokens held back from the context window when deciding whether a tool
/// result fits, on a gauge built with [`ContextGauge::default`]. A
/// [`SeedAgent`](crate::reactor::seed::SeedAgent) sizes its reserve from its own budgets
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
/// between the [`SeedAgent`](crate::reactor::seed::SeedAgent), which reads each response's
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
        Self::new(DEFAULT_CONTEXT_WINDOW, CONTEXT_BUFFER_TOKENS)
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
    pub(crate) fn refuse(&self) -> u64 {
        self.0.refused.fetch_add(1, Ordering::Relaxed)
    }
}

/// The least `max_tokens` a question at the end of a session is sent with
/// (see [`phase_floor`])
pub const MIN_PHASE_TOKENS: u32 = 1_024;

/// The least `max_tokens` worth sending a question at the end of a session:
/// enough to answer in, and more than a fixed thinking budget (the API
/// refuses `max_tokens` at or under `budget_tokens`)
pub fn phase_floor(prompt: &Prompt) -> u32 {
    match prompt.thinking {
        Some(Thinking::Enabled { budget_tokens, .. }) => {
            MIN_PHASE_TOKENS.max(budget_tokens.get().saturating_add(1))
        }
        _ => MIN_PHASE_TOKENS,
    }
}

/// What [`fit_max_tokens`] did to a request's `max_tokens`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    /// The whole budget fits; nothing changed
    Whole,
    /// Lowered from `from` to `room`, what the context has room for
    Clamped { from: u32, room: u32 },
    /// Not even [`phase_floor`] fits: the request would be refused or
    /// useless, so it is not sent. `max_tokens` is unchanged
    NoRoom { room: u32, floor: u32 },
}

/// Lower `prompt`'s `max_tokens` to what the context has room for, so the
/// request is one the endpoint accepts: blallama refuses input +
/// `max_tokens` past its window, as Anthropic does. The reserve keeps the
/// memory rewrite room for its budget less what the last round added
/// unchecked (framing, refusal notes, estimation error), which this trims,
/// as it does for retries and the optional questions after it.
pub fn fit_max_tokens(gauge: &ContextGauge, prompt: &mut Prompt) -> Fit {
    let room = u32::try_from(gauge.phase_room()).unwrap_or(u32::MAX);
    let floor = phase_floor(prompt);
    if room < floor {
        return Fit::NoRoom { room, floor };
    }
    let from = prompt.max_tokens.get();
    if room >= from {
        return Fit::Whole;
    }
    prompt.max_tokens = NonZeroU32::new(room).expect("room >= floor > 0");
    Fit::Clamped { from, room }
}

/// Whether a failed reply to a question is left out of the retry: near the
/// edge, seating it would leave the retry no room to answer in. The retry
/// note then joins the question's own turn, which costs that tail on a
/// prefix cache but gives the retry its whole `budget` back (a memory
/// rewrite is worth more than the cache). When it is, the reply's
/// `output_tokens` are taken back off the gauge.
pub fn unseat_for_retry(
    gauge: &ContextGauge,
    budget: u32,
    output_tokens: u64,
) -> bool {
    let unseat = gauge.phase_room() < u64::from(budget);
    if unseat {
        gauge.set(gauge.get().saturating_sub(output_tokens));
    }
    unseat
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
pub fn estimate_tokens(text: &str) -> u64 {
    text.len() as u64 / BYTES_PER_TOKEN
}
