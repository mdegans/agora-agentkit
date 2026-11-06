//! What the `memory` tool's `log`, `show` and `diff` say, from versions the
//! runner gathers. Pure: no I/O, no keys.

use chrono::{DateTime, NaiveDate, Utc};

use super::id::{
    Candidate, HistoryRef, MemoryId, Resolution, resolve, short_head,
};
use crate::govlog::Sha256Hex;
use crate::ids::GovernanceLogId;
use crate::requests::GetContentInput;
use crate::text::{change_counts, diff_units, line_diff};

/// Where a version of an agent's memory was found
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionSource {
    /// Sealed and verified, on the agent's line
    Sealed,
    /// A local archive (`state.<utc>.json`): not sealed, so no head
    LocalCopy,
    /// Sealed on a side line the operator rebased away from (a displaced
    /// outbox entry whose chain verifies)
    SideLine,
    /// Sealed and verified, but not on the agent's main line, for no
    /// reason known
    OffLine,
    /// Set aside when the agent's line was rebased, on a date if known
    SetAside { on: Option<NaiveDate> },
    /// Never saved: the run ended before saving (unsigned)
    NeverSaved,
    /// A copy from before a memory loss, written into the chain later: it
    /// was never the agent's memory on the day it is dated next to
    PriorCopy,
}

impl VersionSource {
    /// What a version from here is labelled with; nothing for a sealed one
    pub fn label(&self) -> Option<String> {
        match self {
            Self::Sealed => None,
            Self::LocalCopy => Some("local copy, not sealed".into()),
            Self::SideLine => {
                Some("on a side line your operator rebased".into())
            }
            Self::OffLine => Some("sealed, off your main line".into()),
            Self::SetAside { on: Some(on) } => {
                Some(format!("set aside on {on} when your line was rebased"))
            }
            Self::SetAside { on: None } => Some(
                "set aside when your line was rebased, date unknown".into(),
            ),
            Self::NeverSaved => {
                Some("never saved: the run ended before saving".into())
            }
            Self::PriorCopy => Some("a copy from before a memory loss".into()),
        }
    }

    /// Whether a version from here is on the agent's main line: sealed
    /// there, or a local copy of it. `log` compares a version's size and
    /// text only with another on the main line
    pub fn main_line(&self) -> bool {
        matches!(self, Self::Sealed | Self::LocalCopy)
    }
}

/// A version's memory: its text, or why it couldn't be read just now.
/// An unopened version is never read as an empty memory. Its `Debug`
/// shows the size, never the text (agora CLAUDE.md rule 6)
#[derive(Clone, PartialEq, Eq)]
pub enum VersionText {
    Opened {
        id: MemoryId,
        /// `memory.content`, raw
        content: String,
    },
    /// Couldn't be opened just now (a sealed blob that couldn't be
    /// fetched, say)
    Unopened {
        /// Its id, if the runner knows it without opening it
        id: Option<MemoryId>,
        /// Why, in the agent's words
        reason: String,
    },
}

/// One version of an agent's memory, as the runner found it. `Debug`
/// shows no memory text
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryVersion {
    /// Its sealed head, when it has one
    pub head: Option<Sha256Hex>,
    /// When it was saved (a sealed entry's `state_at`, an archive's
    /// time), if known
    pub at: Option<DateTime<Utc>>,
    pub source: VersionSource,
    /// The anchor its head is in, if any
    pub anchored: Option<GovernanceLogId>,
    pub text: VersionText,
}

impl std::fmt::Debug for VersionText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Opened { id, content } => f
                .debug_struct("Opened")
                .field("id", id)
                .field("chars", &content.chars().count())
                .finish(),
            Self::Unopened { id, reason } => f
                .debug_struct("Unopened")
                .field("id", id)
                .field("reason", reason)
                .finish(),
        }
    }
}

/// What an unopened version's row and block say
const UNOPENED: &str = "couldn't be opened just now";

impl MemoryVersion {
    /// An opened version
    pub fn opened(
        id: MemoryId,
        content: impl Into<String>,
        at: Option<DateTime<Utc>>,
        source: VersionSource,
    ) -> Self {
        Self {
            head: None,
            at,
            source,
            anchored: None,
            text: VersionText::Opened {
                id,
                content: content.into(),
            },
        }
    }

    /// Its memory id, if known
    pub fn id(&self) -> Option<MemoryId> {
        match &self.text {
            VersionText::Opened { id, .. } => Some(*id),
            VersionText::Unopened { id, .. } => *id,
        }
    }

    /// Its memory, if opened
    pub fn content(&self) -> Option<&str> {
        match &self.text {
            VersionText::Opened { content, .. } => Some(content),
            VersionText::Unopened { .. } => None,
        }
    }

    /// Its size, in characters, if opened
    pub fn chars(&self) -> Option<usize> {
        self.content().map(|c| c.chars().count())
    }

    /// How it is named: its memory id, or else its head
    fn name(&self) -> String {
        match (self.id(), &self.head) {
            (Some(id), _) => id.to_string(),
            (None, Some(head)) => format!("head {}", short_head(head)),
            (None, None) => "a version".into(),
        }
    }

    /// `mem-3f9a1c07be (2026-11-05 21:14, 13,539 chars)`, with its source's
    /// label when it has one; no size when it couldn't be opened
    pub fn describe(&self) -> String {
        self.describe_as(false)
    }

    /// [`describe`](Self::describe), without the local-copy label when
    /// `hide_local`: every version in view is one, and it is said once
    fn describe_as(&self, hide_local: bool) -> String {
        let label = self
            .source
            .label()
            .filter(|_| {
                !(hide_local && self.source == VersionSource::LocalCopy)
            })
            .map(|l| format!(", {l}"))
            .unwrap_or_default();
        let size = self
            .chars()
            .map(|n| format!(", {} chars", grouped(n)))
            .unwrap_or_default();
        format!("{} ({}{size}{label})", self.name(), stamp(self.at))
    }

    /// Whether both are opened and hold the same text
    fn same_text(&self, other: &Self) -> bool {
        matches!((self.content(), other.content()), (Some(a), Some(b)) if a == b)
    }

    /// An unopened version's block in `show`, or its answer in `diff`
    fn unopened_note(&self, hide_local: bool) -> Option<String> {
        match &self.text {
            VersionText::Unopened { reason, .. } => Some(format!(
                "{} {UNOPENED} ({reason}); ask again later.",
                self.describe_as(hide_local)
            )),
            VersionText::Opened { .. } => None,
        }
    }
}

/// Order `history` as the renderers take it: newest first, versions with
/// no date last (in the order given)
pub fn newest_first(history: &mut [MemoryVersion]) {
    history.sort_by_key(|v| (v.at.is_none(), std::cmp::Reverse(v.at)));
}

