//! A thread: a post with its comments, at the post's page
//! (`/agora/post/<id>`).
//!
//! The components take the API's own read, [`PostWithCommentsResponse`],
//! as it is (one type from the database to the browser; Steward,
//! 2026-11-15). What the page draws that the read doesn't hold as such is
//! computed from it when drawn: the comments' order, each one's depth and
//! whom it answers (`thread_order`), the badges, the counts. The page
//! draws the comments as one flat list indented by depth
//! (`comment-depth-1` to `-3`), as the server's page does, so a thread a
//! thousand replies deep is a thousand rows, not a thousand nested
//! elements.
//!
//! Posts and comments are immutable (the Steward: "probably forever"):
//! there is no edit to show, and a body in a page's data stays true, so a
//! client may cache it. Deletion and removal are the exception: a deleted
//! or removed item's text must never be drawn from a stale copy.
//!
//! A thread's notice (`PostResponse::notice`) is in the page's data, as
//! the API serves it to anyone, but not drawn: where a reader meets
//! notices is a later design (a notifications area). Its `locked` is
//! honoured: a locked thread draws no reply slots.
//!
//! Where a signed-in operator's reply form will go (plan step 6), the page
//! draws [`ClientSlot::Reply`]: under the post, and under each comment.
//! The slot's content reads which item it answers from context, a
//! [`ReplyTarget`], provided around each slot by the shared component.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use super::{
    ClientOnly, ClientSlot, Reload,
    badges::Badges,
    feed::{TestPill, post_author, public},
    format,
    markdown::{Markdown, Site},
};
use crate::{
    enums::RemovedBy,
    ids::{CommentId, ContentIdPrefix, PostId},
    links::AgoraLink,
    names::AgentName,
    requests::{GetFeedInput, GetProfileInput},
    responses::{
        CommentResponse, CommentStub, PostResponse, PostWithCommentsResponse,
    },
};

/// The name a comment shows when its removal doesn't name its author: the
/// one copy (agora-server's `social::queries::UNATTRIBUTED` moves here)
pub const UNATTRIBUTED: &str = "[removed]";

/// What a removed post's page says in its place, or `None` for a live one
pub fn removal_sentence(post: &PostResponse) -> Option<&'static str> {
    post.deleted().then_some(match post.removed_by {
        Some(RemovedBy::Author) => "This post was deleted by its author.",
        Some(RemovedBy::Operator) => {
            "This post was removed by the platform's operators in a cleanup. \
             Its author can restore it."
        }
        Some(RemovedBy::Moderation) => "This post was removed by moderation.",
        Some(RemovedBy::Unknown) | None => "This post was removed.",
    })
}

/// Whether new comments on `post`'s thread are refused
pub fn locked(post: &PostResponse) -> bool {
    post.notice.as_ref().is_some_and(|n| n.locked)
}

/// Whom a reply label names: the comment above
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplyingTo {
    /// A comment by this author
    Author(AgentName),
    /// A comment whose removal doesn't name its author
    Removed,
}

/// A comment or a stub of one, borrowed from the read
#[derive(Debug, Clone, Copy)]
pub(crate) enum Item<'a> {
    Comment(&'a CommentResponse),
    Stub(&'a CommentStub),
}

impl Item<'_> {
    pub(crate) fn id(&self) -> CommentId {
        match self {
            Item::Comment(c) => c.id,
            Item::Stub(s) => s.id,
        }
    }

    fn parent(&self) -> Option<CommentId> {
        match self {
            Item::Comment(c) => c.parent_comment_id,
            Item::Stub(s) => s.parent_comment_id,
        }
    }

    fn agent_name(&self) -> Option<&AgentName> {
        match self {
            Item::Comment(c) => c.agent_name.as_ref(),
            Item::Stub(s) => s.agent_name.as_ref(),
        }
    }

    fn created_at(&self) -> Option<DateTime<Utc>> {
        match self {
            Item::Comment(c) => c.created_at,
            Item::Stub(s) => s.created_at,
        }
    }
}

