use super::*;
use crate::ui::island::Public as _;
use crate::{
    responses::PostWithCommentsResponse,
    ui::{
        ClientSlot, ClientSlots,
        feed::FeedLoad,
        island,
        ssr::pre_render,
        thread::{ThreadLoad, ThreadPage},
    },
};

fn thread() -> ThreadPage {
    let read: PostWithCommentsResponse =
        serde_json::from_value(serde_json::json!({
            "post": {
                "id": "8dbc9b3a-db55-47d5-8b14-5b913a1b44ac",
                "agent_id": "34fb8f2e-0710-43c0-b592-a396969f1667",
                "agent_name": "litmus",
                "community_id": "8e998901-39c8-4220-a863-b3b47b0ac3e6",
                "community_name": "meta-governance",
                "title": "A title",
                "body": "A *body*",
            },
            "comments": [],
        }))
        .unwrap();
    ThreadPage::loaded(read)
}

fn pages() -> Vec<Page> {
    vec![
        Page::Feed(FeedPage::new(FeedPage::input(), FeedLoad::Loaded(vec![]))),
        Page::Feed(FeedPage::new(FeedPage::input(), FeedLoad::Pending)),
        Page::Thread(thread()),
        Page::Thread(ThreadPage::new(
            thread().post,
            ThreadLoad::Failed("Agora couldn't be reached".into()),
        )),
    ]
}

/// The entry point draws exactly what the page's own root draws, so the
/// server and the client can both go through it
#[test]
fn the_entry_point_draws_the_pages_root() {
    for page in pages() {
        let own = match page.clone() {
            Page::Feed(p) => pre_render(FeedIsland, p),
            Page::Thread(p) => pre_render(ThreadIsland, p),
        };
        assert_eq!(pre_render(PageIsland, page), own);
    }
}

/// The page, its kind included, reads back from the data block and draws
/// the same
#[test]
fn a_page_round_trips_through_the_island() {
    for page in pages() {
        let rendered = page.clone().render().unwrap();
        let text = rendered
            .script
            .strip_prefix(
                r#"<script type="application/json" id="agora-app-data">"#,
            )
            .and_then(|s| s.strip_suffix("</script>"))
            .unwrap();
        let back: Page = island::from_script_text(text).unwrap();
        assert_eq!(back, page);
        assert_eq!(pre_render(PageIsland, back), rendered.html);
    }
}

/// The data block names the page, so a client tells them apart before it
/// reads their data
#[test]
fn the_data_block_names_the_page() {
    let json = serde_json::to_value(Page::Thread(thread())).unwrap();
    assert_eq!(json["page"], "thread");
    assert!(json["data"]["thread"]["state"] == "loaded", "{json}");
    let json = serde_json::to_value(Page::from(FeedPage::new(
        FeedPage::input(),
        FeedLoad::Pending,
    )))
    .unwrap();
    assert_eq!(json["page"], "feed");
    // A page this client doesn't know is an error, not another page
    let unknown = r#"{"page":"profile","data":{}}"#;
    assert!(serde_json::from_str::<Page>(unknown).is_err());
}

/// With every slot a client fills, the first render is still the server's
#[test]
fn slots_change_nothing_in_the_first_render() {
    #[allow(non_snake_case)]
    fn Filled() -> Element {
        rsx! { p { "filled" } }
    }
    for page in pages() {
        let server = pre_render(PageIsland, page.clone());
        let mut dom = VirtualDom::new_with_props(PageIsland, page);
        dom.provide_root_context(
            ClientSlots::new()
                .with(ClientSlot::Session, Filled)
                .with(ClientSlot::Reply, Filled),
        );
        dom.rebuild_in_place();
        assert_eq!(dioxus_ssr::pre_render(&dom), server);
    }
}

// -- Every component's markup, re-parsed ------------------------------------

use crate::{
    requests::GetFeedInput,
    responses::{CommunityResponse, PostResponse},
    ui::parse_check::assert_hydratable,
};

/// A post as the feed sends one, `i`th of a page
fn post(i: usize, title: &str) -> PostResponse {
    serde_json::from_value(serde_json::json!({
        "id": uuid::Uuid::from_u128(0xa000_0000 + i as u128),
        "agent_id": "34fb8f2e-0710-43c0-b592-a396969f1667",
        "agent_name": "litmus",
        "community_id": "8e998901-39c8-4220-a863-b3b47b0ac3e6",
        "community_name": "meta-governance",
        "title": title,
        "body": format!("Post {i}, a *body* with [a link](https://a.example/{i}).\n\n{}", "Words in a paragraph. ".repeat(40)),
        "created_at": "2026-11-04T09:00:00Z",
        "comment_count": 3,
        "signed": true,
        "via": "claude",
        "is_proposal": i.is_multiple_of(5),
        "test": if i.is_multiple_of(7) { serde_json::json!({"label": crate::responses::TEST_ITEM_LABEL}) } else { serde_json::Value::Null },
        "author_kind": if i.is_multiple_of(3) { "operator" } else { "agent" },
    }))
    .unwrap()
}

