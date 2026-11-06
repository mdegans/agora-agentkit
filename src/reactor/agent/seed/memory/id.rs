//! [`MemoryId`]: the name of one memory text, keyed per agent, and
//! [`HistoryRef`], the `id` an agent passes to the `memory` tool.

use std::fmt;
use std::str::FromStr;

use hkdf::Hkdf;
use rand::RngCore;
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::govlog::Sha256Hex;
use crate::ids::AgentId;

/// What a [`MemoryId`] is shown with
pub const MEMORY_ID_PREFIX: &str = "mem-";

/// Hex digits a [`MemoryId`] is shown with
pub const MEMORY_ID_SHOWN: usize = 10;

/// Hex digits a sealed head is shown with
pub const HEAD_SHOWN: usize = 12;

/// The fewest hex digits a [`HistoryRef`] takes
pub const MIN_REF_HEX: usize = 6;

/// The frozen domain tag of [`MemoryId::of`]. Changing it, or anything else
/// in that function's input, changes every id
const DOMAIN: &[u8] = b"agora memory id v1";

/// The per-agent secret a [`MemoryId`] is computed under. Kept by the
/// runner, never in the agent's state
pub struct MemoryIdKey(Zeroizing<[u8; 32]>);

impl MemoryIdKey {
    /// A fresh random key
    pub fn generate() -> Self {
        let mut k = Zeroizing::new([0u8; 32]);
        OsRng.fill_bytes(k.as_mut());
        Self(k)
    }

    /// From raw bytes, as the runner stores them
    pub fn from_bytes(bytes: Zeroizing<[u8; 32]>) -> Self {
        Self(bytes)
    }

    /// The raw bytes, for the runner to store
    pub fn expose(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for MemoryIdKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MemoryIdKey([REDACTED])")
    }
}

impl fmt::Display for MemoryIdKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

/// One memory text's id: HMAC-SHA256 under the agent's [`MemoryIdKey`].
/// Shown as `mem-` and its first ten hex digits; held and serialized whole
/// (64 hex digits)
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MemoryId([u8; 32]);

impl MemoryId {
    /// The id of `content` (raw `memory.content`, not the rendered prompt
    /// text) for `agent`.
    ///
    /// The input is frozen: the domain tag, the agent's UUID bytes and the
    /// content, each preceded by its length as a big-endian `u64`, so no
    /// two inputs frame alike. Golden values in the tests pin it.
    pub fn of(key: &MemoryIdKey, agent: AgentId, content: &str) -> Self {
        let mut input = Zeroizing::new(Vec::with_capacity(
            DOMAIN.len() + 16 + content.len() + 24,
        ));
        for field in [DOMAIN, agent.as_uuid().as_bytes(), content.as_bytes()] {
            input.extend_from_slice(&(field.len() as u64).to_be_bytes());
            input.extend_from_slice(field);
        }
        // HKDF-Extract(salt, IKM) is defined as HMAC-Hash(salt, IKM) (RFC
        // 5869 § 2.2), so this is exactly HMAC-SHA256(key, input), without
        // `hmac` as a direct dependency
        let (prk, _) = Hkdf::<Sha256>::extract(Some(key.expose()), &input);
        Self(prk.into())
    }

    /// All 64 hex digits
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// The ten hex digits it is shown with, without `mem-`
    pub fn short_hex(&self) -> String {
        let mut s = self.to_hex();
        s.truncate(MEMORY_ID_SHOWN);
        s
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl From<[u8; 32]> for MemoryId {
    fn from(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

/// `mem-3f9a1c07be`
impl fmt::Display for MemoryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{MEMORY_ID_PREFIX}{}", self.short_hex())
    }
}

impl fmt::Debug for MemoryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MemoryId({})", self.to_hex())
    }
}

/// The whole id, 64 hex digits, `mem-` optional
impl FromStr for MemoryId {
    type Err = HistoryRefError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let r: HistoryRef = s.parse()?;
        let bytes = hex::decode(&r.hex)
            .ok()
            .and_then(|b| <[u8; 32]>::try_from(b).ok())
            .ok_or_else(|| HistoryRefError(s.to_string()))?;
        Ok(Self(bytes))
    }
}

impl Serialize for MemoryId {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for MemoryId {
    fn deserialize<D: serde::Deserializer<'de>>(
        d: D,
    ) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// A head as shown: its first [`HEAD_SHOWN`] hex digits
pub fn short_head(head: &Sha256Hex) -> String {
    let mut s = head.to_hex();
    s.truncate(HEAD_SHOWN);
    s
}

/// A version as an agent names it: a memory id (`mem-` optional) or a
/// sealed head, by [`MIN_REF_HEX`] hex digits or more of its start
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HistoryRef {
    /// Lowercase
    hex: String,
    /// Written with `mem-`, so only a memory id can match
    memory: bool,
}

/// Not a memory id or a head
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "\"{0}\" is not a memory id or a head: give a memory id (mem- and hex \
     digits, as in your Memory heading) or a head, at least 6 hex digits \
     of either"
)]
pub struct HistoryRefError(String);

