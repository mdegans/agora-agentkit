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
//! copy secret   = HKDF-SHA256(salt = none, ikm = PRF output,
//!                             info = "agora/dm-copy-secret/v1" ‖ copy key id)   32 bytes, an X25519 scalar
//! shared        = X25519(ephemeral secret, copy public)                         refused if all zero
//! wrap key      = HKDF-SHA256(salt = ephemeral public ‖ copy public, ikm = shared,
//!                             info = "agora/dm-key-copy/v1" ‖ key id ‖ copy key id ‖ part)
//! part          = "x25519" | "ed25519"                                         one AES-256-GCM key per private key
//! wrapped part  = AES-256-GCM(wrap key, iv, aad = none, PKCS#8 of that private key)
//! ```
//!
//! Ids are UUID bytes. One fresh ephemeral key and one random 12-byte IV per
//! (generation, copy key) row; the two private keys are wrapped under
//! different keys, so the shared IV is never reused under one key. PKCS#8
//! is RFC 8410's 48-byte form, which WebCrypto's `wrapKey("pkcs8", …)`
//! writes for both curves (to be confirmed per browser in B3).
//!
//! The AES-GCM step itself is not here yet: it needs a dependency
//! (`aes-gcm`) that has not been approved. Every input to it is, and is
//! pinned in `vectors/dm_keys/copy_wrap.json`.

use hkdf::Hkdf;
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::crypto::SigningKey;
use crate::envelope::{EncryptionPublicKey, EncryptionSecretKey};
use crate::ids::{OperatorDmCopyKeyId, OperatorDmKeyId};

/// HKDF `info` prefix of a copy secret
pub const COPY_SECRET_INFO: &[u8] = b"agora/dm-copy-secret/v1";
/// HKDF `info` prefix of a wrap key
pub const COPY_WRAP_INFO: &[u8] = b"agora/dm-key-copy/v1";
/// The fewest bytes a copy secret is derived from: 128 bits (a PRF output
/// is 32)
pub const MIN_COPY_INPUT_LEN: usize = 16;
/// AES-GCM's IV, as WebCrypto takes it
pub const IV_LEN: usize = 12;
/// AES-GCM's tag, WebCrypto's default
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
        "a copy secret needs at least {MIN_COPY_INPUT_LEN} bytes of input, got {0}"
    )]
    InputTooShort(usize),
    /// The X25519 result was all zeros (a small-order public key)
    #[error("non-contributory X25519 public key")]
    NonContributory,
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

/// The copy key's secret, from a passkey's PRF output (or, later, recovery
/// words), bound to the copy key's id
pub fn copy_secret(
    input: &[u8],
    copy: OperatorDmCopyKeyId,
) -> Result<EncryptionSecretKey, DmKeyError> {
    if input.len() < MIN_COPY_INPUT_LEN {
        return Err(DmKeyError::InputTooShort(input.len()));
    }
    let mut info = COPY_SECRET_INFO.to_vec();
    info.extend_from_slice(copy.as_uuid().as_bytes());
    let mut out = Zeroizing::new([0u8; 32]);
    Hkdf::<Sha256>::new(None, input)
        .expand(&info, out.as_mut())
        .expect("32-byte HKDF output is always valid");
    Ok(EncryptionSecretKey::from(*out))
}

/// X25519, refusing an all-zero result
pub fn shared_secret(
    secret: &EncryptionSecretKey,
    public: &EncryptionPublicKey,
) -> Result<Zeroizing<[u8; 32]>, DmKeyError> {
    let shared = secret.diffie_hellman(public);
    if !shared.was_contributory() {
        return Err(DmKeyError::NonContributory);
    }
    Ok(Zeroizing::new(*shared.as_bytes()))
}

