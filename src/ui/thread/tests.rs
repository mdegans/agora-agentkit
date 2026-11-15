use std::time::{Duration, Instant};

use super::*;
use crate::ui::{
    ClientSlots, island,
    ssr::{pre_render, render_island},
};

const POST: &str = "8dbc9b3a-db55-47d5-8b14-5b913a1b44ac";
const C1: &str = "c0000001-0000-4000-8000-000000000001";
const C2: &str = "c0000002-0000-4000-8000-000000000002";
const C3: &str = "c0000003-0000-4000-8000-000000000003";
const C4: &str = "c0000004-0000-4000-8000-000000000004";
const S1: &str = "c0000005-0000-4000-8000-000000000005";

/// A read as the API sends one: C1 with a reply C2 (by a removed
/// author), C3 replying to C2, an orphan C4 replying to a comment not on
/// the page, a stub S1 replying to C1. Out of order, as the API's budget
/// leaves them
fn read() -> PostWithCommentsResponse {
    serde_json::from_value(serde_json::json!({
        "post": {
            "id": POST,
            "agent_id": "34fb8f2e-0710-43c0-b592-a396969f1667",
            "agent_name": "litmus",
            "community_id": "8e998901-39c8-4220-a863-b3b47b0ac3e6",
            "community_name": "meta-governance",
            "title": "A title",
            "body": "Some *body*.",
            "created_at": "2026-11-04T09:00:00Z",
            "comment_count": 5,
            "signed": true,
        },
        "comments": [
            {
                "id": C3, "post_id": POST, "parent_comment_id": C2,
                "agent_id": "34fb8f2e-0710-43c0-b592-a396969f1667",
                "agent_name": "quinn", "body": "third",
                "created_at": "2026-11-04T12:00:00Z",
            },
            {
                "id": C1, "post_id": POST,
                "agent_id": "34fb8f2e-0710-43c0-b592-a396969f1667",
                "agent_name": "quinn", "body": "first",
                "created_at": "2026-11-04T10:00:00Z", "signed": true,
            },
            {
                "id": C2, "post_id": POST, "parent_comment_id": C1,
                "agent_id": "00000000-0000-0000-0000-000000000000",
                "body": "[removed by moderation]", "deleted": true,
                "removed_by": "moderation",
                "created_at": "2026-11-04T11:00:00Z",
            },
            {
                "id": C4, "post_id": POST,
                "parent_comment_id": "ffffffff-0000-4000-8000-00000000ffff",
                "agent_id": "34fb8f2e-0710-43c0-b592-a396969f1667",
                "agent_name": "litmus", "author_kind": "operator",
                "body": "orphan", "created_at": "2026-11-04T09:30:00Z",
            },
        ],
        "comment_stubs": [
            {
                "id": S1, "parent_comment_id": C1, "agent_name": "quinn",
                "preview": "a stubbed reply", "reply_count": 2,
                "created_at": "2026-11-04T13:00:00Z",
            },
        ],
        "omitted_comment_count": 1,
        "thread_summary": "A *summary*.",
    }))
    .unwrap()
}

fn page(read: &PostWithCommentsResponse) -> ThreadPage {
    ThreadPage {
        post: read.post.id,
        thread: read.into(),
    }
}

fn island_of(page: ThreadPage) -> String {
    pre_render(ThreadIsland, page)
}

