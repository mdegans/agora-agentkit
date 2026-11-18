use std::{
    cell::{Cell, RefCell},
    collections::BTreeSet,
};

use dioxus_history::MemoryHistory;
use dioxus_router::navigator;

use super::*;
use crate::ui::island::Anonymous as _;
use crate::{
    enums::FeedSort,
    fixtures,
    responses::PostResponse,
    ui::{
        ClientSlot, ClientSlots, DocumentTitle,
        comment::{COMMENT_NOT_FOUND, CommentLoad, CommentPage},
        feed::{COMMUNITY_NOT_FOUND, FeedLoad},
        island,
        page::PageIsland,
        parse_check::{Tok, assert_hydratable, tokens},
        ssr::pre_render,
        thread::POST_NOT_FOUND,
    },
};

// -- Every kind decided --------------------------------------------------------

/// Each of `AgoraLink`'s variants by name: exhaustive, so a new variant
/// fails to compile here until its examples are added below
fn variant(link: &AgoraLink) -> &'static str {
    match link {
        AgoraLink::Content { .. } => "Content",
        AgoraLink::Read(_) => "Read",
        AgoraLink::GovernanceLog(_) => "GovernanceLog",
        AgoraLink::Feed(_) => "Feed",
        AgoraLink::Search(_) => "Search",
        AgoraLink::Profile(_) => "Profile",
        AgoraLink::Constitution(_) => "Constitution",
        AgoraLink::Inbox(_) => "Inbox",
        AgoraLink::Page(_) => "Page",
    }
}

/// Every name [`variant`] has
const VARIANTS: [&str; 9] = [
    "Content",
    "Read",
    "GovernanceLog",
    "Feed",
    "Search",
    "Profile",
    "Constitution",
    "Inbox",
    "Page",
];

const POST: &str = "8dbc9b3a-db55-47d5-8b14-5b913a1b44ac";
const COMMENT: &str = "c0000001-0000-4000-8000-000000000001";

/// A path below the base of every kind of link (links' own `EVERY_KIND`
/// has neither `inbox` nor `page`, nor a post by its full id), and whether
/// the client draws it
fn every_kind() -> Vec<(String, bool)> {
    let client = [
        "/feed".to_string(),
        "/feed?sort=active".into(),
        "/feed?offset=25&sort=active".into(),
        "/feed?limit=10&offset=20&sort=diverse".into(),
        "/community/meta-governance".into(),
        "/community/meta-governance?offset=25&sort=active".into(),
        format!("/post/{POST}"),
        // A comment by its full id with no options (the path doesn't know
        // whether the id is a post's: the read checks)
        format!("/comment/{COMMENT}"),
    ];
    let server = [
        // A post by its short id, or with options: `get_content`'s read
        "/post/8dbc9b3a".to_string(),
        format!("/post/{POST}?comment_budget=4096"),
        format!("/post/{POST}?detail=summary"),
        // A comment by its short id, or with options: `get_content`'s read
        "/comment/c0000001".into(),
        format!("/comment/{COMMENT}?comment_budget=4096"),
        format!("/content/{POST}"),
        format!("/content/{COMMENT}"),
        "/gov/GOV-2026-0006".into(),
        "/gov/GOV-2026-0010/clerk-reads".into(),
        "/gov/DATA-2026-0010/votes/index.md?lines=586-627&commit=89fe7c6"
            .into(),
        "/gov?type=council_decision".into(),
        "/protocol?lines=412-455".into(),
        "/calendar".into(),
        "/prompts/tier2_reviewer".into(),
        "/git/agora-agentkit/src/links.rs?lines=40-60&commit=47c2cf5".into(),
        "/constitution".into(),
        "/constitution?version=0.3".into(),
        "/search?q=sorting+code".into(),
        "/search?q=hello&offset=10".into(),
        "/agent/litmus".into(),
        "/inbox".into(),
        "/inbox?message=1a2b3c4d".into(),
        // `agora://page/about`, the server's `/agora/about`
        "/page/about".into(),
    ];
    client
        .into_iter()
        .map(|p| (p, true))
        .chain(server.into_iter().map(|p| (p, false)))
        .collect()
}