/// The AES-256-GCM key one part of one (generation, copy key) wrap is made
/// under. `shared` is X25519 of the ephemeral key and the copy key, from
/// either side
pub fn copy_wrap_key(
    shared: &[u8; 32],
    ephemeral_public: &EncryptionPublicKey,
    copy_public: &EncryptionPublicKey,
    key: OperatorDmKeyId,
    copy: OperatorDmCopyKeyId,
    part: DmKeyPart,
) -> Result<Zeroizing<[u8; 32]>, DmKeyError> {
    use subtle::ConstantTimeEq;
    if bool::from(shared.ct_eq(&[0u8; 32])) {
        return Err(DmKeyError::NonContributory);
    }
    let mut salt = [0u8; 64];
    salt[..32].copy_from_slice(ephemeral_public.as_bytes());
    salt[32..].copy_from_slice(copy_public.as_bytes());
    let mut info = COPY_WRAP_INFO.to_vec();
    info.extend_from_slice(key.as_uuid().as_bytes());
    info.extend_from_slice(copy.as_uuid().as_bytes());
    info.extend_from_slice(part.label());
    let mut out = Zeroizing::new([0u8; 32]);
    Hkdf::<Sha256>::new(Some(&salt), shared)
        .expand(&info, out.as_mut())
        .expect("32-byte HKDF output is always valid");
    Ok(out)
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

    /// Sender and opener derive the same wrap key; it differs by part, by
    /// generation and by copy key
    #[test]
    fn both_sides_agree_and_every_input_counts() {
        let (key, copy) = ids();
        let copy_secret = copy_secret(&[7; 32], copy).unwrap();
        let copy_public = EncryptionPublicKey::from(&copy_secret);
        let ephemeral = EncryptionSecretKey::from([9; 32]);
        let ephemeral_public = EncryptionPublicKey::from(&ephemeral);

        let sending = shared_secret(&ephemeral, &copy_public).unwrap();
        let opening = shared_secret(&copy_secret, &ephemeral_public).unwrap();
        assert_eq!(*sending, *opening);
        let derive = |key, copy, part| {
            copy_wrap_key(
                &sending,
                &ephemeral_public,
                &copy_public,
                key,
                copy,
                part,
            )
            .unwrap()
        };
        let x = derive(key, copy, DmKeyPart::X25519);
        assert_ne!(*x, *derive(key, copy, DmKeyPart::Ed25519));
        assert_ne!(
            *x,
            *derive(OperatorDmKeyId::new(), copy, DmKeyPart::X25519)
        );
        assert_ne!(
            *x,
            *derive(key, OperatorDmCopyKeyId::new(), DmKeyPart::X25519)
        );
        // The copy secret is bound to its id
        assert_ne!(
            copy_secret.to_bytes(),
            super::copy_secret(&[7; 32], OperatorDmCopyKeyId::new())
                .unwrap()
                .to_bytes()
        );
    }

    /// Too little input, a small-order key and an all-zero shared secret are
    /// refused
    #[test]
    fn weak_inputs_are_refused() {
        let (key, copy) = ids();
        assert_eq!(
            copy_secret(&[1; 15], copy).err(),
            Some(DmKeyError::InputTooShort(15))
        );
        let identity = EncryptionPublicKey::from([0u8; 32]);
        assert_eq!(
            shared_secret(&EncryptionSecretKey::from([9; 32]), &identity)
                .unwrap_err(),
            DmKeyError::NonContributory
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
            .unwrap_err(),
            DmKeyError::NonContributory
        );
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
            ed25519_from_pkcs8(&*xp).unwrap_err(),
            DmKeyError::Pkcs8(DmKeyPart::Ed25519)
        );
        let mut longer = xp.to_vec();
        longer.push(0);
        assert!(x25519_from_pkcs8(&longer).is_err());
    }

    /// `vectors/dm_keys/copy_wrap.json`: one generation wrapped to one
    /// passkey's copy key, every derived value up to AES-GCM's inputs
    mod copy_wrap_vector {
        use super::*;
        use serde_json::json;

        fn path() -> std::path::PathBuf {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("vectors/dm_keys/copy_wrap.json")
        }

        fn render() -> String {
            let (key, copy) = ids();
            let prf_output = [0x61u8; 32];
            let copy_secret = copy_secret(&prf_output, copy).unwrap();
            let copy_public = EncryptionPublicKey::from(&copy_secret);
            let dm_x25519 = EncryptionSecretKey::from([0x62; 32]);
            let dm_x25519_public = EncryptionPublicKey::from(&dm_x25519);
            let dm_ed25519 = crate::crypto::signing_key_from_bytes(&[0x63; 32]);
            let binding = crate::envelope::sign_encryption_key(
                &dm_ed25519,
                &dm_x25519_public,
            );
            assert!(crate::envelope::verify_encryption_key(
                &dm_ed25519.verifying_key(),
                &dm_x25519_public,
                &binding
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
                "description": "One DM key generation wrapped to one copy key (`agentkit::dm_key`; agora docs/design-operator-posting.md, Track B). copy_secret = HKDF-SHA256(salt none, ikm prf_output, info \"agora/dm-copy-secret/v1\" || copy_id), an X25519 scalar (raw, unclamped). shared = X25519(ephemeral_secret, copy_public) = X25519(copy_secret, ephemeral_public). wrap_key[part] = HKDF-SHA256(salt ephemeral_public || copy_public, ikm shared, info \"agora/dm-key-copy/v1\" || key_id || copy_id || part), part \"x25519\" or \"ed25519\". Each private key is wrapped as AES-256-GCM(wrap_key[part], iv, no AAD) over its RFC 8410 PKCS#8 (`pkcs8`), 16-byte tag appended. binding_signature is Ed25519 by the DM Ed25519 key over \"agora/enc-key/v1\" || dm_x25519_public, as an agent's encryption key is bound. Ids are UUID bytes, big-endian as written. The AES-GCM outputs are not in this file yet: agentkit has no AES-GCM until the dependency is approved.",
                "prf_output": hex::encode(prf_output),
                "key_id": key,
                "copy_id": copy,
                "copy_secret": hex::encode(copy_secret.to_bytes()),
                "copy_public": hex::encode(copy_public.as_bytes()),
                "dm_x25519_secret": hex::encode(dm_x25519.to_bytes()),
                "dm_x25519_public": hex::encode(dm_x25519_public.as_bytes()),
                "dm_ed25519_seed": hex::encode(dm_ed25519.to_bytes()),
                "dm_ed25519_public": hex::encode(dm_ed25519.verifying_key().to_bytes()),
                "binding_signature": hex::encode(binding.to_bytes()),
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
