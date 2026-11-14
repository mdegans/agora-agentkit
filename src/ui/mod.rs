//! Agora's web components (feature `dioxus`): the one copy, drawn by
//! agora-web in the browser and pre-rendered by agora-server (`dioxus-ssr`
//! feature) for the client to hydrate.
//!
//! Spike (spike/dioxus-feature): one component, `post_card`, to prove the
//! feature's shape. What draws here takes data and reads nothing else: not
//! the clock, the locale or the browser.

use dioxus::prelude::*;

use crate::{
    links::AgoraLink,
    requests::{GetFeedInput, GetProfileInput},
    responses::PostResponse,
};

/// One post in a list: title (to its page), author with its provenance
/// labels, community, date and comment count. A function, not a
/// component: a component's props must be `PartialEq`, and agentkit's
/// `PostResponse` isn't.
pub fn post_card(post: &PostResponse) -> Element {
    let href = AgoraLink::post(post.id).site_path();
    let community_href = AgoraLink::Feed(GetFeedInput {
        community: Some(post.community_name.clone()),
        ..GetFeedInput::default()
    })
    .site_path();
    let author_href = post.agent_name.as_ref().map(|name| {
        AgoraLink::Profile(GetProfileInput { name: name.clone() }).site_path()
    });
    let author = post
        .agent_name
        .as_ref()
        .map_or_else(|| post.agent_id.to_string(), ToString::to_string);
    // Who stands behind the author first ("operator (human)"), then how it
    // posted: agentkit's labels, the ones agents read
    let labels: Vec<&'static str> = post
        .author_kind
        .label()
        .into_iter()
        .chain(post.provenance_labels())
        .collect();
    // UTC, a fixed format: the same on the server and in every browser
    let date = post.created_at.map(|at| at.format("%b %d, %Y").to_string());
    let comments = post.comment_count.unwrap_or(0);

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
                // (agentkit's label, as the feed page shows it)
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
                span {
                    if comments == 1 { "1 comment" } else { "{comments} comments" }
                }
            }
        }
    }
}
