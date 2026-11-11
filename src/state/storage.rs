//! [`Mirrored`]: a local [`Storage`] whose saves are also sealed and
//! chained for every enrolled agent, and queued for upload for one that
//! [`uploads`](Enrolment::uploads).
//!
//! Under `<agent_dir>/sealed/`:
//!
//! - `chained`, `refused`: markers. Only a chained, unrefused agent is
//!   sealed. `chained` was `enrolled` before 0.158, when it also meant
//!   "uploads"; renamed so no older runner seals or uploads an agent that
//!   is only chained ([`AgentSeal::migrate_marker`])
//! - `lock`: held across a save's three steps ([`SealLock`])
//! - `head.json`: the local head ([`HeadFile`])
//! - `blobs/<sha256>`: every sealed blob an entry here names, sent or not,
//!   so each version opens from here whatever Agora holds; only unnamed
//!   ones go ([`AgentSeal::remove_unnamed_blobs`]). Before 0.158 the blobs
//!   of sent versions were pruned: those versions are sealed on Agora only
//!   (and in plaintext in `FsStorage`'s archives)
//! - `outbox/<ord>.json`: committed entries not yet uploaded, `<ord>.tmp`
//!   one being written, `<ord>-<hash>.abandoned` one whose local write
//!   never came, `<ord>-<hash>.displaced` one set aside by
//!   [`AgentSeal::rebase_onto`] (kept, with its blob)
//! - `sent/<ord>.json`: uploaded entries
//! - `seal_failed.json`: a local write that went ahead unsealed
//!   ([`SealFailed`])
//!
//! A save is crash-safe in this order: seal into `outbox/<ord>.tmp`, the
//! local write, then commit (the rename and `head.json`).
//! [`AgentSeal::recover`] finishes a commit a crash interrupted. A seal
//! that fails never stops the local write. Nothing outside `sealed/` is
//! ever deleted, and inside it only blobs no record names.
//!
//! What Agora can see besides sizes (to a bucket) and times: whether a
//! blob is a keyframe or a delta, which says a little about how much a
//! state changed, and the absence of an entry for a session whose state
//! didn't change at all, since an unchanged save is not sealed.
//!
//! [`Storage`]: crate::reactor::Storage

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use super::chain::{
    BlobEncoding, BlobRef, GenesisOrigin, STATE_ENTRY_VERSION,
    SignedStateEntry, StateEntry,
};
use super::codec::{self, Encoded, Keyframe, KeyframePolicy};
use super::crypto::{
    BlobContext, EntryBlobError, StateKey, open_entry_blob, seal_blob,
};
use super::verify::HeadRef;
use crate::crypto::SigningKey;
use crate::enums::StateEntryKind;
use crate::govlog::{Sha256Hex, canonical_json};
use crate::ids::AgentId;
use crate::reactor::{AgentNotFound, FsStorage, Loaded, Storage};

/// The sealed directory, in an agent's
pub const SEALED_DIR: &str = "sealed";
/// The marker of a chained agent, in its `sealed/`
pub const CHAINED: &str = "chained";
/// The marker before 0.158, read only by [`AgentSeal::migrate_marker`]
const LEGACY_ENROLLED: &str = "enrolled";
const REFUSED: &str = "refused";
const LOCK: &str = "lock";
const HEAD: &str = "head.json";
const OUTBOX: &str = "outbox";
const SENT: &str = "sent";
const BLOBS: &str = "blobs";
const SEAL_FAILED: &str = "seal_failed.json";
/// A runner's set-aside lines, each in a directory of its own
const LOST: &str = "lost";

/// Blobs kept while a fork holds pruning, past which it alerts
pub const BLOB_ALERT: usize = 256;

/// A [`Storage`] that keeps each agent in a directory of its own
pub trait AgentDir {
    fn agent_dir(&self, id: AgentId) -> PathBuf;
}

impl AgentDir for FsStorage {
    fn agent_dir(&self, id: AgentId) -> PathBuf {
        FsStorage::agent_dir(self, id)
    }
}

/// A local [`Storage`] that can mark a chained agent's state so binaries
/// before 0.158 refuse it instead of running it unsealed
#[async_trait::async_trait]
pub trait MarkedLocal: Storage {
    async fn save_marked(
        &mut self,
        id: AgentId,
        value: serde_json::Value,
    ) -> Result<(), Self::Error>;
}

#[async_trait::async_trait]
impl MarkedLocal for FsStorage {
    /// [`SEALED_FORMAT`](crate::reactor::storage::SEALED_FORMAT)
    async fn save_marked(
        &mut self,
        id: AgentId,
        value: serde_json::Value,
    ) -> Result<(), Self::Error> {
        self.save_sealed_raw(id, value).await
    }
}

/// What an agent's entries are signed and sealed with
pub struct SealKeys {
    pub signing: SigningKey,
    pub state_key: StateKey,
    pub key_gen: u32,
}

impl std::fmt::Debug for SealKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SealKeys {{ key_gen: {}, [REDACTED] }}", self.key_gen)
    }
}

/// The runner's say on who uploads and with what keys. Whether an agent
/// is sealed is `sealed/chained`'s alone
pub trait Enrolment: Send + Sync {
    /// Whether the agent's chain is uploaded to Agora (the consent ledger).
    /// Decides only [`Outbox::notify`] and [`AgoraOutbox::drain`], never the
    /// seal or the load's check
    ///
    /// [`AgoraOutbox::drain`]: super::outbox::AgoraOutbox::drain
    fn uploads(&self, id: AgentId) -> bool;
    /// Its keys, from the runner's own secrets
    fn keys(&self, id: AgentId) -> Option<SealKeys>;
}

impl<E: Enrolment> Enrolment for std::sync::Arc<E> {
    fn uploads(&self, id: AgentId) -> bool {
        (**self).uploads(id)
    }

    fn keys(&self, id: AgentId) -> Option<SealKeys> {
        (**self).keys(id)
    }
}

/// Told when an entry is committed for an agent that
/// [`uploads`](Enrolment::uploads), to wake a drainer. Never does network
/// work itself: it is called under the reactor's storage lock
pub trait Outbox: Send + Sync {
    fn notify(&self, _id: AgentId) {}
}

impl Outbox for () {}

/// The current keyframe, for the next delta
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyframeRef {
    /// The keyframe's own entry, which its blob is bound to
    pub entry: StateEntry,
    /// Deltas written against it
    pub deltas: u32,
}

impl KeyframeRef {
    pub fn entry_hash(&self) -> Sha256Hex {
        self.entry.entry_hash()
    }

    pub fn blob_sha256(&self) -> Option<Sha256Hex> {
        self.entry.blob.as_ref().map(|b| b.sha256)
    }
}

/// `sealed/head.json`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeadFile {
    pub head: HeadRef,
    /// SHA-256 of the last sealed state's plaintext. **Local only**: never
    /// uploaded, since no public value is derived from plaintext
    pub plaintext_sha256: Option<Sha256Hex>,
    pub keyframe: Option<KeyframeRef>,
    /// The next outbox item's number
    pub next_ord: u64,
    /// Heads a later save forked from, oldest first
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub displaced: Vec<HeadRef>,
    /// The head's `created_at` (µs): a child's is never earlier
    #[serde(default)]
    pub created_at: i64,
}

/// What `sealed/` holds back from the server, for an alert
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize,
)]
pub struct Held {
    /// Committed entries not yet uploaded
    pub outbox_items: usize,
    /// Blob files, and their bytes
    pub blobs: usize,
    pub blob_bytes: u64,
    /// Forks not yet resolved
    pub displaced: usize,
}

/// An entry in the outbox, and how it moves the head
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxItem {
    pub ord: u64,
    pub entry: SignedStateEntry,
    /// Local only, as in [`HeadFile`]; `None` keeps the head's
    pub plaintext_sha256: Option<Sha256Hex>,
    pub keyframe_after: Option<KeyframeRef>,
    /// The head it forked from, when its parent was not the head
    pub displaced: Option<HeadRef>,
}

/// Something sealed failed
#[derive(Debug, thiserror::Error)]
pub enum SealError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("seal: {0}")]
    Seal(#[from] crate::envelope::EnvelopeError),
    #[error("enrolled, but there is no sealed head")]
    NoHead,
    #[error("no sealing keys for this agent")]
    NoKeys,
    #[error("refused: only an operator can clear it")]
    Refused,
    #[error("a genesis needs an empty chain, and anything else a parent")]
    BadParent,
    #[error("the outbox item to commit is not the one prepared")]
    Raced,
    /// A forked save's parent is in no record here, so its time can't be
    /// checked
    #[error("the parent entry is not in the head, keyframe or outbox")]
    ParentNotFound,
}

/// Why a local write went ahead unsealed, without content
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SealFailureKind {
    Io,
    Json,
    Seal,
    NoHead,
    NoKeys,
    BadParent,
    Raced,
    /// Not a failure: the agent is refused
    Refused,
    /// Not a failure: the agent opted out after enrolling. Written before
    /// 0.158, when opting out stopped the seal; never written now, but an
    /// old `seal_failed.json` still reads
    OptedOut,
}

impl SealError {
    /// Its kind, for [`SealFailed`]
    pub fn kind(&self) -> SealFailureKind {
        match self {
            Self::Io(_) => SealFailureKind::Io,
            Self::Json(_) => SealFailureKind::Json,
            Self::Seal(_) => SealFailureKind::Seal,
            Self::NoHead => SealFailureKind::NoHead,
            Self::NoKeys => SealFailureKind::NoKeys,
            Self::BadParent | Self::ParentNotFound => {
                SealFailureKind::BadParent
            }
            Self::Raced => SealFailureKind::Raced,
            Self::Refused => SealFailureKind::Refused,
        }
    }
}

/// `sealed/seal_failed.json`: the local write went ahead unsealed. The
/// next seal of a state clears it
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealFailed {
    pub at: DateTime<Utc>,
    /// Local only, as in [`HeadFile`]
    pub local_plaintext_sha256: Sha256Hex,
    pub error: SealFailureKind,
}

/// What [`AgentSeal::prepare`] adds
#[derive(Clone)]
pub struct Append<'a> {
    pub kind: StateEntryKind,
    /// Uncompressed
    pub plaintext: &'a [u8],
    /// For a snapshot, the plaintext's hash, to become the head's
    pub plaintext_sha256: Option<Sha256Hex>,
    pub state_at: DateTime<Utc>,
    /// `None` only for the genesis
    pub parent: Option<HeadRef>,
}

impl std::fmt::Debug for Append<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Append")
            .field("kind", &self.kind)
            .field("plaintext", &"[REDACTED]")
            .field("state_at", &self.state_at)
            .field("parent", &self.parent)
            .finish_non_exhaustive()
    }
}

/// A sealed entry written to `outbox/<ord>.tmp`, not yet committed
#[derive(Debug, Clone)]
pub struct Prepared {
    pub item: OutboxItem,
}

/// What [`AgentSeal::recover`] found
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recovery {
    NotEnrolled,
    /// Enrolled with no `head.json`
    NoHead,
    /// The local state is the head's
    Intact(HeadRef),
    /// The local state was an uncommitted item's; now committed
    Committed(HeadRef),
    /// There is no local state
    NoLocalState(HeadRef),
    /// The local state is one written unsealed (`seal_failed.json`): hold
    /// and alert, or reseal; it is not a loss
    SealFailed(HeadRef, SealFailureKind),
    /// The local state matches neither the head nor a pending item
    Mismatch(HeadRef),
    /// `sealed/` couldn't be read; the local state loaded without a base
    Unreadable(SealFailureKind),
}

/// What [`AgentSeal::migrate_marker`] did
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerMigration {
    /// `enrolled` is now `chained`
    Migrated,
    /// Already `chained`, with no `enrolled`
    Already,
    /// Neither marker
    NotEnrolled,
}

/// The per-agent advisory lock on `sealed/lock`, held across a save's
/// three steps. Released on drop
#[derive(Debug)]
pub struct SealLock(#[allow(dead_code)] std::fs::File);

/// One agent's `sealed/` directory
#[derive(Debug, Clone)]
pub struct AgentSeal {
    agent_id: AgentId,
    dir: PathBuf,
}

fn sha256(bytes: &[u8]) -> Sha256Hex {
    Sha256Hex::from(<[u8; 32]>::from(Sha256::digest(bytes)))
}

/// The local-only hash of a state's sealed plaintext, for a value as
/// loaded
pub fn plaintext_hash(state: &serde_json::Value) -> Sha256Hex {
    sha256(&codec::plaintext(state))
}

/// `state` as a load will read it back: serialized and parsed again. A
/// float need not survive the round trip, so a save hashes and seals
/// this, not the value in memory
pub fn as_reloaded(
    state: &serde_json::Value,
) -> Result<serde_json::Value, serde_json::Error> {
    serde_json::from_slice(&Zeroizing::new(serde_json::to_vec(state)?))
}

async fn exists(path: &Path) -> std::io::Result<bool> {
    tokio::fs::try_exists(path).await
}

/// The blobs an item names: its entry's, and its keyframe's
fn item_blobs(item: &OutboxItem) -> impl Iterator<Item = Sha256Hex> + '_ {
    let entry = item.entry.entry.blob.as_ref().map(|b| b.sha256);
    let keyframe = item.keyframe_after.as_ref().and_then(|k| k.blob_sha256());
    entry.into_iter().chain(keyframe)
}

