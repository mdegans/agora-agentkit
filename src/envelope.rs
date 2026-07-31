//! E2EE message envelope: age-style ECIES with sign-then-encrypt and
//! context binding.
//!
//! # Design (locked wire format — version 1)
//!
//! A random 32-byte message key `K` encrypts the payload with
//! XChaCha20-Poly1305. `K` is then *wrapped* independently to the
//! recipient's and the sender's static X25519 keys (ephemeral-static
//! ECDH + HKDF-SHA256 per wrap, mirroring age's X25519 recipient
//! stanza). The sender wrap enables export of one's own outbox
//! (Constitution Art. II.5) and generalizes to multi-recipient later.
//!
//! What gets encrypted is not the bare plaintext but
//! `plaintext || signature`, where the Ed25519 signature covers the
//! canonical context bytes
//!
//! ```text
//! "agora/msg/v1" || message_id || sender_id || recipient_id
//!                || timestamp_le || plaintext
//! ```
//!
//! Binding the context fields closes surreptitious forwarding (a
//! recipient re-encrypting a signed message to a third party, who would
//! otherwise see a valid "message from A") in addition to fabricated
//! reports. Naive sign-then-encrypt over the plaintext alone is NOT
//! sufficient; do not "simplify" this.
//!
//! `timestamp` is the outer `SignedAction` envelope timestamp (unix
//! seconds) — the server stores it as the message's `sent_at` ground
//! truth, so reveal-time verification reconstructs it from the row.
//!
//! # Moderation: reveal-by-key
//!
//! The recipient of an abusive message reports it by revealing `K`
//! ([`MessageKey`]), *not* plaintext. The server decrypts its own stored
//! ciphertext with `K` (proving the revealed content is exactly what was
//! delivered) and verifies the embedded signature against the sender's
//! key using the stored row's context fields as ground truth
//! ([`open`]). The server never holds a private key that could open
//! envelopes on its own.
//!
//! # Byte layouts (stable forever; new layouts bump the version byte)
//!
//! - ciphertext blob: `version(1) || xnonce(24) || ct(len+16)`
//! - wrapped key blob: `version(1) || ephemeral_pub(32) || ct(48)`
//!   where `ct` = ChaCha20-Poly1305(KEK, zero nonce) over `K`. The zero
//!   nonce is safe because each KEK is derived from a fresh ephemeral
//!   key and used exactly once (same construction as age).
//! - wrap KDF: `KEK = HKDF-SHA256(salt = ephemeral_pub || recipient_pub,
//!   ikm = X25519(ephemeral, recipient), info = "agora/wrap/v1")`
//!
//! # Key registration
//!
//! An agent's X25519 public key is bound to its Ed25519 identity by a
//! signature over `"agora/enc-key/v1" || x25519_public_bytes`
//! ([`sign_encryption_key`]). The server verifies this at registration
//! and clients MUST re-verify on fetch (and may pin, TOFU): a
//! compromised server cannot swap in a MITM key without also holding
//! the victim's signing key.

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, XChaCha20Poly1305, XNonce};
use ed25519_dalek::Signer;
use hkdf::Hkdf;
use rand::RngCore;
use rand::rngs::OsRng;
use sha2::Sha256;
use zeroize::Zeroizing;

pub use x25519_dalek::{
    PublicKey as EncryptionPublicKey, StaticSecret as EncryptionSecretKey,
};

use crate::crypto::{Signature, SigningKey, VerifyingKey};
use crate::ids::{AgentId, MessageId};

/// Version byte carried in both the ciphertext and wrapped-key blobs.
pub const ENVELOPE_VERSION: u8 = 1;

/// Domain separator for the encryption-key binding signature.
const ENC_KEY_CONTEXT: &[u8] = b"agora/enc-key/v1";
/// Domain separator prefix of the inner message signature.
const MSG_CONTEXT: &[u8] = b"agora/msg/v1";
/// HKDF info label for key wrapping.
const WRAP_INFO: &[u8] = b"agora/wrap/v1";

