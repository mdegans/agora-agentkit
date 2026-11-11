//! [`AgoraOutbox`]: uploads each agent's committed entries in order, away
//! from the reactor's storage lock

use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::storage::{
    AgentSeal, Enrolment, Held, Outbox, OutboxItem, SealError,
};
use super::verify::{HeadStatus, head_status};
use crate::client::{Client, Error as ClientError, StateAuth, state_conflict};
use crate::govlog::Sha256Hex;
use crate::ids::AgentId;

const STATUS: &str = "status.json";

/// Damaged blobs one drain sends again, at most
pub const HEAL_PER_DRAIN: usize = 64;

/// `sealed/status.json`: how the last drain left the agent
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DrainStatus {
    pub status: HeadStatus,
    pub at: DateTime<Utc>,
    /// Items still in the outbox
    pub waiting: usize,
    /// What `sealed/` holds back, for an alert while a fork waits
    #[serde(default)]
    pub held: Held,
}

/// What one [`AgoraOutbox::drain`] did
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Drained {
    Ran(DrainReport),
    /// Nothing was read or sent: the agent doesn't
    /// [`upload`](Enrolment::uploads). Checked first
    LocalOnly,
    /// Nothing was sent: no `sealed/chained`, so no chain
    NotEnrolled,
    /// [`uploads`](Enrolment::uploads) turned false mid-drain: `uploaded`
    /// items went before it, and the rest wait in the outbox, as after a
    /// network error
    Withdrawn {
        uploaded: usize,
    },
}

/// Called once by a [`drain_with`](AgoraOutbox::drain_with) that is about
/// to upload, so a caller can record what it sends
#[async_trait::async_trait]
pub trait BeforeUpload: Send {
    /// After the drain's [`uploads`](Enrolment::uploads) check passed for
    /// the first item, before its first network call, with every item it
    /// will try in order. Never called when there is nothing to send
    async fn before_upload(&mut self, items: &[OutboxItem]);
}

#[async_trait::async_trait]
impl BeforeUpload for () {
    async fn before_upload(&mut self, _: &[OutboxItem]) {}
}

/// What a [`Drained::Ran`] uploaded, and how it left the agent
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrainReport {
    pub uploaded: usize,
    pub status: HeadStatus,
    pub waiting: usize,
    pub held: Held,
    pub healing: Healing,
}

/// The blobs Agora's check at rest found missing or damaged, and what a
/// drain did about them
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Healing {
    /// As Agora counted them before this drain; `None` when it didn't say
    /// (a server before the check, a held drain, or the head unreadable)
    pub damaged: Option<u64>,
    /// Sent again from here, whole
    pub healed: usize,
    /// Not here, or here but not hashing to its name: nothing to send
    pub unhealable: usize,
}

/// A drain that stopped; the items stay where they are
#[derive(Debug, thiserror::Error)]
pub enum DrainError {
    #[error(transparent)]
    Seal(#[from] SealError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Client(#[from] ClientError),
    #[error("no sealing keys for this agent")]
    NoKeys,
}

/// The [`Outbox`] that uploads to Agora. Clone it: one copy goes to
/// [`Mirrored`](super::storage::Mirrored), another runs the drain
#[derive(Clone)]
pub struct AgoraOutbox<E> {
    client: Client,
    /// The `FsStorage` root, `<root>/<agent_id>/sealed`
    root: PathBuf,
    enrolment: Arc<E>,
    wake: Arc<tokio::sync::Notify>,
}

impl<E> Outbox for AgoraOutbox<E>
where
    E: Send + Sync,
{
    fn notify(&self, _id: AgentId) {
        self.wake.notify_one();
    }
}

impl<E: Enrolment> AgoraOutbox<E> {
    pub fn new(
        client: Client,
        root: impl Into<PathBuf>,
        enrolment: Arc<E>,
    ) -> Self {
        Self {
            client,
            root: root.into(),
            enrolment,
            wake: Arc::new(tokio::sync::Notify::new()),
        }
    }

    /// Until an entry is committed
    pub async fn notified(&self) {
        self.wake.notified().await
    }

    fn seal(&self, id: AgentId) -> AgentSeal {
        AgentSeal::new(&self.root.join(id.to_string()), id)
    }

