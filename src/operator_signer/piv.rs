//! PIV attestation as X.509: reading a slot's attestation certificate and
//! checking it back to a pinned PIV attestation CA.
//!
//! The one parse path for every PIV attestation:
//! [`YubicoAttestationVerifier`] calls it with Yubico's PIV CAs, and the
//! mock verifier (feature `mock-hardware-key`) with a mock CA. The slot
//! certificate's facts are read from Yubico's extensions
//! (`1.3.6.1.4.1.41482.3.*`), which the mock writes too.
//!
//! **The real hierarchy.** A YubiKey's slot certificate is issued by the
//! device's own attestation certificate (`f9`, subject
//! `CN=Yubico PIV Attestation`, `CA:TRUE, pathlen:0`), which is issued by a
//! PIV attestation CA:
//! - before firmware 5.7.4, `Yubico PIV Root CA Serial 263751`;
//! - from 5.7.4, `Yubico PIV Attestation A 1`, `B 1` or `B2 1`, which sit
//!   under `Yubico Attestation Intermediate A 1` / `B 1` and `Yubico
//!   Attestation Root 1`.
//!
//! Root 1 and its two intermediates also issue the FIDO, OpenPGP, SD and
//! HSM attestation CAs, whose certificates know nothing of PIV. So the
//! anchors are the PIV CAs themselves, pinned, and never Root 1, the
//! intermediates or the rest of Yubico's bundle (the U2F root signs FIDO
//! batch keys directly; YubiKey 4 isn't supported).
//!
//! **What is checked.** PIV is a positive requirement:
//! - the device chain is `[f9]`, or `[f9, anchor]` with the anchor the same
//!   certificate as a trusted one (equal as parsed DER);
//! - `f9`'s subject is the device subject, compared as DER, so its string
//!   type counts (a YubiKey's is a UTF8String); `f9` is a CA (`cA=TRUE`, and
//!   `keyCertSign` if it has a `keyUsage`), carries no policy, and is
//!   issued by a trust anchor whose `pathLenConstraint` allows it;
//! - the slot certificate is not a CA and is issued by `f9`;
//! - unknown critical extensions, and any extension twice, are refused;
//! - each certificate's inner and outer signature algorithms agree, with
//!   the parameters the algorithm prescribes, and the hash matches the
//!   curve.
//!
//! Links are checked top-down: `f9` under the anchor first, then the slot
//! certificate under `f9`, so no signature is checked under a key not yet
//! authenticated. RSA keys are capped at 4096 bits.
//!
//! **Not checked:** validity dates (Yubico's slot certificates run to the
//! year 9999), revocation.
//!
//! **Algorithms:** issuers may sign with ECDSA P-256 / SHA-256, ECDSA
//! P-384 / SHA-384 or RSA PKCS#1 v1.5 / SHA-256 (every Yubico PIV CA and
//! `f9` is RSA). Anything else is
//! [`AttestationError::UnsupportedAlgorithm`]. Slot keys may be Ed25519 or
//! P-256.

use std::str::FromStr;

use x509_cert::Certificate;
use x509_cert::der::asn1::{Null, ObjectIdentifier};
use x509_cert::der::{Any, Decode, Encode};
use x509_cert::ext::pkix::{BasicConstraints, KeyUsage};
use x509_cert::name::Name;
use x509_cert::spki::SubjectPublicKeyInfoOwned;

use super::{
    AttestationBundle, AttestationError, AttestationVerifier, AttestedKey,
    DerCertificate, DeviceSerial, EcdsaP256PublicKey, FirmwareVersion,
    KeyPolicy, OperatorPublicKey, PinPolicy, PivAttestation, PivSerial,
    TouchPolicy,
};

/// Yubico's firmware extension: three bytes, major, minor, patch.
pub const OID_YUBICO_FIRMWARE: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("1.3.6.1.4.1.41482.3.3");
/// Yubico's serial extension: a DER INTEGER.
pub const OID_YUBICO_SERIAL: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("1.3.6.1.4.1.41482.3.7");
/// Yubico's policy extension: two bytes, PIN policy then touch policy.
pub const OID_YUBICO_POLICY: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("1.3.6.1.4.1.41482.3.8");

/// `basicConstraints`.
const OID_BASIC_CONSTRAINTS: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("2.5.29.19");
/// `keyUsage`.
const OID_KEY_USAGE: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("2.5.29.15");
/// `id-ecPublicKey`.
pub(crate) const OID_EC_PUBLIC_KEY: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("1.2.840.10045.2.1");
/// `prime256v1` (P-256).
pub(crate) const OID_P256: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("1.2.840.10045.3.1.7");
/// `ecdsa-with-SHA256`.
pub(crate) const OID_ECDSA_SHA256: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("1.2.840.10045.4.3.2");
/// `secp384r1` (P-384).
const OID_P384: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.132.0.34");
/// `ecdsa-with-SHA384`.
pub(crate) const OID_ECDSA_SHA384: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("1.2.840.10045.4.3.3");
/// `rsaEncryption`.
pub(crate) const OID_RSA: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.1");
/// `sha256WithRSAEncryption`.
pub(crate) const OID_RSA_SHA256: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.11");
/// `id-Ed25519`.
pub(crate) const OID_ED25519: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("1.3.101.112");

