//! The site's pages behind the Dioxus router, routed by the one table of
//! paths there is: [`AgoraLink`]'s.
//!
//! - [`Route`] is an [`AgoraLink`] with a hand-written
//!   [`Routable`]: `FromStr` reads a path below the site's [`base`] with
//!   [`AgoraLink::from_site_path`], `Display` writes
//!   [`AgoraLink::site_path`] below it, `SITE_MAP` is empty, and `render`
//!   draws what [`ClientPage::of`] decides. There is no second table.
//! - [`ClientPage::of`] is the one list of which links the client draws:
//!   an exhaustive `match` over the kinds, so a new kind fails to compile
//!   until someone decides its page. Only the feed, a post's thread and
//!   a comment have components; every other kind is drawn by the server, so
//!   `FromStr` refuses it and a link to it is a full page load.
//! - [`PageData`] is the data source above the router: the first page from
//!   the data block the server embedded, every page after a client-side
//!   navigation through the client's [`Fetch`].
//! - [`To`] is a link between pages: the router's `Link` under a router
//!   and for a client page, a plain `<a href>` otherwise. `to!` is the
//!   same for a link whose text is a literal, so that with no router the
//!   markup is byte for byte the `<a>` it replaced.
//! - [`SiteRoot`] is the root on both sides, given a history as root
//!   context: the server's [`ssr::render`] (a memory history), the
//!   browser's (dioxus-web's `WebHistory`).
//!
//! # What the router does on its own, and what this module does about it
//!
//! - **It rewrites the address bar** whenever `Display` differs from the
//!   history's current route (on start, `?offset=0&sort=active` becomes
//!   `?sort=active&offset=0`), and dioxus-web's rewrite drops the
//!   `#fragment` and scrolls to the top. [`SiteRoot`] wraps the history it
//!   is given so the router reads the canonical form (the route's
//!   `Display`, with no fragment) while the address bar keeps what was
//!   typed, fragment and scroll included. The canonical form is
//!   [`AgoraLink::site_path`]'s, with `q` first and the other options by
//!   name. A replace with the page it is already on is dropped, and the
//!   rewrite never happens at all.
//! - **A path that doesn't parse** is drawn as `/` with an error thrown
//!   into the tree. The server never gets there: [`ssr::route`] refuses
//!   the path first, which is the handler's 404 (or its own page). On the
//!   client, [`To`] draws a link the parser refuses as a plain `<a>`, and
//!   should the router reach such a path anyway (a history entry made
//!   outside it), [`SiteRoot`] catches the error and loads the page from
//!   the server, keeping the address as it is.
//! - **`/` must parse** or the router panics. It is the global feed, as
//!   the site's root `https://<site>/agora` reads as `agora://feed`, but
//!   only for the router: while the site is under `/agora`, `/agora/` is
//!   the server's communities page, so [`ssr::route`] refuses `/`, and
//!   `Display` never writes it.
//! - **`Link` panics with no router** (in debug): [`To`] draws a plain
//!   `<a>` there.
//! - **Levels**: no page has an `Outlet`, so `render` draws nothing for a
//!   `level` above 0.
//!
//! Links written by authors inside post and comment bodies
//! ([`markdown`](super::markdown)) stay plain `<a>`s: a full page load.

use std::{cell::RefCell, fmt, rc::Rc, str::FromStr};

use dioxus::prelude::*;
use dioxus_history::History;
use dioxus_router::{Link, ParseRouteError, Routable, Router, SiteMapSegment};
use futures::future::LocalBoxFuture;

use super::{
    Reload,
    comment::{CommentIsland, CommentLoad, CommentPage},
    feed::{FeedIsland, FeedPage},
    page::Page,
    thread::{ThreadIsland, ThreadLoad, ThreadPage},
};
use crate::{
    ids::{CommentId, ContentRef, PostId},
    links::{AgoraLink, AgoraLinkError, ContentKind, SITE_PREFIX},
    requests::{GetContentInput, GetFeedInput},
};

/// The path the site is mounted at, the router's base: [`SITE_PREFIX`]
/// without its last `/` (`/agora` today, `""` at the root). The server
/// gives it to [`ssr::render`] and writes it in the page for the client's
/// history (`<meta name="agora-base">`)
pub fn base() -> &'static str {
    SITE_PREFIX.strip_suffix('/').unwrap_or(SITE_PREFIX)
}