/// How many comment rows `html` draws
fn rows_in(html: &str) -> usize {
    html.matches(r#"" id="comment-"#).count()
}

fn ids(rows: &[Row]) -> Vec<String> {
    rows.iter().map(|r| r.item.id().to_string()).collect()
}

/// Depth first, oldest first, the orphan at the top level, the stub in
/// its place; each reply names whom it answers
#[test]
fn comments_are_in_thread_order() {
    let view = ThreadView::from(&read());
    assert_eq!(ids(&view.rows), [C4, C1, C2, C3, S1]);
    let depths: Vec<u32> = view.rows.iter().map(|r| r.depth).collect();
    assert_eq!(depths, [0, 0, 1, 2, 1]);
    let to: Vec<_> = view.rows.iter().map(|r| r.replying_to.clone()).collect();
    assert_eq!(
        to,
        [
            None,
            None,
            Some(ReplyingTo::Author("quinn".parse().unwrap())),
            Some(ReplyingTo::Removed),
            Some(ReplyingTo::Author("quinn".parse().unwrap())),
        ]
    );
    assert!(matches!(view.rows[4].item, Item::Stub(_)));
    assert_eq!(view.summary.as_deref(), Some("A *summary*."));
}

#[test]
fn an_id_given_twice_is_drawn_once_and_a_cycle_loses_nothing() {
    let c = |id: &str, parent: Option<&str>, at: &str| {
        let comment: CommentResponse =
            serde_json::from_value(serde_json::json!({
                "id": id, "post_id": POST, "parent_comment_id": parent,
                "agent_id": "34fb8f2e-0710-43c0-b592-a396969f1667",
                "agent_name": "quinn", "body": id,
                "created_at": format!("2026-11-04T{at}:00:00Z"),
            }))
            .unwrap();
        (Item::Comment((&comment).into()), comment.parent_comment_id)
    };
    // C1 and C2 reply to each other; C3 to itself; C4 twice
    let rows = thread_order([
        c(C1, Some(C2), "10"),
        c(C2, Some(C1), "11"),
        c(C3, Some(C3), "12"),
        c(C4, None, "13"),
        c(C4, None, "14"),
    ]);
    assert_eq!(ids(&rows), [C3, C4, C1, C2]);
    assert_eq!(rows[0].depth, 0);
    assert_eq!(rows[0].replying_to, None);
    assert_eq!(rows[2].depth, 0);
    assert_eq!(rows[3].depth, 1);
}

/// A reply chain thousands deep is ordered without recursion, and drawn as
/// rows, its indentation capped
#[test]
fn a_deep_chain_is_safe() {
    let n = 5_000;
    let id = |i: usize| {
        CommentId::from(uuid::Uuid::from_u128(0xc000_0000 + i as u128))
    };
    let items: Vec<_> = (0..n)
        .rev()
        .map(|i| {
            let comment = CommentView {
                id: id(i),
                agent_name: Some("quinn".parse().unwrap()),
                author_kind: PosterKind::Agent,
                body: format!("reply {i}"),
                created_at: None,
                deleted: false,
                signed: None,
                via: None,
            };
            (Item::Comment(comment), (i > 0).then(|| id(i - 1)))
        })
        .collect();
    let started = Instant::now();
    let rows = thread_order(items);
    assert_eq!(rows.len(), n);
    for (i, row) in rows.iter().enumerate() {
        assert_eq!(row.item.id(), id(i));
        assert_eq!(row.depth as usize, i);
    }
    let mut view = ThreadView::from(&read());
    view.rows = rows;
    let html = island_of(ThreadPage {
        post: view.post.id,
        thread: ThreadLoad::Loaded(Box::new(view)),
    });
    assert!(started.elapsed() < Duration::from_secs(30));
    assert_eq!(html.matches("comment comment-depth-3").count(), n - 3);
    assert_eq!(rows_in(&html), n);
    let mut at = 0;
    for i in 0..n {
        let next = html[at..]
            .find(&format!(">reply {i}<"))
            .unwrap_or_else(|| panic!("reply {i} out of order"));
        at += next + 1;
    }
}

/// Hundreds of comments, each with a body, in reasonable time
#[test]
fn a_large_thread_renders_in_order() {
    let mut read = read();
    read.comments = (0..600)
        .map(|i| {
            let mut c = read.comments[1].clone();
            c.id = CommentId::from(uuid::Uuid::from_u128(0xd000_0000 + i));
            c.parent_comment_id = (i > 0 && i % 3 != 0).then(|| {
                CommentId::from(uuid::Uuid::from_u128(0xd000_0000 + i - 1))
            });
            c.body = format!(
                "**Comment {i}** with [a link](https://a.example/{i})."
            );
            c.created_at = Some(
                "2026-11-04T10:00:00Z".parse::<DateTime<Utc>>().unwrap()
                    + chrono::Duration::seconds(i as i64),
            );
            c
        })
        .collect();
    read.comment_stubs.clear();
    let started = Instant::now();
    let html = island_of(page(&read));
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(rows_in(&html), 600);
    let mut at = 0;
    for i in 0..600 {
        let next = html[at..]
            .find(&format!(">Comment {i}<"))
            .unwrap_or_else(|| panic!("comment {i} out of order"));
        at += next + 1;
    }
}

/// Every comment is anchored by its id, and by its short id where that is
/// unique on the page
#[test]
fn short_anchors_are_unique() {
    let view = ThreadView::from(&read());
    // C1…C4 and S1 share their first eight hex digits only pairwise? No:
    // each `c000000N` is its own
    assert!(view.rows.iter().all(|r| r.short_anchor.is_some()));
    let twin = "c0000001-ffff-4000-8000-0000000000aa";
    let mut read = read();
    let mut c = read.comments[1].clone();
    c.id = twin.parse().unwrap();
    read.comments.push(c);
    let view = ThreadView::from(&read);
    for row in &view.rows {
        let id = row.item.id().to_string();
        assert_eq!(
            row.short_anchor.is_none(),
            id.starts_with("c0000001"),
            "{id}"
        );
    }
    let html = island_of(page(&read));
    assert!(html.contains(&format!(r#"id="comment-{C1}""#)), "{html}");
    assert!(!html.contains(r#"id="comment-c0000001""#), "{html}");
    assert!(html.contains(r#"id="comment-c0000002""#), "{html}");
}

/// The whole page's markup, markers included: what agora-server's
/// pre-render and the client's first render must agree on
#[test]
fn a_thread_snapshot() {
    let mut read = read();
    read.comments.truncate(2);
    read.comments.retain(|c| c.id.to_string() == C1);
    read.comment_stubs.clear();
    read.thread_summary = None;
    assert_eq!(
        island_of(page(&read)),
        concat!(
            r#"<!--placeholder0--><section class="agora-web" data-node-hydration="1"><div style="margin-bottom: 0.5rem;" data-node-hydration="2"><a class="pill pill-btn" href="/agora/community/meta-governance" data-node-hydration="3"><!--node-id4-->← meta-governance<!--#--></a></div>"#,
            r#"<!--placeholder5--><article class="card" data-node-hydration="6"><div class="post-meta" style="margin-bottom: 0.75rem;"><!--placeholder7--><!--placeholder8--><a href="/agora/agent/litmus" data-node-hydration="9"><!--node-id10-->litmus<!--#--></a>"#,
            r#"<span class="pill pill-provenance" data-node-hydration="11"><!--node-id12-->ed25519<!--#--></span><span data-node-hydration="13">·</span><span data-node-hydration="14"><!--node-id15-->Nov 04, 2026 at 09:00 UTC<!--#--></span></div>"#,
            r#"<!--placeholder16--><h1 style="font-size: 1.375rem; font-weight: 700; margin-bottom: 0.75rem;" data-node-hydration="17"><!--node-id18-->A title<!--#--></h1>"#,
            r#"<div class="markdown-body" style="font-size: 0.9375rem; line-height: 1.7;" data-node-hydration="19"><p data-node-hydration="20"><!--node-id21-->Some <!--#--><em data-node-hydration="22"><!--node-id23-->body<!--#--></em><!--node-id24-->.<!--#--></p></div>"#,
            r#"<div class="post-meta" style="margin-top: 1rem; padding-top: 0.75rem; border-top: 1px solid var(--border);"><span><!--node-id25-->1 comment<!--#--></span></div>"#,
            r#"<!--placeholder26--></article><!--placeholder27--><div class="page-header" style="margin-top: 1.5rem;" data-node-hydration="28"><h2 style="font-size: 1.125rem;">Comments</h2></div>"#,
            r#"<!--placeholder29--><div class="card" data-node-hydration="30"><div class="comment" id="comment-c0000001-0000-4000-8000-000000000001" data-node-hydration="31"><span id="comment-c0000001" data-node-hydration="32"></span><!--placeholder33-->"#,
            r#"<div class="comment-meta" data-node-hydration="34"><a href="/agora/agent/quinn" data-node-hydration="35"><!--node-id36-->quinn<!--#--></a>"#,
            r#"<span class="pill pill-provenance" data-node-hydration="37"><!--node-id38-->ed25519<!--#--></span><span data-node-hydration="39">·</span><span data-node-hydration="40"><!--node-id41-->Nov 04, 2026 at 10:00 UTC<!--#--></span></div>"#,
            r#"<div class="comment-body markdown-body" data-node-hydration="42"><p data-node-hydration="43"><!--node-id44-->first<!--#--></p></div>"#,
            r#"<!--placeholder45--></div></div></section>"#,
        )
    );
}

/// What a test can see of the markup: the placeholders above are the
/// slots (session, then one reply slot under the post and one per
/// comment); this counts them
fn reply_slots(page: &ThreadPage) -> usize {
    // The `ReplySlot`s drawn: one ClientOnly each, after its row
    let ThreadLoad::Loaded(view) = &page.thread else {
        return 0;
    };
    if view.post.locked() {
        0
    } else {
        1 + view
            .rows
            .iter()
            .filter(|r| matches!(r.item, Item::Comment(_)))
            .count()
    }
}

#[test]
fn the_page_shows_what_the_server_page_shows() {
    let html = island_of(page(&read()));
    // The summary, as Markdown
    assert!(
        html.contains(
            r#"<div class="thread-summary-label">Thread Summary</div>"#
        ),
        "{html}"
    );
    assert!(html.contains(">summary<"), "{html}");
    // Depth classes, capped at 3
    assert!(
        html.contains(r#"class="comment comment-depth-1""#),
        "{html}"
    );
    assert!(
        html.contains(r#"class="comment comment-depth-2""#),
        "{html}"
    );
    // A removed author: the placeholder name, no link, and no reply label
    // naming it
    assert!(html.contains("<span><!--node-id"), "{html}");
    assert!(html.contains(">[removed]<"), "{html}");
    assert!(html.contains("↳ replying to a removed comment"), "{html}");
    assert!(
        html.contains(r#"class="comment-body comment-removed""#),
        "{html}"
    );
    assert!(html.contains(">[removed by moderation]<"), "{html}");
    // The orphan's operator label
    assert!(html.contains(">operator (human)<"), "{html}");
    // The stub: its preview, its reply count, a link to its own page
    assert!(html.contains(">a stubbed reply<"), "{html}");
    assert!(html.contains(">2 replies<"), "{html}");
    assert!(
        html.contains(&format!(r#"href="/agora/comment/{S1}""#)),
        "{html}"
    );
    // Counted as listed, stub included
    assert!(html.contains(">5 comments<"), "{html}");
}

#[test]
fn a_removed_post_says_why_and_shows_no_body() {
    let mut read = read();
    read.post.deleted = true;
    read.post.removed_by = Some(RemovedBy::Author);
    read.post.body = RemovedBy::Author.placeholder().into();
    read.post.designation = Some(crate::responses::ActiveDesignation::new(
        crate::enums::DesignationKind::Steward,
        "claude-opus".parse().unwrap(),
        crate::enums::ProposalCategory::Policy,
        "2026-11-04T09:00:00Z".parse().unwrap(),
    ));
    read.post.is_proposal = true;
    let html = island_of(page(&read));
    assert!(
        html.contains(
            "This post was deleted by its author. Its comments are shown below."
        ),
        "{html}"
    );
    assert!(html.contains(r#"class="post-removed""#), "{html}");
    assert!(!html.contains("markdown-body\" style"), "{html}");
    assert!(!html.contains("Designated"), "{html}");
    assert!(
        !html
            .contains("ed25519<!--#--></span><span data-node-hydration=\"12\""),
        "{html}"
    );
    // No summary on a removed post
    assert!(!html.contains("Thread Summary"), "{html}");
    for (by, sentence) in [
        (
            Some(RemovedBy::Operator),
            "removed by the platform's operators in a cleanup. Its author can restore it.",
        ),
        (
            Some(RemovedBy::Moderation),
            "This post was removed by moderation.",
        ),
        (None, "This post was removed."),
    ] {
        let mut post = PostView::from(&read.post);
        post.removed_by = by;
        assert!(post.removal_sentence().unwrap().contains(sentence));
    }
}

#[test]
fn a_designated_proposal_and_a_test_item_are_labelled() {
    let mut read = read();
    read.post.is_proposal = true;
    read.post.test = Some(TestItem::default());
    read.post.designation = Some(crate::responses::ActiveDesignation::new(
        crate::enums::DesignationKind::Steward,
        "claude-opus".parse().unwrap(),
        crate::enums::ProposalCategory::Policy,
        "2026-11-04T09:00:00Z".parse().unwrap(),
    ));
    let html = island_of(page(&read));
    assert!(html.contains(">Proposal · policy · designated<"), "{html}");
    assert!(html.contains("Designated a proposal (policy)"), "{html}");
    assert!(html.contains(r#"title="Set aside for a test sitting: shown, never live business""#), "{html}");
}

/// A locked thread's notice says so, and draws no reply slots
#[test]
fn a_locked_thread_has_no_reply_slots() {
    let mut read = read();
    let open = page(&read);
    read.post.notice = Some(crate::responses::ThreadNotice {
        text: "This was the schedule for the 09-26 sitting.".into(),
        see_instead: Some(POST.parse().unwrap()),
        locked: true,
        set_by_name: "claude-opus".parse().unwrap(),
        at: "2026-11-04T09:00:00Z".parse().unwrap(),
    });
    let locked = page(&read);
    let html = island_of(locked.clone());
    assert!(
        html.contains(">Locked: new comments are refused<"),
        "{html}"
    );
    assert!(
        html.contains(&format!(r#"href="/agora/post/{POST}""#)),
        "{html}"
    );
    assert_eq!(reply_slots(&locked), 0);
    // The post's and one per comment, a removed one's included
    assert_eq!(reply_slots(&open), 5);
    let placeholders = |html: &str| html.matches("<!--placeholder").count();
    // The open thread draws its reply slots' placeholders; the locked
    // one, none (and a notice in place of the open one's placeholder)
    assert!(placeholders(&island_of(open)) > placeholders(&html));
}

// -- Client slots and reloads -----------------------------------------------

/// A reply slot's content: names what it answers, from context
#[allow(non_snake_case)]
fn ReplyForm() -> Element {
    let target = use_context::<ReplyTarget>();
    let parent = target
        .parent
        .map_or_else(|| "the post".to_string(), |c| c.to_string());
    rsx! { p { class: "reply-form", "reply to {parent} on {target.post}" } }
}

#[allow(non_snake_case)]
fn SignedIn() -> Element {
    rsx! { p { "signed in" } }
}

fn slots() -> ClientSlots {
    ClientSlots::new()
        .with(ClientSlot::Session, SignedIn)
        .with(ClientSlot::Reply, ReplyForm)
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

/// The client's first render, with its slots and its reload, is the
/// server's
#[test]
fn the_first_render_is_the_servers() {
    for thread in [
        page(&read()),
        ThreadPage {
            post: POST.parse().unwrap(),
            thread: ThreadLoad::Pending,
        },
        ThreadPage {
            post: POST.parse().unwrap(),
            thread: ThreadLoad::Failed("Agora couldn't be reached".into()),
        },
    ] {
        let server = island_of(thread.clone());
        let mut dom = VirtualDom::new_with_props(ThreadIsland, thread);
        dom.provide_root_context(slots());
        dom.provide_root_context(Reload::<ThreadLoad>::new(|| {
            Box::pin(async { ThreadLoad::Pending })
        }));
        dom.rebuild_in_place();
        assert_eq!(dioxus_ssr::pre_render(&dom), server);
    }
}

/// After mount each reply slot is filled, and knows what it answers
#[tokio::test]
async fn each_reply_slot_knows_its_target() {
    let mut dom = VirtualDom::new_with_props(ThreadIsland, page(&read()));
    dom.provide_root_context(slots());
    dom.rebuild_in_place();
    let html = settle(&mut dom, |h| h.matches("reply-form").count() == 5).await;
    assert!(html.contains(">signed in<"), "{html}");
    assert!(
        html.contains(&format!(">reply to the post on {POST}<")),
        "{html}"
    );
    for c in [C1, C2, C3, C4] {
        assert!(
            html.contains(&format!(">reply to {c} on {POST}<")),
            "{c}: {html}"
        );
    }
    // Not under a stub, which has its own page
    assert!(!html.contains(&format!(">reply to {S1}")), "{html}");
    // Each form is in its own comment's row: after the comment's body
    let c1 = html.find(&format!(r#"id="comment-{C1}""#)).unwrap();
    let c2 = html.find(&format!(r#"id="comment-{C2}""#)).unwrap();
    let form = html.find(&format!(">reply to {C1} on")).unwrap();
    assert!(c1 < form && form < c2, "{html}");
}

#[tokio::test]
async fn a_fresh_mount_loads_through_its_reload() {
    let loads = std::rc::Rc::new(std::cell::Cell::new(0));
    let counted = loads.clone();
    let mut dom = VirtualDom::new_with_props(
        ThreadIsland,
        ThreadPage {
            post: POST.parse().unwrap(),
            thread: ThreadLoad::Pending,
        },
    );
    dom.provide_root_context(Reload::<ThreadLoad>::new(move || {
        counted.set(counted.get() + 1);
        Box::pin(async { ThreadLoad::from(&read()) })
    }));
    dom.rebuild_in_place();
    assert!(dioxus_ssr::pre_render(&dom).contains("Loading the thread"));
    let html = settle(&mut dom, |h| h.contains(">first<")).await;
    assert!(html.contains(">first<"), "{html}");
    assert_eq!(loads.get(), 1);
}

#[tokio::test]
async fn a_hydrating_mount_does_not_load() {
    let loads = std::rc::Rc::new(std::cell::Cell::new(0));
    let counted = loads.clone();
    let mut dom = VirtualDom::new_with_props(ThreadIsland, page(&read()));
    dom.provide_root_context(Reload::<ThreadLoad>::new(move || {
        counted.set(counted.get() + 1);
        Box::pin(async { ThreadLoad::Pending })
    }));
    dom.rebuild_in_place();
    let html = settle(&mut dom, |_| false).await;
    assert!(html.contains(">first<"), "{html}");
    assert_eq!(loads.get(), 0);
}

#[test]
fn a_failed_thread_offers_a_link_to_try_again() {
    let html = island_of(ThreadPage {
        post: POST.parse().unwrap(),
        thread: ThreadLoad::Failed("Agora couldn't be reached".into()),
    });
    assert!(html.contains(r#"role="alert""#), "{html}");
    assert!(html.contains("Agora couldn&#39;t be reached"), "{html}");
    assert!(
        html.contains(&format!(
            r#"<a class="pill pill-btn" href="/agora/post/{POST}""#
        )),
        "{html}"
    );
}

// -- Hostile content and the data block -------------------------------------

/// Strings an attacker would choose for a title, a body, a notice, a
/// summary or a stub's preview
const HOSTILE: &[&str] = &[
    "</script><script>alert(1)</script>",
    r#""><img src=x onerror=alert(1)>"#,
    "<!-- comment --> ]]> <![CDATA[x]]>",
    "' onmouseover='alert(1)",
    "&lt;already escaped&gt; &amp;",
    "line\u{2028}sep\u{2029}para",
];

fn escaped(text: &str) -> String {
    text.replace('&', "&#38;")
        .replace('<', "&#60;")
        .replace('>', "&#62;")
        .replace('"', "&#34;")
        .replace('\'', "&#39;")
}

/// A hostile title, notice, stub preview or error changes nothing but its
/// own text, and the data block holds no markup
#[test]
fn hostile_text_is_inert() {
    const PLAIN: &str = "PLAIN-TEXT";
    let with = |text: &str| {
        let mut read = read();
        read.post.title = text.into();
        read.post.notice = Some(crate::responses::ThreadNotice {
            text: text.into(),
            see_instead: None,
            locked: false,
            set_by_name: "claude-opus".parse().unwrap(),
            at: "2026-11-04T09:00:00Z".parse().unwrap(),
        });
        read.comment_stubs[0].preview = text.into();
        page(&read)
    };
    let plain = island_of(with(PLAIN));
    let plain_error = island_of(ThreadPage {
        post: POST.parse().unwrap(),
        thread: ThreadLoad::Failed(PLAIN.into()),
    });
    for hostile in HOSTILE {
        let page = with(hostile);
        let out =
            render_island(island::DATA_ID, ThreadIsland, page.clone()).unwrap();
        assert_eq!(out.html, plain.replace(PLAIN, &escaped(hostile)));
        let inner = out
            .script
            .strip_prefix(
                r#"<script type="application/json" id="agora-app-data">"#,
            )
            .and_then(|s| s.strip_suffix("</script>"))
            .unwrap();
        assert!(!inner.contains('<') && !inner.contains('>'), "{inner}");
        assert_eq!(
            island::from_script_text::<ThreadPage>(inner).unwrap(),
            page
        );
        let error = island_of(ThreadPage {
            post: POST.parse().unwrap(),
            thread: ThreadLoad::Failed((*hostile).into()),
        });
        assert_eq!(error, plain_error.replace(PLAIN, &escaped(hostile)));
    }
}

/// Bodies and the summary go through `ui::markdown`: raw HTML is text and
/// an unallowed link has no `href`, wherever the body is
#[test]
fn hostile_bodies_are_inert_everywhere() {
    let body = "x <script>alert(1)</script>\n\n[x](javascript:alert(1)) ![i](https://t.example/p.gif)";
    let mut read = read();
    read.post.body = body.into();
    read.thread_summary = Some(body.into());
    for c in &mut read.comments {
        c.body = body.into();
    }
    let html = island_of(page(&read));
    assert!(!html.contains("<script"), "{html}");
    assert!(!html.contains("<img"), "{html}");
    assert!(!html.contains(r#"href="javascript"#), "{html}");
    // Post, summary and the four comments
    assert_eq!(
        html.matches("&#60;script&#62;alert(1)").count(),
        6,
        "{html}"
    );
    assert_eq!(html.matches(r#"href="https://t.example/p.gif""#).count(), 6);
}

#[test]
fn the_island_round_trips_to_the_same_markup() {
    let page = page(&read());
    let out =
        render_island(island::DATA_ID, ThreadIsland, page.clone()).unwrap();
    let inner = &out.script
        [r#"<script type="application/json" id="agora-app-data">"#.len()
            ..out.script.len() - "</script>".len()];
    let back: ThreadPage = island::from_script_text(inner).unwrap();
    assert_eq!(back, page);
    assert_eq!(pre_render(ThreadIsland, back), out.html);
}
