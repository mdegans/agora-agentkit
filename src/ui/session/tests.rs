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
    let handle = dom.in_scope(ScopeId::ROOT, || {
        let portal: Rc<dyn Portal> = Rc::new(portal);
        let drafts = crate::ui::compose::Drafts::new_root(portal.clone());
        SessionHandle::new_root(
            portal,
            drafts,
            crate::ui::inbox::InboxStore::new_root(),
        )
    });
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

// -- The check window (W3) ------------------------------------------------

use crate::ui::test_portal::{check_needed, window_options};

fn coded(status: u16, code: RefusalCode, message: &str) -> ApiError {
    ApiError::refused(status, ErrorResponse::coded(code, message).into())
}

/// The bootstrap's `check_needed` is a session outside its window: not
/// signed out, not failed. A failing refresh keeps it (the tap stays on
/// offer); a 200 or a 401 replace it
#[test]
fn outside_the_window_is_its_own_state() {
    assert_eq!(
        Session::from_bootstrap(Err(check_needed())),
        Session::CheckNeeded(None)
    );
    let mut s = Session::CheckNeeded(None);
    s.after_refresh(Err(ApiError::Network("offline".into())));
    assert_eq!(s, Session::CheckNeeded(None));
    s.after_refresh(Ok(bootstrap(true)));
    assert_eq!(s, signed_in(true));
    let mut s = Session::CheckNeeded(None);
    s.after_refresh(Err(ApiError::SignedOut("expired".into())));
    assert_eq!(s, Session::SignedOut);
}

/// A request refused `check_needed` puts this session outside its window,
/// asking nothing (the refusal says it all); an older session's refusal
/// asks again. Never signed out
#[test]
fn a_check_needed_refusal_keeps_the_session() {
    let mut s = signed_in(true);
    let write = used(&s);
    assert_eq!(
        s.after_write_error(&write, &check_needed()),
        FollowUp::Nothing
    );
    assert_eq!(s, Session::CheckNeeded(None));
    let mut newer = Session::from_bootstrap(Ok(bootstrap_with(true, "newer")));
    assert_eq!(
        newer.after_write_error(&write, &check_needed()),
        FollowUp::Refresh
    );
    assert!(can_write(&newer));
}

/// The session line outside a window: still signed in, what happened,
/// what is kept, and the tap in place. No sign-in link, no way to the
/// sign-in page
#[test]
fn the_line_outside_a_window_offers_the_tap_not_a_sign_in() {
    #[component]
    fn Drawn() -> Element {
        rsx! {
            SessionStatusView {
                session: Session::CheckNeeded(None),
                return_to: Some("/agora/app".to_owned()),
                on_retry: |_| {},
                check: rsx! {
                    WindowTapView {
                        tap: WindowTap::Idle,
                        focus: false,
                        on_start: |_| {},
                        on_confirm: |_| {},
                        on_cancel: |_| {},
                    }
                },
            }
        }
    }
    let mut dom = VirtualDom::new(Drawn);
    dom.rebuild_in_place();
    let html = dioxus_ssr::render(&dom).replace("&#39;", "'");
    assert!(html.contains("You're still signed in"), "{html}");
    assert!(html.contains("your drafts are kept"), "{html}");
    assert!(html.contains(">Confirm with a passkey</button>"), "{html}");
    assert!(!html.contains("login"), "{html}");
    assert!(!html.contains("Sign in"), "{html}");
}

/// When the window ends, in the bootstrap's words: one time when the idle
/// end is the absolute one, both otherwise
#[test]
fn the_window_is_said_with_its_times() {
    let window = bootstrap(true).window;
    assert_eq!(
        window_line(&window),
        "Passkey check open until Oct 30, 2025 at 12:00 UTC, later if you \
         keep using Agora (at most until Nov 01, 2025 at 00:00 UTC)."
    );
    let last = crate::operator::CheckWindowStatus {
        idle_expires_at: window.expires_at,
        ..window
    };
    assert_eq!(
        window_line(&last),
        "Passkey check open until Nov 01, 2025 at 00:00 UTC."
    );
    assert!(draw(signed_in(true)).contains("Passkey check open until"));
}