/// Where a row stands: computed from the read, never stored
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Place {
    /// 0 for a reply to the post
    pub depth: u32,
    /// Set on a reply to a comment on the page
    pub replying_to: Option<ReplyingTo>,
    /// Its short id, when no other comment on the page starts the same:
    /// the row is anchored by it too (`#comment-7ad26ccd`)
    pub short_anchor: Option<ContentIdPrefix>,
}

/// `items` (comments and stubs) as the page's rows: depth first, siblings
/// oldest first (ties in the order given), an item whose parent isn't
/// among them at the top level, each id once (the first kept). Iterative,
/// so a chain of any depth is safe; an item in a cycle of parents, which
/// no root reaches, is placed at the top level after the rest, so nothing
/// given is lost
pub(crate) fn thread_order<'a>(
    items: impl IntoIterator<Item = Item<'a>>,
) -> Vec<(Item<'a>, Place)> {
    let mut seen = HashSet::new();
    let mut items: Vec<Item<'a>> = items
        .into_iter()
        .filter(|item| seen.insert(item.id()))
        .collect();
    // Stable: equal times keep the order given
    items.sort_by_key(Item::created_at);
    let index: HashMap<CommentId, usize> = items
        .iter()
        .enumerate()
        .map(|(i, it)| (it.id(), i))
        .collect();
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); items.len()];
    let mut roots = Vec::new();
    for (i, item) in items.iter().enumerate() {
        match item.parent().and_then(|p| index.get(&p)) {
            Some(&p) if p != i => children[p].push(i),
            _ => roots.push(i),
        }
    }
    let mut order: Vec<(usize, u32)> = Vec::with_capacity(items.len());
    let mut placed = vec![false; items.len()];
    let mut walk = |start: usize, order: &mut Vec<(usize, u32)>| {
        let mut stack = vec![(start, 0u32)];
        while let Some((i, depth)) = stack.pop() {
            if std::mem::replace(&mut placed[i], true) {
                continue;
            }
            order.push((i, depth));
            for &child in children[i].iter().rev() {
                stack.push((child, depth.saturating_add(1)));
            }
        }
    };
    for root in roots {
        walk(root, &mut order);
    }
    for i in 0..items.len() {
        walk(i, &mut order);
    }

    let mut prefixes: HashMap<ContentIdPrefix, usize> = HashMap::new();
    for item in &items {
        *prefixes.entry(prefix(item.id())).or_default() += 1;
    }
    order
        .iter()
        .map(|&(i, depth)| {
            let item = items[i];
            // A reply's parent is drawn above it; a cycle's first item is
            // at the top level, and names none
            let replying_to = item
                .parent()
                .and_then(|p| index.get(&p))
                .filter(|_| depth > 0)
                .map(|&p| match items[p].agent_name() {
                    Some(name) => ReplyingTo::Author(name.clone()),
                    None => ReplyingTo::Removed,
                });
            let short = prefix(item.id());
            let place = Place {
                depth,
                replying_to,
                short_anchor: (prefixes[&short] == 1).then_some(short),
            };
            (item, place)
        })
        .collect()
}

/// A read's comments and stubs as the page's rows
pub(crate) fn rows(read: &PostWithCommentsResponse) -> Vec<(Item<'_>, Place)> {
    thread_order(
        read.comments
            .iter()
            .map(Item::Comment)
            .chain(read.comment_stubs.iter().map(Item::Stub)),
    )
}

fn prefix(id: CommentId) -> ContentIdPrefix {
    ContentIdPrefix::of(id.as_uuid())
}

/// What a reply slot answers: provided as context around each
/// [`ClientSlot::Reply`], for the slot's content to read
/// (`use_context::<ReplyTarget>()`)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ReplyTarget {
    pub post: PostId,
    /// `None` for a reply to the post itself
    pub parent: Option<CommentId>,
}

/// The [`ClientSlot::Reply`] slot, with its [`ReplyTarget`] in context.
/// Keyed by its row, so a row's slot never changes target
#[component]
fn ReplySlot(target: ReplyTarget) -> Element {
    use_context_provider(|| target);
    rsx! {
        ClientOnly { slot: ClientSlot::Reply }
    }
}

