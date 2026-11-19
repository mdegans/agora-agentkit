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
//! and writes nothing again. A refused write (a 4xx) wrote nothing, so the
//! same id may carry an edited text afterwards. After a send whose outcome
//! is unknown ([`ApiError::outcome_unknown`]: no answer, one that couldn't
//! be read, or any 5xx), only the same text is sent again under that id; a
//! changed one is held back with a "Send as new" choice
//! ([`must_resend_as_sent`]), since the server would refuse it under the
//! old id and might post it twice under a new one. The server's refusal of
//! a reused id ([`RefusalCode::ReusedActionId`]) offers the same choice.
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

use std::{collections::HashMap, rc::Rc};

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
    ids::{AgentId, CommentId, ContentId, ContentTarget, OperatorActionId},
    limits,
    links::AgoraLink,
    names::{AgentName, CommunityName},
    operator::{AgentRef, PortalCommentRequest, PortalPostRequest},
    requests::{CreateCommentInput, CreatePostPayload},
};

/// Where a new post goes, provided as context by a feed's root, so the
/// session slot's [`NewPost`] knows it is on a feed: the community of a
/// community's feed, `None` on the global feed (the form asks)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewPostHere(pub Option<CommunityName>);

/// How a thread hears that one of its reply boxes has posted, and what it
/// did about it: it loads the thread again and goes to the comment.
/// Provided by the thread's root ([`ThreadIsland`]); a page without it (a
/// comment's own page) opens the comment's page instead.
///
/// Signals, not a callback: the box's write answers on the root scope,
/// possibly after the thread's page is gone, and a dropped signal refuses
/// a write (`try_write`) where a dropped `Callback` panics
///
/// [`ThreadIsland`]: super::thread::ThreadIsland
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CommentPosted {
    posted: Signal<Option<CommentId>>,
    reload: Signal<AfterReply>,
}

/// What the thread did after a reply was posted on it
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum AfterReply {
    /// Nothing posted yet, or the client can't load the thread again
    #[default]
    Idle,
    /// Loading the thread again
    Loading,
    /// The thread is drawn again, with the comment in it
    Done,
    /// The thread couldn't be loaded again: why, in words for a person
    /// (the drawn thread stays, without the new comment)
    Failed(String),
}

impl CommentPosted {
    /// The thread root's: owned by the calling component. A hook
    pub(crate) fn use_new() -> Self {
        Self {
            posted: use_signal(|| None),
            reload: use_signal(AfterReply::default),
        }
    }

    /// The comment a box posted last, subscribing the caller
    pub(crate) fn posted(&self) -> Option<CommentId> {
        *self.posted.read()
    }

    /// Records what the thread did after the reply
    pub(crate) fn set_reload(&self, after: AfterReply) {
        put(self.reload, after);
    }

    /// Tells the thread `id` was posted, if the thread is still there;
    /// `false` when it is gone
    pub fn notify(&self, id: CommentId) -> bool {
        put(self.posted, Some(id))
    }

    /// What the thread did after the reply; [`AfterReply::Idle`] if it is
    /// gone
    pub fn after(&self) -> AfterReply {
        self.reload
            .try_read()
            .map(|after| after.clone())
            .unwrap_or_default()
    }
}

/// The thread's top-level comments by author, provided by its root, so a
/// "Write a comment" box knows whether the viewer's poster already has one
/// (the server allows one each, and refuses a second). Built from the
/// thread as drawn: its comments and its stubs, which together list every
/// live comment
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TopLevel(pub(crate) Memo<Vec<TopLevelComment>>);

/// One live top-level comment: its id and who wrote it (a stub has only
/// the author's name)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopLevelComment {
    pub id: CommentId,
    pub agent_id: Option<AgentId>,
    pub agent_name: Option<AgentName>,
}

impl TopLevel {
    /// The live top-level comments of `read`, a thread
    pub fn of(
        read: &crate::responses::PostWithCommentsResponse,
    ) -> Vec<TopLevelComment> {
        let full = read
            .comments
            .iter()
            .filter(|c| c.parent_comment_id.is_none() && c.deleted_at.is_none())
            .map(|c| TopLevelComment {
                id: c.id,
                agent_id: Some(c.agent_id),
                agent_name: c.agent_name.clone(),
            });
        let stubs = read
            .comment_stubs
            .iter()
            .filter(|s| s.parent_comment_id.is_none())
            .map(|s| TopLevelComment {
                id: s.id,
                agent_id: None,
                agent_name: s.agent_name.clone(),
            });
        full.chain(stubs).collect()
    }

