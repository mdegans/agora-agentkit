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

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

use crate::ids::{ContentId, ContentIdPrefix, GovernanceLogId, PostId};

/// One post or comment shown in full this session
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shown {
    /// The read that showed it, as the pointer names it: `your read of
    /// 7ad26ccd`
    pub read: String,
    /// Its text as shown between its element's tags, in bytes: the one
    /// size its pointer and every listing give it
    pub bytes: u64,
    /// Its raw body's bytes, as the server counts them in a post's
    /// `size`, so a listing can take what is in the context out of a size
    /// the server sent
    pub raw: u64,
    /// For a comment, the post it is on
    pub post: Option<PostId>,
    /// The turn that showed it ([`ShownOnce::turn`])
    pub turn: u64,
}

#[derive(Debug, Default)]
struct Inner {
    shown: HashMap<ContentId, Shown>,
    /// Marked by the result being rendered, not yet in the context
    pending: Vec<(ContentId, Shown)>,
    /// What the result being rendered is, for its [`Shown::read`]
    read: String,
    /// The notes on how to read a result (what the elements are, what a
    /// pointer is) shown this session: each once, like the items
    legends: HashSet<&'static str>,
    /// Shown by the result being rendered
    pending_legends: Vec<&'static str>,
    /// Governance entries' summaries shown this session (betas of 0.108:
    /// get_council_meetings showed GOV-2026-0017's, and a summary read
    /// showed it again)
    /// By id and the text's [`text_key`]: one text is one summary across
    /// versions, and a revised record's two texts are two (betas of 0.113.0)
    summaries: HashMap<(GovernanceLogId, u64), Shown>,
    pending_summaries: Vec<((GovernanceLogId, u64), Shown)>,
    /// The same, by id and version (`latest` for a record with one text),
    /// for a part read, which carries no summary to key by
    summaries_at: HashMap<(GovernanceLogId, &'static str), Shown>,
    pending_summaries_at: Vec<((GovernanceLogId, &'static str), Shown)>,
    /// Parts of governance records shown this session (an attachment, a
    /// data set's file or lines of one), by the read's canonical link
    /// (0.112.0, beta round)
    parts: HashMap<String, Shown>,
    pending_parts: Vec<(String, Shown)>,
    /// How the result being rendered points at further reads (0.112.0)
    cues: Cues,
    /// The agent's turns this session, counted by the tool's `on_turn`
    turn: u64,
}

/// How a result points at further reads: in `agora://` links when it was
/// asked for by one, and with the full governance reads left, where a read
/// it points to would spend one (beta round of 0.112.0). Set by the tool
/// before it renders; not a record of what was shown
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Cues {
    /// The result was asked for by `fetch_agora_link`: its pointers are
    /// links
    pub links: bool,
    /// Full governance reads left this session, when the tool counts them
    pub full_reads_left: Option<u64>,
    /// The read asked for a summary by name (`detail=summary`): it is shown,
    /// even if it was shown before
    pub summary_asked: bool,
}

/// A key for `text` by its content, so the same text is one key whatever
/// read or version showed it (betas of 0.113.0). `DefaultHasher::new` is
/// keyed the same in every process; nothing here outlives the session
pub fn text_key(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.trim().hash(&mut h);
    h.finish()
}

/// A summary as shown once: without the "Read this first" sentence a
/// data set's summary carries or not, as its read has a listing or not
/// (betas of 0.113.0: one summary was two, and its two pointers two sizes)
pub fn summary_text(text: &str) -> String {
    const OPEN: &str = "Read this first: attachment=\"";
    let Some(at) = text.find(OPEN) else {
        return text.trim().to_string();
    };
    let Some(end) = text[at + OPEN.len()..].find("\".") else {
        return text.trim().to_string();
    };
    let end = at + OPEN.len() + end + 2;
    format!("{}{}", text[..at].trim_end(), &text[end..])
        .trim()
        .to_string()
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
        inner.pending_legends.clear();
        inner.pending_summaries.clear();
        inner.pending_summaries_at.clear();
        inner.pending_parts.clear();
        inner.read = read.into();
    }

    /// Where the part of a record `link` reads was shown this session
    pub fn earlier_part(&self, link: &str) -> Option<Shown> {
        let inner = self.0.read().expect("shown-once lock");
        inner.parts.get(link).cloned().or_else(|| {
            inner
                .pending_parts
                .iter()
                .find(|(p, _)| p == link)
                .map(|(_, s)| s.clone())
        })
    }

    /// The result being rendered shows the part `link` reads, `bytes` long
    pub fn mark_part(&self, link: String, bytes: usize) {
        let mut inner = self.0.write().expect("shown-once lock");
        let read = inner.read.clone();
        let turn = inner.turn;
        inner.pending_parts.push((
            link,
            Shown {
                read,
                bytes: bytes as u64,
                raw: bytes as u64,
                post: None,
                turn,
            },
        ));
    }

    /// A new turn of the agent's begins
    pub fn next_turn(&self) {
        self.0.write().expect("shown-once lock").turn += 1;
    }

    /// The turn this is, from 0
    pub fn turn(&self) -> u64 {
        self.0.read().expect("shown-once lock").turn
    }

    /// How the result about to be rendered points at further reads
    pub fn set_cues(&self, cues: Cues) {
        self.0.write().expect("shown-once lock").cues = cues;
    }

    /// See [`Cues`]
    pub fn cues(&self) -> Cues {
        self.0.read().expect("shown-once lock").cues
    }

    /// Where `id`'s summary `text` was shown this session, if it was; with
    /// no text, where any of its summaries was
    pub fn earlier_summary(
        &self,
        id: &GovernanceLogId,
        text: Option<&str>,
    ) -> Option<Shown> {
        let key = text.map(|t| text_key(&summary_text(t)));
        let wanted = |k: &(GovernanceLogId, u64)| {
            k.0 == *id && key.is_none_or(|key| k.1 == key)
        };
        let inner = self.0.read().expect("shown-once lock");
        inner
            .pending_summaries
            .iter()
            .find(|(k, _)| wanted(k))
            .map(|(_, s)| s.clone())
            .or_else(|| {
                inner
                    .summaries
                    .iter()
                    .find(|(k, _)| wanted(k))
                    .map(|(_, s)| s.clone())
            })
    }

    /// How much of `id`'s summary `text` was shown this session: the
    /// longest run of its leading paragraphs a read showed, and where.
    /// Never more than was shown (betas of 0.113.0): the rest is the
    /// reader's to read. Its offset is into [`summary_text`]`(text)`
    pub fn summary_seen(
        &self,
        id: &GovernanceLogId,
        text: &str,
    ) -> Option<(Shown, usize)> {
        let t = summary_text(text);
        let mut ends: Vec<usize> = t
            .match_indices("\n\n")
            .map(|(at, _)| at)
            .chain(std::iter::once(t.len()))
            .collect();
        ends.reverse();
        ends.into_iter().find_map(|end| {
            self.earlier_summary(id, Some(&t[..end])).map(|s| (s, end))
        })
    }

    /// Where `id`'s summary at `version` was shown this session, if it was
    pub fn earlier_summary_at(
        &self,
        id: &GovernanceLogId,
        version: &'static str,
    ) -> Option<Shown> {
        let key = (id.clone(), version);
        let inner = self.0.read().expect("shown-once lock");
        inner
            .pending_summaries_at
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, s)| s.clone())
            .or_else(|| inner.summaries_at.get(&key).cloned())
    }

