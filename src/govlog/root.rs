//! Root certificates: what makes a governance signing key the chain's.
//!
//! The online key signs entries; the root keys sign nothing but
//! [`KeyCertStatement`]s, offline, on hardware. A key holds the chain iff
//! a [`KeyCertificate`] says so at that position:
//!
//! ```text
//! signed_bytes = "agora-governance-root-v1\n" || canonical_json(statement)
//! signature    = Ed25519( root_key, signed_bytes )      -- no timestamp
//! ```
//!
//! The prefix is domain separation: nothing else a root key might ever
//! sign begins with it.

use super::{
    PublicKeyHex, Sha256Hex, SignatureHex, TrustedHead, canonical_json,
};
use crate::crypto::Signature;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// What every root signature begins with
pub const ROOT_DOMAIN: &[u8] = b"agora-governance-root-v1\n";

/// The [`KeyCertStatement`] version this module produces and verifies
pub const KEY_CERT_VERSION: u32 = 1;

/// The governance root keys this build of agentkit trusts.
///
/// Generated on-device and attested; see `governance/root` in the agora
/// repository. Changing the set is a release, not a chain entry.
pub const ROOT_KEYS: &[&str] = &[
    // YubiKey 5 Nano 12585769
    "d29ed152161d23d75cec48ade38859db07f48f3dc15a337179a8f20b13f12cd5",
    // YubiKey 5Ci 17129552
    "200e8efe32391d7f4a1d763c4de0739acfe2e63e69d8be7e19b29d3f5075fb53",
];

/// How many of [`ROOT_KEYS`] must sign a [`KeyCertificate`]
pub const ROOT_THRESHOLD: usize = 1;

/// What a certified key is being certified as
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(rename_all = "snake_case")]
pub enum CertPurpose {
    // The key the chain started under, certified after the fact.
    Genesis,
    // The incoming key of a `RotationReason::Routine`.
    Routine,
    // The incoming key of a `RotationReason::Compromise`.
    Compromise,
}

/// What a root key signs: `key` holds the chain from `from_seq`.
///
/// Every field is always serialized, `null` when absent, and unknown
/// fields are refused: the bytes a verifier rebuilds are exactly the bytes
/// the Steward read before touching the device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(deny_unknown_fields)]
pub struct KeyCertStatement {
    /// Always [`KEY_CERT_VERSION`]
    pub agora_governance_key_cert: u32,
    /// The online signing key being certified
    pub key: PublicKeyHex,
    pub purpose: CertPurpose,
    /// The first `chain_seq` `key` signs
    pub from_seq: u64,
    /// The rotation entry's own `prev_hash`, so a certificate is good at
    /// one position of one chain. `null` for [`CertPurpose::Genesis`].
    pub prev_hash: Option<Sha256Hex>,
    /// [`CertPurpose::Compromise`] only: the last entry trusted under the
    /// outgoing key. The root says where the window opens, not the online
    /// key that may be the thief's.
    pub last_trusted: Option<TrustedHead>,
}

impl KeyCertStatement {
    /// `key` is the key the chain started under
    pub fn genesis(key: PublicKeyHex) -> Self {
        Self {
            agora_governance_key_cert: KEY_CERT_VERSION,
            key,
            purpose: CertPurpose::Genesis,
            from_seq: 1,
            prev_hash: None,
            last_trusted: None,
        }
    }

    /// `key` takes over after the routine rotation appended at `seq`,
    /// whose `prev_hash` is `prev_hash`
    pub fn routine(
        key: PublicKeyHex,
        seq: u64,
        prev_hash: Option<Sha256Hex>,
    ) -> Self {
        Self {
            agora_governance_key_cert: KEY_CERT_VERSION,
            key,
            purpose: CertPurpose::Routine,
            from_seq: seq + 1,
            prev_hash,
            last_trusted: None,
        }
    }

    /// `key` signs the compromise declaration appended at `seq` and
    /// everything after it
    pub fn compromise(
        key: PublicKeyHex,
        seq: u64,
        prev_hash: Option<Sha256Hex>,
        last_trusted: TrustedHead,
    ) -> Self {
        Self {
            agora_governance_key_cert: KEY_CERT_VERSION,
            key,
            purpose: CertPurpose::Compromise,
            from_seq: seq,
            prev_hash,
            last_trusted: Some(last_trusted),
        }
    }

