//! Governance log attestation: the envelope the server signs at insert and
//! any client can verify.
//!
//! Every entry commits to its own fields and to the hash of the entry before
//! it, so the log is a chain: change or remove any entry and every later
//! link stops verifying. The envelope (version 1) is
//!
//! ```text
//! data_hash  = SHA-256( canonical_json(data) )
//! preimage   = {"agora_governance_log":1,"id":…,"entry_type":…,
//!               "created_at":<unix micros>,"prev_hash":<hex|null>,
//!               "data_hash":<hex>}
//! entry_hash = SHA-256( preimage )
//! signature  = crypto::sign( key, entry_hash, signed_at unix seconds )
//! ```
//!
//! What is *not* in the envelope, on purpose: `tags` (an index the Clerk may
//! revise), `chain_seq` (an index; the `prev_hash` links prove order), and
//! anything derived such as the precedent summary. `signed_at` is bound by
//! the signature rather than the hash, so a retroactive attestation — an
//! entry signed long after it was recorded — is visible as such and cannot
//! be quietly back-dated. See [`is_retroactive`].
//!
//! History is never rewritten. Two entry types amend it instead, and both
//! are ordinary signed links whose `data` the verifier reads:
//!
//! - [`Amendment`] (`AMD-`) names an earlier entry and says what changed
//!   about its force ([`Standing`]) or its content ([`Redaction`]). A
//!   redaction replaces values in the target's `data` in place; the
//!   original `entry_hash` stays on the row so later links still verify,
//!   and the amendment's `resulting_data_hash` is what the redacted data
//!   must now hash to. [`EntryVerdict::content_matches`] is the check.
//! - [`KeyRotation`] (`KEY-`) moves the chain to a new signing key. A
//!   routine rotation is signed by the old key; a compromise declaration
//!   is signed by the new one and is authentic only if that key is in the
//!   verifier's out-of-band [`KeyAnchor`], which is what makes a key thief
//!   visible rather than authoritative. See [`verify_chain`].
//!
//! The envelope itself is unchanged by any of this: `ENVELOPE_VERSION` is
//! still 1 and what it does and does not cover is exactly as above.

use crate::crypto::{self, Signature, SigningKey, VerifyingKey};
use crate::enums::GovernanceLogEntryType;
use crate::ids::{GovernanceLogId, GovernanceLogPrefix};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};

pub use crate::enums::{AmendmentKind, KeyStatus, Standing};

/// The shared test vectors in `vectors/govlog`; see [`vectors`]
#[cfg(test)]
mod vectors;

/// The envelope version this module produces and verifies
pub const ENVELOPE_VERSION: u32 = 1;

/// An attestation signed more than this long after its entry was recorded
/// is [retroactive](is_retroactive)
pub const RETROACTIVE_AFTER: chrono::Duration = chrono::Duration::seconds(60);

// ---------------------------------------------------------------------------
// Fixed-size hex newtypes
// ---------------------------------------------------------------------------

/// A hex string of the wrong length or alphabet for the type it was parsed
/// into
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{type_name}: expected {expected} bytes of hex, got {got:?}")]
pub struct HexLengthError {
    pub type_name: &'static str,
    pub expected: usize,
    pub got: String,
}

macro_rules! hex_bytes {
    ($(#[$meta:meta])* $name:ident, $len:expr) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash)]
        #[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
        #[cfg_attr(feature = "sqlx", sqlx(transparent))]
        pub struct $name([u8; $len]);

        impl $name {
            /// The raw bytes
            pub fn as_bytes(&self) -> &[u8; $len] {
                &self.0
            }

            /// Lowercase hex, as on the wire
            pub fn to_hex(&self) -> String {
                hex::encode(self.0)
            }
        }

        impl From<[u8; $len]> for $name {
            fn from(bytes: [u8; $len]) -> Self {
                Self(bytes)
            }
        }

        impl TryFrom<&[u8]> for $name {
            type Error = HexLengthError;

            fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
                <[u8; $len]>::try_from(bytes).map(Self).map_err(|_| {
                    HexLengthError {
                        type_name: stringify!($name),
                        expected: $len,
                        got: hex::encode(bytes),
                    }
                })
            }
        }

        impl TryFrom<Vec<u8>> for $name {
            type Error = HexLengthError;

            fn try_from(bytes: Vec<u8>) -> Result<Self, Self::Error> {
                Self::try_from(bytes.as_slice())
            }
        }

        impl std::str::FromStr for $name {
            type Err = HexLengthError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                let bytes = hex::decode(s.trim()).map_err(|_| HexLengthError {
                    type_name: stringify!($name),
                    expected: $len,
                    got: s.to_string(),
                })?;
                Self::try_from(bytes.as_slice())
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.to_hex())
            }
        }

        impl std::fmt::Debug for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}({})", stringify!($name), self.to_hex())
            }
        }

        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(
                &self,
                s: S,
            ) -> Result<S::Ok, S::Error> {
                s.serialize_str(&self.to_hex())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(
                d: D,
            ) -> Result<Self, D::Error> {
                let s = String::deserialize(d)?;
                s.parse().map_err(serde::de::Error::custom)
            }
        }

        // Hand-written for the same reason as every id newtype: a derived
        // schema becomes a `$ref` into `$defs`, which the Claude.ai MCP
        // connector mangles (see CLAUDE.md in the agora repo).
        #[cfg(feature = "schemars")]
        impl schemars::JsonSchema for $name {
            fn inline_schema() -> bool {
                true
            }

            fn schema_name() -> std::borrow::Cow<'static, str> {
                std::borrow::Cow::Borrowed(stringify!($name))
            }

            fn schema_id() -> std::borrow::Cow<'static, str> {
                std::borrow::Cow::Borrowed(concat!(
                    module_path!(),
                    "::",
                    stringify!($name)
                ))
            }

            fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
                schemars::json_schema!({
                    "type": "string",
                    "pattern": format!("^[0-9a-f]{{{}}}$", $len * 2),
                    "description": format!("{} bytes, lowercase hex", $len),
                })
            }
        }
    };
}

hex_bytes!(
    /// A SHA-256 digest, hex on the wire
    Sha256Hex,
    32
);

hex_bytes!(
    /// An Ed25519 signature, hex on the wire
    SignatureHex,
    64
);

hex_bytes!(
    /// An Ed25519 public key, hex on the wire
    PublicKeyHex,
    32
);

impl From<Signature> for SignatureHex {
    fn from(sig: Signature) -> Self {
        Self(sig.to_bytes())
    }
}

impl From<&SignatureHex> for Signature {
    fn from(sig: &SignatureHex) -> Self {
        Signature::from_bytes(&sig.0)
    }
}

impl From<&VerifyingKey> for PublicKeyHex {
    fn from(key: &VerifyingKey) -> Self {
        Self(key.to_bytes())
    }
}

impl PublicKeyHex {
    /// The key, if the bytes are a valid curve point
    pub fn to_verifying_key(
        &self,
    ) -> Result<VerifyingKey, ed25519_dalek::SignatureError> {
        VerifyingKey::from_bytes(&self.0)
    }
}

// ---------------------------------------------------------------------------
// Canonical JSON and hashing
// ---------------------------------------------------------------------------

/// `value` as compact JSON with object keys sorted bytewise at every level.
///
/// `serde_json::to_vec` on a [`serde_json::Value`] is *not* canonical:
/// with the `preserve_order` feature (on in every Agora workspace, off in
/// this crate's own tests) objects serialize in insertion order, so the
/// same value hashes differently depending on who built it. Strings and
/// numbers use serde_json's own formatting, which is deterministic for a
/// given value.
pub fn canonical_json(value: &serde_json::Value) -> Vec<u8> {
    let mut out = Vec::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &serde_json::Value, out: &mut Vec<u8>) {
    use serde_json::Value;
    match value {
        Value::Null => out.extend_from_slice(b"null"),
        Value::Bool(b) => {
            out.extend_from_slice(if *b { b"true" } else { b"false" })
        }
        Value::Number(n) => serde_json::to_writer(&mut *out, n)
            .expect("a number always serializes"),
        Value::String(s) => serde_json::to_writer(&mut *out, s)
            .expect("a string always serializes"),
        Value::Array(items) => {
            out.push(b'[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_canonical(item, out);
            }
            out.push(b']');
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_unstable();
            out.push(b'{');
            for (i, key) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                serde_json::to_writer(&mut *out, key)
                    .expect("a string always serializes");
                out.push(b':');
                write_canonical(&map[key], out);
            }
            out.push(b'}');
        }
    }
}

/// SHA-256 over [`canonical_json`]
pub fn data_hash(data: &serde_json::Value) -> Sha256Hex {
    Sha256Hex(Sha256::digest(canonical_json(data)).into())
}

/// The fields an entry's hash commits to
///
/// A struct rather than a [`serde_json::Value`] so the preimage serializes
/// in declaration order whatever `preserve_order` says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Envelope {
    /// Always [`ENVELOPE_VERSION`]
    pub agora_governance_log: u32,
    pub id: GovernanceLogId,
    pub entry_type: GovernanceLogEntryType,
    /// Unix microseconds — the precision Postgres stores
    pub created_at: i64,
    pub prev_hash: Option<Sha256Hex>,
    pub data_hash: Sha256Hex,
}

impl Envelope {
    /// The version-1 envelope for these fields
    pub fn new(
        id: GovernanceLogId,
        entry_type: GovernanceLogEntryType,
        created_at: DateTime<Utc>,
        prev_hash: Option<Sha256Hex>,
        data_hash: Sha256Hex,
    ) -> Self {
        Self {
            agora_governance_log: ENVELOPE_VERSION,
            id,
            entry_type,
            created_at: created_at.timestamp_micros(),
            prev_hash,
            data_hash,
        }
    }

    /// `created_at` as a timestamp again
    pub fn created_at(&self) -> DateTime<Utc> {
        DateTime::from_timestamp_micros(self.created_at)
            .expect("an Envelope only ever holds an in-range timestamp")
    }

    /// The bytes that are hashed
    pub fn preimage(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("an Envelope always serializes")
    }

    /// SHA-256 over [`preimage`](Self::preimage)
    pub fn entry_hash(&self) -> Sha256Hex {
        Sha256Hex(Sha256::digest(self.preimage()).into())
    }
}

/// `t` with anything below a microsecond dropped, so the value hashed is the
/// value Postgres will store
pub fn truncate_to_micros(t: DateTime<Utc>) -> DateTime<Utc> {
    DateTime::from_timestamp_micros(t.timestamp_micros())
        .expect("a timestamp that came from a DateTime is in range")
}

/// `t` with anything below a second dropped, so `signed_at` round-trips to
/// the integer the signature covers
pub fn truncate_to_seconds(t: DateTime<Utc>) -> DateTime<Utc> {
    DateTime::from_timestamp(t.timestamp(), 0)
        .expect("a timestamp that came from a DateTime is in range")
}

/// `true` when the attestation was signed more than [`RETROACTIVE_AFTER`]
/// after the entry was recorded — history signed after the fact, which
/// proves the key holder vouches for it now, not that it was signed then
pub fn is_retroactive(
    created_at: DateTime<Utc>,
    signed_at: DateTime<Utc>,
) -> bool {
    signed_at - created_at > RETROACTIVE_AFTER
}

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

/// What the server attests about one governance log entry
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct GovernanceAttestation {
    /// Envelope version; see the module docs for what `1` commits to
    pub envelope_version: u32,
    /// Position in the chain, from 1. An index, not part of the envelope:
    /// the `prev_hash` links are what prove order.
    pub chain_seq: u64,
    /// `entry_hash` of the previous entry; `null` only for the first
    pub prev_hash: Option<Sha256Hex>,
    /// SHA-256 of the entry's canonical `data`
    pub data_hash: Sha256Hex,
    /// SHA-256 of the envelope; what the signature covers
    pub entry_hash: Sha256Hex,
    /// Ed25519 over `entry_hash` and `signed_at`, by the platform's
    /// governance signing key
    pub signature: SignatureHex,
    /// When the signature was made. Distinct from `created_at`: see
    /// `retroactive`.
    pub signed_at: DateTime<Utc>,
    /// `true` when signed well after the entry was recorded — the entries
    /// that predate signing were attested this way, which proves the
    /// Steward vouches for them, not that they were signed at the time
    pub retroactive: bool,
}

/// One link of the chain as `GET /api/governance/log/chain` returns it —
/// everything needed to verify linkage and signatures, plus `data` for the
/// entries a verifier has to read
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct GovernanceChainLink {
    pub id: GovernanceLogId,
    pub entry_type: GovernanceLogEntryType,
    pub created_at: DateTime<Utc>,
    pub attestation: GovernanceAttestation,
    /// Present for `amendment` and `key_rotation` entries, whose content
    /// is what the chain means and is small by construction. A Council
    /// transcript is neither, and is read one entry at a time instead.
    /// [`verify_chain`] hashes this raw value against `data_hash` before
    /// reading it, so unknown future fields neither break an older
    /// verifier nor escape the signature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// The platform's governance signing key, as `GET
/// /api/governance/signing-key` publishes it
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct GovernanceSigningKey {
    /// Always `"ed25519"`
    pub algorithm: String,
    pub public_key: PublicKeyHex,
    /// The envelope version entries are currently signed under
    pub envelope_version: u32,
}

impl GovernanceSigningKey {
    /// The published form of `key`
    pub fn new(key: &VerifyingKey) -> Self {
        Self {
            algorithm: "ed25519".to_string(),
            public_key: key.into(),
            envelope_version: ENVELOPE_VERSION,
        }
    }
}

// ---------------------------------------------------------------------------
// Amendments
// ---------------------------------------------------------------------------

/// The [`Amendment`] payload version this module produces and verifies
pub const AMENDMENT_VERSION: u32 = 1;

/// An amendment is malformed
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AmendmentError {
    #[error("agora_governance_amendment is {0}, not {AMENDMENT_VERSION}")]
    UnsupportedVersion(u32),
    #[error("kind `redaction` requires a `redaction`")]
    MissingRedaction,
    #[error("`redaction` is only valid on kind `redaction`")]
    UnexpectedRedaction,
    #[error("amendment target {0} is not an entry of this chain")]
    UnknownTarget(GovernanceLogId),
    #[error("amendment target {0} is not an earlier entry")]
    ForwardReference(GovernanceLogId),
    #[error("target_entry_hash is not {0}'s entry_hash")]
    WrongTargetHash(GovernanceLogId),
}

/// The `data` of an `amendment` entry: what an earlier entry now means.
///
/// The target is never edited — except its `data` under a [`Redaction`] —
/// so the amendment is the whole record of the change, and both are
/// signed links of the same chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct Amendment {
    /// Always [`AMENDMENT_VERSION`]
    pub agora_governance_amendment: u32,
    pub target: GovernanceLogId,
    /// The target's `entry_hash` — binds this to one exact entry
    pub target_entry_hash: Sha256Hex,
    pub kind: AmendmentKind,
    /// The governance entry that authorizes this, when one does
    /// (e.g. `GOV-2026-0005`). `None` for a lawful-deletion redaction.
    #[serde(default)]
    pub authority: Option<GovernanceLogId>,
    /// Section or legal basis, human-readable: `"§1 (Red Team Cases
    /// Recharacterized)"`, `"GDPR Art. 17(1)(a)"`. Never personal data.
    pub basis: String,
    /// The label readers and prompts show next to the target
    pub note: String,
    /// Why, at length — `note` is the label, this is the reasoning, and it
    /// is part of the signed record. Never personal data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
    /// Present iff `kind` is [`AmendmentKind::Redaction`]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redaction: Option<Redaction>,
}

