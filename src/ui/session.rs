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
//! **What draws takes props.** [`SessionStatusView`] draws a [`Session`]
//! and reads nothing; [`SessionStatus`] reads the context and draws it.
//!
//! [`ClientOnly`]: super::ClientOnly

use std::{fmt, rc::Rc};

use dioxus::prelude::*;
use futures::future::LocalBoxFuture;
use url::{Position, Url};

use crate::{
    errors::{ApiErrorBody, RefusalCode},
    ids::CommentId,
    links::AgoraLink,
    operator::{
        AgentRef, OperatorRef, PortalCommentRequest, PortalFriendshipRequest,
        PortalMarkReadRequest, PortalMarkReadResponse, PortalMessageRequest,
        PortalMessageTap, PortalPostRequest, PortalSessionResponse,
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
        RefusalCode::NetworkChanged => Some(
            "Your network changed while you were checking with your \
             passkey, so the check didn't count. Nothing was done: check \
             again from here. Your text is kept here.",
        ),
        // Until the tap in place (W3), the page can only say so
        RefusalCode::CheckNeeded => Some(
            "Agora needs a fresh passkey check: the last one lapsed, or you \
             are on another network. Nothing was done. Check with your \
             passkey (in another tab, so this page stays), then send it \
             again. Your text is kept here.",
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
    /// The state a bootstrap answer means: a 401 is signed out, any other
    /// failure is [`Session::Failed`]
    pub fn from_bootstrap(
        answer: Result<PortalSessionResponse, ApiError>,
    ) -> Self {
        match answer {
            Ok(portal) => Self::SignedIn(SignedIn::new(portal)),
            Err(ApiError::SignedOut(_)) => Self::SignedOut,
            Err(e) => Self::Failed(e),
        }
    }

    /// A refreshed bootstrap's answer, applied. A 200 or a 401 replaces the
    /// state (the bootstrap says whether the session may write, so a lifted
    /// suspension shows at once). Any other failure replaces it only when
    /// no one is known to be signed in: a signed-in page (and any form
    /// drawn for it) stays while the server is unreachable
    pub fn after_refresh(
        &mut self,
        answer: Result<PortalSessionResponse, ApiError>,
    ) {
        let next = Self::from_bootstrap(answer);
        if matches!((&*self, &next), (Self::SignedIn(_), Self::Failed(_))) {
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
    /// - a 403 the server named with a code (a message to a non-friend, a
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

    /// Applies a refresh's answer, unless a later refresh has started
    fn finish_refresh(
        self,
        generation: u64,
        answer: Result<PortalSessionResponse, ApiError>,
    ) {
        if *self.generation.peek() != generation {
            return;
        }
        let mut next = self.state.peek().clone();
        next.after_refresh(answer);
        self.replace(next);
    }

    /// Sets the state, if it changed (so readers don't redraw for
    /// nothing). A signed-in state is first shown to the drafts, which
    /// are cleared if it is another operator's or poster's
    fn replace(self, next: Session) {
        if let Session::SignedIn(signed_in) = &next {
            self.drafts.signed_in(signed_in);
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
        dioxus::core::spawn_forever(async move {
            let answer = answer.await;
            if let Err(e) = &answer {
                self.note_write_error(&signed_in, e);
            }
            done(answer);
        });
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
            if let Err(e) = &answer {
                self.note_write_error(&signed_in, e);
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
        dioxus::core::spawn_forever(async move {
            let answer = answer.await;
            if let Err(e) = &answer {
                self.note_write_error(&signed_in, e);
            }
            done(answer);
        });
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
        Session::Failed(e) => rsx! {
            p { class: "post-meta", role: "status",
                "Couldn't check who is signed in: {e} "
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
    }
}

#[cfg(all(test, feature = "dioxus-ssr"))]
mod tests;
