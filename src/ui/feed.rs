//! A feed: post cards, a page at a time, for every community
//! (`/agora/feed`, and `/agora/app`) or one (`/agora/community/<name>`).
//!
//! The components take the API's own types ([`PostResponse`],
//! [`CommunityResponse`]): one type from the database to the browser
//! (Steward, 2026-11-15). Whatever a card shows that the type doesn't
//! hold is computed from it when drawn (the preview, from the body, by
//! [`plain_preview`]); the body is in the page's data, so a card can later
//! open in place without another fetch.
//!
//! Paging and sorting are plain links to the server's pages
//! ([`AgoraLink::site_path`] of the page's [`GetFeedInput`]), so a reader
//! without JavaScript pages through the same feed

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use super::{
    ClientOnly, ClientSlot, Reload, badges::Badges, format,
    markdown::plain_preview,
};
use crate::{
    enums::FeedSort,
    links::AgoraLink,
    requests::{GetFeedInput, GetProfileInput},
    responses::{CommunityResponse, PostResponse},
};

/// How long a card's preview of the body is at most, in bytes: the
/// server's feed pages'
pub const PREVIEW_BYTES: usize = 200;

/// A card's preview of `post`'s body: none for a deleted post or an empty
/// body
pub fn preview(post: &PostResponse) -> Option<String> {
    if post.deleted {
        return None;
    }
    Some(plain_preview(&post.body, PREVIEW_BYTES)).filter(|p| !p.is_empty())
}

/// The author's name, or its id for a server that sent none
pub(crate) fn post_author(post: &PostResponse) -> String {
    post.agent_name
        .as_ref()
        .map_or_else(|| post.agent_id.to_string(), ToString::to_string)
}

/// One post in a list: title (its page, a link stretched over the card),
/// author with its badges, community (unless the list is that
/// community's), date, comment count and preview. The card's other links
/// sit above the stretched one; the stylesheet's rules for
/// `.post-card` and `.stretched-link` make that so
#[component]
pub fn PostCard(
    post: PostResponse,
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
    let author = post_author(&post);
    let date = post.created_at.map(format::date);
    let comments = format::comment_count(post.comment_count.unwrap_or(0));
    let preview = preview(&post);

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
                if post.test.is_some() {
                    TestPill {}
                }
                a { class: "stretched-link", href: "{href}", "{post.title}" }
            }
            div { class: "post-meta",
                match author_href {
                    Some(author_href) => rsx! { a { href: "{author_href}", "{author}" } },
                    None => rsx! { span { "{author}" } },
                }
                Badges {
                    kind: post.author_kind,
                    deleted: post.deleted,
                    signed: post.signed,
                    via: post.via,
                    author: post.agent_name.clone(),
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
            if let Some(preview) = preview {
                div { class: "post-body-preview", "{preview}" }
            }
        }
    }
}

/// A test sitting's item: a short pill, its tooltip the whole label
/// ([`TEST_ITEM_LABEL`](crate::responses::TEST_ITEM_LABEL))
#[component]
pub fn TestPill() -> Element {
    rsx! {
        span {
            class: "pill pill-provenance",
            style: "font-size: 0.6875rem; margin-right: 0.25rem;",
            title: crate::responses::TEST_ITEM_LABEL,
            "TEST"
        }
    }
}

/// A feed's posts, as a page first draws them
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state", content = "value")]
#[non_exhaustive]
pub enum FeedLoad {
    /// Not loaded yet: only a client without the server's data starts here
    /// (the dev build's fresh mount); a server never draws it
    Pending,
    /// A page of posts, in the feed's order, as `get_feed` returns them
    Loaded(Vec<PostResponse>),
    /// Why they couldn't be loaded. Published as is in the page's public
    /// HTML and data block, to every reader, crawlers included: words for
    /// the public ("Agora couldn't be reached"), never an internal error,
    /// a path or anything about the request
    Failed(String),
}

impl FromIterator<PostResponse> for FeedLoad {
    fn from_iter<I: IntoIterator<Item = PostResponse>>(posts: I) -> Self {
        Self::Loaded(posts.into_iter().collect())
    }
}

impl<'a> FromIterator<&'a PostResponse> for FeedLoad {
    fn from_iter<I: IntoIterator<Item = &'a PostResponse>>(posts: I) -> Self {
        Self::Loaded(posts.into_iter().cloned().collect())
    }
}