/// The mock root CA's subject.
pub const MOCK_ROOT_SUBJECT: &str =
    "CN=Agora MOCK PIV Root CA - NOT A REAL DEVICE";
/// A mock device's `f9`-style attestation certificate subject.
pub const MOCK_DEVICE_SUBJECT: &str =
    "CN=Agora MOCK PIV Attestation - NOT A REAL DEVICE";
/// A YubiKey's `f9` attestation certificate subject. Parsed into a `Name`
/// (a UTF8String common name, as on real keys) and compared as DER.
pub const YUBICO_DEVICE_SUBJECT: &str = "CN=Yubico PIV Attestation";

/// The most certificates a device chain holds: `f9`, and optionally a copy
/// of the trust anchor that issued it.
const MAX_DEVICE_CHAIN: usize = 2;

/// The largest RSA issuer key, in bits.
const MAX_RSA_BITS: usize = 4096;

/// A Yubico PIV attestation CA, pinned. Each was fetched from Yubico's own
/// site on 2026-10-26 and checked against an independently fetched copy.
struct PinnedAnchor {
    /// Where Yubico publishes it.
    #[cfg_attr(not(test), allow(dead_code))]
    source: &'static str,
    /// SHA-256 of the DER certificate.
    #[cfg_attr(not(test), allow(dead_code))]
    sha256: &'static str,
    der: &'static [u8],
}

/// The CAs that may issue a YubiKey's `f9` certificate.
const YUBICO_PIV_ANCHORS: [PinnedAnchor; 4] = [
    // CN=Yubico PIV Root CA Serial 263751 (firmware before 5.7.4). The
    // reissue with pathlen:1; an older copy with pathlen:0 can't issue f9.
    PinnedAnchor {
        source: "https://developers.yubico.com/PKI/yubico-piv-ca-1.pem",
        sha256: "63ece914e54dd87915f34033c85af4c0696ba1512f8add66ced738331207b546",
        der: include_bytes!("yubico/piv-root-ca-serial-263751.der"),
    },
    // CN=Yubico PIV Attestation A 1 (5.7.4+, under Intermediate A 1).
    PinnedAnchor {
        source: "https://developers.yubico.com/PKI/yubico-intermediate.pem",
        sha256: "6de693f05376f5d8ca29069261e1c8626c75d503bd2edbfd75354cad1f722870",
        der: include_bytes!("yubico/piv-attestation-a-1.der"),
    },
    // CN=Yubico PIV Attestation B 1 (5.7.4+, under Intermediate B 1).
    PinnedAnchor {
        source: "https://developers.yubico.com/PKI/yubico-intermediate.pem",
        sha256: "2d55b7998f4e42569d6d8fa382b6dc77d1dacf07358b19701163892922b17052",
        der: include_bytes!("yubico/piv-attestation-b-1.der"),
    },
    // CN=Yubico PIV Attestation B2 1 (5.7.4+, under Intermediate B 1).
    PinnedAnchor {
        source: "https://developers.yubico.com/PKI/yubico-intermediate.pem",
        sha256: "0c90b7d184a36edf50a35f9be935f0c5689bfdcfe5bdd073366cafb49061a440",
        der: include_bytes!("yubico/piv-attestation-b2-1.der"),
    },
];

/// What a slot's attestation certificate says, once its chain checks out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotFacts {
    /// The slot's key.
    pub public_key: OperatorPublicKey,
    /// The device's serial.
    pub serial: PivSerial,
    /// The device's firmware.
    pub firmware: FirmwareVersion,
    /// The key's PIN and touch policies.
    pub policy: KeyPolicy,
}

/// The CAs a verifier trusts to issue `f9`, parsed once, and the subject
/// its device certificates carry. Every anchor must be a CA.
#[derive(Clone)]
pub(crate) struct TrustedRoots {
    anchors: Vec<Certificate>,
    device_subject: Name,
}

impl TrustedRoots {
    /// Parse anchors from DER.
    pub(crate) fn new(
        anchors: &[DerCertificate],
        device_subject: &'static str,
    ) -> Result<Self, AttestationError> {
        let anchors = anchors
            .iter()
            .enumerate()
            .map(|(index, der)| parse(index, der))
            .collect::<Result<Vec<_>, _>>()?;
        for (index, anchor) in anchors.iter().enumerate() {
            check_extensions(index, anchor)?;
            if ca_path_length(index, anchor)?.is_none() {
                return Err(AttestationError::RootRefused {
                    reason: "a trust anchor is not a CA",
                });
            }
        }
        Ok(Self {
            anchors,
            device_subject: Name::from_str(device_subject)
                .expect("a valid device subject"),
        })
    }
}

