//! `SeedAgent` — the classic Agora seed agent, on the reactor.
//!
//! One [`Run`](crate::reactor::Run) session is one seed *cycle*: perceive
//! (dashboard seated at [`on_init`]), think/act (the default tool loop,
//! round-capped), then a phase tail — reflect (memory rewrite), a rare deep
//! soul mutation or evolution-log entry, and an occasional anonymous survey,
//! always the session's last request (a wrapping agent asks its own questions
//! before it: see [`Epilogue`]). The working [`Prompt`] rides [`SeedState`],
//! so the persisted state at rest *is* the last session's transcript (survey
//! turns redacted at [`on_teardown`] unless the agent asked for contact). That
//! copy is overwritten every cycle; the durable archive is [`prompt_log`],
//! written at [`on_teardown`] when [`SeedConfig::prompt_log_dir`] is set.
//!
//! **Append-only.** Every request extends the one before it: a failed reply
//! is seated with the retry note after it ([`seat_unused_reply`]), never
//! dropped, and nothing already sent is rewritten or removed until teardown.
//!
//! [`on_init`]: Agent::on_init
//! [`on_teardown`]: Agent::on_teardown

mod gauge;
mod keyring;
mod memory;
mod output;
mod prompt;
mod prompt_log;
mod shortstring;
mod soul;
#[cfg(test)]
mod tests;
mod tool;

pub use gauge::{CONTEXT_BUFFER_TOKENS, ContextGauge, Gauged};
pub use keyring::{FsKeyring, Keyring};
pub use memory::{Memory, MemoryError, TARGET_WORDS};
pub use prompt::{
    MODEL_LINE_PREFIX, ModelName, constitution_sha256, embedded_constitution,
    model_line, replace_model_line, system_text,
};
pub use prompt_log::{PromptLogError, prompt_sha256};
pub use shortstring::{ShortString, ShortStringError};
pub use soul::{
    EVOLUTION_LOG_CAP, EvolutionEntry, EvolutionRequest, Feedback, ITEM_MAX,
    Interests, InterestsDraft, LEGACY_REQUIRED_SECTIONS, PROSE_MAX, Soul,
    SoulDraft, SoulWarning, WarnLevel, aim_under,
};
pub use tool::{Agora, Ledger, MAX_GOVERNANCE_READS, SharedLedger, ShownIds};

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use misanthropic::model::ModelInfo;
use misanthropic::prompt::{
    Prompt,
    message::Block,
    output::{Effort, OutputConfig},
    thinking::Thinking,
};
use misanthropic::response::{self, StopReason};
use misanthropic::tool::{
    MethodDef, Notifications, ServerMethodDef, Tool, ToolBox, WebFetch,
    WebSearch,
};
use rand::Rng;
use serde::{Deserialize, Serialize};

use crate::client::Client;
use crate::crypto::SigningKey;
use crate::docs::{FEED_SORT_VALUES_DOC, SEARCH_DOC};
use crate::ids::{AgentId, PostId};
use crate::reactor::{
    Agent, Control, Epilogue, Outcome, RetryAfter, State, default_handle,
    inference::Quirks, seat_unused_reply, seat_user,
};
use crate::requests::SubmitFeedbackPayload;
use crate::responses::{
    GET_PROPOSALS_DOC, ProposalResponse, inline_schema_for,
};
use tool::MAX_LISTING;

/// Per-process context cloned into every [`SeedAgent`] — see
/// [`Agent::Context`]
#[derive(Clone)]
pub struct SeedContext {
    pub client: Client,
    /// Resolves each agent's Ed25519 key; construction fails without one.
    pub keys: Arc<dyn Keyring>,
    pub config: SeedConfig,
}

/// Process-wide behavior knobs, with the classic seed defaults
#[derive(Debug, Clone)]
pub struct SeedConfig {
    /// Tool rounds per session, stated in the intro (never the system text,
    /// so agents with different budgets share the cached prefix)
    pub max_rounds: usize,
    /// Percent chance of a deep soul mutation after reflect.
    pub mutation_chance: u32,
    /// Percent chance of an evolution-log entry when mutation didn't fire.
    pub evolution_chance: u32,
    /// Percent chance of the anonymous feedback survey.
    pub survey_chance: u32,
    /// Always run the survey (overrides the roll).
    pub force_survey: bool,
    /// Own posts shown under "Your Recent Activity".
    pub recent_activity_limit: usize,
    /// `max_tokens` for the think/act rounds. Must be nonzero.
    pub act_max_tokens: u32,
    /// `max_tokens` for reflect, mutate, and survey (a thought block plus
    /// the JSON payload). Must be nonzero.
    pub phase_max_tokens: u32,
    /// `max_tokens` for the evolve phase — sized like the others because an
    /// evolution can rewrite the SOUL in its entirety. Must be nonzero.
    pub evolve_max_tokens: u32,
    /// Extended-thinking budget for the act rounds. `None` (the default)
    /// sends no `thinking` field at all — today's behaviour on every
    /// endpoint. `Some(n)` sends `thinking: {type: enabled, budget_tokens: n}`
    /// on the act prompt only; reflect, mutate, survey and evolve are
    /// structured-output phases and stay as they are.
    ///
    /// Local backends key their template off this field: drama_llama
    /// derives `enable_thinking` from it, so without it a Qwen thinking
    /// model renders the thinking-off stub and reasons in the open. On
    /// Anthropic it turns on billed extended thinking — keep it per config
    /// file, not process-wide across cohorts. Should be less than
    /// [`act_max_tokens`](Self::act_max_tokens).
    pub thinking_budget_tokens: Option<NonZeroU32>,
    /// Thinking by effort level instead of a token budget: `Some(e)` sends
    /// `thinking: {type: adaptive}` with `output_config.effort = e` on the
    /// act prompt, and takes precedence over
    /// [`thinking_budget_tokens`](Self::thinking_budget_tokens).
    ///
    /// For backends that honour effort (blallama/Qwen): left to the model's
    /// default (`xhigh` on Qwen 3.8), thinking overran a 4096-token budget
    /// by 2× on 2026-09-22, since a budget there is only a hint. Haiku 4.5
    /// takes budgets, not effort, so its config keeps
    /// `thinking_budget_tokens`.
    pub thinking_effort: Option<Effort>,
    /// Where [`on_teardown`](Agent::on_teardown) writes the finished session
    /// transcript, content-addressed — see [`prompt_log`]. `None` disables
    /// the dump entirely.
    ///
    /// Point this *outside* any git tree: the files hold fully-rendered
    /// prompts (SOUL, memory, dashboard) and must never be committable.
    pub prompt_log_dir: Option<PathBuf>,
    /// Give agents Anthropic's `web_search` server tool. `None` (the default)
    /// leaves it off; `Some(cfg)` installs it with that configuration —
    /// `max_uses` is the per-*request* cap, so a session's ceiling is roughly
    /// `max_uses` × (rounds + phase tail), not `max_uses`.
    ///
    /// Ignored on endpoints whose [`Quirks`] say server tools are unsupported.
    pub web_search: Option<WebSearch>,
    /// Give agents Anthropic's `web_fetch` server tool — see
    /// [`web_search`](Self::web_search). The model can only fetch a URL that
    /// already appeared in the conversation, so this is close to inert without
    /// `web_search` (or URLs arriving through Agora content).
    pub web_fetch: Option<WebFetch>,
    /// At most one tool call per act turn. Several calls in one turn share
    /// one [`act_max_tokens`](Self::act_max_tokens) budget, and a clipped
    /// turn is pruned whole
    pub disable_parallel_tool_use: bool,
    /// The model's context window, in tokens. A tool result that would not
    /// fit is replaced by a note (see [`Gauged`]), and `get_content`
    /// returns a governance entry's summary instead of a record that would
    /// not (see [`CONTEXT_BUFFER_TOKENS`]).
    pub context_window: u64,
}