/// Whether the agent's sealed history was checked for this call. When it
/// couldn't be, the runner passes its local copies, and `log` and `show`
/// say so ([`UNCHECKED`]) and give no head
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SealCheck {
    #[default]
    Checked,
    Unchecked,
}

/// The memory in the agent's context this session: what `show` is
/// anchored on and `diff` compares with when it is given one version.
/// `Debug` shows its size, not its text
#[derive(Clone, Copy)]
pub struct CurrentMemory<'a> {
    pub id: MemoryId,
    pub content: &'a str,
    /// Its place in the history the renderers are given (newest first):
    /// the newest version with the same text. `None` when no kept version
    /// has it
    pub position: Option<usize>,
}

impl std::fmt::Debug for CurrentMemory<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CurrentMemory")
            .field("id", &self.id)
            .field("chars", &self.content.chars().count())
            .field("position", &self.position)
            .finish()
    }
}

impl<'a> CurrentMemory<'a> {
    /// `content`, placed in `history` by its text
    pub fn new(
        id: MemoryId,
        content: &'a str,
        history: &[MemoryVersion],
    ) -> Self {
        Self {
            id,
            content,
            position: history.iter().position(|v| v.content() == Some(content)),
        }
    }
}

/// What `log` says above its rows when every version is a local copy
pub const LOCAL_ONLY: &str = "These are your runner's local copies. They \
     aren't sealed, so they have no heads.";

/// What `log` and `show` say first when the sealed history couldn't be
/// checked ([`SealCheck::Unchecked`])
pub const UNCHECKED: &str = "Your sealed history couldn't be checked just \
     now, so none of your versions is shown as sealed and no head is given. \
     These are your runner's local copies, unsigned: nothing here vouches \
     for them.";

/// How `show` and `diff` print a memory
pub const AS_STORED: &str = "Memory is printed as stored: in your context, \
     its `#` and `##` headings appear as `###`.";

/// What `show` says first when it shows more than one version
pub const SHOW_LEGEND: &str = "Newest first. Each change runs forward, \
     from the older version to the newer: `-` lines were in the older, `+` \
     lines are in the newer.";

/// What heads a diff to the current memory
const TO_CURRENT: &str = "your current memory (in context above)";

/// `2026-11-05 21:14`, UTC, or `date unknown`
fn stamp(at: Option<DateTime<Utc>>) -> String {
    match at {
        Some(at) => at.format("%Y-%m-%d %H:%M").to_string(),
        None => "date unknown".into(),
    }
}

