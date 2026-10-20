//! Reading part of a data set's file (0.103): a [`LineRange`], and the
//! [`DataCitation`] that names a file at a commit, with or without one.
//!
//! A range is a selective read, not a page: there is no cursor and no
//! "next" (agora `memory/feedback_no_paging.md`). A reader who means to
//! read a whole file reads it whole; a range is for a part, or for a file
//! too large for one read ([`MAX_WHOLE_READ_BYTES`](super::MAX_WHOLE_READ_BYTES)).

use serde::{Deserialize, Serialize};

use super::{DataFilePath, GitCommitId, GovernanceLogId};
use crate::ids::GovernanceLogPrefix;

/// Lines `start` to `end` of a file, 1-based and inclusive; `end` absent
/// means to the end of the file.
///
/// Written `"110-300"`, or `"110-"` to the end. Read leniently, as models
/// and people write ranges: `"110..300"`, `"110–300"`, `"L110-L300"`,
/// `"lines 110-300"`, `"-300"` (from the first line), `"110"` (that line
/// alone), or a bare number. Validated: lines are numbered from 1, and a
/// range cannot end before it starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LineRange {
    start: u64,
    end: Option<u64>,
}

/// A string that is not a [`LineRange`]: the message says why and what the
/// form is
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("lines={asked:?}: {why} {}", LineRange::FORM)]
pub struct LineRangeError {
    pub asked: String,
    pub why: &'static str,
}

/// Where a [`LineRange`] falls in a text: the byte span to serve, and the
/// range as served (its end clamped to the file's last line)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineSpan {
    pub served: LineRange,
    pub start_byte: usize,
    pub end_byte: usize,
}

/// A [`LineRange`] that starts past a file's last line
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "lines={asked} starts past the end of the file, which has {total_lines} \
     line(s)"
)]
pub struct PastEnd {
    pub asked: LineRange,
    pub total_lines: u64,
}

impl LineRange {
    /// How a range is written, for error messages
    pub const FORM: &'static str = "Write a range of lines as \"<from>-<to>\" \
         (1-based, both included, e.g. \"110-300\"), or \"<from>-\" to read to \
         the end of the file.";

    /// How a refused range opens, as the seed runner shows a malformed
    /// `lines` argument, so that every range error reads alike (0.105.0)
    pub const INVALID: &'static str = "Invalid arguments at `lines`: ";

    /// Lines `start..=end` (`end` absent: to the end), or why not
    pub fn new(start: u64, end: Option<u64>) -> Result<Self, &'static str> {
        if start == 0 {
            return Err("lines are numbered from 1.");
        }
        if end.is_some_and(|end| end < start) {
            return Err("the range ends before it starts.");
        }
        Ok(Self { start, end })
    }

    /// The first line, from 1
    pub fn start(&self) -> u64 {
        self.start
    }

    /// The last line, or `None` for the end of the file
    pub fn end(&self) -> Option<u64> {
        self.end
    }

    /// The span of `text` this range covers. A line is what ends at a
    /// `\n`, or the text's end; an end past the last line is the last line
    pub fn span(&self, text: &str) -> Result<LineSpan, PastEnd> {
        let total_lines = count_lines(text);
        if self.start > total_lines {
            return Err(PastEnd {
                asked: *self,
                total_lines,
            });
        }
        let end = self.end.map_or(total_lines, |e| e.min(total_lines));
        // Byte offset of each line's start, as needed
        let mut start_byte = 0;
        let mut end_byte = text.len();
        for (line, (at, _)) in (1..).zip(text.match_indices('\n')) {
            if line + 1 == self.start {
                start_byte = at + 1;
            }
            if line == end {
                end_byte = at + 1;
                break;
            }
        }
        Ok(LineSpan {
            served: Self {
                start: self.start,
                end: Some(end),
            },
            start_byte,
            end_byte,
        })
    }
}

/// How many lines `text` has: one per `\n`, and one more for text after
/// the last `\n`. An empty text has none
pub fn count_lines(text: &str) -> u64 {
    text.split_inclusive('\n').count() as u64
}