/// The session line, read from the context, over `portal`
fn line(portal: FakePortal) -> (VirtualDom, SessionHandle) {
    fn root() -> Element {
        rsx! { SessionStatus {} }
    }
    let mut dom = VirtualDom::new(root);
    provide(&dom, portal);
    dom.rebuild_in_place();
    settle(&mut dom);
    let handle = dom.in_scope(ScopeId::ROOT, consume_context::<SessionHandle>);
    (dom, handle)
}

fn html(dom: &VirtualDom) -> String {
    dioxus_ssr::render(dom).replace("&#39;", "'")
}

/// The tap in place: "Confirm with a passkey" asks for the 0x15 tap, "Tap
/// your passkey to confirm" opens the prompt with its options, and its
/// answer opens the window; the bootstrap is fetched again and the page
/// carries on signed in, with no sign-in anywhere on the way
#[test]
fn the_tap_in_place_opens_a_window() {
    let portal = FakePortal::check_needed();
    let (mut dom, handle) = line(portal.clone());
    let page = html(&dom);
    assert!(page.contains("You're still signed in"), "{page}");
    assert!(page.contains(">Confirm with a passkey</button>"), "{page}");
    dom.in_scope(ScopeId::ROOT, || handle.start_window_tap());
    settle(&mut dom);
    assert_eq!(portal.0.borrow().window_taps, 1);
    let page = html(&dom);
    assert!(
        page.contains(">Tap your passkey to confirm</button>"),
        "{page}"
    );
    assert!(page.contains(">Cancel</button>"), "{page}");
    assert!(
        portal.0.borrow().prompts.is_empty(),
        "no prompt before the press"
    );
    dom.in_scope(ScopeId::ROOT, || handle.confirm_window_tap());
    settle(&mut dom);
    {
        let fake = portal.0.borrow();
        assert_eq!(fake.prompts, vec![window_options()]);
        assert_eq!(fake.opens.len(), 1);
        assert_eq!(
            fake.opens[0].assertion,
            crate::ui::test_portal::assertion()
        );
        // The bootstrap, the status before the tap, and the bootstrap
        // again after the open
        assert_eq!(fake.session_calls, 3);
    }
    assert_eq!(state(&dom, handle), signed_in(true));
    assert_eq!(handle.tap.peek().clone(), WindowTap::Idle);
    let page = html(&dom);
    assert!(page.contains("Signed in as mdegans"), "{page}");
    assert!(!page.contains("login"), "{page}");
}

/// Cancel before the prompt: nothing opened, the first button back
#[test]
fn the_tap_can_be_cancelled() {
    let portal = FakePortal::check_needed();
    let (mut dom, handle) = line(portal.clone());
    dom.in_scope(ScopeId::ROOT, || handle.start_window_tap());
    settle(&mut dom);
    dom.in_scope(ScopeId::ROOT, || handle.cancel_window_tap());
    settle(&mut dom);
    assert!(html(&dom).contains(">Confirm with a passkey</button>"));
    assert!(portal.0.borrow().opens.is_empty());
    assert_eq!(state(&dom, handle), Session::CheckNeeded(None));
}

/// The start's refusal, the finish's, the browser's failure, and what the
/// page says
type TapCase = (
    Option<ApiError>,
    Option<ApiError>,
    Option<TapFailed>,
    &'static str,
);

