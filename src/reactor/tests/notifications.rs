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

/// Fails its first `failures` requests (unreachable, or answered with a
/// wait), pushing `note` on the first; records every request it is sent
struct PushWhileHeld {
    mailbox: Mailbox,
    note: Option<&'static str>,
    failures_left: AtomicUsize,
    unreachable: bool,
    sent: Arc<Mutex<Vec<String>>>,
}

impl PushWhileHeld {
    fn new(
        mailbox: Mailbox,
        note: Option<&'static str>,
        unreachable: bool,
    ) -> Self {
        Self {
            mailbox,
            note,
            failures_left: AtomicUsize::new(2),
            unreachable,
            sent: Arc::default(),
        }
    }
}

#[async_trait::async_trait]
impl Inference for PushWhileHeld {
    type Error = TestError;

    // `fetch_update` is `try_update` from Rust 1.99, which 1.98 lacks
    #[allow(deprecated)]
    async fn infer<P>(&self, prompt: P) -> Result<response::Message, TestError>
    where
        P: Serialize + Send,
    {
        let first = self.sent.lock().unwrap().is_empty();
        self.sent
            .lock()
            .unwrap()
            .push(serde_json::to_string(&prompt).unwrap());
        let failing = self
            .failures_left
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                n.checked_sub(1)
            })
            .is_ok();
        if !failing {
            return Ok(message(StopReason::EndTurn));
        }
        if let (true, Some(note)) = (first, self.note) {
            self.mailbox.send(note, vec![Role::User]).unwrap();
        }
        Err(if self.unreachable {
            TestError::Unreachable
        } else {
            TestError::Transient(Duration::from_millis(1))
        })
    }

    async fn models(&self) -> Result<misanthropic::model::Models, TestError> {
        Ok(offered_models())
    }
}

/// Drive a fresh [`NotifyAgent`] through a [`PushWhileHeld`] holding its
/// mailbox, holding on unreachable, and return the requests it was sent
async fn sent_while_held(
    note: Option<&'static str>,
    unreachable: bool,
) -> Vec<String> {
    let agent = notify_agent();
    let inference =
        PushWhileHeld::new(agent.mailbox.clone(), note, unreachable);
    let sent = inference.sent.clone();
    let mut reactor: Reactor<_, _, NotifyAgent> =
        Reactor::new(inference, MemStore::default(), vec![agent])
            .with_unreachable_hold(Duration::from_millis(1));
    let report = reactor.run().await.unwrap();
    assert_eq!(report.done, 1, "{:?}", report.errors);
    let sent = sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 3, "two failures, then the response");
    sent
}

/// A push that arrives while the endpoint can't be reached is seated before
/// the next attempt, not after the response (S2): the re-sent request
/// carries it, the one already sent does not, and it is seated once
#[tokio::test]
async fn a_push_while_held_enters_the_next_attempt() {
    let sent = sent_while_held(Some("arrived while held"), true).await;
    assert!(!sent[0].contains("arrived while held"), "{}", sent[0]);
    assert!(sent[1].contains("arrived while held"), "{}", sent[1]);
    assert!(sent[1].contains("[notification: test_tool]"), "{}", sent[1]);
    assert_eq!(sent[2].matches("arrived while held").count(), 1);
}

/// The same for a retry the endpoint asked for (a 429/529 with a wait)
#[tokio::test]
async fn a_push_during_a_retry_wait_enters_the_retry() {
    let sent = sent_while_held(Some("arrived during the wait"), false).await;
    assert!(!sent[0].contains("arrived during the wait"), "{}", sent[0]);
    assert!(sent[1].contains("arrived during the wait"), "{}", sent[1]);
}

/// With nothing pushed, every re-attempt sends the very same request: the
/// extra `on_turn` changes nothing, markers included, when nothing arrived
#[tokio::test]
async fn a_retry_with_nothing_new_resends_the_same_bytes() {
    let sent = sent_while_held(None, true).await;
    assert_eq!(sent[0], sent[1]);
    assert_eq!(sent[1], sent[2]);
}