    /// The result being rendered shows `id`'s summary `text`. Each of its
    /// leading paragraphs counts as shown too: a served summary is the
    /// signed one with the server's notes after it, and a record's own
    /// copy is the signed one alone (betas of 0.113.0)
    /// `version` is which of a revised record's texts it is (`None` for
    /// one text, or the latest)
    pub fn mark_summary(
        &self,
        id: GovernanceLogId,
        version: Option<&'static str>,
        text: &str,
    ) {
        self.mark_summary_from(id, version, text, 0)
    }

    /// [`Self::mark_summary`] for a result that showed `text` from offset
    /// `from` (into [`summary_text`]) on, the rest pointed at: its leading
    /// paragraphs keep the reads that showed them
    pub fn mark_summary_from(
        &self,
        id: GovernanceLogId,
        version: Option<&'static str>,
        text: &str,
        from: usize,
    ) {
        let text = &summary_text(text);
        let mut inner = self.0.write().expect("shown-once lock");
        let read = inner.read.clone();
        let turn = inner.turn;
        let shown = Shown {
            read,
            bytes: text.len() as u64,
            raw: text.len() as u64,
            post: None,
            turn,
        };
        inner
            .pending_summaries_at
            .push(((id.clone(), version.unwrap_or("latest")), shown.clone()));
        let ends = text
            .match_indices("\n\n")
            .map(|(at, _)| at)
            .chain(std::iter::once(text.len()));
        for end in ends.filter(|&end| end > from) {
            inner
                .pending_summaries
                .push(((id.clone(), text_key(&text[..end])), shown.clone()));
        }
    }

    /// Whether the note `legend` was shown this session (or by the result
    /// being rendered), without showing it
    pub fn legend_shown(&self, legend: &'static str) -> bool {
        let inner = self.0.read().expect("shown-once lock");
        inner.legends.contains(legend)
            || inner.pending_legends.contains(&legend)
    }

