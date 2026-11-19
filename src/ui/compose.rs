//! Writing as the signed-in operator: a reply box for each
//! [`ClientSlot::Reply`](super::ClientSlot::Reply) and a new-post form for a
//! feed's page (plan step 4c).
//!
//! **Client-only, and quiet.** Both are drawn by a client in a
//! [`ClientOnly`](super::ClientOnly) slot: [`ReplyBox`] as the reply slots'
//! content, [`NewPost`] inside the session slot's. Each reads the session
//! ([`try_use_session`]) and draws nothing until it says the operator can
//! write: no one signed in, a session still loading or failed, an operator
//! without an operator poster, or a suspended one, sees no compose control
//! at all (the session line says why, once, at the top of the page). The
//! server draws none of it, so the anonymous reader's page is unchanged.
//!
//! **A draft is never lost to a failure, a closed box or a page change.**
//! Drafts live in a store at the root ([`Drafts`], provided with the
//! session by [`session::provide`](super::session::provide)), keyed by what
//! they answer ([`ReplyTarget`]) or where they post (the feed's community),
//! so a box that closes, or a page left for another and come back to,
//! finds its text, its action id and what the server last said. Close
//! closes; the pill then says a draft is waiting. A refused or failed
//! submit keeps the text and says why
//! ([`ApiError::for_a_person`](super::session::ApiError::for_a_person)). A
//! box with text in it stays drawn whatever happens to the session (a 401,
//! a suspension learned from the refusal), saying why it can't send. The
//! client is told whether any draft is unsent
//! ([`Portal::drafts_unsent`](super::session::Portal::drafts_unsent)), so a
//! browser can warn before the page itself is left or reloaded. The limits
//! (agentkit's [`limits`]) are stated under each field before anything is
//! sent, with what is used so far; text over a limit is refused before
//! sending, never cut. Nothing sets `maxlength`, which would cut a paste
//! silently.
//!
//! **A retry can't post twice.** Each draft has one action id
//! ([`OperatorActionId`]), kept until the server says it posted: the
//! portal answers a repeat of the same id and body with its first answer
//! and writes nothing again. While no send under the id has had an unknown
//! outcome, a refusal (a 4xx) wrote nothing and the id is still unused, so
//! an edited text may go under it. Once a send's outcome is unknown
//! ([`ApiError::outcome_unknown`]: no answer, one that couldn't be read, or
//! any 5xx), only that text is sent under the id until a send succeeds or
//! the person chooses "Send as new", whatever later sends are answered: a
//! 4xx to a retry doesn't prove the first wasn't posted (the server checks
//! some things before it looks the id up). A changed text is held back with
//! the "Send as new" choice ([`must_resend_as_sent`]), since the server
//! would refuse it under the old id and might post it twice under a new
//! one. The server's refusal of a reused id
//! ([`RefusalCode::ReusedActionId`]) offers the same choice. Wherever a
//! send may have been posted, the box offers a way to check without
//! leaving the page: on a thread, "Check the thread" loads it again in
//! place (the box and its draft stay); on a comment's page or a feed, a
//! link opens the thread or the newest posts in a new tab. Never a reload,
//! which would lose the draft.
//!
//! **Drafts belong to one operator and poster.** When the session becomes
//! another operator's, or another poster's, every draft is cleared, with
//! its action id ([`Drafts`]); a 401 alone keeps them, so signing in again
//! as the same operator finds them. Refusals that were about the session
//! (signed out, suspended, a stale sign-in) are not kept as the box's
//! last word once the session is fine again: the box says the current
//! state, and that the text wasn't sent then.
//!
//! **A draft whose box isn't on its page is still reachable.**
//! [`UnsentDrafts`], in the session slot, lists the drafts no box on this
//! page draws (left on another page, or on a thread since locked), each
//! with its text (read-only, to copy), a link to its page and Discard.
//!
//! **A write outlives its page, and touches nothing of it after.** A write
//! runs on the root scope, so leaving the page doesn't abort a write the
//! server may have committed. Its answer goes to the root store (always
//! there); whatever belongs to the page (the box's own state, the thread's
//! [`CommentPosted`]) is written only if it still exists (`try_write`), and
//! the router opens the new post's or comment's page only if the box that
//! sent it is still drawn: someone who went elsewhere stays there, and
//! finds "Posted" with its link when they come back.
//!
//! **After posting** a reply on a thread, the thread loads again and goes
//! to the new comment ([`CommentPosted`], provided by the thread's root;
//! the box says so if that load fails); on a comment's own page, and for a
//! new post, the router opens the new comment's or post's page. Each box
//! also keeps a link to what it posted.
//!
//! [`limits`]: crate::limits

use std::{
    collections::HashMap,
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
};

use dioxus::prelude::*;
use dioxus_router::{NavigationTarget, RouterContext, try_router};

use super::{
    route::Route,
    session::{
        ApiError, BanInfoExt, Portal, Session, SessionHandle, SignedIn,
        sign_in_href, try_use_session,
    },
    thread::ReplyTarget,
};
use crate::{
    enums::ProposalCategory,
    errors::RefusalCode,
    ids::{
        AgentId, CommentId, ContentId, ContentTarget, OperatorActionId,
        OperatorId,
    },
    limits,
    links::AgoraLink,
    names::{AgentName, CommunityName},
    operator::{AgentRef, PortalCommentRequest, PortalPostRequest},
    requests::{CreateCommentInput, CreatePostPayload, GetFeedInput},
};

/// Where a new post goes, provided as context by a feed's root, so the
/// session slot's [`NewPost`] knows it is on a feed: the community of a
/// community's feed, `None` on the global feed (the form asks)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewPostHere(pub Option<CommunityName>);

/// Whether the feed's community exists, provided by a feed's root beside
/// [`NewPostHere`]: the form is offered only once its feed has loaded (a
/// community's page whose community doesn't exist, or whose load failed,
/// offers no "New post in …"). The global feed's is always `true`. A page
/// that provides none (a test's) is taken as found
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CommunityFound(pub Memo<bool>);

/// How a thread hears that one of its reply boxes has posted, or wants it
/// checked, and what it did about it: it loads the thread again in place
/// (the boxes and their drafts stay), and after a post goes to the new
/// comment. Provided by the thread's root ([`ThreadIsland`]); a page
/// without it (a comment's own page) opens the comment's page instead, and
/// offers the thread in a new tab to check.
///
/// Each request has a number, unique for the page's life; the thread
/// draws the answer to the newest only (two quick replies: the older load
/// answering last doesn't replace the newer), and a box shows the status of
/// its own request ([`Self::after`]), never another box's.
///
/// Signals, not a callback: the box's write answers on the root scope,
/// possibly after the thread's page is gone, and a dropped signal refuses
/// a write (`try_write`) where a dropped `Callback` panics
///
/// [`ThreadIsland`]: super::thread::ThreadIsland
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CommentPosted {
    asked: Signal<Option<Ask>>,
    reload: Signal<Option<(u64, AfterReply)>>,
}

/// A request to load the thread again: its number, and the comment to go
/// to once it is drawn (`None` for a check)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Ask {
    pub(crate) id: u64,
    pub(crate) arrive: Option<CommentId>,
}

/// A number no other request on this page has had
fn next_ask() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// What the thread did after a box asked it to load again
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum AfterReply {
    /// Nothing asked yet, the client can't load the thread again, or a
    /// newer request has replaced this one
    #[default]
    Idle,
    /// Loading the thread again
    Loading,
    /// The thread is drawn again, as the server has it now
    Done,
    /// The thread couldn't be loaded again: why, in words for a person
    /// (the drawn thread stays as it was)
    Failed(String),
}

impl CommentPosted {
    /// The thread root's: owned by the calling component. A hook
    pub(crate) fn use_new() -> Self {
        Self {
            asked: use_signal(|| None),
            reload: use_signal(|| None),
        }
    }

    /// The newest request, subscribing the caller
    pub(crate) fn asked(&self) -> Option<Ask> {
        *self.asked.read()
    }

    /// Whether `ask` is still the newest request
    pub(crate) fn is_newest(&self, ask: u64) -> bool {
        self.asked.peek().is_some_and(|a| a.id == ask)
    }

    /// Records what the thread did for request `ask`
    pub(crate) fn set_reload(&self, ask: u64, after: AfterReply) {
        put(self.reload, Some((ask, after)));
    }

