//! PIV attestation as X.509: reading a slot's attestation certificate and
//! checking its chain to a trusted root.
//!
//! The one parse path for every PIV attestation:
//! [`YubicoAttestationVerifier`] calls it with Yubico's published roots,
//! and the mock verifier (feature `mock-hardware-key`) with a mock root CA.
//! The slot certificate's facts are read from Yubico's extensions
//! (`1.3.6.1.4.1.41482.3.*`), which the mock writes too.
//!
//! **Chain shape.** Yubico's hierarchies are root → `f9` → slot (older
//! devices) and root → intermediate → `f9` → slot (newer ones), with the
//! root supplied by the verifier and the rest by the device. So:
//! - the device chain holds 1 or 2 certificates;
//! - `f9` (index 1) is the only issuer allowed not to be a CA: Yubico's
//!   `f9` certificates aren't marked as one. Every certificate above it,
//!   and every root, must have `basicConstraints cA=TRUE`, and
//!   `keyCertSign` if it has a `keyUsage`;
//! - only the slot certificate may carry a policy;
//! - unknown critical extensions, and any extension twice, are refused;
//! - each certificate's inner and outer signature algorithms must agree,
//!   with the parameters the algorithm prescribes.
//!
//! Without the shape rules, any attested slot key could sign a "slot
//! certificate" naming itself as issuer, for any key with any serial and
//! policy, and every link would verify.
//!
//! **Not checked:** validity dates (Yubico's slot certificates run to the
//! year 9999), revocation.
//!
//! **Algorithms:** issuers may sign with ECDSA P-256 / SHA-256, ECDSA
//! P-384 / SHA-384 (for Yubico's newer hierarchy) or RSA PKCS#1 v1.5 /
//! SHA-256 (Yubico's older roots and `f9` certificates are RSA). The hash
//! must match the curve. Any other algorithm is refused as
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

/// The mock root CA's subject. A real verifier refuses a root with it.
pub const MOCK_ROOT_SUBJECT: &str =
    "CN=Agora MOCK PIV Root CA - NOT A REAL DEVICE";
/// A mock device's `f9`-style attestation certificate subject.
pub const MOCK_DEVICE_SUBJECT: &str =
    "CN=Agora MOCK PIV Attestation - NOT A REAL DEVICE";

/// The most certificates a device chain holds: `f9` and one intermediate.
const MAX_DEVICE_CHAIN: usize = 2;

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

/// Root certificates a verifier trusts, parsed once. Every one must be a
/// CA.
#[derive(Clone)]
pub struct TrustedRoots(Vec<Certificate>);

impl TrustedRoots {
    /// Parse roots from DER.
    pub fn from_der(
        roots: &[DerCertificate],
    ) -> Result<Self, AttestationError> {
        let roots = roots
            .iter()
            .enumerate()
            .map(|(index, der)| parse(index, der))
            .collect::<Result<Vec<_>, _>>()?;
        for (index, root) in roots.iter().enumerate() {
            check_extensions(index, root)?;
            if !is_ca(index, root)? {
                return Err(AttestationError::RootRefused {
                    reason: "a root is not a CA",
                });
            }
        }
        Ok(Self(roots))
    }

    /// Whether any root has the mock root's subject.
    fn has_mock_root(&self) -> bool {
        let mock = Name::from_str(MOCK_ROOT_SUBJECT).expect("a valid name");
        self.0
            .iter()
            .any(|root| *root.tbs_certificate().subject() == mock)
    }
}

impl std::fmt::Debug for TrustedRoots {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list()
            .entries(
                self.0
                    .iter()
                    .map(|c| c.tbs_certificate().subject().to_string()),
            )
            .finish()
    }
}

/// Checks Yubico PIV attestations against Yubico's roots.
///
/// The caller supplies the roots: Yubico publishes them at
/// <https://developers.yubico.com/PKI/>, and which to trust is the
/// server's configuration, not this crate's. A mock root is refused.
#[derive(Debug, Clone)]
pub struct YubicoAttestationVerifier {
    roots: TrustedRoots,
}