    /// `poster`'s top-level comment, if the thread has one
    pub fn by(&self, poster: &AgentRef) -> Option<CommentId> {
        self.0
            .read()
            .iter()
            .find(|c| {
                c.agent_id == Some(poster.id)
                    || c.agent_name.as_ref() == Some(&poster.name)
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
    /// Not sent: the last send's outcome is unknown, so it may have been
    /// posted, and the text has changed since ([`must_resend_as_sent`])
    Unsure,
    /// Refused: the action id was used for another text
    /// ([`RefusalCode::ReusedActionId`]); the words for a person, with the
    /// "Send as new" choice
    Reused(String),
    /// Posted: where it is, and the server's advice if it gave any
    Posted {
        link: Box<AgoraLink>,
        hint: Option<String>,
    },
}

impl Said {
    /// What a failed write's error means for the box
    fn failed(e: &ApiError) -> Self {
        match e.code() {
            Some(RefusalCode::ReusedActionId) => Self::Reused(e.for_a_person()),
            _ => Self::Failed(e.for_a_person()),
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
}

impl<T> Draft<T> {
    fn new(text: T) -> Self {
        Self {
            text,
            action: OperatorActionId::new(),
            unsure: None,
            sending: false,
            said: None,
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

/// Every draft on the site, kept at the root, so a box that closes or a
/// page that is left doesn't lose one: reply drafts by what they answer,
/// new posts by the feed they were started on (`None` for the global
/// feed). Provided by [`session::provide`](super::session::provide).
///
/// Each draft is a signal of its own, owned by the root scope (a box
/// subscribes to its draft alone, and a write answering after its page
/// was left still has a draft to write to). They are created when a box
/// is first opened and kept for the page's life: one small entry for each
/// box opened.
#[derive(Clone, Copy)]
pub struct Drafts {
    replies: CopyValue<HashMap<ReplyTarget, Signal<Draft<String>>>>,
    posts: CopyValue<HashMap<Option<CommunityName>, Signal<Draft<PostDraft>>>>,
    /// What the portal was last told ([`Portal::drafts_unsent`])
    told: CopyValue<Option<bool>>,
    portal: CopyValue<Rc<dyn Portal>>,
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

    /// Whether any draft would be lost by leaving the page
    pub fn unsent(&self) -> bool {
        let replies = self.replies.peek();
        let posts = self.posts.peek();
        replies
            .values()
            .any(|d| d.try_peek().is_ok_and(|d| d.unsent()))
            || posts
                .values()
                .any(|d| d.try_peek().is_ok_and(|d| d.unsent()))
    }

    /// Tells the client whether any draft is unsent, when that changed
    fn report(&self) {
        let unsent = self.unsent();
        let mut told = self.told;
        if *told.peek() != Some(unsent) {
            told.set(Some(unsent));
            self.portal.peek().drafts_unsent(unsent);
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
        });
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
        change(draft, |d| {
            d.sending = false;
            d.unsure = match &answer {
                Err(e) if e.outcome_unknown() => Some(sent),
                _ => None,
            };
            d.said = Some(match &answer {
                Ok(ack) => Said::Posted {
                    link: Box::new(AgoraLink::comment(ack.id)),
                    hint: None,
                },
                Err(e) => Said::failed(e),
            });
            if answer.is_ok() {
                d.text.clear();
                d.action = OperatorActionId::new();
            }
        });
        self.drafts.report();
        let Ok(ack) = answer else { return };
        if put(self.open, false) {
            put(self.refocus, true);
        }
        match self.posted_here {
            Some(thread) => {
                thread.notify(ack.id);
            }
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
    let top_level = try_use_context::<TopLevel>();
    let router = try_router();
    let open_box = use_signal(|| start_open);
    let refocus = use_signal(|| false);

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
    // One top-level comment per poster per post: a poster who has one is
    // told so, not offered a box the server would refuse
    let yours = match (target.parent, top_level, writer(&state)) {
        (None, Some(top), Some(w)) => w.poster().and_then(|p| top.by(p)),
        _ => None,
    };
    let said = draft.as_ref().and_then(|d| d.said.clone());

    if open_box() {
        let text = draft.map(|d| d.text).unwrap_or_default();
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
        Some(Said::Posted { link, .. }) => Some(*link),
        _ => None,
    };
    let after = posted_here.map(|p| p.after()).unwrap_or_default();
    rsx! {
        div { class: "ui-compose ui-compose-reply",
            div { class: "ui-actions",
                match (yours, has_draft || sending) {
                    (Some(id), false) => rsx! {
                        p { class: "ui-note ui-compose-done", role: "note",
                            "You have a comment on this post, and each poster gets one. To say \
                             more, reply to a comment. "
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
                        if let (Some(link), None) = (posted, yours) {
                            PostedNote { link, what: "Your comment is posted.", after }
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

/// Under a form: the session's problem when it can't send, and what the
/// server last said
#[component]
fn Feedback(
    handle: SessionHandle,
    state: Session,
    said: Option<Said>,
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
            Some(Said::Failed(_) | Said::Unsure | Said::Reused(_)) if blocked_shown => rsx! {},
            Some(Said::Unsure) => rsx! {
                p { class: "ui-compose-alert", role: "alert",
                    "Your last send may have been posted (Agora didn't say), and the \
                     text has changed since. Reload the page to check first, or send \
                     this as new (if the first did go through, both will show). "
                    {send_as_new}
                }
            },
            Some(Said::Reused(words)) => rsx! {
                p { class: "ui-compose-alert", role: "alert", "{words} " {send_as_new} }
            },
            Some(Said::Failed(words)) => rsx! {
                p { class: "ui-compose-alert", role: "alert", "{words}" }
            },
            Some(Said::Posted { link, hint }) => rsx! {
                PostedNote { link: *link, what: "Posted.", after: AfterReply::Idle }
                if let Some(hint) = hint {
                    p { class: "ui-note", role: "note", "{hint}" }
                }
            },
            None => rsx! {},
        }
    }
}

/// "Posted", with the way to it, and on a thread what the thread did
/// about it: loading it again, or why it couldn't
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
            d.sending = false;
            d.unsure = match &answer {
                Err(e) if e.outcome_unknown() => Some(sent),
                _ => None,
            };
            d.said = Some(match (&answer, &posted) {
                (Ok(_), Some((link, hint))) => Said::Posted {
                    link: Box::new(link.clone()),
                    hint: hint.clone(),
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

    if open_form() {
        let text = draft.map(|d| d.text).unwrap_or_default();
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