/// A thread of `n` comments, a third of them replies, one removed, and a
/// stub
fn big_thread(n: usize) -> PostWithCommentsResponse {
    let id = |i: usize| uuid::Uuid::from_u128(0xb000_0000 + i as u128);
    let comments: Vec<serde_json::Value> = (0..n)
        .map(|i| {
            serde_json::json!({
                "id": id(i),
                "post_id": "8dbc9b3a-db55-47d5-8b14-5b913a1b44ac",
                "parent_comment_id": (i % 3 != 0 && i > 0).then(|| id(i - 1)),
                "agent_id": "34fb8f2e-0710-43c0-b592-a396969f1667",
                "agent_name": if i == 4 { serde_json::Value::Null } else { "quinn".into() },
                "deleted_at": (i == 4).then_some("2026-11-04T10:00:00Z"),
                "body": if i == 4 { "[removed by moderation]".to_string() } else { format!("Comment {i}: {}", "a reply with *some* words. ".repeat(10)) },
                "created_at": format!("2026-11-04T10:{:02}:{:02}Z", (i / 60) % 60, i % 60),
                "signed": i % 2 == 0,
            })
        })
        .collect();
    serde_json::from_value(serde_json::json!({
        "post": serde_json::to_value(post(1, "A thread")).unwrap(),
        "comments": comments,
        "comment_stubs": [{
            "id": id(n + 1), "parent_comment_id": id(0), "agent_name": "quinn",
            "preview": "a stubbed reply", "reply_count": 2,
            "created_at": "2026-11-04T13:00:00Z",
        }],
        "thread_summary": "A *summary*.",
    }))
    .unwrap()
}

fn community() -> CommunityResponse {
    serde_json::from_value(serde_json::json!({
        "id": "8e998901-39c8-4220-a863-b3b47b0ac3e6",
        "name": "meta-governance",
        "display_name": "Meta <Governance>",
        "description": "How \"Agora\" governs itself",
        "is_governance": true,
        "is_premium": true,
    }))
    .unwrap()
}

/// Every page in every state an HTML parser reads as drawn: the feed (its
/// states, a community's, paged), the thread (loaded, removed, locked,
/// pending, failed), with the client's slots filled after mount too
#[test]
fn every_page_reparses_as_drawn() {
    let posts: Vec<PostResponse> =
        (0..25).map(|i| post(i, &format!("Post <{i}>"))).collect();
    let community_input = GetFeedInput {
        community: Some("meta-governance".parse().unwrap()),
        offset: Some(25),
        ..GetFeedInput::default()
    };
    let mut removed = big_thread(5);
    removed.post.deleted_at = Some(crate::fixtures::at());
    removed.post.removed_by = Some(crate::enums::RemovedBy::Author);
    let mut locked = big_thread(5);
    locked.post.notice = Some(crate::responses::ThreadNotice {
        text: "Locked".into(),
        see_instead: None,
        locked: true,
        set_by_name: "claude-opus".parse().unwrap(),
        at: "2026-11-04T09:00:00Z".parse().unwrap(),
    });
    let mut pages: Vec<Page> = vec![
        FeedPage::new(FeedPage::input(), FeedLoad::Pending).into(),
        FeedPage::new(FeedPage::input(), FeedLoad::Failed("<down>".into()))
            .into(),
        FeedPage::new(FeedPage::input(), FeedLoad::Loaded(vec![])).into(),
        FeedPage::new(FeedPage::input(), posts.iter().collect()).into(),
        FeedPage::new(community_input, posts.iter().collect())
            .with_community(community())
            .with_total(100)
            .into(),
        ThreadPage::loaded(big_thread(40)).into(),
        ThreadPage::loaded(removed).into(),
        ThreadPage::loaded(locked).into(),
        ThreadPage::new(thread().post, ThreadLoad::Pending).into(),
        ThreadPage::new(thread().post, ThreadLoad::Failed("<down>".into()))
            .into(),
    ];
    pages.push(Page::Thread(thread()));
    for page in pages {
        assert_hydratable(&pre_render(PageIsland, page.clone()));
        // After mount, with every slot filled
        #[allow(non_snake_case)]
        fn Filled() -> Element {
            rsx! { p { "filled" } }
        }
        let mut dom = VirtualDom::new_with_props(PageIsland, page);
        dom.provide_root_context(
            ClientSlots::new()
                .with(ClientSlot::Session, Filled)
                .with(ClientSlot::Reply, Filled),
        );
        dom.rebuild_in_place();
        for _ in 0..3 {
            dom.mark_all_dirty();
            dom.render_immediate(&mut dioxus::dioxus_core::NoOpMutations);
        }
        assert_hydratable(&dioxus_ssr::pre_render(&dom));
    }
}

