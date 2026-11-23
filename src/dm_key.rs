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
//! ```text
//! copy secret   = HKDF-SHA256(salt = none, ikm = CopySecretInput,
//!                             info = "agora/dm-copy-secret/v1" ‖ copy key id)   32 bytes, an X25519 scalar
//! shared        = X25519(ephemeral secret, copy public)                         refused if all zero
//! wrap key      = HKDF-SHA256(salt = ephemeral public ‖ copy public, ikm = shared,
//!                             info = "agora/dm-key-copy/v1" ‖ key id ‖ copy key id ‖ part)
//! part          = "x25519" | "ed25519"                                         one key per private key
//! wrapped part  = AEAD(wrap key, iv, aad = none, PKCS#8 of that private key)
//! endorsement   = Ed25519 by the live generation's DM key over
//!                 "agora/dm-copy/v1" ‖ copy key id ‖ copy public
//! ```
//!
//! Ids are UUID bytes. One fresh ephemeral key and one random 12-byte IV per
//! (generation, copy key) row; the two private keys are wrapped under
//! different keys, so the shared IV is never reused under one key.
//!
//! The wrap key derivation is the envelope's KEK
//! ([`derive_kek_from_bytes`](crate::envelope)) under its own `info`.
//!
//! **The AEAD step is not here yet.** The spec has AES-256-GCM (so
//! WebCrypto's `unwrapKey` imports the keys non-extractable); reusing the
//! envelope's ChaCha20-Poly1305 wrap is the alternative, and the Steward is
//! choosing. Every input to it is here and pinned in
//! `vectors/dm_keys/copy_wrap.json`.
//!
//! # A successful unwrap proves nothing
//!
//! Copy public keys are public, so anyone (a server included) can wrap a key
//! pair of its own choosing to one, and it unwraps cleanly. After unwrapping,
//! the page must check the keys against the generation's public keys before
//! using them: sign and verify with the Ed25519 key against `ed25519_public`,
//! and for the X25519 key, a Diffie-Hellman with a fresh ephemeral key both
//! ways (`X25519(x, eph_pub) == X25519(eph, x25519_public)`). The
//! generation's public keys are in turn bound by its 0x14 tap
//! ([`DmKeysSubject`](crate::operator_security::challenge::DmKeysSubject))
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
//! RFC 8410's 48-byte form, the one this crate makes. The wire admits wrapped
//! keys up to [`MAX_WRAPPED_LEN`], because the browser both wraps and unwraps
//! and some browsers may write the longer RFC 5958 form (with the public
//! key); Rust never needs to read those.

use ed25519_dalek::Signer;
use hkdf::Hkdf;
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::crypto::{Signature, SigningKey, VerifyingKey};
use crate::envelope::{EncryptionPublicKey, EncryptionSecretKey};
use crate::ids::{OperatorDmCopyKeyId, OperatorDmKeyId};

/// HKDF `info` prefix of a copy secret
pub const COPY_SECRET_INFO: &[u8] = b"agora/dm-copy-secret/v1";
/// HKDF `info` prefix of a wrap key
pub const COPY_WRAP_INFO: &[u8] = b"agora/dm-key-copy/v1";
/// What a copy key's endorsement covers first
pub const COPY_ENDORSEMENT_CONTEXT: &[u8] = b"agora/dm-copy/v1";
/// The fewest bytes of recovery entropy a copy secret is derived from: 128
/// bits
pub const MIN_RECOVERY_ENTROPY_LEN: usize = 16;
/// A PRF output's length (WebAuthn's `prf` extension)
pub const PRF_OUTPUT_LEN: usize = 32;
/// The IV each wrap row stores, 12 bytes as WebCrypto's AES-GCM takes it
pub const IV_LEN: usize = 12;
/// The AEAD tag
pub const TAG_LEN: usize = 16;
/// RFC 8410's PKCS#8 of a 32-byte private key, without the optional public
/// key
pub const PKCS8_LEN: usize = 48;
/// The shortest wrapped private key: [`PKCS8_LEN`] and its tag
pub const MIN_WRAPPED_LEN: usize = PKCS8_LEN + TAG_LEN;
/// The longest wrapped private key accepted: room for a browser that adds
/// the optional attributes or public key to its PKCS#8 (RFC 5958 v2), and
/// refused beyond, never cut
pub const MAX_WRAPPED_LEN: usize = 160;

