//! FIDO attestation of a passkey: is this registration's authenticator a
//! YubiKey? (agora `docs/design-operator-api.md`, *YubiKey passkeys: FIDO
//! attestation*.)
//!
//! A **YubiKey** is a passkey whose registration carried a `packed`
//! attestation chaining to a pinned Yubico FIDO root. Batch attestation
//! proves the model, not the device: a batch of at least 100,000 keys shares
//! its attestation key, which is enough to tell a hardware key from a synced
//! phone passkey.
//!
//! **A separate trust store.** [`YUBICO_FIDO_ANCHORS`] is never mixed with the
//! PIV anchors in [`super::piv`]: those verify PIV chains and these verify
//! FIDO registrations, and neither set is accepted for the other's job.
//! **Yubico Attestation Root 1 is never pinned**, nor its intermediates: they
//! issue the PIV CAs too. The anchors are the FIDO CAs themselves.
//!
//! **What is checked** ([`FidoTrustStore::verify`]):
//! - the attestation signature, ES256, over `authData ‖ SHA-256(clientData)`,
//!   with the attestation certificate's key;
//! - WebAuthn §8.2.1 on that certificate: version 3; subject `C` (two
//!   letters), `O`, `OU = Authenticator Attestation` and `CN`; `basicConstraints`
//!   present with `CA:FALSE`; and its AAGUID extension
//!   (`1.3.6.1.4.1.45724.1.1.4`), when present, not critical and equal to the
//!   AAGUID in `authData`;
//! - the shape [`super::piv`] demands of a certificate: no extension twice, no
//!   unknown critical extension, inner and outer signature algorithms equal;
//! - the certificate is signed directly by a pinned anchor. `x5c` is `[leaf]`
//!   or `[leaf, anchor]`, the second being byte for byte a pinned anchor, and
//!   then ignored.
//!
//! **Not checked:** validity dates and revocation, as for PIV. A compromised
//! batch key can't be revoked until FIDO MDS is used (the spec's *YubiKey
//! passkeys*). Only ES256 attestation is accepted: it is what YubiKeys sign
//! with; another algorithm is refused rather than trusted.
//!
//! The library that runs the WebAuthn ceremony (the server's) verifies the
//! challenge, origin and RP ID. This module takes its word for none of what is
//! here, and is never given a chance to: it reads the AAGUID out of the signed
//! bytes itself.

use sha2::{Digest, Sha256};
use x509_cert::Certificate;
use x509_cert::certificate::Version;
use x509_cert::der::Decode;
use x509_cert::der::asn1::ObjectIdentifier;
use x509_cert::ext::pkix::BasicConstraints;

use super::piv::{check_extensions, check_signature, leaf_public_key};
use super::{AttestationError, DerCertificate, OperatorPublicKey};

/// `id-fido-gen-ce-aaguid`, the AAGUID extension of §8.2.1.
pub const OID_FIDO_AAGUID: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("1.3.6.1.4.1.45724.1.1.4");

const OID_C: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.4.6");
const OID_O: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.4.10");
const OID_OU: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.4.11");
const OID_CN: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.4.3");

/// The `OU` §8.2.1 requires, literally.
const ATTESTATION_OU: &[u8] = b"Authenticator Attestation";

/// The most certificates `x5c` may hold: the attestation certificate, and
/// optionally a copy of the anchor that issued it.
const MAX_X5C: usize = 2;

/// A Yubico FIDO attestation CA, pinned. Fetched from Yubico's own site on
/// 2026-10-30 and checked by SHA-256 against the fingerprints vetted in
/// build step 4 (2026-10-29).
#[derive(Debug, Clone, Copy)]
pub struct PinnedFidoAnchor {
    /// Its subject's common name.
    pub name: &'static str,
    /// Where Yubico publishes it.
    pub source: &'static str,
    /// SHA-256 of the DER certificate, in hex.
    pub sha256: &'static str,
    der: &'static [u8],
}

