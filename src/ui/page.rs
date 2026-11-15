//! One entry point for every page, on both sides: the server says which
//! root a page draws by the [`Page`] it embeds, and the client reads the
//! same [`Page`] back from the data block and mounts [`PageIsland`] with
//! it, so neither side keeps a list of pages of its own.
//!
//! ```ignore
//! // agora-server
//! let page = Page::Thread(ThreadPage::loaded(read));
//! let Rendered { html, script } = page.render()?;
//! // agora-web
//! let page: Page = island::from_script_text(&text)?;
//! let dom = VirtualDom::new_with_props(PageIsland, page.clone())
//!     .with_root_context(slots);
//! ```
//!
//! The client provides the [`Reload`](super::Reload) the page's root
//! reads, by matching on the [`Page`] (`Reload<FeedLoad>` for a feed,
//! `Reload<ThreadLoad>` for a thread). Links between pages stay plain
//! `<a href>`s to server-rendered pages: there is no client-side router.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use super::{
    feed::{FeedIsland, FeedPage},
    thread::{ThreadIsland, ThreadPage},
};

/// A page an island draws, and its data. In the data block as
/// `{"page": "thread", "data": {…}}`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "page", content = "data")]
#[non_exhaustive]
// One per page, built once and moved into its root: boxing the larger
// variant would buy nothing
#[allow(clippy::large_enum_variant)]
pub enum Page {
    /// `/agora/feed`, `/agora/community/<name>` and `/agora/app`
    Feed(FeedPage),
    /// `/agora/post/<id>`
    Thread(ThreadPage),
}

impl From<FeedPage> for Page {
    fn from(page: FeedPage) -> Self {
        Self::Feed(page)
    }
}

impl From<ThreadPage> for Page {
    fn from(page: ThreadPage) -> Self {
        Self::Thread(page)
    }
}

impl Page {
    /// The island, pre-rendered, with its data block
    /// ([`island::DATA_ID`](super::island::DATA_ID))
    #[cfg(feature = "dioxus-ssr")]
    pub fn render(self) -> Result<super::ssr::Rendered, serde_json::Error> {
        super::ssr::render_island(super::island::DATA_ID, PageIsland, self)
    }
}

/// Every page's root: the page's own root, from its data
#[allow(non_snake_case)]
pub fn PageIsland(page: Page) -> Element {
    match page {
        Page::Feed(FeedPage {
            feed,
            input,
            community,
            total,
        }) => rsx! {
            FeedIsland { feed, input, community, total }
        },
        Page::Thread(ThreadPage { post, thread }) => rsx! {
            ThreadIsland { post, thread }
        },
    }
}

#[cfg(all(test, feature = "dioxus-ssr"))]
mod tests;