/// A page of the site: the link it is
#[derive(Debug, Clone, PartialEq)]
pub struct Route(pub AgoraLink);

/// Why a path is not a page the router draws
#[derive(Debug, thiserror::Error)]
pub enum RouteError {
    /// Not a path below the base (`/feed`)
    #[error("{0:?} is not a path")]
    NotAPath(String),
    /// Not a link's path
    #[error("{path} is not a page: {error}")]
    NotALink { path: String, error: AgoraLinkError },
    /// A link whose page the server draws ([`ClientPage::of`])
    #[error("{path} is {link}, a page the server draws")]
    ServerPage { path: String, link: Box<AgoraLink> },
    /// `/`, which only the router reads (as the global feed)
    #[error(
        "/ is the server's page while the site is mounted at {SITE_PREFIX}"
    )]
    Root,
    /// A base other than [`base`]: the links would point elsewhere
    #[error("the site is mounted at {expected:?}, not {given:?}")]
    Base {
        given: String,
        expected: &'static str,
    },
    /// The data block
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

/// A page the client draws, and what it draws it from: what
/// [`ClientPage::of`] makes of a link
#[derive(Debug, Clone, PartialEq)]
pub enum ClientPage {
    /// The global feed or a community's ([`super::feed`])
    Feed(GetFeedInput),
    /// A post with its comments ([`super::thread`])
    Thread(PostId),
    /// A comment under its post and the comments it answers
    /// ([`super::comment`])
    Comment(CommentId),
}

impl ClientPage {
    /// The page the client draws for `link`, or `None` for a page the
    /// server draws. The one list: `FromStr` accepts exactly these,
    /// `render` draws them, and [`To`] links to them client-side
    pub fn of(link: &AgoraLink) -> Option<Self> {
        match link {
            AgoraLink::Feed(input) => Some(Self::Feed(input.clone())),
            // A post with no options is its page; with any (a budget,
            // lines, an attachment), what `get_content` reads, which the
            // server draws, as it does a short id
            AgoraLink::Content {
                kind: ContentKind::Post,
                read,
            } => match &read.id {
                ContentRef::Content(id)
                    if *read == GetContentInput::new(*id) =>
                {
                    // The link says it is a post; the read checks
                    Some(Self::Thread(PostId::from(*id.as_uuid())))
                }
                _ => None,
            },
            // A comment by its full id with no options is its page; a short
            // id or a read with options is `get_content`'s, the server's
            AgoraLink::Content {
                kind: ContentKind::Comment,
                read,
            } => match &read.id {
                ContentRef::Content(id)
                    if *read == GetContentInput::new(*id) =>
                {
                    Some(Self::Comment(CommentId::from(*id.as_uuid())))
                }
                _ => None,
            },
            // `content/<id>` names no kind: the server resolves it and
            // redirects to the post's or the comment's page
            AgoraLink::Content {
                kind: ContentKind::Either,
                ..
            } => None,
            AgoraLink::Read(_) => None,
            AgoraLink::GovernanceLog(_) => None,
            AgoraLink::Search(_) => None,
            AgoraLink::Profile(_) => None,
            AgoraLink::Constitution(_) => None,
            AgoraLink::Inbox(_) => None,
            // A page no kind names: the server's own (`/agora/about`)
            AgoraLink::Page(_) => None,
        }
    }

    /// The page's link
    pub fn link(&self) -> AgoraLink {
        match self {
            Self::Feed(input) => AgoraLink::Feed(input.clone()),
            Self::Thread(post) => AgoraLink::post(*post),
            Self::Comment(comment) => AgoraLink::comment(*comment),
        }
    }
}

impl Route {
    /// What the router reads `/` as: the global feed, as the site's root
    /// page URL reads as `agora://feed`
    pub fn root() -> Self {
        Self(AgoraLink::Feed(GetFeedInput::default()))
    }

    /// The page the client draws for this route, `None` for one the server
    /// draws (never a route the router holds: `FromStr` refuses them)
    pub fn page(&self) -> Option<ClientPage> {
        ClientPage::of(&self.0)
    }

