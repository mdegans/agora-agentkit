//! Checking a quote against its source before it is signed.
//!
//! An obligation's `quote` is "the words in the source that create it,
//! verbatim" (`docs/design-obligations.md`), and once signed it is
//! permanent. So before signing, the writer looks for it in the source.
//!
//! **Normalisation.** Sources are hard-wrapped markdown files, Council
//! records with code blocks, and posts with emphasis. A quote that differs
//! from its source only in layout is the same words, so both sides are
//! compared after [`normalize`]:
//!
//! - every `*` and every backtick is removed (markdown bold, italics and
//!   code spans);
//! - every run of whitespace, line breaks and indentation included,
//!   becomes one space;
//! - leading and trailing whitespace is dropped.
//!
//! Nothing else is changed: case, punctuation, typographic quotes and
//! dashes all have to match. A quote found verbatim is reported
//! [`Exact`](QuoteCheck::Exact); one found only after normalising,
//! [`Normalized`](QuoteCheck::Normalized), so the reviewer sees which.
//!
//! Pure and dependency-light (serde_json only). Moved from the agora
//! server's staging module in 0.75.

use std::fmt;

/// A quote's text with markdown emphasis and code marks removed and all
/// whitespace collapsed to single spaces. See the [module docs](self).
pub fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = false;
    for c in s.chars() {
        if c == '*' || c == '`' {
            continue;
        }
        if c.is_whitespace() {
            space = true;
            continue;
        }
        if space && !out.is_empty() {
            out.push(' ');
        }
        space = false;
        out.push(c);
    }
    out
}

