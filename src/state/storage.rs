//! [`Mirrored`]: a local [`Storage`] whose saves are also sealed, chained
//! and queued for upload, for agents that opted in and are enrolled.
//!
//! Under `<agent_dir>/sealed/`:
//!
//! - `enrolled`, `refused`: markers. Only an enrolled, unrefused agent is
//!   sealed
//! - `head.json`: the local head ([`HeadFile`])
//! - `blobs/<sha256>`: every sealed blob
//! - `outbox/<ord>.json`: committed entries not yet uploaded, `<ord>.tmp`
//!   one being written, `<ord>.abandoned` one whose local write never came
//! - `sent/<ord>.json`: uploaded entries
//!
//! A save is crash-safe in this order: seal into `outbox/<ord>.tmp`, the
//! local write, then commit (the rename and `head.json`). [`AgentSeal::recover`]
//! finishes a commit a crash interrupted. Nothing here deletes.
//!
//! [`Storage`]: crate::reactor::Storage

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::chain::{
    BlobEncoding, BlobRef, GenesisOrigin, STATE_ENTRY_VERSION,
    SignedStateEntry, StateEntry,
};
use super::codec::{self, Encoded, Keyframe, KeyframePolicy};
use super::crypto::{StateKey, open_blob, seal_blob};
use super::verify::HeadRef;
use crate::crypto::SigningKey;
use crate::enums::StateEntryKind;
use crate::govlog::{Sha256Hex, canonical_json};
use crate::ids::AgentId;
use crate::reactor::{AgentNotFound, FsStorage, Loaded, Storage, Version};

/// The sealed directory, in an agent's
pub const SEALED_DIR: &str = "sealed";
const ENROLLED: &str = "enrolled";
const REFUSED: &str = "refused";
const HEAD: &str = "head.json";
const OUTBOX: &str = "outbox";
const SENT: &str = "sent";
const BLOBS: &str = "blobs";

/// A [`Storage`] that keeps each agent in a directory of its own
pub trait AgentDir {
    fn agent_dir(&self, id: AgentId) -> PathBuf;
}

impl AgentDir for FsStorage {
    fn agent_dir(&self, id: AgentId) -> PathBuf {
        FsStorage::agent_dir(self, id)
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

/// The runner's say on who is sealed and with what
pub trait Enrolment: Send + Sync {
    /// Whether the agent opted in (the consent ledger)
    fn opted_in(&self, id: AgentId) -> bool;
    /// Its keys, from the runner's own secrets
    fn keys(&self, id: AgentId) -> Option<SealKeys>;
}

impl<E: Enrolment> Enrolment for std::sync::Arc<E> {
    fn opted_in(&self, id: AgentId) -> bool {
        (**self).opted_in(id)
    }

    fn keys(&self, id: AgentId) -> Option<SealKeys> {
        (**self).keys(id)
    }
}

/// Told when an entry is committed, to wake a drainer. Never does network
/// work itself: it is called under the reactor's storage lock
pub trait Outbox: Send + Sync {
    fn notify(&self, _id: AgentId) {}
}

impl Outbox for () {}

/// The current keyframe, for the next delta
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyframeRef {
    pub entry_hash: Sha256Hex,
    pub blob_sha256: Sha256Hex,
    pub key_gen: u32,
    /// Deltas written against it
    pub deltas: u32,
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

/// Something sealed failed. The local write never happens without its seal
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
    #[error("not a sealed-state version: {0:?}")]
    BadVersion(String),
    #[error("a genesis needs an empty chain, and anything else a parent")]
    BadParent,
}

/// What [`Append`] adds
#[derive(Debug, Clone)]
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
    /// The local state matches neither the head nor a pending item
    Mismatch(HeadRef),
}

/// One agent's `sealed/` directory
#[derive(Debug, Clone)]
pub struct AgentSeal {
    agent_id: AgentId,
    dir: PathBuf,
}

/// `seq:hash`
pub fn version_of(head: &HeadRef) -> Version {
    Version::new(format!("{}:{}", head.seq, head.entry_hash))
}

/// The inverse of [`version_of`]
pub fn head_of(version: &Version) -> Result<HeadRef, SealError> {
    let bad = || SealError::BadVersion(version.as_str().to_owned());
    let (seq, hash) = version.as_str().split_once(':').ok_or_else(bad)?;
    Ok(HeadRef {
        seq: seq.parse().map_err(|_| bad())?,
        entry_hash: hash.parse().map_err(|_| bad())?,
    })
}

fn sha256(bytes: &[u8]) -> Sha256Hex {
    Sha256Hex::from(<[u8; 32]>::from(Sha256::digest(bytes)))
}

/// The local-only hash of a state's sealed plaintext
pub fn plaintext_hash(state: &serde_json::Value) -> Sha256Hex {
    sha256(&codec::plaintext(state))
}

async fn exists(path: &Path) -> std::io::Result<bool> {
    tokio::fs::try_exists(path).await
}

/// tmp, fsync, rename, directory fsync
async fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
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

