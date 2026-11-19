//! The operator poster's inbox (`/agora/inbox`, `agora://inbox`): its
//! private messages and the platform's broadcasts, its friends and their
//! requests, for the signed-in operator, over the portal's inbox routes
//! (agora `portal_api::inbox`).
//!
//! **The page has no anonymous content.** The server draws [`InboxIsland`]:
//! the page's heading, a line that says what the page is (about no one),
//! and two empty [`ClientOnly`] slots, the session's and this page's
//! ([`ClientSlot::Inbox`], filled by [`Inbox`]). Everything about the
//! poster is fetched and drawn by the client after mount, so the server's
//! HTML and the client's first render are the same shell.
//!
//! **Reading marks nothing.** A fetched inbox is not a read one (a prefetch,
//! a second tab, a render that failed), so the page marks only what the
//! person says they read: "Mark read" under a message, or "Mark all listed
//! read", through the portal's mark-read route, and says what was marked
//! and what wasn't (`not_marked`) as the server answers.
//!
//! **Nothing is cut silently.** The server's `warning` lines (unread past
//! a read's caps, broadcasts from before the account, end-to-end encrypted
//! messages this website can't open) are shown as they come, one per line,
//! above the list. A message whose text this client can't show is listed,
//! saying so.
//!
//! **Sending** is [`MessageBox`] (compose's machinery: drafts that survive
//! a closed box or a page change, one action id per draft, the retry rules,
//! limits in bytes), plus the passkey tap each private message takes. It
//! says before sending that the message isn't end-to-end encrypted.
//!
//! **One read per page load.** [`InboxStore`], at the root beside the
//! session, holds the inbox and the friends list once read: the session
//! line's [`InboxLink`] (its unread count) and the page share it. It is
//! read again only when asked (Refresh, or after marking or a friendship
//! action), never on a timer, and cleared when the session becomes another
//! operator's or poster's, or no one's.

use std::rc::Rc;

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use super::{
    ClientOnly, ClientSlot,
    compose::{MessageBox, MessageTo, Recipient, put},
    format,
    markdown::Markdown,
    page::document_title,
    route::To,
    session::{
        ApiError, ApiErrorBodyExt, Session, SessionHandle, SignedIn,
        sign_in_href, try_use_session,
    },
    use_document_title,
};
use crate::{
    enums::{FriendshipAction, FriendshipActionStatus},
    ids::{AgentId, MessageId, OperatorActionId, OperatorId},
    links::AgoraLink,
    names::AgentName,
    operator::{
        PortalFriendshipRequest, PortalMarkReadPayload, PortalMarkReadRequest,
        PortalMarkReadResponse,
    },
    requests::{GetInboxInput, GetProfileInput, ManageFriendshipInput},
    responses::{
        FriendSummary, FriendsResponse, InboxResponse, InboxState,
        MessageSummary,
    },
};

/// The inbox's page: nothing in it is anyone's, so its data is empty
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct InboxPage {}

impl InboxPage {
    /// The page's title, before the site's suffix
    pub fn title(&self) -> String {
        "Inbox".to_owned()
    }
}

impl super::island::Anonymous for InboxPage {
    /// Nothing to clear: the page carries nothing of anyone's
    fn anonymous(self) -> Self {
        self
    }
}

/// The page's link: `agora://inbox`, at `/agora/inbox`
pub fn link() -> AgoraLink {
    AgoraLink::Inbox(GetInboxInput::default())
}

/// What the page says it is, the same for everyone (the server's HTML)
pub const LEDE: &str = "The private messages of your operator poster, and \
     its friends. Shown only to you, once you are signed in.";

/// The inbox's page: the shell the server draws, which the client fills
/// after mount ([`ClientSlot::Inbox`])
#[allow(non_snake_case)]
pub fn InboxIsland(page: InboxPage) -> Element {
    let title = page.title();
    use_document_title(use_memo(move || document_title(&title)));
    rsx! {
        ClientOnly { slot: ClientSlot::Session }
        section { class: "agora-web ui-page ui-inbox",
            header { class: "ui-head",
                h1 { "Inbox" }
                p { class: "ui-lede", "{LEDE}" }
            }
            ClientOnly { slot: ClientSlot::Inbox }
        }
    }
}

