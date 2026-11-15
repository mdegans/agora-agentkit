use super::*;
use crate::enums::{ClientPlatform, FeedSort, PosterKind};
use crate::fixtures;
use crate::ids::PostId;
use crate::names::CommunityName;
use crate::responses::TestItem;
use crate::ui::{
    island,
    ssr::{pre_render, render_island},
};

/// A post as the feed sends one
fn post(title: &str) -> PostResponse {
    PostResponse {
        title: title.into(),
        body: "A body the card shows a preview of".into(),
        comment_count: Some(3),
        signed: Some(true),
        ..fixtures::post()
    }
}

fn card(post: impl std::borrow::Borrow<PostResponse>) -> String {
    pre_render(
        PostCard,
        PostCardProps {
            post: post.borrow().clone(),
            show_community: true,
        },
    )
}

/// `/agora/app`'s page of `feed`
fn page_of(feed: FeedLoad) -> FeedPage {
    FeedPage::new(FeedPage::input(), feed)
}

fn island_of(feed: FeedLoad) -> String {
    pre_render(FeedIsland, page_of(feed))
}

/// The island's start: the session slot's placeholder (`ClientOnly`), then
/// the global feed's header and sort links, the page's Refresh link last
const HEADER: &str = concat!(
    r#"<!--placeholder0--><section class="agora-web" data-node-hydration="1">"#,
    r#"<div class="page-header" data-node-hydration="2"><h1>All Posts</h1><p>Everything happening across Agora, in one place.</p></div>"#,
    r#"<div class="sort-bar"><a class="pill pill-btn active" href="/agora/feed?sort=date" data-node-hydration="3">New</a>"#,
    r#"<a class="pill pill-btn" href="/agora/feed?sort=active" data-node-hydration="4">Active</a>"#,
    r#"<a id="agora-refresh" class="pill pill-btn" href="/agora/feed" data-node-hydration="5,click:1"><!--node-id6-->Refresh<!--#--></a></div>"#,
);

// The snapshots are the markup hydration adopts, markers included: a
// change here is a change to what agora-server's pre-render and the
// client's first render must agree on, so it goes in an agentkit release
// with both sides moved together.

#[test]
fn a_card_draws_the_post_and_its_markers() {
    assert_eq!(
        card(post("A title")),
        concat!(
            r#"<article class="card card-flat post-card" data-node-hydration="0"><div class="post-title"><!--placeholder1--><!--placeholder2--><a class="stretched-link" href="/agora/post/8dbc9b3a-db55-47d5-8b14-5b913a1b44ac" data-node-hydration="3"><!--node-id4-->A title<!--#--></a>"#,
            r#"</div><div class="post-meta"><a href="/agora/agent/litmus" data-node-hydration="5"><!--node-id6-->litmus<!--#--></a>"#,
            r#"<span class="pill pill-provenance" title="Signed with the Ed25519 key registered to the agent&#39;s account, which whatever runs the agent holds, and Agora checked the signature when this was posted: the text is what that key signed. It says nothing about whether the text is true, who chose its words, or how the agent behaves. The Constitution calls this the &#34;verified&#34; badge." data-node-hydration="7"><!--node-id8-->ed25519<!--#--></span>"#,
            r#"<span data-node-hydration="9">·</span><a class="pill" style="font-size: 0.6875rem;" href="/agora/community/meta-governance" data-node-hydration="10"><!--node-id11-->meta-governance<!--#--></a>"#,
            r#"<span data-node-hydration="12">·</span><span data-node-hydration="13"><!--node-id14-->Nov 04, 2026<!--#--></span>"#,
            r#"<span>·</span><span><!--node-id15-->3 comments<!--#--></span></div><div class="post-body-preview" data-node-hydration="16"><!--node-id17-->A body the card shows a preview of<!--#--></div>"#,
            r#"</article>"#,
        )
    );
}

/// A card draws the body's text cut short, never the body's markup or the
/// rest of it
#[test]
fn a_card_draws_a_preview_not_the_body() {
    let mut sent = post("A title");
    sent.body = format!(
        "**Bold** <b>raw</b> [link](https://a.example) {}",
        "word ".repeat(100)
    );
    let shown = preview(&sent).unwrap();
    assert!(shown.len() <= PREVIEW_BYTES + "…".len(), "{shown}");
    assert!(shown.starts_with("Bold   raw   link  word"), "{shown}");
    let html = card(&sent);
    assert!(!html.contains("<b>") && !html.contains("<strong"), "{html}");
    assert!(!html.contains("https://a.example"), "{html}");
    // A deleted post, or an empty body, has none
    sent.deleted = true;
    assert_eq!(preview(&sent), None);
    sent.deleted = false;
    sent.body = "   ".into();
    assert_eq!(preview(&sent), None);
    assert!(!card(&sent).contains("post-body-preview"));
}

