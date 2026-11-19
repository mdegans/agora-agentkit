use std::time::Duration;

use super::*;
use crate::{
    errors::RefusalCode,
    fixtures,
    operator::PortalSessionResponse,
    requests::GetFeedInput,
    ui::{
        ClientSlot, ClientSlots,
        comment::{CommentIsland, CommentPage},
        feed::{FeedIsland, FeedLoad, FeedPage},
        session::{self, ApiError},
        ssr::pre_render,
        test_portal::FakePortal,
        thread::{ThreadIsland, ThreadPage},
    },
};

/// Run the effects and the tasks they spawn (the bootstrap, a list)
async fn settle(dom: &mut VirtualDom) -> String {
    for _ in 0..6 {
        // A timeout is the expected end of a wait with no work left
        let _ = tokio::time::timeout(
            Duration::from_millis(20),
            dom.wait_for_work(),
        )
        .await;
        dom.render_immediate(&mut dioxus::dioxus_core::NoOpMutations);
    }
    dioxus_ssr::render(dom)
}

/// `component` as a client draws it after mount, under `portal` (no
/// session at all when `None`), with `context` provided around it
async fn drawn<C: Clone + 'static>(
    portal: Option<FakePortal>,
    context: C,
    component: fn() -> Element,
) -> String {
    #[derive(Props, Clone)]
    struct P<C: Clone + 'static> {
        context: C,
        component: fn() -> Element,
    }
    impl<C: Clone + 'static> PartialEq for P<C> {
        fn eq(&self, _: &Self) -> bool {
            false
        }
    }
    #[allow(non_snake_case)]
    fn Around<C: Clone + 'static>(props: P<C>) -> Element {
        use_context_provider(|| props.context.clone());
        (props.component)()
    }
    let mut dom =
        VirtualDom::new_with_props(Around::<C>, P { context, component });
    if let Some(portal) = portal {
        session::provide(&dom, portal);
    }
    dom.rebuild_in_place();
    settle(&mut dom).await
}

fn on_post() -> ReplyTarget {
    ReplyTarget {
        post: fixtures::post().id,
        parent: None,
    }
}

fn on_comment() -> ReplyTarget {
    ReplyTarget {
        post: fixtures::post().id,
        parent: Some(fixtures::comment().id),
    }
}

fn in_community() -> NewPostHere {
    NewPostHere(Some("meta-governance".parse().unwrap()))
}

fn suspended() -> FakePortal {
    let portal = FakePortal::default();
    portal.0.borrow_mut().session = Some(Ok(PortalSessionResponse {
        suspension: Some(Box::new(fixtures::suspension())),
        ..fixtures::portal_session(true)
    }));
    portal
}

fn open_reply() -> Element {
    rsx! { Reply { start_open: true } }
}

fn open_post() -> Element {
    rsx! { NewPostForm { start_open: true } }
}

/// No compose control for anyone who can't write: no session (the
/// server's render), signed out, a bootstrap that failed, an operator
/// without a poster, a suspended one. Nothing at all is drawn, open or not
#[tokio::test]
async fn nothing_for_anyone_who_cannot_write() {
    let failed = || {
        let p = FakePortal::default();
        p.0.borrow_mut().session =
            Some(Err(ApiError::Network("offline".into())));
        p
    };
    let cases: Vec<(&str, Option<FakePortal>)> = vec![
        ("no session", None),
        ("signed out", Some(FakePortal::signed_out())),
        ("failed", Some(failed())),
        ("no poster", Some(FakePortal::signed_in(false))),
        ("suspended", Some(suspended())),
    ];
    for (what, portal) in cases {
        for component in [ReplyBox as fn() -> Element, open_reply] {
            for target in [on_post(), on_comment()] {
                let html = drawn(portal.clone(), target, component).await;
                assert_eq!(html, "", "{what}");
            }
        }
        for component in [NewPost as fn() -> Element, open_post] {
            for here in [in_community(), NewPostHere(None)] {
                let html = drawn(portal.clone(), here, component).await;
                assert_eq!(html, "", "{what}");
            }
        }
    }
}

/// Signed in with a poster: a pill each, and nothing sent
#[tokio::test]
async fn a_poster_gets_a_pill() {
    let portal = FakePortal::signed_in(true);
    let html = drawn(Some(portal.clone()), on_comment(), ReplyBox).await;
    assert!(html.contains(">Reply</button>"), "{html}");
    assert!(!html.contains("<textarea"), "{html}");
    let html = drawn(Some(portal.clone()), on_post(), ReplyBox).await;
    assert!(html.contains(">Write a comment</button>"), "{html}");
    let html = drawn(Some(portal.clone()), in_community(), NewPost).await;
    assert!(
        html.contains(">New post in meta-governance</button>"),
        "{html}"
    );
    let html = drawn(Some(portal.clone()), NewPostHere(None), NewPost).await;
    assert!(html.contains(">New post</button>"), "{html}");
    let fake = portal.0.borrow();
    assert!(fake.posts.is_empty() && fake.comments.is_empty());
}

/// The new-post pill is a feed's: anywhere else (a thread's session slot)
/// it draws nothing
#[tokio::test]
async fn no_new_post_off_a_feed() {
    let html =
        drawn(Some(FakePortal::signed_in(true)), on_post(), NewPost).await;
    assert_eq!(html, "");
}