/// The blobs every entry record under `dir` names, at any depth: a file
/// holding an [`OutboxItem`], a [`HeadFile`] (its keyframe), or a list of
/// items (a runner's record of what it set aside). Anything else there is
/// not a record
async fn lost_blobs(dir: &Path) -> Result<HashSet<Sha256Hex>, SealError> {
    #[derive(Deserialize)]
    struct Listed {
        items: Vec<OutboxItem>,
    }
    let mut named = HashSet::new();
    let mut dirs = vec![dir.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        let mut rd = match tokio::fs::read_dir(&dir).await {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        while let Some(e) = rd.next_entry().await? {
            let kind = e.file_type().await?;
            if kind.is_dir() {
                dirs.push(e.path());
                continue;
            }
            if !kind.is_file() {
                continue;
            }
            let bytes = tokio::fs::read(e.path()).await?;
            if let Ok(item) = serde_json::from_slice::<OutboxItem>(&bytes) {
                named.extend(item_blobs(&item));
            } else if let Ok(head) = serde_json::from_slice::<HeadFile>(&bytes)
            {
                named.extend(head.keyframe.and_then(|k| k.blob_sha256()));
            } else if let Ok(listed) = serde_json::from_slice::<Listed>(&bytes)
            {
                named.extend(listed.items.iter().flat_map(item_blobs));
            }
        }
    }
    Ok(named)
}

/// tmp, fsync, rename, directory fsync
pub(crate) async fn write_atomic(
    path: &Path,
    bytes: &[u8],
) -> std::io::Result<()> {
    let dir = path.parent().expect("a file in a directory");
    tokio::fs::create_dir_all(dir).await?;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".writing");
    let tmp = PathBuf::from(tmp);
    {
        let mut file = tokio::fs::File::create(&tmp).await?;
        tokio::io::AsyncWriteExt::write_all(&mut file, bytes).await?;
        file.sync_all().await?;
    }
    tokio::fs::rename(&tmp, path).await?;
    sync_dir(dir).await
}

async fn sync_dir(dir: &Path) -> std::io::Result<()> {
    tokio::fs::File::open(dir).await?.sync_all().await
}

fn ord_name(ord: u64, ext: &str) -> String {
    format!("{ord:020}.{ext}")
}

/// `<ord>-<entry hash>.<ext>`: an item set aside, never overwritten
fn set_aside(item: &OutboxItem, ext: &str) -> String {
    format!("{:020}-{}.{ext}", item.ord, item.entry.entry_hash())
}

/// Opening the keyframe, compressing and sealing, on a blocking thread
struct SealJob {
    agent_id: AgentId,
    key_gen: u32,
    seq: u64,
    prev_hash: Option<Sha256Hex>,
    state_key: StateKey,
    policy: KeyframePolicy,
    plaintext: Zeroizing<Vec<u8>>,
    /// The current keyframe and its sealed bytes
    keyframe: Option<(KeyframeRef, Vec<u8>)>,
}

impl SealJob {
    fn run(self) -> (BlobEncoding, Vec<u8>) {
        let keyframe_plaintext =
            self.keyframe.as_ref().and_then(|(kf, sealed)| {
                let compressed =
                    open_entry_blob(&self.state_key, &kf.entry, sealed).ok()?;
                codec::decompress(&compressed, None).ok()
            });
        let encoded = match (&self.keyframe, &keyframe_plaintext) {
            (Some((kf, _)), Some(pt)) => self.policy.encode(
                &self.plaintext,
                Some(Keyframe {
                    plaintext: pt,
                    deltas: kf.deltas,
                }),
            ),
            _ => Encoded::Keyframe(codec::compress(&self.plaintext, None)),
        };
        let (encoding, compressed) = match encoded {
            Encoded::Keyframe(c) => (BlobEncoding::Keyframe, c),
            Encoded::Delta(c) => (
                BlobEncoding::Delta {
                    base: self
                        .keyframe
                        .as_ref()
                        .expect("a delta has a keyframe")
                        .0
                        .entry_hash(),
                },
                c,
            ),
        };
        let sealed = seal_blob(
            &self.state_key,
            &BlobContext {
                agent_id: self.agent_id,
                key_gen: self.key_gen,
                seq: self.seq,
                prev_hash: self.prev_hash.as_ref(),
                encoding: &encoding,
            },
            &compressed,
        );
        (encoding, sealed)
    }
}

