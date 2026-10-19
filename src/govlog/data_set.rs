//! The `data` of a `data_set` entry (`DATA-YYYY-NNNN`): the signed
//! manifest of a data set whose bytes live in a git repository.
//!
//! The entry pins one [commit](GitCommitId) and lists every file with its
//! size and [SHA-256](Sha256Hex), so the signature covers the bytes and not
//! just a name for them. A server serves only the paths listed here and only
//! after [checking](DataFile::check) each one; a correction is a revision
//! naming a new commit, never a moving ref.
//!
//! Redactable like a [`StewardRecord`](super::StewardRecord): the prose
//! fields are plain strings a redaction marker can replace.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{GitCommitId, Sha256Hex};

/// The only `agora_data_set` version there is
pub const DATA_SET_VERSION: u32 = 1;

/// The largest file one `get_content` read shows whole: what it holds at
/// its largest `comment_budget`. A larger listed file is read by
/// [`LineRange`] only (0.103)
pub const MAX_WHOLE_READ_BYTES: u64 =
    crate::requests::GetContentInput::MAX_COMMENT_BUDGET as u64;

/// The largest file a data set may list, 16 MiB (0.103; until then the
/// same as [`MAX_WHOLE_READ_BYTES`]). A server refuses to read a larger
/// blob at all.
///
/// Why 16 MiB: a ranged read still inflates the whole blob and checks its
/// SHA-256 before it serves any line (a slice of an unchecked blob is never
/// served), so this bounds the memory and hashing one read can cost. It is
/// also the object cap gitoxide applies under reduced trust, the limit the
/// data repository's readers were built and tested against (agora
/// `memory/project_data_sets.md`). Larger data belongs split across files.
pub const MAX_DATA_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// The `data` of a `data_set` entry. See the [module docs](self).
///
/// Unknown keys are tolerated, as on a record: a stored manifest also
/// carries the writer's `_blind`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct DataSet {
    /// Always [`DATA_SET_VERSION`]
    pub agora_data_set: u32,
    pub title: String,
    /// What the data is, where it came from and how to read it (markdown)
    pub description: String,
    /// Who compiled it, in their own words
    pub compiled_by: String,
    /// The data repository's name, as it is cloned (`agora-data`). Each
    /// data set may have its own repository (0.103): a server reads it at
    /// `<its data repositories>/<repo>.git`
    pub repo: DataRepoName,
    /// The commit every file is read at
    pub commit: GitCommitId,
    /// The file a reader should open first; one of `files`
    pub index: DataFilePath,
    /// Every file a server may serve, sorted by path
    pub files: Vec<DataFile>,
}