impl std::fmt::Display for LineRange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.end {
            Some(end) => write!(f, "{}-{end}", self.start),
            None => write!(f, "{}-", self.start),
        }
    }
}

impl std::str::FromStr for LineRange {
    type Err = LineRangeError;

    fn from_str(asked: &str) -> Result<Self, Self::Err> {
        let error = |why| LineRangeError {
            asked: asked.to_string(),
            why,
        };
        let mut s = asked.trim().trim_start_matches('#');
        for word in ["lines", "line"] {
            if let Some(head) = s.get(..word.len())
                && head.eq_ignore_ascii_case(word)
            {
                s = s[word.len()..].trim_start();
                s = s.strip_prefix('=').unwrap_or(s).trim_start();
                break;
            }
        }
        // `L110` as a code host writes it
        let number = |p: &str| -> Result<Option<u64>, LineRangeError> {
            let p = p.trim();
            let p = p.strip_prefix(['L', 'l']).unwrap_or(p);
            if p.is_empty() {
                return Ok(None);
            }
            if !p.chars().all(|c| c.is_ascii_digit()) {
                return Err(error("that is not a range of line numbers."));
            }
            p.parse()
                .map(Some)
                .map_err(|_| error("that line number is too large."))
        };
        let (from, to, ranged) =
            ["...", "..", "-", "\u{2013}", "\u{2014}", " to "]
                .iter()
                .find_map(|sep| s.split_once(sep).map(|(a, b)| (a, b, true)))
                .unwrap_or((s, s, false));
        let start = number(from)?;
        let end = number(to)?;
        let (start, end) = match (start, end, ranged) {
            (None, _, false) => return Err(error("no line numbers given.")),
            (Some(n), _, false) => (n, Some(n)),
            (None, None, true) => return Err(error("no line numbers given.")),
            (None, end, true) => (1, end),
            (Some(start), end, true) => (start, end),
        };
        Self::new(start, end).map_err(error)
    }
}

impl Serialize for LineRange {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

/// A string, or a bare number for one line
impl<'de> Deserialize<'de> for LineRange {
    fn deserialize<D: serde::Deserializer<'de>>(
        d: D,
    ) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Text(String),
            Line(u64),
        }
        match Raw::deserialize(d)? {
            Raw::Text(s) => s.parse().map_err(serde::de::Error::custom),
            Raw::Line(n) => Self::new(n, Some(n)).map_err(|why| {
                serde::de::Error::custom(LineRangeError {
                    asked: n.to_string(),
                    why,
                })
            }),
        }
    }
}

// A plain string with no `pattern`, inline: the agora CLAUDE.md rules for
// tool schemas (no `$ref`; no `pattern`, which a strict decoder would
// enforce by inventing values). Parsing checks it.
#[cfg(feature = "schemars")]
impl schemars::JsonSchema for LineRange {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("LineRange")
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed(concat!(module_path!(), "::LineRange"))
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "description": "Lines of a file, 1-based and inclusive: \
                            \"110-300\", \"110-\" to the end of the file, \
                            \"-300\" from its start, or \"110\" for one line",
        })
    }
}

/// The first 7 to 40 hex digits of a git commit id, lowercase: how a
/// [`DataCitation`] names the commit it was read at
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GitCommitPrefix(String);

impl GitCommitPrefix {
    /// The fewest hex digits accepted, as `git` abbreviates by default
    pub const MIN_LEN: usize = 7;
    /// A whole SHA-1 commit id
    pub const MAX_LEN: usize = 40;

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// `true` when `commit` starts with this prefix
    pub fn matches(&self, commit: &GitCommitId) -> bool {
        commit.to_hex().starts_with(&self.0)
    }
}

impl std::str::FromStr for GitCommitPrefix {
    type Err = DataCitationError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        let ok = (Self::MIN_LEN..=Self::MAX_LEN).contains(&s.len())
            && s.chars().all(|c| c.is_ascii_hexdigit());
        ok.then(|| Self(s.to_ascii_lowercase()))
            .ok_or_else(|| DataCitationError::Commit(s.to_string()))
    }
}

