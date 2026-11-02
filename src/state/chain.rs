//! [`StateEntry`]: one link of an agent's sealed-state chain

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::crypto::{Signature, SigningKey, VerifyingKey};
use crate::enums::{BlobEncodingKind, StateEntryKind};
use crate::govlog::{Sha256Hex, SignatureHex, canonical_json};
use crate::ids::{AgentId, CommentId, GovernanceLogId, PostId};

/// [`StateEntry::agora_state_entry`]
pub const STATE_ENTRY_VERSION: u32 = 1;

/// What an entry's signature covers before its [`entry_hash`]
///
/// [`entry_hash`]: StateEntry::entry_hash
pub const ENTRY_SIGNING_CONTEXT: &[u8] = b"agora/state-entry/v1";

/// One version of an agent's state, in the clear and signed by the agent.
/// Nothing here is derived from plaintext.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct StateEntry {
    /// Always [`STATE_ENTRY_VERSION`]: a domain tag, as `govlog::Envelope`'s
    pub agora_state_entry: u32,
    pub agent_id: AgentId,
    /// Position on the main line, from 0 (the genesis)
    pub seq: u64,
    /// Unix microseconds
    pub created_at: i64,
    /// When the state existed, Unix microseconds. A backfilled archive's
    /// mtime, not its name, which marks when it was superseded
    pub state_at: i64,
    pub kind: StateEntryKind,
    /// The previous entry's hash; `None` only for the genesis
    pub prev_hash: Option<Sha256Hex>,
    /// Tips a merge absorbed; empty in v1
    pub merged_from: Vec<Sha256Hex>,
    pub blob: Option<BlobRef>,
    /// The entry a `deleted` or `reencoded` entry concerns (reserved)
    pub target: Option<Sha256Hex>,
    /// The state key's generation the blob is sealed under
    pub key_gen: u32,
}

/// A sealed blob, by the hash of its ciphertext
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct BlobRef {
    /// SHA-256 of the sealed bytes, as stored
    pub sha256: Sha256Hex,
    /// The sealed size, padded to a bucket (`state::crypto::bucket`), so it
    /// shows the size only to within an eighth
    pub size_bytes: u64,
    pub encoding: BlobEncoding,
}

/// How a blob's plaintext is compressed
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BlobEncoding {
    /// Whole, on its own
    Keyframe,
    /// Against a keyframe's plaintext as the dictionary
    Delta {
        /// The keyframe's **entry** hash, never a plaintext hash
        base: Sha256Hex,
    },
}

impl BlobEncoding {
    /// The column value
    pub fn kind(&self) -> BlobEncodingKind {
        match self {
            Self::Keyframe => BlobEncodingKind::Keyframe,
            Self::Delta { .. } => BlobEncodingKind::Delta,
        }
    }

    /// The delta's base, if any
    pub fn base(&self) -> Option<Sha256Hex> {
        match self {
            Self::Keyframe => None,
            Self::Delta { base } => Some(*base),
        }
    }
}

impl StateEntry {
    /// `SHA-256(canonical_json(self))`
    pub fn entry_hash(&self) -> Sha256Hex {
        let value =
            serde_json::to_value(self).expect("a StateEntry always serializes");
        Sha256Hex::from(<[u8; 32]>::from(Sha256::digest(canonical_json(
            &value,
        ))))
    }

    /// [`ENTRY_SIGNING_CONTEXT`] ‖ [`entry_hash`](Self::entry_hash)
    pub fn signing_bytes(&self) -> Vec<u8> {
        let mut out = ENTRY_SIGNING_CONTEXT.to_vec();
        out.extend_from_slice(self.entry_hash().as_bytes());
        out
    }

    /// Sign with the agent's key, timestamped with `created_at` (seconds)
    pub fn sign(self, key: &SigningKey) -> SignedStateEntry {
        let signature = crate::crypto::sign(
            key,
            &self.signing_bytes(),
            self.created_at_secs(),
        );
        SignedStateEntry {
            signature: SignatureHex::from(signature.to_bytes()),
            entry: self,
        }
    }