/// The CAs that may issue a YubiKey's FIDO attestation certificate.
pub const YUBICO_FIDO_ANCHORS: [PinnedFidoAnchor; 4] = [
    PinnedFidoAnchor {
        name: "Yubico U2F Root CA Serial 457200631",
        source: "https://developers.yubico.com/PKI/yubico-fido-ca-1.pem",
        sha256: "0fa1386f80eb8713263ae5c1d84deb455bdf08aea50ab05503cefee82b092d42",
        der: include_bytes!("yubico/fido-u2f-root-457200631.der"),
    },
    PinnedFidoAnchor {
        name: "Yubico FIDO Attestation A 1",
        source: "https://developers.yubico.com/PKI/yubico-intermediate.pem",
        sha256: "4ebabc9cbc964f722c985f3784d057e9ef5dcf454dcb4f767fccca58cf16a4e8",
        der: include_bytes!("yubico/fido-attestation-a-1.der"),
    },
    PinnedFidoAnchor {
        name: "Yubico FIDO Attestation B 1",
        source: "https://developers.yubico.com/PKI/yubico-intermediate.pem",
        sha256: "4eadda86ff62cff987e111a07910d8554de42f71d5d6da7744610e09012dd319",
        der: include_bytes!("yubico/fido-attestation-b-1.der"),
    },
    PinnedFidoAnchor {
        name: "Yubico FIDO Attestation B2 1",
        source: "https://developers.yubico.com/PKI/yubico-intermediate.pem",
        sha256: "3c6fe819adbd80afe75dc90af7bba34c95b2b7ac64384816e033f63b7c93848b",
        der: include_bytes!("yubico/fido-attestation-b2-1.der"),
    },
];

/// Why a packed attestation doesn't show a pinned chain.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FidoAttestationError {
    /// `x5c` is empty, or longer than `[leaf, anchor]`.
    #[error("x5c holds {len} certificates")]
    ChainLength { len: usize },
    /// `x5c`'s second certificate is not a pinned anchor.
    #[error("x5c carries a certificate that is not a pinned FIDO anchor")]
    ForeignCertificate,
    /// No pinned anchor issued the attestation certificate.
    #[error(
        "the attestation certificate does not chain to a pinned FIDO anchor"
    )]
    UntrustedRoot,
    /// The attestation isn't ES256, or its key isn't P-256.
    #[error("only ES256 attestation is accepted")]
    UnsupportedAlgorithm,
    /// The signature over `authData ‖ clientDataHash` doesn't verify.
    #[error("the attestation signature does not verify")]
    BadSignature,
    /// §8.2.1: not version 3.
    #[error("the attestation certificate is not version 3")]
    NotVersion3,
    /// §8.2.1: the subject's fields.
    #[error("the attestation certificate's subject breaks 8.2.1: {field}")]
    Subject { field: &'static str },
    /// §8.2.1: `basicConstraints` missing, or `CA:TRUE`.
    #[error("the attestation certificate is not CA:FALSE")]
    NotCaFalse,
    /// §8.2.1: the AAGUID extension is malformed or critical.
    #[error("the attestation certificate's AAGUID extension is malformed")]
    AaguidExtension,
    /// §8.2.1: the AAGUID extension differs from `authData`'s.
    #[error(
        "the attestation certificate's AAGUID differs from the authenticator data's"
    )]
    AaguidMismatch,
    /// The authenticator data can't be read.
    #[error("malformed authenticator data")]
    AuthData,
    /// A certificate's shape or signature ([`super::piv`]'s checks).
    #[error(transparent)]
    Certificate(#[from] AttestationError),
}

/// What a verified attestation proves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FidoAttested {
    /// SHA-256 of the anchor it chains to: what the server stores
    /// (`operator_passkeys.attestation_root_sha256`).
    pub root_sha256: [u8; 32],
    /// The AAGUID in the authenticator data (the model).
    pub aaguid: [u8; 16],
}

/// The anchors FIDO registrations are checked against. Holds only FIDO CAs:
/// [`Self::yubico`] in production.
#[derive(Clone)]
pub struct FidoTrustStore {
    anchors: Vec<(Certificate, [u8; 32])>,
}

impl std::fmt::Debug for FidoTrustStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list()
            .entries(
                self.anchors
                    .iter()
                    .map(|(c, _)| c.tbs_certificate().subject().to_string()),
            )
            .finish()
    }
}

impl Default for FidoTrustStore {
    fn default() -> Self {
        Self::yubico()
    }
}

