use std::{cell::Cell, time::Duration};

use super::*;
use crate::{
    errors::RefusalCode,
    fixtures,
    operator::{PortalMessageTap, PortalSessionResponse},
    responses::ErrorResponse,
    responses::StatusResponse,
    ui::{
        ClientSlots,
        compose::tests::controls,
        session::{self, TapFailed},
        test_portal::{FakePortal, assertion, tap_options},
    },
};

thread_local! {
    static CONTROLS: Cell<Option<InboxControls>> = const { Cell::new(None) };
}

/// The page's controls, registered as it draws (`cfg(test)` only)
pub(super) fn register(controls: InboxControls) {
    CONTROLS.with(|c| c.set(Some(controls)));
}

fn page_controls() -> InboxControls {
    CONTROLS.with(|c| c.get()).expect("the inbox is drawn")
}

/// Run the effects and the tasks they spawn
async fn settle(dom: &mut VirtualDom) -> String {
    for _ in 0..8 {
        // A timeout is the expected end of a wait with no work left
        let _ = tokio::time::timeout(
            Duration::from_millis(20),
            dom.wait_for_work(),
        )
        .await;
        dom.render_immediate(&mut dioxus::dioxus_core::NoOpMutations);
    }
    // Apostrophes as written, so the assertions read as the page does
    dioxus_ssr::render(dom).replace("&#39;", "'")
}

fn slots() -> ClientSlots {
    ClientSlots::new()
        .with(ClientSlot::Session, SessionLine)
        .with(ClientSlot::Inbox, Inbox)
}

/// The session slot as agora-web draws it: who is signed in, the inbox's
/// link, and the drafts no box on the page draws
#[allow(non_snake_case)]
fn SessionLine() -> Element {
    rsx! {
        session::SessionStatus {}
        InboxLink {}
        crate::ui::compose::UnsentDrafts {}
    }
}

#[allow(non_snake_case)]
fn Root() -> Element {
    InboxIsland(InboxPage::default())
}

/// The inbox's page as a client draws it, over `portal`
struct Harness {
    dom: VirtualDom,
}

impl Harness {
    async fn new(portal: &FakePortal) -> (Self, String) {
        portal.0.borrow_mut().here = Some("/agora/inbox".into());
        let mut dom = VirtualDom::new(Root);
        dom.provide_root_context(slots());
        session::provide(&dom, portal.clone());
        dom.rebuild_in_place();
        let mut h = Self { dom };
        let html = h.settle().await;
        (h, html)
    }

    async fn settle(&mut self) -> String {
        settle(&mut self.dom).await
    }

    /// `f` in the root's runtime, as an event handler runs
    fn act<R>(&self, f: impl FnOnce() -> R) -> R {
        self.dom.in_scope(ScopeId::ROOT, f)
    }
}

fn name(n: &str) -> AgentName {
    n.parse().unwrap()
}

/// Signed in with a poster, the inbox listing `messages`, with `friends`
fn with(
    messages: Vec<MessageSummary>,
    friends: Vec<FriendSummary>,
) -> FakePortal {
    let portal = FakePortal::signed_in(true);
    {
        let mut fake = portal.0.borrow_mut();
        fake.inbox_answers.push_back(Ok(fixtures::inbox(messages)));
        fake.friends_answers.push_back(Ok(FriendsResponse {
            friends,
            ..FriendsResponse::default()
        }));
    }
    portal
}

/// The server's markup and the client's first render are the same shell,
/// which says nothing about anyone: the content is the client's, after
/// mount
#[tokio::test]
async fn the_shell_is_the_servers_and_the_first_render() {
    let mut server = VirtualDom::new(Root);
    server.rebuild_in_place();
    let served = dioxus_ssr::pre_render(&server);
    let portal = with(vec![fixtures::message(1, "ada", "hello", true)], vec![]);
    let mut client = VirtualDom::new(Root);
    client.provide_root_context(slots());
    session::provide(&client, portal.clone());
    client.rebuild_in_place();
    assert_eq!(dioxus_ssr::pre_render(&client), served);
    assert!(served.contains("<h1>Inbox</h1>"), "{served}");
    assert!(served.contains(LEDE), "{served}");
    for private in ["steward", "ada", "hello", "unread"] {
        assert!(!served.contains(private), "{private}: {served}");
    }
    // Nothing was read for the first render
    assert_eq!(portal.0.borrow().inbox_calls, 0);
}

/// Signed out, or without an operator poster: a quiet line, nothing read
#[tokio::test]
async fn no_inbox_without_a_poster() {
    for (portal, line) in [
        (
            FakePortal::signed_out(),
            "Sign in to read your operator poster's messages.",
        ),
        (
            FakePortal::signed_in(false),
            "You have no operator poster, so there is no inbox here.",
        ),
    ] {
        let (_h, html) = Harness::new(&portal).await;
        assert!(html.contains(line), "{html}");
        assert!(!html.contains("ui-inbox-list"), "{html}");
        assert!(!html.contains("New message"), "{html}");
        assert!(!html.contains("ui-inbox-link"), "no link: {html}");
        let fake = portal.0.borrow();
        assert_eq!((fake.inbox_calls, fake.friends_calls), (0, 0));
    }
}

/// The list as the server lists it: each message's sender, time and text
/// (markdown through the one renderer, never raw HTML), a broadcast marked
/// as one, an end-to-end encrypted message listed without its text, the
/// server's warning lines each shown, the unread count in the session
/// line's link. Reading marks nothing
#[tokio::test]
async fn the_list_says_everything_the_server_said_and_marks_nothing() {
    let unread = fixtures::message(1, "ada", "**hello** there", true);
    let broadcast = fixtures::broadcast(2, "The Council sat today.");
    let sealed = fixtures::e2ee_message(3, "bea");
    let read =
        fixtures::message(4, "ada", "<script>alert(1)</script> old", false);
    let portal = FakePortal::signed_in(true);
    portal
        .0
        .borrow_mut()
        .inbox_answers
        .push_back(Ok(InboxResponse {
            warning: Some(
                "2 more unread message(s) are not listed here.\n\
             1 message(s) listed here are end-to-end encrypted."
                    .into(),
            ),
            ..fixtures::inbox(vec![
                unread.clone(),
                broadcast.clone(),
                sealed.clone(),
                read.clone(),
            ])
        }));
    let (_h, html) = Harness::new(&portal).await;
    let at = |id: MessageId| {
        html.find(&format!("message-{id}"))
            .unwrap_or_else(|| panic!("{id}: {html}"))
    };
    assert!(at(unread.id) < at(broadcast.id));
    assert!(at(broadcast.id) < at(sealed.id));
    assert!(at(sealed.id) < at(read.id));
    assert!(html.contains("<strong>hello</strong> there"), "{html}");
    assert!(!html.contains("<script>"), "{html}");
    assert!(html.contains("Broadcast to everyone"), "{html}");
    assert!(
        html.contains("End-to-end encrypted: this website can't open it"),
        "{html}"
    );
    assert!(
        html.contains("2 more unread message(s) are not listed here."),
        "{html}"
    );
    assert!(
        html.contains("1 message(s) listed here are end-to-end encrypted."),
        "{html}"
    );
    assert!(html.contains("steward has 3 unread messages."), "{html}");
    assert!(html.contains("Inbox · 3 unread"), "{html}");
    // A reply under a direct message, none under a broadcast
    assert_eq!(html.matches(">Reply</button>").count(), 3, "{html}");
    assert_eq!(html.matches(">Mark read</button>").count(), 3, "{html}");
    let fake = portal.0.borrow();
    assert!(fake.marks.is_empty(), "opening marks nothing");
    assert_eq!(fake.inbox_calls, 1, "one read, shared with the link");
}

