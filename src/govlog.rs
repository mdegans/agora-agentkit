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
//! Redaction is not a chain break. The original `entry_hash` stays on the
//! row so later links still verify; a signed amendment entry names what
//! changed. [`EntryVerdict::content_matches`] is the check that notices.

use crate::crypto::{self, Signature, SigningKey, VerifyingKey};
use crate::enums::GovernanceLogEntryType;
use crate::ids::GovernanceLogId;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

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
/// everything needed to verify linkage and signatures, without `data`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct GovernanceChainLink {
    pub id: GovernanceLogId,
    pub entry_type: GovernanceLogEntryType,
    pub created_at: DateTime<Utc>,
    pub attestation: GovernanceAttestation,
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
    /// The entry's current `data` hashes to the attested `data_hash`.
    /// `null` when the verifier did not read `data` (the chain endpoint
    /// carries none). `false` with a clean chain means the content was
    /// changed after attestation — look for an amendment entry naming it.
    #[serde(default)]
    pub content_matches: Option<bool>,
    /// See [`GovernanceAttestation::retroactive`]
    pub retroactive: bool,
    /// `created_at` is earlier than the previous link's. Informational:
    /// chain order is what is attested, and a clock step does not break it.
    pub out_of_order: bool,
    /// Amendment entries that name this one. Empty until amendments exist.
    #[serde(default)]
    pub amended_by: Vec<GovernanceLogId>,
    /// What failed, when something did
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
}

/// A verification of the whole chain
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct GovernanceVerification {
    pub public_key: PublicKeyHex,
    /// Every entry's signature and link verified, and no entry's content
    /// is known to differ from what was attested
    pub ok: bool,
    /// The last entry in the chain
    #[serde(default)]
    pub head: Option<GovernanceLogId>,
    /// In chain order
    pub entries: Vec<EntryVerdict>,
}

impl GovernanceVerification {
    /// Recompute `ok` from the entries
    pub fn settle(mut self) -> Self {
        self.ok = self.entries.iter().all(|e| {
            e.signature_valid
                && e.link_valid
                && e.content_matches != Some(false)
        });
        self
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

/// Verify a whole chain under `key`.
///
/// Links are sorted by `chain_seq` first, so the caller's order does not
/// matter. `content_matches` is `None` throughout: a chain carries no
/// `data`; see [`verify_data`] for that half. `retroactive` and
/// `out_of_order` are recomputed from the timestamps, not copied.
pub fn verify_chain(
    links: &[GovernanceChainLink],
    key: &VerifyingKey,
) -> GovernanceVerification {
    let mut links: Vec<&GovernanceChainLink> = links.iter().collect();
    links.sort_by_key(|l| l.attestation.chain_seq);

    let mut entries = Vec::with_capacity(links.len());
    let mut prev: Option<&GovernanceChainLink> = None;
    for (i, link) in links.iter().enumerate() {
        let a = &link.attestation;
        let expected_seq = i as u64 + 1;
        let mut problems: Vec<String> = Vec::new();

        let (hash_ok, signature_valid) = match verify_link(link, key) {
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

        entries.push(EntryVerdict {
            id: link.id.clone(),
            chain_seq: a.chain_seq,
            signature_valid,
            link_valid,
            content_matches: None,
            retroactive: is_retroactive(link.created_at, a.signed_at),
            out_of_order,
            amended_by: Vec::new(),
            problem: (!problems.is_empty()).then(|| problems.join("; ")),
        });
        prev = Some(link);
    }

    GovernanceVerification {
        public_key: key.into(),
        ok: false,
        head: prev.map(|p| p.id.clone()),
        entries,
    }
    .settle()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::generate_keypair;
    use serde_json::json;

    fn gov(n: u32) -> GovernanceLogId {
        format!("GOV-2026-{n:04}").parse().unwrap()
    }

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + secs, 123_456_789).unwrap()
    }

    fn link(
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
        }
    }

    fn chain(key: &SigningKey, n: u32) -> Vec<GovernanceChainLink> {
        let mut out: Vec<GovernanceChainLink> = Vec::new();
        for i in 1..=n {
            let data = json!({"title": format!("Decision {i}"), "outcome": "approved"});
            let l = link(key, i, out.last(), &data, at(i as i64 * 10 + 1));
            out.push(l);
        }
        out
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
        let v = verify_chain(&c, &pk);
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
        let v = verify_chain(&[], &pk);
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
        let v = verify_chain(&c, &pk);
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
        let v = verify_chain(&c, &pk);
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
        let v = verify_chain(&c, &pk);
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
        };
        second.attestation.retroactive = true; // a lying flag on the wire
        let v = verify_chain(&[first, second], &pk);
        assert!(v.ok, "{v:#?}");
        assert!(v.entries[0].retroactive);
        assert!(!v.entries[1].retroactive, "recomputed from timestamps");
        assert!(v.entries[1].out_of_order);
    }

    #[test]
    fn content_mismatch_settles_to_not_ok() {
        let (key, pk) = generate_keypair();
        let c = chain(&key, 1);
        let mut v = verify_chain(&c, &pk);
        v.entries[0].content_matches = Some(true);
        assert!(v.clone().settle().ok);
        v.entries[0].content_matches = Some(false);
        assert!(!v.settle().ok);
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
