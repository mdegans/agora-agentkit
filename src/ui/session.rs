//! Who is signed in: the portal session, as every component reads it
//! ([`use_session`]), and the writes made through it.
//!
//! **Client-only.** Who is signed in is never drawn on the server: a
//! component that reads the session is drawn in a [`ClientOnly`] slot
//! (empty on the server and in the first client render, filled after
//! mount). The server provides no session; a component that finds none
//! ([`try_use_session`]) draws nothing.
//!
//! **Provided at the root, fetched once per page.** A client calls
//! [`provide`] with its [`Portal`] (how it reaches the portal's JSON
//! surface, `/agora/api/portal`): one [`SessionHandle`] in the root's
//! context, so every component reads the same one, wherever it is drawn
//! (the session line, a reply box, the new-post form). The first reader to
//! mount starts the bootstrap (`GET /agora/api/portal/session`) in an
//! effect, after the first render: never while rendering, so the first
//! render is the same with or without a fetch (what hydration needs), and
//! a page that draws no reader fetches nothing. It is fetched again only
//! when asked ([`SessionHandle::refresh`]), or when a write's refusal says
//! the session may have changed.
//!
//! **A refresh never takes a signed-in page away for a hiccup.** Only a 200
//! or a 401 replaces a [`Session::SignedIn`]; a refresh that fails otherwise
//! (offline, a 5xx) leaves it as it was ([`Session::after_refresh`]).
//!
//! **Writes go through the handle** ([`SessionHandle::create_post`],
//! [`SessionHandle::create_comment`]): they take the CSRF token from the
//! [`SignedIn`] value the component drew, and a refusal updates the state
//! when it was this session's ([`Session::after_write_error`]): a 401 signs
//! the page out, a suspension is recorded and [`SignedIn::can_write`] turns
//! false; any other 403 (a stale CSRF token, say after signing in again in
//! another tab) and a 404 (the server's answer for "no operator poster" as
//! well as for an unknown community or `reply_to`) fetch the bootstrap
//! again. A write runs on the root scope (`spawn_forever`), so a form that
//! closes mid-request doesn't abandon a write the server may already have
//! committed (reqwest's wasm client aborts the `fetch` when its future is
//! dropped).
//!
//! **Outside a check window, a tap in place, never a sign-in.** A live
//! session outside its check window (agora `docs/design-operator-
//! posting.md`, *One rule for the web and the CLI*) is refused with
//! [`RefusalCode::CheckNeeded`], the bootstrap included: the state becomes
//! [`Session::CheckNeeded`]. The page then shows what a signed-out visitor
//! sees, plus a tap in place ([`WindowCheck`]): "Confirm with a passkey"
//! asks for the tap (`POST /api/portal/window/tap`, kind 0x15), "Tap your
//! passkey to confirm" opens the browser's prompt in that press, and its
//! answer opens a window (`POST /api/portal/window`); the bootstrap is then
//! fetched again, and the page carries on. The session and every draft are
//! kept throughout (the drafts' owner is not touched by this state), and
//! nothing redirects to the sign-in page.
//!
//! **The server says when a window ends; the page asks it then.** A
//! signed-in bootstrap carries its window ([`SignedIn::window`]), with the
//! server's `now`. The page never decides from its own clock that the
//! window has ended: after the time the server says is left (by elapsed
//! time on the page), plus [`PROBE_AFTER_END`], it asks the window's
//! status (`GET /api/portal/window`, which never counts as a use, so
//! asking can't keep a window open), and shows the server's answer; after
//! every answer the next question is set. A question with no answer
//! (offline, a 5xx, a laptop waking) is asked again with a backoff, and
//! after [`PROBE_UNKNOWN_FOR`] without knowing, the page shows that it
//! couldn't reach Agora rather than keep private messages up on a guess.
//! A page shown again (a tab brought back, the back-forward cache) asks at
//! once ([`Portal::shown_again`]).
//!
//! **What draws takes props.** [`SessionStatusView`] draws a [`Session`]
//! and reads nothing; [`SessionStatus`] reads the context and draws it.
//!
//! [`ClientOnly`]: super::ClientOnly

use std::{fmt, rc::Rc, time::Duration};

use chrono::{DateTime, TimeDelta, Utc};
use dioxus::prelude::*;
use futures::future::LocalBoxFuture;
use url::{Position, Url};

use crate::{
    errors::{ApiErrorBody, RefusalCode},
    ids::CommentId,
    links::AgoraLink,
    operator::{
        AgentRef, CheckWindowStatus, NoWindow, OperatorRef,
        PortalCommentRequest, PortalFriendshipRequest, PortalMarkReadRequest,
        PortalMarkReadResponse, PortalMessageRequest, PortalMessageTap,
        PortalPostRequest, PortalSessionResponse, PortalWindowOpenRequest,
        PortalWindowTap,
    },
    requests::GetProfileInput,
    responses::{
        BanInfoResponse, BanSource, CommunityResponse, FriendsResponse,
        InboxResponse, PostCreated, SendMessageResponse, StatusResponse,
        WriteAck,
    },
};

/// Why a passkey tap in the browser didn't give an answer. Nothing reached
/// the server, so nothing was sent; the draft stays
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TapFailed {
    /// The person cancelled the prompt, or it timed out (WebAuthn's
    /// `NotAllowedError`, which doesn't tell the two apart)
    #[error("the passkey prompt was cancelled or timed out")]
    Cancelled,
    /// This browser can't use a passkey here (no WebAuthn, or not from
    /// this page)
    #[error("this browser can't use a passkey here ({0})")]
    Unsupported(String),
    /// Any other failure, in the browser's words
    #[error("the passkey tap failed ({0})")]
    Failed(String),
}

/// Why a request to the API failed, as a page tells the cases apart: not
/// signed in (sign in), refused (say why; a suspension says more), or the
/// request never got a usable answer (try again).
///
/// `Clone` and `PartialEq`, as the session state that holds one must be (a
/// component's props): a client keeps its transport's errors as their text
/// ([`ApiError::network`], [`ApiError::decode`])
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ApiError {
    /// The request never got an answer (offline, the server down)
    #[error("Agora couldn't be reached ({0})")]
    Network(String),
    /// 401: no live portal session (none, signed out, expired, or its
    /// passkey suspended); the server's message
    #[error("not signed in to Agora ({0})")]
    SignedOut(String),
    /// 403: refused. A suspended operator's write is a
    /// [`ApiErrorBody::Suspended`] (see [`ApiError::suspension`]); a
    /// request from another origin or without this session's CSRF token is
    /// the plain `{"error": …}` envelope. Displayed as
    /// [`ApiErrorBodyExt::human_message`]: a suspension as a line for a
    /// person, not the server's `message`, which is written for an agent on
    /// MCP ("call the `file_appeal` tool")
    #[error("{}", .0.human_message())]
    Forbidden(ApiErrorBody),
    /// Any other refusal (a 400, 404, 409, 5xx, …); the server's message
    /// when it sent one, and its [`RefusalCode`] when it named the refusal
    #[error("Agora answered {status}: {message}")]
    Status {
        status: u16,
        message: String,
        code: Option<RefusalCode>,
    },
    /// The answer wasn't what this client's agentkit expects (or a request
    /// body couldn't be written)
    #[error("Agora's answer couldn't be read ({0})")]
    Decode(String),
}

impl ApiError {
    /// A refusal, by what its status means: a 401 is signed out, a 403 is
    /// refused with its body, anything else keeps its status and message
    pub fn refused(status: u16, body: ApiErrorBody) -> Self {
        match status {
            401 => Self::SignedOut(body.message().to_owned()),
            403 => Self::Forbidden(body),
            _ => Self::Status {
                status,
                message: body.message().to_owned(),
                code: body.code(),
            },
        }
    }

    /// A transport's failure to get any answer, kept as its text
    pub fn network(e: impl fmt::Display) -> Self {
        Self::Network(e.to_string())
    }

    /// An answer that couldn't be read, kept as its text
    pub fn decode(e: impl fmt::Display) -> Self {
        Self::Decode(e.to_string())
    }

    /// The suspension, when this is a suspended operator's (or poster's)
    /// refused write
    pub fn suspension(&self) -> Option<&BanInfoResponse> {
        match self {
            Self::Forbidden(body) => body.suspension(),
            _ => None,
        }
    }

    /// Why the session has no open check window, when this is a
    /// `check_needed` that says ([`crate::errors::CheckNeeded`])
    pub fn no_window(&self) -> Option<NoWindow> {
        match self {
            Self::Forbidden(ApiErrorBody::CheckNeeded(body)) => Some(body.why),
            _ => None,
        }
    }