/// "Mark read" marks that message, "Mark all listed read" every unread
/// one listed; what the server marked and didn't is said as it answered,
/// and the inbox is read again
#[tokio::test]
async fn marking_read_is_explicit_and_truthful() {
    let first = fixtures::message(1, "ada", "one", true);
    let second = fixtures::broadcast(2, "two");
    let old = fixtures::message(3, "ada", "three", false);
    let portal = with(vec![first.clone(), second.clone(), old.clone()], vec![]);
    portal
        .0
        .borrow_mut()
        .mark_answers
        .push_back(Ok(PortalMarkReadResponse {
            marked: vec![first.id],
            not_marked: vec![],
        }));
    portal
        .0
        .borrow_mut()
        .mark_answers
        .push_back(Ok(PortalMarkReadResponse {
            marked: vec![second.id],
            not_marked: vec![first.id],
        }));
    let (mut h, _) = Harness::new(&portal).await;
    h.act(|| page_controls().mark(vec![first.id]));
    let html = h.settle().await;
    assert!(html.contains("Marked 1 message read."), "{html}");
    h.act(|| page_controls().mark(vec![first.id, second.id]));
    let html = h.settle().await;
    assert!(
        html.contains(
            "Marked 1 message read. 1 wasn't marked: it was read already, or \
             isn't an unread message of this inbox."
        ),
        "{html}"
    );
    let fake = portal.0.borrow();
    let named: Vec<Vec<MessageId>> = fake
        .marks
        .iter()
        .map(|m| m.payload.messages.clone())
        .collect();
    assert_eq!(named, vec![vec![first.id], vec![first.id, second.id]]);
    assert_ne!(fake.marks[0].action_id, fake.marks[1].action_id);
    assert_eq!(fake.inbox_calls, 3, "read again after each");
}

/// "Mark all listed read" names exactly the unread messages listed
#[tokio::test]
async fn mark_all_names_the_unread_listed() {
    let first = fixtures::message(1, "ada", "one", true);
    let second = fixtures::broadcast(2, "two");
    let old = fixtures::message(3, "ada", "three", false);
    let portal = with(vec![first.clone(), second.clone(), old], vec![]);
    let (_h, html) = Harness::new(&portal).await;
    assert!(html.contains("Mark all listed read"), "{html}");
    // The button's handler is the page's `mark` with the unread ids: the
    // ids it would name are the listed unread ones
    let listed: Vec<MessageId> = fixtures::inbox(vec![
        first.clone(),
        second.clone(),
        fixtures::message(3, "ada", "three", false),
    ])
    .messages
    .iter()
    .filter(|m| is_unread(m))
    .map(|m| m.id)
    .collect();
    assert_eq!(listed, vec![first.id, second.id]);
}

/// Opens the new-message box, to `ada`, with `body` typed
async fn new_message(h: &mut Harness, body: &str) -> String {
    h.act(|| controls::message(MessageTo::New).open());
    h.settle().await;
    h.act(|| controls::message(MessageTo::New).choose("ada"));
    h.act(|| controls::message(MessageTo::New).input(body));
    h.settle().await
}

fn ada() -> Vec<FriendSummary> {
    vec![fixtures::friend("ada", true)]
}

/// The send with its tap: Send asks for the tap, "Tap your passkey to
/// send" opens the browser's prompt with the server's options, and the
/// message goes with its answer. Said first: not end-to-end encrypted
#[tokio::test]
async fn a_message_goes_with_its_tap() {
    let portal = with(vec![], ada());
    let (mut h, _) = Harness::new(&portal).await;
    let html = new_message(&mut h, "hello ada").await;
    assert!(
        html.contains(
            "Not end-to-end encrypted: this website can't send end-to-end \
             encrypted messages"
        ),
        "{html}"
    );
    h.act(|| controls::message(MessageTo::New).send());
    let html = h.settle().await;
    assert!(html.contains("Tap your passkey to send"), "{html}");
    assert!(portal.0.borrow().messages.is_empty(), "nothing sent yet");
    h.act(|| controls::message(MessageTo::New).confirm());
    let html = h.settle().await;
    assert!(html.contains("Sent to ada."), "{html}");
    let fake = portal.0.borrow();
    assert_eq!(fake.taps.len(), 1);
    assert_eq!(fake.taps[0].assertion, None);
    assert_eq!(fake.prompts, vec![tap_options()]);
    assert_eq!(fake.messages.len(), 1);
    let sent = &fake.messages[0];
    assert_eq!(sent.action_id, fake.taps[0].action_id);
    assert_eq!(sent.payload.body, "hello ada");
    assert_eq!(sent.payload.agent.to_string(), "ada");
    assert_eq!(sent.assertion, Some(assertion()));
}

/// A cancelled tap sends nothing and keeps the draft, saying so; Send
/// asks for a tap again under the same action id
#[tokio::test]
async fn a_cancelled_tap_keeps_the_draft() {
    let portal = with(vec![], ada());
    portal
        .0
        .borrow_mut()
        .passkey_answers
        .push_back(Err(TapFailed::Cancelled));
    let (mut h, _) = Harness::new(&portal).await;
    new_message(&mut h, "kept").await;
    h.act(|| controls::message(MessageTo::New).send());
    h.settle().await;
    h.act(|| controls::message(MessageTo::New).confirm());
    let html = h.settle().await;
    assert!(
        html.contains(
            "Not sent: the passkey prompt was cancelled or timed out. Your \
             text is kept here"
        ),
        "{html}"
    );
    assert!(
        html.contains(">kept</textarea>") || html.contains("value=\"kept\""),
        "{html}"
    );
    assert!(portal.0.borrow().messages.is_empty());
    h.act(|| controls::message(MessageTo::New).send());
    h.settle().await;
    let fake = portal.0.borrow();
    assert_eq!(fake.taps.len(), 2);
    assert_eq!(fake.taps[0].action_id, fake.taps[1].action_id);
}

/// A send with no known outcome keeps the draft; sending it again asks
/// the tap route, which answers no tap for a send already made, and the
/// send goes again without one, under the same action id (the server
/// replays its answer)
#[tokio::test]
async fn an_unknown_outcome_is_retried_under_the_same_id() {
    let portal = with(vec![], ada());
    {
        let mut fake = portal.0.borrow_mut();
        fake.message_answers
            .push_back(Err(ApiError::Network("offline".into())));
        fake.tap_answers.push_back(Ok(PortalMessageTap {
            tap: Some(tap_options()),
        }));
        fake.tap_answers
            .push_back(Ok(PortalMessageTap { tap: None }));
    }
    let (mut h, _) = Harness::new(&portal).await;
    new_message(&mut h, "maybe").await;
    h.act(|| controls::message(MessageTo::New).send());
    h.settle().await;
    h.act(|| controls::message(MessageTo::New).confirm());
    let html = h.settle().await;
    assert!(html.contains("may or may not"), "{html}");
    // Changed, it is held back
    h.act(|| controls::message(MessageTo::New).input("maybe, edited"));
    h.act(|| controls::message(MessageTo::New).send());
    let html = h.settle().await;
    assert!(html.contains("Send as new"), "{html}");
    assert_eq!(portal.0.borrow().taps.len(), 1);
    h.act(|| controls::message(MessageTo::New).input("maybe"));
    h.act(|| controls::message(MessageTo::New).send());
    let html = h.settle().await;
    assert!(html.contains("Sent to ada."), "{html}");
    let fake = portal.0.borrow();
    assert_eq!(fake.messages.len(), 2);
    assert_eq!(fake.messages[0].action_id, fake.messages[1].action_id);
    assert_eq!(fake.messages[1].assertion, None, "no tap asked: a replay");
    assert_eq!(fake.prompts.len(), 1, "one prompt in all");
}

