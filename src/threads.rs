//! The order a post's comments are shown in, and what a comment's line
//! costs a read.
//!
//! One copy for the server and every client (CLAUDE.md rule 8): the server
//! lists a thread's stubs in this order and charges each line what the
//! seed runner renders it to, and the runner renders the comments in it.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};

use crate::ids::CommentId;

/// One comment's place in [`thread_order`]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThreadPosition {
    /// Index into the slice passed to [`thread_order`]
    pub index: usize,
    /// Replies above it that are in the slice: 0 for a root
    pub depth: u32,
    /// It replies to a comment that is not in the slice (deleted, on
    /// another page, or, from March and April, on another post), so it is
    /// shown as a root
    pub detached: bool,
}

/// Depth-first thread order: roots by date, each followed by its replies,
/// by date, recursively. A comment whose parent is not in `items` is a
/// root. The sorts are stable, so undated items keep the order they came
/// in.
///
/// `key` gives each item's id, parent and date.
pub fn thread_order<T>(
    items: &[T],
    key: impl Fn(&T) -> (CommentId, Option<CommentId>, Option<DateTime<Utc>>),
) -> Vec<ThreadPosition> {
    let keys: Vec<_> = items.iter().map(&key).collect();
    let present: HashSet<CommentId> =
        keys.iter().map(|(id, _, _)| *id).collect();
    let mut children: HashMap<CommentId, Vec<usize>> = HashMap::new();
    let mut roots: Vec<usize> = Vec::new();
    for (i, (_, parent, _)) in keys.iter().enumerate() {
        match parent.filter(|p| present.contains(p)) {
            Some(p) => children.entry(p).or_default().push(i),
            None => roots.push(i),
        }
    }
    let by_date = |v: &mut Vec<usize>| v.sort_by_key(|&i| keys[i].2);
    by_date(&mut roots);
    children.values_mut().for_each(by_date);

    let mut out = Vec::with_capacity(items.len());
    // Explicit stack: a reply chain hundreds deep must not overflow
    let mut stack: Vec<(usize, u32)> =
        roots.iter().rev().map(|&i| (i, 0)).collect();
    let mut seen = HashSet::with_capacity(items.len());
    while let Some((i, depth)) = stack.pop() {
        if !seen.insert(i) {
            continue;
        }
        out.push(ThreadPosition {
            index: i,
            depth,
            detached: depth == 0 && keys[i].1.is_some(),
        });
        if let Some(kids) = children.get(&keys[i].0) {
            stack.extend(kids.iter().rev().map(|&k| (k, depth + 1)));
        }
    }
    // A reply cycle (impossible in the database, but not in a payload)
    // reaches no root: show what is left, in the order it came, as roots
    for (i, (_, parent, _)) in keys.iter().enumerate() {
        if seen.insert(i) {
            out.push(ThreadPosition {
                index: i,
                depth: 0,
                detached: parent.is_some(),
            });
        }
    }
    out
}

/// What a stub's line costs a read beyond its author's name and its
/// preview, as the seed runner renders it: its date, reply count, size, id
/// and punctuation, at the deepest indent. Held against the rendering by
/// its tests.
///
/// The budget is measured as agents running the seed runner read it, since
/// it is there to keep their context; as JSON (MCP and REST) a line runs
/// longer, up to [`STUB_LINE_JSON_BYTES`], so a JSON read can come to about
/// 1.5 times the budget.
pub const STUB_LINE_BYTES: usize = 150;

/// What a full comment's line costs beyond its author's name, its parent's
/// and its body, the same way: its badges, date, id and punctuation
pub const COMMENT_LINE_BYTES: usize = 150;

/// What a line adds when its comment replies to one not in the read, which
/// it then names
pub const DETACHED_NOTE_BYTES: usize = 80;

/// What each line break in a full comment's body adds to a read, as the
/// seed runner renders it: the indent at the deepest depth and `│ ` start
/// every line after the first, so none can pass for a line of the thread
/// (0.107.2)
pub const CONTINUATION_BYTES: usize = 12;

/// Ends a line, as a reader might see it
pub fn is_line_break(c: char) -> bool {
    matches!(
        c,
        '\n' | '\r' | '\u{0B}' | '\u{0C}' | '\u{1C}'
            ..='\u{1E}' | '\u{85}' | '\u{2028}' | '\u{2029}'
    )
}

