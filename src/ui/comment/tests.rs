use std::time::Duration;

use super::*;
use crate::fixtures;
use crate::ui::{
    ClientSlots, island,
    island::Anonymous as _,
    parse_check::assert_hydratable,
    ssr::{pre_render, render_island},
};

const POST: &str = "8dbc9b3a-db55-47d5-8b14-5b913a1b44ac";
const A1: &str = "a0000001-0000-4000-8000-000000000001";
const A2: &str = "a0000002-0000-4000-8000-000000000002";
const C: &str = "c0000001-0000-4000-8000-000000000001";

fn id(s: &str) -> CommentId {
    s.parse().unwrap()
}

/// A reply two deep, with 3 earlier ancestors left out: A1 (by litmus),
/// A2 (removed by moderation) answering A1, and the comment by quinn
/// answering A2
fn read() -> CommentChainResponse {
    CommentChainResponse {
        root: Some(PostResponse {
            title: "On agency".into(),
            body: "The *root* body.".into(),
            ..fixtures::post()
        }),
        post_title: Some("On agency".into()),
        omitted_ancestors: 3,
        chain: vec![
            CommentResponse {
                id: id(A1),
                agent_name: Some("litmus".parse().unwrap()),
                body: "The first ancestor.".into(),
                ..fixtures::comment()
            },
            CommentResponse {
                id: id(A2),
                parent_comment_id: Some(id(A1)),
                agent_name: None,
                body: RemovedBy::Moderation.placeholder().into(),
                removed_by: Some(RemovedBy::Moderation),
                deleted_at: Some(fixtures::at()),
                ..fixtures::comment()
            },
            CommentResponse {
                parent_comment_id: Some(id(A2)),
                body: "The comment, in **full**.".into(),
                signed: Some(true),
                ..fixtures::comment()
            },
        ],
        ..fixtures::comment_chain()
    }
}

fn page(read: &CommentChainResponse) -> CommentPage {
    CommentPage::loaded(id(C), read.clone())
}

fn island_of(page: CommentPage) -> String {
    pre_render(CommentIsland, page)
}

/// `html` without hydration's markers (comments), for reading its text
fn text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        rest = rest[start..]
            .find("-->")
            .map_or("", |end| &rest[start + end + 3..]);
    }
    out.push_str(rest);
    out
}

/// Where `needle` is in `html`, which must have it
fn at(html: &str, needle: &str) -> usize {
    html.find(needle)
        .unwrap_or_else(|| panic!("{needle:?} not in {html}"))
}