impl std::fmt::Display for GitCommitPrefix {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<GitCommitId> for GitCommitPrefix {
    fn from(commit: GitCommitId) -> Self {
        Self(commit.to_hex())
    }
}

/// A data set's file, cited at the commit it was read at, and optionally
/// lines of it: `DATA-2026-0001:<commit>:<path>:110-300`, or without the
/// range for the whole file (0.103). The commit may be left out,
/// `DATA-2026-0001:<path>` or `DATA-2026-0001:<path>:110-300`, for the
/// commit the set pins as it stands (0.109.1).
///
/// `get_content` takes one as its `id` and reads that file (and those
/// lines). The commit is a prefix of at least seven hex digits of the
/// commit the data set pins. A server reads the set as revised (its latest
/// version) when the commit is the latest version's, and as originally
/// signed when it is the original's, so a citation made before any revision
/// keeps reading what it cited. One made at an intermediate revision's
/// commit stops resolving once a later revision pins another commit. One
/// without a commit reads the latest version, as the set's own id does: it
/// is unambiguous today (a set pins exactly one commit) but follows any
/// revision, so the canonical citation, which a record should keep, names
/// the commit; the file read says which it was.
///
/// After the set's id and its `:` (with no space: an id, `: ` and prose is
/// not a citation), the text up to the next `:` is the commit when it is
/// 7 to 40 hex digits; anything else (a `/`, a `.`, a shorter
/// or longer run of hex digits) starts the path. So
/// `DATA-2026-0001:abcdef1/file.md` is the file `abcdef1/file.md` at the
/// pinned commit, and `DATA-2026-0001:abcdef1:file.md` is `file.md` at
/// commit `abcdef1`. A path whose text before its first `:` is 7 to 40
/// hex digits (or a file at the root named only with them) is cited with
/// its commit, since without one that text reads as a commit.
///
/// The path runs to the end, or to the last `:` when what follows that is
/// a range written `<from>-<to>` or `<from>-`, so a path may hold a `:`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DataCitation {
    /// A `DATA-…` id
    pub set: GovernanceLogId,
    /// The commit cited, or `None` for the one the set pins as it stands
    /// (0.109.1)
    pub commit: Option<GitCommitPrefix>,
    pub path: DataFilePath,
    pub lines: Option<LineRange>,
}

/// A string shaped like a [`DataCitation`] (a `DATA-…` id and a `:`) that
/// is not one
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DataCitationError {
    #[error("{0:?} is not a data set's id (DATA-YYYY-NNNN). {form}", form = DataCitation::FORM)]
    NotDataSet(String),
    #[error(
        "{0:?} is not a commit: give at least {min} of the commit's hex digits. {form}",
        min = GitCommitPrefix::MIN_LEN,
        form = DataCitation::FORM
    )]
    Commit(String),
    #[error("no file path after the data set's id or commit. {form}", form = DataCitation::FORM)]
    NoPath,
    #[error("{0}")]
    Path(#[from] super::DataFilePathError),
}

impl DataCitation {
    /// How a citation is written, for error messages
    pub const FORM: &'static str = "A data set's file is cited as \
         DATA-YYYY-NNNN:<commit>:<path>, or \
         DATA-YYYY-NNNN:<commit>:<path>:<from>-<to> for some of its lines \
         (the commit may be left out for the one the set pins); to read the \
         data set itself, give its id alone.";

    /// `true` when `s` starts with a `DATA-…` id and a `:`, so it is meant
    /// as a citation and its error is the one to show
    pub fn is_citation_shaped(s: &str) -> bool {
        s.trim_start().split_once(':').is_some_and(|(head, _)| {
            head.parse::<GovernanceLogId>()
                .is_ok_and(|id| id.prefix() == GovernanceLogPrefix::Data)
        })
    }

    /// `true` when this citation reads at `commit`: it names a prefix of
    /// it, or no commit at all (0.109.1)
    pub fn reads_at(&self, commit: &GitCommitId) -> bool {
        self.commit.as_ref().is_none_or(|c| c.matches(commit))
    }

    /// This citation naming `commit`, its canonical form once the commit a
    /// commit-less one read at is known (0.109.1)
    pub fn at(self, commit: impl Into<GitCommitPrefix>) -> Self {
        Self {
            commit: Some(commit.into()),
            ..self
        }
    }
}