/// One file of a [`DataSet`]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct DataFile {
    /// Relative to the repository root
    pub path: DataFilePath,
    /// Size in bytes
    pub bytes: u64,
    /// SHA-256 of the file's bytes (not git's object hash)
    pub sha256: Sha256Hex,
    /// One line on what the file holds
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl DataSet {
    /// What a writer refuses to sign: another version, a blank text field,
    /// no files, paths out of order or repeated, an index that is not
    /// listed, or a note that is blank or more than one line
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.agora_data_set != DATA_SET_VERSION {
            return Err("agora_data_set is not the version this writer writes");
        }
        let blank = |s: &str| s.trim().is_empty();
        if blank(&self.title)
            || blank(&self.description)
            || blank(&self.compiled_by)
        {
            return Err(
                "title, description and compiled_by must all say something",
            );
        }
        if self.files.is_empty() {
            return Err("a data set lists at least one file");
        }
        if self.files.windows(2).any(|w| w[0].path >= w[1].path) {
            return Err("files must be sorted by path, each listed once");
        }
        if self.files.iter().any(|f| f.bytes > MAX_DATA_FILE_BYTES) {
            return Err(
                "a file is larger than MAX_DATA_FILE_BYTES (16 MiB); split it",
            );
        }
        if self.file(&self.index).is_none() {
            return Err("the index must be one of the listed files");
        }
        if self.files.iter().any(|f| {
            f.note
                .as_deref()
                .is_some_and(|n| blank(n) || n.contains(['\n', '\r']))
        }) {
            return Err("a file's note is one line, or left out");
        }
        Ok(())
    }

    /// The listed file at `path`, if it is listed
    pub fn file(&self, path: &DataFilePath) -> Option<&DataFile> {
        self.files
            .binary_search_by(|f| f.path.cmp(path))
            .ok()
            .map(|i| &self.files[i])
            // Unsorted (an invalid manifest): still never a wrong answer.
            .or_else(|| self.files.iter().find(|f| &f.path == path))
    }

    /// Every file's size, added up
    pub fn total_bytes(&self) -> u64 {
        self.files.iter().map(|f| f.bytes).sum()
    }

    /// The files under `prefix` (a directory ending in `/`): how many, and
    /// their bytes added up
    pub fn under(&self, prefix: &str) -> (u64, u64) {
        self.files
            .iter()
            .filter(|f| f.path.as_str().starts_with(prefix))
            .fold((0, 0), |(n, b), f| (n + 1, b + f.bytes))
    }

    /// The directories at the repository root that hold files, in order
    fn top_directories(&self) -> Vec<&str> {
        let mut dirs: Vec<&str> = Vec::new();
        for f in &self.files {
            if let Some((dir, _)) = f.path.as_str().split_once('/')
                && !dirs.contains(&dir)
            {
                dirs.push(dir);
            }
        }
        dirs
    }

    /// What `attachment=<asked>` names: a listed file, or a directory
    /// (ending in `/`). Paths are relative to the repository root; one
    /// relative to a directory at the root (`README.md` for
    /// `votes/README.md`, the way a set's own files cite each other)
    /// resolves when exactly one directory holds it, and the target says
    /// what was asked (`asked` is set only then). Surrounding whitespace
    /// and a leading `./` or `/` are ignored. (0.91.4)
    pub fn resolve(
        &self,
        asked: &str,
    ) -> Result<DataSetTarget<'_>, DataSetLookupError> {
        let trimmed = asked.trim();
        let path = trimmed.strip_prefix("./").unwrap_or(trimmed);
        let path = path.trim_start_matches('/');
        let lookup_error = |kind| DataSetLookupError {
            asked: asked.to_string(),
            kind,
        };
        if path.is_empty() {
            return Err(lookup_error(DataSetLookupFailure::NotFound));
        }
        let tops = self.top_directories();
        let file_at =
            |p: &str| p.parse().ok().and_then(|p: DataFilePath| self.file(&p));
        let directory = |prefix: String| {
            let (files, bytes) = self.under(&prefix);
            (files > 0).then_some((prefix, files, bytes))
        };
        // One match, or the error for none or several
        let one = |mut found: Vec<String>| match found.len() {
            1 => Ok(found.pop()),
            0 => Ok(None),
            _ => Err(lookup_error(DataSetLookupFailure::Ambiguous(found))),
        };
        if path.ends_with('/') {
            if directory(path.to_string()).is_some() {
                return Ok(DataSetTarget::Directory {
                    prefix: path.to_string(),
                    asked: None,
                });
            }
            let found = tops
                .iter()
                .filter_map(|top| directory(format!("{top}/{path}")))
                .map(|(prefix, ..)| prefix)
                .collect();
            return match one(found)? {
                Some(prefix) => Ok(DataSetTarget::Directory {
                    asked: Some(asked.to_string()),
                    prefix,
                }),
                None => Err(lookup_error(DataSetLookupFailure::NotFound)),
            };
        }
        if let Some(file) = file_at(path) {
            return Ok(DataSetTarget::File { file, asked: None });
        }
        let not_a_file = |(prefix, files, bytes)| {
            lookup_error(DataSetLookupFailure::Directory {
                prefix,
                files,
                bytes,
            })
        };
        if let Some(dir) = directory(format!("{path}/")) {
            return Err(not_a_file(dir));
        }
        let found = tops
            .iter()
            .map(|top| format!("{top}/{path}"))
            .filter(|p| file_at(p).is_some())
            .collect();
        if let Some(full) = one(found)?
            && let Some(file) = file_at(&full)
        {
            return Ok(DataSetTarget::File {
                file,
                asked: Some(asked.to_string()),
            });
        }
        let dirs: Vec<_> = tops
            .iter()
            .filter_map(|top| directory(format!("{top}/{path}/")))
            .collect();
        match dirs.len() {
            0 => Err(lookup_error(DataSetLookupFailure::NotFound)),
            1 => Err(not_a_file(dirs.into_iter().next().expect("one"))),
            _ => Err(lookup_error(DataSetLookupFailure::Ambiguous(
                dirs.into_iter().map(|(prefix, ..)| prefix).collect(),
            ))),
        }
    }
}

