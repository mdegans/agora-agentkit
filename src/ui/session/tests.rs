use dioxus::dioxus_core::NoOpMutations;

use super::*;
use crate::{
    errors::ApiErrorBody,
    fixtures::{self, CSRF},
    responses::ErrorResponse,
    ui::test_portal::FakePortal,
};

fn bootstrap_with(poster: bool, token: &str) -> PortalSessionResponse {
    PortalSessionResponse {
        csrf_token: crate::operator::CsrfToken(token.into()),
        ..fixtures::portal_session(poster)
    }
}

fn bootstrap(poster: bool) -> PortalSessionResponse {
    fixtures::portal_session(poster)
}

fn signed_in(poster: bool) -> Session {
    Session::from_bootstrap(Ok(bootstrap(poster)))
}

/// The `SignedIn` a write was made with
fn used(session: &Session) -> SignedIn {
    match session {
        Session::SignedIn(s) => s.clone(),
        other => panic!("not signed in: {other:?}"),
    }
}

fn banned() -> ApiError {
    fixtures::suspension().into()
}

fn plain_403() -> ApiError {
    ApiError::Forbidden(ApiErrorBody::Plain(ErrorResponse::new(
        "a write must carry this session's CSRF token",
    )))
}

fn decode_error() -> ApiError {
    serde_json::from_str::<PortalSessionResponse>("{}")
        .unwrap_err()
        .into()
}

fn can_write(session: &Session) -> bool {
    matches!(session, Session::SignedIn(s) if s.can_write())
}

/// A refusal reads as its status says
#[test]
fn a_refusal_is_read_by_its_status() {
    let body = |e: &str| ApiErrorBody::Plain(ErrorResponse::new(e));
    assert_eq!(
        ApiError::refused(401, body("expired")),
        ApiError::SignedOut("expired".into())
    );
    assert_eq!(
        ApiError::refused(403, body("csrf")),
        ApiError::Forbidden(body("csrf"))
    );
    assert_eq!(
        ApiError::refused(409, body("already replied")),
        ApiError::Status {
            status: 409,
            message: "already replied".into(),
            code: None,
        }
    );
    assert!(
        ApiError::refused(403, fixtures::suspension().into())
            .suspension()
            .is_some()
    );
}

/// What a person is told when a write fails: the server's words for a
/// refusal, and that the text is kept and a retry can't double-post
#[test]
fn a_failure_is_told_to_a_person() {
    let refused = ApiError::Status {
        status: 409,
        message: "you already replied to this comment".into(),
        code: None,
    };
    assert_eq!(
        refused.for_a_person(),
        "Agora refused it: you already replied to this comment"
    );
    let offline = ApiError::Network("offline".into()).for_a_person();
    assert!(offline.contains("kept"), "{offline}");
    assert!(
        offline.contains("unchanged can't post it twice"),
        "{offline}"
    );
    let five = ApiError::Status {
        status: 503,
        message: "standing could not be checked".into(),
        code: None,
    }
    .for_a_person();
    // A 5xx never says nothing was posted: it may have been
    assert!(
        five.contains("(503)") && five.contains("couldn't say whether"),
        "{five}"
    );
    assert!(!five.contains("Nothing was posted"), "{five}");
    assert!(!offline.contains("Nothing was posted"), "{offline}");
    // A coded refusal is worded for a person, not as the server's text
    // (which is for an agent and names `reply_to`)
    let agent_words = "You already have a top-level comment on post p: c. \
                       pass that comment's id as `reply_to`.";
    let coded = ApiError::refused(
        409,
        ErrorResponse::coded(RefusalCode::AlreadyCommented, agent_words).into(),
    );
    assert_eq!(coded.code(), Some(RefusalCode::AlreadyCommented));
    let words = coded.for_a_person();
    assert!(words.contains("one top-level comment per post"), "{words}");
    assert!(!words.contains("reply_to"), "{words}");
    let reused = ApiError::refused(
        409,
        ErrorResponse::coded(RefusalCode::ReusedActionId, "x").into(),
    );
    assert!(reused.for_a_person().contains("send this as new"));
    // The same words without the code are the server's, as they are
    let uncoded =
        ApiError::refused(409, ErrorResponse::new(agent_words).into());
    assert_eq!(uncoded.code(), None);
    assert!(uncoded.for_a_person().contains("reply_to"));
    assert_eq!(
        banned().for_a_person(),
        "Your account is suspended: you can read, but not post."
    );
    assert!(
        ApiError::SignedOut("x".into())
            .for_a_person()
            .contains("sign in")
    );
}