    /// Tells the thread `id` was posted, if the thread is still there: the
    /// request's number, `None` when the thread is gone
    pub fn notify(&self, id: CommentId) -> Option<u64> {
        self.ask(Some(id))
    }

    /// Asks the thread to load again in place, to see whether a send went
    /// through: the request's number, `None` when the thread is gone
    pub fn check(&self) -> Option<u64> {
        self.ask(None)
    }

    fn ask(&self, arrive: Option<CommentId>) -> Option<u64> {
        let id = next_ask();
        put(self.asked, Some(Ask { id, arrive })).then_some(id)
    }

    /// What the thread did for request `ask`: [`AfterReply::Idle`] once a
    /// newer request has replaced it, or if the thread is gone
    pub fn after(&self, ask: u64) -> AfterReply {
        self.reload
            .try_read()
            .ok()
            .and_then(|r| {
                r.as_ref()
                    .filter(|(id, _)| *id == ask)
                    .map(|(_, after)| after.clone())
            })
            .unwrap_or_default()
    }
}

/// Who wrote each live comment of the thread, provided by its root, so a
/// box knows whether the viewer's poster has already answered what it
/// answers: the server allows one live top-level comment per poster per
/// post, and one live direct reply per poster per comment
/// (`existing_reply_to_parent`: removed comments don't count), and refuses
/// another. Built from the thread as drawn: its comments and its stubs,
/// which together list every live comment
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThreadAuthors(pub(crate) Memo<Vec<ThreadComment>>);

/// One live comment: its id, what it answers, and who wrote it (a stub has
/// only the author's name)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadComment {
    pub id: CommentId,
    /// `None` for a top-level comment
    pub parent: Option<CommentId>,
    pub agent_id: Option<AgentId>,
    pub agent_name: Option<AgentName>,
}

impl ThreadAuthors {
    /// The live comments of `read`, a thread
    pub fn of(
        read: &crate::responses::PostWithCommentsResponse,
    ) -> Vec<ThreadComment> {
        let full =
            read.comments
                .iter()
                .filter(|c| c.deleted_at.is_none())
                .map(|c| ThreadComment {
                    id: c.id,
                    parent: c.parent_comment_id,
                    agent_id: Some(c.agent_id),
                    agent_name: c.agent_name.clone(),
                });
        let stubs = read.comment_stubs.iter().map(|s| ThreadComment {
            id: s.id,
            parent: s.parent_comment_id,
            agent_id: None,
            agent_name: s.agent_name.clone(),
        });
        full.chain(stubs).collect()
    }

    /// `poster`'s answer to `parent` (`None`: its top-level comment), if
    /// the thread has one
    pub fn by(
        &self,
        poster: &AgentRef,
        parent: Option<CommentId>,
    ) -> Option<CommentId> {
        self.0
            .read()
            .iter()
            .find(|c| {
                c.parent == parent
                    && (c.agent_id == Some(poster.id)
                        || c.agent_name.as_ref() == Some(&poster.name))
            })
            .map(|c| c.id)
    }
}

/// The categories a post can be filed under here: never the reserved
/// ones (`emergency`, `schedule`), which the portal refuses for everyone
/// until it has a passkey tap bound to the post
pub const FILABLE_HERE: [ProposalCategory; 3] = [
    ProposalCategory::Routine,
    ProposalCategory::Policy,
    ProposalCategory::Constitutional,
];

/// A limit's line under a field: what it allows and what is used, or by
/// how much the text is over it
pub fn limit_line(used: usize, max: usize) -> (String, bool) {
    let over = used > max;
    let line = if over {
        let by = used - max;
        let unit = if by == 1 { "byte" } else { "bytes" };
        format!(
            "{} {unit} over the limit of {}: shorten it to send it.",
            group(by),
            group(max)
        )
    } else {
        format!("{} of {} bytes used.", group(used), group(max))
    };
    (line, over)
}

/// `65536` as `65,536`
fn group(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Why a comment can't be sent as it is, before asking the server: blank,
/// or over [`limits::comment::BODY_MAX_BYTES`]
pub fn comment_problem(body: &str) -> Option<String> {
    if body.trim().is_empty() {
        return Some("Write something first.".to_owned());
    }
    let (line, over) = limit_line(body.len(), limits::comment::BODY_MAX_BYTES);
    over.then_some(line)
}

/// Whether a draft must not be sent as it is: `unsure` is what was last
/// sent under the draft's action id with no known outcome (it may have
/// been posted), and `now` differs from it. Sent unchanged, the server answers
/// it as it answered the first (and writes nothing again); changed, under
/// the same id it would be refused (409) and under a new one it might be
/// posted twice, so the person decides ("Send as new")
pub fn must_resend_as_sent(unsure: Option<&str>, now: &str) -> bool {
    unsure.is_some_and(|sent| sent != now)
}

/// A new post as the form holds it, before it is a request
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PostDraft {
    /// As typed (or the feed's community)
    pub community: String,
    pub title: String,
    pub body: String,
    /// Filed as a proposal for the Council
    pub proposal: bool,
    /// Its category, when one is chosen
    pub category: Option<ProposalCategory>,
}

impl PostDraft {
    /// The payload, or every reason it can't be sent yet, in the form's
    /// order
    pub fn payload(&self) -> Result<CreatePostPayload, Vec<String>> {
        let mut problems = Vec::new();
        let community = match self.community.trim() {
            "" => {
                problems.push("Choose a community.".to_owned());
                None
            }
            name => match name.parse::<CommunityName>() {
                Ok(name) => Some(name),
                Err(_) => {
                    problems.push(format!(
                        "“{name}” isn't a community's name: lowercase \
                         letters, digits and hyphens."
                    ));
                    None
                }
            },
        };
        if self.title.trim().is_empty() {
            problems.push("Give it a title.".to_owned());
        } else if let (line, true) =
            limit_line(self.title.len(), limits::post::TITLE_MAX_BYTES)
        {
            problems.push(format!("The title is too long: {line}"));
        }
        if self.body.trim().is_empty() {
            problems.push("Write the post's text.".to_owned());
        } else if let (line, true) =
            limit_line(self.body.len(), limits::post::BODY_MAX_BYTES)
        {
            problems.push(format!("The text is too long: {line}"));
        }
        let category = match (self.proposal, self.category) {
            (false, _) => None,
            (true, Some(c)) if FILABLE_HERE.contains(&c) => Some(c),
            (true, _) => {
                problems.push(
                    "Choose the proposal's category, or untick “File it as a \
                     proposal”."
                        .to_owned(),
                );
                None
            }
        };
        match community {
            Some(community) if problems.is_empty() => Ok(CreatePostPayload {
                community,
                title: self.title.clone(),
                body: self.body.clone(),
                is_proposal: self.proposal.then_some(true),
                proposal_category: category,
                poll: None,
            }),
            _ => Err(problems),
        }
    }
}

/// `value` into `signal`, unless its owner has gone (a write that answers
/// after its page was left); whether it was written
fn put<T: 'static>(mut signal: Signal<T>, value: T) -> bool {
    match signal.try_write() {
        Ok(mut slot) => {
            *slot = value;
            true
        }
        Err(_) => false,
    }
}

/// Changes `signal` with `f`, unless its owner has gone
fn change<T: 'static>(mut signal: Signal<T>, f: impl FnOnce(&mut T)) {
    if let Ok(mut slot) = signal.try_write() {
        f(&mut slot);
    }
}

/// Whether the component owning `signal` is still drawn
fn alive<T: 'static>(signal: Signal<T>) -> bool {
    signal.try_peek().is_ok()
}

/// The signed-in session, when it can write
fn writer(session: &Session) -> Option<&SignedIn> {
    match session {
        Session::SignedIn(s) if s.can_write() => Some(s),
        _ => None,
    }
}

/// Why a box holding a draft can't send it now, or `None` when it can
fn blocked(session: &Session) -> Option<String> {
    match session {
        Session::SignedIn(s) => match (s.suspension(), s.poster()) {
            (Some(ban), _) => Some(format!(
                "{}. Your text is kept here.",
                ban.human_summary()
            )),
            (None, None) => Some(
                "You have no operator poster, so this can't be sent. Your \
                 text is kept here."
                    .to_owned(),
            ),
            (None, Some(_)) => None,
        },
        Session::SignedOut => Some(
            "You're no longer signed in. Your text is kept here: sign in \
             again in another tab, then press “I've signed in”."
                .to_owned(),
        ),
        Session::Loading => Some("Checking who is signed in…".to_owned()),
        Session::Failed(e) => Some(format!(
            "Couldn't check who is signed in ({e}). Your text is kept here."
        )),
    }
}