/// What a data set's `attachment=` named (0.91.4)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataSetTarget<'a> {
    /// A listed file
    File {
        file: &'a DataFile,
        /// What was asked, when it is not the file's path
        asked: Option<String>,
    },
    /// The files under a directory: `prefix` ends in `/`
    Directory {
        prefix: String,
        /// What was asked, when it is not `prefix`
        asked: Option<String>,
    },
}

/// `attachment=` names no file or directory of a data set: the message says
/// what it was and what to ask for instead, in full (0.91.4)
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("attachment={asked:?}: {kind}")]
pub struct DataSetLookupError {
    pub asked: String,
    pub kind: DataSetLookupFailure,
}

/// Why a [`DataSetLookupError`]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DataSetLookupFailure {
    /// A directory, asked for without its trailing `/`
    #[error(
        "{prefix} is a directory of this data set ({files} files, {bytes} \
         bytes), not a file. List it with attachment=\"{prefix}\" (a directory \
         ends in `/`), or read one of its files by its path."
    )]
    Directory {
        prefix: String,
        files: u64,
        bytes: u64,
    },
    /// A path relative to a top directory that more than one holds
    #[error(
        "that path is under more than one of this data set's top directories: \
         {}. Ask for one of them by its full path.",
        .0.join(", ")
    )]
    Ambiguous(Vec<String>),
    #[error(
        "no file or directory of this data set is at that path. Paths are \
         relative to the data repository's root (a path relative to a \
         directory at the root also works when only one directory holds it); \
         a directory ends in `/`."
    )]
    NotFound,
}

impl DataFile {
    /// The listing for `bytes` read at `path`, with no note
    pub fn of(path: DataFilePath, bytes: &[u8]) -> Self {
        Self {
            path,
            bytes: bytes.len() as u64,
            sha256: Sha256Hex::from(<[u8; 32]>::from(Sha256::digest(bytes))),
            note: None,
        }
    }

    /// `Ok` when `bytes` are exactly the bytes this listing signs for
    pub fn check(&self, bytes: &[u8]) -> Result<(), DataFileMismatch> {
        let got = Self::of(self.path.clone(), bytes);
        if got.bytes == self.bytes && got.sha256 == self.sha256 {
            return Ok(());
        }
        Err(DataFileMismatch {
            path: self.path.clone(),
            expected_bytes: self.bytes,
            expected_sha256: self.sha256,
            got_bytes: got.bytes,
            got_sha256: got.sha256,
        })
    }
}

/// The bytes read for a [`DataFile`] are not the ones its manifest signs
/// for. Never serve them.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "{path}: the manifest signs for {expected_bytes} bytes with SHA-256 \
     {expected_sha256}, but {got_bytes} bytes with SHA-256 {got_sha256} were read"
)]
pub struct DataFileMismatch {
    pub path: DataFilePath,
    pub expected_bytes: u64,
    pub expected_sha256: Sha256Hex,
    pub got_bytes: u64,
    pub got_sha256: Sha256Hex,
}

/// A string that is not a [`DataRepoName`]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "not a data repository name (1 to 63 lowercase letters, digits and `-`, \
     starting with a letter or digit): {0:?}"
)]
pub struct DataRepoNameError(pub String);

/// The name of a data repository, as a [`DataSet`] names it: lowercase
/// letters, digits and `-`, starting with a letter or digit, at most 63
/// bytes (0.103). Nothing in it can name a path outside a directory of
/// repositories (no `.`, `/` or `\`), so a server may join it to one before
/// touching the filesystem.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(try_from = "String", into = "String")]
pub struct DataRepoName(String);

