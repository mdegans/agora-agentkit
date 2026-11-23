//! Copies of an operator poster's DM keys (agora
//! `docs/design-operator-posting.md`, Track B).
//!
//! An operator poster's DM key pair (X25519 to receive, Ed25519 for the
//! [`envelope`](crate::envelope)'s inner signature, bound as an agent's are
//! by [`sign_encryption_key`](crate::envelope::sign_encryption_key)) lives
//! in the browser. Each generation is stored wrapped once per *copy key*:
//! an X25519 key derived from something the operator holds (a passkey's
//! PRF output; later, recovery words). Copy keys are asymmetric so a new
//! generation can be wrapped to every copy without tapping each passkey.
//!
//! # Construction
//!
//! Each private key is wrapped with the envelope's own key wrap (the one
//! that wraps a message key, [`unwrap_key`](crate::envelope::unwrap_key)),
//! under its own HKDF `info` and AEAD `aad`. One copy of the construction:
//!
//! ```text
//! copy secret   = HKDF-SHA256(salt = none, ikm = CopySecretInput,
//!                             info = "agora/dm-copy-secret/v1" ‖ copy key id)   32 bytes, an X25519 scalar
//! label[part]   = key id ‖ copy key id ‖ part,  part = "x25519" | "ed25519"
//! per part, with a fresh ephemeral X25519 key:
//!   shared      = X25519(ephemeral secret, copy public)                         refused if all zero
//!   KEK         = HKDF-SHA256(salt = ephemeral public ‖ copy public, ikm = shared,
//!                             info = "agora/dm-key-copy/v1" ‖ label[part])
//!   wrapped     = 0x01 ‖ ephemeral public (32) ‖ ChaCha20-Poly1305(KEK, zero nonce,
//!                             aad = label[part], the raw 32-byte private key)      81 bytes
//! endorsement   = Ed25519 by the live generation's DM key over
//!                 "agora/dm-copy/v1" ‖ copy key id ‖ copy public
//! ```
//!
//! The zero nonce is the envelope's reasoning: each KEK comes from a fresh
//! ephemeral key and wraps once. The label is in both the KEK and the AAD, so
//! a blob moved to another part, generation or copy key fails to open.
//!
//! **What is wrapped is the raw private key** (the X25519 scalar, the Ed25519
//! seed): the envelope's wrap takes exactly 32 bytes, and its output is a
//! fixed [`WRAPPED_KEY_LEN`] bytes, refused at any other length. The browser
//! opens the wrap in wasm and imports the key into WebCrypto non-extractable,
//! which takes PKCS#8 (WebCrypto's `raw` is for public keys only), so it
//! builds the PKCS#8 here ([`DmPrivateKeys::x25519_pkcs8`],
//! [`DmPrivateKeys::ed25519_pkcs8`]) and zeroizes its buffers.
//!
//! # What B2's table holds
//!
//! `operator_dm_key_wraps`: `key_id`, `copy_key_id` (the primary key),
//! `wrapped_x25519` and `wrapped_ed25519`, each `BYTEA` of exactly
//! [`WRAPPED_KEY_LEN`] (81) bytes. No ephemeral key column (it is in each
//! blob) and no IV column (the nonce is zero by construction).
//!
//! # A successful unwrap proves nothing
//!
//! Copy public keys are public, so anyone (a server included) can wrap a key
//! pair of its own choosing to one, and it unwraps cleanly. After unwrapping,
//! check the keys against the generation's public keys before using them
//! ([`DmPrivateKeys::matches`]; in WebCrypto: sign and verify with the
//! Ed25519 key, and a Diffie-Hellman with a fresh ephemeral key both ways for
//! the X25519 key). The generation's public keys are in turn bound by its
//! 0x14 tap ([`DmKeysSubject`](crate::operator_security::challenge::DmKeysSubject))
//! and by the binding signature.
//!
//! The endorsement does the same for copy keys: whoever adds or rotates
//! checks that each copy public it wraps to was endorsed by a DM key it
//! trusts ([`verify_copy_endorsement`]), so a copy key planted in the table
//! gets no wrap.
//!
//! # PKCS#8
//!
//! [`x25519_pkcs8`] and [`ed25519_pkcs8`] write, and the parsers read, only
//! RFC 8410's 48-byte form. PKCS#8 is never wrapped or stored; it is only
//! what this crate hands WebCrypto's `importKey`.

