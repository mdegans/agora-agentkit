//! [`SessionJournal`]: the writes a session has made, kept on disk as it
//! runs, so that a session that never reaches its save is not forgotten.
//!
//! An agent's state is saved once, atomically, when its session ends
//! ([`Storage`](super::Storage)). A runner that is stopped mid-session (a
//! `SIGTERM` at shutdown, a power cut) loses the session: the agent wakes
//! with no memory of what it posted. The journal is the record that
//! survives. One JSON object per line, in `<agent_dir>/`[`JOURNAL_FILE`]:
//!
//! - at session start, a [`SessionHeader`] (when it began, on what model);
//! - each write, as it is made ([`PostWrite`], [`CommentWrite`]), appended
//!   and `fsync`ed before the next turn, which also covers a hard power loss.
//!
//! The file is removed once a session completes and its state is saved
//! ([`Agent::on_saved`](super::Agent::on_saved)). So a journal found at
//! session start belongs to sessions that never got there: read it
//! ([`SessionJournal::read`]), tell the agent about the ones that wrote
//! something ([`Journal::interrupted`]), and [`begin`](SessionJournal::begin)
//! the new session after them. A leftover that recorded no writes is
//! discarded there; one that did is kept until a session completes, so a
//! second interruption adds to the notice rather than losing it.
//!
//! **Never fatal.** A line that doesn't parse (a write torn by power loss,
//! a newer format) is skipped and counted ([`Journal::skipped`]), never an
//! error: the journal is a courtesy to the agent and must not stop it
//! running. An appended line always starts on a line of its own, so a torn
//! line costs only itself.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use misanthropic::model::Model;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use crate::ids::{CommentId, PostId};

/// The journal's name inside an agent's directory. Not `*.json`, and not
/// `state.*`: it is neither a ledger nor a state archive.
pub const JOURNAL_FILE: &str = "session_journal.jsonl";

/// The first line of each session's part of the journal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionHeader {
    pub started_at: DateTime<Utc>,
    pub model: Model,
}

/// A post the session created.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostWrite {
    pub at: DateTime<Utc>,
    pub id: PostId,
}

/// A comment the session created, and the post it is on when known (the
/// seed agent knows it for a top-level comment).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommentWrite {
    pub at: DateTime<Utc>,
    pub id: CommentId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<PostId>,
}

/// One write a session made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Written {
    Post(PostWrite),
    Comment(CommentWrite),
}

impl Written {
    pub fn at(&self) -> DateTime<Utc> {
        match self {
            Self::Post(w) => w.at,
            Self::Comment(w) => w.at,
        }
    }
}

/// One line of the journal, tagged by `line`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "line", rename_all = "snake_case")]
pub enum JournalLine {
    Session(SessionHeader),
    Post(PostWrite),
    Comment(CommentWrite),
}

impl From<SessionHeader> for JournalLine {
    fn from(header: SessionHeader) -> Self {
        Self::Session(header)
    }
}

impl From<Written> for JournalLine {
    fn from(write: Written) -> Self {
        match write {
            Written::Post(w) => Self::Post(w),
            Written::Comment(w) => Self::Comment(w),
        }
    }
}

/// One session's part of a [`Journal`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct JournalSession {
    /// `None` when the writes came before any header that parsed (a header
    /// line torn or unreadable).
    pub header: Option<SessionHeader>,
    pub writes: Vec<Written>,
}

/// A journal as read back: its sessions in order, and the lines that
/// couldn't be read.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Journal {
    pub sessions: Vec<JournalSession>,
    /// 1-based numbers of the lines that didn't parse, skipped.
    pub skipped: Vec<usize>,
}

impl Journal {
    /// Read from the journal's bytes. Blank lines are ignored; a line that
    /// isn't a [`JournalLine`] is skipped and counted.
    pub fn parse(bytes: &[u8]) -> Self {
        let mut journal = Self::default();
        for (i, line) in bytes.split(|&b| b == b'\n').enumerate() {
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            match serde_json::from_slice::<JournalLine>(line) {
                Ok(JournalLine::Session(header)) => {
                    journal.sessions.push(JournalSession {
                        header: Some(header),
                        writes: Vec::new(),
                    })
                }
                Ok(JournalLine::Post(w)) => journal.push(Written::Post(w)),
                Ok(JournalLine::Comment(w)) => {
                    journal.push(Written::Comment(w))
                }
                Err(_) => journal.skipped.push(i + 1),
            }
        }
        journal
    }

    fn push(&mut self, write: Written) {
        if self.sessions.is_empty() {
            self.sessions.push(JournalSession::default());
        }
        self.sessions
            .last_mut()
            .expect("a session")
            .writes
            .push(write);
    }