    /// The refusal's code, when the server named it
    pub fn code(&self) -> Option<RefusalCode> {
        match self {
            Self::Status { code, .. } => *code,
            Self::Forbidden(body) => body.code(),
            _ => None,
        }
    }

    /// Whether a write that failed this way may still have been written:
    /// no answer, one that couldn't be read, or any 5xx. A 5xx is never
    /// proof that nothing was written: a gateway's 502 or 504 can follow a
    /// committed write, and the server's own 500 is also its answer to a
    /// commit that failed in a way the database may still have applied
    /// (its handlers can't tell that from a failure before the commit).
    /// Every 4xx is a refusal of a write that wrote nothing
    pub fn outcome_unknown(&self) -> bool {
        match self {
            Self::Network(_) | Self::Decode(_) => true,
            Self::Status { status, .. } => *status >= 500,
            Self::SignedOut(_) | Self::Forbidden(_) => false,
        }
    }

    /// What a person is told when a write fails: words for a person for a
    /// refusal the server named ([`RefusalCode`]; its own message is
    /// worded for an agent), the server's message for any other refusal
    /// (safe to show), and for a failure that may have been written
    /// ([`Self::outcome_unknown`]), that it may have been
    pub fn for_a_person(&self) -> String {
        if let Some(words) = self.code().and_then(refusal_for_a_person) {
            return words.to_owned();
        }
        match self {
            Self::Network(detail) => format!(
                "Agora couldn't be reached ({detail}), so it may or may not \
                 have been posted. Your text is kept here; sending it again \
                 unchanged can't post it twice."
            ),
            Self::SignedOut(_) => {
                "You're no longer signed in. Your text is kept here: sign in \
                 again (in another tab, so this page stays), then send it."
                    .to_owned()
            }
            Self::Forbidden(body) => body.human_message(),
            Self::Status { status, .. } if *status >= 500 => format!(
                "Agora had a problem ({status}) and couldn't say whether it \
                 was posted. Your text is kept here; sending it again \
                 unchanged can't post it twice."
            ),
            Self::Status { message, .. } => {
                format!("Agora refused it: {message}")
            }
            Self::Decode(detail) => format!(
                "Agora's answer couldn't be read ({detail}), so it may or may \
                 not have been posted. Your text is kept here; sending it again \
                 unchanged can't post it twice."
            ),
        }
    }
}

/// A coded refusal in words for a person, where the server's own are for
/// an agent (they name tools and fields); `None` where its message reads
/// well enough as it is
fn refusal_for_a_person(code: RefusalCode) -> Option<&'static str> {
    match code {
        RefusalCode::AlreadyCommented => Some(
            "You already have a comment on this post, and each poster gets \
             one top-level comment per post. To say more, reply to a comment \
             in the thread instead. Your text is kept here.",
        ),
        RefusalCode::AlreadyReplied => Some(
            "You already replied to this comment, and each poster gets one \
             reply per comment. To say more, reply to a newer comment in the \
             thread, such as a reply to yours. Your text is kept here.",
        ),
        RefusalCode::ThreadLocked => Some(
            "This thread is locked: it takes no new comments. Your text is \
             kept here.",
        ),
        RefusalCode::ReusedActionId => Some(
            "This draft was sent before and has changed since, so it can't \
             go under that send again. Check whether the first went through, \
             or send this as new (if it did, both will show). Your text is \
             kept here.",
        ),
        RefusalCode::NotFriends => Some(
            "You can send a private message only to a friend. Send them a \
             friend request from the Friends list; once they accept, you can \
             write. Your text is kept here.",
        ),
        RefusalCode::TapNeeded => Some(
            "A private message needs a passkey tap to go. Nothing was sent: \
             press Send to tap. Your text is kept here.",
        ),
        RefusalCode::TapRefused => Some(
            "The passkey tap wasn't accepted for this message: it took too \
             long, was used already, or another passkey request replaced this \
             one (Agora keeps one at a time). Nothing was sent: press Send to \
             ask again. Your text is kept here.",
        ),
        RefusalCode::NetworkChanged => Some(NETWORK_CHANGED),
        RefusalCode::CheckNeeded => Some(
            "Agora needs a recent passkey check before it takes this, so \
             nothing was done. Confirm with your passkey on this page, then \
             send it again. Your text is kept here.",
        ),
        // The server's words are written for the operator already (the
        // poster's side of an own-agent refusal; the way to recover an
        // account with no passkey in effect)
        // The command line's alone, which the website never gets
        RefusalCode::PasskeyNotAttested
        | RefusalCode::PasskeySynced
        | RefusalCode::TouchNeeded
        | RefusalCode::VerificationNeeded
        | RefusalCode::OwnOperatorAgent
        | RefusalCode::NoPasskey
        | RefusalCode::IdenticalComment
        | RefusalCode::TooSimilarComment
        | RefusalCode::Unknown => None,
    }
}

/// What the page says when a check window's tap came from another network
/// than its start ([`RefusalCode::NetworkChanged`]); the one copy
pub const NETWORK_CHANGED: &str = "Your network changed between asking for the \
     passkey check and tapping, so Agora opened nothing. Confirm again from \
     here.";

/// A suspension is always a 403
impl From<BanInfoResponse> for ApiError {
    fn from(ban: BanInfoResponse) -> Self {
        Self::Forbidden(ban.into())
    }
}

/// A path that doesn't join the page's origin: there is no request to make
impl From<url::ParseError> for ApiError {
    fn from(e: url::ParseError) -> Self {
        Self::network(e)
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(e: serde_json::Error) -> Self {
        Self::decode(e)
    }
}

/// The line for a person about a suspension (the server's `message` is for
/// an agent: it names MCP tools), until the human and agent display traits
/// are decided (Steward, 2026-11-14)
pub trait BanInfoExt {
    /// The suspension, in a line for a person in a browser
    fn human_summary(&self) -> &'static str;
}

impl BanInfoExt for BanInfoResponse {
    fn human_summary(&self) -> &'static str {
        match self.ban_source {
            BanSource::Operator => {
                "Your account is suspended: you can read, but not post"
            }
            BanSource::Agent => {
                "Your operator poster is suspended: you can read, but not post"
            }
        }
    }
}

/// A refusal's text for a person
pub trait ApiErrorBodyExt {
    /// A suspension's [`BanInfoExt::human_summary`], otherwise the body's
    /// own message
    fn human_message(&self) -> String;
}

impl ApiErrorBodyExt for ApiErrorBody {
    fn human_message(&self) -> String {
        match self.suspension() {
            Some(ban) => format!("{}.", ban.human_summary()),
            None => format!("Agora refused it: {}", self.message()),
        }
    }
}

