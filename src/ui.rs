//! Agora's web components (feature `dioxus`): the one copy, drawn by
//! agora-web in the browser and pre-rendered by agora-server ([`ssr`],
//! feature `dioxus-ssr`) for the browser to hydrate.
//!
//! # Layout
//!
//! One submodule per page or area ([`feed`] today; a thread, the governance
//! log, a profile to come), each holding its view types and components
//! together. What several pages draw the same way goes in a shared module
//! ([`format`](mod@format) for text, and so on). The data hand-off is [`island`], the
//! pre-render [`ssr`].
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
//!   `dangerous_inner_html`: titles, names and error messages are hostile.

use std::{fmt, rc::Rc};

use futures::future::LocalBoxFuture;

pub use client_only::{ClientOnly, ClientOnlyProps, ClientSlot, ClientSlots};

mod client_only;
pub mod feed;
pub mod format;
pub mod island;
#[cfg(feature = "dioxus-ssr")]
pub mod ssr;

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