/// The link at `path` below the base, as the server reads its path
fn link_at(path: &str) -> AgoraLink {
    AgoraLink::from_site_path(&[SITE_PREFIX, &path[1..]].concat())
        .unwrap()
        .unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// Every kind has an example, and each is decided: a page the client
/// draws parses and round-trips path → route → path (canonical), and every
/// other is refused by both parsers, as a page of the server's
#[test]
fn every_kind_is_decided_and_round_trips() {
    let mut seen = BTreeSet::new();
    for (path, client) in every_kind() {
        let link = link_at(&path);
        seen.insert(variant(&link));
        assert_eq!(ClientPage::of(&link).is_some(), client, "{path}");
        let route = path.parse::<Route>();
        let pre = ssr::route(&path);
        if client {
            let route = route.unwrap_or_else(|e| panic!("{path}: {e}"));
            assert_eq!(route.0, link);
            // The canonical form is the link's site path below the base
            let shown = route.to_string();
            assert_eq!([base(), &shown].concat(), link.site_path());
            assert_eq!(shown.parse::<Route>().unwrap(), route, "{path}");
            assert_eq!(pre.unwrap(), ClientPage::of(&link).unwrap());
            assert_eq!(route.page().unwrap().link(), link, "{path}");
            println!("{path:48} -> {shown}");
        } else {
            let e = route.expect_err(&path);
            assert!(matches!(e, RouteError::ServerPage { .. }), "{path}: {e}");
            assert!(
                matches!(pre, Err(RouteError::ServerPage { .. })),
                "{path}: {pre:?}"
            );
            println!("{path:48} -> refused: {e}");
        }
    }
    assert_eq!(
        seen,
        BTreeSet::from(VARIANTS),
        "a variant without an example"
    );
}

/// The canonical form: options by name (`q` first), defaults as given. A
/// path in another order is the same route, and `Display` writes the
/// canonical one
#[test]
fn a_non_canonical_path_is_the_same_route() {
    for (given, canonical) in [
        ("/feed?sort=active&offset=25", "/feed?offset=25&sort=active"),
        (
            "/community/meta-governance?sort=active&limit=10",
            "/community/meta-governance?limit=10&sort=active",
        ),
        ("/feed?offset=0", "/feed?offset=0"),
    ] {
        let route: Route = given.parse().unwrap();
        assert_eq!(route.to_string(), canonical, "{given}");
        assert_eq!(canonical.parse::<Route>().unwrap(), route);
    }
}

/// `/` is the global feed for the router only: the server's pre-check
/// refuses it, and `Display` never writes it, for any link
#[test]
fn the_root_parses_for_the_router_only() {
    for root in ["/", "", "#top", "/#top"] {
        assert_eq!(root.parse::<Route>().unwrap(), Route::root(), "{root:?}");
    }
    assert_eq!(Route::root().to_string(), "/feed");
    for root in ["/", "/?sort=active"] {
        assert!(matches!(ssr::route(root), Err(RouteError::Root)), "{root}");
    }
    for (path, _) in every_kind() {
        let shown = Route(link_at(&path)).to_string();
        assert!(shown.starts_with('/') && shown.len() > 1, "{path}: {shown}");
    }
}

/// A fragment is dropped by the router's parse and refused by the server's
/// (a request has none); nonsense is refused, saying why
#[test]
fn fragments_and_nonsense() {
    let anchored: Route = format!("/post/{POST}#comment-abc").parse().unwrap();
    assert_eq!(anchored.to_string(), format!("/post/{POST}"));
    assert!(matches!(
        ssr::route(&format!("/post/{POST}#x")),
        Err(RouteError::NotAPath(_))
    ));
    for bad in ["feed", "?sort=active"] {
        let e = bad.parse::<Route>().unwrap_err();
        assert!(matches!(e, RouteError::NotAPath(_)), "{bad}: {e}");
    }
    for bad in [
        "/nope",
        "/post/not-an-id",
        "/gov/../x",
        "/api/social/feed",
        "//feed",
        "/feed?sort=bogus",
    ] {
        let e = bad.parse::<Route>().unwrap_err();
        println!("{bad:24} -> {e}");
        assert!(
            matches!(
                e,
                RouteError::NotALink { .. } | RouteError::ServerPage { .. }
            ),
            "{bad}: {e}"
        );
        assert!(ssr::route(bad).is_err(), "{bad}");
    }
}

/// No page has an `Outlet`: nothing below level 0
#[test]
fn nothing_is_drawn_below_the_first_level() {
    let empty = VNode::empty().unwrap();
    for level in [1, 2] {
        let drawn = Route::root().render(level).unwrap();
        assert_eq!(drawn.template, empty.template);
        assert_eq!(drawn.dynamic_nodes.len(), empty.dynamic_nodes.len());
    }
}

// -- The server's render ---------------------------------------------------------

fn feed_page() -> Page {
    FeedPage::new(
        GetFeedInput {
            sort: Some(FeedSort::Date),
            ..GetFeedInput::default()
        },
        [fixtures::post()].iter().collect(),
    )
    .with_total(60)
    .into()
}

fn thread_page() -> Page {
    ThreadPage::loaded(fixtures::post_with_comments()).into()
}

fn comment_page() -> Page {
    CommentPage::loaded(fixtures::comment().id, fixtures::comment_chain())
        .into()
}

/// `markup` as a reader's tree reads it, less what only hydration and the
/// router's `Link` add: the markers (comments, `data-node-hydration`) and
/// `aria-current`, with text split by markers joined again. Attributes are
/// sorted (`tokens`)
fn without_markers(markup: &str) -> Vec<Tok> {
    let mut out: Vec<Tok> = Vec::new();
    for token in tokens(markup) {
        match token {
            Tok::Comment(_) => {}
            Tok::Start(name, attrs) => out.push(Tok::Start(
                name,
                attrs
                    .into_iter()
                    .filter(|(k, _)| {
                        k != "data-node-hydration" && k != "aria-current"
                    })
                    .collect(),
            )),
            Tok::Text(text) => match out.last_mut() {
                Some(Tok::Text(before)) => before.push_str(&text),
                _ => out.push(Tok::Text(text)),
            },
            end => out.push(end),
        }
    }
    out
}

/// The router draws each page as its root alone does, but for what its
/// `Link`s add: each link to a page the client draws gets a click
/// listener (`,click:1`), its text a marker, `href` written first, and
/// `aria-current="page"` on a link to the page it is on. Links to pages
/// the server draws are drawn as they were
#[test]
fn the_router_draws_each_page_as_its_root_does() {
    for page in [feed_page(), thread_page(), comment_page()] {
        let routed = ssr::render(base(), page.clone().public()).unwrap().html;
        let alone = pre_render(PageIsland, page.clone());
        println!("ALONE:\n{alone}\nROUTED:\n{routed}\n");
        assert_eq!(without_markers(&routed), without_markers(&alone));
        assert_hydratable(&routed);
        if matches!(page, Page::Comment(_)) {
            continue;
        }
        // The profile link (a page the server draws) is the same `<a>`
        assert!(
            routed.contains(
                r#"<a href="/agora/agent/litmus" data-node-hydration="#
            )
        );
        assert!(
            !routed.contains(
                r#"/agora/agent/litmus" data-node-hydration="8,click"#
            )
        );
    }
    let routed = ssr::render(base(), feed_page().public()).unwrap().html;
    // Sort, the card's title and community, the next page: the router's
    assert_eq!(routed.matches(",click:1\"").count(), 6, "{routed}");
    assert_eq!(routed.matches("aria-current").count(), 1);
    assert!(
        routed.contains(
            r#"<a href="/agora/feed?sort=date" class="pill pill-btn active" aria-current="page""#
        ),
        "{routed}"
    );
}

/// The data block is the page's, stamped with this agentkit, and reads
/// back as the page: a client of another agentkit refuses it
#[test]
fn the_data_block_is_the_page_and_its_version() {
    let rendered = ssr::render(base(), thread_page().public()).unwrap();
    let text = rendered
        .script
        .strip_prefix(r#"<script type="application/json" id="agora-app-data">"#)
        .and_then(|s| s.strip_suffix("</script>"))
        .unwrap();
    let back: Page = island::from_script_text(text).unwrap();
    assert_eq!(back, thread_page().public().into_inner());
    let other = text.replace(island::AGENTKIT_VERSION, "0.0.1");
    assert!(matches!(
        island::from_script_text::<Page>(&other),
        Err(island::IslandError::Version { .. })
    ));
}

/// The base is the site's: another would point every link elsewhere
#[test]
fn another_base_is_refused() {
    for other in ["", "/", "/agora/", "/elsewhere"] {
        assert!(
            matches!(
                ssr::render(other, feed_page().public()),
                Err(RouteError::Base { .. })
            ),
            "{other:?}"
        );
    }
    assert_eq!(base(), "/agora");
}

/// Every page in every state reads back as drawn through the router
#[test]
fn every_routed_page_reparses_as_drawn() {
    let community = GetFeedInput {
        community: Some("meta-governance".parse().unwrap()),
        ..GetFeedInput::default()
    };
    for page in [
        feed_page(),
        thread_page(),
        FeedPage::new(GetFeedInput::default(), FeedLoad::Failed("<x>".into()))
            .into(),
        FeedPage::new(community.clone(), [fixtures::post()].iter().collect())
            .with_community(fixtures::community())
            .into(),
        ThreadPage::new(fixtures::post().id, ThreadLoad::Failed("<x>".into()))
            .into(),
        comment_page(),
        CommentPage::new(
            fixtures::comment().id,
            CommentLoad::Failed("<x>".into()),
        )
        .into(),
    ] {
        assert_hydratable(&ssr::render(base(), page.public()).unwrap().html);
    }
}

// -- Links outside and inside a router -----------------------------------------------

#[component]
fn Links(links: Vec<AgoraLink>) -> Element {
    rsx! {
        for link in links {
            To { link, class: "c", "{0}" }
        }
    }
}

/// `links` drawn with no router, and under one at `/feed`
fn links_both_ways(links: Vec<AgoraLink>) -> (String, String) {
    let alone = pre_render(
        Links,
        LinksProps {
            links: links.clone(),
        },
    );
    #[component]
    fn Routed(links: Vec<AgoraLink>) -> Element {
        // A router whose only page draws the links
        #[derive(Clone, PartialEq, Debug)]
        struct Only;
        impl std::str::FromStr for Only {
            type Err = String;
            fn from_str(_: &str) -> Result<Self, String> {
                Ok(Only)
            }
        }
        impl fmt::Display for Only {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("/feed")
            }
        }
        impl Routable for Only {
            const SITE_MAP: &'static [SiteMapSegment] = &[];
            fn render(&self, _: usize) -> Element {
                let links = use_context::<Vec<AgoraLink>>();
                rsx! { Links { links } }
            }
        }
        use_context_provider(|| links);
        rsx! { Router::<Only> {} }
    }
    let history: Rc<dyn History> =
        Rc::new(MemoryHistory::with_initial_path("/feed").with_prefix(base()));
    let mut dom = VirtualDom::new_with_props(Routed, RoutedProps { links })
        .with_root_context(history);
    dom.rebuild_in_place();
    (alone, dioxus_ssr::pre_render(&dom))
}

/// With no router a link is a plain `<a>` (the router's `Link` would
/// panic); under one, a link to a page the client draws is the router's,
/// and any other stays a plain `<a>`, a full page load
#[test]
fn a_link_is_the_routers_only_under_a_router_and_to_a_client_page() {
    let client = AgoraLink::post(fixtures::post().id);
    let server = link_at("/agent/litmus");
    let page = link_at("/page/about");
    let (alone, routed) =
        links_both_ways(vec![client.clone(), server.clone(), page.clone()]);
    println!("ALONE:  {alone}\nROUTED: {routed}");
    for link in [&client, &server, &page] {
        assert!(
            alone.contains(&format!(
                r#"<a class="c" href="{}""#,
                link.site_path()
            )),
            "{alone}"
        );
    }
    assert!(!alone.contains("click"), "{alone}");
    assert!(
        routed.contains(&format!(
            r#"<a href="{}" class="c" data-node-hydration="0,click:1""#,
            client.site_path()
        )),
        "{routed}"
    );
    for link in [&server, &page] {
        let plain = format!(
            r#"<a class="c" href="{}" data-node-hydration="#,
            link.site_path()
        );
        let at = routed.find(&plain).unwrap_or_else(|| panic!("{routed}"));
        let marker = &routed[at + plain.len()..];
        assert!(!marker[..marker.find('"').unwrap() + 1].contains("click"));
    }
}

// -- The client: hydration, navigation, loads, titles ----------------------------

/// A history that writes down what is done to it, over a memory history:
/// the browser's, as far as the router can tell
struct Recorder {
    inner: MemoryHistory,
    log: Rc<RefCell<Vec<String>>>,
}

impl History for Recorder {
    fn current_route(&self) -> String {
        self.inner.current_route()
    }
    fn current_prefix(&self) -> Option<String> {
        self.inner.current_prefix()
    }
    fn go_back(&self) {
        self.log.borrow_mut().push("back".into());
        self.inner.go_back()
    }
    fn go_forward(&self) {
        self.log.borrow_mut().push("forward".into());
        self.inner.go_forward()
    }
    fn push(&self, route: String) {
        self.log.borrow_mut().push(format!("push {route}"));
        self.inner.push(route)
    }
    fn replace(&self, path: String) {
        self.log.borrow_mut().push(format!("replace {path}"));
        self.inner.replace(path)
    }
    fn external(&self, url: String) -> bool {
        self.log.borrow_mut().push(format!("external {url}"));
        true
    }
}

/// The client's API, counting its reads
#[derive(Clone, Default)]
struct Api {
    reads: Rc<RefCell<Vec<String>>>,
    /// What a thread's read answers; loaded when `None`
    thread: Rc<RefCell<Option<ThreadLoad>>>,
    /// What a comment's read answers; loaded when `None`
    comment: Rc<RefCell<Option<CommentLoad>>>,
}

fn post_titled(title: &str) -> PostResponse {
    PostResponse {
        title: title.into(),
        body: format!("The body of {title}."),
        ..fixtures::post()
    }
}

impl Fetch for Api {
    fn feed(&self, input: GetFeedInput) -> LocalBoxFuture<'static, FeedPage> {
        self.reads
            .borrow_mut()
            .push(format!("feed {}", Route(AgoraLink::Feed(input.clone()))));
        Box::pin(async move {
            let page = FeedPage::new(
                input.clone(),
                [post_titled("Fetched")].iter().collect(),
            );
            match &input.community {
                Some(name) if name == "nowhere" => {
                    FeedPage::new(input, FeedLoad::NotFound)
                }
                Some(_) => page.with_community(fixtures::community()),
                None => page,
            }
        })
    }

    fn comment(
        &self,
        comment: CommentId,
    ) -> LocalBoxFuture<'static, CommentLoad> {
        self.reads.borrow_mut().push(format!("comment {comment}"));
        let answer = self.comment.borrow().clone();
        Box::pin(async move {
            answer.unwrap_or_else(|| {
                let mut read = fixtures::comment_chain();
                read.chain[0].id = comment;
                read.chain[0].body = "A fetched comment.".into();
                CommentLoad::Loaded(Box::new(read))
            })
        })
    }

    fn thread(&self, post: PostId) -> LocalBoxFuture<'static, ThreadLoad> {
        self.reads.borrow_mut().push(format!("thread {post}"));
        let answer = self.thread.borrow().clone();
        Box::pin(async move {
            answer.unwrap_or_else(|| {
                let mut read = fixtures::post_with_comments();
                read.post = post_titled("A fetched post");
                read.post.body =
                    format!("{} [The feed]({OWN_FEED})", read.post.body);
                ThreadLoad::Loaded(Box::new(read))
            })
        })
    }
}