const X25519_PKCS8_PREFIX: [u8; 16] = [
    0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x6e,
    0x04, 0x22, 0x04, 0x20,
];
const ED25519_PKCS8_PREFIX: [u8; 16] = [
    0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70,
    0x04, 0x22, 0x04, 0x20,
];

/// Errors from the copy-key derivations
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DmKeyError {
    #[error(
        "recovery entropy needs at least {MIN_RECOVERY_ENTROPY_LEN} bytes, got {0}"
    )]
    EntropyTooShort(usize),
    /// The X25519 result was all zeros (a small-order public key)
    #[error("non-contributory X25519 public key")]
    NonContributory,
    /// Not RFC 8410's 48-byte PKCS#8 of the expected curve
    #[error("not a {0} private key in RFC 8410 PKCS#8")]
    Pkcs8(DmKeyPart),
}

impl From<crate::envelope::EnvelopeError> for DmKeyError {
    /// Only `NonContributory` reaches here: the KDF's one refusal
    fn from(e: crate::envelope::EnvelopeError) -> Self {
        debug_assert!(
            matches!(e, crate::envelope::EnvelopeError::NonContributory),
            "{e}"
        );
        Self::NonContributory
    }
}

/// Which private key of a generation a wrap holds
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DmKeyPart {
    X25519,
    Ed25519,
}

impl DmKeyPart {
    pub const ALL: [Self; 2] = [Self::X25519, Self::Ed25519];

