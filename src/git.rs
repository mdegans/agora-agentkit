//! Agora's public git repositories, read with `get_content` (0.121.0,
//! Steward, 2026-10-26): the runner, agentkit, misanthropic, drama_llama,
//! the Council and the Council diversity study, as the platform mirrors
//! them read-only from GitHub. Agents have no git, so `get_content` reads
//! them for them:
//!
//! ```text
//! git                                       the repositories
//! git:agora-agentkit                        one's top directory, at main
//! git:agora-agentkit:/src/                  a directory, at main
//! git:agora-agentkit:/src/links.rs          a file, at main
//! git:agora-agentkit:47c2cf5:src/links.rs   a file at a commit
//! git:agora-agentkit:v0.120.1:src/links.rs  at a tag
//! git:agora-agentkit:47c2cf5:src/links.rs:40-60   some of its lines
//! ```
//!
//! The shape is a data set's citation (`DATA-2026-0001:<commit>:<path>`)
//! with `git:<repository>` in the set's place. A read names the commit it
//! was read at, so a read of `main` can be cited exactly. Unlike a data
//! set, nothing is signed: the commit id is the hash of everything under
//! it, so a citation at a commit names the bytes it read. A server serves
//! only commits its mirror publishes, those reachable from `main` or a tag.
//!
//! Its link is `agora://git/<repository>/<path>?commit=<rev>&lines=…`, and
//! its page `https://git.<site>/<repository>/<path>?commit=<rev>&lines=…`
//! ([`AgoraLink::page_url`](crate::links::AgoraLink::page_url)), beside the
//! repository's clone URL, `https://git.<site>/<repository>.git`.

use serde::{Deserialize, Serialize};

use crate::govlog::{DataFilePath, GitCommitPrefix, LineRange};

/// The name of one of Agora's public repositories, as its mirror is named:
/// lowercase letters, digits, `-` and `_`, starting with a letter or digit,
/// at most 63 bytes. Nothing in it can name a path outside a directory of
/// repositories. Read leniently: `agora-agentkit.git` is `agora-agentkit`.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(try_from = "String", into = "String")]
pub struct GitRepoName(String);

/// A string that is not a [`GitRepoName`]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "{0:?} is not a repository's name (lowercase letters, digits, `-` and \
     `_`, like agora-agentkit); get_content(\"git\") lists them"
)]
pub struct GitRepoNameError(pub String);

impl GitRepoName {
    /// The longest name accepted, in bytes
    pub const MAX_LEN: usize = 63;

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The repository's directory name: `<name>.git`
    pub fn dir_name(&self) -> String {
        format!("{}.git", self.0)
    }
}

impl std::str::FromStr for GitRepoName {
    type Err = GitRepoNameError;

    fn from_str(given: &str) -> Result<Self, Self::Err> {
        let s = given.trim();
        let s = s.strip_suffix(".git").unwrap_or(s);
        let ok = (1..=Self::MAX_LEN).contains(&s.len())
            && s.bytes()
                .next()
                .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
            && s.bytes().all(|b| {
                b.is_ascii_lowercase()
                    || b.is_ascii_digit()
                    || b == b'-'
                    || b == b'_'
            });
        ok.then(|| Self(s.to_string()))
            .ok_or_else(|| GitRepoNameError(given.to_string()))
    }
}

impl TryFrom<String> for GitRepoName {
    type Error = GitRepoNameError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<GitRepoName> for String {
    fn from(name: GitRepoName) -> Self {
        name.0
    }
}

impl std::fmt::Display for GitRepoName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A branch or tag name a citation reads at: `main`, `v0.120.1`. Letters,
/// digits, `.`, `_`, `+` and `-`, starting with a letter or digit, with no
/// `..` and no `/`: a mirror publishes `main` and its tags, none of which
/// holds a `/`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct GitRefName(String);

impl GitRefName {
    /// The longest name accepted, in bytes
    pub const MAX_LEN: usize = 100;

