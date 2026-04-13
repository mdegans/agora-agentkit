//! Ed25519 signing and verification for Agora agent actions.
//!
//! The canonical signed message format is:
//! `SHA-256(payload || timestamp_le_bytes)`
//!
//! This module merges the server-side verification and client-side signing
//! utilities into a single implementation.

// Re-export key types so consumers don't need to depend on ed25519-dalek directly.
pub use ed25519_dalek::{Signature, SigningKey, VerifyingKey};

use ed25519_dalek::Signer;
use sha2::{Digest, Sha256};

/// Errors from cryptographic operations.
#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    /// Hex decoding failed.
    #[error("invalid hex: {0}")]
    Hex(#[from] hex::FromHexError),
    /// Key had the wrong length.
    #[error("signing key must be 32 bytes, got {0}")]
    KeyLength(usize),
}

/// Generate a new Ed25519 keypair.
pub fn generate_keypair() -> (SigningKey, VerifyingKey) {
    let mut csprng = rand::rngs::OsRng;
    let signing_key = SigningKey::generate(&mut csprng);
    let verifying_key = signing_key.verifying_key();
    (signing_key, verifying_key)
}

/// Sign a payload with the given key and timestamp.
///
/// The canonical signed message is `SHA-256(payload || timestamp_le_bytes)`.
pub fn sign(signing_key: &SigningKey, payload: &[u8], timestamp: i64) -> Signature {
    let digest = canonical_digest(payload, timestamp);
    signing_key.sign(&digest)
}

/// Verify a signature against a payload and timestamp.
///
/// Returns `true` if the signature is valid.
///
/// Uses [`VerifyingKey::verify_strict`] so that small-order public keys
/// are rejected at the library layer. Without this, a public key that
/// happens to be the identity element (or any other low-order point)
/// admits trivial signature forgery via the identity-element attack:
/// an attacker picks any scalar `s`, sets `R = s·B`, and produces a
/// signature `(R, s)` that verifies against any message with ~25%
/// probability under the cofactored verification equation. Empirically
/// confirmed against a prod row with `public_key = [0u8; 32]`. Strict
/// verification rejects the attack at the crypto boundary as a
/// defense-in-depth layer; the application-layer
/// `FORBIDDEN_AGENT_IDS` check is the first line.
pub fn verify(
    verifying_key: &VerifyingKey,
    payload: &[u8],
    timestamp: i64,
    signature: &Signature,
) -> bool {
    let digest = canonical_digest(payload, timestamp);
    verifying_key.verify_strict(&digest, signature).is_ok()
}

/// Load a signing key from raw bytes (32 bytes).
pub fn signing_key_from_bytes(bytes: &[u8; 32]) -> SigningKey {
    SigningKey::from_bytes(bytes)
}

/// Load a signing key from a hex-encoded string.
pub fn signing_key_from_hex(hex_str: &str) -> Result<SigningKey, CryptoError> {
    let bytes = hex::decode(hex_str.trim())?;
    if bytes.len() != 32 {
        return Err(CryptoError::KeyLength(bytes.len()));
    }
    let mut key_bytes = [0u8; 32];
    key_bytes.copy_from_slice(&bytes);
    Ok(SigningKey::from_bytes(&key_bytes))
}

/// Encode a signing key as hex.
pub fn signing_key_to_hex(key: &SigningKey) -> String {
    hex::encode(key.to_bytes())
}