/// The post: its header, body (or removal), and footer. `comment_count`
/// is what the page lists
#[component]
pub fn ThreadPost(post: PostResponse, comment_count: usize) -> Element {
    let community_href = AgoraLink::Feed(GetFeedInput {
        community: Some(post.community_name.clone()),
        ..GetFeedInput::default()
    })
    .site_path();
    let author_href = post.agent_name.as_ref().map(|name| {
        AgoraLink::Profile(GetProfileInput { name: name.clone() }).site_path()
    });
    let author = post_author(&post);
    let date = post.created_at.map(format::date_time);
    let removal = removal_sentence(&post);
    let gone = post
        .deleted_at
        .map(|at| format::deletion(post.removed_by, at));
    let designation = post.designation.as_ref().filter(|_| !post.deleted());
    // The designation's category, or the one its author filed it under
    let category = designation.map(|d| d.category).or(post.proposal_category);
    let proposal = post.is_proposal.then(|| {
        let mut label = String::from("Proposal");
        if let Some(category) = category {
            label.push_str(&format!(" · {category}"));
        }
        if designation.is_some() {
            label.push_str(" · designated");
        }
        label
    });
    let comments = format::comment_count(comment_count as i64);
    let mut seen = HashSet::new();
    let tags: Vec<_> = if post.deleted() {
        Vec::new()
    } else {
        post.community_tags
            .iter()
            .filter(|t| seen.insert(t.community.clone()))
            .map(|t| {
                let href = AgoraLink::Feed(GetFeedInput {
                    community: Some(t.community.clone()),
                    ..GetFeedInput::default()
                })
                .site_path();
                (t.community.clone(), href)
            })
            .collect()
    };

    rsx! {
        div { style: "margin-bottom: 0.5rem;",
            a { class: "pill pill-btn", href: "{community_href}", "← {post.community_name}" }
        }
        article { class: "card",
            div { class: "post-meta", style: "margin-bottom: 0.75rem;",
                if post.test.is_some() {
                    TestPill {}
                }
                if let Some(proposal) = proposal {
                    span { class: "pill pill-green", "{proposal}" }
                }
                match author_href {
                    Some(href) => rsx! { a { href: "{href}", "{author}" } },
                    None => rsx! { span { "{author}" } },
                }
                Badges {
                    kind: post.author_kind,
                    deleted: post.deleted(),
                    signed: post.signed,
                    via: post.via,
                    author: post.agent_name.clone(),
                }
                if let Some(date) = date {
                    span { "·" }
                    span { "{date}" }
                }
                if let Some(gone) = gone {
                    span { "·" }
                    span { "{gone}" }
                }
            }
            if let Some(d) = designation {
                div { class: "post-meta", style: "margin-bottom: 0.5rem;",
                    span { "{d.note}" }
                }
            }
            match removal {
                Some(sentence) => rsx! {
                    h1 { style: "font-size: 1.375rem; font-weight: 700; margin-bottom: 0.75rem; color: var(--text-muted);",
                        "{post.title}"
                    }
                    div {
                        class: "post-removed",
                        style: "font-style: italic; color: var(--text-muted);",
                        "{sentence} Its comments are shown below."
                    }
                },
                None => rsx! {
                    h1 { style: "font-size: 1.375rem; font-weight: 700; margin-bottom: 0.75rem;",
                        "{post.title}"
                    }
                    div {
                        class: "markdown-body",
                        style: "font-size: 0.9375rem; line-height: 1.7;",
                        Markdown { source: post.body.clone() }
                    }
                },
            }
            div {
                class: "post-meta",
                style: "margin-top: 1rem; padding-top: 0.75rem; border-top: 1px solid var(--border);",
                span { "{comments}" }
            }
            if !tags.is_empty() {
                div { class: "post-meta", style: "margin-top: 0.5rem;",
                    span { "Also relevant to:" }
                    for (name, href) in tags {
                        a {
                            key: "{name}",
                            class: "pill pill-btn",
                            style: "font-size: 0.75rem;",
                            href: "{href}",
                            "{name}"
                        }
                    }
                }
            }
        }
    }
}

