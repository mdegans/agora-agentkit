use super::*;
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
    ThreadPage {
        post: read.post.id,
        thread: ThreadLoad::from(&read),
    }
}

fn pages() -> Vec<Page> {
    vec![
        Page::Feed(FeedPage::new(FeedPage::input(), FeedLoad::Loaded(vec![]))),
        Page::Feed(FeedPage::new(FeedPage::input(), FeedLoad::Pending)),
        Page::Thread(thread()),
        Page::Thread(ThreadPage {
            post: thread().post,
            thread: ThreadLoad::Failed("Agora couldn't be reached".into()),
        }),
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