impl FidoTrustStore {
    /// Trust the pinned Yubico FIDO CAs, and nothing else.
    pub fn yubico() -> Self {
        Self {
            anchors: YUBICO_FIDO_ANCHORS
                .iter()
                .map(|anchor| {
                    (
                        Certificate::from_der(anchor.der)
                            .expect("a pinned anchor parses"),
                        Sha256::digest(anchor.der).into(),
                    )
                })
                .collect(),
        }
    }

    /// Trust only this mock root: tests and dev builds.
    #[cfg(feature = "mock-hardware-key")]
    pub fn mock(root: &super::mock::MockRootCa) -> Self {
        let der = root.certificate().as_der();
        Self {
            anchors: vec![(
                Certificate::from_der(der).expect("a mock root parses"),
                Sha256::digest(der).into(),
            )],
        }
    }

    /// Whether `sha256` is one of this store's anchors: whether a stored
    /// verification result makes its passkey a YubiKey.
    pub fn is_trusted_root(&self, sha256: &[u8]) -> bool {
        self.anchors.iter().any(|(_, h)| h.as_slice() == sha256)
    }

    /// Check a packed attestation: `es256_signature` (DER) by `x5c[0]` over
    /// `signed_data` (`authData ‖ SHA-256(clientDataJSON)`), and `x5c` to a
    /// pinned anchor. See the module docs.
    pub fn verify(
        &self,
        es256_signature: &[u8],
        x5c: &[&[u8]],
        signed_data: &[u8],
    ) -> Result<FidoAttested, FidoAttestationError> {
        let (leaf_der, rest) = x5c
            .split_first()
            .ok_or(FidoAttestationError::ChainLength { len: 0 })?;
        if x5c.len() > MAX_X5C {
            return Err(FidoAttestationError::ChainLength { len: x5c.len() });
        }
        let auth_data = signed_data
            .len()
            .checked_sub(32)
            .map(|n| &signed_data[..n])
            .ok_or(FidoAttestationError::AuthData)?;
        let aaguid = aaguid_of(auth_data)?;
        let parse = |der: &[u8]| {
            Certificate::from_der(der).map_err(|e| {
                FidoAttestationError::Certificate(AttestationError::Malformed {
                    index: 0,
                    message: e.to_string(),
                })
            })
        };
        // A device may hand over its issuing CA too: only the very
        // certificate of an anchor is accepted, and then ignored.
        if let Some(extra) = rest.first() {
            let extra = parse(extra)?;
            if !self.anchors.iter().any(|(anchor, _)| *anchor == extra) {
                return Err(FidoAttestationError::ForeignCertificate);
            }
        }
        let leaf = parse(leaf_der)?;
        check_extensions(0, &leaf)?;

        // Top-down, as in `piv`: no key is used before it is authenticated
        let root_sha256 = self
            .anchors
            .iter()
            .filter(|(anchor, _)| {
                anchor.tbs_certificate().subject()
                    == leaf.tbs_certificate().issuer()
            })
            .find(|(anchor, _)| {
                check_signature(
                    0,
                    &leaf,
                    anchor.tbs_certificate().subject_public_key_info(),
                )
                .is_ok()
            })
            .map(|(_, sha256)| *sha256)
            .ok_or(FidoAttestationError::UntrustedRoot)?;

        check_certificate_requirements(&leaf, &aaguid)?;
        check_attestation_signature(&leaf, es256_signature, signed_data)?;
        Ok(FidoAttested {
            root_sha256,
            aaguid,
        })
    }
}