/// A client's site: the router over a recorded history at `address` (as
/// the browser has it, fragment and all), the embedded page, the API,
/// a title hook, and a session slot that keeps the scope of the page
/// it is in (to navigate from, as a `Link`'s handler does)
struct Client {
    dom: VirtualDom,
    log: Rc<RefCell<Vec<String>>>,
    api: Api,
    titles: Rc<RefCell<Vec<String>>>,
}

thread_local! {
    static PAGE_SCOPE: Cell<Option<ScopeId>> = const { Cell::new(None) };
}

#[allow(non_snake_case)]
fn Session() -> Element {
    use_hook(|| PAGE_SCOPE.set(Some(dioxus::dioxus_core::current_scope_id())));
    rsx! { span { "signed out" } }
}

impl Client {
    fn new(address: &str, first: Page) -> Self {
        Self::with_data(address, |api| PageData::client(first, api))
    }

    fn with_data(address: &str, data: impl FnOnce(Api) -> PageData) -> Self {
        let log = Rc::new(RefCell::new(Vec::new()));
        let history: Rc<dyn History> = Rc::new(Recorder {
            inner: MemoryHistory::with_initial_path(address)
                .with_prefix(base()),
            log: log.clone(),
        });
        let api = Api::default();
        let titles = Rc::new(RefCell::new(Vec::new()));
        let seen = titles.clone();
        let mut dom = VirtualDom::new(SiteRoot)
            .with_root_context(history)
            .with_root_context(data(api.clone()))
            .with_root_context(
                ClientSlots::new().with(ClientSlot::Session, Session),
            )
            .with_root_context(DocumentTitle::new(move |t| {
                seen.borrow_mut().push(t.to_string())
            }));
        dom.rebuild_in_place();
        Self {
            dom,
            log,
            api,
            titles,
        }
    }