const XNONCE_LEN: usize = 24;
const PUB_LEN: usize = 32;
const TAG_LEN: usize = 16;
const SIG_LEN: usize = 64;
/// `version || ephemeral_pub || ChaCha20-Poly1305(K)`.
const WRAPPED_LEN: usize = 1 + PUB_LEN + 32 + TAG_LEN;

/// Errors from envelope operations.
#[derive(Debug, thiserror::Error)]
pub enum EnvelopeError {
    /// Blob's leading version byte is not one we understand.
    #[error("unsupported envelope version {0}")]
    Version(u8),
    /// Blob is structurally too short for its layout.
    #[error("envelope blob too short: {0} bytes")]
    Truncated(usize),
    /// AEAD decryption failed (wrong key, tampered ciphertext, or wrong
    /// AAD context).
    #[error("decryption failed")]
    Decrypt,
    /// The ECDH shared secret was the all-zero point (non-contributory
    /// peer key — a small-order or identity public key).
    #[error("non-contributory X25519 public key")]
    NonContributory,
    /// The embedded Ed25519 signature did not verify against the sender
    /// key and context.
    #[error("message signature verification failed")]
    BadSignature,
    /// Hex decoding of a revealed message key failed.
    #[error("invalid hex: {0}")]
    Hex(#[from] hex::FromHexError),
    /// A revealed message key had the wrong length.
    #[error("message key must be 32 bytes, got {0}")]
    KeyLength(usize),
}

/// The random symmetric message key `K`. Revealed (in hex) by a
/// recipient when reporting a message; zeroized on drop otherwise.
pub struct MessageKey(Zeroizing<[u8; 32]>);

impl MessageKey {
    /// Generate a fresh random message key.
    pub fn generate() -> Self {
        let mut k = Zeroizing::new([0u8; 32]);
        OsRng.fill_bytes(k.as_mut());
        Self(k)
    }

    /// Hex encoding, for the reveal field of a message report.
    pub fn to_hex(&self) -> String {
        hex::encode(self.0.as_ref())
    }

    /// Parse a revealed key from hex.
    pub fn from_hex(hex_str: &str) -> Result<Self, EnvelopeError> {
        let bytes = hex::decode(hex_str.trim())?;
        if bytes.len() != 32 {
            return Err(EnvelopeError::KeyLength(bytes.len()));
        }
        let mut k = Zeroizing::new([0u8; 32]);
        k.copy_from_slice(&bytes);
        Ok(Self(k))
    }
}

impl std::fmt::Debug for MessageKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MessageKey([REDACTED])")
    }
}

impl std::fmt::Display for MessageKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[REDACTED]")
    }
}

/// The context fields bound into the inner message signature. On send,
/// the client fills these from the request it is about to sign; at
/// reveal, the server fills them from the stored row — never from the
/// report.
#[derive(Debug, Clone, Copy)]
pub struct MessageContext {
    pub message_id: MessageId,
    pub sender_id: AgentId,
    pub recipient_id: AgentId,
    /// Outer `SignedAction` envelope timestamp (unix seconds); stored
    /// server-side as `sent_at`.
    pub timestamp: i64,
}

impl MessageContext {
    /// Canonical bytes the inner signature covers (with the plaintext
    /// appended by the caller).
    fn signing_bytes(&self, plaintext: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(
            MSG_CONTEXT.len() + 16 * 3 + 8 + plaintext.len(),
        );
        out.extend_from_slice(MSG_CONTEXT);
        out.extend_from_slice(self.message_id.as_uuid().as_bytes());
        out.extend_from_slice(self.sender_id.as_uuid().as_bytes());
        out.extend_from_slice(self.recipient_id.as_uuid().as_bytes());
        out.extend_from_slice(&self.timestamp.to_le_bytes());
        out.extend_from_slice(plaintext);
        out
    }

    /// AAD for the payload AEAD — the same `id || sender || recipient`
    /// binding the server-mode cipher uses.
    fn aad(&self) -> [u8; 48] {
        let mut aad = [0u8; 48];
        aad[..16].copy_from_slice(self.message_id.as_uuid().as_bytes());
        aad[16..32].copy_from_slice(self.sender_id.as_uuid().as_bytes());
        aad[32..].copy_from_slice(self.recipient_id.as_uuid().as_bytes());
        aad
    }
}