use ed25519_dalek::Signer;
use hkdf::Hkdf;
use rand::rngs::OsRng;
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::crypto::{Signature, SigningKey, VerifyingKey};
pub use crate::envelope::WRAPPED_KEY_LEN;
use crate::envelope::{EncryptionPublicKey, EncryptionSecretKey};
use crate::ids::{OperatorDmCopyKeyId, OperatorDmKeyId};

/// HKDF `info` prefix of a copy secret
pub const COPY_SECRET_INFO: &[u8] = b"agora/dm-copy-secret/v1";
/// HKDF `info` prefix of a wrap's KEK
pub const COPY_WRAP_INFO: &[u8] = b"agora/dm-key-copy/v1";
/// What a copy key's endorsement covers first
pub const COPY_ENDORSEMENT_CONTEXT: &[u8] = b"agora/dm-copy/v1";
/// The fewest bytes of recovery entropy a copy secret is derived from: 128
/// bits
pub const MIN_RECOVERY_ENTROPY_LEN: usize = 16;
/// A PRF output's length (WebAuthn's `prf` extension)
pub const PRF_OUTPUT_LEN: usize = 32;
/// RFC 8410's PKCS#8 of a 32-byte private key, without the optional public
/// key
pub const PKCS8_LEN: usize = 48;

const X25519_PKCS8_PREFIX: [u8; 16] = [
    0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x6e,
    0x04, 0x22, 0x04, 0x20,
];
const ED25519_PKCS8_PREFIX: [u8; 16] = [
    0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70,
    0x04, 0x22, 0x04, 0x20,
];

/// Errors from the copy keys and their wraps
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DmKeyError {
    #[error(
        "recovery entropy needs at least {MIN_RECOVERY_ENTROPY_LEN} bytes, got {0}"
    )]
    EntropyTooShort(usize),
    /// The X25519 result was all zeros (a small-order public key)
    #[error("non-contributory X25519 public key")]
    NonContributory,
    /// A wrap didn't open: the wrong copy key, a blob of another part,
    /// generation or copy key, a tampered blob, or not a wrap at all
    #[error("the {0} wrap did not open")]
    Unwrap(DmKeyPart),
    /// Not RFC 8410's 48-byte PKCS#8 of the expected curve
    #[error("not a {0} private key in RFC 8410 PKCS#8")]
    Pkcs8(DmKeyPart),
}

/// Which private key of a generation a wrap holds
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DmKeyPart {
    X25519,
    Ed25519,
}

impl DmKeyPart {
    pub const ALL: [Self; 2] = [Self::X25519, Self::Ed25519];

    /// The part's name, last in the wrap's label
    pub const fn label(self) -> &'static [u8] {
        match self {
            Self::X25519 => b"x25519",
            Self::Ed25519 => b"ed25519",
        }
    }
}

impl std::fmt::Display for DmKeyPart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::X25519 => "X25519",
            Self::Ed25519 => "Ed25519",
        })
    }
}

/// Recovery words' decoded entropy: at least
/// [`MIN_RECOVERY_ENTROPY_LEN`] bytes. Bytes, never the words' text, so
/// that words can't be passed in as a string: a copy secret's strength is the
/// entropy, and the words' spelling (case, spaces, Unicode forms) must not
/// change which key they open. Decoding words to entropy is the wordlist's
/// job (a later slice)
#[derive(Clone)]
pub struct RecoveryEntropy(Zeroizing<Vec<u8>>);

impl TryFrom<Vec<u8>> for RecoveryEntropy {
    type Error = DmKeyError;

    fn try_from(bytes: Vec<u8>) -> Result<Self, Self::Error> {
        let bytes = Zeroizing::new(bytes);
        if bytes.len() < MIN_RECOVERY_ENTROPY_LEN {
            return Err(DmKeyError::EntropyTooShort(bytes.len()));
        }
        Ok(Self(bytes))
    }
}

impl std::fmt::Debug for RecoveryEntropy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RecoveryEntropy([REDACTED])")
    }
}

/// What a copy secret is derived from: one per [`DmCopyKind`]
///
/// [`DmCopyKind`]: crate::enums::DmCopyKind
pub enum CopySecretInput<'a> {
    /// A passkey's PRF output, given with user verification
    PrfOutput(&'a [u8; PRF_OUTPUT_LEN]),
    RecoveryWords(&'a RecoveryEntropy),
}

impl CopySecretInput<'_> {
    fn bytes(&self) -> &[u8] {
        match self {
            Self::PrfOutput(b) => b.as_slice(),
            Self::RecoveryWords(e) => e.0.as_slice(),
        }
    }
}