/// Every way the tap can fail, and what the page says: each leaves the
/// session outside its window with the tap on offer, except a 401 (the
/// session itself ended: signed out) and a finish with no answer (the
/// bootstrap is asked what it opened)
#[test]
fn every_refusal_of_the_tap_is_said() {
    let lost = "This connection doesn't tell Agora which network you are \
                on, so it can't open a check window here.";
    // (start's answer, open's answer, the browser's, what the page says)
    let cases: Vec<TapCase> = vec![
        (
            None,
            Some(coded(403, RefusalCode::NetworkChanged, "x")),
            None,
            "Your network changed between asking for the passkey check and \
             tapping, so Agora opened nothing.",
        ),
        // The server's own words: written for operators, and only it knows
        // which of the reasons it was
        (
            None,
            Some(coded(
                403,
                RefusalCode::TapRefused,
                "This passkey's signature counter went backwards. Nothing was opened.",
            )),
            None,
            "This passkey's signature counter went backwards. Nothing was opened.",
        ),
        (
            Some(ApiError::Forbidden(ErrorResponse::new(lost).into())),
            None,
            None,
            lost,
        ),
        (
            Some(coded(400, RefusalCode::NoPasskey, "No passkey in effect.")),
            None,
            None,
            "No passkey in effect.",
        ),
        (
            Some(ApiError::Network("offline".into())),
            None,
            None,
            "Agora couldn't be asked for the check (Agora couldn't be \
             reached (offline)). Nothing was opened: try again.",
        ),
        (
            None,
            None,
            Some(TapFailed::Cancelled),
            "No check was made: the passkey prompt was cancelled or timed \
             out. Nothing was opened, and nothing here was lost.",
        ),
    ];
    for (start, open, browser, words) in cases {
        let portal = FakePortal::check_needed();
        {
            let mut fake = portal.0.borrow_mut();
            if let Some(e) = start.clone() {
                fake.window_tap_answers.push_back(Err(e));
            }
            if let Some(e) = open.clone() {
                fake.open_answers.push_back(Err(e));
            }
            if let Some(f) = browser.clone() {
                fake.passkey_answers.push_back(Err(f));
            }
        }
        let (mut dom, handle) = line(portal.clone());
        dom.in_scope(ScopeId::ROOT, || handle.start_window_tap());
        settle(&mut dom);
        dom.in_scope(ScopeId::ROOT, || handle.confirm_window_tap());
        settle(&mut dom);
        let page = html(&dom);
        assert!(page.contains(words), "{words}\n{page}");
        assert!(page.contains(">Confirm with a passkey</button>"), "{page}");
        assert!(page.contains(r#"role="alert""#), "{page}");
        assert_eq!(state(&dom, handle), Session::CheckNeeded(None), "{words}");
        assert!(!page.contains("login"), "{page}");
    }

    // The session ended: signed out, the sign-in offered
    let portal = FakePortal::check_needed();
    portal
        .0
        .borrow_mut()
        .window_tap_answers
        .push_back(Err(ApiError::SignedOut("expired".into())));
    let (mut dom, handle) = line(portal.clone());
    dom.in_scope(ScopeId::ROOT, || handle.start_window_tap());
    settle(&mut dom);
    assert_eq!(state(&dom, handle), Session::SignedOut);

    // No answer to the finish: it may have opened, and the bootstrap says
    // it did
    let portal = FakePortal::check_needed();
    portal
        .0
        .borrow_mut()
        .open_answers
        .push_back(Err(ApiError::Network("offline".into())));
    let (mut dom, handle) = line(portal.clone());
    dom.in_scope(ScopeId::ROOT, || handle.start_window_tap());
    settle(&mut dom);
    portal.0.borrow_mut().session = Some(Ok(bootstrap(true)));
    dom.in_scope(ScopeId::ROOT, || handle.confirm_window_tap());
    settle(&mut dom);
    assert_eq!(state(&dom, handle), signed_in(true));
    // ... or that it didn't, which the page says
    let portal = FakePortal::check_needed();
    portal
        .0
        .borrow_mut()
        .open_answers
        .push_back(Err(ApiError::Network("offline".into())));
    let (mut dom, handle) = line(portal.clone());
    dom.in_scope(ScopeId::ROOT, || handle.start_window_tap());
    settle(&mut dom);
    dom.in_scope(ScopeId::ROOT, || handle.confirm_window_tap());
    settle(&mut dom);
    assert_eq!(state(&dom, handle), Session::CheckNeeded(None));
    let page = html(&dom);
    assert!(
        page.contains(
            "Agora didn't answer the tap (Agora couldn't be reached \
             (offline)), and no check window is open: nothing was opened."
        ),
        "the finish's own error, not a guess: {page}"
    );
    assert!(!page.contains("network may have changed"), "{page}");

    // A 503 (the server busy) at the finish, the same
    let portal = FakePortal::check_needed();
    portal
        .0
        .borrow_mut()
        .open_answers
        .push_back(Err(ApiError::Status {
            status: 503,
            message: "busy".into(),
            code: None,
        }));
    let (mut dom, handle) = line(portal.clone());
    dom.in_scope(ScopeId::ROOT, || handle.start_window_tap());
    settle(&mut dom);
    dom.in_scope(ScopeId::ROOT, || handle.confirm_window_tap());
    settle(&mut dom);
    let page = html(&dom);
    assert!(
        page.contains("Agora didn't answer the tap (Agora answered 503: busy)"),
        "{page}"
    );
}

/// The page never decides from its own clock that a window has ended: it
/// asks the server at the window's end (by the bootstrap's times, plus
/// `PROBE_AFTER_END`), and shows what the server says. Here it says the
/// window ended: the tap in place
#[test]
fn the_page_asks_the_server_at_the_windows_end() {
    let portal = FakePortal::signed_in(true);
    let (mut dom, handle) = line(portal.clone());
    let window = bootstrap(true).window;
    {
        let fake = portal.0.borrow();
        assert_eq!(fake.waits.len(), 1, "one probe set");
        let expected = (window.ends_at() + PROBE_AFTER_END - window.now)
            .to_std()
            .unwrap();
        assert_eq!(fake.waits[0].0, expected);
        assert_eq!(fake.session_calls, 1);
    }
    // Until it fires, the page asks nothing, whatever its clock
    portal.0.borrow_mut().now = Some(window.expires_at + TimeDelta::days(1));
    settle(&mut dom);
    assert_eq!(portal.0.borrow().session_calls, 1);
    assert_eq!(state(&dom, handle), signed_in(true));
    portal.0.borrow_mut().session = Some(Err(check_needed()));
    portal.fire(0);
    settle(&mut dom);
    assert_eq!(portal.0.borrow().session_calls, 2);
    assert_eq!(state(&dom, handle), Session::CheckNeeded(None));
    assert!(html(&dom).contains(">Confirm with a passkey</button>"));
    assert_eq!(
        portal.0.borrow().waits.len(),
        1,
        "no probe outside a window"
    );
}

/// A probe the server admits was a use of the window: the page probes
/// again only if it used the window itself since, so a clock running ahead
/// can't keep a window open by probing
#[test]
fn a_probe_alone_never_keeps_the_window_open() {
    // Not used since: no further probe
    let portal = FakePortal::signed_in(true);
    let (mut dom, _handle) = line(portal.clone());
    portal.fire(0);
    settle(&mut dom);
    assert_eq!(portal.0.borrow().session_calls, 2, "the probe asked");
    assert_eq!(portal.0.borrow().waits.len(), 1, "and set none after");

    // Used since (a write answered): the next probe is set
    let portal = FakePortal::signed_in(true);
    let (mut dom, handle) = line(portal.clone());
    dom.in_scope(ScopeId::ROOT, || handle.note_used());
    portal.fire(0);
    settle(&mut dom);
    assert_eq!(portal.0.borrow().waits.len(), 2, "the next probe set");

    // An answered write through the handle is a use
    let portal = FakePortal::signed_in(true);
    let (mut dom, handle) = line(portal.clone());
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
            |_| {},
        )
    });
    settle(&mut dom);
    portal.fire(0);
    settle(&mut dom);
    assert_eq!(portal.0.borrow().waits.len(), 2, "the write was a use");
}