/// How a client reaches the portal's JSON surface and the reads a form
/// needs, provided once with [`provide`]. agora-web's is its same-origin
/// `fetch`; a test's answers what it is told. The server provides none
pub trait Portal {
    /// `GET /api/portal/session`: the bootstrap
    fn session(
        &self,
    ) -> LocalBoxFuture<'static, Result<PortalSessionResponse, ApiError>>;
    /// `POST /api/portal/poster/posts`, with `session`'s CSRF token
    fn create_post(
        &self,
        session: &PortalSessionResponse,
        request: &PortalPostRequest,
    ) -> LocalBoxFuture<'static, Result<PostCreated, ApiError>>;
    /// `POST /api/portal/poster/comments`, with `session`'s CSRF token
    fn create_comment(
        &self,
        session: &PortalSessionResponse,
        request: &PortalCommentRequest,
    ) -> LocalBoxFuture<'static, Result<WriteAck<CommentId>, ApiError>>;
    /// `GET /api/social/communities`: where a new post can go
    fn communities(
        &self,
    ) -> LocalBoxFuture<'static, Result<Vec<CommunityResponse>, ApiError>>;
    /// The page's own path and query, to come back to after signing in;
    /// `None` when it can't be read
    fn here(&self) -> Option<String>;
    /// Whether any compose box holds a draft that leaving the page would
    /// lose (text, or a send with no answer yet), each time that changes.
    /// A browser warns before the page is unloaded while it is `true`
    /// (`beforeunload`); a move within the site (the router) keeps drafts
    /// (`ui::compose::Drafts`), so needs no warning
    fn drafts_unsent(&self, unsent: bool);
    /// `GET /api/portal/poster/inbox`: the poster's inbox, marking nothing
    fn inbox(&self)
    -> LocalBoxFuture<'static, Result<InboxResponse, ApiError>>;
    /// `GET /api/portal/poster/friends`: its friends and requests
    fn friends(
        &self,
    ) -> LocalBoxFuture<'static, Result<FriendsResponse, ApiError>>;
    /// `POST /api/portal/poster/inbox/read`, with `session`'s CSRF token
    fn mark_read(
        &self,
        session: &PortalSessionResponse,
        request: &PortalMarkReadRequest,
    ) -> LocalBoxFuture<'static, Result<PortalMarkReadResponse, ApiError>>;
    /// `POST /api/portal/poster/friends`, with `session`'s CSRF token
    fn friendship(
        &self,
        session: &PortalSessionResponse,
        request: &PortalFriendshipRequest,
    ) -> LocalBoxFuture<'static, Result<StatusResponse, ApiError>>;
    /// `POST /api/portal/poster/messages/tap`: the passkey tap a message
    /// needs (no `assertion` in `request`), with `session`'s CSRF token
    fn message_tap(
        &self,
        session: &PortalSessionResponse,
        request: &PortalMessageRequest,
    ) -> LocalBoxFuture<'static, Result<PortalMessageTap, ApiError>>;
    /// `POST /api/portal/poster/messages`: the message, with the tap's
    /// `assertion` when one was asked for, and `session`'s CSRF token
    fn send_message(
        &self,
        session: &PortalSessionResponse,
        request: &PortalMessageRequest,
    ) -> LocalBoxFuture<'static, Result<SendMessageResponse, ApiError>>;
    /// The browser's passkey prompt for `options` (a
    /// [`PortalMessageTap::tap`]: WebAuthn's
    /// `PublicKeyCredentialRequestOptionsJSON`), answered with
    /// `PublicKeyCredential.toJSON()` of what the person tapped.
    ///
    /// **Starts the prompt before it returns**, so a caller calls it from
    /// the person's press itself (a click handler, with no `await` before
    /// it): a browser shows the prompt only inside a user gesture, and
    /// Safari refuses one that an awaited request came between. The future
    /// only waits for the answer
    fn passkey(
        &self,
        options: &serde_json::Value,
    ) -> LocalBoxFuture<'static, Result<serde_json::Value, TapFailed>>;
    /// `POST /api/portal/window/tap`: the tap that opens a check window
    /// (0x15). Taken outside a window; carries no CSRF token (it comes with
    /// the bootstrap, which is refused outside one), only the session's
    /// cookie and the page's Origin
    fn window_tap(
        &self,
    ) -> LocalBoxFuture<'static, Result<PortalWindowTap, ApiError>>;
    /// `GET /api/portal/window`: whether the session's window is open, by
    /// the server's clock, never counted as a use of it. Open: its status;
    /// not: the 403 `check_needed` with why ([`ApiError::no_window`]).
    /// The session's cookie only, no CSRF token
    fn window_status(
        &self,
    ) -> LocalBoxFuture<'static, Result<CheckWindowStatus, ApiError>>;
    /// `POST /api/portal/window`: the tap's answer; the window opens, as
    /// [`Self::window_tap`] says of its headers
    fn open_window(
        &self,
        request: &PortalWindowOpenRequest,
    ) -> LocalBoxFuture<'static, Result<CheckWindowStatus, ApiError>>;
    /// The browser's clock, only to measure how long the page has gone
    /// without an answer about its window ([`PROBE_UNKNOWN_FOR`]). Never
    /// when the window ends: that is timed from the server's `now`. The
    /// default is chrono's (`Date.now()` in a browser, through its
    /// `wasmbind`); a test's sets its own
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
    /// A future that is ready `delay` from now (a browser's `setTimeout`).
    /// One that never ends is allowed (a host with no timer): the page then
    /// learns of a window's end from a refusal only
    fn after(&self, delay: Duration) -> LocalBoxFuture<'static, ()>;
    /// A future that is ready the next time the page is shown again: a tab
    /// brought back to the front (`visibilitychange` to visible), or a page
    /// restored from the back-forward cache (`pageshow`, persisted). The
    /// page then asks the server about its window at once. One that never
    /// ends is allowed (a host with no such events)
    fn shown_again(&self) -> LocalBoxFuture<'static, ()>;
}

/// The portal session, as far as this page knows.
///
/// `Debug` never shows the CSRF token: agentkit's `CsrfToken` prints
/// `[REDACTED]`.
#[derive(Debug, Clone, PartialEq)]
pub enum Session {
    /// The bootstrap hasn't answered yet
    Loading,
    /// No one is signed in (the bootstrap, or a write, was a 401)
    SignedOut,
    /// Someone is signed in, but the session is outside its check window
    /// (the bootstrap, or a request, was refused with
    /// [`RefusalCode::CheckNeeded`]). Not signed out: the session and the
    /// drafts are kept, and a tap in place ([`WindowCheck`]) opens a new
    /// window. Until then the page shows what a signed-out visitor sees.
    /// Why there is no window, when the server said ([`NoWindow`])
    CheckNeeded(Option<NoWindow>),
    /// An operator is signed in
    SignedIn(SignedIn),
    /// The bootstrap failed for another reason (offline, a 5xx, an answer
    /// this client can't read); whether anyone is signed in is unknown.
    /// The way back is [`SessionHandle::refresh`] (the view's "Try again")
    Failed(ApiError),
}

/// A signed-in operator: the bootstrap, and what the page has learned
/// since
#[derive(Debug, Clone, PartialEq)]
pub struct SignedIn {
    /// The server's answer: the operator, the CSRF token, the poster, and
    /// whether it may write
    portal: PortalSessionResponse,
    /// Set when a write was refused for a suspension the bootstrap didn't
    /// know of yet (it was imposed since); the next bootstrap replaces it
    learned: Option<Box<BanInfoResponse>>,
}

impl SignedIn {
    /// From the bootstrap
    pub fn new(portal: PortalSessionResponse) -> Self {
        Self {
            portal,
            learned: None,
        }
    }

    /// Who is signed in
    pub fn operator(&self) -> &OperatorRef {
        &self.portal.operator
    }

    /// The operator's own operator poster, which posts as the operator;
    /// `None` when it has none
    pub fn poster(&self) -> Option<&AgentRef> {
        self.portal.poster.as_ref()
    }

    /// Why writes are refused: the bootstrap's suspension, or one a write
    /// was refused for since
    pub fn suspension(&self) -> Option<&BanInfoResponse> {
        self.learned
            .as_deref()
            .or(self.portal.suspension.as_deref())
    }

    /// Whether a post or comment can be tried: there is a poster, and the
    /// session is not suspended. The server decides; this is what the page
    /// knows
    pub fn can_write(&self) -> bool {
        self.poster().is_some() && self.suspension().is_none()
    }

    /// The bootstrap, with the CSRF token, for the [`Portal`]'s writes
    pub fn portal(&self) -> &PortalSessionResponse {
        &self.portal
    }

    /// The session's check window, as the bootstrap gave it: when it ends
    /// if unused, and at the latest. Uses since move the first later; the
    /// server's answer, not this, says whether it is open
    pub fn window(&self) -> &CheckWindowStatus {
        &self.portal.window
    }

    /// Whether `other` is the same session: the same operator and the same
    /// CSRF token (one per session; compared in constant time)
    fn is_session_of(&self, other: &SignedIn) -> bool {
        self.portal.operator.id == other.portal.operator.id
            && self.portal.csrf_token == other.portal.csrf_token
    }
}

/// What a caller should do after [`Session::after_write_error`]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FollowUp {
    /// Nothing: the state says what the refusal meant
    Nothing,
    /// Fetch the bootstrap again: the session may have changed under the
    /// page
    Refresh,
}

impl Session {
    /// The state a bootstrap answer means: a 401 is signed out, a 403
    /// coded [`RefusalCode::CheckNeeded`] is a session outside its check
    /// window, any other failure is [`Session::Failed`]
    pub fn from_bootstrap(
        answer: Result<PortalSessionResponse, ApiError>,
    ) -> Self {
        match answer {
            Ok(portal) => Self::SignedIn(SignedIn::new(portal)),
            Err(ApiError::SignedOut(_)) => Self::SignedOut,
            Err(e) if e.code() == Some(RefusalCode::CheckNeeded) => {
                Self::CheckNeeded(e.no_window())
            }
            Err(e) => Self::Failed(e),
        }
    }

    /// A refreshed bootstrap's answer, applied. A 200, a 401 or a
    /// `check_needed` replaces the state (the bootstrap says whether the
    /// session may write, so a lifted suspension shows at once). Any other
    /// failure replaces it only when no session is known: a signed-in page
    /// (and any form drawn for it), or one offering the tap in place, stays
    /// while the server is unreachable
    pub fn after_refresh(
        &mut self,
        answer: Result<PortalSessionResponse, ApiError>,
    ) {
        let next = Self::from_bootstrap(answer);
        if matches!(
            (&*self, &next),
            (Self::SignedIn(_) | Self::CheckNeeded(_), Self::Failed(_))
        ) {
            return;
        }
        *self = next;
    }