/// The copy key's secret, bound to the copy key's id
pub fn copy_secret(
    input: CopySecretInput<'_>,
    copy: OperatorDmCopyKeyId,
) -> EncryptionSecretKey {
    let mut info = COPY_SECRET_INFO.to_vec();
    info.extend_from_slice(copy.as_uuid().as_bytes());
    let mut out = Zeroizing::new([0u8; 32]);
    Hkdf::<Sha256>::new(None, input.bytes())
        .expand(&info, out.as_mut())
        .expect("32-byte HKDF output is always valid");
    EncryptionSecretKey::from(*out)
}

/// `key id ‖ copy key id ‖ part`: the AEAD's `aad`, and the KEK's `info`
/// after [`COPY_WRAP_INFO`]
pub fn wrap_label(
    key: OperatorDmKeyId,
    copy: OperatorDmCopyKeyId,
    part: DmKeyPart,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(32 + 7);
    out.extend_from_slice(key.as_uuid().as_bytes());
    out.extend_from_slice(copy.as_uuid().as_bytes());
    out.extend_from_slice(part.label());
    out
}

fn wrap_info(label: &[u8]) -> Vec<u8> {
    let mut info = COPY_WRAP_INFO.to_vec();
    info.extend_from_slice(label);
    info
}

/// A generation's two private keys wrapped to one copy key, each
/// [`WRAPPED_KEY_LEN`] bytes
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrappedDmKeys {
    pub x25519: [u8; WRAPPED_KEY_LEN],
    pub ed25519: [u8; WRAPPED_KEY_LEN],
}

/// A generation's private keys, opened. Zeroized on drop (both key types
/// zeroize themselves) and redacted in `Debug`. Proves nothing until
/// [`Self::matches`] the generation's public keys (see the module docs)
pub struct DmPrivateKeys {
    x25519: EncryptionSecretKey,
    ed25519: SigningKey,
}

impl DmPrivateKeys {
    pub fn new(x25519: EncryptionSecretKey, ed25519: SigningKey) -> Self {
        Self { x25519, ed25519 }
    }

    pub fn x25519(&self) -> &EncryptionSecretKey {
        &self.x25519
    }

    pub fn ed25519(&self) -> &SigningKey {
        &self.ed25519
    }

    /// The X25519 key as WebCrypto's `importKey("pkcs8", …)` takes it
    pub fn x25519_pkcs8(&self) -> Zeroizing<[u8; PKCS8_LEN]> {
        x25519_pkcs8(&self.x25519)
    }

    /// The Ed25519 key as WebCrypto's `importKey("pkcs8", …)` takes it
    pub fn ed25519_pkcs8(&self) -> Zeroizing<[u8; PKCS8_LEN]> {
        ed25519_pkcs8(&self.ed25519)
    }

    /// Both keys are the private halves of these public keys: the check that
    /// makes an unwrap mean something
    pub fn matches(
        &self,
        x25519_public: &EncryptionPublicKey,
        ed25519_public: &VerifyingKey,
    ) -> bool {
        use subtle::ConstantTimeEq;
        let x = EncryptionPublicKey::from(&self.x25519);
        let ed = self.ed25519.verifying_key();
        bool::from(
            x.as_bytes().ct_eq(x25519_public.as_bytes())
                & ed.as_bytes().ct_eq(ed25519_public.as_bytes()),
        )
    }
}

impl std::fmt::Debug for DmPrivateKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DmPrivateKeys([REDACTED])")
    }
}

/// Wrap a generation's private keys to a copy key, a fresh ephemeral key per
/// part. Refuses a low-order copy key
pub fn wrap_dm_keys(
    keys: &DmPrivateKeys,
    copy_public: &EncryptionPublicKey,
    key: OperatorDmKeyId,
    copy: OperatorDmCopyKeyId,
) -> Result<WrappedDmKeys, DmKeyError> {
    wrap_dm_keys_with(
        keys,
        copy_public,
        key,
        copy,
        [
            EncryptionSecretKey::random_from_rng(OsRng),
            EncryptionSecretKey::random_from_rng(OsRng),
        ],
    )
}