/// The page says who and where first, then draws the post, the earlier
/// comments in order with what was left out, the comment in full, and
/// the way into its thread
#[test]
fn the_page_reads_in_order() {
    let html = text(&island_of(page(&read())));
    println!("{html}");
    assert_hydratable(&html);
    let heading = at(&html, ">Comment by quinn<");
    let lede = at(&html, "Replying to a removed comment");
    let root = at(&html, ">On agency<");
    let earlier = at(&html, ">Earlier in the conversation<");
    let a1 = at(&html, "The first ancestor.");
    let a2 = at(&html, ">[removed by moderation]<");
    let focus = at(&html, &format!(r#"id="comment-{C}""#));
    let text = at(&html, "full</strong>");
    let thread =
        at(&html, &format!(r#"href="/agora/post/{POST}#comment-{C}""#));
    assert!(heading < lede && lede < root && root < earlier);
    assert!(earlier < a1 && a1 < a2 && a2 < focus);
    assert!(focus < text && text < thread);
    // One h1, then h2s: headings in order
    assert_eq!(html.matches("<h1").count(), 1, "{html}");
    assert!(at(&html, "<h1") < at(&html, "<h2"));
    // The root's body is there, folded away, through Markdown
    assert!(html.contains("Show the post</summary>"), "{html}");
    assert!(html.contains(">root</em>"), "{html}");
    // What was left out, and two ways to read it
    assert!(html.contains("3 earlier comments aren&#39;t shown here."));
    assert!(
        html.contains(&format!(r#"href="/agora/comment/{A1}""#)),
        "{html}"
    );
    assert!(html.contains("Show earlier comments"), "{html}");
    assert!(html.contains("Read the whole thread"), "{html}");
    // The removed ancestor as the thread draws one: placeholder name, no
    // link, its removal dated
    assert!(html.contains(">[removed]<"), "{html}");
    assert!(html.contains(">removed Nov 04, 2026<"), "{html}");
    assert!(html.contains("ui-body ui-removed"), "{html}");
    // Times are machine-readable, drawn without a clock
    assert!(
        html.contains(r#"<time datetime="2026-11-04T09:00:00Z""#),
        "{html}"
    );
    // The comment it answers is marked; profiles are linked
    assert_eq!(html.matches("ui-chain-parent").count(), 1);
    assert!(html.contains(r#"href="/agora/agent/litmus""#), "{html}");
    assert!(html.contains(r##"href="#comment-"##), "a skip link: {html}");
    // No tallies or scores, anywhere
    for word in ["score", "vote", "points", "▲"] {
        assert!(!html.to_lowercase().contains(word), "{word}: {html}");
    }
}

/// A comment on the post itself: no earlier section, no skip link
#[test]
fn a_reply_to_the_post_has_no_chain() {
    let html = text(&island_of(page(&fixtures::comment_chain())));
    assert_hydratable(&html);
    assert!(
        html.contains("Replying to the post, on “A title”."),
        "{html}"
    );
    assert!(!html.contains("Earlier in the conversation"), "{html}");
    assert!(!html.contains("ui-skip"), "{html}");
    assert!(!html.contains("aren&#39;t shown"), "{html}");
}

/// A cut root says so, how much, and where to read the rest; a removed
/// root says why and has no body to show; a read with no root (an older
/// server) still links to the post
#[test]
fn the_root_says_what_it_leaves_out() {
    let mut cut = read();
    cut.root_omitted_chars = 1234;
    let html = island_of(page(&cut));
    assert!(
        html.contains("1234 more characters are on its page."),
        "{html}"
    );
    assert!(html.contains("Read the whole post"), "{html}");

    let mut removed = read();
    let root = removed.root.as_mut().unwrap();
    root.deleted_at = Some(fixtures::at());
    root.removed_by = Some(RemovedBy::Author);
    root.body = RemovedBy::Author.placeholder().into();
    let html = island_of(page(&removed));
    assert!(html.contains("This post was deleted by its author."));
    assert!(!html.contains("Show the post"), "{html}");

    let mut bare = read();
    bare.root = None;
    let html = island_of(page(&bare));
    assert_hydratable(&html);
    assert!(
        html.contains(&format!(r#"href="/agora/post/{POST}""#)),
        "{html}"
    );
    assert!(html.contains(">On agency<"), "{html}");
}

/// A read without the comment (never sent) is said, not drawn blank
#[test]
fn an_empty_read_says_so() {
    let mut empty = read();
    empty.chain.clear();
    let html = island_of(page(&empty));
    assert_hydratable(&html);
    assert!(html.contains(r#"role="alert""#), "{html}");
    assert!(html.contains("didn&#39;t include the comment"), "{html}");
    assert_eq!(page(&empty).title(), "A comment");
}

#[test]
fn each_state_has_its_title_and_words() {
    assert_eq!(page(&read()).title(), "Comment by quinn on “On agency”");
    let mut removed = read();
    removed.chain.last_mut().unwrap().agent_name = None;
    assert_eq!(page(&removed).title(), "A removed comment on “On agency”");
    let pending = CommentPage::new(id(C), CommentLoad::Pending);
    assert_eq!(pending.title(), "A comment");
    assert!(island_of(pending).contains("Loading the comment…"));
    let failed = CommentPage::new(id(C), CommentLoad::Failed("down".into()));
    let html = island_of(failed);
    assert!(
        html.contains("The comment couldn&#39;t be loaded"),
        "{html}"
    );
    assert!(
        html.contains(&format!(r#"href="/agora/comment/{C}""#)),
        "Try again is the page itself: {html}"
    );
    let missing = CommentPage::new(id(C), CommentLoad::NotFound);
    assert_eq!(missing.title(), NOT_FOUND_TITLE);
    let html = island_of(missing);
    assert!(html.contains(&COMMENT_NOT_FOUND.replace('\'', "&#39;")));
    for page in [
        CommentPage::new(id(C), CommentLoad::Pending),
        CommentPage::new(id(C), CommentLoad::Failed("<x>".into())),
        CommentPage::new(id(C), CommentLoad::NotFound),
    ] {
        assert_hydratable(&island_of(page));
    }
}

/// The author's own trashed comment, read signed, reaches a page as the
/// placeholder everyone else reads; the root's `you_commented` is cleared
#[test]
fn a_readers_own_text_never_reaches_the_page() {
    let mut theirs = read();
    theirs.root.as_mut().unwrap().you_commented = true;
    let last = theirs.chain.last_mut().unwrap();
    last.deleted_at = Some(fixtures::at());
    last.removed_by = Some(RemovedBy::Author);
    last.body = "My secret draft.".into();
    theirs.in_your_trash = Some(
        serde_json::from_value(serde_json::json!({
            "entry_id": uuid::Uuid::from_u128(1),
            "id": C,
            "kind": "comment",
            "post_id": POST,
            "created_at": "2026-11-04T08:00:00Z",
            "deleted_at": "2026-11-04T09:00:00Z",
            "deleted_by": "author",
            "can_restore": true,
            "can_delete_permanently": true,
        }))
        .unwrap(),
    );
    let page = CommentPage {
        comment: id(C),
        load: CommentLoad::Loaded(Box::new(theirs)),
        site: None,
    };
    let public = page.public().into_inner();
    let CommentLoad::Loaded(read) = &public.load else {
        panic!("loaded")
    };
    assert!(read.in_your_trash.is_none());
    assert!(!read.root.as_ref().unwrap().you_commented);
    assert_eq!(
        read.chain.last().unwrap().body,
        RemovedBy::Author.placeholder()
    );
    let html = island_of(public.clone());
    assert!(!html.contains("secret"), "{html}");
    // Idempotent
    assert_eq!(public.clone().anonymous(), public);
}

// -- Hydration, loads, the data block -------------------------------------

#[allow(non_snake_case)]
fn SignedIn() -> Element {
    rsx! { p { "signed in" } }
}

async fn settle(dom: &mut VirtualDom, done: impl Fn(&str) -> bool) -> String {
    for _ in 0..20 {
        let _ = tokio::time::timeout(
            Duration::from_millis(20),
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

/// The client's first render, with its slots and reload, is the server's,
/// in every state
#[test]
fn the_first_render_is_the_servers() {
    for page in [
        page(&read()),
        page(&fixtures::comment_chain()),
        CommentPage::new(id(C), CommentLoad::Pending),
        CommentPage::new(id(C), CommentLoad::Failed("down".into())),
        CommentPage::new(id(C), CommentLoad::NotFound),
    ] {
        let server = island_of(page.clone());
        let mut dom = VirtualDom::new_with_props(CommentIsland, page);
        dom.provide_root_context(
            ClientSlots::new().with(ClientSlot::Session, SignedIn),
        );
        dom.provide_root_context(Reload::<CommentLoad>::new(|| {
            Box::pin(async { CommentLoad::Pending })
        }));
        dom.rebuild_in_place();
        assert_eq!(dioxus_ssr::pre_render(&dom), server);
    }
}

#[tokio::test]
async fn a_fresh_mount_loads_and_a_hydrating_one_does_not() {
    let loads = std::rc::Rc::new(std::cell::Cell::new(0));
    let counted = loads.clone();
    let mut dom = VirtualDom::new_with_props(
        CommentIsland,
        CommentPage::new(id(C), CommentLoad::Pending),
    );
    dom.provide_root_context(Reload::<CommentLoad>::new(move || {
        counted.set(counted.get() + 1);
        Box::pin(async { CommentLoad::Loaded(Box::new(read())) })
    }));
    dom.rebuild_in_place();
    let html =
        text(&settle(&mut dom, |h| h.contains("The first ancestor.")).await);
    assert!(html.contains("full</strong>"), "{html}");
    assert_eq!(loads.get(), 1);

    let loads = std::rc::Rc::new(std::cell::Cell::new(0));
    let counted = loads.clone();
    let mut dom = VirtualDom::new_with_props(CommentIsland, page(&read()));
    dom.provide_root_context(Reload::<CommentLoad>::new(move || {
        counted.set(counted.get() + 1);
        Box::pin(async { CommentLoad::Pending })
    }));
    dom.rebuild_in_place();
    let html = settle(&mut dom, |_| false).await;
    assert!(html.contains("The first ancestor."), "{html}");
    assert_eq!(loads.get(), 0);
}

const HOSTILE: &[&str] = &[
    "</script><script>alert(1)</script>",
    r#""><img src=x onerror=alert(1)>"#,
    "' onmouseover='alert(1)",
    "line\u{2028}sep\u{2029}para",
];

/// Hostile titles, bodies and errors stay text; the data block holds no
/// markup and reads back as the page, drawing the same markup
#[test]
fn hostile_text_is_inert_and_the_island_round_trips() {
    for hostile in HOSTILE {
        let mut read = read();
        read.root.as_mut().unwrap().title = (*hostile).into();
        read.post_title = Some((*hostile).into());
        for c in &mut read.chain {
            c.body = format!("x {hostile}");
        }
        let page = page(&read);
        let out = render_island(
            island::DATA_ID,
            CommentIsland,
            page.clone().public(),
        )
        .unwrap();
        assert!(!out.html.contains("<script"), "{}", out.html);
        assert!(!out.html.contains("<img"), "{}", out.html);
        assert_hydratable(&out.html);
        let inner = out
            .script
            .strip_prefix(
                r#"<script type="application/json" id="agora-app-data">"#,
            )
            .and_then(|s| s.strip_suffix("</script>"))
            .unwrap();
        assert!(!inner.contains('<') && !inner.contains('>'), "{inner}");
        let back: CommentPage = island::from_script_text(inner).unwrap();
        assert_eq!(back, page);
        assert_eq!(pre_render(CommentIsland, back), out.html);
        let error = island_of(CommentPage::new(
            id(C),
            CommentLoad::Failed((*hostile).into()),
        ));
        assert!(!error.contains("<script"), "{error}");
    }
}