impl std::fmt::Debug for TrustedRoots {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list()
            .entries(
                self.anchors
                    .iter()
                    .map(|c| c.tbs_certificate().subject().to_string()),
            )
            .finish()
    }
}

/// Checks Yubico PIV attestations against Yubico's PIV attestation CAs,
/// which this crate pins (see the module docs).
#[derive(Debug, Clone)]
pub struct YubicoAttestationVerifier {
    roots: TrustedRoots,
}

impl YubicoAttestationVerifier {
    /// Trust every pinned Yubico PIV attestation CA.
    pub fn new() -> Self {
        let pinned: Vec<DerCertificate> = YUBICO_PIV_ANCHORS
            .iter()
            .map(|anchor| DerCertificate::from_der(anchor.der.to_vec()))
            .collect();
        Self {
            roots: TrustedRoots::new(&pinned, YUBICO_DEVICE_SUBJECT)
                .expect("the pinned anchors are CAs"),
        }
    }

    /// Trust only these of the pinned CAs. Each must be one of the pinned
    /// certificates byte for byte; anything else (a mock root, Root 1, a
    /// FIDO CA, or a pinned CA's key in another certificate) is refused,
    /// whatever its name.
    pub fn from_anchors(
        anchors: &[DerCertificate],
    ) -> Result<Self, AttestationError> {
        let pinned = |anchor: &DerCertificate| {
            YUBICO_PIV_ANCHORS
                .iter()
                .any(|pinned| pinned.der == anchor.as_der())
        };
        if !anchors.iter().all(pinned) {
            return Err(AttestationError::RootRefused {
                reason: "not a pinned Yubico PIV attestation CA",
            });
        }
        Ok(Self {
            roots: TrustedRoots::new(anchors, YUBICO_DEVICE_SUBJECT)?,
        })
    }
}

impl Default for YubicoAttestationVerifier {
    fn default() -> Self {
        Self::new()
    }
}

impl AttestationVerifier for YubicoAttestationVerifier {
    type Evidence = PivAttestation;

    fn evidence(
        bundle: &AttestationBundle,
    ) -> Result<&PivAttestation, AttestationError> {
        match bundle {
            AttestationBundle::YubicoPiv(evidence) => Ok(evidence),
            AttestationBundle::MockPiv(_) => {
                Err(AttestationError::WrongFormat {
                    format: bundle.format(),
                })
            }
        }
    }

    fn verify(
        &self,
        evidence: &PivAttestation,
        claimed: &OperatorPublicKey,
    ) -> Result<AttestedKey, AttestationError> {
        let facts = verify_attestation(
            &evidence.slot_certificate,
            &evidence.device_chain,
            &self.roots,
        )?;
        attested(facts, claimed, DeviceSerial::Yubico)
    }
}

/// An [`AttestedKey`] from checked facts, if they are about `claimed`.
pub(crate) fn attested(
    facts: SlotFacts,
    claimed: &OperatorPublicKey,
    serial: fn(PivSerial) -> DeviceSerial,
) -> Result<AttestedKey, AttestationError> {
    if facts.public_key != *claimed {
        return Err(AttestationError::KeyMismatch);
    }
    Ok(AttestedKey::new(
        facts.public_key,
        serial(facts.serial),
        facts.firmware,
        facts.policy,
    ))
}

