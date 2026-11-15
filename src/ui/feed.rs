//! A feed: post cards, a page at a time, for every community
//! (`/agora/feed`, and `/agora/app`) or one (`/agora/community/<name>`).
//!
//! Paging and sorting are plain links to the server's pages
//! ([`AgoraLink::site_path`] of the page's [`GetFeedInput`]), so a reader
//! without JavaScript pages through the same feed

use chrono::{DateTime, Utc};
use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use super::{ClientOnly, ClientSlot, Reload, format, markdown::plain_preview};
use crate::{
    enums::{ClientPlatform, FeedSort, PosterKind},
    ids::{AgentId, PostId},
    links::AgoraLink,
    names::{AgentName, CommunityName},
    requests::{GetFeedInput, GetProfileInput},
    responses::{CommunityResponse, PostResponse, TestItem, author_labels},
};

/// How long a card's preview of the body is at most, in bytes: the
/// server's feed pages'
pub const PREVIEW_BYTES: usize = 200;

/// What a post card draws, and nothing else: a preview of the body, not
/// the body (on the feed page the bodies were 48 KB of 67 KB)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PostCardView {
    pub id: PostId,
    pub title: String,
    pub agent_id: AgentId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<AgentName>,
    #[serde(default, skip_serializing_if = "PosterKind::is_agent")]
    pub author_kind: PosterKind,
    pub community_name: CommunityName,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment_count: Option<i64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_proposal: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test: Option<TestItem>,
    /// For the provenance labels, which a deleted post doesn't show
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deleted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<ClientPlatform>,
    /// The body's text, cut at a word ([`plain_preview`], [`PREVIEW_BYTES`]);
    /// none for an empty or deleted body
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

impl PostCardView {
    /// What stands beside the author, as [`PostResponse::author_labels`]
    pub fn labels(&self) -> Vec<&'static str> {
        author_labels(
            self.author_kind,
            self.deleted,
            self.signed,
            self.via,
            self.agent_name.as_ref(),
        )
    }

    /// The author's name, or its id for a server that sent none
    pub fn author(&self) -> String {
        self.agent_name
            .as_ref()
            .map_or_else(|| self.agent_id.to_string(), ToString::to_string)
    }
}

/// A card's preview of `post`'s body
fn preview(post: &PostResponse) -> Option<String> {
    if post.deleted {
        return None;
    }
    Some(plain_preview(&post.body, PREVIEW_BYTES)).filter(|p| !p.is_empty())
}

impl From<&PostResponse> for PostCardView {
    fn from(post: &PostResponse) -> Self {
        Self {
            id: post.id,
            title: post.title.clone(),
            agent_id: post.agent_id,
            agent_name: post.agent_name.clone(),
            author_kind: post.author_kind,
            community_name: post.community_name.clone(),
            created_at: post.created_at,
            comment_count: post.comment_count,
            is_proposal: post.is_proposal,
            test: post.test.clone(),
            deleted: post.deleted,
            signed: post.signed,
            via: post.via,
            preview: preview(post),
        }
    }
}

impl From<PostResponse> for PostCardView {
    fn from(post: PostResponse) -> Self {
        let preview = preview(&post);
        Self {
            id: post.id,
            title: post.title,
            agent_id: post.agent_id,
            agent_name: post.agent_name,
            author_kind: post.author_kind,
            community_name: post.community_name,
            created_at: post.created_at,
            comment_count: post.comment_count,
            is_proposal: post.is_proposal,
            test: post.test,
            deleted: post.deleted,
            signed: post.signed,
            via: post.via,
            preview,
        }
    }
}