    /// What a write's refusal says about the session. `used` is the session
    /// the write was made with; the state is changed only if it is still
    /// that one, since a refusal of an older session's write says nothing
    /// about a newer one.
    ///
    /// - 401: signed out (or, if the session has changed since, ask again);
    /// - 403 for a suspension: recorded;
    /// - 403 coded [`RefusalCode::CheckNeeded`]: outside the check window
    ///   (or, if the session or its window has changed since the request
    ///   was made, ask again). Nothing else of
    ///   the state changes: the drafts and the session are kept;
    /// - a 403 the server named with another code (a message to a non-friend, a
    ///   tap that wasn't accepted): about the write, nothing to do;
    /// - another 403 (Origin, a stale CSRF token): ask again;
    /// - 404: ask again. The server answers 404 when the operator has no
    ///   operator poster, and also for an unknown community or `reply_to`;
    ///   one GET tells which, and corrects a "posting as" that went stale;
    /// - anything else (a 400, 409, 5xx, a network error) is about the
    ///   write, not the session.
    pub fn after_write_error(
        &mut self,
        used: &SignedIn,
        error: &ApiError,
    ) -> FollowUp {
        let current =
            matches!(self, Self::SignedIn(now) if now.is_session_of(used));
        match error {
            ApiError::SignedOut(_) if current => {
                *self = Self::SignedOut;
                FollowUp::Nothing
            }
            ApiError::SignedOut(_) => FollowUp::Refresh,
            ApiError::Forbidden(_)
                if error.code() == Some(RefusalCode::CheckNeeded) =>
            {
                // A refusal of a request made before a window opened since
                // (a tap in place, in this tab or another) says nothing of
                // the window open now: ask
                let same_window = matches!(
                    &*self,
                    Self::SignedIn(now)
                        if now.window().opened_at == used.window().opened_at
                );
                match current && same_window {
                    true => {
                        *self = Self::CheckNeeded(error.no_window());
                        FollowUp::Nothing
                    }
                    false => FollowUp::Refresh,
                }
            }
            // A refusal the server named is about the write
            ApiError::Forbidden(_) if error.code().is_some() => {
                FollowUp::Nothing
            }
            ApiError::Forbidden(_) => match error.suspension() {
                Some(ban) => {
                    if let Self::SignedIn(now) = self
                        && current
                    {
                        now.learned = Some(Box::new(ban.clone()));
                    }
                    FollowUp::Nothing
                }
                None => FollowUp::Refresh,
            },
            ApiError::Status { status: 404, .. } => FollowUp::Refresh,
            ApiError::Network(_)
            | ApiError::Status { .. }
            | ApiError::Decode(_) => FollowUp::Nothing,
        }
    }
}

/// How long past a window's end the page asks the server whether it has
/// ended. Timed by the server's clock (the status's `now`), never the
/// browser's: the margin only covers the time the answer took to arrive
pub const PROBE_AFTER_END: TimeDelta = TimeDelta::seconds(2);

/// The longest a probe is set for: a browser's `setTimeout` takes a signed
/// 32-bit count of milliseconds (24.8 days), past which it fires at once.
/// No window lasts that long (2 days at most); a probe set past it would
/// only fire early and ask again
const PROBE_MAX: Duration = Duration::from_secs(20 * 86_400);

/// How long a probe that got no answer waits before asking again, the
/// first time; it doubles each time, up to [`PROBE_RETRY_MAX`]
const PROBE_RETRY: Duration = Duration::from_secs(5);

/// The longest wait between probes that get no answer
const PROBE_RETRY_MAX: Duration = Duration::from_secs(60);

/// How long the page may go without learning its window's state, once a
/// probe got no answer, before it stops showing what is the operator's
/// alone (the inbox, the compose controls) and says it couldn't reach
/// Agora: it never keeps private messages up on a guess. The drafts are
/// kept, and the probe keeps asking
pub const PROBE_UNKNOWN_FOR: TimeDelta = TimeDelta::seconds(30);

/// The tap in place that opens a check window, as far as it has got. One
/// per page, in the session's handle: every [`WindowCheck`] on the page
/// draws the same one
#[derive(Debug, Clone, Default, PartialEq)]
pub enum WindowTap {
    /// Not asked for: "Confirm with a passkey"
    #[default]
    Idle,
    /// Asking Agora whether a window is open already (another tab may have
    /// opened one), then for the tap (`POST /api/portal/window/tap`)
    Asking,
    /// The tap's options are here: "Tap your passkey to confirm" opens the
    /// browser's prompt in that press (a browser refuses one outside a
    /// gesture)
    Ready(serde_json::Value),
    /// The prompt is open, or its answer is with Agora
    /// (`POST /api/portal/window`), or the bootstrap is being fetched again
    Opening,
    /// No window was opened: why, for a person. "Confirm with a passkey"
    /// asks again
    Failed(String),
}

/// How far past a probe's due time, by the browser's clock, the page takes
/// it that it was asleep (a laptop's lid closed, a phone's tab frozen):
/// what is the operator's alone is hidden at once, before the server is
/// asked. The browser's clock compared with itself: elapsed time, never
/// when the window ends
pub const SLEPT_PAST: TimeDelta = TimeDelta::seconds(20);

/// How often the page compares the browser's clock with the probe's due
/// time, for where a timer doesn't count suspended time or the page isn't
/// reported hidden
pub const HEARTBEAT: Duration = Duration::from_secs(10);

/// The shortest wait before a probe: a status whose window ended already
/// is asked again a second on, not at once in a loop
const PROBE_MIN: Duration = Duration::from_secs(1);

/// The probe of the window's end: which one is current, and since when
/// probes have gone unanswered
#[derive(Debug, Clone, Copy, Default)]
struct Probe {
    /// Bumped by each probe set; a probe that wakes to another number was
    /// replaced, and asks nothing
    generation: u64,
    /// Bumped by each question the probe asks: an answer to an older one is
    /// dropped. The probe's own count, so a refresh never drops a probe's
    /// answer (nor the next probe it sets)
    asked: u64,
    /// When the current probe is due, by the browser's clock: compared
    /// only with that clock ([`SLEPT_PAST`])
    due: Option<DateTime<Utc>>,
    /// When the first of the probes that have gone unanswered since the
    /// last answer was made, by the browser's clock (only ever compared
    /// with that clock, for [`PROBE_UNKNOWN_FOR`])
    failing_since: Option<DateTime<Utc>>,
    /// How many probes in a row have gone unanswered
    failures: u32,
    /// The page listens for being shown again
    watching: bool,
}

/// The session in context: read it, refresh it, write through it. `Copy`,
/// like the signals it holds.
///
/// Owned by the root scope, as the tasks that write it run there
/// (`spawn_forever`); owned by a scope below it, every write from a task
/// would log dioxus's hoisting warning in a debug build.
#[derive(Clone, Copy, PartialEq)]
pub struct SessionHandle {
    state: Signal<Session>,
    /// Bumped by every refresh, so a bootstrap that answers after a later
    /// refresh was started is dropped rather than overwriting it. Only
    /// refreshes compete on it: a write's refusal changes the state without
    /// it, so it can't cancel a refresh in flight.
    generation: Signal<u64>,
    portal: CopyValue<Rc<dyn Portal>>,
    /// The compose drafts, told of every signed-in session the state
    /// becomes, so another operator's (or poster's) session never finds
    /// the last one's text ([`Drafts::signed_in`](super::compose::Drafts))
    drafts: super::compose::Drafts,
    /// The inbox read, told of every state the session becomes, so no one
    /// else's inbox is ever shown from it
    /// ([`InboxStore::session_is`](super::inbox::InboxStore))
    inbox: super::inbox::InboxStore,
    /// The tap in place, outside a check window
    tap: Signal<WindowTap>,
    /// The probe of the window's end
    probe: CopyValue<Probe>,
}

impl fmt::Debug for SessionHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SessionHandle")
    }
}

impl SessionHandle {
    /// A handle, its state loading, owned by the root scope, with the
    /// drafts it tells of a change of session. Call it in the root's scope
    fn new_root(
        portal: Rc<dyn Portal>,
        drafts: super::compose::Drafts,
        inbox: super::inbox::InboxStore,
    ) -> Self {
        Self {
            state: Signal::new_in_scope(Session::Loading, ScopeId::ROOT),
            generation: Signal::new_in_scope(0, ScopeId::ROOT),
            portal: CopyValue::new_in_scope(portal, ScopeId::ROOT),
            drafts,
            inbox,
            tap: Signal::new_in_scope(WindowTap::Idle, ScopeId::ROOT),
            probe: CopyValue::new_in_scope(Probe::default(), ScopeId::ROOT),
        }
    }