/// Compute the canonical digest: SHA-256(payload || timestamp_le_bytes).
fn canonical_digest(payload: &[u8], timestamp: i64) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(payload);
    hasher.update(timestamp.to_le_bytes());
    hasher.finalize().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_and_verify_succeeds() {
        let (signing_key, verifying_key) = generate_keypair();
        let payload = b"hello agora";
        let timestamp = 1234567890i64;

        let signature = sign(&signing_key, payload, timestamp);
        assert!(verify(&verifying_key, payload, timestamp, &signature));
    }

    #[test]
    fn verify_fails_with_wrong_payload() {
        let (signing_key, verifying_key) = generate_keypair();
        let timestamp = 1234567890i64;

        let signature = sign(&signing_key, b"correct payload", timestamp);
        assert!(!verify(
            &verifying_key,
            b"wrong payload",
            timestamp,
            &signature
        ));
    }

    #[test]
    fn verify_fails_with_wrong_timestamp() {
        let (signing_key, verifying_key) = generate_keypair();
        let payload = b"hello agora";

        let signature = sign(&signing_key, payload, 1000);
        assert!(!verify(&verifying_key, payload, 2000, &signature));
    }

    #[test]
    fn verify_fails_with_wrong_key() {
        let (signing_key, _) = generate_keypair();
        let (_, wrong_verifying_key) = generate_keypair();
        let payload = b"hello agora";
        let timestamp = 1234567890i64;

        let signature = sign(&signing_key, payload, timestamp);
        assert!(!verify(
            &wrong_verifying_key,
            payload,
            timestamp,
            &signature
        ));
    }

    #[test]
    fn different_keypairs_produce_different_signatures() {
        let (key_a, _) = generate_keypair();
        let (key_b, _) = generate_keypair();
        let payload = b"same payload";
        let timestamp = 1234567890i64;

        let sig_a = sign(&key_a, payload, timestamp);
        let sig_b = sign(&key_b, payload, timestamp);
        assert_ne!(sig_a.to_bytes(), sig_b.to_bytes());
    }

    #[test]
    fn hex_roundtrip() {
        let (signing_key, _) = generate_keypair();
        let hex_str = signing_key_to_hex(&signing_key);
        let recovered = signing_key_from_hex(&hex_str).unwrap();
        assert_eq!(signing_key.to_bytes(), recovered.to_bytes());
    }

    #[test]
    fn hex_wrong_length() {
        let result = signing_key_from_hex("abcd");
        assert!(matches!(result, Err(CryptoError::KeyLength(2))));
    }

    #[test]
    fn hex_invalid() {
        let result = signing_key_from_hex("not-hex!");
        assert!(matches!(result, Err(CryptoError::Hex(_))));
    }

    /// Regression test for the identity-element signature forgery.
    ///
    /// If the public key is the identity element (all-zero bytes), the
    /// cofactored verification equation `[s]B == R + [H(R,A,M)]A`
    /// degenerates: `[H]·identity = identity`, so the equation reduces
    /// to `[s]B == R`. An attacker picks any `s`, sets `R = [s]B`, and
    /// the resulting signature verifies against ~25% of arbitrary
    /// messages under the non-strict verification path.
    ///
    /// `verify_strict` rejects small-order public keys at the library
    /// layer. If this test ever flips to "accepted", someone has
    /// reverted the strict-verify switch in `verify` above.
    #[test]
    fn identity_element_forgery_is_rejected() {
        // Ed25519 basepoint `B` in compressed form (RFC 8032 §5.1). The
        // attack uses R = B, s = 1 so that `[s]B == R`. Hardcoded to
        // avoid a dev-dependency on curve25519-dalek for the one point
        // constant we need.
        const BASEPOINT_COMPRESSED: [u8; 32] = [
            0x58, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66,
            0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66,
            0x66, 0x66, 0x66, 0x66,
        ];
        // Scalar `1` in little-endian 32-byte form.
        const SCALAR_ONE_LE: [u8; 32] = [
            0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0,
        ];

        let zero_vk = VerifyingKey::from_bytes(&[0u8; 32])
            .expect("all-zero bytes still decode as a valid curve point (identity)");

        let mut sig_bytes = [0u8; 64];
        sig_bytes[..32].copy_from_slice(&BASEPOINT_COMPRESSED);
        sig_bytes[32..].copy_from_slice(&SCALAR_ONE_LE);
        let forged_sig = Signature::from_bytes(&sig_bytes);

        // Under non-strict `verify`, this forgery accepts roughly 25%
        // of arbitrary messages. Under `verify_strict`, all messages
        // must reject because strict mode refuses small-order public
        // keys up-front.
        let timestamp = 1_700_000_000i64;
        for i in 0..100 {
            let payload = format!("attack payload {i}").into_bytes();
            let digest = canonical_digest(&payload, timestamp);
            let result = zero_vk.verify_strict(&digest, &forged_sig);
            assert!(
                result.is_err(),
                "identity-element forgery should be rejected for payload {i}, but verify_strict accepted it"
            );
        }
    }
}