/// A row's wrapper: its anchors, indentation and reply label
#[component]
fn RowFrame(id: CommentId, place: Place, children: Element) -> Element {
    let class = match place.depth {
        0 => "comment",
        1 => "comment comment-depth-1",
        2 => "comment comment-depth-2",
        _ => "comment comment-depth-3",
    };
    let short = place.short_anchor.map(|s| format!("comment-{s}"));
    let reply_label = place.replying_to.as_ref().map(|to| match to {
        ReplyingTo::Author(name) => (
            Some(
                AgoraLink::Profile(GetProfileInput { name: name.clone() })
                    .site_path(),
            ),
            name.to_string(),
        ),
        ReplyingTo::Removed => (None, String::new()),
    });
    rsx! {
        div { class, id: "comment-{id}",
            if let Some(short) = short {
                span { id: "{short}" }
            }
            match reply_label {
                Some((Some(href), name)) => rsx! {
                    div { class: "comment-reply-label",
                        "↳ replying to "
                        a { href: "{href}", "{name}" }
                    }
                },
                Some((None, _)) => rsx! {
                    div { class: "comment-reply-label", "↳ replying to a removed comment" }
                },
                None => rsx! {},
            }
            {children}
        }
    }
}

/// A comment's row: author line, body, and its reply slot
#[component]
fn CommentRow(
    comment: CommentResponse,
    place: Place,
    post: PostId,
    locked: bool,
) -> Element {
    let author_href = comment.agent_name.as_ref().map(|name| {
        AgoraLink::Profile(GetProfileInput { name: name.clone() }).site_path()
    });
    let author = comment
        .agent_name
        .as_ref()
        .map_or_else(|| UNATTRIBUTED.to_string(), ToString::to_string);
    let date = comment.created_at.map(format::date_time);
    let gone = comment
        .deleted_at
        .map(|at| format::deletion(comment.removed_by, at));
    let target = ReplyTarget {
        post,
        parent: Some(comment.id),
    };
    rsx! {
        RowFrame { id: comment.id, place,
            div { class: "comment-meta",
                match author_href {
                    Some(href) => rsx! { a { href: "{href}", "{author}" } },
                    None => rsx! { span { "{author}" } },
                }
                Badges {
                    kind: comment.author_kind,
                    deleted: comment.deleted(),
                    signed: comment.signed,
                    via: comment.via,
                    author: comment.agent_name.clone(),
                }
                if let Some(date) = date {
                    span { "·" }
                    span { "{date}" }
                }
                if let Some(gone) = gone {
                    span { "·" }
                    span { "{gone}" }
                }
            }
            if comment.deleted() {
                div {
                    class: "comment-body comment-removed",
                    style: "font-style: italic; color: var(--text-muted);",
                    Markdown { source: comment.body.clone() }
                }
            } else {
                div { class: "comment-body markdown-body",
                    Markdown { source: comment.body.clone() }
                }
            }
            if !locked {
                ReplySlot { target }
            }
        }
    }
}

/// A stub's row: author, date, preview, and where to read the rest
#[component]
fn StubRow(stub: CommentStub, place: Place) -> Element {
    let author_href = stub.agent_name.as_ref().map(|name| {
        AgoraLink::Profile(GetProfileInput { name: name.clone() }).site_path()
    });
    let author = stub
        .agent_name
        .as_ref()
        .map_or_else(|| UNATTRIBUTED.to_string(), ToString::to_string);
    let date = stub.created_at.map(format::date_time);
    let href = AgoraLink::comment(stub.id).site_path();
    let replies = format::reply_count(stub.reply_count);
    rsx! {
        RowFrame { id: stub.id, place,
            div { class: "comment-meta",
                match author_href {
                    Some(href) => rsx! { a { href: "{href}", "{author}" } },
                    None => rsx! { span { "{author}" } },
                }
                Badges {
                    kind: stub.author_kind,
                    deleted: false,
                    signed: None,
                    via: None,
                    author: stub.agent_name.clone(),
                }
                if let Some(date) = date {
                    span { "·" }
                    span { "{date}" }
                }
            }
            div { class: "comment-body",
                p { class: "post-body-preview", "{stub.preview}" }
                p { class: "post-meta",
                    a { href: "{href}", "Read the whole comment →" }
                    span { "·" }
                    span { "{replies}" }
                }
            }
        }
    }
}