/// §8.2.1, on the attestation certificate.
fn check_certificate_requirements(
    leaf: &Certificate,
    aaguid: &[u8; 16],
) -> Result<(), FidoAttestationError> {
    let tbs = leaf.tbs_certificate();
    if tbs.version() != Version::V3 {
        return Err(FidoAttestationError::NotVersion3);
    }
    check_subject(leaf)?;
    let constraints = tbs
        .get_extension::<BasicConstraints>()
        .map_err(|_| FidoAttestationError::NotCaFalse)?
        .map(|(_, constraints)| constraints)
        .ok_or(FidoAttestationError::NotCaFalse)?;
    if constraints.ca {
        return Err(FidoAttestationError::NotCaFalse);
    }
    for extension in tbs.extensions().into_iter().flatten() {
        if extension.extn_id == OID_FIDO_AAGUID {
            if extension.critical {
                return Err(FidoAttestationError::AaguidExtension);
            }
            // An OCTET STRING holding the 16 bytes
            let value = x509_cert::der::asn1::OctetString::from_der(
                extension.extn_value.as_bytes(),
            )
            .map_err(|_| FidoAttestationError::AaguidExtension)?;
            let value: [u8; 16] = value
                .as_bytes()
                .try_into()
                .map_err(|_| FidoAttestationError::AaguidExtension)?;
            if value != *aaguid {
                return Err(FidoAttestationError::AaguidMismatch);
            }
        }
    }
    Ok(())
}

fn check_subject(leaf: &Certificate) -> Result<(), FidoAttestationError> {
    let mut country = None;
    let mut organization = None;
    let mut unit = None;
    let mut common_name = None;
    for attribute in leaf.tbs_certificate().subject().iter() {
        let value = attribute.value.value();
        if attribute.oid == OID_C {
            country = Some(value);
        } else if attribute.oid == OID_O {
            organization = Some(value);
        } else if attribute.oid == OID_OU {
            unit = Some(value);
        } else if attribute.oid == OID_CN {
            common_name = Some(value);
        }
    }
    let subject = |field| FidoAttestationError::Subject { field };
    if !country
        .is_some_and(|c| c.len() == 2 && c.iter().all(u8::is_ascii_alphabetic))
    {
        return Err(subject("C"));
    }
    if organization.is_none_or(<[u8]>::is_empty) {
        return Err(subject("O"));
    }
    if unit != Some(ATTESTATION_OU) {
        return Err(subject("OU"));
    }
    if common_name.is_none_or(<[u8]>::is_empty) {
        return Err(subject("CN"));
    }
    Ok(())
}

fn check_attestation_signature(
    leaf: &Certificate,
    es256_signature: &[u8],
    signed_data: &[u8],
) -> Result<(), FidoAttestationError> {
    use p256::ecdsa::signature::Verifier;
    let OperatorPublicKey::EcdsaP256(key) = leaf_public_key(leaf)
        .map_err(|_| FidoAttestationError::UnsupportedAlgorithm)?
    else {
        return Err(FidoAttestationError::UnsupportedAlgorithm);
    };
    let signature = super::piv::ecdsa_from_der::<32>(es256_signature)
        .and_then(|rs| p256::ecdsa::Signature::from_slice(&rs).ok())
        .ok_or(FidoAttestationError::BadSignature)?;
    key.to_p256()
        .verify(signed_data, &signature)
        .map_err(|_| FidoAttestationError::BadSignature)
}

// ---------------------------------------------------------------------------
// Reading authenticator data
// ---------------------------------------------------------------------------

/// rpIdHash (32), flags (1), signCount (4).
const AUTH_DATA_HEADER: usize = 37;
/// The attested-credential-data flag.
const FLAG_AT: u8 = 0x40;

/// The attested credential data: AAGUID (16), id length (2), id, COSE key…
fn attested_credential_data(
    auth_data: &[u8],
) -> Result<&[u8], FidoAttestationError> {
    if auth_data.len() < AUTH_DATA_HEADER || auth_data[32] & FLAG_AT == 0 {
        return Err(FidoAttestationError::AuthData);
    }
    Ok(&auth_data[AUTH_DATA_HEADER..])
}

/// The AAGUID in authenticator data that carries attested credential data.
pub fn aaguid_of(auth_data: &[u8]) -> Result<[u8; 16], FidoAttestationError> {
    attested_credential_data(auth_data)?
        .get(..16)
        .and_then(|a| a.try_into().ok())
        .ok_or(FidoAttestationError::AuthData)
}

/// The COSE public key's bytes, exactly as the authenticator sent them, in
/// authenticator data that carries attested credential data: what
/// `SHA-256 of its COSE public key` (*Sudo*, tag 0x05) hashes.
pub fn cose_key_of(auth_data: &[u8]) -> Result<&[u8], FidoAttestationError> {
    let data = attested_credential_data(auth_data)?;
    let id_len = data
        .get(16..18)
        .map(|b| usize::from(u16::from_be_bytes([b[0], b[1]])))
        .ok_or(FidoAttestationError::AuthData)?;
    let key = data
        .get(18 + id_len..)
        .ok_or(FidoAttestationError::AuthData)?;
    let len = cbor_item_len(key, 0).ok_or(FidoAttestationError::AuthData)?;
    Ok(&key[..len])
}