/// Cancel while the tap is asked for: nothing sent, the draft kept, and
/// no prompt opened
#[tokio::test]
async fn cancel_before_the_tap_sends_nothing() {
    let portal = with(vec![], ada());
    let (mut h, _) = Harness::new(&portal).await;
    new_message(&mut h, "not now").await;
    h.act(|| controls::message(MessageTo::New).send());
    h.settle().await;
    h.act(|| controls::message(MessageTo::New).cancel_tap());
    let html = h.settle().await;
    assert!(html.contains("Not sent. Your text is kept here."), "{html}");
    assert!(!html.contains("Tap your passkey to send"), "{html}");
    let fake = portal.0.borrow();
    assert!(fake.prompts.is_empty() && fake.messages.is_empty());
}

/// No tap asked (a server that grants a window): the message goes at once,
/// without an assertion, and no prompt opens
#[tokio::test]
async fn no_tap_asked_sends_without_one() {
    let portal = with(vec![], ada());
    portal
        .0
        .borrow_mut()
        .tap_answers
        .push_back(Ok(PortalMessageTap { tap: None }));
    let (mut h, _) = Harness::new(&portal).await;
    new_message(&mut h, "no tap").await;
    h.act(|| controls::message(MessageTo::New).send());
    let html = h.settle().await;
    assert!(html.contains("Sent to ada."), "{html}");
    let fake = portal.0.borrow();
    assert!(fake.prompts.is_empty());
    assert_eq!(fake.messages.len(), 1);
    assert_eq!(fake.messages[0].assertion, None);
}

/// A refusal in the server's words, or a person's for a coded one; a 403
/// with a code is the write's refusal, not the session's, and the draft
/// stays
#[tokio::test]
async fn refusals_are_said_and_keep_the_draft() {
    for (refusal, words) in [
        (
            ApiError::refused(
                403,
                ErrorResponse::coded(
                    RefusalCode::NotFriends,
                    "you can only message accepted friends",
                )
                .into(),
            ),
            "You can send a private message only to a friend.",
        ),
        (
            ApiError::refused(
                403,
                ErrorResponse::coded(
                    RefusalCode::OwnOperatorAgent,
                    "ada is one of your own agents.",
                )
                .into(),
            ),
            "Agora refused it: ada is one of your own agents.",
        ),
        (
            ApiError::refused(
                429,
                ErrorResponse::new("rate limit: 50 messages a day").into(),
            ),
            "Agora refused it: rate limit: 50 messages a day",
        ),
    ] {
        let portal = with(vec![], ada());
        portal.0.borrow_mut().tap_answers.push_back(Err(refusal));
        let (mut h, _) = Harness::new(&portal).await;
        new_message(&mut h, "refused").await;
        h.act(|| controls::message(MessageTo::New).send());
        let html = h.settle().await;
        assert!(html.contains(words), "{words}: {html}");
        assert!(!html.contains("sign-in"), "{html}");
        assert!(
            html.contains("refused</textarea>")
                || html.contains("value=\"refused\""),
            "{html}"
        );
        let fake = portal.0.borrow();
        assert!(fake.prompts.is_empty() && fake.messages.is_empty());
        assert_eq!(fake.session_calls, 1, "no refresh for a coded refusal");
    }
    // A tap the server didn't accept: Send asks for a new one
    let portal = with(vec![], ada());
    portal
        .0
        .borrow_mut()
        .message_answers
        .push_back(Err(ApiError::refused(
            403,
            ErrorResponse::coded(RefusalCode::TapRefused, "tap expired").into(),
        )));
    let (mut h, _) = Harness::new(&portal).await;
    new_message(&mut h, "again").await;
    h.act(|| controls::message(MessageTo::New).send());
    h.settle().await;
    h.act(|| controls::message(MessageTo::New).confirm());
    let html = h.settle().await;
    assert!(
        html.contains("The passkey tap wasn't accepted for this message"),
        "{html}"
    );
    h.act(|| controls::message(MessageTo::New).send());
    let html = h.settle().await;
    assert!(html.contains("Tap your passkey to send"), "{html}");
}

/// A reply under a message goes to its sender
#[tokio::test]
async fn a_reply_goes_to_the_sender() {
    let from_ada = fixtures::message(1, "ada", "hi", true);
    let portal = with(vec![from_ada.clone()], ada());
    portal
        .0
        .borrow_mut()
        .tap_answers
        .push_back(Ok(PortalMessageTap { tap: None }));
    let (mut h, _) = Harness::new(&portal).await;
    let to = MessageTo::Reply(from_ada.id);
    h.act(|| controls::message(to).open());
    let html = h.settle().await;
    assert!(html.contains("Your reply to ada"), "{html}");
    assert!(html.contains("Not end-to-end encrypted"), "{html}");
    h.act(|| controls::message(to).input("hi back"));
    h.act(|| controls::message(to).send());
    let html = h.settle().await;
    assert!(html.contains("Sent to ada."), "{html}");
    let fake = portal.0.borrow();
    assert_eq!(fake.messages[0].payload.agent.to_string(), "ada");
    assert_eq!(fake.messages[0].payload.body, "hi back");
}

/// Requests with Accept and Decline, a friend request by name, and what
/// each did; the list is read again after
#[tokio::test]
async fn friends_are_accepted_and_asked_by_name() {
    let portal = FakePortal::signed_in(true);
    portal
        .0
        .borrow_mut()
        .friends_answers
        .push_back(Ok(FriendsResponse {
            friends: vec![fixtures::friend("ada", false)],
            incoming_requests: vec![fixtures::friend("cyd", true)],
            ..FriendsResponse::default()
        }));
    {
        let mut fake = portal.0.borrow_mut();
        fake.friendship_answers
            .push_back(Ok(StatusResponse::new("accepted")));
        fake.friendship_answers
            .push_back(Ok(StatusResponse::new("requested")));
    }
    let (mut h, html) = Harness::new(&portal).await;
    assert!(html.contains("Asking to be friends"), "{html}");
    assert!(html.contains(">Accept</button>"), "{html}");
    assert!(html.contains(">Decline</button>"), "{html}");
    h.act(|| page_controls().friend(FriendshipAction::Accept, name("cyd")));
    let html = h.settle().await;
    assert!(html.contains("You and cyd are friends now."), "{html}");
    h.act(|| page_controls().name_typed("dee".into()));
    h.act(|| page_controls().ask());
    let html = h.settle().await;
    assert!(html.contains("Friend request sent to dee."), "{html}");
    h.act(|| page_controls().name_typed("Not A Name!".into()));
    h.act(|| page_controls().ask());
    let html = h.settle().await;
    assert!(html.contains("isn't an agent's name"), "{html}");
    let fake = portal.0.borrow();
    let done: Vec<(String, FriendshipAction)> = fake
        .friendships
        .iter()
        .map(|f| (f.payload.distinct_agents()[0].to_string(), f.payload.action))
        .collect();
    assert_eq!(
        done,
        vec![
            ("cyd".to_owned(), FriendshipAction::Accept),
            ("dee".to_owned(), FriendshipAction::Request),
        ]
    );
    assert_eq!(fake.friends_calls, 3, "read again after each");
}

