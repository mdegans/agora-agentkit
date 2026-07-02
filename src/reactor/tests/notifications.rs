//! The notification-drain half of the provided defaults: a pushed
//! [`Notification`] is seated at the turn boundary (`on_turn`) or turns a
//! quiescent `handle` into [`Control::Continue`].
//!
//! [`Notification`]: misanthropic::tool::Notification

use misanthropic::prompt::message::Block;
use misanthropic::tool::{Mailbox, Notifications};

use super::*;

/// A [`TestAgent`]-shaped agent that holds the consumer end of a mailbox and
/// hands it to the provided defaults via [`Agent::notifications`].
struct NotifyAgent {
    id: AgentId,
    state: TestState,
    prompt: Prompt,
    tools: misanthropic::tool::ToolBox,
    /// Send side, kept so a test can push mid-flight.
    mailbox: Mailbox,
    notifications: Option<Notifications>,
}

#[async_trait::async_trait]
impl Agent for NotifyAgent {
    type State = TestState;
    type Context = ();
    type Error = TestError;

    fn new(
        id: AgentId,
        state: TestState,
        _context: (),
    ) -> Result<Self, TestError> {
        let mut mailbox = Mailbox::new("test_tool");
        let notifications = mailbox.subscribe();
        let mut agent = Self {
            id,
            state,
            prompt: Prompt::default(),
            tools: misanthropic::tool::ToolBox::new(),
            mailbox,
            notifications,
        };
        agent
            .prompt
            .push_message((Role::User, "start"))
            .map_err(|e| TestError::Msg(e.to_string()))?;
        Ok(agent)
    }

    fn id(&self) -> AgentId {
        self.id
    }

    fn state(&self) -> &TestState {
        &self.state
    }

    fn prompt(&self) -> &Prompt {
        &self.prompt
    }

    fn parts(&mut self) -> (&mut misanthropic::tool::ToolBox, &mut Prompt) {
        (&mut self.tools, &mut self.prompt)
    }

    fn notifications(&mut self) -> Option<&mut Notifications> {
        self.notifications.as_mut()
    }

    fn model(&self) -> ModelInfo {
        model_info(false)
    }
}

fn notify_agent() -> NotifyAgent {
    NotifyAgent::new(
        AgentId::new(),
        TestState {
            behavior: Behavior::Complete,
            turns_left: 1,
            poison: None,
        },
        (),
    )
    .unwrap()
}

/// All text in a message, flattened — enough to assert seating.
fn text_of(msg: &misanthropic::prompt::Message) -> String {
    msg.iter()
        .filter_map(|b| match b {
            Block::Text { text, .. } => Some(text.to_string()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A quiescent response with a pending push seats the push (labeled with its
/// source) as a fresh user turn and continues; the next quiescent response
/// (nothing pending) quiesces normally.
#[tokio::test]
async fn quiescent_with_pending_push_continues() {
    let mut agent = notify_agent();
    agent
        .mailbox
        .send("job 7 finished", vec![Role::User])
        .unwrap();

    let control = agent.handle(message(StopReason::EndTurn)).await.unwrap();
    assert_eq!(control, Control::Continue);

    let last = agent.prompt().messages.last().unwrap();
    assert_eq!(last.role, Role::User);
    let text = text_of(last);
    assert!(text.contains("[notification: test_tool]"), "{text}");
    assert!(text.contains("job 7 finished"), "{text}");

    // Nothing pending now: the same quiescent response ends the session.
    let control = agent.handle(message(StopReason::EndTurn)).await.unwrap();
    assert_eq!(control, Control::Done(Outcome::Complete));
}

/// `on_turn` merges pushes into the trailing user turn rather than seating an
/// (illegal) adjacent one.
#[tokio::test]
async fn on_turn_merges_into_trailing_user_turn() {
    let mut agent = notify_agent();
    let before = agent.prompt().messages.len();
    agent
        .mailbox
        .send("you have mail", vec![Role::User])
        .unwrap();

    agent.on_turn().await.unwrap();

    assert_eq!(agent.prompt().messages.len(), before, "merged, not seated");
    let last = agent.prompt().messages.last().unwrap();
    assert_eq!(last.role, Role::User);
    let text = text_of(last);
    assert!(text.contains("start"), "{text}");
    assert!(text.contains("you have mail"), "{text}");
}

/// No receiver (the default `notifications`) and an empty queue both leave the
/// defaults untouched: quiescence still quiesces.
#[tokio::test]
async fn empty_queue_changes_nothing() {
    let mut agent = notify_agent();
    let before = agent.prompt().messages.len();

    agent.on_turn().await.unwrap();
    assert_eq!(agent.prompt().messages.len(), before);

    let control = agent.handle(message(StopReason::EndTurn)).await.unwrap();
    assert_eq!(control, Control::Done(Outcome::Complete));
}
