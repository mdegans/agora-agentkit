//! The state key, its wrap, and sealed blobs

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rand::RngCore;
use rand::rngs::OsRng;
use zeroize::Zeroizing;

use super::chain::BlobEncoding;
use super::keys::{StateKeyRecord, WrappedStateKey};
use crate::crypto::SigningKey;
use crate::envelope::{
    EncryptionPublicKey, EncryptionSecretKey, EnvelopeError, unwrap_secret,
    wrap_secret,
};
use crate::govlog::PublicKeyHex;
use crate::ids::AgentId;

/// HKDF `info` of a state key's wrap
pub const STATE_WRAP_INFO: &[u8] = b"agora/state-wrap/v1";

/// What every sealed blob's AAD begins with
pub const BLOB_AAD_CONTEXT: &[u8] = b"agora/state/v1";

/// The first byte of a sealed blob
pub const SEALED_BLOB_VERSION: u8 = 1;

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

/// `"agora/state/v1" ‖ agent_id ‖ key_gen ‖ encoding tag ‖ base (or zeros)`
fn blob_aad(
    agent_id: AgentId,
    key_gen: u32,
    encoding: &BlobEncoding,
) -> Vec<u8> {
    let mut aad = BLOB_AAD_CONTEXT.to_vec();
    aad.extend_from_slice(agent_id.as_uuid().as_bytes());
    aad.extend_from_slice(&key_gen.to_le_bytes());
    let (tag, base) = match encoding {
        BlobEncoding::Keyframe => (0u8, [0u8; 32]),
        BlobEncoding::Delta { base } => (1u8, *base.as_bytes()),
    };
    aad.push(tag);
    aad.extend_from_slice(&base);
    aad
}

/// Seal `plaintext` (already compressed) under `key`: `ver ‖ nonce ‖ ct`
pub fn seal_blob(
    key: &StateKey,
    agent_id: AgentId,
    key_gen: u32,
    encoding: &BlobEncoding,
    plaintext: &[u8],
) -> Vec<u8> {
    let mut nonce = [0u8; XNONCE_LEN];
    OsRng.fill_bytes(&mut nonce);
    let ct = XChaCha20Poly1305::new(key.0.as_ref().into())
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad: &blob_aad(agent_id, key_gen, encoding),
            },
        )
        .expect("XChaCha20-Poly1305 encryption is infallible in memory");
    let mut out = Vec::with_capacity(1 + XNONCE_LEN + ct.len());
    out.push(SEALED_BLOB_VERSION);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    out
}

/// Open a [`seal_blob`]. Fails on any other agent, generation, encoding
/// or base than it was sealed with
pub fn open_blob(
    key: &StateKey,
    agent_id: AgentId,
    key_gen: u32,
    encoding: &BlobEncoding,
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
                aad: &blob_aad(agent_id, key_gen, encoding),
            },
        )
        .map(Zeroizing::new)
        .map_err(|_| EnvelopeError::Decrypt)
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
        let base = BlobEncoding::Delta {
            base: Sha256Hex::from([1; 32]),
        };
        let sealed = seal_blob(&key, id, 3, &base, b"hello");
        assert_eq!(&*open_blob(&key, id, 3, &base, &sealed).unwrap(), b"hello");
        let wrong_base = BlobEncoding::Delta {
            base: Sha256Hex::from([2; 32]),
        };
        assert!(open_blob(&key, id, 3, &wrong_base, &sealed).is_err());
        assert!(
            open_blob(&key, id, 3, &BlobEncoding::Keyframe, &sealed).is_err()
        );
        assert!(open_blob(&key, id, 4, &base, &sealed).is_err());
        assert!(open_blob(&key, AgentId::new(), 3, &base, &sealed).is_err());
        assert!(
            open_blob(&StateKey::generate(), id, 3, &base, &sealed).is_err()
        );
        assert_eq!(format!("{key:?} {key}"), "StateKey([REDACTED]) [REDACTED]");
    }
}