    /// The exact bytes a root key signs
    pub fn signed_bytes(&self) -> Vec<u8> {
        let value = serde_json::to_value(self)
            .expect("a KeyCertStatement always serializes");
        let mut out = ROOT_DOMAIN.to_vec();
        out.extend(canonical_json(&value));
        out
    }
}

/// One root key's signature over [`KeyCertStatement::signed_bytes`]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(deny_unknown_fields)]
pub struct RootSignature {
    pub root_key: PublicKeyHex,
    pub signature: SignatureHex,
}

/// A [`KeyCertStatement`] and the root signatures over it
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(deny_unknown_fields)]
pub struct KeyCertificate {
    pub statement: KeyCertStatement,
    pub signatures: Vec<RootSignature>,
}

/// A certificate does not certify what it was presented for
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CertificateError {
    #[error("agora_governance_key_cert is {0}, not {KEY_CERT_VERSION}")]
    UnsupportedVersion(u32),
    #[error(
        "the certificate is for a different key, purpose or chain position"
    )]
    WrongStatement,
    #[error(
        "{valid} valid root signature(s) where {needed} are needed; \
         unknown and repeated signers count for nothing"
    )]
    BelowThreshold { valid: usize, needed: usize },
}

impl KeyCertificate {
    /// `statement`, not yet signed by anyone
    pub fn unsigned(statement: KeyCertStatement) -> Self {
        Self {
            statement,
            signatures: Vec::new(),
        }
    }

    /// This certificate, plus `signature`
    pub fn with(mut self, signature: RootSignature) -> Self {
        self.signatures.push(signature);
        self
    }

    /// At least [`threshold`](RootSet::threshold) distinct keys of `roots`
    /// signed this statement.
    ///
    /// A signature by a key outside `roots`, a second one by the same key,
    /// or one that does not verify is ignored rather than fatal: a future
    /// root set may overlap this one.
    pub fn verify(&self, roots: &RootSet) -> Result<(), CertificateError> {
        let version = self.statement.agora_governance_key_cert;
        if version != KEY_CERT_VERSION {
            return Err(CertificateError::UnsupportedVersion(version));
        }
        let message = self.statement.signed_bytes();
        let mut signers = HashSet::new();
        for s in &self.signatures {
            if !roots.contains(&s.root_key) {
                continue;
            }
            let Ok(key) = s.root_key.to_verifying_key() else {
                continue;
            };
            if key
                .verify_strict(&message, &Signature::from(&s.signature))
                .is_ok()
            {
                signers.insert(s.root_key);
            }
        }
        if signers.len() >= roots.threshold() {
            Ok(())
        } else {
            Err(CertificateError::BelowThreshold {
                valid: signers.len(),
                needed: roots.threshold(),
            })
        }
    }

    /// [`verify`](Self::verify), and the statement is exactly `expected` —
    /// which the verifier derives from the chain, never from the
    /// certificate
    pub fn verify_for(
        &self,
        expected: &KeyCertStatement,
        roots: &RootSet,
    ) -> Result<(), CertificateError> {
        if self.statement != *expected {
            return Err(CertificateError::WrongStatement);
        }
        self.verify(roots)
    }
}

/// The root keys a verifier trusts, and how many must agree
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootSet {
    keys: HashSet<PublicKeyHex>,
    threshold: usize,
}

impl RootSet {
    /// [`ROOT_KEYS`] at [`ROOT_THRESHOLD`]
    pub fn published() -> Self {
        Self::new(
            ROOT_KEYS.iter().map(|k| {
                k.parse().expect("ROOT_KEYS are valid 32-byte hex keys")
            }),
            ROOT_THRESHOLD,
        )
    }

    /// A `threshold` of zero would certify anything, so it is raised to one
    pub fn new(
        keys: impl IntoIterator<Item = PublicKeyHex>,
        threshold: usize,
    ) -> Self {
        Self {
            keys: keys.into_iter().collect(),
            threshold: threshold.max(1),
        }
    }

    pub fn contains(&self, key: &PublicKeyHex) -> bool {
        self.keys.contains(key)
    }

    pub fn threshold(&self) -> usize {
        self.threshold
    }

    /// The root keys, in no particular order
    pub fn keys(&self) -> impl Iterator<Item = &PublicKeyHex> {
        self.keys.iter()
    }
}
