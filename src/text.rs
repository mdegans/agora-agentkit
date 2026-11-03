//! Text helpers shared by the seed prompt and the runner

use std::time::Duration;

use similar::{ChangeTag, TextDiff};

/// How long [`line_diff`]'s Myers search runs (it is quadratic on texts
/// with little in common). Past it Myers stops searching and returns a
/// correct but coarser diff: more lines shown as removed and re-added than
/// strictly needed (Steward, 2026-11-03: a few seconds is fine)
pub const LINE_DIFF_TIMEOUT: Duration = Duration::from_secs(2);

/// What changed from `before` to `after`, for a model to read: removed
/// units as `  - …`, added ones as `  + …`, and each run of unchanged ones
/// counted on a line of its own instead of shown.
///
/// The units are lines, or sentences when both texts are on at most two
/// lines: one removed sentence is one unit, not the whole paragraph twice
/// (second iteration of 0.113.0). Line ends never count as a change; a
/// sentence's surrounding whitespace doesn't either, but a line's does.
///
/// The diff gives up refining after [`LINE_DIFF_TIMEOUT`] and settles for a
/// coarser one, still correct. The output is not bounded: it can be as
/// long as both texts together, so callers cap what they show.
pub fn line_diff(before: &str, after: &str) -> String {
    let few = before.lines().count() <= 2 && after.lines().count() <= 2;
    fn split(s: &str, few: bool) -> Vec<&str> {
        if few {
            s.split_inclusive(". ")
                .map(str::trim)
                .filter(|u| !u.is_empty())
                .collect()
        } else {
            s.lines().collect()
        }
    }
    let (old, new) = (split(before, few), split(after, few));
    let unit = if few {
        ("sentence", "sentences")
    } else {
        ("line", "lines")
    };
    let units = |n: usize| if n == 1 { unit.0 } else { unit.1 };
    let diff = TextDiff::configure()
        .algorithm(similar::Algorithm::Myers)
        .timeout(LINE_DIFF_TIMEOUT)
        .diff_slices(&old, &new);
    let changes: Vec<_> = diff.iter_all_changes().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < changes.len() {
        let change = &changes[i];
        match change.tag() {
            ChangeTag::Equal => {
                let run = changes[i..]
                    .iter()
                    .take_while(|c| c.tag() == ChangeTag::Equal)
                    .count();
                let where_ = if i == 0 {
                    " before"
                } else if i + run == changes.len() {
                    " after"
                } else {
                    ""
                };
                out.push_str(&format!(
                    "  ({run} {} unchanged{where_})\n",
                    units(run)
                ));
                i += run;
                continue;
            }
            ChangeTag::Delete => {
                out.push_str(&format!("  - {}\n", change.value()))
            }
            ChangeTag::Insert => {
                out.push_str(&format!("  + {}\n", change.value()))
            }
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unchanged_ends_are_counted_and_changes_shown() {
        let before = "a\nb\nc\nd\ne\n";
        let after = "a\nb\nC\nd\ne";
        assert_eq!(
            line_diff(before, after),
            "  (2 lines unchanged before)\n  - c\n  + C\n  (2 lines unchanged after)\n"
        );
    }

    #[test]
    fn two_changes_count_the_lines_between() {
        let before = "a\nb\nc\nd\ne\nf";
        let after = "A\nb\nc\nd\ne\nF";
        assert_eq!(
            line_diff(before, after),
            "  - a\n  + A\n  (4 lines unchanged)\n  - f\n  + F\n"
        );
    }

    #[test]
    fn added_removed_and_unchanged_texts() {
        assert_eq!(line_diff("", "x\ny\nz"), "  + x\n  + y\n  + z\n");
        assert_eq!(line_diff("x\ny\nz", ""), "  - x\n  - y\n  - z\n");
        assert_eq!(
            line_diff("x\ny\nz", "x\ny\nz"),
            "  (3 lines unchanged before)\n"
        );
        assert_eq!(line_diff("", ""), "");
    }

    /// Review of 0.148: 40,000 lines with nothing in common took 6.9 s
    /// (release) without a deadline
    #[test]
    fn a_large_disjoint_diff_returns_quickly() {
        let before: String =
            (0..40_000).map(|i| format!("before {i}\n")).collect();
        let after: String =
            (0..40_000).map(|i| format!("after {i}\n")).collect();
        let start = std::time::Instant::now();
        let diff = line_diff(&before, &after);
        // The deadline, and generous for a debug build on a loaded machine
        assert!(
            start.elapsed() < LINE_DIFF_TIMEOUT + Duration::from_secs(8),
            "{:?}",
            start.elapsed()
        );
        assert_eq!(diff.lines().count(), 80_000);
        assert!(diff.starts_with("  - before 0\n"));
    }

    /// A line's surrounding whitespace is part of it
    #[test]
    fn a_lines_whitespace_counts() {
        assert_eq!(
            line_diff("a\nb \nc\nd", "a\nb\nc\nd"),
            "  (1 line unchanged before)\n  - b \n  + b\n  (2 lines unchanged after)\n"
        );
    }

    #[test]
    fn prose_is_compared_by_sentence() {
        let diff = line_diff(
            "One. Two is removed. Three. Four. Five. Six.",
            "One. Three. Four. Five. Six.",
        );
        assert_eq!(
            diff,
            "  (1 sentence unchanged before)\n  - Two is removed.\n  \
             (4 sentences unchanged after)\n"
        );
    }
}