impl DataRepoName {
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

impl std::str::FromStr for DataRepoName {
    type Err = DataRepoNameError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let ok = (1..=Self::MAX_LEN).contains(&s.len())
            && s.bytes().all(|b| {
                b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'
            })
            && !s.starts_with('-');
        ok.then(|| Self(s.to_string()))
            .ok_or_else(|| DataRepoNameError(s.to_string()))
    }
}

impl TryFrom<String> for DataRepoName {
    type Error = DataRepoNameError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<DataRepoName> for String {
    fn from(name: DataRepoName) -> Self {
        name.0
    }
}

impl std::fmt::Display for DataRepoName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for DataRepoName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[cfg(feature = "schemars")]
impl schemars::JsonSchema for DataRepoName {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("DataRepoName")
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed(concat!(module_path!(), "::DataRepoName"))
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "description": "A data repository's name, e.g. \"agora-data\"",
        })
    }
}

/// A path that is not a [`DataFilePath`]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "not a data file path (relative, `/`-separated, no empty, `.` or `..` \
     parts, no backslashes or control characters): {0:?}"
)]
pub struct DataFilePathError(pub String);

/// A file's path in a data repository: relative to its root and
/// `/`-separated, with nothing in it that could climb out of the tree
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(try_from = "String", into = "String")]
pub struct DataFilePath(String);

impl DataFilePath {
    /// The longest path accepted, in bytes
    pub const MAX_LEN: usize = 1024;

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::str::FromStr for DataFilePath {
    type Err = DataFilePathError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let ok = !s.is_empty()
            && s.len() <= Self::MAX_LEN
            && !s.chars().any(|c| c == '\\' || c.is_control())
            && s.split('/')
                .all(|part| !part.is_empty() && part != "." && part != "..");
        ok.then(|| Self(s.to_string()))
            .ok_or_else(|| DataFilePathError(s.to_string()))
    }
}

impl TryFrom<String> for DataFilePath {
    type Error = DataFilePathError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<DataFilePath> for String {
    fn from(path: DataFilePath) -> Self {
        path.0
    }
}

impl std::fmt::Display for DataFilePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for DataFilePath {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

// Inline, like every id newtype: a derived schema would be a `$ref`.
#[cfg(feature = "schemars")]
impl schemars::JsonSchema for DataFilePath {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("DataFilePath")
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed(concat!(module_path!(), "::DataFilePath"))
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "description": "A file's path in the data repository, relative \
                            to its root, e.g. \"votes/index.md\"",
        })
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::super::{Blind, blind_data, non_integer_number};
    use super::*;

    fn path(s: &str) -> DataFilePath {
        s.parse().unwrap()
    }

    pub(crate) fn manifest() -> DataSet {
        let mut readme = DataFile::of(path("votes/README.md"), b"# Votes\n");
        readme.note = Some("Read me first".into());
        DataSet {
            agora_data_set: DATA_SET_VERSION,
            title: "Ratification votes".into(),
            description: "Every ballot cast on GOV-2026-0001.".into(),
            compiled_by: "claude-opus, from the governance log".into(),
            repo: "agora-data".parse().unwrap(),
            commit: "0123456789abcdef0123456789abcdef01234567".parse().unwrap(),
            index: path("votes/README.md"),
            // Sorted: "R" (0x52) before "b" (0x62).
            files: vec![
                readme,
                DataFile::of(path("votes/ballots.csv"), b"seat,vote\n"),
            ],
        }
    }

    #[test]
    fn a_manifest_validates_and_round_trips() {
        let m = manifest();
        assert_eq!(m.validate(), Ok(()));
        let value = serde_json::to_value(&m).unwrap();
        assert_eq!(value["agora_data_set"], 1);
        assert_eq!(value["commit"], "0123456789abcdef0123456789abcdef01234567");
        assert_eq!(value["files"][0]["path"], "votes/README.md");
        assert_eq!(
            value["files"][1]["sha256"],
            // sha256("seat,vote\n")
            hex::encode(Sha256::digest(b"seat,vote\n"))
        );
        assert!(value["files"][1].get("note").is_none());
        assert_eq!(non_integer_number(&value), None);
        let back: DataSet = serde_json::from_value(value).unwrap();
        assert_eq!(back, m);
        assert_eq!(m.total_bytes(), 8 + 10);
    }