// -- The fix round: probes that fail, a window opened elsewhere ---------------

/// A probe that gets no answer never ends probing: it is set again with a
/// backoff, the page stays as it was while it can't know for a short
/// while, then stops showing what is the operator's alone and says Agora
/// couldn't be reached; and the first answer puts it back
#[test]
fn a_probe_that_fails_asks_again_and_never_guesses() {
    let portal = FakePortal::signed_in(true);
    let (mut dom, handle) = line(portal.clone());
    let start = bootstrap(true).window.ends_at() + PROBE_AFTER_END;
    portal.0.borrow_mut().now = Some(start);
    portal.0.borrow_mut().session =
        Some(Err(ApiError::Network("offline".into())));
    portal.fire_last();
    settle(&mut dom);
    assert_eq!(state(&dom, handle), signed_in(true), "not yet: a moment");
    let waits: Vec<_> = portal.0.borrow().waits.iter().map(|w| w.0).collect();
    assert_eq!(waits.len(), 2, "set again, not dropped: {waits:?}");
    assert_eq!(waits[1], Duration::from_secs(5));

    portal.0.borrow_mut().now = Some(start + PROBE_UNKNOWN_FOR);
    portal.fire_last();
    settle(&mut dom);
    assert!(
        matches!(state(&dom, handle), Session::Failed(_)),
        "unknown for too long: nothing of the operator's on a guess"
    );
    let page = html(&dom);
    assert!(
        page.contains("Agora couldn't be reached (offline)"),
        "{page}"
    );
    assert!(!page.contains("Signed in as"), "{page}");
    let waits: Vec<_> = portal.0.borrow().waits.iter().map(|w| w.0).collect();
    assert_eq!(waits.len(), 3, "still asking: {waits:?}");
    assert_eq!(waits[2], Duration::from_secs(10), "backing off");

    // A 5xx is no answer either
    portal.0.borrow_mut().session = Some(Err(ApiError::Status {
        status: 502,
        message: "bad gateway".into(),
        code: None,
    }));
    portal.fire_last();
    settle(&mut dom);
    assert_eq!(portal.0.borrow().waits.len(), 4);

    // Back: the server's answer is what the page shows
    portal.0.borrow_mut().session = Some(Err(check_needed()));
    portal.fire_last();
    settle(&mut dom);
    assert_eq!(state(&dom, handle), Session::CheckNeeded(None));
}