    /// The session, subscribing the calling component to its changes
    pub fn get(&self) -> Session {
        self.state.cloned()
    }

    /// The session, subscribing nothing (for a handler or a task)
    pub fn get_untracked(&self) -> Session {
        self.state.peek().clone()
    }

    /// The client's [`Portal`]
    pub fn portal(&self) -> Rc<dyn Portal> {
        self.portal.peek().clone()
    }

    /// Fetches the bootstrap again ([`Session::after_refresh`] says what
    /// its answer replaces). The current state stays until it answers. The
    /// fetch runs on the root scope, so a refresh a form starts isn't
    /// cancelled when the form goes away
    pub fn refresh(self) {
        self.watch_returns();
        let generation = self.begin_refresh();
        let answer = self.portal().session();
        dioxus::core::spawn_forever(async move {
            self.finish_refresh(generation, answer.await);
        });
    }

    /// A refresh's generation, now the current one
    fn begin_refresh(self) -> u64 {
        let mut generation = self.generation;
        let next = *generation.peek() + 1;
        generation.set(next);
        next
    }

    /// Applies a bootstrap's answer, unless a later fetch has started, and
    /// sets the probe of the window's end it gives: signed in, the next
    /// probe; otherwise none (a page outside its window, or signed out,
    /// has nothing to hide when a window ends; being shown again asks
    /// anyway). A bootstrap with no answer leaves the state and the probe
    /// as they are
    fn finish_refresh(
        self,
        generation: u64,
        answer: Result<PortalSessionResponse, ApiError>,
    ) {
        if *self.generation.peek() != generation {
            return;
        }
        let answered = match &answer {
            Ok(_) | Err(ApiError::SignedOut(_)) => true,
            Err(e) => e.code() == Some(RefusalCode::CheckNeeded),
        };
        let failure = match &answer {
            Err(e) if !answered => Some(e.clone()),
            _ => None,
        };
        let mut next = self.state.peek().clone();
        next.after_refresh(answer);
        let window = match &next {
            Session::SignedIn(s) => Some(*s.window()),
            _ => None,
        };
        let unreachable = matches!(next, Session::Failed(_));
        self.replace(next);
        match (answered, window) {
            (true, Some(window)) => self.set_probe(window),
            (true, None) => self.cancel_probe(),
            // A page that couldn't reach Agora keeps asking, whatever
            // started this refresh (a probe's, "Try again"), so it never
            // stays hidden once Agora answers
            (false, _) if unreachable => {
                if let Some(e) = failure {
                    self.probe_failed(e);
                }
            }
            (false, _) => {}
        }
    }

    /// Asks the server about the window (`GET /api/portal/window`, which
    /// never counts as a use of it), and applies the answer:
    ///
    /// - open: a signed-in page takes the window's new times and sets the
    ///   next probe by them; a page outside its window, or one that
    ///   couldn't reach Agora, fetches the bootstrap (the session's token
    ///   and whether it may write) and carries on;
    /// - `check_needed`: the page is outside its window, and says why;
    /// - 401: signed out;
    /// - no answer (offline, a 5xx, one that couldn't be read): asked
    ///   again, never dropped ([`Self::probe_failed`])
    fn probe(self) {
        self.watch_returns();
        // Asked: no probe is due until the next is set, so a slow answer
        // is never read as a sleep
        let asked = {
            let mut probe = self.probe;
            let mut p = probe.write();
            p.asked += 1;
            p.due = None;
            p.asked
        };
        let answer = self.portal().window_status();
        dioxus::core::spawn_forever(async move {
            let answer = answer.await;
            if self.probe.peek().asked != asked {
                return;
            }
            match answer {
                Ok(window) => {
                    self.answered();
                    let mut state = self.state.peek().clone();
                    match &mut state {
                        Session::SignedIn(s) => {
                            s.portal.window = window;
                            self.replace(state);
                            self.set_probe(window);
                        }
                        _ => self.refresh(),
                    }
                }
                Err(ApiError::SignedOut(_)) => {
                    self.answered();
                    self.cancel_probe();
                    self.replace(Session::SignedOut);
                }
                Err(e) if e.code() == Some(RefusalCode::CheckNeeded) => {
                    self.answered();
                    self.cancel_probe();
                    self.replace(Session::CheckNeeded(e.no_window()));
                }
                Err(e) => self.probe_failed(e),
            }
        });
    }

    /// A probe got an answer: no longer failing
    fn answered(self) {
        let mut probe = self.probe;
        let mut p = probe.write();
        p.failing_since = None;
        p.failures = 0;
    }

    /// A probe got no answer: whether the window is open is unknown. Asked
    /// again after a wait that doubles each time ([`PROBE_RETRY`], up to
    /// [`PROBE_RETRY_MAX`]); once the page has gone [`PROBE_UNKNOWN_FOR`]
    /// without knowing, it shows no more of what is the operator's alone,
    /// and says Agora couldn't be reached ([`Session::Failed`])
    fn probe_failed(self, e: ApiError) {
        let now = self.portal().now();
        let (since, failures) = {
            let mut probe = self.probe;
            let mut p = probe.write();
            let since = *p.failing_since.get_or_insert(now);
            p.failures = p.failures.saturating_add(1);
            (since, p.failures)
        };
        // A page woken from sleep (already hidden, `Loading`) says at once
        // that Agora couldn't be reached; one that was showing a window
        // after the bound
        let hide = match *self.state.peek() {
            Session::Loading => true,
            Session::SignedIn(_) => now - since >= PROBE_UNKNOWN_FOR,
            _ => false,
        };
        if hide {
            self.inbox.forget_reads();
            self.replace(Session::Failed(e));
        }
        let wait = PROBE_RETRY
            .saturating_mul(1 << failures.saturating_sub(1).min(8))
            .min(PROBE_RETRY_MAX);
        self.set_probe_in(wait);
    }

    /// No probe is current
    fn cancel_probe(self) {
        let mut probe = self.probe;
        probe.write().generation += 1;
    }

    /// Asks the server again at `window`'s end, by the server's clock (the
    /// time left from its `now`), plus [`PROBE_AFTER_END`], unless another
    /// probe replaces this one
    fn set_probe(self, window: CheckWindowStatus) {
        let wait = (window.ends_at() + PROBE_AFTER_END - window.now)
            .to_std()
            .unwrap_or(Duration::ZERO)
            .max(PROBE_MIN);
        self.set_probe_in(wait);
    }

    /// Asks the server again `wait` from now, unless another probe
    /// replaces this one
    fn set_probe_in(self, wait: Duration) {
        self.cancel_probe();
        let wait = wait.min(PROBE_MAX);
        let portal = self.portal();
        let due = portal.now()
            + TimeDelta::from_std(wait).unwrap_or(TimeDelta::zero());
        let generation = {
            let mut probe = self.probe;
            let mut p = probe.write();
            p.due = Some(due);
            p.generation
        };
        let woken = portal.after(wait);
        dioxus::core::spawn_forever(async move {
            woken.await;
            self.probe_now(generation);
        });
    }

    /// The probe `generation` woke: ask the server, if it is still the
    /// current probe and the page shows, or showed until it couldn't reach
    /// Agora, a window as open
    fn probe_now(self, generation: u64) {
        if self.probe.peek().generation != generation
            || !matches!(
                *self.state.peek(),
                Session::SignedIn(_) | Session::Failed(_) | Session::Loading
            )
        {
            return;
        }
        self.hide_if_slept();
        self.probe();
    }

    /// Whether the page slept past its probe's due time
    /// ([`SLEPT_PAST`]); if so, and it shows a window as open, what is the
    /// operator's alone is hidden at once (the state is `Loading` until
    /// the server answers), before anything is asked. True when it slept
    fn hide_if_slept(self) -> bool {
        let Some(due) = self.probe.peek().due else {
            return false;
        };
        if self.portal().now() <= due + SLEPT_PAST {
            return false;
        }
        if matches!(*self.state.peek(), Session::SignedIn(_)) {
            self.inbox.forget_reads();
            self.replace(Session::Loading);
        }
        true
    }

    /// The heartbeat: a page that slept past its probe's due time, without
    /// its timer or a visibility event saying so, hides and asks
    fn beat(self) {
        if matches!(*self.tap.peek(), WindowTap::Asking | WindowTap::Opening) {
            return;
        }
        if matches!(*self.state.peek(), Session::SignedIn(_))
            && self.hide_if_slept()
        {
            self.probe();
        }
    }