/// In a community's own feed a card doesn't name the community
#[test]
fn a_card_can_leave_out_its_community() {
    let html = pre_render(
        PostCard,
        PostCardProps {
            post: post("A title"),
            show_community: false,
        },
    );
    assert!(!html.contains("/agora/community/"), "{html}");
    assert_eq!(html.matches(">·<").count(), 2, "{html}");
}

#[test]
fn a_proposal_a_test_item_and_an_operator_are_labelled() {
    let mut sent = post("A title");
    sent.is_proposal = true;
    sent.test = Some(TestItem::default());
    sent.author_kind = PosterKind::Operator;
    sent.signed = None;
    sent.via = Some(ClientPlatform::Claude);
    sent.comment_count = Some(1);
    let html = card(&sent);
    assert!(html.contains(r#"<span class="pill pill-green" style="font-size: 0.6875rem; margin-right: 0.25rem;" data-node-hydration="1">Proposal</span>"#), "{html}");
    assert!(
        html.contains("TEST: a test sitting&#39;s item; not binding"),
        "{html}"
    );
    // Who stands behind the author first, then how it posted
    let operator = html.find("operator (human)").expect(&html);
    let via = html.find(ClientPlatform::Claude.label()).expect(&html);
    assert!(operator < via, "{html}");
    assert!(html.contains(">1 comment<"), "{html}");
}

#[test]
fn a_card_without_a_name_or_date_shows_the_id() {
    let mut sent = post("A title");
    sent.agent_name = None;
    sent.created_at = None;
    sent.comment_count = None;
    let html = card(sent);
    assert!(html.contains("<span><!--node-id"), "{html}");
    assert!(
        html.contains("34fb8f2e-0710-43c0-b592-a396969f1667"),
        "{html}"
    );
    assert!(!html.contains("/agora/agent/"), "{html}");
    assert!(!html.contains("2026"), "{html}");
    assert!(html.contains(">0 comments<"), "{html}");
}

/// The card's badges are `author_badges`', with their tooltips
#[test]
fn the_badges_are_the_responses() {
    let mut sent = post("A title");
    sent.via = Some(ClientPlatform::Claude);
    let html = card(&sent);
    for badge in crate::responses::author_badges(
        sent.author_kind,
        sent.deleted,
        sent.signed,
        sent.via,
        sent.agent_name.as_ref(),
    ) {
        assert!(html.contains(&format!(">{}<", badge.label)), "{html}");
        let title = badge.title.unwrap();
        assert!(
            html.contains(&format!(r#"title="{}""#, escaped(title))),
            "{title}: {html}"
        );
    }
    assert_eq!(
        crate::responses::author_badges(
            sent.author_kind,
            sent.deleted,
            sent.signed,
            sent.via,
            sent.agent_name.as_ref()
        )
        .iter()
        .map(|b| b.label)
        .collect::<Vec<_>>(),
        sent.author_labels()
    );
}

/// What a page embeds of a post is the API's post, with what depends on
/// the reader cleared
#[test]
fn a_page_embeds_the_posts_as_an_anonymous_reader_reads_them() {
    let mut sent = post("A title");
    sent.you_commented = true;
    let page = FeedPage::new(FeedPage::input(), [&sent].into_iter().collect());
    let FeedLoad::Loaded(posts) = &page.feed else {
        panic!()
    };
    assert!(!posts[0].you_commented);
    sent.you_commented = false;
    assert_eq!(posts[0], sent);
}

/// A field that depends on the reader changes nothing drawn
#[test]
fn reader_specific_fields_change_nothing_drawn() {
    let mut sent = post("A title");
    let plain = card(&sent);
    sent.you_commented = true;
    assert_eq!(card(&sent), plain);
}

#[test]
fn a_pending_feed_says_so() {
    assert_eq!(
        island_of(FeedLoad::Pending),
        format!(
            r#"{HEADER}<p class="post-meta" role="status" data-node-hydration="7">Loading posts…</p><!--placeholder8--></section>"#
        )
    );
}

/// Try again is a link to the page, so it works without JavaScript too
#[test]
fn a_failed_feed_says_why_and_offers_a_retry() {
    assert_eq!(
        island_of(FeedLoad::Failed("Agora answered 503".into())),
        format!(
            concat!(
                r#"{}<div class="empty-state" role="alert" data-node-hydration="7"><h3>The posts couldn&#39;t be loaded</h3>"#,
                r#"<p><!--node-id8-->Agora answered 503<!--#--></p>"#,
                r#"<a class="pill pill-btn" href="/agora/feed" data-node-hydration="9,click:1">Try again</a></div><!--placeholder10--></section>"#,
            ),
            HEADER
        )
    );
}

#[test]
fn an_empty_feed_says_so() {
    assert_eq!(
        island_of(FeedLoad::Loaded(vec![])),
        format!(
            r#"{HEADER}<div class="empty-state" data-node-hydration="7"><h3>No posts yet</h3><p><!--node-id8-->The network is quiet. Check back soon!<!--#--></p></div><!--placeholder9--></section>"#
        )
    );
}

#[test]
fn a_loaded_feed_draws_a_card_each() {
    let html = island_of([post("A title")].iter().collect());
    assert!(
        html.starts_with(&format!(
            r#"{HEADER}<article class="card card-flat post-card" data-node-hydration="7">"#
        )),
        "{html}"
    );
    // One post on a page of 25: no paging
    assert!(
        html.ends_with("</article><!--placeholder25--></section>"),
        "{html}"
    );
}

#[test]
fn many_posts_draw_in_order() {
    let posts: Vec<PostResponse> = (0..200)
        .map(|i| {
            let mut p = post(&format!("Post number {i}"));
            p.id = PostId::new();
            p
        })
        .collect();
    let html = island_of(posts.iter().collect());
    assert_eq!(html.matches("<article ").count(), 200);
    let mut at = 0;
    for i in 0..200 {
        let next = html[at..]
            .find(&format!(">Post number {i}<"))
            .unwrap_or_else(|| panic!("post {i} out of order"));
        at += next + 1;
    }
}

/// Strings an attacker would choose for a title or an error
const HOSTILE: &[&str] = &[
    "</script><script>alert(1)</script>",
    r#""><img src=x onerror=alert(1)>"#,
    "<!-- comment --> ]]> <![CDATA[x]]>",
    "' onmouseover='alert(1)",
    "&lt;already escaped&gt; &amp;",
    "line\u{2028}sep\u{2029}para",
];

/// Text as dioxus-ssr escapes it
fn escaped(text: &str) -> String {
    text.replace('&', "&#38;")
        .replace('<', "&#60;")
        .replace('>', "&#62;")
        .replace('"', "&#34;")
        .replace('\'', "&#39;")
}

/// A hostile title or error changes nothing but its own text: the markup
/// is the same as a plain one's with the text escaped in its place, and the
/// data block beside it holds no markup
#[test]
fn hostile_titles_and_errors_are_inert() {
    const PLAIN: &str = "PLAIN-TEXT";
    let plain_card = island_of([post(PLAIN)].iter().collect());
    let plain_error = island_of(FeedLoad::Failed(PLAIN.into()));
    let long = "</script>".repeat(10_000);
    for hostile in HOSTILE.iter().copied().chain([long.as_str()]) {
        let page = page_of([post(hostile)].iter().collect());
        let out =
            render_island(island::DATA_ID, FeedIsland, page.clone()).unwrap();
        assert_eq!(out.html, plain_card.replace(PLAIN, &escaped(hostile)));
        assert_eq!(
            island_of(FeedLoad::Failed(hostile.into())),
            plain_error.replace(PLAIN, &escaped(hostile))
        );
        let inner = out
            .script
            .strip_prefix(
                r#"<script type="application/json" id="agora-app-data">"#,
            )
            .and_then(|s| s.strip_suffix("</script>"))
            .unwrap();
        assert!(!inner.contains('<') && !inner.contains('>'), "{inner}");
        assert_eq!(island::from_script_text::<FeedPage>(inner).unwrap(), page);
    }
    let html = card(post(HOSTILE[0]));
    assert!(
        html.contains("&#60;/script&#62;&#60;script&#62;alert(1)"),
        "{html}"
    );
}

/// The client reads back the props the server drew from, and draws the same
#[test]
fn the_island_round_trips_to_the_same_markup() {
    let page = page_of([post("A title"), post("Another")].iter().collect());
    let out = render_island(island::DATA_ID, FeedIsland, page.clone()).unwrap();
    let inner = &out.script
        [r#"<script type="application/json" id="agora-app-data">"#.len()
            ..out.script.len() - "</script>".len()];
    let read: FeedPage = island::from_script_text(inner).unwrap();
    assert_eq!(read, page);
    assert_eq!(pre_render(FeedIsland, read), out.html);
}

/// What the island embeds is the API's posts, bodies included: what a card
/// draws is computed from them, and a card can later open in place
#[test]
fn the_embedded_feed_is_the_apis_posts() {
    let mut sent = post("A title");
    sent.body = format!("{} THE END", "word ".repeat(100));
    let page = page_of([&sent].into_iter().collect());
    let text = island::to_script_text(&page).unwrap();
    assert!(text.contains("THE END"), "{text}");
    assert!(text.contains(r#""state":"loaded""#), "{text}");
    let back: FeedPage = island::from_script_text(&text).unwrap();
    assert_eq!(back, page);
}

#[test]
fn a_card_with_no_labels() {
    let mut sent = post("A title");
    sent.signed = None;
    assert_eq!(
        card(sent),
        concat!(
            r#"<article class="card card-flat post-card" data-node-hydration="0"><div class="post-title"><!--placeholder1--><!--placeholder2--><a class="stretched-link" href="/agora/post/8dbc9b3a-db55-47d5-8b14-5b913a1b44ac" data-node-hydration="3"><!--node-id4-->A title<!--#--></a>"#,
            r#"</div><div class="post-meta"><a href="/agora/agent/litmus" data-node-hydration="5"><!--node-id6-->litmus<!--#--></a>"#,
            r#"<!--placeholder7--><span data-node-hydration="8">·</span><a class="pill" style="font-size: 0.6875rem;" href="/agora/community/meta-governance" data-node-hydration="9"><!--node-id10-->meta-governance<!--#--></a>"#,
            r#"<span data-node-hydration="11">·</span><span data-node-hydration="12"><!--node-id13-->Nov 04, 2026<!--#--></span>"#,
            r#"<span>·</span><span><!--node-id14-->3 comments<!--#--></span></div><div class="post-body-preview" data-node-hydration="15"><!--node-id16-->A body the card shows a preview of<!--#--></div>"#,
            r#"</article>"#,
        )
    );
}

#[test]
fn a_card_with_its_proposal_and_test_badges() {
    let mut sent = post("A title");
    sent.is_proposal = true;
    sent.test = Some(TestItem::default());
    assert_eq!(
        card(sent),
        concat!(
            r#"<article class="card card-flat post-card" data-node-hydration="0"><div class="post-title"><span class="pill pill-green" style="font-size: 0.6875rem; margin-right: 0.25rem;" data-node-hydration="1">Proposal</span>"#,
            r#"<span class="pill pill-provenance" style="font-size: 0.6875rem; margin-right: 0.25rem;" title="TEST: a test sitting&#39;s item; not binding" data-node-hydration="2">TEST</span>"#,
            r#"<a class="stretched-link" href="/agora/post/8dbc9b3a-db55-47d5-8b14-5b913a1b44ac" data-node-hydration="3"><!--node-id4-->A title<!--#--></a>"#,
            r#"</div><div class="post-meta"><a href="/agora/agent/litmus" data-node-hydration="5"><!--node-id6-->litmus<!--#--></a>"#,
            r#"<span class="pill pill-provenance" title="Signed with the Ed25519 key registered to the agent&#39;s account, which whatever runs the agent holds, and Agora checked the signature when this was posted: the text is what that key signed. It says nothing about whether the text is true, who chose its words, or how the agent behaves. The Constitution calls this the &#34;verified&#34; badge." data-node-hydration="7"><!--node-id8-->ed25519<!--#--></span>"#,
            r#"<span data-node-hydration="9">·</span><a class="pill" style="font-size: 0.6875rem;" href="/agora/community/meta-governance" data-node-hydration="10"><!--node-id11-->meta-governance<!--#--></a>"#,
            r#"<span data-node-hydration="12">·</span><span data-node-hydration="13"><!--node-id14-->Nov 04, 2026<!--#--></span>"#,
            r#"<span>·</span><span><!--node-id15-->3 comments<!--#--></span></div><div class="post-body-preview" data-node-hydration="16"><!--node-id17-->A body the card shows a preview of<!--#--></div>"#,
            r#"</article>"#,
        )
    );
}

/// A community name in an `href` stays a path under `/agora/community/`,
/// and only its two interpolations differ from a plain card's
#[test]
fn hostile_community_names_stay_in_their_path() {
    let plain = card(post("T"));
    let mut accepted = 0;
    for name in [
        r#""><script>alert(1)</script>"#,
        "a?b#c",
        "..",
        "%2e%2e/%2e%2e",
        "javascript:alert(1)",
        "//evil.example/x",
        "a b\tc\nd",
        "x' onmouseover='alert(1)",
        r"\\evil",
        "ａ＜script＞",
        "meta-governance-2",
    ] {
        let Ok(community) = CommunityName::new(name) else {
            continue;
        };
        accepted += 1;
        let mut sent = post("T");
        sent.community_name = community.clone();
        let html = card(&sent);
        let at = html.find(r#"href="/agora/community/"#).expect(&html)
            + r#"href=""#.len();
        let href = &html[at..at + html[at..].find('"').unwrap()];
        // As the browser reads the attribute (dioxus-ssr escapes `'`, `&`
        // and `"` in it as references)
        let read = href
            .replace("&#39;", "'")
            .replace("&#34;", "\"")
            .replace("&#38;", "&");
        assert!(
            !read.contains(['<', '>', '"', ' ', '\n', '\t', '?', '#', '\\']),
            "{read}"
        );
        assert!(!read["/agora/community/".len()..].contains('/'), "{read}");
        let want = plain
            .replace(
                r#"href="/agora/community/meta-governance""#,
                &format!(r#"href="{href}""#),
            )
            .replace(
                ">meta-governance<",
                &format!(">{}<", escaped(&community.to_string())),
            );
        assert_eq!(html, want, "{name:?}");
    }
    assert!(accepted >= 1);
}

#[test]
fn a_refreshing_feed_says_so() {
    let render = |feed: FeedLoad| {
        let mut dom = VirtualDom::new_with_props(
            FeedListing,
            FeedListingProps {
                page: page_of(feed),
                refreshing: true,
                on_refresh: None,
            },
        );
        dom.rebuild_in_place();
        dioxus_ssr::pre_render(&dom)
    };
    let html = render([post("A title")].iter().collect());
    assert!(
        html.contains(r#"<a id="agora-refresh" class="pill pill-btn" href="/agora/feed" aria-busy="true" data-node-hydration="4,click:1"><!--node-id5-->Refreshing…<!--#--></a>"#),
        "{html}"
    );
    let html = render(FeedLoad::Failed("e".into()));
    assert!(
        html.contains(r#"<a class="pill pill-btn" href="/agora/feed" aria-busy="true" data-node-hydration="8,click:1">Try again</a>"#),
        "{html}"
    );
}

/// The central claim: what a client provides (a handler, a `Reload`) never
/// changes the markup, so the server, which provides neither, draws the
/// client's first render
#[test]
fn a_handler_or_a_reload_changes_nothing_drawn() {
    // A handler is made inside the runtime, in a component
    #[component]
    fn Harness(feed: FeedLoad, handler: bool) -> Element {
        if handler {
            rsx! { FeedListing { page: page_of(feed), on_refresh: move |()| {} } }
        } else {
            rsx! { FeedListing { page: page_of(feed) } }
        }
    }
    fn list(feed: FeedLoad, handler: bool) -> String {
        pre_render(Harness, HarnessProps { feed, handler })
    }
    fn with_reload(feed: FeedLoad) -> String {
        let mut dom = VirtualDom::new_with_props(FeedIsland, page_of(feed));
        dom.provide_root_context(Reload::<FeedLoad>::new(|| {
            Box::pin(async { FeedLoad::Pending })
        }));
        dom.rebuild_in_place();
        dioxus_ssr::pre_render(&dom)
    }
    for feed in [
        FeedLoad::Pending,
        FeedLoad::Failed("e".into()),
        FeedLoad::Loaded(vec![]),
        [post("A"), post("B")].iter().collect(),
    ] {
        let server = island_of(feed.clone());
        assert_eq!(with_reload(feed.clone()), server);
        assert_eq!(list(feed.clone(), true), list(feed, false));
    }
}

fn with_ids(titles: &[(&str, &str)]) -> Vec<PostResponse> {
    titles
        .iter()
        .map(|(id, title)| {
            let mut p = post(title);
            p.id = id.parse().unwrap();
            p
        })
        .collect()
}

const A: &str = "00000000-0000-4000-8000-00000000000a";
const B: &str = "00000000-0000-4000-8000-00000000000b";

#[test]
fn a_post_listed_twice_is_drawn_once() {
    let posts = with_ids(&[(A, "first"), (B, "other"), (A, "again")]);
    let html = island_of(posts.iter().collect());
    assert_eq!(html.matches("<article ").count(), 2, "{html}");
    assert!(
        html.contains(">first<") && !html.contains(">again<"),
        "{html}"
    );
    assert!(html.find(">first<") < html.find(">other<"));
}

/// Drive a VirtualDom's effects and tasks until `done` or a budget runs out
async fn settle(dom: &mut VirtualDom, done: impl Fn(&str) -> bool) -> String {
    for _ in 0..20 {
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(20),
            dom.wait_for_work(),
        )
        .await;
        dom.render_immediate(&mut dioxus::dioxus_core::NoOpMutations);
        let html = dioxus_ssr::pre_render(dom);
        if done(&html) {
            return html;
        }
    }
    dioxus_ssr::pre_render(dom)
}

/// A fresh mount (no server data) loads through the client's `Reload`
/// after its first render, and a reload that lists a post twice re-renders
/// (a keyed diff) without tripping Dioxus's unique-key assertion
#[tokio::test]
async fn a_fresh_mount_loads_through_its_reload() {
    let loads = std::rc::Rc::new(std::cell::Cell::new(0));
    let counted = loads.clone();
    let mut dom =
        VirtualDom::new_with_props(FeedIsland, page_of(FeedLoad::Pending));
    dom.provide_root_context(Reload::<FeedLoad>::new(move || {
        counted.set(counted.get() + 1);
        Box::pin(async {
            with_ids(&[(A, "loaded"), (B, "other"), (A, "again")])
                .iter()
                .collect()
        })
    }));
    dom.rebuild_in_place();
    let first = dioxus_ssr::pre_render(&dom);
    assert!(first.contains("Loading posts"), "{first}");
    let later = settle(&mut dom, |html| html.contains(">loaded<")).await;
    assert!(later.contains(">loaded<"), "{later}");
    assert_eq!(later.matches("<article ").count(), 2, "{later}");
    assert_eq!(loads.get(), 1);
}

/// With the server's data, a client never loads on mount
#[tokio::test]
async fn a_hydrating_mount_does_not_load() {
    let loads = std::rc::Rc::new(std::cell::Cell::new(0));
    let counted = loads.clone();
    let mut dom = VirtualDom::new_with_props(
        FeedIsland,
        page_of([post("server")].iter().collect()),
    );
    dom.provide_root_context(Reload::<FeedLoad>::new(move || {
        counted.set(counted.get() + 1);
        Box::pin(async { FeedLoad::Loaded(vec![]) })
    }));
    dom.rebuild_in_place();
    let html = settle(&mut dom, |_| false).await;
    assert!(html.contains(">server<"), "{html}");
    assert_eq!(loads.get(), 0);
}

/// A list that lists a post twice, re-rendered as another such list: a
/// keyed diff, where Dioxus asserts (in debug) that keys are unique
#[tokio::test]
async fn a_repeated_post_survives_a_keyed_diff() {
    #[component]
    fn Changing() -> Element {
        let mut feed: Signal<FeedLoad> = use_signal(|| {
            with_ids(&[(A, "a1"), (B, "b1"), (A, "a1 again")])
                .iter()
                .collect()
        });
        use_effect(move || {
            feed.set(
                with_ids(&[(B, "b2"), (A, "a2"), (B, "b2 again")])
                    .iter()
                    .collect(),
            );
        });
        rsx! { FeedListing { page: page_of(feed()) } }
    }
    let mut dom = VirtualDom::new(Changing);
    dom.rebuild_in_place();
    let html = settle(&mut dom, |html| html.contains(">b2<")).await;
    assert_eq!(html.matches("<article ").count(), 2, "{html}");
    assert!(html.find(">b2<") < html.find(">a2<"), "{html}");
}

fn community(name: &str) -> CommunityResponse {
    CommunityResponse {
        name: name.parse().unwrap(),
        description: Some("How Agora governs itself".into()),
        is_governance: true,
        ..fixtures::community()
    }
}

fn community_page(feed: FeedLoad, input: GetFeedInput) -> FeedPage {
    FeedPage {
        community: Some(community("meta-governance")),
        ..FeedPage::new(input, feed)
    }
}

fn input_of(community: &str) -> GetFeedInput {
    GetFeedInput {
        community: Some(CommunityName::new(community).unwrap()),
        ..GetFeedInput::default()
    }
}

/// A community's feed: its header as the server's page has it, its own
/// sort links, and cards that don't repeat its name
#[test]
fn a_community_feed_has_its_header() {
    let page = community_page(
        [post("A title")].iter().collect(),
        input_of("meta-governance"),
    );
    let html = pre_render(FeedIsland, page);
    assert!(
        html.contains(concat!(
            r#"<div class="page-header" data-node-hydration="2"><h1><!--node-id3-->Meta Governance<!--#--></h1>"#,
            r#"<p data-node-hydration="4"><!--node-id5-->How Agora governs itself<!--#--></p>"#,
            r#"<div style="margin-top: 0.5rem;" data-node-hydration="6"><span class="pill pill-purple" data-node-hydration="7">Governance</span><!--placeholder8--></div></div>"#,
        )),
        "{html}"
    );
    assert!(
        html.contains(r#"href="/agora/community/meta-governance?sort=date""#),
        "{html}"
    );
    assert!(
        html.contains(r#"href="/agora/community/meta-governance?sort=active""#),
        "{html}"
    );
    assert!(html.contains(r#"id="agora-refresh" class="pill pill-btn" href="/agora/community/meta-governance""#), "{html}");
    // The card's own community pill is left out
    assert_eq!(
        html.matches("/agora/community/meta-governance").count(),
        3,
        "{html}"
    );
    let empty = pre_render(
        FeedIsland,
        community_page(FeedLoad::Loaded(vec![]), input_of("meta-governance")),
    );
    assert!(
        empty.contains("This community is waiting for its first post."),
        "{empty}"
    );
}

#[test]
fn the_active_sort_is_marked() {
    let input = GetFeedInput {
        sort: Some(FeedSort::Active),
        ..GetFeedInput::default()
    };
    let html =
        pre_render(FeedIsland, FeedPage::new(input, FeedLoad::Loaded(vec![])));
    assert!(
        html.contains(r#"<a class="pill pill-btn" href="/agora/feed?sort=date" data-node-hydration="3">New</a><a class="pill pill-btn active" href="/agora/feed?sort=active" data-node-hydration="4">Active</a>"#),
        "{html}"
    );
}

fn full_page(n: usize) -> FeedLoad {
    (0..n)
        .map(|i| {
            let mut p = post(&format!("Post {i}"));
            p.id = PostId::new();
            p
        })
        .collect::<Vec<_>>()
        .iter()
        .collect()
}

/// Paging as the server's feed pages do it, links that work without
/// JavaScript: with a count, "Page 2 of 3" and both links; without one
/// (the API gives none), a next link when the page is full
#[test]
fn paging_is_plain_links() {
    let input = GetFeedInput {
        sort: Some(FeedSort::Active),
        limit: Some(10),
        offset: Some(10),
        ..input_of("meta-governance")
    };
    let page = FeedPage {
        total: Some(25),
        ..community_page(full_page(10), input.clone())
    };
    let pages = page.pages();
    assert_eq!(
        pages,
        Pages {
            page: 2,
            of: Some(3),
            newer: Some("/agora/community/meta-governance?limit=10&sort=active".into()),
            older: Some(
                "/agora/community/meta-governance?limit=10&offset=20&sort=active".into()
            ),
            by_date: "/agora/community/meta-governance?limit=10&sort=date".into(),
            by_activity: "/agora/community/meta-governance?limit=10&sort=active".into(),
        }
    );
    let html = pre_render(FeedIsland, page);
    assert!(
        html.contains(
            r#"<a class="pill pill-btn" href="/agora/community/meta-governance?limit=10&#38;sort=active" data-node-hydration="#
        ),
        "{html}"
    );
    assert!(html.contains("Page 2 of 3"), "{html}");
    assert!(
        html.contains("Next →") && html.contains("← Previous"),
        "{html}"
    );

    // The last page: no next
    let last = FeedPage {
        total: Some(25),
        ..community_page(
            full_page(5),
            GetFeedInput {
                offset: Some(20),
                ..input.clone()
            },
        )
    };
    assert_eq!(last.pages().older, None);
    assert_eq!(last.pages().of, Some(3));

    // No count: a full page has a next link, a short one doesn't
    let unknown = community_page(full_page(10), input.clone());
    assert_eq!(unknown.pages().of, None);
    assert!(unknown.pages().older.is_some());
    let html = pre_render(FeedIsland, unknown);
    assert!(html.contains("Page 2<"), "{html}");
    let short = community_page(full_page(3), input);
    assert!(short.pages().older.is_none());

    // The first page of a short feed draws no paging at all
    let html = island_of(full_page(3));
    assert!(!html.contains("pagination"), "{html}");
}

/// A community's display name and description are someone's to choose:
/// inert, as titles are
#[test]
fn hostile_community_headers_are_inert() {
    const PLAIN: &str = "PLAIN-TEXT";
    let page_with = |text: &str| {
        let mut c = community("meta-governance");
        c.display_name = text.into();
        c.description = Some(text.into());
        FeedPage {
            community: Some(c),
            ..FeedPage::new(
                input_of("meta-governance"),
                FeedLoad::Loaded(vec![]),
            )
        }
    };
    let plain = pre_render(FeedIsland, page_with(PLAIN));
    for hostile in HOSTILE {
        let page = page_with(hostile);
        let out =
            render_island(island::DATA_ID, FeedIsland, page.clone()).unwrap();
        assert_eq!(out.html, plain.replace(PLAIN, &escaped(hostile)));
        let inner = out
            .script
            .strip_prefix(
                r#"<script type="application/json" id="agora-app-data">"#,
            )
            .and_then(|s| s.strip_suffix("</script>"))
            .unwrap();
        assert!(!inner.contains('<') && !inner.contains('>'), "{inner}");
        assert_eq!(island::from_script_text::<FeedPage>(inner).unwrap(), page);
    }
}

/// A page from a server that predates the paging fields still reads: they
/// default to the first page of the global feed
#[test]
fn an_older_page_reads_with_defaults() {
    let page: FeedPage =
        serde_json::from_str(r#"{"feed":{"state":"loaded","value":[]}}"#)
            .unwrap();
    assert_eq!(
        page,
        FeedPage::new(GetFeedInput::default(), FeedLoad::Loaded(vec![]))
    );
}

/// Paging arithmetic can't overflow, whatever offset and limit a link asks
/// for
#[test]
fn paging_never_overflows() {
    for (offset, limit, total) in [
        (u32::MAX, 1, None),
        (u32::MAX, 1, Some(u64::MAX)),
        (u32::MAX - 1, 100, Some(u64::from(u32::MAX))),
        (0, 0, None),
    ] {
        let input = GetFeedInput {
            offset: Some(offset),
            limit: Some(limit),
            ..GetFeedInput::default()
        };
        let page = FeedPage {
            total,
            ..FeedPage::new(input, full_page(limit.clamp(1, 100) as usize))
        };
        let pages = page.pages();
        assert!(pages.page >= 1);
        assert!(
            pages.older.is_none() || offset < u32::MAX - 100,
            "{pages:?}"
        );
        island_of_page(page);
    }
    let page = FeedPage::new(
        GetFeedInput {
            offset: Some(u32::MAX),
            limit: Some(1),
            ..GetFeedInput::default()
        },
        full_page(1),
    );
    assert_eq!(page.pages().page, u64::from(u32::MAX) + 1);
    assert!(page.pages().older.is_none());
}

fn island_of_page(page: FeedPage) -> String {
    pre_render(FeedIsland, page)
}

/// A random or diverse feed marks neither New nor Active
#[test]
fn an_unnamed_sort_marks_neither() {
    for sort in [FeedSort::Random, FeedSort::Diverse] {
        let input = GetFeedInput {
            sort: Some(sort),
            ..GetFeedInput::default()
        };
        let html =
            island_of_page(FeedPage::new(input, FeedLoad::Loaded(vec![])));
        assert!(!html.contains("pill-btn active"), "{html}");
    }
}
