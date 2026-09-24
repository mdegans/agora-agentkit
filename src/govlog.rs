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
//!   about its force ([`Standing`]) or its content ([`Redaction`]). Its
//!   own free text is committed to, not contained (see [`TextCommitment`]),
//!   because an amendment is the one thing that can never be redacted. A
//!   redaction replaces values in the target's `data` in place; the
//!   original `entry_hash` stays on the row so later links still verify,
//!   and the amendment's `resulting_data_hash` is what the redacted data
//!   must now hash to. [`EntryVerdict::content_matches`] is the check.
//! - [`KeyRotation`] (`KEY-`) moves the chain to a new signing key. A
//!   routine rotation is signed by the old key and a compromise
//!   declaration by the new one, but neither signature is what makes the
//!   change authentic: a [`KeyCertificate`] from the offline root keys
//!   ([`ROOT_KEYS`]) is, so holding the online key is never enough to
//!   move the chain. See [`verify_chain`].
//!
//! A third series is reserved but read by no verifier: a [`StewardRecord`]
//! (`REC-`) says what was done — a key ceremony, a restore — and decides
//! nothing. It exists because a rotation can never be redacted and so
//! carries keys and hashes only; the narrative that names people goes in an
//! entry that can be.
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

mod texts;
pub use texts::{
    AmendmentText, AmendmentTextStatus, AmendmentTexts, CommittedText,
    TextCommitment, TextStatus, WITHHELD_TEXT,
};

mod council;
pub use council::{
    AgendaRanking, Ballot, CouncilAttachment, CouncilDecisionRecord,
    CouncilRound, CouncilSeat, CouncilVote, DecisionCategory, FinalVotes,
    PlacedProposal, SeatRanking, SeatResponse,
};

mod redactable;
pub use redactable::{REDACTION_MARKER_PATTERN, Redactable};

mod record;
pub use record::{
    RecordAttachment, RecordParticipant, STEWARD_RECORD_VERSION, StewardRecord,
};

mod root;
pub use root::{
    CertPurpose, CertificateError, KEY_CERT_VERSION, KeyCertStatement,
    KeyCertificate, ROOT_DOMAIN, ROOT_KEYS, ROOT_THRESHOLD, RootSet,
    RootSignature,
};

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

hex_bytes!(
    /// A blinding value: 32 random bytes carried in a redactable entry's
    /// `data` under [`BLIND_KEY`]. See [`blind_data`] for what it is for.
    Blind,
    32
);

hex_bytes!(
    /// The salt of a [`TextCommitment`]: 32 random bytes kept beside the
    /// text, and deleted with it
    TextSalt,
    32
);

impl Blind {
    /// A fresh value from the operating system's random source
    pub fn random() -> Self {
        use rand::RngCore;
        let mut bytes = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        Self(bytes)
    }
}

impl TextSalt {
    /// A fresh value from the operating system's random source
    pub fn random() -> Self {
        Self(*Blind::random().as_bytes())
    }
}

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

/// The RFC 6901 pointer to the first number in `data` that is not a 64-bit
/// integer, if there is one.
///
/// Governance `data` never contains one. [`canonical_json`] writes a number
/// the way `serde_json` does, and how that prints a float has changed
/// between releases (`1e21` became `1e+21`); an integer past `u64` is a
/// float to it as well, and its float parsing is not exactly rounded. A
/// hash that is meant to be permanent cannot depend on any of that, so the
/// writer refuses such `data` ([`blind_data`], [`redact_data`]) and a
/// verifier reports it without hashing it. A fraction goes in a string.
pub fn non_integer_number(data: &serde_json::Value) -> Option<String> {
    fn find(value: &serde_json::Value, path: &mut String) -> bool {
        use serde_json::Value::{Array, Number, Object};
        let mark = path.len();
        match value {
            Number(n) => return n.is_f64(),
            Array(items) => {
                for (i, item) in items.iter().enumerate() {
                    path.push_str(&format!("/{i}"));
                    if find(item, path) {
                        return true;
                    }
                    path.truncate(mark);
                }
            }
            Object(map) => {
                for (key, item) in map {
                    path.push('/');
                    path.push_str(&key.replace('~', "~0").replace('/', "~1"));
                    if find(item, path) {
                        return true;
                    }
                    path.truncate(mark);
                }
            }
            _ => {}
        }
        false
    }
    let mut path = String::new();
    find(data, &mut path).then_some(path)
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
    /// The texts a version 2 [`Amendment`] commits to, as far as the
    /// platform still holds them. Outside the envelope on purpose: a text
    /// can be erased without the chain changing.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "read_as_written"
    )]
    pub texts: Option<AmendmentTexts>,
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

/// The [`Amendment`] payload version this module produces. Version 1,
/// whose texts are in the signed `data`, still verifies: the platform has
/// three, all reviewed to hold no personal data.
pub const AMENDMENT_VERSION: u32 = 2;

