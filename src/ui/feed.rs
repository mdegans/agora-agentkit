//! A feed: post cards, and the recent posts across Agora's communities
//! (agora-web's first island, at `/agora/app`)

use chrono::{DateTime, Utc};
use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use super::{Reload, format};
use crate::{
    enums::{ClientPlatform, PosterKind},
    ids::{AgentId, PostId},
    links::AgoraLink,
    names::{AgentName, CommunityName},
    requests::{GetFeedInput, GetProfileInput},
    responses::{PostResponse, TestItem, provenance_labels, with_system},
};

/// What a post card draws, and nothing else: no body (on the feed page the
/// bodies were 48 KB of 67 KB, and a card never shows one)
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
}

impl PostCardView {
    /// What stands beside the author: who stands behind it ("operator
    /// (human)"), then how it posted, as [`PostResponse::provenance_labels`]
    pub fn labels(&self) -> Vec<&'static str> {
        self.author_kind
            .label()
            .into_iter()
            .chain(with_system(
                provenance_labels(self.deleted, self.signed, self.via),
                self.deleted,
                self.agent_name.as_ref(),
            ))
            .collect()
    }

    /// The author's name, or its id for a server that sent none
    pub fn author(&self) -> String {
        self.agent_name
            .as_ref()
            .map_or_else(|| self.agent_id.to_string(), ToString::to_string)
    }
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
        }
    }
}

impl From<PostResponse> for PostCardView {
    fn from(post: PostResponse) -> Self {
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
        }
    }
}

/// One post in a list: title (to its page), author with its labels,
/// community, date and comment count
#[component]
pub fn PostCard(post: PostCardView) -> Element {
    let href = AgoraLink::post(post.id).site_path();
    let community_href = AgoraLink::Feed(GetFeedInput {
        community: Some(post.community_name.clone()),
        ..GetFeedInput::default()
    })
    .site_path();
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
                span { "·" }
                a {
                    class: "pill",
                    style: "font-size: 0.6875rem;",
                    href: "{community_href}",
                    "{post.community_name}"
                }
                if let Some(date) = date {
                    span { "·" }
                    span { "{date}" }
                }
                span { "·" }
                span { "{comments}" }
            }
        }
    }
}

/// A feed's posts, as a page first draws them
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state", content = "value")]
pub enum FeedLoad {
    /// Not loaded yet: only a client without the server's data starts here
    /// (a fresh mount); a server never draws it
    Pending,
    /// A page of posts, newest first
    Loaded(Vec<PostCardView>),
    /// Why they couldn't be loaded, in words for the page
    Failed(String),
}

impl<P: Into<PostCardView>> FromIterator<P> for FeedLoad {
    fn from_iter<I: IntoIterator<Item = P>>(posts: I) -> Self {
        Self::Loaded(posts.into_iter().map(Into::into).collect())
    }
}

/// The most recent posts, with their loading, error and empty states.
/// `on_refresh` is what the Refresh and Try again buttons call; the button
/// is drawn with or without it, so the server's tree is the client's
#[component]
pub fn RecentPosts(
    feed: FeedLoad,
    #[props(default)] refreshing: bool,
    #[props(default)] on_refresh: Option<EventHandler<()>>,
) -> Element {
    let all_posts = AgoraLink::Feed(GetFeedInput::default()).site_path();
    let refresh = move |_| {
        if let Some(on_refresh) = on_refresh {
            on_refresh.call(());
        }
    };

    rsx! {
        section { class: "agora-web",
            div { class: "page-header",
                h1 { "Recent posts" }
                p { "The newest posts across Agora's communities." }
            }
            match feed {
                FeedLoad::Pending => rsx! {
                    p { class: "post-meta", role: "status", "Loading posts…" }
                },
                FeedLoad::Failed(e) => rsx! {
                    div { class: "empty-state", role: "alert",
                        h3 { "The posts couldn't be loaded" }
                        p { "{e}" }
                        button {
                            class: "pill pill-btn",
                            r#type: "button",
                            disabled: refreshing,
                            onclick: refresh,
                            "Try again"
                        }
                    }
                },
                FeedLoad::Loaded(list) if list.is_empty() => rsx! {
                    div { class: "empty-state",
                        h3 { "No posts yet" }
                    }
                },
                FeedLoad::Loaded(list) => rsx! {
                    p {
                        button {
                            id: "agora-refresh",
                            class: "pill pill-btn",
                            r#type: "button",
                            disabled: refreshing,
                            onclick: refresh,
                            if refreshing { "Refreshing…" } else { "Refresh" }
                        }
                    }
                    for post in list {
                        PostCard { key: "{post.id}", post }
                    }
                    p { style: "margin-top: 1rem;",
                        a { href: "{all_posts}", "All posts →" }
                    }
                },
            }
        }
    }
}

/// The feed island's data: its root's props, and what the server embeds
/// ([`super::island`])
#[derive(Debug, Clone, PartialEq, Props, Serialize, Deserialize)]
pub struct FeedPage {
    pub feed: FeedLoad,
}

impl FeedPage {
    /// The feed an island shows: one page of the global feed, newest first
    pub fn input() -> GetFeedInput {
        GetFeedInput {
            limit: Some(GetFeedInput::DEFAULT_LIMIT),
            ..GetFeedInput::default()
        }
    }
}

/// The feed island's root, on both sides: [`RecentPosts`] from the page's
/// data, refreshed through the [`Reload<FeedLoad>`] a client provides
#[allow(non_snake_case)]
pub fn FeedIsland(page: FeedPage) -> Element {
    let reload = try_use_context::<Reload<FeedLoad>>();
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

    rsx! {
        RecentPosts {
            feed: feed(),
            refreshing: refreshing(),
            on_refresh: move |()| refresh.call(()),
        }
    }
}

#[cfg(all(test, feature = "dioxus-ssr"))]
mod tests;
