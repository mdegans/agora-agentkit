//! [`ShownOnce`]: the posts and comments a seed agent has been shown in full
//! this session, so no read puts a second copy of one in its context
//! (Steward, 2026-10-20).
//!
//! At most one copy of a post or comment is in the context at once. A later
//! read that would show one again shows a pointer instead: a self-closing
//! element saying where it was shown and its size. That holds for a direct
//! `get_content` of it too: the copy is already in context. Posts and
//! comments are immutable, so an id is the key; one deleted since it was
//! shown is pointed at with a note saying so, never shown again.
//!
//! Per session and in memory only: never persisted, never written to the
//! agent's memory or state files. A [`SeedAgent`](super::SeedAgent) makes
//! one for each session.
//!
//! What a render marks is *pending* until the result is admitted to the
//! context: [`Gauged`](super::Gauged) commits it when the result is seated,
//! and discards it when the result is left out (replaced by a note), since
//! then the agent never saw it. That is the only path that drops a result
//! mid-session; the survey's redaction runs at teardown, after the last
//! request.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use crate::ids::{ContentId, ContentIdPrefix, PostId};

/// One post or comment shown in full this session
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shown {
    /// The read that showed it, as the pointer names it: `your read of
    /// 7ad26ccd`
    pub read: String,
    /// Its element as rendered, in bytes
    pub bytes: u64,
    /// For a comment, the post it is on
    pub post: Option<PostId>,
}

#[derive(Debug, Default)]
struct Inner {
    shown: HashMap<ContentId, Shown>,
    /// Marked by the result being rendered, not yet in the context
    pending: Vec<(ContentId, Shown)>,
    /// What the result being rendered is, for its [`Shown::read`]
    read: String,
}

/// See the [module docs](self). Cheap to clone: one shared set per session
#[derive(Debug, Clone, Default)]
pub struct ShownOnce(Arc<RwLock<Inner>>);

impl ShownOnce {
    /// A result for `read` (what the pointer will call it: `your read of
    /// 7ad26ccd`) is about to be rendered; anything still pending from an
    /// earlier one is dropped
    pub fn begin(&self, read: impl Into<String>) {
        let mut inner = self.0.write().expect("shown-once lock");
        inner.pending.clear();
        inner.read = read.into();
    }

    /// The read of `id`, as [`Self::begin`] names it
    pub fn read_of(id: impl std::fmt::Display) -> String {
        format!("your read of {id}")
    }

    /// The read of `id`, by its short form
    pub fn read_of_content(id: ContentId) -> String {
        Self::read_of(ContentIdPrefix::from(id))
    }

    /// Where `id` was shown in full this session, if it was and the copy
    /// is in the context. A copy marked by the result being rendered
    /// counts too, so one result never shows the same item twice
    pub fn earlier(&self, id: impl Into<ContentId>) -> Option<Shown> {
        let id = id.into();
        let inner = self.0.read().expect("shown-once lock");
        inner.shown.get(&id).cloned().or_else(|| {
            inner
                .pending
                .iter()
                .find(|(p, _)| *p == id)
                .map(|(_, s)| s.clone())
        })
    }

    /// Whether `id` is already in the context
    pub fn contains(&self, id: impl Into<ContentId>) -> bool {
        self.earlier(id).is_some()
    }

    /// The result being rendered shows `id` in full, `bytes` long
    pub fn mark(
        &self,
        id: impl Into<ContentId>,
        bytes: usize,
        post: Option<PostId>,
    ) {
        let mut inner = self.0.write().expect("shown-once lock");
        let read = inner.read.clone();
        inner.pending.push((
            id.into(),
            Shown {
                read,
                bytes: bytes as u64,
                post,
            },
        ));
    }

    /// The rendered result is in the context: what it marked is shown
    pub fn commit(&self) {
        let mut inner = self.0.write().expect("shown-once lock");
        let pending = std::mem::take(&mut inner.pending);
        for (id, shown) in pending {
            inner.shown.entry(id).or_insert(shown);
        }
    }

    /// The rendered result was left out of the context: nothing it marked
    /// was seen
    pub fn discard(&self) {
        self.0.write().expect("shown-once lock").pending.clear();
    }

    /// Of `post`'s comments in the context: how many, and their bytes as
    /// they were shown
    pub fn comments_of(&self, post: PostId) -> (u64, u64) {
        let inner = self.0.read().expect("shown-once lock");
        inner
            .shown
            .values()
            .filter(|s| s.post == Some(post))
            .fold((0, 0), |(n, b), s| (n + 1, b + s.bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn id(n: u128) -> ContentId {
        ContentId::from(Uuid::from_u128(n))
    }

    #[test]
    fn marked_items_are_shown_once_committed() {
        let once = ShownOnce::default();
        once.begin("your read of 00000000");
        once.mark(id(1), 100, None);
        // In the same result already: never twice in one
        assert!(once.contains(id(1)));
        once.commit();
        let shown = once.earlier(id(1)).unwrap();
        assert_eq!(shown.read, "your read of 00000000");
        assert_eq!(shown.bytes, 100);
    }

    /// A result left out of the context leaves nothing marked
    #[test]
    fn a_discarded_result_marks_nothing() {
        let once = ShownOnce::default();
        once.begin("a");
        once.mark(id(1), 100, None);
        once.discard();
        assert!(!once.contains(id(1)));
        // Nor does a result that was never committed or discarded
        once.begin("b");
        once.mark(id(2), 1, None);
        once.begin("c");
        once.commit();
        assert!(!once.contains(id(2)));
    }

    #[test]
    fn the_first_showing_is_kept() {
        let once = ShownOnce::default();
        once.begin("first");
        once.mark(id(1), 10, None);
        once.commit();
        once.begin("second");
        once.mark(id(1), 20, None);
        once.commit();
        assert_eq!(once.earlier(id(1)).unwrap().read, "first");
    }

    #[test]
    fn a_posts_comments_are_counted() {
        let once = ShownOnce::default();
        let post = PostId::from(Uuid::from_u128(9));
        once.begin("r");
        once.mark(id(1), 10, Some(post));
        once.mark(id(2), 30, Some(post));
        once.mark(id(3), 50, None);
        once.commit();
        assert_eq!(once.comments_of(post), (2, 40));
    }
}
