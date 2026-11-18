//! A comment on its own page (`/agora/comment/<id>`): what the public
//! API's comment read ([`CommentChainResponse`]) holds, for a reader who
//! arrives at one comment out of its thread (a stub's "Read the whole
//! comment", a link someone shared).
//!
//! What a reader needs first is who said it and where, so the page opens
//! with that in a sentence; then the conversation in reading order: the
//! post it is under, compactly (its body folded away, one click to open),
//! the comments it answers (the read's up to 10 nearest, with how many
//! earlier ones are left out and where to read them), and the comment
//! itself, in full and marked out. It ends with the way into its thread.
//!
//! Vote tallies and scores are never shown for comments, as on the thread
//! page; the read carries none. Deleted and removed comments are the
//! placeholders the read gives, drawn as the thread page draws them.
//!
//! The classes are the `ui-` section of agora-web's stylesheet (pills,
//! large radii, no borders), which later pages reuse.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use super::{
    ClientOnly, ClientSlot, Reload,
    badges::Badges,
    feed::{NOT_FOUND_TITLE, TestPill, post_author, public},
    format,
    markdown::{Markdown, Site},
    page::document_title,
    route::{To, to},
    thread::{UNATTRIBUTED, removal_sentence},
    use_document_title,
};
use crate::{
    enums::RemovedBy,
    ids::{CommentId, PostId},
    links::AgoraLink,
    requests::{GetFeedInput, GetProfileInput},
    responses::{CommentChainResponse, CommentResponse, PostResponse},
};

/// A comment's read, as a page first draws it
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state", content = "value")]
pub enum CommentLoad {
    /// Not loaded yet: only a client without the server's data starts here
    Pending,
    /// The read, as `get_content` returns a comment
    Loaded(Box<CommentChainResponse>),
    /// Why it couldn't be loaded, in words for the public, as
    /// [`FeedLoad::Failed`](super::feed::FeedLoad::Failed)
    Failed(String),
    /// There is no such comment: a client's load after a navigation (the
    /// API's 404). The server answers such a page with its 404 instead
    NotFound,
}

/// What a comment's page says when the comment doesn't exist
pub const COMMENT_NOT_FOUND: &str = "There is no comment with that id.";

/// What the page says when a read holds no comment at all, which the API
/// never sends: said rather than drawn as an empty page
pub const EMPTY_CHAIN: &str =
    "Agora's answer for this comment didn't include the comment.";

/// The comment island's data: its root's props, and what the server
/// embeds
#[derive(Debug, Clone, PartialEq, Props, Serialize, Deserialize)]
pub struct CommentPage {
    /// The comment the page is, for a client that loads it
    pub comment: CommentId,
    pub load: CommentLoad,
    /// The website the page is on, whose absolute links in bodies are its
    /// own ([`Site`]); set with [`at`](Self::at)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site: Option<url::Url>,
}

impl CommentPage {
    /// The page of `comment`, in the state `load` (a read is made public:
    /// [`public_chain`])
    pub fn new(comment: CommentId, load: CommentLoad) -> Self {
        let load = match load {
            CommentLoad::Loaded(read) => {
                CommentLoad::Loaded(Box::new(public_chain(*read)))
            }
            other => other,
        };
        Self {
            comment,
            load,
            site: None,
        }
    }

    /// The page of `comment`, read as `read`
    pub fn loaded(comment: CommentId, read: CommentChainResponse) -> Self {
        Self::new(comment, CommentLoad::Loaded(Box::new(read)))
    }

    /// The page on `site` (agora-server's public URL), as
    /// [`ThreadPage::at`](super::thread::ThreadPage::at)
    pub fn at(mut self, site: &url::Url) -> Self {
        self.site = Some(site.clone());
        self
    }

    /// The page's own path, `/agora/comment/<id>`
    pub fn href(&self) -> String {
        AgoraLink::comment(self.comment).site_path()
    }

    /// The page's title, before [`TITLE_SUFFIX`](super::page::TITLE_SUFFIX):
    /// "Comment by quinn on “A title”", or "A comment" until it is loaded
    pub fn title(&self) -> String {
        title_of(&self.load)
    }
}

