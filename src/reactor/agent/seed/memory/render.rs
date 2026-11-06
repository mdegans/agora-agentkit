//! What the `memory` tool's `log`, `show` and `diff` say, from versions the
//! runner gathers. Pure: no I/O, no keys.

use chrono::{DateTime, NaiveDate, Utc};

use super::id::{MemoryId, short_head};
use crate::govlog::Sha256Hex;
use crate::ids::GovernanceLogId;
use crate::text::line_diff;

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
    /// Set aside on a date, when the agent's line was rebased
    SetAside { on: NaiveDate },
    /// Never saved: the run ended before saving (unsigned)
    NeverSaved,
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
            Self::SetAside { on } => {
                Some(format!("set aside on {on} when your line was rebased"))
            }
            Self::NeverSaved => {
                Some("never saved: the run ended before saving".into())
            }
        }
    }
}

/// One version of an agent's memory, as the runner found it
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryVersion {
    pub id: MemoryId,
    /// Its sealed head, when it has one
    pub head: Option<Sha256Hex>,
    /// When it was saved (a sealed entry's `state_at`, an archive's time)
    pub at: DateTime<Utc>,
    pub source: VersionSource,
    /// The anchor its head is in, if any
    pub anchored: Option<GovernanceLogId>,
    /// `memory.content`, raw
    pub content: String,
}

impl MemoryVersion {
    /// Its size, in characters
    pub fn chars(&self) -> usize {
        self.content.chars().count()
    }

    /// `mem-3f9a1c07be (2026-11-05 21:14, 13,539 chars)`, with its source's
    /// label when it has one
    pub fn describe(&self) -> String {
        let label = self
            .source
            .label()
            .map(|l| format!(", {l}"))
            .unwrap_or_default();
        format!(
            "{} ({}, {} chars{label})",
            self.id,
            stamp(self.at),
            grouped(self.chars())
        )
    }
}

/// The memory in the agent's context this session: what `show` is
/// anchored on and `diff` compares with when it is given one version
#[derive(Debug, Clone, Copy)]
pub struct CurrentMemory<'a> {
    pub id: MemoryId,
    pub content: &'a str,
}

/// What `log` says above its rows when no version is sealed
pub const LOCAL_ONLY: &str = "These are your runner's local copies. They \
     aren't sealed, so they have no heads.";

/// What `show` says first when it shows more than one version
pub const SHOW_LEGEND: &str = "Newest first. Each change runs forward, \
     from the older version to the newer: `-` lines were in the older, `+` \
     lines are in the newer.";

/// What heads a diff to the current memory
const TO_CURRENT: &str = "your current memory (in context above)";

/// `2026-11-05 21:14`, UTC
fn stamp(at: DateTime<Utc>) -> String {
    at.format("%Y-%m-%d %H:%M").to_string()
}