/// Check `slot_certificate` through `device_chain` (`[f9]` or
/// `[f9, anchor]`) to one of `roots`, then read its facts. See the module
/// docs for what is checked.
pub(crate) fn verify_attestation(
    slot_certificate: &DerCertificate,
    device_chain: &[DerCertificate],
    roots: &TrustedRoots,
) -> Result<SlotFacts, AttestationError> {
    let len = device_chain.len();
    if len == 0 || len > MAX_DEVICE_CHAIN {
        return Err(AttestationError::ChainLength { len });
    }
    let slot = parse(0, slot_certificate)?;
    let f9 = parse(1, &device_chain[0])?;
    // A device may hand over its issuing CA too: only the same certificate
    // as a trust anchor (equal as parsed DER) is accepted, and then ignored.
    if let Some(extra) = device_chain.get(1) {
        let extra = parse(2, extra)?;
        if !roots.anchors.contains(&extra) {
            return Err(AttestationError::ChainLength { len });
        }
    }

    check_extensions(0, &slot)?;
    check_extensions(1, &f9)?;
    if slot
        .tbs_certificate()
        .get_extension::<BasicConstraints>()
        .map_err(|e| malformed(0, e))?
        .is_some_and(|(_, constraints)| constraints.ca)
    {
        return Err(AttestationError::UnexpectedCa { index: 0 });
    }
    if has_extension(&f9, OID_YUBICO_POLICY) {
        return Err(AttestationError::PolicyAboveLeaf { index: 1 });
    }
    if *f9.tbs_certificate().subject() != roots.device_subject {
        return Err(AttestationError::NotADeviceCertificate { index: 1 });
    }
    if ca_path_length(1, &f9)?.is_none() {
        return Err(AttestationError::NotACa { index: 1 });
    }
    // `f9` issues only the slot certificate, which is not a CA, so its own
    // pathLenConstraint (0 on a YubiKey) always holds.

    // Top-down: `f9` under an anchor, then the slot under `f9`. Every anchor
    // of the right name is tried; one whose pathLenConstraint is 0 can't
    // issue `f9`, which is a CA.
    let mut path_length_exceeded = false;
    let issued_by_an_anchor = roots
        .anchors
        .iter()
        .filter(|anchor| {
            anchor.tbs_certificate().subject() == f9.tbs_certificate().issuer()
        })
        .any(|anchor| {
            let verified = check_signature(
                1,
                &f9,
                anchor.tbs_certificate().subject_public_key_info(),
            )
            .is_ok();
            // Checked at construction: every anchor is a CA.
            let allows_f9 =
                !matches!(ca_path_length(2, anchor), Ok(Some(Some(0))));
            path_length_exceeded |= verified && !allows_f9;
            verified && allows_f9
        });
    if !issued_by_an_anchor {
        return Err(if path_length_exceeded {
            AttestationError::PathLength { index: 2 }
        } else {
            AttestationError::UntrustedRoot
        });
    }

    if slot.tbs_certificate().issuer() != f9.tbs_certificate().subject() {
        return Err(AttestationError::IssuerMismatch { index: 0 });
    }
    check_signature(0, &slot, f9.tbs_certificate().subject_public_key_info())?;

    read_slot_facts(&slot)
}

fn parse(
    index: usize,
    der: &DerCertificate,
) -> Result<Certificate, AttestationError> {
    Certificate::from_der(der.as_der()).map_err(|e| malformed(index, e))
}

fn malformed(index: usize, e: impl std::fmt::Display) -> AttestationError {
    AttestationError::Malformed {
        index,
        message: e.to_string(),
    }
}

fn unsupported(index: usize, oid: ObjectIdentifier) -> AttestationError {
    AttestationError::UnsupportedAlgorithm {
        index,
        oid: oid.to_string(),
    }
}

/// No extension twice, and no critical extension this module doesn't
/// process.
fn check_extensions(
    index: usize,
    cert: &Certificate,
) -> Result<(), AttestationError> {
    let processed = [OID_BASIC_CONSTRAINTS, OID_KEY_USAGE];
    let mut seen = Vec::new();
    for extension in cert.tbs_certificate().extensions().into_iter().flatten() {
        if seen.contains(&extension.extn_id) {
            return Err(AttestationError::DuplicateExtension {
                index,
                oid: extension.extn_id.to_string(),
            });
        }
        seen.push(extension.extn_id);
        if extension.critical && !processed.contains(&extension.extn_id) {
            return Err(AttestationError::UnknownCriticalExtension {
                index,
                oid: extension.extn_id.to_string(),
            });
        }
    }
    Ok(())
}

fn has_extension(cert: &Certificate, oid: ObjectIdentifier) -> bool {
    cert.tbs_certificate()
        .extensions()
        .into_iter()
        .flatten()
        .any(|e| e.extn_id == oid)
}

/// For a CA (`basicConstraints cA=TRUE`, and `keyCertSign` if there is a
/// `keyUsage`), `Some` of its `pathLenConstraint`; `None` if it is not one.
fn ca_path_length(
    index: usize,
    cert: &Certificate,
) -> Result<Option<Option<u8>>, AttestationError> {
    let tbs = cert.tbs_certificate();
    let constraints = tbs
        .get_extension::<BasicConstraints>()
        .map_err(|e| malformed(index, e))?
        .map(|(_, constraints)| constraints)
        .filter(|constraints| constraints.ca);
    let may_sign_certificates = tbs
        .get_extension::<KeyUsage>()
        .map_err(|e| malformed(index, e))?
        .is_none_or(|(_, usage)| usage.key_cert_sign());
    Ok(constraints
        .filter(|_| may_sign_certificates)
        .map(|constraints| constraints.path_len_constraint))
}

/// Absent, or an explicit NULL.
fn absent_or_null(parameters: Option<&Any>) -> bool {
    parameters.is_none_or(|p| p.decode_as::<Null>().is_ok())
}