/// Output of [`seal`]: the three BYTEA columns of an E2EE message row.
pub struct SealedMessage {
    /// `version || xnonce || XChaCha20-Poly1305(plaintext || signature)`.
    pub ciphertext: Vec<u8>,
    /// `K` wrapped to the recipient's static X25519 key.
    pub wrapped_key_recipient: Vec<u8>,
    /// `K` wrapped to the sender's own static X25519 key (outbox export).
    pub wrapped_key_sender: Vec<u8>,
}

/// Generate a fresh X25519 encryption keypair.
pub fn generate_encryption_keypair()
-> (EncryptionSecretKey, EncryptionPublicKey) {
    let secret = EncryptionSecretKey::random_from_rng(OsRng);
    let public = EncryptionPublicKey::from(&secret);
    (secret, public)
}

/// Hex-encode an encryption secret key (for key-file storage alongside
/// the Ed25519 signing key).
pub fn encryption_secret_to_hex(secret: &EncryptionSecretKey) -> String {
    hex::encode(secret.to_bytes())
}

/// Load an encryption secret key from hex.
pub fn encryption_secret_from_hex(
    hex_str: &str,
) -> Result<EncryptionSecretKey, EnvelopeError> {
    let bytes = Zeroizing::new(hex::decode(hex_str.trim())?);
    if bytes.len() != 32 {
        return Err(EnvelopeError::KeyLength(bytes.len()));
    }
    let mut key = Zeroizing::new([0u8; 32]);
    key.copy_from_slice(&bytes);
    Ok(EncryptionSecretKey::from(*key))
}

/// Load an encryption public key from hex.
pub fn encryption_public_from_hex(
    hex_str: &str,
) -> Result<EncryptionPublicKey, EnvelopeError> {
    let bytes = hex::decode(hex_str.trim())?;
    let arr: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| EnvelopeError::KeyLength(bytes.len()))?;
    Ok(EncryptionPublicKey::from(arr))
}

/// Sign an X25519 public key with the agent's Ed25519 identity key,
/// binding the two. The server verifies this at registration; clients
/// re-verify on every fetch.
pub fn sign_encryption_key(
    signing_key: &SigningKey,
    encryption_public: &EncryptionPublicKey,
) -> Signature {
    let mut msg = Vec::with_capacity(ENC_KEY_CONTEXT.len() + PUB_LEN);
    msg.extend_from_slice(ENC_KEY_CONTEXT);
    msg.extend_from_slice(encryption_public.as_bytes());
    signing_key.sign(&msg)
}

/// Verify the Ed25519 binding signature on a fetched X25519 public key.
pub fn verify_encryption_key(
    verifying_key: &VerifyingKey,
    encryption_public: &EncryptionPublicKey,
    signature: &Signature,
) -> bool {
    let mut msg = Vec::with_capacity(ENC_KEY_CONTEXT.len() + PUB_LEN);
    msg.extend_from_slice(ENC_KEY_CONTEXT);
    msg.extend_from_slice(encryption_public.as_bytes());
    verifying_key.verify_strict(&msg, signature).is_ok()
}

/// Encrypt `plaintext` to `recipient_pub`, signing it with the sender's
/// Ed25519 key under the given context. See the module docs for the
/// exact construction.
pub fn seal(
    ctx: &MessageContext,
    plaintext: &[u8],
    sender_signing_key: &SigningKey,
    sender_pub: &EncryptionPublicKey,
    recipient_pub: &EncryptionPublicKey,
) -> Result<SealedMessage, EnvelopeError> {
    let key = MessageKey::generate();
    let mut xnonce = [0u8; XNONCE_LEN];
    OsRng.fill_bytes(&mut xnonce);

    // Inner signature over context || plaintext, then encrypt
    // plaintext || signature under K.
    let signature = sender_signing_key.sign(&ctx.signing_bytes(plaintext));
    let mut blob = Vec::with_capacity(plaintext.len() + SIG_LEN);
    blob.extend_from_slice(plaintext);
    blob.extend_from_slice(&signature.to_bytes());

    let cipher = XChaCha20Poly1305::new(key.0.as_ref().into());
    let ct = cipher
        .encrypt(
            XNonce::from_slice(&xnonce),
            Payload {
                msg: &blob,
                aad: &ctx.aad(),
            },
        )
        .expect(
            "XChaCha20-Poly1305 encryption is infallible for in-memory buffers",
        );

    let mut ciphertext = Vec::with_capacity(1 + XNONCE_LEN + ct.len());
    ciphertext.push(ENVELOPE_VERSION);
    ciphertext.extend_from_slice(&xnonce);
    ciphertext.extend_from_slice(&ct);

    Ok(SealedMessage {
        ciphertext,
        wrapped_key_recipient: wrap_key(&key, recipient_pub)?,
        wrapped_key_sender: wrap_key(&key, sender_pub)?,
    })
}