    #[test]
    fn a_stored_manifest_still_reads() {
        let stored = blind_data(
            &serde_json::to_value(manifest()).unwrap(),
            Blind::from([7; 32]),
        )
        .unwrap();
        let read: DataSet = serde_json::from_value(stored).unwrap();
        assert_eq!(read, manifest());
    }

    #[test]
    fn what_a_writer_refuses() {
        let refused = |f: &dyn Fn(&mut DataSet)| {
            let mut m = manifest();
            f(&mut m);
            m.validate().unwrap_err()
        };
        refused(&|m| m.agora_data_set = 2);
        refused(&|m| m.compiled_by = " ".into());
        refused(&|m| m.files.clear());
        refused(&|m| m.files.reverse());
        refused(&|m| {
            let again = m.files[0].clone();
            m.files.insert(0, again);
        });
        refused(&|m| m.index = path("votes/missing.md"));
        refused(&|m| m.files[0].note = Some("two\nlines".into()));
        refused(&|m| m.files[0].note = Some("".into()));
        refused(&|m| m.files[1].bytes = MAX_DATA_FILE_BYTES + 1);
    }

    #[test]
    fn a_tampered_file_is_refused() {
        let m = manifest();
        let ballots = m.file(&path("votes/ballots.csv")).unwrap();
        assert_eq!(ballots.check(b"seat,vote\n"), Ok(()));
        let err = ballots.check(b"seat,vote\nlawyer,yes\n").unwrap_err();
        assert_eq!(err.expected_bytes, 10);
        assert_eq!(err.got_bytes, 21);
        // Same length, one byte changed: the hash is what catches it.
        let err = ballots.check(b"seat,VOTE\n").unwrap_err();
        assert_eq!(err.got_bytes, err.expected_bytes);
        assert_ne!(err.got_sha256, err.expected_sha256);
        assert!(err.to_string().contains("votes/ballots.csv"), "{err}");
        assert!(m.file(&path("votes/other.csv")).is_none());
    }