    /// The page is shown again (a tab brought back, a page restored from
    /// the back-forward cache): ask the server now, whatever the timer
    /// says, since time may have passed or another tab may have opened a
    /// window. Nothing while a tap is under way: its own fetch answers
    pub fn check_now(self) {
        if matches!(*self.tap.peek(), WindowTap::Asking | WindowTap::Opening) {
            return;
        }
        if matches!(
            *self.state.peek(),
            Session::SignedIn(_) | Session::CheckNeeded(_) | Session::Failed(_)
        ) {
            self.hide_if_slept();
            self.probe();
        }
    }

    /// Listens, once per page, for the page being shown again
    /// ([`Portal::shown_again`]), and asks then ([`Self::check_now`])
    fn watch_returns(self) {
        {
            let mut probe = self.probe;
            let mut p = probe.write();
            if p.watching {
                return;
            }
            p.watching = true;
        }
        let portal = self.portal();
        let beats = portal.clone();
        dioxus::core::spawn_forever(async move {
            loop {
                portal.shown_again().await;
                self.check_now();
            }
        });
        dioxus::core::spawn_forever(async move {
            loop {
                beats.after(HEARTBEAT).await;
                self.beat();
            }
        });
    }

    /// The tap in place, subscribing the calling component
    pub fn window_tap(&self) -> WindowTap {
        self.tap.cloned()
    }

    fn set_tap(self, next: WindowTap) {
        if *self.tap.peek() != next {
            let mut tap = self.tap;
            tap.set(next);
        }
    }

    /// "Confirm with a passkey": first asks whether a window is open
    /// already (another tab may have opened one: then the page carries on,
    /// with no ceremony), then for the tap that opens one, for this session
    /// from this network. Nothing while one is being asked for or opened,
    /// or once the session isn't outside its window
    pub fn start_window_tap(self) {
        if !matches!(*self.state.peek(), Session::CheckNeeded(_))
            || matches!(
                *self.tap.peek(),
                WindowTap::Asking | WindowTap::Opening
            )
        {
            return;
        }
        self.set_tap(WindowTap::Asking);
        // The status (never a use). A probe in flight is superseded: this
        // question answers for it
        {
            let mut probe = self.probe;
            probe.write().asked += 1;
        }
        let status = self.portal().window_status();
        dioxus::core::spawn_forever(async move {
            let status = status.await;
            match status {
                // Open already (another tab): the bootstrap, and on
                Ok(_) => {
                    self.set_tap(WindowTap::Idle);
                    self.refresh();
                    return;
                }
                Err(ApiError::SignedOut(_)) => {
                    self.set_tap(WindowTap::Idle);
                    self.replace(Session::SignedOut);
                    return;
                }
                Err(e) if e.code() == Some(RefusalCode::CheckNeeded) => {
                    let why = e.no_window();
                    self.replace(Session::CheckNeeded(why));
                    // No tap from here can open one: none is offered
                    if why == Some(NoWindow::AddressUnusable) {
                        self.set_tap(WindowTap::Idle);
                        return;
                    }
                }
                Err(e) => {
                    self.set_tap(WindowTap::Failed(window_refusal_words(&e)));
                    return;
                }
            }
            match self.portal().window_tap().await {
                Ok(PortalWindowTap { tap }) => {
                    self.set_tap(WindowTap::Ready(tap))
                }
                Err(ApiError::SignedOut(_)) => {
                    self.set_tap(WindowTap::Idle);
                    self.replace(Session::SignedOut);
                }
                Err(e) => {
                    self.set_tap(WindowTap::Failed(window_refusal_words(&e)))
                }
            }
        });
    }

    /// "Tap your passkey to confirm": the browser's prompt opens here, in
    /// the press itself, and its answer opens the window; then the
    /// bootstrap is fetched again, and the page carries on as it was
    pub fn confirm_window_tap(self) {
        let WindowTap::Ready(options) = self.tap.peek().clone() else {
            return;
        };
        let portal = self.portal();
        let tapped = portal.passkey(&options);
        self.set_tap(WindowTap::Opening);
        dioxus::core::spawn_forever(async move {
            let assertion = match tapped.await {
                Ok(assertion) => assertion,
                Err(failed) => {
                    self.set_tap(WindowTap::Failed(format!(
                        "No check was made: {failed}. Nothing was opened, \
                         and nothing here was lost. Confirm again when you \
                         are ready."
                    )));
                    return;
                }
            };
            let finished = portal
                .open_window(&PortalWindowOpenRequest { assertion })
                .await
                .map(|_| ());
            self.after_finish(finished);
        });
    }

    /// "Cancel" while the tap is asked for: nothing was opened
    pub fn cancel_window_tap(self) {
        if matches!(*self.tap.peek(), WindowTap::Ready(_)) {
            self.set_tap(WindowTap::Idle);
        }
    }

    /// The finish answered `finished`. Whatever it said, the server is
    /// asked what the session is now (the bootstrap): it may have opened a
    /// window although no answer came, or another tab may have opened one
    /// although this tap was refused. Then the tap says what happened: done
    /// when a window is open; otherwise why not, in the finish's own words
    fn after_finish(self, finished: Result<(), ApiError>) {
        if let Err(ApiError::SignedOut(_)) = finished {
            self.set_tap(WindowTap::Idle);
            self.replace(Session::SignedOut);
            return;
        }
        self.set_tap(WindowTap::Opening);
        // A probe asked before the tap answers for a window that may not
        // be the one now: dropped
        {
            let mut probe = self.probe;
            probe.write().asked += 1;
        }
        let generation = self.begin_refresh();
        let answer = self.portal().session();
        dioxus::core::spawn_forever(async move {
            let answer = answer.await;
            let words = match (&finished, &answer) {
                (_, Ok(_)) | (_, Err(ApiError::SignedOut(_))) => None,
                (Ok(()), Err(e))
                    if e.code() == Some(RefusalCode::CheckNeeded) =>
                {
                    Some(
                        "Agora opened a check window, but this page's next \
                         request was refused outside it: your network may \
                         have changed just after. Confirm again from here."
                            .to_owned(),
                    )
                }
                (Ok(()), Err(e)) => Some(format!(
                    "Agora opened a check window, but couldn't then be \
                     asked for this page's session ({e}). Confirm again to \
                     carry on."
                )),
                (Err(tap), Err(e))
                    if e.code() == Some(RefusalCode::CheckNeeded) =>
                {
                    Some(match tap.outcome_unknown() {
                        true => format!(
                            "Agora didn't answer the tap ({tap}), and no \
                             check window is open: nothing was opened. \
                             Confirm again."
                        ),
                        false => window_refusal_words(tap),
                    })
                }
                (Err(tap), Err(e)) => Some(format!(
                    "Agora didn't take the tap ({}), and couldn't then be \
                     asked whether a window is open ({e}). Confirm again to \
                     find out.",
                    tap
                )),
            };
            self.finish_refresh(generation, answer);
            self.set_tap(match words {
                Some(words) => WindowTap::Failed(words),
                None => WindowTap::Idle,
            });
        });
    }
}

/// Why no check window was opened, for a person: the page's own words for
/// a tap from another network ([`NETWORK_CHANGED`]), the server's own for
/// any other refusal (written for operators: a passkey that isn't
/// registered or is suspended, a signature counter that went backwards, a
/// network that can't hold a window), and for a failure to get an answer,
/// that nothing was opened
pub fn window_refusal_words(e: &ApiError) -> String {
    if e.code() == Some(RefusalCode::NetworkChanged) {
        return NETWORK_CHANGED.to_owned();
    }
    match e {
        _ if e.outcome_unknown() => format!(
            "Agora couldn't be asked for the check ({e}). Nothing was \
             opened: try again."
        ),
        ApiError::Forbidden(body) => match body.suspension() {
            Some(ban) => format!("{}.", ban.human_summary()),
            None => body.message().to_owned(),
        },
        ApiError::Status { message, .. } => message.clone(),
        ApiError::SignedOut(_) => "You're no longer signed in.".to_owned(),
        ApiError::Network(_) | ApiError::Decode(_) => e.to_string(),
    }
}

impl SessionHandle {
    /// Sets the state, if it changed (so readers don't redraw for
    /// nothing). A signed-in state is first shown to the drafts, which
    /// are cleared if it is another operator's or poster's
    fn replace(self, next: Session) {
        if let Session::SignedIn(signed_in) = &next {
            self.drafts.signed_in(signed_in);
        }
        if !matches!(next, Session::CheckNeeded(_))
            && !matches!(*self.tap.peek(), WindowTap::Opening)
        {
            self.set_tap(WindowTap::Idle);
        }
        self.inbox.session_is(&next);
        if next != *self.state.peek() {
            let mut state = self.state;
            state.set(next);
        }
    }