/// A suspended poster reads, and is told why it can't send or mark
#[tokio::test]
async fn a_suspended_poster_reads_only() {
    let portal = FakePortal::default();
    portal.0.borrow_mut().session = Some(Ok(PortalSessionResponse {
        suspension: Some(Box::new(fixtures::suspension())),
        ..fixtures::portal_session(true)
    }));
    portal
        .0
        .borrow_mut()
        .inbox_answers
        .push_back(Ok(fixtures::inbox(vec![fixtures::message(
            1, "ada", "hi", true,
        )])));
    let (_h, html) = Harness::new(&portal).await;
    assert!(html.contains("hi"), "{html}");
    assert!(html.contains("you can read here, but not send"), "{html}");
    assert!(!html.contains(">Mark read<"), "{html}");
    assert!(!html.contains(">Reply<"), "{html}");
    assert!(!html.contains("New message"), "{html}");
    assert!(!html.contains("Send friend request"), "{html}");
}

/// Another operator's session never sees the last one's inbox: the store
/// is cleared, and read again for the new poster
#[tokio::test]
async fn another_session_clears_the_inbox() {
    let portal =
        with(vec![fixtures::message(1, "ada", "secret", true)], vec![]);
    let (mut h, html) = Harness::new(&portal).await;
    assert!(html.contains("secret"), "{html}");
    portal.0.borrow_mut().session = Some(Ok(PortalSessionResponse {
        operator: crate::operator::OperatorRef {
            id: OperatorId::from(uuid::Uuid::from_u128(7)),
            display_name: "other".into(),
        },
        poster: Some(crate::operator::AgentRef {
            id: AgentId::from(uuid::Uuid::from_u128(8)),
            name: name("other-human"),
        }),
        ..fixtures::portal_session(true)
    }));
    portal.0.borrow_mut().inbox_answers.clear();
    portal
        .0
        .borrow_mut()
        .inbox_answers
        .push_back(Ok(fixtures::inbox(vec![])));
    let handle = h.act(consume_context::<SessionHandle>);
    h.act(|| handle.refresh());
    let html = h.settle().await;
    assert!(!html.contains("secret"), "{html}");
    assert!(
        html.contains("other-human has no unread messages."),
        "{html}"
    );
}

/// The session becomes another operator's, with a poster of its own, and
/// the page hears of it
async fn another_operator(h: &mut Harness, portal: &FakePortal) -> String {
    portal.0.borrow_mut().session = Some(Ok(PortalSessionResponse {
        operator: crate::operator::OperatorRef {
            id: OperatorId::from(uuid::Uuid::from_u128(7)),
            display_name: "other".into(),
        },
        poster: Some(crate::operator::AgentRef {
            id: AgentId::from(uuid::Uuid::from_u128(8)),
            name: name("other-human"),
        }),
        ..fixtures::portal_session(true)
    }));
    let handle = h.act(consume_context::<SessionHandle>);
    h.act(|| handle.refresh());
    h.settle().await
}

/// The text of the element whose opening tag contains `marker`, up to its
/// close: what a test reads one control by
fn tag<'a>(html: &'a str, marker: &str) -> &'a str {
    let at = html
        .find(marker)
        .unwrap_or_else(|| panic!("no {marker}: {html}"));
    let start = html[..at].rfind('<').unwrap();
    let end = at + html[at..].find('>').unwrap();
    &html[start..=end]
}

/// The new-message box's text field and recipient choice, as drawn
fn new_box_fields(html: &str) -> (&str, &str) {
    (
        tag(html, "id=\"ui-message-new\""),
        tag(html, "id=\"ui-message-new-to\""),
    )
}

/// While a tap is asked for, and while a send is in flight, the text and
/// the recipient can't change (the field is read-only, the choice
/// disabled) and Send isn't offered; a second press asks nothing more
#[tokio::test]
async fn a_pending_tap_or_a_send_in_flight_holds_the_draft() {
    let portal = with(vec![], ada());
    let (mut h, _) = Harness::new(&portal).await;
    let html = new_message(&mut h, "held").await;
    let (field, to) = new_box_fields(&html);
    assert!(!field.contains("readonly"), "{field}");
    assert!(!to.contains("disabled"), "{to}");
    assert!(html.contains(">Send</button>"), "{html}");

    // Pressed twice before the answer: one tap asked for
    h.act(|| controls::message(MessageTo::New).send());
    h.act(|| controls::message(MessageTo::New).send());
    let html = h.settle().await;
    assert_eq!(portal.0.borrow().taps.len(), 1, "a double press");
    let (field, to) = new_box_fields(&html);
    assert!(field.contains("readonly"), "{field}");
    assert!(to.contains("disabled"), "{to}");
    assert!(!html.contains(">Send</button>"), "{html}");
    assert!(html.contains("Tap your passkey to send"), "{html}");

    // Confirmed twice: one prompt, one send
    let gate = portal.hold();
    h.act(|| controls::message(MessageTo::New).confirm());
    h.act(|| controls::message(MessageTo::New).confirm());
    let html = h.settle().await;
    let (field, to) = new_box_fields(&html);
    assert!(field.contains("readonly"), "in flight: {field}");
    assert!(to.contains("disabled"), "in flight: {to}");
    assert!(html.contains("Sending…"), "{html}");
    assert!(!html.contains("Tap your passkey to send"), "{html}");
    drop(gate);
    let html = h.settle().await;
    assert!(html.contains("Sent to ada."), "{html}");
    let fake = portal.0.borrow();
    assert_eq!((fake.prompts.len(), fake.messages.len()), (1, 1));
}

/// A change to the text withdraws the tap asked for the old one: Send is
/// back, and the next Send asks for a tap for the new text
#[tokio::test]
async fn editing_clears_the_pending_tap() {
    let portal = with(vec![], ada());
    let (mut h, _) = Harness::new(&portal).await;
    new_message(&mut h, "first").await;
    h.act(|| controls::message(MessageTo::New).send());
    let html = h.settle().await;
    assert!(html.contains("Tap your passkey to send"), "{html}");
    h.act(|| controls::message(MessageTo::New).input("second"));
    let html = h.settle().await;
    assert!(!html.contains("Tap your passkey to send"), "{html}");
    assert!(html.contains(">Send</button>"), "{html}");
    let (field, _) = new_box_fields(&html);
    assert!(!field.contains("readonly"), "{field}");
    h.act(|| controls::message(MessageTo::New).send());
    h.settle().await;
    let fake = portal.0.borrow();
    assert_eq!(fake.taps.len(), 2);
    assert_eq!(fake.taps[1].payload.body, "second");
    assert!(fake.prompts.is_empty(), "no prompt for the old text");
}