    /// Upload `id`'s outbox, oldest first: the blob, then the entry. On a
    /// `409` the rest is held and the status recorded. Never deletes, and
    /// touches nothing for an agent that doesn't
    /// [`upload`](Enrolment::uploads), which is asked again before each
    /// item
    pub async fn drain(&self, id: AgentId) -> Result<Drained, DrainError> {
        self.drain_with(id, &mut ()).await
    }

    /// [`drain`](Self::drain), telling `before` what it is about to send
    pub async fn drain_with<B: BeforeUpload + ?Sized>(
        &self,
        id: AgentId,
        before: &mut B,
    ) -> Result<Drained, DrainError> {
        if !self.enrolment.uploads(id) {
            return Ok(Drained::LocalOnly);
        }
        let seal = self.seal(id);
        if !seal.is_enrolled().await? {
            return Ok(Drained::NotEnrolled);
        }
        let keys = self.enrolment.keys(id).ok_or(DrainError::NoKeys)?;
        let auth = StateAuth {
            agent_id: id,
            key: &keys.signing,
        };
        let items = seal.outbox().await?;
        let mut uploaded = 0;
        for (i, item) in items.iter().enumerate() {
            if !self.enrolment.uploads(id) {
                tracing::info!(
                    agent_id = %id,
                    uploaded,
                    waiting = items.len() - i,
                    "sealed uploads stopped: uploading was switched off"
                );
                return Ok(Drained::Withdrawn { uploaded });
            }
            if i == 0 {
                before.before_upload(&items).await;
            }
            if let Some(blob) = &item.entry.entry.blob {
                let sealed = seal.read_blob(&blob.sha256).await?;
                self.client
                    .put_state_blob(auth, &blob.sha256, &sealed)
                    .await?;
            }
            match self.client.post_state_entry(auth, &item.entry).await {
                Ok(_) => {}
                Err(e) if state_conflict(&e).is_some() => {
                    let server: Vec<Sha256Hex> = self
                        .client
                        .get_state_chain(auth)
                        .await?
                        .iter()
                        .map(|e| e.entry_hash())
                        .collect();
                    if !server.contains(&item.entry.entry_hash()) {
                        let local =
                            seal.head().await?.map(|h| h.head.entry_hash);
                        let rest: Vec<_> = items[i..]
                            .iter()
                            .map(|i| i.entry.entry.clone())
                            .collect();
                        let status =
                            head_status(local.as_ref(), &rest, &server);
                        tracing::warn!(
                            agent_id = %id,
                            status = ?status,
                            waiting = rest.len(),
                            "sealed uploads held: the server's head moved on"
                        );
                        return self
                            .finish(
                                &seal,
                                uploaded,
                                status,
                                rest.len(),
                                Healing::default(),
                            )
                            .await
                            .map(Drained::Ran);
                    }
                    // Taken before, unacknowledged
                }
                Err(e) => return Err(e.into()),
            }
            seal.mark_sent(item.ord).await?;
            uploaded += 1;
        }
        let pruned = seal.remove_unnamed_blobs().await?;
        tracing::debug!(agent_id = %id, uploaded, pruned, "sealed outbox drained");
        let healing = match self.heal(id, &seal, auth).await {
            Ok(Some(healing)) => healing,
            Ok(None) => return Ok(Drained::Withdrawn { uploaded }),
            // The uploads stand; the next drain looks again
            Err(e) => {
                tracing::warn!(
                    agent_id = %id,
                    error = %e,
                    "damaged sealed blobs not checked this drain"
                );
                Healing::default()
            }
        };
        self.finish(&seal, uploaded, HeadStatus::Same, 0, healing)
            .await
            .map(Drained::Ran)
    }