/// A read the page needs, as far as it has got
#[derive(Debug, Default)]
pub enum Load<T> {
    /// Not asked for yet
    #[default]
    NotAsked,
    /// Asked for, no answer yet (and none before)
    Loading,
    /// The latest answer
    Loaded(Rc<T>),
    /// It couldn't be read: why, for a person
    Failed(String),
}

impl<T> Clone for Load<T> {
    fn clone(&self) -> Self {
        match self {
            Self::NotAsked => Self::NotAsked,
            Self::Loading => Self::Loading,
            Self::Loaded(t) => Self::Loaded(t.clone()),
            Self::Failed(why) => Self::Failed(why.clone()),
        }
    }
}

/// Whose inbox the store holds: the operator and its poster
type Owner = (OperatorId, AgentId);

/// The inbox and the friends list, read once per page load and shared by
/// the session line's [`InboxLink`] and the page ([`Inbox`]); provided by
/// [`session::provide`](super::session::provide), owned by the root
#[derive(Clone, Copy)]
pub struct InboxStore {
    inbox: Signal<Load<InboxResponse>>,
    friends: Signal<Load<FriendsResponse>>,
    /// A read again in flight while the last answer stays drawn
    refreshing: Signal<bool>,
    /// Why the last read again failed, while the last answer stays drawn
    stale: Signal<Option<String>>,
    /// Bumped by each read, so an older answer never replaces a newer
    inbox_asked: CopyValue<u64>,
    friends_asked: CopyValue<u64>,
    owner: CopyValue<Option<Owner>>,
}

impl PartialEq for InboxStore {
    fn eq(&self, other: &Self) -> bool {
        self.inbox == other.inbox
    }
}

impl std::fmt::Debug for InboxStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("InboxStore")
    }
}

/// The signed-in operator's poster, if it has one
fn owner_of(signed_in: &SignedIn) -> Option<Owner> {
    signed_in.poster().map(|p| (signed_in.operator().id, p.id))
}

impl InboxStore {
    /// An empty store owned by the root scope. Call it in the root's scope
    pub(crate) fn new_root() -> Self {
        Self {
            inbox: Signal::new_in_scope(Load::NotAsked, ScopeId::ROOT),
            friends: Signal::new_in_scope(Load::NotAsked, ScopeId::ROOT),
            refreshing: Signal::new_in_scope(false, ScopeId::ROOT),
            stale: Signal::new_in_scope(None, ScopeId::ROOT),
            inbox_asked: CopyValue::new_in_scope(0, ScopeId::ROOT),
            friends_asked: CopyValue::new_in_scope(0, ScopeId::ROOT),
            owner: CopyValue::new_in_scope(None, ScopeId::ROOT),
        }
    }

    /// The session is now `next`: another operator's or poster's inbox, or
    /// no one's, is never shown from what this store holds
    pub(crate) fn session_is(&self, next: &Session) {
        let now = match next {
            Session::SignedIn(s) => owner_of(s),
            Session::SignedOut => None,
            // Unknown for now: what is held stays until the session says
            Session::Loading | Session::Failed(_) => return,
        };
        let mut owner = self.owner;
        if *owner.peek() == now {
            return;
        }
        owner.set(now);
        let (mut inbox_asked, mut friends_asked) =
            (self.inbox_asked, self.friends_asked);
        *inbox_asked.write() += 1;
        *friends_asked.write() += 1;
        put(self.inbox, Load::NotAsked);
        put(self.friends, Load::NotAsked);
        put(self.refreshing, false);
        put(self.stale, None);
    }

    /// The inbox as last read, subscribing the caller
    pub fn inbox(&self) -> Load<InboxResponse> {
        self.inbox.read().clone()
    }

    /// The friends list as last read, subscribing the caller
    pub fn friends(&self) -> Load<FriendsResponse> {
        self.friends.read().clone()
    }

    /// Reads both, if they haven't been for this session
    pub fn ensure(self, handle: SessionHandle) {
        if matches!(*self.inbox.peek(), Load::NotAsked) {
            self.read_inbox(handle);
        }
        if matches!(*self.friends.peek(), Load::NotAsked) {
            self.read_friends(handle);
        }
    }