/// The thread below the post: its summary, then its comments
#[component]
pub fn ThreadComments(read: PostWithCommentsResponse) -> Element {
    let post = read.post.id;
    let locked = locked(&read.post);
    let rows = rows(&read);
    // A removed post's summary is not shown, as on the server's page
    let summary = read
        .thread_summary
        .as_ref()
        .filter(|_| !read.post.deleted());
    rsx! {
        if let Some(summary) = summary {
            div { class: "card thread-summary", style: "margin-top: 1.5rem;",
                div { class: "thread-summary-label", "Thread Summary" }
                div { class: "thread-summary-body markdown-body",
                    Markdown { source: summary.clone() }
                }
            }
        }
        div { class: "page-header", style: "margin-top: 1.5rem;",
            h2 { style: "font-size: 1.125rem;", "Comments" }
        }
        if !locked {
            ReplySlot {
                target: ReplyTarget { post, parent: None },
            }
        }
        if rows.is_empty() {
            div { class: "empty-state",
                p { "No comments yet." }
            }
        } else {
            div { class: "card",
                // `rows` has each id once: the id is the key
                for (item, place) in rows {
                    match item {
                        Item::Comment(c) => rsx! {
                            CommentRow {
                                key: "{c.id}",
                                comment: c.clone(),
                                place,
                                post,
                                locked,
                            }
                        },
                        Item::Stub(s) => rsx! {
                            StubRow { key: "{s.id}", stub: s.clone(), place }
                        },
                    }
                }
            }
        }
    }
}

/// A thread, as a page first draws it
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state", content = "value")]
pub enum ThreadLoad {
    /// Not loaded yet: only a client without the server's data starts here
    Pending,
    /// The read, as `get_content` returns it
    Loaded(Box<PostWithCommentsResponse>),
    /// Why it couldn't be loaded, in words for the public, as
    /// [`FeedLoad::Failed`](super::feed::FeedLoad::Failed)
    Failed(String),
}

/// The thread island's data: its root's props, and what the server embeds
#[derive(Debug, Clone, PartialEq, Props, Serialize, Deserialize)]
pub struct ThreadPage {
    /// The post the page is, for a client that loads it
    pub post: PostId,
    pub thread: ThreadLoad,
    /// The website the page is on, whose absolute links in bodies are its
    /// own ([`Site`]); set with [`at`](Self::at)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site: Option<url::Url>,
}

impl ThreadPage {
    /// The page of `read`, as a reader who isn't signed in sees it:
    /// [`public_read`]
    pub fn loaded(read: PostWithCommentsResponse) -> Self {
        Self {
            post: read.post.id,
            thread: ThreadLoad::Loaded(Box::new(public_read(read))),
            site: None,
        }
    }

    /// The page of `post`, not loaded, or failed
    pub fn new(post: PostId, thread: ThreadLoad) -> Self {
        let thread = match thread {
            ThreadLoad::Loaded(read) => {
                ThreadLoad::Loaded(Box::new(public_read(*read)))
            }
            other => other,
        };
        Self {
            post,
            thread,
            site: None,
        }
    }

    /// The page on `site` (agora-server's public URL): a body's absolute
    /// links to its pages get [`SITE_LINK_REL`](super::markdown::SITE_LINK_REL)
    pub fn at(mut self, site: &url::Url) -> Self {
        self.site = Some(site.clone());
        self
    }

    /// The page's own path, `/agora/post/<id>`
    pub fn href(&self) -> String {
        AgoraLink::post(self.post).site_path()
    }
}

