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
/// (second iteration of 0.113.0). A sentence never spans a line end, so
/// every line of the output carries its prefix. Line ends never count as a change; a
/// sentence's surrounding whitespace doesn't either, but a line's does.
///
/// The diff gives up refining after [`LINE_DIFF_TIMEOUT`] and settles for a
/// coarser one, still correct. The output is not bounded: it can be as
/// long as both texts together, so callers cap what they show.
pub fn line_diff(before: &str, after: &str) -> String {
    let few = by_sentence(before, after);
    fn split(s: &str, few: bool) -> Vec<&str> {
        if few {
            // Lines first, so a sentence ending one never runs into the
            // next, and no unit holds a line end
            s.lines()
                .flat_map(|line| line.split_inclusive(". "))
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

/// Whether [`line_diff`] compares `before` and `after` by sentence: when
/// both are on at most two lines
pub fn by_sentence(before: &str, after: &str) -> bool {
    before.lines().count() <= 2 && after.lines().count() <= 2
}

/// What [`line_diff`] compares `before` and `after` by, plural: `lines` or
/// `sentences`
pub fn diff_units(before: &str, after: &str) -> &'static str {
    if by_sentence(before, after) {
        "sentences"
    } else {
        "lines"
    }
}

/// The units a [`line_diff`] output adds and removes: `(added, removed)`
pub fn change_counts(diff: &str) -> (usize, usize) {
    let count = |prefix| diff.lines().filter(|l| l.starts_with(prefix)).count();
    (count("  + "), count("  - "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_and_units() {
        let diff = line_diff("a\nb\nc\n", "a\nB\nc\nd\n");
        assert_eq!(change_counts(&diff), (2, 1));
        assert_eq!(diff_units("a\nb\nc", "a"), "lines");
        assert_eq!(diff_units("One. Two.", "One."), "sentences");
    }

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

    /// 0.148.1: a sentence ending a line was one unit with the next line's
    #[test]
    fn a_line_end_ends_a_sentence() {
        assert_eq!(
            line_diff("One. Two.\nThree.", "One. Two.\nFour."),
            "  (2 sentences unchanged before)\n  - Three.\n  + Four.\n"
        );
    }

    /// 0.148.1: an added line of several sentences printed its second
    /// line without a prefix
    #[test]
    fn every_output_line_has_its_prefix() {
        let diff = line_diff("One.", "One.\nTwo. Three.");
        assert_eq!(
            diff,
            "  (1 sentence unchanged before)\n  + Two.\n  + Three.\n"
        );
        let diff = line_diff("A. B.\nC. D.", "");
        assert_eq!(diff, "  - A.\n  - B.\n  - C.\n  - D.\n");
        for line in diff.lines() {
            assert!(line.starts_with("  - "), "{diff}");
        }
    }

    #[test]
    fn crlf_is_a_line_end() {
        assert_eq!(
            line_diff("One. Two.\r\nThree.", "One. Two.\nThree."),
            "  (3 sentences unchanged before)\n"
        );
        let diff = line_diff("One.\r\nTwo.", "One.\r\nTwo. Three.");
        assert_eq!(diff, "  (2 sentences unchanged before)\n  + Three.\n");
        assert!(!diff.contains('\r'), "{diff:?}");
        // Line mode too
        assert_eq!(
            line_diff("a\r\nb\r\nc\r\nx", "a\nb\nc\ny"),
            "  (3 lines unchanged before)\n  - x\n  + y\n"
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