impl HistoryRef {
    /// The hex digits given, lowercase, without `mem-`
    pub fn hex(&self) -> &str {
        &self.hex
    }

    /// Whether it was written with `mem-`, so it names a memory id only
    pub fn is_memory(&self) -> bool {
        self.memory
    }

    /// Whether `id` starts with it
    pub fn matches_memory(&self, id: &MemoryId) -> bool {
        id.to_hex().starts_with(&self.hex)
    }

    /// Whether `head` starts with it (never, when written with `mem-`)
    pub fn matches_head(&self, head: &Sha256Hex) -> bool {
        !self.memory && head.to_hex().starts_with(&self.hex)
    }
}

impl FromStr for HistoryRef {
    type Err = HistoryRefError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = || HistoryRefError(s.to_string());
        let lower = s.trim().to_ascii_lowercase();
        let (hex, memory) = match lower.strip_prefix(MEMORY_ID_PREFIX) {
            Some(rest) => (rest.to_string(), true),
            None => (lower, false),
        };
        if hex.len() < MIN_REF_HEX
            || hex.len() > 64
            || !hex.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(err());
        }
        Ok(Self { hex, memory })
    }
}

impl fmt::Display for HistoryRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.memory {
            f.write_str(MEMORY_ID_PREFIX)?;
        }
        f.write_str(&self.hex)
    }
}

impl From<&MemoryId> for HistoryRef {
    fn from(id: &MemoryId) -> Self {
        Self {
            hex: id.to_hex(),
            memory: true,
        }
    }
}

impl Serialize for HistoryRef {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for HistoryRef {
    fn deserialize<D: serde::Deserializer<'de>>(
        d: D,
    ) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

// Hand-written so it is inline (a derive's `$ref` is banned in tool
// schemas, agora CLAUDE.md), and a plain string with no `pattern`: the
// parse says what is wrong
impl schemars::JsonSchema for HistoryRef {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> std::borrow::Cow<'static, str> {
        "HistoryRef".into()
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        concat!(module_path!(), "::HistoryRef").into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "description": "A memory id (mem-3f9a1c07be) or a head, at least \
                            6 hex digits of either",
        })
    }
}

/// One version a [`HistoryRef`] could mean
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Candidate {
    Memory(MemoryId),
    Head(Sha256Hex),
}

impl fmt::Display for Candidate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Memory(id) => id.fmt(f),
            Self::Head(head) => write!(f, "head {}", short_head(head)),
        }
    }
}

/// What a [`HistoryRef`] resolves to among the versions known
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    One(Candidate),
    /// More than one: the agent is shown them, to give more digits
    Ambiguous(Vec<Candidate>),
    None,
}

/// Resolve `r` against the memory ids and heads known, each counted once
/// however often it appears
pub fn resolve<'a>(
    r: &HistoryRef,
    memories: impl IntoIterator<Item = &'a MemoryId>,
    heads: impl IntoIterator<Item = &'a Sha256Hex>,
) -> Resolution {
    let mut found: Vec<Candidate> = Vec::new();
    for id in memories {
        let c = Candidate::Memory(*id);
        if r.matches_memory(id) && !found.contains(&c) {
            found.push(c);
        }
    }
    for head in heads {
        let c = Candidate::Head(*head);
        if r.matches_head(head) && !found.contains(&c) {
            found.push(c);
        }
    }
    match found.len() {
        0 => Resolution::None,
        1 => Resolution::One(found.pop().expect("one")),
        _ => Resolution::Ambiguous(found),
    }
}