#[test]
fn a_bootstrap_answer_is_a_state() {
    assert!(can_write(&signed_in(true)));
    let without = used(&signed_in(false));
    assert!(!without.can_write());
    assert_eq!(without.operator().display_name, "mdegans");
    assert_eq!(without.suspension(), None);

    assert_eq!(
        Session::from_bootstrap(Err(ApiError::SignedOut(
            "not signed in".into()
        ))),
        Session::SignedOut
    );
    for e in [
        ApiError::Network("offline".into()),
        decode_error(),
        ApiError::Status {
            status: 502,
            message: "no message".into(),
            code: None,
        },
        plain_403(),
    ] {
        assert_eq!(Session::from_bootstrap(Err(e.clone())), Session::Failed(e));
    }
}

/// The bootstrap says when the session may not write (agentkit 0.171's
/// `suspension`): such a session draws no compose control
#[test]
fn a_suspended_bootstrap_cannot_write() {
    let suspended = Session::from_bootstrap(Ok(PortalSessionResponse {
        suspension: Some(Box::new(fixtures::suspension())),
        ..bootstrap(true)
    }));
    let s = used(&suspended);
    assert!(!s.can_write());
    assert_eq!(
        s.suspension().unwrap().ban_reason.as_deref(),
        Some("Art. IV test ban")
    );
    assert!(s.poster().is_some());
}

/// A refresh that fails other than with a 401 doesn't take a signed-in
/// page away; a 401 or a 200 replaces anything
#[test]
fn a_failed_refresh_keeps_signed_in() {
    let mut session = signed_in(true);
    session.after_refresh(Err(ApiError::Network("offline".into())));
    assert_eq!(session, signed_in(true));
    session.after_refresh(Err(ApiError::SignedOut("expired".into())));
    assert_eq!(session, Session::SignedOut);
    session.after_refresh(Ok(bootstrap(false)));
    assert_eq!(session, signed_in(false));
    let mut session = Session::SignedOut;
    session.after_refresh(Err(ApiError::Network("offline".into())));
    assert_eq!(
        session,
        Session::Failed(ApiError::Network("offline".into()))
    );
}

/// The bootstrap is authoritative on standing: a suspension learned from a
/// refusal stays until the next bootstrap, which replaces it (a lifted one
/// shows at once)
#[test]
fn a_refresh_replaces_a_learned_suspension() {
    let mut session = signed_in(true);
    let write = used(&session);
    session.after_write_error(&write, &banned());
    assert!(!can_write(&session));
    session.after_refresh(Ok(bootstrap(true)));
    assert!(can_write(&session));
}

#[test]
fn a_401_on_a_write_signs_out() {
    for poster in [true, false] {
        let mut session = signed_in(poster);
        let write = used(&session);
        let follow_up = session
            .after_write_error(&write, &ApiError::SignedOut("expired".into()));
        assert_eq!(session, Session::SignedOut);
        assert_eq!(follow_up, FollowUp::Nothing);
    }
}

/// A refusal of an older session's write doesn't touch a newer one
#[test]
fn an_older_sessions_refusal_leaves_the_new_one() {
    let old = used(&signed_in(true));
    let newer =
        Session::from_bootstrap(Ok(bootstrap_with(true, "newer-token")));

    let mut session = newer.clone();
    assert_eq!(
        session.after_write_error(&old, &banned()),
        FollowUp::Nothing
    );
    assert_eq!(session, newer);
    assert!(can_write(&session));

    let mut session = newer.clone();
    assert_eq!(
        session.after_write_error(&old, &ApiError::SignedOut("expired".into())),
        FollowUp::Refresh
    );
    assert_eq!(session, newer);
}

#[test]
fn a_suspension_stops_writes() {
    let mut session = signed_in(true);
    let write = used(&session);
    assert_eq!(
        session.after_write_error(&write, &banned()),
        FollowUp::Nothing
    );
    let signed_in = used(&session);
    assert!(!signed_in.can_write());
    assert_eq!(signed_in.poster().unwrap().name.as_str(), "steward");
}

#[test]
fn another_403_or_a_404_refreshes() {
    for e in [
        plain_403(),
        ApiError::Status {
            status: 404,
            message: "you have no operator poster".into(),
            code: None,
        },
    ] {
        let mut session = signed_in(true);
        let write = used(&session);
        assert_eq!(
            session.after_write_error(&write, &e),
            FollowUp::Refresh,
            "{e}"
        );
        assert_eq!(session, signed_in(true), "{e}");
    }
}