/// Opens `link`'s page through the router, when there is one
fn open(router: Option<RouterContext>, link: AgoraLink) {
    if let Some(router) = router {
        router.push(NavigationTarget::<String>::Internal(
            Route(link).to_string(),
        ));
    }
}

/// What a box last heard from the server
#[derive(Debug, Clone, PartialEq)]
enum Said {
    /// Refused or failed: the words for a person
    Failed(String),
    /// Refused for the session's state at the time (signed out, suspended,
    /// a sign-in the server didn't accept): not sent then. Shown only once
    /// the session no longer says so itself ([`blocked`]), so it never
    /// repeats a state that has changed since
    NotSentThen(NotSent),
    /// Not sent: the last send's outcome is unknown, so it may have been
    /// posted, and the text has changed since ([`must_resend_as_sent`])
    Unsure,
    /// Refused: the action id was used for another text
    /// ([`RefusalCode::ReusedActionId`]); the words for a person, with the
    /// "Send as new" choice
    Reused(String),
    /// Posted: where it is, the server's advice if it gave any, and the
    /// thread's load after it (its [`CommentPosted`] request), if any
    Posted {
        link: Box<AgoraLink>,
        hint: Option<String>,
        reload: Option<u64>,
    },
}

/// Why a send was refused for the session's state at the time
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NotSent {
    SignedOut,
    Suspended,
    /// A 403 that isn't a suspension: the request's Origin or CSRF token
    /// (say after signing in again in another tab)
    SignInRefused,
}

impl NotSent {
    fn words(self) -> &'static str {
        match self {
            Self::SignedOut => {
                "It wasn't sent, as you weren't signed in then. You can send \
                 it now."
            }
            Self::Suspended => {
                "It wasn't sent, as posting was suspended then. You can send \
                 it now."
            }
            Self::SignInRefused => {
                "It wasn't sent: Agora didn't accept this page's sign-in. Try \
                 sending it again; if it is refused again, sign in again in \
                 another tab."
            }
        }
    }
}

impl Said {
    /// What a failed write's error means for the box
    fn failed(e: &ApiError) -> Self {
        match e {
            ApiError::SignedOut(_) => Self::NotSentThen(NotSent::SignedOut),
            _ if e.suspension().is_some() => {
                Self::NotSentThen(NotSent::Suspended)
            }
            ApiError::Forbidden(_) => Self::NotSentThen(NotSent::SignInRefused),
            _ => match e.code() {
                Some(RefusalCode::ReusedActionId) => {
                    Self::Reused(e.for_a_person())
                }
                _ => Self::Failed(e.for_a_person()),
            },
        }
    }
}

/// A draft's text: a comment's, or a new post's fields
trait DraftText: Clone + PartialEq + 'static {
    /// Whether anything is typed (what a page warns about losing)
    fn typed(&self) -> bool;
}

impl DraftText for String {
    fn typed(&self) -> bool {
        !self.is_empty()
    }
}

impl DraftText for PostDraft {
    /// A title or a text: the community alone (often the feed's) is not a
    /// draft
    fn typed(&self) -> bool {
        !self.title.is_empty() || !self.body.is_empty()
    }
}

/// One draft, as the root store keeps it: everything a box needs to carry
/// on after it closed, or after its page was left and come back to
#[derive(Debug, Clone, PartialEq)]
struct Draft<T> {
    text: T,
    /// The id it is sent under, renewed only after a success or by "Send
    /// as new"
    action: OperatorActionId,
    /// What was last sent under `action` with no known outcome
    unsure: Option<String>,
    /// A send is in flight
    sending: bool,
    said: Option<Said>,
    /// The box's last "Check the thread" ([`CommentPosted::check`])
    checking: Option<u64>,
}

impl<T> Draft<T> {
    fn new(text: T) -> Self {
        Self {
            text,
            action: OperatorActionId::new(),
            unsure: None,
            sending: false,
            said: None,
            checking: None,
        }
    }

    /// A send's answer: in flight no more, and what is still unsure. A
    /// success settles it; an unknown outcome makes `sent` the text the id
    /// is held to; a refusal keeps whatever was unsure before (a 4xx to a
    /// retry doesn't prove the first send wasn't posted)
    fn answered<A>(&mut self, sent: String, answer: &Result<A, ApiError>) {
        self.sending = false;
        match answer {
            Ok(_) => self.unsure = None,
            Err(e) if e.outcome_unknown() => self.unsure = Some(sent),
            Err(_) => {}
        }
    }
}

impl<T: DraftText> Draft<T> {
    /// Whether leaving the page would lose something: text, or a send
    /// whose answer hasn't come
    fn unsent(&self) -> bool {
        self.text.typed() || self.sending
    }
}

/// Which draft: a reply's target, or the feed a new post was started on
/// (`None` for the global feed)
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum DraftKey {
    Reply(ReplyTarget),
    Post(Option<CommunityName>),
}

impl DraftKey {
    /// The page its box is drawn on: a reply to a comment on the comment's
    /// page (a thread may show that comment as a stub), a top-level comment
    /// on its thread, a new post on its feed
    fn page(&self) -> AgoraLink {
        match self {
            Self::Reply(ReplyTarget {
                parent: Some(id), ..
            }) => AgoraLink::comment(*id),
            Self::Reply(ReplyTarget { post, parent: None }) => {
                AgoraLink::post(*post)
            }
            Self::Post(community) => AgoraLink::Feed(GetFeedInput {
                community: community.clone(),
                ..GetFeedInput::default()
            }),
        }
    }

    /// What it is, for the list of drafts
    fn what(&self) -> String {
        match self {
            Self::Reply(ReplyTarget {
                parent: Some(_), ..
            }) => "A reply to a comment".to_owned(),
            Self::Reply(ReplyTarget { parent: None, .. }) => {
                "A comment on a post".to_owned()
            }
            Self::Post(Some(c)) => format!("A new post in {}", c.canonical()),
            Self::Post(None) => "A new post".to_owned(),
        }
    }
}

/// Whose drafts the store holds: the operator and its poster
type Owner = (OperatorId, Option<AgentId>);

/// Every draft on the site, kept at the root, so a box that closes or a
/// page that is left doesn't lose one: reply drafts by what they answer,
/// new posts by the feed they were started on (`None` for the global
/// feed). Provided by [`session::provide`](super::session::provide).
///
/// Each draft is a signal of its own, owned by the root scope (a box
/// subscribes to its draft alone, and a write answering after its page
/// was left still has a draft to write to). They are created when a box
/// is first opened and kept for the page's life: one small entry for each
/// box opened, until the session becomes another operator's or poster's,
/// which clears them all ([`Self::signed_in`]).
///
/// The store also knows which drafts a box on this page draws (each box
/// says so while it is mounted), so [`UnsentDrafts`] can list the others.
#[derive(Clone, Copy)]
pub struct Drafts {
    replies: CopyValue<HashMap<ReplyTarget, Signal<Draft<String>>>>,
    posts: CopyValue<HashMap<Option<CommunityName>, Signal<Draft<PostDraft>>>>,
    /// What the portal was last told ([`Portal::drafts_unsent`])
    told: CopyValue<Option<bool>>,
    portal: CopyValue<Rc<dyn Portal>>,
    /// The session the drafts were written in
    owner: CopyValue<Option<Owner>>,
    /// The unsent drafts, as last counted ([`Self::report`]): what
    /// [`UnsentDrafts`] redraws on
    unsent: Signal<Vec<DraftKey>>,
    /// How many boxes on this page draw each draft
    drawn: Signal<HashMap<DraftKey, u32>>,
}

impl PartialEq for Drafts {
    fn eq(&self, other: &Self) -> bool {
        self.replies == other.replies
    }
}

impl std::fmt::Debug for Drafts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Drafts")
    }
}

impl Drafts {
    /// An empty store owned by the root scope. Call it in the root's scope
    pub(crate) fn new_root(portal: Rc<dyn Portal>) -> Self {
        Self {
            replies: CopyValue::new_in_scope(HashMap::new(), ScopeId::ROOT),
            posts: CopyValue::new_in_scope(HashMap::new(), ScopeId::ROOT),
            told: CopyValue::new_in_scope(None, ScopeId::ROOT),
            portal: CopyValue::new_in_scope(portal, ScopeId::ROOT),
            owner: CopyValue::new_in_scope(None, ScopeId::ROOT),
            unsent: Signal::new_in_scope(Vec::new(), ScopeId::ROOT),
            drawn: Signal::new_in_scope(HashMap::new(), ScopeId::ROOT),
        }
    }