    pub async fn is_enrolled(&self) -> std::io::Result<bool> {
        exists(&self.dir.join(ENROLLED)).await
    }

    pub async fn is_refused(&self) -> std::io::Result<bool> {
        exists(&self.dir.join(REFUSED)).await
    }

    /// Mark enrolment done: uploads and sealed saves start
    pub async fn enroll(&self) -> std::io::Result<()> {
        write_atomic(&self.dir.join(ENROLLED), b"").await
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
    pub async fn mark_sent(&self, ord: u64) -> std::io::Result<()> {
        let sent = self.dir.join(SENT);
        tokio::fs::create_dir_all(&sent).await?;
        tokio::fs::rename(
            self.dir.join(OUTBOX).join(ord_name(ord, "json")),
            sent.join(ord_name(ord, "json")),
        )
        .await?;
        sync_dir(&sent).await?;
        sync_dir(&self.dir.join(OUTBOX)).await
    }

    /// The plaintext of the current keyframe, if it opens
    async fn keyframe_plaintext(
        &self,
        kf: &KeyframeRef,
        keys: &SealKeys,
    ) -> Option<Vec<u8>> {
        if kf.key_gen != keys.key_gen {
            return None;
        }
        let sealed = self.read_blob(&kf.blob_sha256).await.ok()?;
        let compressed = open_blob(
            &keys.state_key,
            self.agent_id,
            kf.key_gen,
            &BlobEncoding::Keyframe,
            &sealed,
        )
        .ok()?;
        codec::decompress(&compressed, None).ok()
    }

    /// Step 1: seal, sign and write `outbox/<ord>.tmp`
    pub async fn prepare(
        &self,
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

        let current = match (&head, displaced) {
            (Some(h), None) if append.kind == StateEntryKind::Snapshot => {
                h.keyframe
            }
            _ => None,
        };
        let mut keyframe_plaintext = None;
        if let Some(kf) = &current {
            keyframe_plaintext = self.keyframe_plaintext(kf, keys).await;
        }
        let encoded = match (&current, &keyframe_plaintext) {
            (Some(kf), Some(pt)) => policy.encode(
                append.plaintext,
                Some(Keyframe {
                    plaintext: pt,
                    deltas: kf.deltas,
                }),
            ),
            _ => Encoded::Keyframe(codec::compress(append.plaintext, None)),
        };
        let (encoding, compressed) = match encoded {
            Encoded::Keyframe(c) => (BlobEncoding::Keyframe, c),
            Encoded::Delta(c) => (
                BlobEncoding::Delta {
                    base: current.expect("a delta has a keyframe").entry_hash,
                },
                c,
            ),
        };
        let sealed = seal_blob(
            &keys.state_key,
            self.agent_id,
            keys.key_gen,
            &encoding,
            &compressed,
        );
        let blob_sha256 = sha256(&sealed);
        let blob_path = self.dir.join(BLOBS).join(blob_sha256.to_hex());
        if !exists(&blob_path).await? {
            write_atomic(&blob_path, &sealed).await?;
        }

        let now = Utc::now();
        let entry = StateEntry {
            agora_state_entry: STATE_ENTRY_VERSION,
            agent_id: self.agent_id,
            seq: append.parent.map_or(0, |p| p.seq + 1),
            created_at: now.timestamp_micros(),
            state_at: append.state_at.timestamp_micros(),
            kind: append.kind,
            prev_hash: append.parent.map(|p| p.entry_hash),
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
            head.as_ref().and_then(|h| h.keyframe)
        } else {
            Some(match encoding {
                BlobEncoding::Keyframe => KeyframeRef {
                    entry_hash: entry.entry_hash(),
                    blob_sha256,
                    key_gen: keys.key_gen,
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
            // A save whose local write never came
            tokio::fs::rename(&tmp, outbox.join(ord_name(ord, "abandoned")))
                .await?;
        }
        write_atomic(&tmp, &serde_json::to_vec_pretty(&item)?).await?;
        Ok(Prepared { item })
    }

    /// Step 3: rename the item into the outbox and move the head
    pub async fn commit(
        &self,
        prepared: &Prepared,
    ) -> Result<HeadRef, SealError> {
        self.commit_item(&prepared.item).await
    }

    async fn commit_item(
        &self,
        item: &OutboxItem,
    ) -> Result<HeadRef, SealError> {
        let outbox = self.dir.join(OUTBOX);
        let tmp = outbox.join(ord_name(item.ord, "tmp"));
        if exists(&tmp).await? {
            tokio::fs::rename(&tmp, outbox.join(ord_name(item.ord, "json")))
                .await?;
            sync_dir(&outbox).await?;
        }
        let old = self.head().await?;
        let head = HeadRef {
            entry_hash: item.entry.entry_hash(),
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
            keyframe: item.keyframe_after,
            next_ord: item.ord + 1,
            displaced,
        };
        write_atomic(&self.dir.join(HEAD), &serde_json::to_vec_pretty(&file)?)
            .await?;
        Ok(head)
    }

    /// At start: compare the local state's hash with the head's, and
    /// commit the newest uncommitted item that matches it
    pub async fn recover(
        &self,
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
        match pending
            .into_iter()
            .find(|i| i.plaintext_sha256.as_ref() == Some(local))
        {
            Some(item) => {
                Ok(Recovery::Committed(self.commit_item(&item).await?))
            }
            None => Ok(Recovery::Mismatch(head.head)),
        }
    }

    /// Start a chain with its genesis
    pub async fn genesis(
        &self,
        keys: &SealKeys,
        origin: &GenesisOrigin,
        state_at: DateTime<Utc>,
    ) -> Result<HeadRef, SealError> {
        let plaintext = canonical_json(&serde_json::to_value(origin)?);
        let prepared = self
            .prepare(
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
        self.commit(&prepared).await
    }

    /// Append a state with no local write of its own: a backfilled
    /// archive, an `archived_prior` copy, or the current state at
    /// enrolment
    pub async fn append_state(
        &self,
        keys: &SealKeys,
        policy: &KeyframePolicy,
        kind: StateEntryKind,
        state: &serde_json::Value,
        state_at: DateTime<Utc>,
    ) -> Result<HeadRef, SealError> {
        let head = self.head().await?.ok_or(SealError::NoHead)?;
        let plaintext = codec::plaintext(state);
        let plaintext_sha256 =
            (kind == StateEntryKind::Snapshot).then(|| sha256(&plaintext));
        let prepared = self
            .prepare(
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
        self.commit(&prepared).await
    }

    /// Open the state a sealed snapshot holds, for a restore or a check
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
        let plaintext =
            self.open_plaintext(keys, entry, dict.as_deref()).await?;
        let mut envelope: serde_json::Value =
            serde_json::from_slice(&plaintext)?;
        Ok(envelope.get_mut("state").ok_or(OpenError::NoBlob)?.take())
    }

    async fn open_plaintext(
        &self,
        keys: &SealKeys,
        entry: &StateEntry,
        dict: Option<&[u8]>,
    ) -> Result<Vec<u8>, OpenError> {
        let blob = entry.blob.as_ref().ok_or(OpenError::NoBlob)?;
        if entry.key_gen != keys.key_gen {
            return Err(OpenError::KeyGen(entry.key_gen));
        }
        let sealed = self.read_blob(&blob.sha256).await?;
        let compressed = open_blob(
            &keys.state_key,
            self.agent_id,
            entry.key_gen,
            &blob.encoding,
            &sealed,
        )?;
        Ok(codec::decompress(&compressed, dict)?)
    }
}

/// A sealed state that would not open
#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("seal: {0}")]
    Seal(#[from] crate::envelope::EnvelopeError),
    #[error("{0}")]
    Codec(#[from] codec::CodecError),
    #[error("the entry has no blob")]
    NoBlob,
    #[error("a delta needs its own keyframe")]
    WrongBase,
    #[error("sealed under key generation {0}, which isn't held")]
    KeyGen(u32),
}

/// [`Mirrored`]'s errors
#[derive(Debug, thiserror::Error)]
pub enum MirroredError<E> {
    #[error(transparent)]
    Local(E),
    #[error(transparent)]
    Seal(#[from] SealError),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    NotFound(#[from] AgentNotFound),
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

/// A local [`Storage`] whose saves are sealed first, for agents the
/// [`Enrolment`] opted in that are enrolled and not refused. Loads stay
/// local; uploads are the [`Outbox`]'s, never inside a save
pub struct Mirrored<L, O, E> {
    local: L,
    outbox: O,
    enrolment: E,
    policy: KeyframePolicy,
}

impl<L, O, E> Mirrored<L, O, E> {
    pub fn new(local: L, outbox: O, enrolment: E) -> Self {
        Self {
            local,
            outbox,
            enrolment,
            policy: KeyframePolicy::default(),
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
}

impl<L: AgentDir, O, E> Mirrored<L, O, E> {
    /// The agent's `sealed/` directory
    pub fn seal(&self, id: AgentId) -> AgentSeal {
        AgentSeal::new(&self.local.agent_dir(id), id)
    }
}

impl<L: AgentDir, O, E: Enrolment> Mirrored<L, O, E> {
    /// Whether a save of `id` is sealed
    async fn sealing(
        &self,
        id: AgentId,
    ) -> Result<Option<AgentSeal>, SealError> {
        if !self.enrolment.opted_in(id) {
            return Ok(None);
        }
        let seal = self.seal(id);
        if !seal.is_enrolled().await? {
            return Ok(None);
        }
        if seal.is_refused().await? {
            return Err(SealError::Refused);
        }
        Ok(Some(seal))
    }
}

#[async_trait::async_trait]
impl<L, O, E> Storage for Mirrored<L, O, E>
where
    L: Storage + AgentDir,
    O: Outbox,
    E: Enrolment,
{
    type Error = MirroredError<L::Error>;

    async fn save_raw(
        &mut self,
        id: AgentId,
        value: serde_json::Value,
        base: Option<&Version>,
    ) -> Result<Option<Version>, Self::Error> {
        let Some(seal) = self.sealing(id).await? else {
            return self
                .local
                .save_raw(id, value, base)
                .await
                .map_err(MirroredError::Local);
        };
        let head = seal.head().await?.ok_or(SealError::NoHead)?;
        let plaintext = codec::plaintext(&value);
        let plaintext_sha256 = sha256(&plaintext);
        if head.plaintext_sha256 == Some(plaintext_sha256) {
            self.local
                .save_raw(id, value, None)
                .await
                .map_err(MirroredError::Local)?;
            return Ok(Some(version_of(&head.head)));
        }
        let parent = match base {
            Some(base) => head_of(base)?,
            None => {
                tracing::warn!(
                    agent_id = %id,
                    "sealed save without a base; parented on the local head"
                );
                head.head
            }
        };
        let keys = self.enrolment.keys(id).ok_or(SealError::NoKeys)?;
        let prepared = seal
            .prepare(
                &keys,
                &self.policy,
                Append {
                    kind: StateEntryKind::Snapshot,
                    plaintext: &plaintext,
                    plaintext_sha256: Some(plaintext_sha256),
                    state_at: Utc::now(),
                    parent: Some(parent),
                },
            )
            .await?;
        self.local
            .save_raw(id, value, None)
            .await
            .map_err(MirroredError::Local)?;
        let new = seal.commit(&prepared).await?;
        if prepared.item.displaced.is_some() {
            tracing::warn!(
                agent_id = %id,
                base = %parent.entry_hash,
                displaced = %head.head.entry_hash,
                "sealed save forked from an older head; held for a merge"
            );
        }
        self.outbox.notify(id);
        Ok(Some(version_of(&new)))
    }

    async fn load_raw(&self, id: AgentId) -> Result<Loaded, Self::Error> {
        let Loaded { value, .. } = self
            .local
            .load_raw(id)
            .await
            .map_err(MirroredError::Local)?;
        if !self.enrolment.opted_in(id) {
            return Ok(Loaded { value, base: None });
        }
        let seal = self.seal(id);
        let base = match seal.recover(Some(&plaintext_hash(&value))).await? {
            Recovery::Intact(h) | Recovery::Committed(h) => {
                Some(version_of(&h))
            }
            // The runner's start check decides; the base is the head's
            Recovery::Mismatch(h) | Recovery::NoLocalState(h) => {
                Some(version_of(&h))
            }
            Recovery::NotEnrolled | Recovery::NoHead => None,
        };
        Ok(Loaded { value, base })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::generate_keypair;
    use crate::state::verify::{TrustedKeys, verify_chain};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    /// A signing key and a state key, raw
    type KeyBytes = ([u8; 32], [u8; 32]);

    /// Every agent opted in, with fixed keys
    #[derive(Clone, Default)]
    struct Keys {
        keys: Arc<Mutex<HashMap<AgentId, KeyBytes>>>,
        out: Arc<Mutex<Vec<AgentId>>>,
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
        fn opted_in(&self, id: AgentId) -> bool {
            !self.out.lock().unwrap().contains(&id)
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

    type Store = Mirrored<FsStorage, (), Keys>;

    /// An enrolled agent: genesis and its first state, sealed
    async fn enrolled(
        dir: &Path,
    ) -> (Store, AgentId, crate::crypto::VerifyingKey) {
        let keys = Keys::default();
        let id = AgentId::new();
        let vk = keys.add(id);
        let store = Mirrored::new(FsStorage::new(dir), (), keys.clone());
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
        seal.enroll().await.unwrap();
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

    #[tokio::test]
    async fn an_agent_not_enrolled_or_not_opted_in_is_never_sealed() {
        let dir = tempfile::tempdir().unwrap();
        let keys = Keys::default();
        let id = AgentId::new();
        keys.add(id);
        let mut store =
            Mirrored::new(FsStorage::new(dir.path()), (), keys.clone());
        assert_eq!(store.save_raw(id, state(1), None).await.unwrap(), None);
        assert!(!exists(&store.seal(id).dir).await.unwrap());

        let (mut store, id, _) = enrolled(dir.path()).await;
        store.enrolment.out.lock().unwrap().push(id);
        let before = store.seal(id).outbox().await.unwrap().len();
        assert_eq!(store.save_raw(id, state(50), None).await.unwrap(), None);
        assert_eq!(store.seal(id).outbox().await.unwrap().len(), before);
        assert_eq!(store.load_raw(id).await.unwrap().base, None);
    }

    #[tokio::test]
    async fn a_refused_agent_is_not_saved_at_all() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, id, _) = enrolled(dir.path()).await;
        store.seal(id).refuse("wiped").await.unwrap();
        assert!(matches!(
            store.save_raw(id, state(99), None).await,
            Err(MirroredError::Seal(SealError::Refused))
        ));
        assert_eq!(store.local.load_raw(id).await.unwrap().value, state(10));
        store.seal(id).clear_refusal().await.unwrap();
        store.save_raw(id, state(99), None).await.unwrap();
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
        let (h1, h2) = (head_of(&first).unwrap(), head_of(&second).unwrap());
        assert_eq!(h1.seq, h2.seq, "siblings");
        let head = store.seal(id).head().await.unwrap().unwrap();
        assert_eq!(head.head, h2);
        assert_eq!(head.displaced, vec![h1]);
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
            let prepared = seal
                .prepare(
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
}
