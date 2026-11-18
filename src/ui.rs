//! Agora's web components (feature `dioxus`): the one copy, drawn by
//! agora-web in the browser and pre-rendered by agora-server ([`ssr`],
//! feature `dioxus-ssr`) for the browser to hydrate.
//!
//! # Layout
//!
//! One submodule per page, each holding its components and its root:
//! [`feed`] (the global feed, a community's, `/agora/app`), [`thread`]
//! (a post with its comments) and [`comment`] (one comment under its post
//! and the comments it answers). [`page`] is the one entry point over them.
//! What several pages draw the same way has a module of its own:
//! [`markdown`] for bodies, [`badges`] for the author's badges,
//! [`format`](mod@format) for dates and counts. The data hand-off is
//! [`island`], the pre-render [`ssr`], client-only content [`ClientOnly`].
//! [`route`] puts the pages behind the Dioxus router: its [`SiteRoot`] is
//! the root of every page the client draws, on both sides.
//!
//! [`SiteRoot`]: route::SiteRoot
//!
//! **Components take the API's own types** ([`PostResponse`],
//! [`PostWithCommentsResponse`], [`CommunityResponse`], …): one type from
//! the database to the browser, with no parallel view type to drift from
//! it (Steward, 2026-11-15). What a page draws that a type doesn't hold
//! (a preview, a thread's order and depths, badges, counts) is computed
//! from it when drawn, by functions here; a module may keep a private
//! helper that borrows from the type (`thread::Item`), never a public
//! struct copying its fields. A field a page needs that the type lacks
//! goes on the wire type.
//!
//! A page has:
//!
//! - a **load** enum, `Pending` / `Loaded` / `Failed`, for a client that
//!   mounts without the server's data (the dev build) and for a reload;
//! - a **page** type, the root's props (`Props`, serde, with
//!   constructors), holding the API's responses
//!   and what a client needs to load it again (the feed's input, the
//!   post's id), and [`island::Anonymous`]: **a page's data is the
//!   anonymous reader's view, enforced at render**. The render paths
//!   ([`ssr::render_island`], [`island::to_script_text`], `render` on a
//!   public [`page::Page`], [`route::ssr::render`]) take only an
//!   [`island::Public`], which `.public()` makes once (`you_commented`,
//!   `in_your_trash` and the trashed text cleared; a poll kept only when
//!   it is closed and read anonymously, [`thread::public_poll`]), however
//!   the page was built, and no component draws a reader-specific field.
//!   Serialising a page type yourself (`serde_json::to_string(&page)`)
//!   does not: that is the caller's to do, with `.public()`;
//! - a **root**, `fn XIsland(page: XPage) -> Element`: the
//!   [`ClientSlot::Session`] slot first, then the page, refreshed through
//!   the [`Reload`] a client provides, its title given to the
//!   [`DocumentTitle`] a client provides;
//! - components below it that take props and fetch nothing.
//!
//! The types here (pages, loads, [`ClientSlot`], [`page::Page`]) are
//! exhaustive on purpose (Steward, 2026-11-15): every consumer is ours, and
//! a new field or variant that fails to compile at each construction and
//! match is the drift protection, where a wildcard arm would hide a new
//! page. Revisit if `ui` gets consumers we don't control.
//!
//! The data block holds the responses whole, bodies included: a heavier
//! page, bounded by what the server put on it.
//!
//! [`PostResponse`]: crate::responses::PostResponse
//! [`PostWithCommentsResponse`]: crate::responses::PostWithCommentsResponse
//! [`CommunityResponse`]: crate::responses::CommunityResponse
//!
//! ## Adding a page
//!
//! 1. A module with the three above, following [`thread`].
//! 2. A [`page::Page`] variant, and its arm in [`page::PageIsland`] and
//!    [`page::Page::title`]; a [`route::ClientPage`] variant, its arm in
//!    [`route::ClientPage::of`] in place of the kind's `None` (which is
//!    then a page the client draws: `FromStr` accepts it, [`route::To`]
//!    links to it client-side), its route component in `route`, and its
//!    load in [`route::Fetch`].
//! 3. Tests: a full-markup snapshot of each state, hostile text in every
//!    field a person chooses, the island round trip, the first render
//!    with the client's slots and `Reload` equal to the server's, and each
//!    state in `page`'s `every_page_reparses_as_drawn` (an HTML parser
//!    reads the markup as drawn).
//! 4. agora-server renders it at its URL with [`route::ssr::render`];
//!    agora-web needs only the `Reload` for its load.
//!
//! ## Adding a component
//!
//! Props are the API's types (`Clone + PartialEq`). A link to another page
//! is a [`route::To`] of its [`AgoraLink`](crate::links::AgoraLink): an
//! `<a href>` to the page's [`site_path`](crate::links::AgoraLink::site_path),
//! so a reader without JavaScript (and a crawler) follows it, which under
//! the router navigates client-side when the client draws that page.
//! Something a client does instead of following a link (Refresh, Try
//! again) is an `onclick` on a plain `<a>` to the page itself, which
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
//! is built, bounded by the source's length. Its module docs have the
//! policy; its parity corpus holds it to what agora-server draws today.
//! The server's own pages draw bodies with it too, unhydrated
//! ([`markdown::to_html`]), and cards' previews with
//! [`markdown::plain_preview`]: one renderer.
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
//!   after it. The tests re-parse every page's markup with html5ever to
//!   check (`parse_check`).

use std::{fmt, rc::Rc};

use dioxus::prelude::*;
use futures::future::LocalBoxFuture;

pub use client_only::{ClientOnly, ClientOnlyProps, ClientSlot, ClientSlots};

pub mod badges;
mod client_only;
pub mod comment;
pub mod feed;
pub mod format;
pub mod island;
pub mod markdown;
pub mod page;
#[cfg(all(test, feature = "dioxus-ssr"))]
mod parse_check;
pub mod route;
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

/// Where a page's title goes when it changes, provided as Dioxus context
/// by a client: a page's root gives it [`Page::document_title`] after
/// each render that changes it (in an effect, so never on the server).
/// The site's CSP blocks dioxus's own `document::Title`, so the client
/// sets `document.title` itself
///
/// [`Page::document_title`]: page::Page::document_title
#[derive(Clone)]
pub struct DocumentTitle(Rc<dyn Fn(&str)>);

impl DocumentTitle {
    pub fn new(set: impl Fn(&str) + 'static) -> Self {
        Self(Rc::new(set))
    }

    /// Give the page's title
    pub fn set(&self, title: &str) {
        (self.0)(title)
    }
}

impl fmt::Debug for DocumentTitle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DocumentTitle")
    }
}

/// Give `title` to the [`DocumentTitle`] a client provided, after each
/// render in which it changed. A hook
pub(crate) fn use_document_title(title: Memo<String>) {
    let to = try_use_context::<DocumentTitle>();
    use_effect(move || {
        let title = title();
        if let Some(to) = &to {
            to.set(&title);
        }
    });
}