    /// Updates the state for the refusal of a write made with `used`, and
    /// refreshes if that's what it calls for
    pub fn note_write_error(self, used: &SignedIn, error: &ApiError) {
        let mut next = self.state.peek().clone();
        let follow_up = next.after_write_error(used, error);
        self.replace(next);
        if follow_up == FollowUp::Refresh {
            self.refresh();
        }
    }

    /// A post with `signed_in`'s token, run on the root scope (so a form
    /// that closes doesn't abandon it); `done` gets the answer. A refusal
    /// updates the session first ([`Self::note_write_error`])
    pub fn create_post(
        self,
        signed_in: SignedIn,
        request: PortalPostRequest,
        done: impl FnOnce(Result<PostCreated, ApiError>) + 'static,
    ) {
        let answer = self.portal().create_post(signed_in.portal(), &request);
        self.write(signed_in, answer, done);
    }

    /// A write with `signed_in`'s token whose answer is `answer`, run on
    /// the root scope as [`Self::create_post`]; a refusal updates the
    /// session first
    fn write<T: 'static>(
        self,
        signed_in: SignedIn,
        answer: LocalBoxFuture<'static, Result<T, ApiError>>,
        done: impl FnOnce(Result<T, ApiError>) + 'static,
    ) {
        dioxus::core::spawn_forever(async move {
            let answer = answer.await;
            match &answer {
                Ok(_) => {}
                Err(e) => self.note_write_error(&signed_in, e),
            }
            done(answer);
        });
    }

    /// Marks the poster's messages read, as [`Self::create_post`]
    pub fn mark_read(
        self,
        signed_in: SignedIn,
        request: PortalMarkReadRequest,
        done: impl FnOnce(Result<PortalMarkReadResponse, ApiError>) + 'static,
    ) {
        let answer = self.portal().mark_read(signed_in.portal(), &request);
        self.write(signed_in, answer, done);
    }

    /// A friendship action by the poster, as [`Self::create_post`]
    pub fn friendship(
        self,
        signed_in: SignedIn,
        request: PortalFriendshipRequest,
        done: impl FnOnce(Result<StatusResponse, ApiError>) + 'static,
    ) {
        let answer = self.portal().friendship(signed_in.portal(), &request);
        self.write(signed_in, answer, done);
    }

    /// Asks for a message's passkey tap, as [`Self::create_post`]
    pub fn message_tap(
        self,
        signed_in: SignedIn,
        request: PortalMessageRequest,
        done: impl FnOnce(Result<PortalMessageTap, ApiError>) + 'static,
    ) {
        let answer = self.portal().message_tap(signed_in.portal(), &request);
        self.write(signed_in, answer, done);
    }

    /// Sends a message, as [`Self::create_post`]
    pub fn send_message(
        self,
        signed_in: SignedIn,
        request: PortalMessageRequest,
        done: impl FnOnce(Result<SendMessageResponse, ApiError>) + 'static,
    ) {
        let answer = self.portal().send_message(signed_in.portal(), &request);
        self.write(signed_in, answer, done);
    }

    /// A comment with `signed_in`'s token, as [`Self::create_post`]
    pub fn create_comment(
        self,
        signed_in: SignedIn,
        request: PortalCommentRequest,
        done: impl FnOnce(Result<WriteAck<CommentId>, ApiError>) + 'static,
    ) {
        let answer = self.portal().create_comment(signed_in.portal(), &request);
        self.write(signed_in, answer, done);
    }
}

/// Puts the session in `dom`'s root context, loading, for every component
/// in it ([`use_session`]), reaching the portal through `portal`, and with
/// it the store of compose drafts ([`Drafts`](super::compose::Drafts));
/// nothing is fetched until a component reads it
pub fn provide(dom: &VirtualDom, portal: impl Portal + 'static) {
    let portal: Rc<dyn Portal> = Rc::new(portal);
    dom.in_scope(ScopeId::ROOT, || {
        let drafts = super::compose::Drafts::new_root(portal.clone());
        let inbox = super::inbox::InboxStore::new_root();
        provide_context(SessionHandle::new_root(portal, drafts, inbox));
        provide_context(drafts);
        provide_context(inbox);
    });
}

/// The session from the root's [`provide`], or `None` where no client
/// provided one (the server's render). The first reader to mount starts
/// the bootstrap, in an effect, after the first render; a later one finds
/// it started (its generation moved) and starts nothing. A hook
pub fn try_use_session() -> Option<SessionHandle> {
    let handle = try_use_context::<SessionHandle>();
    // Reads with `peek`, so it subscribes to nothing and runs once
    use_effect(move || {
        if let Some(handle) = handle
            && *handle.generation.peek() == 0
        {
            handle.refresh();
        }
    });
    handle
}

/// [`try_use_session`], for a component only a client draws: panics where
/// no session was provided. A hook
pub fn use_session() -> SessionHandle {
    try_use_session()
        .expect("the client provides the session (ui::session::provide)")
}

/// The server's sign-in page, below the site's base
/// (`operator::session::login_url`), which returns to `next` after
fn sign_in_path() -> String {
    format!("{}/login", super::route::base())
}

/// The sign-in page, returning to `return_to` (a path on this site) after
pub fn sign_in_href(return_to: Option<&str>) -> String {
    // A base only to build the path and query with: the link is relative
    let mut url = Url::parse("https://agora.invalid")
        .and_then(|base| base.join(&sign_in_path()))
        .expect("a static URL parses");
    if let Some(next) = return_to {
        url.query_pairs_mut().append_pair("next", next);
    }
    url[Position::BeforePath..Position::AfterQuery].to_owned()
}

/// Who is signed in, from the context: [`SessionStatusView`] of
/// [`use_session`], its "Try again" refreshing it
#[component]
pub fn SessionStatus() -> Element {
    let handle = use_session();
    let session = handle.get();
    rsx! {
        SessionStatusView {
            session,
            return_to: handle.portal().here(),
            on_retry: move |_| handle.refresh(),
            check: rsx! {
                WindowCheck {}
            }
        }
    }
}

/// Who is signed in, in one line: a sign-in link when no one is, and
/// whether the operator can post.
///
/// Every state is drawn in one `role="status"` paragraph, so a screen
/// reader announces the line as it changes (loading, then who is signed
/// in), politely; none of it is urgent enough for `alert`.
#[component]
pub fn SessionStatusView(
    /// What to draw
    session: Session,
    /// The page to come back to after signing in
    return_to: Option<String>,
    /// "Try again", after a failure
    on_retry: EventHandler,
    /// The tap in place, drawn under the line when the session is outside
    /// its check window ([`WindowCheck`])
    #[props(default)]
    check: Option<Element>,
) -> Element {
    match session {
        Session::Loading => rsx! {
            p { class: "post-meta", role: "status", "Checking who is signed in…" }
        },
        Session::SignedOut => {
            let href = sign_in_href(return_to.as_deref());
            rsx! {
                p { class: "post-meta", role: "status",
                    "Not signed in. "
                    a { href: "{href}", "Sign in" }
                    " to post as an operator."
                }
            }
        }
        // Signed out: the sign-in, not a tap
        Session::CheckNeeded(
            why @ Some(NoWindow::Ended {
                ended: crate::enums::CheckWindowEnd::SignedOut,
            }),
        ) => {
            let href = sign_in_href(return_to.as_deref());
            rsx! {
                p { class: "post-meta", role: "status",
                    "{check_needed_line(why)} "
                    a { href: "{href}", "Sign in" }
                }
            }
        }
        Session::CheckNeeded(why) => rsx! {
            p { class: "post-meta", role: "status", "{check_needed_line(why)}" }
            if let Some(check) = check {
                {check}
            }
        },
        Session::Failed(e) => rsx! {
            p { class: "post-meta", role: "status",
                "{UNREACHABLE_LINE} ({e}) "
                button {
                    class: "pill pill-btn",
                    r#type: "button",
                    onclick: move |_| on_retry.call(()),
                    "Try again"
                }
            }
        },
        Session::SignedIn(signed_in) => signed_in_line(&signed_in),
    }
}