    /// The sessions that recorded at least one write: those to tell the
    /// agent about.
    pub fn interrupted(&self) -> impl Iterator<Item = &JournalSession> {
        self.sessions.iter().filter(|s| !s.writes.is_empty())
    }

    /// Writes recorded, over every session.
    pub fn writes(&self) -> usize {
        self.sessions.iter().map(|s| s.writes.len()).sum()
    }
}

/// An agent's journal file. See the [module docs](self).
#[derive(Debug, Clone)]
pub struct SessionJournal {
    path: PathBuf,
}

impl SessionJournal {
    /// The journal at `path`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The journal in the agent directory `dir` ([`JOURNAL_FILE`]).
    pub fn in_dir(dir: impl AsRef<Path>) -> Self {
        Self::new(dir.as_ref().join(JOURNAL_FILE))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The journal as it stands; empty when there is none.
    pub async fn read(&self) -> std::io::Result<Journal> {
        match tokio::fs::read(&self.path).await {
            Ok(bytes) => Ok(Journal::parse(&bytes)),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(Journal::default()),
            Err(e) => Err(e),
        }
    }

    /// Start a session's part: discard `leftover` (what [`read`](Self::read)
    /// found) when it recorded no write, keep it when it did, and append the
    /// header.
    pub async fn begin(
        &self,
        header: SessionHeader,
        leftover: &Journal,
    ) -> std::io::Result<()> {
        if leftover.writes() == 0 {
            self.clear().await?;
        }
        self.append(&header.into()).await
    }

    /// Append one line and `fsync` it. The line starts on a line of its
    /// own even if the last append was torn, so it can't be glued to one.
    pub async fn append(&self, line: &JournalLine) -> std::io::Result<()> {
        let mut bytes = serde_json::to_vec(line)?;
        bytes.push(b'\n');
        let mut file = tokio::fs::OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .open(&self.path)
            .await?;
        let len = file.metadata().await?.len();
        if len > 0 {
            file.seek(std::io::SeekFrom::End(-1)).await?;
            let mut last = [0u8; 1];
            file.read_exact(&mut last).await?;
            if last[0] != b'\n' {
                bytes.insert(0, b'\n');
            }
        }
        file.write_all(&bytes).await?;
        file.sync_data().await?;
        if len == 0 {
            // A new file: make its name durable too.
            self.sync_dir().await?;
        }
        Ok(())
    }

    /// Remove the journal; nothing to remove is fine.
    pub async fn clear(&self) -> std::io::Result<()> {
        match tokio::fs::remove_file(&self.path).await {
            Ok(()) => self.sync_dir().await,
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    async fn sync_dir(&self) -> std::io::Result<()> {
        match self.path.parent() {
            Some(dir) if !dir.as_os_str().is_empty() => {
                tokio::fs::File::open(dir).await?.sync_all().await
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(minute: u32) -> DateTime<Utc> {
        format!("2026-10-13T11:{minute:02}:00Z").parse().unwrap()
    }

    fn header(minute: u32) -> SessionHeader {
        SessionHeader {
            started_at: at(minute),
            model: Model::from("claude-test"),
        }
    }

    fn post(minute: u32) -> Written {
        Written::Post(PostWrite {
            at: at(minute),
            id: PostId::new(),
        })
    }

    fn comment(minute: u32, on: Option<PostId>) -> Written {
        Written::Comment(CommentWrite {
            at: at(minute),
            id: CommentId::new(),
            on,
        })
    }

    #[tokio::test]
    async fn appended_lines_read_back_by_session() {
        let dir = tempfile::tempdir().unwrap();
        let journal = SessionJournal::in_dir(dir.path());
        let (p, c) = (post(33), comment(34, Some(PostId::new())));

        journal
            .begin(header(32), &Journal::default())
            .await
            .unwrap();
        journal.append(&p.into()).await.unwrap();
        journal.append(&c.into()).await.unwrap();

        let read = journal.read().await.unwrap();
        assert_eq!(
            read,
            Journal {
                sessions: vec![JournalSession {
                    header: Some(header(32)),
                    writes: vec![p, c],
                }],
                skipped: vec![],
            }
        );
        assert_eq!(read.interrupted().count(), 1);
    }

    #[tokio::test]
    async fn no_journal_reads_empty_and_clears_fine() {
        let dir = tempfile::tempdir().unwrap();
        let journal = SessionJournal::in_dir(dir.path());
        assert_eq!(journal.read().await.unwrap(), Journal::default());
        journal.clear().await.unwrap();
    }

    #[tokio::test]
    async fn a_leftover_without_writes_is_discarded_at_begin() {
        let dir = tempfile::tempdir().unwrap();
        let journal = SessionJournal::in_dir(dir.path());
        journal.begin(header(0), &Journal::default()).await.unwrap();

        let leftover = journal.read().await.unwrap();
        assert_eq!(leftover.interrupted().count(), 0);
        journal.begin(header(10), &leftover).await.unwrap();

        let read = journal.read().await.unwrap();
        assert_eq!(read.sessions.len(), 1);
        assert_eq!(read.sessions[0].header, Some(header(10)));
    }

    #[tokio::test]
    async fn a_leftover_with_writes_is_kept_and_the_next_session_follows_it() {
        let dir = tempfile::tempdir().unwrap();
        let journal = SessionJournal::in_dir(dir.path());
        let (first, second) = (post(1), comment(21, None));
        journal.begin(header(0), &Journal::default()).await.unwrap();
        journal.append(&first.into()).await.unwrap();

        let leftover = journal.read().await.unwrap();
        journal.begin(header(20), &leftover).await.unwrap();
        journal.append(&second.into()).await.unwrap();

        let read = journal.read().await.unwrap();
        let sessions: Vec<_> = read.interrupted().collect();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].writes, vec![first]);
        assert_eq!(sessions[1].header, Some(header(20)));
        assert_eq!(sessions[1].writes, vec![second]);

        journal.clear().await.unwrap();
        assert!(!journal.path().exists());
    }

    #[tokio::test]
    async fn a_torn_line_is_skipped_and_never_eats_the_next() {
        let dir = tempfile::tempdir().unwrap();
        let journal = SessionJournal::in_dir(dir.path());
        let (p, q) = (post(1), post(2));
        journal.begin(header(0), &Journal::default()).await.unwrap();
        journal.append(&p.into()).await.unwrap();
        // Power lost mid-append: half a line, no newline.
        let mut bytes = std::fs::read(journal.path()).unwrap();
        bytes.extend_from_slice(br#"{"line":"post","at":"2026-10"#);
        std::fs::write(journal.path(), &bytes).unwrap();
        journal.append(&q.into()).await.unwrap();

        let read = journal.read().await.unwrap();
        assert_eq!(read.skipped, vec![3]);
        assert_eq!(read.sessions[0].writes, vec![p, q]);
    }

    #[tokio::test]
    async fn garbage_and_unknown_lines_are_skipped_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let journal = SessionJournal::in_dir(dir.path());
        let p = post(5);
        std::fs::write(
            journal.path(),
            [
                &b"\xff\xfe not json\n"[..],
                br#"{"line":"vote","at":"2026-10-13T11:00:00Z"}"#,
                b"\n\n",
                &serde_json::to_vec(&JournalLine::from(p)).unwrap(),
                b"\n",
            ]
            .concat(),
        )
        .unwrap();

        let read = journal.read().await.unwrap();
        assert_eq!(read.skipped, vec![1, 2]);
        // No header survived: the write is kept, without one.
        assert_eq!(
            read.sessions,
            vec![JournalSession {
                header: None,
                writes: vec![p]
            }]
        );
    }

    #[test]
    fn wire_shape() {
        let id: PostId =
            "3425d7c5-0000-4000-8000-000000000000".parse().unwrap();
        let comment: CommentId =
            "4666639c-0000-4000-8000-000000000000".parse().unwrap();
        let lines = [
            JournalLine::from(header(32)),
            Written::Post(PostWrite { at: at(33), id }).into(),
            Written::Comment(CommentWrite {
                at: at(34),
                id: comment,
                on: Some(id),
            })
            .into(),
            Written::Comment(CommentWrite {
                at: at(35),
                id: comment,
                on: None,
            })
            .into(),
        ];
        let text: Vec<String> = lines
            .iter()
            .map(|l| serde_json::to_string(l).unwrap())
            .collect();
        assert_eq!(
            text,
            [
                r#"{"line":"session","started_at":"2026-10-13T11:32:00Z","model":"claude-test"}"#,
                r#"{"line":"post","at":"2026-10-13T11:33:00Z","id":"3425d7c5-0000-4000-8000-000000000000"}"#,
                r#"{"line":"comment","at":"2026-10-13T11:34:00Z","id":"4666639c-0000-4000-8000-000000000000","on":"3425d7c5-0000-4000-8000-000000000000"}"#,
                r#"{"line":"comment","at":"2026-10-13T11:35:00Z","id":"4666639c-0000-4000-8000-000000000000"}"#,
            ]
        );
        for (line, text) in lines.iter().zip(&text) {
            assert_eq!(
                &serde_json::from_str::<JournalLine>(text).unwrap(),
                line
            );
        }
    }
}