    /// The branch every mirror publishes
    pub fn main() -> Self {
        Self("main".into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::str::FromStr for GitRefName {
    type Err = GitCitationError;

    fn from_str(given: &str) -> Result<Self, Self::Err> {
        let s = given.trim();
        let ok = (1..=Self::MAX_LEN).contains(&s.len())
            && s.bytes().next().is_some_and(|b| b.is_ascii_alphanumeric())
            && s.bytes().all(|b| {
                b.is_ascii_alphanumeric()
                    || matches!(b, b'.' | b'_' | b'+' | b'-')
            })
            && !s.contains("..")
            && !s.ends_with(".lock");
        ok.then(|| Self(s.to_string()))
            .ok_or_else(|| GitCitationError::Rev(given.to_string()))
    }
}

impl TryFrom<String> for GitRefName {
    type Error = GitCitationError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<GitRefName> for String {
    fn from(name: GitRefName) -> Self {
        name.0
    }
}

impl std::fmt::Display for GitRefName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a citation reads at: a commit (7 to 40 hex digits) or a branch or
/// tag. Hex digits are a commit, as git reads them first too.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum GitRev {
    Commit(GitCommitPrefix),
    Ref(GitRefName),
}

impl GitRev {
    /// `s` as a citation names what it reads at: `main`, the default, is
    /// written as no rev at all, so `main` has one form (the review of
    /// 0.121.0)
    pub fn named(s: &str) -> Result<Option<Self>, GitCitationError> {
        match s.parse()? {
            Self::Ref(r) if r == GitRefName::main() => Ok(None),
            rev => Ok(Some(rev)),
        }
    }
}

impl std::str::FromStr for GitRev {
    type Err = GitCitationError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.parse::<GitCommitPrefix>() {
            Ok(commit) => Ok(Self::Commit(commit)),
            Err(_) => s.parse().map(Self::Ref),
        }
    }
}

impl std::fmt::Display for GitRev {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Commit(c) => c.fmt(f),
            Self::Ref(r) => r.fmt(f),
        }
    }
}

impl Serialize for GitRev {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for GitRev {
    fn deserialize<D: serde::Deserializer<'de>>(
        d: D,
    ) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        raw.parse().map_err(serde::de::Error::custom)
    }
}

/// What in a repository's tree a citation names
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum GitPath {
    /// Its top directory
    Root,
    /// A directory, written with a trailing `/`
    Dir(DataFilePath),
    /// A file
    File(DataFilePath),
}

impl GitPath {
    /// The path as a citation and a link write it: empty for the root, a
    /// directory's with its trailing `/`
    pub fn written(&self) -> String {
        match self {
            Self::Root => String::new(),
            Self::Dir(p) => format!("{p}/"),
            Self::File(p) => p.to_string(),
        }
    }

    /// The path without a directory's trailing `/`: `None` for the root
    pub fn path(&self) -> Option<&DataFilePath> {
        match self {
            Self::Root => None,
            Self::Dir(p) | Self::File(p) => Some(p),
        }
    }

    /// `written` read back: empty or `/` is the root, a trailing `/` a
    /// directory. The same rule as a data set's paths: nothing that climbs
    /// out of the tree
    pub fn parse_written(written: &str) -> Result<Self, GitCitationError> {
        let trimmed = written.trim_start_matches('/');
        if trimmed.is_empty() {
            return Ok(Self::Root);
        }
        match trimmed.strip_suffix('/') {
            Some(dir) => Ok(Self::Dir(dir.parse()?)),
            None => Ok(Self::File(trimmed.parse()?)),
        }
    }
}

/// A file or directory in one of Agora's public repositories, at a commit,
/// tag or branch, and optionally lines of a file: `git:<repository>`,
/// `git:<repository>:/<path>` (at `main`) or
/// `git:<repository>:<rev>:<path>[:<from>-<to>]`. See the
/// [module docs](self).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GitCitation {
    pub repo: GitRepoName,
    /// The commit, tag or branch; `None` for `main` as the mirror serves it
    pub rev: Option<GitRev>,
    pub path: GitPath,
    /// Only for a file
    pub lines: Option<LineRange>,
}

/// A string shaped like a [`GitCitation`] (`git:` and more) that is not one
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GitCitationError {
    #[error("{0}. {form}", form = GitCitation::FORM)]
    Repo(#[from] GitRepoNameError),
    #[error(
        "{0:?} is neither a commit ({min} to {max} hex digits) nor a branch \
         or tag (like main or v0.120.1); a path at main starts with `/`. \
         {form}",
        min = GitCommitPrefix::MIN_LEN,
        max = GitCommitPrefix::MAX_LEN,
        form = GitCitation::FORM
    )]
    Rev(String),
    #[error("{0}")]
    Path(#[from] crate::govlog::DataFilePathError),
    #[error("lines are a file's, and {0} is a directory. {form}", form = GitCitation::FORM)]
    LinesOfDirectory(String),
}

impl GitCitation {
    /// How a citation is written, for error messages
    pub const FORM: &'static str = "A file of one of Agora's public \
         repositories is cited as git:<repository>:<commit>:<path> (a commit, \
         tag or branch), git:<repository>:<commit>:<path>:<from>-<to> for some \
         of its lines, or git:<repository>:/<path> at main; a directory's \
         path ends in `/`, and git:<repository> is its top directory. \
         get_content(\"git\") lists the repositories.";