    fn html(&self) -> String {
        dioxus_ssr::pre_render(&self.dom)
    }

    /// One render, no effects or loads
    fn render(&mut self) -> String {
        self.dom
            .render_immediate(&mut dioxus::dioxus_core::NoOpMutations);
        self.html()
    }

    /// Effects and loads, until `done` or a budget runs out
    async fn settle(&mut self, done: impl Fn(&str) -> bool) -> String {
        for _ in 0..30 {
            let _ = tokio::time::timeout(
                std::time::Duration::from_millis(10),
                self.dom.wait_for_work(),
            )
            .await;
            self.dom
                .render_immediate(&mut dioxus::dioxus_core::NoOpMutations);
            let html = self.html();
            if done(&html) {
                return html;
            }
        }
        self.html()
    }

    /// What a `Link`'s click does: push the route, from inside the page
    fn go(&mut self, link: AgoraLink) {
        let scope = PAGE_SCOPE.get().expect("a page drew its session slot");
        self.dom.in_scope(scope, || {
            navigator().push(Route(link));
        });
    }

    fn back(&mut self) {
        let scope = PAGE_SCOPE.get().expect("a page drew its session slot");
        self.dom.in_scope(scope, || navigator().go_back());
    }

    fn reads(&self) -> Vec<String> {
        self.api.reads.borrow().clone()
    }