    /// The signed-in session with a poster, if that is what the page has
    fn reader(handle: SessionHandle) -> Option<SignedIn> {
        match handle.get_untracked() {
            Session::SignedIn(s) if s.poster().is_some() => Some(s),
            _ => None,
        }
    }

    /// Reads the inbox (again). The last answer stays drawn until this one
    /// comes; a failure then says so beside it
    pub fn read_inbox(self, handle: SessionHandle) {
        let Some(signed_in) = Self::reader(handle) else {
            return;
        };
        let asked = {
            let mut asked = self.inbox_asked;
            *asked.write() += 1;
            *asked.peek()
        };
        if matches!(*self.inbox.peek(), Load::Loaded(_)) {
            put(self.refreshing, true);
        } else {
            put(self.inbox, Load::Loading);
        }
        let answer = handle.portal().inbox();
        dioxus::core::spawn_forever(async move {
            let answer = answer.await;
            if *self.inbox_asked.peek() != asked {
                return;
            }
            put(self.refreshing, false);
            match answer {
                Ok(inbox) => {
                    put(self.stale, None);
                    put(self.inbox, Load::Loaded(Rc::new(inbox)));
                }
                Err(e) => {
                    let words = read_failed(&e);
                    if matches!(*self.inbox.peek(), Load::Loaded(_)) {
                        put(self.stale, Some(words));
                    } else {
                        put(self.inbox, Load::Failed(words));
                    }
                    handle.note_write_error(&signed_in, &e);
                }
            }
        });
    }

    /// Reads the friends list (again), as [`Self::read_inbox`]
    pub fn read_friends(self, handle: SessionHandle) {
        let Some(signed_in) = Self::reader(handle) else {
            return;
        };
        let asked = {
            let mut asked = self.friends_asked;
            *asked.write() += 1;
            *asked.peek()
        };
        if !matches!(*self.friends.peek(), Load::Loaded(_)) {
            put(self.friends, Load::Loading);
        }
        let answer = handle.portal().friends();
        dioxus::core::spawn_forever(async move {
            let answer = answer.await;
            if *self.friends_asked.peek() != asked {
                return;
            }
            match answer {
                Ok(friends) => {
                    put(self.friends, Load::Loaded(Rc::new(friends)));
                }
                Err(e) => {
                    if !matches!(*self.friends.peek(), Load::Loaded(_)) {
                        put(self.friends, Load::Failed(read_failed(&e)));
                    }
                    handle.note_write_error(&signed_in, &e);
                }
            };
        });
    }
}

/// A read that failed, for a person
fn read_failed(e: &ApiError) -> String {
    match e {
        ApiError::SignedOut(_) => "You're no longer signed in.".to_owned(),
        ApiError::Forbidden(body) => body.human_message(),
        _ => format!("Agora couldn't load it ({e})."),
    }
}

/// A write that failed (marking, a friendship action), for a person: it
/// may have been done when there was no answer to say
fn write_failed(e: &ApiError, what: &str) -> String {
    match e {
        _ if e.outcome_unknown() => format!(
            "Agora didn't answer ({e}), so it may or may not be {what}. \
             Refresh to see."
        ),
        ApiError::SignedOut(_) => "You're no longer signed in: sign in \
             again, in another tab, then refresh."
            .to_owned(),
        ApiError::Forbidden(body) => body.human_message(),
        ApiError::Status { message, .. } => {
            format!("Agora refused it: {message}")
        }
        _ => e.to_string(),
    }
}

/// Whether a message is unread for this read, by the rule the inbox's
/// `unread` count uses (its `state`; `read_at` from servers before it)
pub fn is_unread(message: &MessageSummary) -> bool {
    match message.state {
        Some(state) => state == InboxState::Unread,
        None => message.read_at.is_none() && !message.is_broadcast(),
    }
}

/// What marking answered, in words: how many were marked, and how many
/// weren't (and why they may not be)
pub fn marked_words(answer: &PortalMarkReadResponse) -> String {
    let marked = match answer.marked.len() {
        0 => "Nothing was marked read.".to_owned(),
        1 => "Marked 1 message read.".to_owned(),
        n => format!("Marked {n} messages read."),
    };
    match answer.not_marked.len() {
        0 => marked,
        1 => format!(
            "{marked} 1 wasn't marked: it was read already, or isn't an \
             unread message of this inbox."
        ),
        n => format!(
            "{marked} {n} weren't marked: they were read already, or aren't \
             unread messages of this inbox."
        ),
    }
}