/// Every string in `value` with its RFC 6901 pointer, prefixed by
/// `prefix`: arrays in order, object keys sorted bytewise as
/// [`canonical_json`](crate::govlog::canonical_json) sorts them, so the
/// order is the same whether or not serde_json preserves key order. The
/// top-level blind (`_blind`) is left out: it is random hex, never
/// anyone's words.
pub fn string_leaves<'a>(
    prefix: &str,
    value: &'a serde_json::Value,
) -> Vec<(String, &'a str)> {
    fn walk<'a>(
        at: String,
        v: &'a serde_json::Value,
        top: bool,
        out: &mut Vec<(String, &'a str)>,
    ) {
        match v {
            serde_json::Value::String(s) => out.push((at, s)),
            serde_json::Value::Array(items) => {
                for (i, item) in items.iter().enumerate() {
                    walk(format!("{at}/{i}"), item, false, out);
                }
            }
            serde_json::Value::Object(map) => {
                let mut entries: Vec<_> = map.iter().collect();
                entries
                    .sort_by(|(a, _), (b, _)| a.as_bytes().cmp(b.as_bytes()));
                for (k, item) in entries {
                    if top && k == crate::govlog::BLIND_KEY {
                        continue;
                    }
                    let key = k.replace('~', "~0").replace('/', "~1");
                    walk(format!("{at}/{key}"), item, false, out);
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(prefix.to_string(), value, true, &mut out);
    out
}

/// What a search for a quote found
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuoteCheck {
    /// Verbatim, at these locations
    Exact { at: Vec<String> },
    /// Only after [`normalize`], at these locations
    Normalized { at: Vec<String> },
    /// Nowhere. The closest location, if any text of the quote was found
    Missing { closest: Option<Closest> },
}

impl QuoteCheck {
    /// Whether the quote was found, verbatim or normalised
    pub fn found(&self) -> bool {
        !matches!(self, Self::Missing { .. })
    }
}

/// Which end of the quote a [`Closest`] match runs from
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuoteEnd {
    Start,
    End,
}

/// Where the longest piece of a missing quote was found: its first (or
/// last) `matched` characters of `of`, normalised, occur at `at`, and the
/// quote departs from the source at `departs`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Closest {
    pub at: String,
    pub matched: usize,
    pub of: usize,
    pub from: QuoteEnd,
    /// Up to 40 characters of the normalised quote where it stops
    /// matching
    pub departs: String,
}

impl fmt::Display for Closest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let end = match self.from {
            QuoteEnd::Start => "first",
            QuoteEnd::End => "last",
        };
        write!(
            f,
            "closest is {}: its {end} {} of {} characters (normalised) match, then it departs at {:?}",
            self.at, self.matched, self.of, self.departs
        )
    }
}

/// A quote that cannot be checked
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum QuoteError {
    #[error("the quote is empty once markdown and whitespace are removed")]
    Empty,
}

/// Look for `quote` in `sources`, each a location and its text. Every
/// location where it is found is listed, so the reviewer can compare them
/// with the stated `quote_location`.
pub fn check<'a>(
    quote: &str,
    sources: impl IntoIterator<Item = (String, &'a str)>,
) -> Result<QuoteCheck, QuoteError> {
    let wanted = normalize(quote);
    if wanted.is_empty() {
        return Err(QuoteError::Empty);
    }
    let sources: Vec<(String, &str)> = sources.into_iter().collect();
    let exact: Vec<String> = sources
        .iter()
        .filter(|(_, text)| text.contains(quote))
        .map(|(at, _)| at.clone())
        .collect();
    if !exact.is_empty() {
        return Ok(QuoteCheck::Exact { at: exact });
    }
    let normalized: Vec<(String, String)> = sources
        .into_iter()
        .map(|(at, text)| (at, normalize(text)))
        .collect();
    let found: Vec<String> = normalized
        .iter()
        .filter(|(_, text)| text.contains(&wanted))
        .map(|(at, _)| at.clone())
        .collect();
    if !found.is_empty() {
        return Ok(QuoteCheck::Normalized { at: found });
    }
    Ok(QuoteCheck::Missing {
        closest: closest(&wanted, &normalized),
    })
}

/// The location holding the longest prefix or suffix of `wanted`. Both
/// are monotone (a text that holds a prefix holds every shorter one), so
/// each is a binary search over `wanted`'s character boundaries.
fn closest(wanted: &str, texts: &[(String, String)]) -> Option<Closest> {
    let bounds: Vec<usize> = wanted
        .char_indices()
        .map(|(i, _)| i)
        .chain([wanted.len()])
        .collect();
    let chars = bounds.len() - 1;
    let longest = |holds: &dyn Fn(usize) -> bool| {
        let (mut lo, mut hi) = (0, chars);
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            if holds(mid) { lo = mid } else { hi = mid - 1 }
        }
        lo
    };
    let mut best: Option<Closest> = None;
    for (at, text) in texts {
        let prefix = longest(&|n| text.contains(&wanted[..bounds[n]]));
        let suffix = longest(&|n| text.contains(&wanted[bounds[chars - n]..]));
        let (matched, from) = if suffix > prefix {
            (suffix, QuoteEnd::End)
        } else {
            (prefix, QuoteEnd::Start)
        };
        if matched == 0 || best.as_ref().is_some_and(|b| b.matched >= matched) {
            continue;
        }
        let departs = match from {
            QuoteEnd::Start => {
                wanted[bounds[matched]..].chars().take(40).collect()
            }
            QuoteEnd::End => {
                let before: Vec<char> =
                    wanted[..bounds[chars - matched]].chars().collect();
                before[before.len().saturating_sub(40)..].iter().collect()
            }
        };
        best = Some(Closest {
            at: at.clone(),
            matched,
            of: chars,
            from,
            departs,
        });
    }
    best
}

/// What the quote check found, for the dry run
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuoteReport {
    /// Checked by the writer: found verbatim or normalised
    Checked { source: String, found: QuoteCheck },
    /// Checked by the operator, who is named here, never in the entry
    Operator { source: String, by: Option<String> },
}