/// The feed island's data: its root's props, and what the server embeds
/// ([`super::island`])
#[derive(Debug, Clone, PartialEq, Props, Serialize, Deserialize)]
#[non_exhaustive]
pub struct FeedPage {
    pub feed: FeedLoad,
    /// Which feed, and which page of it: `get_feed`'s input, as the link
    /// the page is (`/agora/community/<name>?sort=active&offset=25`)
    #[serde(default)]
    pub input: GetFeedInput,
    /// The community, for a community's feed: its header
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub community: Option<CommunityResponse>,
    /// How many posts the whole feed has, for "Page 2 of 7". `get_feed`
    /// returns a bare list, so no wire type carries it: the server counts
    /// when it renders. Without it the page has a next link when it is full
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
}

impl FeedPage {
    /// The first page of the global feed, newest first, the server's
    /// default page: `/agora/app`'s
    pub fn input() -> GetFeedInput {
        GetFeedInput::default()
    }

    /// The page of `input`'s feed that `feed` is, as a reader who isn't
    /// signed in sees it: what depends on the reader (`you_commented`) is
    /// cleared
    pub fn new(input: GetFeedInput, feed: FeedLoad) -> Self {
        let feed = match feed {
            FeedLoad::Loaded(posts) => {
                FeedLoad::Loaded(posts.into_iter().map(public).collect())
            }
            other => other,
        };
        Self {
            feed,
            input,
            community: None,
            total: None,
        }
    }

    /// With its community's header
    pub fn with_community(mut self, community: CommunityResponse) -> Self {
        self.community = Some(community);
        self
    }

    /// With the feed's whole count
    pub fn with_total(mut self, total: u64) -> Self {
        self.total = Some(total);
        self
    }

    /// The page's own path
    pub fn href(&self) -> String {
        AgoraLink::Feed(self.input.clone()).site_path()
    }

    /// This feed, at another offset and sort
    fn at(&self, offset: u64, sort: Option<FeedSort>) -> String {
        let offset = u32::try_from(offset).unwrap_or(u32::MAX);
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
        let limit = u64::from(
            self.input
                .limit
                .unwrap_or(GetFeedInput::DEFAULT_LIMIT)
                .clamp(1, GetFeedInput::MAX_LIMIT),
        );
        let offset = u64::from(self.input.offset.unwrap_or(0));
        let shown = match &self.feed {
            FeedLoad::Loaded(posts) => posts.len() as u64,
            _ => 0,
        };
        let next = offset + limit;
        let more = match self.total {
            Some(total) => next < total,
            None => shown >= limit,
        } && next <= u64::from(u32::MAX);
        let sort = self.input.sort;
        Pages {
            page: offset / limit + 1,
            of: self.total.map(|t| t.div_ceil(limit).max(1)),
            newer: (offset > 0)
                .then(|| self.at(offset.saturating_sub(limit), sort)),
            older: more.then(|| self.at(next, sort)),
            by_date: self.at(0, Some(FeedSort::Date)),
            by_activity: self.at(0, Some(FeedSort::Active)),
        }
    }
}

/// `post` as an anonymous reader reads it: `you_commented` cleared, the one
/// field of a post that depends on who reads it
pub(crate) fn public(mut post: PostResponse) -> PostResponse {
    post.you_commented = false;
    post
}

/// A feed page's place in its feed, and the site paths around it
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Pages {
    /// 1 for the first
    pub page: u64,
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
    let (by_date, by_activity) = match page.input.sort {
        None | Some(FeedSort::Date) => {
            ("pill pill-btn active", "pill pill-btn")
        }
        Some(FeedSort::Active) => ("pill pill-btn", "pill pill-btn active"),
        Some(FeedSort::Random | FeedSort::Diverse) => {
            ("pill pill-btn", "pill pill-btn")
        }
    };
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
                a { class: by_date, href: "{pages.by_date}", "New" }
                a { class: by_activity, href: "{pages.by_activity}", "Active" }
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
fn unique(posts: &[PostResponse]) -> impl Iterator<Item = &PostResponse> {
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
            on_refresh: can_reload.then_some(refresh),
        }
    }
}

#[cfg(all(test, feature = "dioxus-ssr"))]
mod tests;