/// An amendment is malformed
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AmendmentError {
    #[error("agora_governance_amendment is {0}, not 1 or {AMENDMENT_VERSION}")]
    UnsupportedVersion(u32),
    #[error(
        "a version 1 amendment carries its texts and a version \
         {AMENDMENT_VERSION} one commits to them; this does neither \
         consistently"
    )]
    TextShape,
    #[error("`{0}` beside the entry is not the text the entry committed to")]
    TextMismatch(&'static str),
    #[error("texts beside an entry that commits to none")]
    UncommittedText,
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
    /// Recharacterized)"`, `"GDPR Art. 17(1)(a)"`. Never personal data —
    /// and erasable, for the day that rule is broken.
    pub basis: AmendmentText,
    /// The label readers and prompts show next to the target
    pub note: AmendmentText,
    /// Why, at length — `note` is the label, this is the reasoning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<AmendmentText>,
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

/// An [`Amendment`] and the texts it commits to: what a writer appends,
/// the first as the entry's `data` and the second beside it
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AmendmentDraft {
    pub amendment: Amendment,
    pub texts: AmendmentTexts,
}

impl AmendmentDraft {
    /// An amendment of `kind` against `target`.
    ///
    /// Redactions go through [`redaction`](Self::redaction) instead, which
    /// is the only way to get a [`Redaction`] whose `resulting_data_hash`
    /// is the hash of data that actually exists. A `&str` or `String`
    /// text gets a [random](TextSalt::random) salt.
    pub fn new(
        target: GovernanceLogId,
        target_entry_hash: Sha256Hex,
        kind: AmendmentKind,
        basis: impl Into<CommittedText>,
        note: impl Into<CommittedText>,
    ) -> Result<Self, AmendmentError> {
        if kind == AmendmentKind::Redaction {
            return Err(AmendmentError::MissingRedaction);
        }
        let (basis, note) = (basis.into(), note.into());
        Ok(Self {
            amendment: Amendment {
                agora_governance_amendment: AMENDMENT_VERSION,
                target,
                target_entry_hash,
                kind,
                authority: None,
                basis: AmendmentText::Committed(basis.commitment()),
                note: AmendmentText::Committed(note.commitment()),
                rationale: None,
                redaction: None,
            },
            texts: AmendmentTexts {
                basis: Some(basis),
                note: Some(note),
                rationale: None,
            },
        })
    }

    /// A redaction of `fields` from the target's `data`, with the redacted
    /// data it commits to.
    ///
    /// `amendment_id` is the id this amendment will be appended under: the
    /// marker left behind names it, so the redaction says who ordered it.
    /// Append both together or neither — the returned `data` is what the
    /// target's row must hold for [`verify_chain`] to accept it. `blind`
    /// is the target's new [`Blind`]: [random](Blind::random), so a
    /// rehearsal's `resulting_data_hash` is not the real one's.
    #[allow(clippy::too_many_arguments)]
    pub fn redaction(
        amendment_id: &GovernanceLogId,
        target: GovernanceLogId,
        target_entry_hash: Sha256Hex,
        basis: impl Into<CommittedText>,
        note: impl Into<CommittedText>,
        fields: Vec<String>,
        data: &serde_json::Value,
        blind: Blind,
    ) -> Result<(Self, serde_json::Value), RedactError> {
        let redacted = redact_data(data, &fields, amendment_id, blind)?;
        let (basis, note) = (basis.into(), note.into());
        Ok((
            Self {
                amendment: Amendment {
                    agora_governance_amendment: AMENDMENT_VERSION,
                    target,
                    target_entry_hash,
                    kind: AmendmentKind::Redaction,
                    authority: None,
                    basis: AmendmentText::Committed(basis.commitment()),
                    note: AmendmentText::Committed(note.commitment()),
                    rationale: None,
                    redaction: Some(Redaction {
                        fields,
                        resulting_data_hash: data_hash(&redacted),
                    }),
                },
                texts: AmendmentTexts {
                    basis: Some(basis),
                    note: Some(note),
                    rationale: None,
                },
            },
            redacted,
        ))
    }

    /// The draft with the governance entry that authorizes it
    pub fn with_authority(mut self, authority: GovernanceLogId) -> Self {
        self.amendment.authority = Some(authority);
        self
    }

    /// The draft with its [`rationale`](Amendment::rationale)
    pub fn with_rationale(
        mut self,
        rationale: impl Into<CommittedText>,
    ) -> Self {
        let rationale = rationale.into();
        self.amendment.rationale =
            Some(AmendmentText::Committed(rationale.commitment()));
        self.texts.rationale = Some(rationale);
        self
    }
}

impl Amendment {
    /// Version, the shape of the texts for that version, and the
    /// redaction-shape invariant — everything checkable without the rest
    /// of the chain
    pub fn validate(&self) -> Result<(), AmendmentError> {
        let plain = match self.agora_governance_amendment {
            1 => true,
            AMENDMENT_VERSION => false,
            other => return Err(AmendmentError::UnsupportedVersion(other)),
        };
        let texts =
            [Some(&self.basis), Some(&self.note), self.rationale.as_ref()];
        if texts.into_iter().flatten().any(|t| t.is_plain() != plain) {
            return Err(AmendmentError::TextShape);
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

    /// Where each committed text stands given what is `beside` the entry;
    /// `None` for version 1, whose texts are in the signed `data`.
    ///
    /// A text that is beside the entry and is not the one committed to,
    /// or that the entry never committed to at all, is an error: someone
    /// put words next to a signed entry that the signer did not write.
    pub fn text_status(
        &self,
        beside: Option<&AmendmentTexts>,
    ) -> Result<Option<AmendmentTextStatus>, AmendmentError> {
        let empty = AmendmentTexts::default();
        let beside = beside.unwrap_or(&empty);
        let (Some(basis), Some(note)) = (
            self.basis.status(beside.basis.as_ref()),
            self.note.status(beside.note.as_ref()),
        ) else {
            return if beside.is_empty() {
                Ok(None)
            } else {
                Err(AmendmentError::UncommittedText)
            };
        };
        let rationale = match (&self.rationale, &beside.rationale) {
            (None, Some(_)) => return Err(AmendmentError::UncommittedText),
            (None, None) => None,
            (Some(text), beside) => text.status(beside.as_ref()),
        };
        for (name, status) in [
            ("basis", Some(basis)),
            ("note", Some(note)),
            ("rationale", rationale),
        ] {
            if status == Some(TextStatus::Mismatch) {
                return Err(AmendmentError::TextMismatch(name));
            }
        }
        Ok(Some(AmendmentTextStatus {
            basis,
            note,
            rationale,
        }))
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

/// The top-level key of a redactable entry's `data` that holds its
/// [`Blind`]
pub const BLIND_KEY: &str = "_blind";

/// Whether entries of this type can be redacted, and so carry a [`Blind`].
/// Amendments and key rotations cannot: verifiers read their `data`, and a
/// chain whose own corrections can be edited proves nothing.
pub fn is_redactable(entry_type: GovernanceLogEntryType) -> bool {
    !matches!(
        entry_type,
        GovernanceLogEntryType::Amendment | GovernanceLogEntryType::KeyRotation
    )
}

/// `data` cannot be blinded
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BlindError {
    #[error("a redactable entry's data must be a JSON object")]
    NotAnObject,
    #[error(
        "data already has a {BLIND_KEY:?} key; the writer supplies it, not the caller"
    )]
    AlreadyBlinded,
    #[error(
        "{0:?} is a number that is not a 64-bit integer; governance data \
         never contains one (put a fraction in a string)"
    )]
    NonIntegerNumber(String),
}

/// `data` with a [`Blind`] under [`BLIND_KEY`] — what a writer signs and
/// stores for every [redactable](is_redactable) entry.
///
/// An entry's `data_hash` is public and permanent: the chain cannot verify
/// without it. After a redaction everything in `data` *except* the removed
/// values is public too, so without a blind anyone could test a guess at a
/// removed value — a name, a handle — by putting it back and hashing. The
/// blind is 256 bits of the preimage that [`redact_data`] replaces along
/// with the values, so the old hash can no longer be reproduced by anyone
/// who did not already hold the unredacted entry. It is not a secret while
/// the entry is whole, and it is not part of the envelope: verifiers hash
/// `data` as they always did.
///
/// Entries written before blinding existed have none. Their first
/// redaction is only as safe as the removed values are hard to guess
/// (redact the enclosing value when in doubt); it leaves a blind behind,
/// so later ones are protected.
pub fn blind_data(
    data: &serde_json::Value,
    blind: Blind,
) -> Result<serde_json::Value, BlindError> {
    if let Some(pointer) = non_integer_number(data) {
        return Err(BlindError::NonIntegerNumber(pointer));
    }
    let mut out = data.clone();
    let object = out.as_object_mut().ok_or(BlindError::NotAnObject)?;
    if object.contains_key(BLIND_KEY) {
        return Err(BlindError::AlreadyBlinded);
    }
    object.insert(BLIND_KEY.to_string(), blind.to_hex().into());
    Ok(out)
}

/// A [`Redaction`] cannot be applied as asked
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RedactError {
    #[error("pointer {0:?} does not resolve in the entry's data")]
    Unresolved(String),
    #[error("the empty pointer would redact the whole entry")]
    WholeEntry,
    #[error("a redaction names at least one pointer")]
    NoFields,
    #[error("{0:?} is the entry's blind; every redaction replaces it already")]
    BlindPointer(String),
    #[error(
        "{0:?} is a number that is not a 64-bit integer; governance data \
         never contains one"
    )]
    NonIntegerNumber(String),
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
///
/// The entry's [`Blind`] is replaced by `blind` — a fresh one, not a
/// marker. Destroying the old value is what stops a removed value being
/// confirmed against the entry's original `data_hash` (see [`blind_data`]);
/// leaving a *new* one is what protects the next redaction of the same
/// entry, whose removed values could otherwise be tested against this
/// one's public `resulting_data_hash`. An entry that predates blinding
/// gains one here. `blind` must be [random](Blind::random) outside tests.
pub fn redact_data(
    data: &serde_json::Value,
    fields: &[String],
    amendment_id: &GovernanceLogId,
    blind: Blind,
) -> Result<serde_json::Value, RedactError> {
    if fields.is_empty() {
        return Err(RedactError::NoFields);
    }
    if let Some(pointer) = non_integer_number(data) {
        return Err(RedactError::NonIntegerNumber(pointer));
    }
    let blind_pointer = format!("/{BLIND_KEY}");
    let marker = serde_json::Value::String(redaction_marker(amendment_id));
    let mut out = data.clone();
    for pointer in fields {
        if pointer.is_empty() {
            return Err(RedactError::WholeEntry);
        }
        if *pointer == blind_pointer {
            return Err(RedactError::BlindPointer(pointer.clone()));
        }
        let slot = out
            .pointer_mut(pointer)
            .ok_or_else(|| RedactError::Unresolved(pointer.clone()))?;
        *slot = marker.clone();
    }
    // Every entry a writer has produced is an object. Anything else has
    // nowhere to keep a blind, and staying redactable matters more.
    if let Some(object) = out.as_object_mut() {
        object.insert(BLIND_KEY.to_string(), blind.to_hex().into());
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
    /// A text no longer `beside` the entry reads [`WITHHELD_TEXT`]
    pub fn new(
        id: GovernanceLogId,
        created_at: DateTime<Utc>,
        amendment: &Amendment,
        beside: Option<&AmendmentTexts>,
    ) -> Self {
        let beside = beside.cloned().unwrap_or_default();
        Self {
            id,
            kind: amendment.kind,
            authority: amendment.authority.clone(),
            basis: amendment.basis.resolve(beside.basis.as_ref()).into(),
            note: amendment.note.resolve(beside.note.as_ref()).into(),
            rationale: amendment
                .rationale
                .as_ref()
                .map(|r| r.resolve(beside.rationale.as_ref()).into()),
            created_at,
        }
    }
}

// ---------------------------------------------------------------------------
// Key rotation
// ---------------------------------------------------------------------------

/// The [`KeyRotation`] payload version this module produces and verifies
pub const KEY_ROTATION_VERSION: u32 = 2;

/// The key the chain started under, as this build of agentkit knows it.
///
/// Frozen. It predates the root keys, so until the chain's first rotation
/// carries its retroactive [`KeyCertificate`] this list is the only
/// second channel a verifier has for it; every later key is certified by
/// [`ROOT_KEYS`] instead and never appears here.
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
    // rotation.
    Compromise,
}

/// The last entry the compromised key is trusted for
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(deny_unknown_fields)]
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
    #[error("a compromise certificate must name last_trusted")]
    MissingLastTrusted,
    #[error("certificate: {0}")]
    Certificate(#[from] CertificateError),
    #[error("outgoing_certificate: {0}")]
    OutgoingCertificate(CertificateError),
    #[error(
        "the chain's first rotation must carry the genesis key's \
         outgoing_certificate"
    )]
    MissingGenesisCertificate,
    #[error(
        "outgoing_certificate belongs on the chain's first rotation and \
         nowhere else"
    )]
    UnexpectedOutgoingCertificate,
    #[error("old_key is not the key that was in force")]
    WrongOldKey,
    #[error("last_trusted does not name an earlier entry of this chain")]
    UnknownLastTrusted,
    #[error("new_key has already held this chain; a key is never brought back")]
    ReusedKey,
    #[error("last_trusted names an entry an earlier compromise repudiated")]
    RepudiatedLastTrusted,
}