    /// A path below the base, read strictly: no `/`, no `#fragment`
    fn parse(path: &str) -> Result<(Self, ClientPage), RouteError> {
        let Some(rest) = path.strip_prefix('/') else {
            return Err(RouteError::NotAPath(path.to_string()));
        };
        if rest.is_empty() || rest.starts_with('?') {
            return Err(RouteError::Root);
        }
        let full = [SITE_PREFIX, rest].concat();
        let link = match AgoraLink::from_site_path(&full) {
            Some(Ok(link)) => link,
            Some(Err(error)) => {
                return Err(RouteError::NotALink {
                    path: path.to_string(),
                    error,
                });
            }
            None => return Err(RouteError::NotAPath(path.to_string())),
        };
        match ClientPage::of(&link) {
            Some(page) => Ok((Self(link), page)),
            None => Err(RouteError::ServerPage {
                path: path.to_string(),
                link: Box::new(link),
            }),
        }
    }
}

/// `path` without its `#fragment`
fn without_fragment(path: &str) -> &str {
    path.split_once('#').map_or(path, |(path, _)| path)
}

impl FromStr for Route {
    type Err = RouteError;

    /// A path below the base, as the history gives it, maybe with a
    /// `#fragment` (dropped). `/` (and `""`) is [`Route::root`]
    fn from_str(path: &str) -> Result<Self, Self::Err> {
        let path = without_fragment(path);
        if path.is_empty() || path == "/" {
            return Ok(Self::root());
        }
        Self::parse(path).map(|(route, _)| route)
    }
}

impl fmt::Display for Route {
    /// The link's [`site_path`](AgoraLink::site_path) below the base:
    /// `/feed?sort=active`. Never `/`: every link's path names its kind
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let path = self.0.site_path();
        f.write_str(path.strip_prefix(base()).unwrap_or(&path))
    }
}

impl Routable for Route {
    /// None: the pages are the links, and nothing pre-renders a static
    /// site from a list of them
    const SITE_MAP: &'static [SiteMapSegment] = &[];

    /// The page at level 0, as a list of one keyed by its path, so a
    /// navigation from one feed page to another mounts a new page rather
    /// than giving the last one new props over its old state (a key on a
    /// lone node doesn't do that: only a keyed list's diff does). Nothing
    /// at a deeper level: no page has an `Outlet`
    fn render(&self, level: usize) -> Element {
        if level > 0 {
            return VNode::empty();
        }
        let key = self.to_string();
        let page = match ClientPage::of(&self.0) {
            Some(ClientPage::Feed(input)) => rsx! {
                FeedRoute { key: "{key}", input }
            },
            Some(ClientPage::Thread(post)) => rsx! {
                ThreadRoute { key: "{key}", post }
            },
            Some(ClientPage::Comment(comment)) => rsx! {
                CommentRoute { key: "{key}", comment }
            },
            None => rsx! {
                ServerPage { key: "{key}", link: self.0.clone() }
            },
        };
        rsx! {
            {std::iter::once(page)}
        }
    }
}

/// How a client loads a page after a navigation (and again, on Refresh),
/// over the public API. In words for the public on failure, as
/// [`FeedLoad::Failed`](super::feed::FeedLoad::Failed); a missing post or
/// community is `NotFound`, not a failure
pub trait Fetch {
    /// The feed's page of `input`: its posts, and for a community's feed
    /// the community's header (its `total` only if the API gives one)
    fn feed(&self, input: GetFeedInput) -> LocalBoxFuture<'static, FeedPage>;
    /// The post with its comments
    fn thread(&self, post: PostId) -> LocalBoxFuture<'static, ThreadLoad>;
    /// The comment under its post, with the comments it answers: the
    /// public API's comment read (`GET /api/content/<id>`, its
    /// `ContentResponse::Comment`), anonymously
    fn comment(
        &self,
        comment: CommentId,
    ) -> LocalBoxFuture<'static, CommentLoad>;
}

/// The data source above the router, as root context
#[derive(Clone)]
pub struct PageData {
    /// The page the server embedded, for the first page drawn and no
    /// other
    first: Rc<RefCell<Option<Page>>>,
    /// The client's loads; `None` on the server, which fetches nothing
    fetch: Option<Rc<dyn Fetch>>,
    /// The website the pages are on, for a thread's [`Site`](super::markdown::Site)
    /// after a navigation: the first page's when it names one, else
    /// [`at`](Self::at)'s
    site: Option<url::Url>,
}

impl PageData {
    /// The server's: the page it read
    pub fn server(first: Page) -> Self {
        Self {
            site: first.site().cloned(),
            first: Rc::new(RefCell::new(Some(first))),
            fetch: None,
        }
    }

