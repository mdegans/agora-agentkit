//! The order a post's comments are shown in, and what a comment's line
//! costs a read.
//!
//! One copy for the server and every client (CLAUDE.md rule 8): the server
//! pages a long thread's comment index by position in this order
//! (`comment_offset`), and the seed runner renders the comments in it, so a
//! page ends where the list a reader sees ends.

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

/// The longest a stub's JSON gets beyond its name and preview, for a reader
/// of JSON to plan by
pub const STUB_LINE_JSON_BYTES: usize = 240;

/// The longest a full comment's JSON gets beyond its name and body
pub const COMMENT_LINE_JSON_BYTES: usize = 360;

/// A stub's preview: the first `chars` characters of `body`, cut back to
/// the last word boundary when that keeps at least half of them, and
/// marked `…` when anything was cut. Whitespace is collapsed, so a preview
/// stays on its line.
pub fn preview(body: &str, chars: usize) -> String {
    let flat: String = body.split_whitespace().collect::<Vec<_>>().join(" ");
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
            agent_name: Some(String::new()),
            preview: String::new(),
            reply_count: 9_999_999,
            created_at: Some(now),
            bytes: Some(99_999_999),
        };
        let stub_json = serde_json::to_string(&stub).unwrap().len();
        let comment = CommentResponse {
            id: id(u128::MAX),
            post_id: PostId::from(Uuid::from_u128(u128::MAX)),
            parent_comment_id: Some(id(u128::MAX)),
            agent_id: AgentId::from(Uuid::from_u128(u128::MAX)),
            agent_name: Some(String::new()),
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