#[test]
fn other_refusals_leave_the_session() {
    for e in [
        ApiError::Network("offline".into()),
        decode_error(),
        ApiError::Status {
            status: 400,
            message: "unknown field `x`".into(),
            code: None,
        },
        ApiError::Status {
            status: 409,
            message: "this action id was already used for a different request"
                .into(),
            code: None,
        },
    ] {
        let mut session = signed_in(true);
        let write = used(&session);
        assert_eq!(
            session.after_write_error(&write, &e),
            FollowUp::Nothing,
            "{e}"
        );
        assert_eq!(session, signed_in(true), "{e}");
    }
}

/// The CSRF token is a credential: `Debug` never prints it
#[test]
fn debug_hides_the_token() {
    let mut session = signed_in(true);
    let write = used(&session);
    session.after_write_error(&write, &banned());
    let printed = format!("{session:?} {:#?}", signed_in(false));
    assert!(!printed.contains(CSRF), "{printed}");
    assert!(printed.contains("[REDACTED]"), "{printed}");
    assert!(printed.contains("mdegans"), "{printed}");
}

#[test]
fn sign_in_returns_here() {
    assert_eq!(sign_in_href(None), "/agora/login");
    assert_eq!(
        sign_in_href(Some("/agora/app?x=1&y=2")),
        "/agora/login?next=%2Fagora%2Fapp%3Fx%3D1%26y%3D2"
    );
}