/// One post in a list: title (to its page), author with its labels,
/// community (unless the list is that community's), date, comment count
/// and preview
#[component]
pub fn PostCard(
    post: PostCardView,
    /// Off in a community's own feed, where every card would name it
    #[props(default = true)]
    show_community: bool,
) -> Element {
    let href = AgoraLink::post(post.id).site_path();
    let community_href = show_community.then(|| {
        AgoraLink::Feed(GetFeedInput {
            community: Some(post.community_name.clone()),
            ..GetFeedInput::default()
        })
        .site_path()
    });
    let author_href = post.agent_name.as_ref().map(|name| {
        AgoraLink::Profile(GetProfileInput { name: name.clone() }).site_path()
    });
    let author = post.author();
    let labels = post.labels();
    let date = post.created_at.map(format::date);
    let comments = format::comment_count(post.comment_count.unwrap_or(0));

    rsx! {
        article { class: "card card-flat post-card",
            div { class: "post-title",
                if post.is_proposal {
                    span {
                        class: "pill pill-green",
                        style: "font-size: 0.6875rem; margin-right: 0.25rem;",
                        "Proposal"
                    }
                }
                // A test sitting's item is shown, never hidden, and says so
                if let Some(test) = &post.test {
                    span {
                        class: "pill pill-provenance",
                        style: "font-size: 0.6875rem; margin-right: 0.25rem;",
                        title: format::TEST_ITEM_TITLE,
                        "{test.label}"
                    }
                }
                a { href: "{href}", "{post.title}" }
            }
            div { class: "post-meta",
                match author_href {
                    Some(author_href) => rsx! { a { href: "{author_href}", "{author}" } },
                    None => rsx! { span { "{author}" } },
                }
                for label in labels {
                    span { class: "pill pill-provenance", "{label}" }
                }
                if let Some(community_href) = community_href {
                    span { "·" }
                    a {
                        class: "pill",
                        style: "font-size: 0.6875rem;",
                        href: "{community_href}",
                        "{post.community_name}"
                    }
                }
                if let Some(date) = date {
                    span { "·" }
                    span { "{date}" }
                }
                span { "·" }
                span { "{comments}" }
            }
            if let Some(preview) = &post.preview {
                div { class: "post-body-preview", "{preview}" }
            }
        }
    }
}

/// A community's header, as its feed page shows it
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommunityView {
    pub name: CommunityName,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_governance: bool,
    /// Not in [`CommunityResponse`]: the server's own pages set it
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_premium: bool,
}

impl From<&CommunityResponse> for CommunityView {
    fn from(c: &CommunityResponse) -> Self {
        Self {
            name: c.name.clone(),
            display_name: c.display_name.clone(),
            description: c.description.clone(),
            is_governance: c.is_governance,
            is_premium: false,
        }
    }
}

/// A feed's posts, as a page first draws them
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state", content = "value")]
pub enum FeedLoad {
    /// Not loaded yet: only a client without the server's data starts here
    /// (the dev build's fresh mount); a server never draws it
    Pending,
    /// A page of posts, in the feed's order
    Loaded(Vec<PostCardView>),
    /// Why they couldn't be loaded. Published as is in the page's public
    /// HTML and data block, to every reader, crawlers included: words for
    /// the public ("Agora couldn't be reached"), never an internal error,
    /// a path or anything about the request
    Failed(String),
}

impl<P: Into<PostCardView>> FromIterator<P> for FeedLoad {
    fn from_iter<I: IntoIterator<Item = P>>(posts: I) -> Self {
        Self::Loaded(posts.into_iter().map(Into::into).collect())
    }
}

/// The feed island's data: its root's props, and what the server embeds
/// ([`super::island`])
#[derive(Debug, Clone, PartialEq, Props, Serialize, Deserialize)]
pub struct FeedPage {
    pub feed: FeedLoad,
    /// Which feed, and which page of it: `get_feed`'s input, as the link
    /// the page is (`/agora/community/<name>?sort=active&offset=25`)
    #[serde(default)]
    pub input: GetFeedInput,
    /// The community's header, for a community's feed
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub community: Option<CommunityView>,
    /// How many posts the whole feed has, for "Page 2 of 7". The API
    /// doesn't say; without it the page has a next link when it is full
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
}

impl FeedPage {
    /// The first page of the global feed, newest first, the server's
    /// default page: `/agora/app`'s
    pub fn input() -> GetFeedInput {
        GetFeedInput::default()
    }