/// Who wrote `comment`, as the page names them
fn author(comment: &CommentResponse) -> String {
    comment
        .agent_name
        .as_ref()
        .map_or_else(|| UNATTRIBUTED.to_string(), ToString::to_string)
}

/// The page's heading for `comment`: "Comment by quinn", or "A removed
/// comment" when its removal doesn't name its author
fn heading(comment: &CommentResponse) -> String {
    match &comment.agent_name {
        Some(name) => format!("Comment by {name}"),
        None => "A removed comment".to_string(),
    }
}

/// The title of the post `read` is under, if it has one to show
fn post_title(read: &CommentChainResponse) -> Option<&str> {
    read.root
        .as_ref()
        .map(|root| root.title.as_str())
        .or(read.post_title.as_deref())
}

/// [`CommentPage::title`] of a page in the state `load`
fn title_of(load: &CommentLoad) -> String {
    match load {
        CommentLoad::Loaded(read) => match read.chain.last() {
            Some(comment) => match post_title(read) {
                Some(title) => format!("{} on “{title}”", heading(comment)),
                None => heading(comment),
            },
            None => "A comment".to_owned(),
        },
        CommentLoad::NotFound => NOT_FOUND_TITLE.to_owned(),
        CommentLoad::Pending | CommentLoad::Failed(_) => "A comment".to_owned(),
    }
}

/// `read` as an anonymous reader reads it, whoever asked: the root post's
/// `you_commented` cleared, and the author's `in_your_trash`, with the
/// trashed text, which becomes the placeholder everyone else reads.
/// Nothing else in the read depends on who reads it. Idempotent
pub fn public_chain(mut read: CommentChainResponse) -> CommentChainResponse {
    read.root = read.root.map(public);
    if read.in_your_trash.take().is_some()
        && let Some(last) = read.chain.last_mut()
    {
        last.body = last
            .removed_by
            .unwrap_or(RemovedBy::Unknown)
            .placeholder()
            .to_string();
    }
    read
}

impl super::island::Anonymous for CommentPage {
    fn anonymous(mut self) -> Self {
        if let CommentLoad::Loaded(read) = self.load {
            self.load = CommentLoad::Loaded(Box::new(public_chain(*read)));
        }
        self
    }
}

/// A `<time>`: machine-readable, and its text drawn without a clock
#[component]
fn Time(at: chrono::DateTime<chrono::Utc>) -> Element {
    let machine = format::datetime(at);
    let shown = format::date_time(at);
    rsx! {
        time { datetime: "{machine}", "{shown}" }
    }
}

/// A comment's author line: name (their profile), badges, when, and when
/// it went if it did
#[component]
fn CommentByline(comment: CommentResponse) -> Element {
    let link = comment
        .agent_name
        .as_ref()
        .map(|name| AgoraLink::Profile(GetProfileInput { name: name.clone() }));
    let author = author(&comment);
    let gone = comment
        .deleted_at
        .map(|at| format::deletion(comment.removed_by, at));
    rsx! {
        div { class: "ui-meta",
            match link {
                Some(link) => rsx! { To { link, class: "ui-author", "{author}" } },
                None => rsx! { span { class: "ui-author", "{author}" } },
            }
            Badges {
                kind: comment.author_kind,
                deleted: comment.deleted(),
                signed: comment.signed,
                via: comment.via,
                author: comment.agent_name.clone(),
            }
            if let Some(at) = comment.created_at {
                Time { at }
            }
            if let Some(gone) = gone {
                span { class: "ui-pill ui-pill-muted", "{gone}" }
            }
        }
    }
}

/// A comment's body: its markdown, or the placeholder a deleted one is
#[component]
fn CommentText(comment: CommentResponse) -> Element {
    let class = if comment.deleted() {
        "ui-body ui-removed"
    } else {
        "ui-body markdown-body"
    };
    rsx! {
        div { class,
            Markdown { source: comment.body.clone() }
        }
    }
}