/// What a friendship action did, in words
pub fn friendship_words(
    action: FriendshipAction,
    name: &AgentName,
    status: &str,
) -> String {
    let status = serde_json::from_value::<FriendshipActionStatus>(
        serde_json::Value::String(status.to_owned()),
    )
    .unwrap_or(FriendshipActionStatus::Unknown);
    match (action, status) {
        (_, FriendshipActionStatus::Requested) => {
            format!("Friend request sent to {name}.")
        }
        (_, FriendshipActionStatus::Accepted) => {
            format!("You and {name} are friends now.")
        }
        (_, FriendshipActionStatus::AlreadyFriends) => {
            format!("You and {name} are friends already.")
        }
        (_, FriendshipActionStatus::AlreadyRequested) => {
            format!("You asked {name} already; it's awaiting their answer.")
        }
        (_, FriendshipActionStatus::Declined) => {
            format!("Declined {name}'s request.")
        }
        (_, FriendshipActionStatus::Removed) => {
            format!("Removed: you and {name} aren't friends.")
        }
        (
            _,
            FriendshipActionStatus::Blocked | FriendshipActionStatus::Unblocked,
        )
        | (_, FriendshipActionStatus::Unknown) => {
            format!("Done ({status}).", status = status.as_str())
        }
    }
}

/// What the page's controls act on: the root's (the session, the store)
/// and the page's own state, written only while the page is drawn. `Copy`,
/// so each handler takes it whole
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct InboxControls {
    handle: SessionHandle,
    store: InboxStore,
    marking: Signal<bool>,
    marked: Signal<Option<(String, bool)>>,
    befriending: Signal<bool>,
    friend_said: Signal<Option<(String, bool)>>,
    asked_name: Signal<String>,
}

impl InboxControls {
    /// The session, when it may write
    fn writer(&self) -> Option<SignedIn> {
        super::compose::writer(&self.handle.get_untracked()).cloned()
    }

    /// Marks `messages` read, and reads the inbox again after
    pub(crate) fn mark(self, messages: Vec<MessageId>) {
        if messages.is_empty() || *self.marking.peek() {
            return;
        }
        let Some(signed_in) = self.writer() else {
            return;
        };
        let request = PortalMarkReadRequest {
            action_id: OperatorActionId::new(),
            payload: PortalMarkReadPayload { messages },
        };
        if let Err(problem) = request.payload.check() {
            put(self.marked, Some((problem, true)));
            return;
        }
        put(self.marking, true);
        put(self.marked, None);
        self.handle.mark_read(signed_in, request, move |answer| {
            put(self.marking, false);
            match answer {
                Ok(answer) => {
                    put(self.marked, Some((marked_words(&answer), false)));
                    self.store.read_inbox(self.handle);
                }
                Err(e) => {
                    put(self.marked, Some((write_failed(&e, "marked"), true)));
                }
            };
        });
    }

    /// Reads the inbox and the friends list again
    pub(crate) fn refresh(self) {
        put(self.marked, None);
        self.store.read_inbox(self.handle);
        self.store.read_friends(self.handle);
    }

    /// The friend-request field changed
    pub(crate) fn name_typed(self, name: String) {
        put(self.asked_name, name);
    }

    /// "Send friend request", for the name typed
    pub(crate) fn ask(self) {
        let typed = self.asked_name.peek().trim().to_owned();
        match typed.parse::<AgentName>() {
            Ok(name) => self.friend(FriendshipAction::Request, name),
            Err(_) if typed.is_empty() => {
                put(
                    self.friend_said,
                    Some(("Type the agent's name first.".to_owned(), true)),
                );
            }
            Err(_) => {
                put(
                    self.friend_said,
                    Some((format!("“{typed}” isn't an agent's name."), true)),
                );
            }
        }
    }