fn wrap_dm_keys_with(
    keys: &DmPrivateKeys,
    copy_public: &EncryptionPublicKey,
    key: OperatorDmKeyId,
    copy: OperatorDmCopyKeyId,
    [ephemeral_x25519, ephemeral_ed25519]: [EncryptionSecretKey; 2],
) -> Result<WrappedDmKeys, DmKeyError> {
    let wrap = |part, secret: &[u8; 32], ephemeral| {
        let label = wrap_label(key, copy, part);
        let blob = crate::envelope::wrap_secret_with(
            secret,
            copy_public,
            &wrap_info(&label),
            &label,
            ephemeral,
        )
        .map_err(|_| DmKeyError::NonContributory)?;
        Ok::<_, DmKeyError>(
            <[u8; WRAPPED_KEY_LEN]>::try_from(blob.as_slice())
                .expect("the envelope's wrap is WRAPPED_KEY_LEN bytes"),
        )
    };
    Ok(WrappedDmKeys {
        x25519: wrap(
            DmKeyPart::X25519,
            &Zeroizing::new(keys.x25519.to_bytes()),
            ephemeral_x25519,
        )?,
        ed25519: wrap(
            DmKeyPart::Ed25519,
            &Zeroizing::new(keys.ed25519.to_bytes()),
            ephemeral_ed25519,
        )?,
    })
}

/// Open a generation's wraps with the copy key's secret. A success proves
/// nothing until [`DmPrivateKeys::matches`] the generation's public keys
pub fn unwrap_dm_keys(
    wrapped: &WrappedDmKeys,
    copy_secret: &EncryptionSecretKey,
    key: OperatorDmKeyId,
    copy: OperatorDmCopyKeyId,
) -> Result<DmPrivateKeys, DmKeyError> {
    let open = |part, blob: &[u8]| {
        let label = wrap_label(key, copy, part);
        crate::envelope::unwrap_secret(
            blob,
            copy_secret,
            &wrap_info(&label),
            &label,
        )
        .map_err(|e| match e {
            crate::envelope::EnvelopeError::NonContributory => {
                DmKeyError::NonContributory
            }
            _ => DmKeyError::Unwrap(part),
        })
    };
    let x25519 = open(DmKeyPart::X25519, &wrapped.x25519)?;
    let ed25519 = open(DmKeyPart::Ed25519, &wrapped.ed25519)?;
    Ok(DmPrivateKeys {
        x25519: EncryptionSecretKey::from(*x25519),
        ed25519: crate::crypto::signing_key_from_bytes(&ed25519),
    })
}

/// What a copy key's endorsement covers
pub fn copy_endorsement_bytes(
    copy: OperatorDmCopyKeyId,
    copy_public: &EncryptionPublicKey,
) -> Vec<u8> {
    let mut out = COPY_ENDORSEMENT_CONTEXT.to_vec();
    out.extend_from_slice(copy.as_uuid().as_bytes());
    out.extend_from_slice(copy_public.as_bytes());
    out
}

/// A DM Ed25519 key endorses a copy key: whoever holds the DM keys put it
/// there
pub fn endorse_copy(
    dm_key: &SigningKey,
    copy: OperatorDmCopyKeyId,
    copy_public: &EncryptionPublicKey,
) -> Signature {
    dm_key.sign(&copy_endorsement_bytes(copy, copy_public))
}

/// The endorsement verifies (strictly) against a DM Ed25519 key
pub fn verify_copy_endorsement(
    dm_key: &VerifyingKey,
    copy: OperatorDmCopyKeyId,
    copy_public: &EncryptionPublicKey,
    endorsement: &Signature,
) -> bool {
    dm_key
        .verify_strict(&copy_endorsement_bytes(copy, copy_public), endorsement)
        .is_ok()
}

/// RFC 8410 PKCS#8 of an X25519 private key
pub fn x25519_pkcs8(
    secret: &EncryptionSecretKey,
) -> Zeroizing<[u8; PKCS8_LEN]> {
    pkcs8(X25519_PKCS8_PREFIX, &Zeroizing::new(secret.to_bytes()))
}

/// RFC 8410 PKCS#8 of an Ed25519 private key (its seed)
pub fn ed25519_pkcs8(key: &SigningKey) -> Zeroizing<[u8; PKCS8_LEN]> {
    pkcs8(ED25519_PKCS8_PREFIX, &Zeroizing::new(key.to_bytes()))
}

/// The inverse of [`x25519_pkcs8`]. Only the 48-byte form
pub fn x25519_from_pkcs8(
    bytes: &[u8],
) -> Result<EncryptionSecretKey, DmKeyError> {
    from_pkcs8(X25519_PKCS8_PREFIX, bytes, DmKeyPart::X25519)
        .map(|k| EncryptionSecretKey::from(*k))
}

/// The inverse of [`ed25519_pkcs8`]. Only the 48-byte form
pub fn ed25519_from_pkcs8(bytes: &[u8]) -> Result<SigningKey, DmKeyError> {
    from_pkcs8(ED25519_PKCS8_PREFIX, bytes, DmKeyPart::Ed25519)
        .map(|k| crate::crypto::signing_key_from_bytes(&k))
}