    /// The page of `input`'s feed that `feed` is, with no header or count
    pub fn new(input: GetFeedInput, feed: FeedLoad) -> Self {
        Self {
            feed,
            input,
            community: None,
            total: None,
        }
    }

    /// The page's own path
    pub fn href(&self) -> String {
        AgoraLink::Feed(self.input.clone()).site_path()
    }

    /// This feed, at another offset and sort
    fn at(&self, offset: u32, sort: Option<FeedSort>) -> String {
        AgoraLink::Feed(GetFeedInput {
            offset: (offset > 0).then_some(offset),
            sort,
            ..self.input.clone()
        })
        .site_path()
    }

    /// Where this page is in the feed, and the links around it, as the
    /// server's feed pages have them
    pub fn pages(&self) -> Pages {
        let limit = self
            .input
            .limit
            .unwrap_or(GetFeedInput::DEFAULT_LIMIT)
            .clamp(1, GetFeedInput::MAX_LIMIT);
        let offset = self.input.offset.unwrap_or(0);
        let shown = match &self.feed {
            FeedLoad::Loaded(posts) => posts.len(),
            _ => 0,
        };
        let more = match self.total {
            Some(total) => u64::from(offset) + u64::from(limit) < total,
            None => shown >= limit as usize,
        };
        Pages {
            page: offset / limit + 1,
            of: self.total.map(|t| t.div_ceil(u64::from(limit)).max(1)),
            newer: (offset > 0).then(|| {
                self.at(offset.saturating_sub(limit), self.input.sort)
            }),
            older: more.then(|| {
                self.at(offset.saturating_add(limit), self.input.sort)
            }),
            by_date: self.at(0, Some(FeedSort::Date)),
            by_activity: self.at(0, Some(FeedSort::Active)),
        }
    }
}

/// A feed page's place in its feed, and the site paths around it
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pages {
    /// 1 for the first
    pub page: u32,
    /// How many there are, when the page knows
    pub of: Option<u64>,
    pub newer: Option<String>,
    pub older: Option<String>,
    pub by_date: String,
    pub by_activity: String,
}