    /// The client's: the page from the data block, and its loads
    pub fn client(first: Page, fetch: impl Fetch + 'static) -> Self {
        Self {
            site: first.site().cloned(),
            first: Rc::new(RefCell::new(Some(first))),
            fetch: Some(Rc::new(fetch)),
        }
    }

    /// The website the pages are on, for the threads a client loads after
    /// a navigation when the first page names none (a feed's doesn't)
    pub fn at(mut self, site: &url::Url) -> Self {
        self.site = Some(site.clone());
        self
    }

    /// The embedded page if it is `page`'s, taken: only the first page
    /// drawn may have it, whether it matches or not
    fn take(&self, page: &ClientPage) -> Option<Page> {
        self.first
            .borrow_mut()
            .take()
            .filter(|first| first.client_page() == *page)
    }
}

impl fmt::Debug for PageData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PageData")
            .field("first", &self.first)
            .field("fetch", &self.fetch.is_some())
            .field("site", &self.site)
            .finish()
    }
}

/// A feed's page: the embedded one on first load, else `Pending`, which
/// `FeedIsland` loads through the [`Reload`] given here
#[component]
fn FeedRoute(input: GetFeedInput) -> Element {
    let data = use_context::<PageData>();
    let page = use_hook(|| match data.take(&ClientPage::Feed(input.clone())) {
        Some(Page::Feed(page)) => page,
        _ => FeedPage::pending(input.clone()),
    });
    use_hook(|| {
        if let Some(fetch) = data.fetch.clone() {
            let input = input.clone();
            provide_context(Reload::<FeedPage>::new(move || {
                fetch.feed(input.clone())
            }));
        }
    });
    FeedIsland(page)
}

/// A post's page, as [`FeedRoute`]
#[component]
fn ThreadRoute(post: PostId) -> Element {
    let data = use_context::<PageData>();
    let page = use_hook(|| match data.take(&ClientPage::Thread(post)) {
        Some(Page::Thread(page)) => page,
        _ => {
            let page = ThreadPage::new(post, ThreadLoad::Pending);
            match &data.site {
                Some(site) => page.at(site),
                None => page,
            }
        }
    });
    use_hook(|| {
        if let Some(fetch) = data.fetch.clone() {
            provide_context(Reload::<ThreadLoad>::new(move || {
                fetch.thread(post)
            }));
        }
    });
    ThreadIsland(page)
}

/// A comment's page, as [`FeedRoute`]
#[component]
fn CommentRoute(comment: CommentId) -> Element {
    let data = use_context::<PageData>();
    let page = use_hook(|| match data.take(&ClientPage::Comment(comment)) {
        Some(Page::Comment(page)) => page,
        _ => {
            let page = CommentPage::new(comment, CommentLoad::Pending);
            match &data.site {
                Some(site) => page.at(site),
                None => page,
            }
        }
    });
    use_hook(|| {
        if let Some(fetch) = data.fetch.clone() {
            provide_context(Reload::<CommentLoad>::new(move || {
                fetch.comment(comment)
            }));
        }
    });
    CommentIsland(page)
}

/// A page the server draws, should the router ever hold one. It doesn't
/// (`FromStr` refuses these, and [`To`] draws plain links to them); if it
/// did, the page is loaded from the server, with its link shown until it is
#[component]
fn ServerPage(link: AgoraLink) -> Element {
    let href = link.site_path();
    let to = href.clone();
    use_effect(move || {
        load_from_server(to.clone());
    });
    rsx! {
        Opening { href }
    }
}

/// What the reader sees while a page loads from the server instead
#[component]
fn Opening(href: String) -> Element {
    rsx! {
        section { class: "agora-web",
            p { class: "post-meta", role: "status",
                "Opening "
                a { href: "{href}", "{href}" }
                "…"
            }
        }
    }
}

/// A full page load of `href` (a path on the site), through the history
/// the renderer gave (dioxus-web's sets `location.href`; a memory
/// history does nothing)
fn load_from_server(href: String) {
    match try_consume_context::<Rc<dyn History>>() {
        Some(history) => {
            if !history.external(href.clone()) {
                tracing::warn!(%href, "the history can't load a page");
            }
        }
        None => tracing::error!(%href, "no history to load a page with"),
    }
}

/// Whether a link to `link` is the router's: under a router, to a page the
/// client draws. What [`To`] and `to!` decide by
pub fn routed(link: &AgoraLink) -> bool {
    dioxus_router::hooks::try_router().is_some()
        && ClientPage::of(link).is_some()
}