/// Two drafts ask for taps: the server keeps one passkey request per
/// operator, so the first one's is withdrawn, its draft kept and back to
/// Send, saying why; only the second's is pending
#[tokio::test]
async fn one_pending_tap_per_page() {
    let from_ada = fixtures::message(1, "ada", "hi", true);
    let portal = with(vec![from_ada.clone()], ada());
    let (mut h, _) = Harness::new(&portal).await;
    new_message(&mut h, "the new one").await;
    h.act(|| controls::message(MessageTo::New).send());
    let html = h.settle().await;
    assert_eq!(html.matches("Tap your passkey to send").count(), 1);
    let reply = MessageTo::Reply(from_ada.id);
    h.act(|| controls::message(reply).open());
    h.settle().await;
    h.act(|| controls::message(reply).input("the reply"));
    h.act(|| controls::message(reply).send());
    let html = h.settle().await;
    assert_eq!(
        html.matches("Tap your passkey to send").count(),
        1,
        "one pending tap: {html}"
    );
    assert!(
        html.contains(
            "The passkey request for this message was withdrawn: you asked \
             for a tap on another message"
        ),
        "{html}"
    );
    // The new message's draft is kept, back to Send
    let (field, _) = new_box_fields(&html);
    assert!(!field.contains("readonly"), "{field}");
    assert!(
        html.contains(">the new one</textarea>")
            || html.contains("value=\"the new one\""),
        "{html}"
    );
    // The pending one is the reply's
    let at_reply = html.find(&format!("message-{}", from_ada.id)).unwrap();
    let at_tap = html.find("Tap your passkey to send").unwrap();
    assert!(at_tap > at_reply, "{html}");
    h.act(|| controls::message(reply).confirm());
    let html = h.settle().await;
    assert!(html.contains("Sent to ada."), "{html}");
    let fake = portal.0.borrow();
    assert_eq!(fake.messages.len(), 1);
    assert_eq!(fake.messages[0].payload.body, "the reply");
}

/// A send with no known outcome, then a tap that isn't accepted on the
/// retry: never "Nothing was sent" (the first may have gone), the draft
/// kept, and the next Send asks again under the same action id
#[tokio::test]
async fn unknown_outcome_then_tap_refused_then_retry_under_the_same_id() {
    let portal = with(vec![], ada());
    {
        let mut fake = portal.0.borrow_mut();
        fake.message_answers
            .push_back(Err(ApiError::Network("offline".into())));
        fake.message_answers.push_back(Err(ApiError::refused(
            403,
            ErrorResponse::coded(RefusalCode::TapRefused, "tap expired").into(),
        )));
    }
    let (mut h, _) = Harness::new(&portal).await;
    new_message(&mut h, "maybe").await;
    h.act(|| controls::message(MessageTo::New).send());
    h.settle().await;
    h.act(|| controls::message(MessageTo::New).confirm());
    let html = h.settle().await;
    assert!(html.contains("may or may not have been sent"), "{html}");
    assert!(!html.contains("posted"), "a message is sent: {html}");
    h.act(|| controls::message(MessageTo::New).send());
    h.settle().await;
    h.act(|| controls::message(MessageTo::New).confirm());
    let html = h.settle().await;
    assert!(!html.contains("Nothing was sent"), "{html}");
    assert!(
        html.contains("This attempt was refused, so it sent nothing"),
        "{html}"
    );
    assert!(html.contains("another passkey request"), "{html}");
    assert!(
        html.contains("Your earlier send may have gone through"),
        "{html}"
    );
    assert!(html.contains("value=\"maybe\""), "{html}");
    h.act(|| controls::message(MessageTo::New).send());
    h.settle().await;
    h.act(|| controls::message(MessageTo::New).confirm());
    let html = h.settle().await;
    assert!(html.contains("Sent to ada."), "{html}");
    let fake = portal.0.borrow();
    assert_eq!(fake.messages.len(), 3);
    assert!(
        fake.messages
            .iter()
            .all(|m| m.action_id == fake.messages[0].action_id),
        "one action id throughout"
    );
}

/// After a send with no known outcome, the tap call failing (a 5xx) or
/// refused (a code) never says nothing was sent: the earlier send may
/// have gone, and asking for a tap sends nothing
#[tokio::test]
async fn a_failed_tap_call_while_unsure_says_what_this_attempt_did() {
    for (failure, words) in [
        (
            ApiError::Status {
                status: 502,
                message: "bad gateway".into(),
                code: None,
            },
            "asking for a tap sends nothing",
        ),
        (
            ApiError::refused(
                403,
                ErrorResponse::coded(
                    RefusalCode::NotFriends,
                    "you can only message accepted friends",
                )
                .into(),
            ),
            "This attempt was refused, so it sent nothing: you can send a \
             private message only to a friend.",
        ),
    ] {
        let portal = with(vec![], ada());
        {
            let mut fake = portal.0.borrow_mut();
            fake.message_answers
                .push_back(Err(ApiError::Network("offline".into())));
            fake.tap_answers.push_back(Ok(PortalMessageTap {
                tap: Some(tap_options()),
            }));
            fake.tap_answers.push_back(Err(failure));
        }
        let (mut h, _) = Harness::new(&portal).await;
        new_message(&mut h, "unsure").await;
        h.act(|| controls::message(MessageTo::New).send());
        h.settle().await;
        h.act(|| controls::message(MessageTo::New).confirm());
        h.settle().await;
        h.act(|| controls::message(MessageTo::New).send());
        let html = h.settle().await;
        assert!(html.contains(words), "{words}: {html}");
        assert!(!html.contains("Nothing was sent"), "{html}");
        assert!(
            html.contains("Your earlier send may have gone through"),
            "{html}"
        );
        assert!(html.contains("value=\"unsure\""), "{html}");
        // Unsure still: the box offers no "nothing happened" reset
        let fake = portal.0.borrow();
        assert_eq!(fake.taps[0].action_id, fake.taps[1].action_id);
    }
}

/// A coded refusal of the send itself (not the tap call) keeps the draft
/// and its action id: a refusal renews nothing, so the next Send asks
/// under the same id
#[tokio::test]
async fn a_refused_send_keeps_the_draft_and_its_action_id() {
    let portal = with(vec![], ada());
    portal
        .0
        .borrow_mut()
        .message_answers
        .push_back(Err(ApiError::refused(
            403,
            ErrorResponse::coded(
                RefusalCode::NotFriends,
                "you can only message accepted friends",
            )
            .into(),
        )));
    let (mut h, _) = Harness::new(&portal).await;
    new_message(&mut h, "refused at the end").await;
    h.act(|| controls::message(MessageTo::New).send());
    h.settle().await;
    h.act(|| controls::message(MessageTo::New).confirm());
    let html = h.settle().await;
    assert!(
        html.contains("You can send a private message only to a friend."),
        "{html}"
    );
    assert!(html.contains("value=\"refused at the end\""), "{html}");
    assert!(html.contains(">Send</button>"), "{html}");
    h.act(|| controls::message(MessageTo::New).send());
    h.settle().await;
    let fake = portal.0.borrow();
    assert_eq!(fake.taps.len(), 2);
    assert_eq!(fake.taps[0].action_id, fake.taps[1].action_id);
    assert_eq!(fake.messages[0].action_id, fake.taps[1].action_id);
}

