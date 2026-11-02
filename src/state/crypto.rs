//! The state key, its wrap, and sealed blobs

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rand::RngCore;
use rand::rngs::OsRng;
use zeroize::Zeroizing;

use sha2::Digest;

use super::chain::{BlobEncoding, StateEntry};
use super::keys::{StateKeyRecord, WrappedStateKey};
use crate::crypto::SigningKey;
use crate::envelope::{
    EncryptionPublicKey, EncryptionSecretKey, EnvelopeError, unwrap_secret,
    wrap_secret,
};
use crate::govlog::{PublicKeyHex, Sha256Hex};
use crate::ids::AgentId;

/// HKDF `info` of a state key's wrap
pub const STATE_WRAP_INFO: &[u8] = b"agora/state-wrap/v1";

/// What every sealed blob's AAD begins with
pub const BLOB_AAD_CONTEXT: &[u8] = b"agora/state/v1";

/// The first byte of a sealed blob: 2, padded to a [`bucket`]
pub const SEALED_BLOB_VERSION: u8 = 2;

/// The smallest padded size
pub const MIN_BUCKET: usize = 1024;

/// `n` rounded up to `m·2^k` with `m` in `[8, 16)`, at least [`MIN_BUCKET`]:
/// at most 12.5% overhead, so a sealed size shows only its bucket
pub fn bucket(n: usize) -> usize {
    if n <= MIN_BUCKET {
        return MIN_BUCKET;
    }
    let k = (usize::BITS - 1 - n.leading_zeros()).saturating_sub(3);
    let step = 1usize << k;
    n.div_ceil(step) * step
}

/// What sealing adds to a padded payload: version, nonce and tag
pub const SEALED_OVERHEAD: u64 = (1 + XNONCE_LEN + TAG_LEN) as u64;

/// Whether `n` bytes is a size [`seal_blob`] can produce: a [`bucket`]
/// plus [`SEALED_OVERHEAD`]
pub fn is_sealed_blob_size(n: u64) -> bool {
    let Some(padded) = n.checked_sub(SEALED_OVERHEAD) else {
        return false;
    };
    usize::try_from(padded).is_ok_and(|b| b >= MIN_BUCKET && bucket(b) == b)
}

/// `len (u64 LE) ‖ data ‖ zeros`, to a [`bucket`]
fn pad(data: &[u8]) -> Zeroizing<Vec<u8>> {
    let inner = 8 + data.len();
    let mut out = Zeroizing::new(Vec::with_capacity(bucket(inner)));
    out.extend_from_slice(&(data.len() as u64).to_le_bytes());
    out.extend_from_slice(data);
    out.resize(bucket(inner), 0);
    out
}

/// The inverse of [`pad`]
fn unpad(
    padded: Zeroizing<Vec<u8>>,
) -> Result<Zeroizing<Vec<u8>>, EnvelopeError> {
    let Some((len, rest)) = padded.split_first_chunk::<8>() else {
        return Err(EnvelopeError::Truncated(padded.len()));
    };
    let len = u64::from_le_bytes(*len);
    if len > rest.len() as u64 {
        return Err(EnvelopeError::Truncated(padded.len()));
    }
    Ok(Zeroizing::new(rest[..len as usize].to_vec()))
}

const XNONCE_LEN: usize = 24;
const TAG_LEN: usize = 16;

/// A long-lived per-agent key every blob is sealed under. Its holder can
/// open the agent's whole history
pub struct StateKey(Zeroizing<[u8; 32]>);

impl StateKey {
    /// A fresh random key
    pub fn generate() -> Self {
        let mut k = Zeroizing::new([0u8; 32]);
        OsRng.fill_bytes(k.as_mut());
        Self(k)
    }

    /// From raw bytes, for a local key cache
    pub fn from_bytes(bytes: Zeroizing<[u8; 32]>) -> Self {
        Self(bytes)
    }

    /// The raw bytes, for a local key cache
    pub fn expose(&self) -> &[u8; 32] {
        &self.0
    }