/// What a [`AmendmentKind::Redaction`] removed, and what is left
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct Redaction {
    /// RFC 6901 JSON pointers into the target's `data` whose values were
    /// replaced. Paths only: never the removed content, never the subject.
    pub fields: Vec<String>,
    /// What the target's `data` hashes to after redaction, so the redacted
    /// content is itself verifiable and cannot be altered again silently
    pub resulting_data_hash: Sha256Hex,
}

impl Amendment {
    /// An amendment of `kind` against `target`.
    ///
    /// Redactions go through [`redaction`](Self::redaction) instead, which
    /// is the only way to get a [`Redaction`] whose `resulting_data_hash`
    /// is the hash of data that actually exists.
    pub fn new(
        target: GovernanceLogId,
        target_entry_hash: Sha256Hex,
        kind: AmendmentKind,
        basis: impl Into<String>,
        note: impl Into<String>,
    ) -> Result<Self, AmendmentError> {
        if kind == AmendmentKind::Redaction {
            return Err(AmendmentError::MissingRedaction);
        }
        Ok(Self {
            agora_governance_amendment: AMENDMENT_VERSION,
            target,
            target_entry_hash,
            kind,
            authority: None,
            basis: basis.into(),
            note: note.into(),
            rationale: None,
            redaction: None,
        })
    }

    /// The amendment with the governance entry that authorizes it
    pub fn with_authority(mut self, authority: GovernanceLogId) -> Self {
        self.authority = Some(authority);
        self
    }

    /// The amendment with its [`rationale`](Self::rationale)
    pub fn with_rationale(mut self, rationale: impl Into<String>) -> Self {
        self.rationale = Some(rationale.into());
        self
    }

    /// A redaction of `fields` from the target's `data`, with the redacted
    /// data it commits to.
    ///
    /// `amendment_id` is the id this amendment will be appended under: the
    /// marker left behind names it, so the redaction says who ordered it.
    /// Append both together or neither — the returned `data` is what the
    /// target's row must hold for [`verify_chain`] to accept it.
    pub fn redaction(
        amendment_id: &GovernanceLogId,
        target: GovernanceLogId,
        target_entry_hash: Sha256Hex,
        basis: impl Into<String>,
        note: impl Into<String>,
        fields: Vec<String>,
        data: &serde_json::Value,
    ) -> Result<(Self, serde_json::Value), RedactError> {
        let redacted = redact_data(data, &fields, amendment_id)?;
        Ok((
            Self {
                agora_governance_amendment: AMENDMENT_VERSION,
                target,
                target_entry_hash,
                kind: AmendmentKind::Redaction,
                authority: None,
                basis: basis.into(),
                note: note.into(),
                rationale: None,
                redaction: Some(Redaction {
                    fields,
                    resulting_data_hash: data_hash(&redacted),
                }),
            },
            redacted,
        ))
    }

    /// Version and the redaction-shape invariant — everything checkable
    /// without the rest of the chain
    pub fn validate(&self) -> Result<(), AmendmentError> {
        if self.agora_governance_amendment != AMENDMENT_VERSION {
            return Err(AmendmentError::UnsupportedVersion(
                self.agora_governance_amendment,
            ));
        }
        match (self.kind, &self.redaction) {
            (AmendmentKind::Redaction, None) => {
                Err(AmendmentError::MissingRedaction)
            }
            (k, Some(_)) if k != AmendmentKind::Redaction => {
                Err(AmendmentError::UnexpectedRedaction)
            }
            _ => Ok(()),
        }
    }
}

/// What an amendment does to the [`Standing`] of the entry it names, when
/// it changes it at all
pub fn kind_standing(kind: AmendmentKind) -> Option<Standing> {
    match kind {
        AmendmentKind::NonPrecedential => Some(Standing::NonPrecedential),
        AmendmentKind::Overruled => Some(Standing::Overruled),
        AmendmentKind::Superseded => Some(Standing::Superseded),
        AmendmentKind::Reinstated => Some(Standing::InForce),
        AmendmentKind::Correction
        | AmendmentKind::Redaction
        | AmendmentKind::Reattested => None,
    }
}

/// The [`Standing`] conferred by `kinds` — the amendments naming one entry,
/// in chain order. The last one that changes standing wins.
pub fn standing(kinds: impl IntoIterator<Item = AmendmentKind>) -> Standing {
    kinds
        .into_iter()
        .filter_map(kind_standing)
        .last()
        .unwrap_or_default()
}

/// A JSON pointer in a [`Redaction`] does not resolve
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RedactError {
    #[error("pointer {0:?} does not resolve in the entry's data")]
    Unresolved(String),
    #[error("the empty pointer would redact the whole entry")]
    WholeEntry,
}

/// The marker a redaction leaves in place of a value
pub fn redaction_marker(amendment_id: &GovernanceLogId) -> String {
    format!("[redacted by {amendment_id}]")
}

/// `data` with the value at each RFC 6901 pointer in `fields` replaced by
/// [`redaction_marker`].
///
/// Whole-value replacement only: a redaction tool that can write arbitrary
/// replacement prose is a rewrite tool. The server and every verifier share
/// this one definition, because what it returns is what the target's
/// `resulting_data_hash` covers.
pub fn redact_data(
    data: &serde_json::Value,
    fields: &[String],
    amendment_id: &GovernanceLogId,
) -> Result<serde_json::Value, RedactError> {
    let marker = serde_json::Value::String(redaction_marker(amendment_id));
    let mut out = data.clone();
    for pointer in fields {
        if pointer.is_empty() {
            return Err(RedactError::WholeEntry);
        }
        let slot = out
            .pointer_mut(pointer)
            .ok_or_else(|| RedactError::Unresolved(pointer.clone()))?;
        *slot = marker.clone();
    }
    Ok(out)
}

/// What a reader needs next to an amended entry
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct AmendmentNotice {
    pub id: GovernanceLogId,
    pub kind: AmendmentKind,
    #[serde(default)]
    pub authority: Option<GovernanceLogId>,
    pub basis: String,
    pub note: String,
    /// See [`Amendment::rationale`]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl AmendmentNotice {
    /// The notice for `amendment`, appended as `id` at `created_at`
    pub fn new(
        id: GovernanceLogId,
        created_at: DateTime<Utc>,
        amendment: &Amendment,
    ) -> Self {
        Self {
            id,
            kind: amendment.kind,
            authority: amendment.authority.clone(),
            basis: amendment.basis.clone(),
            note: amendment.note.clone(),
            rationale: amendment.rationale.clone(),
            created_at,
        }
    }
}

// ---------------------------------------------------------------------------
// Key rotation
// ---------------------------------------------------------------------------

/// The [`KeyRotation`] payload version this module produces and verifies
pub const KEY_ROTATION_VERSION: u32 = 1;

/// The governance signing keys this build of agentkit trusts, oldest first.
///
/// This is the second channel: the crate is published from credentials the
/// server does not hold, so a key that is served but not here is either a
/// thief or an out-of-date agentkit, and both are worth saying out loud.
/// Rotating means **add the new key here and release first, then append the
/// rotation entry** — never the other way round, or every up-to-date client
/// sees the chain move to a key it cannot anchor. CI checks the last
/// element against what the platform serves; see `just check-published-keys`.
pub const PUBLISHED_KEYS: &[&str] =
    &["ebb3091dd328f1463362c171121921b2fe14628e3fc4c145deaccefb85c0e78a"];

/// Why the key changed
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(rename_all = "snake_case")]
pub enum RotationReason {
    // Scheduled or voluntary; the old key signed the rotation itself.
    Routine,
    // The old key is in someone else's hands; the new key signed the
    // rotation and only the trust anchor can authenticate it.
    Compromise,
}

/// The last entry the compromised key is trusted for
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct TrustedHead {
    pub id: GovernanceLogId,
    pub chain_seq: u64,
    pub entry_hash: Sha256Hex,
}

/// A rotation is malformed, unauthenticated, or inconsistent with the chain
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RotationError {
    #[error("agora_governance_key_rotation is {0}, not {KEY_ROTATION_VERSION}")]
    UnsupportedVersion(u32),
    #[error("new_key is not a valid Ed25519 public key")]
    BadNewKey,
    #[error(
        "the proof of possession does not verify for this rotation at this position"
    )]
    BadProof,
    #[error("a compromise rotation must name last_trusted")]
    MissingLastTrusted,
    #[error("a routine rotation must not name last_trusted")]
    UnexpectedLastTrusted,
    #[error("old_key is not the key that was in force")]
    WrongOldKey,
    #[error("last_trusted does not name an earlier entry of this chain")]
    UnknownLastTrusted,
    #[error(
        "a compromise rotation to a key outside the trust anchor authenticates nothing"
    )]
    UnanchoredNewKey,
    #[error("new_key has already held this chain; a key is never brought back")]
    ReusedKey,
    #[error("last_trusted names an entry an earlier compromise repudiated")]
    RepudiatedLastTrusted,
}

/// The `data` of a `key_rotation` entry.
///
/// Build one with [`routine`](Self::routine) or
/// [`compromise`](Self::compromise): both compute the proof of possession,
/// which is the only thing standing between "the Steward moved the chain to
/// a new key" and "someone published a key they do not hold".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct KeyRotation {
    /// Always [`KEY_ROTATION_VERSION`]
    pub agora_governance_key_rotation: u32,
    pub reason: RotationReason,
    pub old_key: PublicKeyHex,
    pub new_key: PublicKeyHex,
    /// `crypto::sign(new_key, ` [`RotationStatement::hash`] `,
    /// proof_signed_at)` — the new key signing for itself, at one position
    /// in one chain
    pub proof: SignatureHex,
    /// Unix seconds; what the proof signature covers
    pub proof_signed_at: i64,
    /// Compromise only: the last entry trusted under `old_key`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_trusted: Option<TrustedHead>,
    pub note: String,
}

/// What the proof of possession signs
///
/// A struct rather than a [`serde_json::Value`] for the same reason as
/// [`Envelope`]: declaration-ordered serialization whatever `preserve_order`
/// says. `prev_hash` is the rotation entry's own, so a proof lifted out of
/// one chain position does not verify in another.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RotationStatement {
    /// Always [`KEY_ROTATION_VERSION`]
    pub agora_governance_key_rotation: u32,
    pub reason: RotationReason,
    pub old_key: PublicKeyHex,
    pub new_key: PublicKeyHex,
    pub prev_hash: Option<Sha256Hex>,
}

impl RotationStatement {
    /// The statement for a rotation at the position named by `prev_hash`
    pub fn new(
        reason: RotationReason,
        old_key: PublicKeyHex,
        new_key: PublicKeyHex,
        prev_hash: Option<Sha256Hex>,
    ) -> Self {
        Self {
            agora_governance_key_rotation: KEY_ROTATION_VERSION,
            reason,
            old_key,
            new_key,
            prev_hash,
        }
    }

    /// The bytes that are hashed
    pub fn preimage(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a RotationStatement always serializes")
    }

    /// SHA-256 over [`preimage`](Self::preimage) — what the proof covers
    pub fn hash(&self) -> Sha256Hex {
        Sha256Hex(Sha256::digest(self.preimage()).into())
    }
}

impl KeyRotation {
    /// A scheduled rotation from `old_key` to `new_signing_key`.
    ///
    /// The entry itself is signed by the **old** key; entries after it
    /// verify under the new one. `prev_hash` is the rotation entry's own.
    pub fn routine(
        old_key: PublicKeyHex,
        new_signing_key: &SigningKey,
        prev_hash: Option<Sha256Hex>,
        now: DateTime<Utc>,
        note: impl Into<String>,
    ) -> Self {
        Self::build(
            RotationReason::Routine,
            old_key,
            new_signing_key,
            None,
            prev_hash,
            now,
            note,
        )
    }

