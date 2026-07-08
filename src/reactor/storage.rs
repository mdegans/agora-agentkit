//! [`FsStorage`] — a directory-per-agent [`Storage`] backend.
//!
//! [`Storage`]: super::Storage

use std::path::{Path, PathBuf};

use crate::ids::AgentId;

use super::backend::{AgentNotFound, Storage};

/// The envelope format [`FsStorage`] writes. Bump on layout change; `load`
/// rejects anything newer so an old binary never half-reads a new layout.
const FORMAT: u64 = 1;

/// The value file inside an agent's directory.
const STATE_FILE: &str = "state.json";

/// A [`Storage`] over `<root>/<agent_id>/state.json`, agent-kind-agnostic.
///
/// Values are wrapped in a `{format, state}` envelope and written
/// atomically (tmp + rename + directory fsync). **Every overwrite archives
/// the previous value** to a `state.<utc-ms>.json` sibling first — retention
/// is deliberately infinite (rollback + transcript history; compress cold
/// archives externally if a deployment ever cares).
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

    /// Move `state.json` aside to `state.<utc-ms>.json`, suffixing `-N` on
    /// a same-millisecond collision (retries, tests).
    async fn archive(&self, dir: &Path) -> Result<(), std::io::Error> {
        let current = dir.join(STATE_FILE);
        if !tokio::fs::try_exists(&current).await? {
            return Ok(());
        }
        let ts = chrono::Utc::now().format("%Y%m%dT%H%M%S%3fZ");
        let mut candidate = dir.join(format!("state.{ts}.json"));
        let mut n = 0u32;
        while tokio::fs::try_exists(&candidate).await? {
            n += 1;
            candidate = dir.join(format!("state.{ts}-{n}.json"));
        }
        tokio::fs::rename(&current, &candidate).await
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
        "unsupported envelope format {found} (this binary reads ≤ {FORMAT})"
    )]
    Format { found: u64 },
    #[error("not an FsStorage envelope: missing {0}")]
    Envelope(&'static str),
}

impl crate::reactor::RetryAfter for FsStorageError {}

#[async_trait::async_trait]
impl Storage for FsStorage {
    type Error = FsStorageError;

    async fn save_raw(
        &mut self,
        id: AgentId,
        value: serde_json::Value,
    ) -> Result<(), Self::Error> {
        let dir = self.agent_dir(id);
        tokio::fs::create_dir_all(&dir).await?;

        let envelope = serde_json::json!({
            "format": FORMAT,
            "state": value,
        });
        // Pretty: these files are meant to be occasionally read (and, for
        // things like hand-tuning a requested capability, edited) by the
        // operator.
        let bytes = serde_json::to_vec_pretty(&envelope)?;

        // tmp + fsync + archive + rename + dir fsync: a crash at any point
        // leaves either the old value or the new one, never a torn file.
        let tmp = dir.join("state.json.tmp");
        {
            let mut file = tokio::fs::File::create(&tmp).await?;
            tokio::io::AsyncWriteExt::write_all(&mut file, &bytes).await?;
            file.sync_all().await?;
        }
        self.archive(&dir).await?;
        tokio::fs::rename(&tmp, dir.join(STATE_FILE)).await?;
        tokio::fs::File::open(&dir).await?.sync_all().await?;
        Ok(())
    }

    async fn load_raw(
        &self,
        id: AgentId,
    ) -> Result<serde_json::Value, Self::Error> {
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
        if found > FORMAT {
            return Err(FsStorageError::Format { found });
        }
        Ok(envelope
            .get_mut("state")
            .ok_or(FsStorageError::Envelope("state"))?
            .take())
    }

    // `save_all_raw`/`load_all_raw` keep the provided loops: with a
    // directory per agent there is no shared file to batch over, and each
    // save already fsyncs only its own directory.
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

        storage.save_raw(id, value(1)).await.unwrap();
        assert_eq!(storage.load_raw(id).await.unwrap(), value(1));
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
            storage.save_raw(id, value(n)).await.unwrap();
        }
        assert_eq!(storage.load_raw(id).await.unwrap(), value(3));

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

    #[tokio::test]
    async fn wire_shape_is_the_versioned_envelope() {
        let dir = tempfile::tempdir().unwrap();
        let mut storage = FsStorage::new(dir.path());
        let id = AgentId::new();

        storage.save_raw(id, value(1)).await.unwrap();
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
            serde_json::json!({ "format": FORMAT + 1, "state": {} })
                .to_string(),
        )
        .unwrap();

        assert!(matches!(
            storage.load_raw(id).await,
            Err(FsStorageError::Format { found }) if found == FORMAT + 1
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
            .save_all_raw([(a, value(1)), (b, value(2))].into_iter())
            .await
            .unwrap();
        assert_eq!(storage.load_raw(a).await.unwrap(), value(1));
        assert_eq!(storage.load_raw(b).await.unwrap(), value(2));
    }
}
