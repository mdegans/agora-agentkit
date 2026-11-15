//! Client-only state in a shared component: an empty slot on the server
//! and in the client's first render, filled after mount.
//!
//! A shared root draws [`ClientOnly`] where client-only content goes (who
//! is signed in, a restored draft), naming the slot ([`ClientSlot`]). The
//! server provides no [`ClientSlots`], so it draws an empty placeholder;
//! the client's first render draws the same placeholder, whatever it
//! provides, and hydration adopts it. After mount an effect fills the slot
//! with the client's component, which then renders and runs its hooks in a
//! scope of its own.
//!
//! A client provides the slots once, as root context:
//!
//! ```ignore
//! let dom = VirtualDom::new_with_props(FeedIsland, page)
//!     .with_root_context(ClientSlots::new().with(ClientSlot::Session, SessionStatus));
//! ```
//!
//! A slot's content gets its data and shares state through context:
//!
//! - State shared between slots, or between a slot and the rest of the
//!   island (a session the island's own components also read), goes in
//!   root context beside the [`ClientSlots`]: the root is every slot's
//!   ancestor and the island's.
//! - Page data reaches a slot's content through context the shared root
//!   provides from its props, so the content needs no props of its own.
//! - Context a slot's content provides (a session provider) reaches its
//!   own children only: it is not an ancestor of the rest of the island.

use std::fmt;

use dioxus::prelude::*;

/// Where a shared component leaves room for client-only content. Named here,
/// with the components that place them, so the server and the client agree
/// on what each slot is
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ClientSlot {
    /// Who is signed in, and how to sign in or out
    Session,
}

/// What a client draws in each slot, provided as root context. The server
/// provides none
#[derive(Clone, Default)]
pub struct ClientSlots(Vec<(ClientSlot, fn() -> Element)>);

impl ClientSlots {
    pub fn new() -> Self {
        Self::default()
    }

    /// Draw `component` in `slot`, in place of anything given for it before
    pub fn with(
        mut self,
        slot: ClientSlot,
        component: fn() -> Element,
    ) -> Self {
        self.0.retain(|(s, _)| *s != slot);
        self.0.push((slot, component));
        self
    }

    /// What `slot` draws, if the client gave it anything
    pub(crate) fn get(&self, slot: ClientSlot) -> Option<fn() -> Element> {
        self.0.iter().find(|(s, _)| *s == slot).map(|(_, c)| *c)
    }
}

impl fmt::Debug for ClientSlots {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list()
            .entries(self.0.iter().map(|(s, _)| s))
            .finish()
    }
}

/// `slot`'s client content, after mount; until then, and wherever no
/// [`ClientSlots`] gives the slot anything, an empty placeholder
#[component]
pub fn ClientOnly(slot: ClientSlot) -> Element {
    let content =
        try_use_context::<ClientSlots>().and_then(|slots| slots.get(slot));
    // The first render draws the placeholder whatever the context holds,
    // because no effect runs before hydration ends: dioxus-web 0.7.10
    // calls `rebuild` and then `rehydrate` synchronously, with no poll
    // between them (`src/lib.rs`, lines 146 and 150), and effects run only
    // in its work loop after that (line 169 on). Recheck on a Dioxus bump.
    let mut mounted = use_signal(|| false);
    use_effect(move || mounted.set(true));

    match content {
        Some(component) if mounted() => rsx! { SlotContent { component } },
        _ => rsx! {},
    }
}

/// A slot's component, as a child, so its hooks have their own scope
#[component]
fn SlotContent(#[props(into)] component: SlotComponent) -> Element {
    (component.0)()
}

/// A slot's component, compared by address
#[derive(Clone, Copy)]
struct SlotComponent(fn() -> Element);

impl PartialEq for SlotComponent {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::fn_addr_eq(self.0, other.0)
    }
}

impl From<fn() -> Element> for SlotComponent {
    fn from(component: fn() -> Element) -> Self {
        Self(component)
    }
}

#[cfg(all(test, feature = "dioxus-ssr"))]
mod tests;