    /// A declaration that `old_key` is compromised, trusted only through
    /// `last_trusted`.
    ///
    /// The entry is signed by the **new** key — the old one proves nothing
    /// any more — so a verifier accepts it only from its [`KeyAnchor`].
    /// `last_trusted` must name an entry from before any earlier
    /// compromise window; a reattestation inside one restores the entry,
    /// not the ability to anchor trust there.
    pub fn compromise(
        old_key: PublicKeyHex,
        new_signing_key: &SigningKey,
        last_trusted: TrustedHead,
        prev_hash: Option<Sha256Hex>,
        now: DateTime<Utc>,
        note: impl Into<String>,
    ) -> Self {
        Self::build(
            RotationReason::Compromise,
            old_key,
            new_signing_key,
            Some(last_trusted),
            prev_hash,
            now,
            note,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn build(
        reason: RotationReason,
        old_key: PublicKeyHex,
        new_signing_key: &SigningKey,
        last_trusted: Option<TrustedHead>,
        prev_hash: Option<Sha256Hex>,
        now: DateTime<Utc>,
        note: impl Into<String>,
    ) -> Self {
        let new_key = PublicKeyHex::from(&new_signing_key.verifying_key());
        let proof_signed_at = truncate_to_seconds(now).timestamp();
        let statement =
            RotationStatement::new(reason, old_key, new_key, prev_hash);
        let proof = crypto::sign(
            new_signing_key,
            statement.hash().as_bytes(),
            proof_signed_at,
        );
        Self {
            agora_governance_key_rotation: KEY_ROTATION_VERSION,
            reason,
            old_key,
            new_key,
            proof: proof.into(),
            proof_signed_at,
            last_trusted,
            note: note.into(),
        }
    }

    /// The statement this rotation's proof covers, at `prev_hash`
    pub fn statement(&self, prev_hash: Option<Sha256Hex>) -> RotationStatement {
        RotationStatement::new(
            self.reason,
            self.old_key,
            self.new_key,
            prev_hash,
        )
    }

    /// Version, `last_trusted` shape, and the proof of possession at the
    /// position `prev_hash` names — everything checkable without the rest
    /// of the chain
    pub fn verify_proof(
        &self,
        prev_hash: Option<Sha256Hex>,
    ) -> Result<(), RotationError> {
        if self.agora_governance_key_rotation != KEY_ROTATION_VERSION {
            return Err(RotationError::UnsupportedVersion(
                self.agora_governance_key_rotation,
            ));
        }
        match (self.reason, &self.last_trusted) {
            (RotationReason::Compromise, None) => {
                return Err(RotationError::MissingLastTrusted);
            }
            (RotationReason::Routine, Some(_)) => {
                return Err(RotationError::UnexpectedLastTrusted);
            }
            _ => {}
        }
        let new_key = self
            .new_key
            .to_verifying_key()
            .map_err(|_| RotationError::BadNewKey)?;
        crypto::verify(
            &new_key,
            self.statement(prev_hash).hash().as_bytes(),
            self.proof_signed_at,
            &Signature::from(&self.proof),
        )
        .then_some(())
        .ok_or(RotationError::BadProof)
    }
}

/// The keys a verifier trusts out of band — the half of the trust model
/// the chain cannot supply, because a chain signed end to end by a thief
/// is internally perfect.
///
/// [`published`](Self::published) is this build's [`PUBLISHED_KEYS`];
/// [`pinned`](Self::pinned) is the key a client saw first and kept. Both
/// together is the recommendation: `KeyAnchor::published().with(pinned)`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeyAnchor {
    keys: HashSet<PublicKeyHex>,
}

impl KeyAnchor {
    /// The keys compiled into this build of agentkit
    pub fn published() -> Self {
        PUBLISHED_KEYS
            .iter()
            .map(|k| {
                k.parse()
                    .expect("PUBLISHED_KEYS are valid 32-byte hex keys")
            })
            .collect()
    }

    /// Just the one key, as a client that pinned what it saw first trusts it
    pub fn pinned(key: PublicKeyHex) -> Self {
        std::iter::once(key).collect()
    }

    /// This anchor, plus `key`
    pub fn with(mut self, key: PublicKeyHex) -> Self {
        self.keys.insert(key);
        self
    }

    pub fn contains(&self, key: &PublicKeyHex) -> bool {
        self.keys.contains(key)
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// The anchored keys, in no particular order
    pub fn keys(&self) -> impl Iterator<Item = &PublicKeyHex> {
        self.keys.iter()
    }
}

impl FromIterator<PublicKeyHex> for KeyAnchor {
    fn from_iter<I: IntoIterator<Item = PublicKeyHex>>(iter: I) -> Self {
        Self {
            keys: iter.into_iter().collect(),
        }
    }
}

/// One key's span of the chain, as [`verify_chain`] derives it and
/// `GET /api/governance/signing-keys` publishes it
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct GovernanceKeyRecord {
    pub public_key: PublicKeyHex,
    /// The first `chain_seq` this key signed
    pub from_seq: u64,
    /// The last `chain_seq` this key is trusted for; `null` while active.
    /// For a compromised key this is `last_trusted`, not the seq at which
    /// the compromise was declared.
    #[serde(default)]
    pub through_seq: Option<u64>,
    pub status: KeyStatus,
    /// The rotation entry that introduced this key; `null` for the genesis
    /// key, which predates the chain
    #[serde(default)]
    pub introduced_by: Option<GovernanceLogId>,
    /// The rotation entry that ended this key's span
    #[serde(default)]
    pub retired_by: Option<GovernanceLogId>,
}

/// The signing key history as `GET /api/governance/signing-keys` returns it
///
/// An object rather than a bare array: MCP structured content needs a
/// top-level object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct GovernanceSigningKeys {
    /// Oldest first
    pub keys: Vec<GovernanceKeyRecord>,
}

/// The verdict on one entry
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct EntryVerdict {
    pub id: GovernanceLogId,
    pub chain_seq: u64,
    /// The signature verifies over `entry_hash` and `signed_at` under the
    /// published key
    pub signature_valid: bool,
    /// `entry_hash` recomputes from the envelope fields, `prev_hash` is the
    /// previous entry's `entry_hash`, and `chain_seq` is contiguous
    pub link_valid: bool,
    /// The entry's current `data` hashes to the attested `data_hash` — or,
    /// when a redaction names the entry, to the redaction's
    /// `resulting_data_hash`. `null` when the verifier did not read `data`.
    /// `false` with a clean chain means the content was changed after
    /// attestation and no amendment says so.
    #[serde(default)]
    pub content_matches: Option<bool>,
    /// See [`GovernanceAttestation::retroactive`]
    pub retroactive: bool,
    /// `created_at` is earlier than the previous link's. Informational:
    /// chain order is what is attested, and a clock step does not break it.
    pub out_of_order: bool,
    /// Amendment entries that name this one
    #[serde(default)]
    pub amended_by: Vec<GovernanceLogId>,
    /// The key the signature was checked under — the one in force at this
    /// position, or for a compromise declaration the anchored new key
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signed_by: Option<PublicKeyHex>,
    /// A redaction amendment names this entry, so its `data` has lawfully
    /// changed since it was attested
    #[serde(default)]
    pub redacted: bool,
    /// What the entry's `data` must hash to now, when it has been redacted
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redacted_data_hash: Option<Sha256Hex>,
    /// Signed inside a compromise window and not reattested: the key
    /// holder of record disclaims it
    #[serde(default)]
    pub repudiated: bool,
    /// [`AmendmentKind::Reattested`] amendments vouching for this entry
    /// under a later, trusted key
    #[serde(default)]
    pub reattested_by: Vec<GovernanceLogId>,
    /// What failed, when something did
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
}

/// A verification of the whole chain
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct GovernanceVerification {
    /// The key in force for the next entry — the active end of `keys`
    pub public_key: PublicKeyHex,
    /// Every entry's signature and link verified under the key in force,
    /// no entry's content is known to differ from what was attested, and
    /// every amendment and rotation is well-formed. Repudiated entries do
    /// not clear this by themselves: repudiation is a declared state, not
    /// a defect, and `repudiated` is where to look for it.
    pub ok: bool,
    /// The last entry in the chain
    #[serde(default)]
    pub head: Option<GovernanceLogId>,
    /// In chain order
    pub entries: Vec<EntryVerdict>,
    /// The signing key history the chain itself declares, oldest first
    #[serde(default)]
    pub keys: Vec<GovernanceKeyRecord>,
    /// Keys the chain moved to that this verifier's [`KeyAnchor`] does not
    /// vouch for. Not a failure — an out-of-date agentkit looks exactly
    /// like this — but it is also what a key thief looks like, so a
    /// reference client says so loudly.
    #[serde(default)]
    pub unanchored_keys: Vec<PublicKeyHex>,
    /// Entries inside a compromise window that no reattestation restored
    #[serde(default)]
    pub repudiated: Vec<GovernanceLogId>,
}

impl GovernanceVerification {
    /// Recompute `ok` from the entries
    pub fn settle(mut self) -> Self {
        self.ok = self.entries.iter().all(|e| {
            e.signature_valid
                && e.link_valid
                && e.content_matches != Some(false)
                && e.problem.is_none()
        });
        self
    }

    /// Record whether `data` is the content `link` attested — or what a
    /// redaction of it left behind.
    ///
    /// The chain endpoint carries `data` only for amendments and
    /// rotations, so this is how a caller that read an entry in full folds
    /// that read into the report. `false` (and a `false`
    /// [`content_matches`](EntryVerdict::content_matches), which clears
    /// [`ok`](Self::ok) on the next [`settle`](Self::settle)) when the
    /// entry is not in this report at all.
    pub fn check_content(
        &mut self,
        link: &GovernanceChainLink,
        data: &serde_json::Value,
    ) -> bool {
        let hash = data_hash(data);
        let Some(entry) = self.entries.iter_mut().find(|e| e.id == link.id)
        else {
            return false;
        };
        let ok = hash == link.attestation.data_hash
            || entry.redacted_data_hash == Some(hash);
        entry.content_matches = Some(ok);
        ok
    }
}

// ---------------------------------------------------------------------------
// Signing
// ---------------------------------------------------------------------------

/// Attest an entry: the one construction path for
/// [`GovernanceAttestation`], used by the server at insert and by tests
/// building fixtures.
///
/// `signed_at` is truncated to whole seconds, the precision the signature
/// covers; store the value the attestation carries, not the one passed in.
pub fn attest(
    key: &SigningKey,
    envelope: &Envelope,
    chain_seq: u64,
    signed_at: DateTime<Utc>,
) -> GovernanceAttestation {
    let signed_at = truncate_to_seconds(signed_at);
    let entry_hash = envelope.entry_hash();
    let signature =
        crypto::sign(key, entry_hash.as_bytes(), signed_at.timestamp());
    GovernanceAttestation {
        envelope_version: envelope.agora_governance_log,
        chain_seq,
        prev_hash: envelope.prev_hash,
        data_hash: envelope.data_hash,
        entry_hash,
        signature: signature.into(),
        signed_at,
        retroactive: is_retroactive(envelope.created_at(), signed_at),
    }
}

// ---------------------------------------------------------------------------
// Verification
// ---------------------------------------------------------------------------

/// Why one link failed on its own, before chain context
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LinkError {
    #[error(
        "envelope version {0} is not supported (this verifier knows {ENVELOPE_VERSION})"
    )]
    UnsupportedVersion(u32),
    #[error("entry_hash does not recompute from the envelope fields")]
    HashMismatch,
    #[error("signature does not verify under the published key")]
    BadSignature,
}

/// Recompute a link's `entry_hash` from its fields
pub fn recompute_entry_hash(link: &GovernanceChainLink) -> Sha256Hex {
    Envelope::new(
        link.id.clone(),
        link.entry_type,
        link.created_at,
        link.attestation.prev_hash,
        link.attestation.data_hash,
    )
    .entry_hash()
}

/// Verify one link in isolation: version, hash recomputation, signature
pub fn verify_link(
    link: &GovernanceChainLink,
    key: &VerifyingKey,
) -> Result<(), LinkError> {
    let a = &link.attestation;
    if a.envelope_version != ENVELOPE_VERSION {
        return Err(LinkError::UnsupportedVersion(a.envelope_version));
    }
    if recompute_entry_hash(link) != a.entry_hash {
        return Err(LinkError::HashMismatch);
    }
    if !crypto::verify(
        key,
        a.entry_hash.as_bytes(),
        a.signed_at.timestamp(),
        &Signature::from(&a.signature),
    ) {
        return Err(LinkError::BadSignature);
    }
    Ok(())
}

/// `true` when `data` is what `link` attested
pub fn verify_data(
    link: &GovernanceChainLink,
    data: &serde_json::Value,
) -> bool {
    data_hash(data) == link.attestation.data_hash
}

/// The id series an entry type must use, and must not
fn prefix_problem(link: &GovernanceChainLink) -> Option<String> {
    let prefix = link.id.prefix();
    let reserved =
        matches!(prefix, GovernanceLogPrefix::Amd | GovernanceLogPrefix::Key);
    let expected = match link.entry_type {
        GovernanceLogEntryType::Amendment => Some(GovernanceLogPrefix::Amd),
        GovernanceLogEntryType::KeyRotation => Some(GovernanceLogPrefix::Key),
        _ => None,
    };
    match expected {
        Some(want) if prefix != want => Some(format!(
            "the id of a {} entry must be in the {want}- series, not {}",
            link.entry_type, link.id
        )),
        None if reserved => Some(format!(
            "{prefix}- ids are reserved for amendment and key_rotation \
             entries, but {} is a {}",
            link.id, link.entry_type
        )),
        _ => None,
    }
}

/// Which earlier entry an amendment names, once it is known to be one
fn amendment_target(
    amendment: &Amendment,
    seq: u64,
    seq_of: &HashMap<&str, u64>,
    links: &[&GovernanceChainLink],
) -> Result<u64, AmendmentError> {
    amendment.validate()?;
    let target = *seq_of.get(amendment.target.as_str()).ok_or_else(|| {
        AmendmentError::UnknownTarget(amendment.target.clone())
    })?;
    if target >= seq {
        return Err(AmendmentError::ForwardReference(amendment.target.clone()));
    }
    if links[target as usize - 1].attestation.entry_hash
        != amendment.target_entry_hash
    {
        return Err(AmendmentError::WrongTargetHash(amendment.target.clone()));
    }
    Ok(target)
}

/// The key history as the walk discovers it
struct KeyWalk {
    /// Oldest first; the last entry is the active key
    history: Vec<(GovernanceKeyRecord, VerifyingKey)>,
    unanchored: Vec<PublicKeyHex>,
    /// Every key that has held the chain, voided ones included. The anchor
    /// lists retired and compromised keys too, so without this a thief
    /// could declare a "compromise" that rotates back to the key they stole.
    seen: HashSet<PublicKeyHex>,
    /// `chain_seq`s inside a compromise window
    repudiated: HashSet<u64>,
}

impl KeyWalk {
    fn new(genesis: &VerifyingKey, anchor: &KeyAnchor) -> Self {
        let public_key = PublicKeyHex::from(genesis);
        Self {
            history: vec![(
                GovernanceKeyRecord {
                    public_key,
                    from_seq: 1,
                    through_seq: None,
                    status: KeyStatus::Active,
                    introduced_by: None,
                    retired_by: None,
                },
                *genesis,
            )],
            unanchored: if anchor.contains(&public_key) {
                Vec::new()
            } else {
                vec![public_key]
            },
            seen: HashSet::from([public_key]),
            repudiated: HashSet::new(),
        }
    }