/// `authData` out of an attestation object (a CBOR map with text keys).
pub fn auth_data_of(
    attestation_object: &[u8],
) -> Result<&[u8], FidoAttestationError> {
    let bad = || FidoAttestationError::AuthData;
    let (major, count, mut at) =
        cbor_head(attestation_object).ok_or_else(bad)?;
    if major != 5 {
        return Err(bad());
    }
    for _ in 0..count {
        let key_len =
            cbor_item_len(attestation_object.get(at..).ok_or_else(bad)?, 0)
                .ok_or_else(bad)?;
        let key = attestation_object.get(at..at + key_len).ok_or_else(bad)?;
        at += key_len;
        let value = attestation_object.get(at..).ok_or_else(bad)?;
        let value_len = cbor_item_len(value, 0).ok_or_else(bad)?;
        if key.get(1..) == Some(b"authData".as_slice()) {
            let (major, len, head) = cbor_head(value).ok_or_else(bad)?;
            if major != 2 {
                return Err(bad());
            }
            return value
                .get(head..head + usize::try_from(len).map_err(|_| bad())?)
                .ok_or_else(bad);
        }
        at += value_len;
    }
    Err(bad())
}

/// SHA-256 of the COSE key in an attestation object: the digest a 0x05 tap
/// and `POST /passkeys/{id}/confirm` bind.
pub fn cose_key_sha256(
    attestation_object: &[u8],
) -> Result<[u8; 32], FidoAttestationError> {
    let key = cose_key_of(auth_data_of(attestation_object)?)?;
    Ok(Sha256::digest(key).into())
}

/// A CBOR item's head: major type, argument, head length. Definite lengths
/// only, as CTAP2's canonical form has it.
fn cbor_head(bytes: &[u8]) -> Option<(u8, u64, usize)> {
    let first = *bytes.first()?;
    let (major, info) = (first >> 5, first & 0x1f);
    let (argument, head) = match info {
        0..=23 => (u64::from(info), 1),
        24 => (u64::from(*bytes.get(1)?), 2),
        25 => (
            u64::from(u16::from_be_bytes(bytes.get(1..3)?.try_into().ok()?)),
            3,
        ),
        26 => (
            u64::from(u32::from_be_bytes(bytes.get(1..5)?.try_into().ok()?)),
            5,
        ),
        27 => (u64::from_be_bytes(bytes.get(1..9)?.try_into().ok()?), 9),
        _ => return None,
    };
    Some((major, argument, head))
}

/// How many bytes the CBOR item at the start of `bytes` takes. A COSE key or
/// an attestation statement nests a few levels at most.
fn cbor_item_len(bytes: &[u8], depth: u8) -> Option<usize> {
    const MAX_DEPTH: u8 = 8;
    if depth > MAX_DEPTH {
        return None;
    }
    let (major, argument, head) = cbor_head(bytes)?;
    let argument = usize::try_from(argument).ok()?;
    let len = match major {
        0 | 1 | 7 => head,
        2 | 3 => head.checked_add(argument)?,
        4 | 5 => {
            let items = if major == 5 {
                argument.checked_mul(2)?
            } else {
                argument
            };
            let mut at = head;
            for _ in 0..items {
                at = at
                    .checked_add(cbor_item_len(bytes.get(at..)?, depth + 1)?)?;
            }
            at
        }
        6 => head.checked_add(cbor_item_len(bytes.get(head..)?, depth + 1)?)?,
        _ => return None,
    };
    (len <= bytes.len()).then_some(len)
}

/// The DER of a pinned anchor, by index: for the tests of the store.
pub fn pinned_anchor_der(anchor: &PinnedFidoAnchor) -> DerCertificate {
    DerCertificate::from_der(anchor.der.to_vec())
}

#[cfg(all(test, feature = "mock-hardware-key"))]
mod tests {
    use super::super::mock::{FidoLeafOptions, MockRootCa};
    use super::*;