/// The page shown again (a tab brought back, a page from the back-forward
/// cache) asks at once: a window another tab opened carries this page on,
/// and one that ended while it was hidden shows the tap
#[test]
fn being_shown_again_asks_the_server() {
    let portal = FakePortal::check_needed();
    let (mut dom, handle) = line(portal.clone());
    assert_eq!(state(&dom, handle), Session::CheckNeeded(None));
    portal.0.borrow_mut().session = Some(Ok(bootstrap(true)));
    portal.show_again();
    settle(&mut dom);
    assert_eq!(
        state(&dom, handle),
        signed_in(true),
        "opened in another tab"
    );
    portal.0.borrow_mut().session = Some(Err(check_needed()));
    portal.show_again();
    settle(&mut dom);
    assert_eq!(
        state(&dom, handle),
        Session::CheckNeeded(None),
        "ended while hidden"
    );
}

/// "Confirm with a passkey" asks first whether a window is open: one that
/// another tab opened carries the page on, with no second ceremony
#[test]
fn no_ceremony_when_a_window_is_open_already() {
    let portal = FakePortal::check_needed();
    let (mut dom, handle) = line(portal.clone());
    portal.0.borrow_mut().session = Some(Ok(bootstrap(true)));
    dom.in_scope(ScopeId::ROOT, || handle.start_window_tap());
    settle(&mut dom);
    assert_eq!(state(&dom, handle), signed_in(true));
    assert_eq!(portal.0.borrow().window_taps, 0, "no tap asked for");
    assert!(portal.0.borrow().prompts.is_empty());
}

