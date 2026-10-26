//! `AgoraSession`: a seed agent's tools for one session, wired once for the
//! runner and anyone else who wants to see what an agent sees

use misanthropic::tool::{self, Tool, ToolBox, Use};

use super::{Agora, ContextGauge, Gauged, ShownIds, ShownOnce};
use crate::tools::ToolCall;

/// An [`Agora`] in a [`Gauged`] in a flat [`ToolBox`], as the runner seats
/// it for an agent's session (0.122.0)
pub struct AgoraSession {
    pub tools: ToolBox,
    pub context: ContextGauge,
    pub shown: ShownIds,
    pub shown_once: ShownOnce,
}

impl AgoraSession {
    /// `agora`'s tools for a session held to `context`
    pub fn new(agora: Agora, context: ContextGauge) -> Self {
        let shown = ShownIds::default();
        // Per session, in memory: never persisted (0.108)
        let shown_once = ShownOnce::default();
        let id = agora.agent_id();
        let client = agora.client().clone();
        let agora = agora
            .with_context_guard(context.clone())
            .with_shown_ids(shown.clone())
            .with_shown_once(shown_once.clone());
        let tools = ToolBox::flat().add(
            Gauged::new(agora, context.clone())
                .for_agent(id)
                .with_shown_once(shown_once.clone())
                .with_prompt_check(client),
        );
        Self {
            tools,
            context,
            shown,
            shown_once,
        }
    }

    /// Answer `call` in a turn of its own, as the agent's model would be
    /// answered
    pub async fn call(&mut self, call: &ToolCall) -> tool::Result {
        self.shown_once.next_turn();
        self.tools
            .call(Use {
                id: "call".into(),
                name: call.name().into(),
                input: call.input(),
                cache_control: None,
                caller: None,
            })
            .await
    }
}