impl std::str::FromStr for DataCitation {
    type Err = DataCitationError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        let (head, rest) = s
            .split_once(':')
            .ok_or_else(|| DataCitationError::NotDataSet(s.to_string()))?;
        let set: GovernanceLogId = head
            .parse()
            .ok()
            .filter(|id: &GovernanceLogId| {
                id.prefix() == GovernanceLogPrefix::Data
            })
            .ok_or_else(|| DataCitationError::NotDataSet(head.to_string()))?;
        // A commit is 7 to 40 hex digits, so any other text before the next
        // `:` (a `/`, a `.`, a short hex word like `a` or `cafe`) is the
        // path, read at the pinned commit (0.109.1)
        let is_hex = |t: &str| t.trim().parse::<GitCommitPrefix>().is_ok();
        let (commit, rest) = match rest.split_once(':') {
            // `DATA-2026-0001::<path>`: an empty commit is one left out
            Some((commit, rest)) if commit.trim().is_empty() => (None, rest),
            Some((commit, rest)) if is_hex(commit) => {
                (Some(commit.parse::<GitCommitPrefix>()?), rest)
            }
            // A citation is written without a space after the id; an id,
            // `: ` and prose is not one (`DATA-2026-0001: the votes`), and
            // is refused as before 0.109.1
            _ if rest.starts_with(char::is_whitespace) => {
                return Err(DataCitationError::Commit(rest.trim().to_string()));
            }
            Some(_) => (None, rest),
            // A commit with nothing after it: the path is missing (a root
            // file named only with hex digits is cited with its commit)
            None if is_hex(rest) => {
                rest.parse::<GitCommitPrefix>()?;
                return Err(DataCitationError::NoPath);
            }
            None => (None, rest),
        };
        // The range in a citation is strictly `<from>-<to>` or `<from>-`,
        // as `Display` writes it, so a path ending in `:12` is never read
        // as a range of another file (the review of 0.103)
        let canonical = |r: &str| {
            r.split_once('-').is_some_and(|(a, b)| {
                !a.is_empty()
                    && a.bytes().all(|c| c.is_ascii_digit())
                    && b.bytes().all(|c| c.is_ascii_digit())
            })
        };
        let (path, lines) = match rest.rsplit_once(':') {
            Some((path, range)) if !path.is_empty() && canonical(range) => {
                match range.parse::<LineRange>() {
                    Ok(lines) => (path, Some(lines)),
                    Err(_) => (rest, None),
                }
            }
            _ => (rest, None),
        };
        let path = path.trim();
        let path = path.strip_prefix("./").unwrap_or(path);
        let path = path.trim_start_matches('/');
        if path.is_empty() {
            return Err(DataCitationError::NoPath);
        }
        Ok(Self {
            set,
            commit,
            path: path.parse()?,
            lines,
        })
    }
}

