//! [`Agent`] and related traits like [`State`]

mod state;
pub use state::State;

use std::num::NonZeroU32;

use misanthropic::{
    model::ModelInfo,
    prompt::{
        Prompt,
        message::{Block, Role},
    },
    response::{self, StopReason},
    tool::{Tool, ToolBox, Use},
};

use crate::ids::AgentId;

/// Box a concrete error into the boxed trait object the lifecycle hooks and
/// tool dispatch funnel through `Agent::Error: From<Box<dyn Error …>>`.
fn boxed<E: std::error::Error + Send + Sync + 'static>(
    e: E,
) -> Box<dyn std::error::Error + Send + Sync> {
    Box::new(e)
}

/// An `Agent` abstraction.
#[async_trait::async_trait]
pub trait Agent: Sized + Send {
    /// Serializable [`Agent`] `State`. Should include everything necessary to
    /// save/load the agent in the same functional state.
    type State: State;
    type Error: super::Error + From<Box<dyn std::error::Error + Send + Sync>>;

    /// Reconstruct from persisted state. Sync and fallible: build the base
    /// prompt and the [`ToolBox`]; defer *all* async setup to
    /// [`on_init`](Agent::on_init).
    // We might need a `Context` associated type to provide ephemeral things
    // like a clone of a database client to an agent. Otherwise we need another
    // impl constructor which the reactor can't know to call. So the `Context`
    // would be Clone and the reactor would hold it and when it creates the
    // agent, pass a clone to it. Perhaps also Default.
    fn new(id: AgentId, state: Self::State) -> Result<Self, Self::Error>;

    /// Unique UUID for this [`Agent`]
    fn id(&self) -> AgentId;

    /// [`Agent::State`] accessor for snapshotting.
    // Sync by design: content pushed from subscribed tools drains at the turn
    // boundary (`on_turn`), never mid-accessor.
    fn state(&self) -> &Self::State;

    /// The request to send next. **Invariant: the returned prompt always ends
    /// in a [`Role::User`](misanthropic::prompt::message::Role) message** — the
    /// default [`handle`](Agent::handle)'s tool path re-establishes it; an
    /// [`on_quiesce`](Agent::on_quiesce) override returning
    /// [`Control::Continue`] must do the same.
    fn prompt(&self) -> &Prompt;

    /// The toolbox and working prompt, borrowed together so the default
    /// `handle` and the lifecycle hooks can wire them without a double `&mut
    /// self`.
    fn parts(&mut self) -> (&mut ToolBox, &mut Prompt);

    /// Consume one assistant response and tell the reactor what to do next.
    ///
    /// The **default** is the whole agentic mechanic and rarely needs
    /// overriding: pass through a `PauseTurn`; route a `MaxTokens` clip to
    /// [`on_truncate`](Agent::on_truncate); otherwise extract `tool_use`
    /// blocks, and either dispatch them through the [`ToolBox`] (seating the
    /// `tool_result`-led user turn) or — if the model called no tools — hand
    /// the quiescent response to [`on_quiesce`](Agent::on_quiesce). A round
    /// that lands no successful tool call returns [`Control::Stalled`], which
    /// the reactor counts toward a give-up cap.
    ///
    /// Override only for agents whose step isn't "use tools until quiescent".
    async fn handle(
        &mut self,
        response: response::Message,
    ) -> Result<Control, Self::Error> {
        // A paused turn is waiting on an in-flight server tool. v1 registers no
        // server tools, so this is a safety net: continue and let the next
        // infer retry (we don't seat the partial turn).
        if matches!(response.stop_reason, Some(StopReason::PauseTurn)) {
            return Ok(Control::Continue);
        }

        // Nothing from a clipped turn is seated or dispatched — a truncated
        // `tool_use` can arrive as valid JSON yet be missing arguments the
        // model never got to emit.
        if matches!(response.stop_reason, Some(StopReason::MaxTokens)) {
            return self.on_truncate(&response).await;
        }

        let calls: Vec<Use> = response
            .inner
            .content
            .iter()
            .filter_map(|block| block.tool_use().cloned())
            .collect();

        if calls.is_empty() {
            // Quiescent: seat the assistant turn, then advance the session.
            {
                let (_, prompt) = self.parts();
                prompt
                    .push_message(response.inner.clone())
                    .map_err(|e| Self::Error::from(boxed(e)))?;
            }
            return self.on_quiesce(&response).await;
        }

        // Dispatch the tool calls; seat all results as one user turn.
        let (tools, prompt) = self.parts();
        prompt
            .push_message(response.inner)
            .map_err(|e| Self::Error::from(boxed(e)))?;
        let mut results = Vec::with_capacity(calls.len());
        let mut progressed = false;
        for call in calls {
            let result = tools.call(call).await;
            progressed |= !result.is_error;
            results.push(Block::from(result));
        }
        prompt
            .push_message((Role::User, results))
            .map_err(|e| Self::Error::from(boxed(e)))?;

        Ok(if progressed {
            Control::Continue
        } else {
            Control::Stalled
        })
    }

