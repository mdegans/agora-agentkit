//! [`Keyring`] — per-process signing-key provider for [`SeedContext`].
//!
//! Keys ride [`Agent::Context`], never [`Agent::State`] — the state
//! serialization plane goes to [`Storage`], and backups of it must not carry
//! secrets.
//!
//! [`Agent::Context`]: crate::reactor::Agent::Context
//! [`Agent::State`]: crate::reactor::Agent::State
//! [`SeedContext`]: super::SeedContext
//! [`Storage`]: crate::reactor::Storage

use crate::crypto::SigningKey;
use crate::ids::AgentId;

/// Resolves an [`AgentId`] to its Ed25519 [`SigningKey`]
pub trait Keyring: Send + Sync {
    /// The signing key for `id`, or `None` if this ring doesn't hold one
    fn signing_key(&self, id: AgentId) -> Option<SigningKey>;
}

/// The per-agent secrets file inside `<dir>/<agent_id>/`.
const SECRETS_FILE: &str = "secrets.json";

/// A [`Keyring`] over `<dir>/<agent_id>/secrets.json` — the secrets half of
/// the `FsStorage` layout, split at the top level so the two trees can live
/// on separate volumes. Files are `0600` in `0700` directories; lookups are
/// per-call lazy reads through zeroized buffers.
#[derive(Debug, Clone)]
pub struct FsKeyring {
    dir: std::path::PathBuf,
}

/// Wire shape of `secrets.json`. A struct (not a bare hex file) so future
/// per-agent secrets land without a layout migration.
#[derive(serde::Serialize, serde::Deserialize)]
struct Secrets {
    /// Ed25519 signing key, 64 hex chars.
    signing_key: String,
}

impl zeroize::Zeroize for Secrets {
    fn zeroize(&mut self) {
        self.signing_key.zeroize();
    }
}

impl FsKeyring {
    /// A ring rooted at `dir` (the secrets path).
    pub fn new(dir: impl Into<std::path::PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// `id`'s secrets file.
    fn secrets_path(&self, id: AgentId) -> std::path::PathBuf {
        self.dir.join(id.to_string()).join(SECRETS_FILE)
    }

    /// Write `key` for `id` (registration flows). **Refuses to overwrite**
    /// (`ErrorKind::AlreadyExists`): clobbering a signing key orphans the
    /// server-side identity, so replacing one is a deliberate, manual act.
    pub fn insert(&self, id: AgentId, key: &SigningKey) -> std::io::Result<()> {
        use std::io::Write;
        use zeroize::Zeroize;

        let agent_dir = self.dir.join(id.to_string());
        std::fs::create_dir_all(&agent_dir)?;
        let secrets = zeroize::Zeroizing::new(Secrets {
            signing_key: crate::crypto::signing_key_to_hex(key),
        });
        let mut json = serde_json::to_string_pretty(&*secrets)
            .map_err(std::io::Error::other)?;

        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            options.mode(0o600);
            std::fs::set_permissions(
                &agent_dir,
                std::fs::Permissions::from_mode(0o700),
            )?;
        }
        let result =
            options.open(self.secrets_path(id)).and_then(|mut file| {
                file.write_all(json.as_bytes())?;
                file.sync_all()
            });
        json.zeroize();
        result
    }
}

impl Keyring for FsKeyring {
    fn signing_key(&self, id: AgentId) -> Option<SigningKey> {
        let raw = zeroize::Zeroizing::new(
            std::fs::read_to_string(self.secrets_path(id)).ok()?,
        );
        let secrets: zeroize::Zeroizing<Secrets> = zeroize::Zeroizing::new(
            serde_json::from_str(&raw)
                .map_err(|e| {
                    tracing::warn!("unparseable secrets for {id}: {e}");
                })
                .ok()?,
        );
        crate::crypto::signing_key_from_hex(&secrets.signing_key)
            .map_err(|e| {
                tracing::warn!("bad signing key for {id}: {e}");
            })
            .ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::generate_keypair;

    use std::collections::HashMap;

    /// The in-memory case: any map loaded at process start.
    impl Keyring for HashMap<AgentId, SigningKey> {
        fn signing_key(&self, id: AgentId) -> Option<SigningKey> {
            self.get(&id).cloned()
        }
    }

    #[test]
    fn hashmap_ring_resolves() {
        let id = AgentId::new();
        let (key, _) = generate_keypair();
        let ring: HashMap<AgentId, SigningKey> =
            [(id, key.clone())].into_iter().collect();

        assert_eq!(
            ring.signing_key(id).map(|k| k.to_bytes()),
            Some(key.to_bytes())
        );
        assert!(ring.signing_key(AgentId::new()).is_none());
    }

    #[test]
    fn fs_ring_insert_and_resolve_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let ring = FsKeyring::new(dir.path());
        let id = AgentId::new();
        let (key, _) = generate_keypair();

        ring.insert(id, &key).unwrap();
        assert_eq!(
            ring.signing_key(id).map(|k| k.to_bytes()),
            Some(key.to_bytes())
        );
        assert!(ring.signing_key(AgentId::new()).is_none());
    }

    #[test]
    fn fs_ring_refuses_to_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let ring = FsKeyring::new(dir.path());
        let id = AgentId::new();
        let (key, _) = generate_keypair();

        ring.insert(id, &key).unwrap();
        let err = ring.insert(id, &key).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn fs_ring_corrupt_secrets_is_none_not_panic() {
        let dir = tempfile::tempdir().unwrap();
        let ring = FsKeyring::new(dir.path());
        let id = AgentId::new();

        let agent_dir = dir.path().join(id.to_string());
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(agent_dir.join(SECRETS_FILE), b"not json").unwrap();
        assert!(ring.signing_key(id).is_none());

        // Parseable JSON, bad hex.
        std::fs::write(
            agent_dir.join(SECRETS_FILE),
            b"{\"signing_key\": \"deadbeef\"}",
        )
        .unwrap();
        assert!(ring.signing_key(id).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn fs_ring_writes_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let ring = FsKeyring::new(dir.path());
        let id = AgentId::new();
        let (key, _) = generate_keypair();
        ring.insert(id, &key).unwrap();

        let agent_dir = dir.path().join(id.to_string());
        let file_mode = std::fs::metadata(agent_dir.join(SECRETS_FILE))
            .unwrap()
            .permissions()
            .mode();
        let dir_mode =
            std::fs::metadata(&agent_dir).unwrap().permissions().mode();
        assert_eq!(file_mode & 0o777, 0o600);
        assert_eq!(dir_mode & 0o777, 0o700);
    }
}