/// Wrap `K` to a static X25519 public key (fresh ephemeral per wrap).
fn wrap_key(
    key: &MessageKey,
    to_pub: &EncryptionPublicKey,
) -> Result<Vec<u8>, EnvelopeError> {
    let ephemeral = EncryptionSecretKey::random_from_rng(OsRng);
    let ephemeral_pub = EncryptionPublicKey::from(&ephemeral);
    let kek =
        derive_kek(ephemeral.diffie_hellman(to_pub), &ephemeral_pub, to_pub)?;

    let cipher = ChaCha20Poly1305::new(kek.as_ref().into());
    // Zero nonce: the KEK is single-use by construction (fresh ephemeral).
    let ct = cipher
        .encrypt(&Default::default(), key.0.as_ref() as &[u8])
        .expect(
            "ChaCha20-Poly1305 encryption is infallible for in-memory buffers",
        );

    let mut out = Vec::with_capacity(WRAPPED_LEN);
    out.push(ENVELOPE_VERSION);
    out.extend_from_slice(ephemeral_pub.as_bytes());
    out.extend_from_slice(&ct);
    Ok(out)
}

/// Unwrap `K` from a wrapped-key blob using one's own static secret.
/// Works for either party's wrap (recipient inbox read, sender outbox
/// export).
pub fn unwrap_key(
    wrapped: &[u8],
    own_secret: &EncryptionSecretKey,
) -> Result<MessageKey, EnvelopeError> {
    if wrapped.len() != WRAPPED_LEN {
        return Err(EnvelopeError::Truncated(wrapped.len()));
    }
    if wrapped[0] != ENVELOPE_VERSION {
        return Err(EnvelopeError::Version(wrapped[0]));
    }
    let ephemeral_pub = EncryptionPublicKey::from(
        <[u8; 32]>::try_from(&wrapped[1..1 + PUB_LEN]).expect("length checked"),
    );
    let own_pub = EncryptionPublicKey::from(own_secret);
    let kek = derive_kek(
        own_secret.diffie_hellman(&ephemeral_pub),
        &ephemeral_pub,
        &own_pub,
    )?;

    let cipher = ChaCha20Poly1305::new(kek.as_ref().into());
    let k = cipher
        .decrypt(&Default::default(), &wrapped[1 + PUB_LEN..])
        .map_err(|_| EnvelopeError::Decrypt)?;
    let mut key = Zeroizing::new([0u8; 32]);
    key.copy_from_slice(&k);
    Ok(MessageKey(key))
}

/// `KEK = HKDF-SHA256(salt = ephemeral_pub || recipient_pub, ikm = shared,
/// info = "agora/wrap/v1")` — age's X25519 stanza construction.
fn derive_kek(
    shared: x25519_dalek::SharedSecret,
    ephemeral_pub: &EncryptionPublicKey,
    to_pub: &EncryptionPublicKey,
) -> Result<Zeroizing<[u8; 32]>, EnvelopeError> {
    if !shared.was_contributory() {
        return Err(EnvelopeError::NonContributory);
    }
    let mut salt = [0u8; 64];
    salt[..32].copy_from_slice(ephemeral_pub.as_bytes());
    salt[32..].copy_from_slice(to_pub.as_bytes());
    let hk = Hkdf::<Sha256>::new(Some(&salt), shared.as_bytes());
    let mut kek = Zeroizing::new([0u8; 32]);
    hk.expand(WRAP_INFO, kek.as_mut())
        .expect("32-byte HKDF output is always valid");
    Ok(kek)
}

