//! SPIKE (feature `router`): every page of the site behind the Dioxus
//! router, routed by the one table of paths there is, [`AgoraLink`]'s.
//!
//! - [`Route`] wraps an [`AgoraLink`] and implements
//!   [`Routable`](dioxus_router::Routable) by hand: `FromStr` reads a path
//!   below the router's base as a link, `Display` is the link's
//!   [`site_path`](AgoraLink::site_path) below it, `SITE_MAP` is empty and
//!   `render` is one exhaustive `match` over the kinds.
//! - [`PageData`] is the one data source above the router: on the first
//!   render it answers from the page the server embedded; every page after
//!   a client-side navigation starts `Pending` and its island loads it
//!   through [`Fetch`] in an effect, as a fresh mount does today.
//! - [`To`] is a link between pages: the router's `Link` under a router
//!   (client-side navigation), a plain `<a>` without one (the islands that
//!   are still mounted on their own).

use std::{cell::RefCell, fmt, rc::Rc, str::FromStr};

use dioxus::prelude::*;
use dioxus_router::{Link, Routable, Router, SiteMapSegment};
use futures::future::LocalBoxFuture;

use super::{
    Reload,
    feed::{FeedIsland, FeedLoad, FeedPage},
    page::Page,
    thread::{ThreadIsland, ThreadLoad, ThreadPage},
};
use crate::{
    ids::{ContentRef, PostId},
    links::{AgoraLink, ContentKind, SITE_PREFIX},
    requests::GetFeedInput,
};

/// The site's prefix as the router's base path sees it: `/agora`.
/// `site_path()` is `/agora/<kind>…`; the router's paths are `/<kind>…`,
/// and the history adds whatever base the site is mounted at (`/agora`
/// today, nothing at the root)
fn mount() -> &'static str {
    SITE_PREFIX.trim_end_matches('/')
}

/// A page of the site: the link it is
#[derive(Debug, Clone, PartialEq)]
pub struct Route(pub AgoraLink);

/// Why a path is no page of the router's
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteError(pub String);

impl fmt::Display for RouteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RouteError {}

impl FromStr for Route {
    type Err = RouteError;

    /// `path` is below the router's base, as the history gives it: a path,
    /// maybe a query, maybe a `#fragment` (the web history keeps it; the
    /// link doesn't). `/` is the global feed: the router parses `/` when a
    /// path doesn't parse, and panics if `/` doesn't either
    fn from_str(path: &str) -> Result<Self, Self::Err> {
        let path = path.split_once('#').map_or(path, |(p, _)| p);
        if path.is_empty() || path == "/" {
            return Ok(Self(AgoraLink::Feed(GetFeedInput::default())));
        }
        if !path.starts_with('/') {
            return Err(RouteError(format!("{path:?} is not a path")));
        }
        let full = [mount(), path].concat();
        match AgoraLink::from_site_path(&full) {
            // A page the server draws (`/about`, `/account`): not the
            // router's. Refused, so a `Link` to it is an external one (a
            // full page load), and the router never claims a server page
            Some(Ok(AgoraLink::Page(page))) => Err(RouteError(format!(
                "{path} is a page of the server's ({})",
                page.path()
            ))),
            Some(Ok(link)) => Ok(Self(link)),
            Some(Err(e)) => Err(RouteError(e.to_string())),
            None => Err(RouteError(format!("{path} is not a page"))),
        }
    }
}

impl fmt::Display for Route {
    /// The link's `site_path()` below the site's prefix: `/feed?sort=active`
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let path = self.0.site_path();
        f.write_str(path.strip_prefix(mount()).unwrap_or(&path))
    }
}

impl Routable for Route {
    /// No static site map: the pages are the links, and there is no list
    /// of them to generate (nothing here pre-renders a static site)
    const SITE_MAP: &'static [SiteMapSegment] = &[];

    /// One level (no layouts, no nested routes: no page calls `Outlet`).
    /// Each page is keyed by its path, so a navigation from one feed page
    /// to the next mounts a new page instead of reusing the last one's
    /// state
    fn render(&self, level: usize) -> Element {
        if level > 0 {
            return VNode::empty();
        }
        let key = self.to_string();
        match &self.0 {
            AgoraLink::Feed(input) => {
                rsx! { FeedRoute { key: "{key}", input: input.clone() } }
            }
            AgoraLink::Content { kind, read } => match (kind, &read.id) {
                (ContentKind::Post, ContentRef::Content(id)) => rsx! {
                    PostRoute { key: "{key}", post: PostId::from(*id.as_uuid()) }
                },
                _ => rsx! { NoPageYet { key: "{key}", link: self.0.clone() } },
            },
            AgoraLink::Read(_) => {
                rsx! { NoPageYet { key: "{key}", link: self.0.clone() } }
            }
            AgoraLink::GovernanceLog(_) => {
                rsx! { NoPageYet { key: "{key}", link: self.0.clone() } }
            }
            AgoraLink::Search(_) => {
                rsx! { NoPageYet { key: "{key}", link: self.0.clone() } }
            }
            AgoraLink::Profile(_) => {
                rsx! { NoPageYet { key: "{key}", link: self.0.clone() } }
            }
            AgoraLink::Constitution(_) => {
                rsx! { NoPageYet { key: "{key}", link: self.0.clone() } }
            }
            AgoraLink::Inbox(_) => {
                rsx! { NoPageYet { key: "{key}", link: self.0.clone() } }
            }
            // Never parsed (FromStr refuses it), here to be exhaustive
            AgoraLink::Page(_) => {
                rsx! { NoPageYet { key: "{key}", link: self.0.clone() } }
            }
        }
    }
}