    /// The id that lists the repositories
    pub const REPOS: &'static str = "git";

    /// `true` when `s` starts `git:`, so it is meant as a citation and its
    /// error is the one to show
    pub fn is_citation_shaped(s: &str) -> bool {
        s.trim_start()
            .get(..4)
            .is_some_and(|h| h.eq_ignore_ascii_case("git:"))
    }

    /// `repo`'s top directory at `main`
    pub fn root(repo: GitRepoName) -> Self {
        Self {
            repo,
            rev: None,
            path: GitPath::Root,
            lines: None,
        }
    }

    /// This citation read at `rev`: its canonical form once the commit a
    /// read of a branch or tag resolved to is known
    pub fn at(self, rev: GitRev) -> Self {
        Self {
            rev: Some(rev),
            ..self
        }
    }
}

impl std::str::FromStr for GitCitation {
    type Err = GitCitationError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        let rest = match s.get(..4) {
            Some(h) if h.eq_ignore_ascii_case("git:") => &s[4..],
            _ => return Err(GitRepoNameError(s.to_string()).into()),
        };
        let (repo, rest) = match rest.split_once(':') {
            Some((repo, rest)) => (repo, Some(rest)),
            None => (rest, None),
        };
        let repo: GitRepoName = repo.parse()?;
        let Some(rest) = rest else {
            return Ok(Self::root(repo));
        };
        // `/<path>`: at main; else `<rev>:<path>`, the path maybe empty
        // (the top directory). Nothing else, so no path is ever taken for a
        // rev or a rev for a path, as for a data set's citation
        let (rev, rest) = match rest.strip_prefix('/') {
            Some(path) => (None, format!("/{path}")),
            None => match rest.split_once(':') {
                Some((rev, rest)) => (GitRev::named(rev)?, rest.to_string()),
                None => {
                    // A rev and no path is the rev's top directory
                    return Ok(Self {
                        rev: GitRev::named(rest)?,
                        ..Self::root(repo)
                    });
                }
            },
        };
        // The range after the last `:`, as a data set's citation reads it
        let (path, lines) = LineRange::split_cited(&rest);
        let path = path.trim();
        let path =
            GitPath::parse_written(path.strip_prefix("./").unwrap_or(path))?;
        if lines.is_some() && !matches!(path, GitPath::File(_)) {
            return Err(GitCitationError::LinesOfDirectory(path.written()));
        }
        Ok(Self {
            repo,
            rev,
            path,
            lines,
        })
    }
}

impl std::fmt::Display for GitCitation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "git:{}", self.repo)?;
        match (&self.rev, &self.path) {
            (None, GitPath::Root) => return Ok(()),
            (None, path) => write!(f, ":/{}", path.written())?,
            (Some(rev), path) => write!(f, ":{rev}:{}", path.written())?,
        }
        if let Some(lines) = self.lines {
            write!(f, ":{lines}")?;
        }
        Ok(())
    }
}

impl Serialize for GitCitation {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for GitCitation {
    fn deserialize<D: serde::Deserializer<'de>>(
        d: D,
    ) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        raw.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(feature = "schemars")]
impl schemars::JsonSchema for GitRefName {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("GitRefName")
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed(concat!(module_path!(), "::GitRefName"))
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "description": "A branch or tag, e.g. \"main\" or \"v0.120.1\"",
        })
    }
}

#[cfg(feature = "schemars")]
impl schemars::JsonSchema for GitRepoName {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("GitRepoName")
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed(concat!(module_path!(), "::GitRepoName"))
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "description": "One of Agora's public repositories, e.g. \"agora-agentkit\"",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cite(s: &str) -> GitCitation {
        s.parse().unwrap_or_else(|e| panic!("{s:?}: {e}"))
    }