/// [`To`] for a link whose text is a literal, written where the link is:
/// `{to!(link, class: "pill"; "New")}` inside `rsx!`. Outside a router (or
/// to a page the server draws) it is the `a { class, href, "New" }` it
/// replaces, in the caller's own template, so the markup is byte for byte
/// what that `a` drew: a component's children are a dynamic node, and the
/// pre-render marks a dynamic node's text (`<!--node-id…-->`), which `To`
/// would add around static text (text that is formatted, `"{name}"`, is
/// marked either way: use `To`, which `rsx!` can format it for). The
/// attributes are written in the order given, `href` last
macro_rules! to {
    ($link:expr $(, $name:ident : $value:expr)* ; $text:literal) => {{
        let link: $crate::links::AgoraLink = $link;
        if $crate::ui::route::routed(&link) {
            rsx! {
                $crate::ui::route::To { link, $($name: $value,)* $text }
            }
        } else {
            let href = link.site_path();
            rsx! {
                a { $($name: $value,)* href: "{href}", $text }
            }
        }
    }};
}
pub(crate) use to;

/// A link to another page of the site: the router's `Link` under a router
/// for a page the client draws ([`ClientPage::of`]), else a plain
/// `<a href>` to its [`site_path`](AgoraLink::site_path), drawn exactly as
/// `a { class, style, href, "{text}" }` would be. Under a router the
/// `Link` adds its click listener (a hydration marker),
/// `aria-current="page"` on a link to the page it is on, and a marker
/// around literal text, and writes `href` before `class`
#[component]
pub fn To(
    link: AgoraLink,
    #[props(into, default)] class: Option<String>,
    #[props(into, default)] style: Option<String>,
    children: Element,
) -> Element {
    if routed(&link) {
        return rsx! {
            Link { to: Route(link), class, style, {children} }
        };
    }
    let href = link.site_path();
    rsx! {
        a { class, style, href: "{href}", {children} }
    }
}

/// The site's root on both sides: the router over the history the
/// renderer gives as root context, [`PageData`] (root context too) and
/// the client's slots. See the module docs for what it does with the
/// history
#[allow(non_snake_case)]
pub fn SiteRoot() -> Element {
    let given = use_hook(|| {
        let given = try_consume_context::<Rc<dyn History>>();
        if let Some(given) = &given {
            provide_context::<Rc<dyn History>>(Rc::new(SiteHistory {
                inner: given.clone(),
            }));
        }
        given
    });
    let Some(given) = given else {
        return rsx! {
            section { class: "agora-web",
                div { class: "empty-state", role: "alert",
                    h3 { "This page couldn't be drawn" }
                    p { "The site's client was started without a history." }
                }
            }
        };
    };
    rsx! {
        ErrorBoundary {
            handle_error: move |errors: ErrorContext| {
                let href = full_path(given.as_ref());
                match errors.error() {
                    Some(e) if e.0.downcast_ref::<ParseRouteError>().is_some() => rsx! {
                        NotRouted { href }
                    },
                    error => {
                        let said = error.map(|e| e.to_string()).unwrap_or_default();
                        tracing::error!(error = %said, "a page of the site failed to draw");
                        rsx! {
                            section { class: "agora-web",
                                div { class: "empty-state", role: "alert",
                                    h3 { "This page couldn't be drawn" }
                                    p { "{said}" }
                                    a { class: "pill pill-btn", href: "{href}", "Load it again" }
                                }
                            }
                        }
                    }
                }
            },
            Router::<Route> {}
        }
    }
}

/// The router at a path it doesn't draw: the page, from the server, at the
/// address as it is
#[component]
fn NotRouted(href: String) -> Element {
    let to = href.clone();
    use_effect(move || load_from_server(to.clone()));
    rsx! {
        Opening { href }
    }
}

/// The history's whole current path, its base and `#fragment` included
fn full_path(history: &dyn History) -> String {
    [
        history.current_prefix().unwrap_or_default(),
        history.current_route(),
    ]
    .concat()
}

/// The history the router reads, over the one the renderer gave: the
/// route as `Display` writes it, so the router never rewrites the address
/// bar to canonicalise it (see the module docs)
struct SiteHistory {
    inner: Rc<dyn History>,
}