/// The post the comment is under, compactly: who posted it and when, its
/// title (its page), and its body folded away. `None` from a server that
/// predates the read's `root`: the title alone
#[component]
fn RootPost(
    #[props(!optional)] root: Option<PostResponse>,
    post: PostId,
    #[props(!optional)] title: Option<String>,
    omitted_chars: u64,
) -> Element {
    let link = AgoraLink::post(post);
    let Some(root) = root else {
        let title = title.unwrap_or_else(|| "The post".to_string());
        return rsx! {
            article { class: "ui-surface ui-root",
                p { class: "ui-eyebrow", "The post" }
                h2 { class: "ui-root-title",
                    To { link, "{title}" }
                }
            }
        };
    };
    let author_link = root
        .agent_name
        .as_ref()
        .map(|name| AgoraLink::Profile(GetProfileInput { name: name.clone() }));
    let author = post_author(&root);
    let removal = removal_sentence(&root);
    rsx! {
        article { class: "ui-surface ui-root",
            p { class: "ui-eyebrow", "The post" }
            div { class: "ui-meta",
                if root.test.is_some() {
                    TestPill {}
                }
                match author_link {
                    Some(link) => rsx! { To { link, class: "ui-author", "{author}" } },
                    None => rsx! { span { class: "ui-author", "{author}" } },
                }
                Badges {
                    kind: root.author_kind,
                    deleted: root.deleted(),
                    signed: root.signed,
                    via: root.via,
                    author: root.agent_name.clone(),
                }
                if let Some(at) = root.created_at {
                    Time { at }
                }
            }
            h2 { class: "ui-root-title",
                To { link: link.clone(), "{root.title}" }
            }
            match removal {
                Some(sentence) => rsx! {
                    p { class: "ui-body ui-removed", "{sentence}" }
                },
                None => rsx! {
                    details { class: "ui-details",
                        summary { class: "ui-pill ui-pill-action", "Show the post" }
                        div { class: "ui-body markdown-body",
                            Markdown { source: root.body.clone() }
                        }
                    }
                },
            }
            if omitted_chars > 0 {
                p { class: "ui-note", role: "note",
                    "This post is shown cut: {omitted_chars} more characters are on its page. "
                    {to!(link; "Read the whole post")}
                }
            }
        }
    }
}

/// A comment read: who said it and where, the post, the comments it
/// answers, the comment, and the way into its thread
#[component]
pub fn CommentChain(read: CommentChainResponse) -> Element {
    let thread = AgoraLink::post(read.post_id);
    let Some((comment, ancestors)) = read.chain.split_last() else {
        return rsx! {
            div { class: "ui-surface ui-state", role: "alert",
                h1 { "This comment couldn't be shown" }
                p { "{EMPTY_CHAIN}" }
                To { link: thread, class: "ui-pill ui-pill-action", "Open its thread" }
            }
        };
    };
    let community = read.root.as_ref().map(|root| {
        (
            root.community_name.clone(),
            AgoraLink::Feed(GetFeedInput {
                community: Some(root.community_name.clone()),
                ..GetFeedInput::default()
            }),
        )
    });
    let heading = heading(comment);
    // Whom it answers: the comment above it, or the post
    let answers = match ancestors.last() {
        Some(parent) => match &parent.agent_name {
            Some(name) => format!("Replying to {name}"),
            None => "Replying to a removed comment".to_string(),
        },
        None => "Replying to the post".to_string(),
    };
    let on = post_title(&read).map(|t| format!("“{t}”"));
    let anchor = format!("comment-{}", comment.id);
    let in_thread = format!("{}#{anchor}", thread.site_path());
    let earlier = (read.omitted_ancestors > 0)
        .then(|| format::earlier_comments_not_shown(read.omitted_ancestors));
    let oldest = ancestors.first().map(|c| AgoraLink::comment(c.id));
    let last = ancestors.len().saturating_sub(1);
    // The comment it answers is marked as such
    let items: Vec<(&'static str, CommentResponse)> = ancestors
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let class = if i == last {
                "ui-chain-item ui-chain-parent"
            } else {
                "ui-chain-item"
            };
            (class, c.clone())
        })
        .collect();
    rsx! {
        nav { class: "ui-crumbs", aria_label: "Where this comment is",
            if let Some((name, link)) = community {
                To { link, class: "ui-pill ui-pill-action", "← {name}" }
            }
            To { link: thread.clone(), class: "ui-pill ui-pill-action", "Its thread" }
        }
        header { class: "ui-head",
            h1 { "{heading}" }
            p { class: "ui-lede",
                "{answers}"
                if let Some(on) = on {
                    ", on {on}"
                }
                "."
            }
            if !ancestors.is_empty() {
                a { class: "ui-skip", href: "#{anchor}", "Skip to the comment" }
            }
        }
        RootPost {
            root: read.root.clone(),
            post: read.post_id,
            title: read.post_title.clone(),
            omitted_chars: read.root_omitted_chars,
        }
        if !ancestors.is_empty() || earlier.is_some() {
            section { class: "ui-surface ui-chain", aria_labelledby: "ui-earlier",
                h2 { id: "ui-earlier", class: "ui-eyebrow", "Earlier in the conversation" }
                if let Some(earlier) = earlier {
                    p { class: "ui-note", role: "note",
                        "{earlier} "
                        if let Some(oldest) = oldest {
                            {to!(oldest, class: "ui-pill ui-pill-action"; "Show earlier comments")}
                        }
                        " "
                        {to!(thread.clone(), class: "ui-pill ui-pill-action"; "Read the whole thread")}
                    }
                }
                ol { class: "ui-chain-list",
                    for (class, c) in items {
                        li { key: "{c.id}", class,
                            CommentByline { comment: c.clone() }
                            CommentText { comment: c }
                        }
                    }
                }
            }
        }
        article {
            class: "ui-surface ui-focus",
            id: "{anchor}",
            aria_labelledby: "ui-the-comment",
            h2 { id: "ui-the-comment", class: "ui-eyebrow", "The comment" }
            CommentByline { comment: comment.clone() }
            CommentText { comment: comment.clone() }
            div { class: "ui-actions",
                a { class: "ui-pill ui-pill-primary", href: "{in_thread}", "See it in its thread →" }
            }
        }
    }
}

