use super::*;
use crate::{
    fixtures,
    ui::{feed::FeedIsland, ssr::pre_render, thread::ThreadIsland},
};

fn feed_page() -> Page {
    FeedPage::new(FeedPage::input(), FeedLoad::Loaded(vec![fixtures::post()]))
        .into()
}

fn thread_page() -> Page {
    ThreadPage::loaded(fixtures::post_with_comments()).into()
}

fn post_path() -> String {
    format!("/post/{}", fixtures::post().id)
}

/// Every path round-trips: parse, display, parse again
#[test]
fn paths_round_trip() {
    for path in [
        "/feed",
        "/feed?sort=active&offset=25",
        "/community/meta-governance",
        &post_path(),
        "/gov/GOV-2026-0006",
        "/protocol?lines=412-455",
        "/constitution?version=0.3",
        "/agent/litmus",
        "/search?q=hello",
        "/inbox",
    ] {
        let route: Route =
            path.parse().unwrap_or_else(|e| panic!("{path}: {e}"));
        let shown = route.to_string();
        println!("{path:40} -> {:?} -> {shown}", route.0);
        let again: Route = shown.parse().unwrap();
        assert_eq!(again, route, "{path}");
    }
}

/// `/` is the global feed (the router falls back to `/`); a fragment is
/// dropped; server pages and nonsense are refused
#[test]
fn what_parses_and_what_does_not() {
    assert_eq!(
        "/".parse::<Route>().unwrap(),
        Route(AgoraLink::Feed(GetFeedInput::default()))
    );
    let anchored: Route =
        format!("{}#comment-abc", post_path()).parse().unwrap();
    assert_eq!(anchored.to_string(), post_path());
    for bad in [
        "/about",
        "/account",
        "/nope",
        "/post/not-an-id",
        "/gov/../x",
        "/api/social/feed",
    ] {
        let e = bad.parse::<Route>().unwrap_err();
        println!("{bad:24} -> Err({e})");
    }
}

/// Two paths from one app: the server draws each page's own root through
/// the router, byte for byte as the page's root alone draws it
#[test]
fn the_server_draws_two_paths_from_one_app() {
    let feed = ssr::render("/agora", "/feed", Some(feed_page())).unwrap();
    let thread =
        ssr::render("/agora", &post_path(), Some(thread_page())).unwrap();
    println!("FEED  /agora/feed:\n{}\n", feed.html);
    println!("POST  /agora{}:\n{}\n", post_path(), thread.html);
    assert!(feed.html.contains("All Posts"), "{}", feed.html);
    assert!(feed.html.contains("A title"));
    assert!(thread.html.contains("A body."), "{}", thread.html);
    assert!(!thread.html.contains("All Posts"));
    // Links in the cards are the router's: href with the history's base
    assert!(
        feed.html.contains(&format!(
            r#"href="/agora/post/{}""#,
            fixtures::post().id
        )),
        "{}",
        feed.html
    );
    assert!(feed.script.contains("\"page\":\"feed\""), "{}", feed.script);
}

/// The same app at the root: no base, links without `/agora`
#[test]
fn the_server_draws_at_the_root_too() {
    let feed = ssr::render("", "/feed", Some(feed_page())).unwrap();
    assert!(
        feed.html
            .contains(&format!(r#"href="/post/{}""#, fixtures::post().id)),
        "{}",
        feed.html
    );
    let root = ssr::render("", "/", Some(feed_page())).unwrap();
    assert_eq!(root.html, feed.html);
}

/// A path that is no page is refused before anything is drawn: the
/// handler's 404
#[test]
fn an_unknown_path_is_refused_before_drawing() {
    let e = ssr::render("/agora", "/nope", None).unwrap_err();
    println!("/nope -> {e}");
}

/// Without its data (a client after a navigation), a page starts Pending;
/// with it, the router draws what the page's root draws with the links
/// made the router's: the server's first page has no fetch
#[test]
fn pending_without_data() {
    let feed = ssr::render("/agora", "/feed", None).unwrap();
    assert!(feed.html.contains("Loading posts…"), "{}", feed.html);
    let other =
        ssr::render("/agora", "/feed?sort=active", Some(feed_page())).unwrap();
    // The embedded page is for another input: not used
    assert!(other.html.contains("Loading posts…"), "{}", other.html);
    let thread = ssr::render("/agora", &post_path(), None).unwrap();
    assert!(thread.html.contains("Loading"), "{}", thread.html);
}

/// What the router adds around a page: compared with the page's root
/// drawn alone (no router), the only differences are the links that are
/// the router's
#[test]
fn the_router_adds_only_link_markers() {
    let routed = ssr::render("/agora", "/feed", Some(feed_page()))
        .unwrap()
        .html;
    let Page::Feed(page) = feed_page() else {
        unreachable!()
    };
    let alone = pre_render(FeedIsland, page);
    println!("ALONE:\n{alone}\nROUTED:\n{routed}");
    let Page::Thread(page) = thread_page() else {
        unreachable!()
    };
    let _ = pre_render(ThreadIsland, page);
}

/// What the router itself does with a path that doesn't parse, when no
/// handler refused it first: it throws a `ParseRouteError` and draws `/`
#[test]
fn the_router_alone_with_an_unparseable_path() {
    use dioxus_history::{History, MemoryHistory};
    let history: Rc<dyn History> = Rc::new(
        MemoryHistory::with_initial_path("/nope").with_prefix("/agora"),
    );
    let mut dom = VirtualDom::new(SiteRoot)
        .with_root_context(history)
        .with_root_context(PageData::server(None));
    dom.rebuild_in_place();
    let html = dioxus_ssr::pre_render(&dom);
    println!("ROUTER AT /nope:\n{html}");
}