/// Check `cert`'s signature under `issuer_key`.
pub(crate) fn check_signature(
    index: usize,
    cert: &Certificate,
    issuer_key: &SubjectPublicKeyInfoOwned,
) -> Result<(), AttestationError> {
    let outer = cert.signature_algorithm();
    if cert.tbs_certificate().signature() != outer {
        return Err(AttestationError::AlgorithmMismatch { index });
    }
    let tbs = cert
        .tbs_certificate()
        .to_der()
        .map_err(|e| malformed(index, e))?;
    let signature = cert
        .signature()
        .as_bytes()
        .ok_or(AttestationError::BadSignature { index })?;

    if outer.oid == OID_ECDSA_SHA256 {
        use p256::ecdsa::signature::Verifier;
        if outer.parameters.is_some() {
            return Err(AttestationError::AlgorithmMismatch { index });
        }
        let OperatorPublicKey::EcdsaP256(key) = public_key(index, issuer_key)?
        else {
            return Err(unsupported(index, issuer_key.algorithm.oid));
        };
        // A certificate may carry a high `s`: malleability doesn't matter
        // for a certificate the issuer signed once.
        let signature = ecdsa_from_der::<32>(signature)
            .and_then(|rs| p256::ecdsa::Signature::from_slice(&rs).ok())
            .ok_or(AttestationError::BadSignature { index })?;
        return key
            .to_p256()
            .verify(&tbs, &signature)
            .map_err(|_| AttestationError::BadSignature { index });
    }

    if outer.oid == OID_ECDSA_SHA384 {
        use p384::ecdsa::signature::Verifier;
        if outer.parameters.is_some() {
            return Err(AttestationError::AlgorithmMismatch { index });
        }
        let key = p384_issuer_key(index, issuer_key)?;
        let signature = ecdsa_from_der::<48>(signature)
            .and_then(|rs| p384::ecdsa::Signature::from_slice(&rs).ok())
            .ok_or(AttestationError::BadSignature { index })?;
        return key
            .verify(&tbs, &signature)
            .map_err(|_| AttestationError::BadSignature { index });
    }

    if outer.oid == OID_RSA_SHA256 {
        use rsa::pkcs1::DecodeRsaPublicKey;
        use rsa::signature::Verifier;
        if !absent_or_null(outer.parameters.as_ref()) {
            return Err(AttestationError::AlgorithmMismatch { index });
        }
        if issuer_key.algorithm.oid != OID_RSA
            || !absent_or_null(issuer_key.algorithm.parameters.as_ref())
        {
            return Err(unsupported(index, issuer_key.algorithm.oid));
        }
        use rsa::traits::PublicKeyParts;
        let key = issuer_key
            .subject_public_key
            .as_bytes()
            .and_then(|der| rsa::RsaPublicKey::from_pkcs1_der(der).ok())
            .ok_or_else(|| unsupported(index, issuer_key.algorithm.oid))?;
        if key.size() * 8 > MAX_RSA_BITS {
            return Err(AttestationError::KeyTooLarge { index });
        }
        let signature = rsa::pkcs1v15::Signature::try_from(signature)
            .map_err(|_| AttestationError::BadSignature { index })?;
        return rsa::pkcs1v15::VerifyingKey::<rsa::sha2::Sha256>::new(key)
            .verify(&tbs, &signature)
            .map_err(|_| AttestationError::BadSignature { index });
    }

    Err(unsupported(index, outer.oid))
}

/// A P-384 issuer key (`id-ecPublicKey` on `secp384r1`). P-384 keys sign
/// certificates only; no operator key is P-384.
fn p384_issuer_key(
    index: usize,
    spki: &SubjectPublicKeyInfoOwned,
) -> Result<p384::ecdsa::VerifyingKey, AttestationError> {
    let oid = spki.algorithm.oid;
    if oid != OID_EC_PUBLIC_KEY {
        return Err(unsupported(index, oid));
    }
    let curve = spki
        .algorithm
        .parameters
        .as_ref()
        .and_then(|p| p.decode_as::<ObjectIdentifier>().ok())
        .ok_or(AttestationError::AlgorithmMismatch { index })?;
    if curve != OID_P384 {
        return Err(unsupported(index, curve));
    }
    spki.subject_public_key
        .as_bytes()
        .and_then(|bytes| {
            p384::ecdsa::VerifyingKey::from_sec1_bytes(bytes).ok()
        })
        .ok_or_else(|| unsupported(index, curve))
}

/// An ECDSA signature's `r ‖ s` (each `N` bytes) from its DER form,
/// `SEQUENCE { r, s }`.
fn ecdsa_from_der<const N: usize>(der: &[u8]) -> Option<Vec<u8>> {
    use x509_cert::der::asn1::Uint;
    let integers = Vec::<Uint>::from_der(der).ok()?;
    if integers.len() != 2 {
        return None;
    }
    let mut bytes = vec![0u8; 2 * N];
    for (half, integer) in bytes.chunks_mut(N).zip(&integers) {
        let value = integer.as_bytes();
        if value.len() > N {
            return None;
        }
        half[N - value.len()..].copy_from_slice(value);
    }
    Some(bytes)
}

/// An ECDSA signature's DER form from `r ‖ s` (the inverse of
/// `ecdsa_from_der`).
#[cfg(feature = "mock-hardware-key")]
pub(crate) fn ecdsa_to_der(r_s: &[u8]) -> Vec<u8> {
    use x509_cert::der::asn1::Uint;
    let integers: Vec<Uint> = r_s
        .chunks(r_s.len() / 2)
        .map(|half| Uint::new(half).expect("a half is an integer"))
        .collect();
    integers.to_der().expect("two integers encode")
}