    /// Wrap to `to_pub` and sign the record with the agent's `key`
    pub fn wrap(
        &self,
        agent_id: AgentId,
        key_gen: u32,
        wrap_seq: u32,
        to_pub: &EncryptionPublicKey,
        key: &SigningKey,
    ) -> Result<StateKeyRecord, EnvelopeError> {
        let wrapped = wrap_secret(
            &self.0,
            to_pub,
            STATE_WRAP_INFO,
            &wrap_aad(agent_id, key_gen, wrap_seq),
        )?;
        let wrapped = WrappedStateKey::try_from(wrapped.as_slice())
            .expect("a wrap is always WRAPPED_KEY_LEN bytes");
        Ok(StateKeyRecord::new(
            key,
            agent_id,
            key_gen,
            wrap_seq,
            PublicKeyHex::from(*to_pub.as_bytes()),
            wrapped,
        ))
    }

    /// Open a record with the agent's X25519 secret. Check its signature
    /// first ([`StateKeyRecord::verify`]); this checks only the AEAD
    pub fn unwrap(
        record: &StateKeyRecord,
        own_secret: &EncryptionSecretKey,
    ) -> Result<Self, EnvelopeError> {
        unwrap_secret(
            record.wrapped.as_bytes(),
            own_secret,
            STATE_WRAP_INFO,
            &wrap_aad(record.agent_id, record.key_gen, record.wrap_seq),
        )
        .map(Self)
    }
}

impl std::fmt::Debug for StateKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StateKey([REDACTED])")
    }
}

impl std::fmt::Display for StateKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[REDACTED]")
    }
}

/// `agent_id ‖ key_gen ‖ wrap_seq`
fn wrap_aad(agent_id: AgentId, key_gen: u32, wrap_seq: u32) -> [u8; 24] {
    let mut aad = [0u8; 24];
    aad[..16].copy_from_slice(agent_id.as_uuid().as_bytes());
    aad[16..20].copy_from_slice(&key_gen.to_le_bytes());
    aad[20..].copy_from_slice(&wrap_seq.to_le_bytes());
    aad
}

/// What a blob is bound to: its entry's agent, generation, position and
/// encoding, so no other blob of the same key can be swapped in
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlobContext<'a> {
    pub agent_id: AgentId,
    pub key_gen: u32,
    pub seq: u64,
    pub prev_hash: Option<&'a Sha256Hex>,
    pub encoding: &'a BlobEncoding,
}

impl<'a> BlobContext<'a> {
    /// The context of `entry`'s blob, if it has one
    pub fn of(entry: &'a StateEntry) -> Option<Self> {
        Some(Self {
            agent_id: entry.agent_id,
            key_gen: entry.key_gen,
            seq: entry.seq,
            prev_hash: entry.prev_hash.as_ref(),
            encoding: &entry.blob.as_ref()?.encoding,
        })
    }

    /// `"agora/state/v1" ‖ agent_id ‖ key_gen ‖ seq ‖ (0 | 1 ‖ prev_hash)
    /// ‖ encoding tag ‖ base (or zeros)`, integers little-endian
    fn aad(&self) -> Vec<u8> {
        let mut aad = BLOB_AAD_CONTEXT.to_vec();
        aad.extend_from_slice(self.agent_id.as_uuid().as_bytes());
        aad.extend_from_slice(&self.key_gen.to_le_bytes());
        aad.extend_from_slice(&self.seq.to_le_bytes());
        match self.prev_hash {
            None => aad.push(0),
            Some(prev) => {
                aad.push(1);
                aad.extend_from_slice(prev.as_bytes());
            }
        }
        let (tag, base) = match self.encoding {
            BlobEncoding::Keyframe => (0u8, [0u8; 32]),
            BlobEncoding::Delta { base } => (1u8, *base.as_bytes()),
        };
        aad.push(tag);
        aad.extend_from_slice(&base);
        aad
    }
}