    /// The last part of the wrap key's `info`
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

/// X25519, refusing an all-zero result
pub fn shared_secret(
    secret: &EncryptionSecretKey,
    public: &EncryptionPublicKey,
) -> Result<Zeroizing<[u8; 32]>, DmKeyError> {
    let shared = Zeroizing::new(*secret.diffie_hellman(public).as_bytes());
    crate::envelope::check_contributory(&shared)?;
    Ok(shared)
}

/// The key one part of one (generation, copy key) wrap is made under.
/// `shared` is X25519 of the ephemeral key and the copy key, from either
/// side
pub fn copy_wrap_key(
    shared: &[u8; 32],
    ephemeral_public: &EncryptionPublicKey,
    copy_public: &EncryptionPublicKey,
    key: OperatorDmKeyId,
    copy: OperatorDmCopyKeyId,
    part: DmKeyPart,
) -> Result<Zeroizing<[u8; 32]>, DmKeyError> {
    let mut info = COPY_WRAP_INFO.to_vec();
    info.extend_from_slice(key.as_uuid().as_bytes());
    info.extend_from_slice(copy.as_uuid().as_bytes());
    info.extend_from_slice(part.label());
    Ok(crate::envelope::derive_kek_from_bytes(
        shared,
        ephemeral_public,
        copy_public,
        &info,
    )?)
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
/// there. The browser signs with its WebCrypto key over
/// [`copy_endorsement_bytes`]; this is the same, for tests and native
/// clients
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

/// RFC 8410 PKCS#8 of an X25519 private key: what is wrapped
pub fn x25519_pkcs8(
    secret: &EncryptionSecretKey,
) -> Zeroizing<[u8; PKCS8_LEN]> {
    pkcs8(X25519_PKCS8_PREFIX, &secret.to_bytes())
}

/// RFC 8410 PKCS#8 of an Ed25519 private key (its seed): what is wrapped
pub fn ed25519_pkcs8(key: &SigningKey) -> Zeroizing<[u8; PKCS8_LEN]> {
    pkcs8(ED25519_PKCS8_PREFIX, &key.to_bytes())
}

/// The inverse of [`x25519_pkcs8`]. Only the 48-byte form (see the module
/// docs)
pub fn x25519_from_pkcs8(
    bytes: &[u8],
) -> Result<EncryptionSecretKey, DmKeyError> {
    from_pkcs8(X25519_PKCS8_PREFIX, bytes, DmKeyPart::X25519)
        .map(|k| EncryptionSecretKey::from(*k))
}

/// The inverse of [`ed25519_pkcs8`]. Only the 48-byte form (see the module
/// docs)
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

    /// Sender and opener derive the same wrap key; it differs by part, by
    /// generation, by copy key id and by copy public key
    #[test]
    fn both_sides_agree_and_every_input_counts() {
        let (key, copy) = ids();
        let copy_secret =
            copy_secret(CopySecretInput::PrfOutput(&prf(7)), copy);
        let copy_public = EncryptionPublicKey::from(&copy_secret);
        let ephemeral = EncryptionSecretKey::from([9; 32]);
        let ephemeral_public = EncryptionPublicKey::from(&ephemeral);

        let sending = shared_secret(&ephemeral, &copy_public).unwrap();
        let opening = shared_secret(&copy_secret, &ephemeral_public).unwrap();
        assert_eq!(*sending, *opening);
        let derive = |copy_public: &EncryptionPublicKey, key, copy, part| {
            copy_wrap_key(
                &sending,
                &ephemeral_public,
                copy_public,
                key,
                copy,
                part,
            )
            .unwrap()
        };
        let x = derive(&copy_public, key, copy, DmKeyPart::X25519);
        assert_ne!(*x, *derive(&copy_public, key, copy, DmKeyPart::Ed25519));
        assert_ne!(
            *x,
            *derive(
                &copy_public,
                OperatorDmKeyId::new(),
                copy,
                DmKeyPart::X25519
            )
        );
        assert_ne!(
            *x,
            *derive(
                &copy_public,
                key,
                OperatorDmCopyKeyId::new(),
                DmKeyPart::X25519
            )
        );
        // Only the copy public key changed: the salt carries it
        let other_public =
            EncryptionPublicKey::from(&EncryptionSecretKey::from([8; 32]));
        assert_ne!(*x, *derive(&other_public, key, copy, DmKeyPart::X25519));
        // The copy secret is bound to its id
        assert_ne!(
            copy_secret.to_bytes(),
            super::copy_secret(
                CopySecretInput::PrfOutput(&prf(7)),
                OperatorDmCopyKeyId::new()
            )
            .to_bytes()
        );
    }

    /// Recovery entropy: 16 bytes accepted, 15 refused; the two kinds of
    /// input give different keys from the same bytes only by their ids
    #[test]
    fn recovery_entropy_needs_128_bits() {
        let (_, copy) = ids();
        assert_eq!(
            RecoveryEntropy::try_from(vec![1; 15])
                .err()
                .map(|e| e.to_string()),
            Some(DmKeyError::EntropyTooShort(15).to_string())
        );
        let entropy = RecoveryEntropy::try_from(vec![1; 16]).unwrap();
        assert_eq!(format!("{entropy:?}"), "RecoveryEntropy([REDACTED])");
        let words = copy_secret(CopySecretInput::RecoveryWords(&entropy), copy);
        let passkey = copy_secret(CopySecretInput::PrfOutput(&prf(1)), copy);
        assert_ne!(words.to_bytes(), passkey.to_bytes());
    }

    /// A small-order key and an all-zero shared secret are refused
    #[test]
    fn weak_inputs_are_refused() {
        let (key, copy) = ids();
        let identity = EncryptionPublicKey::from([0u8; 32]);
        assert_eq!(
            shared_secret(&EncryptionSecretKey::from([9; 32]), &identity).err(),
            Some(DmKeyError::NonContributory)
        );
        assert_eq!(
            copy_wrap_key(
                &[0; 32],
                &identity,
                &identity,
                key,
                copy,
                DmKeyPart::X25519
            )
            .err(),
            Some(DmKeyError::NonContributory)
        );
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
        let x = EncryptionSecretKey::from([3; 32]);
        let e = crate::crypto::signing_key_from_bytes(&[4; 32]);
        let xp = x25519_pkcs8(&x);
        let ep = ed25519_pkcs8(&e);
        assert_eq!(x25519_from_pkcs8(&*xp).unwrap().to_bytes(), x.to_bytes());
        assert_eq!(ed25519_from_pkcs8(&*ep).unwrap().to_bytes(), e.to_bytes());
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
    /// passkey's copy key, every derived value up to the AEAD's inputs, the
    /// copy key's endorsement, and each key as a JWK
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
            let dm_x25519 = EncryptionSecretKey::from([0x62; 32]);
            let dm_x25519_public = EncryptionPublicKey::from(&dm_x25519);
            let dm_ed25519 = crate::crypto::signing_key_from_bytes(&[0x63; 32]);
            let dm_ed25519_public = dm_ed25519.verifying_key();
            let binding = crate::envelope::sign_encryption_key(
                &dm_ed25519,
                &dm_x25519_public,
            );
            assert!(crate::envelope::verify_encryption_key(
                &dm_ed25519_public,
                &dm_x25519_public,
                &binding
            ));
            let endorsement = endorse_copy(&dm_ed25519, copy, &copy_public);
            assert!(verify_copy_endorsement(
                &dm_ed25519_public,
                copy,
                &copy_public,
                &endorsement
            ));
            let ephemeral = EncryptionSecretKey::from([0x64; 32]);
            let ephemeral_public = EncryptionPublicKey::from(&ephemeral);
            let iv = [0x65u8; IV_LEN];

            let shared = shared_secret(&ephemeral, &copy_public).unwrap();
            assert_eq!(
                *shared,
                *shared_secret(&copy_secret, &ephemeral_public).unwrap()
            );
            let wrap_key = |part| {
                hex::encode(
                    *copy_wrap_key(
                        &shared,
                        &ephemeral_public,
                        &copy_public,
                        key,
                        copy,
                        part,
                    )
                    .unwrap(),
                )
            };
            let vector = json!({
                "description": "One DM key generation wrapped to one copy key (`agentkit::dm_key`; agora docs/design-operator-posting.md, Track B). copy_secret = HKDF-SHA256(salt none, ikm prf_output, info \"agora/dm-copy-secret/v1\" || copy_id), an X25519 scalar (raw, unclamped). shared = X25519(ephemeral_secret, copy_public) = X25519(copy_secret, ephemeral_public). wrap_key[part] = HKDF-SHA256(salt ephemeral_public || copy_public, ikm shared, info \"agora/dm-key-copy/v1\" || key_id || copy_id || part), part \"x25519\" or \"ed25519\". Each private key is wrapped by an AEAD under wrap_key[part] with `iv` and no AAD, over its RFC 8410 PKCS#8 (`pkcs8`). binding_signature is Ed25519 by the DM Ed25519 key over \"agora/enc-key/v1\" || dm_x25519_public, as an agent's encryption key is bound. copy_endorsement is Ed25519 by the DM Ed25519 key over \"agora/dm-copy/v1\" || copy_id || copy_public. `jwk` gives each private key as an OKP JWK (d and x base64url, unpadded). Ids are UUID bytes, big-endian as written. The wrapped outputs are not in this file yet: the AEAD (AES-256-GCM, or the envelope's ChaCha20-Poly1305) is still being chosen.",
                "prf_output": hex::encode(prf_output),
                "key_id": key,
                "copy_id": copy,
                "copy_secret": hex::encode(copy_secret.to_bytes()),
                "copy_public": hex::encode(copy_public.as_bytes()),
                "dm_x25519_secret": hex::encode(dm_x25519.to_bytes()),
                "dm_x25519_public": hex::encode(dm_x25519_public.as_bytes()),
                "dm_ed25519_seed": hex::encode(dm_ed25519.to_bytes()),
                "dm_ed25519_public": hex::encode(dm_ed25519_public.to_bytes()),
                "binding_signature": hex::encode(binding.to_bytes()),
                "copy_endorsement": hex::encode(endorsement.to_bytes()),
                "ephemeral_secret": hex::encode(ephemeral.to_bytes()),
                "ephemeral_public": hex::encode(ephemeral_public.as_bytes()),
                "shared": hex::encode(*shared),
                "iv": hex::encode(iv),
                "wrap_key": {
                    "x25519": wrap_key(DmKeyPart::X25519),
                    "ed25519": wrap_key(DmKeyPart::Ed25519),
                },
                "pkcs8": {
                    "x25519": hex::encode(*x25519_pkcs8(&dm_x25519)),
                    "ed25519": hex::encode(*ed25519_pkcs8(&dm_ed25519)),
                },
                "jwk": {
                    "copy": jwk("X25519", &copy_secret.to_bytes(), copy_public.as_bytes()),
                    "dm_x25519": jwk("X25519", &dm_x25519.to_bytes(), dm_x25519_public.as_bytes()),
                    "dm_ed25519": jwk("Ed25519", &dm_ed25519.to_bytes(), &dm_ed25519_public.to_bytes()),
                    "ephemeral": jwk("X25519", &ephemeral.to_bytes(), ephemeral_public.as_bytes()),
                },
            });
            serde_json::to_string_pretty(&vector).unwrap() + "\n"
        }

        #[test]
        fn the_vector_is_what_the_generator_writes() {
            let committed = std::fs::read_to_string(path()).unwrap();
            assert_eq!(render(), committed, "stale; run `just vectors`");
        }

        #[test]
        #[ignore = "writes vectors/dm_keys; run it with `just vectors`"]
        fn regenerate_the_vector() {
            std::fs::create_dir_all(path().parent().unwrap()).unwrap();
            std::fs::write(path(), render()).unwrap();
        }
    }
}