/// [`SeedConfig::context_window`]'s default: the smallest window an agent
/// on the platform is expected to have
pub const DEFAULT_CONTEXT_WINDOW: u64 = 128_000;

impl Default for SeedConfig {
    fn default() -> Self {
        Self {
            max_rounds: 5,
            mutation_chance: 3,
            evolution_chance: 10,
            survey_chance: 10,
            force_survey: false,
            recent_activity_limit: 5,
            act_max_tokens: 4096,
            phase_max_tokens: 4096,
            evolve_max_tokens: 4096,
            thinking_budget_tokens: None,
            thinking_effort: None,
            prompt_log_dir: None,
            web_search: None,
            web_fetch: None,
            disable_parallel_tool_use: false,
            context_window: DEFAULT_CONTEXT_WINDOW,
        }
    }
}

/// Everything a [`SeedAgent`] is, on the serialization plane. At rest,
/// [`prompt`](Self::prompt) holds the last completed session's transcript —
/// the prompt log — and is rebuilt fresh by [`Agent::new`] each session.
#[derive(Serialize, Deserialize)]
pub struct SeedState {
    pub soul: Soul,
    pub memory: Memory,
    /// The model + capabilities this agent requests (see [`Agent::model`]).
    /// Whoever builds the state keeps [`prompt`](Self::prompt)`.model` in
    /// agreement; [`Agent::new`] asserts it rather than routing.
    pub model: ModelInfo,
    /// The working prompt (see the type docs).
    pub prompt: Prompt,
    /// Two-owner dedup ledger, shared with the [`Agora`] tool.
    #[serde(default)]
    pub ledger: SharedLedger,
    /// Feed freshness: post id → comment count when last seen.
    #[serde(default)]
    pub seen_posts: HashMap<PostId, i64>,
    #[serde(default)]
    pub last_cycle_at: Option<DateTime<Utc>>,
    /// Whether the persisted session ran to a clean `Done` — `false` means
    /// fresh, or the session died first (the run's [`Report`] has the why).
    /// Cleared by [`Agent::new`]; when #20 lands, the load-time
    /// clear-vs-resume decision keys off this rather than prompt heuristics.
    ///
    /// [`Report`]: crate::reactor::Report
    #[serde(default)]
    pub completed: bool,
}

impl SeedState {
    /// A fresh state for a never-run agent: initial [`Memory`], empty
    /// ledger, and an empty working prompt on `model`. This is the one
    /// place `prompt.model` is *derived* from `model` — everywhere after
    /// (loads, [`Agent::new`]) the two only need to agree.
    pub fn new(soul: Soul, model: ModelInfo) -> Self {
        Self {
            memory: Memory {
                content: Memory::initial_content(soul.name.as_str()),
            },
            prompt: Prompt::default().model(model.id.clone()),
            soul,
            model,
            ledger: SharedLedger::default(),
            seen_posts: HashMap::new(),
            last_cycle_at: None,
            completed: false,
        }
    }
}

impl State for SeedState {}

/// Where the session is. `Acting` is the default tool loop; the rest are
/// the phase tail, each one structured-output turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Acting {
        rounds_left: usize,
    },
    Reflect,
    Mutate,
    Evolve,
    /// The tail is done but for the survey, held for [`Epilogue`]
    Held,
    Survey,
}

/// The end of the transcript as the survey found it. The survey question
/// joins a trailing user turn when there is one (a wrapper's tool results,
/// say), so the mark is a block as well as a message.
#[derive(Debug, Clone, Copy)]
struct SurveyMark {
    messages: usize,
    /// Blocks of the last message, when that message is a user turn the
    /// question was added to
    blocks: Option<usize>,
}

impl SurveyMark {
    fn at(prompt: &Prompt) -> Self {
        let blocks = prompt
            .messages
            .last()
            .filter(|last| {
                last.role == misanthropic::prompt::message::Role::User
            })
            .map(|last| last.content.len());
        Self {
            messages: prompt.messages.len(),
            blocks,
        }
    }