impl SiteHistory {
    /// The page the address is, if the router draws it
    fn current(&self) -> Result<Route, RouteError> {
        self.inner.current_route().parse()
    }
}

impl History for SiteHistory {
    /// The canonical route, without its fragment; a path that doesn't
    /// parse as it is (without its fragment), for the router to refuse
    fn current_route(&self) -> String {
        let raw = self.inner.current_route();
        match raw.parse::<Route>() {
            Ok(route) => route.to_string(),
            Err(_) => without_fragment(&raw).to_string(),
        }
    }

    fn current_prefix(&self) -> Option<String> {
        self.inner.current_prefix()
    }

    fn can_go_back(&self) -> bool {
        self.inner.can_go_back()
    }

    fn go_back(&self) {
        self.inner.go_back()
    }

    fn can_go_forward(&self) -> bool {
        self.inner.can_go_forward()
    }

    fn go_forward(&self) {
        self.inner.go_forward()
    }

    /// A page the router draws is pushed; any other is loaded from the
    /// server
    fn push(&self, route: String) {
        if route.parse::<Route>().is_ok() {
            self.inner.push(route);
        } else {
            self.external(
                self.inner.current_prefix().unwrap_or_default() + &route,
            );
        }
    }

    /// Not to the page it is on (the router's canonicalising rewrite: the
    /// address keeps what was typed, its fragment and scroll), and never
    /// from an address the router doesn't draw (its fallback to `/`:
    /// [`SiteRoot`] loads that page from the server instead)
    fn replace(&self, path: String) {
        let Ok(now) = self.current() else {
            return;
        };
        match path.parse::<Route>() {
            Ok(next) if next == now => {}
            Ok(_) => self.inner.replace(path),
            Err(_) => {
                self.external(
                    self.inner.current_prefix().unwrap_or_default() + &path,
                );
            }
        }
    }

    fn external(&self, url: String) -> bool {
        self.inner.external(url)
    }

    fn updater(&self, callback: std::sync::Arc<dyn Fn() + Send + Sync>) {
        self.inner.updater(callback)
    }

    fn include_prevent_default(&self) -> bool {
        self.inner.include_prevent_default()
    }
}

/// The server's pre-render of the site (feature `dioxus-ssr`): which
/// requests are the router's ([`ssr::route`], before
/// anything is read or drawn), and the page drawn through the router
/// ([`ssr::render`])
#[cfg(feature = "dioxus-ssr")]
pub mod ssr {
    use std::rc::Rc;

    use dioxus::prelude::*;
    use dioxus_history::{History, MemoryHistory};

    use super::{ClientPage, PageData, Route, RouteError, SiteRoot};
    use crate::ui::{island, page::Page, ssr::Rendered};

    /// The page a request's path (and query) below the [`base`](super::base) is, or why
    /// the router doesn't draw it: the handler's 404, or its own page for
    /// a link of a kind the server draws. Stricter than `FromStr`: `/` is
    /// refused ([`RouteError::Root`]), and so is a `#fragment` (a request
    /// has none)
    pub fn route(path_and_query: &str) -> Result<ClientPage, RouteError> {
        if path_and_query.contains('#') {
            return Err(RouteError::NotAPath(path_and_query.to_string()));
        }
        Route::parse(path_and_query).map(|(_, page)| page)
    }

    /// `page` drawn through the router at its own route, as markup for
    /// inside the [`island::ROOT_ID`] element, and its data block, in
    /// their public form ([`island::Public`]). `base` is where the site is
    /// mounted, which must be [`base`](super::base)
    pub fn render(
        base: &str,
        page: island::Public<Page>,
    ) -> Result<Rendered, RouteError> {
        if base != super::base() {
            return Err(RouteError::Base {
                given: base.to_string(),
                expected: super::base(),
            });
        }
        let script = island::script_element(island::DATA_ID, &page)?;
        let page = page.into_inner();
        let route = Route(page.client_page().link());
        let mut history = MemoryHistory::with_initial_path(route);
        if !base.is_empty() {
            history = history.with_prefix(base);
        }
        let history: Rc<dyn History> = Rc::new(history);
        let mut dom = VirtualDom::new(SiteRoot)
            .with_root_context(history)
            .with_root_context(PageData::server(page));
        dom.rebuild_in_place();
        Ok(Rendered {
            html: dioxus_ssr::pre_render(&dom),
            script,
        })
    }
}

#[cfg(all(test, feature = "dioxus-ssr"))]
mod tests;