/// The operator key a certificate's subject key is.
fn public_key(
    index: usize,
    spki: &SubjectPublicKeyInfoOwned,
) -> Result<OperatorPublicKey, AttestationError> {
    let oid = spki.algorithm.oid;
    let bytes = spki
        .subject_public_key
        .as_bytes()
        .ok_or_else(|| unsupported(index, oid))?;
    if oid == OID_ED25519 {
        if spki.algorithm.parameters.is_some() {
            return Err(AttestationError::AlgorithmMismatch { index });
        }
        let bytes: &[u8; 32] =
            bytes.try_into().map_err(|_| unsupported(index, oid))?;
        let key = ed25519_dalek::VerifyingKey::from_bytes(bytes)
            .map_err(|_| unsupported(index, oid))?;
        return Ok(OperatorPublicKey::Ed25519(key));
    }
    if oid == OID_EC_PUBLIC_KEY {
        let curve = spki
            .algorithm
            .parameters
            .as_ref()
            .and_then(|p| p.decode_as::<ObjectIdentifier>().ok());
        return match curve {
            Some(curve) if curve == OID_P256 => {
                EcdsaP256PublicKey::from_sec1(bytes)
                    .map(OperatorPublicKey::EcdsaP256)
                    .map_err(|_| unsupported(index, curve))
            }
            Some(curve) => Err(unsupported(index, curve)),
            None => Err(AttestationError::AlgorithmMismatch { index }),
        };
    }
    Err(unsupported(index, oid))
}

fn extension<'a>(
    cert: &'a Certificate,
    oid: ObjectIdentifier,
    name: &'static str,
) -> Result<&'a [u8], AttestationError> {
    cert.tbs_certificate()
        .extensions()
        .into_iter()
        .flatten()
        .find(|e| e.extn_id == oid)
        .map(|e| e.extn_value.as_bytes())
        .ok_or(AttestationError::MissingExtension { name })
}

fn read_slot_facts(cert: &Certificate) -> Result<SlotFacts, AttestationError> {
    let public_key =
        public_key(0, cert.tbs_certificate().subject_public_key_info())?;

    let serial = u32::from_der(extension(cert, OID_YUBICO_SERIAL, "serial")?)
        .map(PivSerial::new)
        .map_err(|_| AttestationError::BadExtension { name: "serial" })?;

    let firmware = match extension(cert, OID_YUBICO_FIRMWARE, "firmware")? {
        &[major, minor, patch] => FirmwareVersion::new(major, minor, patch),
        _ => return Err(AttestationError::BadExtension { name: "firmware" }),
    };

    let bad_policy = AttestationError::BadExtension { name: "policy" };
    let &[pin, touch] = extension(cert, OID_YUBICO_POLICY, "policy")? else {
        return Err(bad_policy);
    };
    let pin = match pin {
        1 => PinPolicy::Never,
        2 => PinPolicy::Once,
        3 => PinPolicy::Always,
        _ => return Err(bad_policy),
    };
    let touch = match touch {
        1 => TouchPolicy::Never,
        2 => TouchPolicy::Always,
        3 => TouchPolicy::Cached,
        _ => return Err(bad_policy),
    };

    Ok(SlotFacts {
        public_key,
        serial,
        firmware,
        policy: KeyPolicy { pin, touch },
    })
}

/// The policy extension's two bytes (the inverse of the reading above).
#[cfg(feature = "mock-hardware-key")]
pub(crate) fn policy_bytes(policy: KeyPolicy) -> [u8; 2] {
    let pin = match policy.pin {
        PinPolicy::Never => 1,
        PinPolicy::Once => 2,
        PinPolicy::Always => 3,
    };
    let touch = match policy.touch {
        TouchPolicy::Never => 1,
        TouchPolicy::Always => 2,
        TouchPolicy::Cached => 3,
    };
    [pin, touch]
}