impl AgentSeal {
    /// `<agent_dir>/sealed`
    pub fn new(agent_dir: &Path, agent_id: AgentId) -> Self {
        Self {
            agent_id,
            dir: agent_dir.join(SEALED_DIR),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn agent_id(&self) -> AgentId {
        self.agent_id
    }

    /// Take the agent's lock, waiting for another writer to finish
    pub async fn lock(&self) -> Result<SealLock, SealError> {
        tokio::fs::create_dir_all(&self.dir).await?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.dir.join(LOCK))?;
        let file =
            tokio::task::spawn_blocking(move || file.lock().map(|()| file))
                .await
                .map_err(std::io::Error::other)??;
        Ok(SealLock(file))
    }

    /// Whether `sealed/chained` exists. The pre-0.158 `sealed/enrolled`
    /// counts only once [`migrate_marker`](Self::migrate_marker) has run
    pub async fn is_enrolled(&self) -> std::io::Result<bool> {
        exists(&self.dir.join(CHAINED)).await
    }

    /// Whether the pre-0.158 `sealed/enrolled` is still here, for a dry
    /// run of [`migrate_marker`](Self::migrate_marker). Read-only
    pub async fn has_legacy_marker(&self) -> std::io::Result<bool> {
        exists(&self.dir.join(LEGACY_ENROLLED)).await
    }

    /// Replace a pre-0.158 `sealed/enrolled` with `sealed/chained`: the new
    /// marker first, then the old one removed, each made durable, so a
    /// crash at any point leaves at least one and a rerun finishes. The new
    /// marker keeps the old one's bytes and modification time, which a
    /// runner reads as when the agent enrolled. Run it before anything
    /// reads the marker
    pub async fn migrate_marker(&self) -> Result<MarkerMigration, SealError> {
        let legacy = self.dir.join(LEGACY_ENROLLED);
        let chained = self.dir.join(CHAINED);
        if !exists(&legacy).await? {
            return Ok(if exists(&chained).await? {
                MarkerMigration::Already
            } else {
                MarkerMigration::NotEnrolled
            });
        }
        let _lock = self.lock().await?;
        if !exists(&chained).await? {
            let bytes = tokio::fs::read(&legacy).await?;
            let modified = tokio::fs::metadata(&legacy).await?.modified()?;
            let tmp = self.dir.join(format!("{CHAINED}.writing"));
            let mut file =
                tokio::fs::File::create(&tmp).await?.into_std().await;
            tokio::task::spawn_blocking(move || {
                std::io::Write::write_all(&mut file, &bytes)?;
                file.set_modified(modified)?;
                file.sync_all()
            })
            .await
            .map_err(std::io::Error::other)??;
            tokio::fs::rename(&tmp, &chained).await?;
            sync_dir(&self.dir).await?;
        }
        match tokio::fs::remove_file(&legacy).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        sync_dir(&self.dir).await?;
        Ok(MarkerMigration::Migrated)
    }

    pub async fn is_refused(&self) -> std::io::Result<bool> {
        exists(&self.dir.join(REFUSED)).await
    }

    /// Mark the agent chained (`sealed/chained`): sealed saves start.
    /// Uploads are [`Enrolment::uploads`]'s
    pub async fn enroll(&self) -> std::io::Result<()> {
        write_atomic(&self.dir.join(CHAINED), b"").await
    }

    /// Block every seal until an operator clears it
    pub async fn refuse(&self, reason: &str) -> std::io::Result<()> {
        write_atomic(&self.dir.join(REFUSED), reason.as_bytes()).await
    }

    /// Clear a refusal: an operator's act, kept as `refused.cleared.<utc>`
    pub async fn clear_refusal(&self) -> std::io::Result<()> {
        let at = Utc::now().format("%Y%m%dT%H%M%S%3fZ");
        tokio::fs::rename(
            self.dir.join(REFUSED),
            self.dir.join(format!("{REFUSED}.cleared.{at}")),
        )
        .await?;
        sync_dir(&self.dir).await
    }

    pub async fn head(&self) -> Result<Option<HeadFile>, SealError> {
        match tokio::fs::read(self.dir.join(HEAD)).await {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub async fn read_blob(&self, sha: &Sha256Hex) -> std::io::Result<Vec<u8>> {
        tokio::fs::read(self.dir.join(BLOBS).join(sha.to_hex())).await
    }

    /// Committed items not yet uploaded, oldest first
    pub async fn outbox(&self) -> Result<Vec<OutboxItem>, SealError> {
        let next = self.head().await?.map_or(0, |h| h.next_ord);
        let mut items: Vec<OutboxItem> = self
            .items(OUTBOX, "json")
            .await?
            .into_iter()
            .filter(|i| i.ord < next)
            .collect();
        items.sort_by_key(|i| i.ord);
        Ok(items)
    }

    /// Uploaded items, oldest first
    pub async fn sent(&self) -> Result<Vec<OutboxItem>, SealError> {
        let mut items = self.items(SENT, "json").await?;
        items.sort_by_key(|i| i.ord);
        Ok(items)
    }

    async fn items(
        &self,
        sub: &str,
        ext: &str,
    ) -> Result<Vec<OutboxItem>, SealError> {
        let dir = self.dir.join(sub);
        let mut out = Vec::new();
        let mut rd = match tokio::fs::read_dir(&dir).await {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(out);
            }
            Err(e) => return Err(e.into()),
        };
        while let Some(e) = rd.next_entry().await? {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) == Some(ext) {
                out.push(serde_json::from_slice(
                    &tokio::fs::read(&path).await?,
                )?);
            }
        }
        Ok(out)
    }

    /// Move an uploaded item from the outbox to `sent/`
    pub async fn mark_sent(&self, ord: u64) -> Result<(), SealError> {
        let _lock = self.lock().await?;
        let sent = self.dir.join(SENT);
        tokio::fs::create_dir_all(&sent).await?;
        tokio::fs::rename(
            self.dir.join(OUTBOX).join(ord_name(ord, "json")),
            sent.join(ord_name(ord, "json")),
        )
        .await?;
        sync_dir(&sent).await?;
        sync_dir(&self.dir.join(OUTBOX)).await?;
        Ok(())
    }

    /// Step 1: seal, sign and write `outbox/<ord>.tmp`
    pub async fn prepare(
        &self,
        _lock: &SealLock,
        keys: &SealKeys,
        policy: &KeyframePolicy,
        append: Append<'_>,
    ) -> Result<Prepared, SealError> {
        if self.is_refused().await? {
            return Err(SealError::Refused);
        }
        let head = self.head().await?;
        let ord = head.as_ref().map_or(0, |h| h.next_ord);
        let displaced = match (&head, &append.parent) {
            (None, None) if append.kind == StateEntryKind::Genesis => None,
            (Some(h), Some(p)) if append.kind != StateEntryKind::Genesis => {
                (h.head != *p).then_some(h.head)
            }
            _ => return Err(SealError::BadParent),
        };

        // Never before the parent, whatever the clock did since
        // (`time_went_back`, the rule `verify_chain` holds a child to)
        let parent_created_at = match (&head, append.parent) {
            (_, None) => None,
            (Some(h), Some(p)) if h.head == p => Some(h.created_at),
            (_, Some(p)) => Some(
                self.created_at_of(head.as_ref(), p)
                    .await?
                    .ok_or(SealError::ParentNotFound)?,
            ),
        };

        let current = match (&head, displaced) {
            (Some(h), None) if append.kind == StateEntryKind::Snapshot => {
                h.keyframe.clone()
            }
            _ => None,
        };
        // The keyframe's sealed bytes, read here; opening, compressing and
        // sealing run off the async threads (q11 takes seconds)
        let keyframe_sealed = match &current {
            Some(kf) if kf.entry.key_gen == keys.key_gen => {
                match kf.blob_sha256() {
                    Some(sha) => self.read_blob(&sha).await.ok(),
                    None => None,
                }
            }
            _ => None,
        };
        let seq = append.parent.map_or(0, |p| p.seq + 1);
        let prev_hash = append.parent.map(|p| p.entry_hash);
        let job = SealJob {
            agent_id: self.agent_id,
            key_gen: keys.key_gen,
            seq,
            prev_hash,
            state_key: StateKey::from_bytes(Zeroizing::new(
                *keys.state_key.expose(),
            )),
            policy: *policy,
            plaintext: Zeroizing::new(append.plaintext.to_vec()),
            keyframe: current.clone().zip(keyframe_sealed),
        };
        let (encoding, sealed) = tokio::task::spawn_blocking(move || job.run())
            .await
            .map_err(std::io::Error::other)?;
        let blob_sha256 = sha256(&sealed);
        let blob_path = self.dir.join(BLOBS).join(blob_sha256.to_hex());
        if !exists(&blob_path).await? {
            write_atomic(&blob_path, &sealed).await?;
        }

        let entry = StateEntry {
            agora_state_entry: STATE_ENTRY_VERSION,
            agent_id: self.agent_id,
            seq,
            created_at: {
                let now = Utc::now().timestamp_micros();
                match parent_created_at {
                    Some(p) if crate::state::chain::time_went_back(p, now) => p,
                    _ => now,
                }
            },
            state_at: append.state_at.timestamp_micros(),
            kind: append.kind,
            prev_hash,
            merged_from: Vec::new(),
            blob: Some(BlobRef {
                sha256: blob_sha256,
                size_bytes: sealed.len() as u64,
                encoding,
            }),
            target: None,
            key_gen: keys.key_gen,
        };
        let entry = entry.sign(&keys.signing);
        let keyframe_after = if append.kind != StateEntryKind::Snapshot {
            head.as_ref().and_then(|h| h.keyframe.clone())
        } else {
            Some(match encoding {
                BlobEncoding::Keyframe => KeyframeRef {
                    entry: entry.entry.clone(),
                    deltas: 0,
                },
                BlobEncoding::Delta { .. } => {
                    let kf = current.expect("a delta has a keyframe");
                    KeyframeRef {
                        deltas: kf.deltas + 1,
                        ..kf
                    }
                }
            })
        };
        let item = OutboxItem {
            ord,
            entry,
            plaintext_sha256: append.plaintext_sha256,
            keyframe_after,
            displaced,
        };

        let outbox = self.dir.join(OUTBOX);
        let tmp = outbox.join(ord_name(ord, "tmp"));
        if exists(&tmp).await? {
            // A save whose local write never came: kept, by its entry
            let old: OutboxItem =
                serde_json::from_slice(&tokio::fs::read(&tmp).await?)?;
            tokio::fs::rename(&tmp, outbox.join(set_aside(&old, "abandoned")))
                .await?;
        }
        write_atomic(&tmp, &serde_json::to_vec_pretty(&item)?).await?;
        Ok(Prepared { item })
    }

    /// `parent`'s `created_at`, from the head, its keyframe or an item in
    /// `outbox/` or `sent/` (set aside as displaced too)
    async fn created_at_of(
        &self,
        head: Option<&HeadFile>,
        parent: HeadRef,
    ) -> Result<Option<i64>, SealError> {
        if let Some(h) = head {
            if h.head == parent {
                return Ok(Some(h.created_at));
            }
            if let Some(kf) = &h.keyframe
                && kf.entry.seq == parent.seq
                && kf.entry.entry_hash() == parent.entry_hash
            {
                return Ok(Some(kf.entry.created_at));
            }
        }
        for (sub, ext) in
            [(OUTBOX, "json"), (SENT, "json"), (OUTBOX, "displaced")]
        {
            for item in self.items(sub, ext).await? {
                let e = &item.entry.entry;
                if e.seq == parent.seq && e.entry_hash() == parent.entry_hash {
                    return Ok(Some(e.created_at));
                }
            }
        }
        Ok(None)
    }

    /// Step 3: rename the item into the outbox and move the head
    pub async fn commit(
        &self,
        _lock: &SealLock,
        prepared: &Prepared,
    ) -> Result<HeadRef, SealError> {
        self.commit_item(&prepared.item).await
    }

    /// Whether the head is `prepared`'s entry: a commit that failed after
    /// `head.json` was written
    pub async fn is_committed(
        &self,
        prepared: &Prepared,
    ) -> Result<Option<HeadRef>, SealError> {
        let hash = prepared.item.entry.entry_hash();
        Ok(self
            .head()
            .await?
            .map(|h| h.head)
            .filter(|h| h.entry_hash == hash))
    }

    async fn commit_item(
        &self,
        item: &OutboxItem,
    ) -> Result<HeadRef, SealError> {
        let outbox = self.dir.join(OUTBOX);
        let tmp = outbox.join(ord_name(item.ord, "tmp"));
        let json = outbox.join(ord_name(item.ord, "json"));
        let hash = item.entry.entry_hash();
        // The file at this ord must be this item, or another writer's
        let on_disk = |path: PathBuf| async move {
            match tokio::fs::read(&path).await {
                Ok(bytes) => serde_json::from_slice::<OutboxItem>(&bytes)
                    .map(|i| Some(i.entry.entry_hash()))
                    .map_err(SealError::from),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(e.into()),
            }
        };
        match on_disk(tmp.clone()).await? {
            Some(h) if h == hash => {
                tokio::fs::rename(&tmp, &json).await?;
                sync_dir(&outbox).await?;
            }
            Some(_) => return Err(SealError::Raced),
            None => {
                if on_disk(json).await? != Some(hash) {
                    return Err(SealError::Raced);
                }
            }
        }
        let old = self.head().await?;
        let head = HeadRef {
            entry_hash: hash,
            seq: item.entry.entry.seq,
        };
        let mut displaced = old
            .as_ref()
            .map(|h| h.displaced.clone())
            .unwrap_or_default();
        displaced.extend(item.displaced);
        let file = HeadFile {
            head,
            plaintext_sha256: item
                .plaintext_sha256
                .or(old.as_ref().and_then(|h| h.plaintext_sha256)),
            keyframe: item.keyframe_after.clone(),
            next_ord: item.ord + 1,
            displaced,
            created_at: item.entry.entry.created_at,
        };
        write_atomic(&self.dir.join(HEAD), &serde_json::to_vec_pretty(&file)?)
            .await?;
        if item.plaintext_sha256.is_some() {
            // A seal of the state supersedes an unsealed write before it
            match tokio::fs::remove_file(self.dir.join(SEAL_FAILED)).await {
                Ok(()) => sync_dir(&self.dir).await?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(head)
    }

    /// At start: compare the local state's hash (of the value as loaded)
    /// with the head's, and commit the newest uncommitted item that
    /// matches it
    pub async fn recover(
        &self,
        local: Option<&Sha256Hex>,
    ) -> Result<Recovery, SealError> {
        if !self.is_enrolled().await? {
            return Ok(Recovery::NotEnrolled);
        }
        let lock = self.lock().await?;
        self.recover_locked(&lock, local).await
    }

    /// [`recover`](Self::recover), holding the lock
    pub async fn recover_locked(
        &self,
        _lock: &SealLock,
        local: Option<&Sha256Hex>,
    ) -> Result<Recovery, SealError> {
        if !self.is_enrolled().await? {
            return Ok(Recovery::NotEnrolled);
        }
        let Some(head) = self.head().await? else {
            return Ok(Recovery::NoHead);
        };
        let Some(local) = local else {
            return Ok(Recovery::NoLocalState(head.head));
        };
        if head.plaintext_sha256.as_ref() == Some(local) {
            return Ok(Recovery::Intact(head.head));
        }
        let mut pending: Vec<OutboxItem> = self
            .items(OUTBOX, "json")
            .await?
            .into_iter()
            .chain(self.items(OUTBOX, "tmp").await?)
            .filter(|i| i.ord >= head.next_ord)
            .collect();
        pending.sort_by_key(|i| std::cmp::Reverse(i.ord));
        if let Some(item) = pending
            .into_iter()
            .find(|i| i.plaintext_sha256.as_ref() == Some(local))
        {
            return Ok(Recovery::Committed(self.commit_item(&item).await?));
        }
        match self.seal_failed().await? {
            Some(f) if f.local_plaintext_sha256 == *local => {
                Ok(Recovery::SealFailed(head.head, f.error))
            }
            _ => Ok(Recovery::Mismatch(head.head)),
        }
    }

    /// The last unsealed local write, if one is outstanding
    pub async fn seal_failed(&self) -> Result<Option<SealFailed>, SealError> {
        match tokio::fs::read(self.dir.join(SEAL_FAILED)).await {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Record a local write that went ahead unsealed
    pub async fn record_seal_failed(
        &self,
        local_plaintext_sha256: Sha256Hex,
        error: SealFailureKind,
    ) -> Result<(), SealError> {
        let failed = SealFailed {
            at: Utc::now(),
            local_plaintext_sha256,
            error,
        };
        write_atomic(
            &self.dir.join(SEAL_FAILED),
            &serde_json::to_vec_pretty(&failed)?,
        )
        .await?;
        Ok(())
    }

    /// Delete the blobs nothing here names: an abandoned item's the head
    /// has passed, and one a crash left before its item. Never one an entry
    /// of the main line or a kept side line names (`sent/`, the outbox,
    /// `.displaced`, anything under `lost/`), whatever the server holds, so
    /// every version still opens from here. While a fork is unresolved
    /// nothing is deleted, with an alert past [`BLOB_ALERT`] such blobs.
    /// Touches nothing outside `blobs/`
    pub async fn remove_unnamed_blobs(&self) -> Result<usize, SealError> {
        let _lock = self.lock().await?;
        let Some(head) = self.head().await? else {
            return Ok(0);
        };
        let named = self.named_blobs(&head).await?;
        // Under the lock, so no prepare is between writing a blob and its
        // item
        let blobs = self.dir.join(BLOBS);
        let mut unnamed = Vec::new();
        let mut rd = match tokio::fs::read_dir(&blobs).await {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e.into()),
        };
        while let Some(e) = rd.next_entry().await? {
            let Some(sha) = e
                .file_name()
                .to_str()
                .and_then(|n| n.parse::<Sha256Hex>().ok())
            else {
                continue;
            };
            if !named.contains(&sha) {
                unnamed.push(e.path());
            }
        }
        if !head.displaced.is_empty() {
            if unnamed.len() > BLOB_ALERT {
                tracing::error!(
                    agent_id = %self.agent_id,
                    unnamed = unnamed.len(),
                    forks = head.displaced.len(),
                    "unnamed sealed blobs held for an unresolved fork"
                );
            }
            return Ok(0);
        }
        let mut removed = 0;
        for path in unnamed {
            match tokio::fs::remove_file(path).await {
                Ok(()) => removed += 1,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        if removed > 0 {
            sync_dir(&blobs).await?;
        }
        Ok(removed)
    }

    /// Every blob an entry here names, but an abandoned item's the head
    /// has passed
    async fn named_blobs(
        &self,
        head: &HeadFile,
    ) -> Result<HashSet<Sha256Hex>, SealError> {
        let mut named = lost_blobs(&self.dir.join(LOST)).await?;
        for ext in ["json", "lost"] {
            for item in self.items(SENT, ext).await? {
                named.extend(item_blobs(&item));
            }
        }
        for ext in ["json", "tmp", "displaced"] {
            for item in self.items(OUTBOX, ext).await? {
                named.extend(item_blobs(&item));
            }
        }
        for item in self.items(OUTBOX, "abandoned").await? {
            if item.ord >= head.next_ord {
                named.extend(item_blobs(&item));
            }
        }
        named.extend(head.keyframe.as_ref().and_then(|k| k.blob_sha256()));
        Ok(named)
    }

    /// What is held back from the server
    pub async fn held(&self) -> Result<Held, SealError> {
        let mut held = Held {
            outbox_items: self.outbox().await?.len(),
            displaced: self.head().await?.map_or(0, |h| h.displaced.len()),
            ..Held::default()
        };
        if let Ok(mut rd) = tokio::fs::read_dir(self.dir.join(BLOBS)).await {
            while let Some(e) = rd.next_entry().await? {
                held.blobs += 1;
                held.blob_bytes += e.metadata().await?.len();
            }
        }
        Ok(held)
    }

    /// Resolve a fork for the operator (sealed-accept): set every outbox
    /// item aside as `.displaced` (kept, with its blob), and seal `local`
    /// (a value as loaded) as a keyframe child of `server_head`, which the
    /// caller verified. Clear a refusal first
    pub async fn rebase_onto(
        &self,
        lock: &SealLock,
        keys: &SealKeys,
        server_head: &StateEntry,
        local: &serde_json::Value,
    ) -> Result<HeadRef, SealError> {
        let outbox = self.dir.join(OUTBOX);
        for item in self.items(OUTBOX, "json").await? {
            tokio::fs::rename(
                outbox.join(ord_name(item.ord, "json")),
                outbox.join(set_aside(&item, "displaced")),
            )
            .await?;
        }
        if exists(&outbox).await? {
            sync_dir(&outbox).await?;
        }
        let old = self.head().await?;
        let parent = HeadRef {
            entry_hash: server_head.entry_hash(),
            seq: server_head.seq,
        };
        let next_ord = old.as_ref().map_or(0, |h| h.next_ord);
        let based = HeadFile {
            head: parent,
            plaintext_sha256: None,
            keyframe: None,
            next_ord,
            displaced: Vec::new(),
            created_at: server_head.created_at,
        };
        write_atomic(&self.dir.join(HEAD), &serde_json::to_vec_pretty(&based)?)
            .await?;
        let plaintext = codec::plaintext(local);
        let prepared = self
            .prepare(
                lock,
                keys,
                &KeyframePolicy::default(),
                Append {
                    kind: StateEntryKind::Snapshot,
                    plaintext: &plaintext,
                    plaintext_sha256: Some(sha256(&plaintext)),
                    state_at: Utc::now(),
                    parent: Some(parent),
                },
            )
            .await?;
        self.commit(lock, &prepared).await
    }

    /// Start a chain with its genesis
    pub async fn genesis(
        &self,
        keys: &SealKeys,
        origin: &GenesisOrigin,
        state_at: DateTime<Utc>,
    ) -> Result<HeadRef, SealError> {
        let lock = self.lock().await?;
        let plaintext = canonical_json(&serde_json::to_value(origin)?);
        let prepared = self
            .prepare(
                &lock,
                keys,
                &KeyframePolicy::default(),
                Append {
                    kind: StateEntryKind::Genesis,
                    plaintext: &plaintext,
                    plaintext_sha256: None,
                    state_at,
                    parent: None,
                },
            )
            .await?;
        self.commit(&lock, &prepared).await
    }

    /// Append a state with no local write of its own: a backfilled
    /// archive, an `archived_prior` copy, or the current state at
    /// enrolment or after a refusal is cleared. `state` is a value as
    /// loaded, whose hash a later load will match
    pub async fn append_state(
        &self,
        keys: &SealKeys,
        policy: &KeyframePolicy,
        kind: StateEntryKind,
        state: &serde_json::Value,
        state_at: DateTime<Utc>,
    ) -> Result<HeadRef, SealError> {
        let lock = self.lock().await?;
        let head = self.head().await?.ok_or(SealError::NoHead)?;
        let plaintext = codec::plaintext(state);
        let plaintext_sha256 =
            (kind == StateEntryKind::Snapshot).then(|| sha256(&plaintext));
        let prepared = self
            .prepare(
                &lock,
                keys,
                policy,
                Append {
                    kind,
                    plaintext: &plaintext,
                    plaintext_sha256,
                    state_at,
                    parent: Some(head.head),
                },
            )
            .await?;
        self.commit(&lock, &prepared).await
    }

    /// Open the state a sealed snapshot holds, for a restore or a check.
    /// Its blob (and a delta's keyframe's) must still be local
    pub async fn open_state(
        &self,
        keys: &SealKeys,
        entry: &StateEntry,
        keyframe: Option<&StateEntry>,
    ) -> Result<serde_json::Value, OpenError> {
        let blob = entry.blob.as_ref().ok_or(OpenError::NoBlob)?;
        let dict = match blob.encoding {
            BlobEncoding::Keyframe => None,
            BlobEncoding::Delta { base } => {
                let kf = keyframe
                    .filter(|k| k.entry_hash() == base)
                    .ok_or(OpenError::WrongBase)?;
                Some(self.open_plaintext(keys, kf, None).await?)
            }
        };
        let plaintext = self
            .open_plaintext(keys, entry, dict.as_deref().map(|d| &d[..]))
            .await?;
        let mut envelope: serde_json::Value =
            serde_json::from_slice(&plaintext)?;
        Ok(envelope.get_mut("state").ok_or(OpenError::NoBlob)?.take())
    }

    /// Open the [`GenesisOrigin`] a genesis entry seals. Its blob must
    /// still be local
    pub async fn open_origin(
        &self,
        keys: &SealKeys,
        genesis: &StateEntry,
    ) -> Result<GenesisOrigin, OpenError> {
        if genesis.kind != StateEntryKind::Genesis {
            return Err(OpenError::NotGenesis(genesis.kind));
        }
        let plaintext = self.open_plaintext(keys, genesis, None).await?;
        Ok(serde_json::from_slice(&plaintext)?)
    }

    async fn open_plaintext(
        &self,
        keys: &SealKeys,
        entry: &StateEntry,
        dict: Option<&[u8]>,
    ) -> Result<Zeroizing<Vec<u8>>, OpenError> {
        let blob = entry.blob.as_ref().ok_or(OpenError::NoBlob)?;
        if entry.key_gen != keys.key_gen {
            return Err(OpenError::KeyGen(entry.key_gen));
        }
        let sealed = self.read_blob(&blob.sha256).await?;
        let compressed = open_entry_blob(&keys.state_key, entry, &sealed)?;
        Ok(codec::decompress(&compressed, dict)?)
    }

    /// Rebuild a lost `head.json` from the server's chain: verified under
    /// `trusted`, the head's state opened from its blobs (`fetch`, checked
    /// against each `BlobRef`), and its plaintext hash compared with the
    /// local state's. Writes `head.json` and the current keyframe's blob
    /// only if all of that passes, and never a genesis. Leaves `enrolled`
    /// to the caller
    pub async fn adopt_server_head<F, Fut>(
        &self,
        _lock: &SealLock,
        chain: &[SignedStateEntry],
        trusted: &super::verify::TrustedKeys,
        keys: &SealKeys,
        local: Option<&serde_json::Value>,
        mut fetch: F,
    ) -> Result<Adopted, AdoptError>
    where
        F: FnMut(Sha256Hex) -> Fut + Send,
        Fut: std::future::Future<
                Output = Result<
                    Vec<u8>,
                    Box<dyn std::error::Error + Send + Sync>,
                >,
            > + Send,
    {
        if self.head().await?.is_some() {
            return Err(AdoptError::HeadExists);
        }
        let report = super::verify::verify_chain(self.agent_id, chain, trusted);
        if !report.intact {
            return Err(AdoptError::Chain(report.problems));
        }
        let last = chain.last().ok_or(AdoptError::Empty)?;
        let head = HeadRef {
            entry_hash: last.entry_hash(),
            seq: last.entry.seq,
        };
        let snapshot = chain
            .iter()
            .rev()
            .map(|s| &s.entry)
            .find(|e| e.kind == StateEntryKind::Snapshot)
            .ok_or(AdoptError::NoSnapshot)?;
        let keyframe = match snapshot.blob.as_ref().map(|b| b.encoding) {
            Some(BlobEncoding::Delta { base }) => chain
                .iter()
                .map(|s| &s.entry)
                .find(|e| e.entry_hash() == base)
                .ok_or(AdoptError::Chain(Vec::new()))?,
            _ => snapshot,
        };
        let mut open = async |entry: &StateEntry, dict: Option<&[u8]>| {
            if entry.key_gen != keys.key_gen {
                return Err(AdoptError::Open(OpenError::KeyGen(entry.key_gen)));
            }
            let sha = entry.blob.as_ref().ok_or(OpenError::NoBlob)?.sha256;
            let sealed = fetch(sha).await.map_err(AdoptError::Fetch)?;
            let compressed = open_entry_blob(&keys.state_key, entry, &sealed)
                .map_err(OpenError::from)?;
            let plaintext = codec::decompress(&compressed, dict)
                .map_err(OpenError::from)?;
            Ok::<_, AdoptError>((sealed, plaintext))
        };
        let (kf_sealed, kf_plaintext) = open(keyframe, None).await?;
        let plaintext = if std::ptr::eq(keyframe, snapshot) {
            kf_plaintext
        } else {
            open(snapshot, Some(&kf_plaintext)).await?.1
        };
        let server_sha256 = sha256(&plaintext);
        drop(plaintext);

        let kf_hash = keyframe.entry_hash();
        let deltas = chain
            .iter()
            .filter_map(|s| s.entry.blob.as_ref())
            .filter(|b| b.encoding == BlobEncoding::Delta { base: kf_hash })
            .count() as u32;
        let mut next_ord = 0;
        for (sub, ext) in [
            (OUTBOX, "json"),
            (OUTBOX, "tmp"),
            (OUTBOX, "abandoned"),
            (SENT, "json"),
        ] {
            for item in self.items(sub, ext).await? {
                next_ord = next_ord.max(item.ord + 1);
            }
        }
        let blob_path = self
            .dir
            .join(BLOBS)
            .join(keyframe.blob.as_ref().expect("opened").sha256.to_hex());
        if !exists(&blob_path).await? {
            write_atomic(&blob_path, &kf_sealed).await?;
        }
        let file = HeadFile {
            head,
            plaintext_sha256: Some(server_sha256),
            keyframe: Some(KeyframeRef {
                entry: keyframe.clone(),
                deltas,
            }),
            next_ord,
            displaced: Vec::new(),
            created_at: last.entry.created_at,
        };
        write_atomic(&self.dir.join(HEAD), &serde_json::to_vec_pretty(&file)?)
            .await?;
        Ok(match local {
            Some(v) if plaintext_hash(v) == server_sha256 => {
                Adopted::Intact(head)
            }
            _ => Adopted::Mismatch(head),
        })
    }
}

/// What [`AgentSeal::adopt_server_head`] found, having written `head.json`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Adopted {
    /// The local state is the server head's
    Intact(HeadRef),
    /// It isn't: keep the refusal; the operator decides
    Mismatch(HeadRef),
}

/// Why a server head was not adopted; nothing was written
#[derive(Debug, thiserror::Error)]
pub enum AdoptError {
    #[error("head.json exists; nothing to rebuild")]
    HeadExists,
    #[error("the server's chain is empty")]
    Empty,
    #[error("the server's chain has no snapshot")]
    NoSnapshot,
    #[error("the server's chain does not verify: {0:?}")]
    Chain(Vec<super::verify::ChainProblem>),
    #[error("fetching a blob: {0}")]
    Fetch(Box<dyn std::error::Error + Send + Sync>),
    #[error(transparent)]
    Open(#[from] OpenError),
    #[error(transparent)]
    Seal(#[from] SealError),
}

impl From<std::io::Error> for AdoptError {
    fn from(e: std::io::Error) -> Self {
        Self::Seal(e.into())
    }
}

impl From<serde_json::Error> for AdoptError {
    fn from(e: serde_json::Error) -> Self {
        Self::Seal(e.into())
    }
}

/// A sealed state that would not open
#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Blob(#[from] EntryBlobError),
    #[error("{0}")]
    Codec(#[from] codec::CodecError),
    #[error("the entry has no blob")]
    NoBlob,
    #[error("a delta needs its own keyframe")]
    WrongBase,
    #[error("sealed under key generation {0}, which isn't held")]
    KeyGen(u32),
    #[error("a {0:?} entry, not a genesis")]
    NotGenesis(StateEntryKind),
}

/// [`Mirrored`]'s errors: the local store's alone, since a seal never
/// fails a save
#[derive(Debug, thiserror::Error)]
pub enum MirroredError<E> {
    #[error(transparent)]
    Local(E),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    NotFound(#[from] AgentNotFound),
    /// Only from [`Mirrored::enroll`]: a save never fails on a seal
    #[error(transparent)]
    Seal(#[from] SealError),
}

impl<E: crate::reactor::RetryAfter> crate::reactor::RetryAfter
    for MirroredError<E>
{
    fn retry_after(&self) -> Option<std::time::Duration> {
        match self {
            Self::Local(e) => e.retry_after(),
            _ => None,
        }
    }
}

/// A local [`Storage`] whose saves are sealed first, for agents that are
/// enrolled and not refused. Loads stay local; uploads are the
/// [`Outbox`]'s, never inside a save, and only for an agent that
/// [`uploads`](Enrolment::uploads)
pub struct Mirrored<L, O, E> {
    local: L,
    outbox: O,
    enrolment: E,
    policy: KeyframePolicy,
    recoveries: std::sync::Mutex<BTreeMap<AgentId, Recovery>>,
}

impl<L, O, E> Mirrored<L, O, E> {
    pub fn new(local: L, outbox: O, enrolment: E) -> Self {
        Self {
            local,
            outbox,
            enrolment,
            policy: KeyframePolicy::default(),
            recoveries: Default::default(),
        }
    }

    pub fn with_policy(mut self, policy: KeyframePolicy) -> Self {
        self.policy = policy;
        self
    }

    pub fn local(&self) -> &L {
        &self.local
    }

    pub fn outbox(&self) -> &O {
        &self.outbox
    }

    /// What the last load of `id` found in `sealed/`, for the runner's
    /// start check
    pub fn recovery(&self, id: AgentId) -> Option<Recovery> {
        self.recoveries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&id)
            .cloned()
    }
}

impl<L: AgentDir, O, E> Mirrored<L, O, E> {
    /// The agent's `sealed/` directory
    pub fn seal(&self, id: AgentId) -> AgentSeal {
        AgentSeal::new(&self.local.agent_dir(id), id)
    }
}

/// How a commit came out
#[derive(Debug)]
enum Settled {
    Committed(HeadRef),
    /// Recorded as `kind`; the save returns `base`
    Failed {
        error: SealError,
        kind: SealFailureKind,
        base: Option<HeadRef>,
    },
}

/// A commit's outcome, given whether the head is its entry when it
/// failed: a failure after the head moved is a commit, and one where the
/// head can't be read gives no base, so the next start says `SealFailed`
/// rather than seeing a fork
fn settle(
    commit: Result<HeadRef, SealError>,
    committed: Result<Option<HeadRef>, SealError>,
    base: Option<&HeadRef>,
) -> Settled {
    match (commit, committed) {
        (Ok(new), _) | (Err(_), Ok(Some(new))) => Settled::Committed(new),
        (Err(error), Ok(None)) => Settled::Failed {
            kind: error.kind(),
            error,
            base: base.copied(),
        },
        (Err(_), Err(error)) => Settled::Failed {
            kind: SealFailureKind::Io,
            error,
            base: None,
        },
    }
}

/// How far a save's seal got before the local write
enum Step {
    NotEnrolled,
    /// Same bytes as the head's
    Unchanged(HeadRef),
    Prepared(Box<Prepared>),
}

impl<L: AgentDir, O, E: Enrolment> Mirrored<L, O, E> {
    /// Step 1 of a save of an enrolled agent, under `lock`
    async fn prepare(
        &self,
        lock: &SealLock,
        seal: &AgentSeal,
        id: AgentId,
        plaintext: &[u8],
        plaintext_sha256: Sha256Hex,
        base: Option<&HeadRef>,
    ) -> Result<Step, SealError> {
        if !seal.is_enrolled().await? {
            return Ok(Step::NotEnrolled);
        }
        if seal.is_refused().await? {
            return Err(SealError::Refused);
        }
        let head = seal.head().await?.ok_or(SealError::NoHead)?;
        if head.plaintext_sha256 == Some(plaintext_sha256) {
            return Ok(Step::Unchanged(head.head));
        }
        let Some(parent) = base.copied() else {
            tracing::error!(
                agent_id = %id,
                head = %head.head.entry_hash,
                "sealed save without a base; saved locally, unsealed"
            );
            return Err(SealError::BadParent);
        };
        let keys = self.enrolment.keys(id).ok_or(SealError::NoKeys)?;
        seal.prepare(
            lock,
            &keys,
            &self.policy,
            Append {
                kind: StateEntryKind::Snapshot,
                plaintext,
                plaintext_sha256: Some(plaintext_sha256),
                state_at: Utc::now(),
                parent: Some(parent),
            },
        )
        .await
        .map(|p| Step::Prepared(Box::new(p)))
    }

    /// A local write that went ahead unsealed: logged and recorded
    async fn unsealed(
        seal: &AgentSeal,
        id: AgentId,
        plaintext_sha256: Sha256Hex,
        kind: SealFailureKind,
        error: Option<&SealError>,
    ) {
        match error {
            Some(e) if kind != SealFailureKind::Refused => tracing::error!(
                agent_id = %id,
                error = %e,
                kind = ?kind,
                "seal failed; the state was saved locally, unsealed"
            ),
            _ => tracing::warn!(
                agent_id = %id,
                kind = ?kind,
                "state saved locally, unsealed"
            ),
        }
        if let Err(e) = seal.record_seal_failed(plaintext_sha256, kind).await {
            tracing::error!(
                agent_id = %id,
                error = %e,
                "could not record the unsealed write"
            );
        }
    }
}

impl<L: MarkedLocal, O, E> Mirrored<L, O, E> {
    /// The local write: marked for an enrolled agent, plain otherwise
    async fn write_local(
        &mut self,
        id: AgentId,
        value: serde_json::Value,
        enrolled: bool,
    ) -> Result<(), MirroredError<L::Error>> {
        if enrolled {
            self.local.save_marked(id, value).await
        } else {
            self.local.save_raw(id, value, None).await.map(drop)
        }
        .map_err(MirroredError::Local)
    }
}

impl<L, O, E> Mirrored<L, O, E>
where
    L: MarkedLocal + AgentDir,
    E: Enrolment,
{
    /// Enrolment's last step, after its genesis, archives and current
    /// state are sealed: mark the local state, align the head with the
    /// state as marked (a float may not survive the rewrite), then write
    /// `enrolled`
    pub async fn enroll(
        &mut self,
        id: AgentId,
    ) -> Result<HeadRef, MirroredError<L::Error>> {
        let seal = self.seal(id);
        let Loaded { value, .. } = self
            .local
            .load_raw(id)
            .await
            .map_err(MirroredError::Local)?;
        self.local
            .save_marked(id, value)
            .await
            .map_err(MirroredError::Local)?;
        let Loaded { value, .. } = self
            .local
            .load_raw(id)
            .await
            .map_err(MirroredError::Local)?;
        let head = seal.head().await?.ok_or(SealError::NoHead)?;
        let head = if head.plaintext_sha256 == Some(plaintext_hash(&value)) {
            head.head
        } else {
            let keys = self.enrolment.keys(id).ok_or(SealError::NoKeys)?;
            seal.append_state(
                &keys,
                &self.policy,
                StateEntryKind::Snapshot,
                &value,
                Utc::now(),
            )
            .await?
        };
        seal.enroll().await.map_err(SealError::from)?;
        Ok(head)
    }
}

#[async_trait::async_trait]
impl<L, O, E> Storage for Mirrored<L, O, E>
where
    L: MarkedLocal + AgentDir,
    O: Outbox,
    E: Enrolment,
{
    type Error = MirroredError<L::Error>;
    type Version = HeadRef;

    /// A seal that fails never stops the local write: it is recorded in
    /// `seal_failed.json` instead, as is a write of a refused agent's state
    async fn save_raw(
        &mut self,
        id: AgentId,
        value: serde_json::Value,
        base: Option<&HeadRef>,
    ) -> Result<Option<HeadRef>, Self::Error> {
        let seal = self.seal(id);
        // Hash and seal what a load will read, not the value in memory
        let reloaded = as_reloaded(&value)?;
        let plaintext = codec::plaintext(&reloaded);
        let plaintext_sha256 = sha256(&plaintext);
        drop(reloaded);

        // An agent not enrolled gets no `sealed/` at all
        if let Ok(false) = seal.is_enrolled().await {
            self.write_local(id, value, false).await?;
            return Ok(None);
        }
        let lock = match seal.lock().await {
            Ok(lock) => lock,
            Err(e) => {
                self.write_local(id, value, true).await?;
                Self::unsealed(&seal, id, plaintext_sha256, e.kind(), Some(&e))
                    .await;
                return Ok(base.copied());
            }
        };
        let step = self
            .prepare(&lock, &seal, id, &plaintext, plaintext_sha256, base)
            .await;
        match step {
            Ok(Step::NotEnrolled) => {
                self.write_local(id, value, false).await?;
                Ok(None)
            }
            Ok(Step::Unchanged(head)) => {
                self.write_local(id, value, true).await?;
                Ok(Some(head))
            }
            Ok(Step::Prepared(prepared)) => {
                self.write_local(id, value, true).await?;
                let commit = seal.commit(&lock, &prepared).await;
                let committed = match &commit {
                    Ok(_) => Ok(None),
                    Err(_) => seal.is_committed(&prepared).await,
                };
                match settle(commit, committed, base) {
                    Settled::Committed(new) => {
                        if let Some(displaced) = prepared.item.displaced {
                            tracing::warn!(
                                agent_id = %id,
                                displaced = %displaced.entry_hash,
                                "sealed save forked from an older head; held for a merge"
                            );
                        }
                        if self.enrolment.uploads(id) {
                            self.outbox.notify(id);
                        }
                        Ok(Some(new))
                    }
                    Settled::Failed { error, kind, base } => {
                        Self::unsealed(
                            &seal,
                            id,
                            plaintext_sha256,
                            kind,
                            Some(&error),
                        )
                        .await;
                        Ok(base)
                    }
                }
            }
            Err(e) => {
                self.write_local(id, value, true).await?;
                Self::unsealed(&seal, id, plaintext_sha256, e.kind(), Some(&e))
                    .await;
                Ok(base.copied())
            }
        }
    }

    /// Local, always. A problem in `sealed/` degrades to no base and
    /// [`Recovery::Unreadable`] ([`Mirrored::recovery`]), never a failed load
    async fn load_raw(
        &self,
        id: AgentId,
    ) -> Result<Loaded<serde_json::Value, HeadRef>, Self::Error> {
        let seal = self.seal(id);
        if let Ok(false) = seal.is_enrolled().await {
            let Loaded { value, .. } = self
                .local
                .load_raw(id)
                .await
                .map_err(MirroredError::Local)?;
            return Ok(Loaded { value, base: None });
        }
        // The local read and the check under one lock, so no save lands
        // between them
        let lock = seal.lock().await;
        let Loaded { value, .. } = self
            .local
            .load_raw(id)
            .await
            .map_err(MirroredError::Local)?;
        let checked = match &lock {
            Ok(lock) => {
                seal.recover_locked(lock, Some(&plaintext_hash(&value)))
                    .await
            }
            Err(_) => Err(SealError::Io(std::io::Error::other("no lock"))),
        };
        drop(lock);
        let recovery = checked.unwrap_or_else(|e| {
            tracing::warn!(
                agent_id = %id,
                error = %e,
                kind = ?e.kind(),
                "sealed state unreadable; loaded without a base"
            );
            Recovery::Unreadable(e.kind())
        });
        let base = match &recovery {
            Recovery::Intact(h)
            | Recovery::Committed(h)
            | Recovery::NoLocalState(h)
            | Recovery::SealFailed(h, _) => Some(*h),
            // Not this state's head: a save must not parent onto it
            Recovery::Mismatch(_)
            | Recovery::NotEnrolled
            | Recovery::NoHead
            | Recovery::Unreadable(_) => None,
        };
        self.recoveries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, recovery);
        Ok(Loaded { value, base })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::generate_keypair;
    use crate::reactor::storage::SEALED_FORMAT;
    use crate::state::verify::{TrustedKeys, verify_chain};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    /// A signing key and a state key, raw
    type KeyBytes = ([u8; 32], [u8; 32]);

    /// Fixed keys; every agent uploads except those in `local_only`
    #[derive(Clone, Default)]
    struct Keys {
        keys: Arc<Mutex<HashMap<AgentId, KeyBytes>>>,
        local_only: Arc<Mutex<Vec<AgentId>>>,
    }

    impl Keys {
        fn add(&self, id: AgentId) -> crate::crypto::VerifyingKey {
            let (sk, vk) = generate_keypair();
            let state = StateKey::generate();
            self.keys
                .lock()
                .unwrap()
                .insert(id, (sk.to_bytes(), *state.expose()));
            vk
        }
    }

    impl Enrolment for Keys {
        fn uploads(&self, id: AgentId) -> bool {
            !self.local_only.lock().unwrap().contains(&id)
        }

        fn keys(&self, id: AgentId) -> Option<SealKeys> {
            let (sk, state) = *self.keys.lock().unwrap().get(&id)?;
            Some(SealKeys {
                signing: SigningKey::from_bytes(&sk),
                state_key: StateKey::from_bytes(zeroize::Zeroizing::new(state)),
                key_gen: 0,
            })
        }
    }

    fn state(n: usize) -> serde_json::Value {
        let memory: String =
            (0..n).map(|i| format!("Day {i}: a thread. ")).collect();
        serde_json::json!({ "memory": memory })
    }

    /// Counts [`Outbox::notify`] calls, by agent
    #[derive(Clone, Default)]
    struct Wakes(Arc<Mutex<Vec<AgentId>>>);

    impl Outbox for Wakes {
        fn notify(&self, id: AgentId) {
            self.0.lock().unwrap().push(id);
        }
    }

    impl Wakes {
        fn of(&self, id: AgentId) -> usize {
            self.0.lock().unwrap().iter().filter(|&&w| w == id).count()
        }
    }

    type Store = Mirrored<FsStorage, Wakes, Keys>;

    /// An enrolled agent: genesis and its first state, sealed
    async fn enrolled(
        dir: &Path,
    ) -> (Store, AgentId, crate::crypto::VerifyingKey) {
        let keys = Keys::default();
        let id = AgentId::new();
        let vk = keys.add(id);
        let store =
            Mirrored::new(FsStorage::new(dir), Wakes::default(), keys.clone());
        store
            .local
            .clone()
            .save_raw(id, state(10), None)
            .await
            .unwrap();
        let seal = store.seal(id);
        let k = keys.keys(id).unwrap();
        let origin = GenesisOrigin {
            class: crate::state::GenesisClass::None,
            incident: None,
            prior_copy_held: false,
            verified_at: None,
        };
        seal.genesis(&k, &origin, Utc::now()).await.unwrap();
        seal.append_state(
            &k,
            &KeyframePolicy::default(),
            StateEntryKind::Snapshot,
            &state(10),
            Utc::now(),
        )
        .await
        .unwrap();
        let mut store = store;
        store.enroll(id).await.unwrap();
        (store, id, vk)
    }

    async fn chain(seal: &AgentSeal) -> Vec<SignedStateEntry> {
        let mut items = seal.sent().await.unwrap();
        items.extend(seal.outbox().await.unwrap());
        items.into_iter().map(|i| i.entry).collect()
    }

    #[tokio::test]
    async fn saves_chain_deltas_and_skip_unchanged_states() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, id, vk) = enrolled(dir.path()).await;
        let mut base = store.load_raw(id).await.unwrap().base;
        for n in 11..15 {
            base = store.save_raw(id, state(n), base.as_ref()).await.unwrap();
        }
        // Unchanged: no entry
        let before = store.seal(id).outbox().await.unwrap().len();
        let same = store.save_raw(id, state(14), base.as_ref()).await.unwrap();
        assert_eq!(same, base);
        assert_eq!(store.seal(id).outbox().await.unwrap().len(), before);
        assert_eq!(store.outbox.of(id), 4, "one wake per commit");

        let entries = chain(&store.seal(id)).await;
        assert_eq!(entries.len(), 6, "genesis, enrolment state, four saves");
        let report = verify_chain(id, &entries, &TrustedKeys::new(vk));
        assert!(report.intact, "{:?}", report.problems);
        assert!(entries[3..].iter().any(|e| matches!(
            e.entry.blob.as_ref().unwrap().encoding,
            BlobEncoding::Delta { .. }
        )));

        // Each sealed state opens back to what was saved
        let k = store.enrolment.keys(id).unwrap();
        let seal = store.seal(id);
        let keyframe_of =
            |e: &StateEntry| match e.blob.as_ref().unwrap().encoding {
                BlobEncoding::Delta { base } => entries
                    .iter()
                    .map(|s| &s.entry)
                    .find(|k| k.entry_hash() == base),
                BlobEncoding::Keyframe => None,
            };
        for (e, n) in entries[1..].iter().zip(10..) {
            let opened = seal
                .open_state(&k, &e.entry, keyframe_of(&e.entry))
                .await
                .unwrap();
            assert_eq!(opened, state(n));
        }
        // A delta opened against the wrong keyframe fails
        let delta = entries
            .iter()
            .rev()
            .find(|e| keyframe_of(&e.entry).is_some())
            .unwrap();
        assert!(
            seal.open_state(&k, &delta.entry, Some(&entries[0].entry))
                .await
                .is_err()
        );
        // No plaintext hash in anything uploaded
        let local = store
            .seal(id)
            .head()
            .await
            .unwrap()
            .unwrap()
            .plaintext_sha256
            .unwrap();
        for e in &entries {
            assert!(
                !serde_json::to_string(e).unwrap().contains(&local.to_hex())
            );
        }
    }

    /// No `sealed/chained`, no `sealed/`, whether it uploads or not
    #[tokio::test]
    async fn an_agent_not_enrolled_is_never_sealed() {
        let dir = tempfile::tempdir().unwrap();
        let keys = Keys::default();
        let mut store =
            Mirrored::new(FsStorage::new(dir.path()), Wakes::default(), keys);
        for uploads in [true, false] {
            let id = AgentId::new();
            store.enrolment.add(id);
            if !uploads {
                store.enrolment.local_only.lock().unwrap().push(id);
            }
            assert_eq!(store.save_raw(id, state(1), None).await.unwrap(), None);
            assert_eq!(store.load_raw(id).await.unwrap().base, None);
            assert_eq!(store.save_raw(id, state(2), None).await.unwrap(), None);
            assert!(!exists(&store.seal(id).dir).await.unwrap(), "{uploads}");
            assert_eq!(store.outbox.of(id), 0);
        }
    }

    /// Each snapshot's state, opened from the blobs on disk
    async fn open_all(
        seal: &AgentSeal,
        keys: &SealKeys,
        entries: &[SignedStateEntry],
    ) -> Vec<serde_json::Value> {
        let mut states = Vec::new();
        for e in entries
            .iter()
            .filter(|e| e.entry.kind == StateEntryKind::Snapshot)
        {
            let keyframe = match e.entry.blob.as_ref().unwrap().encoding {
                BlobEncoding::Delta { base } => entries
                    .iter()
                    .map(|s| &s.entry)
                    .find(|k| k.entry_hash() == base),
                BlobEncoding::Keyframe => None,
            };
            states
                .push(seal.open_state(keys, &e.entry, keyframe).await.unwrap());
        }
        states
    }

    /// Enrolled but not uploading: chained, delta'd and checked at load
    /// exactly as an uploading agent is, and the outbox never woken
    #[tokio::test]
    async fn an_enrolled_agent_that_doesnt_upload_is_chained_locally() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, id, vk) = enrolled(dir.path()).await;
        store.enrolment.local_only.lock().unwrap().push(id);
        let mut base = store.load_raw(id).await.unwrap().base;
        assert!(matches!(store.recovery(id), Some(Recovery::Intact(_))));
        for n in 11..19 {
            base = store.save_raw(id, state(n), base.as_ref()).await.unwrap();
            assert!(base.is_some());
            store.load_raw(id).await.unwrap();
            assert!(matches!(store.recovery(id), Some(Recovery::Intact(_))));
        }
        assert_eq!(store.outbox.of(id), 0, "nothing to upload");
        let seal = store.seal(id);
        assert_eq!(seal.seal_failed().await.unwrap(), None);
        assert_eq!(format_of(&store, id), SEALED_FORMAT);

        let entries = chain(&seal).await;
        assert_eq!(entries.len(), 10, "genesis, enrolment state, eight saves");
        let report = verify_chain(id, &entries, &TrustedKeys::new(vk));
        assert!(report.intact, "{:?}", report.problems);
        assert!(entries.iter().any(|e| matches!(
            e.entry.blob.as_ref().unwrap().encoding,
            BlobEncoding::Delta { .. }
        )));

        // A change outside the store is caught, as for anyone
        store
            .local
            .clone()
            .save_raw(id, state(1), None)
            .await
            .unwrap();
        assert_eq!(store.load_raw(id).await.unwrap().base, None);
        assert!(matches!(store.recovery(id), Some(Recovery::Mismatch(_))));
    }

    /// Nothing is ever sent for an agent that never uploaded, so pruning
    /// keeps every version it has
    #[tokio::test]
    async fn pruning_keeps_every_blob_of_an_agent_that_doesnt_upload() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, id, _) = enrolled(dir.path()).await;
        store.enrolment.local_only.lock().unwrap().push(id);
        let mut base = store.load_raw(id).await.unwrap().base;
        for n in 11..19 {
            base = store.save_raw(id, state(n), base.as_ref()).await.unwrap();
        }
        let seal = store.seal(id);
        let before = blobs(&seal).await;
        assert_eq!(seal.remove_unnamed_blobs().await.unwrap(), 0);
        assert_eq!(blobs(&seal).await, before);

        let k = store.enrolment.keys(id).unwrap();
        let entries = chain(&seal).await;
        let opened = open_all(&seal, &k, &entries).await;
        let saved: Vec<_> = (10..19).map(state).collect();
        assert_eq!(opened, saved);
    }

    /// A `seal_failed.json` written for an opted-out agent before 0.158
    /// still reads, and the next seal clears it
    #[tokio::test]
    async fn an_old_opted_out_record_reads_and_the_next_save_clears_it() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, id, _) = enrolled(dir.path()).await;
        let seal = store.seal(id);
        store.local.save_marked(id, state(50)).await.unwrap();
        let local = store.local.load_raw(id).await.unwrap().value;
        let old = serde_json::json!({
            "at": "2026-11-01T00:00:00Z",
            "local_plaintext_sha256": plaintext_hash(&local),
            "error": "opted_out",
        });
        write_atomic(
            &seal.dir().join(SEAL_FAILED),
            &serde_json::to_vec(&old).unwrap(),
        )
        .await
        .unwrap();
        let base = store.load_raw(id).await.unwrap().base;
        assert!(matches!(
            store.recovery(id),
            Some(Recovery::SealFailed(_, SealFailureKind::OptedOut))
        ));
        store.save_raw(id, state(51), base.as_ref()).await.unwrap();
        assert_eq!(seal.seal_failed().await.unwrap(), None);
        store.load_raw(id).await.unwrap();
        assert!(matches!(store.recovery(id), Some(Recovery::Intact(_))));
    }