fn pkcs8(prefix: [u8; 16], key: &[u8; 32]) -> Zeroizing<[u8; PKCS8_LEN]> {
    let mut out = Zeroizing::new([0u8; PKCS8_LEN]);
    out[..16].copy_from_slice(&prefix);
    out[16..].copy_from_slice(key);
    out
}

fn from_pkcs8(
    prefix: [u8; 16],
    bytes: &[u8],
    part: DmKeyPart,
) -> Result<Zeroizing<[u8; 32]>, DmKeyError> {
    if bytes.len() != PKCS8_LEN || bytes[..16] != prefix {
        return Err(DmKeyError::Pkcs8(part));
    }
    let mut out = Zeroizing::new([0u8; 32]);
    out.copy_from_slice(&bytes[16..]);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn ids() -> (OperatorDmKeyId, OperatorDmCopyKeyId) {
        (
            OperatorDmKeyId::from(Uuid::from_u128(
                0x6b1e_0d2a_4c3f_4e5a_9b8c_7d6e_5f40_3122,
            )),
            OperatorDmCopyKeyId::from(Uuid::from_u128(
                0xc0b1_7e2a_9d3c_4b5e_8f60_1a2b_3c4d_5e6f,
            )),
        )
    }

    fn prf(seed: u8) -> [u8; PRF_OUTPUT_LEN] {
        [seed; PRF_OUTPUT_LEN]
    }

    fn pair(seed: u8) -> DmPrivateKeys {
        DmPrivateKeys::new(
            EncryptionSecretKey::from([seed; 32]),
            crate::crypto::signing_key_from_bytes(&[seed ^ 0xff; 32]),
        )
    }

    fn copy_key(seed: u8) -> (EncryptionSecretKey, EncryptionPublicKey) {
        let (_, copy) = ids();
        let secret = copy_secret(CopySecretInput::PrfOutput(&prf(seed)), copy);
        let public = EncryptionPublicKey::from(&secret);
        (secret, public)
    }

    /// A wrap opens with its copy key into the same keys, which match the
    /// generation's public keys; a fresh ephemeral per part and per wrap
    #[test]
    fn wrap_round_trips_and_matches() {
        let (key, copy) = ids();
        let keys = pair(3);
        let (secret, public) = copy_key(7);
        let wrapped = wrap_dm_keys(&keys, &public, key, copy).unwrap();
        assert_ne!(wrapped.x25519[1..33], wrapped.ed25519[1..33]);
        assert_ne!(wrap_dm_keys(&keys, &public, key, copy).unwrap(), wrapped);
        let opened = unwrap_dm_keys(&wrapped, &secret, key, copy).unwrap();
        assert_eq!(opened.x25519().to_bytes(), keys.x25519().to_bytes());
        assert_eq!(opened.ed25519().to_bytes(), keys.ed25519().to_bytes());
        assert!(opened.matches(
            &EncryptionPublicKey::from(keys.x25519()),
            &keys.ed25519().verifying_key()
        ));
        assert_eq!(format!("{opened:?}"), "DmPrivateKeys([REDACTED])");
        // Any key pair wraps to a public copy key: only `matches` tells
        let planted = pair(4);
        let wrapped = wrap_dm_keys(&planted, &public, key, copy).unwrap();
        let opened = unwrap_dm_keys(&wrapped, &secret, key, copy).unwrap();
        assert!(!opened.matches(
            &EncryptionPublicKey::from(keys.x25519()),
            &keys.ed25519().verifying_key()
        ));
        assert!(!opened.matches(
            &EncryptionPublicKey::from(planted.x25519()),
            &keys.ed25519().verifying_key()
        ));
    }

    /// A blob moved to the other part, another generation or another copy
    /// key, a flipped byte anywhere, or the wrong copy key: refused
    #[test]
    fn tampered_wraps_are_refused() {
        let (key, copy) = ids();
        let (secret, public) = copy_key(7);
        let wrapped = wrap_dm_keys(&pair(3), &public, key, copy).unwrap();
        let swapped = WrappedDmKeys {
            x25519: wrapped.ed25519,
            ed25519: wrapped.x25519,
        };
        assert_eq!(
            unwrap_dm_keys(&swapped, &secret, key, copy).err(),
            Some(DmKeyError::Unwrap(DmKeyPart::X25519))
        );
        let both_x = WrappedDmKeys {
            ed25519: wrapped.x25519,
            ..wrapped.clone()
        };
        assert_eq!(
            unwrap_dm_keys(&both_x, &secret, key, copy).err(),
            Some(DmKeyError::Unwrap(DmKeyPart::Ed25519))
        );
        assert!(
            unwrap_dm_keys(&wrapped, &secret, OperatorDmKeyId::new(), copy)
                .is_err()
        );
        assert!(
            unwrap_dm_keys(&wrapped, &secret, key, OperatorDmCopyKeyId::new())
                .is_err()
        );
        // The copy secret of another copy key
        let (other, _) = copy_key(8);
        assert!(unwrap_dm_keys(&wrapped, &other, key, copy).is_err());
        for i in 0..WRAPPED_KEY_LEN {
            for part in DmKeyPart::ALL {
                let mut bad = wrapped.clone();
                match part {
                    DmKeyPart::X25519 => bad.x25519[i] ^= 1,
                    DmKeyPart::Ed25519 => bad.ed25519[i] ^= 1,
                }
                assert!(
                    unwrap_dm_keys(&bad, &secret, key, copy).is_err(),
                    "byte {i} of {part}"
                );
            }
        }
    }

    /// Wrapping to a low-order copy key is refused, as is a blob whose
    /// ephemeral key is low order
    #[test]
    fn weak_keys_are_refused() {
        let (key, copy) = ids();
        let identity = EncryptionPublicKey::from([0u8; 32]);
        assert_eq!(
            wrap_dm_keys(&pair(3), &identity, key, copy).err(),
            Some(DmKeyError::NonContributory)
        );
        let (secret, public) = copy_key(7);
        let mut wrapped = wrap_dm_keys(&pair(3), &public, key, copy).unwrap();
        wrapped.x25519[1..33].copy_from_slice(&[0; 32]);
        assert_eq!(
            unwrap_dm_keys(&wrapped, &secret, key, copy).err(),
            Some(DmKeyError::NonContributory)
        );
    }

    /// The copy secret is bound to its id; recovery entropy needs 128 bits
    #[test]
    fn copy_secret_inputs() {
        let (_, copy) = ids();
        let a = copy_secret(CopySecretInput::PrfOutput(&prf(7)), copy);
        let b = copy_secret(
            CopySecretInput::PrfOutput(&prf(7)),
            OperatorDmCopyKeyId::new(),
        );
        assert_ne!(a.to_bytes(), b.to_bytes());
        assert_eq!(
            RecoveryEntropy::try_from(vec![1; 15]).err(),
            Some(DmKeyError::EntropyTooShort(15))
        );
        let entropy = RecoveryEntropy::try_from(vec![1; 16]).unwrap();
        assert_eq!(format!("{entropy:?}"), "RecoveryEntropy([REDACTED])");
        let words = copy_secret(CopySecretInput::RecoveryWords(&entropy), copy);
        assert_ne!(words.to_bytes(), a.to_bytes());
    }

    /// An endorsement verifies for its copy id and public key and DM key
    /// only
    #[test]
    fn endorsement_binds_id_key_and_endorser() {
        let (_, copy) = ids();
        let dm = crate::crypto::signing_key_from_bytes(&[5; 32]);
        let public =
            EncryptionPublicKey::from(&EncryptionSecretKey::from([6; 32]));
        let sig = endorse_copy(&dm, copy, &public);
        let vk = dm.verifying_key();
        assert!(verify_copy_endorsement(&vk, copy, &public, &sig));
        assert!(!verify_copy_endorsement(
            &vk,
            OperatorDmCopyKeyId::new(),
            &public,
            &sig
        ));
        let other =
            EncryptionPublicKey::from(&EncryptionSecretKey::from([7; 32]));
        assert!(!verify_copy_endorsement(&vk, copy, &other, &sig));
        let stranger = crate::crypto::signing_key_from_bytes(&[8; 32]);
        assert!(!verify_copy_endorsement(
            &stranger.verifying_key(),
            copy,
            &public,
            &sig
        ));
    }

    /// PKCS#8 round-trips; the other curve's, or a longer form, is refused
    #[test]
    fn pkcs8_round_trips_and_refuses_the_other_curve() {
        let keys = pair(3);
        let xp = keys.x25519_pkcs8();
        let ep = keys.ed25519_pkcs8();
        assert_eq!(
            x25519_from_pkcs8(&*xp).unwrap().to_bytes(),
            keys.x25519().to_bytes()
        );
        assert_eq!(
            ed25519_from_pkcs8(&*ep).unwrap().to_bytes(),
            keys.ed25519().to_bytes()
        );
        assert_eq!(
            x25519_from_pkcs8(&*ep).err(),
            Some(DmKeyError::Pkcs8(DmKeyPart::X25519))
        );
        assert_eq!(
            ed25519_from_pkcs8(&*xp).err(),
            Some(DmKeyError::Pkcs8(DmKeyPart::Ed25519))
        );
        let mut longer = xp.to_vec();
        longer.push(0);
        assert!(x25519_from_pkcs8(&longer).is_err());
    }

    /// `vectors/dm_keys/copy_wrap.json`: one generation wrapped to one
    /// passkey's copy key with fixed ephemeral keys, every step, the copy
    /// key's endorsement, and each key as a JWK
    mod copy_wrap_vector {
        use super::*;
        use serde_json::json;

        fn path() -> std::path::PathBuf {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("vectors/dm_keys/copy_wrap.json")
        }

        /// RFC 4648 §5 base64url without padding, as JWK writes it. Here,
        /// for the vector only: the crate has no base64 dependency
        fn b64url(bytes: &[u8]) -> String {
            const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
            let mut out = String::new();
            for chunk in bytes.chunks(3) {
                let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| {
                    n | (u32::from(*b) << (16 - 8 * i))
                });
                for i in 0..=chunk.len() {
                    out.push(char::from(
                        ALPHABET[((n >> (18 - 6 * i)) & 63) as usize],
                    ));
                }
            }
            out
        }

        #[test]
        fn b64url_matches_rfc_4648() {
            // RFC 4648 §10's vectors, unpadded
            for (input, want) in [
                ("", ""),
                ("f", "Zg"),
                ("fo", "Zm8"),
                ("foo", "Zm9v"),
                ("foob", "Zm9vYg"),
                ("fooba", "Zm9vYmE"),
                ("foobar", "Zm9vYmFy"),
            ] {
                assert_eq!(b64url(input.as_bytes()), want);
            }
            assert_eq!(b64url(&[0xfb, 0xff]), "-_8");
        }

        fn jwk(crv: &str, d: &[u8], x: &[u8]) -> serde_json::Value {
            json!({"kty": "OKP", "crv": crv, "d": b64url(d), "x": b64url(x)})
        }

        fn render() -> String {
            let (key, copy) = ids();
            let prf_output = prf(0x61);
            let copy_secret =
                copy_secret(CopySecretInput::PrfOutput(&prf_output), copy);
            let copy_public = EncryptionPublicKey::from(&copy_secret);
            let keys = DmPrivateKeys::new(
                EncryptionSecretKey::from([0x62; 32]),
                crate::crypto::signing_key_from_bytes(&[0x63; 32]),
            );
            let dm_x25519_public = EncryptionPublicKey::from(keys.x25519());
            let dm_ed25519_public = keys.ed25519().verifying_key();
            let binding = crate::envelope::sign_encryption_key(
                keys.ed25519(),
                &dm_x25519_public,
            );
            let endorsement = endorse_copy(keys.ed25519(), copy, &copy_public);
            assert!(verify_copy_endorsement(
                &dm_ed25519_public,
                copy,
                &copy_public,
                &endorsement
            ));
            let ephemeral = |fill| EncryptionSecretKey::from([fill; 32]);
            let wrapped = wrap_dm_keys_with(
                &keys,
                &copy_public,
                key,
                copy,
                [ephemeral(0x64), ephemeral(0x65)],
            )
            .unwrap();
            let opened =
                unwrap_dm_keys(&wrapped, &copy_secret, key, copy).unwrap();
            assert!(opened.matches(&dm_x25519_public, &dm_ed25519_public));

            let part = |part, fill, blob: &[u8; WRAPPED_KEY_LEN]| {
                let eph = ephemeral(fill);
                let eph_public = EncryptionPublicKey::from(&eph);
                let shared = eph.diffie_hellman(&copy_public);
                let label = wrap_label(key, copy, part);
                let kek = crate::envelope::derive_kek_from_bytes(
                    shared.as_bytes(),
                    &eph_public,
                    &copy_public,
                    &wrap_info(&label),
                )
                .unwrap();
                json!({
                    "ephemeral_secret": hex::encode(eph.to_bytes()),
                    "ephemeral_public": hex::encode(eph_public.as_bytes()),
                    "shared": hex::encode(shared.as_bytes()),
                    "label": hex::encode(&label),
                    "kek": hex::encode(*kek),
                    "wrapped": hex::encode(blob),
                })
            };
            let vector = json!({
                "description": "One DM key generation wrapped to one copy key (`agentkit::dm_key`; agora docs/design-operator-posting.md, Track B). copy_secret = HKDF-SHA256(salt none, ikm prf_output, info \"agora/dm-copy-secret/v1\" || copy_id), an X25519 scalar (raw, unclamped). Per part (\"x25519\": the DM X25519 secret; \"ed25519\": the DM Ed25519 seed), with its own ephemeral key: label = key_id || copy_id || part; shared = X25519(ephemeral_secret, copy_public) = X25519(copy_secret, ephemeral_public); kek = HKDF-SHA256(salt ephemeral_public || copy_public, ikm shared, info \"agora/dm-key-copy/v1\" || label); wrapped = 0x01 || ephemeral_public || ChaCha20-Poly1305(kek, 12 zero bytes nonce, aad label, the raw 32-byte private key), 81 bytes: the envelope's key wrap. binding_signature is Ed25519 by the DM Ed25519 key over \"agora/enc-key/v1\" || dm_x25519_public, as an agent's encryption key is bound. copy_endorsement is Ed25519 by the DM Ed25519 key over \"agora/dm-copy/v1\" || copy_id || copy_public. `pkcs8` is what the browser imports into WebCrypto after opening a wrap; `jwk` gives each private key as an OKP JWK (d and x base64url, unpadded). Ids are UUID bytes, big-endian as written.",
                "prf_output": hex::encode(prf_output),
                "key_id": key,
                "copy_id": copy,
                "copy_secret": hex::encode(copy_secret.to_bytes()),
                "copy_public": hex::encode(copy_public.as_bytes()),
                "dm_x25519_secret": hex::encode(keys.x25519().to_bytes()),
                "dm_x25519_public": hex::encode(dm_x25519_public.as_bytes()),
                "dm_ed25519_seed": hex::encode(keys.ed25519().to_bytes()),
                "dm_ed25519_public": hex::encode(dm_ed25519_public.to_bytes()),
                "binding_signature": hex::encode(binding.to_bytes()),
                "copy_endorsement": hex::encode(endorsement.to_bytes()),
                "wraps": {
                    "x25519": part(DmKeyPart::X25519, 0x64, &wrapped.x25519),
                    "ed25519": part(DmKeyPart::Ed25519, 0x65, &wrapped.ed25519),
                },
                "pkcs8": {
                    "x25519": hex::encode(*keys.x25519_pkcs8()),
                    "ed25519": hex::encode(*keys.ed25519_pkcs8()),
                },
                "jwk": {
                    "copy": jwk("X25519", &copy_secret.to_bytes(), copy_public.as_bytes()),
                    "dm_x25519": jwk("X25519", &keys.x25519().to_bytes(), dm_x25519_public.as_bytes()),
                    "dm_ed25519": jwk("Ed25519", &keys.ed25519().to_bytes(), &dm_ed25519_public.to_bytes()),
                },
            });
            serde_json::to_string_pretty(&vector).unwrap() + "\n"
        }

        #[test]
        fn the_vector_is_what_the_generator_writes() {
            let committed = std::fs::read_to_string(path()).unwrap();
            assert_eq!(render(), committed, "stale; run `just vectors`");
        }

        /// The committed wraps open from the file alone, through the public
        /// API
        #[test]
        fn the_committed_wraps_open() {
            let v: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(path()).unwrap())
                    .unwrap();
            let bytes = |s: &serde_json::Value| {
                hex::decode(s.as_str().unwrap()).unwrap()
            };
            let wrapped = WrappedDmKeys {
                x25519: bytes(&v["wraps"]["x25519"]["wrapped"])
                    .try_into()
                    .unwrap(),
                ed25519: bytes(&v["wraps"]["ed25519"]["wrapped"])
                    .try_into()
                    .unwrap(),
            };
            let copy_secret = EncryptionSecretKey::from(
                <[u8; 32]>::try_from(bytes(&v["copy_secret"])).unwrap(),
            );
            let id = |k: &str| v[k].as_str().unwrap().parse::<Uuid>().unwrap();
            let opened = unwrap_dm_keys(
                &wrapped,
                &copy_secret,
                OperatorDmKeyId::from(id("key_id")),
                OperatorDmCopyKeyId::from(id("copy_id")),
            )
            .unwrap();
            assert_eq!(
                hex::encode(opened.x25519().to_bytes()),
                v["dm_x25519_secret"]
            );
            assert_eq!(
                hex::encode(opened.ed25519().to_bytes()),
                v["dm_ed25519_seed"]
            );
        }

        #[test]
        #[ignore = "writes vectors/dm_keys; run it with `just vectors`"]
        fn regenerate_the_vector() {
            std::fs::create_dir_all(path().parent().unwrap()).unwrap();
            std::fs::write(path(), render()).unwrap();
        }
    }
}