/// A subject public key info for an operator key (the inverse of
/// `public_key`).
#[cfg(feature = "mock-hardware-key")]
pub(crate) fn spki(key: &OperatorPublicKey) -> SubjectPublicKeyInfoOwned {
    use x509_cert::spki::AlgorithmIdentifierOwned;

    let (algorithm, bytes): (AlgorithmIdentifierOwned, &[u8]) = match key {
        OperatorPublicKey::Ed25519(key) => (
            AlgorithmIdentifierOwned {
                oid: OID_ED25519,
                parameters: None,
            },
            key.as_bytes(),
        ),
        OperatorPublicKey::EcdsaP256(key) => (
            AlgorithmIdentifierOwned {
                oid: OID_EC_PUBLIC_KEY,
                parameters: Some(
                    Any::encode_from(&OID_P256).expect("an OID encodes"),
                ),
            },
            key.as_sec1(),
        ),
    };
    SubjectPublicKeyInfoOwned {
        algorithm,
        subject_public_key: x509_cert::der::asn1::BitString::from_bytes(bytes)
            .expect("a whole number of bytes"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operator_signer::{MockPivAttestation, PinPolicy, TouchPolicy};

    const REAL_SLOT: &[u8] = include_bytes!(
        "../../tests/fixtures/operator_signer/yubikey-5.7.4-slot-9c.der"
    );
    const REAL_F9: &[u8] = include_bytes!(
        "../../tests/fixtures/operator_signer/yubikey-5.7.4-f9.der"
    );
    const OTHER_SLOT: &[u8] = include_bytes!(
        "../../tests/fixtures/operator_signer/other-yubikey-slot-9c.der"
    );
    const ROOT_1: &[u8] = include_bytes!(
        "../../tests/fixtures/operator_signer/yubico-attestation-root-1.der"
    );
    const INTERMEDIATE_B_1: &[u8] = include_bytes!(
        "../../tests/fixtures/operator_signer/yubico-attestation-intermediate-b-1.der"
    );
    const FIDO_B_1: &[u8] = include_bytes!(
        "../../tests/fixtures/operator_signer/yubico-fido-attestation-b-1.der"
    );

    fn der(bytes: &[u8]) -> DerCertificate {
        DerCertificate::from_der(bytes.to_vec())
    }

    fn slot_key(bytes: &[u8]) -> OperatorPublicKey {
        let cert = Certificate::from_der(bytes).unwrap();
        public_key(0, cert.tbs_certificate().subject_public_key_info()).unwrap()
    }

    #[test]
    fn pinned_anchors_are_the_published_certificates() {
        use sha2::{Digest, Sha256};
        for anchor in &YUBICO_PIV_ANCHORS {
            assert!(
                anchor.source.starts_with("https://developers.yubico.com/")
            );
            assert_eq!(hex::encode(Sha256::digest(anchor.der)), anchor.sha256);
        }
        YubicoAttestationVerifier::new();
    }

    #[test]
    fn a_real_yubikey_5_7_4_attestation_verifies() {
        let key = slot_key(REAL_SLOT);
        let evidence = PivAttestation {
            slot_certificate: der(REAL_SLOT),
            device_chain: vec![der(REAL_F9)],
        };
        let verifier = YubicoAttestationVerifier::new();
        let attested = verifier.verify(&evidence, &key).unwrap();
        assert_eq!(
            attested.serial(),
            DeviceSerial::Yubico(PivSerial::new(38_906_121))
        );
        assert_eq!(attested.firmware(), FirmwareVersion::new(5, 7, 4));
        assert_eq!(
            attested.policy(),
            KeyPolicy {
                pin: PinPolicy::Always,
                touch: TouchPolicy::Always,
            }
        );

        // With the issuing CA appended, as a device may send it.
        let b_1 = der(include_bytes!("yubico/piv-attestation-b-1.der"));
        let with_anchor = PivAttestation {
            device_chain: vec![der(REAL_F9), b_1.clone()],
            ..evidence.clone()
        };
        verifier.verify(&with_anchor, &key).unwrap();
        // Anchored at B 1 alone, too.
        YubicoAttestationVerifier::from_anchors(&[b_1])
            .unwrap()
            .verify(&evidence, &key)
            .unwrap();

        // Another YubiKey's slot certificate, issued by the same `f9`
        // certificate and key: on 5.7.4 the device attestation key is shared
        // across devices, so the serial comes from the slot certificate.
        let other = PivAttestation {
            slot_certificate: der(OTHER_SLOT),
            device_chain: vec![der(REAL_F9)],
        };
        let attested = verifier.verify(&other, &slot_key(OTHER_SLOT)).unwrap();
        assert_eq!(
            attested.serial(),
            DeviceSerial::Yubico(PivSerial::new(38_906_327))
        );
        assert!(matches!(
            verifier.verify(&other, &key),
            Err(AttestationError::KeyMismatch)
        ));

        // Root 1 and Intermediate B 1 in the chain are not anchors.
        let long = PivAttestation {
            device_chain: vec![der(REAL_F9), der(INTERMEDIATE_B_1)],
            ..evidence
        };
        assert!(matches!(
            verifier.verify(&long, &key),
            Err(AttestationError::ChainLength { len: 2 })
        ));
    }

    /// A real legacy-hierarchy `f9` (YubiKey 5Ci, firmware 5.2.7) chains to
    /// the pinned `PIV Root CA Serial 263751`, and is a CA with pathlen 0.
    /// The certificate identifies the Steward's own key, so it isn't in the
    /// repository: the test reads it from `AGORA_TEST_LEGACY_F9_PEM` (or the
    /// default path) and skips, saying so, when it is absent. The committed
    /// legacy-shape coverage is the mock's RSA-root chain.
    #[test]
    fn a_real_legacy_f9_chains_to_the_pinned_piv_root() {
        let path =
            std::env::var("AGORA_TEST_LEGACY_F9_PEM").unwrap_or_else(|_| {
                "/tmp/claude-1001/-home-claude-agora-Projects-agora/\
             e5d7c613-732e-4706-b44b-f6af45f92470/scratchpad/real-legacy/\
             f9-yubikey-5.2.7.pem"
                    .to_owned()
            });
        let Ok(pem) = std::fs::read_to_string(&path) else {
            eprintln!(
                "SKIPPED a_real_legacy_f9_chains_to_the_pinned_piv_root: \
                 no legacy f9 at {path} (set AGORA_TEST_LEGACY_F9_PEM)"
            );
            return;
        };
        let f9 = Certificate::from_der(&pem_body(&pem)).unwrap();
        assert_eq!(
            f9.tbs_certificate().subject().to_string(),
            YUBICO_DEVICE_SUBJECT
        );
        assert_eq!(ca_path_length(1, &f9).unwrap(), Some(Some(0)));
        check_extensions(1, &f9).unwrap();
        assert!(!has_extension(&f9, OID_YUBICO_POLICY));

        let legacy = Certificate::from_der(YUBICO_PIV_ANCHORS[0].der).unwrap();
        assert_eq!(
            f9.tbs_certificate().issuer(),
            legacy.tbs_certificate().subject()
        );
        assert_eq!(ca_path_length(2, &legacy).unwrap(), Some(Some(1)));
        check_signature(
            1,
            &f9,
            legacy.tbs_certificate().subject_public_key_info(),
        )
        .unwrap();
        // And under no other pinned CA.
        for anchor in &YUBICO_PIV_ANCHORS[1..] {
            let anchor = Certificate::from_der(anchor.der).unwrap();
            assert!(
                check_signature(
                    1,
                    &f9,
                    anchor.tbs_certificate().subject_public_key_info()
                )
                .is_err()
            );
        }
    }

    /// The DER inside a single-certificate PEM.
    fn pem_body(pem: &str) -> Vec<u8> {
        const ALPHABET: &[u8] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let text: Vec<u8> = pem
            .lines()
            .skip_while(|line| !line.starts_with("-----BEGIN CERTIFICATE"))
            .skip(1)
            .take_while(|line| !line.starts_with("-----END"))
            .flat_map(|line| line.trim().bytes())
            .filter(|b| *b != b'=')
            .collect();
        let mut out = Vec::new();
        let (mut buffer, mut bits) = (0u32, 0);
        for byte in text {
            let value =
                ALPHABET.iter().position(|a| *a == byte).unwrap() as u32;
            buffer = (buffer << 6) | value;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((buffer >> bits) as u8);
                buffer &= (1 << bits) - 1;
            }
        }
        out
    }

    #[test]
    fn the_device_subject_is_the_real_f9s_as_der() {
        let f9 = Certificate::from_der(REAL_F9).unwrap();
        let expected = Name::from_str(YUBICO_DEVICE_SUBJECT).unwrap();
        assert_eq!(
            f9.tbs_certificate().subject().to_der().unwrap(),
            expected.to_der().unwrap()
        );
    }

    #[test]
    fn from_anchors_takes_only_the_pinned_certificates_themselves() {
        for anchor in &YUBICO_PIV_ANCHORS {
            YubicoAttestationVerifier::from_anchors(&[der(anchor.der)])
                .unwrap();
        }
        // The f9 (a CA, but not pinned) and a pinned CA's certificate with
        // one byte changed are both refused.
        let mut altered = YUBICO_PIV_ANCHORS[2].der.to_vec();
        let last = altered.len() - 1;
        altered[last] ^= 1;
        for refused in [der(REAL_F9), DerCertificate::from_der(altered)] {
            assert!(matches!(
                YubicoAttestationVerifier::from_anchors(&[refused]),
                Err(AttestationError::RootRefused { .. })
            ));
        }
    }

    #[test]
    fn only_the_pinned_piv_cas_can_be_anchors() {
        for (name, bytes) in [
            ("Root 1", ROOT_1),
            ("Intermediate B 1", INTERMEDIATE_B_1),
            ("FIDO Attestation B 1", FIDO_B_1),
        ] {
            assert!(
                matches!(
                    YubicoAttestationVerifier::from_anchors(&[der(bytes)]),
                    Err(AttestationError::RootRefused { .. })
                ),
                "{name} accepted as an anchor"
            );
        }
    }

    #[test]
    fn the_yubico_verifier_refuses_mock_bundles() {
        let bundle = AttestationBundle::MockPiv(MockPivAttestation {
            slot_certificate: DerCertificate::from_der(vec![0x30, 0x00]),
            device_chain: Vec::new(),
        });
        assert!(matches!(
            YubicoAttestationVerifier::evidence(&bundle),
            Err(AttestationError::WrongFormat { format: "mock_piv" })
        ));
    }
}