/// Another operator signing in clears every message draft, a pending tap
/// with it: nothing one operator wrote can be sent as another's
#[tokio::test]
async fn an_operator_switch_clears_message_drafts_and_taps() {
    let from_ada = fixtures::message(1, "ada", "hi", true);
    let portal = with(vec![from_ada.clone()], ada());
    let (mut h, _) = Harness::new(&portal).await;
    new_message(&mut h, "the first operator's").await;
    h.act(|| controls::message(MessageTo::New).send());
    let html = h.settle().await;
    assert!(html.contains("Tap your passkey to send"), "{html}");
    let html = another_operator(&mut h, &portal).await;
    assert!(!html.contains("Tap your passkey to send"), "{html}");
    assert!(!html.contains("the first operator's"), "{html}");
    assert!(!html.contains("draft waiting"), "{html}");
    assert_eq!(portal.0.borrow().unsent.last(), Some(&false));
}

/// The page's own signals are the session's: what was marked, a
/// friendship's answer and a name typed don't cross to another operator
#[tokio::test]
async fn page_signals_dont_cross_a_session_switch() {
    let from_ada = fixtures::message(1, "ada", "hi", true);
    let portal = with(vec![from_ada.clone()], ada());
    let (mut h, _) = Harness::new(&portal).await;
    h.act(|| page_controls().mark(vec![from_ada.id]));
    h.act(|| page_controls().friend(FriendshipAction::Request, name("dee")));
    h.settle().await;
    h.act(|| page_controls().name_typed("cyd".into()));
    let html = h.settle().await;
    assert!(html.contains("Marked 1 message read."), "{html}");
    assert!(html.contains("Friend request sent to dee."), "{html}");
    assert!(html.contains("value=\"cyd\""), "{html}");
    let html = another_operator(&mut h, &portal).await;
    assert!(html.contains("other-human"), "{html}");
    assert!(!html.contains("Marked 1 message read."), "{html}");
    assert!(!html.contains("Friend request sent to dee."), "{html}");
    assert!(!html.contains("value=\"cyd\""), "{html}");

    // A mark in flight at the switch answers into nothing of the new page
    let gate = portal.hold();
    h.act(|| page_controls().mark(vec![from_ada.id]));
    h.settle().await;
    another_operator_back(&mut h, &portal).await;
    drop(gate);
    let html = h.settle().await;
    assert!(!html.contains("Marked 1 message read."), "{html}");
}

/// The session back to the fixture's operator
async fn another_operator_back(h: &mut Harness, portal: &FakePortal) {
    portal.0.borrow_mut().session = Some(Ok(fixtures::portal_session(true)));
    let handle = h.act(consume_context::<SessionHandle>);
    h.act(|| handle.refresh());
    h.settle().await;
}

/// The friends list read again and failing: the list stays, and the page
/// says it may be out of date and why
#[tokio::test]
async fn a_failed_friends_refresh_says_the_list_may_be_stale() {
    let portal = with(vec![], ada());
    portal
        .0
        .borrow_mut()
        .friends_answers
        .push_back(Err(ApiError::Network("offline".into())));
    let (mut h, html) = Harness::new(&portal).await;
    assert!(!html.contains("may be out of date"), "{html}");
    h.act(|| page_controls().refresh());
    let html = h.settle().await;
    assert!(
        html.contains(
            "Couldn't refresh the friends list: Agora couldn't load it ("
        ),
        "{html}"
    );
    assert!(
        html.contains("The list shown may be out of date."),
        "{html}"
    );
    assert!(html.contains(">ada</a>"), "the list stays: {html}");
}

/// The new-message box says what the friends list is: loading, failed
/// (with a way to try again), or truly empty; and a draft that has a
/// recipient always shows whom it goes to
#[tokio::test]
async fn the_new_message_box_says_what_the_friends_list_is() {
    // Failed: said, with Try again, never "no friends"
    let portal = FakePortal::signed_in(true);
    portal
        .0
        .borrow_mut()
        .friends_answers
        .push_back(Err(ApiError::Network("offline".into())));
    let (mut h, _) = Harness::new(&portal).await;
    h.act(|| controls::message(MessageTo::New).open());
    let html = h.settle().await;
    assert!(
        html.contains("Your friends list couldn't be loaded."),
        "{html}"
    );
    assert!(!html.contains("You have no friends"), "{html}");
    assert!(html.contains(">Try again</button>"), "{html}");
    // A recipient chosen before (the draft's) is shown even so
    h.act(|| controls::message(MessageTo::New).choose("ada"));
    let html = h.settle().await;
    let (_, to) = new_box_fields(&html);
    assert!(to.contains("select"), "{to}");
    assert!(html.contains("<option value=\"ada\" selected"), "{html}");

    // Truly empty: said so
    let portal = with(vec![], vec![]);
    let (mut h, _) = Harness::new(&portal).await;
    h.act(|| controls::message(MessageTo::New).open());
    let html = h.settle().await;
    assert!(
        html.contains("You have no friends to write to yet."),
        "{html}"
    );

    // A chosen recipient no longer on the list: still shown, and said
    let portal = with(vec![], ada());
    let (mut h, _) = Harness::new(&portal).await;
    new_message(&mut h, "to ada").await;
    portal.0.borrow_mut().friends_answers.clear();
    portal
        .0
        .borrow_mut()
        .friends_answers
        .push_back(Ok(FriendsResponse::default()));
    h.act(|| page_controls().refresh());
    let html = h.settle().await;
    assert!(html.contains("<option value=\"ada\" selected"), "{html}");
    assert!(
        html.contains("ada isn't in your friends list as last read"),
        "{html}"
    );
}

/// Loading, the box says the list is loading, not that there are no
/// friends
#[tokio::test]
async fn a_loading_friends_list_is_not_an_empty_one() {
    let portal = FakePortal::signed_in(true);
    let (mut h, _) = Harness::new(&portal).await;
    // The list being read again from nothing: drawn before the answer
    let store = h.act(consume_context::<InboxStore>);
    h.act(|| put(store.friends, Load::Loading));
    h.act(|| controls::message(MessageTo::New).open());
    h.dom
        .render_immediate(&mut dioxus::dioxus_core::NoOpMutations);
    let html = dioxus_ssr::render(&h.dom).replace("&#39;", "'");
    assert!(html.contains("Loading your friends…"), "{html}");
    assert!(!html.contains("You have no friends"), "{html}");
}

/// The session line's link alone reads the inbox, not the friends list,
/// and says quietly when the count couldn't be read
#[tokio::test]
async fn the_link_reads_only_the_inbox_and_says_when_it_cant() {
    #[allow(non_snake_case)]
    fn LinkOnly() -> Element {
        rsx! { ClientOnly { slot: ClientSlot::Session } }
    }
    let portal = FakePortal::signed_in(true);
    portal
        .0
        .borrow_mut()
        .inbox_answers
        .push_back(Err(ApiError::Network("offline".into())));
    let mut dom = VirtualDom::new(LinkOnly);
    dom.provide_root_context(
        ClientSlots::new().with(ClientSlot::Session, InboxLink),
    );
    session::provide(&dom, portal.clone());
    dom.rebuild_in_place();
    let html = settle(&mut dom).await;
    assert!(html.contains("Inbox · count unavailable"), "{html}");
    let fake = portal.0.borrow();
    assert_eq!((fake.inbox_calls, fake.friends_calls), (1, 0));
}