/// What [`SessionStatusView`] draws for `session`
fn draw(session: Session) -> String {
    #[component]
    fn Drawn(session: Session) -> Element {
        rsx! {
            SessionStatusView {
                session,
                return_to: Some("/agora/app".to_owned()),
                on_retry: |_| {},
            }
        }
    }
    let mut dom = VirtualDom::new_with_props(Drawn, DrawnProps { session });
    dom.rebuild_in_place();
    let html = dioxus_ssr::render(&dom);
    assert!(!html.contains(CSRF), "{html}");
    assert_eq!(html.matches(r#"role="status""#).count(), 1, "{html}");
    html
}

#[test]
fn draws_each_state() {
    assert!(draw(Session::Loading).contains("Checking who is signed in…"));
    let out = draw(Session::SignedOut);
    assert!(out.contains("Not signed in."), "{out}");
    assert!(
        out.contains(r#"href="/agora/login?next=%2Fagora%2Fapp""#),
        "{out}"
    );
    let failed = draw(Session::Failed(ApiError::Network("offline".into())));
    assert!(
        failed.contains("Couldn&#39;t check who is signed in: Agora couldn&#39;t be reached (offline)"),
        "{failed}"
    );
    assert!(failed.contains(">Try again</button>"), "{failed}");
    let poster = draw(signed_in(true));
    assert!(
        poster.contains("Signed in as mdegans, posting as "),
        "{poster}"
    );
    assert!(
        poster.contains(r#"<a href="/agora/agent/steward">steward</a>"#),
        "{poster}"
    );
    let none = draw(signed_in(false));
    assert!(
        none.contains(
            "You have no operator poster yet, so you can&#39;t post here."
        ),
        "{none}"
    );
    let mut session = signed_in(true);
    let write = used(&session);
    session.after_write_error(&write, &banned());
    let html = draw(session);
    assert!(
        html.contains("Your account is suspended: you can read, but not post."),
        "{html}"
    );
    assert!(
        html.contains(r#"href="https://subliminal.technology/agora/protocol""#),
        "{html}"
    );
    assert!(!html.contains("posting as"), "{html}");
}

/// A dom whose root provides the session over `portal`, and two readers
fn two_readers(portal: FakePortal) -> VirtualDom {
    #[component]
    fn Root() -> Element {
        rsx! {
            SessionStatus {}
            div { SessionStatus {} }
        }
    }
    let dom = VirtualDom::new(Root);
    provide(&dom, portal);
    dom
}

fn settle(dom: &mut VirtualDom) {
    for _ in 0..4 {
        dom.render_immediate(&mut NoOpMutations);
        dom.process_events();
    }
}

/// The bootstrap is fetched after the first render, once, whatever the
/// number of readers; the first render draws "loading"
#[test]
fn the_bootstrap_starts_after_the_first_render_once() {
    let portal = FakePortal::signed_in(true);
    let mut dom = two_readers(portal.clone());
    dom.rebuild_in_place();
    assert_eq!(
        portal.0.borrow().session_calls,
        0,
        "fetched while rendering"
    );
    assert_eq!(
        dioxus_ssr::render(&dom)
            .matches("Checking who is signed in…")
            .count(),
        2
    );
    settle(&mut dom);
    assert_eq!(portal.0.borrow().session_calls, 1);
    let html = dioxus_ssr::render(&dom);
    assert_eq!(html.matches("Signed in as mdegans").count(), 2, "{html}");
}

/// A page that draws no reader fetches nothing
#[test]
fn no_reader_no_fetch() {
    fn empty() -> Element {
        rsx! {}
    }
    let portal = FakePortal::signed_in(true);
    let mut dom = VirtualDom::new(empty);
    provide(&dom, portal.clone());
    dom.rebuild_in_place();
    settle(&mut dom);
    assert_eq!(portal.0.borrow().session_calls, 0);
}

/// Where no session is provided (the server), a reader that tolerates it
/// draws nothing and fetches nothing
#[test]
fn no_session_on_the_server() {
    #[component]
    fn Reader() -> Element {
        match try_use_session() {
            Some(_) => rsx! { "session" },
            None => rsx! {},
        }
    }
    let mut dom = VirtualDom::new(Reader);
    dom.rebuild_in_place();
    settle(&mut dom);
    assert_eq!(dioxus_ssr::render(&dom), "");
}

fn dom_and_handle(portal: FakePortal) -> (VirtualDom, SessionHandle) {
    fn empty() -> Element {
        rsx! {}
    }
    let mut dom = VirtualDom::new(empty);
    dom.rebuild_in_place();
    let handle = dom
        .in_scope(ScopeId::ROOT, || SessionHandle::new_root(Rc::new(portal)));
    (dom, handle)
}

fn state(dom: &VirtualDom, handle: SessionHandle) -> Session {
    dom.in_scope(ScopeId::ROOT, || handle.state.peek().clone())
}

fn set(dom: &VirtualDom, handle: SessionHandle, session: Session) {
    dom.in_scope(ScopeId::ROOT, || handle.replace(session));
}

/// A failing refresh keeps a signed-in page signed in
#[test]
fn a_failing_refresh_keeps_signed_in() {
    let (mut dom, handle) = dom_and_handle(FakePortal::default());
    set(&dom, handle, signed_in(true));
    dom.in_scope(ScopeId::ROOT, || handle.refresh());
    dom.process_events();
    assert_eq!(state(&dom, handle), signed_in(true));
}

/// An answer to a refresh that a later one overtook is dropped
#[test]
fn a_stale_refresh_answer_is_dropped() {
    let (dom, handle) = dom_and_handle(FakePortal::default());
    dom.in_scope(ScopeId::ROOT, || {
        let first = handle.begin_refresh();
        let second = handle.begin_refresh();
        handle.finish_refresh(second, Ok(bootstrap(true)));
        handle.finish_refresh(first, Err(ApiError::SignedOut("old".into())));
    });
    assert_eq!(state(&dom, handle), signed_in(true));
}

/// A write's refusal doesn't cancel a refresh in flight
#[test]
fn a_write_refusal_doesnt_drop_a_refresh() {
    let (dom, handle) = dom_and_handle(FakePortal::default());
    set(&dom, handle, signed_in(true));
    let write = used(&signed_in(true));
    dom.in_scope(ScopeId::ROOT, || {
        let refresh = handle.begin_refresh();
        handle.note_write_error(&write, &banned());
        handle.finish_refresh(refresh, Ok(bootstrap_with(true, "newer-token")));
    });
    assert_eq!(
        state(&dom, handle),
        Session::from_bootstrap(Ok(bootstrap_with(true, "newer-token")))
    );
}

/// A refused write through the handle updates the session (a 401 signs
/// out) before its caller hears the answer
#[test]
fn a_refused_write_updates_the_session_first() {
    let portal = FakePortal::signed_in(true);
    portal
        .0
        .borrow_mut()
        .comment_answers
        .push_back(Err(ApiError::SignedOut("expired".into())));
    let (mut dom, handle) = dom_and_handle(portal.clone());
    set(&dom, handle, signed_in(true));
    let heard = Rc::new(std::cell::RefCell::new(None));
    let seen = heard.clone();
    dom.in_scope(ScopeId::ROOT, || {
        handle.create_comment(
            used(&signed_in(true)),
            PortalCommentRequest {
                action_id: crate::ids::OperatorActionId::new(),
                payload: crate::requests::CreateCommentInput {
                    reply_to: crate::ids::ContentTarget::Id(
                        fixtures::post().id.into(),
                    ),
                    body: "hello".into(),
                },
            },
            move |answer| {
                *seen.borrow_mut() = Some((answer, state_now(handle)));
            },
        )
    });
    dom.process_events();
    let (answer, then) = heard.borrow_mut().take().expect("answered");
    assert_eq!(answer, Err(ApiError::SignedOut("expired".into())));
    assert_eq!(then, Session::SignedOut);
    assert_eq!(portal.0.borrow().comments.len(), 1);
}

fn state_now(handle: SessionHandle) -> Session {
    handle.state.peek().clone()
}
