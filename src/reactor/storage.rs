//! [`FsStorage`] — a directory-per-agent [`Storage`] backend.
//!
//! [`Storage`]: super::Storage

use std::path::{Path, PathBuf};

use crate::ids::AgentId;

use super::backend::{AgentNotFound, Loaded, NoVersion, Storage};

/// The envelope format [`FsStorage`] writes. Bump on layout change; `load`
/// rejects anything newer so an old binary never half-reads a new layout.
pub const FORMAT: u64 = 1;

/// The format of a chained agent's envelope: format 1 plus `"sealed":
/// true` ([`FsStorage::save_sealed_raw`]). Format 2 until 0.158: binaries
/// before then refuse a format-3 state instead of running the agent
/// unsealed, and before 0.147 refuse any sealed one. A one-way door: after
/// one save by 0.158, an older runner can't load the agent. Format 2 is
/// still read
pub const SEALED_FORMAT: u64 = 3;

/// The newest format this binary reads
pub const MAX_FORMAT: u64 = SEALED_FORMAT;

/// The value file inside an agent's directory.
const STATE_FILE: &str = "state.json";
/// A save's new value, before its rename over [`STATE_FILE`]
const TMP_FILE: &str = "state.json.tmp";
/// A test's file: a save in its directory fails after creating its tmp
#[cfg(test)]
const FAIL_AFTER_TMP: &str = "fail-after-tmp";
/// Archive and aside names' timestamps (UTC, milliseconds)
const STAMP: &str = "%Y%m%dT%H%M%S%3fZ";

/// How long a save waits for another save of the same agent to release
/// the directory's lock: room for a slow fsync of a large state on a busy
/// disk
pub const LOCK_DEADLINE: std::time::Duration =
    std::time::Duration::from_secs(60);

/// A [`Storage`] over `<root>/<agent_id>/state.json`, agent-kind-agnostic.
///
/// Values are wrapped in a `{format, state}` envelope and written
/// atomically (tmp + rename + directory fsync). **Every overwrite archives
/// the previous value** to a `state.<utc-ms>.json` hard link first, so
/// `state.json` is never absent — retention is deliberately infinite
/// (rollback + transcript history; compress cold archives externally if a
/// deployment ever cares).
///
/// A save holds an exclusive lock on the agent's directory, waiting at
/// most [`LOCK_DEADLINE`] for it, and never truncates a `state.json.tmp`
/// it finds there: that is a save cut short, set aside as
/// `state.json.tmp.<utc-ms>.aside` first. A save that fails removes what
/// it made itself (its tmp, and its archive link before the rename), so
/// retries leave nothing behind.
///
/// Every save fails, loudly:
/// - on a filesystem without hard links;
/// - where Linux's `fs.protected_hardlinks` refuses the link: the runner's
///   user must own `state.json` or be able to read and write it, so a
///   state file restored as another user fails every save with EPERM;
/// - where the agent's directory can't be locked (`flock` on a directory
///   descriptor; NFS emulates it, and it may fail there).
#[derive(Debug, Clone)]
pub struct FsStorage {
    root: PathBuf,
}

impl FsStorage {
    /// A store rooted at `root`. The directory is created lazily on first
    /// save; a missing root just means no agents yet.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The directory holding `id`'s value and archives.
    pub fn agent_dir(&self, id: AgentId) -> PathBuf {
        self.root.join(id.to_string())
    }

    /// `id`'s [`SessionJournal`], beside its `state.json`.
    ///
    /// [`SessionJournal`]: super::journal::SessionJournal
    pub fn journal(&self, id: AgentId) -> super::journal::SessionJournal {
        super::journal::SessionJournal::in_dir(self.agent_dir(id))
    }

    /// Hard-link `state.json` as `state.<utc-ms>.json`, if there is one
    async fn archive(
        &self,
        dir: &Path,
    ) -> Result<Option<PathBuf>, std::io::Error> {
        archive_as(dir, chrono::Utc::now().format(STAMP)).await
    }
}

