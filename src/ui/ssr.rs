//! The server's pre-render (feature `dioxus-ssr`): a root as hydratable
//! markup, and the data block it was drawn from, so agora-server never
//! touches dioxus-ssr itself.
//!
//! Hydration without dioxus-fullstack: `pre_render` writes the markers
//! dioxus-web's hydration reads (`data-node-hydration` on elements with
//! dynamic attributes or listeners, `<!--node-id…-->` around dynamic text,
//! `<!--placeholder…-->`). They are Dioxus internals, which is why the pin
//! is exact. Every text and attribute value is escaped by dioxus-ssr.

use dioxus::{dioxus_core::ComponentFunction, prelude::*};
use serde::Serialize;

use super::island;

/// `root` drawn from `props`, as markup for inside [`island::ROOT_ID`]'s
/// element, with the markers hydration reads
pub fn pre_render<P, M>(root: impl ComponentFunction<P, M>, props: P) -> String
where
    P: Clone + 'static,
    M: 'static,
{
    let mut dom = VirtualDom::new_with_props(root, props);
    dom.rebuild_in_place();
    dioxus_ssr::pre_render(&dom)
}

/// An island, pre-rendered
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendered {
    /// For inside the [`island::ROOT_ID`] element
    pub html: String,
    /// The [`island::script_element`], for outside it
    pub script: String,
}

/// [`pre_render`] and the data block from the same props, so the client's
/// first render has what the server's had
pub fn render_island<P, M>(
    root: impl ComponentFunction<P, M>,
    props: P,
) -> Result<Rendered, serde_json::Error>
where
    P: Clone + Serialize + 'static,
    M: 'static,
{
    let script = island::script_element(&props)?;
    Ok(Rendered {
        html: pre_render(root, props),
        script,
    })
}