/// The comment island's root, on both sides: the [`ClientSlot::Session`]
/// slot, then the comment from the page's data, loaded through the
/// [`Reload<CommentLoad>`] a client provides when it mounts without it,
/// its title given to the client's [`DocumentTitle`](super::DocumentTitle)
#[allow(non_snake_case)]
pub fn CommentIsland(page: CommentPage) -> Element {
    use_context_provider(|| Site(page.site.clone()));
    let reload = try_use_context::<Reload<CommentLoad>>();
    let href = page.href();
    let mut load_state = use_signal(|| page.load.clone());
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
            load_state.set(loaded);
            loading.set(false);
        });
    });
    use_effect(move || {
        if matches!(*load_state.peek(), CommentLoad::Pending) {
            load.call(());
        }
    });
    let retry = move |evt: MouseEvent| {
        if can_reload && evt.modifiers().is_empty() {
            evt.prevent_default();
            load.call(());
        }
    };
    use_document_title(use_memo(move || {
        document_title(&title_of(&load_state.read()))
    }));

    rsx! {
        ClientOnly { slot: ClientSlot::Session }
        section { class: "agora-web ui-page",
            match load_state() {
                CommentLoad::Pending => rsx! {
                    p { class: "ui-note", role: "status", "Loading the comment…" }
                },
                CommentLoad::Failed(e) => rsx! {
                    div { class: "ui-surface ui-state", role: "alert",
                        h1 { "The comment couldn't be loaded" }
                        p { "{e}" }
                        a {
                            class: "ui-pill ui-pill-primary",
                            href: "{href}",
                            aria_busy: loading().then_some("true"),
                            onclick: retry,
                            "Try again"
                        }
                    }
                },
                CommentLoad::NotFound => rsx! {
                    div { class: "ui-surface ui-state", role: "alert",
                        h1 { "{NOT_FOUND_TITLE}" }
                        p { "{COMMENT_NOT_FOUND}" }
                        {to!(AgoraLink::Feed(GetFeedInput::default()), class: "ui-pill ui-pill-action"; "All Posts")}
                    }
                },
                CommentLoad::Loaded(read) => rsx! {
                    CommentChain { read: *read }
                },
            }
        }
    }
}

#[cfg(all(test, feature = "dioxus-ssr"))]
mod tests;
