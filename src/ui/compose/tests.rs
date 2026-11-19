use std::time::Duration;

use super::*;
use crate::{
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

/// After a send with no answer, the same text may be sent again (the
/// server answers it as the first); changed text is held back for the
/// person to decide, and a refusal or a failure with an answer is known
#[test]
fn a_changed_text_after_no_answer_is_held_back() {
    assert!(!must_resend_as_sent(None, "anything"));
    assert!(!must_resend_as_sent(Some("hello"), "hello"));
    assert!(must_resend_as_sent(Some("hello"), "hello, world"));
    assert!(outcome_unknown(&ApiError::Network("offline".into())));
    assert!(outcome_unknown(&ApiError::Decode("eof".into())));
    for known in [
        ApiError::SignedOut("x".into()),
        ApiError::Status {
            status: 409,
            message: "already replied".into(),
        },
        ApiError::Status {
            status: 500,
            message: "internal error".into(),
        },
    ] {
        assert!(!outcome_unknown(&known), "{known}");
    }
}