/// `13,539`
pub fn grouped(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `(+1,204)`, `(−1,204)` or `(±0)`
fn delta(now: usize, before: usize) -> String {
    match now.cmp(&before) {
        std::cmp::Ordering::Greater => format!("(+{})", grouped(now - before)),
        std::cmp::Ordering::Less => {
            format!("(\u{2212}{})", grouped(before - now))
        }
        std::cmp::Ordering::Equal => "(±0)".into(),
    }
}

/// Whether `log` compares `a` and `b` (size, sameness): only two versions
/// on the main line. Across a prior copy, a side line or a set-aside one,
/// a size change would read as a loss or a gain that never happened (beta
/// round 16: −449 on a first day, against a copy from before a loss)
fn comparable(a: &MemoryVersion, b: &MemoryVersion) -> bool {
    a.source.main_line() && b.source.main_line()
}

/// A version's head as shown: none when the history wasn't checked
fn shown_head(v: &MemoryVersion, seal: SealCheck) -> Option<&Sha256Hex> {
    v.head.as_ref().filter(|_| seal == SealCheck::Checked)
}

/// Whether a `log` page may end at row `i`: its cursor (its head, or else
/// its memory id, which [`log_start`] reads as that memory's oldest
/// version) leads on to the row after it
fn cursor_row(history: &[MemoryVersion], i: usize, seal: SealCheck) -> bool {
    shown_head(&history[i], seal).is_some()
        || history[i].id().is_some_and(|id| {
            !history[i + 1..].iter().any(|v| v.id() == Some(id))
        })
}

/// The cursor that pages on from row `i`: its head, or its memory id
fn cursor(v: &MemoryVersion, seal: SealCheck) -> String {
    match (shown_head(v, seal), v.id()) {
        (Some(head), _) => short_head(head),
        (None, Some(id)) => id.to_string(),
        (None, None) => unreachable!("a cursor row has a head or an id"),
    }
}

/// Where `log` starts for `before`: the row after the version it names.
/// A head names its version; a memory id names that memory's oldest
/// version, so a page that ends on it goes on from there. `Err` when it
/// names none, or several (see [`Resolution`]); the note is for a `mem-`
/// ref that named a head ([`Resolution::note`])
pub fn log_start(
    history: &[MemoryVersion],
    before: &HistoryRef,
) -> Result<(usize, Option<String>), Resolution> {
    let ids: Vec<MemoryId> = history.iter().filter_map(|v| v.id()).collect();
    let resolution = resolve(
        before,
        ids.iter(),
        history.iter().filter_map(|v| v.head.as_ref()),
    );
    let note = resolution.note(before);
    let row = match resolution.candidate() {
        Some(Candidate::Memory(id)) => {
            history.iter().rposition(|v| v.id() == Some(id))
        }
        Some(Candidate::Head(head)) => {
            history.iter().rposition(|v| v.head == Some(head))
        }
        None => None,
    };
    match row {
        Some(i) => Ok((i + 1, note)),
        None => Err(resolution),
    }
}

/// `log`: about `limit` rows of `history` (every version, newest first;
/// see [`newest_first`]) from `start`, each sized against the version
/// below it. The newest version of `current` is marked "in your context":
/// the memory this session loaded, which an earlier session wrote (not
/// "this session", beta round 17).
///
/// `2026-11-05 21:14  mem-3f9a1c07be  head 346ac9543d0c  memory 13,539
/// chars (−1,204)  ← in your context`
///
/// A page ends on a row whose cursor pages on without skipping or
/// repeating a row ([`log_start`]): one row short of `limit` or more, when
/// the row at the limit is a memory with older versions and no head.
pub fn log(
    history: &[MemoryVersion],
    start: usize,
    limit: usize,
    current: &CurrentMemory<'_>,
    seal: SealCheck,
) -> String {
    if history.is_empty() {
        return "Your runner has kept no versions of your memory.".into();
    }
    if start >= history.len() {
        return format!(
            "No versions are older than that: you have {} in all.",
            history.len()
        );
    }
    let mut out = String::new();
    let all_local =
        history.iter().all(|v| v.source == VersionSource::LocalCopy);
    if seal == SealCheck::Unchecked {
        out.push_str(UNCHECKED);
        out.push_str("\n\n");
    } else if all_local {
        out.push_str(LOCAL_ONLY);
        out.push_str("\n\n");
    }
    let mut end = start.saturating_add(limit.max(1)).min(history.len());
    if end < history.len() {
        end = (start..end)
            .rev()
            .find(|&i| cursor_row(history, i, seal))
            .or_else(|| {
                (end..history.len()).find(|&i| cursor_row(history, i, seal))
            })
            .map_or(history.len(), |i| i + 1);
    }
    for i in start..end {
        let v = &history[i];
        let mut row = stamp(v.at);
        if let Some(id) = v.id() {
            row.push_str(&format!("  {id}"));
        }
        if let Some(head) = shown_head(v, seal) {
            row.push_str(&format!("  head {}", short_head(head)));
        }
        match (&v.text, v.chars()) {
            (VersionText::Unopened { reason, .. }, _) => {
                row.push_str(&format!("  ({UNOPENED}: {reason})"));
            }
            _ if i > start
                && comparable(&history[i - 1], v)
                && history[i - 1].same_text(v) =>
            {
                row.push_str("  (same memory as above)");
            }
            (_, Some(chars)) => {
                row.push_str(&format!("  memory {} chars", grouped(chars)));
                if let Some(older) = history
                    .get(i + 1)
                    .filter(|o| comparable(v, o))
                    .and_then(|o| o.chars())
                {
                    row.push(' ');
                    row.push_str(&delta(chars, older));
                }
            }
            (_, None) => {}
        }
        if let Some(label) = v.source.label().filter(|_| {
            !(all_local || seal == SealCheck::Unchecked)
                || v.source != VersionSource::LocalCopy
        }) {
            row.push_str(&format!("  ({label})"));
        }
        if let Some(anchor) =
            v.anchored.as_ref().filter(|_| seal == SealCheck::Checked)
        {
            row.push_str(&format!("  anchored in {anchor}"));
        }
        if current.position == Some(i) {
            row.push_str("  ← in your context");
        }
        out.push_str(&row);
        out.push('\n');
    }
    let older = history.len() - end;
    if older > 0 {
        out.push_str(&format!(
            "{older} older version{}: ask with `before` set to {}.\n",
            if older == 1 { "" } else { "s" },
            cursor(&history[end - 1], seal)
        ));
    }
    out.truncate(out.trim_end().len());
    out
}

/// Whether a version is shorter shown whole than as its diff. A diff of
/// long changed lines can pass half the version's size with most of it
/// unchanged (beta round 16), so the test is the length, not a share
fn whole_is_shorter(diff: &str, version: &str) -> bool {
    version.len() < diff.len()
}

/// What a version shown whole in place of its diff is labelled with
const SHORTER_WHOLE: &str = "shown in full (shorter than its changes)";

/// `, n versions between not asked for`, or nothing
fn between(n: usize) -> String {
    match n {
        0 => String::new(),
        1 => ", 1 version between not asked for".into(),
        n => format!(", {n} versions between not asked for"),
    }
}

/// An opened version whole, under its header
fn whole(header: &str, content: &str) -> String {
    let content = content.trim_end();
    let content = if content.is_empty() {
        "(empty)"
    } else {
        content
    };
    format!("{header}:\n{content}")
}

/// `show`'s first opened block, for several versions: the newest opened
/// one asked for, anchored on the current memory when that is not older
/// than it
fn anchored(
    history: &[MemoryVersion],
    newest: usize,
    content: &str,
    current: &CurrentMemory<'_>,
    hide_local: bool,
) -> String {
    let v = &history[newest];
    let describe = || v.describe_as(hide_local);
    if content == current.content {
        return format!("{} is {TO_CURRENT}.", describe());
    }
    let gap = match current.position {
        // Older than it: a diff would run backward
        Some(c) if c > newest => {
            return whole(
                &format!(
                    "{}, shown in full: newer than your current memory",
                    describe()
                ),
                content,
            );
        }
        Some(c) => between(newest - c - 1),
        None => {
            ", your current memory is not among your kept versions".to_string()
        }
    };
    let diff = line_diff(content, current.content);
    if whole_is_shorter(&diff, content) {
        whole(&format!("{}{gap}, {SHORTER_WHOLE}", describe()), content)
    } else {
        format!("{} → {TO_CURRENT}{gap}:\n{}", describe(), diff.trim_end())
    }
}

/// `show`: the versions `picked` (indexes into `history`, newest first,
/// in any order) like `git log -p`, at most `budget` bytes.
///
/// One version is shown whole, unless it is the current memory. Of
/// several, the newest opens with what changed from it to `current`, or
/// whole when that is shorter or `current` is older; each older one is
/// what changed from it to the next newer one picked, or whole when that
/// is shorter. A version that couldn't be opened says so, and the diffs
/// pass over it. Past `budget`, the rest are listed by id and size, never
/// cut. When every version asked for is a local copy, that is said once
/// ([`LOCAL_ONLY`]) rather than on each, as `log` does.
pub fn show(
    history: &[MemoryVersion],
    picked: &[usize],
    current: CurrentMemory<'_>,
    budget: usize,
    seal: SealCheck,
) -> String {
    let mut picked: Vec<usize> = picked
        .iter()
        .copied()
        .filter(|&i| i < history.len())
        .collect();
    picked.sort_unstable();
    picked.dedup();
    if picked.is_empty() {
        return "No versions to show.".into();
    }

    let mut blocks: Vec<(usize, String)> = Vec::new();
    // As `log`: no local-copy label on each version when every one is,
    // or the history wasn't checked; said once instead
    let all_local = picked
        .iter()
        .all(|&i| history[i].source == VersionSource::LocalCopy);
    let hide = all_local || seal == SealCheck::Unchecked;
    let describe = |i: usize| history[i].describe_as(hide);
    let preface: &[&str] = match (seal, all_local) {
        (SealCheck::Unchecked, _) => &[UNCHECKED],
        (SealCheck::Checked, true) => &[LOCAL_ONLY],
        (SealCheck::Checked, false) => &[],
    };
    // Joined with spaces: the opening paragraph
    let mut legend: Vec<&str> = preface.to_vec();
    if let [only] = picked.as_slice() {
        let v = &history[*only];
        let block = match v.content() {
            None => v.unopened_note(hide).unwrap_or_default(),
            Some(content) if content == current.content => {
                let named = format!("{} is {TO_CURRENT}.", describe(*only));
                return match legend.as_slice() {
                    [] => named,
                    _ => format!("{}\n\n{named}", legend.join(" ")),
                };
            }
            Some(content) => {
                legend.push(AS_STORED);
                whole(&describe(*only), content)
            }
        };
        blocks.push((*only, block));
    } else {
        legend.push(SHOW_LEGEND);
        legend.push(AS_STORED);
        // The next newer opened version picked, as (index, content)
        let mut newer: Option<(usize, &str)> = None;
        for &i in &picked {
            let v = &history[i];
            let Some(content) = v.content() else {
                blocks.push((i, v.unopened_note(hide).unwrap_or_default()));
                continue;
            };
            let block = match newer {
                None => anchored(history, i, content, &current, hide),
                Some((n, newer_content)) => {
                    let asked =
                        picked.iter().filter(|&&p| p > n && p < i).count();
                    let gap = between(i - n - 1 - asked);
                    let newer_name = history[n].name();
                    if content == newer_content {
                        format!(
                            "{} → {newer_name}{gap}: the same memory.",
                            describe(i)
                        )
                    } else {
                        let diff = line_diff(content, newer_content);
                        if whole_is_shorter(&diff, content) {
                            whole(
                                &format!(
                                    "{}{gap}, {SHORTER_WHOLE}",
                                    describe(i)
                                ),
                                content,
                            )
                        } else {
                            format!(
                                "{} → {newer_name}{gap}:\n{}",
                                describe(i),
                                diff.trim_end()
                            )
                        }
                    }
                }
            };
            blocks.push((i, block));
            newer = Some((i, content));
        }
    }

    // The most blocks that fit with the footer listing the rest
    let at_most = budget >= GetContentInput::MAX_COMMENT_BUDGET as usize;
    // The versions asked for that aren't the current memory: when one
    // alone is left out, asking for fewer can't help (beta round 17)
    let others = picked
        .iter()
        .filter(|&&i| history[i].content() != Some(current.content))
        .count();
    let size = |i: usize| {
        blocks
            .iter()
            .find(|(b, _)| *b == i)
            .map_or(0, |(_, block)| block.len())
    };
    let footer = |left: &[usize]| -> String {
        match left {
            [] => String::new(),
            [i] if (blocks.len() == 1 || others == 1) && at_most => format!(
                "{} takes {} bytes to show, more than the largest budget \
                 ({} bytes), so it can't be shown whole. Compare it with a \
                 version near it with `diff` instead.",
                describe(*i),
                grouped(size(*i)),
                grouped(budget)
            ),
            [i] if blocks.len() > 1 && others == 1 => format!(
                "{} takes {} bytes to show, more than your budget of {} \
                 bytes; ask again with a larger budget, or `diff` it against \
                 a version near it.",
                describe(*i),
                grouped(size(*i)),
                grouped(budget)
            ),
            [i] if blocks.len() == 1 => format!(
                "{} takes {} bytes to show, more than your budget of {} \
                 bytes; ask again with a larger budget.",
                describe(*i),
                grouped(size(*i)),
                grouped(budget)
            ),
            left => {
                let mut f = format!(
                    "Not shown, past your budget of {} bytes; ask for \
                     fewer{}:",
                    grouped(budget),
                    if at_most { "" } else { ", or a larger budget" }
                );
                for i in left {
                    f.push_str("\n- ");
                    f.push_str(&describe(*i));
                }
                f
            }
        }
    };
    let join = |parts: &[&str]| -> String {
        parts
            .iter()
            .filter(|p| !p.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join("\n\n")
    };
    let legend = legend.join(" ");
    let indexes: Vec<usize> = blocks.iter().map(|(i, _)| *i).collect();
    for shown in (0..=blocks.len()).rev() {
        let mut parts: Vec<&str> = vec![&legend];
        parts.extend(blocks[..shown].iter().map(|(_, b)| b.as_str()));
        let tail = footer(&indexes[shown..]);
        parts.push(&tail);
        let out = join(&parts);
        if out.len() <= budget || shown == 0 {
            // Nothing fits: the list alone, under its preface, so a list
            // without heads or labels never goes unexplained
            return match (shown, preface) {
                (0, []) => tail,
                (0, preface) => format!("{}\n\n{tail}", preface.join(" ")),
                _ => out,
            };
        }
    }
    unreachable!("the loop returns at zero")
}

/// `diff`: what changed from `a` to `b` (or to `current`, when there is
/// no `b`), whole. A version that couldn't be opened says so instead
pub fn diff(
    a: &MemoryVersion,
    b: Option<&MemoryVersion>,
    current: CurrentMemory<'_>,
) -> String {
    // As `log`: when every version compared is a local copy, it is said
    // once, not on each
    let all_local = std::iter::once(a)
        .chain(b)
        .all(|v| v.source == VersionSource::LocalCopy);
    let said = |text: String| match all_local {
        true => format!("{LOCAL_ONLY}\n\n{text}"),
        false => text,
    };
    for v in std::iter::once(a).chain(b) {
        if let Some(note) = v.unopened_note(all_local) {
            return said(format!("No diff: {note}"));
        }
    }
    let a_text = a.content().unwrap_or_default();
    let (to_name, to_text) = match b {
        Some(b) => (b.describe_as(all_local), b.content().unwrap_or_default()),
        None => (TO_CURRENT.to_string(), current.content),
    };
    if a_text == to_text {
        return said(format!(
            "{} and {to_name} are the same memory: nothing changed.",
            a.describe_as(all_local)
        ));
    }
    let diff = line_diff(a_text, to_text);
    let (added, removed) = change_counts(&diff);
    said(format!(
        "{} → {to_name} (+{added} / \u{2212}{removed} {}):\n{}\n\n{AS_STORED}",
        a.describe_as(all_local),
        diff_units(a_text, to_text),
        diff.trim_end()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn id(n: u8) -> MemoryId {
        MemoryId::from([n; 32])
    }

    fn at(day: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, day, 21, 14, 0).unwrap()
    }

    fn version(n: u8, day: u32, content: &str) -> MemoryVersion {
        MemoryVersion {
            head: Some(Sha256Hex::from([n.wrapping_add(0x30); 32])),
            ..MemoryVersion::opened(
                id(n),
                content,
                Some(at(day)),
                VersionSource::Sealed,
            )
        }
    }

    fn local(n: u8, day: u32, content: &str) -> MemoryVersion {
        MemoryVersion {
            head: None,
            source: VersionSource::LocalCopy,
            ..version(n, day, content)
        }
    }

    /// Twenty numbered lines, `changed` of them edited
    fn lines(tag: &str, changed: usize) -> String {
        (0..20)
            .map(|i| {
                if i < changed {
                    format!("line {i} edited in {tag}\n")
                } else {
                    format!("line {i} is a line of memory that stays\n")
                }
            })
            .collect()
    }

    fn current<'a>(
        n: u8,
        content: &'a str,
        history: &[MemoryVersion],
    ) -> CurrentMemory<'a> {
        CurrentMemory::new(id(n), content, history)
    }

    /// `Debug` never shows memory text
    #[test]
    fn debug_shows_no_memory() {
        let secret = "SECRET-MEMORY-TEXT";
        let opened = version(1, 1, secret);
        let cur = current(1, secret, std::slice::from_ref(&opened));
        let text = format!("{opened:?} {cur:?} {:?}", opened.text);
        assert!(!text.contains(secret), "{text}");
        assert!(text.contains("chars: 18"), "{text}");
        assert!(text.contains(&id(1).to_hex()), "{text}");
    }

    #[test]
    fn grouped_and_delta() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(13_539), "13,539");
        assert_eq!(grouped(1_234_567), "1,234,567");
        assert_eq!(delta(10, 1214), "(\u{2212}1,204)");
        assert_eq!(delta(1214, 10), "(+1,204)");
        assert_eq!(delta(5, 5), "(±0)");
    }

    #[test]
    fn log_rows() {
        let mut side = version(3, 2, "abc");
        side.source = VersionSource::SideLine;
        let mut anchored = version(1, 4, "abcdef");
        anchored.anchored = Some("ANC-2026-0005".parse().unwrap());
        let history = vec![
            version(1, 5, "abcdef"),
            anchored,
            version(2, 3, "abcdefgh"),
            side,
            version(4, 1, "a"),
        ];
        let now = current(1, "abcdef", &history);
        let out = log(&history, 0, 20, &now, SealCheck::Checked);
        let rows: Vec<&str> = out.lines().collect();
        assert_eq!(
            rows[0],
            format!(
                "2026-11-05 21:14  {}  head 313131313131  memory 6 chars (±0)  \
                 ← in your context",
                id(1)
            )
        );
        assert_eq!(
            rows[1],
            format!(
                "2026-11-04 21:14  {}  head 313131313131  (same memory as \
                 above)  anchored in ANC-2026-0005",
                id(1)
            )
        );
        // No size change against the side line below it
        assert!(rows[2].ends_with("memory 8 chars"), "{}", rows[2]);
        assert!(
            rows[3].ends_with("(on a side line your operator rebased)"),
            "{}",
            rows[3]
        );
        assert!(rows[4].ends_with("memory 1 chars"), "{}", rows[4]);
        assert_eq!(rows.len(), 5);

        // A page, with the way on
        let out = log(&history, 1, 2, &now, SealCheck::Checked);
        let rows: Vec<&str> = out.lines().collect();
        assert_eq!(rows.len(), 3);
        // The first row of a page is never "same as above": none is shown
        assert!(rows[0].contains("memory 6 chars"), "{}", rows[0]);
        assert!(!rows[0].contains("in your context"));
        assert_eq!(
            rows[2],
            "2 older versions: ask with `before` set to 323232323232."
        );
        assert!(
            log(&history, 9, 2, &now, SealCheck::Checked)
                .starts_with("No versions")
        );
        assert!(
            log(&[], 0, 2, &now, SealCheck::Checked).contains("no versions")
        );
    }

    /// A prior copy, a side line or a set-aside version is never compared
    /// with a main-line one: no size change, never "same memory as above"
    #[test]
    fn log_compares_only_main_line_rows() {
        assert_eq!(
            VersionSource::PriorCopy.label().unwrap(),
            "a copy from before a memory loss"
        );
        for other in [
            VersionSource::PriorCopy,
            VersionSource::SideLine,
            VersionSource::OffLine,
            VersionSource::SetAside { on: None },
            VersionSource::NeverSaved,
        ] {
            let mut prior = version(2, 3, &"x".repeat(473));
            prior.source = other;
            let mut same = version(3, 2, "short");
            same.source = other;
            let history =
                [version(1, 4, "short"), same, prior, version(4, 1, "short")];
            let out = log(
                &history,
                0,
                20,
                &current(1, "short", &history),
                SealCheck::Checked,
            );
            let rows: Vec<&str> = out.lines().collect();
            // Nothing on the main line below the newest row to compare
            assert_eq!(
                rows[0],
                format!(
                    "2026-11-04 21:14  {}  head 313131313131  memory 5 chars  \
                     ← in your context",
                    id(1)
                ),
                "{other:?}"
            );
            assert!(
                !rows[1].contains("same memory as above"),
                "{other:?}: {out}"
            );
            assert!(
                !out.contains('\u{2212}')
                    && !out.contains("(+")
                    && !out.contains('±'),
                "{other:?}: {out}"
            );
        }
        // Main-line rows still compare: sealed with a local copy
        let mut copy = version(2, 1, "abc");
        copy.source = VersionSource::LocalCopy;
        copy.head = None;
        let history = [version(1, 2, "abcdef"), copy];
        let out = log(
            &history,
            0,
            20,
            &current(9, "z", &history),
            SealCheck::Checked,
        );
        assert!(
            out.lines().next().unwrap().ends_with("memory 6 chars (+3)"),
            "{out}"
        );
    }

    #[test]
    fn log_of_local_copies_says_so_once() {
        let history = [local(1, 2, "x"), local(2, 1, "y")];
        let out = log(
            &history,
            0,
            20,
            &current(9, "z", &history),
            SealCheck::Checked,
        );
        assert!(out.starts_with(LOCAL_ONLY), "{out}");
        assert!(!out.contains("local copy, not sealed"), "{out}");
        assert!(!out.contains("  head "), "{out}");
        assert!(out.ends_with(&format!("{}  memory 1 chars", id(2))));
    }

    /// Only the local-copy label goes, and only when every row is one
    #[test]
    fn other_labels_show_beside_local_copies() {
        let mut unsaved = local(3, 3, "u");
        unsaved.source = VersionSource::NeverSaved;
        let history = [unsaved, local(1, 2, "x"), local(2, 1, "y")];
        let out = log(
            &history,
            0,
            20,
            &current(9, "z", &history),
            SealCheck::Checked,
        );
        assert!(!out.starts_with(LOCAL_ONLY), "{out}");
        assert!(out.contains("(never saved: the run ended before saving)"));
        assert!(out.contains("(local copy, not sealed)"), "{out}");
    }

    /// A memory id names its oldest version, so a page never ends on a
    /// memory that recurs further down: paging visits every row once
    #[test]
    fn log_pages_through_repeated_memories() {
        // All local, A B A B A C: no heads to page by
        let history = [
            local(1, 9, "a"),
            local(2, 8, "b"),
            local(1, 7, "a"),
            local(2, 6, "b"),
            local(1, 5, "a"),
            local(3, 4, "c"),
        ];
        let now = current(1, "a", &history);
        let mut start = 0;
        let mut seen = Vec::new();
        for _ in 0..10 {
            let out = log(&history, start, 2, &now, SealCheck::Checked);
            seen.extend(
                out.lines()
                    .filter(|l| l.starts_with("2026"))
                    .map(|l| l[..16].to_string()),
            );
            let Some(next) =
                out.lines().last().and_then(|l| l.split("set to ").nth(1))
            else {
                break;
            };
            let before: HistoryRef =
                next.trim_end_matches('.').parse().unwrap();
            start = log_start(&history, &before).unwrap().0;
        }
        let want: Vec<String> = history.iter().map(|v| stamp(v.at)).collect();
        assert_eq!(seen, want);
    }

    #[test]
    fn log_start_reads_heads_and_memory_ids() {
        let history =
            [version(1, 3, "a"), version(2, 2, "b"), version(1, 1, "a")];
        let head: HistoryRef = short_head(history[1].head.as_ref().unwrap())
            .parse()
            .unwrap();
        assert_eq!(log_start(&history, &head).unwrap(), (2, None));
        let mem: HistoryRef = id(1).to_string().parse().unwrap();
        assert_eq!(log_start(&history, &mem).unwrap(), (3, None));
        // `mem-` with a head's digits
        let via: HistoryRef =
            format!("mem-{}", short_head(history[1].head.as_ref().unwrap()))
                .parse()
                .unwrap();
        let (start, note) = log_start(&history, &via).unwrap();
        assert_eq!(start, 2);
        assert!(note.unwrap().contains("matches no memory id"));
        let none: HistoryRef = "mem-999999".parse().unwrap();
        assert_eq!(log_start(&history, &none), Err(Resolution::None));
    }

    #[test]
    fn one_version_is_shown_whole() {
        let history = vec![
            version(1, 5, "now\n"),
            version(2, 4, "then\n"),
            version(3, 3, ""),
        ];
        let now = current(1, "now\n", &history);
        let out = show(&history, &[1], now, 32_768, SealCheck::Checked);
        assert_eq!(
            out,
            format!("{AS_STORED}\n\n{}:\nthen", history[1].describe())
        );
        let out = show(&history, &[2], now, 32_768, SealCheck::Checked);
        assert!(out.ends_with(":\n(empty)"), "{out}");
        // The current memory alone is named, not repeated
        assert_eq!(
            show(&history, &[0], now, 32_768, SealCheck::Checked),
            format!(
                "{} is your current memory (in context above).",
                history[0].describe()
            )
        );
    }

    #[test]
    fn show_opens_with_the_change_to_the_current_memory() {
        let now = lines("now", 1);
        let then = lines("then", 2);
        let older = lines("older", 3);
        let history = vec![
            version(1, 5, &now),
            version(2, 4, &then),
            version(3, 3, "skipped"),
            version(4, 2, &older),
        ];
        let out = show(
            &history,
            &[3, 1],
            current(1, &now, &history),
            32_768,
            SealCheck::Checked,
        );
        let blocks: Vec<&str> = out.split("\n\n").collect();
        assert_eq!(blocks[0], format!("{SHOW_LEGEND} {AS_STORED}"));
        assert!(
            blocks[1].starts_with(&format!(
                "{} → your current memory (in context above):\n",
                history[1].describe()
            )),
            "{}",
            blocks[1]
        );
        assert!(blocks[1].contains("  + line 0 edited in now"));
        assert!(blocks[1].contains("  - line 1 edited in then"));
        // Forward from the older to the newer, naming the gap
        assert!(
            blocks[2].starts_with(&format!(
                "{} → {}, 1 version between not asked for:\n",
                history[3].describe(),
                id(2)
            )),
            "{}",
            blocks[2]
        );
        assert!(blocks[2].contains("  - line 2 edited in older"));
        assert!(blocks[2].contains("  + line 2 is a line of memory"));
        assert_eq!(blocks.len(), 3);

        // Versions between the current memory and the newest asked for
        let out = show(
            &history,
            &[3, 2],
            current(1, &now, &history),
            32_768,
            SealCheck::Checked,
        );
        assert!(
            out.contains(&format!(
                "{}, 1 version between not asked for, shown in full",
                history[2].describe()
            )),
            "{out}"
        );
    }

    const C: SealCheck = SealCheck::Checked;

    /// Never a backward diff: a version newer than the current memory is
    /// shown whole; and a current memory no version holds is said to be so
    #[test]
    fn the_anchor_never_runs_backward() {
        let newer = lines("newer", 1);
        let now = lines("now", 2);
        let older = lines("older", 3);
        let history = vec![
            version(1, 5, &newer),
            version(2, 4, &now),
            version(3, 3, &older),
        ];
        let out =
            show(&history, &[0, 2], current(2, &now, &history), 32_768, C);
        let blocks: Vec<&str> = out.split("\n\n").collect();
        assert_eq!(
            blocks[1].lines().next().unwrap(),
            format!(
                "{}, shown in full: newer than your current memory:",
                history[0].describe()
            )
        );
        let other = lines("x", 1);
        let out =
            show(&history, &[1, 2], current(9, &other, &history), 32_768, C);
        assert!(
            out.contains("your current memory is not among your kept versions"),
            "{out}"
        );
    }

    #[test]
    fn the_current_memory_is_named_not_repeated() {
        let now = lines("now", 1);
        let then = lines("then", 2);
        let history = vec![version(1, 5, &now), version(2, 4, &then)];
        let out =
            show(&history, &[0, 1], current(1, &now, &history), 32_768, C);
        let blocks: Vec<&str> = out.split("\n\n").collect();
        assert_eq!(
            blocks[1],
            format!(
                "{} is your current memory (in context above).",
                history[0].describe()
            )
        );
        assert!(blocks[2].starts_with(&format!(
            "{} → {}:",
            history[1].describe(),
            id(1)
        )));
    }

    /// Sameness is by text, not id: a replaced key changes ids, not text
    #[test]
    fn same_text_under_another_id_is_the_same_memory() {
        let history =
            vec![version(1, 5, "x\ny\nz\n"), version(2, 4, "x\ny\nz\n")];
        let out = show(
            &history,
            &[0, 1],
            current(9, "x\ny\nz\n", &history),
            32_768,
            C,
        );
        assert!(out.contains(": the same memory."), "{out}");
        assert!(
            diff(&history[1], Some(&history[0]), current(9, "", &history))
                .ends_with("nothing changed.")
        );
        let out = log(&history, 0, 20, &current(9, "", &history), C);
        assert!(out.contains("(same memory as above)"), "{out}");
    }

    fn unopened(n: u8, day: Option<u32>) -> MemoryVersion {
        MemoryVersion {
            head: Some(Sha256Hex::from([n.wrapping_add(0x30); 32])),
            at: day.map(at),
            source: VersionSource::Sealed,
            anchored: None,
            text: VersionText::Unopened {
                id: None,
                reason: "Agora didn't answer".into(),
            },
        }
    }

    #[test]
    fn new_labels() {
        assert_eq!(
            VersionSource::OffLine.label().unwrap(),
            "sealed, off your main line"
        );
        assert_eq!(
            VersionSource::SetAside { on: None }.label().unwrap(),
            "set aside when your line was rebased, date unknown"
        );
        assert_eq!(
            VersionSource::SetAside {
                on: Some(at(2).date_naive())
            }
            .label()
            .unwrap(),
            "set aside on 2026-11-02 when your line was rebased"
        );
    }

    /// Unknown dates sort last and read "date unknown"
    #[test]
    fn unknown_dates_sort_last() {
        let mut history = vec![
            local(1, 1, "a"),
            MemoryVersion {
                at: None,
                ..local(2, 1, "b")
            },
            local(3, 3, "c"),
        ];
        newest_first(&mut history);
        let ids: Vec<_> = history.iter().map(|v| v.id().unwrap()).collect();
        assert_eq!(ids, [id(3), id(1), id(2)]);
        let out = log(&history, 0, 20, &current(9, "z", &history), C);
        assert!(
            out.ends_with(&format!("date unknown  {}  memory 1 chars", id(2))),
            "{out}"
        );
    }

    /// An unopened version keeps its row, is never an empty memory, and
    /// diffs pass over it
    #[test]
    fn unopened_versions_say_so() {
        let now = lines("now", 1);
        let mid = lines("mid", 2);
        let history = vec![
            version(1, 5, &now),
            unopened(7, Some(4)),
            version(2, 3, &mid),
            unopened(8, None),
        ];
        let cur = current(1, &now, &history);
        let out = log(&history, 0, 20, &cur, C);
        let rows: Vec<&str> = out.lines().collect();
        // The row above it has no delta against it
        assert!(rows[0].ends_with("chars  ← in your context"), "{out}");
        assert_eq!(
            rows[1],
            "2026-11-04 21:14  head 373737373737  (couldn't be opened just \
             now: Agora didn't answer)"
        );
        assert!(!rows[2].contains("same memory"), "{out}");
        assert!(
            !rows[2].contains("(+") && !rows[2].contains('\u{2212}'),
            "{out}"
        );
        assert_eq!(
            rows[3],
            "date unknown  head 383838383838  (couldn't be opened just now: \
             Agora didn't answer)"
        );

        // `show`: its own note, and the diff runs past it
        let out = show(&history, &[0, 1, 2], cur, 32_768, C);
        let blocks: Vec<&str> = out.split("\n\n").collect();
        assert_eq!(
            blocks[2],
            "head 373737373737 (2026-11-04 21:14) couldn't be opened just \
             now (Agora didn't answer); ask again later."
        );
        assert!(
            blocks[3].starts_with(&format!(
                "{} → {}:",
                history[2].describe(),
                id(1)
            )),
            "{}",
            blocks[3]
        );
        let alone = show(&history, &[3], cur, 32_768, C);
        assert!(alone.ends_with("ask again later."), "{alone}");
        assert!(!alone.contains("(empty)"));

        // `diff` says so instead of diffing
        let out = diff(&history[1], None, cur);
        assert_eq!(
            out,
            "No diff: head 373737373737 (2026-11-04 21:14) couldn't be opened \
             just now (Agora didn't answer); ask again later."
        );
        assert_eq!(out.matches(UNOPENED).count(), 1);
        let out = diff(&history[0], Some(&history[3]), cur);
        assert!(out.contains("ask again later"), "{out}");
    }

    /// An unchecked history says so, never "they aren't sealed", and shows
    /// no head
    #[test]
    fn an_unchecked_history_vouches_for_nothing() {
        let history = vec![local(1, 2, "x\n"), local(2, 1, "y\n")];
        let cur = current(1, "x\n", &history);
        let out = log(&history, 0, 20, &cur, SealCheck::Unchecked);
        assert!(out.starts_with(UNCHECKED), "{out}");
        assert!(!out.contains(LOCAL_ONLY));
        assert!(!out.contains("local copy, not sealed"), "{out}");
        // Heads passed anyway are not shown
        let sealed = vec![version(1, 2, "x\n"), version(2, 1, "y\n")];
        let out = log(&sealed, 0, 1, &cur, SealCheck::Unchecked);
        assert!(!out.contains("  head "), "{out}");
        assert!(out.ends_with(&format!("set to {}.", id(1))), "{out}");

        let out = show(&history, &[1], cur, 32_768, SealCheck::Unchecked);
        assert!(
            out.starts_with(&format!("{UNCHECKED} {AS_STORED}\n\n")),
            "{out}"
        );
        let out = show(&history, &[0, 1], cur, 32_768, SealCheck::Unchecked);
        assert!(
            out.starts_with(&format!(
                "{UNCHECKED} {SHOW_LEGEND} {AS_STORED}\n\n"
            )),
            "{out}"
        );
        let out = show(&history, &[0], cur, 32_768, SealCheck::Unchecked);
        assert!(out.starts_with(&format!("{UNCHECKED}\n\n")), "{out}");

        // Nothing fits the budget: the list, still under the preface
        for picked in [&[1][..], &[0, 1]] {
            let out = show(&history, picked, cur, 10, SealCheck::Unchecked);
            assert!(out.starts_with(&format!("{UNCHECKED}\n\n")), "{out}");
            assert!(out.contains("budget of 10 bytes"), "{out}");
            // The same list, under the other preface: these are local
            // copies, so checked says that once instead
            let checked = show(&history, picked, cur, 10, SealCheck::Checked);
            assert_eq!(
                out.strip_prefix(UNCHECKED),
                checked.strip_prefix(LOCAL_ONLY),
                "{checked}"
            );
        }
    }

    /// One version besides the current memory, left out: "fewer" can't
    /// help, so the note says a larger budget or `diff` (beta round 17)
    #[test]
    fn one_other_version_past_the_budget_is_not_told_fewer() {
        let now = lines("now", 1);
        let history =
            vec![version(1, 3, &now), version(2, 2, &lines("then", 20))];
        let cur = current(1, &now, &history);
        // The current memory's line fits; the other version doesn't
        let named = format!(
            "{} is your current memory (in context above).",
            history[0].describe()
        );
        let budget = SHOW_LEGEND.len() + AS_STORED.len() + named.len() + 400;
        let out = show(&history, &[0, 1], cur, budget, SealCheck::Checked);
        assert!(out.contains(&named), "{out}");
        assert!(
            out.ends_with(&format!(
                "more than your budget of {} bytes; ask again with a larger \
                 budget, or `diff` it against a version near it.",
                grouped(budget)
            )),
            "{out}"
        );
        assert!(!out.contains("fewer"), "{out}");
        // Two others left out: fewer can help
        let three = vec![
            version(1, 3, &now),
            version(2, 2, &lines("then", 20)),
            version(3, 1, &lines("older", 20)),
        ];
        let cur = current(1, &now, &three);
        let out = show(&three, &[0, 1, 2], cur, budget, SealCheck::Checked);
        assert!(out.contains("ask for fewer, or a larger budget:"), "{out}");
    }

    /// As `log`: when every version in view is a local copy, `show` and
    /// `diff` say so once, not on each version (beta round 17)
    #[test]
    fn show_and_diff_say_local_copy_once() {
        let label = "local copy, not sealed";
        let history = vec![
            local(1, 3, &lines("now", 1)),
            local(2, 2, &lines("then", 2)),
            local(3, 1, &lines("older", 3)),
        ];
        let cur = current(1, history[0].content().unwrap(), &history);
        for picked in [&[1][..], &[1, 2], &[0, 1, 2]] {
            let out = show(&history, picked, cur, 32_768, SealCheck::Checked);
            assert!(!out.contains(label), "{out}");
            assert!(out.starts_with(&format!("{LOCAL_ONLY} ")), "{out}");
            assert_eq!(out.matches(LOCAL_ONLY).count(), 1, "{out}");
        }
        // Past the budget, the list too
        let out = show(&history, &[1, 2], cur, 10, SealCheck::Checked);
        assert!(
            out.starts_with(&format!("{LOCAL_ONLY}\n\nNot shown")),
            "{out}"
        );
        assert!(!out.contains(label), "{out}");
        for out in [
            diff(&history[2], Some(&history[1]), cur),
            diff(&history[1], None, cur),
            diff(&history[1], Some(&history[1]), cur),
        ] {
            assert!(out.starts_with(&format!("{LOCAL_ONLY}\n\n")), "{out}");
            assert!(!out.contains(label), "{out}");
        }

        // Mixed: each version keeps its label, and nothing is said once
        let mixed = vec![version(1, 3, &lines("now", 1)), history[1].clone()];
        let cur = current(1, mixed[0].content().unwrap(), &mixed);
        let out = show(&mixed, &[0, 1], cur, 32_768, SealCheck::Checked);
        assert!(out.contains(label) && !out.contains(LOCAL_ONLY), "{out}");
        let out = diff(&mixed[1], Some(&mixed[0]), cur);
        assert!(out.contains(label) && !out.contains(LOCAL_ONLY), "{out}");
    }

    /// 20 of 60 lines changed, the changed ones long: past half the
    /// version's size, but shorter than it, so a diff (beta round 16)
    #[test]
    fn long_changed_lines_still_diff() {
        let text = |tag: &str| -> String {
            (0..60)
                .map(|i| {
                    if i < 20 {
                        format!(
                            "{i} {tag} {}\n",
                            "long changed words ".repeat(8)
                        )
                    } else {
                        format!("{i} kept {}\n", "steady words here ".repeat(8))
                    }
                })
                .collect()
        };
        let (now, then) = (text("now"), text("then"));
        let history = vec![version(1, 5, &now), version(2, 4, &then)];
        let diff = line_diff(&then, &now);
        assert!(diff.len() * 2 > then.len() && diff.len() < then.len() * 2);
        let out = show(
            &history,
            &[1, 0],
            current(1, &now, &history),
            1 << 20,
            SealCheck::Checked,
        );
        assert!(!out.contains("shown in full"), "{out}");
    }

    #[test]
    fn a_version_shorter_than_its_changes_is_shown_whole() {
        let now = lines("now", 20);
        let then = lines("then", 0);
        let older = lines("older", 20);
        let history = vec![
            version(1, 5, &now),
            version(2, 4, &then),
            version(3, 3, &older),
        ];
        let out = show(
            &history,
            &[1, 2],
            current(1, &now, &history),
            32_768,
            SealCheck::Checked,
        );
        let blocks: Vec<&str> = out.split("\n\n").collect();
        assert_eq!(
            blocks[1].lines().next().unwrap(),
            format!(
                "{}, shown in full (shorter than its changes):",
                history[1].describe()
            )
        );
        assert!(blocks[1].ends_with(then.trim_end()));
        assert_eq!(
            blocks[2].lines().next().unwrap(),
            format!(
                "{}, shown in full (shorter than its changes):",
                history[2].describe()
            )
        );
        assert!(blocks[2].ends_with(older.trim_end()));
    }

    #[test]
    fn past_the_budget_versions_are_listed_not_cut() {
        let history: Vec<MemoryVersion> = (0..4)
            .map(|i| {
                version(i + 1, 5 - u32::from(i), &lines(&format!("v{i}"), 20))
            })
            .collect();
        let now = current(1, history[0].content().unwrap(), &history);
        let full =
            show(&history, &[0, 1, 2, 3], now, 1 << 20, SealCheck::Checked);
        let budget = full.len() / 2;
        let out =
            show(&history, &[0, 1, 2, 3], now, budget, SealCheck::Checked);
        // The footer counts too
        assert!(out.len() <= budget, "{} > {budget}", out.len());
        let (shown, listed) = out.split_once("Not shown").unwrap();
        // Whole blocks only
        for block in shown.trim_end().split("\n\n").skip(1) {
            assert!(full.contains(block));
        }
        let listed: Vec<&str> = listed.lines().skip(1).collect();
        assert!(!listed.is_empty());
        assert_eq!(
            listed.last().unwrap(),
            &format!("- {}", history[3].describe())
        );

        // One version past the budget asks for a larger budget
        let out = show(&history, &[2], now, 10, SealCheck::Checked);
        assert!(out.contains("more than your budget of 10 bytes; ask again with a larger budget"), "{out}");
        assert!(!out.contains("fewer"), "{out}");

        // At the largest budget, never "a larger budget"
        let huge: String =
            "x".repeat(GetContentInput::MAX_COMMENT_BUDGET as usize + 1);
        let big = [version(7, 1, &huge), version(8, 1, &huge)];
        let max = GetContentInput::MAX_COMMENT_BUDGET as usize;
        let out =
            show(&big, &[0], current(9, "", &big), max, SealCheck::Checked);
        assert!(out.contains("can't be shown whole"), "{}", &out[..200]);
        assert!(!out.contains("larger budget"));
        let out =
            show(&big, &[0, 1], current(9, "", &big), max, SealCheck::Checked);
        assert!(
            out.contains("ask for fewer:"),
            "{}",
            &out[..200.min(out.len())]
        );
        assert!(!out.contains("larger budget"));
    }

    #[test]
    fn diff_is_whole_and_counts_its_units() {
        let a = version(1, 1, "a\nb\nc\n");
        let b = version(2, 2, "a\nB\nc\nd\n");
        let now = current(3, "a\n", &[]);
        let out = diff(&a, Some(&b), now);
        assert_eq!(
            out,
            format!(
                "{} → {} (+2 / \u{2212}1 lines):\n  (1 line unchanged before)\n  - b\n  + B\n  (1 line unchanged)\n  + d\n\n{AS_STORED}",
                a.describe(),
                b.describe()
            )
        );
        let out = diff(&a, None, now);
        assert!(out.starts_with(&format!(
            "{} → your current memory (in context above) (+0 / \u{2212}2 lines):",
            a.describe()
        )), "{out}");
        assert!(diff(&a, Some(&a), now).ends_with("nothing changed."));
        // Two lines or fewer are compared by sentence
        let short = version(4, 1, "One. Two.");
        let out = diff(&short, None, current(5, "One. Three.", &[]));
        assert!(out.contains("(+1 / \u{2212}1 sentences)"), "{out}");
    }
}