    #[tokio::test]
    async fn a_refused_agent_is_saved_locally_and_never_sealed() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, id, _) = enrolled(dir.path()).await;
        let seal = store.seal(id);
        seal.refuse("wiped").await.unwrap();
        let before = seal.outbox().await.unwrap().len();
        let base = store.load_raw(id).await.unwrap().base;
        let got = store.save_raw(id, state(99), base.as_ref()).await.unwrap();
        assert_eq!(got, base, "the base doesn't move");
        assert_eq!(store.local.load_raw(id).await.unwrap().value, state(99));
        assert_eq!(seal.outbox().await.unwrap().len(), before);
        assert_eq!(
            seal.seal_failed().await.unwrap().unwrap().error,
            SealFailureKind::Refused
        );
        seal.clear_refusal().await.unwrap();
        // After the refusal is cleared, the write is known
        let base = store.load_raw(id).await.unwrap().base;
        assert!(matches!(
            store.recovery(id),
            Some(Recovery::SealFailed(_, SealFailureKind::Refused))
        ));
        store.save_raw(id, state(100), base.as_ref()).await.unwrap();
        assert_eq!(seal.outbox().await.unwrap().len(), before + 1);
        assert_eq!(seal.seal_failed().await.unwrap(), None);
    }

    /// A seal that fails still writes the state, records it, and the next
    /// seal clears the record
    #[tokio::test]
    async fn a_failed_seal_never_stops_the_local_write() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, id, _) = enrolled(dir.path()).await;
        let seal = store.seal(id);
        let base = store.load_raw(id).await.unwrap().base;
        let saved_keys =
            store.enrolment.keys.lock().unwrap().remove(&id).unwrap();

        let got = store.save_raw(id, state(42), base.as_ref()).await.unwrap();
        assert_eq!(got, base);
        let local = store.local.load_raw(id).await.unwrap().value;
        assert_eq!(local, state(42), "written, unsealed");
        let failed = seal.seal_failed().await.unwrap().unwrap();
        assert_eq!(failed.error, SealFailureKind::NoKeys);
        assert_eq!(failed.local_plaintext_sha256, plaintext_hash(&local));
        // Not the May signature: its own verdict
        let head = seal.head().await.unwrap().unwrap().head;
        assert_eq!(
            seal.recover(Some(&plaintext_hash(&local))).await.unwrap(),
            Recovery::SealFailed(head, SealFailureKind::NoKeys)
        );
        // Anything else is still a mismatch
        assert!(matches!(
            seal.recover(Some(&plaintext_hash(&state(1))))
                .await
                .unwrap(),
            Recovery::Mismatch(_)
        ));

        // An IO failure inside sealed/ is no different
        store.enrolment.keys.lock().unwrap().insert(id, saved_keys);
        let blobs = seal.dir().join(BLOBS);
        tokio::fs::rename(&blobs, seal.dir().join("blobs.moved"))
            .await
            .unwrap();
        tokio::fs::write(&blobs, b"not a directory").await.unwrap();
        store.save_raw(id, state(43), base.as_ref()).await.unwrap();
        assert_eq!(store.local.load_raw(id).await.unwrap().value, state(43));
        assert_eq!(
            seal.seal_failed().await.unwrap().unwrap().error,
            SealFailureKind::Io
        );
        tokio::fs::remove_file(&blobs).await.unwrap();
        tokio::fs::rename(seal.dir().join("blobs.moved"), &blobs)
            .await
            .unwrap();

        // The next seal clears it
        store.save_raw(id, state(44), base.as_ref()).await.unwrap();
        assert_eq!(seal.seal_failed().await.unwrap(), None);
        let local = store.local.load_raw(id).await.unwrap().value;
        assert!(matches!(
            seal.recover(Some(&plaintext_hash(&local))).await.unwrap(),
            Recovery::Intact(_)
        ));
    }

    #[tokio::test]
    async fn two_loads_and_two_saves_fork_and_nothing_is_lost() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, id, vk) = enrolled(dir.path()).await;
        let a = store.load_raw(id).await.unwrap().base;
        let b = store.load_raw(id).await.unwrap().base;
        assert_eq!(a, b);
        let first = store
            .save_raw(id, state(20), a.as_ref())
            .await
            .unwrap()
            .unwrap();
        let second = store
            .save_raw(id, state(21), b.as_ref())
            .await
            .unwrap()
            .unwrap();
        let (h1, h2) = (first, second);
        assert_eq!(h1.seq, h2.seq, "siblings");
        let head = store.seal(id).head().await.unwrap().unwrap();
        assert_eq!(head.head, h2);
        assert_eq!(head.displaced, vec![h1]);
        // Nothing is pruned while the fork is unresolved
        for ord in 0..3 {
            store.seal(id).mark_sent(ord).await.unwrap();
        }
        assert_eq!(store.seal(id).remove_unnamed_blobs().await.unwrap(), 0);
        // Both are kept, and the chain shows the fork
        let entries = chain(&store.seal(id)).await;
        let report = verify_chain(id, &entries, &TrustedKeys::new(vk));
        assert!(report.problems.contains(
            &crate::state::verify::ChainProblem::Fork { seq: h2.seq }
        ));
        assert!(matches!(
            entries
                .last()
                .unwrap()
                .entry
                .blob
                .as_ref()
                .unwrap()
                .encoding,
            BlobEncoding::Keyframe
        ));
    }

    /// The three write steps, with a crash after each
    #[tokio::test]
    async fn a_crash_between_any_two_steps_recovers() {
        for crash_after in 1..=3 {
            let dir = tempfile::tempdir().unwrap();
            let (store, id, _) = enrolled(dir.path()).await;
            let seal = store.seal(id);
            let k = store.enrolment.keys(id).unwrap();
            let head = seal.head().await.unwrap().unwrap();
            let next = state(77);
            let plaintext = codec::plaintext(&next);
            let lock = seal.lock().await.unwrap();
            let prepared = seal
                .prepare(
                    &lock,
                    &k,
                    &KeyframePolicy::default(),
                    Append {
                        kind: StateEntryKind::Snapshot,
                        plaintext: &plaintext,
                        plaintext_sha256: Some(sha256(&plaintext)),
                        state_at: Utc::now(),
                        parent: Some(head.head),
                    },
                )
                .await
                .unwrap();
            drop(lock);
            if crash_after >= 2 {
                store
                    .local
                    .clone()
                    .save_raw(id, next.clone(), None)
                    .await
                    .unwrap();
            }
            if crash_after >= 3 {
                // The rename, not the head
                let outbox = seal.dir.join(OUTBOX);
                tokio::fs::rename(
                    outbox.join(ord_name(prepared.item.ord, "tmp")),
                    outbox.join(ord_name(prepared.item.ord, "json")),
                )
                .await
                .unwrap();
            }
            let local = store.local.load_raw(id).await.unwrap().value;
            let got =
                seal.recover(Some(&plaintext_hash(&local))).await.unwrap();
            let new = HeadRef {
                entry_hash: prepared.item.entry.entry_hash(),
                seq: prepared.item.entry.entry.seq,
            };
            match crash_after {
                1 => assert_eq!(got, Recovery::Intact(head.head)),
                _ => assert_eq!(
                    got,
                    Recovery::Committed(new),
                    "crash after {crash_after}"
                ),
            }
            // And a second start finds it intact
            let again =
                seal.recover(Some(&plaintext_hash(&local))).await.unwrap();
            assert!(matches!(again, Recovery::Intact(_)), "{again:?}");
            if crash_after >= 2 {
                assert_eq!(
                    seal.outbox().await.unwrap().last().unwrap().ord,
                    prepared.item.ord
                );
            }
        }
    }

    #[tokio::test]
    async fn a_state_changed_outside_the_store_is_a_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let (store, id, _) = enrolled(dir.path()).await;
        store
            .local
            .clone()
            .save_raw(id, serde_json::json!({ "memory": "" }), None)
            .await
            .unwrap();
        let local = store.local.load_raw(id).await.unwrap().value;
        assert!(matches!(
            store
                .seal(id)
                .recover(Some(&plaintext_hash(&local)))
                .await
                .unwrap(),
            Recovery::Mismatch(_)
        ));
        assert!(matches!(
            store.seal(id).recover(None).await.unwrap(),
            Recovery::NoLocalState(_)
        ));
    }

    /// A float that doesn't survive serde_json's default parse: the save
    /// hashes what the load reads
    #[tokio::test]
    async fn a_float_state_is_intact_after_a_reload() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, id, _) = enrolled(dir.path()).await;
        let base = store.load_raw(id).await.unwrap().base;
        let value =
            serde_json::json!({ "memory": "m", "x": 9856906946328.693_f64 });
        let new = store.save_raw(id, value, base.as_ref()).await.unwrap();
        let loaded = store.load_raw(id).await.unwrap();
        assert_eq!(store.recovery(id), Some(Recovery::Intact(new.unwrap())));
        assert_eq!(loaded.base, new);
    }

    /// Two stores on one directory save at once: the lock orders them,
    /// and both entries are kept, one as the head and one displaced
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn two_concurrent_writers_lose_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (mut a, id, _) = enrolled(dir.path()).await;
        let mut b = Mirrored::new(
            FsStorage::new(dir.path()),
            Wakes::default(),
            a.enrolment.clone(),
        );
        let base = a.load_raw(id).await.unwrap().base;
        let before = a.seal(id).outbox().await.unwrap().len();
        let (ra, rb) = tokio::join!(
            a.save_raw(id, state(30), base.as_ref()),
            b.save_raw(id, state(31), base.as_ref()),
        );
        let (ha, hb) = (ra.unwrap().unwrap(), rb.unwrap().unwrap());
        assert_ne!(ha, hb);
        let seal = a.seal(id);
        let outbox = seal.outbox().await.unwrap();
        assert_eq!(outbox.len(), before + 2, "both recorded");
        let hashes: Vec<_> =
            outbox.iter().map(|i| i.entry.entry_hash()).collect();
        assert!(
            hashes.contains(&ha.entry_hash) && hashes.contains(&hb.entry_hash)
        );
        let head = seal.head().await.unwrap().unwrap();
        assert!(head.head == ha || head.head == hb);
        assert_eq!(head.displaced.len(), 1);
        assert_eq!(seal.seal_failed().await.unwrap(), None);
    }

    /// A commit refuses an outbox item that is not the one it prepared
    #[tokio::test]
    async fn a_commit_refuses_another_writers_item() {
        let dir = tempfile::tempdir().unwrap();
        let (store, id, _) = enrolled(dir.path()).await;
        let seal = store.seal(id);
        let k = store.enrolment.keys(id).unwrap();
        let head = seal.head().await.unwrap().unwrap();
        let lock = seal.lock().await.unwrap();
        let append = |pt: &'static [u8]| Append {
            kind: StateEntryKind::Snapshot,
            plaintext: pt,
            plaintext_sha256: Some(sha256(pt)),
            state_at: Utc::now(),
            parent: Some(head.head),
        };
        let policy = KeyframePolicy::default();
        let first = seal
            .prepare(&lock, &k, &policy, append(b"one"))
            .await
            .unwrap();
        let second = seal
            .prepare(&lock, &k, &policy, append(b"two"))
            .await
            .unwrap();
        assert_eq!(first.item.ord, second.item.ord);
        assert!(matches!(
            seal.commit(&lock, &first).await,
            Err(SealError::Raced)
        ));
        assert_eq!(seal.head().await.unwrap().unwrap().head, head.head);
        seal.commit(&lock, &second).await.unwrap();
        drop(lock);
        // The abandoned item's blob goes once the head is past it; the
        // committed one's stays until uploaded
        let blob =
            |p: &Prepared| p.item.entry.entry.blob.as_ref().unwrap().sha256;
        assert_eq!(seal.remove_unnamed_blobs().await.unwrap(), 1);
        assert!(seal.read_blob(&blob(&first)).await.is_err());
        assert!(seal.read_blob(&blob(&second)).await.is_ok());
        // The abandoned record itself is kept
        assert_eq!(seal.items(OUTBOX, "abandoned").await.unwrap().len(), 1);
    }

    /// A commit that fails after moving the head still counts
    #[tokio::test]
    async fn a_commit_error_after_the_head_moved_is_a_commit() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, id, _) = enrolled(dir.path()).await;
        let seal = store.seal(id);
        let base = store.load_raw(id).await.unwrap().base;
        // Clearing `seal_failed.json` is the commit's last step: make it fail
        tokio::fs::create_dir(seal.dir().join(SEAL_FAILED))
            .await
            .unwrap();
        let new = store
            .save_raw(id, state(60), base.as_ref())
            .await
            .unwrap()
            .unwrap();
        assert_ne!(Some(new), base);
        assert_eq!(seal.head().await.unwrap().unwrap().head, new);
        tokio::fs::remove_dir(seal.dir().join(SEAL_FAILED))
            .await
            .unwrap();
        // The next save is no fork
        store.save_raw(id, state(61), Some(&new)).await.unwrap();
        assert!(seal.head().await.unwrap().unwrap().displaced.is_empty());
        assert_eq!(seal.seal_failed().await.unwrap(), None);
    }

    /// An enrolled agent's save without a base is written, not sealed
    #[tokio::test]
    async fn a_save_without_a_base_is_recorded_not_guessed() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, id, _) = enrolled(dir.path()).await;
        let seal = store.seal(id);
        let before = seal.outbox().await.unwrap().len();
        assert_eq!(store.save_raw(id, state(70), None).await.unwrap(), None);
        assert_eq!(store.local.load_raw(id).await.unwrap().value, state(70));
        assert_eq!(seal.outbox().await.unwrap().len(), before);
        assert_eq!(
            seal.seal_failed().await.unwrap().unwrap().error,
            SealFailureKind::BadParent
        );
    }

    /// A broken `sealed/` degrades a load, never fails it
    #[tokio::test]
    async fn a_load_degrades_on_a_sealed_problem() {
        let dir = tempfile::tempdir().unwrap();
        let (store, id, _) = enrolled(dir.path()).await;
        tokio::fs::write(store.seal(id).dir().join(HEAD), b"{not json")
            .await
            .unwrap();
        let loaded = store.load_raw(id).await.unwrap();
        assert_eq!(loaded.value, state(10));
        assert_eq!(loaded.base, None);
        assert_eq!(
            store.recovery(id),
            Some(Recovery::Unreadable(SealFailureKind::Json))
        );
    }

    #[test]
    fn append_debug_shows_no_plaintext() {
        let a = Append {
            kind: StateEntryKind::Snapshot,
            plaintext: b"secret memory",
            plaintext_sha256: None,
            state_at: Utc::now(),
            parent: None,
        };
        let shown = format!("{a:?}");
        assert!(!shown.contains("secret") && shown.contains("REDACTED"));
    }

    /// Every blob under `sealed/blobs`, as a server would hold them
    async fn blobs(seal: &AgentSeal) -> HashMap<Sha256Hex, Vec<u8>> {
        let mut out = HashMap::new();
        let mut rd = tokio::fs::read_dir(seal.dir().join(BLOBS)).await.unwrap();
        while let Some(e) = rd.next_entry().await.unwrap() {
            let sha: Sha256Hex =
                e.file_name().to_str().unwrap().parse().unwrap();
            out.insert(sha, tokio::fs::read(e.path()).await.unwrap());
        }
        out
    }

    fn fetcher(
        blobs: HashMap<Sha256Hex, Vec<u8>>,
    ) -> impl FnMut(
        Sha256Hex,
    ) -> std::future::Ready<
        Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>>,
    > + Send {
        move |sha| {
            std::future::ready(
                blobs.get(&sha).cloned().ok_or_else(|| "missing".into()),
            )
        }
    }

    /// A store whose `sealed/` was lost, with what the server holds
    async fn lost(
        dir: &Path,
    ) -> (
        Store,
        AgentId,
        crate::crypto::VerifyingKey,
        Vec<SignedStateEntry>,
        HashMap<Sha256Hex, Vec<u8>>,
        HeadRef,
    ) {
        let (mut store, id, vk) = enrolled(dir).await;
        let mut base = store.load_raw(id).await.unwrap().base;
        for n in 11..14 {
            base = store.save_raw(id, state(n), base.as_ref()).await.unwrap();
        }
        let seal = store.seal(id);
        let server = chain(&seal).await;
        let blobs = blobs(&seal).await;
        tokio::fs::remove_dir_all(seal.dir()).await.unwrap();
        (store, id, vk, server, blobs, base.unwrap())
    }

    #[tokio::test]
    async fn a_lost_head_is_rebuilt_from_the_server() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, id, vk, server, blobs, head) = lost(dir.path()).await;
        let seal = store.seal(id);
        let keys = store.enrolment.keys(id).unwrap();
        let local = store.local.load_raw(id).await.unwrap().value;
        let lock = seal.lock().await.unwrap();
        let adopted = seal
            .adopt_server_head(
                &lock,
                &server,
                &TrustedKeys::new(vk),
                &keys,
                Some(&local),
                fetcher(blobs),
            )
            .await
            .unwrap();
        drop(lock);
        assert_eq!(adopted, Adopted::Intact(head));
        assert_eq!(seal.head().await.unwrap().unwrap().head, head);
        // Saving goes on from it, a delta against the adopted keyframe
        seal.enroll().await.unwrap();
        let base = store.load_raw(id).await.unwrap().base;
        assert_eq!(store.recovery(id), Some(Recovery::Intact(head)));
        let next = store
            .save_raw(id, state(14), base.as_ref())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(next.seq, head.seq + 1);
        let item = seal.outbox().await.unwrap().pop().unwrap();
        assert!(matches!(
            item.entry.entry.blob.unwrap().encoding,
            BlobEncoding::Delta { .. }
        ));
    }

    #[tokio::test]
    async fn an_adopted_head_that_isnt_the_local_state_is_a_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let (store, id, vk, server, blobs, head) = lost(dir.path()).await;
        let seal = store.seal(id);
        let keys = store.enrolment.keys(id).unwrap();
        let lock = seal.lock().await.unwrap();
        let adopted = seal
            .adopt_server_head(
                &lock,
                &server,
                &TrustedKeys::new(vk),
                &keys,
                Some(&serde_json::json!({ "memory": "" })),
                fetcher(blobs),
            )
            .await
            .unwrap();
        assert_eq!(adopted, Adopted::Mismatch(head));
        // Twice is refused
        assert!(matches!(
            seal.adopt_server_head(
                &lock,
                &server,
                &TrustedKeys::new(vk),
                &keys,
                None,
                fetcher(HashMap::new()),
            )
            .await,
            Err(AdoptError::HeadExists)
        ));
    }

    #[tokio::test]
    async fn a_bad_chain_or_blob_is_never_adopted() {
        let dir = tempfile::tempdir().unwrap();
        let (store, id, vk, server, blobs, _) = lost(dir.path()).await;
        let seal = store.seal(id);
        let keys = store.enrolment.keys(id).unwrap();
        let lock = seal.lock().await.unwrap();
        let local = store.local.load_raw(id).await.unwrap().value;

        // Untrusted key
        let (_, other) = generate_keypair();
        let r = seal
            .adopt_server_head(
                &lock,
                &server,
                &TrustedKeys::new(other),
                &keys,
                Some(&local),
                fetcher(blobs.clone()),
            )
            .await;
        assert!(matches!(r, Err(AdoptError::Chain(_))), "{r:?}");
        // Truncated: no genesis
        let r = seal
            .adopt_server_head(
                &lock,
                &server[1..],
                &TrustedKeys::new(vk),
                &keys,
                Some(&local),
                fetcher(blobs.clone()),
            )
            .await;
        assert!(matches!(r, Err(AdoptError::Chain(_))), "{r:?}");
        // A blob swapped for another
        let head_blob =
            server.last().unwrap().entry.blob.as_ref().unwrap().sha256;
        let other_blob = server[1].entry.blob.as_ref().unwrap().sha256;
        let mut swapped = blobs.clone();
        swapped.insert(head_blob, blobs[&other_blob].clone());
        let r = seal
            .adopt_server_head(
                &lock,
                &server,
                &TrustedKeys::new(vk),
                &keys,
                Some(&local),
                fetcher(swapped),
            )
            .await;
        assert!(matches!(r, Err(AdoptError::Open(_))), "{r:?}");
        // A blob the server lacks
        let r = seal
            .adopt_server_head(
                &lock,
                &server,
                &TrustedKeys::new(vk),
                &keys,
                Some(&local),
                fetcher(HashMap::new()),
            )
            .await;
        assert!(matches!(r, Err(AdoptError::Fetch(_))), "{r:?}");
        // Nothing was written
        assert_eq!(seal.head().await.unwrap(), None);
    }

    /// A held fork resolved for the operator: the local state sealed as a
    /// keyframe child of the server's head, the other line kept aside
    #[tokio::test]
    async fn rebase_onto_resolves_a_fork_and_keeps_the_other_line() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, id, vk) = enrolled(dir.path()).await;
        let base = store.load_raw(id).await.unwrap().base;
        let h1 = store
            .save_raw(id, state(20), base.as_ref())
            .await
            .unwrap()
            .unwrap();
        let h2 = store
            .save_raw(id, state(21), base.as_ref())
            .await
            .unwrap()
            .unwrap();
        let seal = store.seal(id);
        // The server took the first line: genesis, enrolment state, h1
        let items = seal.outbox().await.unwrap();
        let server: Vec<_> =
            items[..3].iter().map(|i| i.entry.clone()).collect();
        for item in &items[..3] {
            seal.mark_sent(item.ord).await.unwrap();
        }
        assert_eq!(server[2].entry_hash(), h1.entry_hash);
        let h2_blob = items[3].entry.entry.blob.as_ref().unwrap().sha256;

        let keys = store.enrolment.keys(id).unwrap();
        let local = store.local.load_raw(id).await.unwrap().value;
        let lock = seal.lock().await.unwrap();
        let new = seal
            .rebase_onto(&lock, &keys, &server[2].entry, &local)
            .await
            .unwrap();
        drop(lock);
        assert_eq!(new.seq, h1.seq + 1);
        let head = seal.head().await.unwrap().unwrap();
        assert!(head.displaced.is_empty());
        let outbox = seal.outbox().await.unwrap();
        assert_eq!(outbox.len(), 1);
        assert_eq!(outbox[0].entry.entry.prev_hash, Some(h1.entry_hash));
        // The other line is kept, with its blob, through a prune
        let aside = seal.items(OUTBOX, "displaced").await.unwrap();
        assert_eq!(aside[0].entry.entry_hash(), h2.entry_hash);
        seal.remove_unnamed_blobs().await.unwrap();
        assert!(seal.read_blob(&h2_blob).await.is_ok());
        // The server's line plus the rebased entry verify
        let mut line = server.clone();
        line.push(outbox[0].entry.clone());
        assert!(verify_chain(id, &line, &TrustedKeys::new(vk)).intact);
        // And the local state is its head's
        assert_eq!(store.load_raw(id).await.unwrap().base, Some(new));
    }

    /// A mismatched state gets no base to save against
    #[tokio::test]
    async fn a_mismatch_loads_without_a_base() {
        let dir = tempfile::tempdir().unwrap();
        let (store, id, _) = enrolled(dir.path()).await;
        store
            .local
            .clone()
            .save_raw(id, serde_json::json!({ "memory": "" }), None)
            .await
            .unwrap();
        let loaded = store.load_raw(id).await.unwrap();
        assert_eq!(loaded.base, None);
        assert!(matches!(store.recovery(id), Some(Recovery::Mismatch(_))));
    }

    /// A clock stepped back never makes an entry older than its parent
    #[tokio::test]
    async fn a_child_is_never_older_than_its_parent() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, id, vk) = enrolled(dir.path()).await;
        let seal = store.seal(id);
        let mut head = seal.head().await.unwrap().unwrap();
        let future = Utc::now().timestamp_micros() + 3_600_000_000;
        head.created_at = future;
        write_atomic(
            &seal.dir().join(HEAD),
            &serde_json::to_vec(&head).unwrap(),
        )
        .await
        .unwrap();
        let base = store.load_raw(id).await.unwrap().base;
        store.save_raw(id, state(80), base.as_ref()).await.unwrap();
        let item = seal.outbox().await.unwrap().pop().unwrap();
        assert_eq!(item.entry.entry.created_at, future);
        let entries = chain(&seal).await;
        let report = verify_chain(id, &entries, &TrustedKeys::new(vk));
        assert!(report.intact);
    }

    /// A forked save's parent, not the head, floors its time too: a clock
    /// stepped back since the parent never makes the child older (review
    /// of 0.148)
    #[tokio::test]
    async fn a_forked_child_is_never_older_than_its_parent() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, id, vk) = enrolled(dir.path()).await;
        let seal = store.seal(id);
        // A parent stamped an hour ahead: the clock has since stepped back
        let mut head = seal.head().await.unwrap().unwrap();
        let future = Utc::now().timestamp_micros() + 3_600_000_000;
        head.created_at = future;
        write_atomic(
            &seal.dir().join(HEAD),
            &serde_json::to_vec(&head).unwrap(),
        )
        .await
        .unwrap();
        let base = store.load_raw(id).await.unwrap().base;
        let parent = store
            .save_raw(id, state(80), base.as_ref())
            .await
            .unwrap()
            .unwrap();
        // Two loads of it; one save moves the head on, the other forks
        let a = store.load_raw(id).await.unwrap().base;
        let b = store.load_raw(id).await.unwrap().base;
        store.save_raw(id, state(81), a.as_ref()).await.unwrap();
        let forked = store
            .save_raw(id, state(82), b.as_ref())
            .await
            .unwrap()
            .unwrap();
        let head = seal.head().await.unwrap().unwrap();
        assert_eq!(head.head, forked);
        assert_eq!(head.displaced.len(), 1, "a fork");
        let item = seal.outbox().await.unwrap().pop().unwrap();
        assert_eq!(item.entry.entry.prev_hash, Some(parent.entry_hash));
        assert_eq!(item.entry.entry.created_at, future);
        let entries = chain(&seal).await;
        let report = verify_chain(id, &entries, &TrustedKeys::new(vk));
        assert!(
            !report.problems.iter().any(|p| matches!(
                p,
                crate::state::verify::ChainProblem::TimeWentBack { .. }
            )),
            "{:?}",
            report.problems
        );
    }

    /// A parent in no record here fails the seal rather than going
    /// unchecked
    #[tokio::test]
    async fn a_parent_nowhere_here_fails_the_seal() {
        let dir = tempfile::tempdir().unwrap();
        let (store, id, _) = enrolled(dir.path()).await;
        let seal = store.seal(id);
        let k = store.enrolment.keys(id).unwrap();
        let plaintext = codec::plaintext(&state(90));
        let before = seal.outbox().await.unwrap();
        let lock = seal.lock().await.unwrap();
        let err = seal
            .prepare(
                &lock,
                &k,
                &KeyframePolicy::default(),
                Append {
                    kind: StateEntryKind::Snapshot,
                    plaintext: &plaintext,
                    plaintext_sha256: Some(sha256(&plaintext)),
                    state_at: Utc::now(),
                    parent: Some(HeadRef {
                        entry_hash: Sha256Hex::from([0x5a; 32]),
                        seq: 0,
                    }),
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(err, SealError::ParentNotFound), "{err}");
        assert_eq!(err.kind(), SealFailureKind::BadParent);
        assert_eq!(seal.outbox().await.unwrap(), before, "nothing added");
    }

    /// Abandoned items at one ord all stay, and a blob nothing names goes
    #[tokio::test]
    async fn abandoned_items_never_collide_and_orphans_are_swept() {
        let dir = tempfile::tempdir().unwrap();
        let (store, id, _) = enrolled(dir.path()).await;
        let seal = store.seal(id);
        let k = store.enrolment.keys(id).unwrap();
        let head = seal.head().await.unwrap().unwrap();
        let lock = seal.lock().await.unwrap();
        let policy = KeyframePolicy::default();
        for pt in [&b"one"[..], b"two", b"three"] {
            seal.prepare(
                &lock,
                &k,
                &policy,
                Append {
                    kind: StateEntryKind::Snapshot,
                    plaintext: pt,
                    plaintext_sha256: Some(sha256(pt)),
                    state_at: Utc::now(),
                    parent: Some(head.head),
                },
            )
            .await
            .unwrap();
        }
        drop(lock);
        assert_eq!(seal.items(OUTBOX, "abandoned").await.unwrap().len(), 2);
        // A crash between a blob and its item leaves an orphan
        let orphan = Sha256Hex::from([0xab; 32]);
        tokio::fs::write(seal.dir().join(BLOBS).join(orphan.to_hex()), b"x")
            .await
            .unwrap();
        tokio::fs::write(seal.dir().join(BLOBS).join("not-a-blob"), b"x")
            .await
            .unwrap();
        seal.remove_unnamed_blobs().await.unwrap();
        assert!(seal.read_blob(&orphan).await.is_err());
        assert!(seal.dir().join(BLOBS).join("not-a-blob").exists());
        // The pending tmp's blob is kept
        let tmp = seal.items(OUTBOX, "tmp").await.unwrap();
        let sha = tmp[0].entry.entry.blob.as_ref().unwrap().sha256;
        assert!(seal.read_blob(&sha).await.is_ok());
    }

    /// Sent versions keep their blobs: after every version is
    /// acknowledged and a sweep, each still opens from here. What goes is
    /// what nothing names: an orphan, and an abandoned item the head has
    /// passed, unless a set-aside record under `lost/` names it
    #[tokio::test]
    async fn every_acknowledged_version_still_opens_after_a_sweep() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, id, _) = enrolled(dir.path()).await;
        let seal = store.seal(id);
        let k = store.enrolment.keys(id).unwrap();
        let policy = KeyframePolicy::default();
        let mut base = store.load_raw(id).await.unwrap().base;
        // Abandon an item at the next number, twice; each save then
        // passes it
        let mut abandoned = Vec::new();
        for n in 11..19 {
            if n == 12 || n == 14 {
                let lock = seal.lock().await.unwrap();
                seal.prepare(
                    &lock,
                    &k,
                    &policy,
                    Append {
                        kind: StateEntryKind::Snapshot,
                        plaintext: b"never written",
                        plaintext_sha256: Some(sha256(b"never written")),
                        state_at: Utc::now(),
                        parent: base,
                    },
                )
                .await
                .unwrap();
            }
            base = store.save_raw(id, state(n), base.as_ref()).await.unwrap();
            if n == 12 || n == 14 {
                let items = seal.items(OUTBOX, "abandoned").await.unwrap();
                abandoned.push(
                    items.into_iter().find(|i| !abandoned.contains(i)).unwrap(),
                );
            }
        }
        for item in seal.outbox().await.unwrap() {
            seal.mark_sent(item.ord).await.unwrap();
        }
        // One abandoned item is named by a runner's set-aside record
        let lost = seal.dir().join("lost").join("20261111T000000Z");
        std::fs::create_dir_all(&lost).unwrap();
        std::fs::write(
            lost.join("INTENT"),
            serde_json::to_vec(&serde_json::json!({
                "note": "n", "items": [abandoned[1]],
            }))
            .unwrap(),
        )
        .unwrap();
        let orphan = Sha256Hex::from([0xab; 32]);
        std::fs::write(seal.dir().join(BLOBS).join(orphan.to_hex()), b"x")
            .unwrap();

        assert_eq!(seal.remove_unnamed_blobs().await.unwrap(), 2);
        assert!(seal.read_blob(&orphan).await.is_err());
        let blob = |i: &OutboxItem| i.entry.entry.blob.as_ref().unwrap().sha256;
        assert!(seal.read_blob(&blob(&abandoned[0])).await.is_err());
        assert!(seal.read_blob(&blob(&abandoned[1])).await.is_ok());
        assert!(seal.outbox().await.unwrap().is_empty());
        let entries = chain(&seal).await;
        let opened = open_all(&seal, &k, &entries).await;
        assert_eq!(opened, (10..19).map(state).collect::<Vec<_>>());
    }

    #[tokio::test]
    async fn a_genesis_origin_opens_and_nothing_else_does() {
        let dir = tempfile::tempdir().unwrap();
        let (store, id, _) = enrolled(dir.path()).await;
        let seal = store.seal(id);
        let k = store.enrolment.keys(id).unwrap();
        let entries = chain(&seal).await;
        let origin = seal.open_origin(&k, &entries[0].entry).await.unwrap();
        assert_eq!(
            origin,
            GenesisOrigin {
                class: crate::state::GenesisClass::None,
                incident: None,
                prior_copy_held: false,
                verified_at: None,
            }
        );
        assert!(matches!(
            seal.open_origin(&k, &entries[1].entry).await,
            Err(OpenError::NotGenesis(StateEntryKind::Snapshot))
        ));
    }

    #[test]
    fn a_commit_settles_by_where_the_head_is() {
        let new = HeadRef {
            entry_hash: Sha256Hex::from([1; 32]),
            seq: 3,
        };
        let base = HeadRef {
            entry_hash: Sha256Hex::from([2; 32]),
            seq: 2,
        };
        let io = || SealError::Io(std::io::Error::other("disk"));
        assert!(
            matches!(settle(Ok(new), Ok(None), Some(&base)), Settled::Committed(h) if h == new)
        );
        // Failed after the head moved: committed
        assert!(
            matches!(settle(Err(io()), Ok(Some(new)), Some(&base)), Settled::Committed(h) if h == new)
        );
        // Failed before: the base stands
        assert!(matches!(
            settle(Err(SealError::Raced), Ok(None), Some(&base)),
            Settled::Failed { kind: SealFailureKind::Raced, base: Some(b), .. } if b == base
        ));
        // Unknown: no base, recorded as io
        assert!(matches!(
            settle(Err(SealError::Raced), Err(io()), Some(&base)),
            Settled::Failed {
                kind: SealFailureKind::Io,
                base: None,
                ..
            }
        ));
    }

    fn format_of(store: &Store, id: AgentId) -> serde_json::Value {
        let raw: serde_json::Value = serde_json::from_slice(
            &std::fs::read(store.local.agent_dir(id).join("state.json"))
                .unwrap(),
        )
        .unwrap();
        raw["format"].clone()
    }

    /// An enrolled agent's state is marked so binaries before 0.158 refuse
    /// it; anyone else's is written as before. The mark changes no hash
    #[tokio::test]
    async fn an_enrolled_agents_state_is_marked_and_others_are_not() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, id, _) = enrolled(dir.path()).await;
        assert_eq!(format_of(&store, id), SEALED_FORMAT, "marked at enrolment");
        let base = store.load_raw(id).await.unwrap().base;
        assert!(matches!(store.recovery(id), Some(Recovery::Intact(_))));
        store.save_raw(id, state(90), base.as_ref()).await.unwrap();
        assert_eq!(format_of(&store, id), SEALED_FORMAT);
        store.load_raw(id).await.unwrap();
        assert!(matches!(store.recovery(id), Some(Recovery::Intact(_))));
        // Enrolled but not uploading: still marked
        store.enrolment.local_only.lock().unwrap().push(id);
        store.save_raw(id, state(91), None).await.unwrap();
        assert_eq!(format_of(&store, id), SEALED_FORMAT);

        // Not enrolled: format 1, byte for byte what FsStorage writes
        let other = AgentId::new();
        store.enrolment.add(other);
        store.save_raw(other, state(5), None).await.unwrap();
        let ours =
            std::fs::read(store.local.agent_dir(other).join("state.json"))
                .unwrap();
        let plain_dir = tempfile::tempdir().unwrap();
        let mut plain = FsStorage::new(plain_dir.path());
        plain.save_raw(other, state(5), None).await.unwrap();
        let theirs =
            std::fs::read(plain.agent_dir(other).join("state.json")).unwrap();
        assert_eq!(ours, theirs);
        assert!(!store.seal(other).dir().exists());
    }

    /// A state saved at format 2 (0.147-0.157) loads intact and saves at
    /// format 3 with the same plaintext hash
    #[tokio::test]
    async fn a_format_2_state_saves_at_3_with_the_same_hash() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, id, _) = enrolled(dir.path()).await;
        let path = store.local.agent_dir(id).join("state.json");
        let v2 = serde_json::to_vec_pretty(&serde_json::json!({
            "format": 2, "sealed": true, "state": state(10),
        }))
        .unwrap();
        std::fs::write(&path, v2).unwrap();
        let loaded = store.load_raw(id).await.unwrap();
        assert!(matches!(store.recovery(id), Some(Recovery::Intact(_))));
        let hash = plaintext_hash(&loaded.value);
        store
            .save_raw(id, loaded.value, loaded.base.as_ref())
            .await
            .unwrap();
        assert_eq!(format_of(&store, id), 3);
        let again = store.load_raw(id).await.unwrap().value;
        assert_eq!(plaintext_hash(&again), hash);
        assert!(matches!(store.recovery(id), Some(Recovery::Intact(_))));
        assert_eq!(codec::PLAINTEXT_FORMAT, 1);
    }

    /// The pre-0.158 marker, at every point a migration can stop: never
    /// read as chained until migrated, never left with neither marker, and
    /// a rerun finishes
    #[tokio::test]
    async fn the_old_marker_migrates_and_a_crash_never_loses_both() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, id, _) = enrolled(dir.path()).await;
        let seal = store.seal(id);
        let old = seal.dir().join(LEGACY_ENROLLED);
        let new = seal.dir().join(CHAINED);
        let enrolled_at = std::time::SystemTime::UNIX_EPOCH
            + std::time::Duration::from_secs(1_760_000_000);
        // (old, new, leftover tmp) as a crash leaves them
        let states = [
            (true, false, false),
            (true, false, true),
            (true, true, false),
            (false, true, false),
        ];
        for (has_old, has_new, writing) in states {
            let _ = std::fs::remove_file(&old);
            let _ = std::fs::remove_file(&new);
            for (path, present) in [(&old, has_old), (&new, has_new)] {
                // A completed earlier run's `new` already has the old time
                if present {
                    std::fs::write(path, b"since").unwrap();
                    std::fs::File::options()
                        .write(true)
                        .open(path)
                        .unwrap()
                        .set_modified(enrolled_at)
                        .unwrap();
                }
            }
            if writing {
                std::fs::write(seal.dir().join("chained.writing"), b"")
                    .unwrap();
            }
            let label = format!("{has_old} {has_new} {writing}");
            assert_eq!(seal.has_legacy_marker().await.unwrap(), has_old);
            assert_eq!(seal.is_enrolled().await.unwrap(), has_new, "{label}");
            let got = seal.migrate_marker().await.unwrap();
            let want = if has_old {
                MarkerMigration::Migrated
            } else {
                MarkerMigration::Already
            };
            assert_eq!(got, want, "{label}");
            assert!(new.exists() && !old.exists(), "{label}");
            assert!(!seal.has_legacy_marker().await.unwrap());
            let modified = std::fs::metadata(&new).unwrap().modified().unwrap();
            assert_eq!(modified, enrolled_at, "{label}");
            assert_eq!(std::fs::read(&new).unwrap(), b"since", "{label}");
            assert_eq!(
                seal.migrate_marker().await.unwrap(),
                MarkerMigration::Already
            );
        }
        // Old marker only: not chained, so a save goes plain, and after
        // the migration the start check catches it. Hence: migrate first
        std::fs::remove_file(&new).unwrap();
        std::fs::write(&old, b"").unwrap();
        let before = chain(&seal).await.len();
        assert_eq!(store.load_raw(id).await.unwrap().base, None);
        store.save_raw(id, state(70), None).await.unwrap();
        assert_eq!(chain(&seal).await.len(), before, "not sealed");
        assert_eq!(format_of(&store, id), 1);
        assert_eq!(
            seal.migrate_marker().await.unwrap(),
            MarkerMigration::Migrated
        );
        assert_eq!(store.load_raw(id).await.unwrap().base, None);
        assert!(matches!(store.recovery(id), Some(Recovery::Mismatch(_))));

        // Neither: nothing created
        let other = store.seal(AgentId::new());
        assert_eq!(
            other.migrate_marker().await.unwrap(),
            MarkerMigration::NotEnrolled
        );
        assert!(!other.dir().exists());
    }
}