impl Resolution {
    /// What an agent is told of an ambiguous `r`: the candidates, to give
    /// more digits. `None` unless ambiguous
    pub fn ambiguity(&self, r: &HistoryRef) -> Option<String> {
        let Self::Ambiguous(candidates) = self else {
            return None;
        };
        let list: Vec<String> =
            candidates.iter().map(|c| format!("- {c}")).collect();
        Some(format!(
            "\"{r}\" could mean any of these; give more digits:\n{}",
            list.join("\n")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(b: u8) -> MemoryIdKey {
        MemoryIdKey::from_bytes(Zeroizing::new([b; 32]))
    }

    fn agent() -> AgentId {
        "0b6f4c1e-8d2a-4f3b-9c5d-7e1a2b3c4d5e".parse().unwrap()
    }

    /// RFC 4231 test case 2: the extract is HMAC-SHA256
    #[test]
    fn hkdf_extract_is_hmac_sha256() {
        let (prk, _) = Hkdf::<Sha256>::extract(
            Some(b"Jefe"),
            b"what do ya want for nothing?",
        );
        assert_eq!(
            hex::encode(prk),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    /// Golden values: any change to the input rule changes these, and
    /// every id agents have been shown. Checked against Python's `hmac`
    /// over the framing [`MemoryId::of`] documents
    #[test]
    fn golden_ids() {
        let id = MemoryId::of(&key(7), agent(), "# Memory\n\nRemembered.\n");
        assert_eq!(
            id.to_hex(),
            "212b76b7546359ae82bcb17b2ea81d4f09db7a10ae4e4b3726d344c83675d57c"
        );
        assert_eq!(id.to_string(), format!("mem-{}", &id.to_hex()[..10]));
        let empty = MemoryId::of(&key(7), agent(), "");
        assert_eq!(
            empty.to_hex(),
            "2e1a8c593e1b846de07d878c9b57e3055dfa71e5610b5a16d99a65112dc7ec58"
        );
        let other_key = MemoryId::of(&key(8), agent(), "");
        assert_eq!(
            other_key.to_hex(),
            "d649ac8d086b1faa460a5fd4de9d44454f9bc9a474da0faf12fa2968ce481a0a"
        );
    }

    #[test]
    fn the_id_depends_on_key_agent_and_content() {
        let a = MemoryId::of(&key(1), agent(), "x");
        assert_ne!(a, MemoryId::of(&key(2), agent(), "x"));
        assert_ne!(a, MemoryId::of(&key(1), AgentId::new(), "x"));
        assert_ne!(a, MemoryId::of(&key(1), agent(), "x "));
        assert_eq!(a, MemoryId::of(&key(1), agent(), "x"));
    }

    #[test]
    fn ids_serialize_whole_and_show_short() {
        let id = MemoryId::of(&key(3), agent(), "m");
        let json = serde_json::to_value(id).unwrap();
        assert_eq!(json, serde_json::json!(id.to_hex()));
        assert_eq!(serde_json::from_value::<MemoryId>(json).unwrap(), id);
        assert_eq!(id.to_string().len(), 4 + 10);
        assert!(format!("{id:?}").contains(&id.to_hex()));
    }

    #[test]
    fn the_key_is_redacted() {
        let k = key(9);
        assert_eq!(format!("{k:?}"), "MemoryIdKey([REDACTED])");
        assert_eq!(k.to_string(), "[REDACTED]");
    }

    #[test]
    fn refs_parse_with_or_without_mem() {
        let r: HistoryRef = "mem-3F9A1C".parse().unwrap();
        assert_eq!(r.hex(), "3f9a1c");
        assert!(r.is_memory());
        assert_eq!(r.to_string(), "mem-3f9a1c");
        let bare: HistoryRef = " 3f9a1c07be ".parse().unwrap();
        assert!(!bare.is_memory());
        for bad in [
            "mem-3f9a1",
            "3f9a1",
            "mem-",
            "",
            "mem-3f9a1g07",
            "x".repeat(65).as_str(),
        ] {
            assert!(bad.parse::<HistoryRef>().is_err(), "{bad}");
        }
        let err = "zz".parse::<HistoryRef>().unwrap_err().to_string();
        assert!(err.contains("memory id"), "{err}");
    }

    #[test]
    fn refs_match_by_prefix_and_resolve() {
        let a = MemoryId::from([0xab; 32]);
        let b = MemoryId::from({
            let mut x = [0xab; 32];
            x[5] = 0;
            x
        });
        let head = Sha256Hex::from([0xab; 32]);
        let other_head = Sha256Hex::from([0x12; 32]);

        // `mem-` excludes heads
        let r: HistoryRef = "mem-abababab".parse().unwrap();
        assert!(r.matches_memory(&a) && !r.matches_head(&head));
        let res = resolve(&r, [&a, &b, &a], [&head]);
        assert_eq!(
            res,
            Resolution::Ambiguous(vec![
                Candidate::Memory(a),
                Candidate::Memory(b)
            ])
        );
        let text = res.ambiguity(&r).unwrap();
        assert!(text.contains("mem-ababababab"), "{text}");

        // Enough digits to tell them apart
        let r: HistoryRef = "mem-abababababab".parse().unwrap();
        assert_eq!(
            resolve(&r, [&a, &b], [&head]),
            Resolution::One(Candidate::Memory(a))
        );

        // Bare hex can mean either namespace
        let r: HistoryRef = "ababababababab".parse().unwrap();
        let res = resolve(&r, [&a], [&head, &other_head]);
        assert_eq!(
            res,
            Resolution::Ambiguous(vec![
                Candidate::Memory(a),
                Candidate::Head(head)
            ])
        );
        assert!(res.ambiguity(&r).unwrap().contains("head abababababab"));

        let r: HistoryRef = "121212".parse().unwrap();
        assert_eq!(
            resolve(&r, [&a], [&head, &other_head]),
            Resolution::One(Candidate::Head(other_head))
        );
        let r: HistoryRef = "999999".parse().unwrap();
        assert_eq!(resolve(&r, [&a], [&head]), Resolution::None);
        assert_eq!(Resolution::None.ambiguity(&r), None);
    }

    #[test]
    fn a_whole_id_round_trips_through_a_ref() {
        let id = MemoryId::of(&key(4), agent(), "z");
        let r = HistoryRef::from(&id);
        assert!(r.matches_memory(&id));
        assert_eq!(r.to_string().parse::<MemoryId>().unwrap(), id);
        assert_eq!(id.to_hex().parse::<MemoryId>().unwrap(), id);
        assert!(id.short_hex().parse::<MemoryId>().is_err());
    }
}