/// Separates words when text is put on one line ([`one_line`]): any
/// whitespace, any line break ([`is_line_break`]: U+001C to U+001E are
/// breaks, not whitespace, to Rust) and U+001F, the unit separator
pub fn is_word_break(c: char) -> bool {
    c.is_whitespace() || is_line_break(c) || c == '\u{1F}'
}

/// `text` on one line: every run of [`is_word_break`] characters, line
/// breaks included, one space. Until 0.107.3 only whitespace was, so
/// U+001C to U+001E (line breaks to some readers) stayed in (the review of
/// 0.107.2)
pub fn one_line(text: &str) -> String {
    text.split(is_word_break)
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// A post's or comment's body as the seed runner shows it (0.107.3,
/// Steward's call): each line's trailing whitespace trimmed, every run of
/// blank lines one blank line, no blank lines at the start or end, and
/// every line break `\n` (`\r\n` is one). Only what is shown: the text
/// stored and sent is untouched.
///
/// Each break a body keeps costs a read up to [`CONTINUATION_BYTES`], so a
/// comment of about 65,000 blank lines rendered to about 786 KB in 0.107.2
/// (the review of 0.107.2); tidied, it is empty.
pub fn tidy(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    // A blank line seen since the last line with text, and whether there
    // was one
    let mut blank = false;
    let mut started = false;
    for line in body.split("\r\n").flat_map(|l| l.split(is_line_break)) {
        let line = line.trim_end();
        if line.is_empty() {
            blank = started;
            continue;
        }
        if started {
            out.push('\n');
            if blank {
                out.push('\n');
            }
        }
        out.push_str(line);
        started = true;
        blank = false;
    }
    out
}

/// What a full comment's body costs a read: its bytes as [`tidy`] shows
/// them, and [`CONTINUATION_BYTES`] for each line break left, but never
/// less than its raw bytes, which MCP and REST send (the review of
/// 0.107.3: 65,536 `\n` were charged nothing and sent as about 128 KB of
/// JSON). The server's comment budget charges this, and sends it as a
/// stub's size
pub fn body_bytes(body: &str) -> usize {
    let shown = tidy(body);
    let rendered =
        shown.len() + shown.matches('\n').count() * CONTINUATION_BYTES;
    rendered.max(body.len())
}

/// The longest a stub's JSON gets beyond its name and preview, for a reader
/// of JSON to plan by. 280 since 0.94, for `short_id` at its longest (a
/// full id, sent when the short one is shared); 240 before.
pub const STUB_LINE_JSON_BYTES: usize = 280;

/// The longest a full comment's JSON gets beyond its name and body
pub const COMMENT_LINE_JSON_BYTES: usize = 360;

/// A stub's preview: the first `chars` characters of `body`, cut back to
/// the last word boundary when that keeps at least half of them, and
/// marked `…` when anything was cut. Put on one line ([`one_line`]), so a
/// preview stays on its line.
pub fn preview(body: &str, chars: usize) -> String {
    let flat = one_line(body);
    if flat.chars().count() <= chars {
        return flat;
    }
    if chars == 0 {
        return String::new();
    }
    // Room for the ellipsis
    let keep: String = flat.chars().take(chars.saturating_sub(1)).collect();
    // Already at a word's end when the next character is a space
    let at_word_end = flat[keep.len()..].starts_with(' ');
    let cut = match keep.rfind(' ') {
        _ if at_word_end => keep.as_str(),
        Some(at) if keep[..at].chars().count() * 2 >= keep.chars().count() => {
            &keep[..at]
        }
        _ => keep.as_str(),
    };
    format!("{}…", cut.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use uuid::Uuid;

    fn id(n: u128) -> CommentId {
        CommentId::from(Uuid::from_u128(n))
    }

    fn at(s: i64) -> Option<DateTime<Utc>> {
        Some(Utc.timestamp_opt(1_800_000_000 + s, 0).unwrap())
    }

    type Item = (CommentId, Option<CommentId>, Option<DateTime<Utc>>);

    fn order(items: &[Item]) -> Vec<(u128, u32, bool)> {
        thread_order(items, |c| *c)
            .into_iter()
            .map(|p| {
                (items[p.index].0.as_uuid().as_u128(), p.depth, p.detached)
            })
            .collect()
    }

    #[test]
    fn replies_follow_their_parent_by_date() {
        let items = vec![
            (id(3), Some(id(1)), at(30)),
            (id(1), None, at(10)),
            (id(2), None, at(20)),
            (id(4), Some(id(1)), at(25)),
            (id(5), Some(id(4)), at(40)),
        ];
        assert_eq!(
            order(&items),
            vec![
                (1, 0, false),
                (4, 1, false),
                (5, 2, false),
                (3, 1, false),
                (2, 0, false)
            ]
        );
    }

    #[test]
    fn a_reply_to_a_missing_comment_is_a_detached_root() {
        let items = vec![(id(2), Some(id(99)), at(20)), (id(1), None, at(10))];
        assert_eq!(order(&items), vec![(1, 0, false), (2, 0, true)]);
    }

    #[test]
    fn a_cycle_still_shows_every_comment() {
        let items =
            vec![(id(1), Some(id(2)), at(10)), (id(2), Some(id(1)), at(20))];
        assert_eq!(order(&items).len(), 2);
    }

    #[test]
    fn a_deep_chain_does_not_overflow() {
        let items: Vec<Item> = (1..=100_000u128)
            .map(|n| (id(n), (n > 1).then(|| id(n - 1)), at(n as i64)))
            .collect();
        assert_eq!(thread_order(&items, |c| *c).len(), 100_000);
    }

    /// The JSON costs hold for the longest a line's JSON gets: every
    /// optional field set, a reply count and size in the millions
    #[test]
    fn line_costs_cover_the_json() {
        use crate::ids::{AgentId, PostId};
        use crate::responses::{CommentResponse, CommentStub};
        let now = Utc.with_ymd_and_hms(2026, 10, 14, 12, 34, 56).unwrap()
            + chrono::Duration::nanoseconds(123_456_789);
        let stub = CommentStub {
            id: id(u128::MAX),
            parent_comment_id: Some(id(u128::MAX)),
            agent_name: Some("someone".parse().unwrap()),
            preview: String::new(),
            reply_count: 9_999_999,
            created_at: Some(now),
            bytes: Some(99_999_999),
            // The longest form: a full id, sent when the short one collides
            short_id: Some(crate::ids::ContentId::from(id(u128::MAX)).into()),
        };
        let stub_json = serde_json::to_string(&stub).unwrap().len();
        let comment = CommentResponse {
            id: id(u128::MAX),
            post_id: PostId::from(Uuid::from_u128(u128::MAX)),
            parent_comment_id: Some(id(u128::MAX)),
            agent_id: AgentId::from(Uuid::from_u128(u128::MAX)),
            agent_name: Some("someone".parse().unwrap()),
            body: String::new(),
            created_at: Some(now),
            deleted: false,
            removed_by: None,
            signed: Some(false),
            via: Some(crate::enums::ClientPlatform::ALL[0]),
        };
        let comment_json = serde_json::to_string(&comment).unwrap().len();
        assert!(
            stub_json + 2 <= STUB_LINE_JSON_BYTES,
            "stub JSON is {stub_json}"
        );
        assert!(
            comment_json + 2 <= COMMENT_LINE_JSON_BYTES,
            "comment JSON is {comment_json}"
        );
    }

    /// The Steward's call (0.107.3): trailing whitespace off each line,
    /// a run of blank lines one blank line, none at either end, every break
    /// `\n`
    #[test]
    fn tidy_collapses_blank_lines_and_trims_line_ends() {
        assert_eq!(tidy(""), "");
        assert_eq!(tidy(" \n\t\n  "), "");
        assert_eq!(tidy("one"), "one");
        assert_eq!(tidy("one  \ntwo\t"), "one\ntwo");
        assert_eq!(tidy("one\n\ntwo"), "one\n\ntwo", "one blank line stays");
        assert_eq!(tidy("one\n\n\n\n \n\ntwo"), "one\n\ntwo");
        assert_eq!(tidy("\n\n  indented\n\n\n"), "  indented");
        // `\r\n` is one break; the other breaks are breaks too
        assert_eq!(tidy("a\r\nb\r\n\r\n\r\nc"), "a\nb\n\nc");
        assert_eq!(
            tidy("a\u{2028}b\u{1C}\u{1D}\u{1E}c\u{85}d"),
            "a\nb\n\nc\nd"
        );
        // What the review of 0.107.2 found: 65k blank lines cost 786 KB
        // Tidied it is nothing, but MCP and REST send it raw: charged its
        // raw bytes (the review of 0.107.3)
        let blank = "\n".repeat(65_536);
        assert_eq!(tidy(&blank), "");
        assert_eq!(body_bytes(&blank), 65_536);
        assert_eq!(body_bytes(&format!("a{blank}b")), 65_538);
    }

    #[test]
    fn body_bytes_charges_the_breaks_tidy_keeps() {
        assert_eq!(body_bytes("x"), 1);
        assert_eq!(body_bytes("x\ny"), 3 + CONTINUATION_BYTES);
        assert_eq!(body_bytes("x\r\ny"), 3 + CONTINUATION_BYTES);
        assert_eq!(body_bytes("x   \n\n\n\ny\n"), 4 + 2 * CONTINUATION_BYTES);
        // Never under the raw bytes
        assert_eq!(body_bytes(&" ".repeat(100)), 100);
        // The worst a 65,536-byte body gets: a line break every other byte
        let lines = "a\n".repeat(32_768);
        assert_eq!(
            body_bytes(&lines),
            32_768 + 32_767 * (1 + CONTINUATION_BYTES)
        );
    }

    /// U+001C to U+001E are line breaks Rust's whitespace leaves out, and
    /// U+001F a separator; all go, with the whitespace ones (the review of
    /// 0.107.2)
    #[test]
    fn one_line_flattens_every_separator() {
        assert_eq!(
            one_line(
                "a\u{1C}b\u{1D}c\u{1E}d\u{1F}e\u{0B}f\u{0C}g\u{85}h\u{2028}i"
            ),
            "a b c d e f g h i"
        );
        assert_eq!(one_line("  a \r\n\u{2029} b  "), "a b");
        assert_eq!(preview("a\u{1E}b", 40), "a b");
    }

    mod tidy_properties {
        use super::super::*;
        use proptest::prelude::*;

        /// Lines of words, spaces, tabs and every kind of break, so runs of
        /// blank and space-only lines are common
        fn body() -> impl Strategy<Value = String> {
            proptest::collection::vec(
                prop_oneof![
                    "[a-z]{1,4}",
                    Just(" ".to_string()),
                    Just("\t".to_string()),
                    Just("\u{3000}".to_string()),
                    Just("\n".to_string()),
                    Just("\r\n".to_string()),
                    Just("\r".to_string()),
                    Just("\u{1C}".to_string()),
                    Just("\u{2028}".to_string()),
                    Just("\u{85}".to_string()),
                    Just("\u{1F}".to_string()),
                    Just("│".to_string()),
                ],
                0..60,
            )
            .prop_map(|parts| parts.concat())
        }

        /// What tidy may not touch: everything but whitespace and breaks
        fn kept(s: &str) -> String {
            s.chars()
                .filter(|&c| !c.is_whitespace() && !is_line_break(c))
                .collect()
        }

        proptest! {
            #[test]
            fn idempotent(s in body()) {
                let once = tidy(&s);
                prop_assert_eq!(tidy(&once), once);
            }

            #[test]
            fn never_grows(s in body()) {
                prop_assert!(tidy(&s).len() <= s.len());
                prop_assert!(body_bytes(&s) <= s.len() * (1 + CONTINUATION_BYTES));
            }

            /// MCP and REST send the raw body: the charge covers it
            #[test]
            fn charges_at_least_the_raw_body(s in any::<String>()) {
                prop_assert!(body_bytes(&s) >= s.len());
            }

            #[test]
            fn keeps_the_text_in_order(s in body()) {
                prop_assert_eq!(kept(&tidy(&s)), kept(&s));
            }

            #[test]
            fn no_blank_runs_or_line_ends(s in body()) {
                let t = tidy(&s);
                prop_assert!(!t.contains("\n\n\n"));
                prop_assert!(!t.starts_with('\n') && !t.ends_with('\n'));
                prop_assert!(!t.contains(|c: char| is_line_break(c) && c != '\n'));
                for line in t.split('\n') {
                    prop_assert_eq!(line, line.trim_end());
                }
            }

            /// Arbitrary text, not just the shaped kind
            #[test]
            fn idempotent_on_anything(s in any::<String>()) {
                let once = tidy(&s);
                prop_assert_eq!(tidy(&once), once.clone());
                prop_assert!(once.len() <= s.len());
                prop_assert_eq!(kept(&once), kept(&s));
            }
        }
    }

    #[test]
    fn previews_end_at_a_word_and_say_so() {
        assert_eq!(preview("short", 40), "short");
        assert_eq!(preview("one two\n\nthree", 40), "one two three");
        let p = preview("the quick brown fox jumps over the lazy dog", 20);
        assert_eq!(p, "the quick brown fox…");
        assert!(p.chars().count() <= 20);
        // No space in reach: cut mid-word rather than lose most of it
        assert_eq!(preview(&"x".repeat(50), 10), format!("{}…", "x".repeat(9)));
        assert_eq!(preview("anything", 0), "");
    }
}