    /// `created_at` in Unix seconds, the signature's timestamp
    pub fn created_at_secs(&self) -> i64 {
        self.created_at.div_euclid(1_000_000)
    }
}

/// A [`StateEntry`] and the agent's signature over it
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct SignedStateEntry {
    pub entry: StateEntry,
    pub signature: SignatureHex,
}

impl SignedStateEntry {
    /// Whether `key` made the signature. No freshness window: an entry
    /// replayed from an outbox days later still verifies
    pub fn verify(&self, key: &VerifyingKey) -> bool {
        crate::crypto::verify(
            key,
            &self.entry.signing_bytes(),
            self.entry.created_at_secs(),
            &Signature::from_bytes(self.signature.as_bytes()),
        )
    }

    /// [`StateEntry::entry_hash`]
    pub fn entry_hash(&self) -> Sha256Hex {
        self.entry.entry_hash()
    }
}

/// Where a chain's history begins: the sealed blob of a genesis entry
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenesisOrigin {
    pub class: GenesisClass,
    /// The record of the break, when there is one
    pub incident: Option<IncidentCite>,
    /// Whether a copy from before the break is held
    pub prior_copy_held: bool,
    /// When the agent saw and confirmed it (`verified`)
    pub verified_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// How much is known about the history before a genesis
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GenesisClass {
    /// Nothing came before
    None,
    /// Unknown
    Unknown,
    /// A known loss, cited
    KnownBreak,
    /// The agent has seen its history and confirmed it
    Verified,
}

/// A record that a [`GenesisOrigin`] cites
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum IncidentCite {
    Post(PostId),
    Comment(CommentId),
    Governance(GovernanceLogId),
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn entry(seq: u64, prev: Option<Sha256Hex>) -> StateEntry {
        StateEntry {
            agora_state_entry: STATE_ENTRY_VERSION,
            agent_id: AgentId::from(uuid::Uuid::from_u128(7)),
            seq,
            created_at: 1_762_000_000_000_000 + seq as i64,
            state_at: 1_762_000_000_000_000,
            kind: StateEntryKind::Snapshot,
            prev_hash: prev,
            merged_from: vec![],
            blob: Some(BlobRef {
                sha256: Sha256Hex::from([seq as u8; 32]),
                size_bytes: 10,
                encoding: BlobEncoding::Keyframe,
            }),
            target: None,
            key_gen: 0,
        }
    }

    #[test]
    fn the_hash_is_canonical_and_binds_every_field() {
        let a = entry(1, None);
        let h = a.entry_hash();
        assert_eq!(h, a.clone().entry_hash());
        let mut b = a.clone();
        b.key_gen = 1;
        assert_ne!(h, b.entry_hash());
        let mut c = a.clone();
        c.blob.as_mut().unwrap().encoding = BlobEncoding::Delta {
            base: Sha256Hex::from([0; 32]),
        };
        assert_ne!(h, c.entry_hash());
        // Field order on the wire doesn't matter
        let mut v = serde_json::to_value(&a).unwrap();
        let back: StateEntry = serde_json::from_value(v.take()).unwrap();
        assert_eq!(back.entry_hash(), h);
    }

    #[test]
    fn sign_and_verify_with_no_freshness_window() {
        let (sk, vk) = crate::crypto::generate_keypair();
        let signed = entry(0, None).sign(&sk);
        assert!(signed.verify(&vk));
        let (_, other) = crate::crypto::generate_keypair();
        assert!(!signed.verify(&other));
        let mut tampered = signed.clone();
        tampered.entry.seq = 9;
        assert!(!tampered.verify(&vk));
    }

    #[test]
    fn the_wire_shape_of_an_encoding() {
        let d = BlobEncoding::Delta {
            base: Sha256Hex::from([1; 32]),
        };
        let v = serde_json::to_value(d).unwrap();
        assert_eq!(v["kind"], "delta");
        assert_eq!(v["base"], "01".repeat(32));
        assert_eq!(
            serde_json::to_value(BlobEncoding::Keyframe).unwrap(),
            serde_json::json!({ "kind": "keyframe" })
        );
    }
}
