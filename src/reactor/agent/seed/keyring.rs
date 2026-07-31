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
use crate::envelope::EncryptionSecretKey;
use crate::ids::AgentId;

/// Resolves an [`AgentId`] to its Ed25519 [`SigningKey`]
pub trait Keyring: Send + Sync {
    /// The signing key for `id`, or `None` if this ring doesn't hold one
    fn signing_key(&self, id: AgentId) -> Option<SigningKey>;

    /// The X25519 encryption key for `id`, or `None` if this ring
    /// doesn't hold one (the agent then sends/receives server-mode
    /// only). Default `None` so existing rings keep compiling.
    fn encryption_key(&self, _id: AgentId) -> Option<EncryptionSecretKey> {
        None
    }
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
    /// X25519 encryption key, 64 hex chars. Absent for agents
    /// provisioned before E2EE messaging;
    /// [`FsKeyring::ensure_encryption_key`] backfills it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    encryption_key: Option<String>,
}

impl zeroize::Zeroize for Secrets {
    fn zeroize(&mut self) {
        self.signing_key.zeroize();
        self.encryption_key.zeroize();
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
            // Backfilled by `ensure_encryption_key` at first session.
            encryption_key: None,
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

    /// Load `id`'s X25519 encryption key, generating and persisting one
    /// if the secrets file exists but has none yet (backfill for agents
    /// provisioned before E2EE messaging). Errors if `id` has no secrets
    /// file at all — the signing identity must exist first.
    ///
    /// Unlike signing keys, regenerating an encryption key is a
    /// survivable rotation (old messages keep their stored wraps but
    /// become locally undecryptable), which is why this backfills
    /// rather than refusing.
    pub fn ensure_encryption_key(
        &self,
        id: AgentId,
    ) -> std::io::Result<EncryptionSecretKey> {
        use std::io::Write;
        use zeroize::Zeroize;

        let path = self.secrets_path(id);
        let raw = zeroize::Zeroizing::new(std::fs::read_to_string(&path)?);
        let mut secrets: zeroize::Zeroizing<Secrets> = zeroize::Zeroizing::new(
            serde_json::from_str(&raw).map_err(std::io::Error::other)?,
        );

        if let Some(hex_key) = &secrets.encryption_key {
            return crate::envelope::encryption_secret_from_hex(hex_key)
                .map_err(std::io::Error::other);
        }

        let (secret, _) = crate::envelope::generate_encryption_keypair();
        secrets.encryption_key =
            Some(crate::envelope::encryption_secret_to_hex(&secret));
        let mut json = serde_json::to_string_pretty(&*secrets)
            .map_err(std::io::Error::other)?;

        // Atomic replace: write a 0600 sibling, fsync, rename over.
        let tmp = path.with_extension("json.tmp");
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let result = options
            .open(&tmp)
            .and_then(|mut file| {
                file.write_all(json.as_bytes())?;
                file.sync_all()
            })
            .and_then(|_| std::fs::rename(&tmp, &path));
        json.zeroize();
        result.map(|_| secret)
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

    fn encryption_key(&self, id: AgentId) -> Option<EncryptionSecretKey> {
        let raw = zeroize::Zeroizing::new(
            std::fs::read_to_string(self.secrets_path(id)).ok()?,
        );
        let secrets: zeroize::Zeroizing<Secrets> =
            zeroize::Zeroizing::new(serde_json::from_str(&raw).ok()?);
        crate::envelope::encryption_secret_from_hex(
            secrets.encryption_key.as_deref()?,
        )
        .map_err(|e| {
            tracing::warn!("bad encryption key for {id}: {e}");
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
    fn fs_ring_backfills_and_round_trips_encryption_key() {
        let dir = tempfile::tempdir().unwrap();
        let ring = FsKeyring::new(dir.path());
        let id = AgentId::new();
        let (key, _) = generate_keypair();
        ring.insert(id, &key).unwrap();

        // Pre-E2EE secrets file: no encryption key yet.
        assert!(Keyring::encryption_key(&ring, id).is_none());

        // Backfill generates and persists…
        let generated = ring.ensure_encryption_key(id).unwrap();
        // …idempotently…
        let again = ring.ensure_encryption_key(id).unwrap();
        assert_eq!(generated.to_bytes(), again.to_bytes());
        // …and the trait accessor sees it, with the signing key intact.
        let loaded = Keyring::encryption_key(&ring, id).unwrap();
        assert_eq!(generated.to_bytes(), loaded.to_bytes());
        assert!(ring.signing_key(id).is_some());
    }

    #[test]
    fn ensure_encryption_key_requires_signing_identity() {
        let dir = tempfile::tempdir().unwrap();
        let ring = FsKeyring::new(dir.path());
        assert!(ring.ensure_encryption_key(AgentId::new()).is_err());
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