    /// Send again, up to [`HEAL_PER_DRAIN`], the blobs Agora's check at
    /// rest found missing or damaged that are here and hash to their names.
    /// `None` when [`uploads`](Enrolment::uploads) turned false
    async fn heal(
        &self,
        id: AgentId,
        seal: &AgentSeal,
        auth: StateAuth<'_>,
    ) -> Result<Option<Healing>, DrainError> {
        if !self.enrolment.uploads(id) {
            return Ok(None);
        }
        let damaged = self.client.get_state_head(auth).await?.damaged_blobs;
        let mut healing = Healing {
            damaged,
            ..Healing::default()
        };
        if damaged.unwrap_or(0) == 0 {
            return Ok(Some(healing));
        }
        let problems = self.client.get_state_problems(auth).await?;
        for blob in problems.damaged_blobs.iter().take(HEAL_PER_DRAIN) {
            if !self.enrolment.uploads(id) {
                return Ok(None);
            }
            let bytes = match seal.read_blob(&blob.sha256).await {
                Ok(bytes) => bytes,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    healing.unhealable += 1;
                    continue;
                }
                Err(e) => return Err(e.into()),
            };
            let hashed = Sha256Hex::from(<[u8; 32]>::from(
                <sha2::Sha256 as sha2::Digest>::digest(&bytes),
            ));
            if hashed != blob.sha256 {
                tracing::error!(
                    agent_id = %id,
                    sha256 = %blob.sha256,
                    "a sealed blob here doesn't hash to its name; not sent"
                );
                healing.unhealable += 1;
                continue;
            }
            self.client
                .put_state_blob(auth, &blob.sha256, &bytes)
                .await?;
            healing.healed += 1;
        }
        tracing::warn!(
            agent_id = %id,
            damaged = damaged.unwrap_or(0),
            healed = healing.healed,
            unhealable = healing.unhealable,
            "blobs Agora found damaged at rest; whole copies sent again"
        );
        Ok(Some(healing))
    }

    async fn finish(
        &self,
        seal: &AgentSeal,
        uploaded: usize,
        status: HeadStatus,
        waiting: usize,
        healing: Healing,
    ) -> Result<DrainReport, DrainError> {
        let held = seal.held().await?;
        if status != HeadStatus::Same {
            tracing::error!(
                agent_id = %seal.agent_id(),
                status = ?status,
                outbox_items = held.outbox_items,
                blobs = held.blobs,
                blob_bytes = held.blob_bytes,
                forks = held.displaced,
                "sealed uploads held; an operator resolves it (sealed-accept)"
            );
        }
        let file = DrainStatus {
            status,
            at: Utc::now(),
            waiting,
            held,
        };
        super::storage::write_atomic(
            &seal.dir().join(STATUS),
            &serde_json::to_vec_pretty(&file).map_err(SealError::from)?,
        )
        .await?;
        Ok(DrainReport {
            uploaded,
            status,
            waiting,
            held,
            healing,
        })
    }