    const AAGUID: [u8; 16] = [7; 16];

    fn auth_data(aaguid: &[u8; 16]) -> Vec<u8> {
        let mut out = vec![0u8; 32];
        out.push(0x45); // UP | UV | AT
        out.extend(1u32.to_be_bytes());
        out.extend(aaguid);
        out.extend(4u16.to_be_bytes());
        out.extend([1, 2, 3, 4]);
        // {1: 1, 3: -8, -1: 6, -2: h'0505'}
        out.extend([0xa4, 1, 1, 3, 0x27, 0x20, 6, 0x21, 0x42, 5, 5]);
        out
    }

    fn signed(aaguid: &[u8; 16]) -> Vec<u8> {
        let mut out = auth_data(aaguid);
        out.extend([9u8; 32]);
        out
    }

    fn attest(
        root: &MockRootCa,
        options: FidoLeafOptions,
        signed_data: &[u8],
    ) -> (DerCertificate, Vec<u8>) {
        let leaf = root.issue_fido_attestation(options);
        let signature = leaf.sign_es256(signed_data);
        (leaf.certificate().clone(), signature)
    }

    #[test]
    fn the_pins_are_the_vetted_certificates() {
        for anchor in &YUBICO_FIDO_ANCHORS {
            assert_eq!(
                hex::encode(Sha256::digest(anchor.der)),
                anchor.sha256,
                "{}",
                anchor.name
            );
            let cert = Certificate::from_der(anchor.der).unwrap();
            let subject = cert.tbs_certificate().subject().to_string();
            assert!(subject.contains(anchor.name), "{subject}");
        }
    }

    #[test]
    fn no_pin_is_root_1_or_a_piv_ca() {
        let store = FidoTrustStore::yubico();
        // Yubico Attestation Root 1, and its intermediates
        for forbidden in [
            "62760c6a6ef91679f454c8902b80fd009825b3f25da90f1fbace2ec6586cd5a8",
            "4698a1d3389c3ec60016c216250f1d0439922832d65142327436376dc2942b55",
            "d4cc3f456fdaf4e7812a21aab1dfe9d8e27d24e2fd2d6f21c9940109f0daa754",
            // The PIV anchors
            "63ece914e54dd87915f34033c85af4c0696ba1512f8add66ced738331207b546",
            "6de693f05376f5d8ca29069261e1c8626c75d503bd2edbfd75354cad1f722870",
            "2d55b7998f4e42569d6d8fa382b6dc77d1dacf07358b19701163892922b17052",
            "0c90b7d184a36edf50a35f9be935f0c5689bfdcfe5bdd073366cafb49061a440",
        ] {
            assert!(!store.is_trusted_root(&hex::decode(forbidden).unwrap()));
        }
    }

    #[test]
    fn a_good_chain_verifies_and_names_its_root() {
        let root = MockRootCa::generate();
        let store = FidoTrustStore::mock(&root);
        let data = signed(&AAGUID);
        let (leaf, signature) =
            attest(&root, FidoLeafOptions::good(AAGUID), &data);
        let attested =
            store.verify(&signature, &[leaf.as_der()], &data).unwrap();
        assert_eq!(attested.aaguid, AAGUID);
        assert!(store.is_trusted_root(&attested.root_sha256));
        // With a copy of the anchor too
        store
            .verify(
                &signature,
                &[leaf.as_der(), root.certificate().as_der()],
                &data,
            )
            .unwrap();
    }

    #[test]
    fn a_chain_to_another_root_is_not_a_yubikey() {
        let (theirs, ours) = (MockRootCa::generate(), MockRootCa::generate());
        let data = signed(&AAGUID);
        let (leaf, signature) =
            attest(&theirs, FidoLeafOptions::good(AAGUID), &data);
        // Another mock root, and the real Yubico store
        for store in [FidoTrustStore::mock(&ours), FidoTrustStore::yubico()] {
            assert_eq!(
                store.verify(&signature, &[leaf.as_der()], &data),
                Err(FidoAttestationError::UntrustedRoot)
            );
        }
        // Bringing its own root along doesn't help
        assert_eq!(
            FidoTrustStore::mock(&ours).verify(
                &signature,
                &[leaf.as_der(), theirs.certificate().as_der()],
                &data
            ),
            Err(FidoAttestationError::ForeignCertificate)
        );
    }

