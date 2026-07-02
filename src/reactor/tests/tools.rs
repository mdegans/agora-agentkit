//! The tool-dispatch half of the default [`Agent::handle`]: a `tool_use`
//! response is routed through the [`ToolBox`], its result seated as the next
//! user turn, and the drive continues — or, when no call progresses, stalls.
//!
//! [`Agent::handle`]: crate::reactor::Agent::handle
//! [`ToolBox`]: misanthropic::tool::ToolBox

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use misanthropic::prompt::message::Content;
use misanthropic::response::StopReason;
use misanthropic::tool::tool;
use schemars::JsonSchema;
use serde::Deserialize;

use super::*;

#[derive(Debug, Deserialize, JsonSchema)]
struct Echo {
    /// Text to echo back.
    text: String,
}

/// A tool that counts its calls and can be told to fail — so one tool exercises
/// both the progressing (`Continue`) and the no-progress (`Stalled`) branches of
/// the default `handle`. A failure returns a model-facing error, which the macro
/// renders as an `is_error` tool result.
struct CountingTool {
    calls: Arc<AtomicUsize>,
    fail: bool,
}

#[tool(name = "counting")]
impl CountingTool {
    /// Echo the text back.
    #[method]
    async fn echo(&mut self, args: Echo) -> Result<Content, Content> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            Err("tool failed".into())
        } else {
            Ok(format!("echo: {}", args.text).into())
        }
    }
}

/// An assistant response calling `name` with `input`, ended by `stop`.
fn tool_use_message(
    id: &str,
    name: &str,
    input: serde_json::Value,
    stop: StopReason,
) -> response::Message {
    serde_json::from_value(serde_json::json!({
        "id": "msg_test",
        "role": "assistant",
        "content": [{
            "type": "tool_use",
            "id": id,
            "name": name,
            "input": input,
        }],
        "model": "claude-3-5-haiku-latest",
        "stop_reason": stop_str(stop),
        "stop_sequence": null,
    }))
    .expect("valid tool_use response::Message fixture")
}

/// A `Complete` [`TestAgent`] with a [`CountingTool`] registered, alongside the
/// route the [`ToolBox`] minted for its one method — a mock scripts a `tool_use`
/// against that exact route, so the namespacing never has to be hardcoded.
fn tool_agent(calls: Arc<AtomicUsize>, fail: bool) -> (TestAgent, String) {
    let mut a = agent(Behavior::Complete, 1);
    a.tools.push_typed(CountingTool { calls, fail });
    let route = a
        .tools
        .method_names()
        .next()
        .expect("the tool registered one method")
        .to_string();
    (a, route)
}

/// A `tool_use` response is dispatched through the `ToolBox`, its result seated,
/// and the drive continues; the next (quiescent) response quiesces the agent to
/// `Complete`. The counter proves the tool actually ran.
#[tokio::test]
async fn tool_call_dispatches_and_continues() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (a, route) = tool_agent(calls.clone(), false);
    let mock = MockInference::scripted([
        tool_use_message(
            "toolu_1",
            &route,
            serde_json::json!({ "text": "hi" }),
            StopReason::ToolUse,
        ),
        message(StopReason::EndTurn),
    ]);
    let mut reactor: Reactor<_, _, TestAgent> =
        Reactor::new(mock, MemStore::default(), [a]);
    let report = reactor.run().await.unwrap();

    assert_eq!(report.done, 1, "agent completed after the tool round");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "the tool was invoked once");
}

/// A `tool_use` whose dispatch fails makes no progress, so `handle` returns
/// `Stalled`; repeated, it hits the stall cap and the agent is failed. The tool
/// still ran each round.
#[tokio::test]
async fn failing_tool_call_stalls_to_cap() {
    const MAX: usize =
        Reactor::<MockInference, MemStore, TestAgent>::MAX_STALLS;
    let calls = Arc::new(AtomicUsize::new(0));
    let (a, route) = tool_agent(calls.clone(), true);
    let mock = MockInference::scripted((0..MAX).map(|i| {
        tool_use_message(
            &format!("toolu_{i}"),
            &route,
            serde_json::json!({ "text": "hi" }),
            StopReason::ToolUse,
        )
    }));
    let mut reactor: Reactor<_, _, TestAgent> =
        Reactor::new(mock, MemStore::default(), [a]);
    let report = reactor.run().await.unwrap();

    assert_eq!(report.failed, 1, "stalled to the cap");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        MAX,
        "the tool ran each stalling round"
    );
}

/// A clipped (`MaxTokens`) response is never dispatched, even when it carries a
/// well-formed `tool_use` — its arguments may be missing pieces the model never
/// got to emit. The default `handle` routes it to `on_truncate`: the tool never
/// runs, nothing is seated, and the round stalls.
#[tokio::test]
async fn truncated_tool_use_is_not_dispatched() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (mut a, route) = tool_agent(calls.clone(), false);
    let seated = a.prompt.messages.len();

    let control = a
        .handle(tool_use_message(
            "toolu_1",
            &route,
            serde_json::json!({ "text": "hi" }),
            StopReason::MaxTokens,
        ))
        .await
        .unwrap();

    assert_eq!(control, Control::Stalled);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "the clipped call never ran"
    );
    assert_eq!(a.prompt.messages.len(), seated, "nothing was seated");
}