    fn log(&self) -> Vec<String> {
        self.log.borrow().clone()
    }
}

/// The client's first render is the server's, from the same page and the
/// same route, whatever the address bar holds: no load, no rewrite of the
/// address, its fragment kept (and so the scroll the browser gave it)
#[tokio::test]
async fn the_client_hydrates_the_servers_page_and_keeps_the_address() {
    for (page, addresses) in [
        (feed_page(), vec!["/feed?sort=date", "/feed?sort=date#far"]),
        (
            FeedPage::new(
                GetFeedInput {
                    sort: Some(FeedSort::Active),
                    offset: Some(25),
                    ..GetFeedInput::default()
                },
                [fixtures::post()].iter().collect(),
            )
            .into(),
            // Not canonical: options in another order
            vec![
                "/feed?sort=active&offset=25",
                "/feed?sort=active&offset=25#far",
            ],
        ),
        (
            thread_page(),
            vec![
                format!("/post/{POST}").leak() as &str,
                format!("/post/{POST}#comment-{POST}").leak(),
            ],
        ),
        (
            comment_page(),
            vec![format!("/comment/{COMMENT}").leak() as &str],
        ),
    ] {
        let server = ssr::render(base(), page.clone().public()).unwrap().html;
        for address in addresses {
            let mut client = Client::new(address, page.clone());
            assert_eq!(client.html(), server, "{address}");
            // After mount: only the session slot is filled
            let html = client.settle(|_| false).await.replace(
                r#"<span data-node-hydration="0">signed out</span>"#,
                "<!--placeholder0-->",
            );
            assert_eq!(
                html, server,
                "{address}: nothing else changes after mount"
            );
            assert!(
                client.reads().is_empty(),
                "{address}: {:?}",
                client.reads()
            );
            assert!(client.log().is_empty(), "{address}: {:?}", client.log());
            assert_eq!(
                *client.titles.borrow(),
                vec![page.document_title()],
                "{address}"
            );
        }
    }
}