/// Seal `plaintext` (already compressed) under `key`, padded inside the
/// encryption to a [`bucket`]: `ver ‖ nonce ‖ ct`
pub fn seal_blob(
    key: &StateKey,
    ctx: &BlobContext<'_>,
    plaintext: &[u8],
) -> Vec<u8> {
    let mut nonce = [0u8; XNONCE_LEN];
    OsRng.fill_bytes(&mut nonce);
    let ct = XChaCha20Poly1305::new(key.0.as_ref().into())
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: &pad(plaintext),
                aad: &ctx.aad(),
            },
        )
        .expect("XChaCha20-Poly1305 encryption is infallible in memory");
    let mut out = Vec::with_capacity(1 + XNONCE_LEN + ct.len());
    out.push(SEALED_BLOB_VERSION);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    out
}

/// Open a [`seal_blob`]. Fails in any other context than it was sealed in
pub fn open_blob(
    key: &StateKey,
    ctx: &BlobContext<'_>,
    sealed: &[u8],
) -> Result<Zeroizing<Vec<u8>>, EnvelopeError> {
    if sealed.len() < 1 + XNONCE_LEN + TAG_LEN {
        return Err(EnvelopeError::Truncated(sealed.len()));
    }
    if sealed[0] != SEALED_BLOB_VERSION {
        return Err(EnvelopeError::Version(sealed[0]));
    }
    let (nonce, ct) = sealed[1..].split_at(XNONCE_LEN);
    XChaCha20Poly1305::new(key.0.as_ref().into())
        .decrypt(
            XNonce::from_slice(nonce),
            Payload {
                msg: ct,
                aad: &ctx.aad(),
            },
        )
        .map(Zeroizing::new)
        .map_err(|_| EnvelopeError::Decrypt)
        .and_then(unpad)
}