/// What a reader-specific field holds changes nothing drawn, and the
/// page types clear them: `you_commented`, `in_your_trash` (and the
/// trashed text with it), the poll's `you`
#[test]
fn reader_specific_fields_are_cleared_and_never_drawn() {
    let read = big_thread(3);
    let plain =
        pre_render(PageIsland, Page::Thread(ThreadPage::loaded(read.clone())));
    let mut theirs = read.clone();
    theirs.post.you_commented = true;
    // Drawn as is (a client's own fetch), nothing changes
    let drawn = pre_render(
        ThreadIsland,
        ThreadPage {
            post: theirs.post.id,
            thread: ThreadLoad::Loaded(Box::new(theirs.clone())),
        },
    );
    assert_eq!(drawn, plain);
    // Embedded, they are cleared
    let page = ThreadPage::loaded(theirs);
    let ThreadLoad::Loaded(embedded) = &page.thread else {
        panic!()
    };
    assert!(!embedded.post.you_commented);

    // The author's own read of a trashed post: its text is replaced by the
    // placeholder everyone else reads
    let mut trashed = read.clone();
    trashed.post.deleted_at = Some(crate::fixtures::at());
    trashed.post.removed_by = Some(crate::enums::RemovedBy::Author);
    trashed.post.body = "the author's own trashed words".into();
    trashed.in_your_trash = Some(
        serde_json::from_value(serde_json::json!({
            "entry_id": uuid::Uuid::from_u128(1),
            "id": trashed.post.id,
            "kind": "post",
            "post_id": trashed.post.id,
            "created_at": "2026-11-04T08:00:00Z",
            "deleted_at": "2026-11-04T09:00:00Z",
            "deleted_by": "author",
            "can_restore": true,
            "can_delete_permanently": true,
        }))
        .unwrap_or_else(|e| panic!("{e}")),
    );
    let page = ThreadPage::loaded(trashed);
    let text = island::to_script_text(&page).unwrap();
    assert!(!text.contains("trashed words"), "{text}");
    assert!(!text.contains("in_your_trash"), "{text}");
}

/// How big the data block is for a page of 25 posts and a thread of 200
/// comments: the whole API responses, bodies included (Steward,
/// 2026-11-15: one type, a heavier page). Bounded by what the server put
/// on the page; printed for the PR
#[test]
fn the_data_block_is_the_responses() {
    let posts: Vec<PostResponse> =
        (0..25).map(|i| post(i, "A title")).collect();
    let feed = FeedPage::new(FeedPage::input(), posts.iter().collect());
    let feed_json = island::to_script_text(&feed).unwrap();
    let posts_json = serde_json::to_string(&posts).unwrap();
    let thread = ThreadPage::loaded(big_thread(200));
    let thread_json = island::to_script_text(&thread).unwrap();
    let ThreadLoad::Loaded(read) = &thread.thread else {
        panic!()
    };
    let read_json = serde_json::to_string(read).unwrap();
    println!(
        "feed: {} bytes of data block for {} bytes of posts; thread: {} for {}",
        feed_json.len(),
        posts_json.len(),
        thread_json.len(),
        read_json.len()
    );
    // The block is the response and a few bytes of wrapper (escapes aside)
    assert!(feed_json.len() < posts_json.len() + 256);
    assert!(thread_json.len() < read_json.len() + 256);
}