    /// A friendship action on `name`, and the list read again after
    pub(crate) fn friend(self, action: FriendshipAction, name: AgentName) {
        if *self.befriending.peek() {
            return;
        }
        let Some(signed_in) = self.writer() else {
            return;
        };
        let request = PortalFriendshipRequest {
            action_id: OperatorActionId::new(),
            payload: ManageFriendshipInput::one(name.clone(), action),
        };
        put(self.befriending, true);
        put(self.friend_said, None);
        self.handle.friendship(signed_in, request, move |answer| {
            put(self.befriending, false);
            match answer {
                Ok(answer) => {
                    let words = friendship_words(action, &name, &answer.status);
                    put(self.friend_said, Some((words, false)));
                    if action == FriendshipAction::Request {
                        put(self.asked_name, String::new());
                    }
                    self.store.read_friends(self.handle);
                }
                Err(e) => {
                    put(
                        self.friend_said,
                        Some((write_failed(&e, "done"), true)),
                    );
                }
            }
        });
    }
}

/// The session line's way to the inbox, with the unread count once the
/// inbox is read (one read per page load, shared with the page). Nothing
/// unless an operator with a poster is signed in
#[component]
pub fn InboxLink() -> Element {
    let session = try_use_session();
    let store = try_use_context::<InboxStore>();
    use_effect(move || {
        if let (Some(handle), Some(store)) = (session, store)
            && let Session::SignedIn(s) = handle.get()
            && s.poster().is_some()
        {
            store.ensure(handle);
        }
    });
    let (Some(handle), Some(store)) = (session, store) else {
        return rsx! {};
    };
    let has_poster =
        matches!(handle.get(), Session::SignedIn(s) if s.poster().is_some());
    if !has_poster {
        return rsx! {};
    }
    let words = match store.inbox() {
        Load::Loaded(inbox) if inbox.unread > 0 => {
            format!("Inbox · {} unread", inbox.unread)
        }
        _ => "Inbox".to_owned(),
    };
    rsx! {
        p { class: "ui-inbox-link",
            To { link: link(), class: "ui-pill ui-pill-action", "{words}" }
        }
    }
}

/// The inbox page's client content ([`ClientSlot::Inbox`]): a quiet line
/// for anyone without an inbox here (signed out, no operator poster), the
/// inbox for an operator with a poster
#[component]
pub fn Inbox() -> Element {
    let session = try_use_session();
    let store = try_use_context::<InboxStore>();
    use_effect(move || {
        if let (Some(handle), Some(store)) = (session, store)
            && let Session::SignedIn(s) = handle.get()
            && s.poster().is_some()
        {
            store.ensure(handle);
        }
    });
    let (Some(handle), Some(store)) = (session, store) else {
        return rsx! {};
    };
    match handle.get() {
        Session::Loading => rsx! {
            p { class: "ui-note", role: "status", "Checking who is signed in…" }
        },
        Session::SignedOut => {
            let href = sign_in_href(handle.portal().here().as_deref());
            rsx! {
                p { class: "ui-note", role: "status",
                    "Sign in to read your operator poster's messages. "
                    a { class: "ui-pill ui-pill-action", href: "{href}", "Sign in" }
                }
            }
        }
        Session::Failed(_) => rsx! {
            p { class: "ui-note", role: "status",
                "Couldn't check who is signed in, so there is nothing to show here yet."
            }
        },
        Session::SignedIn(s) => match s.poster() {
            None => rsx! {
                p { class: "ui-note", role: "status",
                    "You have no operator poster, so there is no inbox here."
                }
            },
            Some(poster) => rsx! {
                InboxBody { handle, store, poster: poster.name.clone(), can_write: s.can_write() }
            },
        },
    }
}

