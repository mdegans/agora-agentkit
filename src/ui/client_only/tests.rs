use std::time::Duration;

use super::*;
use crate::ui::{
    feed::{FeedIsland, FeedLoad, FeedPage},
    ssr::pre_render,
};

/// A slot's content with a hook of its own
#[allow(non_snake_case)]
fn SignedIn() -> Element {
    let name = use_signal(|| "litmus");
    rsx! { p { class: "session", "Signed in as {name}" } }
}

#[allow(non_snake_case)]
fn Other() -> Element {
    rsx! { p { "other" } }
}

fn page() -> FeedPage {
    FeedPage {
        feed: FeedLoad::Loaded(vec![]),
    }
}

/// A root mounted as the client mounts it, `slots` in root context,
/// before any effect has run: its first render
fn first_render<P: Clone + 'static, M: 'static>(
    root: impl dioxus::dioxus_core::ComponentFunction<P, M>,
    props: P,
    slots: Option<ClientSlots>,
) -> (VirtualDom, String) {
    let mut dom = VirtualDom::new_with_props(root, props);
    if let Some(slots) = slots {
        dom.provide_root_context(slots);
    }
    dom.rebuild_in_place();
    let html = dioxus_ssr::pre_render(&dom);
    (dom, html)
}

/// Run the effects and what they schedule
async fn settle(dom: &mut VirtualDom) -> String {
    for _ in 0..5 {
        let _ = tokio::time::timeout(
            Duration::from_millis(20),
            dom.wait_for_work(),
        )
        .await;
        dom.render_immediate(&mut dioxus::dioxus_core::NoOpMutations);
    }
    dioxus_ssr::pre_render(dom)
}

fn session() -> ClientSlots {
    ClientSlots::new().with(ClientSlot::Session, SignedIn)
}

/// The server's placeholder, exactly
#[test]
fn the_server_draws_one_placeholder() {
    let html = pre_render(
        ClientOnly,
        ClientOnlyProps {
            slot: ClientSlot::Session,
        },
    );
    assert_eq!(html, "<!--placeholder0-->");
}

/// The client's first render is the server's, whatever slots it provides
#[test]
fn the_first_render_is_the_servers() {
    let props = ClientOnlyProps {
        slot: ClientSlot::Session,
    };
    let server = pre_render(ClientOnly, props.clone());
    for slots in [None, Some(ClientSlots::new()), Some(session())] {
        assert_eq!(
            first_render(ClientOnly, props.clone(), slots.clone()).1,
            server,
            "{slots:?}"
        );
    }
    let server = pre_render(FeedIsland, page());
    assert!(
        server.starts_with("<!--placeholder0--><section "),
        "{server}"
    );
    for slots in [None, Some(ClientSlots::new()), Some(session())] {
        assert_eq!(
            first_render(FeedIsland, page(), slots.clone()).1,
            server,
            "{slots:?}"
        );
    }
}

/// After mount, the slot is filled where the client gave it content
#[tokio::test]
async fn after_mount_the_slot_is_filled() {
    let (mut dom, first) = first_render(FeedIsland, page(), Some(session()));
    assert!(!first.contains("Signed in"), "{first}");
    let later = settle(&mut dom).await;
    assert!(
        later.starts_with(
            r#"<p class="session" data-node-hydration="0"><!--node-id1-->Signed in as litmus<!--#--></p><section "#
        ),
        "{later}"
    );
    // The rest of the island is as it was
    assert!(later.contains("No posts yet"), "{later}");
}

/// No slots, or none for this slot: nothing, before and after mount
#[tokio::test]
async fn without_content_the_slot_stays_empty() {
    let server = pre_render(FeedIsland, page());
    for slots in [None, Some(ClientSlots::new())] {
        let (mut dom, _) = first_render(FeedIsland, page(), slots.clone());
        assert_eq!(settle(&mut dom).await, server, "{slots:?}");
    }
}

#[test]
fn a_slot_given_twice_draws_the_last() {
    let slots = ClientSlots::new()
        .with(ClientSlot::Session, Other)
        .with(ClientSlot::Session, SignedIn);
    let got = slots.get(ClientSlot::Session).unwrap();
    assert!(std::ptr::fn_addr_eq(got, SignedIn as fn() -> Element));
    assert_eq!(format!("{slots:?}"), "[Session]");
    assert!(ClientSlots::new().get(ClientSlot::Session).is_none());
}

/// The swap after mount replaces the placeholder hydration adopted (by its
/// id), with one node, and touches nothing else
#[tokio::test]
async fn the_swap_replaces_the_hydrated_placeholder() {
    use dioxus::dioxus_core::Mutation;

    let mut dom = VirtualDom::new_with_props(
        ClientOnly,
        ClientOnlyProps {
            slot: ClientSlot::Session,
        },
    );
    dom.provide_root_context(session());
    let first = dom.rebuild_to_vec();
    let placeholders: Vec<_> = first
        .edits
        .iter()
        .filter_map(|edit| match edit {
            Mutation::CreatePlaceholder { id } => Some(*id),
            _ => None,
        })
        .collect();
    let [placeholder] = placeholders[..] else {
        panic!("{:?}", first.edits);
    };
    let _ =
        tokio::time::timeout(Duration::from_millis(20), dom.wait_for_work())
            .await;
    let after = dom.render_immediate_to_vec();
    let replaced: Vec<_> = after
        .edits
        .iter()
        .filter(|edit| matches!(edit, Mutation::ReplaceWith { .. }))
        .collect();
    assert!(
        matches!(replaced[..], [Mutation::ReplaceWith { id, m: 1 }] if *id == placeholder),
        "{:?}",
        after.edits
    );
}