    /// The last [`drain`](Self::drain)'s status, if any
    pub async fn status(
        &self,
        id: AgentId,
    ) -> Result<Option<DrainStatus>, DrainError> {
        match tokio::fs::read(self.seal(id).dir().join(STATUS)).await {
            Ok(bytes) => Ok(Some(
                serde_json::from_slice(&bytes).map_err(SealError::from)?,
            )),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::SigningKey;
    use crate::enums::StateEntryKind;
    use crate::state::SignedStateEntry;
    use crate::state::codec::KeyframePolicy;
    use crate::state::crypto::StateKey;
    use crate::state::storage::SealKeys;
    use crate::state::verify::{TrustedKeys, verify_chain};
    use crate::state::{GenesisClass, GenesisOrigin};
    use httpmock::prelude::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// One agent's keys, and whether it uploads
    struct One {
        signing: [u8; 32],
        state: [u8; 32],
        uploads: AtomicBool,
        key_reads: AtomicUsize,
    }

    impl Enrolment for One {
        fn uploads(&self, _: AgentId) -> bool {
            self.uploads.load(Ordering::SeqCst)
        }

        fn keys(&self, _: AgentId) -> Option<SealKeys> {
            self.key_reads.fetch_add(1, Ordering::SeqCst);
            Some(SealKeys {
                signing: SigningKey::from_bytes(&self.signing),
                state_key: StateKey::from_bytes(zeroize::Zeroizing::new(
                    self.state,
                )),
                key_gen: 0,
            })
        }
    }

    async fn setup(
        server: &MockServer,
    ) -> (tempfile::TempDir, AgoraOutbox<One>, AgentId) {
        setup_uploading(server, true).await
    }

    /// An enrolled agent with its genesis and one state in the outbox
    async fn setup_uploading(
        server: &MockServer,
        uploads: bool,
    ) -> (tempfile::TempDir, AgoraOutbox<One>, AgentId) {
        let dir = tempfile::tempdir().unwrap();
        let id = AgentId::new();
        let enrolment = Arc::new(One {
            signing: [3; 32],
            state: *StateKey::generate().expose(),
            uploads: AtomicBool::new(uploads),
            key_reads: AtomicUsize::new(0),
        });
        let seal = AgentSeal::new(&dir.path().join(id.to_string()), id);
        let keys = enrolment.keys(id).unwrap();
        let origin = GenesisOrigin {
            class: GenesisClass::Unknown,
            incident: None,
            prior_copy_held: false,
            verified_at: None,
        };
        seal.genesis(&keys, &origin, Utc::now()).await.unwrap();
        seal.append_state(
            &keys,
            &KeyframePolicy::default(),
            StateEntryKind::Snapshot,
            &serde_json::json!({ "memory": "m" }),
            Utc::now(),
        )
        .await
        .unwrap();
        seal.enroll().await.unwrap();
        let client =
            Client::new(url::Url::parse(&server.base_url()).unwrap()).unwrap();
        let outbox = AgoraOutbox::new(client, dir.path(), enrolment);
        (dir, outbox, id)
    }

    fn ran(drained: Drained) -> DrainReport {
        match drained {
            Drained::Ran(report) => report,
            other => panic!("{other:?}"),
        }
    }

    async fn blob_count(seal: &AgentSeal) -> usize {
        let mut n = 0;
        let mut rd =
            tokio::fs::read_dir(seal.dir().join("blobs")).await.unwrap();
        while rd.next_entry().await.unwrap().is_some() {
            n += 1;
        }
        n
    }

    fn head_json() -> serde_json::Value {
        serde_json::json!({
            "id": uuid::Uuid::nil(),
            "head": { "entry_hash": "00".repeat(32), "seq": 0, "updated_at": "2026-11-02T00:00:00Z" },
        })
    }

    #[tokio::test]
    async fn the_outbox_drains_in_order_and_moves_to_sent() {
        let server = MockServer::start_async().await;
        let (_dir, outbox, id) = setup(&server).await;
        let blobs = server
            .mock_async(|when, then| {
                when.method(PUT).path_includes("/api/state/blobs/");
                then.status(200).json_body(serde_json::json!({
                    "id": uuid::Uuid::nil(), "sha256": "00".repeat(32), "size_bytes": 1,
                }));
            })
            .await;
        let entries = server
            .mock_async(|when, then| {
                when.method(POST).path("/agora/api/state/entries");
                then.status(200).json_body(head_json());
            })
            .await;
        let report = ran(outbox.drain(id).await.unwrap());
        assert_eq!(report.uploaded, 2);
        assert_eq!(report.status, HeadStatus::Same);
        blobs.assert_calls_async(2).await;
        entries.assert_calls_async(2).await;
        let seal = outbox.seal(id);
        assert!(seal.outbox().await.unwrap().is_empty());
        assert_eq!(seal.sent().await.unwrap().len(), 2);
        // Every sent version's blob stays
        for item in seal.sent().await.unwrap() {
            let sha = item.entry.entry.blob.unwrap().sha256;
            assert!(seal.read_blob(&sha).await.is_ok());
        }
        assert_eq!(blob_count(&seal).await, 2);
        // Its siblings untouched
        for f in ["head.json", "chained"] {
            assert!(seal.dir().join(f).exists(), "{f}");
        }
        assert_eq!(
            outbox.status(id).await.unwrap().unwrap().status,
            HeadStatus::Same
        );
    }

    #[tokio::test]
    async fn a_conflict_holds_the_rest_and_keeps_it() {
        let server = MockServer::start_async().await;
        let (_dir, outbox, id) = setup(&server).await;
        server
            .mock_async(|when, then| {
                when.method(PUT);
                then.status(200).json_body(serde_json::json!({
                    "id": uuid::Uuid::nil(), "sha256": "00".repeat(32), "size_bytes": 1,
                }));
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method(POST).path("/agora/api/state/entries");
                then.status(409).json_body(serde_json::json!({
                    "head": { "entry_hash": "11".repeat(32), "seq": 0, "updated_at": "2026-11-02T00:00:00Z" },
                }));
            })
            .await;
        // Someone else's genesis is the server's head
        let mut other =
            outbox.seal(id).outbox().await.unwrap()[0].entry.clone();
        other.entry.state_at += 1;
        let other = other.entry.sign(&SigningKey::from_bytes(&[3; 32]));
        let stored = serde_json::json!({
            "id": uuid::Uuid::nil(),
            "entry": other.entry,
            "signature": other.signature,
            "received_at": "2026-11-02T00:00:00Z",
        });
        server
            .mock_async(|when, then| {
                when.method(GET).path("/agora/api/state/entries");
                then.status(200).json_body(serde_json::json!({
                    "entries": [stored], "next_after": null,
                }));
            })
            .await;
        let report = ran(outbox.drain(id).await.unwrap());
        assert_eq!(report.uploaded, 0);
        assert_eq!(report.status, HeadStatus::Diverged);
        assert_eq!(report.waiting, 2);
        assert_eq!(report.held.outbox_items, 2);
        assert_eq!(report.held.blobs, 2);
        assert!(report.held.blob_bytes >= 2 * 1024);
        let status = outbox.status(id).await.unwrap().unwrap();
        assert_eq!(status.status, HeadStatus::Diverged);
        assert_eq!(status.held, report.held);
        assert_eq!(
            outbox.seal(id).outbox().await.unwrap().len(),
            2,
            "nothing lost"
        );
        // Blobs not acknowledged are all kept
        let mut n = 0;
        let mut rd = tokio::fs::read_dir(outbox.seal(id).dir().join("blobs"))
            .await
            .unwrap();
        while rd.next_entry().await.unwrap().is_some() {
            n += 1;
        }
        assert_eq!(n, 2);
    }

    /// An agent that doesn't upload: no key read, no call made, nothing
    /// moved or written, and an outcome of its own
    #[tokio::test]
    async fn a_local_only_agent_drains_nothing() {
        let server = MockServer::start_async().await;
        let any = server
            .mock_async(|_, then| {
                then.status(500);
            })
            .await;
        let (_dir, outbox, id) = setup_uploading(&server, false).await;
        let seal = outbox.seal(id);
        let items = seal.outbox().await.unwrap();
        let blobs = blob_count(&seal).await;
        let key_reads = outbox.enrolment.key_reads.load(Ordering::SeqCst);

        assert_eq!(outbox.drain(id).await.unwrap(), Drained::LocalOnly);
        any.assert_calls_async(0).await;
        assert_eq!(
            outbox.enrolment.key_reads.load(Ordering::SeqCst),
            key_reads,
            "no key read"
        );
        assert_eq!(seal.outbox().await.unwrap(), items);
        assert!(seal.sent().await.unwrap().is_empty());
        assert_eq!(blob_count(&seal).await, blobs);
        assert_eq!(outbox.status(id).await.unwrap(), None, "nothing written");
    }

    /// Chained while local-only, then uploading: the whole chain goes up
    /// in order and verifies as the server received it
    #[tokio::test]
    async fn uploads_switched_on_sends_the_whole_local_chain_in_order() {
        let server = MockServer::start_async().await;
        let (_dir, outbox, id) = setup_uploading(&server, false).await;
        let seal = outbox.seal(id);
        let keys = outbox.enrolment.keys(id).unwrap();
        for n in 0..4 {
            seal.append_state(
                &keys,
                &KeyframePolicy::default(),
                StateEntryKind::Snapshot,
                &serde_json::json!({ "memory": format!("m{n}").repeat(200) }),
                Utc::now(),
            )
            .await
            .unwrap();
        }
        assert_eq!(outbox.drain(id).await.unwrap(), Drained::LocalOnly);
        let local: Vec<_> = seal
            .outbox()
            .await
            .unwrap()
            .into_iter()
            .map(|i| i.entry)
            .collect();
        assert_eq!(local.len(), 6, "genesis, a state, four more");

        outbox.enrolment.uploads.store(true, Ordering::SeqCst);
        server
            .mock_async(|when, then| {
                when.method(PUT).path_includes("/api/state/blobs/");
                then.status(200).json_body(serde_json::json!({
                    "id": uuid::Uuid::nil(), "sha256": "00".repeat(32), "size_bytes": 1,
                }));
            })
            .await;
        #[derive(serde::Deserialize)]
        struct Posted {
            entry: SignedStateEntry,
        }
        let posted = Arc::new(Mutex::new(Vec::new()));
        let record = posted.clone();
        server
            .mock_async(move |when, then| {
                when.method(POST).path("/agora/api/state/entries").is_true(
                    move |req| {
                        let p: Posted =
                            serde_json::from_slice(req.body_ref()).unwrap();
                        record.lock().unwrap().push(p.entry);
                        true
                    },
                );
                then.status(200).json_body(head_json());
            })
            .await;
        let report = ran(outbox.drain(id).await.unwrap());
        assert_eq!(report.uploaded, 6);
        assert_eq!(report.status, HeadStatus::Same);
        let posted = posted.lock().unwrap().clone();
        assert_eq!(posted, local, "every entry, in order");
        let vk = SigningKey::from_bytes(&[3; 32]).verifying_key();
        let report = verify_chain(id, &posted, &TrustedKeys::new(vk));
        assert!(report.intact, "{:?}", report.problems);
    }

    /// Appends `n` more states to `id`'s chain
    async fn more(outbox: &AgoraOutbox<One>, id: AgentId, n: usize) {
        let keys = outbox.enrolment.keys(id).unwrap();
        for i in 0..n {
            outbox
                .seal(id)
                .append_state(
                    &keys,
                    &KeyframePolicy::default(),
                    StateEntryKind::Snapshot,
                    &serde_json::json!({ "memory": format!("x{i}").repeat(100) }),
                    Utc::now(),
                )
                .await
                .unwrap();
        }
    }

    async fn accept_blobs(server: &MockServer) {
        server
            .mock_async(|when, then| {
                when.method(PUT).path_includes("/api/state/blobs/");
                then.status(200).json_body(serde_json::json!({
                    "id": uuid::Uuid::nil(), "sha256": "00".repeat(32), "size_bytes": 1,
                }));
            })
            .await;
    }

    /// Uploading switched off after the first item: that one is on the
    /// server and in `sent/`, the rest wait, and nothing else is written
    #[tokio::test]
    async fn a_withdrawal_stops_the_drain_at_the_next_item() {
        let server = MockServer::start_async().await;
        let (_dir, outbox, id) = setup(&server).await;
        more(&outbox, id, 2).await;
        accept_blobs(&server).await;
        let enrolment = outbox.enrolment.clone();
        let entries = server
            .mock_async(move |when, then| {
                when.method(POST).path("/agora/api/state/entries").is_true(
                    move |_| {
                        enrolment.uploads.store(false, Ordering::SeqCst);
                        true
                    },
                );
                then.status(200).json_body(head_json());
            })
            .await;
        assert_eq!(
            outbox.drain(id).await.unwrap(),
            Drained::Withdrawn { uploaded: 1 }
        );
        entries.assert_calls_async(1).await;
        let seal = outbox.seal(id);
        assert_eq!(seal.sent().await.unwrap().len(), 1);
        assert_eq!(seal.outbox().await.unwrap().len(), 3);
        assert_eq!(outbox.status(id).await.unwrap(), None);
    }

    #[tokio::test]
    async fn an_agent_without_the_marker_is_not_enrolled() {
        let server = MockServer::start_async().await;
        let any = server
            .mock_async(|_, then| {
                then.status(500);
            })
            .await;
        let (_dir, outbox, _) = setup(&server).await;
        let other = AgentId::new();
        assert_eq!(outbox.drain(other).await.unwrap(), Drained::NotEnrolled);
        any.assert_calls_async(0).await;
        assert!(!outbox.seal(other).dir().exists());
    }

    /// What a [`BeforeUpload`] saw, and how many entries had been posted
    /// when it did
    struct Seen {
        calls: Vec<(Vec<u64>, usize)>,
        posted: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl BeforeUpload for Seen {
        async fn before_upload(&mut self, items: &[OutboxItem]) {
            self.calls.push((
                items.iter().map(|i| i.ord).collect(),
                self.posted.load(Ordering::SeqCst),
            ));
        }
    }

    /// Told once, before any upload, with every item; never for a drain
    /// that sends nothing
    #[tokio::test]
    async fn before_upload_hears_only_of_a_drain_that_sends() {
        let server = MockServer::start_async().await;
        let (_dir, outbox, id) = setup_uploading(&server, false).await;
        more(&outbox, id, 1).await;
        accept_blobs(&server).await;
        let posted = Arc::new(AtomicUsize::new(0));
        let count = posted.clone();
        server
            .mock_async(move |when, then| {
                when.method(POST).path("/agora/api/state/entries").is_true(
                    move |_| {
                        count.fetch_add(1, Ordering::SeqCst);
                        true
                    },
                );
                then.status(200).json_body(head_json());
            })
            .await;
        let mut seen = Seen {
            calls: Vec::new(),
            posted: posted.clone(),
        };
        assert_eq!(
            outbox.drain_with(id, &mut seen).await.unwrap(),
            Drained::LocalOnly
        );
        assert!(seen.calls.is_empty());

        outbox.enrolment.uploads.store(true, Ordering::SeqCst);
        let ords: Vec<u64> = outbox
            .seal(id)
            .outbox()
            .await
            .unwrap()
            .iter()
            .map(|i| i.ord)
            .collect();
        assert_eq!(
            ran(outbox.drain_with(id, &mut seen).await.unwrap()).uploaded,
            3
        );
        assert_eq!(seen.calls, vec![(ords, 0)], "once, before any post");
        // Nothing left to send: not told
        ran(outbox.drain_with(id, &mut seen).await.unwrap());
        assert_eq!(
            outbox.drain_with(AgentId::new(), &mut seen).await.unwrap(),
            Drained::NotEnrolled
        );
        assert_eq!(seen.calls.len(), 1);
    }

    /// Agora's check at rest found three blobs damaged: one held here whole
    /// is sent again; one held here but not hashing to its name, and one
    /// not here, are unhealable and never sent. On a drain with nothing to
    /// send
    #[tokio::test]
    async fn damaged_blobs_held_whole_here_are_sent_again() {
        let server = MockServer::start_async().await;
        let (_dir, outbox, id) = setup(&server).await;
        let seal = outbox.seal(id);
        let items = seal.outbox().await.unwrap();
        let sha = |i: usize| items[i].entry.entry.blob.as_ref().unwrap().sha256;
        let (whole, rotten, absent) =
            (sha(0), sha(1), Sha256Hex::from([9; 32]));
        let put = |sha: Sha256Hex| {
            let server = &server;
            async move {
                server
                    .mock_async(|when, then| {
                        when.method(PUT).path_includes(sha.to_hex());
                        then.status(200).json_body(serde_json::json!({
                            "id": uuid::Uuid::nil(), "sha256": sha.to_hex(), "size_bytes": 1,
                        }));
                    })
                    .await
            }
        };
        let (put_whole, put_rotten, put_absent) =
            (put(whole).await, put(rotten).await, put(absent).await);
        server
            .mock_async(|when, then| {
                when.method(POST).path("/agora/api/state/entries");
                then.status(200).json_body(head_json());
            })
            .await;
        let head = server
            .mock_async(|when, then| {
                when.method(GET).path("/agora/api/state/head");
                then.status(200).json_body(serde_json::json!({
                    "head": null, "stored_bytes": 0,
                }));
            })
            .await;
        // An older server says nothing: nothing more is asked
        let report = ran(outbox.drain(id).await.unwrap());
        assert_eq!(report.uploaded, 2);
        assert_eq!(report.healing, Healing::default());
        head.delete_async().await;

        std::fs::write(seal.dir().join("blobs").join(rotten.to_hex()), b"rot")
            .unwrap();
        server
            .mock_async(|when, then| {
                when.method(GET).path("/agora/api/state/head");
                then.status(200).json_body(serde_json::json!({
                    "head": null, "stored_bytes": 0, "damaged_blobs": 3,
                }));
            })
            .await;
        let listed: Vec<_> = [
            (whole, "missing"),
            (rotten, "wrong_hash"),
            (absent, "wrong_size"),
        ]
        .iter()
        .map(|(sha, kind)| {
            serde_json::json!({
                "sha256": sha.to_hex(), "kind": kind, "since": "2026-11-11T00:00:00Z",
            })
        })
        .collect();
        let problems = server
            .mock_async(|when, then| {
                when.method(GET).path("/agora/api/state/problems");
                then.status(200)
                    .json_body(serde_json::json!({ "damaged_blobs": listed }));
            })
            .await;
        let report = ran(outbox.drain(id).await.unwrap());
        assert_eq!(report.uploaded, 0);
        assert_eq!(
            report.healing,
            Healing {
                damaged: Some(3),
                healed: 1,
                unhealable: 2,
            }
        );
        problems.assert_calls_async(1).await;
        put_whole.assert_calls_async(2).await;
        put_rotten.assert_calls_async(1).await;
        put_absent.assert_calls_async(0).await;
    }
}