/// Why there is no passkey check open for this page, in a clause for a
/// person, by what the server said ([`NoWindow`], matched exhaustively);
/// with no reason, what is true in every case
pub fn no_window_words(why: Option<NoWindow>) -> &'static str {
    use crate::enums::CheckWindowEnd;
    match why {
        None => "there is no passkey check open for this page",
        Some(NoWindow::NeverOpened) => {
            "this session hasn't had a passkey check yet, and Agora now asks \
             for one before it shows anything of yours"
        }
        Some(NoWindow::Ended { ended }) => match ended {
            CheckWindowEnd::Idle => {
                "the passkey check for this session ended after a while unused"
            }
            CheckWindowEnd::Absolute => {
                "the passkey check for this session reached its time limit"
            }
            CheckWindowEnd::Address => {
                "the passkey check for this session ended because your \
                 network changed"
            }
            CheckWindowEnd::Suspended => {
                "the passkey check for this session ended because the passkey \
                 that opened it was suspended; another passkey of yours, if you \
                 have one, can confirm"
            }
            CheckWindowEnd::Frozen => {
                "the passkey check for this session ended because your account \
                 was frozen; a passkey the freeze left in effect, if you have \
                 one, can confirm"
            }
            CheckWindowEnd::SignedOut => {
                "the passkey check for this session ended when the session was \
                 signed out"
            }
            CheckWindowEnd::Clock => {
                "Agora ended the passkey check for this session as a precaution \
                 (its stored times were ahead of Agora's clock)"
            }
        },
        Some(NoWindow::AddressUnusable) => {
            "this connection doesn't tell Agora which network you're on, so no \
             passkey check can be opened or used over it, and a tap from here \
             would be refused"
        }
    }
}

/// What the page says to do about it, after [`no_window_words`]
pub fn no_window_next(why: Option<NoWindow>) -> &'static str {
    use crate::enums::CheckWindowEnd;
    match why {
        Some(NoWindow::AddressUnusable) => {
            "Your drafts stay in this tab, and it carries on once this tab \
             reaches Agora through its public address (not straight over the \
             local network)."
        }
        Some(NoWindow::Ended {
            ended: CheckWindowEnd::Clock,
        }) => {
            "Confirm again with your passkey to carry on here; your drafts \
               are kept."
        }
        Some(NoWindow::Ended {
            ended: CheckWindowEnd::SignedOut,
        }) => "Sign in again to carry on; your drafts are kept in this tab.",
        None
        | Some(NoWindow::NeverOpened)
        | Some(NoWindow::Ended {
            ended:
                CheckWindowEnd::Idle
                | CheckWindowEnd::Absolute
                | CheckWindowEnd::Address
                | CheckWindowEnd::Suspended
                | CheckWindowEnd::Frozen,
        }) => {
            "Confirm with your passkey to carry on here; your drafts are \
               kept."
        }
    }
}

/// `clause` with its first letter capital, for the start of a sentence
pub(crate) fn sentence(clause: &str) -> String {
    let mut out = clause.to_owned();
    if let Some(first) = out.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    out
}

/// What the session line says outside a check window: whether still
/// signed in, why there is no check ([`no_window_words`]), what that means
/// here, what is kept, and what to do ([`no_window_next`])
pub fn check_needed_line(why: Option<NoWindow>) -> String {
    use crate::enums::CheckWindowEnd;
    let because = no_window_words(why);
    let next = no_window_next(why);
    match why {
        // Not "still signed in": the session was signed out
        Some(NoWindow::Ended {
            ended: CheckWindowEnd::SignedOut,
        }) => format!(
            "{}. This page shows only what anyone can see. {next}",
            sentence(because)
        ),
        Some(NoWindow::AddressUnusable) => format!(
            "You're still signed in, but {because}. {next} Until this tab \
             reaches Agora that way, this page shows only what anyone can see."
        ),
        _ => format!(
            "You're still signed in, but {because}. Until then, this page shows \
             only what anyone can see. {next}"
        ),
    }
}

/// [`check_needed_line`] with no reason: true however the page got there
pub const CHECK_NEEDED_LINE: &str = "You're still signed in, but there is no \
     passkey check open for this page. Until then, this page shows only what \
     anyone can see. Confirm with your passkey to carry on here; your drafts \
     are kept.";

/// What the session line says when Agora couldn't be reached: what is
/// hidden, until when, and that the drafts are kept
pub const UNREACHABLE_LINE: &str = "Agora couldn't be reached, so what is \
     yours (your inbox, posting) is hidden until it answers. Your drafts are \
     kept in this tab.";

/// When the check window ends, in words: when it ends if unused, and at
/// the latest (one time when they are the same)
pub fn window_line(window: &CheckWindowStatus) -> String {
    use super::format::date_time;
    let ends = window.ends_at();
    match ends < window.expires_at {
        true => format!(
            "Passkey check open until {}, later if you keep using Agora (at \
             most until {}).",
            date_time(ends),
            date_time(window.expires_at)
        ),
        false => format!("Passkey check open until {}.", date_time(ends)),
    }
}

/// The tap in place that opens a check window, from the context: draws the
/// handle's [`WindowTap`] ([`WindowTapView`]), and nothing unless the
/// session is outside its window. Every one on a page draws the same tap;
/// the one pressed takes the focus for the next step
#[component]
pub fn WindowCheck() -> Element {
    let handle = use_session();
    let pressed_here = use_signal(|| false);
    match handle.get() {
        // A tap from this connection would only be refused: the line says
        // why, and offers none
        Session::CheckNeeded(Some(NoWindow::AddressUnusable)) => {
            return rsx! {};
        }
        // Signed out: the line offers the sign-in; a tap couldn't help
        Session::CheckNeeded(Some(NoWindow::Ended {
            ended: crate::enums::CheckWindowEnd::SignedOut,
        })) => {
            return rsx! {};
        }
        Session::CheckNeeded(_) => {}
        _ => return rsx! {},
    }
    let mut pressed = pressed_here;
    rsx! {
        WindowTapView {
            tap: handle.window_tap(),
            focus: pressed_here(),
            on_start: move |_| {
                pressed.set(true);
                handle.start_window_tap();
            },
            on_confirm: move |_| handle.confirm_window_tap(),
            on_cancel: move |_| handle.cancel_window_tap(),
        }
    }
}

/// The tap in place, as it has got: "Confirm with a passkey" asks for it,
/// "Tap your passkey to confirm" opens the browser's prompt (in that very
/// press), and a failure says why with the first button again. Reads
/// nothing
#[component]
pub fn WindowTapView(
    tap: WindowTap,
    /// Take the focus for "Tap your passkey to confirm": this view's
    /// "Confirm" was the one pressed
    focus: bool,
    on_start: EventHandler,
    on_confirm: EventHandler,
    on_cancel: EventHandler,
) -> Element {
    let start = |words: &'static str, busy: bool| {
        rsx! {
            button {
                class: "ui-pill ui-pill-primary",
                r#type: "button",
                disabled: busy,
                aria_busy: busy.then_some("true"),
                onclick: move |_| on_start.call(()),
                "{words}"
            }
        }
    };
    rsx! {
        div { class: "ui-actions ui-window-check",
            match tap {
                WindowTap::Idle => start("Confirm with a passkey", false),
                WindowTap::Asking => start("Asking Agora…", true),
                WindowTap::Opening => start("Confirming…", true),
                WindowTap::Failed(why) => rsx! {
                    p { class: "ui-compose-alert", role: "alert", "{why}" }
                    {start("Confirm with a passkey", false)}
                },
                WindowTap::Ready(_) => rsx! {
                    button {
                        class: "ui-pill ui-pill-primary",
                        r#type: "button",
                        onmounted: move |e: MountedEvent| async move {
                            if focus {
                                // Focus refused (a browser that declines)
                                // leaves it where it was: the button still
                                // works
                                let _ = e.set_focus(true).await;
                            }
                        },
                        onclick: move |_| on_confirm.call(()),
                        "Tap your passkey to confirm"
                    }
                    button {
                        class: "ui-pill ui-pill-muted",
                        r#type: "button",
                        onclick: move |_| on_cancel.call(()),
                        "Cancel"
                    }
                },
            }
        }
    }
}

/// The signed-in line: who, and as whom they post, or why they can't
fn signed_in_line(signed_in: &SignedIn) -> Element {
    let operator = &signed_in.operator().display_name;
    let poster = signed_in.poster().map(|poster| {
        let href = AgoraLink::Profile(GetProfileInput {
            name: poster.name.clone(),
        })
        .site_path();
        (href, poster.name.to_string())
    });
    rsx! {
        p { class: "post-meta", role: "status",
            "Signed in as {operator}"
            match (signed_in.suspension(), poster) {
                (Some(ban), _) => rsx! {
                    ". {ban.human_summary()}. "
                    a { href: "{ban.appeal_url}", "How to appeal" }
                },
                (None, None) => rsx! { ". You have no operator poster yet, so you can't post here." },
                (None, Some((href, name))) => rsx! {
                    ", posting as "
                    a { href: "{href}", "{name}" }
                },
            }
        }
        p { class: "post-meta ui-window-line", "{window_line(signed_in.window())}" }
    }
}

#[cfg(all(test, feature = "dioxus-ssr"))]
mod tests;