    #[test]
    fn the_piv_anchors_are_not_fido_anchors() {
        // A PIV CA in x5c is foreign to the FIDO store, by name and by bytes
        let store = FidoTrustStore::yubico();
        let root = MockRootCa::generate();
        let data = signed(&AAGUID);
        let (leaf, signature) =
            attest(&root, FidoLeafOptions::good(AAGUID), &data);
        let piv = include_bytes!("yubico/piv-attestation-a-1.der");
        assert_eq!(
            store.verify(&signature, &[leaf.as_der(), piv], &data),
            Err(FidoAttestationError::ForeignCertificate)
        );
    }

    #[test]
    fn a_signature_by_another_key_is_refused() {
        let root = MockRootCa::generate();
        let store = FidoTrustStore::mock(&root);
        let data = signed(&AAGUID);
        let (leaf, _) = attest(&root, FidoLeafOptions::good(AAGUID), &data);
        let (_, other) = attest(&root, FidoLeafOptions::good(AAGUID), &data);
        assert_eq!(
            store.verify(&other, &[leaf.as_der()], &data),
            Err(FidoAttestationError::BadSignature)
        );
        // ...and one over other bytes (another challenge)
        let (leaf, signature) =
            attest(&root, FidoLeafOptions::good(AAGUID), &data);
        let mut other_data = data.clone();
        *other_data.last_mut().unwrap() ^= 1;
        assert_eq!(
            store.verify(&signature, &[leaf.as_der()], &other_data),
            Err(FidoAttestationError::BadSignature)
        );
    }

    #[test]
    fn section_8_2_1_is_enforced() {
        let root = MockRootCa::generate();
        let store = FidoTrustStore::mock(&root);
        let data = signed(&AAGUID);
        let check = |options| {
            let (leaf, signature) = attest(&root, options, &data);
            store.verify(&signature, &[leaf.as_der()], &data)
        };
        let good = FidoLeafOptions::good(AAGUID);
        assert_eq!(
            check(FidoLeafOptions {
                aaguid: Some([8; 16]),
                ..good.clone()
            }),
            Err(FidoAttestationError::AaguidMismatch)
        );
        assert_eq!(
            check(FidoLeafOptions {
                aaguid_critical: true,
                ..good.clone()
            }),
            // Refused as an unknown critical extension, before 8.2.1's own
            Err(FidoAttestationError::Certificate(
                AttestationError::UnknownCriticalExtension {
                    index: 0,
                    oid: OID_FIDO_AAGUID.to_string(),
                }
            ))
        );
        assert!(
            check(FidoLeafOptions {
                aaguid: None,
                ..good.clone()
            })
            .is_ok(),
            "the AAGUID extension is optional"
        );
        assert_eq!(
            check(FidoLeafOptions {
                subject: "C=SE,O=Yubico AB,OU=Something Else,CN=x",
                ..good.clone()
            }),
            Err(FidoAttestationError::Subject { field: "OU" })
        );
        assert_eq!(
            check(FidoLeafOptions {
                subject: "O=Yubico AB,OU=Authenticator Attestation,CN=x",
                ..good.clone()
            }),
            Err(FidoAttestationError::Subject { field: "C" })
        );
        assert_eq!(
            check(FidoLeafOptions {
                ca: true,
                ..good.clone()
            }),
            Err(FidoAttestationError::NotCaFalse)
        );
        assert_eq!(
            check(FidoLeafOptions {
                omit_basic_constraints: true,
                ..good
            }),
            Err(FidoAttestationError::NotCaFalse)
        );
    }

    #[test]
    fn x5c_is_a_leaf_and_at_most_its_anchor() {
        let root = MockRootCa::generate();
        let store = FidoTrustStore::mock(&root);
        let data = signed(&AAGUID);
        let (leaf, signature) =
            attest(&root, FidoLeafOptions::good(AAGUID), &data);
        assert_eq!(
            store.verify(&signature, &[], &data),
            Err(FidoAttestationError::ChainLength { len: 0 })
        );
        let anchor = root.certificate().as_der();
        assert_eq!(
            store.verify(&signature, &[leaf.as_der(), anchor, anchor], &data),
            Err(FidoAttestationError::ChainLength { len: 3 })
        );
    }