impl std::fmt::Display for QuoteReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Checked {
                source,
                found: QuoteCheck::Exact { at },
            } => write!(f, "verbatim in {source} at {}", at.join(", ")),
            Self::Checked {
                source,
                found: QuoteCheck::Normalized { at },
            } => write!(
                f,
                "in {source} once markdown and line wraps are normalised, at {}",
                at.join(", ")
            ),
            Self::Checked { source, .. } => write!(f, "NOT in {source}"),
            Self::Operator {
                source,
                by: Some(by),
            } => write!(f, "{source}: checked by the operator, {by}"),
            Self::Operator { source, by: None } => write!(
                f,
                "{source}: NOT CHECKED. The operator checks this source; --yes refuses \
                 without --quote-checked-by <who>"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(at: &str, text: &str) -> Vec<(String, &'static str)> {
        vec![(at.to_string(), Box::leak(text.to_string().into_boxed_str()))]
    }

    #[test]
    fn normalising_removes_layout_and_emphasis_only() {
        assert_eq!(
            normalize("  a⏎\n   **pending your\tapproval**  of `x`  "),
            "a⏎ pending your approval of x"
        );
        // Case, punctuation and typographic characters are words
        assert_eq!(normalize("It’s — “so”"), "It’s — “so”");
        assert_eq!(normalize(" \n* `` **"), "");
    }

    #[test]
    fn an_exact_quote_is_found_everywhere_it_is() {
        let doc = serde_json::json!({
            "_blind": "abc",
            "rounds": [{"position": "P100 first. Then P47."}, {"rationale": "**P100 first.**"}],
            "a/b": "P100 first."
        });
        let leaves = string_leaves("", &doc);
        assert!(leaves.iter().all(|(at, _)| at != "/_blind"));
        assert_eq!(
            check("P100 first.", leaves).unwrap(),
            QuoteCheck::Exact {
                at: vec![
                    "/a~1b".into(),
                    "/rounds/0/position".into(),
                    "/rounds/1/rationale".into(),
                ]
            }
        );
    }

    #[test]
    fn a_wrapped_or_bold_source_matches_only_normalised() {
        let source = "a\nsentence for the Protocol will be put to you at the next review of\n\
                      accumulated Protocol revisions.";
        let quote = "a sentence for the Protocol will be put to you at the next review of \
                     accumulated Protocol revisions.";
        assert_eq!(
            check(quote, one("ops", source)).unwrap(),
            QuoteCheck::Normalized {
                at: vec!["ops".into()]
            }
        );
        assert!(
            check(
                "pending your approval",
                one("ops", "**pending your approval** of")
            )
            .unwrap()
            .found()
        );
    }

    #[test]
    fn a_missing_quote_names_the_closest_location() {
        let sources = vec![
            ("/a".to_string(), "nothing like it"),
            (
                "/b".to_string(),
                "The slot will be built after the sitting.",
            ),
        ];
        let QuoteCheck::Missing { closest: Some(c) } =
            check("The slot will be built before the sitting.", sources)
                .unwrap()
        else {
            panic!("not missing")
        };
        assert_eq!(c.at, "/b");
        assert_eq!(c.from, QuoteEnd::Start);
        assert_eq!(c.matched, "The slot will be built ".chars().count());
        assert!(c.departs.starts_with("before"));

        let QuoteCheck::Missing { closest: Some(c) } = check(
            "Totally different start, the sitting.",
            one("/b", "after the sitting."),
        )
        .unwrap() else {
            panic!("not missing")
        };
        assert_eq!(c.from, QuoteEnd::End);
        assert!(c.departs.ends_with("start,"));

        assert_eq!(
            check("zzz", one("/a", "abc")).unwrap(),
            QuoteCheck::Missing { closest: None }
        );
    }

    #[test]
    fn an_empty_quote_is_refused() {
        assert_eq!(check(" ** ` ", one("/a", "x")), Err(QuoteError::Empty));
    }
}