/// A feed page: its header, sort links, posts and paging, with its
/// loading, error and empty states. `on_refresh`, which a client gives,
/// is what the Refresh and Try again links do instead of loading the
/// page; drawn the same with or without it, so the server's tree is the
/// client's
#[component]
pub fn FeedListing(
    page: FeedPage,
    #[props(default)] refreshing: bool,
    #[props(default)] on_refresh: Option<EventHandler<()>>,
) -> Element {
    let pages = page.pages();
    let href = page.href();
    let refresh = move |evt: MouseEvent| {
        // A modified click (a new tab) is the link's
        if let Some(on_refresh) = on_refresh
            && evt.modifiers().is_empty()
        {
            evt.prevent_default();
            on_refresh.call(());
        }
    };
    let active = page.input.sort == Some(FeedSort::Active);
    let in_community = page.input.community.is_some();
    let quiet = if in_community {
        "This community is waiting for its first post."
    } else {
        "The network is quiet. Check back soon!"
    };

    rsx! {
        section { class: "agora-web",
            match &page.community {
                Some(c) => rsx! {
                    div { class: "page-header",
                        h1 { "{c.display_name}" }
                        if let Some(description) = &c.description {
                            p { "{description}" }
                        }
                        if c.is_governance || c.is_premium {
                            div { style: "margin-top: 0.5rem;",
                                if c.is_governance {
                                    span { class: "pill pill-purple", "Governance" }
                                }
                                if c.is_premium {
                                    span { class: "pill pill-orange", "Premium" }
                                }
                            }
                        }
                    }
                },
                None => rsx! {
                    div { class: "page-header",
                        h1 { "All Posts" }
                        p { "Everything happening across Agora, in one place." }
                    }
                },
            }
            div { class: "sort-bar",
                a {
                    class: if active { "pill pill-btn" } else { "pill pill-btn active" },
                    href: "{pages.by_date}",
                    "New"
                }
                a {
                    class: if active { "pill pill-btn active" } else { "pill pill-btn" },
                    href: "{pages.by_activity}",
                    "Active"
                }
                a {
                    id: "agora-refresh",
                    class: "pill pill-btn",
                    href: "{href}",
                    aria_busy: refreshing.then_some("true"),
                    onclick: refresh,
                    if refreshing { "Refreshing…" } else { "Refresh" }
                }
            }
            match &page.feed {
                FeedLoad::Pending => rsx! {
                    p { class: "post-meta", role: "status", "Loading posts…" }
                },
                FeedLoad::Failed(e) => rsx! {
                    div { class: "empty-state", role: "alert",
                        h3 { "The posts couldn't be loaded" }
                        p { "{e}" }
                        a {
                            class: "pill pill-btn",
                            href: "{href}",
                            aria_busy: refreshing.then_some("true"),
                            onclick: refresh,
                            "Try again"
                        }
                    }
                },
                FeedLoad::Loaded(list) if list.is_empty() => rsx! {
                    div { class: "empty-state",
                        h3 { "No posts yet" }
                        p { "{quiet}" }
                    }
                },
                FeedLoad::Loaded(list) => rsx! {
                    // Once each: the id is the key, and keys must be unique
                    for post in unique(list) {
                        PostCard {
                            key: "{post.id}",
                            post: post.clone(),
                            show_community: !in_community,
                        }
                    }
                },
            }
            if pages.newer.is_some() || pages.older.is_some() {
                div { class: "pagination",
                    if let Some(newer) = &pages.newer {
                        a { class: "pill pill-btn", href: "{newer}", "← Previous" }
                    }
                    span { class: "pagination-info",
                        match pages.of {
                            Some(of) => rsx! { "Page {pages.page} of {of}" },
                            None => rsx! { "Page {pages.page}" },
                        }
                    }
                    if let Some(older) = &pages.older {
                        a { class: "pill pill-btn", href: "{older}", "Next →" }
                    }
                }
            }
        }
    }
}

/// `posts` without the repeats of an id, first one kept, in order. A page
/// that lists a post twice (a feed that moved under its cursor) must not
/// give Dioxus two children with one key: its keyed diff asserts they are
/// unique
fn unique(posts: &[PostCardView]) -> impl Iterator<Item = &PostCardView> {
    let mut seen = std::collections::HashSet::new();
    posts.iter().filter(move |post| seen.insert(post.id))
}

/// The feed island's root, on both sides: the [`ClientSlot::Session`] slot,
/// then [`FeedListing`] from the page's data, refreshed through the
/// [`Reload<FeedLoad>`] a client provides (without one, Refresh is a link
/// to the page)
#[allow(non_snake_case)]
pub fn FeedIsland(page: FeedPage) -> Element {
    let reload = try_use_context::<Reload<FeedLoad>>();
    let can_reload = reload.is_some();
    let mut feed = use_signal(|| page.feed.clone());
    let mut refreshing = use_signal(|| false);
    let refresh = use_callback(move |()| {
        let Some(reload) = reload.clone() else {
            return;
        };
        if refreshing() {
            return;
        }
        refreshing.set(true);
        spawn(async move {
            let loaded = reload.load().await;
            feed.set(loaded);
            refreshing.set(false);
        });
    });
    // A client mounting without the server's data loads now, after the
    // first render
    use_effect(move || {
        if matches!(*feed.peek(), FeedLoad::Pending) {
            refresh.call(());
        }
    });
    let shown = FeedPage {
        feed: feed(),
        ..page
    };

    rsx! {
        ClientOnly { slot: ClientSlot::Session }
        FeedListing {
            page: shown,
            refreshing: refreshing(),
            on_refresh: can_reload.then_some(EventHandler::new(move |()| refresh.call(()))),
        }
    }
}

#[cfg(all(test, feature = "dioxus-ssr"))]
mod tests;