/// A send whose answer never came says it may have been sent (a message
/// is sent, not posted), and anything else the server said on a send is
/// still shown
#[tokio::test]
async fn a_sent_message_shows_what_the_server_said_besides() {
    let portal = with(vec![], ada());
    portal.0.borrow_mut().message_answers.push_back(Ok(
        crate::responses::SendMessageResponse {
            notice: Some("ada asked the Council to stop.".into()),
            warning: Some("A cap was reached.".into()),
            id: crate::ui::test_portal::sent_message(),
            encryption: crate::enums::MessageEncryption::Server,
            sent_at: Some(fixtures::at()),
        },
    ));
    let (mut h, _) = Harness::new(&portal).await;
    new_message(&mut h, "hello").await;
    h.act(|| controls::message(MessageTo::New).send());
    h.settle().await;
    h.act(|| controls::message(MessageTo::New).confirm());
    let html = h.settle().await;
    assert!(html.contains("Sent to ada."), "{html}");
    assert!(html.contains("ada asked the Council to stop."), "{html}");
    assert!(html.contains("A cap was reached."), "{html}");
}

// -- The check window (W3) ------------------------------------------------

/// The window ends while a reply is being written (the page's probe at
/// the window's end finds it ended): the inbox shows what a signed-out
/// visitor sees, plus the tap in place, and nothing it had read; the reply
/// and a friend's name typed are kept. After the tap the inbox is read
/// again, the reply box opens as it was, with its text, and sends
#[tokio::test]
async fn a_reply_being_written_survives_the_windows_end() {
    let from_ada = fixtures::message(1, "ada", "a private hello", true);
    let portal = with(vec![from_ada.clone()], ada());
    let (mut h, _) = Harness::new(&portal).await;
    let to = MessageTo::Reply(from_ada.id);
    h.act(|| controls::message(to).open());
    h.settle().await;
    h.act(|| controls::message(to).input("a reply in progress"));
    h.act(|| page_controls().name_typed("dee".into()));
    let html = h.settle().await;
    assert!(html.contains("a reply in progress"), "{html}");
    let reads = portal.0.borrow().inbox_calls;

    portal.0.borrow_mut().session =
        Some(Err(crate::ui::test_portal::check_needed()));
    portal.fire(0);
    let html = h.settle().await;
    assert!(html.contains(INBOX_CHECK_NEEDED), "{html}");
    assert!(html.contains(">Confirm with a passkey</button>"), "{html}");
    for private in ["a private hello", "ui-inbox-list", "dee"] {
        assert!(!html.contains(private), "{private}: {html}");
    }
    // The operator's own text stays reachable, read-only, in the list of
    // drafts: its box is on this page, hidden, so no link to this page
    assert_eq!(html.matches("a reply in progress").count(), 1, "{html}");
    assert!(html.contains("readonly=true"), "{html}");
    assert!(
        html.contains("Its box is on this page, hidden until you confirm with your passkey."),
        "{html}"
    );
    assert!(!html.contains("Open its page"), "{html}");
    assert!(!html.contains("Sign in"), "{html}");
    assert!(!html.contains("login"), "{html}");

    let handle = h.act(consume_context::<session::SessionHandle>);
    h.act(|| handle.start_window_tap());
    h.settle().await;
    h.act(|| handle.confirm_window_tap());
    let html = h.settle().await;
    assert!(
        portal.0.borrow().inbox_calls > reads,
        "read again after the tap"
    );
    assert!(html.contains("a private hello"), "{html}");
    assert!(
        html.contains("a reply in progress"),
        "the box open as it was: {html}"
    );
    assert!(html.contains(r#"value="dee""#), "the name kept: {html}");
    h.act(|| controls::message(to).send());
    h.settle().await;
    h.act(|| controls::message(to).confirm());
    let html = h.settle().await;
    assert!(html.contains("Sent to ada."), "{html}");
    assert_eq!(
        portal.0.borrow().messages[0].payload.body,
        "a reply in progress"
    );
}

/// A message whose tap call is refused because the window ended keeps its
/// draft; the tap in place opens a window, and Send then asks for the
/// message's own tap as before
#[tokio::test]
async fn a_message_refused_check_needed_keeps_its_draft() {
    let portal = with(vec![], ada());
    portal
        .0
        .borrow_mut()
        .tap_answers
        .push_back(Err(crate::ui::test_portal::check_needed()));
    let (mut h, _) = Harness::new(&portal).await;
    new_message(&mut h, "kept for later").await;
    h.act(|| controls::message(MessageTo::New).send());
    let html = h.settle().await;
    assert!(html.contains(INBOX_CHECK_NEEDED), "{html}");
    let handle = h.act(consume_context::<session::SessionHandle>);
    h.act(|| handle.start_window_tap());
    h.settle().await;
    h.act(|| handle.confirm_window_tap());
    let html = h.settle().await.replace("&#39;", "'");
    assert!(html.contains("kept for later"), "{html}");
    assert!(
        html.contains(
            "It wasn't sent, as Agora needed a recent passkey check then"
        ),
        "{html}"
    );
    h.act(|| controls::message(MessageTo::New).send());
    let html = h.settle().await;
    assert!(html.contains("Tap your passkey to send"), "{html}");
}

/// The tap in place, with the server outside a window until it
async fn tap_in_place(h: &mut Harness, portal: &FakePortal) -> String {
    portal.0.borrow_mut().session =
        Some(Err(crate::ui::test_portal::check_needed()));
    let handle = h.act(consume_context::<session::SessionHandle>);
    h.act(|| handle.start_window_tap());
    h.settle().await;
    h.act(|| handle.confirm_window_tap());
    h.settle().await
}

/// A mark and a friendship action refused because the window ended keep
/// their notice through the tap: it wasn't done, and to press it again
#[tokio::test]
async fn a_refused_mark_or_friendship_says_so_after_the_tap() {
    let from_ada = fixtures::message(1, "ada", "one", true);
    let portal = with(vec![from_ada.clone()], ada());
    {
        let mut fake = portal.0.borrow_mut();
        fake.mark_answers
            .push_back(Err(crate::ui::test_portal::check_needed()));
    }
    let (mut h, _) = Harness::new(&portal).await;
    h.act(|| page_controls().mark(vec![from_ada.id]));
    let html = h.settle().await;
    assert!(html.contains(INBOX_CHECK_NEEDED), "{html}");
    let html = tap_in_place(&mut h, &portal).await;
    assert!(
        html.contains("Not marked: Agora needed a recent passkey check first."),
        "{html}"
    );
    assert!(html.contains("press it again"), "{html}");

    portal
        .0
        .borrow_mut()
        .friendship_answers
        .push_back(Err(crate::ui::test_portal::check_needed()));
    h.act(|| page_controls().friend(FriendshipAction::Request, name("dee")));
    h.settle().await;
    let html = tap_in_place(&mut h, &portal).await;
    assert!(
        html.contains("Not done: Agora needed a recent passkey check first."),
        "{html}"
    );
}

/// A message's 0x13 tap asked for before the window ended is kept through
/// the lapse: after the window's tap, "Tap your passkey to send" is still
/// there, and sends
#[tokio::test]
async fn a_pending_message_tap_survives_the_lapse() {
    let portal = with(vec![], ada());
    let (mut h, _) = Harness::new(&portal).await;
    new_message(&mut h, "hello ada").await;
    h.act(|| controls::message(MessageTo::New).send());
    let html = h.settle().await;
    assert!(html.contains("Tap your passkey to send"), "{html}");
    portal.0.borrow_mut().session =
        Some(Err(crate::ui::test_portal::check_needed()));
    portal.fire_last();
    let html = h.settle().await;
    assert!(html.contains(INBOX_CHECK_NEEDED), "{html}");
    let html = tap_in_place(&mut h, &portal).await;
    assert!(html.contains("Tap your passkey to send"), "{html}");
    assert!(html.contains("hello ada"), "{html}");
    h.act(|| controls::message(MessageTo::New).confirm());
    let html = h.settle().await;
    assert!(html.contains("Sent to ada."), "{html}");
    let fake = portal.0.borrow();
    assert_eq!(fake.messages.len(), 1);
    assert_eq!(fake.messages[0].payload.body, "hello ada");
    assert!(fake.messages[0].assertion.is_some());
}

/// A probe that can't reach Agora for long enough takes the messages off
/// the page (never kept up on a guess); the drafts stay, and the first
/// answer brings the inbox back, read again
#[tokio::test]
async fn an_unreachable_probe_hides_the_inbox() {
    let from_ada = fixtures::message(1, "ada", "a private hello", true);
    let portal = with(vec![from_ada], ada());
    let (mut h, html) = Harness::new(&portal).await;
    assert!(html.contains("a private hello"), "{html}");
    let start = fixtures::portal_session(true).window.ends_at();
    portal.0.borrow_mut().now = Some(start);
    portal.0.borrow_mut().session =
        Some(Err(ApiError::Network("asleep".into())));
    portal.fire_last();
    let html = h.settle().await;
    assert!(html.contains("a private hello"), "a moment's grace: {html}");
    portal.0.borrow_mut().now = Some(start + session::PROBE_UNKNOWN_FOR);
    portal.fire_last();
    let html = h.settle().await;
    assert!(!html.contains("a private hello"), "{html}");
    assert!(
        html.contains("Agora couldn't be reached (asleep)"),
        "{html}"
    );
    let reads = portal.0.borrow().inbox_calls;
    portal.0.borrow_mut().session = Some(Ok(fixtures::portal_session(true)));
    portal.fire_last();
    let html = h.settle().await;
    assert!(html.contains("a private hello"), "{html}");
    assert!(portal.0.borrow().inbox_calls > reads, "read again");
}

/// Woken hours after the window's end, offline: the private messages go at
/// once (the page slept past its probe), before any answer, and the page
/// says Agora couldn't be reached
#[tokio::test]
async fn woken_long_after_the_end_hides_at_once() {
    let from_ada = fixtures::message(1, "ada", "a private hello", true);
    let portal = with(vec![from_ada], ada());
    let (mut h, html) = Harness::new(&portal).await;
    assert!(html.contains("a private hello"), "{html}");
    let end = fixtures::portal_session(true).window.ends_at();
    portal.0.borrow_mut().now = Some(end + chrono::TimeDelta::hours(3));
    portal.0.borrow_mut().session =
        Some(Err(ApiError::Network("asleep".into())));
    portal.fire_last();
    let html = h.settle().await;
    assert!(!html.contains("a private hello"), "{html}");
    assert!(html.contains(INBOX_UNREACHABLE), "{html}");
}

/// The heartbeat finds a sleep no timer reported: hidden, then asked
#[tokio::test]
async fn the_heartbeat_finds_a_sleep() {
    let from_ada = fixtures::message(1, "ada", "a private hello", true);
    let portal = with(vec![from_ada], ada());
    let (mut h, _) = Harness::new(&portal).await;
    let end = fixtures::portal_session(true).window.ends_at();
    portal.0.borrow_mut().now = Some(end + chrono::TimeDelta::hours(3));
    portal.0.borrow_mut().session =
        Some(Err(crate::ui::test_portal::check_needed()));
    let asked = portal.0.borrow().status_calls;
    // The answer held: hidden before it comes
    let gate = portal.hold();
    portal.beat();
    let html = h.settle().await;
    assert!(!html.contains("a private hello"), "hidden at once: {html}");
    assert!(
        portal.0.borrow().status_calls > asked,
        "asked after the beat"
    );
    drop(gate);
    let html = h.settle().await;
    assert!(!html.contains("a private hello"), "{html}");
    assert!(html.contains(INBOX_CHECK_NEEDED), "{html}");
}

/// A slow answer to a probe that fired on time is not a sleep: a heartbeat
/// while it is awaited leaves the inbox shown
#[tokio::test]
async fn a_slow_answer_is_not_a_sleep() {
    let from_ada = fixtures::message(1, "ada", "a private hello", true);
    let portal = with(vec![from_ada], ada());
    let (mut h, _) = Harness::new(&portal).await;
    let window = fixtures::portal_session(true).window;
    let due =
        window.now + (window.ends_at() - window.now) + session::PROBE_AFTER_END;
    portal.0.borrow_mut().now = Some(due);
    let gate = portal.hold();
    portal.fire_last();
    h.settle().await;
    portal.0.borrow_mut().now = Some(due + chrono::TimeDelta::seconds(25));
    portal.beat();
    let html = h.settle().await;
    assert!(html.contains("a private hello"), "still shown: {html}");
    drop(gate);
    let html = h.settle().await;
    assert!(html.contains("a private hello"), "{html}");
}

/// A page that couldn't reach Agora comes back when it answers, even when
/// the status answers before the bootstrap does, and when "Try again" is
/// pressed while a retry is in flight
#[tokio::test]
async fn an_unreachable_page_comes_back_when_agora_answers() {
    let from_ada = fixtures::message(1, "ada", "a private hello", true);
    let portal = with(vec![from_ada], ada());
    let (mut h, _) = Harness::new(&portal).await;
    let end = fixtures::portal_session(true).window.ends_at();
    portal.0.borrow_mut().now = Some(end + chrono::TimeDelta::hours(3));
    portal.0.borrow_mut().session =
        Some(Err(ApiError::Network("asleep".into())));
    portal.fire_last();
    h.settle().await;
    // The status is back, the bootstrap not yet: still asking
    portal
        .0
        .borrow_mut()
        .status_answers
        .push_back(Ok(fixtures::portal_session(true).window));
    let waits = portal.0.borrow().waits.len();
    portal.fire_last();
    let html = h.settle().await;
    assert!(html.contains(INBOX_UNREACHABLE), "{html}");
    assert!(portal.0.borrow().waits.len() > waits, "a next question set");
    // "Try again" while that retry waits: still failing, still asking
    let handle = h.act(consume_context::<session::SessionHandle>);
    h.act(|| handle.refresh());
    h.settle().await;
    // Agora answers
    portal.0.borrow_mut().session = Some(Ok(fixtures::portal_session(true)));
    portal.fire_last();
    let html = h.settle().await;
    assert!(html.contains("a private hello"), "{html}");
}

/// A connection that can't hold a window: the inbox, the session line and
/// a compose box say why and what will bring it back, and none asks for a
/// confirmation it can't give
#[tokio::test]
async fn an_unusable_address_never_asks_to_confirm() {
    let portal = with(vec![], ada());
    portal.0.borrow_mut().session = Some(Err(ApiError::refused(
        403,
        crate::errors::CheckNeeded::new(
            "x",
            crate::operator::NoWindow::AddressUnusable,
        )
        .into(),
    )));
    let (_h, html) = Harness::new(&portal).await;
    let html = html.replace("&#39;", "'");
    assert!(html.contains("public address"), "{html}");
    assert!(!html.to_lowercase().contains("confirm"), "{html}");
    assert!(html.contains("Your drafts stay in this tab"), "{html}");
}