    /// Whether the note `legend` is still to be shown this session; if it
    /// is, the result being rendered shows it (so a caller that asks
    /// shows it)
    pub fn legend(&self, legend: &'static str) -> bool {
        let mut inner = self.0.write().expect("shown-once lock");
        if inner.legends.contains(legend)
            || inner.pending_legends.contains(&legend)
        {
            return false;
        }
        inner.pending_legends.push(legend);
        true
    }

    /// The read of `id`, as [`Self::begin`] names it
    pub fn read_of(id: impl std::fmt::Display) -> String {
        format!("your read of {id}")
    }

    /// The read of a governance entry, as a pointer names it: its part,
    /// and which version, when the record has two (`your read of
    /// GOV-2026-0017 (original)`)
    pub fn read_of_entry(
        entry: &crate::responses::GovernanceEntryResponse,
    ) -> String {
        let mut read = Self::read_of(&entry.id);
        if let Some(n) = entry.round {
            read.push_str(&format!(" round {n}"));
        } else if let Some(a) = &entry.attachment {
            read.push_str(&format!(" {a:?}"));
        }
        if let Some(version) = entry.version_label() {
            read.push_str(&format!(" ({version})"));
        }
        read
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

    /// The result being rendered shows `id` in full, `bytes` long, its
    /// body `raw` bytes as stored
    pub fn mark(
        &self,
        id: impl Into<ContentId>,
        bytes: usize,
        raw: usize,
        post: Option<PostId>,
    ) {
        let mut inner = self.0.write().expect("shown-once lock");
        let read = inner.read.clone();
        let turn = inner.turn;
        inner.pending.push((
            id.into(),
            Shown {
                read,
                bytes: bytes as u64,
                raw: raw as u64,
                post,
                turn,
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
        let legends = std::mem::take(&mut inner.pending_legends);
        inner.legends.extend(legends);
        // The latest showing: a summary is shown again only when asked
        // for by name, and a later pointer names that read
        let summaries = std::mem::take(&mut inner.pending_summaries);
        for (id, shown) in summaries {
            inner.summaries.insert(id, shown);
        }
        let summaries = std::mem::take(&mut inner.pending_summaries_at);
        for (id, shown) in summaries {
            inner.summaries_at.insert(id, shown);
        }
        let parts = std::mem::take(&mut inner.pending_parts);
        for (link, shown) in parts {
            inner.parts.entry(link).or_insert(shown);
        }
    }

    /// The rendered result was left out of the context: nothing it marked
    /// was seen
    pub fn discard(&self) {
        let mut inner = self.0.write().expect("shown-once lock");
        inner.pending.clear();
        inner.pending_legends.clear();
        inner.pending_summaries.clear();
        inner.pending_summaries_at.clear();
        inner.pending_parts.clear();
    }

    /// Of `post`'s comments in the context: how many, and their raw bodies'
    /// bytes, as the server counts them in the post's `size`
    pub fn comments_of(&self, post: PostId) -> (u64, u64) {
        let inner = self.0.read().expect("shown-once lock");
        inner
            .shown
            .values()
            .filter(|s| s.post == Some(post))
            .fold((0, 0), |(n, b), s| (n + 1, b + s.raw))
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
        once.mark(id(1), 100, 100, None);
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
        once.mark(id(1), 100, 100, None);
        once.discard();
        assert!(!once.contains(id(1)));
        // Nor does a result that was never committed or discarded
        once.begin("b");
        once.mark(id(2), 1, 1, None);
        once.begin("c");
        once.commit();
        assert!(!once.contains(id(2)));
    }

    /// A note is shown once a session, and again after a result that
    /// showed it was left out
    #[test]
    fn a_legend_is_shown_once() {
        let once = ShownOnce::default();
        once.begin("a");
        assert!(once.legend("x"));
        assert!(!once.legend("x"), "not twice in one result");
        once.discard();
        once.begin("b");
        assert!(once.legend("x"));
        once.commit();
        once.begin("c");
        assert!(!once.legend("x"));
    }

    #[test]
    fn the_first_showing_is_kept() {
        let once = ShownOnce::default();
        once.begin("first");
        once.mark(id(1), 10, 10, None);
        once.commit();
        once.begin("second");
        once.mark(id(1), 20, 20, None);
        once.commit();
        assert_eq!(once.earlier(id(1)).unwrap().read, "first");
    }

    #[test]
    fn a_posts_comments_are_counted() {
        let once = ShownOnce::default();
        let post = PostId::from(Uuid::from_u128(9));
        once.begin("r");
        once.mark(id(1), 10, 10, Some(post));
        once.mark(id(2), 30, 30, Some(post));
        once.mark(id(3), 50, 50, None);
        once.commit();
        assert_eq!(once.comments_of(post), (2, 40));
    }
}