/// `13,539`
pub(crate) fn grouped(n: usize) -> String {
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

/// `log`: up to `limit` rows of `history` (every version, newest first)
/// from `start`, each sized against the version below it. `current` is
/// the memory in context: its newest version is marked "this session".
///
/// `2026-11-05 21:14  mem-3f9a1c07be  head 346ac9543d0c  memory 13,539
/// chars (−1,204)  ← this session`
pub fn log(
    history: &[MemoryVersion],
    start: usize,
    limit: usize,
    current: &MemoryId,
) -> String {
    let mut out = String::new();
    let sealed = history.iter().any(|v| v.source == VersionSource::Sealed);
    if history.is_empty() {
        return "Your runner has kept no versions of your memory.".into();
    }
    if !sealed {
        out.push_str(LOCAL_ONLY);
        out.push_str("\n\n");
    }
    let this_session = history.iter().position(|v| v.id == *current);
    let end = start.saturating_add(limit).min(history.len());
    if start >= history.len() {
        return format!(
            "No versions are older than that: you have {} in all.",
            history.len()
        );
    }
    for i in start..end {
        let v = &history[i];
        let mut row = format!("{}  {}", stamp(v.at), v.id);
        if let Some(head) = &v.head {
            row.push_str(&format!("  head {}", short_head(head)));
        }
        if i > start && history[i - 1].id == v.id {
            row.push_str("  (same memory as above)");
        } else {
            row.push_str(&format!("  memory {} chars", grouped(v.chars())));
            if let Some(older) = history.get(i + 1) {
                row.push(' ');
                row.push_str(&delta(v.chars(), older.chars()));
            }
        }
        if sealed && let Some(label) = v.source.label() {
            row.push_str(&format!("  ({label})"));
        }
        if let Some(anchor) = &v.anchored {
            row.push_str(&format!("  anchored in {anchor}"));
        }
        if this_session == Some(i) {
            row.push_str("  ← this session");
        }
        out.push_str(&row);
        out.push('\n');
    }
    let older = history.len() - end;
    if older > 0 {
        let last = &history[end - 1];
        let next = match &last.head {
            Some(head) => short_head(head),
            None => last.id.to_string(),
        };
        out.push_str(&format!(
            "{older} older version{}: ask with `before` set to {next}.\n",
            if older == 1 { "" } else { "s" }
        ));
    }
    out.truncate(out.trim_end().len());
    out
}

/// `diff`'s result, and `show`'s changes: from `before` to `after`,
/// with its line counts
fn changes(before: &str, after: &str) -> (String, usize, usize) {
    let diff = line_diff(before, after);
    let added = diff.lines().filter(|l| l.starts_with("  + ")).count();
    let removed = diff.lines().filter(|l| l.starts_with("  - ")).count();
    (diff, added, removed)
}

/// Whether a diff is too large to stand for the version it describes: over
/// half its size (the keyframe threshold)
fn mostly_rewritten(diff: &str, version: &str) -> bool {
    diff.len() * 2 > version.len()
}

/// `n versions between not asked for`, or nothing
fn between(n: usize) -> String {
    match n {
        0 => String::new(),
        1 => ", 1 version between not asked for".into(),
        n => format!(", {n} versions between not asked for"),
    }
}

/// A version whole, under its header
fn whole(header: &str, v: &MemoryVersion) -> String {
    format!("{header}:\n{}", v.content.trim_end())
}

/// `show`: the versions `picked` (indexes into `history`, newest first,
/// in any order) like `git log -p`, at most `budget` bytes.
///
/// One version is shown whole. Of several, the newest opens with what
/// changed from it to `current`, or whole when that is over half its size;
/// each older one is what changed from it to the next newer one picked, or
/// whole ("mostly rewritten") past the same threshold. Past `budget`, the
/// rest are listed by id and size, never cut.
pub fn show(
    history: &[MemoryVersion],
    picked: &[usize],
    current: CurrentMemory<'_>,
    budget: usize,
) -> String {
    let mut picked: Vec<usize> = picked
        .iter()
        .copied()
        .filter(|&i| i < history.len())
        .collect();
    picked.sort_unstable();
    picked.dedup();
    let Some(&newest) = picked.first() else {
        return "No versions to show.".into();
    };

    let mut blocks: Vec<(usize, String)> = Vec::new();
    if let [only] = picked.as_slice() {
        let v = &history[*only];
        blocks.push((*only, whole(&v.describe(), v)));
    } else {
        let v = &history[newest];
        let block = if v.id == current.id {
            format!("{} is {TO_CURRENT}.", v.describe())
        } else {
            let (diff, _, _) = changes(&v.content, current.content);
            let gap = history
                .iter()
                .position(|h| h.id == current.id)
                .filter(|&c| c < newest)
                .map(|c| newest - c - 1)
                .unwrap_or(0);
            let gap = between(gap);
            if mostly_rewritten(&diff, &v.content) {
                whole(
                    &format!(
                        "{}{gap}, shown in full: mostly rewritten since",
                        v.describe()
                    ),
                    v,
                )
            } else {
                format!(
                    "{} → {TO_CURRENT}{gap}:\n{}",
                    v.describe(),
                    diff.trim_end()
                )
            }
        };
        blocks.push((newest, block));
        for pair in picked.windows(2) {
            let (newer, older) = (&history[pair[0]], &history[pair[1]]);
            let gap = between(pair[1] - pair[0] - 1);
            let block = if older.id == newer.id {
                format!(
                    "{} → {}{gap}: the same memory.",
                    older.describe(),
                    newer.id
                )
            } else {
                let (diff, _, _) = changes(&older.content, &newer.content);
                if mostly_rewritten(&diff, &older.content) {
                    whole(
                        &format!(
                            "{}{gap}, shown in full: mostly rewritten",
                            older.describe()
                        ),
                        older,
                    )
                } else {
                    format!(
                        "{} → {}{gap}:\n{}",
                        older.describe(),
                        newer.id,
                        diff.trim_end()
                    )
                }
            };
            blocks.push((pair[1], block));
        }
    }

    let mut out = String::new();
    if picked.len() > 1 {
        out.push_str(SHOW_LEGEND);
    }
    let mut used = out.len();
    let mut left: Vec<usize> = Vec::new();
    for (i, block) in blocks {
        if !left.is_empty() || used + block.len() + 2 > budget {
            left.push(i);
            continue;
        }
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        used += block.len() + 2;
        out.push_str(&block);
    }
    if !left.is_empty() {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(&format!(
            "Not shown, past your budget of {} bytes; ask for fewer, or a \
             larger budget:",
            grouped(budget)
        ));
        for i in left {
            out.push_str("\n- ");
            out.push_str(&history[i].describe());
        }
    }
    out
}

/// `diff`: what changed from `a` to `b` (or to `current`, when there is
/// no `b`), whole
pub fn diff(
    a: &MemoryVersion,
    b: Option<&MemoryVersion>,
    current: CurrentMemory<'_>,
) -> String {
    let (to_id, to_name, to_text) = match b {
        Some(b) => (b.id, b.describe(), b.content.as_str()),
        None => (current.id, TO_CURRENT.to_string(), current.content),
    };
    if a.id == to_id {
        return format!(
            "{} and {to_name} are the same memory: nothing changed.",
            a.describe()
        );
    }
    let (diff, added, removed) = changes(&a.content, to_text);
    format!(
        "{} → {to_name} (+{added} / \u{2212}{removed} lines):\n{}",
        a.describe(),
        diff.trim_end()
    )
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
            id: id(n),
            head: Some(Sha256Hex::from([n.wrapping_add(0x30); 32])),
            at: at(day),
            source: VersionSource::Sealed,
            anchored: None,
            content: content.into(),
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
        let out = log(&history, 0, 20, &id(1));
        let rows: Vec<&str> = out.lines().collect();
        assert_eq!(
            rows[0],
            format!(
                "2026-11-05 21:14  {}  head {}  memory 6 chars (±0)  ← this session",
                id(1),
                "313131313131"
            )
        );
        assert_eq!(
            rows[1],
            format!(
                "2026-11-04 21:14  {}  head 313131313131  (same memory as above)  \
                 anchored in ANC-2026-0005",
                id(1)
            )
        );
        assert!(rows[2].ends_with("memory 8 chars (+5)"), "{}", rows[2]);
        assert!(
            rows[3].ends_with("(on a side line your operator rebased)"),
            "{}",
            rows[3]
        );
        assert!(rows[4].ends_with("memory 1 chars"), "{}", rows[4]);
        assert_eq!(rows.len(), 5);

        // A page, with the way on
        let out = log(&history, 1, 2, &id(1));
        let rows: Vec<&str> = out.lines().collect();
        assert_eq!(rows.len(), 3);
        // The first row of a page is never "same as above": none is shown
        assert!(rows[0].contains("memory 6 chars"), "{}", rows[0]);
        assert!(!rows[0].contains("this session"));
        assert_eq!(
            rows[2],
            "2 older versions: ask with `before` set to 323232323232."
        );
        assert!(log(&history, 9, 2, &id(1)).starts_with("No versions"));
        assert!(log(&[], 0, 2, &id(1)).contains("no versions"));
    }

    #[test]
    fn log_of_local_copies_says_so_once() {
        let mut a = version(1, 2, "x");
        a.head = None;
        a.source = VersionSource::LocalCopy;
        let mut b = a.clone();
        b.id = id(2);
        b.at = at(1);
        let out = log(&[a, b], 0, 20, &id(9));
        assert!(out.starts_with(LOCAL_ONLY), "{out}");
        assert!(!out.contains("local copy, not sealed"), "{out}");
        assert!(!out.contains("  head "), "{out}");
        assert!(out.ends_with(&format!("{}  memory 1 chars", id(2))));
    }

    #[test]
    fn one_version_is_shown_whole() {
        let history = vec![version(1, 5, "now\n"), version(2, 4, "then\n")];
        let current = CurrentMemory {
            id: id(1),
            content: "now\n",
        };
        let out = show(&history, &[1], current, 32_768);
        assert_eq!(out, format!("{}:\nthen", history[1].describe()));
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
        let current = CurrentMemory {
            id: id(1),
            content: &now,
        };
        let out = show(&history, &[3, 1], current, 32_768);
        let blocks: Vec<&str> = out.split("\n\n").collect();
        assert_eq!(blocks[0], SHOW_LEGEND);
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
    }

    #[test]
    fn the_current_memory_is_named_not_repeated() {
        let now = lines("now", 1);
        let then = lines("then", 2);
        let history = vec![version(1, 5, &now), version(2, 4, &then)];
        let current = CurrentMemory {
            id: id(1),
            content: &now,
        };
        let out = show(&history, &[0, 1], current, 32_768);
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

    #[test]
    fn a_mostly_rewritten_version_is_shown_whole() {
        let now = lines("now", 20);
        let then = lines("then", 0);
        let older = lines("older", 20);
        let history = vec![
            version(1, 5, &now),
            version(2, 4, &then),
            version(3, 3, &older),
        ];
        let current = CurrentMemory {
            id: id(9),
            content: &now,
        };
        let out = show(&history, &[1, 2], current, 32_768);
        let blocks: Vec<&str> = out.split("\n\n").collect();
        assert_eq!(
            blocks[1].lines().next().unwrap(),
            format!(
                "{}, shown in full: mostly rewritten since:",
                history[1].describe()
            )
        );
        assert!(blocks[1].ends_with(then.trim_end()));
        assert_eq!(
            blocks[2].lines().next().unwrap(),
            format!(
                "{}, shown in full: mostly rewritten:",
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
        let current = CurrentMemory {
            id: id(1),
            content: &history[0].content,
        };
        let full = show(&history, &[0, 1, 2, 3], current, 1 << 20);
        let budget = full.len() / 2;
        let out = show(&history, &[0, 1, 2, 3], current, budget);
        assert!(out.len() < full.len());
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

        // One version past the budget is listed too
        let out = show(&history, &[2], current, 10);
        assert!(
            out.starts_with("Not shown, past your budget of 10 bytes"),
            "{out}"
        );
    }

    #[test]
    fn diff_is_whole_and_counts_lines() {
        let a = version(1, 1, "a\nb\nc\n");
        let b = version(2, 2, "a\nB\nc\nd\n");
        let current = CurrentMemory {
            id: id(3),
            content: "a\n",
        };
        let out = diff(&a, Some(&b), current);
        assert_eq!(
            out,
            format!(
                "{} → {} (+2 / \u{2212}1 lines):\n  (1 line unchanged before)\n  - b\n  + B\n  (1 line unchanged)\n  + d",
                a.describe(),
                b.describe()
            )
        );
        let out = diff(&a, None, current);
        assert!(out.starts_with(&format!(
            "{} → your current memory (in context above) (+0 / \u{2212}2 lines):",
            a.describe()
        )), "{out}");
        assert!(diff(&a, Some(&a), current).ends_with("nothing changed."));
    }
}
