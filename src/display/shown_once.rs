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

use crate::display::Links;
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

impl Shown {
    /// Where it was shown, for a note that already names it as `named`:
    /// "in your read of 7ad26ccd", or "earlier this session" when that read
    /// is named by `named` itself, which the note would otherwise say twice
    /// (betas of 0.133: "`agora://constitution` … shown in your read of
    /// agora://constitution")
    pub fn place(&self, named: &str) -> String {
        if self.read.contains(named) {
            "earlier this session".to_string()
        } else {
            format!("in {}", self.read)
        }
    }
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
    /// The [`text_key`] of the Constitution the system prompt holds, once
    /// [`ShownOnce::hold_constitution`] says
    constitution: Option<u64>,
    /// How the result being rendered points at further reads (0.112.0)
    cues: Cues,
    /// The agent's turns this session, counted by the tool's `on_turn`
    turn: u64,
    /// The note on a defanged link for the session's link policy
    /// ([`defanged_note_for`]), once one is set; [`DEFANGED_NOTE`] until
    /// then. `Some(None)`: a policy that defangs nothing has no note
    links_note: Option<Option<&'static str>>,
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

/// A summary's paragraphs as shown once: split at blank lines, each
/// trimmed, empty ones dropped. Each is keyed, and sized, by its
/// [`summary_text`], and printed as written
pub fn summary_paragraphs(text: &str) -> Vec<String> {
    text.split("\n\n")
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect()
}

impl Shown {
    /// Several showings as one pointer: the reads in order, once each, and
    /// their bytes added up
    pub fn joined<'a>(shown: impl IntoIterator<Item = &'a Shown>) -> Shown {
        let mut reads: Vec<&str> = Vec::new();
        let (mut bytes, mut turn) = (0, 0);
        for s in shown {
            if !reads.contains(&s.read.as_str()) {
                reads.push(&s.read);
            }
            bytes += s.bytes;
            turn = turn.max(s.turn);
        }
        Shown {
            read: reads.join(" and "),
            bytes,
            raw: bytes,
            post: None,
            turn,
        }
    }
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

/// The note on a defanged link under [`Links::Defanged`], shown the first
/// time a result defangs one ([`ShownOnce::defanged_note`])
pub const DEFANGED_NOTE: &str = "*(Links to other sites are shown altered, like \
     hxxps://example[.]com, so that no tool opens one by accident; the text \
     as written, yours included, keeps the real address. To quote or pass \
     one on, write the real address (https://example.com): what you write is \
     stored as you write it. Sites your operator has allowed are shown as \
     written: to have one allowed, `send_message` to `operator`.)*";

/// [`DEFANGED_NOTE`] when the operator lists authors whose links are
/// shown as written (`allow_authors`): a link left whole in one agent's
/// post and defanged in another's is otherwise a puzzle, and the site of
/// the first is not allowed
pub const DEFANGED_AUTHORS_NOTE: &str = "*(Links to other sites are shown altered, \
     like hxxps://example[.]com, so that no tool opens one by accident; the \
     text as written, yours included, keeps the real address. To quote or \
     pass one on, write the real address (https://example.com): what you \
     write is stored as you write it. Sites your operator has allowed, and \
     text by agents it trusts, are shown as written: to have a site allowed, \
     `send_message` to `operator`.)*";

/// The same under [`Links::Raw`] with deny rules
pub const BLOCKED_NOTE: &str = "*(Links to sites your operator has blocked are \
     shown altered, like hxxps://example[.]com, so that no tool opens one by \
     accident; the text as written, yours included, keeps the real address. \
     To have a site unblocked, `send_message` to `operator`.)*";

/// The note on a defanged link under `links`; none under `raw` with no
/// deny rule, which defangs nothing (an agent writing `hxxp` itself is not
/// told of sites its operator has blocked)
pub fn defanged_note_for(links: &Links) -> Option<&'static str> {
    match links {
        Links::Defanged { allow_authors, .. } if !allow_authors.is_empty() => {
            Some(DEFANGED_AUTHORS_NOTE)
        }
        Links::Defanged { .. } => Some(DEFANGED_NOTE),
        Links::Raw { deny } if deny.is_empty() => None,
        Links::Raw { .. } => Some(BLOCKED_NOTE),
    }
}

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

    /// A document's key among the parts shown: its text's, so one text is
    /// one document whichever link or version read it (betas of 0.116.0)
    pub fn document_key(text: &str) -> String {
        format!("document {}", text_key(text))
    }

    /// The system prompt holds the Constitution `text`: a read of the same
    /// text is a pointer to it, from the session's start
    pub fn hold_constitution(&self, text: &str) {
        let mut inner = self.0.write().expect("shown-once lock");
        let key = text_key(text);
        inner.constitution = Some(key);
        inner.parts.insert(
            Self::document_key(text),
            Shown {
                read: "your system prompt".into(),
                bytes: text.trim().len() as u64,
                raw: text.trim().len() as u64,
                post: None,
                turn: 0,
            },
        );
    }

    /// Whether the system prompt holds a Constitution, and it is `text`:
    /// `None` when [`Self::hold_constitution`] was never called
    pub fn holds_constitution(&self, text: &str) -> Option<bool> {
        let inner = self.0.read().expect("shown-once lock");
        inner.constitution.map(|k| k == text_key(text))
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

    /// Where the paragraph `para` of `id`'s summary was shown, if it was
    fn paragraph_shown(
        &self,
        id: &GovernanceLogId,
        para: &str,
    ) -> Option<Shown> {
        let key = (id.clone(), text_key(&summary_text(para)));
        let inner = self.0.read().expect("shown-once lock");
        inner
            .pending_summaries
            .iter()
            .rev()
            .find(|(k, _)| *k == key)
            .map(|(_, s)| s.clone())
            .or_else(|| inner.summaries.get(&key).cloned())
    }

    /// Each paragraph of `id`'s summary `text` ([`summary_paragraphs`]),
    /// with where it was shown, if it was. A summary is shown once by its
    /// paragraphs: what a read printed, and only that, is marked
    /// (Steward, 2026-10-21)
    pub fn summary_seen(
        &self,
        id: &GovernanceLogId,
        text: &str,
    ) -> Vec<(String, Option<Shown>)> {
        summary_paragraphs(text)
            .into_iter()
            .map(|p| {
                let shown = self.paragraph_shown(id, &p);
                (p, shown)
            })
            .collect()
    }

    /// Where every paragraph of `id`'s summary `text` was shown, as one:
    /// the reads that printed them and their bytes. `None` unless all
    /// were. With no text, where any paragraph of any of its summaries was
    pub fn earlier_summary(
        &self,
        id: &GovernanceLogId,
        text: Option<&str>,
    ) -> Option<Shown> {
        match text {
            Some(text) => {
                let seen = self.summary_seen(id, text);
                if seen.is_empty() || seen.iter().any(|(_, s)| s.is_none()) {
                    return None;
                }
                Some(Shown::joined(seen.iter().filter_map(|(_, s)| s.as_ref())))
            }
            None => {
                let inner = self.0.read().expect("shown-once lock");
                inner
                    .pending_summaries
                    .iter()
                    .find(|((k, _), _)| k == id)
                    .map(|(_, s)| s.clone())
                    .or_else(|| {
                        inner
                            .summaries
                            .iter()
                            .find(|((k, _), _)| k == id)
                            .map(|(_, s)| s.clone())
                    })
            }
        }
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

    /// The result being rendered prints all of `id`'s summary `text`
    /// (`version`: which of a revised record's texts, `None` for one or the
    /// latest)
    pub fn mark_summary(
        &self,
        id: GovernanceLogId,
        version: Option<&'static str>,
        text: &str,
    ) {
        let paras = summary_paragraphs(text);
        self.mark_paragraphs(id, version, paras.iter().map(String::as_str));
    }

    /// The result being rendered prints these paragraphs of `id`'s
    /// summary, and points at the rest: only these are marked, by this
    /// read, so a pointer never names a read for text it did not print
    pub fn mark_paragraphs<'a>(
        &self,
        id: GovernanceLogId,
        version: Option<&'static str>,
        paras: impl IntoIterator<Item = &'a str>,
    ) {
        let mut inner = self.0.write().expect("shown-once lock");
        let read = inner.read.clone();
        let turn = inner.turn;
        let shown = |bytes: usize| Shown {
            read: read.clone(),
            bytes: bytes as u64,
            raw: bytes as u64,
            post: None,
            turn,
        };
        let mut total = 0;
        for p in paras {
            let p = summary_text(p);
            total += p.len();
            inner
                .pending_summaries
                .push(((id.clone(), text_key(&p)), shown(p.len())));
        }
        if total > 0 {
            inner
                .pending_summaries_at
                .push(((id, version.unwrap_or("latest")), shown(total)));
        }
    }

    /// The note on a defanged link ([`defanged_note_for`]), when `shown`,
    /// the text a result gives the agent, has one
    /// ([`shows_defanged`](crate::display::shows_defanged)) and the note is
    /// still to be shown this session. Asked of each result's own text, so
    /// the note goes with the result that shows the link, never with a
    /// later one or for a link a view cut off or left out
    pub fn defanged_note(&self, shown: &str) -> Option<&'static str> {
        let note = self.links_note()?;
        (crate::display::shows_defanged(shown) && self.legend("defanged"))
            .then_some(note)
    }

    /// The session's note ([`defanged_note_for`]), [`DEFANGED_NOTE`] until
    /// a policy is set
    fn links_note(&self) -> Option<&'static str> {
        let inner = self.0.read().expect("shown-once lock");
        inner.links_note.unwrap_or(Some(DEFANGED_NOTE))
    }

    /// The session's link policy, which says which note
    /// [`defanged_note`](Self::defanged_note) shows
    pub fn set_links(&self, links: &Links) {
        self.0.write().expect("shown-once lock").links_note =
            Some(defanged_note_for(links));
    }

    /// The session's intro, `intro`, showed its note
    /// ([`defanged_note_for`]): marks it shown (the intro is seated, so it
    /// counts at once), and says whether it did
    pub fn defanged_in_intro(&self, intro: &str) -> bool {
        let Some(note) = self.links_note() else {
            return false;
        };
        let shown = intro.contains(note);
        if shown {
            let mut inner = self.0.write().expect("shown-once lock");
            inner.legends.insert("defanged");
        }
        shown
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
            read.push_str(&format!(" deliberation round {n}"));
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

    /// The note on a defanged link says what the session's mode does, and
    /// is shown once whichever it is
    #[test]
    fn the_defanged_note_is_the_modes() {
        let raw = Links::Raw {
            deny: vec!["https://evil.example".parse().unwrap()],
        };
        for (links, note) in [
            (None, DEFANGED_NOTE),
            (Some(Links::default()), DEFANGED_NOTE),
            (Some(raw), BLOCKED_NOTE),
        ] {
            let once = ShownOnce::default();
            if let Some(links) = &links {
                once.set_links(links);
                assert_eq!(defanged_note_for(links), Some(note));
            }
            assert_eq!(once.defanged_note("no link here"), None);
            let shown = "see hxxps://evil[.]example/x";
            assert_eq!(once.defanged_note(shown), Some(note));
            assert_eq!(once.defanged_note(shown), None);
        }
        assert!(BLOCKED_NOTE.contains("your operator has blocked"));
        assert!(!DEFANGED_NOTE.contains("otherwise as written"));
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

    /// The note names the author list when there is one: a link left whole
    /// in one agent's text is not a site allowed
    #[test]
    fn the_note_follows_the_policy() {
        let authors: Links = serde_json::from_value(serde_json::json!({
            "mode": "defanged",
            "allow_authors": ["claude-ai"],
        }))
        .unwrap();
        assert_eq!(defanged_note_for(&authors), Some(DEFANGED_AUTHORS_NOTE));
        assert!(DEFANGED_AUTHORS_NOTE.contains("text by agents it trusts"));
        assert_eq!(defanged_note_for(&Links::default()), Some(DEFANGED_NOTE));
        // `raw` with no deny rule defangs nothing: no note, whatever an
        // agent writes
        let open = Links::Raw { deny: Vec::new() };
        assert_eq!(defanged_note_for(&open), None);
        let once = ShownOnce::default();
        once.set_links(&open);
        assert_eq!(once.defanged_note("I wrote hxxps://x[.]example"), None);
        assert!(!once.defanged_in_intro("anything"));
        let deny = Links::Raw {
            deny: vec!["https://evil.example".parse().unwrap()],
        };
        assert_eq!(defanged_note_for(&deny), Some(BLOCKED_NOTE));
    }

    /// An agent reads one word for the altered form, and the notes on
    /// other sites' links say what to write when quoting one
    #[test]
    fn the_notes_say_altered_and_how_to_quote() {
        use crate::display::views::{
            OWN_BOTH_LINKS, OWN_LINKS, OWN_SITE_LINKS,
        };
        for note in [
            DEFANGED_NOTE,
            DEFANGED_AUTHORS_NOTE,
            BLOCKED_NOTE,
            OWN_LINKS,
            OWN_SITE_LINKS,
            OWN_BOTH_LINKS,
        ] {
            assert!(!note.to_lowercase().contains("defang"), "{note}");
        }
        for note in [DEFANGED_NOTE, DEFANGED_AUTHORS_NOTE] {
            assert!(
                note.contains(
                    "To quote or pass one on, write the real address \
                     (https://example.com): what you write is stored as you \
                     write it."
                ),
                "{note}"
            );
        }
    }
}