/// A click on a client page's link: one read, the page drawn from it, the
/// title after it; back: the last page again, read again (nothing is
/// cached)
#[tokio::test]
async fn a_navigation_loads_the_page_and_back_loads_it_again() {
    let mut client = Client::new("/feed?sort=date", feed_page());
    client.settle(|_| false).await;
    let post = AgoraLink::post(fixtures::post().id);
    client.go(post.clone());
    let html = client.render();
    assert!(html.contains("Loading the thread…"), "{html}");
    let html = client.settle(|_| false).await;
    assert!(html.contains("The body of A fetched post"), "{html}");
    assert_eq!(
        client.reads(),
        vec![format!("thread {}", fixtures::post().id)]
    );
    assert_eq!(client.log(), vec![format!("push {}", Route(post))]);
    assert_eq!(
        client.titles.borrow().last().unwrap(),
        "A fetched post — Agora"
    );

    client.back();
    let html = client.settle(|_| false).await;
    assert!(html.contains(">Fetched<"), "{html}");
    assert_eq!(client.reads().len(), 2);
    assert_eq!(client.reads()[1], "feed /feed?sort=date");
    assert_eq!(client.titles.borrow().last().unwrap(), "All Posts — Agora");
}

/// A community's page after a navigation: its name while it loads, its
/// header once loaded, and its title
#[tokio::test]
async fn a_community_after_a_navigation_has_its_header() {
    let mut client = Client::new("/feed?sort=date", feed_page());
    client.settle(|_| false).await;
    let community = link_at("/community/meta-governance");
    client.go(community);
    let html = client.render();
    assert!(
        html.contains("<h1><!--node-id3-->meta-governance<!--#--></h1>"),
        "{html}"
    );
    assert!(html.contains("Loading posts…"), "{html}");
    assert!(!html.contains("All Posts</h1>"), "{html}");
    let html = client.settle(|_| false).await;
    let display = fixtures::community().display_name;
    assert!(html.contains(&display), "{html}");
    assert_eq!(
        client.titles.borrow().last().unwrap(),
        &format!("{display} — Agora")
    );
}