    /// A real YubiKey's registration: "Real attestation object from SKY3" in
    /// Yubico's java-webauthn-server (`RegistrationTestData.scala`,
    /// `BasicAttestationRsaReal`; `attStmt` is packed ES256, chain to the U2F
    /// root). The one test that exercises a certificate Yubico made.
    #[test]
    fn a_real_yubikey_chains_to_the_u2f_root() {
        let decode = |text: &str| hex::decode(text.trim()).unwrap();
        let object =
            decode(include_str!("yubico/real-sky3-attestation-object.hex"));
        let client = decode(include_str!("yubico/real-sky3-client-data.hex"));
        // Walk the map to `attStmt`: {"alg", "sig", "x5c": [leaf]}
        let (_, entries, mut at) = cbor_head(&object).unwrap();
        let mut statement = None;
        for _ in 0..entries {
            let key = cbor_item_len(&object[at..], 0).unwrap();
            let is_statement = &object[at + 1..at + key] == b"attStmt";
            at += key;
            let value = cbor_item_len(&object[at..], 0).unwrap();
            if is_statement {
                statement = Some(&object[at..at + value]);
            }
            at += value;
        }
        let statement = statement.unwrap();
        let bytes_after = |name: &[u8]| {
            let at = statement
                .windows(name.len())
                .position(|w| w == name)
                .unwrap()
                + name.len();
            &statement[at..]
        };
        let sig = bytes_after(b"csig");
        let (_, sig_len, sig_head) = cbor_head(sig).unwrap();
        let sig = &sig[sig_head..sig_head + sig_len as usize];
        let x5c = bytes_after(b"cx5c");
        let (_, _, array_head) = cbor_head(x5c).unwrap();
        let leaf = &x5c[array_head..];
        let (_, leaf_len, leaf_head) = cbor_head(leaf).unwrap();
        let leaf = &leaf[leaf_head..leaf_head + leaf_len as usize];

        let mut signed = auth_data_of(&object).unwrap().to_vec();
        signed.extend(Sha256::digest(&client));
        let attested = FidoTrustStore::yubico()
            .verify(sig, &[leaf], &signed)
            .unwrap();
        assert_eq!(
            hex::encode(attested.root_sha256),
            YUBICO_FIDO_ANCHORS[0].sha256
        );
    }

    #[test]
    fn cose_keys_and_auth_data_are_read_exactly() {
        let auth = auth_data(&AAGUID);
        assert_eq!(aaguid_of(&auth).unwrap(), AAGUID);
        let cose = cose_key_of(&auth).unwrap();
        assert_eq!(cose, [0xa4, 1, 1, 3, 0x27, 0x20, 6, 0x21, 0x42, 5, 5]);
        // Extensions after the key are not part of it
        let mut with_extensions = auth.clone();
        with_extensions.extend([0xa1, 0x61, b'x', 0x01]);
        assert_eq!(cose_key_of(&with_extensions).unwrap(), cose);

        // {"fmt": "none", "attStmt": {}, "authData": …}
        let mut object = vec![0xa3, 0x63];
        object.extend(b"fmt");
        object.push(0x64);
        object.extend(b"none");
        object.push(0x67);
        object.extend(b"attStmt");
        object.extend([0xa0, 0x68]);
        object.extend(b"authData");
        object.extend([0x58, auth.len() as u8]);
        object.extend(&auth);
        assert_eq!(auth_data_of(&object).unwrap(), auth);
        assert_eq!(
            cose_key_sha256(&object).unwrap().as_slice(),
            Sha256::digest(cose).as_slice()
        );

        // Truncated, or without attested credential data
        assert!(cose_key_of(&auth[..auth.len() - 1]).is_err());
        let mut no_at = auth.clone();
        no_at[32] = 0x05;
        assert!(aaguid_of(&no_at).is_err());
        assert!(auth_data_of(&[0xa1, 0x61]).is_err());
        // A nesting bomb doesn't overflow the stack
        assert!(cbor_item_len(&[0x81; 4096], 0).is_none());
    }
}