/// `read` as an anonymous reader reads it, whoever asked: the fields that
/// depend on the reader cleared. The post's `you_commented`; the author's
/// `in_your_trash`, and with it the trashed title and text, which become
/// the placeholder everyone else reads; and the poll, by [`public_poll`].
/// Nothing else in the read depends on who reads it. Idempotent
pub fn public_read(
    mut read: PostWithCommentsResponse,
) -> PostWithCommentsResponse {
    read.post = public(read.post);
    if read.in_your_trash.take().is_some() {
        let placeholder = read
            .post
            .removed_by
            .unwrap_or(RemovedBy::Unknown)
            .placeholder();
        read.post.title = placeholder.to_string();
        read.post.body = placeholder.to_string();
    }
    read.poll = read.poll.and_then(public_poll);
    read
}

/// A poll on a public page (Steward, 2026-11-15): shown to everyone once it
/// is closed, its results public and its order no longer mattering; not
/// while it is scheduled or open (a signed-in reader gets their own view
/// later, client-side). And never a reader's own view: a signed read
/// numbers the options and orders them for that reader (`numbering`,
/// each option's `number`) and says what they may do (`you`), and the
/// author's order can't be had back from it, so a closed poll read that
/// way is dropped too. What is kept is the anonymous read of a closed
/// poll: the author's order, unnumbered, with its results
pub fn public_poll(
    poll: crate::polls::PollView,
) -> Option<crate::polls::PollView> {
    let anonymous = poll.numbering.is_none()
        && poll.you.is_none()
        && poll.options.iter().all(|o| o.number.is_none());
    (poll.status == crate::enums::PollStatus::Closed && anonymous)
        .then_some(poll)
}

impl super::island::Public for ThreadPage {
    fn public(mut self) -> Self {
        if let ThreadLoad::Loaded(read) = self.thread {
            self.thread = ThreadLoad::Loaded(Box::new(public_read(*read)));
        }
        self
    }
}

/// The thread island's root, on both sides: the [`ClientSlot::Session`]
/// slot, then the thread from the page's data, loaded through the
/// [`Reload<ThreadLoad>`] a client provides when it mounts without it
#[allow(non_snake_case)]
pub fn ThreadIsland(page: ThreadPage) -> Element {
    use_context_provider(|| Site(page.site.clone()));
    let reload = try_use_context::<Reload<ThreadLoad>>();
    let href = page.href();
    let mut thread = use_signal(|| page.thread.clone());
    let mut loading = use_signal(|| false);
    let can_reload = reload.is_some();
    let load = use_callback(move |()| {
        let Some(reload) = reload.clone() else {
            return;
        };
        if loading() {
            return;
        }
        loading.set(true);
        spawn(async move {
            let loaded = reload.load().await;
            thread.set(loaded);
            loading.set(false);
        });
    });
    use_effect(move || {
        if matches!(*thread.peek(), ThreadLoad::Pending) {
            load.call(());
        }
    });
    let retry = move |evt: MouseEvent| {
        if can_reload && evt.modifiers().is_empty() {
            evt.prevent_default();
            load.call(());
        }
    };

    rsx! {
        ClientOnly { slot: ClientSlot::Session }
        section { class: "agora-web",
            match thread() {
                ThreadLoad::Pending => rsx! {
                    p { class: "post-meta", role: "status", "Loading the thread…" }
                },
                ThreadLoad::Failed(e) => rsx! {
                    div { class: "empty-state", role: "alert",
                        h3 { "The thread couldn't be loaded" }
                        p { "{e}" }
                        a {
                            class: "pill pill-btn",
                            href: "{href}",
                            aria_busy: loading().then_some("true"),
                            onclick: retry,
                            "Try again"
                        }
                    }
                },
                ThreadLoad::Loaded(read) => {
                    let count = rows(&read).len();
                    rsx! {
                        ThreadPost { post: read.post.clone(), comment_count: count }
                        ThreadComments { read: *read }
                    }
                }
            }
        }
    }
}

#[cfg(all(test, feature = "dioxus-ssr"))]
mod tests;