    /// Decide what happens when the model stops calling tools.
    // We might not need this. It's a common theme downstream to have an
    // interview after the session (update memory, survey, potential chat),
    // however that can be implemented in a `handle` override.
    async fn on_quiesce(
        &mut self,
        response: &response::Message,
    ) -> Result<Control, Self::Error> {
        let _ = response;
        Ok(Control::Done(Outcome::Complete))
    }

    /// Decide what happens when the response was clipped by
    /// [`max_tokens`](Prompt::max_tokens). The default makes the retry
    /// meaningful rather than blind: double the budget — clamped to the
    /// [`model`](Agent::model) ceiling, when the agent declares one — and
    /// return [`Control::Stalled`] so the stall cap still bounds attempts.
    /// Override to e.g. seat a nudge instead.
    async fn on_truncate(
        &mut self,
        response: &response::Message,
    ) -> Result<Control, Self::Error> {
        let _ = response;
        // 0 = no declared ceiling (`ModelInfo::max_tokens` is
        // serde-defaulted): double unclamped; the stall cap bounds the growth.
        let ceiling = self.model().max_tokens;
        let (_, prompt) = self.parts();
        let current = prompt.max_tokens.get();
        let mut raised = current.saturating_mul(2);
        if ceiling != 0 {
            raised = raised.min(ceiling);
        }
        // At (or past) the ceiling nothing is left to raise and this
        // degenerates to a plain stall.
        if let Some(raised) =
            NonZeroU32::new(raised).filter(|r| r.get() > current)
        {
            prompt.max_tokens = raised;
        }
        Ok(Control::Stalled)
    }

    /// Install tool definitions and run each tool's `on_init`. Called once by
    /// the reactor right after construction.
    async fn on_init(&mut self) -> Result<(), Self::Error> {
        let (tools, prompt) = self.parts();
        tools.prepare(prompt).await?;
        Ok(())
    }

    /// Refresh per-turn tool context. Called before each `infer`.
    async fn on_turn(&mut self) -> Result<(), Self::Error> {
        let (tools, prompt) = self.parts();
        tools.on_turn(prompt).await?;
        Ok(())
    }

    /// Tear down tools. Called once before the final save.
    async fn on_teardown(&mut self) -> Result<(), Self::Error> {
        let (tools, prompt) = self.parts();
        tools.on_teardown(prompt).await?;
        Ok(())
    }

    /// The model and [`Capabilities`] this agent requires. The reactor negotiates
    /// it against what the endpoint offers ([`Inference::models`]) to route the
    /// agent to the batch or sequential path, or reject it.
    ///
    /// [`Capabilities`]: misanthropic::model::Capabilities
    /// [`Inference::models`]: super::Inference::models
    fn model(&self) -> ModelInfo;
}

/// What the reactor should do after [`Agent::handle`] / [`Agent::on_quiesce`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    /// Progress was made (tool results seated, or a new turn began). Keep going.
    Continue,
    /// A round happened but landed nothing: no successful tool call, couldn't
    /// be parsed, or the response was clipped. The reactor counts consecutive
    /// stalls and gives up past a cap.
    Stalled,
    /// The session is over.
    Done(Outcome),
}

/// How a finished agent's session resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The session ran to a clean end (quiescence + interview).
    Complete,
    /// The session gave up — stall cap hit or an unrecoverable error. The agent
    /// is still persisted, but flagged as failed.
    Failed,
}