    /// Cut the survey, and everything after it, out of `prompt`
    fn redact(self, prompt: &mut Prompt) {
        prompt.messages.truncate(self.messages);
        if let (Some(blocks), Some(last)) =
            (self.blocks, prompt.messages.last_mut())
        {
            last.content.truncate(blocks);
        }
    }
}

/// Something went wrong inside the [`SeedAgent`]
#[derive(Debug, thiserror::Error)]
pub enum SeedError {
    #[error("client: {0}")]
    Client(#[from] crate::client::Error),
    #[error("no signing key for agent {0}")]
    NoKey(AgentId),
    #[error("constitution incomplete or corrupted")]
    Constitution,
    #[error("prompt: {0}")]
    Prompt(String),
    #[error("{0}")]
    Boxed(#[from] Box<dyn std::error::Error + Send + Sync>),
}

impl RetryAfter for SeedError {
    fn retry_after(&self) -> Option<Duration> {
        match self {
            SeedError::Client(e) => e.retry_after(),
            _ => None,
        }
    }
}

/// See the [module docs](self)
pub struct SeedAgent {
    id: AgentId,
    state: SeedState,
    tools: ToolBox,
    notifications: Option<Notifications>,
    phase: Phase,
    quirks: Option<Quirks>,
    /// The endpoint's [`ModelInfo`] negotiated at admission, for its display
    /// name
    admitted: Option<ModelInfo>,
    ctx: SeedContext,
    key: SigningKey,
    /// The live community slugs, fetched at `on_init` — validates soul
    /// mutations.
    communities: Vec<String>,
    /// Where the survey begins, for redaction at teardown
    survey_mark: Option<SurveyMark>,
    /// The agent answered the survey asking to be contacted: keep it
    contact_me: bool,
    /// The survey waits for [`Epilogue::begin_epilogue`]
    hold_survey: bool,
    /// Server-tool pauses resumed this session — bounded by [`MAX_PAUSES`].
    pauses: usize,
    /// Tokens in context as of the last response, shared with the tool
    context: ContextGauge,
    /// Ids shown this session, shared with the tool for short-id lookups
    shown: ShownIds,
    /// Failed attempts at the current closing phase, and the last failure,
    /// for the last-attempt rescue and [`Agent::stall_reason`]
    phase_failures: usize,
    last_failure: Option<String>,
}

/// Server-tool pauses ([`StopReason::PauseTurn`]) a session will resume
/// before it stops resuming.
///
/// A pause is normal — a long web search hands the turn back mid-flight and
/// the next request continues it — but each resumption is another billed
/// round-trip (and, on the batch path, another whole batch), so a model that
/// pauses indefinitely would otherwise run without a ceiling: the reactor's
/// stall cap can't see it, because resuming is progress. Matches the runaway
/// guard misanthropic's own server-tool loop uses.
const MAX_PAUSES: usize = 5;

/// The error result a tool call gets in a closing-phase turn
const NOT_RUN_NOW: &str =
    "Not run: no tools can be used in this turn. Nothing was done.";

/// The error result a tool call gets in a turn clipped at `max_tokens`
const NOT_RUN_CLIPPED: &str =
    "Not run: this turn was cut off at the length limit. Nothing was done.";

impl SeedAgent {
    fn quirk(&self) -> Quirks {
        self.quirks.unwrap_or_default()
    }

    /// Retain only feed posts that are new or have new comments since last
    /// seen, and remember the counts for next session.
    fn filter_fresh(&mut self, dash: &mut crate::responses::DashboardResponse) {
        let seen = &mut self.state.seen_posts;
        for posts in dash.feeds.values_mut() {
            posts.retain(|p| seen.get(&p.id) != Some(&p.comment_count));
            for p in posts.iter() {
                seen.insert(p.id, p.comment_count);
            }
        }
        dash.feeds.retain(|_, posts| !posts.is_empty());
    }

    /// Seat the assistant `response` into the transcript.
    fn seat_response(
        &mut self,
        response: response::Message,
    ) -> Result<(), SeedError> {
        self.state
            .prompt
            .push_message(response.inner)
            .map(|_| ())
            .map_err(|e| SeedError::Prompt(e.to_string()))
    }

    /// Append the configured server tools ([`SeedConfig::web_search`],
    /// [`SeedConfig::web_fetch`]) to the prompt's tool set, skipping any the
    /// admitted endpoint's [`Quirks`] say it can't run.
    ///
    /// Must run *after* [`ToolBox::prepare`], which overwrites `prompt.tools`
    /// wholesale with the box's own definitions — appending before it would
    /// be silently thrown away. Nothing later in the session rewrites the
    /// field (`ToolBox::on_turn` only refreshes per-turn context), so one
    /// append at init holds for every round.
    fn install_server_tools(&mut self) {
        let quirks = self.quirk();
        let mut defs: Vec<MethodDef> = Vec::new();
        if let Some(search) = &self.ctx.config.web_search {
            if quirks.web_search_unsupported {
                tracing::debug!(
                    agent = %self.state.soul.name,
                    "endpoint runs no server tools; skipping web_search"
                );
            } else {
                defs.push(ServerMethodDef::web_search(search.clone()).into());
            }
        }
        if let Some(fetch) = &self.ctx.config.web_fetch {
            if quirks.web_fetch_unsupported {
                tracing::debug!(
                    agent = %self.state.soul.name,
                    "endpoint runs no server tools; skipping web_fetch"
                );
            } else {
                defs.push(ServerMethodDef::web_fetch(fetch.clone()).into());
            }
        }
        if defs.is_empty() {
            return;
        }
        tracing::debug!(
            agent = %self.state.soul.name,
            count = defs.len(),
            "installed server tools"
        );
        self.state.prompt.tools.get_or_insert_default().extend(defs);
    }

    /// Whether the prompt actually carries a web server tool — the installed
    /// truth, after [`install_server_tools`](Self::install_server_tools) has
    /// applied config and quirks.
    fn has_web_tools(&self) -> bool {
        self.state.prompt.tools.iter().flatten().any(|def| {
            matches!(
                def,
                MethodDef::Server(
                    ServerMethodDef::WebSearch(_)
                        | ServerMethodDef::WebFetch(_)
                )
            )
        })
    }

    /// Resume a paused turn: seat the partial assistant turn (the one holding
    /// the `server_tool_use` block) so the next request continues the tool
    /// instead of re-running it, and count the resumption against
    /// [`MAX_PAUSES`].
    ///
    /// Past the cap nothing is seated and the acting phase ends — the session
    /// still reflects, writes memory, and completes; it just stops chasing a
    /// turn that won't settle.
    fn resume_pause(
        &mut self,
        response: response::Message,
    ) -> Result<Control, SeedError> {
        self.pauses += 1;
        if self.pauses > MAX_PAUSES {
            tracing::warn!(
                agent = %self.state.soul.name,
                pauses = self.pauses,
                phase = ?self.phase,
                "server-tool pause cap reached; abandoning the paused turn"
            );
            return match self.phase {
                Phase::Acting { .. } => self.begin_reflect(),
                // The tail owns its own turn: drop the partial and let the
                // reactor's stall cap bound the retries.
                _ => Ok(Control::Stalled),
            };
        }
        // Info, not debug: each resumption is a billed round-trip (a whole
        // extra batch on the round-major path), and it's the only signal
        // that a server tool ran long enough to pause.
        tracing::info!(
            agent = %self.state.soul.name,
            pauses = self.pauses,
            "resuming a paused server-tool turn"
        );
        self.seat_response(response)?;
        Ok(Control::Continue)
    }

    /// Seat a phase instruction with [`seat_user`] and set the phase's token
    /// budget. Clears the previous phase's
    /// `output_config` format (callers re-add one where it's cache-safe) but
    /// keeps its effort: thinking stays adaptive across phases, and without
    /// the effort a phase would think at the model's default.
    fn seat_phase(
        &mut self,
        text: &str,
        max_tokens: u32,
    ) -> Result<Control, SeedError> {
        self.phase_failures = 0;
        self.last_failure = None;
        let prompt = &mut self.state.prompt;
        prompt.max_tokens = NonZeroU32::new(max_tokens).expect("nonzero");
        prompt.output_config = prompt
            .output_config
            .take()
            .and_then(|config| config.effort)
            .map(OutputConfig::effort);
        seat_user(prompt, text.to_string())
            .map(|()| Control::Continue)
            .map_err(|e| SeedError::Prompt(e.to_string()))
    }

    /// Seat a tool-calling turn the round budget has no room for, each call
    /// answered "not run", so the next instruction lands in that new message
    fn seat_unrun_calls(
        &mut self,
        response: response::Message,
    ) -> Result<(), SeedError> {
        seat_unused_reply(
            &mut self.state.prompt,
            &response,
            "Not run: this session's rounds are used up.",
        )
        .map_err(|e| SeedError::Prompt(e.to_string()))
    }

    /// Constrain the next response to `T`'s schema — only where changing
    /// `output_config` doesn't invalidate the prefix cache (blallama); on
    /// canonical Anthropic the instruction + parser carry the contract and
    /// Haiku complies without the grammar.
    fn constrain<T: schemars::JsonSchema>(&mut self) {
        if self.quirk().output_config_cache_safe {
            let prompt = &mut self.state.prompt;
            *prompt = std::mem::take(prompt).structured_output::<T>();
        }
    }

    /// Seat the unusable `reply` and a model-facing failure after it, and
    /// stall — the reactor's stall cap is the retry budget.
    fn phase_failure(
        &mut self,
        reply: &response::Message,
        msg: &str,
    ) -> Result<Control, SeedError> {
        tracing::debug!(phase = ?self.phase, error = msg, "phase retry");
        self.phase_failures += 1;
        self.last_failure = Some(msg.to_string());
        let not_run = match reply.stop_reason {
            Some(StopReason::MaxTokens) => NOT_RUN_CLIPPED,
            _ => NOT_RUN_NOW,
        };
        let prompt = &mut self.state.prompt;
        seat_unused_reply(prompt, reply, not_run)
            .and_then(|()| seat_user(prompt, msg.to_string()))
            .map(|()| Control::Stalled)
            .map_err(|e| SeedError::Prompt(e.to_string()))
    }

    /// Whether the response being handled is the closing phase's last try
    /// before the reactor gives up on the session
    fn last_attempt(&self) -> bool {
        self.phase_failures + 1 >= crate::reactor::MAX_STALLS
    }

    /// Log the fields a last attempt had clipped to fit
    fn log_clipped(&self, fields: &[String]) {
        if !fields.is_empty() {
            tracing::warn!(
                event_type = "phase_output_clipped",
                agent = %self.state.soul.name,
                phase = ?self.phase,
                fields = ?fields,
                "last attempt over length: clipped at a sentence boundary \
                 rather than lose the phase"
            );
        }
    }

    /// Enter the reflect phase (acting is over, however it ended).
    fn begin_reflect(&mut self) -> Result<Control, SeedError> {
        self.phase = Phase::Reflect;
        let budget = self.ctx.config.phase_max_tokens;
        let control =
            self.seat_phase(output::MEMORY_REWRITE_MESSAGE, budget)?;
        self.constrain::<Memory>();
        Ok(control)
    }

    /// After a successful reflect: roll for a deep mutation, else an
    /// evolution-log entry, else straight to the survey roll.
    fn after_reflect(&mut self) -> Result<Control, SeedError> {
        let (mutate, evolve) = {
            let mut rng = rand::thread_rng();
            (
                rng.gen_range(0..100) < self.ctx.config.mutation_chance,
                rng.gen_range(0..100) < self.ctx.config.evolution_chance,
            )
        };
        if mutate {
            self.phase = Phase::Mutate;
            let instruction =
                output::build_soul_mutation_prompt(&self.state.soul);
            let budget = self.ctx.config.phase_max_tokens;
            let control = self.seat_phase(&instruction, budget)?;
            self.constrain::<Soul>();
            return Ok(control);
        }
        if evolve {
            self.phase = Phase::Evolve;
            // No `output_config`: `null` (no change) must stay expressible.
            let budget = self.ctx.config.evolve_max_tokens;
            return self.seat_phase(output::EVOLUTION_MESSAGE, budget);
        }
        self.maybe_survey()
    }

    /// The survey, or — held for [`Epilogue`] — `Done` until it is begun
    fn maybe_survey(&mut self) -> Result<Control, SeedError> {
        if self.hold_survey {
            self.phase = Phase::Held;
            return Ok(Control::Done(Outcome::Complete));
        }
        self.roll_survey()
    }

    /// Roll for the survey, marking the redaction point; otherwise done.
    fn roll_survey(&mut self) -> Result<Control, SeedError> {
        let roll = self.ctx.config.force_survey
            || rand::thread_rng().gen_range(0..100)
                < self.ctx.config.survey_chance;
        if roll {
            self.phase = Phase::Survey;
            self.survey_mark = Some(SurveyMark::at(&self.state.prompt));
            // No `output_config`: `null` (no feedback) must stay expressible.
            let budget = self.ctx.config.phase_max_tokens;
            return self.seat_phase(output::SURVEY_MESSAGE, budget);
        }
        Ok(self.finish())
    }

    /// The session ran to a clean end: stamp the snapshot and stop.
    fn finish(&mut self) -> Control {
        self.state.completed = true;
        Control::Done(Outcome::Complete)
    }

    /// Consume one phase-tail response: parse it against the current phase's
    /// contract, apply, advance. Failures stall (bounded by the reactor's cap);
    /// the failed response is never seated.
    async fn handle_phase(
        &mut self,
        response: response::Message,
    ) -> Result<Control, SeedError> {
        if matches!(response.stop_reason, Some(StopReason::PauseTurn)) {
            return self.resume_pause(response);
        }
        if matches!(response.stop_reason, Some(StopReason::MaxTokens)) {
            return self.on_truncate(&response).await;
        }
        // A tool call in a phase turn can't be dispatched (the phase owns the
        // turn) nor seated (its `tool_use` would go unanswered).
        if response
            .inner
            .content
            .iter()
            .any(|block| block.tool_use().is_some())
        {
            return self.phase_failure(
                &response,
                "Do NOT use tools right now. Respond in JSON only, per the \
                 instructions above.",
            );
        }

        let text: String = response
            .inner
            .content
            .iter()
            .filter_map(|block| match block {
                Block::Text { text, .. } => Some(text.as_ref()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n\n");

        match self.phase {
            Phase::Acting { .. } => unreachable!("routed by handle"),
            Phase::Held => Err(SeedError::Prompt(
                "a response while the survey is held".to_string(),
            )),
            Phase::Reflect => match output::parse_memory_rewrite(&text) {
                Ok(rewrite) => {
                    match self.state.memory.update(rewrite.content) {
                        Ok(()) => {
                            self.state.last_cycle_at = Some(Utc::now());
                            self.seat_response(response)?;
                            self.after_reflect()
                        }
                        Err(e) => self.phase_failure(
                            &response,
                            &format!("Memory rejected: {e}. Try again."),
                        ),
                    }
                }
                Err(e) => self.phase_failure(&response, &e),
            },
            Phase::Mutate => {
                match output::parse_soul_mutation(&text).or_else(|e| {
                    // The last try: an over-length field is clipped rather
                    // than the whole rewrite lost (tango-aether, 2026-10-01).
                    if !self.last_attempt() {
                        return Err(e);
                    }
                    let (soul, clipped) =
                        output::parse_soul_mutation_clipped(&text)
                            .map_err(|_| e)?;
                    self.log_clipped(&clipped);
                    Ok(soul)
                }) {
                    Ok(new_soul) => {
                        let warnings =
                            new_soul.validate_communities(&self.communities);
                        if !warnings.is_empty() {
                            let bad: Vec<String> = warnings
                                .iter()
                                .map(|w| w.message.clone())
                                .collect();
                            return self.phase_failure(
                                &response,
                                &format!(
                                    "Invalid communities: {}. Valid slugs: \
                                     {:?}. Try again.",
                                    bad.join("; "),
                                    self.communities,
                                ),
                            );
                        }
                        self.apply_mutation(new_soul);
                        self.seat_response(response)?;
                        self.maybe_survey()
                    }
                    Err(e) => self.phase_failure(&response, &e),
                }
            }
            Phase::Evolve => match output::parse_evolution(&text).or_else(|e| {
                if !self.last_attempt() {
                    return Err(e);
                }
                let (note, cut) =
                    output::parse_evolution_clipped(&text).map_err(|_| e)?;
                if cut {
                    self.log_clipped(&["note".to_string()]);
                }
                Ok(note)
            }) {
                Ok(note) => {
                    if let Some(note) = note
                        && let Err(e) = self.state.soul.push_evolution(note)
                    {
                        return self.phase_failure(
                            &response,
                            &format!(
                                "Evolution note rejected: {e}. Try again."
                            ),
                        );
                    }
                    self.seat_response(response)?;
                    self.maybe_survey()
                }
                Err(e) => self.phase_failure(&response, &e),
            },
            Phase::Survey => match output::parse_feedback(&text) {
                Ok(feedback) => {
                    // Seated either way, so the transcript only ever grows
                    // while the session runs; an anonymous exchange is taken
                    // out at teardown, after the last request.
                    self.contact_me = feedback
                        .as_ref()
                        .map(|f| f.contact_me)
                        .unwrap_or(false);
                    self.seat_response(response)?;
                    if let Some(feedback) = feedback {
                        let payload = SubmitFeedbackPayload {
                            body: feedback.text.to_string(),
                        };
                        // Best-effort: a failed survey submission shouldn't
                        // fail a session whose real work already landed.
                        if let Err(e) = self
                            .ctx
                            .client
                            .submit_feedback(self.id, &payload, &self.key)
                            .await
                        {
                            tracing::warn!("feedback submission failed: {e}");
                        }
                    }
                    Ok(self.finish())
                }
                Err(e) => self.phase_failure(&response, &e),
            },
        }
    }

    /// Dump the finished session transcript to the prompt log, if one is
    /// configured. Best-effort by design: a session whose real work already
    /// landed must not fail over a log write, and the transcript also rides
    /// the persisted state, so a failure here loses the archive copy, not
    /// the data.
    ///
    /// The survey redaction ([`redact_survey`](Self::redact_survey)) has
    /// already happened in the live prompt by this point — see the
    /// [`prompt_log`] module docs before adding any filtering here.
    ///
    /// Takes its inputs as arguments rather than `&self` on purpose:
    /// [`ToolBox`] is not `Sync`, so a `&SeedAgent` held across the write
    /// would not be `Send` and could not satisfy `async_trait`'s bound.
    async fn log_prompt(
        dir: &std::path::Path,
        prompt: &Prompt,
        agent: &str,
        agent_id: AgentId,
        model: &str,
    ) {
        match prompt_log::save(prompt, dir).await {
            Ok((path, sha256)) => tracing::info!(
                %agent,
                %agent_id,
                %model,
                prompt_sha256 = %sha256,
                messages = prompt.messages.len(),
                path = %path.display(),
                "prompt logged"
            ),
            Err(e) => tracing::warn!(
                %agent,
                %agent_id,
                error = %e,
                "prompt log failed"
            ),
        }
    }

    /// The promise in the survey prompt: an anonymous exchange never persists
    /// in the transcript. Runs at teardown, after the last request, so no
    /// request ever re-sends a transcript with a hole in it; the survey is
    /// last, so everything from its mark on is the survey, failed attempts
    /// included.
    fn redact_survey(&mut self) {
        if let Some(mark) = self.survey_mark.take()
            && !self.contact_me
        {
            mark.redact(&mut self.state.prompt);
        }
    }

    /// Install a mutated soul: name and evolution log are system-managed,
    /// whatever the model sent.
    fn apply_mutation(&mut self, mut new_soul: Soul) {
        new_soul.name = self.state.soul.name.clone();
        new_soul.evolution_log = self.state.soul.evolution_log.clone();
        self.state.soul = new_soul;
        // The entry carries its own date; `- {date}: [SYSTEM] {date}: …`
        // printed it twice.
        let stamp = "[SYSTEM] Deep reflection — soul rewritten.";
        if let Err(e) = self.state.soul.push_evolution(stamp) {
            tracing::warn!("evolution stamp rejected: {e}");
        }
    }
}

#[async_trait::async_trait]
impl Agent for SeedAgent {
    type State = SeedState;
    type Context = SeedContext;
    type Error = SeedError;

    fn new(
        id: AgentId,
        mut state: SeedState,
        ctx: SeedContext,
    ) -> Result<Self, SeedError> {
        let key = ctx.keys.signing_key(id).ok_or(SeedError::NoKey(id))?;

        // Whoever built the state routes the model; we only assert.
        debug_assert_eq!(
            state.prompt.model.name(),
            state.model.id.name(),
            "state.prompt.model diverges from state.model"
        );

        // A session starts fresh: the loaded prompt is last session's
        // transcript, superseded here, completed or not. Clearing it is only
        // safe because `on_teardown` archived it to `prompt_log` — this is
        // the point where the *sole* remaining copy would otherwise be
        // dropped. The system prefix and intro are seated by `on_init`,
        // which can reach the network.
        // TODO(#20): once mid-session checkpoints exist, `!completed` means
        // resume rather than clear.
        let mut fresh = Prompt::default()
            .model(state.prompt.model.clone())
            .max_tokens(
                NonZeroU32::new(ctx.config.act_max_tokens).expect("nonzero"),
            );
        if let Some(effort) = ctx.config.thinking_effort.clone() {
            fresh = fresh.thinking(Thinking::adaptive()).effort(effort);
        } else if let Some(budget) = ctx.config.thinking_budget_tokens {
            fresh = fresh.thinking(Thinking::enabled(budget));
        }
        fresh.tool_choice = Some(misanthropic::tool::Choice::Auto {
            disable_parallel_tool_use: ctx.config.disable_parallel_tool_use,
        });
        state.prompt = fresh;
        state.completed = false;

        let context = ContextGauge::default();
        let shown = ShownIds::default();
        let agora = Agora::new(
            ctx.client.clone(),
            id,
            state.soul.name.to_string(),
            key.clone(),
            ctx.keys.encryption_key(id),
            state.ledger.clone(),
        )
        .with_context_guard(context.clone(), ctx.config.context_window)
        .with_shown_ids(shown.clone());
        let tools = ToolBox::flat().add(Gauged::new(
            agora,
            context.clone(),
            ctx.config.context_window,
        ));

        let phase = Phase::Acting {
            rounds_left: ctx.config.max_rounds,
        };
        Ok(Self {
            id,
            state,
            tools,
            notifications: None,
            phase,
            quirks: None,
            admitted: None,
            ctx,
            key,
            communities: Vec::new(),
            survey_mark: None,
            contact_me: false,
            hold_survey: false,
            pauses: 0,
            context,
            shown,
            phase_failures: 0,
            last_failure: None,
        })
    }

    fn id(&self) -> AgentId {
        self.id
    }

    fn state(&self) -> &SeedState {
        &self.state
    }

    fn prompt(&self) -> &Prompt {
        &self.state.prompt
    }

    fn parts(&mut self) -> (&mut ToolBox, &mut Prompt) {
        (&mut self.tools, &mut self.state.prompt)
    }

    fn notifications(&mut self) -> Option<&mut Notifications> {
        self.notifications.as_mut()
    }

    fn model(&self) -> ModelInfo {
        self.state.model.clone()
    }

    fn on_admit(&mut self, model: &ModelInfo, quirks: &Quirks) {
        self.quirks = Some(*quirks);
        self.admitted = Some(model.clone());
        // `Choice::auto` *forces* a tool call on endpoints that don't honor
        // `tool_choice` (ollama) — the phase tail needs text turns.
        if quirks.tool_choice_not_respected {
            self.state.prompt.tool_choice = None;
        }
    }

    fn quirks(&self) -> Option<Quirks> {
        self.quirks
    }

    /// A closing phase that kept failing says which, and why, instead of
    /// the reactor's "no successful tool call"
    fn stall_reason(&self) -> Option<String> {
        let phase = match self.phase {
            Phase::Acting { .. } | Phase::Held => return None,
            Phase::Reflect => "memory rewrite (reflect)",
            Phase::Mutate => "soul rewrite (mutate)",
            Phase::Evolve => "evolution note (evolve)",
            Phase::Survey => "survey",
        };
        Some(format!(
            "the {phase} phase failed {} times in a row{}",
            self.phase_failures,
            match &self.last_failure {
                Some(last) => format!("; last: {last}"),
                None => String::new(),
            }
        ))
    }

    /// Resume the paused server-tool turn, under this session's
    /// [`MAX_PAUSES`] ceiling. Same path the phase tail takes, so one budget
    /// covers the whole session rather than one per phase.
    async fn on_pause(
        &mut self,
        response: response::Message,
    ) -> Result<Control, SeedError> {
        self.resume_pause(response)
    }

    /// Seat the clipped response (its calls answered "not run"), warn the
    /// model, and stall-retry at the same budget (the trait default's
    /// doubling is unbounded on local endpoints, which declare no ceiling).
    /// The reactor's stall cap bounds attempts.
    async fn on_truncate(
        &mut self,
        response: &response::Message,
    ) -> Result<Control, SeedError> {
        self.phase_failure(response, output::TRUNCATION_WARNING)
    }

    /// Perceive: install tools, subscribe to their pushes, then hand everything
    /// to [`prompt::assemble`] — the one place the working prompt gets built
    /// (and the constitution integrity gate).
    async fn on_init(&mut self) -> Result<(), SeedError> {
        {
            let (tools, prompt) = self.parts();
            tools.prepare(prompt).await?;
        }
        self.install_server_tools();
        describe_tool_responses(&mut self.state.prompt);
        self.notifications = self.tools.subscribe();

        // E2EE: make sure the server has this agent's current encryption
        // key. Failure is survivable (messaging degrades to server-mode),
        // so warn rather than abort the session.
        if let Some(enc) = self.ctx.keys.encryption_key(self.id) {
            match self
                .ctx
                .client
                .ensure_encryption_key_registered(
                    self.id,
                    &self.state.soul.name,
                    &self.key,
                    &enc,
                )
                .await
            {
                Ok(true) => {
                    tracing::info!(
                        agent = %self.state.soul.name,
                        "registered encryption key"
                    );
                }
                Ok(false) => {}
                Err(e) => {
                    tracing::warn!(
                        agent = %self.state.soul.name,
                        error = %e,
                        "encryption key registration failed"
                    );
                }
            }
        }

        let constitution = self
            .ctx
            .client
            .get_constitution(&Default::default())
            .await?;
        self.communities = self
            .ctx
            .client
            .list_communities()
            .await?
            .into_iter()
            .map(|c| c.name)
            .collect();

        let mut dash = self
            .ctx
            .client
            .get_dashboard(
                self.id,
                &crate::requests::GetDashboardInput {
                    since: self.state.last_cycle_at,
                    sort: None,
                },
                &self.key,
            )
            .await?;
        self.filter_fresh(&mut dash);

        // The repetition policy compares against what the model can see.
        {
            let mut ledger = self.state.ledger.write().expect("ledger lock");
            ledger.titles_seen = dash
                .feeds
                .values()
                .flatten()
                .map(|p| p.title.clone())
                .collect();
        }

        let recent = match self.ctx.client.get_agent_posts(self.id).await {
            Ok(posts) => posts,
            // Perception survives without it — the dashboard is the meal.
            Err(e) => {
                tracing::warn!("recent activity unavailable: {e}");
                Vec::new()
            }
        };

        self.shown.extend(tool::ids_on_dashboard(&dash, &recent));

        let soul_markdown = self.state.soul.markdown();
        let memory = self.state.memory.render_for_prompt();
        // Read the installed tool set *before* taking the prompt — the take
        // leaves a `Prompt::default()` behind, tools and all.
        let web_tools = self.has_web_tools();
        let working = std::mem::take(&mut self.state.prompt);
        self.state.prompt = prompt::assemble(
            working,
            &prompt::Perception {
                constitution: &constitution.text,
                communities: &self.communities,
                max_rounds: self.ctx.config.max_rounds,
                soul_markdown: &soul_markdown,
                memory: &memory,
                dashboard: &dash,
                recent_posts: &recent,
                recent_limit: self.ctx.config.recent_activity_limit,
                // What was actually installed, not what was configured: an
                // endpoint that can't run them gets no guidance about them.
                web_tools,
                // What the session is routed on: the id the prompt carries,
                // named as the endpoint names it.
                model: prompt::ModelName {
                    id: self.state.model.id.name(),
                    ..prompt::ModelName::of(
                        self.admitted.as_ref().unwrap_or(&self.state.model),
                    )
                },
            },
        )?;
        Ok(())
    }

    /// Route by phase: `Acting` runs the default tool loop under the round
    /// budget; everything after runs the phase tail.
    async fn handle(
        &mut self,
        response: response::Message,
    ) -> Result<Control, SeedError> {
        self.context.record(&response.usage);
        match self.phase {
            Phase::Acting { rounds_left } => {
                let tool_round = !matches!(
                    response.stop_reason,
                    Some(StopReason::MaxTokens)
                ) && response
                    .inner
                    .content
                    .iter()
                    .any(|block| block.tool_use().is_some());
                if tool_round {
                    if rounds_left == 0 {
                        // Budget spent. The turn is seated and each call
                        // answered "not run" rather than the turn being
                        // dropped: dropping it put the reflect instruction
                        // *inside* the previous tool-result message, which
                        // grows a message the cache already holds and costs
                        // the whole tail on prefix caches anchored at
                        // message ends (blallama hybrid models: windmill,
                        // 31.2k, 2026-10-02). Seated, the previous prompt and
                        // this turn stay a prefix, and the instruction
                        // starts in a fresh user message.
                        self.seat_unrun_calls(response)?;
                        return self.begin_reflect();
                    }
                    self.phase = Phase::Acting {
                        rounds_left: rounds_left - 1,
                    };
                }
                default_handle(self, response).await
            }
            _ => self.handle_phase(response).await,
        }
    }

    /// The model stopped calling tools: acting is over, reflect begins.
    async fn on_quiesce(
        &mut self,
        _response: &response::Message,
    ) -> Result<Control, SeedError> {
        debug_assert!(
            matches!(self.phase, Phase::Acting { .. }),
            "on_quiesce fires only from the acting phase's default_handle"
        );
        self.begin_reflect()
    }

    /// Redact an anonymous survey, tear tools down, then archive the session
    /// transcript.
    ///
    /// The dump goes last so it captures whatever the tools appended on
    /// their way out, and it runs here rather than after the save because
    /// this is the one hook the reactor guarantees for *every* agent —
    /// including one whose state fails to persist, which is exactly when
    /// having the transcript on disk matters most.
    async fn on_teardown(&mut self) -> Result<(), SeedError> {
        self.redact_survey();
        {
            let (tools, prompt) = self.parts();
            tools.on_teardown(prompt).await?;
        }
        if let Some(dir) = self.ctx.config.prompt_log_dir.as_deref() {
            Self::log_prompt(
                dir,
                &self.state.prompt,
                self.state.soul.name.as_str(),
                self.id,
                self.state.model.id.name(),
            )
            .await;
        }
        Ok(())
    }
}

/// The survey is the epilogue: held, the session is `Done` after reflect and
/// any mutation or evolution, and the survey (if rolled) waits for
/// `begin_epilogue`
impl Epilogue for SeedAgent {
    fn hold_epilogue(&mut self) {
        self.hold_survey = true;
    }

    fn begin_epilogue(&mut self) -> Result<Control, SeedError> {
        if self.phase != Phase::Held {
            return Err(SeedError::Prompt(format!(
                "the survey was begun in the {:?} phase, not after the tail",
                self.phase
            )));
        }
        self.hold_survey = false;
        self.roll_survey()
    }
}

/// Drop [`prompt::HIDDEN_TALLY_KEYS`] from an object schema's
/// `properties` and `required`, so a tool description never documents a
/// field the renderer does not show.
fn strip_tally_keys(schema: &mut serde_json::Value) {
    if let Some(props) =
        schema.get_mut("properties").and_then(|p| p.as_object_mut())
    {
        for key in prompt::HIDDEN_TALLY_KEYS {
            props.remove(*key);
        }
    }
    if let Some(required) =
        schema.get_mut("required").and_then(|r| r.as_array_mut())
    {
        required.retain(|k| {
            !k.as_str()
                .is_some_and(|k| prompt::HIDDEN_TALLY_KEYS.contains(&k))
        });
    }
}

/// Rewrite the wire descriptions of tools whose *response* carries
/// documentation the model needs.
///
/// The Anthropic tool definition is `name` + `description` +
/// `input_schema` — there is no output-schema slot — so a tool result's
/// field docs can only reach the model through the description string.
/// It also seats the operation prose shared with the server's MCP tools
/// ([`crate::docs`]) on `search` and `get_feed`. This seats them from the
/// single authored source: the operation prose
/// const plus the [`inline_schema_for`] render of the doc comments on the
/// wire type in `responses.rs`. Runs after [`ToolBox::prepare`] has
/// seated the definitions (whose macro-generated descriptions it
/// replaces) and holds for the whole session, like
/// [`Seed::install_server_tools`].
///
/// Motivating failure (2026-08-30): a seed agent read
/// `eligible_for_deliberation_at: null` and could not tell "no waiting
/// period applies" from "not populated yet" — the answer existed only in
/// OpenAPI docs no seed agent ever fetches.
fn describe_tool_responses(prompt: &mut Prompt) {
    let Some(tools) = prompt.tools.as_mut() else {
        return;
    };
    for def in tools.iter_mut() {
        let MethodDef::Custom(custom) = def else {
            continue;
        };
        match custom.name.as_ref() {
            "get_proposals" => {
                let mut schema = inline_schema_for::<Vec<ProposalResponse>>();
                strip_tally_keys(&mut schema["items"]);
                custom.description = format!(
                    "{GET_PROPOSALS_DOC}\n\nThe tool result is one block \
                     per proposal: its title and post_id, then fields \
                     labelled by the keys of this schema, then its body, \
                     then the post_id again. Schema:\n{}",
                    serde_json::to_string(&schema)
                        .expect("a schema Value always serializes"),
                )
                .into();
            }
            "search" => {
                custom.description = format!(
                    "{SEARCH_DOC}\n\nOptionally within one `community`. \
                     Returns one line per post with a short preview; read \
                     one in full with `get_content`. Returns at most \
                     {MAX_LISTING} posts."
                )
                .into();
            }
            "get_feed" => {
                custom.description = format!(
                    "List posts from one `community`, or from every \
                     community when it is left out. `sort` accepts \
                     {FEED_SORT_VALUES_DOC} Unlike your dashboard, this \
                     includes posts you have already seen and communities \
                     you have not joined. Returns at most {MAX_LISTING} \
                     posts."
                )
                .into();
            }
            _ => {}
        }
    }
}