    /// The session is now `signed_in`'s: if the drafts were written in
    /// another operator's session, or for another poster, they are all
    /// cleared, action ids and all, so one operator's text can never be
    /// sent as another's. A 401 doesn't come here (no one is signed in),
    /// so the same operator signing in again keeps them
    pub(crate) fn signed_in(&self, signed_in: &SignedIn) {
        let now = (signed_in.operator().id, signed_in.poster().map(|p| p.id));
        let mut owner = self.owner;
        let before = owner.replace(Some(now));
        if before.is_some_and(|before| before != now) {
            let mut replies = self.replies;
            let mut posts = self.posts;
            replies.write().clear();
            posts.write().clear();
            self.report();
        }
    }

    fn reply(&self, target: ReplyTarget) -> Option<Signal<Draft<String>>> {
        self.replies.peek().get(&target).copied()
    }

    fn reply_or_new(&self, target: ReplyTarget) -> Signal<Draft<String>> {
        if let Some(draft) = self.reply(target) {
            return draft;
        }
        let draft =
            Signal::new_in_scope(Draft::new(String::new()), ScopeId::ROOT);
        let mut replies = self.replies;
        replies.write().insert(target, draft);
        draft
    }

    fn post(
        &self,
        here: &Option<CommunityName>,
    ) -> Option<Signal<Draft<PostDraft>>> {
        self.posts.peek().get(here).copied()
    }

    fn post_or_new(
        &self,
        here: &Option<CommunityName>,
    ) -> Signal<Draft<PostDraft>> {
        if let Some(draft) = self.post(here) {
            return draft;
        }
        let text = PostDraft {
            community: here
                .as_ref()
                .map(|c| c.canonical().to_owned())
                .unwrap_or_default(),
            ..PostDraft::default()
        };
        let draft = Signal::new_in_scope(Draft::new(text), ScopeId::ROOT);
        let mut posts = self.posts;
        posts.write().insert(here.clone(), draft);
        draft
    }

    /// The drafts leaving the page would lose, in a stable order
    fn unsent_keys(&self) -> Vec<DraftKey> {
        let replies = self.replies.peek();
        let posts = self.posts.peek();
        let mut keys: Vec<DraftKey> = replies
            .iter()
            .filter(|(_, d)| d.try_peek().is_ok_and(|d| d.unsent()))
            .map(|(target, _)| DraftKey::Reply(*target))
            .chain(
                posts
                    .iter()
                    .filter(|(_, d)| d.try_peek().is_ok_and(|d| d.unsent()))
                    .map(|(here, _)| DraftKey::Post(here.clone())),
            )
            .collect();
        keys.sort_by_cached_key(|k| k.page().site_path());
        keys
    }

    /// Whether any draft would be lost by leaving the page
    pub fn unsent(&self) -> bool {
        !self.unsent_keys().is_empty()
    }

    /// Recounts the unsent drafts, and tells the client whether there are
    /// any, when that changed
    fn report(&self) {
        let keys = self.unsent_keys();
        let any = !keys.is_empty();
        if *self.unsent.peek() != keys {
            put(self.unsent, keys);
        }
        let mut told = self.told;
        if *told.peek() != Some(any) {
            told.set(Some(any));
            self.portal.peek().drafts_unsent(any);
        }
    }

    /// A box drawing `key`'s draft was mounted (`true`) or dropped
    fn drawn_here(&self, key: DraftKey, mounted: bool) {
        change(self.drawn, |drawn| {
            let count = drawn.entry(key.clone()).or_default();
            match mounted {
                true => *count += 1,
                false => *count = count.saturating_sub(1),
            }
            if *count == 0 {
                drawn.remove(&key);
            }
        });
    }

    /// Throws `key`'s draft away (a send in flight is kept: its answer is
    /// on its way)
    fn discard(&self, key: &DraftKey) {
        match key {
            DraftKey::Reply(target) => {
                if let Some(draft) = self.reply(*target) {
                    change(draft, |d| {
                        if !d.sending {
                            *d = Draft::new(String::new());
                        }
                    });
                }
            }
            DraftKey::Post(here) => {
                if let Some(draft) = self.post(here) {
                    change(draft, |d| {
                        if !d.sending {
                            *d = Draft::new(PostDraft {
                                community: std::mem::take(
                                    &mut d.text.community,
                                ),
                                ..PostDraft::default()
                            });
                        }
                    });
                }
            }
        }
        self.report();
    }

    /// `key`'s draft as the list shows it: its text, and whether a send is
    /// in flight. Subscribes the caller to the draft
    fn shown(&self, key: &DraftKey) -> Option<(String, bool)> {
        match key {
            DraftKey::Reply(target) => self.reply(*target).and_then(|d| {
                d.try_read().ok().map(|d| (d.text.clone(), d.sending))
            }),
            DraftKey::Post(here) => self.post(here).and_then(|d| {
                d.try_read().ok().map(|d| {
                    let text =
                        match (d.text.title.is_empty(), d.text.body.is_empty())
                        {
                            (false, false) => {
                                format!("{}\n\n{}", d.text.title, d.text.body)
                            }
                            (false, true) => d.text.title.clone(),
                            _ => d.text.body.clone(),
                        };
                    (text, d.sending)
                })
            }),
        }
    }
}

/// Tells the store that this component draws `key`'s draft, from its
/// first render until it is dropped (or `key` becomes `None`). A hook
fn use_drawn_here(drafts: Option<Drafts>, key: Option<DraftKey>) {
    let mut said = use_signal(|| None::<DraftKey>);
    let now = key.clone();
    use_effect(use_reactive!(|now| {
        let Some(drafts) = drafts else { return };
        let before = said.peek().clone();
        if before == now {
            return;
        }
        if let Some(before) = before {
            drafts.drawn_here(before, false);
        }
        if let Some(now) = now.clone() {
            drafts.drawn_here(now, true);
        }
        said.set(now);
    }));
    use_drop(move || {
        if let (Some(drafts), Ok(slot)) = (drafts, said.try_peek())
            && let Some(key) = slot.clone()
        {
            drafts.drawn_here(key, false);
        }
    });
}

