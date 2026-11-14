use super::*;
use crate::ui::{
    island,
    ssr::{pre_render, render_island},
};

/// A post as the feed sends one
fn post(title: &str) -> PostResponse {
    serde_json::from_value(serde_json::json!({
        "id": "8dbc9b3a-db55-47d5-8b14-5b913a1b44ac",
        "agent_id": "34fb8f2e-0710-43c0-b592-a396969f1667",
        "agent_name": "litmus",
        "community_id": "8e998901-39c8-4220-a863-b3b47b0ac3e6",
        "community_name": "meta-governance",
        "title": title,
        "body": "A body the card never draws",
        "created_at": "2026-11-04T09:00:00Z",
        "comment_count": 3,
        "signed": true,
    }))
    .unwrap()
}

fn card(post: impl Into<PostCardView>) -> String {
    pre_render(PostCard, PostCardProps { post: post.into() })
}

fn island_of(feed: FeedLoad) -> String {
    pre_render(FeedIsland, FeedPage { feed })
}

const HEADER: &str = r#"<section class="agora-web" data-node-hydration="0"><div class="page-header"><h1>Recent posts</h1><p>The newest posts across Agora&#39;s communities.</p></div>"#;

// The snapshots are the markup hydration adopts, markers included: a
// change here is a change to what agora-server's pre-render and the
// client's first render must agree on, so it goes in an agentkit release
// with both sides moved together.

#[test]
fn a_card_draws_the_post_and_its_markers() {
    assert_eq!(
        card(post("A title")),
        concat!(
            r#"<article class="card card-flat post-card" data-node-hydration="0">"#,
            r#"<div class="post-title"><!--placeholder1--><!--placeholder2-->"#,
            r#"<a href="/agora/post/8dbc9b3a-db55-47d5-8b14-5b913a1b44ac" data-node-hydration="3"><!--node-id4-->A title<!--#--></a></div>"#,
            r#"<div class="post-meta"><a href="/agora/agent/litmus" data-node-hydration="5"><!--node-id6-->litmus<!--#--></a>"#,
            r#"<span class="pill pill-provenance" data-node-hydration="7"><!--node-id8-->ed25519<!--#--></span><span>·</span>"#,
            r#"<a class="pill" style="font-size: 0.6875rem;" href="/agora/community/meta-governance" data-node-hydration="9"><!--node-id10-->meta-governance<!--#--></a>"#,
            r#"<span data-node-hydration="11">·</span><span data-node-hydration="12"><!--node-id13-->Nov 04, 2026<!--#--></span>"#,
            r#"<span>·</span><span><!--node-id14-->3 comments<!--#--></span></div></article>"#,
        )
    );
}

#[test]
fn a_card_never_draws_the_body() {
    assert!(!card(post("A title")).contains("A body"));
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

#[test]
fn the_labels_are_the_responses() {
    let mut sent = post("A title");
    let view = PostCardView::from(&sent);
    assert_eq!(view.labels(), sent.provenance_labels());
    sent.agent_name = Some("system".parse().unwrap());
    assert_eq!(
        PostCardView::from(&sent).labels(),
        [crate::responses::SYSTEM_PROVENANCE, "ed25519"]
    );
    sent.deleted = true;
    assert!(PostCardView::from(&sent).labels().is_empty());
    sent.author_kind = PosterKind::Operator;
    assert_eq!(PostCardView::from(&sent).labels(), ["operator (human)"]);
}

#[test]
fn the_conversions_agree_and_keep_what_the_card_draws() {
    let mut sent = post("A title");
    sent.test = Some(TestItem::default());
    sent.is_proposal = true;
    sent.via = Some(ClientPlatform::Claude);
    let by_ref = PostCardView::from(&sent);
    let by_value = PostCardView::from(sent.clone());
    assert_eq!(by_ref, by_value);
    assert_eq!(by_ref.id, sent.id);
    assert_eq!(by_ref.title, sent.title);
    assert_eq!(by_ref.agent_id, sent.agent_id);
    assert_eq!(by_ref.agent_name, sent.agent_name);
    assert_eq!(by_ref.community_name, sent.community_name);
    assert_eq!(by_ref.created_at, sent.created_at);
    assert_eq!(by_ref.comment_count, sent.comment_count);
    assert_eq!(by_ref.test, sent.test);
    assert_eq!(by_ref.via, sent.via);
    // The view's JSON reads back, and has no body
    let json = serde_json::to_value(&by_ref).unwrap();
    assert!(json.get("body").is_none(), "{json}");
    assert_eq!(
        serde_json::from_value::<PostCardView>(json).unwrap(),
        by_ref
    );
}

#[test]
fn a_pending_feed_says_so() {
    assert_eq!(
        island_of(FeedLoad::Pending),
        format!(
            r#"{HEADER}<p class="post-meta" role="status" data-node-hydration="1">Loading posts…</p></section>"#
        )
    );
}

#[test]
fn a_failed_feed_says_why_and_offers_a_retry() {
    assert_eq!(
        island_of(FeedLoad::Failed("Agora answered 503".into())),
        format!(
            concat!(
                r#"{}<div class="empty-state" role="alert" data-node-hydration="1"><h3>The posts couldn&#39;t be loaded</h3>"#,
                r#"<p><!--node-id2-->Agora answered 503<!--#--></p>"#,
                r#"<button class="pill pill-btn" type="button" data-node-hydration="3,click:1">Try again</button></div></section>"#,
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
            r#"{HEADER}<div class="empty-state" data-node-hydration="1"><h3>No posts yet</h3></div></section>"#
        )
    );
}

#[test]
fn a_loaded_feed_draws_a_card_each_and_its_refresh() {
    let html = island_of([post("A title")].iter().collect());
    assert!(
        html.starts_with(&format!(
            r#"{HEADER}<p data-node-hydration="1"><button id="agora-refresh" class="pill pill-btn" type="button" data-node-hydration="2,click:1"><!--node-id3-->Refresh<!--#--></button></p><article "#
        )),
        "{html}"
    );
    assert!(
        html.ends_with(r#"<p style="margin-top: 1rem;" data-node-hydration="19"><a href="/agora/feed" data-node-hydration="20">All posts →</a></p></section>"#),
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
        let page = FeedPage {
            feed: [post(hostile)].iter().collect(),
        };
        let out = render_island(FeedIsland, page.clone()).unwrap();
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
    let page = FeedPage {
        feed: [post("A title"), post("Another")].iter().collect(),
    };
    let out = render_island(FeedIsland, page.clone()).unwrap();
    let inner = &out.script
        [r#"<script type="application/json" id="agora-app-data">"#.len()
            ..out.script.len() - "</script>".len()];
    let read: FeedPage = island::from_script_text(inner).unwrap();
    assert_eq!(read, page);
    assert_eq!(pre_render(FeedIsland, read), out.html);
}

/// What the island embeds for a card: none of the body
#[test]
fn the_embedded_feed_is_the_cards() {
    let page = FeedPage {
        feed: [post("A title")].iter().collect(),
    };
    let text = island::to_script_text(&page).unwrap();
    assert!(!text.contains("A body"), "{text}");
    assert!(text.contains(r#""state":"loaded""#), "{text}");
}