    /// The key that signs entry `seq`
    fn in_force(&self, seq: u64) -> (PublicKeyHex, VerifyingKey) {
        let (record, key) = self
            .history
            .iter()
            .rev()
            .find(|(r, _)| r.from_seq <= seq)
            .unwrap_or(&self.history[0]);
        (record.public_key, *key)
    }

    /// The key that signs whatever comes next
    fn active(&self) -> PublicKeyHex {
        self.history
            .last()
            .map(|(r, _)| r.public_key)
            .expect("the genesis key is always in the history")
    }

    fn close(
        &mut self,
        through_seq: u64,
        status: KeyStatus,
        by: &GovernanceLogId,
    ) {
        if let Some((record, _)) = self.history.last_mut() {
            record.through_seq = Some(through_seq);
            record.status = status;
            record.retired_by = Some(by.clone());
        }
    }

    fn open(
        &mut self,
        public_key: PublicKeyHex,
        key: VerifyingKey,
        from_seq: u64,
        by: &GovernanceLogId,
    ) {
        self.history.push((
            GovernanceKeyRecord {
                public_key,
                from_seq,
                through_seq: None,
                status: KeyStatus::Active,
                introduced_by: Some(by.clone()),
                retired_by: None,
            },
            key,
        ));
    }

    /// Follow `rotation`, appended as `id` at `seq`
    fn apply(
        &mut self,
        rotation: &KeyRotation,
        link: &GovernanceChainLink,
        seq: u64,
        links: &[&GovernanceChainLink],
        anchor: &KeyAnchor,
    ) -> Result<(), RotationError> {
        rotation.verify_proof(link.attestation.prev_hash)?;
        let new_key = rotation
            .new_key
            .to_verifying_key()
            .map_err(|_| RotationError::BadNewKey)?;
        if self.seen.contains(&rotation.new_key) {
            return Err(RotationError::ReusedKey);
        }
        match rotation.reason {
            RotationReason::Routine => {
                if rotation.old_key != self.in_force(seq).0 {
                    return Err(RotationError::WrongOldKey);
                }
                self.close(seq, KeyStatus::Retired, &link.id);
                self.open(rotation.new_key, new_key, seq + 1, &link.id);
                self.seen.insert(rotation.new_key);
                if !anchor.contains(&rotation.new_key) {
                    self.unanchored.push(rotation.new_key);
                }
                Ok(())
            }
            RotationReason::Compromise => {
                let head = rotation
                    .last_trusted
                    .as_ref()
                    .ok_or(RotationError::MissingLastTrusted)?;
                let trusted_seq = head.chain_seq;
                let names_an_earlier_entry = trusted_seq >= 1
                    && trusted_seq < seq
                    && links[trusted_seq as usize - 1].id == head.id
                    && links[trusted_seq as usize - 1].attestation.entry_hash
                        == head.entry_hash;
                if !names_an_earlier_entry {
                    return Err(RotationError::UnknownLastTrusted);
                }
                // Trust cannot be anchored inside a window nobody trusts,
                // reattested or not: name an entry from before it.
                if self.repudiated.contains(&trusted_seq) {
                    return Err(RotationError::RepudiatedLastTrusted);
                }
                // The key in force at the last trusted entry — rotations
                // inside the window are the thief's, and void.
                if rotation.old_key != self.in_force(trusted_seq).0 {
                    return Err(RotationError::WrongOldKey);
                }
                if !anchor.contains(&rotation.new_key) {
                    return Err(RotationError::UnanchoredNewKey);
                }
                self.history.retain(|(r, _)| r.from_seq <= trusted_seq);
                self.close(trusted_seq, KeyStatus::Compromised, &link.id);
                self.open(rotation.new_key, new_key, seq, &link.id);
                self.seen.insert(rotation.new_key);
                self.repudiated.extend(trusted_seq + 1..seq);
                Ok(())
            }
        }
    }
}