/// How a client loads a page after a navigation
pub trait Fetch {
    fn feed(&self, input: GetFeedInput) -> LocalBoxFuture<'static, FeedLoad>;
    fn thread(&self, post: PostId) -> LocalBoxFuture<'static, ThreadLoad>;
}

/// The data source above the router, in root context
#[derive(Clone)]
pub struct PageData {
    /// The page the server embedded: given to the first page that asks
    /// for it, once. Everything after a navigation is fetched
    first: Rc<RefCell<Option<Page>>>,
    /// The client's loads; `None` on the server (nothing is fetched there)
    fetch: Option<Rc<dyn Fetch>>,
}

impl PageData {
    /// The server's: the page it read, and nothing to fetch with
    pub fn server(first: Option<Page>) -> Self {
        Self {
            first: Rc::new(RefCell::new(first)),
            fetch: None,
        }
    }

    /// The client's: the page from the data block, and its loads
    pub fn client(first: Option<Page>, fetch: impl Fetch + 'static) -> Self {
        Self {
            first: Rc::new(RefCell::new(first)),
            fetch: Some(Rc::new(fetch)),
        }
    }

    fn take_feed(&self, input: &GetFeedInput) -> FeedPage {
        let mut first = self.first.borrow_mut();
        match first.take() {
            Some(Page::Feed(page)) if &page.input == input => page,
            other => {
                *first = other;
                FeedPage::new(input.clone(), FeedLoad::Pending)
            }
        }
    }

    fn take_thread(&self, post: PostId) -> ThreadPage {
        let mut first = self.first.borrow_mut();
        match first.take() {
            Some(Page::Thread(page)) if page.post == post => page,
            other => {
                *first = other;
                ThreadPage::new(post, ThreadLoad::Pending)
            }
        }
    }
}

/// A feed's page: the embedded one on first load, else `Pending`, which
/// `FeedIsland` loads in an effect through the [`Reload`] given here
#[component]
fn FeedRoute(input: GetFeedInput) -> Element {
    let data = use_context::<PageData>();
    let page = use_hook(|| data.take_feed(&input));
    if let Some(fetch) = data.fetch.clone() {
        let input = input.clone();
        use_context_provider(move || {
            Reload::<FeedLoad>::new(move || fetch.feed(input.clone()))
        });
    }
    FeedIsland(page)
}

/// A post's page, as [`FeedRoute`]
#[component]
fn PostRoute(post: PostId) -> Element {
    let data = use_context::<PageData>();
    let page = use_hook(|| data.take_thread(post));
    if let Some(fetch) = data.fetch.clone() {
        use_context_provider(move || {
            Reload::<ThreadLoad>::new(move || fetch.thread(post))
        });
    }
    ThreadIsland(page)
}

/// A kind with no page in the spike: the link, and the server's page
#[component]
fn NoPageYet(link: AgoraLink) -> Element {
    let href = link.site_path();
    rsx! {
        section { class: "agora-web",
            h1 { "{link}" }
            p { "No page for this kind of link yet." }
            a { href: "{href}", "{href}" }
        }
    }
}

/// A link to another page of the site: client-side under a router, a
/// plain `<a href>` without one. Drawn the same on both sides as long as
/// both have a router or neither does
#[component]
pub fn To(
    link: AgoraLink,
    #[props(into, default)] class: Option<String>,
    children: Element,
) -> Element {
    if dioxus_router::hooks::try_router().is_some()
        && let Ok(route) = Route::from_str(&Route(link.clone()).to_string())
    {
        return rsx! {
            Link { to: route, class, {children} }
        };
    }
    let href = link.site_path();
    rsx! {
        a { class, href: "{href}", {children} }
    }
}

/// The site's root: the router, over [`PageData`] and a history the
/// renderer gives (the server's memory history; the browser's)
#[allow(non_snake_case)]
pub fn SiteRoot() -> Element {
    rsx! {
        Router::<Route> {}
    }
}

/// The server's pre-render of the page at `path_and_query` (a path as the
/// request has it, below `base`: `/agora` today, `""` at the root), from
/// the data it read for it
#[cfg(feature = "dioxus-ssr")]
pub mod ssr {
    use std::rc::Rc;

    use dioxus::prelude::*;
    use dioxus_history::{History, MemoryHistory};

    use super::{PageData, Route, RouteError, SiteRoot};
    use crate::ui::{island, page::Page, ssr::Rendered};

    /// The route a request's path below the base is, or why it is none
    /// (the handler's 404, decided before anything is drawn)
    pub fn route(path_and_query: &str) -> Result<Route, RouteError> {
        path_and_query.parse()
    }

    /// The router drawn at `path_and_query` (below `base`), and the data
    /// block of `first`, the page the server read for it
    pub fn render(
        base: &str,
        path_and_query: &str,
        first: Option<Page>,
    ) -> Result<Rendered, RouteError> {
        route(path_and_query)?;
        let first = first.map(island::Public::public);
        let script = island::script_element(island::DATA_ID, &first)
            .map_err(|e| RouteError(e.to_string()))?;
        let mut history = MemoryHistory::with_initial_path(path_and_query);
        if !base.is_empty() {
            history = history.with_prefix(base);
        }
        let history: Rc<dyn History> = Rc::new(history);
        let mut dom = VirtualDom::new(SiteRoot)
            .with_root_context(history)
            .with_root_context(PageData::server(first));
        dom.rebuild_in_place();
        Ok(Rendered {
            html: dioxus_ssr::pre_render(&dom),
            script,
        })
    }
}

#[cfg(all(test, feature = "dioxus-ssr"))]
mod tests;
