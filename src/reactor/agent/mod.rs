mod state;
pub use state::State;

use misanthropic::{prompt::Prompt, response, tool::ToolBox};

use crate::ids::AgentId;

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

    /// A persistable snapshot of this agent's state (including its round/retry
    /// budget, so progress survives a save/restore).
    fn snapshot(&self) -> Self::State;

    /// `None` while the agent is still running; `Some(_)` once the session is
    /// over. The reactor routes the agent to its done/failed bucket by this.
    fn outcome(&self) -> Option<Outcome>;

    /// The request to send next. **Invariant: the returned prompt always ends
    /// in a [`Role::User`](misanthropic::prompt::message::Role) message** —
    /// `handle` re-establishes this after every response.
    fn prompt(&self) -> &Prompt;

    /// The toolbox and working prompt, borrowed together so the provided
    /// lifecycle hooks can wire them without a double `&mut self`.
    fn parts(&mut self) -> (&mut ToolBox, &mut Prompt);

    /// Consume one assistant response — the sole async seam in the step.
    /// Dispatches tool calls via the [`ToolBox`], advances the turn or runs the
    /// end-of-session interview, re-seats the prompt to end in a user turn, and
    /// charges the per-session round/retry budget (flipping
    /// [`outcome`](Agent::outcome) to `Some(Failed)` when exhausted) — or marks
    /// the session complete.
    async fn handle(&mut self, response: response::Message) -> Result<(), Self::Error>;

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

/// How a finished agent's session resolved. `outcome()` returns `None` while
/// the agent is still running, `Some(_)` once it is done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The session ran to a clean end (quiescence + interview).
    Complete,
    /// The session gave up — round/retry budget exhausted or an unrecoverable
    /// error. The agent is still persisted, but flagged as failed.
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