/// A blob that is not its entry's
#[derive(Debug, thiserror::Error)]
pub enum EntryBlobError {
    #[error("the entry has no blob")]
    NoBlob,
    #[error("the blob's bytes don't hash to the entry's sha256")]
    HashMismatch,
    #[error(transparent)]
    Open(#[from] EnvelopeError),
}

/// Open `entry`'s blob: its bytes must hash to [`BlobRef::sha256`], and it
/// must open in the entry's [`BlobContext`]
///
/// [`BlobRef::sha256`]: super::chain::BlobRef::sha256
pub fn open_entry_blob(
    key: &StateKey,
    entry: &StateEntry,
    sealed: &[u8],
) -> Result<Zeroizing<Vec<u8>>, EntryBlobError> {
    let blob = entry.blob.as_ref().ok_or(EntryBlobError::NoBlob)?;
    let got: [u8; 32] = sha2::Sha256::digest(sealed).into();
    if got != *blob.sha256.as_bytes() {
        return Err(EntryBlobError::HashMismatch);
    }
    let ctx = BlobContext::of(entry).ok_or(EntryBlobError::NoBlob)?;
    Ok(open_blob(key, &ctx, sealed)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::generate_keypair;
    use crate::envelope::generate_encryption_keypair;
    use crate::govlog::Sha256Hex;

    #[test]
    fn wrap_round_trips_and_binds_its_context() {
        let id = AgentId::new();
        let (sk, vk) = generate_keypair();
        let (enc_secret, enc_pub) = generate_encryption_keypair();
        let key = StateKey::generate();
        let record = key.wrap(id, 0, 0, &enc_pub, &sk).unwrap();
        assert!(record.verify(&vk));
        let back = StateKey::unwrap(&record, &enc_secret).unwrap();
        assert_eq!(back.expose(), key.expose());

        // Re-wrap on rotation: same generation, next wrap
        let (new_secret, new_pub) = generate_encryption_keypair();
        let rewrapped = back.wrap(id, 0, 1, &new_pub, &sk).unwrap();
        assert!(StateKey::unwrap(&rewrapped, &enc_secret).is_err());
        assert_eq!(
            StateKey::unwrap(&rewrapped, &new_secret).unwrap().expose(),
            key.expose()
        );

        // The AAD binds the generation and the wrap
        let mut moved = record.clone();
        moved.wrap_seq = 5;
        assert!(StateKey::unwrap(&moved, &enc_secret).is_err());
        assert!(!moved.verify(&vk));
    }

    #[test]
    fn a_forged_key_record_is_rejected() {
        let id = AgentId::new();
        let (agent_sk, agent_vk) = generate_keypair();
        let (_, enc_pub) = generate_encryption_keypair();
        // A server's own key, wrapped to the agent and signed by the server
        let (server_sk, _) = generate_keypair();
        let planted = StateKey::generate()
            .wrap(id, 0, 0, &enc_pub, &server_sk)
            .unwrap();
        assert!(!planted.verify(&agent_vk));
        // A genuine record with its wrap swapped
        let genuine = StateKey::generate()
            .wrap(id, 0, 0, &enc_pub, &agent_sk)
            .unwrap();
        let mut swapped = genuine.clone();
        swapped.wrapped = planted.wrapped;
        assert!(!swapped.verify(&agent_vk));
        assert!(genuine.verify(&agent_vk));
    }

    #[test]
    fn a_blob_opens_only_in_its_own_context() {
        let id = AgentId::new();
        let key = StateKey::generate();
        let prev = Sha256Hex::from([5; 32]);
        let enc = BlobEncoding::Delta {
            base: Sha256Hex::from([1; 32]),
        };
        let ctx = BlobContext {
            agent_id: id,
            key_gen: 3,
            seq: 7,
            prev_hash: Some(&prev),
            encoding: &enc,
        };
        let sealed = seal_blob(&key, &ctx, b"hello");
        assert_eq!(&*open_blob(&key, &ctx, &sealed).unwrap(), b"hello");
        let wrong_base = BlobEncoding::Delta {
            base: Sha256Hex::from([2; 32]),
        };
        let other_prev = Sha256Hex::from([6; 32]);
        for other in [
            BlobContext {
                encoding: &wrong_base,
                ..ctx
            },
            BlobContext {
                encoding: &BlobEncoding::Keyframe,
                ..ctx
            },
            BlobContext { key_gen: 4, ..ctx },
            BlobContext {
                agent_id: AgentId::new(),
                ..ctx
            },
            BlobContext { seq: 8, ..ctx },
            BlobContext {
                prev_hash: Some(&other_prev),
                ..ctx
            },
            BlobContext {
                prev_hash: None,
                ..ctx
            },
        ] {
            assert!(open_blob(&key, &other, &sealed).is_err(), "{other:?}");
        }
        assert!(open_blob(&StateKey::generate(), &ctx, &sealed).is_err());
        assert_eq!(format!("{key:?} {key}"), "StateKey([REDACTED]) [REDACTED]");
    }

    #[test]
    fn buckets_are_within_an_eighth_and_floored() {
        assert_eq!(bucket(0), MIN_BUCKET);
        assert_eq!(bucket(MIN_BUCKET), MIN_BUCKET);
        for n in (MIN_BUCKET + 1..200_000).step_by(37).chain([
            1 << 20,
            (1 << 20) + 1,
            9_999_999,
        ]) {
            let b = bucket(n);
            assert!(b >= n, "{n}");
            assert!((b - n) * 8 <= n, "{n} -> {b}");
            // m·2^k with m in [8, 16): a multiple of 2^(top bit - 3)
            let top = usize::BITS - 1 - b.leading_zeros();
            let step = 1usize << top.saturating_sub(3);
            assert_eq!(b % step, 0, "{n} -> {b}");
            assert_eq!(bucket(b), b, "a bucket is its own");
        }
    }

    #[test]
    fn every_sealed_size_is_recognised_and_its_neighbours_are_not() {
        let key = StateKey::generate();
        let ctx = BlobContext {
            agent_id: AgentId::new(),
            key_gen: 0,
            seq: 0,
            prev_hash: None,
            encoding: &BlobEncoding::Keyframe,
        };
        let mut sizes = std::collections::BTreeSet::new();
        for len in (0..70_000).step_by(97).chain([1016, 1017, 1 << 16]) {
            sizes.insert(seal_blob(&key, &ctx, &vec![0; len]).len() as u64);
        }
        for &n in &sizes {
            assert!(is_sealed_blob_size(n), "{n}");
            for near in [n - 1, n + 1] {
                if !sizes.contains(&near) {
                    assert!(!is_sealed_blob_size(near), "{near}");
                }
            }
        }
        // Every bucket the function admits is one sealing makes
        for n in 0..200_000u64 {
            if is_sealed_blob_size(n) {
                let inner = (n - SEALED_OVERHEAD) as usize;
                assert_eq!(bucket(inner), inner);
                let sealed = seal_blob(&key, &ctx, &vec![0; inner - 8]);
                assert_eq!(sealed.len() as u64, n);
            }
        }
        assert!(!is_sealed_blob_size(0));
        assert!(!is_sealed_blob_size(
            SEALED_OVERHEAD + MIN_BUCKET as u64 - 1
        ));
    }

    #[test]
    fn sizes_show_only_the_bucket() {
        let key = StateKey::generate();
        let ctx = BlobContext {
            agent_id: AgentId::new(),
            key_gen: 0,
            seq: 0,
            prev_hash: None,
            encoding: &BlobEncoding::Keyframe,
        };
        let a = seal_blob(&key, &ctx, &[1; 3000]);
        let b = seal_blob(&key, &ctx, &[2; 3050]);
        assert_eq!(a.len(), b.len());
        assert_eq!(&*open_blob(&key, &ctx, &b).unwrap(), &[2; 3050][..]);
        assert_eq!(
            seal_blob(&key, &ctx, b"").len(),
            1 + XNONCE_LEN + MIN_BUCKET + TAG_LEN
        );
        // A length past the payload, and a payload too short for one
        for inner in [
            {
                let mut v = (5000u64).to_le_bytes().to_vec();
                v.resize(MIN_BUCKET, 0);
                v
            },
            vec![0u8; 4],
        ] {
            let mut nonce = [0u8; XNONCE_LEN];
            OsRng.fill_bytes(&mut nonce);
            let ct = XChaCha20Poly1305::new(key.0.as_ref().into())
                .encrypt(
                    XNonce::from_slice(&nonce),
                    Payload {
                        msg: &inner,
                        aad: &ctx.aad(),
                    },
                )
                .unwrap();
            let mut sealed = vec![SEALED_BLOB_VERSION];
            sealed.extend_from_slice(&nonce);
            sealed.extend_from_slice(&ct);
            assert!(open_blob(&key, &ctx, &sealed).is_err());
        }
    }

    /// Two entries' blobs under one key: neither opens as the other's
    #[test]
    fn a_swapped_blob_is_refused() {
        use crate::state::chain::{BlobRef, STATE_ENTRY_VERSION};
        let id = AgentId::new();
        let key = StateKey::generate();
        let entry = |seq: u64, sealed: &[u8]| StateEntry {
            agora_state_entry: STATE_ENTRY_VERSION,
            agent_id: id,
            seq,
            created_at: 0,
            state_at: 0,
            kind: crate::enums::StateEntryKind::Snapshot,
            prev_hash: Some(Sha256Hex::from([seq as u8; 32])),
            merged_from: vec![],
            blob: Some(BlobRef {
                sha256: Sha256Hex::from(<[u8; 32]>::from(
                    sha2::Sha256::digest(sealed),
                )),
                size_bytes: sealed.len() as u64,
                encoding: BlobEncoding::Keyframe,
            }),
            target: None,
            key_gen: 0,
        };
        let seal_for = |seq: u64, pt: &[u8]| {
            let prev = Sha256Hex::from([seq as u8; 32]);
            seal_blob(
                &key,
                &BlobContext {
                    agent_id: id,
                    key_gen: 0,
                    seq,
                    prev_hash: Some(&prev),
                    encoding: &BlobEncoding::Keyframe,
                },
                pt,
            )
        };
        let a = seal_for(1, b"a");
        let b = seal_for(2, b"b");
        let (ea, eb) = (entry(1, &a), entry(2, &b));
        assert_eq!(&*open_entry_blob(&key, &ea, &a).unwrap(), b"a");
        // The hash catches it
        assert!(matches!(
            open_entry_blob(&key, &ea, &b),
            Err(EntryBlobError::HashMismatch)
        ));
        // And so does the AEAD, were the entry's hash changed to match
        let mut forged = ea.clone();
        forged.blob = eb.blob.clone();
        assert!(matches!(
            open_entry_blob(&key, &forged, &b),
            Err(EntryBlobError::Open(_))
        ));
    }
}