/// Verify a whole chain from `genesis_key`, following the rotations it
/// declares and trusting `anchor` for the ones the chain cannot prove.
///
/// Links are sorted by `chain_seq` first, so the caller's order does not
/// matter. `retroactive` and `out_of_order` are recomputed from the
/// timestamps, not copied. `content_matches` is filled only for links that
/// carry `data` — for the rest, see
/// [`check_content`](GovernanceVerification::check_content).
///
/// `genesis_key` is the key the chain started under; it is not in the
/// chain, so a verifier has to be told. If it is not in `anchor` it is
/// reported in `unanchored_keys` rather than rejected — a client pinning
/// what it saw first passes `KeyAnchor::pinned(key)` and gets a clean
/// report.
///
/// The rules the report records that no type states on its own: an id
/// belongs to its entry type's series and appears once (`AMD-` and `KEY-`
/// are reserved for the two types that carry `data`); only an entry whose
/// own signature and linkage verify amends anything or moves the key, so
/// a forged entry cannot also describe the chain; and an amendment inside
/// a repudiated window has no effect unless a [`AmendmentKind::Reattested`]
/// vouches for its own entry first, resolved to a fixpoint.
pub fn verify_chain(
    links: &[GovernanceChainLink],
    genesis_key: &VerifyingKey,
    anchor: &KeyAnchor,
) -> GovernanceVerification {
    let mut links: Vec<&GovernanceChainLink> = links.iter().collect();
    links.sort_by_key(|l| l.attestation.chain_seq);

    let mut seq_of: HashMap<&str, u64> = HashMap::new();
    let mut duplicates: HashSet<&str> = HashSet::new();
    for (i, link) in links.iter().enumerate() {
        if seq_of.insert(link.id.as_str(), i as u64 + 1).is_some() {
            duplicates.insert(link.id.as_str());
        }
    }

    let mut walk = KeyWalk::new(genesis_key, anchor);
    // (seq, amendment id, amendment, target seq), in chain order
    let mut amendments: Vec<(u64, GovernanceLogId, Amendment, u64)> =
        Vec::new();
    let mut entries: Vec<EntryVerdict> = Vec::with_capacity(links.len());
    let mut prev: Option<&GovernanceChainLink> = None;

    for (i, link) in links.iter().enumerate() {
        let a = &link.attestation;
        let expected_seq = i as u64 + 1;
        let mut problems: Vec<String> = Vec::new();

        if let Some(problem) = prefix_problem(link) {
            problems.push(problem);
        }
        if duplicates.contains(link.id.as_str()) {
            problems.push(format!("{} appears more than once", link.id));
        }

        // The two entry types the verifier has to read. `data` is hashed
        // against the envelope before it is parsed, so what is read is
        // what was signed.
        let carries_meaning = matches!(
            link.entry_type,
            GovernanceLogEntryType::Amendment
                | GovernanceLogEntryType::KeyRotation
        );
        let mut amendment: Option<Amendment> = None;
        let mut rotation: Option<KeyRotation> = None;
        let mut content_matches: Option<bool> = None;
        match &link.data {
            Some(data) => {
                let matched = data_hash(data) == a.data_hash;
                content_matches = Some(matched);
                if !matched {
                    if carries_meaning {
                        problems.push(
                            "`data` does not hash to the attested data_hash"
                                .into(),
                        );
                    }
                } else {
                    match link.entry_type {
                        GovernanceLogEntryType::Amendment => {
                            match serde_json::from_value(data.clone()) {
                                Ok(v) => amendment = Some(v),
                                Err(e) => problems.push(format!(
                                    "amendment `data` is malformed: {e}"
                                )),
                            }
                        }
                        GovernanceLogEntryType::KeyRotation => {
                            match serde_json::from_value(data.clone()) {
                                Ok(v) => rotation = Some(v),
                                Err(e) => problems.push(format!(
                                    "key_rotation `data` is malformed: {e}"
                                )),
                            }
                        }
                        _ => {}
                    }
                }
            }
            None if carries_meaning => problems.push(format!(
                "a {} entry must carry its `data`",
                link.entry_type
            )),
            None => {}
        }

        // A compromise declaration is signed by the new key, and is
        // authentic only if the anchor vouches for that key. Everything
        // else is signed by the key in force.
        let declared_key = rotation
            .as_ref()
            .filter(|r| {
                r.reason == RotationReason::Compromise
                    && anchor.contains(&r.new_key)
                    && !walk.seen.contains(&r.new_key)
            })
            .and_then(|r| r.new_key.to_verifying_key().ok());
        let (key_hex, key) = match declared_key {
            Some(k) => (PublicKeyHex::from(&k), k),
            None => walk.in_force(expected_seq),
        };
        // Say why a declaration was not taken at its word; the bad
        // signature that follows is the consequence, not the cause.
        if declared_key.is_none()
            && let Some(r) = rotation
                .as_ref()
                .filter(|r| r.reason == RotationReason::Compromise)
        {
            problems.push(
                if walk.seen.contains(&r.new_key) {
                    RotationError::ReusedKey
                } else {
                    RotationError::UnanchoredNewKey
                }
                .to_string(),
            );
        }

        let (hash_ok, signature_valid) = match verify_link(link, &key) {
            Ok(()) => (true, true),
            Err(LinkError::BadSignature) => {
                problems.push(LinkError::BadSignature.to_string());
                (true, false)
            }
            Err(e) => {
                problems.push(e.to_string());
                (false, false)
            }
        };

        let mut link_valid = hash_ok;
        if a.chain_seq != expected_seq {
            link_valid = false;
            problems.push(format!(
                "chain_seq {} where {expected_seq} was expected",
                a.chain_seq
            ));
        }
        let expected_prev = prev.map(|p| p.attestation.entry_hash);
        if a.prev_hash != expected_prev {
            link_valid = false;
            problems.push(match (a.prev_hash, expected_prev) {
                (Some(_), None) => "first entry names a predecessor".into(),
                (None, Some(_)) => "prev_hash is null mid-chain".into(),
                _ => "prev_hash is not the previous entry's entry_hash".into(),
            });
        }
        let out_of_order = prev.is_some_and(|p| link.created_at < p.created_at);

        // Only an entry that is itself authentic moves the key or amends
        // anything: a forged one already fails the chain, and must not
        // also get to describe it.
        let authentic = signature_valid && link_valid;
        if let Some(rotation) = rotation.as_ref().filter(|_| authentic)
            && let Err(e) =
                walk.apply(rotation, link, expected_seq, &links, anchor)
        {
            problems.push(e.to_string());
        }
        if let Some(amendment) = amendment.filter(|_| authentic) {
            match amendment_target(&amendment, expected_seq, &seq_of, &links) {
                Ok(target) => amendments.push((
                    expected_seq,
                    link.id.clone(),
                    amendment,
                    target,
                )),
                Err(e) => problems.push(e.to_string()),
            }
        }

        entries.push(EntryVerdict {
            id: link.id.clone(),
            chain_seq: a.chain_seq,
            signature_valid,
            link_valid,
            content_matches,
            retroactive: is_retroactive(link.created_at, a.signed_at),
            out_of_order,
            amended_by: Vec::new(),
            signed_by: Some(key_hex),
            redacted: false,
            redacted_data_hash: None,
            repudiated: false,
            reattested_by: Vec::new(),
            problem: (!problems.is_empty()).then(|| problems.join("; ")),
        });
        prev = Some(link);
    }

    // Reattestations first, and to a fixpoint: an amendment inside a
    // repudiated window has no effect unless something later vouches for
    // it, and that something can be another reattestation.
    let mut applied = vec![false; amendments.len()];
    loop {
        let mut changed = false;
        for (i, (seq, id, amendment, target)) in amendments.iter().enumerate() {
            if applied[i]
                || amendment.kind != AmendmentKind::Reattested
                || walk.repudiated.contains(seq)
            {
                continue;
            }
            applied[i] = true;
            entries[*target as usize - 1].reattested_by.push(id.clone());
            walk.repudiated.remove(target);
            changed = true;
        }
        if !changed {
            break;
        }
    }

    for (seq, id, amendment, target) in &amendments {
        if walk.repudiated.contains(seq) {
            continue;
        }
        let entry = &mut entries[*target as usize - 1];
        entry.amended_by.push(id.clone());
        if let Some(redaction) = &amendment.redaction {
            entry.redacted = true;
            entry.redacted_data_hash = Some(redaction.resulting_data_hash);
        }
    }

    // A redacted entry's content is what the redaction left behind.
    for (i, link) in links.iter().enumerate() {
        if entries[i].content_matches == Some(false)
            && let (Some(data), Some(hash)) =
                (&link.data, entries[i].redacted_data_hash)
            && data_hash(data) == hash
        {
            entries[i].content_matches = Some(true);
        }
    }

    let mut seen = HashSet::new();
    walk.unanchored.retain(|key| seen.insert(*key));

    let mut repudiated = Vec::new();
    for (i, entry) in entries.iter_mut().enumerate() {
        if walk.repudiated.contains(&(i as u64 + 1)) {
            entry.repudiated = true;
            repudiated.push(entry.id.clone());
        }
    }

    GovernanceVerification {
        public_key: walk.active(),
        ok: false,
        head: prev.map(|p| p.id.clone()),
        entries,
        keys: walk.history.into_iter().map(|(record, _)| record).collect(),
        unanchored_keys: walk.unanchored,
        repudiated,
    }
    .settle()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::generate_keypair;
    use serde_json::json;

    pub(super) fn gov(n: u32) -> GovernanceLogId {
        format!("GOV-2026-{n:04}").parse().unwrap()
    }

    pub(super) fn amd(n: u32) -> GovernanceLogId {
        format!("AMD-2026-{n:04}").parse().unwrap()
    }

    pub(super) fn key_id(n: u32) -> GovernanceLogId {
        format!("KEY-2026-{n:04}").parse().unwrap()
    }

    pub(super) fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + secs, 123_456_789).unwrap()
    }

    /// The anchor a client that pinned the key it first saw would hold
    fn anchored(key: &VerifyingKey) -> KeyAnchor {
        KeyAnchor::pinned(key.into())
    }

    pub(super) fn link(
        key: &SigningKey,
        n: u32,
        prev: Option<&GovernanceChainLink>,
        data: &serde_json::Value,
        signed_at: DateTime<Utc>,
    ) -> GovernanceChainLink {
        let created_at = truncate_to_micros(at(n as i64 * 10));
        let envelope = Envelope::new(
            gov(n),
            GovernanceLogEntryType::CouncilDecision,
            created_at,
            prev.map(|p| p.attestation.entry_hash),
            data_hash(data),
        );
        let attestation = attest(
            key,
            &envelope,
            prev.map_or(1, |p| p.attestation.chain_seq + 1),
            signed_at,
        );
        GovernanceChainLink {
            id: gov(n),
            entry_type: GovernanceLogEntryType::CouncilDecision,
            created_at,
            attestation,
            data: None,
        }
    }

    pub(super) fn chain(key: &SigningKey, n: u32) -> Vec<GovernanceChainLink> {
        let mut out: Vec<GovernanceChainLink> = Vec::new();
        for i in 1..=n {
            let data = json!({"title": format!("Decision {i}"), "outcome": "approved"});
            let l = link(key, i, out.last(), &data, at(i as i64 * 10 + 1));
            out.push(l);
        }
        out
    }

    /// A chain under construction: one entry per `push`, each series
    /// numbered on its own, `data` carried for the entries a verifier
    /// reads.
    pub(super) struct Chain {
        pub(super) links: Vec<GovernanceChainLink>,
        gov: u32,
        amd: u32,
        key: u32,
    }

    impl Chain {
        pub(super) fn new() -> Self {
            Self {
                links: Vec::new(),
                gov: 0,
                amd: 0,
                key: 0,
            }
        }

        pub(super) fn prev_hash(&self) -> Option<Sha256Hex> {
            self.links.last().map(|l| l.attestation.entry_hash)
        }

        /// The `entry_hash` of the 1-indexed link `seq`
        pub(super) fn hash_at(&self, seq: usize) -> Sha256Hex {
            self.links[seq - 1].attestation.entry_hash
        }

        /// What [`Chain::amend`] will call the next amendment
        pub(super) fn next_amd(&self) -> GovernanceLogId {
            amd(self.amd + 1)
        }

        pub(super) fn push(
            &mut self,
            signer: &SigningKey,
            id: GovernanceLogId,
            entry_type: GovernanceLogEntryType,
            data: serde_json::Value,
            carry: bool,
        ) -> GovernanceLogId {
            let n = self.links.len() as i64 + 1;
            let created_at = truncate_to_micros(at(n * 10));
            let envelope = Envelope::new(
                id.clone(),
                entry_type,
                created_at,
                self.prev_hash(),
                data_hash(&data),
            );
            let attestation =
                attest(signer, &envelope, n as u64, at(n * 10 + 1));
            self.links.push(GovernanceChainLink {
                id: id.clone(),
                entry_type,
                created_at,
                attestation,
                data: carry.then_some(data),
            });
            id
        }

        /// A council decision carrying `data` (which the link does not,
        /// as the chain endpoint does not carry transcripts)
        pub(super) fn entry(
            &mut self,
            signer: &SigningKey,
            data: serde_json::Value,
        ) -> GovernanceLogId {
            self.gov += 1;
            let id = gov(self.gov);
            self.push(
                signer,
                id,
                GovernanceLogEntryType::CouncilDecision,
                data,
                false,
            )
        }

        pub(super) fn decision(
            &mut self,
            signer: &SigningKey,
        ) -> GovernanceLogId {
            let data = json!({"title": format!("Decision {}", self.gov + 1)});
            self.entry(signer, data)
        }

        pub(super) fn amend(
            &mut self,
            signer: &SigningKey,
            amendment: &Amendment,
        ) -> GovernanceLogId {
            self.amd += 1;
            let id = amd(self.amd);
            self.push(
                signer,
                id,
                GovernanceLogEntryType::Amendment,
                serde_json::to_value(amendment).unwrap(),
                true,
            )
        }

        pub(super) fn rotate(
            &mut self,
            signer: &SigningKey,
            rotation: &KeyRotation,
        ) -> GovernanceLogId {
            self.key += 1;
            let id = key_id(self.key);
            self.push(
                signer,
                id,
                GovernanceLogEntryType::KeyRotation,
                serde_json::to_value(rotation).unwrap(),
                true,
            )
        }
    }

    // -- canonical JSON --

    #[test]
    fn canonical_json_sorts_keys_at_every_level() {
        let v = json!({"b": {"z": 1, "a": [{"y": 2, "x": 3}]}, "a": null});
        assert_eq!(
            canonical_json(&v),
            br#"{"a":null,"b":{"a":[{"x":3,"y":2}],"z":1}}"#
        );
    }

    #[test]
    fn canonical_json_is_compact_and_escapes_like_serde() {
        let v = json!({"s": "tab\there \"q\" ünïcode \u{1F600}", "n": [1, -2, 3.5, true, false]});
        let bytes = canonical_json(&v);
        let text = std::str::from_utf8(&bytes).unwrap();
        assert_eq!(
            text,
            r#"{"n":[1,-2,3.5,true,false],"s":"tab\there \"q\" ünïcode 😀"}"#
        );
    }

    #[test]
    fn canonical_json_ignores_insertion_order() {
        let mut a = serde_json::Map::new();
        a.insert("z".into(), json!(1));
        a.insert("a".into(), json!(2));
        let mut b = serde_json::Map::new();
        b.insert("a".into(), json!(2));
        b.insert("z".into(), json!(1));
        assert_eq!(
            canonical_json(&serde_json::Value::Object(a)),
            canonical_json(&serde_json::Value::Object(b))
        );
    }

    #[test]
    fn canonical_json_empty_containers() {
        assert_eq!(canonical_json(&json!({})), b"{}");
        assert_eq!(canonical_json(&json!([])), b"[]");
        assert_eq!(
            canonical_json(&json!({"a": {}, "b": []})),
            br#"{"a":{},"b":[]}"#
        );
    }

    // -- envelope --

    #[test]
    fn preimage_is_declaration_ordered_json() {
        let e = Envelope::new(
            gov(1),
            GovernanceLogEntryType::AppealsCourtDecision,
            at(0),
            None,
            data_hash(&json!({})),
        );
        let text = String::from_utf8(e.preimage()).unwrap();
        assert!(text.starts_with(r#"{"agora_governance_log":1,"id":"GOV-2026-0001","entry_type":"appeals_court_decision","created_at":1700000000123456,"prev_hash":null,"data_hash":""#), "{text}");
    }

    #[test]
    fn every_envelope_field_changes_the_hash() {
        let base = Envelope::new(
            gov(1),
            GovernanceLogEntryType::CouncilDecision,
            at(0),
            None,
            data_hash(&json!({"a":1})),
        );
        let h = base.entry_hash();
        let mut e = base.clone();
        e.id = gov(2);
        assert_ne!(e.entry_hash(), h);
        let mut e = base.clone();
        e.entry_type = GovernanceLogEntryType::PolicyChange;
        assert_ne!(e.entry_hash(), h);
        let mut e = base.clone();
        e.created_at += 1;
        assert_ne!(e.entry_hash(), h);
        let mut e = base.clone();
        e.prev_hash = Some(h);
        assert_ne!(e.entry_hash(), h);
        let mut e = base.clone();
        e.data_hash = data_hash(&json!({"a":2}));
        assert_ne!(e.entry_hash(), h);
        assert_eq!(base.entry_hash(), h, "and it is deterministic");
    }

    #[test]
    fn truncation_matches_what_the_envelope_carries() {
        let t = at(0);
        assert_eq!(truncate_to_micros(t).timestamp_subsec_nanos(), 123_456_000);
        assert_eq!(truncate_to_seconds(t).timestamp_subsec_nanos(), 0);
        assert_eq!(
            Envelope::new(
                gov(1),
                GovernanceLogEntryType::CouncilDecision,
                t,
                None,
                data_hash(&json!(null))
            )
            .created_at,
            truncate_to_micros(t).timestamp_micros()
        );
    }

    // -- hex newtypes --

    #[test]
    fn hex_newtypes_round_trip_and_reject_wrong_lengths() {
        let h = data_hash(&json!(1));
        let s = serde_json::to_string(&h).unwrap();
        assert_eq!(s.len(), 66);
        let back: Sha256Hex = serde_json::from_str(&s).unwrap();
        assert_eq!(back, h);
        assert!(serde_json::from_str::<Sha256Hex>("\"abcd\"").is_err());
        assert!("zz".repeat(32).parse::<Sha256Hex>().is_err());
        assert!(Sha256Hex::try_from(vec![0u8; 31]).is_err());
        assert_eq!(format!("{h:?}"), format!("Sha256Hex({h})"));
    }

    // -- signing and verification --

    #[test]
    fn attest_then_verify_link() {
        let (key, pk) = generate_keypair();
        let l = link(&key, 1, None, &json!({"a": 1}), at(5));
        assert_eq!(verify_link(&l, &pk), Ok(()));
        assert!(verify_data(&l, &json!({"a": 1})));
        assert!(!verify_data(&l, &json!({"a": 2})));
        assert!(!l.attestation.retroactive);
    }

    #[test]
    fn wrong_key_fails_signature_only() {
        let (key, _) = generate_keypair();
        let (_, other) = generate_keypair();
        let l = link(&key, 1, None, &json!({}), at(5));
        assert_eq!(verify_link(&l, &other), Err(LinkError::BadSignature));
    }

    #[test]
    fn tampering_with_any_attested_field_is_detected() {
        let (key, pk) = generate_keypair();
        let l = link(&key, 1, None, &json!({"a": 1}), at(5));

        let mut t = l.clone();
        t.created_at += chrono::Duration::microseconds(1);
        assert_eq!(verify_link(&t, &pk), Err(LinkError::HashMismatch));

        let mut t = l.clone();
        t.entry_type = GovernanceLogEntryType::StewardVeto;
        assert_eq!(verify_link(&t, &pk), Err(LinkError::HashMismatch));

        let mut t = l.clone();
        t.attestation.data_hash = data_hash(&json!({"a": 2}));
        assert_eq!(verify_link(&t, &pk), Err(LinkError::HashMismatch));

        // Back-dating the signature: the hash still recomputes, but the
        // signature covered the real signed_at.
        let mut t = l.clone();
        t.attestation.signed_at -= chrono::Duration::seconds(1);
        assert_eq!(verify_link(&t, &pk), Err(LinkError::BadSignature));

        let mut t = l.clone();
        t.attestation.envelope_version = 2;
        assert_eq!(verify_link(&t, &pk), Err(LinkError::UnsupportedVersion(2)));
    }

    #[test]
    fn a_good_chain_verifies_in_any_input_order() {
        let (key, pk) = generate_keypair();
        let mut c = chain(&key, 4);
        c.reverse();
        let v = verify_chain(&c, &pk, &anchored(&pk));
        assert!(v.ok, "{v:#?}");
        assert_eq!(v.head, Some(gov(4)));
        assert_eq!(
            v.entries.iter().map(|e| e.chain_seq).collect::<Vec<_>>(),
            [1, 2, 3, 4]
        );
        assert!(v.entries.iter().all(|e| e.content_matches.is_none()
            && e.problem.is_none()
            && !e.out_of_order));
        assert_eq!(v.public_key, PublicKeyHex::from(&pk));
    }

    #[test]
    fn empty_chain_is_ok_with_no_head() {
        let (_, pk) = generate_keypair();
        let v = verify_chain(&[], &pk, &anchored(&pk));
        assert!(v.ok);
        assert!(v.head.is_none());
        assert!(v.entries.is_empty());
    }

    #[test]
    fn a_changed_entry_breaks_its_signature_and_the_next_link() {
        let (key, pk) = generate_keypair();
        let mut c = chain(&key, 3);
        // Re-attest entry 2 with different data but the same predecessor,
        // as a key holder rewriting history would.
        let rewritten = link(
            &key,
            2,
            Some(&c[0]),
            &json!({"title": "Decision 2", "outcome": "REJECTED"}),
            at(21),
        );
        c[1] = rewritten;
        let v = verify_chain(&c, &pk, &anchored(&pk));
        assert!(!v.ok);
        assert!(
            v.entries[1].signature_valid && v.entries[1].link_valid,
            "the rewrite itself is well-formed: {:#?}",
            v.entries[1]
        );
        assert!(
            !v.entries[2].link_valid,
            "but entry 3 no longer points at it: {:#?}",
            v.entries[2]
        );
        assert!(
            v.entries[2]
                .problem
                .as_deref()
                .unwrap()
                .contains("prev_hash")
        );
    }

    #[test]
    fn a_removed_entry_is_a_gap_and_a_broken_link() {
        let (key, pk) = generate_keypair();
        let mut c = chain(&key, 3);
        c.remove(1);
        let v = verify_chain(&c, &pk, &anchored(&pk));
        assert!(!v.ok);
        assert!(v.entries[0].link_valid);
        let p = v.entries[1].problem.as_deref().unwrap();
        assert!(p.contains("chain_seq 3 where 2 was expected"), "{p}");
        assert!(p.contains("prev_hash"), "{p}");
    }

    #[test]
    fn a_second_genesis_is_rejected() {
        let (key, pk) = generate_keypair();
        let mut c = chain(&key, 2);
        let rogue = link(&key, 2, None, &json!({}), at(21));
        c[1] = rogue;
        let v = verify_chain(&c, &pk, &anchored(&pk));
        assert!(!v.ok);
        assert!(
            v.entries[1]
                .problem
                .as_deref()
                .unwrap()
                .contains("null mid-chain")
        );
    }

    #[test]
    fn retroactive_and_out_of_order_are_recomputed_not_copied() {
        let (key, pk) = generate_keypair();
        let first = link(&key, 1, None, &json!({}), at(10 + 3600));
        // Second entry recorded *before* the first by the clock, but after it
        // in the chain. Valid chain, flagged order.
        let created = truncate_to_micros(at(5));
        let envelope = Envelope::new(
            gov(2),
            GovernanceLogEntryType::CouncilDecision,
            created,
            Some(first.attestation.entry_hash),
            data_hash(&json!({})),
        );
        let attestation = attest(&key, &envelope, 2, at(6));
        let mut second = GovernanceChainLink {
            id: gov(2),
            entry_type: GovernanceLogEntryType::CouncilDecision,
            created_at: created,
            attestation,
            data: None,
        };
        second.attestation.retroactive = true; // a lying flag on the wire
        let v = verify_chain(&[first, second], &pk, &anchored(&pk));
        assert!(v.ok, "{v:#?}");
        assert!(v.entries[0].retroactive);
        assert!(!v.entries[1].retroactive, "recomputed from timestamps");
        assert!(v.entries[1].out_of_order);
    }

    #[test]
    fn content_mismatch_settles_to_not_ok() {
        let (key, pk) = generate_keypair();
        let c = chain(&key, 1);
        let mut v = verify_chain(&c, &pk, &anchored(&pk));
        v.entries[0].content_matches = Some(true);
        assert!(v.clone().settle().ok);
        v.entries[0].content_matches = Some(false);
        assert!(!v.settle().ok);
    }

    // -- amendments --

    #[test]
    fn an_amendment_fills_amended_by_on_its_target() {
        let (key, pk) = generate_keypair();
        let mut c = Chain::new();
        let target = c.decision(&key);
        c.decision(&key);
        let amendment = Amendment::new(
            target,
            c.hash_at(1),
            AmendmentKind::NonPrecedential,
            "§1 (Red Team Cases Recharacterized)",
            "diagnostic finding — not citable as moderation precedent",
        )
        .unwrap()
        .with_authority(gov(5))
        .with_rationale("§5 leaves the ruling itself standing");
        let id = c.amend(&key, &amendment);

        let v = verify_chain(&c.links, &pk, &anchored(&pk));
        assert!(v.ok, "{v:#?}");
        assert_eq!(v.entries[0].amended_by, vec![id]);
        assert!(v.entries[1].amended_by.is_empty());
        assert!(!v.entries[0].redacted);
        assert_eq!(v.head, Some(amd(1)));
        // The amendment link carries `data`, so its own content is checked.
        assert_eq!(v.entries[2].content_matches, Some(true));
        assert_eq!(v.entries[0].content_matches, None);
        assert_eq!(standing([amendment.kind]), Standing::NonPrecedential);
    }

    #[test]
    fn an_amendment_must_name_an_earlier_entry_by_its_exact_hash() {
        let (key, pk) = generate_keypair();
        let problem = |c: &Chain, at: usize| -> String {
            verify_chain(&c.links, &pk, &anchored(&pk)).entries[at]
                .problem
                .clone()
                .unwrap_or_default()
        };

        let mut c = Chain::new();
        c.decision(&key);
        let unknown = Amendment::new(
            gov(99),
            c.hash_at(1),
            AmendmentKind::Overruled,
            "b",
            "n",
        )
        .unwrap();
        c.amend(&key, &unknown);
        assert!(
            problem(&c, 1).contains("is not an entry of this chain"),
            "{}",
            problem(&c, 1)
        );
        assert!(!verify_chain(&c.links, &pk, &anchored(&pk)).ok);

        // Names an entry that does not exist yet.
        let mut c = Chain::new();
        c.decision(&key);
        let forward = Amendment::new(
            gov(2),
            c.hash_at(1),
            AmendmentKind::Overruled,
            "b",
            "n",
        )
        .unwrap();
        c.amend(&key, &forward);
        c.decision(&key);
        assert!(
            problem(&c, 1).contains("not an earlier entry"),
            "{}",
            problem(&c, 1)
        );

        // Right id, wrong entry.
        let mut c = Chain::new();
        let target = c.decision(&key);
        let wrong_hash = Amendment::new(
            target,
            data_hash(&json!("some other entry")),
            AmendmentKind::Overruled,
            "b",
            "n",
        )
        .unwrap();
        c.amend(&key, &wrong_hash);
        assert!(problem(&c, 1).contains("entry_hash"), "{}", problem(&c, 1));
        assert!(
            verify_chain(&c.links, &pk, &anchored(&pk)).entries[0]
                .amended_by
                .is_empty()
        );
    }

    #[test]
    fn redaction_shape_violations_fail_verification() {
        let (key, pk) = generate_keypair();
        let amend_with = |mutate: &dyn Fn(&mut Amendment)| -> String {
            let mut c = Chain::new();
            let target = c.decision(&key);
            let mut amendment = Amendment::new(
                target,
                c.hash_at(1),
                AmendmentKind::Correction,
                "b",
                "n",
            )
            .unwrap();
            mutate(&mut amendment);
            c.amend(&key, &amendment);
            let v = verify_chain(&c.links, &pk, &anchored(&pk));
            assert!(!v.ok, "{v:#?}");
            v.entries[1].problem.clone().unwrap_or_default()
        };

        // `redaction` is the only way to get a well-formed one, so a
        // redaction kind without a `Redaction` can only be hand-built.
        assert_eq!(
            Amendment::new(
                gov(1),
                data_hash(&json!(null)),
                AmendmentKind::Redaction,
                "b",
                "n"
            ),
            Err(AmendmentError::MissingRedaction)
        );
        let p = amend_with(&|a| a.kind = AmendmentKind::Redaction);
        assert!(p.contains("requires a `redaction`"), "{p}");

        let p = amend_with(&|a| {
            a.redaction = Some(Redaction {
                fields: vec!["/x".into()],
                resulting_data_hash: data_hash(&json!({})),
            })
        });
        assert!(p.contains("only valid on kind"), "{p}");

        let p = amend_with(&|a| a.agora_governance_amendment = 2);
        assert!(p.contains("agora_governance_amendment is 2"), "{p}");
    }

    #[test]
    fn standing_is_the_last_amendment_that_changes_it() {
        use AmendmentKind::*;
        assert_eq!(standing([]), Standing::InForce);
        assert_eq!(standing([Correction, Redaction]), Standing::InForce);
        assert_eq!(
            standing([Correction, NonPrecedential, Redaction]),
            Standing::NonPrecedential
        );
        assert_eq!(standing([Overruled, Reinstated]), Standing::InForce);
        assert_eq!(standing([Reinstated, Superseded]), Standing::Superseded);
        assert_eq!(kind_standing(Reattested), None);
        assert_eq!(kind_standing(Reinstated), Some(Standing::InForce));
    }

    #[test]
    fn a_redaction_verifies_against_the_amendment_and_nothing_else() {
        let (key, pk) = generate_keypair();
        let data = json!({
            "finding": "upheld",
            "subject": {"handle": "someone", "detail": "personal"},
        });
        let mut c = Chain::new();
        let target = c.entry(&key, data.clone());
        let amendment_id = c.next_amd();
        let (amendment, redacted) = Amendment::redaction(
            &amendment_id,
            target,
            c.hash_at(1),
            "GDPR Art. 17(1)(a)",
            "personal data removed on request",
            vec!["/subject/handle".into(), "/subject/detail".into()],
            &data,
        )
        .unwrap();
        assert_eq!(c.amend(&key, &amendment), amendment_id);

        let mut v = verify_chain(&c.links, &pk, &anchored(&pk));
        assert!(v.ok, "{v:#?}");
        assert!(v.entries[0].redacted);
        assert_eq!(v.entries[0].redacted_data_hash, Some(data_hash(&redacted)));
        assert_eq!(
            redacted["subject"]["handle"],
            json!(format!("[redacted by {amendment_id}]"))
        );
        assert_eq!(redacted["finding"], json!("upheld"), "and nothing else");

        // What the server now serves verifies.
        assert!(v.check_content(&c.links[0], &redacted));
        assert_eq!(v.entries[0].content_matches, Some(true));
        assert!(v.clone().settle().ok);
        // So does the original, for anyone who kept a copy.
        assert!(v.check_content(&c.links[0], &data));

        // A second edit does not.
        let mut tampered = redacted.clone();
        tampered["finding"] = json!("overturned");
        assert!(!v.check_content(&c.links[0], &tampered));
        assert_eq!(v.entries[0].content_matches, Some(false));
        assert!(!v.settle().ok);
    }

    #[test]
    fn content_that_matches_nothing_fails_without_an_amendment() {
        let (key, pk) = generate_keypair();
        let mut c = Chain::new();
        c.entry(&key, json!({"a": 1}));
        let mut v = verify_chain(&c.links, &pk, &anchored(&pk));
        assert!(!v.check_content(&c.links[0], &json!({"a": 2})));
        assert!(!v.clone().settle().ok);
        // An entry that is not in the report at all is not a pass either.
        let other = link(&key, 9, None, &json!({}), at(9));
        assert!(!v.check_content(&other, &json!({})));
    }

    #[test]
    fn tampering_with_an_amendments_data_is_caught_by_the_data_hash() {
        let (key, pk) = generate_keypair();
        let mut c = Chain::new();
        let target = c.decision(&key);
        let amendment = Amendment::new(
            target,
            c.hash_at(1),
            AmendmentKind::Overruled,
            "b",
            "overruled by a later decision",
        )
        .unwrap();
        c.amend(&key, &amendment);
        c.links[1].data.as_mut().unwrap()["note"] =
            json!("reinstated, actually");

        let v = verify_chain(&c.links, &pk, &anchored(&pk));
        assert!(!v.ok, "{v:#?}");
        let p = v.entries[1].problem.as_deref().unwrap();
        assert!(p.contains("does not hash to the attested data_hash"), "{p}");
        assert!(
            v.entries[0].amended_by.is_empty(),
            "an unreadable amendment has no effect"
        );
        assert!(
            v.entries[1].signature_valid && v.entries[1].link_valid,
            "the envelope is untouched — only the content is not what it \
             committed to"
        );
    }

    #[test]
    fn an_amendment_or_rotation_must_carry_its_data() {
        let (key, pk) = generate_keypair();
        let mut c = Chain::new();
        let target = c.decision(&key);
        let amendment = Amendment::new(
            target,
            c.hash_at(1),
            AmendmentKind::Correction,
            "b",
            "n",
        )
        .unwrap();
        c.amend(&key, &amendment);
        let rotation = KeyRotation::routine(
            (&pk).into(),
            &key,
            c.prev_hash(),
            at(35),
            "n",
        );
        c.rotate(&key, &rotation);
        c.links[1].data = None;
        c.links[2].data = None;

        let v = verify_chain(&c.links, &pk, &anchored(&pk));
        assert!(!v.ok, "{v:#?}");
        for (i, entry_type) in [(1, "amendment"), (2, "key_rotation")] {
            let p = v.entries[i].problem.as_deref().unwrap();
            assert!(p.contains(&format!("a {entry_type} entry")), "{p}");
            assert!(p.contains("must carry its `data`"), "{p}");
        }
    }

    #[test]
    fn the_id_series_must_match_the_entry_type() {
        let (key, pk) = generate_keypair();
        let mut c = Chain::new();
        let target = c.decision(&key);
        let amendment = Amendment::new(
            target,
            c.hash_at(1),
            AmendmentKind::Correction,
            "b",
            "n",
        )
        .unwrap();
        c.push(
            &key,
            gov(7),
            GovernanceLogEntryType::Amendment,
            serde_json::to_value(&amendment).unwrap(),
            true,
        );
        let v = verify_chain(&c.links, &pk, &anchored(&pk));
        assert!(!v.ok, "{v:#?}");
        let p = v.entries[1].problem.as_deref().unwrap();
        assert!(p.contains("must be in the AMD- series"), "{p}");

        let mut c = Chain::new();
        c.push(
            &key,
            key_id(1),
            GovernanceLogEntryType::CouncilDecision,
            json!({}),
            false,
        );
        let v = verify_chain(&c.links, &pk, &anchored(&pk));
        let p = v.entries[0].problem.as_deref().unwrap();
        assert!(p.contains("reserved"), "{p}");
    }

    #[test]
    fn redact_data_replaces_whole_values_and_refuses_the_rest() {
        let id = amd(3);
        let data = json!({"a": {"b": [1, {"c": "secret"}]}, "d/e": "slash"});
        let out = redact_data(&data, &["/a/b/1/c".into(), "/d~1e".into()], &id)
            .unwrap();
        assert_eq!(out["a"]["b"][1]["c"], json!(redaction_marker(&id)));
        assert_eq!(out["d/e"], json!(redaction_marker(&id)));
        assert_eq!(out["a"]["b"][0], json!(1), "untouched");

        assert_eq!(
            redact_data(&data, &["/a/nope".into()], &id),
            Err(RedactError::Unresolved("/a/nope".into()))
        );
        assert_eq!(
            redact_data(&data, &["".into()], &id),
            Err(RedactError::WholeEntry)
        );
        assert_eq!(redact_data(&data, &[], &id).unwrap(), data);
    }

    // -- key rotation --

    #[test]
    fn a_routine_rotation_moves_the_chain_to_the_new_key() {
        let (old, old_pk) = generate_keypair();
        let (new, new_pk) = generate_keypair();
        let anchor = anchored(&old_pk).with((&new_pk).into());
        let mut c = Chain::new();
        c.decision(&old);
        let rotation = KeyRotation::routine(
            (&old_pk).into(),
            &new,
            c.prev_hash(),
            at(25),
            "scheduled rotation",
        );
        let rotation_id = c.rotate(&old, &rotation);
        c.decision(&new);

        let v = verify_chain(&c.links, &old_pk, &anchor);
        assert!(v.ok, "{v:#?}");
        assert_eq!(v.public_key, (&new_pk).into());
        assert_eq!(
            v.entries[1].signed_by,
            Some((&old_pk).into()),
            "the rotation itself is signed by the old key"
        );
        assert_eq!(v.entries[2].signed_by, Some((&new_pk).into()));
        assert!(v.unanchored_keys.is_empty());
        assert!(v.repudiated.is_empty());
        assert_eq!(v.keys.len(), 2);
        assert_eq!(v.keys[0].public_key, (&old_pk).into());
        assert_eq!(v.keys[0].from_seq, 1);
        assert_eq!(v.keys[0].through_seq, Some(2));
        assert_eq!(v.keys[0].status, KeyStatus::Retired);
        assert_eq!(v.keys[0].introduced_by, None);
        assert_eq!(v.keys[0].retired_by.as_ref(), Some(&rotation_id));
        assert_eq!(v.keys[1].from_seq, 3);
        assert_eq!(v.keys[1].through_seq, None);
        assert_eq!(v.keys[1].status, KeyStatus::Active);
        assert_eq!(v.keys[1].introduced_by.as_ref(), Some(&rotation_id));
    }

    #[test]
    fn the_old_key_cannot_sign_after_a_routine_rotation() {
        let (old, old_pk) = generate_keypair();
        let (new, new_pk) = generate_keypair();
        let anchor = anchored(&old_pk).with((&new_pk).into());
        let mut c = Chain::new();
        c.decision(&old);
        let rotation = KeyRotation::routine(
            (&old_pk).into(),
            &new,
            c.prev_hash(),
            at(25),
            "scheduled",
        );
        c.rotate(&old, &rotation);
        c.decision(&old);

        let v = verify_chain(&c.links, &old_pk, &anchor);
        assert!(!v.ok, "{v:#?}");
        assert!(!v.entries[2].signature_valid);
        assert!(
            v.entries[2].link_valid,
            "the linkage is fine; the key is not"
        );
    }

    #[test]
    fn a_routine_rotation_to_an_unanchored_key_is_followed_and_reported() {
        let (old, old_pk) = generate_keypair();
        let (new, new_pk) = generate_keypair();
        let mut c = Chain::new();
        c.decision(&old);
        let rotation = KeyRotation::routine(
            (&old_pk).into(),
            &new,
            c.prev_hash(),
            at(25),
            "scheduled",
        );
        c.rotate(&old, &rotation);
        c.decision(&new);

        // The anchor has never heard of the new key: chain-valid, and the
        // one thing a reference client must shout about.
        let v = verify_chain(&c.links, &old_pk, &anchored(&old_pk));
        assert!(v.ok, "{v:#?}");
        assert_eq!(v.unanchored_keys, vec![PublicKeyHex::from(&new_pk)]);
        assert_eq!(v.public_key, (&new_pk).into());
    }

    #[test]
    fn a_forged_proof_of_possession_is_rejected() {
        let (old, old_pk) = generate_keypair();
        let (_, new_pk) = generate_keypair();
        let (thief, _) = generate_keypair();
        let mut c = Chain::new();
        c.decision(&old);
        // A rotation to a key nobody holds: the proof is signed by the old
        // key (and by an unrelated one) instead of by `new_key` itself.
        let mut rotation = KeyRotation::routine(
            (&old_pk).into(),
            &thief,
            c.prev_hash(),
            at(25),
            "scheduled",
        );
        rotation.new_key = (&new_pk).into();
        let statement = rotation.statement(c.prev_hash());
        rotation.proof = crypto::sign(
            &old,
            statement.hash().as_bytes(),
            rotation.proof_signed_at,
        )
        .into();
        c.rotate(&old, &rotation);

        assert_eq!(
            rotation.verify_proof(c.links[1].attestation.prev_hash),
            Err(RotationError::BadProof)
        );
        let v = verify_chain(&c.links, &old_pk, &anchored(&old_pk));
        assert!(!v.ok, "{v:#?}");
        assert!(
            v.entries[1]
                .problem
                .as_deref()
                .unwrap()
                .contains("proof of possession")
        );
        assert_eq!(v.public_key, (&old_pk).into(), "the chain does not move");
    }

    #[test]
    fn a_proof_does_not_replay_at_another_position() {
        let (old, old_pk) = generate_keypair();
        let (new, new_pk) = generate_keypair();
        let anchor = anchored(&old_pk).with((&new_pk).into());
        let mut c = Chain::new();
        c.decision(&old);
        // Proof bound to the position right after entry 1 …
        let rotation = KeyRotation::routine(
            (&old_pk).into(),
            &new,
            c.prev_hash(),
            at(25),
            "scheduled",
        );
        // … and appended one entry later.
        c.decision(&old);
        c.rotate(&old, &rotation);

        let v = verify_chain(&c.links, &old_pk, &anchor);
        assert!(!v.ok, "{v:#?}");
        assert!(
            v.entries[2]
                .problem
                .as_deref()
                .unwrap()
                .contains("proof of possession")
        );
        assert_eq!(v.public_key, (&old_pk).into());
    }

    #[test]
    fn a_compromise_repudiates_the_window_and_a_reattestation_restores_one() {
        let (old, old_pk) = generate_keypair();
        let (new, new_pk) = generate_keypair();
        let anchor = anchored(&old_pk).with((&new_pk).into());
        let mut c = Chain::new();
        c.decision(&old); // 1 — the last entry anyone trusts
        c.decision(&old); // 2 — inside the window
        let reattested = c.decision(&old); // 3 — inside, later vouched for
        let rotation = KeyRotation::compromise(
            (&old_pk).into(),
            &new,
            TrustedHead {
                id: gov(1),
                chain_seq: 1,
                entry_hash: c.hash_at(1),
            },
            c.prev_hash(),
            at(45),
            "signing key exfiltrated",
        );
        let rotation_id = c.rotate(&new, &rotation); // 4 — signed by the NEW key
        let vouch = Amendment::new(
            reattested,
            c.hash_at(3),
            AmendmentKind::Reattested,
            "Art. VII",
            "independently verified; the Steward vouches for it",
        )
        .unwrap();
        let vouch_id = c.amend(&new, &vouch); // 5

        let v = verify_chain(&c.links, &old_pk, &anchor);
        assert!(
            v.ok,
            "repudiation is a declared state, not a defect: {v:#?}"
        );
        assert_eq!(v.repudiated, vec![gov(2)]);
        assert!(v.entries[1].repudiated);
        assert!(!v.entries[2].repudiated);
        assert_eq!(v.entries[2].reattested_by, vec![vouch_id.clone()]);
        assert_eq!(v.entries[2].amended_by, vec![vouch_id]);
        assert_eq!(v.entries[3].signed_by, Some((&new_pk).into()));
        assert_eq!(v.public_key, (&new_pk).into());
        assert_eq!(v.keys.len(), 2);
        assert_eq!(v.keys[0].status, KeyStatus::Compromised);
        assert_eq!(
            v.keys[0].through_seq,
            Some(1),
            "trusted through the last trusted entry, not through the \
             declaration"
        );
        assert_eq!(v.keys[0].retired_by.as_ref(), Some(&rotation_id));
        assert_eq!(v.keys[1].from_seq, 4, "the declaration is its own first");
    }

    #[test]
    fn a_stolen_key_cannot_be_rotated_back_in() {
        // K1 is compromised and replaced by K2. Both are published, so both
        // are in every anchor. The thief, still holding K1, declares a
        // "compromise" of K2 naming K1 as the new key.
        let (k1, k1_pk) = generate_keypair();
        let (k2, k2_pk) = generate_keypair();
        let anchor = anchored(&k1_pk).with((&k2_pk).into());
        let mut c = Chain::new();
        c.decision(&k1); // 1
        let real = KeyRotation::compromise(
            (&k1_pk).into(),
            &k2,
            TrustedHead {
                id: gov(1),
                chain_seq: 1,
                entry_hash: c.hash_at(1),
            },
            c.prev_hash(),
            at(25),
            "signing key exfiltrated",
        );
        c.rotate(&k2, &real); // 2
        c.decision(&k2); // 3
        let honest = verify_chain(&c.links, &k1_pk, &anchor);
        assert!(honest.ok, "{honest:#?}");

        let hijack = KeyRotation::compromise(
            (&k2_pk).into(),
            &k1,
            TrustedHead {
                id: gov(2),
                chain_seq: 3,
                entry_hash: c.hash_at(3),
            },
            c.prev_hash(),
            at(45),
            "the Steward's key is the compromised one, trust me",
        );
        c.rotate(&k1, &hijack); // 4 — signed by the stolen key
        c.decision(&k1); // 5

        let v = verify_chain(&c.links, &k1_pk, &anchor);
        assert!(!v.ok);
        assert_eq!(v.public_key, (&k2_pk).into(), "the chain stays with K2");
        assert!(!v.entries[3].signature_valid, "{:#?}", v.entries[3]);
        assert!(!v.entries[4].signature_valid, "K1 signs nothing again");
        assert_eq!(v.keys.len(), 2);
        assert_eq!(v.keys[1].status, KeyStatus::Active);
    }

    #[test]
    fn a_routine_rotation_cannot_reuse_a_key_either() {
        let (k1, k1_pk) = generate_keypair();
        let (k2, k2_pk) = generate_keypair();
        let anchor = anchored(&k1_pk).with((&k2_pk).into());
        let mut c = Chain::new();
        c.decision(&k1);
        let out = KeyRotation::routine(
            (&k1_pk).into(),
            &k2,
            c.prev_hash(),
            at(15),
            "",
        );
        c.rotate(&k1, &out);
        let back = KeyRotation::routine(
            (&k2_pk).into(),
            &k1,
            c.prev_hash(),
            at(25),
            "",
        );
        c.rotate(&k2, &back);
        let v = verify_chain(&c.links, &k1_pk, &anchor);
        assert!(!v.ok);
        assert!(
            v.entries[2]
                .problem
                .as_deref()
                .unwrap()
                .contains("never brought back"),
            "{:#?}",
            v.entries[2]
        );
        assert_eq!(v.public_key, (&k2_pk).into());
    }

    #[test]
    fn a_forged_entry_has_no_effects() {
        let (steward, steward_pk) = generate_keypair();
        let (forger, _) = generate_keypair();
        let anchor = anchored(&steward_pk);
        let mut c = Chain::new();
        let target = c.decision(&steward); // 1
        let fake = Amendment::new(
            target,
            c.hash_at(1),
            AmendmentKind::Overruled,
            "none",
            "overruled, says nobody with the key",
        )
        .unwrap();
        c.amend(&forger, &fake); // 2 — not signed by the key in force
        let grab = KeyRotation::routine(
            (&steward_pk).into(),
            &forger,
            c.prev_hash(),
            at(35),
            "",
        );
        c.rotate(&forger, &grab); // 3 — likewise

        let v = verify_chain(&c.links, &steward_pk, &anchor);
        assert!(!v.ok);
        assert!(v.entries[0].amended_by.is_empty(), "{:#?}", v.entries[0]);
        assert_eq!(v.public_key, (&steward_pk).into());
        assert_eq!(v.keys.len(), 1);
        assert!(v.unanchored_keys.is_empty());
    }

    #[test]
    fn a_repeated_id_is_a_problem() {
        let (key, pk) = generate_keypair();
        let mut c = Chain::new();
        c.decision(&key);
        c.gov = 0;
        c.decision(&key); // GOV-2026-0001 again
        let v = verify_chain(&c.links, &pk, &anchored(&pk));
        assert!(!v.ok);
        assert!(v.entries.iter().all(|e| {
            e.problem
                .as_deref()
                .is_some_and(|p| p.contains("more than once"))
        }));
    }

    #[test]
    fn a_second_compromise_cannot_anchor_inside_the_first_window() {
        let (k1, k1_pk) = generate_keypair();
        let (k2, k2_pk) = generate_keypair();
        let (k3, k3_pk) = generate_keypair();
        let anchor =
            anchored(&k1_pk).with((&k2_pk).into()).with((&k3_pk).into());
        let mut c = Chain::new();
        c.decision(&k1); // 1 — trusted
        c.decision(&k1); // 2 — inside the first window
        let first = KeyRotation::compromise(
            (&k1_pk).into(),
            &k2,
            TrustedHead {
                id: gov(1),
                chain_seq: 1,
                entry_hash: c.hash_at(1),
            },
            c.prev_hash(),
            at(35),
            "",
        );
        c.rotate(&k2, &first); // 3
        let second = KeyRotation::compromise(
            (&k1_pk).into(),
            &k3,
            TrustedHead {
                id: gov(2),
                chain_seq: 2,
                entry_hash: c.hash_at(2),
            },
            c.prev_hash(),
            at(45),
            "",
        );
        c.rotate(&k3, &second); // 4

        let v = verify_chain(&c.links, &k1_pk, &anchor);
        assert!(!v.ok);
        let p = v.entries[3].problem.as_deref().unwrap();
        assert!(p.contains("repudiated"), "{p}");
        assert_eq!(v.keys.len(), 2, "K1 and K2; K3 never took the chain");
        assert_eq!(v.keys[0].through_seq, Some(1));
    }

    #[test]
    fn an_unanchored_compromise_fails_closed() {
        let (old, old_pk) = generate_keypair();
        let (new, _) = generate_keypair();
        let mut c = Chain::new();
        c.decision(&old);
        c.decision(&old);
        let rotation = KeyRotation::compromise(
            (&old_pk).into(),
            &new,
            TrustedHead {
                id: gov(1),
                chain_seq: 1,
                entry_hash: c.hash_at(1),
            },
            c.prev_hash(),
            at(45),
            "trust me",
        );
        c.rotate(&new, &rotation);

        // Nothing in the verifier's world vouches for the new key, so the
        // declaration authenticates nothing.
        let v = verify_chain(&c.links, &old_pk, &anchored(&old_pk));
        assert!(!v.ok, "{v:#?}");
        let p = v.entries[2].problem.as_deref().unwrap();
        assert!(p.contains("outside the trust anchor"), "{p}");
        assert!(!v.entries[2].signature_valid, "checked under the old key");
        assert!(v.repudiated.is_empty(), "and nothing is repudiated");
        assert_eq!(v.public_key, (&old_pk).into());
    }

    /// The scenario the anchor exists for: a thief with the signing key
    /// rotates the chain onto their own, and the Steward answers with a
    /// compromise declaration from before the theft.
    #[test]
    fn a_thiefs_rotation_is_void_once_a_compromise_names_an_earlier_head() {
        let (steward, steward_pk) = generate_keypair();
        let (thief, thief_pk) = generate_keypair();
        let (recovery, recovery_pk) = generate_keypair();
        let anchor = anchored(&steward_pk).with((&recovery_pk).into());

        let mut c = Chain::new();
        c.decision(&steward); // 1 — the last honest entry
        let stolen = KeyRotation::routine(
            (&steward_pk).into(),
            &thief,
            c.prev_hash(),
            at(25),
            "routine",
        );
        c.rotate(&steward, &stolen); // 2 — signed with the stolen key
        c.decision(&thief); // 3 — the thief's own decision

        let declaration = KeyRotation::compromise(
            (&steward_pk).into(),
            &recovery,
            TrustedHead {
                id: gov(1),
                chain_seq: 1,
                entry_hash: c.hash_at(1),
            },
            c.prev_hash(),
            at(55),
            "key stolen; everything after GOV-2026-0001 is disclaimed",
        );
        c.rotate(&recovery, &declaration); // 4

        let v = verify_chain(&c.links, &steward_pk, &anchor);
        assert!(v.ok, "{v:#?}");
        assert_eq!(v.repudiated, vec![key_id(1), gov(2)]);
        assert!(v.entries[1].repudiated && v.entries[2].repudiated);
        assert_eq!(
            v.unanchored_keys,
            vec![PublicKeyHex::from(&thief_pk)],
            "and the theft was visible as it happened"
        );
        assert_eq!(v.public_key, (&recovery_pk).into());
        assert_eq!(v.keys.len(), 2, "the thief's key is not part of history");
        assert_eq!(v.keys[0].public_key, (&steward_pk).into());
        assert_eq!(v.keys[0].status, KeyStatus::Compromised);
        assert_eq!(v.keys[1].public_key, (&recovery_pk).into());
    }

    #[test]
    fn a_compromise_must_name_a_real_head_and_the_key_that_held_it() {
        let (old, old_pk) = generate_keypair();
        let (new, new_pk) = generate_keypair();
        let anchor = anchored(&old_pk).with((&new_pk).into());
        let head = |c: &Chain| TrustedHead {
            id: gov(1),
            chain_seq: 1,
            entry_hash: c.hash_at(1),
        };

        // A head whose hash is not that entry's.
        let mut c = Chain::new();
        c.decision(&old);
        let mut trusted = head(&c);
        trusted.entry_hash = data_hash(&json!("nope"));
        let rotation = KeyRotation::compromise(
            (&old_pk).into(),
            &new,
            trusted,
            c.prev_hash(),
            at(45),
            "n",
        );
        c.rotate(&new, &rotation);
        let v = verify_chain(&c.links, &old_pk, &anchor);
        let p = v.entries[1].problem.as_deref().unwrap();
        assert!(p.contains("last_trusted"), "{p}");

        // An old_key that was never in force.
        let (other, _) = generate_keypair();
        let mut c = Chain::new();
        c.decision(&old);
        let rotation = KeyRotation::compromise(
            (&other.verifying_key()).into(),
            &new,
            head(&c),
            c.prev_hash(),
            at(45),
            "n",
        );
        c.rotate(&new, &rotation);
        let v = verify_chain(&c.links, &old_pk, &anchor);
        let p = v.entries[1].problem.as_deref().unwrap();
        assert!(p.contains("old_key is not the key"), "{p}");

        // A routine rotation that names one anyway.
        let mut c = Chain::new();
        c.decision(&old);
        let mut rotation = KeyRotation::routine(
            (&old_pk).into(),
            &new,
            c.prev_hash(),
            at(45),
            "n",
        );
        rotation.last_trusted = Some(head(&c));
        c.rotate(&old, &rotation);
        let v = verify_chain(&c.links, &old_pk, &anchor);
        let p = v.entries[1].problem.as_deref().unwrap();
        assert!(p.contains("must not name last_trusted"), "{p}");
    }

    #[test]
    fn a_genesis_key_outside_the_anchor_is_reported_not_rejected() {
        let (key, pk) = generate_keypair();
        let c = chain(&key, 2);
        let v = verify_chain(&c, &pk, &KeyAnchor::default());
        assert!(v.ok, "{v:#?}");
        assert_eq!(v.unanchored_keys, vec![PublicKeyHex::from(&pk)]);
        assert_eq!(v.keys.len(), 1);
        assert_eq!(v.keys[0].status, KeyStatus::Active);
        assert_eq!(v.keys[0].from_seq, 1);
        assert_eq!(v.keys[0].through_seq, None);
    }

    #[test]
    fn published_keys_are_curve_points_and_make_an_anchor() {
        let anchor = KeyAnchor::published();
        assert!(!anchor.is_empty());
        assert_eq!(anchor.keys().count(), PUBLISHED_KEYS.len());
        for published in PUBLISHED_KEYS {
            let key: PublicKeyHex = published.parse().unwrap();
            assert!(anchor.contains(&key));
            assert!(
                key.to_verifying_key().is_ok(),
                "{published} is not a valid Ed25519 public key"
            );
        }
        let (_, other) = generate_keypair();
        assert!(!anchor.contains(&(&other).into()));
        assert!(
            anchor
                .clone()
                .with((&other).into())
                .contains(&(&other).into())
        );
    }

    // -- the wire --

    #[test]
    fn amendments_and_rotations_round_trip_as_entry_data() {
        let (key, pk) = generate_keypair();
        let amendment = Amendment::new(
            gov(1),
            data_hash(&json!("x")),
            AmendmentKind::Superseded,
            "Art. VI § 2",
            "superseded by GOV-2026-0009",
        )
        .unwrap()
        .with_authority(gov(9))
        .with_rationale("the later decision covers the same subject");
        let value = serde_json::to_value(&amendment).unwrap();
        assert_eq!(value["kind"], "superseded");
        assert_eq!(value["agora_governance_amendment"], 1);
        assert!(value.get("redaction").is_none(), "{value}");
        assert_eq!(
            serde_json::from_value::<Amendment>(value).unwrap(),
            amendment
        );

        let rotation = KeyRotation::compromise(
            (&pk).into(),
            &key,
            TrustedHead {
                id: gov(1),
                chain_seq: 1,
                entry_hash: data_hash(&json!("x")),
            },
            Some(data_hash(&json!("prev"))),
            at(0),
            "note",
        );
        let value = serde_json::to_value(&rotation).unwrap();
        assert_eq!(value["reason"], "compromise");
        assert_eq!(value["last_trusted"]["chain_seq"], 1);
        assert_eq!(
            serde_json::from_value::<KeyRotation>(value).unwrap(),
            rotation
        );

        let notice = AmendmentNotice::new(amd(1), at(0), &amendment);
        let value = serde_json::to_value(&notice).unwrap();
        assert_eq!(value["id"], "AMD-2026-0001");
        assert_eq!(value["note"], "superseded by GOV-2026-0009");
        assert_eq!(
            serde_json::from_value::<AmendmentNotice>(value).unwrap(),
            notice
        );
    }

    /// The verifier hashes the raw `data` value, so a field it has never
    /// heard of neither breaks it nor escapes the signature — and an
    /// amendment written without one verifies just the same.
    #[test]
    fn an_amendment_verifies_with_or_without_its_optional_fields() {
        let (key, pk) = generate_keypair();
        let mut c = Chain::new();
        let target = c.decision(&key);
        let bare = Amendment::new(
            target.clone(),
            c.hash_at(1),
            AmendmentKind::Correction,
            "clerical",
            "typo in the citation",
        )
        .unwrap();
        assert!(
            serde_json::to_value(&bare)
                .unwrap()
                .get("rationale")
                .is_none()
        );
        c.amend(&key, &bare);
        let full = bare.clone().with_rationale("at length: …");
        c.amend(&key, &full);
        // And a field from a future version of the shape.
        let mut future = serde_json::to_value(&full).unwrap();
        future["superseded_by_something_new"] = json!(["later"]);
        c.push(
            &key,
            amd(3),
            GovernanceLogEntryType::Amendment,
            future,
            true,
        );

        let v = verify_chain(&c.links, &pk, &anchored(&pk));
        assert!(v.ok, "{v:#?}");
        assert_eq!(
            v.entries[0].amended_by,
            vec![amd(1), amd(2), amd(3)],
            "all three name the target"
        );
    }

    /// Pre-0.26 JSON — no `data`, no `signed_by`, no repudiation — still
    /// parses, because every field added since is `#[serde(default)]`.
    #[test]
    fn pre_0_26_wire_still_deserializes() {
        let link: GovernanceChainLink = serde_json::from_value(json!({
            "id": "GOV-2026-0001",
            "entry_type": "council_decision",
            "created_at": "2023-11-14T22:13:20.123456Z",
            "attestation": {
                "envelope_version": 1,
                "chain_seq": 1,
                "prev_hash": null,
                "data_hash": "00".repeat(32),
                "entry_hash": "11".repeat(32),
                "signature": "22".repeat(64),
                "signed_at": "2023-11-14T22:13:21Z",
                "retroactive": false,
            },
        }))
        .unwrap();
        assert!(link.data.is_none());
        assert!(
            serde_json::to_value(&link).unwrap().get("data").is_none(),
            "and a link without data does not grow a null field"
        );

        let verdict: EntryVerdict = serde_json::from_value(json!({
            "id": "GOV-2026-0001",
            "chain_seq": 1,
            "signature_valid": true,
            "link_valid": true,
            "content_matches": null,
            "retroactive": false,
            "out_of_order": false,
            "amended_by": [],
        }))
        .unwrap();
        assert!(!verdict.repudiated && !verdict.redacted);
        assert!(verdict.signed_by.is_none());
        assert!(verdict.reattested_by.is_empty());

        let verification: GovernanceVerification =
            serde_json::from_value(json!({
                "public_key": "33".repeat(32),
                "ok": true,
                "head": "GOV-2026-0001",
                "entries": [],
            }))
            .unwrap();
        assert!(verification.keys.is_empty());
        assert!(verification.unanchored_keys.is_empty());
        assert!(verification.repudiated.is_empty());
    }

    #[test]
    fn the_report_round_trips() {
        let (key, pk) = generate_keypair();
        let mut c = Chain::new();
        let target = c.decision(&key);
        let amendment = Amendment::new(
            target,
            c.hash_at(1),
            AmendmentKind::Overruled,
            "b",
            "n",
        )
        .unwrap();
        c.amend(&key, &amendment);
        let v = verify_chain(&c.links, &pk, &anchored(&pk));
        let text = serde_json::to_string(&v).unwrap();
        assert_eq!(
            serde_json::from_str::<GovernanceVerification>(&text).unwrap(),
            v
        );

        let keys = GovernanceSigningKeys { keys: v.keys };
        let value = serde_json::to_value(&keys).unwrap();
        assert!(value["keys"].is_array(), "an object, not a bare array");
        assert_eq!(value["keys"][0]["status"], "active");
        assert_eq!(
            serde_json::from_value::<GovernanceSigningKeys>(value).unwrap(),
            keys
        );
    }

    /// The envelope is version 1 and there is a live chain signed under it:
    /// these bytes and this hash are load-bearing, not a snapshot to
    /// re-bless when something changes them.
    #[test]
    fn envelope_v1_preimage_and_hash_are_pinned() {
        let envelope = Envelope::new(
            gov(6),
            GovernanceLogEntryType::CouncilDecision,
            at(0),
            Some(Sha256Hex::from([0x11; 32])),
            data_hash(&json!({"outcome": "approved", "title": "Ratification"})),
        );
        assert_eq!(
            String::from_utf8(envelope.preimage()).unwrap(),
            "{\"agora_governance_log\":1,\"id\":\"GOV-2026-0006\",\
             \"entry_type\":\"council_decision\",\
             \"created_at\":1700000000123456,\
             \"prev_hash\":\"1111111111111111111111111111111111111111111111111111111111111111\",\
             \"data_hash\":\"a4adf645ae3f60c56484d01aea87d6d490321d7fc66b1607df14b023fe567c7b\"}"
        );
        assert_eq!(
            envelope.entry_hash().to_hex(),
            "ba27577432f81e415f1c01cc4cfabab6070e3ac50fd468fffe195ef19c0e9464"
        );
    }

    /// The parity check the Steward's second channel exists for: the key
    /// the platform serves must be the newest key this build trusts.
    ///
    /// Networked, so it is `#[ignore]`d and CI runs it as its own job —
    /// a 5G blip should not read as a code failure. `just
    /// check-published-keys`.
    #[cfg(feature = "agora-client")]
    #[tokio::test]
    #[ignore = "networked: hits the live platform"]
    async fn the_published_key_is_the_one_the_platform_serves() {
        let client = crate::client::Client::new(
            url::Url::parse("https://subliminal.technology").unwrap(),
        )
        .unwrap();
        let served = client.get_governance_signing_key().await.unwrap();
        let newest: PublicKeyHex = PUBLISHED_KEYS
            .last()
            .expect("PUBLISHED_KEYS is never empty")
            .parse()
            .unwrap();
        assert_eq!(served.algorithm, "ed25519");
        assert_eq!(
            served.public_key, newest,
            "the platform serves {} but the newest key in PUBLISHED_KEYS is \
             {newest} — if this is a rotation, agentkit publishes the new \
             key FIRST and the rotation entry second",
            served.public_key
        );
    }

    #[cfg(feature = "schemars")]
    #[test]
    fn wire_schemas_are_ref_free() {
        use crate::responses::inline_schema_for;
        for (name, schema) in [
            (
                "GovernanceAttestation",
                inline_schema_for::<GovernanceAttestation>(),
            ),
            (
                "GovernanceChainLink",
                inline_schema_for::<GovernanceChainLink>(),
            ),
            (
                "GovernanceSigningKey",
                inline_schema_for::<GovernanceSigningKey>(),
            ),
            (
                "GovernanceVerification",
                inline_schema_for::<GovernanceVerification>(),
            ),
            (
                "Vec<GovernanceChainLink>",
                inline_schema_for::<Vec<GovernanceChainLink>>(),
            ),
            ("Amendment", inline_schema_for::<Amendment>()),
            ("Redaction", inline_schema_for::<Redaction>()),
            ("AmendmentNotice", inline_schema_for::<AmendmentNotice>()),
            ("KeyRotation", inline_schema_for::<KeyRotation>()),
            ("TrustedHead", inline_schema_for::<TrustedHead>()),
            (
                "GovernanceKeyRecord",
                inline_schema_for::<GovernanceKeyRecord>(),
            ),
            (
                "GovernanceSigningKeys",
                inline_schema_for::<GovernanceSigningKeys>(),
            ),
            ("AmendmentKind", inline_schema_for::<AmendmentKind>()),
            ("Standing", inline_schema_for::<Standing>()),
            ("KeyStatus", inline_schema_for::<KeyStatus>()),
            ("RotationReason", inline_schema_for::<RotationReason>()),
        ] {
            let text = serde_json::to_string(&schema).unwrap();
            assert!(!text.contains("$ref"), "{name} must be $ref-free: {text}");
            assert!(
                !text.contains("$defs"),
                "{name} must be $defs-free: {text}"
            );
        }
        let text =
            serde_json::to_string(&inline_schema_for::<Sha256Hex>()).unwrap();
        assert!(text.contains("^[0-9a-f]{64}$"), "{text}");
        let text = serde_json::to_string(&inline_schema_for::<SignatureHex>())
            .unwrap();
        assert!(text.contains("^[0-9a-f]{128}$"), "{text}");
    }
}