impl std::fmt::Display for DataCitation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:", self.set)?;
        if let Some(commit) = &self.commit {
            write!(f, "{commit}:")?;
        }
        write!(f, "{}", self.path)?;
        if let Some(lines) = self.lines {
            write!(f, ":{lines}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(s: &str) -> LineRange {
        s.parse().unwrap_or_else(|e| panic!("{s:?}: {e}"))
    }

    #[test]
    fn ranges_parse_leniently_and_validate() {
        for (written, start, end) in [
            ("110-300", 110, Some(300)),
            (" 110 - 300 ", 110, Some(300)),
            ("110..300", 110, Some(300)),
            ("110...300", 110, Some(300)),
            ("110\u{2013}300", 110, Some(300)),
            ("L110-L300", 110, Some(300)),
            ("#L110-L300", 110, Some(300)),
            ("lines 110-300", 110, Some(300)),
            ("Lines=110-300", 110, Some(300)),
            ("110 to 300", 110, Some(300)),
            ("110-", 110, None),
            ("110..", 110, None),
            ("-300", 1, Some(300)),
            ("110", 110, Some(110)),
            ("7-7", 7, Some(7)),
        ] {
            let r = range(written);
            assert_eq!((r.start(), r.end()), (start, end), "{written:?}");
        }
        assert_eq!(range("110-300").to_string(), "110-300");
        assert_eq!(range("110-").to_string(), "110-");
        for (bad, why) in [
            ("0-10", "numbered from 1"),
            ("300-110", "ends before it starts"),
            ("", "no line numbers"),
            ("-", "no line numbers"),
            ("abc", "not a range"),
            ("1-2-3", "not a range"),
            ("1.5-3", "not a range"),
            ("99999999999999999999999", "too large"),
        ] {
            let err = bad.parse::<LineRange>().unwrap_err();
            let text = err.to_string();
            assert!(text.contains(why), "{bad:?}: {text}");
            assert!(text.contains("\"110-300\""), "says the form: {text}");
        }
    }

    #[test]
    fn ranges_round_trip_as_strings_and_take_a_bare_number() {
        let r: LineRange = serde_json::from_str("\"110-300\"").unwrap();
        assert_eq!(serde_json::to_value(r).unwrap(), "110-300");
        let one: LineRange = serde_json::from_str("42").unwrap();
        assert_eq!(one.to_string(), "42-42");
        assert!(serde_json::from_str::<LineRange>("0").is_err());
        assert!(serde_json::from_str::<LineRange>("\"9-1\"").is_err());
    }

    #[cfg(feature = "schemars")]
    #[test]
    fn a_range_is_a_plain_string_in_a_schema() {
        let schema = schemars::schema_for!(LineRange);
        let text = serde_json::to_string(&schema).unwrap();
        assert!(!text.contains("$ref") && !text.contains("$defs"), "{text}");
        assert!(!text.contains("pattern"), "{text}");
        assert_eq!(schema.get("type").unwrap(), "string");
    }

    #[test]
    fn spans_cover_whole_lines_and_clamp_the_end() {
        let text = "one\ntwo\nthree\nfour";
        assert_eq!(count_lines(text), 4);
        assert_eq!(count_lines("a\n"), 1);
        assert_eq!(count_lines(""), 0);
        let span = |r: &str| {
            let s = range(r).span(text).unwrap();
            (&text[s.start_byte..s.end_byte], s.served.to_string())
        };
        assert_eq!(span("1-1"), ("one\n", "1-1".into()));
        assert_eq!(span("2-3"), ("two\nthree\n", "2-3".into()));
        assert_eq!(span("3-"), ("three\nfour", "3-4".into()));
        assert_eq!(span("4-99"), ("four", "4-4".into()));
        assert_eq!(span("1-"), (text, "1-4".into()));
        let past = range("5-9").span(text).unwrap_err();
        assert_eq!(past.total_lines, 4);
        assert!(past.to_string().contains("has 4 line(s)"), "{past}");
        assert!(range("1-").span("").is_err());
        // A trailing newline ends the last line; it does not start one
        assert_eq!(range("2-").span("a\nb\n").unwrap().end_byte, 4);
        assert!(range("3-").span("a\nb\n").is_err());
    }

    #[test]
    fn citations_parse_and_print() {
        let c: DataCitation =
            "DATA-2026-0001:89fe7c6:votes/ballots.csv:110-300"
                .parse()
                .unwrap();
        assert_eq!(c.set.as_str(), "DATA-2026-0001");
        assert_eq!(c.commit.as_ref().unwrap().as_str(), "89fe7c6");
        assert_eq!(c.path.as_str(), "votes/ballots.csv");
        assert_eq!(c.lines, Some(range("110-300")));
        assert_eq!(
            c.to_string(),
            "DATA-2026-0001:89fe7c6:votes/ballots.csv:110-300"
        );

        let whole: DataCitation =
            " data-2026-1:89FE7C6:votes/ballots.csv ".parse().unwrap();
        assert_eq!(whole.set.as_str(), "DATA-2026-0001");
        assert_eq!(whole.commit.as_ref().unwrap().as_str(), "89fe7c6");
        assert_eq!(whole.lines, None);
        assert_eq!(
            whole.to_string(),
            "DATA-2026-0001:89fe7c6:votes/ballots.csv"
        );

        // A path may hold a `:`; what follows the last one is a range only
        // when it reads as one
        let colon: DataCitation =
            "DATA-2026-0001:89fe7c6:a:b.txt".parse().unwrap();
        assert_eq!(colon.path.as_str(), "a:b.txt");
        assert_eq!(colon.lines, None);
        // Only the canonical `<from>-<to>` is a range: `log:12` is a file
        let file: DataCitation =
            "DATA-2026-0001:89fe7c6:log:12".parse().unwrap();
        assert_eq!((file.path.as_str(), file.lines), ("log:12", None));
        assert_eq!(file.to_string(), "DATA-2026-0001:89fe7c6:log:12");
        let both: DataCitation =
            "DATA-2026-0001:89fe7c6:a:b.txt:5-".parse().unwrap();
        assert_eq!(both.path.as_str(), "a:b.txt");
        assert_eq!(both.lines, Some(range("5-")));
        let lead: DataCitation =
            "DATA-2026-0001:89fe7c6:./votes/x.md".parse().unwrap();
        assert_eq!(lead.path.as_str(), "votes/x.md");
    }

    #[test]
    fn citations_refuse_what_they_cannot_name() {
        let err = |s: &str| s.parse::<DataCitation>().unwrap_err();
        assert!(matches!(
            err("GOV-2026-0006:89fe7c6:x"),
            DataCitationError::NotDataSet(_)
        ));
        // Six hex digits or 41 are not a commit, so they start a path
        // (0.109.1); the server finds no such file
        assert_eq!(
            "DATA-2026-0001:89fe7c:x"
                .parse::<DataCitation>()
                .unwrap()
                .commit,
            None
        );
        assert_eq!(
            format!("DATA-2026-0001:{}:x", "a".repeat(41))
                .parse::<DataCitation>()
                .unwrap()
                .commit,
            None
        );
        assert!(matches!(
            err("DATA-2026-0001:89fe7c6"),
            DataCitationError::NoPath
        ));
        assert!(matches!(
            err("DATA-2026-0001:89fe7c6:"),
            DataCitationError::NoPath
        ));
        assert!(matches!(err("DATA-2026-0001:"), DataCitationError::NoPath));
        assert!(matches!(err("DATA-2026-0001::"), DataCitationError::NoPath));
        assert!(matches!(
            err("DATA-2026-0001:../secrets"),
            DataCitationError::Path(_)
        ));
        assert!(matches!(
            err("DATA-2026-0001: the votes"),
            DataCitationError::Commit(_)
        ));
        assert!(matches!(
            err("DATA-2026-0001:89fe7c6:../secrets"),
            DataCitationError::Path(_)
        ));
        let text = err("DATA-2026-0001:89fe7c6").to_string();
        assert!(text.contains("DATA-YYYY-NNNN:<commit>:<path>"), "{text}");
        assert!(text.contains("give its id alone"), "{text}");

        assert!(DataCitation::is_citation_shaped("DATA-2026-0001:x"));
        assert!(DataCitation::is_citation_shaped(" data-2026-1:"));
        assert!(!DataCitation::is_citation_shaped("GOV-2026-0006: yes"));
        assert!(!DataCitation::is_citation_shaped("DATA-2026-0001"));
    }

    #[test]
    fn citations_may_leave_the_commit_out() {
        let cite = |s: &str| {
            s.parse::<DataCitation>()
                .unwrap_or_else(|e| panic!("{s:?}: {e}"))
        };
        let c = cite("DATA-2026-0001:votes/ballots.csv:110-300");
        assert_eq!(c.set.as_str(), "DATA-2026-0001");
        assert_eq!(c.commit, None);
        assert_eq!(c.path.as_str(), "votes/ballots.csv");
        assert_eq!(c.lines, Some(range("110-300")));
        assert_eq!(c.to_string(), "DATA-2026-0001:votes/ballots.csv:110-300");

        let whole = cite(" data-2026-1:votes/ballots.csv ");
        assert_eq!((whole.commit.as_ref(), whole.lines), (None, None));
        assert_eq!(whole.to_string(), "DATA-2026-0001:votes/ballots.csv");
        assert_eq!(
            cite("DATA-2026-0001:README.md:5-").lines,
            Some(range("5-"))
        );

        // A first path segment of hex digits is not a commit: a commit
        // holds no `/`
        let hexdir = cite("DATA-2026-0001:abcdef1/file.md");
        assert_eq!(hexdir.commit, None);
        assert_eq!(hexdir.path.as_str(), "abcdef1/file.md");
        let hexdir = cite("DATA-2026-0001:abcdef1/file.md:110-300");
        assert_eq!(
            (hexdir.commit, hexdir.path.as_str(), hexdir.lines),
            (None, "abcdef1/file.md", Some(range("110-300")))
        );
        let long = format!("DATA-2026-0001:{}/x.md", "a".repeat(40));
        assert_eq!(cite(&long).commit, None);
        // ... and a hex segment followed by `:` is
        let at = cite("DATA-2026-0001:abcdef1:file.md");
        assert_eq!(at.commit.as_ref().unwrap().as_str(), "abcdef1");
        assert_eq!(at.path.as_str(), "file.md");

        // Paths with colons, with and without a range
        let colon = cite("DATA-2026-0001:a:b.txt");
        assert_eq!(
            (colon.commit.as_ref(), colon.path.as_str()),
            (None, "a:b.txt")
        );
        let log = cite("DATA-2026-0001:log:12");
        assert_eq!((log.path.as_str(), log.lines), ("log:12", None));
        let both = cite("DATA-2026-0001:a:b.txt:5-9");
        assert_eq!(
            (both.path.as_str(), both.lines),
            ("a:b.txt", Some(range("5-9")))
        );
        let dot = cite("DATA-2026-0001:./votes/x.md");
        assert_eq!(dot.path.as_str(), "votes/x.md");
        // An empty commit is one left out
        let empty = cite("DATA-2026-0001::votes/x.md");
        assert_eq!((empty.commit, empty.path.as_str()), (None, "votes/x.md"));
        // Not a commit, so the start of a path
        assert_eq!(
            cite("DATA-2026-0001:not-hex!:x").path.as_str(),
            "not-hex!:x"
        );
        // An id, `: ` and prose is not a citation
        assert!(matches!(
            "DATA-2026-0001: the votes".parse::<DataCitation>(),
            Err(DataCitationError::Commit(_))
        ));
        // A commit after a space still is one, as before
        assert!(cite("DATA-2026-0001: 89fe7c6:x.md").commit.is_some());

        // Round trips, with and without the commit
        for s in [
            "DATA-2026-0001:votes/ballots.csv",
            "DATA-2026-0001:votes/ballots.csv:110-300",
            "DATA-2026-0001:votes/ballots.csv:110-",
            "DATA-2026-0001:abcdef1/file.md:1-2",
            "DATA-2026-0001:a:b.txt:5-9",
            "DATA-2026-0001:89fe7c6:votes/ballots.csv:110-300",
            "DATA-2026-0001:89fe7c6:abcdef1:file.md",
        ] {
            assert_eq!(cite(s).to_string(), s);
            assert_eq!(cite(&cite(s).to_string()), cite(s));
        }

        // The pinned commit fills it in; any commit reads it
        let commit: GitCommitId =
            "89fe7c6aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".parse().unwrap();
        let other: GitCommitId =
            "0123456aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".parse().unwrap();
        assert!(c.reads_at(&commit) && c.reads_at(&other));
        let pinned = c.clone().at(commit);
        assert!(pinned.reads_at(&commit) && !pinned.reads_at(&other));
        assert_eq!(
            pinned.to_string(),
            "DATA-2026-0001:89fe7c6aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa:votes/ballots.csv:110-300"
        );
        assert_eq!((pinned.path, pinned.lines), (c.path, c.lines));
    }

    #[test]
    fn a_prefix_matches_its_commit() {
        let commit: GitCommitId =
            "89fe7c6aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".parse().unwrap();
        let prefix: GitCommitPrefix = "89FE7C6".parse().unwrap();
        assert!(prefix.matches(&commit));
        assert!(
            !"89fe7c7"
                .parse::<GitCommitPrefix>()
                .unwrap()
                .matches(&commit)
        );
        assert!(GitCommitPrefix::from(commit).matches(&commit));
    }
}