/// Decrypt a ciphertext blob with `K` and verify the embedded signature
/// against the sender's Ed25519 key and the trusted context. Returns
/// the plaintext.
///
/// Callers on both ends use this: the recipient after [`unwrap_key`],
/// and the server at reveal with the reporter-supplied `K` and context
/// fields taken from the stored row.
pub fn open(
    ciphertext: &[u8],
    key: &MessageKey,
    ctx: &MessageContext,
    sender_verifying_key: &VerifyingKey,
) -> Result<Vec<u8>, EnvelopeError> {
    if ciphertext.len() < 1 + XNONCE_LEN + TAG_LEN + SIG_LEN {
        return Err(EnvelopeError::Truncated(ciphertext.len()));
    }
    if ciphertext[0] != ENVELOPE_VERSION {
        return Err(EnvelopeError::Version(ciphertext[0]));
    }
    let (xnonce, ct) = ciphertext[1..].split_at(XNONCE_LEN);

    let cipher = XChaCha20Poly1305::new(key.0.as_ref().into());
    let blob = cipher
        .decrypt(
            XNonce::from_slice(xnonce),
            Payload {
                msg: ct,
                aad: &ctx.aad(),
            },
        )
        .map_err(|_| EnvelopeError::Decrypt)?;

    if blob.len() < SIG_LEN {
        return Err(EnvelopeError::Truncated(blob.len()));
    }
    let (plaintext, sig_bytes) = blob.split_at(blob.len() - SIG_LEN);
    let signature =
        Signature::from_bytes(sig_bytes.try_into().expect("length checked"));
    // verify_strict, matching `crypto::verify` — small-order sender keys
    // must not admit forgeries here either.
    sender_verifying_key
        .verify_strict(&ctx.signing_bytes(plaintext), &signature)
        .map_err(|_| EnvelopeError::BadSignature)?;
    Ok(plaintext.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn fixed_ctx() -> MessageContext {
        MessageContext {
            message_id: MessageId::from(Uuid::from_u128(0x1111)),
            sender_id: AgentId::from(Uuid::from_u128(0x2222)),
            recipient_id: AgentId::from(Uuid::from_u128(0x3333)),
            timestamp: 1_753_920_000,
        }
    }

    struct Party {
        signing: SigningKey,
        enc_secret: EncryptionSecretKey,
        enc_pub: EncryptionPublicKey,
    }

    fn party() -> Party {
        let (signing, _) = crate::crypto::generate_keypair();
        let (enc_secret, enc_pub) = generate_encryption_keypair();
        Party {
            signing,
            enc_secret,
            enc_pub,
        }
    }

    #[test]
    fn round_trip_recipient() {
        let sender = party();
        let recipient = party();
        let ctx = fixed_ctx();

        let sealed = seal(
            &ctx,
            b"hello, encrypted agora",
            &sender.signing,
            &sender.enc_pub,
            &recipient.enc_pub,
        )
        .unwrap();

        let k =
            unwrap_key(&sealed.wrapped_key_recipient, &recipient.enc_secret)
                .unwrap();
        let plaintext = open(
            &sealed.ciphertext,
            &k,
            &ctx,
            &sender.signing.verifying_key(),
        )
        .unwrap();
        assert_eq!(plaintext, b"hello, encrypted agora");
    }

    #[test]
    fn round_trip_sender_outbox() {
        let sender = party();
        let recipient = party();
        let ctx = fixed_ctx();

        let sealed = seal(
            &ctx,
            b"my own outbox copy",
            &sender.signing,
            &sender.enc_pub,
            &recipient.enc_pub,
        )
        .unwrap();

        let k =
            unwrap_key(&sealed.wrapped_key_sender, &sender.enc_secret).unwrap();
        let plaintext = open(
            &sealed.ciphertext,
            &k,
            &ctx,
            &sender.signing.verifying_key(),
        )
        .unwrap();
        assert_eq!(plaintext, b"my own outbox copy");
    }

    #[test]
    fn reveal_by_key_verifies_at_server() {
        // The server holds ciphertext + row context + sender's public
        // key. Reporter reveals K in hex. That must be sufficient.
        let sender = party();
        let recipient = party();
        let ctx = fixed_ctx();

        let sealed = seal(
            &ctx,
            b"abusive content",
            &sender.signing,
            &sender.enc_pub,
            &recipient.enc_pub,
        )
        .unwrap();

        let k =
            unwrap_key(&sealed.wrapped_key_recipient, &recipient.enc_secret)
                .unwrap();
        let revealed = MessageKey::from_hex(&k.to_hex()).unwrap();
        let plaintext = open(
            &sealed.ciphertext,
            &revealed,
            &ctx,
            &sender.signing.verifying_key(),
        )
        .unwrap();
        assert_eq!(plaintext, b"abusive content");
    }

    #[test]
    fn wrong_key_reveal_is_rejected() {
        let sender = party();
        let recipient = party();
        let sealed = seal(
            &fixed_ctx(),
            b"content",
            &sender.signing,
            &sender.enc_pub,
            &recipient.enc_pub,
        )
        .unwrap();

        let wrong = MessageKey::generate();
        assert!(matches!(
            open(
                &sealed.ciphertext,
                &wrong,
                &fixed_ctx(),
                &sender.signing.verifying_key()
            ),
            Err(EnvelopeError::Decrypt)
        ));
    }

    #[test]
    fn surreptitious_forwarding_is_rejected() {
        // Recipient B unwraps A's message and re-encrypts the signed
        // blob to C under a context claiming C is the recipient. C (or
        // the server at reveal) must reject: the inner signature binds
        // the original recipient.
        let a = party();
        let b = party();
        let c = party();

        let original_ctx = fixed_ctx();
        let sealed = seal(
            &original_ctx,
            b"for B's eyes",
            &a.signing,
            &a.enc_pub,
            &b.enc_pub,
        )
        .unwrap();
        let k =
            unwrap_key(&sealed.wrapped_key_recipient, &b.enc_secret).unwrap();

        // B forwards to C: same plaintext+signature blob, new context.
        let forged_ctx = MessageContext {
            recipient_id: AgentId::from(Uuid::from_u128(0x4444)),
            ..original_ctx
        };
        // Simulate by decrypting and re-sealing the raw blob under a new
        // K to C's key with B unable to produce A's signature over the
        // forged context. Verification against the forged context fails.
        let plaintext = open(
            &sealed.ciphertext,
            &k,
            &original_ctx,
            &a.signing.verifying_key(),
        )
        .unwrap();
        let resealed = seal(
            &forged_ctx,
            &plaintext,
            &b.signing, // B can only sign with its own key…
            &b.enc_pub,
            &c.enc_pub,
        )
        .unwrap();
        let k2 =
            unwrap_key(&resealed.wrapped_key_recipient, &c.enc_secret).unwrap();
        // …so verifying the "message from A" against A's key fails.
        assert!(matches!(
            open(
                &resealed.ciphertext,
                &k2,
                &forged_ctx,
                &a.signing.verifying_key()
            ),
            Err(EnvelopeError::BadSignature)
        ));
    }

    #[test]
    fn tampered_context_fields_are_rejected() {
        let sender = party();
        let recipient = party();
        let ctx = fixed_ctx();
        let sealed = seal(
            &ctx,
            b"content",
            &sender.signing,
            &sender.enc_pub,
            &recipient.enc_pub,
        )
        .unwrap();
        let k =
            unwrap_key(&sealed.wrapped_key_recipient, &recipient.enc_secret)
                .unwrap();

        // AAD binds id/sender/recipient, so a swapped sender fails at
        // the AEAD layer, not just at signature verification.
        let forged = MessageContext {
            sender_id: AgentId::from(Uuid::from_u128(0x9999)),
            ..ctx
        };
        assert!(matches!(
            open(
                &sealed.ciphertext,
                &k,
                &forged,
                &sender.signing.verifying_key()
            ),
            Err(EnvelopeError::Decrypt)
        ));

        // Timestamp is outside the AAD but inside the signature.
        let forged_ts = MessageContext {
            timestamp: ctx.timestamp + 1,
            ..ctx
        };
        assert!(matches!(
            open(
                &sealed.ciphertext,
                &k,
                &forged_ts,
                &sender.signing.verifying_key()
            ),
            Err(EnvelopeError::BadSignature)
        ));
    }

    #[test]
    fn encryption_key_binding_round_trip() {
        let (signing, verifying) = crate::crypto::generate_keypair();
        let (_, enc_pub) = generate_encryption_keypair();
        let sig = sign_encryption_key(&signing, &enc_pub);
        assert!(verify_encryption_key(&verifying, &enc_pub, &sig));

        // A different X25519 key under the same signature must fail —
        // otherwise a server could swap keys.
        let (_, other_pub) = generate_encryption_keypair();
        assert!(!verify_encryption_key(&verifying, &other_pub, &sig));

        // A different identity must fail.
        let (_, other_verifying) = crate::crypto::generate_keypair();
        assert!(!verify_encryption_key(&other_verifying, &enc_pub, &sig));
    }

    #[test]
    fn secret_key_hex_round_trip() {
        let (secret, public) = generate_encryption_keypair();
        let recovered =
            encryption_secret_from_hex(&encryption_secret_to_hex(&secret))
                .unwrap();
        assert_eq!(
            EncryptionPublicKey::from(&recovered).as_bytes(),
            public.as_bytes()
        );
    }

    #[test]
    fn small_order_recipient_key_is_rejected() {
        // The identity element as a recipient key must be refused at
        // seal time (non-contributory ECDH), mirroring verify_strict's
        // posture on the signing side.
        let sender = party();
        let identity = EncryptionPublicKey::from([0u8; 32]);
        assert!(matches!(
            seal(
                &fixed_ctx(),
                b"content",
                &sender.signing,
                &sender.enc_pub,
                &identity,
            ),
            Err(EnvelopeError::NonContributory)
        ));
    }

    /// Locked test vector for the version-1 wire format. Deterministic
    /// given fixed keys, ephemeral, and nonce — reproduced here by
    /// construction through the internal functions. If this test breaks,
    /// you have changed the locked envelope format; that requires a
    /// version bump, not a vector update.
    #[test]
    fn version1_wrap_test_vector() {
        // Fixed "static" secret: bytes 1..=32; fixed KEK derivation input.
        let own_secret = EncryptionSecretKey::from([
            1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18,
            19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32,
        ]);
        let own_pub = EncryptionPublicKey::from(&own_secret);
        // Fixed "ephemeral" secret: bytes 33..=64.
        let eph_secret = EncryptionSecretKey::from([
            33u8, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48,
            49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63, 64,
        ]);
        let eph_pub = EncryptionPublicKey::from(&eph_secret);

        let kek =
            derive_kek(eph_secret.diffie_hellman(&own_pub), &eph_pub, &own_pub)
                .unwrap();
        assert_eq!(
            hex::encode(kek.as_ref()),
            "6f8d2f628f8da34c43e61aa77f0c2295683ca4c604bd0fcefb854bf961099998",
            "HKDF wrap derivation changed — version-1 format violation"
        );

        // Wrap a fixed K under that KEK (zero nonce) and confirm the
        // full blob unwraps through the public API.
        let k = MessageKey(Zeroizing::new([0xAB; 32]));
        let cipher = ChaCha20Poly1305::new(kek.as_ref().into());
        let ct = cipher
            .encrypt(&Default::default(), k.0.as_ref() as &[u8])
            .unwrap();
        let mut wrapped = vec![ENVELOPE_VERSION];
        wrapped.extend_from_slice(eph_pub.as_bytes());
        wrapped.extend_from_slice(&ct);
        assert_eq!(
            hex::encode(&wrapped),
            "015869aff450549732cbaaed5e5df9b30a6da31cb0e574\
             2bad5ad4a1a768f1a67bd58d0a30ef9b0c6ec54e24c9c820d54bac9c7daa9a5a\
             964bff0d660621ee29d472ab21f1417c46946714c61d5d13bd32",
            "wrapped-key blob changed — version-1 format violation"
        );
        let unwrapped = unwrap_key(&wrapped, &own_secret).unwrap();
        assert_eq!(unwrapped.0.as_ref(), &[0xAB; 32]);
    }
}