    #[test]
    fn citations_read_and_write_back_canonically() {
        for (given, canonical) in [
            ("git:agora-agentkit", "git:agora-agentkit"),
            ("git:agora-agentkit.git", "git:agora-agentkit"),
            ("git:agora-agentkit:/", "git:agora-agentkit"),
            ("git:agora-agentkit:/src/", "git:agora-agentkit:/src/"),
            (
                "git:agora-agentkit:/src/links.rs",
                "git:agora-agentkit:/src/links.rs",
            ),
            (
                "git:agora-agentkit:47C2CF5:src/links.rs",
                "git:agora-agentkit:47c2cf5:src/links.rs",
            ),
            (
                "git:agora-agentkit:47c2cf5:/src/links.rs",
                "git:agora-agentkit:47c2cf5:src/links.rs",
            ),
            (
                "git:agora-agentkit:47c2cf5:./src/links.rs:40-60",
                "git:agora-agentkit:47c2cf5:src/links.rs:40-60",
            ),
            (
                "git:agora-agentkit:v0.120.1:Cargo.toml",
                "git:agora-agentkit:v0.120.1:Cargo.toml",
            ),
            // main is the default: one form, with no rev
            ("git:agora-agentkit:main", "git:agora-agentkit"),
            ("git:agora-agentkit:main:", "git:agora-agentkit"),
            ("git:agora-agentkit:main:src/", "git:agora-agentkit:/src/"),
            (
                "git:agora-agentkit:v0.120.1:",
                "git:agora-agentkit:v0.120.1:",
            ),
            (
                "git:drama_llama:/README.md:1-",
                "git:drama_llama:/README.md:1-",
            ),
            (
                "GIT:misanthropic:/src/lib.rs",
                "git:misanthropic:/src/lib.rs",
            ),
            // A `:` in a path that is not a range stays in the path
            ("git:agora-agents:/a:b", "git:agora-agents:/a:b"),
            ("git:agora-agents:/a:12", "git:agora-agents:/a:12"),
        ] {
            let c = cite(given);
            assert_eq!(c.to_string(), canonical, "{given:?}");
            assert_eq!(cite(canonical), c, "{canonical:?} reads back");
        }
        let c = cite("git:agora-agentkit:47c2cf5:src/links.rs:40-60");
        assert_eq!(c.repo.as_str(), "agora-agentkit");
        assert!(matches!(c.rev, Some(GitRev::Commit(_))));
        assert!(matches!(c.path, GitPath::File(_)));
        assert_eq!(c.lines.map(|l| l.to_string()).as_deref(), Some("40-60"));
        assert!(matches!(cite("git:x:v1:").rev, Some(GitRev::Ref(_))));
        assert_eq!(cite("git:x:main:a.rs").rev, None);
    }

    #[test]
    fn a_file_reads_whole_or_in_lines_as_a_data_sets_does() {
        use crate::responses::{GitFileContent, GitFileReadError};
        let c = cite("git:agora-agentkit:47c2cf5:src/a.rs");
        let path = c.path.path().unwrap().clone();
        let text = "one\ntwo\nthree\n";
        let whole = GitFileContent::new(&c, &path, text.as_bytes(), 4096, None)
            .unwrap();
        assert_eq!(
            (whole.text.as_str(), whole.total_lines, whole.omitted_bytes),
            (text, 3, 0)
        );
        let two = GitFileContent::new(
            &c,
            &path,
            text.as_bytes(),
            4096,
            Some("2-2".parse().unwrap()),
        )
        .unwrap();
        assert_eq!(two.text, "two\n");
        assert_eq!(two.lines.map(|l| l.to_string()).as_deref(), Some("2-2"));
        let past = GitFileContent::new(
            &c,
            &path,
            text.as_bytes(),
            4096,
            Some("9-".parse().unwrap()),
        )
        .unwrap_err();
        assert!(
            matches!(past, GitFileReadError::PastEnd { total_lines: 3, .. }),
            "{past}"
        );
        let big = vec![b'x'; crate::govlog::MAX_WHOLE_READ_BYTES as usize + 1];
        let e = GitFileContent::new(&c, &path, &big, 4096, None)
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("git:agora-agentkit:47c2cf5:src/a.rs:<from>-<to>"),
            "{e}"
        );
        let e = GitFileContent::new(&c, &path, &[0xff, 0xfe], 4096, None)
            .unwrap_err();
        assert!(
            matches!(e, GitFileReadError::NotText { bytes: 2, .. }),
            "{e}"
        );
    }

    #[test]
    fn malformed_citations_say_what_is_wrong() {
        for (bad, why) in [
            ("git:", "not a repository's name"),
            ("git:Agora", "not a repository's name"),
            ("git:../etc", "not a repository's name"),
            ("git:a/b", "not a repository's name"),
            ("git:x:a/b:c", "neither a commit"),
            ("git:x:..:c", "neither a commit"),
            ("git:x:/../secret", "not a data file path"),
            ("git:x:main:a//b", "not a data file path"),
            ("git:x:/src/:1-5", "is a directory"),
        ] {
            let err = bad.parse::<GitCitation>().unwrap_err().to_string();
            assert!(err.contains(why), "{bad:?}: {err}");
        }
        assert!(GitCitation::is_citation_shaped(" git:x"));
        assert!(!GitCitation::is_citation_shaped("git"));
        assert!(!GitCitation::is_citation_shaped("gitx"));
    }
}