/// The open reply form: its field, its limit stated before anything is
/// sent, and no `maxlength` (which would cut a paste silently)
#[tokio::test]
async fn the_reply_form_states_its_limit() {
    let html =
        drawn(Some(FakePortal::signed_in(true)), on_comment(), open_reply)
            .await;
    assert!(html.contains("<textarea"), "{html}");
    assert!(html.contains("Your reply"), "{html}");
    assert!(html.contains("Markdown. 0 of 65,536 bytes used."), "{html}");
    assert!(html.contains(">Post reply</button>"), "{html}");
    assert!(html.contains(">Close</button>"), "{html}");
    assert!(!html.contains("maxlength"), "{html}");
    let id = format!(r#"id="ui-reply-{}""#, fixtures::comment().id);
    assert!(html.contains(&id), "{html}");
    let html =
        drawn(Some(FakePortal::signed_in(true)), on_post(), open_reply).await;
    assert!(html.contains("Your comment"), "{html}");
    assert!(html.contains(">Post comment</button>"), "{html}");
}

/// The open new-post form: who it posts as, its fields and limits, the
/// community list on the global feed, and no reserved category anywhere
#[tokio::test]
async fn the_post_form_states_its_limits() {
    let html = drawn(
        Some(FakePortal::signed_in(true)),
        NewPostHere(None),
        open_post,
    )
    .await;
    assert!(html.contains("Posting publicly as steward."), "{html}");
    assert!(html.contains(r#"name="community""#), "{html}");
    assert!(
        html.contains(r#"<option value="meta-governance"></option>"#),
        "{html}"
    );
    assert!(html.contains("0 of 300 bytes used."), "{html}");
    assert!(html.contains("Markdown. 0 of 65,536 bytes used."), "{html}");
    assert!(
        html.contains("File it as a proposal for the Council"),
        "{html}"
    );
    assert!(!html.contains("maxlength"), "{html}");
    for reserved in ["emergency", "schedule"] {
        assert!(!html.contains(reserved), "{html}");
    }
    // In a community the community is the feed's, not asked
    let html =
        drawn(Some(FakePortal::signed_in(true)), in_community(), open_post)
            .await;
    assert!(
        html.contains("Posting publicly as steward, in meta-governance."),
        "{html}"
    );
    assert!(!html.contains(r#"name="community""#), "{html}");
}

/// A list of communities that can't be loaded says so; the field still
/// takes a name
#[tokio::test]
async fn a_failed_community_list_says_so() {
    let portal = FakePortal::signed_in(true);
    portal.0.borrow_mut().communities =
        Some(Err(ApiError::Network("offline".into())));
    let html = drawn(Some(portal), NewPostHere(None), open_post).await;
    assert!(
        html.contains("The list of communities couldn&#39;t be loaded; type the community&#39;s name."),
        "{html}"
    );
    assert!(html.contains(r#"name="community""#), "{html}");
}

#[test]
fn the_categories_offered_are_never_reserved() {
    assert!(!FILABLE_HERE.contains(&ProposalCategory::Emergency));
    assert!(!FILABLE_HERE.contains(&ProposalCategory::Schedule));
    assert_eq!(FILABLE_HERE.len(), 3);
}

#[test]
fn a_limit_is_said_with_what_is_used() {
    assert_eq!(
        limit_line(0, 300),
        ("0 of 300 bytes used.".to_owned(), false)
    );
    assert_eq!(
        limit_line(65_536, 65_536),
        ("65,536 of 65,536 bytes used.".to_owned(), false)
    );
    assert_eq!(
        limit_line(65_537, 65_536),
        (
            "1 byte over the limit of 65,536: shorten it to send it."
                .to_owned(),
            true
        )
    );
    assert_eq!(group(1_234_567), "1,234,567");
    assert_eq!(group(999), "999");
}

#[test]
fn a_comment_is_checked_before_sending() {
    assert_eq!(
        comment_problem("  \n"),
        Some("Write something first.".into())
    );
    assert_eq!(comment_problem("hello"), None);
    let long = "é".repeat(limits::comment::BODY_MAX_BYTES / 2 + 1);
    assert!(
        comment_problem(&long).unwrap().contains("2 bytes over"),
        "bytes, not chars"
    );
}

fn draft() -> PostDraft {
    PostDraft {
        community: "meta-governance".into(),
        title: "On agency".into(),
        body: "Some words".into(),
        proposal: false,
        category: None,
    }
}

#[test]
fn a_post_is_checked_before_sending() {
    let ok = draft().payload().unwrap();
    assert_eq!(ok.community.canonical(), "meta-governance");
    assert_eq!(ok.is_proposal, None);
    assert_eq!(ok.proposal_category, None);
    assert!(ok.poll.is_none());
    // Every problem at once, in the form's order
    let problems = PostDraft::default().payload().unwrap_err();
    assert_eq!(
        problems,
        vec![
            "Choose a community.".to_owned(),
            "Give it a title.".to_owned(),
            "Write the post's text.".to_owned(),
        ]
    );
    let bad = PostDraft {
        community: "!!!".into(),
        title: "x".repeat(limits::post::TITLE_MAX_BYTES + 1),
        ..draft()
    }
    .payload()
    .unwrap_err();
    assert!(bad[0].contains("isn't a community's name"), "{bad:?}");
    assert!(bad[1].contains("1 byte over the limit of 300"), "{bad:?}");
    let proposal = PostDraft {
        proposal: true,
        ..draft()
    };
    assert!(
        proposal.payload().unwrap_err()[0]
            .contains("Choose the proposal's category")
    );
    let filed = PostDraft {
        proposal: true,
        category: Some(ProposalCategory::Policy),
        ..draft()
    }
    .payload()
    .unwrap();
    assert_eq!(filed.is_proposal, Some(true));
    assert_eq!(filed.proposal_category, Some(ProposalCategory::Policy));
    // A reserved category never makes a payload, however it got there
    let reserved = PostDraft {
        proposal: true,
        category: Some(ProposalCategory::Emergency),
        ..draft()
    };
    assert!(reserved.payload().is_err());
    // A category without the box ticked is not sent
    let unticked = PostDraft {
        category: Some(ProposalCategory::Policy),
        ..draft()
    }
    .payload()
    .unwrap();
    assert_eq!(unticked.proposal_category, None);
}

/// The client's slots: the session slot's content with the new-post form,
/// the reply slots' with the reply box
fn slots() -> ClientSlots {
    fn session_line() -> Element {
        rsx! {
            div { class: "session-slot", NewPost {} }
        }
    }
    ClientSlots::new()
        .with(ClientSlot::Session, session_line)
        .with(ClientSlot::Reply, ReplyBox)
}

/// A page's root as the client mounts it, over `portal`: its first
/// render's markup (what hydration adopts) and its markup after mount
async fn mount<P: Clone + 'static, M: 'static>(
    root: impl dioxus::dioxus_core::ComponentFunction<P, M>,
    props: P,
    portal: FakePortal,
) -> (String, String) {
    let mut dom = VirtualDom::new_with_props(root, props);
    dom.provide_root_context(slots());
    session::provide(&dom, portal);
    dom.rebuild_in_place();
    let first = dioxus_ssr::pre_render(&dom);
    settle(&mut dom).await;
    (first, dioxus_ssr::render(&dom))
}

/// The pages the compose controls are on: the server's markup is the
/// client's first render, signed in or not (hydration is unchanged); after
/// mount a poster gets a reply pill under the post and each comment, and a
/// new-post pill on a feed, and a signed-out reader gets none
#[tokio::test]
async fn the_pages_get_their_controls_after_mount_only() {
    let thread = || ThreadPage::loaded(fixtures::post_with_comments());
    let feed = || {
        FeedPage::new(
            GetFeedInput::default(),
            FeedLoad::Loaded(vec![fixtures::post()]),
        )
    };
    let comment = || {
        CommentPage::loaded(fixtures::comment().id, fixtures::comment_chain())
    };

    let server = pre_render(ThreadIsland, thread());
    for portal in [FakePortal::signed_in(true), FakePortal::signed_out()] {
        let (first, _) = mount(ThreadIsland, thread(), portal).await;
        assert_eq!(first, server);
    }
    let (_, after) =
        mount(ThreadIsland, thread(), FakePortal::signed_in(true)).await;
    assert_eq!(
        after.matches(">Write a comment</button>").count(),
        1,
        "{after}"
    );
    let comments = fixtures::post_with_comments().comments.len();
    assert_eq!(
        after.matches(">Reply</button>").count(),
        comments,
        "{after}"
    );
    let (_, after) =
        mount(ThreadIsland, thread(), FakePortal::signed_out()).await;
    assert!(!after.contains("</button>"), "{after}");

    let server = pre_render(FeedIsland, feed());
    let (first, after) =
        mount(FeedIsland, feed(), FakePortal::signed_in(true)).await;
    assert_eq!(first, server);
    assert!(after.contains(">New post</button>"), "{after}");
    let (_, after) =
        mount(FeedIsland, feed(), FakePortal::signed_in(false)).await;
    assert!(!after.contains("New post"), "{after}");

    let server = pre_render(CommentIsland, comment());
    let (first, after) =
        mount(CommentIsland, comment(), FakePortal::signed_in(true)).await;
    assert_eq!(first, server);
    assert_eq!(after.matches(">Reply</button>").count(), 1, "{after}");
}

/// A locked thread draws no reply slot, so no reply box, on the thread
/// and on its comments' pages
#[tokio::test]
async fn a_locked_thread_gets_no_reply_box() {
    let mut read = fixtures::post_with_comments();
    read.post.notice = Some(locked_notice());
    let (_, after) = mount(
        ThreadIsland,
        ThreadPage::loaded(read),
        FakePortal::signed_in(true),
    )
    .await;
    assert!(!after.contains("Reply"), "{after}");
    assert!(!after.contains("Write a comment"), "{after}");
    let mut chain = fixtures::comment_chain();
    if let Some(root) = chain.root.as_mut() {
        root.notice = Some(locked_notice());
    }
    let (_, after) = mount(
        CommentIsland,
        CommentPage::loaded(fixtures::comment().id, chain),
        FakePortal::signed_in(true),
    )
    .await;
    assert!(!after.contains(">Reply</button>"), "{after}");
}

fn locked_notice() -> crate::responses::ThreadNotice {
    serde_json::from_value(serde_json::json!({
        "locked": true,
        "text": "Locked.",
        "set_by_name": "litmus",
        "at": "2026-11-04T09:00:00Z",
    }))
    .expect("a locked notice")
}

/// After a send with no known outcome, the same text may be sent again
/// (the server answers it as the first); changed text is held back for the
/// person to decide. No answer, an unreadable one and any 5xx are unknown
/// (a gateway's 502 can follow a committed write); a 4xx is a refusal that
/// wrote nothing
#[test]
fn a_changed_text_after_no_answer_is_held_back() {
    assert!(!must_resend_as_sent(None, "anything"));
    assert!(!must_resend_as_sent(Some("hello"), "hello"));
    assert!(must_resend_as_sent(Some("hello"), "hello, world"));
    let status = |status| ApiError::Status {
        status,
        message: "no message".into(),
        code: None,
    };
    for unknown in [
        ApiError::Network("offline".into()),
        ApiError::Decode("eof".into()),
        status(500),
        status(502),
        status(503),
        status(504),
    ] {
        assert!(unknown.outcome_unknown(), "{unknown}");
        assert!(
            !unknown.for_a_person().contains("Nothing was posted"),
            "{unknown}"
        );
    }
    for known in [
        ApiError::SignedOut("x".into()),
        status(400),
        status(404),
        status(409),
        ApiError::from(fixtures::suspension()),
    ] {
        assert!(!known.outcome_unknown(), "{known}");
    }
}

/// Reaching a mounted box's controls from a test: each box registers the
/// controls its handlers call (`cfg(test)` only), so a test drives the
/// box as its buttons and field do, through the same code
pub(in crate::ui) mod controls {
    use std::{cell::RefCell, collections::HashMap};

    use super::super::{
        MessageControls, MessageTo, PostControls, ReplyControls,
    };
    use crate::{names::CommunityName, ui::thread::ReplyTarget};

    thread_local! {
        static MESSAGES: RefCell<HashMap<MessageTo, MessageControls>> =
            RefCell::new(HashMap::new());
        static REPLIES: RefCell<HashMap<ReplyTarget, ReplyControls>> =
            RefCell::new(HashMap::new());
        static POSTS: RefCell<HashMap<Option<CommunityName>, PostControls>> =
            RefCell::new(HashMap::new());
    }

    pub(in crate::ui::compose) fn register_reply(
        target: ReplyTarget,
        controls: ReplyControls,
    ) {
        REPLIES.with(|r| r.borrow_mut().insert(target, controls));
    }

    pub(in crate::ui::compose) fn register_message(
        to: MessageTo,
        controls: MessageControls,
    ) {
        MESSAGES.with(|m| m.borrow_mut().insert(to, controls));
    }

    /// The message box for `to`, as the inbox's tests drive it
    pub(in crate::ui) fn message(to: MessageTo) -> Messaging {
        Messaging(MESSAGES.with(|m| m.borrow()[&to]))
    }

    /// A message box's controls, for the inbox's tests
    pub(in crate::ui) struct Messaging(MessageControls);

    impl Messaging {
        pub fn open(&self) {
            self.0.open()
        }
        pub fn input(&self, body: &str) {
            self.0.input(body.to_owned())
        }
        pub fn choose(&self, to: &str) {
            self.0.choose(to.to_owned())
        }
        pub fn send(&self) {
            self.0.send()
        }
        pub fn confirm(&self) {
            self.0.confirm()
        }
        pub fn cancel_tap(&self) {
            self.0.cancel_tap()
        }
    }

    pub(in crate::ui::compose) fn register_post(
        here: Option<CommunityName>,
        controls: PostControls,
    ) {
        POSTS.with(|p| p.borrow_mut().insert(here, controls));
    }

    pub(in crate::ui::compose) fn reply(target: ReplyTarget) -> ReplyControls {
        REPLIES.with(|r| r.borrow()[&target])
    }

    pub(in crate::ui::compose) fn post(
        here: Option<CommunityName>,
    ) -> PostControls {
        POSTS.with(|p| p.borrow()[&here])
    }
}

/// A page under a root that can take it away (as navigating elsewhere
/// does), with the client's slots and the session over a portal
struct Harness {
    dom: VirtualDom,
}

thread_local! {
    static SHOWN: std::cell::Cell<Option<Signal<bool>>> =
        const { std::cell::Cell::new(None) };
}

#[derive(Props, Clone)]
struct PageProps {
    page: fn() -> Element,
}

impl PartialEq for PageProps {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::fn_addr_eq(self.page, other.page)
    }
}

#[allow(non_snake_case)]
fn Toggle(props: PageProps) -> Element {
    let shown = use_signal(|| true);
    use_hook(|| SHOWN.with(|s| s.set(Some(shown))));
    rsx! {
        if shown() {
            Page { page: props.page }
        }
    }
}

#[allow(non_snake_case)]
fn Page(props: PageProps) -> Element {
    (props.page)()
}

impl Harness {
    async fn new(portal: &FakePortal, page: fn() -> Element) -> Self {
        let mut dom = VirtualDom::new_with_props(Toggle, PageProps { page });
        dom.provide_root_context(slots());
        session::provide(&dom, portal.clone());
        dom.rebuild_in_place();
        let mut harness = Self { dom };
        harness.settle().await;
        harness
    }

    async fn settle(&mut self) -> String {
        settle(&mut self.dom).await
    }

    /// `f` in the root's runtime, as an event handler runs
    fn act<R>(&self, f: impl FnOnce() -> R) -> R {
        self.dom.in_scope(ScopeId::ROOT, f)
    }

    /// Navigate away: the page is unmounted
    async fn leave(&mut self) {
        self.act(|| {
            let mut shown = SHOWN.with(|s| s.get()).expect("a toggle");
            shown.set(false);
        });
        self.settle().await;
    }
}

fn reply_alone() -> Element {
    use_context_provider(on_post);
    rsx! { ReplyBox {} }
}

fn post_alone() -> Element {
    use_context_provider(in_community);
    rsx! { NewPost {} }
}

fn thread_page() -> Element {
    ThreadIsland(ThreadPage::loaded(fixtures::post_with_comments()))
}

/// Opens the top-level reply box and types `text` into it
async fn typed(h: &mut Harness, text: &str) -> String {
    let c = controls::reply(on_post());
    h.act(|| c.open());
    h.settle().await;
    h.act(|| controls::reply(on_post()).input(text.into()));
    h.settle().await
}

fn send(h: &Harness) {
    h.act(|| controls::reply(on_post()).send());
}

fn ids(portal: &FakePortal) -> Vec<OperatorActionId> {
    portal
        .0
        .borrow()
        .comments
        .iter()
        .map(|c| c.action_id)
        .collect()
}

fn status(status: u16, code: Option<RefusalCode>) -> ApiError {
    ApiError::Status {
        status,
        message: "refused".into(),
        code,
    }
}

/// A retry after a failure whose outcome is unknown goes under the same
/// action id (the server answers a repeat as it answered the first), for
/// no answer at all and for a 5xx alike; a success renews it
#[tokio::test]
async fn a_retry_reuses_the_action_id_until_a_success() {
    for failure in [
        ApiError::Network("offline".into()),
        status(502, None),
        status(500, None),
        ApiError::Decode("eof".into()),
    ] {
        let portal = FakePortal::signed_in(true);
        portal
            .0
            .borrow_mut()
            .comment_answers
            .push_back(Err(failure.clone()));
        let mut h = Harness::new(&portal, reply_alone).await;
        typed(&mut h, "hello").await;
        send(&h);
        let html = h.settle().await;
        assert!(html.contains(r#"value="hello""#), "{failure}: {html}");
        assert!(!html.contains("Nothing was posted"), "{failure}: {html}");
        send(&h);
        h.settle().await;
        let sent = ids(&portal);
        assert_eq!(sent.len(), 2, "{failure}");
        assert_eq!(sent[0], sent[1], "{failure}: the same id on a retry");
        // Posted: the next draft gets a new id
        typed(&mut h, "again").await;
        send(&h);
        h.settle().await;
        let sent = ids(&portal);
        assert_eq!(sent.len(), 3, "{failure}");
        assert_ne!(sent[2], sent[1], "{failure}: renewed after a success");
    }
}

/// A refusal (a 4xx) wrote nothing: an edited text may go under the same
/// id, which is kept
#[tokio::test]
async fn a_refusal_keeps_the_id_for_an_edited_text() {
    let portal = FakePortal::signed_in(true);
    portal
        .0
        .borrow_mut()
        .comment_answers
        .push_back(Err(status(400, None)));
    let mut h = Harness::new(&portal, reply_alone).await;
    typed(&mut h, "hello").await;
    send(&h);
    h.settle().await;
    h.act(|| controls::reply(on_post()).input("hello, edited".into()));
    send(&h);
    h.settle().await;
    let fake = portal.0.borrow();
    assert_eq!(fake.comments.len(), 2);
    assert_eq!(fake.comments[0].action_id, fake.comments[1].action_id);
    assert_eq!(fake.comments[1].payload.body, "hello, edited");
}

/// After a send with no known outcome, a changed text is held back with
/// the "Send as new" choice, sending nothing; choosing it sends the text
/// under a new id
#[tokio::test]
async fn send_as_new_renews_the_id() {
    let portal = FakePortal::signed_in(true);
    portal
        .0
        .borrow_mut()
        .comment_answers
        .push_back(Err(status(504, None)));
    let mut h = Harness::new(&portal, reply_alone).await;
    typed(&mut h, "hello").await;
    send(&h);
    h.settle().await;
    h.act(|| controls::reply(on_post()).input("hello, edited".into()));
    send(&h);
    let html = h.settle().await;
    assert_eq!(ids(&portal).len(), 1, "held back, not sent");
    assert!(html.contains(">Send as new</button>"), "{html}");
    assert!(html.contains("may have been posted"), "{html}");
    h.act(|| controls::reply(on_post()).send_as_new());
    send(&h);
    h.settle().await;
    let sent = ids(&portal);
    assert_eq!(sent.len(), 2);
    assert_ne!(sent[0], sent[1]);
    assert_eq!(portal.0.borrow().comments[1].payload.body, "hello, edited");
}

/// The server's refusal of a reused action id (its code, not its words)
/// leads to the "Send as new" choice
#[tokio::test]
async fn a_reused_id_refusal_offers_send_as_new() {
    let portal = FakePortal::signed_in(true);
    portal
        .0
        .borrow_mut()
        .comment_answers
        .push_back(Err(status(409, Some(RefusalCode::ReusedActionId))));
    let mut h = Harness::new(&portal, reply_alone).await;
    typed(&mut h, "hello").await;
    send(&h);
    let html = h.settle().await;
    assert!(html.contains(">Send as new</button>"), "{html}");
    assert!(html.contains(r#"value="hello""#), "{html}");
    let first = ids(&portal)[0];
    h.act(|| controls::reply(on_post()).send_as_new());
    send(&h);
    h.settle().await;
    let sent = ids(&portal);
    assert_eq!(sent.len(), 2);
    assert_ne!(sent[1], first);
}

/// A second submit while the first is in flight sends nothing
#[tokio::test]
async fn a_double_submit_sends_once() {
    let portal = FakePortal::signed_in(true);
    let gate = portal.hold();
    let mut h = Harness::new(&portal, reply_alone).await;
    typed(&mut h, "hello").await;
    send(&h);
    let html = h.settle().await;
    assert!(html.contains("Posting…"), "{html}");
    send(&h);
    send(&h);
    h.settle().await;
    assert_eq!(ids(&portal).len(), 1);
    gate.send(()).unwrap();
    let html = h.settle().await;
    assert_eq!(ids(&portal).len(), 1);
    assert!(html.contains("Your comment is posted."), "{html}");
}

/// Every kind of failure keeps the draft, in the field
#[tokio::test]
async fn every_failure_keeps_the_draft() {
    for failure in [
        ApiError::Network("offline".into()),
        ApiError::Decode("eof".into()),
        status(500, None),
        status(409, Some(RefusalCode::AlreadyCommented)),
        status(409, Some(RefusalCode::ReusedActionId)),
        status(400, None),
        status(404, None),
        ApiError::SignedOut("expired".into()),
        ApiError::from(fixtures::suspension()),
    ] {
        let portal = FakePortal::signed_in(true);
        portal
            .0
            .borrow_mut()
            .comment_answers
            .push_back(Err(failure.clone()));
        let mut h = Harness::new(&portal, reply_alone).await;
        typed(&mut h, "keep me").await;
        send(&h);
        let html = h.settle().await;
        assert!(html.contains(r#"value="keep me""#), "{failure}: {html}");
        assert!(html.contains(r#"role="alert""#), "{failure}: {html}");
    }
}

/// Leaving the page while a reply is in flight is safe: the answer, after
/// the box and its thread are gone, writes to nothing of theirs (dioxus's
/// `Callback::call` on a dropped callback panics, and agora-web aborts on
/// a panic). The draft at the root records the post, and the box says so
/// when the page is drawn again
#[tokio::test]
async fn leaving_mid_send_is_safe() {
    for page in [thread_page as fn() -> Element, reply_alone] {
        let portal = FakePortal::signed_in(true);
        let gate = portal.hold();
        let mut h = Harness::new(&portal, page).await;
        typed(&mut h, "hello").await;
        send(&h);
        h.settle().await;
        h.leave().await;
        gate.send(()).unwrap();
        let html = h.settle().await;
        assert_eq!(html, "");
        assert_eq!(ids(&portal).len(), 1);
        // Back on the page: the draft is gone (posted), and the box says it
        h.act(|| {
            let mut shown = SHOWN.with(|s| s.get()).unwrap();
            shown.set(true);
        });
        let html = h.settle().await;
        assert!(html.contains("Your comment is posted."), "{html}");
        assert!(!html.contains("draft waiting"), "{html}");
    }
}

/// The thread's `CommentPosted` after the thread is gone: nothing, no
/// panic (the reviewer's reproduction, on the signals that replaced the
/// callback)
#[tokio::test]
async fn comment_posted_after_the_thread_is_gone() {
    thread_local! {
        static STASHED: std::cell::Cell<Option<CommentPosted>> =
            const { std::cell::Cell::new(None) };
    }
    fn grab() -> Element {
        let posted = try_use_context::<CommentPosted>();
        use_hook(|| STASHED.with(|s| s.set(posted)));
        rsx! {}
    }
    let mut dom =
        VirtualDom::new_with_props(Toggle, PageProps { page: thread_page });
    dom.provide_root_context(ClientSlots::new().with(ClientSlot::Reply, grab));
    session::provide(&dom, FakePortal::signed_in(true));
    dom.rebuild_in_place();
    settle(&mut dom).await;
    let posted = STASHED
        .with(|s| s.get())
        .expect("a reply slot under the thread");
    assert!(
        dom.in_scope(ScopeId::ROOT, || posted.notify(fixtures::comment().id))
            .is_some()
    );
    dom.in_scope(ScopeId::ROOT, || {
        let mut shown = SHOWN.with(|s| s.get()).unwrap();
        shown.set(false);
    });
    settle(&mut dom).await;
    let id = fixtures::comment().id;
    let told = dom.in_scope(ScopeId::ROOT, || posted.notify(id));
    assert_eq!(told, None, "nothing to tell: the thread is gone");
    assert_eq!(
        dom.in_scope(ScopeId::ROOT, || posted.after(1)),
        AfterReply::Idle
    );
}

/// Close closes, even when the draft can't be sent (here: the write was
/// refused for a suspension); the draft waits, and the pill says so;
/// opening it again shows the text
#[tokio::test]
async fn close_keeps_the_draft() {
    let portal = FakePortal::signed_in(true);
    portal
        .0
        .borrow_mut()
        .comment_answers
        .push_back(Err(ApiError::from(fixtures::suspension())));
    let mut h = Harness::new(&portal, reply_alone).await;
    typed(&mut h, "kept").await;
    send(&h);
    let html = h.settle().await;
    assert!(html.contains("suspended"), "{html}");
    assert!(
        html.contains(">Can&#39;t post now</button>")
            || html.contains(">Can't post now</button>"),
        "{html}"
    );
    h.act(|| controls::reply(on_post()).close());
    let html = h.settle().await;
    assert!(!html.contains("<textarea"), "{html}");
    assert!(
        html.contains(">Write a comment (draft waiting)</button>"),
        "{html}"
    );
    h.act(|| controls::reply(on_post()).open());
    let html = h.settle().await;
    assert!(html.contains(r#"value="kept""#), "{html}");
    // A writer's box closes the same way, and an empty one leaves a plain pill
    let portal = FakePortal::signed_in(true);
    let mut h = Harness::new(&portal, reply_alone).await;
    typed(&mut h, "").await;
    h.act(|| controls::reply(on_post()).close());
    let html = h.settle().await;
    assert!(html.contains(">Write a comment</button>"), "{html}");
}

/// The client hears whether a draft would be lost by leaving the page:
/// when text is typed, and again when it is posted
#[tokio::test]
async fn unsent_drafts_are_reported() {
    let portal = FakePortal::signed_in(true);
    let mut h = Harness::new(&portal, reply_alone).await;
    typed(&mut h, "hello").await;
    assert_eq!(portal.0.borrow().unsent, vec![true]);
    send(&h);
    h.settle().await;
    assert_eq!(portal.0.borrow().unsent, vec![true, false]);
}

/// A draft is kept across a page change: left and come back to, the box
/// says a draft is waiting, with its text
#[tokio::test]
async fn a_draft_outlives_its_page() {
    let portal = FakePortal::signed_in(true);
    let mut h = Harness::new(&portal, reply_alone).await;
    typed(&mut h, "half a thought").await;
    h.leave().await;
    h.act(|| {
        let mut shown = SHOWN.with(|s| s.get()).unwrap();
        shown.set(true);
    });
    let html = h.settle().await;
    assert!(
        html.contains(">Write a comment (draft waiting)</button>"),
        "{html}"
    );
    h.act(|| controls::reply(on_post()).open());
    let html = h.settle().await;
    assert!(html.contains(r#"value="half a thought""#), "{html}");
}

/// A thread where the viewer's poster already has a top-level comment
/// offers no "Write a comment" box, and says why, with the way to it
#[tokio::test]
async fn a_poster_with_a_top_level_comment_is_told() {
    fn page() -> Element {
        let mut read = fixtures::post_with_comments();
        let poster = fixtures::portal_session(true).poster.unwrap();
        read.comments.push(crate::responses::CommentResponse {
            agent_id: poster.id,
            agent_name: Some(poster.name),
            ..fixtures::comment()
        });
        ThreadIsland(ThreadPage::loaded(read))
    }
    let portal = FakePortal::signed_in(true);
    let mut h = Harness::new(&portal, page).await;
    let html = h.settle().await;
    assert!(!html.contains(">Write a comment"), "{html}");
    assert!(html.contains("You have a comment on this post"), "{html}");
    // Replies under the comment are still offered
    assert!(html.contains(">Reply</button>"), "{html}");
    // Someone else's comment changes nothing
    fn other() -> Element {
        let mut read = fixtures::post_with_comments();
        read.comments.push(fixtures::comment());
        read.comment_stubs.push(fixtures::comment_stub());
        ThreadIsland(ThreadPage::loaded(read))
    }
    let mut h = Harness::new(&portal, other).await;
    let html = h.settle().await;
    assert!(html.contains(">Write a comment</button>"), "{html}");
}

#[test]
fn thread_authors_count_stubs_and_skip_deleted_ones() {
    let poster = fixtures::portal_session(true).poster.unwrap();
    let mut read = fixtures::post_with_comments();
    read.comments.push(crate::responses::CommentResponse {
        agent_id: poster.id,
        deleted_at: Some(chrono::Utc::now()),
        ..fixtures::comment()
    });
    assert!(
        ThreadAuthors::of(&read)
            .iter()
            .all(|c| c.agent_id != Some(poster.id)),
        "a deleted one doesn't count"
    );
    read.comment_stubs.push(crate::responses::CommentStub {
        agent_name: Some(poster.name.clone()),
        ..fixtures::comment_stub()
    });
    let top: Vec<_> = ThreadAuthors::of(&read)
        .into_iter()
        .filter(|c| c.agent_name.as_ref() == Some(&poster.name))
        .collect();
    assert_eq!(top.len(), 1);
    assert_eq!(top[0].parent, fixtures::comment_stub().parent_comment_id);
}

/// The new-post form: the same id after an unknown outcome; a post
/// answered after the feed was left moves no one, and the form says it is
/// up when the feed is back
#[tokio::test]
async fn a_new_post_retries_and_outlives_its_page() {
    let portal = FakePortal::signed_in(true);
    portal
        .0
        .borrow_mut()
        .post_answers
        .push_back(Err(status(503, None)));
    let mut h = Harness::new(&portal, post_alone).await;
    let here = Some("meta-governance".parse::<CommunityName>().unwrap());
    let c = controls::post(here.clone());
    h.act(|| c.open());
    h.settle().await;
    let c = controls::post(here.clone());
    h.act(|| {
        c.edit(|d| d.title = "On agency".into());
        c.edit(|d| d.body = "Some words".into());
    });
    h.act(|| controls::post(here.clone()).send());
    let html = h.settle().await;
    assert!(
        html.contains("couldn&#39;t say whether")
            || html.contains("couldn't say whether"),
        "{html}"
    );
    let gate = portal.hold();
    h.act(|| controls::post(here.clone()).send());
    h.settle().await;
    h.leave().await;
    gate.send(()).unwrap();
    h.settle().await;
    {
        let fake = portal.0.borrow();
        assert_eq!(fake.posts.len(), 2);
        assert_eq!(fake.posts[0].action_id, fake.posts[1].action_id);
    }
    h.act(|| {
        let mut shown = SHOWN.with(|s| s.get()).unwrap();
        shown.set(true);
    });
    let html = h.settle().await;
    assert!(html.contains("Your post is up."), "{html}");
    assert!(
        html.contains(">New post in meta-governance</button>"),
        "{html}"
    );
}

/// The community list: loading, empty and failed are each said, and told
/// apart
#[tokio::test]
async fn an_empty_community_list_is_not_a_loading_one() {
    let portal = FakePortal::signed_in(true);
    portal.0.borrow_mut().communities = Some(Ok(Vec::new()));
    let html = drawn(Some(portal), NewPostHere(None), open_post).await;
    assert!(html.contains("There are no communities to list"), "{html}");
    assert!(!html.contains("Loading the list"), "{html}");
}

// ---------------------------------------------------------------------
// The final review's fixes (2026-11-19)
// ---------------------------------------------------------------------

thread_local! {
    /// The thread's loads, answered in the order they were asked for: each
    /// waits for its sender (a test answers them in any order)
    static LOADS: RefCell<VecDeque<oneshot::Receiver<ThreadLoad>>> =
        const { RefCell::new(VecDeque::new()) };
    /// How many times the thread was loaded
    static LOADED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

use std::{cell::RefCell, collections::VecDeque};

use futures::{FutureExt, channel::oneshot};

use crate::ui::{Reload, thread::ThreadLoad};

/// A load of the thread the test answers: its sender
fn next_load() -> oneshot::Sender<ThreadLoad> {
    let (tx, rx) = oneshot::channel();
    LOADS.with(|l| l.borrow_mut().push_back(rx));
    tx
}

/// The client's `Reload<ThreadLoad>`, over [`LOADS`]
fn thread_reload() -> Reload<ThreadLoad> {
    LOADS.with(|l| l.borrow_mut().clear());
    LOADED.with(|l| l.set(0));
    Reload::new(|| {
        LOADED.with(|l| l.set(l.get() + 1));
        let next = LOADS.with(|l| l.borrow_mut().pop_front());
        async move {
            match next {
                Some(rx) => rx
                    .await
                    .unwrap_or_else(|_| ThreadLoad::Failed("dropped".into())),
                None => ThreadLoad::Failed("no load scripted".into()),
            }
        }
        .boxed_local()
    })
}

impl Harness {
    /// [`Harness::new`], the thread loadable again through [`LOADS`]
    async fn reloadable(portal: &FakePortal, page: fn() -> Element) -> Self {
        let mut dom = VirtualDom::new_with_props(Toggle, PageProps { page });
        dom.provide_root_context(slots());
        dom.provide_root_context(thread_reload());
        session::provide(&dom, portal.clone());
        dom.rebuild_in_place();
        let mut harness = Self { dom };
        harness.settle().await;
        harness
    }
}

fn reply_on_comment_page() -> Element {
    use_context_provider(on_comment);
    rsx! { ReplyBox {} }
}

/// Item 2: no message suggests a reload (it would lose the draft the
/// message says is kept). On a thread, "Check the thread" loads it again
/// in place: the box and its text stay, and the box says what came of it.
/// On a comment's page the thread opens in a new tab; for a new post, the
/// newest posts do
#[tokio::test]
async fn a_check_never_loses_the_draft() {
    // The words, for every failure that may have been posted, and the
    // reused id's
    for e in [
        ApiError::Network("offline".into()),
        ApiError::Decode("eof".into()),
        status(502, None),
        status(409, Some(RefusalCode::ReusedActionId)),
    ] {
        let words = e.for_a_person();
        assert!(!words.to_lowercase().contains("reload"), "{words}");
    }

    let portal = FakePortal::signed_in(true);
    portal
        .0
        .borrow_mut()
        .comment_answers
        .push_back(Err(status(502, None)));
    let mut h = Harness::reloadable(&portal, thread_page).await;
    typed(&mut h, "hello").await;
    send(&h);
    let html = h.settle().await;
    assert!(html.contains(">Check the thread</button>"), "{html}");
    assert!(!html.to_lowercase().contains("reload"), "{html}");
    // Changed since: held back, with the same way to check
    h.act(|| controls::reply(on_post()).input("hello, edited".into()));
    send(&h);
    let html = h.settle().await;
    assert!(html.contains("Check first"), "{html}");
    assert!(html.contains(">Check the thread</button>"), "{html}");
    assert!(!html.to_lowercase().contains("reload"), "{html}");

    let load = next_load();
    h.act(|| controls::reply(on_post()).check());
    let html = h.settle().await;
    assert_eq!(LOADED.with(|l| l.get()), 1);
    assert!(html.contains("Checking the thread…"), "{html}");
    load.send(ThreadLoad::Loaded(Box::new(fixtures::post_with_comments())))
        .unwrap();
    let html = h.settle().await;
    assert!(html.contains("if your text is in it"), "{html}");
    assert!(html.contains(r#"value="hello, edited""#), "the draft stays");
    assert_eq!(ids(&portal).len(), 1, "a check sends nothing");

    // A failed check says so, and offers the thread in a new tab
    let load = next_load();
    h.act(|| controls::reply(on_post()).check());
    h.settle().await;
    load.send(ThreadLoad::Failed("offline".into())).unwrap();
    let html = h.settle().await;
    assert!(
        html.contains("The thread couldn&#39;t be loaded again (offline)")
            || html.contains("The thread couldn't be loaded again (offline)"),
        "{html}"
    );
    assert!(html.contains(r#"target="_blank""#), "{html}");

    // A comment's page: the thread, in a new tab
    let portal = FakePortal::signed_in(true);
    portal
        .0
        .borrow_mut()
        .comment_answers
        .push_back(Err(ApiError::Network("offline".into())));
    let mut h = Harness::new(&portal, reply_on_comment_page).await;
    h.act(|| controls::reply(on_comment()).open());
    h.settle().await;
    h.act(|| controls::reply(on_comment()).input("a reply".into()));
    h.act(|| controls::reply(on_comment()).send());
    let html = h.settle().await;
    let thread = AgoraLink::post(fixtures::post().id).site_path();
    assert!(html.contains(">Open the thread in a new tab</a>"), "{html}");
    assert!(html.contains(&format!(r#"href="{thread}""#)), "{html}");
    assert!(!html.contains("Check the thread"), "{html}");

    // A new post: the community's newest posts, in a new tab
    let portal = FakePortal::signed_in(true);
    portal
        .0
        .borrow_mut()
        .post_answers
        .push_back(Err(status(503, None)));
    let mut h = Harness::new(&portal, post_alone).await;
    let here = Some("meta-governance".parse::<CommunityName>().unwrap());
    h.act(|| controls::post(here.clone()).open());
    h.settle().await;
    h.act(|| {
        let c = controls::post(here.clone());
        c.edit(|d| d.title = "On agency".into());
        c.edit(|d| d.body = "Some words".into());
    });
    h.act(|| controls::post(here.clone()).send());
    let html = h.settle().await;
    let feed = AgoraLink::Feed(GetFeedInput {
        community: here.clone(),
        ..GetFeedInput::default()
    })
    .site_path();
    assert!(
        html.contains(">Open the newest posts in a new tab</a>"),
        "{html}"
    );
    assert!(html.contains(&format!(r#"href="{feed}""#)), "{html}");
    assert!(!html.to_lowercase().contains("reload"), "{html}");
}

/// Item 3: a 401's and a suspension's words aren't kept as the box's last
/// word once the session is fine again: signed in again ("I've signed
/// in"), or the suspension lifted, the box says it wasn't sent then and can
/// be now, and sends
#[tokio::test]
async fn a_session_refusal_goes_when_the_session_is_back() {
    for (refusal, then) in [
        (ApiError::SignedOut("expired".into()), "you weren"),
        (
            ApiError::from(fixtures::suspension()),
            "posting was suspended",
        ),
    ] {
        let portal = FakePortal::signed_in(true);
        portal
            .0
            .borrow_mut()
            .comment_answers
            .push_back(Err(refusal.clone()));
        let mut h = Harness::new(&portal, reply_alone).await;
        typed(&mut h, "kept").await;
        send(&h);
        let html = h.settle().await;
        // Said once, by the session's state
        let now = match &refusal {
            ApiError::SignedOut(_) => "no longer signed in",
            _ => "suspended",
        };
        assert_eq!(html.matches(now).count(), 1, "{refusal}: {html}");
        assert!(!html.contains(then), "{refusal}: {html}");

        // Signed in again in another tab, or the suspension lifted
        h.act(|| controls::reply(on_post()).handle.refresh());
        let html = h.settle().await;
        assert!(!html.contains("no longer signed in"), "{refusal}: {html}");
        assert!(!html.contains("is suspended"), "{refusal}: {html}");
        assert!(html.contains(then), "{refusal}: {html}");
        assert!(html.contains("You can send it now."), "{refusal}: {html}");
        assert!(html.contains(">Post comment</button>"), "{refusal}: {html}");
        assert!(html.contains(r#"value="kept""#), "{refusal}: {html}");
        send(&h);
        let html = h.settle().await;
        assert!(
            html.contains("Your comment is posted."),
            "{refusal}: {html}"
        );
        assert_eq!(ids(&portal).len(), 2);
    }
}

/// Item 4: a write that never answers ends in the client's timeout, which
/// reaches the box as a network failure (agora-web's `api::send`): until
/// then "Posting…", disabled; after it, the unknown-outcome words, the
/// text kept, the button back, and a retry under the same id
#[tokio::test]
async fn a_write_that_times_out_is_an_unknown_outcome() {
    let portal = FakePortal::signed_in(true);
    // What agora-web's timeout makes of reqwest's error
    let timed_out = ApiError::Network("no answer within 30 seconds".into());
    portal
        .0
        .borrow_mut()
        .comment_answers
        .push_back(Err(timed_out.clone()));
    let gate = portal.hold();
    let mut h = Harness::new(&portal, reply_alone).await;
    typed(&mut h, "hello").await;
    send(&h);
    let html = h.settle().await;
    assert!(html.contains("Posting…"), "{html}");
    assert!(html.contains("disabled"), "{html}");
    gate.send(()).unwrap();
    let html = h.settle().await;
    assert!(html.contains("no answer within 30 seconds"), "{html}");
    assert!(html.contains("may or may not have been posted"), "{html}");
    assert!(html.contains(">Post comment</button>"), "{html}");
    assert!(html.contains(r#"value="hello""#), "{html}");
    assert!(timed_out.outcome_unknown());
    send(&h);
    h.settle().await;
    let sent = ids(&portal);
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0], sent[1], "the retry is safe: the same id");
}

/// Item 5: drafts belong to the operator and poster they were written
/// for. Another operator's session (after a 401), or another poster,
/// finds none, and sends under a new id; the same operator signing in
/// again keeps them
#[tokio::test]
async fn another_operators_session_clears_the_drafts() {
    use crate::operator::{AgentRef, OperatorRef};
    let another_operator = PortalSessionResponse {
        operator: OperatorRef {
            id: OperatorId::new(),
            display_name: "someone".into(),
        },
        ..fixtures::portal_session(true)
    };
    let another_poster = PortalSessionResponse {
        poster: Some(AgentRef {
            id: AgentId::new(),
            name: "another-poster".parse().unwrap(),
        }),
        ..fixtures::portal_session(true)
    };
    for (who, next) in [
        ("the same operator", fixtures::portal_session(true)),
        ("another operator", another_operator),
        ("another poster", another_poster),
    ] {
        let portal = FakePortal::signed_in(true);
        portal
            .0
            .borrow_mut()
            .comment_answers
            .push_back(Err(ApiError::SignedOut("expired".into())));
        let mut h = Harness::new(&portal, reply_alone).await;
        typed(&mut h, "the first operator's words").await;
        send(&h);
        let html = h.settle().await;
        assert!(html.contains("no longer signed in"), "{who}: {html}");
        portal.0.borrow_mut().session = Some(Ok(next));
        h.act(|| controls::reply(on_post()).handle.refresh());
        let html = h.settle().await;
        let kept = html.contains("the first operator&#39;s words")
            || html.contains("the first operator's words");
        assert_eq!(kept, who == "the same operator", "{who}: {html}");
        let drafts = h.act(|| controls::reply(on_post()).drafts);
        assert_eq!(h.act(|| drafts.unsent()), kept, "{who}");
        h.act(|| controls::reply(on_post()).input("new words".into()));
        send(&h);
        h.settle().await;
        let sent = ids(&portal);
        assert_eq!(sent.len(), 2, "{who}");
        assert_eq!(
            sent[0] == sent[1],
            kept,
            "{who}: the id goes with the draft"
        );
    }
}

/// Item 6: after a send with no known outcome, a refusal of the unchanged
/// retry doesn't forget it: an edited text is still held back, not sent
/// under the old id
#[tokio::test]
async fn a_refused_retry_keeps_the_unknown_send() {
    let portal = FakePortal::signed_in(true);
    {
        let mut fake = portal.0.borrow_mut();
        fake.comment_answers.push_back(Err(status(502, None)));
        fake.comment_answers.push_back(Err(status(400, None)));
    }
    let mut h = Harness::new(&portal, reply_alone).await;
    typed(&mut h, "hello").await;
    send(&h);
    h.settle().await;
    send(&h);
    h.settle().await;
    assert_eq!(ids(&portal).len(), 2);
    h.act(|| controls::reply(on_post()).input("hello, edited".into()));
    send(&h);
    let html = h.settle().await;
    assert_eq!(ids(&portal).len(), 2, "held back, not sent");
    assert!(html.contains(">Send as new</button>"), "{html}");
}

thread_local! {
    static PAGE_SHOWN: std::cell::Cell<Option<Signal<bool>>> =
        const { std::cell::Cell::new(None) };
}

/// The session slot's list of drafts above a page that can be taken away
#[allow(non_snake_case)]
fn WithList(props: PageProps) -> Element {
    let shown = use_signal(|| true);
    use_hook(|| PAGE_SHOWN.with(|s| s.set(Some(shown))));
    rsx! {
        UnsentDrafts {}
        if shown() {
            Page { page: props.page }
        }
    }
}

/// Item 7: a draft whose box isn't on the page (left for another page, or
/// on a thread locked since) is listed, with its text and a way to its
/// page, and can be discarded; one whose box is drawn isn't listed
#[tokio::test]
async fn a_draft_without_its_box_is_listed_and_can_be_discarded() {
    let portal = FakePortal::signed_in(true);
    let mut dom =
        VirtualDom::new_with_props(WithList, PageProps { page: reply_alone });
    dom.provide_root_context(slots());
    session::provide(&dom, portal.clone());
    dom.rebuild_in_place();
    let mut h = Harness { dom };
    let html = typed(&mut h, "a thought").await;
    assert!(!html.contains("unsent draft"), "drawn here: {html}");
    h.act(|| {
        let mut shown = PAGE_SHOWN.with(|s| s.get()).unwrap();
        shown.set(false);
    });
    let html = h.settle().await;
    assert!(
        html.contains("1 unsent draft isn&#39;t shown on this page")
            || html.contains("1 unsent draft isn't shown on this page"),
        "{html}"
    );
    assert!(html.contains("A comment on a post"), "{html}");
    assert!(html.contains(r#"value="a thought""#), "{html}");
    assert!(html.contains("readonly"), "{html}");
    let thread = AgoraLink::post(fixtures::post().id).site_path();
    assert!(html.contains(&format!(r#"href="{thread}""#)), "{html}");
    assert!(html.contains(">Discard</button>"), "{html}");
    // Discarded: gone from the list, and nothing is unsent
    h.act(|| {
        let drafts = controls::reply(on_post()).drafts;
        drafts.discard(&DraftKey::Reply(on_post()));
    });
    let html = h.settle().await;
    assert!(!html.contains("unsent draft"), "{html}");
    assert_eq!(portal.0.borrow().unsent, vec![true, false]);

    // A thread locked since draws no box: its draft is listed
    thread_local! {
        static LOCKED: std::cell::Cell<Option<Signal<bool>>> =
            const { std::cell::Cell::new(None) };
    }
    #[allow(non_snake_case)]
    fn Lockable() -> Element {
        let locked = use_signal(|| false);
        use_hook(|| LOCKED.with(|l| l.set(Some(locked))));
        rsx! {
            UnsentDrafts {}
            if locked() {
                Locked {}
            } else {
                Unlocked {}
            }
        }
    }
    #[allow(non_snake_case)]
    fn Unlocked() -> Element {
        ThreadIsland(ThreadPage::loaded(with_comment()))
    }
    #[allow(non_snake_case)]
    fn Locked() -> Element {
        let mut read = with_comment();
        read.post.notice = Some(locked_notice());
        ThreadIsland(ThreadPage::loaded(read))
    }
    let portal = FakePortal::signed_in(true);
    let mut dom = VirtualDom::new(Lockable);
    dom.provide_root_context(slots());
    session::provide(&dom, portal.clone());
    dom.rebuild_in_place();
    let mut h = Harness { dom };
    let html = typed(&mut h, "before the lock").await;
    assert!(!html.contains("unsent draft"), "{html}");
    h.act(|| {
        let mut locked = LOCKED.with(|l| l.get()).unwrap();
        locked.set(true);
    });
    let html = h.settle().await;
    assert!(!html.contains("Write a comment"), "locked: {html}");
    assert!(html.contains("1 unsent draft"), "{html}");
    assert!(html.contains(r#"value="before the lock""#), "{html}");
}

/// A thread with one comment, someone else's
fn with_comment() -> crate::responses::PostWithCommentsResponse {
    let mut read = fixtures::post_with_comments();
    read.comments.push(fixtures::comment());
    read
}

fn thread_with_comment() -> Element {
    ThreadIsland(ThreadPage::loaded(with_comment()))
}

/// Item 8: a community's feed offers a new post only once the community
/// has loaded; not one that doesn't exist, nor one whose load failed. The
/// global feed offers it whatever its load
#[tokio::test]
async fn no_new_post_for_a_community_that_is_not_there() {
    let here: Option<CommunityName> = Some("ferns".parse().unwrap());
    let page = |community: Option<CommunityName>, feed: FeedLoad| {
        FeedPage::new(
            GetFeedInput {
                community,
                ..GetFeedInput::default()
            },
            feed,
        )
    };
    for (feed, offered) in [
        (FeedLoad::NotFound, false),
        (FeedLoad::Failed("offline".into()), false),
        (FeedLoad::Loaded(vec![fixtures::post()]), true),
    ] {
        let what = format!("{feed:?}");
        let (_, after) = mount(
            FeedIsland,
            page(here.clone(), feed),
            FakePortal::signed_in(true),
        )
        .await;
        assert_eq!(after.contains("New post"), offered, "{what}: {after}");
    }
    let (_, after) = mount(
        FeedIsland,
        page(None, FeedLoad::Failed("offline".into())),
        FakePortal::signed_in(true),
    )
    .await;
    assert!(after.contains(">New post</button>"), "{after}");
}

/// Item 9: two quick replies on a thread: each box shows its own request's
/// load, and the older load answering last doesn't replace the thread the
/// newer one drew. A pill replaced by the "you have a comment" note gives
/// up its claim on the focus
#[tokio::test]
async fn each_box_shows_its_own_reload_and_the_newest_wins() {
    let under = ReplyTarget {
        post: fixtures::post().id,
        parent: Some(fixtures::comment().id),
    };
    let portal = FakePortal::signed_in(true);
    let mut h = Harness::reloadable(&portal, thread_with_comment).await;
    let first = next_load();
    let second = next_load();
    typed(&mut h, "first").await;
    send(&h);
    h.settle().await;
    h.act(|| controls::reply(under).open());
    h.settle().await;
    h.act(|| controls::reply(under).input("second".into()));
    h.act(|| controls::reply(under).send());
    h.settle().await;
    assert_eq!(LOADED.with(|l| l.get()), 2);
    // The newer load fails; the older answers after it with a thread
    second.send(ThreadLoad::Failed("offline".into())).unwrap();
    let html = h.settle().await;
    assert_eq!(
        html.matches("couldn&#39;t be loaded again").count()
            + html.matches("couldn't be loaded again").count(),
        1,
        "only the second box's: {html}"
    );
    let mut stale = with_comment();
    stale.post.title = "An older answer".into();
    first.send(ThreadLoad::Loaded(Box::new(stale))).unwrap();
    let html = h.settle().await;
    assert!(!html.contains("An older answer"), "the newest wins: {html}");

    // The note replacing the pill clears its focus claim
    let portal = FakePortal::signed_in(true);
    let mut h = Harness::reloadable(&portal, thread_page).await;
    let load = next_load();
    typed(&mut h, "mine").await;
    send(&h);
    h.settle().await;
    let mut read = fixtures::post_with_comments();
    let poster = fixtures::portal_session(true).poster.unwrap();
    read.comments.push(crate::responses::CommentResponse {
        id: crate::ui::test_portal::created_comment(),
        agent_id: poster.id,
        agent_name: Some(poster.name),
        ..fixtures::comment()
    });
    load.send(ThreadLoad::Loaded(Box::new(read))).unwrap();
    let html = h.settle().await;
    assert!(html.contains("You have a comment on this post"), "{html}");
    let refocus = h.act(|| *controls::reply(on_post()).refocus.peek());
    assert!(!refocus, "nothing left to take the focus back");
}

/// Item 10: one reply per poster per comment, as one top-level comment
/// per post: under a comment the poster already answered, a note and a
/// link instead of "Reply"; every other comment still offers it
#[tokio::test]
async fn a_poster_who_replied_to_a_comment_is_told() {
    fn page() -> Element {
        let mut read = with_comment();
        let poster = fixtures::portal_session(true).poster.unwrap();
        let parent = read.comments[0].id;
        read.comments.push(crate::responses::CommentResponse {
            id: CommentId::new(),
            parent_comment_id: Some(parent),
            agent_id: poster.id,
            agent_name: Some(poster.name),
            ..fixtures::comment()
        });
        ThreadIsland(ThreadPage::loaded(read))
    }
    let portal = FakePortal::signed_in(true);
    let mut h = Harness::new(&portal, page).await;
    let html = h.settle().await;
    assert_eq!(
        html.matches("You replied to this comment").count(),
        1,
        "{html}"
    );
    let comments = with_comment().comments.len() + 1;
    assert_eq!(
        html.matches(">Reply</button>").count(),
        comments - 1,
        "{html}"
    );
    assert!(html.contains(">Write a comment</button>"), "{html}");
}
