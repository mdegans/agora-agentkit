//! One entry point for every page, on both sides: the server says which
//! root a page draws by the [`Page`] it embeds, and the client reads the
//! same [`Page`] back from the data block and mounts [`PageIsland`] with
//! it, so neither side keeps a list of pages of its own.
//!
//! ```ignore
//! // agora-server
//! let page = Page::Thread(ThreadPage::loaded(read));
//! let Rendered { html, script } = page.public().render()?;
//! // agora-web
//! let page: Page = island::from_script_text(&text)?;
//! let dom = VirtualDom::new_with_props(PageIsland, page.clone())
//!     .with_root_context(slots);
//! ```
//!
//! The client provides the [`Reload`](super::Reload) the page's root
//! reads, by matching on the [`Page`] (`Reload<FeedPage>` for a feed,
//! `Reload<ThreadLoad>` for a thread). [`PageIsland`] draws one page with
//! no router, its links plain `<a href>`s; the site's pages are drawn
//! through the router by [`route::SiteRoot`](super::route::SiteRoot),
//! which reads the same [`Page`] from the same data block.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use super::{
    feed::{FeedIsland, FeedPage},
    island::Public,
    route::ClientPage,
    thread::{ThreadIsland, ThreadPage},
};

/// A page an island draws, and its data. `page.public().render()` draws and
/// embeds its public form ([`island::Public`](super::island::Public));
/// serialising a `Page` yourself does not, and `.public()` is then the
/// caller's to call.
/// In the data block as
/// `{"page": "thread", "data": {…}}`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "page", content = "data")]
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

impl super::island::Anonymous for Page {
    fn anonymous(self) -> Self {
        match self {
            Page::Feed(page) => Page::Feed(page.anonymous()),
            Page::Thread(page) => Page::Thread(page.anonymous()),
        }
    }
}

impl Public<Page> {
    /// The island, pre-rendered, with its data block
    /// ([`island::DATA_ID`](super::island::DATA_ID)): `page.public().render()`
    #[cfg(feature = "dioxus-ssr")]
    pub fn render(self) -> Result<super::ssr::Rendered, serde_json::Error> {
        super::ssr::render_island(super::island::DATA_ID, PageIsland, self)
    }
}

/// What every page's `<title>` ends with, after [`Page::title`]
pub const TITLE_SUFFIX: &str = " — Agora";

impl Page {
    /// The page's title, before [`TITLE_SUFFIX`]: drawn from what the page
    /// shows, so from its public form (through [`island::Public`](super::island::Public)'s `Deref`)
    /// when the page is public
    pub fn title(&self) -> String {
        match self {
            Page::Feed(page) => page.title(),
            Page::Thread(page) => page.title(),
        }
    }

    /// The page's `<title>`: [`title`](Self::title) and [`TITLE_SUFFIX`]
    pub fn document_title(&self) -> String {
        document_title(&self.title())
    }

    /// The website the page says it is on ([`ThreadPage::at`])
    pub fn site(&self) -> Option<&url::Url> {
        match self {
            Page::Feed(_) => None,
            Page::Thread(page) => page.site.as_ref(),
        }
    }

    /// The page the router draws this one as
    pub fn client_page(&self) -> ClientPage {
        match self {
            Page::Feed(page) => ClientPage::Feed(page.input.clone()),
            Page::Thread(page) => ClientPage::Thread(page.post),
        }
    }
}

/// `title` and [`TITLE_SUFFIX`]
pub(crate) fn document_title(title: &str) -> String {
    [title, TITLE_SUFFIX].concat()
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
        Page::Thread(ThreadPage { post, thread, site }) => rsx! {
            ThreadIsland { post, thread, site }
        },
    }
}

#[cfg(all(test, feature = "dioxus-ssr"))]
mod tests;