    /// What `attachment=` resolves to: exact paths, directories, a path
    /// relative to a top directory, and errors that say what to ask for
    /// (beta round, 2026-10-15; 0.91.4)
    #[test]
    fn attachments_resolve_or_say_why_not() {
        let mut m = manifest();
        m.files.push(DataFile::of(path("votes/raw/r1.json"), b"{}"));
        m.files.sort_by(|a, b| a.path.cmp(&b.path));
        let file = |asked: &str| match m.resolve(asked) {
            Ok(DataSetTarget::File { file, asked }) => {
                (file.path.to_string(), asked)
            }
            other => panic!("{asked}: {other:?}"),
        };
        assert_eq!(file("votes/README.md"), ("votes/README.md".into(), None));
        // Relative to the one top directory, as the set's files cite it
        assert_eq!(
            file("README.md"),
            ("votes/README.md".into(), Some("README.md".into()))
        );
        assert_eq!(
            file("raw/r1.json"),
            ("votes/raw/r1.json".into(), Some("raw/r1.json".into()))
        );
        // Normalised, not resolved: nothing to say
        for exact in [
            "./votes/ballots.csv",
            "/votes/ballots.csv",
            " votes/ballots.csv\n",
        ] {
            assert_eq!(
                file(exact),
                ("votes/ballots.csv".into(), None),
                "{exact:?}"
            );
        }
        for empty in ["", "/", "./", "  "] {
            assert_eq!(
                m.resolve(empty).unwrap_err().kind,
                DataSetLookupFailure::NotFound,
                "{empty:?}"
            );
        }
        let dir = |asked: &str| match m.resolve(asked) {
            Ok(DataSetTarget::Directory { prefix, asked }) => (prefix, asked),
            other => panic!("{asked}: {other:?}"),
        };
        assert_eq!(dir("votes/"), ("votes/".into(), None));
        assert_eq!(dir("raw/"), ("votes/raw/".into(), Some("raw/".into())));
        // A directory without its `/`: says so, with what is in it
        let err = m.resolve("votes/raw").unwrap_err();
        assert_eq!(
            err.kind,
            DataSetLookupFailure::Directory {
                prefix: "votes/raw/".into(),
                files: 1,
                bytes: 2
            }
        );
        let text = err.to_string();
        assert!(text.contains("votes/raw/ is a directory"), "{text}");
        assert!(text.contains("attachment=\"votes/raw/\""), "{text}");
        assert!(matches!(
            m.resolve("raw").unwrap_err().kind,
            DataSetLookupFailure::Directory { .. }
        ));
        assert_eq!(
            m.resolve("votes/missing.csv").unwrap_err().kind,
            DataSetLookupFailure::NotFound
        );
        assert_eq!(
            m.resolve("../x").unwrap_err().kind,
            DataSetLookupFailure::NotFound
        );
        // Under two top directories: ambiguous, never a guess
        m.files.push(DataFile::of(path("tallies/README.md"), b"t"));
        m.files.sort_by(|a, b| a.path.cmp(&b.path));
        let err = m.resolve("README.md").unwrap_err();
        assert_eq!(
            err.kind,
            DataSetLookupFailure::Ambiguous(vec![
                "tallies/README.md".into(),
                "votes/README.md".into()
            ])
        );
        m.files
            .push(DataFile::of(path("tallies/raw/t.json"), b"{}"));
        m.files.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(
            m.resolve("raw/").unwrap_err().kind,
            DataSetLookupFailure::Ambiguous(vec![
                "tallies/raw/".into(),
                "votes/raw/".into()
            ])
        );
    }

    /// A repository name cannot leave the directory of repositories
    /// (0.103)
    #[test]
    fn repo_names_are_one_plain_word() {
        for good in ["agora-data", "a", "witness-2026-10-18", &"a".repeat(63)] {
            assert_eq!(good.parse::<DataRepoName>().unwrap().as_str(), good);
        }
        assert_eq!(
            "agora-data".parse::<DataRepoName>().unwrap().dir_name(),
            "agora-data.git"
        );
        for bad in [
            "",
            "-a",
            "Agora",
            "a.git",
            "..",
            "a/b",
            "a\\b",
            "a b",
            "a_b",
            "/etc",
            &"a".repeat(64),
        ] {
            assert!(bad.parse::<DataRepoName>().is_err(), "{bad:?}");
        }
        let mut m = serde_json::to_value(manifest()).unwrap();
        m["repo"] = "../secrets".into();
        assert!(serde_json::from_value::<DataSet>(m).is_err());
    }

    #[test]
    fn paths_cannot_climb_out() {
        for good in ["index.md", "a/b/c.csv", "with space/ü.txt", ".hidden"] {
            assert_eq!(path(good).as_str(), good);
        }
        for bad in [
            "",
            "/etc/passwd",
            "../secrets",
            "a/../../b",
            "a//b",
            "a/./b",
            "a/",
            "a\\b",
            "a\nb",
            "nul\0",
        ] {
            assert!(bad.parse::<DataFilePath>().is_err(), "{bad:?}");
            assert!(
                serde_json::from_value::<DataFilePath>(serde_json::json!(bad))
                    .is_err(),
                "{bad:?} via serde"
            );
        }
        assert!("a".repeat(1025).parse::<DataFilePath>().is_err());
    }

    #[test]
    fn a_commit_is_twenty_bytes_of_hex() {
        assert!("0123456789abcdef".parse::<GitCommitId>().is_err());
        assert!(
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                .parse::<GitCommitId>()
                .is_err()
        );
        let id: GitCommitId =
            "0123456789ABCDEF0123456789abcdef01234567".parse().unwrap();
        assert_eq!(id.to_hex(), "0123456789abcdef0123456789abcdef01234567");
    }
}