impl YubicoAttestationVerifier {
    /// Trust these Yubico root certificates.
    pub fn from_roots(
        roots: &[DerCertificate],
    ) -> Result<Self, AttestationError> {
        let roots = TrustedRoots::from_der(roots)?;
        if roots.has_mock_root() {
            return Err(AttestationError::RootRefused {
                reason: "the mock root CA is not a Yubico root",
            });
        }
        Ok(Self { roots })
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

/// Check `slot_certificate`'s chain through `device_chain` (nearest first)
/// to one of `roots`, then read its facts. See the module docs for what is
/// checked.
pub fn verify_attestation(
    slot_certificate: &DerCertificate,
    device_chain: &[DerCertificate],
    roots: &TrustedRoots,
) -> Result<SlotFacts, AttestationError> {
    if device_chain.is_empty() || device_chain.len() > MAX_DEVICE_CHAIN {
        return Err(AttestationError::ChainLength {
            len: device_chain.len(),
        });
    }
    let chain = std::iter::once(slot_certificate)
        .chain(device_chain)
        .enumerate()
        .map(|(index, der)| parse(index, der))
        .collect::<Result<Vec<_>, _>>()?;

    for (index, cert) in chain.iter().enumerate() {
        check_extensions(index, cert)?;
        if index >= 1 && has_extension(cert, OID_YUBICO_POLICY) {
            return Err(AttestationError::PolicyAboveLeaf { index });
        }
        // `f9` (index 1) is the one issuer allowed not to be a CA.
        if index >= 2 && !is_ca(index, cert)? {
            return Err(AttestationError::NotACa { index });
        }
    }

    for (index, pair) in chain.windows(2).enumerate() {
        let (cert, issuer) = (&pair[0], &pair[1]);
        if cert.tbs_certificate().issuer() != issuer.tbs_certificate().subject()
        {
            return Err(AttestationError::IssuerMismatch { index });
        }
        check_signature(
            index,
            cert,
            issuer.tbs_certificate().subject_public_key_info(),
        )?;
    }

    // Any root of the right name whose key verifies the top certificate.
    let top_index = chain.len() - 1;
    let top = &chain[top_index];
    let issued_by_a_root = roots
        .0
        .iter()
        .filter(|root| {
            root.tbs_certificate().subject() == top.tbs_certificate().issuer()
        })
        .any(|root| {
            check_signature(
                top_index,
                top,
                root.tbs_certificate().subject_public_key_info(),
            )
            .is_ok()
        });
    if !issued_by_a_root {
        return Err(AttestationError::UntrustedRoot);
    }

    read_slot_facts(&chain[0])
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

/// `basicConstraints cA=TRUE`, and `keyCertSign` if there is a `keyUsage`.
fn is_ca(index: usize, cert: &Certificate) -> Result<bool, AttestationError> {
    let tbs = cert.tbs_certificate();
    let ca = tbs
        .get_extension::<BasicConstraints>()
        .map_err(|e| malformed(index, e))?
        .is_some_and(|(_, constraints)| constraints.ca);
    let may_sign_certificates = tbs
        .get_extension::<KeyUsage>()
        .map_err(|e| malformed(index, e))?
        .is_none_or(|(_, usage)| usage.key_cert_sign());
    Ok(ca && may_sign_certificates)
}

/// Absent, or an explicit NULL.
fn absent_or_null(parameters: Option<&Any>) -> bool {
    parameters.is_none_or(|p| p.decode_as::<Null>().is_ok())
}

/// Check `cert`'s signature under `issuer_key`.
fn check_signature(
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
        let key = issuer_key
            .subject_public_key
            .as_bytes()
            .and_then(|der| rsa::RsaPublicKey::from_pkcs1_der(der).ok())
            .ok_or_else(|| unsupported(index, issuer_key.algorithm.oid))?;
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
    use crate::operator_signer::MockPivAttestation;

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