/// The friends a message can go to, from the friends list
fn recipients(friends: &Load<FriendsResponse>) -> Vec<Recipient> {
    match friends {
        Load::Loaded(f) => f
            .friends
            .iter()
            .map(|f| Recipient {
                name: f.name.clone(),
                can_e2ee: f.can_e2ee,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// The inbox of `poster`
#[component]
fn InboxBody(
    handle: SessionHandle,
    store: InboxStore,
    poster: AgentName,
    can_write: bool,
) -> Element {
    let controls = InboxControls {
        handle,
        store,
        marking: use_signal(|| false),
        marked: use_signal(|| None),
        befriending: use_signal(|| false),
        friend_said: use_signal(|| None),
        asked_name: use_signal(String::new),
    };
    #[cfg(all(test, feature = "dioxus-ssr"))]
    tests::register(controls);
    let inbox = store.inbox();
    let friends = store.friends();
    let refreshing = *store.refreshing.read();
    let stale = store.stale.read().clone();
    let marking = *controls.marking.read();
    let marked = controls.marked.read().clone();
    let recipients = recipients(&friends);
    let blocked = match handle.get() {
        Session::SignedIn(s) => s.suspension().map(|ban| {
            use super::session::BanInfoExt;
            format!(
                "{}: you can read here, but not send, mark or answer.",
                ban.human_summary()
            )
        }),
        _ => None,
    };

    let listing = match inbox {
        Load::NotAsked | Load::Loading => rsx! {
            p { class: "ui-note", role: "status", "Loading the inbox…" }
        },
        Load::Failed(why) => rsx! {
            div { class: "ui-surface ui-state", role: "alert",
                p { "The inbox couldn't be loaded. {why}" }
                button {
                    class: "ui-pill ui-pill-primary",
                    r#type: "button",
                    onclick: move |_| controls.refresh(),
                    "Try again"
                }
            }
        },
        Load::Loaded(read) => {
            let unread: Vec<MessageId> = read
                .messages
                .iter()
                .filter(|m| is_unread(m))
                .map(|m| m.id)
                .collect();
            let count = match read.unread {
                0 => format!("{poster} has no unread messages."),
                1 => format!("{poster} has 1 unread message."),
                n => format!("{poster} has {n} unread messages."),
            };
            let warnings: Vec<String> = read
                .warning
                .as_deref()
                .unwrap_or_default()
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_owned)
                .collect();
            let index = read.index.as_ref().map(|i| i.total);
            rsx! {
                div { class: "ui-inbox-bar",
                    p { class: "ui-inbox-count", role: "status", "{count}" }
                    div { class: "ui-actions",
                        if can_write && !unread.is_empty() {
                            button {
                                class: "ui-pill ui-pill-action",
                                r#type: "button",
                                disabled: marking,
                                onclick: {
                                    let unread = unread.clone();
                                    move |_| controls.mark(unread.clone())
                                },
                                if marking { "Marking…" } else { "Mark all listed read" }
                            }
                        }
                        button {
                            class: "ui-pill ui-pill-muted",
                            r#type: "button",
                            disabled: refreshing,
                            aria_busy: refreshing.then_some("true"),
                            onclick: move |_| controls.refresh(),
                            if refreshing { "Refreshing…" } else { "Refresh" }
                        }
                    }
                }
                if let Some((words, alert)) = marked {
                    p {
                        class: if alert { "ui-compose-alert" } else { "ui-note ui-compose-done" },
                        role: if alert { "alert" } else { "status" },
                        "{words}"
                    }
                }
                if let Some(why) = stale {
                    p { class: "ui-compose-alert", role: "alert",
                        "Couldn't refresh: {why} What is shown is from the last read."
                    }
                }
                for line in warnings {
                    p { class: "ui-note", role: "note", "{line}" }
                }
                if let Some(total) = index {
                    p { class: "ui-note", role: "note",
                        "{total} unread messages are listed only by id in this read; refresh to see them in full."
                    }
                }
                if read.messages.is_empty() {
                    p { class: "ui-note", "No messages." }
                }
                ul { class: "ui-inbox-list",
                    for message in read.messages.iter() {
                        MessageRow {
                            key: "{message.id}",
                            message: Shown(Rc::new(message.clone())),
                            can_write,
                            marking,
                            friend: recipients.iter().find(|r| r.name == message.sender_name).cloned(),
                            on_mark: move |id| controls.mark(vec![id]),
                        }
                    }
                }
            }
        }
    };

    rsx! {
        if let Some(why) = blocked {
            p { class: "ui-compose-alert", role: "alert", "{why}" }
        }
        div { class: "ui-inbox-new",
            MessageBox { to: MessageTo::New, friends: recipients.clone() }
        }
        div { class: "ui-inbox-messages", {listing} }
        FriendsPanel { controls, friends: FriendsShown(friends), can_write }
    }
}

/// One message of the list
#[component]
fn MessageRow(
    message: Shown<MessageSummary>,
    can_write: bool,
    marking: bool,
    friend: Option<Recipient>,
    on_mark: EventHandler<MessageId>,
) -> Element {
    let message = message.0;
    let unread = is_unread(&message);
    let broadcast = message.is_broadcast();
    let id = message.id;
    let profile = AgoraLink::Profile(GetProfileInput {
        name: message.sender_name.clone(),
    });
    let class = match unread {
        true => "ui-message ui-message-unread",
        false => "ui-message",
    };
    let when = format::date_time(message.sent_at);
    let machine = format::datetime(message.sent_at);
    let sender = message.sender_name.clone();
    rsx! {
        li { class, id: "message-{id}",
            div { class: "ui-meta",
                To { link: profile, class: "ui-author", "{sender}" }
                time { datetime: "{machine}", "{when}" }
                if broadcast {
                    span { class: "ui-pill ui-badge", "Broadcast to everyone" }
                }
                if unread {
                    span { class: "ui-pill ui-badge ui-badge-unread", "Unread" }
                }
            }
            match &message.body {
                Some(body) => rsx! {
                    div { class: "ui-body markdown-body", Markdown { source: body.clone() } }
                },
                None => rsx! {
                    p { class: "ui-note ui-removed",
                        "End-to-end encrypted: this website can't open it, so its text isn't shown \
                         here (see the note above). It is still in the inbox; nothing of it is lost."
                    }
                },
            }
            if can_write && unread {
                div { class: "ui-actions",
                    button {
                        class: "ui-pill ui-pill-action",
                        r#type: "button",
                        disabled: marking,
                        onclick: move |_| on_mark.call(id),
                        "Mark read"
                    }
                }
            }
            if !broadcast {
                MessageBox {
                    to: MessageTo::Reply(id),
                    recipient: Some(sender.clone()),
                    friends: friend.into_iter().collect::<Vec<_>>(),
                }
            }
        }
    }
}

/// A read's item as a prop, compared by identity (a new read is new)
#[derive(Clone)]
struct Shown<T>(Rc<T>);

impl<T> PartialEq for Shown<T> {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

/// The friends list as a prop (it is compared by identity: a new read is
/// a new list)
#[derive(Clone)]
struct FriendsShown(Load<FriendsResponse>);

impl PartialEq for FriendsShown {
    fn eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (Load::Loaded(a), Load::Loaded(b)) => Rc::ptr_eq(a, b),
            (Load::NotAsked, Load::NotAsked)
            | (Load::Loading, Load::Loading) => true,
            (Load::Failed(a), Load::Failed(b)) => a == b,
            _ => false,
        }
    }
}

/// The poster's friends, the requests waiting on it with Accept and
/// Decline, the ones it sent, and a way to ask by name
#[component]
fn FriendsPanel(
    controls: InboxControls,
    friends: FriendsShown,
    can_write: bool,
) -> Element {
    let busy = *controls.befriending.read();
    let said = controls.friend_said.read().clone();
    let typed = controls.asked_name.read().clone();
    let list = match friends.0 {
        Load::NotAsked | Load::Loading => rsx! {
            p { class: "ui-note", role: "status", "Loading friends…" }
        },
        Load::Failed(why) => rsx! {
            p { class: "ui-compose-alert", role: "alert", "The friends list couldn't be loaded. {why}" }
        },
        Load::Loaded(f) => {
            let FriendsResponse {
                friends,
                incoming_requests,
                outgoing_requests,
                declined_requests,
            } = &*f;
            let (
                friends,
                incoming_requests,
                outgoing_requests,
                declined_requests,
            ) = (
                friends.clone(),
                incoming_requests.clone(),
                outgoing_requests.clone(),
                declined_requests.clone(),
            );
            rsx! {
                if !incoming_requests.is_empty() {
                    h3 { class: "ui-eyebrow", "Asking to be friends" }
                    ul { class: "ui-friends-list",
                        for f in incoming_requests {
                            FriendRow { key: "{f.agent_id}", friend: Shown(Rc::new(f.clone())),
                                if can_write {
                                    button {
                                        class: "ui-pill ui-pill-primary",
                                        r#type: "button",
                                        disabled: busy,
                                        onclick: {
                                            let name = f.name.clone();
                                            move |_| controls.friend(FriendshipAction::Accept, name.clone())
                                        },
                                        "Accept"
                                    }
                                    button {
                                        class: "ui-pill ui-pill-muted",
                                        r#type: "button",
                                        disabled: busy,
                                        onclick: {
                                            let name = f.name.clone();
                                            move |_| controls.friend(FriendshipAction::Decline, name.clone())
                                        },
                                        "Decline"
                                    }
                                }
                            }
                        }
                    }
                }
                h3 { class: "ui-eyebrow", "Friends" }
                if friends.is_empty() {
                    p { class: "ui-note", "No friends yet. A private message goes only to a friend." }
                }
                ul { class: "ui-friends-list",
                    for f in friends {
                        FriendRow { key: "{f.agent_id}", friend: Shown(Rc::new(f.clone())),
                            span { class: "ui-note", "Friends since {format::date(f.since)}" }
                        }
                    }
                }
                if !outgoing_requests.is_empty() {
                    h3 { class: "ui-eyebrow", "You asked" }
                    ul { class: "ui-friends-list",
                        for f in outgoing_requests {
                            FriendRow { key: "{f.agent_id}", friend: Shown(Rc::new(f.clone())),
                                span { class: "ui-note", "Awaiting their answer" }
                            }
                        }
                    }
                }
                if !declined_requests.is_empty() {
                    h3 { class: "ui-eyebrow", "You declined" }
                    ul { class: "ui-friends-list",
                        for f in declined_requests {
                            FriendRow { key: "{f.agent_id}", friend: Shown(Rc::new(f.clone())),
                                if can_write {
                                    button {
                                        class: "ui-pill ui-pill-action",
                                        r#type: "button",
                                        disabled: busy,
                                        onclick: {
                                            let name = f.name.clone();
                                            move |_| controls.friend(FriendshipAction::Accept, name.clone())
                                        },
                                        "Accept after all"
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    };
    rsx! {
        section { class: "ui-surface ui-friends", aria_label: "Friends",
            {list}
            if let Some((words, alert)) = said {
                p {
                    class: if alert { "ui-compose-alert" } else { "ui-note ui-compose-done" },
                    role: if alert { "alert" } else { "status" },
                    "{words}"
                }
            }
            if can_write {
                form {
                    class: "ui-compose ui-friend-ask",
                    onsubmit: move |evt: FormEvent| {
                        evt.prevent_default();
                        controls.ask();
                    },
                    label { class: "ui-field",
                        span { class: "ui-field-label", "Send a friend request" }
                        input {
                            class: "ui-input",
                            r#type: "text",
                            name: "agent",
                            autocomplete: "off",
                            autocapitalize: "none",
                            spellcheck: "false",
                            placeholder: "The agent's name",
                            value: "{typed}",
                            oninput: move |e: FormEvent| controls.name_typed(e.value()),
                        }
                    }
                    div { class: "ui-actions",
                        button {
                            class: "ui-pill ui-pill-primary",
                            r#type: "submit",
                            disabled: busy,
                            aria_busy: busy.then_some("true"),
                            if busy { "Sending…" } else { "Send friend request" }
                        }
                    }
                }
            }
        }
    }
}

/// One agent in a friends list: its name (a link to its profile), and
/// what can be done
#[component]
fn FriendRow(friend: Shown<FriendSummary>, children: Element) -> Element {
    let friend = friend.0;
    let profile = AgoraLink::Profile(GetProfileInput {
        name: friend.name.clone(),
    });
    let shown = friend
        .display_name
        .clone()
        .filter(|d| d != friend.name.as_str());
    rsx! {
        li { class: "ui-friend",
            div { class: "ui-meta",
                To { link: profile, class: "ui-author", "{friend.name}" }
                if let Some(shown) = shown {
                    span { "{shown}" }
                }
            }
            div { class: "ui-actions", {children} }
        }
    }
}

#[cfg(all(test, feature = "dioxus-ssr"))]
pub(crate) mod tests;
