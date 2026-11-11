//! [`AgoraOutbox`]: uploads each agent's committed entries in order, away
//! from the reactor's storage lock

use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::storage::{AgentSeal, Enrolment, Held, Outbox, SealError};
use super::verify::{HeadStatus, head_status};
use crate::client::{Client, Error as ClientError, StateAuth, state_conflict};
use crate::govlog::Sha256Hex;
use crate::ids::AgentId;

const STATUS: &str = "status.json";

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
    /// [`upload`](Enrolment::uploads), so nothing of its chain is on Agora
    /// from this drain
    LocalOnly,
}

/// What a [`Drained::Ran`] uploaded, and how it left the agent
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrainReport {
    pub uploaded: usize,
    pub status: HeadStatus,
    pub waiting: usize,
    pub held: Held,
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
    /// [`upload`](Enrolment::uploads)
    pub async fn drain(&self, id: AgentId) -> Result<Drained, DrainError> {
        if !self.enrolment.uploads(id) {
            return Ok(Drained::LocalOnly);
        }
        self.upload(id).await.map(Drained::Ran)
    }

    async fn upload(&self, id: AgentId) -> Result<DrainReport, DrainError> {
        let seal = self.seal(id);
        if !seal.is_enrolled().await? {
            return Ok(DrainReport {
                uploaded: 0,
                status: HeadStatus::Same,
                waiting: 0,
                held: Held::default(),
            });
        }
        let keys = self.enrolment.keys(id).ok_or(DrainError::NoKeys)?;
        let auth = StateAuth {
            agent_id: id,
            key: &keys.signing,
        };
        let items = seal.outbox().await?;
        let mut uploaded = 0;
        for (i, item) in items.iter().enumerate() {
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
                            .finish(&seal, uploaded, status, rest.len())
                            .await;
                    }
                    // Taken before, unacknowledged
                }
                Err(e) => return Err(e.into()),
            }
            seal.mark_sent(item.ord).await?;
            uploaded += 1;
        }
        let pruned = seal.prune_blobs().await?;
        tracing::debug!(agent_id = %id, uploaded, pruned, "sealed outbox drained");
        self.finish(&seal, uploaded, HeadStatus::Same, 0).await
    }

    async fn finish(
        &self,
        seal: &AgentSeal,
        uploaded: usize,
        status: HeadStatus,
        waiting: usize,
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
            Drained::LocalOnly => panic!("drained as local-only"),
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
        // Only the current keyframe's blob stays; the genesis blob went
        let kf = seal.head().await.unwrap().unwrap().keyframe.unwrap();
        let mut left = Vec::new();
        let mut rd =
            tokio::fs::read_dir(seal.dir().join("blobs")).await.unwrap();
        while let Some(e) = rd.next_entry().await.unwrap() {
            left.push(e.file_name().into_string().unwrap());
        }
        assert_eq!(left, vec![kf.blob_sha256().unwrap().to_hex()]);
        // Its siblings untouched
        for f in ["head.json", "enrolled"] {
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
}