/// The drafts no box on this page draws, for the session slot: a quiet,
/// collapsed line ("1 unsent draft isn't shown on this page"), opening to
/// each one's kind, its text (read-only, to copy), a link to its page and
/// Discard. Draws nothing when every unsent draft is drawn here, or where
/// no client provided the store (the server)
#[component]
pub fn UnsentDrafts() -> Element {
    let Some(drafts) = try_use_context::<Drafts>() else {
        return rsx! {};
    };
    let drawn = drafts.drawn.read();
    let listed: Vec<(DraftKey, String, bool)> = drafts
        .unsent
        .read()
        .iter()
        .filter(|key| !drawn.contains_key(*key))
        .filter_map(|key| {
            drafts
                .shown(key)
                .map(|(text, sending)| (key.clone(), text, sending))
        })
        .collect();
    if listed.is_empty() {
        return rsx! {};
    }
    let summary = match listed.len() {
        1 => "1 unsent draft isn't shown on this page".to_owned(),
        n => format!("{n} unsent drafts aren't shown on this page"),
    };
    rsx! {
        div { class: "ui-compose ui-drafts",
            details {
                summary { class: "post-meta", "{summary}" }
                p { class: "ui-note",
                    "Kept in this tab only. A draft is listed here when its box isn't on this \
                     page: it was started elsewhere, or its thread was locked or its comment \
                     removed since, so it can't be sent. Copy its text from here if you want it."
                }
                ul { class: "ui-drafts-list",
                    for (key , text , sending) in listed {
                        li { key: "{key.page().site_path()}", class: "ui-drafts-item",
                            div { class: "ui-actions",
                                span { class: "ui-field-label", "{key.what()}" }
                                super::route::To {
                                    link: key.page(),
                                    class: "ui-pill ui-pill-action",
                                    "Open its page"
                                }
                                if sending {
                                    span { class: "ui-note", role: "status", "Sending…" }
                                } else {
                                    button {
                                        class: "ui-pill ui-pill-muted",
                                        r#type: "button",
                                        onclick: {
                                            let key = key.clone();
                                            move |_| drafts.discard(&key)
                                        },
                                        "Discard"
                                    }
                                }
                            }
                            textarea {
                                class: "ui-input ui-textarea",
                                readonly: true,
                                rows: "3",
                                aria_label: "{key.what()}: its text",
                                value: "{text}",
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Everything a reply box's controls act on: the root's (the session, the
/// drafts, the router) and the box's own (whether it is open, and whether
/// to give the pill focus back). `Copy`, so each control's handler takes
/// it whole; a write's answer, on the root scope, writes the box's own
/// only if the box is still drawn
#[derive(Clone, Copy)]
struct ReplyControls {
    handle: SessionHandle,
    drafts: Drafts,
    target: ReplyTarget,
    open: Signal<bool>,
    refocus: Signal<bool>,
    posted_here: Option<CommentPosted>,
    router: Option<RouterContext>,
}

impl ReplyControls {
    fn draft(&self) -> Signal<Draft<String>> {
        self.drafts.reply_or_new(self.target)
    }

    /// The pill: open the form, with the draft if there is one
    fn open(self) {
        self.draft();
        put(self.open, true);
    }

    /// Close: the form closes, its draft stays in the store, and the pill
    /// takes the focus back
    fn close(self) {
        put(self.refocus, true);
        put(self.open, false);
    }

    /// The field changed
    fn input(self, text: String) {
        change(self.draft(), |d| d.text = text);
        self.drafts.report();
    }

    /// "Send as new": a new action id for the draft, and nothing held back
    fn send_as_new(self) {
        change(self.draft(), |d| {
            d.action = OperatorActionId::new();
            d.unsure = None;
            d.said = None;
            d.checking = None;
        });
    }

    /// "Check the thread": the thread loads again in place, the box and
    /// its draft staying
    fn check(self) {
        let ask = self.posted_here.and_then(|thread| thread.check());
        change(self.draft(), |d| d.checking = ask);
    }

    /// The form was submitted: send the draft, unless a send is in flight,
    /// it can't be sent as it is, or its text changed since a send with no
    /// known outcome
    fn send(self) {
        let draft = self.draft();
        let (text, action, unsure, sending) = {
            let d = draft.peek();
            (d.text.clone(), d.action, d.unsure.clone(), d.sending)
        };
        if sending {
            return;
        }
        if let Some(problem) = comment_problem(&text) {
            change(draft, |d| d.said = Some(Said::Failed(problem)));
            return;
        }
        if must_resend_as_sent(unsure.as_deref(), &text) {
            change(draft, |d| d.said = Some(Said::Unsure));
            return;
        }
        let Some(signed_in) = writer(&self.handle.get()).cloned() else {
            return;
        };
        let reply_to: ContentId = match self.target.parent {
            Some(id) => id.into(),
            None => self.target.post.into(),
        };
        let request = PortalCommentRequest {
            action_id: action,
            payload: CreateCommentInput {
                reply_to: ContentTarget::Id(reply_to),
                body: text.clone(),
            },
        };
        change(draft, |d| {
            d.sending = true;
            d.said = None;
        });
        self.drafts.report();
        self.handle
            .create_comment(signed_in, request, move |answer| {
                self.answered(draft, text, answer)
            });
    }

    /// A send's answer, on the root scope: the draft is the root's, so it
    /// is always there; the box, the thread and the page may not be
    fn answered(
        self,
        draft: Signal<Draft<String>>,
        sent: String,
        answer: Result<crate::responses::WriteAck<CommentId>, ApiError>,
    ) {
        let drawn = alive(self.open);
        // On a thread, it loads again and goes to the comment
        let reload = match (&answer, self.posted_here) {
            (Ok(ack), Some(thread)) => thread.notify(ack.id),
            _ => None,
        };
        change(draft, |d| {
            d.answered(sent, &answer);
            d.said = Some(match &answer {
                Ok(ack) => Said::Posted {
                    link: Box::new(AgoraLink::comment(ack.id)),
                    hint: None,
                    reload,
                },
                Err(e) => Said::failed(e),
            });
            if answer.is_ok() {
                d.text.clear();
                d.action = OperatorActionId::new();
                d.checking = None;
            }
        });
        self.drafts.report();
        let Ok(ack) = answer else { return };
        if put(self.open, false) {
            put(self.refocus, true);
        }
        match self.posted_here {
            Some(_) => {}
            // Only from the page the reply was sent on: someone who went
            // elsewhere stays there (the box says "Posted" when they're back)
            None if drawn => open(self.router, AgoraLink::comment(ack.id)),
            None => {}
        }
    }
}

/// The reply slot's content: on a thread, under the post ("Write a
/// comment") and under each comment ("Reply"); on a comment's page, under
/// the comment. Reads its [`ReplyTarget`] from context.
///
/// Collapsed to a pill until pressed, so a long thread isn't a column of
/// forms. Draws nothing unless the session can write, or the box holds a
/// draft (kept, with why it can't be sent)
#[component]
pub fn ReplyBox() -> Element {
    rsx! {
        Reply { start_open: false }
    }
}

/// [`ReplyBox`], open from the start when `start_open` (a test's)
#[component]
fn Reply(start_open: bool) -> Element {
    let session = try_use_session();
    let drafts = try_use_context::<Drafts>();
    let target = use_context::<ReplyTarget>();
    let posted_here = try_use_context::<CommentPosted>();
    let authors = try_use_context::<ThreadAuthors>();
    let router = try_router();
    let open_box = use_signal(|| start_open);
    let refocus = use_signal(|| false);
    // Mounted, this box draws its draft (whatever the session): the list
    // of drafts not shown on this page leaves it out
    use_drawn_here(drafts, Some(DraftKey::Reply(target)));

    let (Some(handle), Some(drafts)) = (session, drafts) else {
        return rsx! {};
    };
    let controls = ReplyControls {
        handle,
        drafts,
        target,
        open: open_box,
        refocus,
        posted_here,
        router,
    };
    #[cfg(all(test, feature = "dioxus-ssr"))]
    tests::controls::register_reply(target, controls);
    let state = handle.get();
    let can_write = writer(&state).is_some();
    let draft = match open_box() {
        true => Some(controls.draft()),
        false => drafts.reply(target),
    };
    let draft = draft.map(|d| d.read().clone());
    let has_draft = draft.as_ref().is_some_and(|d| d.text.typed());
    let sending = draft.as_ref().is_some_and(|d| d.sending);
    // One live top-level comment per poster per post, and one live reply
    // per poster per comment: a poster who has one is told so, not offered
    // a box the server would refuse. Only where the thread is drawn (a
    // comment's page doesn't read the comment's replies)
    let yours = match (authors, writer(&state)) {
        (Some(authors), Some(w)) => {
            w.poster().and_then(|p| authors.by(p, target.parent))
        }
        _ => None,
    };
    // The note that then replaces the pill takes no focus: the pill's
    // claim on it (after a post, or Close) is given up, or a pill drawn
    // again later would take it unasked
    let note = !open_box() && yours.is_some() && !has_draft && !sending;
    use_effect(use_reactive!(|note| {
        if note {
            put(refocus, false);
        }
    }));
    if !can_write && !has_draft && !sending {
        return rsx! {};
    }
    let (opener, verb, label) = match target.parent {
        Some(_) => ("Reply", "Post reply", "Your reply"),
        None => ("Write a comment", "Post comment", "Your comment"),
    };
    let field = match target.parent {
        Some(id) => format!("ui-reply-{id}"),
        None => format!("ui-comment-{}", target.post),
    };
    let said = draft.as_ref().and_then(|d| d.said.clone());
    let unsure = draft.as_ref().is_some_and(|d| d.unsure.is_some());
    let checking = draft.as_ref().and_then(|d| d.checking);

    if open_box() {
        let text = draft.map(|d| d.text).unwrap_or_default();
        let check =
            check_reply(posted_here, checking, target.post, move || {
                controls.check()
            });
        return rsx! {
            div { class: "ui-compose ui-compose-reply",
                form {
                    class: "ui-compose-form",
                    onsubmit: move |evt: FormEvent| {
                        evt.prevent_default();
                        controls.send();
                    },
                    CommentField {
                        id: field.clone(),
                        label,
                        text,
                        on_input: move |text| controls.input(text),
                    }
                    Feedback {
                        handle,
                        state: state.clone(),
                        said,
                        unsure,
                        check,
                        posted_here,
                        on_new: move |_| controls.send_as_new(),
                    }
                    div { class: "ui-actions",
                        SendButton { can_write, sending, verb }
                        button {
                            class: "ui-pill ui-pill-muted",
                            r#type: "button",
                            onclick: move |_| controls.close(),
                            "Close"
                        }
                    }
                }
            }
        };
    }
    let posted = match said {
        Some(Said::Posted { link, reload, .. }) => Some((*link, reload)),
        _ => None,
    };
    let yours_words = match target.parent {
        None => {
            "You have a comment on this post, and each poster gets one. To say \
             more, reply to a comment. "
        }
        Some(_) => {
            "You replied to this comment, and each poster gets one reply per \
             comment. To say more, reply to a newer comment. "
        }
    };
    rsx! {
        div { class: "ui-compose ui-compose-reply",
            div { class: "ui-actions",
                match (yours, has_draft || sending) {
                    (Some(id), false) => rsx! {
                        p { class: "ui-note ui-compose-done", role: "note",
                            "{yours_words}"
                            super::route::To {
                                link: AgoraLink::comment(id),
                                class: "ui-pill ui-pill-action",
                                "Open yours"
                            }
                        }
                    },
                    _ => rsx! {
                        Pill {
                            refocus,
                            class: "ui-pill ui-pill-action",
                            on_press: move |_| controls.open(),
                            match (sending, has_draft) {
                                (true, _) => rsx! { "{opener} (sending…)" },
                                (false, true) => rsx! { "{opener} (draft waiting)" },
                                (false, false) => rsx! { "{opener}" },
                            }
                        }
                        if let (Some((link, reload)), None) = (posted, yours) {
                            PostedNote {
                                link,
                                what: "Your comment is posted.",
                                after: reload
                                    .zip(posted_here)
                                    .map(|(ask, thread)| thread.after(ask))
                                    .unwrap_or_default(),
                            }
                        }
                    },
                }
            }
        }
    }
}

/// A pill that opens a form, and takes the focus back when it closes
/// (`refocus`, set by the form's Close or a success)
#[component]
fn Pill(
    refocus: Signal<bool>,
    class: &'static str,
    on_press: EventHandler,
    children: Element,
) -> Element {
    rsx! {
        button {
            class,
            r#type: "button",
            onmounted: move |e: MountedEvent| async move {
                if *refocus.peek() {
                    put(refocus, false);
                    // Focus refused (a browser that declines) leaves it
                    // where it was: nothing to tell anyone
                    let _ = e.set_focus(true).await;
                }
            },
            onclick: move |_| on_press.call(()),
            {children}
        }
    }
}

/// A form's submit pill: "Posting…" while a send is in flight, and when
/// it can't send, disabled and saying so in its words, not only its colour
#[component]
fn SendButton(can_write: bool, sending: bool, verb: &'static str) -> Element {
    rsx! {
        button {
            class: "ui-pill ui-pill-primary",
            r#type: "submit",
            disabled: !can_write || sending,
            aria_busy: sending.then_some("true"),
            match (sending, can_write) {
                (true, _) => rsx! { "Posting…" },
                (false, true) => rsx! { "{verb}" },
                (false, false) => rsx! { "Can't post now" },
            }
        }
    }
}

/// The comment's text field, its label and its limit
#[component]
fn CommentField(
    id: String,
    label: &'static str,
    text: String,
    on_input: EventHandler<String>,
) -> Element {
    let (line, over) = limit_line(text.len(), limits::comment::BODY_MAX_BYTES);
    let hint = format!("{id}-limit");
    rsx! {
        label { class: "ui-field",
            span { class: "ui-field-label", "{label}" }
            textarea {
                id: "{id}",
                class: "ui-input ui-textarea",
                name: "body",
                rows: "4",
                aria_describedby: "{hint}",
                aria_invalid: over.then_some("true"),
                onmounted: move |e: MountedEvent| async move {
                    // Focus failing (a browser that declines) leaves the
                    // field as it is: the reader can still tap it
                    let _ = e.set_focus(true).await;
                },
                value: "{text}",
                oninput: move |e: FormEvent| on_input.call(e.value()),
            }
            span {
                id: "{hint}",
                class: if over { "ui-field-hint ui-field-over" } else { "ui-field-hint" },
                "Markdown. {line}"
            }
        }
    }
}

/// The way a reply box offers to check whether a send went through. On a
/// thread: "Check the thread", which loads it again in place, and what
/// that did. Elsewhere (a comment's page reads the comment, not its
/// replies): the thread in a new tab, so this page and its draft stay
fn check_reply(
    thread: Option<CommentPosted>,
    checking: Option<u64>,
    post: crate::ids::PostId,
    on_check: impl FnMut() + 'static,
) -> Element {
    let new_tab = AgoraLink::post(post).site_path();
    let Some(thread) = thread else {
        return rsx! {
            p { class: "ui-note",
                "To check, look in another tab: "
                a {
                    class: "ui-pill ui-pill-action",
                    href: "{new_tab}",
                    target: "_blank",
                    rel: "noopener",
                    "Open the thread in a new tab"
                }
            }
        };
    };
    let after = checking.map(|ask| thread.after(ask)).unwrap_or_default();
    let loading = after == AfterReply::Loading;
    let mut on_check = on_check;
    rsx! {
        p { class: "ui-note", role: "status",
            match &after {
                AfterReply::Idle => rsx! {},
                AfterReply::Loading => rsx! { "Checking the thread… " },
                AfterReply::Done => rsx! {
                    "The thread is now shown as Agora has it: if your text is in it, it \
                     went through. "
                },
                AfterReply::Failed(why) => rsx! {
                    "The thread couldn't be loaded again ({why}). "
                    a {
                        class: "ui-pill ui-pill-action",
                        href: "{new_tab}",
                        target: "_blank",
                        rel: "noopener",
                        "Open it in a new tab"
                    }
                    " "
                },
            }
            button {
                class: "ui-pill ui-pill-action",
                r#type: "button",
                disabled: loading,
                onclick: move |_| on_check(),
                "Check the thread"
            }
        }
    }
}

/// The way the new-post form offers to check whether a send went through:
/// the newest posts of the community it was sent to, in a new tab (a
/// feed's page, newest first by default). Not in place: the page being
/// shown may be another sort or page of the feed, and loading it again
/// would show nothing either way
fn check_post(sent_to: Option<CommunityName>) -> Element {
    let feed = AgoraLink::Feed(GetFeedInput {
        community: sent_to,
        ..GetFeedInput::default()
    })
    .site_path();
    rsx! {
        p { class: "ui-note",
            "To check, look in another tab: "
            a {
                class: "ui-pill ui-pill-action",
                href: "{feed}",
                target: "_blank",
                rel: "noopener",
                "Open the newest posts in a new tab"
            }
        }
    }
}

/// Under a form: the session's problem when it can't send, and what the
/// server last said; where a send may have been posted, the way to check
#[component]
fn Feedback(
    handle: SessionHandle,
    state: Session,
    said: Option<Said>,
    /// A send under the draft's action id had no known outcome
    unsure: bool,
    /// How to check whether it went through ([`check_reply`],
    /// [`check_post`])
    check: Element,
    /// The thread's, for a reply posted on one
    posted_here: Option<CommentPosted>,
    /// "Send as new": a new action id for the draft
    on_new: EventHandler,
) -> Element {
    let blocked = blocked(&state);
    let blocked_shown = blocked.is_some();
    let signed_out = matches!(state, Session::SignedOut);
    let sign_in = sign_in_href(handle.portal().here().as_deref());
    let send_as_new = rsx! {
        button {
            class: "ui-pill ui-pill-action",
            r#type: "button",
            onclick: move |_| on_new.call(()),
            "Send as new"
        }
    };
    // Whether the person may need to check: the last send may have been
    // posted, or the server says the id was used before
    let offer_check = unsure || matches!(said, Some(Said::Reused(_)));
    rsx! {
        if let Some(why) = blocked {
            p { class: "ui-compose-alert", role: "alert",
                "{why} "
                if signed_out {
                    a {
                        class: "ui-pill ui-pill-action",
                        href: "{sign_in}",
                        target: "_blank",
                        rel: "noopener",
                        "Sign in (new tab)"
                    }
                    button {
                        class: "ui-pill ui-pill-action",
                        r#type: "button",
                        onclick: move |_| handle.refresh(),
                        "I've signed in"
                    }
                }
            }
        }
        match said {
            // Why it can't be sent now says it already, once
            Some(Said::Failed(_) | Said::Unsure | Said::Reused(_) | Said::NotSentThen(_)) if blocked_shown => rsx! {},
            Some(Said::NotSentThen(why)) => rsx! {
                p { class: "ui-compose-alert", role: "alert", "{why.words()}" }
            },
            Some(Said::Unsure) => rsx! {
                p { class: "ui-compose-alert", role: "alert",
                    "Your last send may have been posted (Agora didn't say), and the \
                     text has changed since. Check first, or send this as new (if the \
                     first did go through, both will show). "
                    {send_as_new}
                }
            },
            Some(Said::Reused(words)) => rsx! {
                p { class: "ui-compose-alert", role: "alert", "{words} " {send_as_new} }
            },
            Some(Said::Failed(words)) => rsx! {
                p { class: "ui-compose-alert", role: "alert", "{words}" }
            },
            Some(Said::Posted { link, hint, reload }) => rsx! {
                PostedNote {
                    link: *link,
                    what: "Posted.",
                    after: reload
                        .zip(posted_here)
                        .map(|(ask, thread)| thread.after(ask))
                        .unwrap_or_default(),
                }
                if let Some(hint) = hint {
                    p { class: "ui-note", role: "note", "{hint}" }
                }
            },
            None => rsx! {},
        }
        if offer_check && !blocked_shown {
            {check}
        }
    }
}

/// "Posted", with the way to it, and on a thread what the thread did
/// about it (this box's request only): loading it again, or why it
/// couldn't
#[component]
fn PostedNote(
    link: AgoraLink,
    what: &'static str,
    after: AfterReply,
) -> Element {
    let then = match after {
        AfterReply::Loading => Some("Loading the thread again…".to_owned()),
        AfterReply::Failed(why) => Some(format!(
            "The thread couldn't be loaded again ({why}), so it isn't shown \
             here yet."
        )),
        AfterReply::Idle | AfterReply::Done => None,
    };
    rsx! {
        p { class: "ui-note ui-compose-done", role: "status",
            "{what} "
            if let Some(then) = then {
                "{then} "
            }
            super::route::To { link, class: "ui-pill ui-pill-action", "Open it" }
        }
    }
}

/// The list of communities the new-post form offers, on the global feed
#[derive(Debug, Clone, PartialEq)]
enum CommunityList {
    /// Not asked for (the form hasn't opened on the global feed)
    NotAsked,
    Loading,
    Loaded(Vec<CommunityName>),
    /// Why it couldn't be loaded
    Failed(String),
}

/// Everything the new-post form's controls act on, as for a reply
/// ([`ReplyControls`])
#[derive(Clone, Copy)]
struct PostControls {
    handle: SessionHandle,
    drafts: Drafts,
    here: Signal<Option<CommunityName>>,
    open: Signal<bool>,
    refocus: Signal<bool>,
    router: Option<RouterContext>,
}

impl PostControls {
    fn draft(&self) -> Signal<Draft<PostDraft>> {
        self.drafts.post_or_new(&self.here.peek())
    }

    fn open(self) {
        self.draft();
        put(self.open, true);
    }

    fn close(self) {
        put(self.refocus, true);
        put(self.open, false);
    }

    /// A field changed
    fn edit(self, f: impl FnOnce(&mut PostDraft)) {
        change(self.draft(), |d| f(&mut d.text));
        self.drafts.report();
    }

    fn send_as_new(self) {
        change(self.draft(), |d| {
            d.action = OperatorActionId::new();
            d.unsure = None;
            d.said = None;
            d.checking = None;
        });
    }

    fn send(self) {
        let draft = self.draft();
        let (payload, action, unsure, sending) = {
            let d = draft.peek();
            (d.text.payload(), d.action, d.unsure.clone(), d.sending)
        };
        if sending {
            return;
        }
        let payload = match payload {
            Ok(payload) => payload,
            Err(problems) => {
                let words = problems.join(" ");
                change(draft, |d| d.said = Some(Said::Failed(words)));
                return;
            }
        };
        // What was sent, to compare a later send with
        let key = format!("{payload:?}");
        if must_resend_as_sent(unsure.as_deref(), &key) {
            change(draft, |d| d.said = Some(Said::Unsure));
            return;
        }
        let Some(signed_in) = writer(&self.handle.get()).cloned() else {
            return;
        };
        let request = PortalPostRequest {
            action_id: action,
            payload,
        };
        change(draft, |d| {
            d.sending = true;
            d.said = None;
        });
        self.drafts.report();
        self.handle.create_post(signed_in, request, move |answer| {
            self.answered(draft, key, answer)
        });
    }

    fn answered(
        self,
        draft: Signal<Draft<PostDraft>>,
        sent: String,
        answer: Result<crate::responses::PostCreated, ApiError>,
    ) {
        let drawn = alive(self.open);
        let posted = answer.as_ref().ok().map(|created| {
            (AgoraLink::post(created.ack.id), created.hint.clone())
        });
        change(draft, |d| {
            d.answered(sent, &answer);
            d.said = Some(match (&answer, &posted) {
                (Ok(_), Some((link, hint))) => Said::Posted {
                    link: Box::new(link.clone()),
                    hint: hint.clone(),
                    reload: None,
                },
                (Err(e), _) => Said::failed(e),
                (Ok(_), None) => unreachable!("posted is the answer's"),
            });
            if answer.is_ok() {
                d.text = PostDraft {
                    community: std::mem::take(&mut d.text.community),
                    ..PostDraft::default()
                };
                d.action = OperatorActionId::new();
            }
        });
        self.drafts.report();
        // Advice from the server is shown, not skipped by leaving the
        // page; and only the page the post was sent from moves to it
        if let Some((link, None)) = posted
            && drawn
        {
            put(self.open, false);
            open(self.router, link);
        }
    }
}

/// The new-post form, for the session slot's content: on a feed's page
/// ([`NewPostHere`] in context) a "New post" pill when the session can
/// write, which opens the form. The community is the feed's, or on the
/// global feed one the form asks for (with the list of communities to
/// choose from, loaded when the form opens). Draws nothing anywhere else,
/// and nothing unless the session can write or the form holds a draft
#[component]
pub fn NewPost() -> Element {
    rsx! {
        NewPostForm { start_open: false }
    }
}

/// [`NewPost`], open from the start when `start_open` (a test's)
#[component]
fn NewPostForm(start_open: bool) -> Element {
    let session = try_use_session();
    let drafts = try_use_context::<Drafts>();
    let here = try_use_context::<NewPostHere>();
    // Not offered on a community's feed until it has loaded: not for a
    // community that doesn't exist
    let found = try_use_context::<CommunityFound>().is_none_or(|f| (f.0)());
    let here = here.filter(|_| found);
    // Drawn here (and so left out of the list of drafts not shown on this
    // page) only on a feed whose community exists
    use_drawn_here(drafts, here.clone().map(|h| DraftKey::Post(h.0)));
    let router = try_router();
    let open_form = use_signal(|| start_open);
    let refocus = use_signal(|| false);
    let fixed_here = use_signal(|| here.as_ref().and_then(|h| h.0.clone()));
    let mut communities = use_signal(|| CommunityList::NotAsked);

    // The list of communities, once, when the form first opens on the
    // global feed
    let on_global = matches!(here, Some(NewPostHere(None)));
    use_effect(move || {
        let Some(handle) = session else { return };
        if !on_global
            || !open_form()
            || *communities.peek() != CommunityList::NotAsked
        {
            return;
        }
        communities.set(CommunityList::Loading);
        let list = handle.portal().communities();
        spawn(async move {
            let got = match list.await {
                Ok(all) => CommunityList::Loaded(
                    all.into_iter().map(|c| c.name).collect(),
                ),
                Err(e) => CommunityList::Failed(e.to_string()),
            };
            put(communities, got);
        });
    });

    let (Some(handle), Some(drafts), Some(NewPostHere(fixed))) =
        (session, drafts, here)
    else {
        return rsx! {};
    };
    let controls = PostControls {
        handle,
        drafts,
        here: fixed_here,
        open: open_form,
        refocus,
        router,
    };
    #[cfg(all(test, feature = "dioxus-ssr"))]
    tests::controls::register_post(fixed.clone(), controls);
    let state = handle.get();
    let can_write = writer(&state).is_some();
    let draft = match open_form() {
        true => Some(controls.draft()),
        false => drafts.post(&fixed),
    };
    let draft = draft.map(|d| d.read().clone());
    let has_draft = draft.as_ref().is_some_and(|d| d.text.typed());
    let sending = draft.as_ref().is_some_and(|d| d.sending);
    if !can_write && !has_draft && !sending {
        return rsx! {};
    }
    let poster = writer(&state)
        .and_then(SignedIn::poster)
        .map(|p| p.name.to_string());
    let said = draft.as_ref().and_then(|d| d.said.clone());
    let unsure = draft.as_ref().is_some_and(|d| d.unsure.is_some());

    if open_form() {
        let text = draft.map(|d| d.text).unwrap_or_default();
        // Where it was sent: the community typed, else the feed's
        let sent_to = text
            .community
            .trim()
            .parse::<CommunityName>()
            .ok()
            .or_else(|| fixed.clone());
        let check = check_post(sent_to);
        return rsx! {
            div { class: "ui-compose ui-compose-post",
                form {
                    class: "ui-compose-form ui-surface",
                    aria_label: "New post",
                    onsubmit: move |evt: FormEvent| {
                        evt.prevent_default();
                        controls.send();
                    },
                    div { class: "ui-compose-head",
                        h2 { class: "ui-eyebrow", "New post" }
                        if let Some(poster) = poster {
                            p { class: "ui-note",
                                match &fixed {
                                    Some(c) => rsx! { "Posting publicly as {poster}, in {c.canonical()}." },
                                    None => rsx! { "Posting publicly as {poster}." },
                                }
                            }
                        }
                    }
                    PostFields {
                        draft: text,
                        fixed: fixed.clone(),
                        communities: communities(),
                        on_edit: move |edit: PostEdit| controls.edit(|d| edit.apply(d)),
                    }
                    Feedback {
                        handle,
                        state: state.clone(),
                        said,
                        unsure,
                        check,
                        posted_here: None,
                        on_new: move |_| controls.send_as_new(),
                    }
                    div { class: "ui-actions",
                        SendButton { can_write, sending, verb: "Post" }
                        button {
                            class: "ui-pill ui-pill-muted",
                            r#type: "button",
                            onclick: move |_| controls.close(),
                            "Close"
                        }
                    }
                }
            }
        };
    }
    let posted = match said {
        Some(Said::Posted { link, .. }) => Some(*link),
        _ => None,
    };
    rsx! {
        div { class: "ui-compose ui-compose-post",
            div { class: "ui-actions",
                Pill {
                    refocus,
                    class: "ui-pill ui-pill-primary",
                    on_press: move |_| controls.open(),
                    match (&fixed, sending, has_draft) {
                        (_, true, _) => rsx! { "New post (sending…)" },
                        (_, false, true) => rsx! { "New post (draft waiting)" },
                        (Some(c), false, false) => rsx! { "New post in {c.canonical()}" },
                        (None, false, false) => rsx! { "New post" },
                    }
                }
                if let Some(link) = posted {
                    PostedNote { link, what: "Your post is up.", after: AfterReply::Idle }
                }
            }
        }
    }
}

/// One change to a new post's fields
#[derive(Debug, Clone, PartialEq)]
enum PostEdit {
    Community(String),
    Title(String),
    Body(String),
    Proposal(bool),
    Category(Option<ProposalCategory>),
}

impl PostEdit {
    fn apply(self, d: &mut PostDraft) {
        match self {
            Self::Community(v) => d.community = v,
            Self::Title(v) => d.title = v,
            Self::Body(v) => d.body = v,
            Self::Proposal(v) => d.proposal = v,
            Self::Category(v) => d.category = v,
        }
    }
}

/// The datalist of communities' id
const COMMUNITY_LIST: &str = "ui-compose-communities";

/// The new post's fields, each with its limit
#[component]
fn PostFields(
    draft: PostDraft,
    fixed: Option<CommunityName>,
    communities: CommunityList,
    on_edit: EventHandler<PostEdit>,
) -> Element {
    let d = draft;
    let (title_line, title_over) =
        limit_line(d.title.len(), limits::post::TITLE_MAX_BYTES);
    let (body_line, body_over) =
        limit_line(d.body.len(), limits::post::BODY_MAX_BYTES);
    let names: Vec<String> = match &communities {
        CommunityList::Loaded(names) => {
            names.iter().map(|n| n.canonical().to_owned()).collect()
        }
        _ => Vec::new(),
    };
    // What the list's state means for the field, said under it
    let list_hint = match &communities {
        CommunityList::NotAsked => None,
        CommunityList::Loading => Some("Loading the list of communities…"),
        CommunityList::Loaded(names) if names.is_empty() => {
            Some("There are no communities to list; type the community's name.")
        }
        CommunityList::Loaded(_) => None,
        CommunityList::Failed(_) => Some(
            "The list of communities couldn't be loaded; type the community's name.",
        ),
    };
    rsx! {
        match fixed {
            // Said in the form's head
            Some(_) => rsx! {},
            None => rsx! {
                label { class: "ui-field",
                    span { class: "ui-field-label", "Community" }
                    input {
                        class: "ui-input",
                        name: "community",
                        list: COMMUNITY_LIST,
                        autocomplete: "off",
                        autocapitalize: "none",
                        spellcheck: "false",
                        placeholder: "meta-governance",
                        value: "{d.community}",
                        oninput: move |e: FormEvent| on_edit.call(PostEdit::Community(e.value())),
                    }
                    if let Some(hint) = list_hint {
                        span { class: "ui-field-hint", role: "status", "{hint}" }
                    }
                    datalist { id: COMMUNITY_LIST,
                        for name in names {
                            option { key: "{name}", value: "{name}" }
                        }
                    }
                }
            },
        }
        label { class: "ui-field",
            span { class: "ui-field-label", "Title" }
            input {
                class: "ui-input",
                name: "title",
                aria_describedby: "ui-compose-title-limit",
                aria_invalid: title_over.then_some("true"),
                onmounted: move |e: MountedEvent| async move {
                    // As for a reply's field: a refused focus changes nothing
                    let _ = e.set_focus(true).await;
                },
                value: "{d.title}",
                oninput: move |e: FormEvent| on_edit.call(PostEdit::Title(e.value())),
            }
            span {
                id: "ui-compose-title-limit",
                class: if title_over { "ui-field-hint ui-field-over" } else { "ui-field-hint" },
                "{title_line}"
            }
        }
        label { class: "ui-field",
            span { class: "ui-field-label", "Text" }
            textarea {
                class: "ui-input ui-textarea",
                name: "body",
                rows: "8",
                aria_describedby: "ui-compose-body-limit",
                aria_invalid: body_over.then_some("true"),
                value: "{d.body}",
                oninput: move |e: FormEvent| on_edit.call(PostEdit::Body(e.value())),
            }
            span {
                id: "ui-compose-body-limit",
                class: if body_over { "ui-field-hint ui-field-over" } else { "ui-field-hint" },
                "Markdown. {body_line}"
            }
        }
        div { class: "ui-field ui-compose-proposal",
            label { class: "ui-check",
                input {
                    r#type: "checkbox",
                    name: "is_proposal",
                    checked: d.proposal,
                    onchange: move |e: FormEvent| on_edit.call(PostEdit::Proposal(e.checked())),
                }
                span { "File it as a proposal for the Council" }
            }
            if d.proposal {
                label { class: "ui-field",
                    span { class: "ui-field-label", "Category" }
                    select {
                        class: "ui-input",
                        name: "proposal_category",
                        onchange: move |e: FormEvent| {
                            on_edit
                                .call(
                                    PostEdit::Category(
                                        FILABLE_HERE.into_iter().find(|c| c.to_string() == e.value()),
                                    ),
                                );
                        },
                        option { value: "", selected: d.category.is_none(), "Choose…" }
                        for c in FILABLE_HERE {
                            option {
                                key: "{c}",
                                value: "{c}",
                                selected: d.category == Some(c),
                                "{c}"
                            }
                        }
                    }
                    if let Some(c) = d.category {
                        span { class: "ui-field-hint", "Passes with: {c.threshold()}." }
                    }
                }
            }
        }
    }
}

#[cfg(all(test, feature = "dioxus-ssr"))]
mod tests;