/// A refused finish asks the status too: a window another tab opened in
/// the meantime carries the page on, and the refusal isn't shown
#[test]
fn a_refused_finish_finds_a_window_opened_elsewhere() {
    let portal = FakePortal::check_needed();
    portal.0.borrow_mut().open_answers.push_back(Err(coded(
        403,
        RefusalCode::TapRefused,
        "replaced",
    )));
    let (mut dom, handle) = line(portal.clone());
    dom.in_scope(ScopeId::ROOT, || handle.start_window_tap());
    settle(&mut dom);
    portal.0.borrow_mut().session = Some(Ok(bootstrap(true)));
    dom.in_scope(ScopeId::ROOT, || handle.confirm_window_tap());
    settle(&mut dom);
    assert_eq!(state(&dom, handle), signed_in(true));
    assert!(!html(&dom).contains("replaced"));
}

/// A `check_needed` for a request made before a window opened since says
/// nothing of the window open now: the page asks, and stays open
#[test]
fn a_late_refusal_from_before_a_tap_doesnt_close_the_new_window() {
    let before = used(&signed_in(true));
    let mut reopened = bootstrap(true);
    reopened.window.opened_at += TimeDelta::hours(1);
    let mut now = Session::from_bootstrap(Ok(reopened));
    assert_eq!(
        now.after_write_error(&before, &check_needed()),
        FollowUp::Refresh
    );
    assert!(can_write(&now), "still open: {now:?}");
}

/// The probe is timed from the server's clock: a browser clock a day off
/// sets the same wait
#[test]
fn the_probe_is_timed_by_the_servers_clock() {
    let portal = FakePortal::signed_in(true);
    portal.0.borrow_mut().now =
        Some(bootstrap(true).window.now + TimeDelta::days(1));
    let (_dom, _handle) = line(portal.clone());
    let window = bootstrap(true).window;
    let expected = (window.ends_at() + PROBE_AFTER_END - window.now)
        .to_std()
        .unwrap();
    assert_eq!(portal.0.borrow().waits[0].0, expected);
}

/// Why there is no window, said truly for each reason the server gives;
/// with none, only what is true in every case
#[test]
fn the_reason_for_no_window_is_said() {
    use crate::enums::CheckWindowEnd;
    let line = |why| check_needed_line(why);
    assert!(
        line(None).contains("there is no passkey check open for this page")
    );
    assert!(!line(None).contains("ended"), "no claim it ended");
    assert!(
        line(Some(NoWindow::NeverOpened))
            .contains("hasn't had a passkey check yet")
    );
    for (ended, words) in [
        (CheckWindowEnd::Idle, "ended after a while unused"),
        (CheckWindowEnd::Absolute, "reached its time limit"),
        (CheckWindowEnd::Address, "because your network changed"),
        (
            CheckWindowEnd::Suspended,
            "your other passkeys can still confirm",
        ),
        (
            CheckWindowEnd::Frozen,
            "the passkeys the freeze left in effect can still confirm",
        ),
        (CheckWindowEnd::SignedOut, "signed out"),
        (CheckWindowEnd::Clock, "ahead of Agora's clock"),
    ] {
        let said = line(Some(NoWindow::Ended { ended }));
        assert!(said.contains(words), "{ended:?}: {said}");
    }
    assert!(line(Some(NoWindow::AddressUnusable)).contains("public address"),);
    // Read from the server's refusal
    let refusal = ApiError::refused(
        403,
        crate::errors::CheckNeeded::new(
            "x",
            NoWindow::Ended {
                ended: CheckWindowEnd::Suspended,
            },
        )
        .into(),
    );
    let s = Session::from_bootstrap(Err(refusal));
    assert_eq!(
        s,
        Session::CheckNeeded(Some(NoWindow::Ended {
            ended: CheckWindowEnd::Suspended
        }))
    );
    let page = draw_check_needed(s);
    assert!(
        page.contains("your other passkeys can still confirm"),
        "{page}"
    );
}

fn draw_check_needed(session: Session) -> String {
    #[component]
    fn Drawn(session: Session) -> Element {
        rsx! {
            SessionStatusView { session, return_to: None, on_retry: |_| {} }
        }
    }
    let mut dom = VirtualDom::new_with_props(Drawn, DrawnProps { session });
    dom.rebuild_in_place();
    dioxus_ssr::render(&dom)
}