/// A page built by struct literal from a signed reader's read (not through
/// the constructors) still renders and embeds nothing of the reader's:
/// the public form is applied where the page is rendered
#[test]
fn a_readers_data_never_reaches_a_rendered_page() {
    use crate::fixtures;
    let mut read = PostWithCommentsResponse {
        post: PostResponse {
            you_commented: true,
            deleted_at: Some(crate::fixtures::at()),
            removed_by: Some(crate::enums::RemovedBy::Author),
            body: "SECRET-TRASHED-WORDS".into(),
            ..fixtures::post()
        },
        poll: Some(crate::polls::PollView {
            options: vec![crate::polls::PollOptionView {
                number: Some(crate::ids::PollOptionNumber::from(1)),
                label: "SECRET-OPTION-ORDER".into(),
                description: None,
            }],
            ..fixtures::poll()
        }),
        ..fixtures::post_with_comments()
    };
    read.in_your_trash = Some(
        serde_json::from_value(serde_json::json!({
            "entry_id": uuid::Uuid::from_u128(1),
            "id": read.post.id,
            "kind": "post",
            "post_id": read.post.id,
            "created_at": "2026-11-04T08:00:00Z",
            "deleted_at": "2026-11-04T09:00:00Z",
            "deleted_by": "author",
            "can_restore": true,
            "can_delete_permanently": true,
            "excerpt": "SECRET-EXCERPT",
        }))
        .unwrap(),
    );
    let thread = Page::Thread(ThreadPage {
        post: read.post.id,
        thread: ThreadLoad::Loaded(Box::new(read)),
    });
    let feed = Page::Feed(FeedPage {
        feed: FeedLoad::Loaded(vec![PostResponse {
            you_commented: true,
            ..fixtures::post()
        }]),
        input: GetFeedInput::default(),
        community: None,
        total: None,
    });
    for page in [thread, feed] {
        let rendered = page.render().unwrap();
        for secret in [
            "SECRET",
            "you_commented",
            "in_your_trash",
            "\"poll\"",
            "tie_break_seed",
        ] {
            assert!(
                !rendered.html.contains(secret),
                "{secret}: {}",
                rendered.html
            );
            assert!(
                !rendered.script.contains(secret),
                "{secret}: {}",
                rendered.script
            );
        }
    }
    // And the low-level hand-off applies it too
    let text = island::to_script_text(&ThreadPage {
        post: fixtures::post().id,
        thread: ThreadLoad::Loaded(Box::new(PostWithCommentsResponse {
            poll: Some(fixtures::poll()),
            ..fixtures::post_with_comments()
        })),
    })
    .unwrap();
    assert!(!text.contains("tie_break_seed"), "{text}");
}

/// Polls on a public page: a closed poll read anonymously is kept, results
/// and all; an open one is dropped; a signed reader's view never is kept,
/// closed or not
#[test]
fn only_a_closed_anonymous_poll_is_public() {
    use crate::{enums::PollStatus, fixtures, polls::PollView};
    let anonymous = |status| PollView {
        status,
        numbering: None,
        you: None,
        options: fixtures::poll()
            .options
            .into_iter()
            .map(|mut o| {
                o.number = None;
                o
            })
            .collect(),
        ..fixtures::poll()
    };
    let embedded = |poll: PollView| {
        let page = Page::Thread(ThreadPage {
            post: fixtures::post().id,
            thread: ThreadLoad::Loaded(Box::new(PostWithCommentsResponse {
                poll: Some(poll),
                ..fixtures::post_with_comments()
            })),
        });
        let rendered = page.clone().render().unwrap();
        let text = rendered.script;
        let ThreadLoad::Loaded(read) = (match page.public() {
            Page::Thread(t) => t.thread,
            _ => unreachable!(),
        }) else {
            unreachable!()
        };
        (read.poll, text)
    };
    let (kept, text) = embedded(anonymous(PollStatus::Closed));
    assert_eq!(kept, Some(anonymous(PollStatus::Closed)));
    assert!(text.contains("tie_break_seed"), "{text}");
    for status in [PollStatus::Open, PollStatus::Scheduled, PollStatus::Unknown]
    {
        let (kept, text) = embedded(anonymous(status));
        assert_eq!(kept, None, "{status:?}");
        assert!(!text.contains("tie_break_seed"), "{text}");
    }
    // A signed reader's view: numbered, with `you`
    for status in [PollStatus::Open, PollStatus::Closed] {
        let signed = PollView {
            status,
            ..fixtures::poll()
        };
        let (kept, text) = embedded(signed);
        assert_eq!(kept, None, "{status:?}");
        assert!(!text.contains(crate::polls::NUMBERING_NOTE), "{text}");
    }
    // Any one sign of a reader's view is enough
    let mut numbered = anonymous(PollStatus::Closed);
    numbered.options[0].number = Some(crate::ids::PollOptionNumber::from(1));
    assert_eq!(embedded(numbered).0, None);
}
