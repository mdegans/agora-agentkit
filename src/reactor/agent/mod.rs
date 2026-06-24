mod state;
pub use state::State;

use misanthropic::{
    prompt::{
        Prompt,
        message::{Block, Content, Role},
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

/// A "sans-IO" agent: it never holds the inference client. The reactor reads
/// the agent's next [`prompt`](Agent::prompt), runs it through an
/// [`Inference`](super::Inference), and hands the response back via
/// [`handle`](Agent::handle). The agent owns its prompt, tools, and history —
/// everything that varies — while the reactor owns the loop and the transport.
///
/// The only async seam in the per-turn step is [`handle`](Agent::handle); the
/// lifecycle hooks are provided and normally not overridden.
#[async_trait::async_trait]
pub trait Agent: Sized + Send {
    type State: State;
    /// `From<Box<dyn Error + Send + Sync>>` lets the provided lifecycle hooks
    /// and `handle`'s tool dispatch `?` the `ToolBox`/`Tool` boxed errors.
    type Error: super::Error + From<Box<dyn std::error::Error + Send + Sync>>;

    /// Reconstruct from persisted state. Sync and fallible: build the base
    /// prompt and the [`ToolBox`]; defer *all* async setup to
    /// [`on_init`](Agent::on_init).
    fn new(id: AgentId, state: Self::State) -> Result<Self, Self::Error>;

    fn id(&self) -> AgentId;

    /// A persistable snapshot of this agent's state (including any turn/phase
    /// counters, so progress survives a save/restore).
    fn snapshot(&self) -> Self::State;

    /// The request to send next. **Invariant: the returned prompt always ends
    /// in a [`Role::User`](misanthropic::prompt::message::Role) message** — the
    /// default [`handle`](Agent::handle) re-establishes this after every
    /// response.
    fn prompt(&self) -> &Prompt;

    /// The toolbox and working prompt, borrowed together so the default
    /// `handle` and the lifecycle hooks can wire them without a double
    /// `&mut self`.
    fn parts(&mut self) -> (&mut ToolBox, &mut Prompt);

    /// Consume one assistant response and tell the reactor what to do next.
    ///
    /// The **default** is the whole agentic mechanic and rarely needs
    /// overriding: pass through a `PauseTurn`; otherwise extract `tool_use`
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
            .push_message((Role::User, Content(results)))
            .map_err(|e| Self::Error::from(boxed(e)))?;

        Ok(if progressed {
            Control::Continue
        } else {
            Control::Stalled
        })
    }

    /// Decide what happens when the model stops calling tools — the session
    /// state machine. Seat the next turn (perceive, the multi-turn reflect
    /// interview, an evolve/survey prompt) and return [`Control::Continue`];
    /// return [`Control::Stalled`] if the response couldn't be parsed (retry,
    /// capped by the reactor); or [`Control::Done`] to end the session. The
    /// quiescent `response` is provided so the agent can parse it (a reflection,
    /// a memory rewrite) before deciding.
    ///
    /// The default is a one-shot agent: quiescence means complete.
    async fn on_quiesce(
        &mut self,
        response: &response::Message,
    ) -> Result<Control, Self::Error> {
        let _ = response;
        Ok(Control::Done(Outcome::Complete))
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
        tools.update_turn_context(prompt).await?;
        Ok(())
    }

    /// Tear down tools. Called once before the final save.
    async fn on_teardown(&mut self) -> Result<(), Self::Error> {
        let (tools, prompt) = self.parts();
        tools.teardown_tools(prompt).await?;
        Ok(())
    }

    /// Which transport this agent prefers. The orchestrator routes by this.
    fn affinity(&self) -> Affinity {
        Affinity::Messages
    }
}

/// What the reactor should do after [`Agent::handle`] / [`Agent::on_quiesce`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    /// Progress was made (tool results seated, or a new turn began). Keep going.
    Continue,
    /// A round happened but landed no successful tool call / couldn't be parsed.
    /// The reactor counts consecutive stalls and gives up past a cap.
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

/// Which transport an agent wants. Routing is the orchestrator's decision; this
/// is the agent's declared preference. Some agents *need* batch to be
/// affordable. (Streaming may join later.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Affinity {
    Messages,
    Batch,
}