/// A failed load says why and offers Try again; a post or community that
/// isn't there says so, with a way on; neither is a blank page
#[tokio::test]
async fn a_failed_or_missing_page_says_so() {
    let mut client = Client::new("/feed?sort=date", feed_page());
    client.settle(|_| false).await;
    *client.api.thread.borrow_mut() =
        Some(ThreadLoad::Failed("Agora couldn't be reached.".into()));
    client.go(AgoraLink::post(fixtures::post().id));
    let html = client.settle(|_| false).await;
    println!("FAILED: {html}");
    assert!(html.contains("The thread couldn&#39;t be loaded"), "{html}");
    assert!(html.contains(r#"role="alert""#), "{html}");
    assert!(html.contains("Try again"), "{html}");
    assert_eq!(client.titles.borrow().last().unwrap(), "A thread — Agora");

    *client.api.thread.borrow_mut() = Some(ThreadLoad::NotFound);
    let other: PostId = "5b1ce000-0000-4000-8000-000000000001".parse().unwrap();
    client.go(AgoraLink::post(other));
    let html = client.settle(|_| false).await;
    println!("MISSING: {html}");
    assert!(
        html.contains(&POST_NOT_FOUND.replace('\'', "&#39;")),
        "{html}"
    );
    assert!(html.contains(r#"href="/agora/feed""#), "{html}");
    assert!(!html.contains("Try again"), "{html}");
    assert_eq!(client.titles.borrow().last().unwrap(), "Not found — Agora");

    client.go(link_at("/community/nowhere"));
    let html = client.settle(|_| false).await;
    assert!(
        html.contains(&COMMUNITY_NOT_FOUND.replace('\'', "&#39;")),
        "{html}"
    );
    assert!(!html.contains("All Posts</h1>"), "{html}");
}

/// The router's own rewrite of the address (to `Display`) never happens:
/// a replace with the page it is on is dropped, another goes through
#[tokio::test]
async fn the_address_is_rewritten_only_to_another_page() {
    let mut client = Client::new("/feed?sort=active&offset=0#far", feed_page());
    client.settle(|_| false).await;
    assert!(client.log().is_empty(), "{:?}", client.log());
    let scope = PAGE_SCOPE.get().unwrap();
    client.dom.in_scope(scope, || {
        navigator().replace(link_route("/feed?offset=0&sort=active"));
    });
    assert!(client.log().is_empty(), "{:?}", client.log());
    client.dom.in_scope(scope, || {
        navigator().replace(link_route("/feed?sort=date"));
    });
    assert_eq!(client.log(), vec!["replace /feed?sort=date".to_string()]);
}

fn link_route(path: &str) -> Route {
    path.parse().unwrap()
}

/// A push to a page the router doesn't draw is a full page load, never a
/// route it would draw as `/`
#[tokio::test]
async fn a_push_to_a_server_page_loads_it_from_the_server() {
    let mut client = Client::new("/feed?sort=date", feed_page());
    client.settle(|_| false).await;
    let scope = PAGE_SCOPE.get().unwrap();
    client.dom.in_scope(scope, || {
        navigator().push(Route(link_at("/agent/litmus")));
    });
    assert_eq!(
        client.log(),
        vec!["external /agora/agent/litmus".to_string()]
    );
}

/// The router at an address it doesn't draw (a history entry made outside
/// it; the server never serves one): the page is loaded from the server at
/// the address as it is, which is never rewritten (to `/feed`, or `/`)
#[tokio::test]
async fn an_address_the_router_refuses_is_loaded_from_the_server() {
    for address in ["/nope#x", "/agent/litmus", "/post/not-an-id"] {
        let mut client = Client::new(address, feed_page());
        let html = client.settle(|_| false).await;
        println!("{address}: {html}\n{:?}", client.log());
        let full = format!("/agora{address}");
        assert!(html.contains(&format!(r#"href="{full}""#)), "{html}");
        assert_eq!(client.log(), vec![format!("external {full}")], "{address}");
        assert!(!html.contains("Fetched"), "{html}");
        assert!(client.reads().is_empty(), "{address}: {:?}", client.reads());
    }
}

/// An absolute link to the site's own feed, in a body
const OWN_FEED: &str = "https://subliminal.technology/agora/feed";

fn site() -> url::Url {
    url::Url::parse("https://subliminal.technology").unwrap()
}

/// A thread on a site, drawn through the router: a body's absolute link to
/// the site gets the site's `rel`, as the thread's root alone draws it, and
/// the client's first render is the server's
#[tokio::test]
async fn a_thread_on_its_site_keeps_its_links_rel_under_the_router() {
    use crate::ui::markdown::{LINK_REL, SITE_LINK_REL};
    let mut read = fixtures::post_with_comments();
    read.post.body = format!("[The feed]({OWN_FEED})");
    let page: Page = ThreadPage::loaded(read).at(&site()).into();
    let routed = ssr::render(base(), page.clone().public()).unwrap().html;
    let ours = format!("rel=\"{SITE_LINK_REL}\"");
    assert!(routed.contains(&ours), "{routed}");
    assert!(!routed.contains(LINK_REL), "{routed}");
    assert_eq!(
        without_markers(&routed),
        without_markers(&pre_render(PageIsland, page.clone()))
    );
    let client = Client::new(&format!("/post/{POST}"), page);
    assert_eq!(client.html(), routed);
}

/// After a navigation, a thread the client loads is on the site the first
/// page named, or the one the client gave (`PageData::at`); with neither,
/// its links get the plain `rel`
#[tokio::test]
async fn a_thread_loaded_after_a_navigation_is_on_the_site() {
    use crate::ui::markdown::{LINK_REL, SITE_LINK_REL};
    for (given, rel) in [(true, SITE_LINK_REL), (false, LINK_REL)] {
        let mut client = Client::with_data("/feed?sort=date", |api| {
            let data = PageData::client(feed_page(), api);
            if given { data.at(&site()) } else { data }
        });
        client.settle(|_| false).await;
        client.go(AgoraLink::post(fixtures::post().id));
        let html = client.settle(|_| false).await;
        assert!(html.contains("The body of A fetched post"), "{html}");
        assert!(html.contains(&format!("rel=\"{rel}\"")), "{given}: {html}");
    }
}

/// The router reads a path strictly, as the inverse of `site_path`
/// (`from_site_path`), not as `AgoraLink::parse_at` reads a path an agent
/// wrote (0.169.0): that takes old pages (`/agora/c/<name>`, which the
/// server redirects), `/agora` itself, and any other path as an
/// `agora://page/…`, and needs a site. Those stay the server's
#[test]
fn the_router_reads_paths_strictly() {
    let at = |path: &str| AgoraLink::parse_at(path, Some(&site())).unwrap();
    assert_eq!(
        at("/agora/c/meta-governance"),
        link_at("/community/meta-governance")
    );
    assert!("/c/meta-governance".parse::<Route>().is_err());
    assert!(matches!(at("/agora/about"), AgoraLink::Page(_)));
    assert!(matches!(
        "/about".parse::<Route>(),
        Err(RouteError::NotALink { .. })
    ));
    // A server page's own path still doesn't read back as its link
    let page = at("/agora/about");
    assert_eq!(page.site_path(), "/agora/about");
    assert!(
        AgoraLink::from_site_path(&page.site_path())
            .unwrap()
            .is_err()
    );
    assert_eq!(at("/agora"), Route::root().0);
    assert!(matches!(ssr::route(""), Err(RouteError::NotAPath(_))));
}

/// A stub's "Read the whole comment" is the router's link to the comment's
/// page: a click loads the comment through the client's API (one read),
/// with no page load and no change to the stub's markup outside a router
#[tokio::test]
async fn a_stubs_link_opens_the_comment_client_side() {
    use crate::responses::PostWithCommentsResponse;
    let stub = fixtures::comment_stub();
    let read = PostWithCommentsResponse {
        comment_stubs: vec![stub.clone()],
        ..fixtures::post_with_comments()
    };
    let page: Page = ThreadPage::loaded(read).into();
    let routed = ssr::render(base(), page.clone().public()).unwrap().html;
    let href = format!("/agora/comment/{}", stub.id);
    // Under the router: the router's link, with its click listener
    let at = routed
        .find(&format!(r#"<a href="{href}" data-node-hydration=""#))
        .unwrap_or_else(|| panic!("{routed}"));
    let marker = &routed[at..];
    assert!(
        marker[..marker.find('>').unwrap()].contains(",click:1"),
        "{routed}"
    );
    // Alone: the plain `<a>` it was
    let alone = pre_render(PageIsland, page.clone());
    let plain = format!(r#"<a href="{href}" data-node-hydration=""#);
    let at = alone.find(&plain).unwrap_or_else(|| panic!("{alone}"));
    let marker = &alone[at..];
    assert!(
        !marker[..marker.find('>').unwrap()].contains("click"),
        "{alone}"
    );

    let mut client = Client::new(&format!("/post/{POST}"), page);
    client.settle(|_| false).await;
    client.go(AgoraLink::comment(stub.id));
    let html = client.render();
    assert!(html.contains("Loading the comment…"), "{html}");
    let html = client.settle(|_| false).await;
    assert!(html.contains("A fetched comment."), "{html}");
    assert_eq!(client.reads(), vec![format!("comment {}", stub.id)]);
    assert_eq!(
        client.log(),
        vec![format!("push {}", Route(AgoraLink::comment(stub.id)))]
    );
    assert_eq!(
        client.titles.borrow().last().unwrap(),
        "Comment by quinn on “A title” — Agora"
    );
}

/// A comment that fails to load says so and offers Try again; one that
/// isn't there says so
#[tokio::test]
async fn a_failed_or_missing_comment_says_so() {
    let mut client = Client::new("/feed?sort=date", feed_page());
    client.settle(|_| false).await;
    *client.api.comment.borrow_mut() =
        Some(CommentLoad::Failed("Agora couldn't be reached.".into()));
    client.go(AgoraLink::comment(fixtures::comment().id));
    let html = client.settle(|_| false).await;
    assert!(
        html.contains("The comment couldn&#39;t be loaded"),
        "{html}"
    );
    assert!(html.contains("Try again"), "{html}");
    assert_eq!(client.titles.borrow().last().unwrap(), "A comment — Agora");

    *client.api.comment.borrow_mut() = Some(CommentLoad::NotFound);
    let other: CommentId =
        "c0000009-0000-4000-8000-000000000009".parse().unwrap();
    client.go(AgoraLink::comment(other));
    let html = client.settle(|_| false).await;
    assert!(
        html.contains(&COMMENT_NOT_FOUND.replace('\'', "&#39;")),
        "{html}"
    );
    assert_eq!(client.titles.borrow().last().unwrap(), "Not found — Agora");
}
