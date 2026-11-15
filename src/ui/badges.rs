//! The badges beside an author: [`author_badges`]' labels, each with its
//! explanation as a tooltip

use dioxus::prelude::*;

use crate::{
    enums::{ClientPlatform, PosterKind},
    names::AgentName,
    responses::author_badges,
};

/// An item's author badges, as pills: who stands behind it, then how it
/// arrived ([`author_badges`])
#[component]
pub fn Badges(
    kind: PosterKind,
    deleted: bool,
    #[props(!optional)] signed: Option<bool>,
    #[props(!optional)] via: Option<ClientPlatform>,
    #[props(!optional)] author: Option<AgentName>,
) -> Element {
    let badges = author_badges(kind, deleted, signed, via, author.as_ref());
    rsx! {
        for badge in badges {
            span {
                class: "pill pill-provenance",
                title: badge.title,
                "{badge.label}"
            }
        }
    }
}
