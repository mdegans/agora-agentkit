//! Agora's web components (feature `dioxus`): the one copy, drawn by
//! agora-web in the browser and pre-rendered by agora-server ([`ssr`],
//! feature `dioxus-ssr`) for the browser to hydrate.
//!
//! # Layout
//!
//! One submodule per page, each holding its view types, its components and
//! its root: [`feed`] (the global feed, a community's, `/agora/app`) and
//! [`thread`] (a post with its comments). [`page`] is the one entry point
//! over them. What several pages draw the same way has a module of its
//! own: [`markdown`] for bodies, [`format`](mod@format) for dates and
//! counts. The data hand-off is [`island`], the pre-render [`ssr`],
//! client-only content [`ClientOnly`].
//!
//! A page has:
//!
//! - **view types** holding only what is drawn, built `From` the API's
//!   response types (a card holds a preview, not the body), serde both
//!   ways: they are what the page embeds;
//! - a **load** enum, `Pending` / `Loaded` / `Failed`, for a client that
//!   mounts without the server's data (the dev build) and for a reload;
//! - a **page** type, the root's props (`Props`, serde), with what a
//!   client needs to load it again (the feed's input, the post's id);
//! - a **root**, `fn XIsland(page: XPage) -> Element`: the
//!   [`ClientSlot::Session`] slot first, then the page, refreshed through
//!   the [`Reload`] a client provides;
//! - components below it that take props and fetch nothing.
//!
//! ## Adding a page
//!
//! 1. A module with the four above, following [`thread`].
//! 2. A [`page::Page`] variant, and its arm in [`page::PageIsland`].
//! 3. Tests: a full-markup snapshot of each state, hostile text in every
//!    field a person chooses, the island round trip, and the first render
//!    with the client's slots and `Reload` equal to the server's.
//! 4. agora-server renders it at its URL with [`page::Page::render`];
//!    agora-web needs only the `Reload` for its load.
//!
//! ## Adding a component
//!
//! Props are view types (`Clone + PartialEq`). Links are plain `<a href>`s
//! to the server's pages, built with [`AgoraLink::site_path`](crate::links::AgoraLink::site_path),
//! so a reader without JavaScript (and a crawler) follows them; there is
//! no client-side router. Something a client does instead of following a
//! link (Refresh, Try again) is an `onclick` on that same link, which
//! `prevent_default`s only when the client gave a handler. A list of
//! components is keyed by id, with repeats dropped (Dioxus asserts keys are
//! unique). Client-only content goes in a [`ClientOnly`] slot, whose
//! content reads what it belongs to from context the shared component
//! provides around it ([`thread::ReplyTarget`]).
//!
//! ## Bodies
//!
//! Post and comment bodies are markdown, drawn by [`markdown::Markdown`]
//! as elements: no HTML is written anywhere, so nothing is sanitised
//! afterwards, and the server's sanitiser's policy (raw HTML as text, an
//! allow-list of link schemes, images as links) is applied while the tree
//! is built. Its module docs have the policy; its parity corpus holds it to
//! what agora-server draws today.
//!
//! # Rules for a shared component
//!
//! The server's markup and the client's first render must be the same tree.
//! Hydration adopts the server's nodes by position and never compares their
//! contents: a different text is silently kept until something re-renders
//! it, and a different structure (a node more or less) misplaces every node
//! after it. Nothing reports either; agora's browser test is the check.
//!
//! - The same root component and the same data on both sides. A root's
//!   props are the page's data: the server embeds them ([`island`]) and the
//!   client mounts the root with what it reads back. No refetch.
//! - Nothing drawn reads the clock, the locale, the time zone, `window` or
//!   randomness. Dates go through [`format`](mod@format) (UTC, a fixed English format).
//!   A relative time ("3h ago") is computed after mount, or from a "now"
//!   carried in the data.
//! - No `use_resource` or suspense in the first render: on the client a
//!   resource is pending at first, where the server drew nothing pending.
//!   Both are fine after it (a refresh, the next page).
//! - Client-only state (who is signed in, a restored draft) is an empty
//!   slot on the server and in the first client render, filled by an
//!   effect: a [`ClientOnly`] at a fixed place in the shared component,
//!   naming its [`ClientSlot`], and the client's [`ClientSlots`] in root
//!   context to fill it.
//! - Components fetch nothing. A client that refetches provides a
//!   [`Reload`] as context; the server provides none.
//! - `cfg(target_arch)` and features never change what is drawn, and
//!   neither does iteration order of a hash map.
//! - Every text is drawn as text (`"{value}"` in `rsx!`), never as
//!   `dangerous_inner_html`: titles, names, bodies and error messages are
//!   hostile.
//! - Nothing drawn is something the browser's HTML parser would rebuild
//!   differently: no `<a>` inside an `<a>`, no block inside a `<p>`, a
//!   table's rows inside a `<tbody>`. Hydration would misplace every node
//!   after it.

use std::{fmt, rc::Rc};

use futures::future::LocalBoxFuture;

pub use client_only::{ClientOnly, ClientOnlyProps, ClientSlot, ClientSlots};

mod client_only;
pub mod feed;
pub mod format;
pub mod island;
pub mod markdown;
pub mod page;
#[cfg(feature = "dioxus-ssr")]
pub mod ssr;
pub mod thread;

/// How a client gets a page's data again (a Refresh button), provided as
/// Dioxus context. The server provides none, so a press before hydration
/// does nothing
pub struct Reload<T>(Rc<dyn Fn() -> LocalBoxFuture<'static, T>>);

impl<T> Reload<T> {
    pub fn new(
        load: impl Fn() -> LocalBoxFuture<'static, T> + 'static,
    ) -> Self {
        Self(Rc::new(load))
    }

    /// Load the data again
    pub fn load(&self) -> LocalBoxFuture<'static, T> {
        (self.0)()
    }
}

impl<T> Clone for Reload<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T> fmt::Debug for Reload<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Reload")
    }
}