/// The `data` of a `key_rotation` entry.
///
/// Build one with [`routine`](Self::routine) or
/// [`compromise`](Self::compromise): both compute the proof of possession.
/// The `certificate` is what authenticates the change; the proof only
/// shows the certified key is one somebody holds.
///
/// No free text, and unknown fields are refused: a rotation can never be
/// redacted, so it carries nothing anyone could need erased. Narrative
/// belongs in a separate, redactable entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(deny_unknown_fields)]
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
    /// The root's word that `new_key` holds the chain from here. For a
    /// compromise its statement also names the last entry trusted under
    /// `old_key`.
    pub certificate: KeyCertificate,
    /// The [`CertPurpose::Genesis`] certificate for the key the chain
    /// started under, which predates the root: on the chain's first
    /// rotation, and only there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outgoing_certificate: Option<KeyCertificate>,
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
    /// verify under the new one. `prev_hash` is the rotation entry's own,
    /// and `certificate` is over [`KeyCertStatement::routine`] at it.
    pub fn routine(
        old_key: PublicKeyHex,
        new_signing_key: &SigningKey,
        prev_hash: Option<Sha256Hex>,
        now: DateTime<Utc>,
        certificate: KeyCertificate,
    ) -> Self {
        Self::build(
            RotationReason::Routine,
            old_key,
            new_signing_key,
            prev_hash,
            now,
            certificate,
        )
    }

    /// A declaration that `old_key` is compromised, trusted only through
    /// the `last_trusted` its `certificate` names.
    ///
    /// The entry is signed by the **new** key — the old one proves nothing
    /// any more. `last_trusted` must name an entry from before any earlier
    /// compromise window; a reattestation inside one restores the entry,
    /// not the ability to anchor trust there.
    pub fn compromise(
        old_key: PublicKeyHex,
        new_signing_key: &SigningKey,
        prev_hash: Option<Sha256Hex>,
        now: DateTime<Utc>,
        certificate: KeyCertificate,
    ) -> Self {
        Self::build(
            RotationReason::Compromise,
            old_key,
            new_signing_key,
            prev_hash,
            now,
            certificate,
        )
    }

    fn build(
        reason: RotationReason,
        old_key: PublicKeyHex,
        new_signing_key: &SigningKey,
        prev_hash: Option<Sha256Hex>,
        now: DateTime<Utc>,
        certificate: KeyCertificate,
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
            certificate,
            outgoing_certificate: None,
        }
    }

    /// This rotation, carrying the genesis key's retroactive certificate
    pub fn with_outgoing(mut self, certificate: KeyCertificate) -> Self {
        self.outgoing_certificate = Some(certificate);
        self
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

    /// Compromise only: the last entry trusted under `old_key`, as the
    /// root certified it
    pub fn last_trusted(&self) -> Option<&TrustedHead> {
        self.certificate.statement.last_trusted.as_ref()
    }

    /// What `certificate` must say for this rotation, appended at `seq`
    /// with `prev_hash`, to be authentic.
    ///
    /// Derived from the chain. Only `last_trusted` is taken from the
    /// certificate, because only the root can say it.
    pub fn expected_statement(
        &self,
        seq: u64,
        prev_hash: Option<Sha256Hex>,
    ) -> Result<KeyCertStatement, RotationError> {
        match self.reason {
            RotationReason::Routine => {
                Ok(KeyCertStatement::routine(self.new_key, seq, prev_hash))
            }
            RotationReason::Compromise => Ok(KeyCertStatement::compromise(
                self.new_key,
                seq,
                prev_hash,
                self.last_trusted()
                    .cloned()
                    .ok_or(RotationError::MissingLastTrusted)?,
            )),
        }
    }

    /// Version and the proof of possession at the position `prev_hash`
    /// names
    pub fn verify_proof(
        &self,
        prev_hash: Option<Sha256Hex>,
    ) -> Result<(), RotationError> {
        if self.agora_governance_key_rotation != KEY_ROTATION_VERSION {
            return Err(RotationError::UnsupportedVersion(
                self.agora_governance_key_rotation,
            ));
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

    /// [`verify_proof`](Self::verify_proof), and `certificate` is the
    /// root's for this key at this position — everything checkable
    /// without the rest of the chain
    pub fn verify_certified(
        &self,
        seq: u64,
        prev_hash: Option<Sha256Hex>,
        roots: &RootSet,
    ) -> Result<(), RotationError> {
        self.verify_proof(prev_hash)?;
        let expected = self.expected_statement(seq, prev_hash)?;
        Ok(self.certificate.verify_for(&expected, roots)?)
    }
}

/// The genesis keys a verifier trusts out of band.
///
/// Only the key the chain started under needs one: every later key is
/// certified by the [`RootSet`]. [`published`](Self::published) is this
/// build's [`PUBLISHED_KEYS`]; [`pinned`](Self::pinned) is the key a
/// client saw first and kept.
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
    /// A [`KeyCertificate`] from the root vouches for this key. `false`
    /// only for a genesis key whose retroactive certificate the chain does
    /// not carry yet.
    #[serde(default)]
    pub certified: bool,
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
    /// position, or for a compromise declaration the certified new key
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
    /// A version 2 amendment's texts: each beside the entry and matching
    /// what it committed to, or withheld. A text that does not match is a
    /// `problem`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub texts: Option<AmendmentTextStatus>,
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
    /// The genesis key, when neither this verifier's [`KeyAnchor`] nor a
    /// [`CertPurpose::Genesis`] certificate in the chain vouches for it.
    /// Not a failure, but a reference client says so loudly. Never a later
    /// key: those are certified or they do not hold the chain at all.
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
        let Some(entry) = self.entries.iter_mut().find(|e| e.id == link.id)
        else {
            return false;
        };
        // Never hashed: see `non_integer_number`.
        let hash = non_integer_number(data).is_none().then(|| data_hash(data));
        let ok = hash.is_some_and(|hash| {
            hash == link.attestation.data_hash
                || entry.redacted_data_hash == Some(hash)
        });
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

/// Whether `read`, serialized again, has the shape `written` had: an
/// object wherever it has one, an array of the same length wherever it
/// has one. Missing and `null` are the same thing.
///
/// serde's derived structs also read positionally from an array, so
/// `[]` is a perfectly good struct of optional fields and `[2, "routine",
/// …]` a perfectly good rotation. No other implementation would agree,
/// and two verifiers that disagree about what is well-formed can be shown
/// two different chains.
fn same_shape(written: &serde_json::Value, read: &serde_json::Value) -> bool {
    use serde_json::Value::{Array, Null, Object};
    match (written, read) {
        (Object(w), Object(r)) => r.iter().all(|(k, r)| match w.get(k) {
            Some(w) => same_shape(w, r),
            None => r.is_null(),
        }),
        (Array(w), Array(r)) => {
            w.len() == r.len() && w.iter().zip(r).all(|(w, r)| same_shape(w, r))
        }
        (_, Object(_) | Array(_)) => false,
        (Object(_) | Array(_), Null) => false,
        _ => true,
    }
}

/// `T` from the JSON it was written as, held to [`same_shape`]
fn read_strictly<T>(written: &serde_json::Value) -> Result<T, String>
where
    T: Serialize + serde::de::DeserializeOwned,
{
    let read: T =
        serde_json::from_value(written.clone()).map_err(|e| e.to_string())?;
    let again = serde_json::to_value(&read).map_err(|e| e.to_string())?;
    if same_shape(written, &again) {
        Ok(read)
    } else {
        Err("an array where an object belongs, or the reverse".into())
    }
}

/// A chain from the JSON it was served as, held to [`same_shape`]: a
/// link is an object, and so is everything in it that should be.
///
/// Prefer this to deserializing [`GovernanceChainLink`]s directly, which
/// also accepts a link written as an array of its fields. Nothing serves
/// one; a verifier that would read it agrees with no other.
pub fn links_from_json(
    chain: &serde_json::Value,
) -> Result<Vec<GovernanceChainLink>, String> {
    chain
        .as_array()
        .ok_or("a chain is an array of links")?
        .iter()
        .map(read_strictly)
        .collect()
}

/// [`read_strictly`] for a field that is outside any signed `data`
fn read_as_written<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Serialize + serde::de::DeserializeOwned,
{
    let written = serde_json::Value::deserialize(deserializer)?;
    if written.is_null() {
        return Ok(None);
    }
    read_strictly(&written)
        .map(Some)
        .map_err(serde::de::Error::custom)
}

/// The id series reserved for one entry type, if it has one. The other
/// types share `GOV-` and `APP-`, which the verifier does not tell apart.
fn reserved_prefix(
    entry_type: GovernanceLogEntryType,
) -> Option<GovernanceLogPrefix> {
    match entry_type {
        GovernanceLogEntryType::Amendment => Some(GovernanceLogPrefix::Amd),
        GovernanceLogEntryType::KeyRotation => Some(GovernanceLogPrefix::Key),
        GovernanceLogEntryType::StewardRecord => Some(GovernanceLogPrefix::Rec),
        GovernanceLogEntryType::CouncilDecision
        | GovernanceLogEntryType::AppealsCourtDecision
        | GovernanceLogEntryType::EmergencyAction
        | GovernanceLogEntryType::PolicyChange
        | GovernanceLogEntryType::StewardVeto => None,
    }
}

/// The entry type a reserved id series belongs to, if it is reserved
fn reserved_for(prefix: GovernanceLogPrefix) -> Option<GovernanceLogEntryType> {
    match prefix {
        GovernanceLogPrefix::Amd => Some(GovernanceLogEntryType::Amendment),
        GovernanceLogPrefix::Key => Some(GovernanceLogEntryType::KeyRotation),
        GovernanceLogPrefix::Rec => Some(GovernanceLogEntryType::StewardRecord),
        GovernanceLogPrefix::Gov | GovernanceLogPrefix::App => None,
    }
}

/// The id series an entry type must use, and must not
fn prefix_problem(link: &GovernanceChainLink) -> Option<String> {
    let prefix = link.id.prefix();
    match (reserved_prefix(link.entry_type), reserved_for(prefix)) {
        (Some(want), _) if prefix != want => Some(format!(
            "the id of a {} entry must be in the {want}- series, not {}",
            link.entry_type, link.id
        )),
        (None, Some(owner)) => Some(format!(
            "{prefix}- ids are reserved for {owner} entries, but {} is a {}",
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
    genesis: PublicKeyHex,
    /// A rotation has carried the genesis key's certificate
    genesis_certified: bool,
}

impl KeyWalk {
    fn new(genesis: &VerifyingKey, anchor: &KeyAnchor) -> Self {
        let public_key = PublicKeyHex::from(genesis);
        Self {
            genesis: public_key,
            genesis_certified: false,
            history: vec![(
                GovernanceKeyRecord {
                    public_key,
                    from_seq: 1,
                    through_seq: None,
                    status: KeyStatus::Active,
                    introduced_by: None,
                    retired_by: None,
                    certified: false,
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
                certified: true,
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
        roots: &RootSet,
    ) -> Result<(), RotationError> {
        rotation.verify_certified(seq, link.attestation.prev_hash, roots)?;
        let new_key = rotation
            .new_key
            .to_verifying_key()
            .map_err(|_| RotationError::BadNewKey)?;
        if self.seen.contains(&rotation.new_key) {
            return Err(RotationError::ReusedKey);
        }
        // The genesis key predates the root, so the first rotation brings
        // its certificate along. Whether that rotation is later voided by
        // a compromise does not matter: the certificate is the root's
        // statement, not the entry's.
        match (&rotation.outgoing_certificate, self.genesis_certified) {
            (None, false) => {
                return Err(RotationError::MissingGenesisCertificate);
            }
            (Some(_), true) => {
                return Err(RotationError::UnexpectedOutgoingCertificate);
            }
            (Some(certificate), false) => certificate
                .verify_for(&KeyCertStatement::genesis(self.genesis), roots)
                .map_err(RotationError::OutgoingCertificate)?,
            (None, true) => {}
        }
        match rotation.reason {
            RotationReason::Routine => {
                if rotation.old_key != self.in_force(seq).0 {
                    return Err(RotationError::WrongOldKey);
                }
                self.close(seq, KeyStatus::Retired, &link.id);
                self.open(rotation.new_key, new_key, seq + 1, &link.id);
            }
            RotationReason::Compromise => {
                let head = rotation
                    .last_trusted()
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
                // The key in force at the last trusted entry: a rotation
                // inside the window is void with the rest of it.
                if rotation.old_key != self.in_force(trusted_seq).0 {
                    return Err(RotationError::WrongOldKey);
                }
                self.history.retain(|(r, _)| r.from_seq <= trusted_seq);
                self.close(trusted_seq, KeyStatus::Compromised, &link.id);
                self.open(rotation.new_key, new_key, seq, &link.id);
                self.repudiated.extend(trusted_seq + 1..seq);
            }
        }
        self.seen.insert(rotation.new_key);
        if !self.genesis_certified {
            self.genesis_certified = true;
            self.history[0].0.certified = true;
            self.unanchored.clear();
        }
        Ok(())
    }
}

/// Verify a whole chain from `genesis_key`, following the rotations that
/// `roots` certified and no others.
///
/// Links are sorted by `chain_seq` first, so the caller's order does not
/// matter. `retroactive` and `out_of_order` are recomputed from the
/// timestamps, not copied. `content_matches` is filled only for links that
/// carry `data` — for the rest, see
/// [`check_content`](GovernanceVerification::check_content).
///
/// `genesis_key` is the key the chain started under; it is not in the
/// chain, so a verifier has to be told. Until the chain's first rotation
/// certifies it, `anchor` is what vouches for it: if it is not there it
/// is reported in `unanchored_keys` rather than rejected — a client
/// pinning what it saw first passes `KeyAnchor::pinned(key)` and gets a
/// clean report. Pass [`RootSet::published`] for `roots` outside tests.
///
/// A rotation is authentic iff its [`KeyCertificate`] is valid for the
/// new key at that position; who signed the entry only follows from which
/// key *can* (the old one for a routine rotation, the new one once the
/// old is compromised). So a thief holding the online key can append
/// entries — which a compromise declaration then repudiates — but can
/// never move the chain.
///
/// The rules the report records that no type states on its own: an id
/// belongs to its entry type's series and appears once (`AMD-`, `KEY-`
/// and `REC-` are each reserved for one type); only an entry whose
/// own signature and linkage verify amends anything or moves the key, so
/// a forged entry cannot also describe the chain; and an amendment inside
/// a repudiated window has no effect unless a [`AmendmentKind::Reattested`]
/// vouches for its own entry first, resolved to a fixpoint.
pub fn verify_chain(
    links: &[GovernanceChainLink],
    genesis_key: &VerifyingKey,
    anchor: &KeyAnchor,
    roots: &RootSet,
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
            Some(data) if non_integer_number(data).is_some() => {
                content_matches = Some(false);
                problems.push(format!(
                    "`data` has a number that is not a 64-bit integer at {:?}; \
                     governance data never contains one",
                    non_integer_number(data).unwrap_or_default()
                ));
            }
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
                            match read_strictly(data) {
                                Ok(v) => amendment = Some(v),
                                Err(e) => problems.push(format!(
                                    "amendment `data` is malformed: {e}"
                                )),
                            }
                        }
                        GovernanceLogEntryType::KeyRotation => {
                            match read_strictly(data) {
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
        // taken at its word only if the root certified that key here.
        // Everything else is signed by the key in force.
        let declared = rotation
            .as_ref()
            .filter(|r| r.reason == RotationReason::Compromise)
            .map(|r| {
                if walk.seen.contains(&r.new_key) {
                    return Err(RotationError::ReusedKey);
                }
                r.verify_certified(expected_seq, a.prev_hash, roots)?;
                r.new_key
                    .to_verifying_key()
                    .map_err(|_| RotationError::BadNewKey)
            });
        let (key_hex, key) = match &declared {
            Some(Ok(k)) => (PublicKeyHex::from(k), *k),
            _ => walk.in_force(expected_seq),
        };
        // Say why a declaration was not taken at its word; the bad
        // signature that follows is the consequence, not the cause.
        if let Some(Err(e)) = &declared {
            problems.push(e.to_string());
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
                walk.apply(rotation, link, expected_seq, &links, roots)
        {
            let problem = e.to_string();
            if !problems.contains(&problem) {
                problems.push(problem);
            }
        }
        // Texts are checked against what the entry signed whether or not
        // the amendment takes effect: a substituted text is a lie about
        // the record either way.
        let mut texts = None;
        if let Some(amendment) = amendment
            .as_ref()
            .filter(|a| authentic && a.validate().is_ok())
        {
            match amendment.text_status(link.texts.as_ref()) {
                Ok(status) => texts = status,
                Err(e) => problems.push(e.to_string()),
            }
        } else if link.texts.as_ref().is_some_and(|t| !t.is_empty()) {
            problems.push(AmendmentError::UncommittedText.to_string());
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
            texts,
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
            && non_integer_number(data).is_none()
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

    pub(super) fn rec(n: u32) -> GovernanceLogId {
        format!("REC-2026-{n:04}").parse().unwrap()
    }

    pub(super) fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + secs, 123_456_789).unwrap()
    }

    /// The anchor a client that pinned the key it first saw would hold
    fn anchored(key: &VerifyingKey) -> KeyAnchor {
        KeyAnchor::pinned(key.into())
    }

    /// `draft` under salts fixed by its texts and `seq`, so that a chain
    /// built twice is the same bytes twice
    pub(super) fn resalted(draft: &AmendmentDraft, seq: u64) -> AmendmentDraft {
        let fix = |field: &str, t: &Option<CommittedText>| {
            t.as_ref().map(|t| {
                let salt = Sha256::digest(format!("{seq}/{field}/{}", t.text));
                CommittedText::with_salt(
                    TextSalt::from(<[u8; 32]>::from(salt)),
                    t.text.clone(),
                )
            })
        };
        let texts = AmendmentTexts {
            basis: fix("basis", &draft.texts.basis),
            note: fix("note", &draft.texts.note),
            rationale: fix("rationale", &draft.texts.rationale),
        };
        let commit = |t: &Option<CommittedText>| {
            t.as_ref().map(|t| AmendmentText::Committed(t.commitment()))
        };
        AmendmentDraft {
            amendment: Amendment {
                basis: commit(&texts.basis).unwrap(),
                note: commit(&texts.note).unwrap(),
                rationale: commit(&texts.rationale),
                ..draft.amendment.clone()
            },
            texts,
        }
    }

    /// `draft` as version 1 wrote it: the texts in the signed `data`
    pub(super) fn v1(draft: AmendmentDraft) -> Amendment {
        let plain =
            |t: Option<CommittedText>| t.map(|t| AmendmentText::Plain(t.text));
        Amendment {
            agora_governance_amendment: 1,
            basis: plain(draft.texts.basis).unwrap(),
            note: plain(draft.texts.note).unwrap(),
            rationale: plain(draft.texts.rationale),
            ..draft.amendment
        }
    }

    /// A throwaway root key. Fixed, so the vectors are byte-stable; the
    /// real ones live on hardware and sign nothing in a test.
    pub(super) fn root(n: u8) -> SigningKey {
        SigningKey::from_bytes(&[0xA0 + n; 32])
    }

    /// `root(1)` and `root(2)`, either of which suffices
    pub(super) fn roots() -> RootSet {
        RootSet::new(
            [1, 2].map(|n| PublicKeyHex::from(&root(n).verifying_key())),
            1,
        )
    }

    /// `statement`, signed by each of `signers` as a root would
    pub(super) fn certify(
        signers: &[&SigningKey],
        statement: KeyCertStatement,
    ) -> KeyCertificate {
        use ed25519_dalek::Signer;
        let message = statement.signed_bytes();
        signers.iter().fold(
            KeyCertificate::unsigned(statement),
            |certificate, signer| {
                certificate.with(RootSignature {
                    root_key: (&signer.verifying_key()).into(),
                    signature: signer.sign(&message).into(),
                })
            },
        )
    }

    /// `root(1)`'s routine certificate for `key` at `c`'s next position
    fn for_new_at(c: &Chain, key: &VerifyingKey) -> KeyCertificate {
        certify(
            &[&root(1)],
            KeyCertStatement::routine(key.into(), c.next_seq(), c.prev_hash()),
        )
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
            texts: None,
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
        /// Whoever signed the first entry
        genesis: Option<PublicKeyHex>,
    }

    impl Chain {
        pub(super) fn new() -> Self {
            Self {
                links: Vec::new(),
                gov: 0,
                amd: 0,
                key: 0,
                genesis: None,
            }
        }

        pub(super) fn prev_hash(&self) -> Option<Sha256Hex> {
            self.links.last().map(|l| l.attestation.entry_hash)
        }

        /// The `entry_hash` of the 1-indexed link `seq`
        pub(super) fn hash_at(&self, seq: usize) -> Sha256Hex {
            self.links[seq - 1].attestation.entry_hash
        }

        /// The `chain_seq` the next entry gets
        pub(super) fn next_seq(&self) -> u64 {
            self.links.len() as u64 + 1
        }

        /// The 1-indexed link `seq`, as a compromise names it
        pub(super) fn head(&self, seq: usize) -> TrustedHead {
            TrustedHead {
                id: self.links[seq - 1].id.clone(),
                chain_seq: seq as u64,
                entry_hash: self.hash_at(seq),
            }
        }

        /// The genesis certificate, if the next rotation is the first
        fn outgoing(&self, rotation: KeyRotation) -> KeyRotation {
            match (self.key, self.genesis) {
                (0, Some(genesis)) => rotation.with_outgoing(certify(
                    &[&root(1)],
                    KeyCertStatement::genesis(genesis),
                )),
                _ => rotation,
            }
        }

        /// A routine rotation to `new` at the next position, certified by
        /// `root(1)`
        pub(super) fn routine(
            &self,
            old: &VerifyingKey,
            new: &SigningKey,
        ) -> KeyRotation {
            let statement = KeyCertStatement::routine(
                (&new.verifying_key()).into(),
                self.next_seq(),
                self.prev_hash(),
            );
            self.outgoing(KeyRotation::routine(
                old.into(),
                new,
                self.prev_hash(),
                at(self.next_seq() as i64 * 10 + 5),
                certify(&[&root(1)], statement),
            ))
        }

        /// A compromise declaration at the next position trusting `old`
        /// through the 1-indexed link `trusted`, certified by `root(1)`
        pub(super) fn compromise(
            &self,
            old: &VerifyingKey,
            new: &SigningKey,
            trusted: usize,
        ) -> KeyRotation {
            let statement = KeyCertStatement::compromise(
                (&new.verifying_key()).into(),
                self.next_seq(),
                self.prev_hash(),
                self.head(trusted),
            );
            self.outgoing(KeyRotation::compromise(
                old.into(),
                new,
                self.prev_hash(),
                at(self.next_seq() as i64 * 10 + 5),
                certify(&[&root(1)], statement),
            ))
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
            self.genesis
                .get_or_insert_with(|| (&signer.verifying_key()).into());
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
                texts: None,
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

        /// `draft`'s amendment as the entry's `data`, its texts beside it
        pub(super) fn amend(
            &mut self,
            signer: &SigningKey,
            draft: &AmendmentDraft,
        ) -> GovernanceLogId {
            let draft = resalted(draft, self.next_seq());
            let id = self.amend_v1(signer, &draft.amendment);
            let link = self.links.last_mut().unwrap();
            link.texts = Some(draft.texts);
            id
        }

        /// An amendment with nothing beside it: version 1, or a version 2
        /// whose texts have all been withheld
        pub(super) fn amend_v1(
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
        let v = verify_chain(&c, &pk, &anchored(&pk), &roots());
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
        let v = verify_chain(&[], &pk, &anchored(&pk), &roots());
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
        let v = verify_chain(&c, &pk, &anchored(&pk), &roots());
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
        let v = verify_chain(&c, &pk, &anchored(&pk), &roots());
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
        let v = verify_chain(&c, &pk, &anchored(&pk), &roots());
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
            texts: None,
        };
        second.attestation.retroactive = true; // a lying flag on the wire
        let v = verify_chain(&[first, second], &pk, &anchored(&pk), &roots());
        assert!(v.ok, "{v:#?}");
        assert!(v.entries[0].retroactive);
        assert!(!v.entries[1].retroactive, "recomputed from timestamps");
        assert!(v.entries[1].out_of_order);
    }

    #[test]
    fn content_mismatch_settles_to_not_ok() {
        let (key, pk) = generate_keypair();
        let c = chain(&key, 1);
        let mut v = verify_chain(&c, &pk, &anchored(&pk), &roots());
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
        let amendment = AmendmentDraft::new(
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

        let v = verify_chain(&c.links, &pk, &anchored(&pk), &roots());
        assert!(v.ok, "{v:#?}");
        assert_eq!(v.entries[0].amended_by, vec![id]);
        assert!(v.entries[1].amended_by.is_empty());
        assert!(!v.entries[0].redacted);
        assert_eq!(v.head, Some(amd(1)));
        // The amendment link carries `data`, so its own content is checked.
        assert_eq!(v.entries[2].content_matches, Some(true));
        assert_eq!(v.entries[0].content_matches, None);
        assert_eq!(
            standing([amendment.amendment.kind]),
            Standing::NonPrecedential
        );
    }

    #[test]
    fn an_amendment_must_name_an_earlier_entry_by_its_exact_hash() {
        let (key, pk) = generate_keypair();
        let problem = |c: &Chain, at: usize| -> String {
            verify_chain(&c.links, &pk, &anchored(&pk), &roots()).entries[at]
                .problem
                .clone()
                .unwrap_or_default()
        };

        let mut c = Chain::new();
        c.decision(&key);
        let unknown = AmendmentDraft::new(
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
        assert!(!verify_chain(&c.links, &pk, &anchored(&pk), &roots()).ok);

        // Names an entry that does not exist yet.
        let mut c = Chain::new();
        c.decision(&key);
        let forward = AmendmentDraft::new(
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
        let wrong_hash = AmendmentDraft::new(
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
            verify_chain(&c.links, &pk, &anchored(&pk), &roots()).entries[0]
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
            let mut amendment = AmendmentDraft::new(
                target,
                c.hash_at(1),
                AmendmentKind::Correction,
                "b",
                "n",
            )
            .unwrap();
            mutate(&mut amendment.amendment);
            c.amend_v1(&key, &amendment.amendment);
            let v = verify_chain(&c.links, &pk, &anchored(&pk), &roots());
            assert!(!v.ok, "{v:#?}");
            v.entries[1].problem.clone().unwrap_or_default()
        };

        // `redaction` is the only way to get a well-formed one, so a
        // redaction kind without a `Redaction` can only be hand-built.
        assert_eq!(
            AmendmentDraft::new(
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

        let p = amend_with(&|a| a.agora_governance_amendment = 3);
        assert!(p.contains("agora_governance_amendment is 3"), "{p}");

        // A version says where the texts are, and both ways round it is
        // held to it.
        let p = amend_with(&|a| a.agora_governance_amendment = 1);
        assert!(p.contains("does neither consistently"), "{p}");
        let p = amend_with(&|a| a.note = AmendmentText::Plain("n".into()));
        assert!(p.contains("does neither consistently"), "{p}");
    }

    #[test]
    fn governance_data_holds_no_number_that_is_not_a_64_bit_integer() {
        let fine = json!({"a": [1, -2, u64::MAX, i64::MIN], "b": {"c": "0.5"}});
        assert_eq!(non_integer_number(&fine), None);
        assert!(blind_data(&fine, Blind::random()).is_ok());

        for (text, pointer) in [
            (r#"{"a": {"b/c": [1, 0.5]}}"#, "/a/b~1c/1"),
            (r#"{"n": 1.0}"#, "/n"),
            (r#"{"n": 1e3}"#, "/n"),
            (r#"{"n": 18446744073709551616}"#, "/n"),
            (r#"{"n": -9223372036854775809}"#, "/n"),
        ] {
            let data: serde_json::Value = serde_json::from_str(text).unwrap();
            assert_eq!(non_integer_number(&data).as_deref(), Some(pointer));
            assert_eq!(
                blind_data(&data, Blind::random()),
                Err(BlindError::NonIntegerNumber(pointer.into())),
                "the writer refuses it"
            );
            assert_eq!(
                redact_data(&data, &["/n".into()], &amd(1), Blind::random()),
                Err(RedactError::NonIntegerNumber(pointer.into()))
            );
        }
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
        let (amendment, redacted) = AmendmentDraft::redaction(
            &amendment_id,
            target,
            c.hash_at(1),
            "GDPR Art. 17(1)(a)",
            "personal data removed on request",
            vec!["/subject/handle".into(), "/subject/detail".into()],
            &data,
            Blind::from([7; 32]),
        )
        .unwrap();
        assert_eq!(c.amend(&key, &amendment), amendment_id);
        assert_eq!(redacted[BLIND_KEY], json!(Blind::from([7; 32])));

        let mut v = verify_chain(&c.links, &pk, &anchored(&pk), &roots());
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
        let mut v = verify_chain(&c.links, &pk, &anchored(&pk), &roots());
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
        let amendment = AmendmentDraft::new(
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

        let v = verify_chain(&c.links, &pk, &anchored(&pk), &roots());
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
        let amendment = AmendmentDraft::new(
            target,
            c.hash_at(1),
            AmendmentKind::Correction,
            "b",
            "n",
        )
        .unwrap();
        c.amend(&key, &amendment);
        let rotation = c.routine(&pk, &generate_keypair().0);
        c.rotate(&key, &rotation);
        c.links[1].data = None;
        c.links[2].data = None;

        let v = verify_chain(&c.links, &pk, &anchored(&pk), &roots());
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
        let amendment = AmendmentDraft::new(
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
            serde_json::to_value(&amendment.amendment).unwrap(),
            true,
        );
        let v = verify_chain(&c.links, &pk, &anchored(&pk), &roots());
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
        let v = verify_chain(&c.links, &pk, &anchored(&pk), &roots());
        let p = v.entries[0].problem.as_deref().unwrap();
        assert!(p.contains("reserved"), "{p}");
    }

    #[test]
    fn redact_data_replaces_whole_values_and_refuses_the_rest() {
        let id = amd(3);
        let blind = Blind::from([9; 32]);
        let data = json!({"a": {"b": [1, {"c": "secret"}]}, "d/e": "slash"});
        let out = redact_data(
            &data,
            &["/a/b/1/c".into(), "/d~1e".into()],
            &id,
            blind,
        )
        .unwrap();
        assert_eq!(out["a"]["b"][1]["c"], json!(redaction_marker(&id)));
        assert_eq!(out["d/e"], json!(redaction_marker(&id)));
        assert_eq!(out["a"]["b"][0], json!(1), "untouched");
        assert_eq!(out[BLIND_KEY], json!(blind), "a legacy entry gains one");

        assert_eq!(
            redact_data(&data, &["/a/nope".into()], &id, blind),
            Err(RedactError::Unresolved("/a/nope".into()))
        );
        assert_eq!(
            redact_data(&data, &["".into()], &id, blind),
            Err(RedactError::WholeEntry)
        );
        assert_eq!(
            redact_data(&data, &[], &id, blind),
            Err(RedactError::NoFields)
        );
        assert_eq!(
            redact_data(&data, &["/_blind".into()], &id, blind),
            Err(RedactError::BlindPointer("/_blind".into()))
        );
    }

    #[test]
    fn blind_data_is_for_objects_and_is_the_writers_to_supply() {
        let blind = Blind::from([1; 32]);
        let out = blind_data(&json!({"finding": "upheld"}), blind).unwrap();
        assert_eq!(out, json!({"finding": "upheld", "_blind": blind}));
        assert_eq!(
            blind_data(&json!([1]), blind),
            Err(BlindError::NotAnObject)
        );
        assert_eq!(blind_data(&out, blind), Err(BlindError::AlreadyBlinded));
        assert_ne!(Blind::random(), Blind::random());

        use GovernanceLogEntryType::*;
        assert!(!is_redactable(Amendment) && !is_redactable(KeyRotation));
        assert!(
            is_redactable(CouncilDecision) && is_redactable(EmergencyAction)
        );
    }

    /// The attack blinding exists for, performed. Everything the attacker
    /// uses is public after a redaction: the redacted data, the entry's
    /// original `data_hash`, and each redaction's `resulting_data_hash`.
    #[test]
    fn a_removed_value_cannot_be_confirmed_by_guessing_it() {
        // Put a guess back where a marker is and see if a public hash agrees
        fn confirms(
            public: &serde_json::Value,
            pointer: &str,
            guess: &str,
            hash: Sha256Hex,
        ) -> bool {
            let mut attempt = public.clone();
            *attempt.pointer_mut(pointer).unwrap() = json!(guess);
            data_hash(&attempt) == hash
        }
        let legacy = json!({"finding": "upheld", "handle": "someone", "city": "Utrecht"});

        // Blinded when written: the right guess confirms nothing.
        let written = blind_data(&legacy, Blind::random()).unwrap();
        let first = redact_data(
            &written,
            &["/handle".into()],
            &amd(1),
            Blind::random(),
        )
        .unwrap();
        assert!(!confirms(&first, "/handle", "someone", data_hash(&written)));

        // A second redaction of the same entry: the first one's
        // `resulting_data_hash` is public too, and covers the city.
        let second =
            redact_data(&first, &["/city".into()], &amd(2), Blind::random())
                .unwrap();
        assert!(!confirms(&second, "/city", "Utrecht", data_hash(&first)));

        // An entry from before blinding has nothing to destroy, and gains a
        // key it never had: strip it and the right guess does confirm. This
        // is the residual exposure of the entries that predate 0.27...
        let mut stripped =
            redact_data(&legacy, &["/handle".into()], &amd(3), Blind::random())
                .unwrap();
        let legacy_first = stripped.clone();
        stripped.as_object_mut().unwrap().remove(BLIND_KEY);
        assert!(confirms(
            &stripped,
            "/handle",
            "someone",
            data_hash(&legacy)
        ));
        // ...and it ends at the first redaction, which left a blind behind.
        let legacy_second = redact_data(
            &legacy_first,
            &["/city".into()],
            &amd(4),
            Blind::random(),
        )
        .unwrap();
        assert!(!confirms(
            &legacy_second,
            "/city",
            "Utrecht",
            data_hash(&legacy_first)
        ));
    }

    // -- key rotation --

    #[test]
    fn a_routine_rotation_moves_the_chain_to_the_new_key() {
        let (old, old_pk) = generate_keypair();
        let (new, new_pk) = generate_keypair();
        let mut c = Chain::new();
        c.decision(&old);
        let rotation = c.routine(&old_pk, &new);
        let rotation_id = c.rotate(&old, &rotation);
        c.decision(&new);

        // Nothing but the root vouches for the new key, and nothing else
        // has to.
        let v = verify_chain(&c.links, &old_pk, &anchored(&old_pk), &roots());
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
        assert!(v.keys[0].certified, "retroactively, by the rotation");
        assert_eq!(v.keys[1].from_seq, 3);
        assert_eq!(v.keys[1].through_seq, None);
        assert_eq!(v.keys[1].status, KeyStatus::Active);
        assert_eq!(v.keys[1].introduced_by.as_ref(), Some(&rotation_id));
        assert!(v.keys[1].certified);
    }

    #[test]
    fn the_old_key_cannot_sign_after_a_routine_rotation() {
        let (old, old_pk) = generate_keypair();
        let (new, _) = generate_keypair();
        let mut c = Chain::new();
        c.decision(&old);
        let rotation = c.routine(&old_pk, &new);
        c.rotate(&old, &rotation);
        c.decision(&old);

        let v = verify_chain(&c.links, &old_pk, &anchored(&old_pk), &roots());
        assert!(!v.ok, "{v:#?}");
        assert!(!v.entries[2].signature_valid);
        assert!(
            v.entries[2].link_valid,
            "the linkage is fine; the key is not"
        );
    }

    /// The scenario the root exists for: the online key alone moves
    /// nothing, however well-formed the rotation it signs.
    #[test]
    fn the_online_key_cannot_certify_its_own_successor() {
        let (old, old_pk) = generate_keypair();
        let (thief, _) = generate_keypair();
        let mut c = Chain::new();
        c.decision(&old);
        let mut rotation = c.routine(&old_pk, &thief);
        // Signed by the stolen online key instead of a root.
        let statement = rotation.certificate.statement.clone();
        rotation.certificate = certify(&[&old], statement);
        c.rotate(&old, &rotation);
        c.decision(&thief);

        let v = verify_chain(&c.links, &old_pk, &anchored(&old_pk), &roots());
        assert!(!v.ok, "{v:#?}");
        let p = v.entries[1].problem.as_deref().unwrap();
        assert!(p.contains("0 valid root signature"), "{p}");
        assert_eq!(v.public_key, (&old_pk).into(), "the chain does not move");
        assert!(!v.entries[2].signature_valid);
        assert_eq!(v.keys.len(), 1);
    }

    #[test]
    fn a_certificate_counts_distinct_known_roots_only() {
        let (old, old_pk) = generate_keypair();
        let (new, _) = generate_keypair();
        let (stranger, _) = generate_keypair();
        let two_of_two = RootSet::new(
            [
                (&root(1).verifying_key()).into(),
                (&root(2).verifying_key()).into(),
            ],
            2,
        );
        let verdict = |signers: &[&SigningKey], roots: &RootSet| {
            let mut c = Chain::new();
            c.decision(&old);
            let mut rotation = c.routine(&old_pk, &new);
            let statement = rotation.certificate.statement.clone();
            rotation.certificate = certify(signers, statement);
            // The genesis certificate is held to the same threshold.
            let genesis = KeyCertStatement::genesis((&old_pk).into());
            rotation.outgoing_certificate =
                Some(certify(&[&root(1), &root(2)], genesis));
            c.rotate(&old, &rotation);
            verify_chain(&c.links, &old_pk, &anchored(&old_pk), roots)
        };

        assert!(verdict(&[&root(1), &root(2)], &two_of_two).ok);
        assert!(verdict(&[&root(2)], &roots()).ok, "either root, 1-of-2");

        for (signers, why) in [
            (vec![&root(1)], "below the threshold"),
            (vec![&root(1), &root(1)], "one root twice is one root"),
            (vec![&root(1), &stranger], "an unknown root is nobody"),
            (vec![], "unsigned"),
        ] {
            let v = verdict(&signers, &two_of_two);
            assert!(!v.ok, "{why}: {v:#?}");
            let p = v.entries[1].problem.as_deref().unwrap();
            assert!(p.contains("where 2 are needed"), "{why}: {p}");
        }

        // An unknown signer beside a sufficient set is not an error.
        assert!(verdict(&[&stranger, &root(1)], &roots()).ok);
    }

    #[test]
    fn a_certificate_is_good_for_one_statement_at_one_position() {
        let (old, old_pk) = generate_keypair();
        let (new, new_pk) = generate_keypair();
        let (other, other_pk) = generate_keypair();
        let anchor = anchored(&old_pk);

        // Certified for the position right after entry 1, appended one
        // entry later. The proof of possession is rebuilt for the new
        // position; the certificate cannot be.
        let mut c = Chain::new();
        c.decision(&old);
        let early = c.routine(&old_pk, &new);
        c.decision(&old);
        let mut rotation = c.routine(&old_pk, &new);
        rotation.certificate = early.certificate;
        c.rotate(&old, &rotation);
        let v = verify_chain(&c.links, &old_pk, &anchor, &roots());
        assert!(!v.ok, "{v:#?}");
        let p = v.entries[2].problem.as_deref().unwrap();
        assert!(
            p.contains("different key, purpose or chain position"),
            "{p}"
        );
        assert_eq!(v.public_key, (&old_pk).into());

        // Certified for one key, presented for another.
        let mut c = Chain::new();
        c.decision(&old);
        let for_new = c.routine(&old_pk, &new);
        let mut rotation = c.routine(&old_pk, &other);
        rotation.certificate = for_new.certificate;
        c.rotate(&old, &rotation);
        let v = verify_chain(&c.links, &old_pk, &anchor, &roots());
        let p = v.entries[1].problem.as_deref().unwrap();
        assert!(
            p.contains("different key, purpose or chain position"),
            "{p}"
        );

        // The statement altered after signing, to match.
        let mut c = Chain::new();
        c.decision(&old);
        let mut rotation = c.routine(&old_pk, &other);
        rotation.certificate = for_new_at(&c, &new_pk);
        rotation.certificate.statement.key = (&other_pk).into();
        c.rotate(&old, &rotation);
        let v = verify_chain(&c.links, &old_pk, &anchor, &roots());
        let p = v.entries[1].problem.as_deref().unwrap();
        assert!(p.contains("0 valid root signature"), "{p}");

        // A routine certificate does not authorize a compromise.
        let mut c = Chain::new();
        c.decision(&old);
        c.decision(&old);
        let mut rotation = c.compromise(&old_pk, &new, 1);
        rotation.certificate = for_new_at(&c, &new_pk);
        c.rotate(&new, &rotation);
        let v = verify_chain(&c.links, &old_pk, &anchor, &roots());
        assert!(!v.ok);
        assert!(v.repudiated.is_empty(), "{v:#?}");
        assert_eq!(v.public_key, (&old_pk).into());
    }

    /// The same signature over the same JSON without the domain prefix —
    /// what a root key tricked into signing "just some JSON" would produce
    #[test]
    fn a_root_signature_without_the_domain_prefix_certifies_nothing() {
        use ed25519_dalek::Signer;
        let (old, old_pk) = generate_keypair();
        let (new, _) = generate_keypair();
        let mut c = Chain::new();
        c.decision(&old);
        let mut rotation = c.routine(&old_pk, &new);
        let statement = rotation.certificate.statement.clone();
        let bare = canonical_json(&serde_json::to_value(&statement).unwrap());
        assert_eq!(
            statement.signed_bytes(),
            [ROOT_DOMAIN, bare.as_slice()].concat()
        );
        rotation.certificate =
            KeyCertificate::unsigned(statement).with(RootSignature {
                root_key: (&root(1).verifying_key()).into(),
                signature: root(1).sign(&bare).into(),
            });
        c.rotate(&old, &rotation);
        let v = verify_chain(&c.links, &old_pk, &anchored(&old_pk), &roots());
        assert!(!v.ok, "{v:#?}");
        assert_eq!(v.public_key, (&old_pk).into());
    }

    #[test]
    fn the_first_rotation_carries_the_genesis_certificate_and_only_it_does() {
        let (k1, k1_pk) = generate_keypair();
        let (k2, k2_pk) = generate_keypair();
        let (k3, _) = generate_keypair();
        let genesis =
            || certify(&[&root(1)], KeyCertStatement::genesis((&k1_pk).into()));

        // Missing.
        let mut c = Chain::new();
        c.decision(&k1);
        let mut rotation = c.routine(&k1_pk, &k2);
        rotation.outgoing_certificate = None;
        c.rotate(&k1, &rotation);
        let v = verify_chain(&c.links, &k1_pk, &anchored(&k1_pk), &roots());
        assert!(!v.ok);
        let p = v.entries[1].problem.as_deref().unwrap();
        assert!(p.contains("genesis key's outgoing_certificate"), "{p}");
        assert_eq!(v.public_key, (&k1_pk).into());

        // For some other key.
        let mut c = Chain::new();
        c.decision(&k1);
        let rotation = c.routine(&k1_pk, &k2).with_outgoing(certify(
            &[&root(1)],
            KeyCertStatement::genesis((&k2_pk).into()),
        ));
        c.rotate(&k1, &rotation);
        let v = verify_chain(&c.links, &k1_pk, &anchored(&k1_pk), &roots());
        let p = v.entries[1].problem.as_deref().unwrap();
        assert!(p.starts_with("outgoing_certificate:"), "{p}");

        // Present, and it is what vouches for a genesis key no anchor
        // knows.
        let mut c = Chain::new();
        c.decision(&k1);
        let rotation = c.routine(&k1_pk, &k2);
        assert_eq!(rotation.outgoing_certificate, Some(genesis()));
        c.rotate(&k1, &rotation);
        let v = verify_chain(&c.links, &k1_pk, &KeyAnchor::default(), &roots());
        assert!(v.ok, "{v:#?}");
        assert!(v.unanchored_keys.is_empty(), "{v:#?}");

        // A second one, later, is refused.
        let again = c.routine(&k2_pk, &k3).with_outgoing(genesis());
        c.rotate(&k2, &again);
        let v = verify_chain(&c.links, &k1_pk, &anchored(&k1_pk), &roots());
        assert!(!v.ok);
        let p = v.entries[2].problem.as_deref().unwrap();
        assert!(p.contains("nowhere else"), "{p}");
    }

    #[test]
    fn a_forged_proof_of_possession_is_rejected() {
        let (old, old_pk) = generate_keypair();
        let (_, new_pk) = generate_keypair();
        let (thief, _) = generate_keypair();
        let mut c = Chain::new();
        c.decision(&old);
        // A rotation to a key nobody holds, certified in good faith: the
        // proof is signed by the old key instead of by `new_key` itself.
        let mut rotation = c.routine(&old_pk, &thief);
        rotation.new_key = (&new_pk).into();
        rotation.certificate = for_new_at(&c, &new_pk);
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
        let v = verify_chain(&c.links, &old_pk, &anchored(&old_pk), &roots());
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
    fn a_compromise_repudiates_the_window_and_a_reattestation_restores_one() {
        let (old, old_pk) = generate_keypair();
        let (new, new_pk) = generate_keypair();
        let mut c = Chain::new();
        c.decision(&old); // 1 — the last entry anyone trusts
        c.decision(&old); // 2 — inside the window
        let reattested = c.decision(&old); // 3 — inside, later vouched for
        let rotation = c.compromise(&old_pk, &new, 1);
        let rotation_id = c.rotate(&new, &rotation); // 4 — signed by the NEW key
        let vouch = AmendmentDraft::new(
            reattested,
            c.hash_at(3),
            AmendmentKind::Reattested,
            "Art. VII",
            "independently verified; the Steward vouches for it",
        )
        .unwrap();
        let vouch_id = c.amend(&new, &vouch); // 5

        let v = verify_chain(&c.links, &old_pk, &anchored(&old_pk), &roots());
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

    /// The root says where the window opens. A declaration cannot trust
    /// the old key one entry further than its certificate does.
    #[test]
    fn last_trusted_is_the_roots_to_say() {
        let (old, old_pk) = generate_keypair();
        let (new, _) = generate_keypair();
        let mut c = Chain::new();
        c.decision(&old); // 1
        c.decision(&old); // 2
        let mut rotation = c.compromise(&old_pk, &new, 1);
        rotation.certificate.statement.last_trusted = Some(c.head(2));
        c.rotate(&new, &rotation);
        let v = verify_chain(&c.links, &old_pk, &anchored(&old_pk), &roots());
        assert!(!v.ok, "{v:#?}");
        assert!(v.repudiated.is_empty());
        assert_eq!(v.public_key, (&old_pk).into());
    }

    #[test]
    fn a_stolen_key_cannot_be_rotated_back_in() {
        // K1 is compromised and replaced by K2. The thief, still holding
        // K1, declares a "compromise" of K2 naming K1 as the new key — and
        // even a root certificate would not bring a key back.
        let (k1, k1_pk) = generate_keypair();
        let (k2, k2_pk) = generate_keypair();
        let mut c = Chain::new();
        c.decision(&k1); // 1
        let real = c.compromise(&k1_pk, &k2, 1);
        c.rotate(&k2, &real); // 2
        c.decision(&k2); // 3
        let honest =
            verify_chain(&c.links, &k1_pk, &anchored(&k1_pk), &roots());
        assert!(honest.ok, "{honest:#?}");

        let hijack = c.compromise(&k2_pk, &k1, 3);
        c.rotate(&k1, &hijack); // 4 — signed by the stolen key
        c.decision(&k1); // 5

        let v = verify_chain(&c.links, &k1_pk, &anchored(&k1_pk), &roots());
        assert!(!v.ok);
        assert_eq!(v.public_key, (&k2_pk).into(), "the chain stays with K2");
        let p = v.entries[3].problem.as_deref().unwrap();
        assert!(p.contains("never brought back"), "{p}");
        assert!(!v.entries[3].signature_valid, "{:#?}", v.entries[3]);
        assert!(!v.entries[4].signature_valid, "K1 signs nothing again");
        assert_eq!(v.keys.len(), 2);
        assert_eq!(v.keys[1].status, KeyStatus::Active);
    }

    #[test]
    fn a_routine_rotation_cannot_reuse_a_key_either() {
        let (k1, k1_pk) = generate_keypair();
        let (k2, k2_pk) = generate_keypair();
        let mut c = Chain::new();
        c.decision(&k1);
        let out = c.routine(&k1_pk, &k2);
        c.rotate(&k1, &out);
        let back = c.routine(&k2_pk, &k1);
        c.rotate(&k2, &back);
        let v = verify_chain(&c.links, &k1_pk, &anchored(&k1_pk), &roots());
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
        let fake = AmendmentDraft::new(
            target,
            c.hash_at(1),
            AmendmentKind::Overruled,
            "none",
            "overruled, says nobody with the key",
        )
        .unwrap();
        c.amend(&forger, &fake); // 2 — not signed by the key in force
        // Certified, even: a certificate is not a licence to skip the old
        // key's signature on a routine rotation.
        let grab = c.routine(&steward_pk, &forger);
        c.rotate(&forger, &grab); // 3 — likewise

        let v = verify_chain(&c.links, &steward_pk, &anchor, &roots());
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
        let v = verify_chain(&c.links, &pk, &anchored(&pk), &roots());
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
        let (k2, _) = generate_keypair();
        let (k3, _) = generate_keypair();
        let mut c = Chain::new();
        c.decision(&k1); // 1 — trusted
        c.decision(&k1); // 2 — inside the first window
        let first = c.compromise(&k1_pk, &k2, 1);
        c.rotate(&k2, &first); // 3
        let second = c.compromise(&k1_pk, &k3, 2);
        c.rotate(&k3, &second); // 4

        let v = verify_chain(&c.links, &k1_pk, &anchored(&k1_pk), &roots());
        assert!(!v.ok);
        let p = v.entries[3].problem.as_deref().unwrap();
        assert!(p.contains("repudiated"), "{p}");
        assert_eq!(v.keys.len(), 2, "K1 and K2; K3 never took the chain");
        assert_eq!(v.keys[0].through_seq, Some(1));
    }

    #[test]
    fn an_uncertified_compromise_fails_closed() {
        let (old, old_pk) = generate_keypair();
        let (new, new_pk) = generate_keypair();
        let mut c = Chain::new();
        c.decision(&old);
        c.decision(&old);
        let mut rotation = c.compromise(&old_pk, &new, 1);
        let statement = rotation.certificate.statement.clone();
        rotation.certificate = certify(&[&new], statement);
        c.rotate(&new, &rotation);

        // Being in an anchor does not help: only the root moves the chain.
        let anchor = anchored(&old_pk).with((&new_pk).into());
        let v = verify_chain(&c.links, &old_pk, &anchor, &roots());
        assert!(!v.ok, "{v:#?}");
        let p = v.entries[2].problem.as_deref().unwrap();
        assert!(p.contains("0 valid root signature"), "{p}");
        assert!(!v.entries[2].signature_valid, "checked under the old key");
        assert!(v.repudiated.is_empty(), "and nothing is repudiated");
        assert_eq!(v.public_key, (&old_pk).into());
    }

    /// A key stolen before anyone knew: the Steward rotates routinely,
    /// then learns the old key was already out, and names a head from
    /// before the rotation. The rotation is void with the rest of the
    /// window.
    #[test]
    fn a_rotation_inside_the_window_is_void_with_it() {
        let (steward, steward_pk) = generate_keypair();
        let (successor, _) = generate_keypair();
        let (recovery, recovery_pk) = generate_keypair();

        let mut c = Chain::new();
        c.decision(&steward); // 1 — the last entry anyone trusts
        c.decision(&steward); // 2 — the thief's, as it turns out
        let routine = c.routine(&steward_pk, &successor);
        c.rotate(&steward, &routine); // 3
        c.decision(&successor); // 4

        let declaration = c.compromise(&steward_pk, &recovery, 1);
        c.rotate(&recovery, &declaration); // 5

        let v = verify_chain(
            &c.links,
            &steward_pk,
            &anchored(&steward_pk),
            &roots(),
        );
        assert!(v.ok, "{v:#?}");
        assert_eq!(v.repudiated, vec![gov(2), key_id(1), gov(3)]);
        assert_eq!(v.public_key, (&recovery_pk).into());
        assert_eq!(v.keys.len(), 2, "the successor is not part of history");
        assert_eq!(v.keys[0].public_key, (&steward_pk).into());
        assert_eq!(v.keys[0].status, KeyStatus::Compromised);
        assert!(
            v.keys[0].certified,
            "the genesis certificate rode in on the voided rotation and \
             is the root's word all the same"
        );
        assert_eq!(v.keys[1].public_key, (&recovery_pk).into());
    }

    #[test]
    fn a_compromise_must_name_a_real_head_and_the_key_that_held_it() {
        let (old, old_pk) = generate_keypair();
        let (new, new_pk) = generate_keypair();
        let anchor = anchored(&old_pk);

        // A head whose hash is not that entry's — certified, so the only
        // thing wrong is what the chain says about it.
        let mut c = Chain::new();
        c.decision(&old);
        let mut trusted = c.head(1);
        trusted.entry_hash = data_hash(&json!("nope"));
        let statement = KeyCertStatement::compromise(
            (&new_pk).into(),
            2,
            c.prev_hash(),
            trusted,
        );
        let mut rotation = c.compromise(&old_pk, &new, 1);
        rotation.certificate = certify(&[&root(1)], statement);
        c.rotate(&new, &rotation);
        let v = verify_chain(&c.links, &old_pk, &anchor, &roots());
        let p = v.entries[1].problem.as_deref().unwrap();
        assert!(p.contains("last_trusted"), "{p}");

        // An old_key that was never in force.
        let (_, other_pk) = generate_keypair();
        let mut c = Chain::new();
        c.decision(&old);
        let rotation = c.compromise(&other_pk, &new, 1);
        c.rotate(&new, &rotation);
        let v = verify_chain(&c.links, &old_pk, &anchor, &roots());
        let p = v.entries[1].problem.as_deref().unwrap();
        assert!(p.contains("old_key is not the key"), "{p}");

        // A compromise whose certificate names no head at all.
        let mut c = Chain::new();
        c.decision(&old);
        let mut rotation = c.compromise(&old_pk, &new, 1);
        rotation.certificate.statement.last_trusted = None;
        c.rotate(&new, &rotation);
        let v = verify_chain(&c.links, &old_pk, &anchor, &roots());
        let p = v.entries[1].problem.as_deref().unwrap();
        assert!(p.contains("must name last_trusted"), "{p}");
    }

    #[test]
    fn a_rotation_carries_no_free_text() {
        let (old, old_pk) = generate_keypair();
        let (new, _) = generate_keypair();
        let mut c = Chain::new();
        c.decision(&old);
        let rotation = c.routine(&old_pk, &new);
        let mut value = serde_json::to_value(&rotation).unwrap();
        assert!(serde_json::from_value::<KeyRotation>(value.clone()).is_ok());
        value["note"] = json!("at the request of …");
        assert!(serde_json::from_value::<KeyRotation>(value).is_err());

        let mut head = serde_json::to_value(c.head(1)).unwrap();
        head["comment"] = json!("the last one I remember signing");
        assert!(serde_json::from_value::<TrustedHead>(head).is_err());

        let mut statement =
            serde_json::to_value(&rotation.certificate.statement).unwrap();
        statement["comment"] = json!("signed in the kitchen");
        assert!(serde_json::from_value::<KeyCertStatement>(statement).is_err());
    }

    #[test]
    fn the_root_statement_bytes_are_pinned() {
        let key: PublicKeyHex = PUBLISHED_KEYS[0].parse().unwrap();
        assert_eq!(
            String::from_utf8(KeyCertStatement::genesis(key).signed_bytes())
                .unwrap(),
            "agora-governance-root-v1\n\
             {\"agora_governance_key_cert\":1,\"from_seq\":1,\
             \"key\":\"ebb3091dd328f1463362c171121921b2fe14628e3fc4c145deaccefb85c0e78a\",\
             \"last_trusted\":null,\"prev_hash\":null,\"purpose\":\"genesis\"}"
        );

        // The same statement `governance/root/root_sign.py --self-test`
        // pins in the agora repository: the tool that signs and the
        // verifiers that check must mean the same bytes.
        let statement = KeyCertStatement::compromise(
            Sha256Hex::from([0xab; 32]).to_string().parse().unwrap(),
            15,
            Some([0xcd; 32].into()),
            TrustedHead {
                id: gov(10),
                chain_seq: 11,
                entry_hash: [0xef; 32].into(),
            },
        );
        assert_eq!(
            Sha256Hex::from(<[u8; 32]>::from(Sha256::digest(
                statement.signed_bytes()
            )))
            .to_string(),
            "633685771e08be126fd12ca4eb98c77120e5868236ff541ecba85ce6a21e6b68"
        );
    }

    #[test]
    fn root_keys_are_curve_points_and_make_a_root_set() {
        let roots = RootSet::published();
        assert_eq!(roots.keys().count(), ROOT_KEYS.len());
        assert_eq!(roots.threshold(), ROOT_THRESHOLD);
        assert!(ROOT_THRESHOLD >= 1 && ROOT_THRESHOLD <= ROOT_KEYS.len());
        for root in ROOT_KEYS {
            let key: PublicKeyHex = root.parse().unwrap();
            assert!(roots.contains(&key));
            assert!(
                key.to_verifying_key().is_ok(),
                "{root} is not a valid Ed25519 public key"
            );
            assert!(
                !PUBLISHED_KEYS.contains(root),
                "a root key never signs entries"
            );
        }
        assert_eq!(RootSet::new([], 0).threshold(), 1);
    }

    #[test]
    fn a_genesis_key_outside_the_anchor_is_reported_not_rejected() {
        let (key, pk) = generate_keypair();
        let c = chain(&key, 2);
        let v = verify_chain(&c, &pk, &KeyAnchor::default(), &roots());
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
        let amendment = AmendmentDraft::new(
            gov(1),
            data_hash(&json!("x")),
            AmendmentKind::Superseded,
            "Art. VI § 2",
            "superseded by GOV-2026-0009",
        )
        .unwrap()
        .with_authority(gov(9))
        .with_rationale("the later decision covers the same subject");
        let AmendmentDraft { amendment, texts } = amendment;
        let value = serde_json::to_value(&amendment).unwrap();
        assert_eq!(value["kind"], "superseded");
        assert_eq!(value["agora_governance_amendment"], 2);
        assert!(value.get("redaction").is_none(), "{value}");
        let text = value.to_string();
        assert!(!text.contains("Art. VI"), "no text in signed data: {text}");
        assert!(!text.contains("salt"), "and no salt: {text}");
        assert_eq!(
            value["note"]["commitment"],
            texts
                .note
                .as_ref()
                .unwrap()
                .commitment()
                .commitment
                .to_string()
        );
        assert_eq!(
            serde_json::from_value::<Amendment>(value).unwrap(),
            amendment
        );
        let beside = serde_json::to_value(&texts).unwrap();
        assert_eq!(beside["basis"]["text"], "Art. VI § 2");
        assert_eq!(
            serde_json::from_value::<AmendmentTexts>(beside).unwrap(),
            texts
        );

        // Version 1, as the platform's three are, still reads.
        let legacy = json!({
            "agora_governance_amendment": 1,
            "target": "GOV-2026-0001",
            "target_entry_hash": data_hash(&json!("x")),
            "kind": "non_precedential",
            "authority": "GOV-2026-0005",
            "basis": "§1",
            "note": "diagnostic finding",
        });
        let legacy: Amendment = serde_json::from_value(legacy).unwrap();
        assert_eq!(legacy.validate(), Ok(()));
        assert_eq!(legacy.basis, AmendmentText::Plain("§1".into()));

        let mut c = Chain::new();
        c.decision(&key);
        let rotation = c.compromise(&pk, &generate_keypair().0, 1);
        let value = serde_json::to_value(&rotation).unwrap();
        assert_eq!(value["agora_governance_key_rotation"], 2);
        assert_eq!(value["reason"], "compromise");
        let statement = &value["certificate"]["statement"];
        assert_eq!(statement["purpose"], "compromise");
        assert_eq!(statement["last_trusted"]["chain_seq"], 1);
        assert_eq!(
            value["outgoing_certificate"]["statement"]["purpose"],
            "genesis"
        );
        assert!(value.get("note").is_none(), "{value}");
        assert_eq!(
            serde_json::from_value::<KeyRotation>(value).unwrap(),
            rotation
        );

        let notice =
            AmendmentNotice::new(amd(1), at(0), &amendment, Some(&texts));
        let value = serde_json::to_value(&notice).unwrap();
        assert_eq!(value["id"], "AMD-2026-0001");
        assert_eq!(value["note"], "superseded by GOV-2026-0009");
        assert_eq!(
            serde_json::from_value::<AmendmentNotice>(value).unwrap(),
            notice
        );

        // The rationale erased: the label stays, and nothing else moves.
        let erased = AmendmentTexts {
            rationale: None,
            ..texts.clone()
        };
        let notice =
            AmendmentNotice::new(amd(1), at(0), &amendment, Some(&erased));
        assert_eq!(notice.note, "superseded by GOV-2026-0009");
        assert_eq!(notice.rationale.as_deref(), Some(WITHHELD_TEXT));
        let notice = AmendmentNotice::new(amd(1), at(0), &amendment, None);
        assert_eq!(notice.basis, WITHHELD_TEXT);
        let notice = AmendmentNotice::new(amd(1), at(0), &legacy, None);
        assert_eq!(notice.note, "diagnostic finding");
    }

    /// The verifier hashes the raw `data` value, so a field it has never
    /// heard of neither breaks it nor escapes the signature — and an
    /// amendment written without one verifies just the same.
    #[test]
    fn an_amendment_verifies_with_or_without_its_optional_fields() {
        let (key, pk) = generate_keypair();
        let mut c = Chain::new();
        let target = c.decision(&key);
        let bare = AmendmentDraft::new(
            target.clone(),
            c.hash_at(1),
            AmendmentKind::Correction,
            "clerical",
            "typo in the citation",
        )
        .unwrap();
        assert!(
            serde_json::to_value(&bare.amendment)
                .unwrap()
                .get("rationale")
                .is_none()
        );
        c.amend(&key, &bare);
        let full = bare.clone().with_rationale("at length: …");
        c.amend(&key, &full);
        // And a field from a future version of the shape.
        let mut future = serde_json::to_value(&full.amendment).unwrap();
        future["superseded_by_something_new"] = json!(["later"]);
        c.push(
            &key,
            amd(3),
            GovernanceLogEntryType::Amendment,
            future,
            true,
        );

        let v = verify_chain(&c.links, &pk, &anchored(&pk), &roots());
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
        let amendment = AmendmentDraft::new(
            target,
            c.hash_at(1),
            AmendmentKind::Overruled,
            "b",
            "n",
        )
        .unwrap();
        c.amend(&key, &amendment);
        let v = verify_chain(&c.links, &pk, &anchored(&pk), &roots());
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

    /// The parity check the Steward's second channel exists for: the live
    /// chain verifies under what this build has compiled in — the genesis
    /// key in [`PUBLISHED_KEYS`] and the roots in [`ROOT_KEYS`] — and the
    /// key the platform serves is the one that walk ends on.
    ///
    /// Before the first rotation that is the genesis key itself; after it,
    /// a key this crate has never heard of and does not need to, because
    /// the root certified it. A served key the roots did not certify, or a
    /// chain that no longer verifies, fails here within the hour.
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
        assert_eq!(served.algorithm, "ed25519");

        let genesis: PublicKeyHex = PUBLISHED_KEYS
            .first()
            .expect("PUBLISHED_KEYS is never empty")
            .parse()
            .unwrap();
        let links = client.get_governance_chain().await.unwrap();
        let report = verify_chain(
            &links,
            &genesis.to_verifying_key().unwrap(),
            &KeyAnchor::published(),
            &RootSet::published(),
        );
        let problems: Vec<_> = report
            .entries
            .iter()
            .filter_map(|e| {
                e.problem.as_ref().map(|p| (e.id.clone(), p.clone()))
            })
            .collect();
        assert!(
            report.ok,
            "the live chain does not verify under this build's genesis key \
             and roots: {problems:#?}"
        );
        assert!(report.unanchored_keys.is_empty(), "{report:#?}");
        assert_eq!(
            served.public_key, report.public_key,
            "the platform serves {} but the chain, followed under the \
             published roots, is held by {}",
            served.public_key, report.public_key
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