/// Hard-link `dir/<from>` as `dir/<prefix><ts><suffix>`, suffixing `-N` to
/// `ts` on a collision; never replaces a file. `None` when there is no
/// `from`
async fn link_unique(
    dir: &Path,
    from: &str,
    prefix: &str,
    ts: impl std::fmt::Display,
    suffix: &str,
) -> Result<Option<PathBuf>, std::io::Error> {
    let from = dir.join(from);
    let mut candidate = dir.join(format!("{prefix}{ts}{suffix}"));
    let mut n = 0u32;
    loop {
        match tokio::fs::hard_link(&from, &candidate).await {
            Ok(()) => return Ok(Some(candidate)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                n += 1;
                candidate = dir.join(format!("{prefix}{ts}-{n}{suffix}"));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(e) => return Err(e),
        }
    }
}

/// Hard-link `state.json` as `state.<ts>.json` (`-N` on a same-millisecond
/// collision), and fsync `dir` so the link is durable before the rename
/// that replaces `state.json`. The link, if there was a `state.json`
async fn archive_as(
    dir: &Path,
    ts: impl std::fmt::Display,
) -> Result<Option<PathBuf>, std::io::Error> {
    let link = link_unique(dir, STATE_FILE, "state.", ts, ".json").await?;
    if link.is_some() {
        sync_dir(dir).await?;
    }
    Ok(link)
}

/// Move a leftover `state.json.tmp` (a save cut short) to
/// `state.json.tmp.<ts>.aside` (`-N` on a collision), never over another
/// file, and fsync `dir`
async fn set_tmp_aside(
    dir: &Path,
    ts: impl std::fmt::Display,
) -> Result<Option<PathBuf>, std::io::Error> {
    let Some(aside) =
        link_unique(dir, TMP_FILE, "state.json.tmp.", ts, ".aside").await?
    else {
        return Ok(None);
    };
    tokio::fs::remove_file(dir.join(TMP_FILE)).await?;
    sync_dir(dir).await?;
    Ok(Some(aside))
}

async fn sync_dir(dir: &Path) -> Result<(), std::io::Error> {
    tokio::fs::File::open(dir).await?.sync_all().await
}

/// An exclusive advisory lock on `dir` itself, held across a save, or
/// `None` once `deadline` has passed. Never blocks a thread
async fn lock_dir(
    dir: &Path,
    deadline: std::time::Duration,
) -> Result<Option<std::fs::File>, std::io::Error> {
    let file = std::fs::File::open(dir)?;
    let until = tokio::time::Instant::now() + deadline;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(Some(file)),
            Err(std::fs::TryLockError::WouldBlock) => {}
            Err(std::fs::TryLockError::Error(e)) => return Err(e),
        }
        if tokio::time::Instant::now() >= until {
            return Ok(None);
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// Error type for [`FsStorage`]. All variants are fatal
/// ([`RetryAfter`]'s default): a failing disk does not get better on retry.
///
/// [`RetryAfter`]: crate::reactor::RetryAfter
#[derive(Debug, thiserror::Error)]
pub enum FsStorageError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    NotFound(#[from] AgentNotFound),
    #[error(
        "unsupported envelope format {found} (this binary reads ≤ {MAX_FORMAT})"
    )]
    Format { found: u64 },
    #[error("not an FsStorage envelope: missing {0}")]
    Envelope(&'static str),
    /// Another save held the agent's directory past [`LOCK_DEADLINE`]
    #[error("agent {0}'s directory stayed locked by another save")]
    Locked(AgentId),
}

impl crate::reactor::RetryAfter for FsStorageError {}

#[async_trait::async_trait]
impl Storage for FsStorage {
    type Error = FsStorageError;
    type Version = NoVersion;

    /// `base` is ignored: a directory keeps no versions to save against
    async fn save_raw(
        &mut self,
        id: AgentId,
        value: serde_json::Value,
        _base: Option<&NoVersion>,
    ) -> Result<Option<NoVersion>, Self::Error> {
        self.write(id, value, false).await?;
        Ok(None)
    }

    async fn load_raw(
        &self,
        id: AgentId,
    ) -> Result<Loaded<serde_json::Value, NoVersion>, Self::Error> {
        self.read(id).await
    }

    // `save_all_raw`/`load_all_raw` keep the provided loops: with a
    // directory per agent there is no shared file to batch over, and each
    // save already fsyncs only its own directory.
}

impl FsStorage {
    /// Save in [`SEALED_FORMAT`], for a sealed agent
    pub async fn save_sealed_raw(
        &mut self,
        id: AgentId,
        value: serde_json::Value,
    ) -> Result<(), FsStorageError> {
        self.write(id, value, true).await
    }

    async fn write(
        &mut self,
        id: AgentId,
        value: serde_json::Value,
        sealed: bool,
    ) -> Result<(), FsStorageError> {
        let dir = self.agent_dir(id);
        tokio::fs::create_dir_all(&dir).await?;

        let envelope = if sealed {
            serde_json::json!({
                "format": SEALED_FORMAT,
                "sealed": true,
                "state": value,
            })
        } else {
            serde_json::json!({
                "format": FORMAT,
                "state": value,
            })
        };
        // Pretty: these files are meant to be occasionally read (and, for
        // things like hand-tuning a requested capability, edited) by the
        // operator.
        let bytes = serde_json::to_vec_pretty(&envelope)?;

        // lock + set aside a leftover tmp + archive (a hard link) + tmp +
        // fsync + rename + dir fsync: `state.json` always exists, whole. A
        // crash between the link and the rename leaves `state.json` the old
        // value, the same inode as the newest archive, and maybe a
        // `state.json.tmp`, whole if the crash came after its fsync; an
        // in-place edit of `state.json` then would alter that archive too.
        // The next save sets that tmp aside, whole, before it writes its
        // own
        let Some(_lock) = lock_dir(&dir, LOCK_DEADLINE).await? else {
            return Err(FsStorageError::Locked(id));
        };
        if let Some(aside) =
            set_tmp_aside(&dir, chrono::Utc::now().format(STAMP)).await?
        {
            tracing::warn!(
                agent_id = %id,
                aside = %aside.display(),
                "a save cut short left state.json.tmp; set aside"
            );
        }
        let link = self.archive(&dir).await?;
        let tmp = dir.join(TMP_FILE);
        let mut made_tmp = false;
        let replaced = async {
            // `create_new`: never another writer's file
            let mut file = tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)
                .await?;
            made_tmp = true;
            #[cfg(test)]
            if dir.join(FAIL_AFTER_TMP).exists() {
                return Err(std::io::Error::other("injected"));
            }
            tokio::io::AsyncWriteExt::write_all(&mut file, &bytes).await?;
            file.sync_all().await?;
            tokio::fs::rename(&tmp, dir.join(STATE_FILE)).await
        }
        .await;
        if let Err(e) = replaced {
            // Nothing replaced: take back what this save made
            let mine = made_tmp.then_some(&tmp).into_iter().chain(&link);
            for path in mine {
                if let Err(err) = tokio::fs::remove_file(path).await {
                    tracing::error!(
                        agent_id = %id,
                        path = %path.display(),
                        error = %err,
                        "a failed save's own file couldn't be removed"
                    );
                }
            }
            return Err(e.into());
        }
        sync_dir(&dir).await?;
        Ok(())
    }

    async fn read(
        &self,
        id: AgentId,
    ) -> Result<Loaded<serde_json::Value, NoVersion>, FsStorageError> {
        let path = self.agent_dir(id).join(STATE_FILE);
        let bytes = match tokio::fs::read(&path).await {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(AgentNotFound(id).into());
            }
            Err(e) => return Err(e.into()),
        };
        let mut envelope: serde_json::Value = serde_json::from_slice(&bytes)?;
        let found = envelope
            .get("format")
            .and_then(serde_json::Value::as_u64)
            .ok_or(FsStorageError::Envelope("format"))?;
        if found > MAX_FORMAT {
            return Err(FsStorageError::Format { found });
        }
        Ok(Loaded {
            value: envelope
                .get_mut("state")
                .ok_or(FsStorageError::Envelope("state"))?
                .take(),
            base: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(n: u64) -> serde_json::Value {
        serde_json::json!({ "soul": { "name": "test" }, "round": n })
    }

    /// Archive siblings (`state.<ts>.json`) in `dir`.
    fn archives(dir: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| {
                let name = p.file_name().unwrap().to_string_lossy();
                name.starts_with("state.")
                    && name.ends_with(".json")
                    && name != STATE_FILE
            })
            .collect()
    }

    #[tokio::test]
    async fn save_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let mut storage = FsStorage::new(dir.path());
        let id = AgentId::new();

        storage.save_raw(id, value(1), None).await.unwrap();
        assert_eq!(storage.load_raw(id).await.unwrap().value, value(1));
    }

    #[tokio::test]
    async fn missing_agent_is_agent_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let storage = FsStorage::new(dir.path());
        let id = AgentId::new();

        match storage.load_raw(id).await {
            Err(FsStorageError::NotFound(AgentNotFound(missing))) => {
                assert_eq!(missing, id);
            }
            other => panic!("expected AgentNotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn every_overwrite_archives_the_previous_value() {
        let dir = tempfile::tempdir().unwrap();
        let mut storage = FsStorage::new(dir.path());
        let id = AgentId::new();

        // Three rapid saves — likely inside one millisecond, so this also
        // exercises the collision suffix.
        for n in 1..=3 {
            storage.save_raw(id, value(n), None).await.unwrap();
        }
        assert_eq!(storage.load_raw(id).await.unwrap().value, value(3));

        let archived = archives(&storage.agent_dir(id));
        assert_eq!(archived.len(), 2, "one archive per overwrite");
        // Nothing lost: the archived envelopes hold values 1 and 2.
        let mut rounds: Vec<u64> = archived
            .iter()
            .map(|p| {
                let env: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap();
                env["state"]["round"].as_u64().unwrap()
            })
            .collect();
        rounds.sort_unstable();
        assert_eq!(rounds, vec![1, 2]);
    }

    fn ino(path: &Path) -> u64 {
        std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(path).unwrap())
    }

    /// The previous value is in exactly one new archive, byte for byte, and
    /// no archive shares `state.json`'s inode once a save is done
    #[tokio::test]
    async fn each_save_archives_the_previous_bytes_once() {
        let dir = tempfile::tempdir().unwrap();
        let mut storage = FsStorage::new(dir.path());
        let id = AgentId::new();
        let agent_dir = storage.agent_dir(id);
        storage.save_raw(id, value(0), None).await.unwrap();
        assert!(archives(&agent_dir).is_empty(), "nothing to archive");
        for n in 1..=4 {
            let previous = std::fs::read(agent_dir.join(STATE_FILE)).unwrap();
            let before = archives(&agent_dir);
            storage.save_raw(id, value(n), None).await.unwrap();
            let new: Vec<_> = archives(&agent_dir)
                .into_iter()
                .filter(|p| !before.contains(p))
                .collect();
            assert_eq!(new.len(), 1, "one archive per save");
            assert_eq!(std::fs::read(&new[0]).unwrap(), previous);
            let current = ino(&agent_dir.join(STATE_FILE));
            for archive in archives(&agent_dir) {
                assert_ne!(ino(&archive), current, "{}", archive.display());
            }
            assert!(!agent_dir.join(TMP_FILE).exists());
        }
    }

    /// The step a crash can stop after: `state.json` is still there, whole,
    /// and the archive is the same file; the rename then gives them apart
    #[tokio::test]
    async fn state_json_is_never_absent_between_the_link_and_the_rename() {
        let dir = tempfile::tempdir().unwrap();
        let mut storage = FsStorage::new(dir.path());
        let id = AgentId::new();
        let agent_dir = storage.agent_dir(id);
        let state = agent_dir.join(STATE_FILE);
        storage.save_raw(id, value(1), None).await.unwrap();
        let old = std::fs::read(&state).unwrap();

        storage.archive(&agent_dir).await.unwrap();
        assert_eq!(std::fs::read(&state).unwrap(), old, "still there");
        let [archive] = &archives(&agent_dir)[..] else {
            panic!("one archive");
        };
        assert_eq!(ino(archive), ino(&state), "the same inode");
        assert_eq!(storage.load_raw(id).await.unwrap().value, value(1));

        let tmp = agent_dir.join(TMP_FILE);
        std::fs::write(&tmp, b"new").unwrap();
        std::fs::rename(&tmp, &state).unwrap();
        assert_eq!(std::fs::read(archive).unwrap(), old, "the old inode kept");
        assert_ne!(ino(archive), ino(&state));
    }

    /// Aside files in `dir`
    fn asides(dir: &Path) -> Vec<PathBuf> {
        let mut found: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.to_string_lossy().ends_with(".aside"))
            .collect();
        found.sort();
        found
    }

    /// The crash state (`state.json` linked as the newest archive, the cut
    /// save whole in the tmp), then a save: the cut save is kept aside byte
    /// for byte, and the extra archive of the same inode is harmless
    #[tokio::test]
    async fn a_save_after_a_cut_one_sets_its_tmp_aside_whole() {
        let dir = tempfile::tempdir().unwrap();
        let mut storage = FsStorage::new(dir.path());
        let id = AgentId::new();
        let agent_dir = storage.agent_dir(id);
        let state = agent_dir.join(STATE_FILE);
        storage.save_raw(id, value(1), None).await.unwrap();
        let old = std::fs::read(&state).unwrap();
        storage.archive(&agent_dir).await.unwrap();
        let cut = serde_json::to_vec_pretty(
            &serde_json::json!({ "format": 1, "state": value(2) }),
        )
        .unwrap();
        std::fs::write(agent_dir.join(TMP_FILE), &cut).unwrap();

        storage.save_raw(id, value(3), None).await.unwrap();
        assert_eq!(storage.load_raw(id).await.unwrap().value, value(3));
        let [aside] = &asides(&agent_dir)[..] else {
            panic!("one aside");
        };
        assert_eq!(std::fs::read(aside).unwrap(), cut, "the cut save, whole");
        assert!(!agent_dir.join(TMP_FILE).exists());
        let archived = archives(&agent_dir);
        assert_eq!(archived.len(), 2, "the old value, archived twice");
        for a in &archived {
            assert_eq!(std::fs::read(a).unwrap(), old);
            assert_ne!(ino(a), ino(&state));
        }
        assert_eq!(ino(&archived[0]), ino(&archived[1]), "one inode");
    }

    /// A tmp is set aside, never truncated, even with no `state.json`
    /// (the old write order's crash state) and on a name collision
    #[tokio::test]
    async fn a_leftover_tmp_is_never_truncated() {
        let dir = tempfile::tempdir().unwrap();
        let mut storage = FsStorage::new(dir.path());
        let id = AgentId::new();
        let agent_dir = storage.agent_dir(id);
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(agent_dir.join(TMP_FILE), b"first").unwrap();
        storage.save_raw(id, value(1), None).await.unwrap();
        assert!(archives(&agent_dir).is_empty(), "no state.json to archive");

        std::fs::write(agent_dir.join(TMP_FILE), b"second").unwrap();
        std::fs::write(agent_dir.join("state.json.tmp.T.aside"), b"taken")
            .unwrap();
        let aside = set_tmp_aside(&agent_dir, "T").await.unwrap().unwrap();
        assert_eq!(aside, agent_dir.join("state.json.tmp.T-1.aside"));
        let kept: Vec<_> = asides(&agent_dir)
            .iter()
            .map(|p| std::fs::read(p).unwrap())
            .collect();
        assert!(kept.contains(&b"first".to_vec()));
        assert!(kept.contains(&b"second".to_vec()));
        assert!(kept.contains(&b"taken".to_vec()));
        assert_eq!(set_tmp_aside(&agent_dir, "T").await.unwrap(), None);
        assert_eq!(storage.load_raw(id).await.unwrap().value, value(1));
    }

    /// Every file in `dir`, by name
    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    /// A save that fails after making its tmp takes back what it made:
    /// retries leave nothing behind, the value stands, and the lock is free
    #[tokio::test]
    async fn a_failed_save_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let mut storage = FsStorage::new(dir.path());
        let id = AgentId::new();
        let agent_dir = storage.agent_dir(id);
        storage.save_raw(id, value(1), None).await.unwrap();
        storage.save_raw(id, value(2), None).await.unwrap();
        std::fs::write(agent_dir.join(FAIL_AFTER_TMP), b"").unwrap();
        let before = names(&agent_dir);
        for _ in 0..5 {
            assert!(storage.save_raw(id, value(3), None).await.is_err());
            assert_eq!(names(&agent_dir), before);
        }
        assert_eq!(storage.load_raw(id).await.unwrap().value, value(2));
        let lock = lock_dir(&agent_dir, std::time::Duration::ZERO).await;
        assert!(lock.unwrap().is_some(), "the lock was released");

        std::fs::remove_file(agent_dir.join(FAIL_AFTER_TMP)).unwrap();
        storage.save_raw(id, value(3), None).await.unwrap();
        assert_eq!(storage.load_raw(id).await.unwrap().value, value(3));
        assert_eq!(archives(&agent_dir).len(), 2);
        assert!(asides(&agent_dir).is_empty());
    }

    /// A held lock is waited for until the deadline, then given up
    #[tokio::test]
    async fn the_directory_lock_has_a_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let short = std::time::Duration::from_millis(100);
        let held = lock_dir(dir.path(), short).await.unwrap().unwrap();
        assert!(lock_dir(dir.path(), short).await.unwrap().is_none());
        drop(held);
        assert!(lock_dir(dir.path(), short).await.unwrap().is_some());
    }

    /// Two saves of one agent at once both land, one after the other
    #[tokio::test]
    async fn concurrent_saves_of_one_agent_take_turns() {
        let dir = tempfile::tempdir().unwrap();
        let storage = FsStorage::new(dir.path());
        let id = AgentId::new();
        storage.clone().save_raw(id, value(0), None).await.unwrap();
        let mut tasks = Vec::new();
        for n in 1..=8 {
            let mut s = storage.clone();
            tasks.push(tokio::spawn(async move {
                s.save_raw(id, value(n), None).await
            }));
        }
        for t in tasks {
            t.await.unwrap().unwrap();
        }
        let agent_dir = storage.agent_dir(id);
        assert_eq!(archives(&agent_dir).len(), 8, "one archive per save");
        assert!(!agent_dir.join(TMP_FILE).exists());
        assert!(asides(&agent_dir).is_empty());
        let mut rounds: Vec<u64> = archives(&agent_dir)
            .iter()
            .chain([&agent_dir.join(STATE_FILE)])
            .map(|p| {
                let env: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap();
                env["state"]["round"].as_u64().unwrap()
            })
            .collect();
        rounds.sort_unstable();
        assert_eq!(rounds, (0..=8).collect::<Vec<_>>(), "nothing lost");
    }

    #[tokio::test]
    async fn a_same_millisecond_archive_takes_the_next_suffix() {
        let dir = tempfile::tempdir().unwrap();
        let mut storage = FsStorage::new(dir.path());
        let id = AgentId::new();
        let agent_dir = storage.agent_dir(id);
        storage.save_raw(id, value(1), None).await.unwrap();
        // Something already holds the plain name: never overwritten
        std::fs::write(agent_dir.join("state.T.json"), b"older").unwrap();
        for _ in 0..3 {
            archive_as(&agent_dir, "T").await.unwrap();
        }
        let current = std::fs::read(agent_dir.join(STATE_FILE)).unwrap();
        assert_eq!(
            std::fs::read(agent_dir.join("state.T.json")).unwrap(),
            b"older"
        );
        for n in 1..=3 {
            let p = agent_dir.join(format!("state.T-{n}.json"));
            assert_eq!(std::fs::read(&p).unwrap(), current, "{n}");
        }
        assert_eq!(archives(&agent_dir).len(), 4);
    }

    #[tokio::test]
    async fn no_state_json_is_nothing_to_archive() {
        let dir = tempfile::tempdir().unwrap();
        archive_as(dir.path(), "T").await.unwrap();
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn wire_shape_is_the_versioned_envelope() {
        let dir = tempfile::tempdir().unwrap();
        let mut storage = FsStorage::new(dir.path());
        let id = AgentId::new();

        storage.save_raw(id, value(1), None).await.unwrap();
        let raw: serde_json::Value = serde_json::from_slice(
            &std::fs::read(storage.agent_dir(id).join(STATE_FILE)).unwrap(),
        )
        .unwrap();
        assert_eq!(raw["format"], FORMAT);
        assert_eq!(raw["state"], value(1));
    }

    #[tokio::test]
    async fn newer_format_is_rejected_not_half_read() {
        let dir = tempfile::tempdir().unwrap();
        let storage = FsStorage::new(dir.path());
        let id = AgentId::new();

        let agent_dir = storage.agent_dir(id);
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(
            agent_dir.join(STATE_FILE),
            serde_json::json!({ "format": MAX_FORMAT + 1, "state": {} })
                .to_string(),
        )
        .unwrap();

        assert!(matches!(
            storage.load_raw(id).await,
            Err(FsStorageError::Format { found }) if found == MAX_FORMAT + 1
        ));
    }

    #[tokio::test]
    async fn non_envelope_file_is_an_envelope_error() {
        let dir = tempfile::tempdir().unwrap();
        let storage = FsStorage::new(dir.path());
        let id = AgentId::new();

        let agent_dir = storage.agent_dir(id);
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(agent_dir.join(STATE_FILE), b"{\"soul\": {}}").unwrap();

        assert!(matches!(
            storage.load_raw(id).await,
            Err(FsStorageError::Envelope("format"))
        ));
    }

    #[tokio::test]
    async fn bulk_save_reports_every_id() {
        let dir = tempfile::tempdir().unwrap();
        let mut storage = FsStorage::new(dir.path());
        let a = AgentId::new();
        let b = AgentId::new();

        storage
            .save_all_raw(
                [(a, value(1), None), (b, value(2), None)].into_iter(),
            )
            .await
            .unwrap();
        assert_eq!(storage.load_raw(a).await.unwrap().value, value(1));
        assert_eq!(storage.load_raw(b).await.unwrap().value, value(2));
    }

    #[tokio::test]
    async fn a_sealed_agent_round_trips_in_format_3_and_old_loaders_refuse_it()
    {
        let dir = tempfile::tempdir().unwrap();
        let mut storage = FsStorage::new(dir.path());
        let id = AgentId::new();
        storage.save_sealed_raw(id, value(1)).await.unwrap();
        assert_eq!(storage.load_raw(id).await.unwrap().value, value(1));
        let raw: serde_json::Value = serde_json::from_slice(
            &std::fs::read(storage.agent_dir(id).join(STATE_FILE)).unwrap(),
        )
        .unwrap();
        assert_eq!(raw["format"], SEALED_FORMAT);
        assert_eq!(raw["sealed"], true);
        // 0.146's check refused anything past format 1, and 0.147-0.157's
        // anything past 2: neither loads it
        let found = raw["format"].as_u64().unwrap();
        assert!(found > 2, "a 0.157 loader rejects format {found}");

        // An unsealed save is format 1, byte for byte as before
        let other = AgentId::new();
        storage.save_raw(other, value(1), None).await.unwrap();
        let bytes =
            std::fs::read(storage.agent_dir(other).join(STATE_FILE)).unwrap();
        let before = serde_json::to_vec_pretty(
            &serde_json::json!({ "format": 1, "state": value(1) }),
        )
        .unwrap();
        assert_eq!(bytes, before);
    }
}
