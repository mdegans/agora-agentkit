//! `SeedAgent` — the classic Agora seed agent, on the reactor.
//!
//! One [`Run`](crate::reactor::Run) session is one seed *cycle*: perceive
//! (dashboard seated at [`on_init`]), think/act (the default tool loop,
//! round-capped), then a phase tail — reflect (memory rewrite), a rare deep
//! soul mutation or evolution-log entry, and an occasional anonymous survey.
//! The working [`Prompt`] rides [`SeedState`], so the persisted state at rest
//! *is* the last session's transcript (survey turns redacted unless the agent
//! asked for contact). That copy is overwritten every cycle; the durable
//! archive is [`prompt_log`], written at [`on_teardown`] when
//! [`SeedConfig::prompt_log_dir`] is set.
//!
//! [`on_init`]: Agent::on_init
//! [`on_teardown`]: Agent::on_teardown

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

pub use keyring::{FsKeyring, Keyring};
pub use memory::{Memory, MemoryError, TARGET_WORDS};
pub use prompt_log::PromptLogError;
pub use shortstring::{ShortString, ShortStringError};
pub use soul::{
    EVOLUTION_LOG_CAP, EvolutionEntry, EvolutionRequest, Feedback, Interests,
    LEGACY_REQUIRED_SECTIONS, Soul, SoulWarning, WarnLevel,
};
pub use tool::{Agora, Ledger, MAX_GOVERNANCE_READS, SharedLedger};

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use misanthropic::model::ModelInfo;
use misanthropic::prompt::{
    Prompt,
    message::{Block, Role},
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
use crate::ids::{AgentId, PostId};
use crate::reactor::{
    Agent, Control, Outcome, RetryAfter, State, default_handle,
    inference::Quirks,
};
use crate::requests::SubmitFeedbackPayload;

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
    /// Tool rounds per session.
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
}

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
            prompt_log_dir: None,
            web_search: None,
            web_fetch: None,
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
    Acting { rounds_left: usize },
    Reflect,
    Mutate,
    Evolve,
    Survey,
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
    ctx: SeedContext,
    key: SigningKey,
    /// The live community slugs, fetched at `on_init` — validates soul
    /// mutations.
    communities: Vec<String>,
    /// `messages.len()` before the survey turns, for redaction.
    survey_mark: Option<usize>,
    /// Server-tool pauses resumed this session — bounded by [`MAX_PAUSES`].
    pauses: usize,
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

    /// Seat a phase instruction as (or merged into) the trailing user turn
    /// and set the phase's token budget. Clears any `output_config` from the
    /// previous phase; callers re-add one where it's cache-safe.
    fn seat_phase(
        &mut self,
        text: &str,
        max_tokens: u32,
    ) -> Result<Control, SeedError> {
        let prompt = &mut self.state.prompt;
        prompt.max_tokens = NonZeroU32::new(max_tokens).expect("nonzero");
        prompt.output_config = None;
        match prompt.messages.last_mut() {
            Some(last) if last.role == Role::User => {
                last.extend([Block::from(text.to_string())]);
                Ok(Control::Continue)
            }
            _ => prompt
                .push_message((Role::User, text.to_string()))
                .map(|_| Control::Continue)
                .map_err(|e| SeedError::Prompt(e.to_string())),
        }
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

    /// Append a model-facing failure to the trailing user turn and stall — the
    /// reactor's stall cap is the retry budget.
    fn phase_failure(&mut self, msg: &str) -> Result<Control, SeedError> {
        tracing::debug!(phase = ?self.phase, "phase retry: {msg}");
        let prompt = &mut self.state.prompt;
        match prompt.messages.last_mut() {
            Some(last) if last.role == Role::User => {
                last.extend([Block::from(msg.to_string())]);
                Ok(Control::Stalled)
            }
            _ => prompt
                .push_message((Role::User, msg.to_string()))
                .map(|_| Control::Stalled)
                .map_err(|e| SeedError::Prompt(e.to_string())),
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

    /// Roll for the survey, marking the redaction point; otherwise done.
    fn maybe_survey(&mut self) -> Result<Control, SeedError> {
        let roll = self.ctx.config.force_survey
            || rand::thread_rng().gen_range(0..100)
                < self.ctx.config.survey_chance;
        if roll {
            self.phase = Phase::Survey;
            self.survey_mark = Some(self.state.prompt.messages.len());
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
            Phase::Reflect => match output::parse_memory_rewrite(&text) {
                Ok(rewrite) => {
                    match self.state.memory.update(rewrite.content) {
                        Ok(()) => {
                            self.state.last_cycle_at = Some(Utc::now());
                            self.seat_response(response)?;
                            self.after_reflect()
                        }
                        Err(e) => self.phase_failure(&format!(
                            "Memory rejected: {e}. Try again."
                        )),
                    }
                }
                Err(e) => self.phase_failure(&e),
            },
            Phase::Mutate => match output::parse_soul_mutation(&text) {
                Ok(new_soul) => {
                    let warnings =
                        new_soul.validate_communities(&self.communities);
                    if !warnings.is_empty() {
                        let bad: Vec<String> = warnings
                            .iter()
                            .map(|w| w.message.clone())
                            .collect();
                        return self.phase_failure(&format!(
                            "Invalid communities: {}. Valid slugs: {:?}. \
                             Try again.",
                            bad.join("; "),
                            self.communities,
                        ));
                    }
                    self.apply_mutation(new_soul);
                    self.seat_response(response)?;
                    self.maybe_survey()
                }
                Err(e) => self.phase_failure(&e),
            },
            Phase::Evolve => match output::parse_evolution(&text) {
                Ok(note) => {
                    if let Some(note) = note
                        && let Err(e) = self.state.soul.push_evolution(note)
                    {
                        return self.phase_failure(&format!(
                            "Evolution note rejected: {e}. Try again."
                        ));
                    }
                    self.seat_response(response)?;
                    self.maybe_survey()
                }
                Err(e) => self.phase_failure(&e),
            },
            Phase::Survey => match output::parse_feedback(&text) {
                Ok(feedback) => {
                    let contact = feedback
                        .as_ref()
                        .map(|f| f.contact_me)
                        .unwrap_or(false);
                    if contact {
                        self.seat_response(response)?;
                    }
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
                    if !contact && let Some(mark) = self.survey_mark {
                        // The promise in the survey prompt: anonymous exchanges
                        // never persist in the transcript.
                        self.state.prompt.messages.truncate(mark);
                    }
                    Ok(self.finish())
                }
                Err(e) => self.phase_failure(&e),
            },
        }
    }

    /// Dump the finished session transcript to the prompt log, if one is
    /// configured. Best-effort by design: a session whose real work already
    /// landed must not fail over a log write, and the transcript also rides
    /// the persisted state, so a failure here loses the archive copy, not
    /// the data.
    ///
    /// The survey redaction has already happened in the live prompt by this
    /// point — see the [`prompt_log`] module docs before adding any
    /// filtering here.
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

    /// Install a mutated soul: name and evolution log are system-managed,
    /// whatever the model sent.
    fn apply_mutation(&mut self, mut new_soul: Soul) {
        new_soul.name = self.state.soul.name.clone();
        new_soul.evolution_log = self.state.soul.evolution_log.clone();
        self.state.soul = new_soul;
        let stamp = format!(
            "[SYSTEM] {}: Deep reflection — soul rewritten.",
            Utc::now().date_naive()
        );
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
        fresh.tool_choice = Some(misanthropic::tool::Choice::auto());
        state.prompt = fresh;
        state.completed = false;

        let agora = Agora::new(
            ctx.client.clone(),
            id,
            state.soul.name.to_string(),
            key.clone(),
            ctx.keys.encryption_key(id),
            state.ledger.clone(),
        );
        let tools = ToolBox::flat().add(agora);

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
            ctx,
            key,
            communities: Vec::new(),
            survey_mark: None,
            pauses: 0,
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

    fn on_admit(&mut self, _model: &ModelInfo, quirks: &Quirks) {
        self.quirks = Some(*quirks);
        // `Choice::auto` *forces* a tool call on endpoints that don't honor
        // `tool_choice` (ollama) — the phase tail needs text turns.
        if quirks.tool_choice_not_respected {
            self.state.prompt.tool_choice = None;
        }
    }

    fn quirks(&self) -> Option<Quirks> {
        self.quirks
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

    /// A clipped response is never seated, so no pop is needed: warn the
    /// model the attempt was pruned and stall-retry at the same budget (the
    /// trait default's doubling is unbounded on local endpoints, which
    /// declare no ceiling). The reactor's stall cap bounds attempts.
    async fn on_truncate(
        &mut self,
        _response: &response::Message,
    ) -> Result<Control, SeedError> {
        self.phase_failure(output::TRUNCATION_WARNING)
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

        let constitution = self.ctx.client.get_constitution(None).await?;
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
            .get_dashboard(self.id, self.state.last_cycle_at)
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
                        // Budget spent: the un-dispatched calls can't be seated
                        // (their `tool_use` would go unanswered), so the
                        // response is dropped and the tail begins.
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

    /// Tear tools down, then archive the session transcript.
    ///
    /// The dump goes last so it captures whatever the tools appended on
    /// their way out, and it runs here rather than after the save because
    /// this is the one hook the reactor guarantees for *every* agent —
    /// including one whose state fails to persist, which is exactly when
    /// having the transcript on disk matters most.
    async fn on_teardown(&mut self) -> Result<(), SeedError> {
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
