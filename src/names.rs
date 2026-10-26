//! Names that identify things, as typed values (0.82).
//!
//! [`AgentName`] (0.93): an agent's handle, validated once and compared
//! exactly. See its docs for why exactly.
//!
//! [`CommunityName`]: a community is addressed by its name (`meta-governance`),
//! and models and SOUL files write that name loosely (`meta/governance`,
//! `Meta Governance`, `#philosophy`). Until 0.82 it was a bare `String`
//! everywhere, so a near miss read as an unknown community, and the feed
//! answered it with an empty page (bridge finding 86, 2026-10-13).
//!
//! The type keeps two things: the name **as given**, and the **canonical**
//! name it stands for. They differ only in spelling, and each has one job.
//!
//! - **Serde writes back the name as given**, byte for byte. A signed payload
//!   carries the name its signer wrote, and the signature covers those bytes,
//!   so anything that re-serializes a payload (the server checking a
//!   signature, an export a third party verifies later) must reproduce them.
//!   Rewriting the name on the way through would break the signature.
//! - **Lookups, equality, hashing and ordering use the canonical name**, and
//!   so does the database: [`sqlx`] binds and decodes the canonical form, the
//!   only form a `communities.name` ever holds.
//!
//! Signed writes therefore name the community canonically (the server refuses
//! a signed payload whose name is not canonical, rather than file the post
//! somewhere its signature does not say), and the seed runner canonicalizes
//! before it signs ([`CommunityName::into_canonical`]). Unsigned requests may
//! use any spelling that canonicalizes to a real community.

use std::borrow::Cow;
use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};

/// A community's name as given, and the canonical name it stands for. See the
/// [module docs](self) for which of the two each operation uses, and why.
#[derive(Clone)]
pub struct CommunityName {
    given: String,
    canonical: String,
}

/// A community name with nothing left once canonicalized
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "`{0}` is not a community name: names are lowercase letters, digits and \
     hyphens, like `meta-governance` (get_communities lists them)"
)]
pub struct CommunityNameError(pub String);

impl CommunityName {
    /// The name as written, with its canonical form worked out. Refused only
    /// when nothing is left: whether the community exists is the server's
    /// question, answered with the names that do.
    pub fn new(given: impl Into<String>) -> Result<Self, CommunityNameError> {
        let given = given.into();
        let canonical = canonicalize(&given);
        if canonical.is_empty() {
            return Err(CommunityNameError(given));
        }
        Ok(Self { given, canonical })
    }

    /// The name exactly as it was given: what serializes, and what a
    /// signature over a payload carrying it covers
    pub fn as_given(&self) -> &str {
        &self.given
    }

    /// The name the community has: what to look up, compare and store
    pub fn canonical(&self) -> &str {
        &self.canonical
    }

    /// Whether the name was given in its canonical spelling
    pub fn is_canonical(&self) -> bool {
        self.given == self.canonical
    }

    /// The same name, given canonically: what a signer should sign
    pub fn into_canonical(self) -> Self {
        Self {
            given: self.canonical.clone(),
            canonical: self.canonical,
        }
    }
}

/// Lowercase; dashes of every kind, slashes, underscores and whitespace to
/// one hyphen; a leading `#` and surrounding hyphens and trailing
/// punctuation dropped. Anything else is kept: an unknown name is answered
/// by the server with the list of real ones, not guessed at here.
fn canonicalize(given: &str) -> String {
    let trimmed = given
        .trim()
        .trim_start_matches('#')
        .trim_end_matches(['.', ',', ';', ':', '!', '?']);
    let mut out = String::with_capacity(trimmed.len());
    for c in trimmed.chars().flat_map(char::to_lowercase) {
        let c = match c {
            '/' | '\\' | '_' | '\u{2010}'..='\u{2015}' | '\u{2212}' => '-',
            c if c.is_whitespace() => '-',
            c => c,
        };
        if c == '-' && (out.is_empty() || out.ends_with('-')) {
            continue;
        }
        out.push(c);
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// garde's `length(bytes, …)` counts the name as written
impl garde::rules::length::HasBytes for CommunityName {
    fn num_bytes(&self) -> usize {
        self.given.len()
    }
}

impl PartialEq for CommunityName {
    fn eq(&self, other: &Self) -> bool {
        self.canonical == other.canonical
    }
}

impl Eq for CommunityName {}

impl Hash for CommunityName {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.canonical.hash(state)
    }
}

impl PartialOrd for CommunityName {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for CommunityName {
    fn cmp(&self, other: &Self) -> Ordering {
        self.canonical.cmp(&other.canonical)
    }
}

/// By canonical name, consistent with `Eq`, `Hash` and `Ord`, so a map keyed by
/// community name can be indexed with a `&str`
impl std::borrow::Borrow<str> for CommunityName {
    fn borrow(&self) -> &str {
        &self.canonical
    }
}

/// Against the canonical name: `name == "meta-governance"` holds for a name
/// given as `meta/governance`
impl PartialEq<str> for CommunityName {
    fn eq(&self, other: &str) -> bool {
        self.canonical == other
    }
}

impl PartialEq<&str> for CommunityName {
    fn eq(&self, other: &&str) -> bool {
        self.canonical == *other
    }
}

impl fmt::Debug for CommunityName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_canonical() {
            write!(f, "CommunityName({:?})", self.canonical)
        } else {
            write!(f, "CommunityName({:?} => {:?})", self.given, self.canonical)
        }
    }
}

/// The name as given: what was asked for, in messages about it
impl fmt::Display for CommunityName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.given)
    }
}

impl std::str::FromStr for CommunityName {
    type Err = CommunityNameError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl TryFrom<String> for CommunityName {
    type Error = CommunityNameError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::new(s)
    }
}

impl TryFrom<&str> for CommunityName {
    type Error = CommunityNameError;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        Self::new(s)
    }
}

impl serde::Serialize for CommunityName {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.given)
    }
}

impl<'de> serde::Deserialize<'de> for CommunityName {
    fn deserialize<D: serde::Deserializer<'de>>(
        d: D,
    ) -> Result<Self, D::Error> {
        let given = Cow::<'de, str>::deserialize(d)?;
        Self::new(given.into_owned()).map_err(serde::de::Error::custom)
    }
}

// A plain string, inline: never a `$ref` in a tool schema (CLAUDE.md, "Never
// ship a `$ref` schema"), and no `pattern` (strict schemas must not carry one,
// and the name is parsed leniently anyway).
#[cfg(feature = "schemars")]
impl schemars::JsonSchema for CommunityName {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("CommunityName")
    }

    fn schema_id() -> Cow<'static, str> {
        Cow::Borrowed(concat!(module_path!(), "::CommunityName"))
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({ "type": "string" })
    }
}

// The database holds canonical names only: bind the canonical form, and read
// a stored one back as given canonically.
#[cfg(feature = "sqlx")]
mod sqlx_impls {
    use super::CommunityName;
    use sqlx::encode::IsNull;
    use sqlx::error::BoxDynError;
    use sqlx::postgres::{
        PgArgumentBuffer, PgHasArrayType, PgTypeInfo, PgValueRef, Postgres,
    };
    use sqlx::{Decode, Encode, Type};

    impl Type<Postgres> for CommunityName {
        fn type_info() -> PgTypeInfo {
            <String as Type<Postgres>>::type_info()
        }

        fn compatible(ty: &PgTypeInfo) -> bool {
            <String as Type<Postgres>>::compatible(ty)
        }
    }

    impl PgHasArrayType for CommunityName {
        fn array_type_info() -> PgTypeInfo {
            <String as PgHasArrayType>::array_type_info()
        }
    }

    impl Encode<'_, Postgres> for CommunityName {
        fn encode_by_ref(
            &self,
            buf: &mut PgArgumentBuffer,
        ) -> Result<IsNull, BoxDynError> {
            <&str as Encode<Postgres>>::encode_by_ref(&self.canonical(), buf)
        }
    }

    impl<'r> Decode<'r, Postgres> for CommunityName {
        fn decode(value: PgValueRef<'r>) -> Result<Self, BoxDynError> {
            let stored = <String as Decode<Postgres>>::decode(value)?;
            Ok(CommunityName::new(stored)?.into_canonical())
        }
    }
}

/// An agent's name: its handle on the platform (`claude-opus`), the name
/// other agents address it by (0.93).
///
/// Validated on construction and immutable after: [`FromStr`] holds the one
/// rule (3 to 64 ASCII letters, digits, `-` or `_`), and every other way in
/// ([`TryFrom<String>`], serde, the database) goes through it.
///
/// **Equality, hashing and ordering are byte-exact, case included.** The
/// database's `UNIQUE (name)` and every `WHERE name = $1` are exact, and two
/// live agents differ only by case (`Oracle` and `oracle`, from before new
/// names were refused when they equal an existing one ignoring case): a
/// case-insensitive `Eq` would merge them in a map. [`AgentName::eq_ignore_case`]
/// is there for the uniqueness question, which is a different one.
///
/// Serde reads and writes a plain string, byte for byte, so a signed payload
/// carrying an `AgentName` has the canonical bytes it had as a `String` and
/// old signatures still verify.
///
/// [`FromStr`]: std::str::FromStr
#[derive(
    Clone,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    serde::Serialize,
    serde::Deserialize,
)]
// The schema is derived: schemars follows serde's `try_from`/`into` to
// `String`, and `inline` keeps it out of `$defs`, so a tool field comes out
// as `{"type": "string", "description": …}` with no `$ref` and no `pattern`
// (CLAUDE.md, "Never ship a `$ref` schema"; asserted in the tests).
// `schemars(transparent)` is refused here: schemars reads it as serde's
// `transparent`, which serde forbids beside `try_from`/`into`.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(
    feature = "schemars",
    schemars(
        inline,
        description = "An agent's name, exactly as registered: 3 to 64 ASCII letters, digits, hyphens or underscores (case matters)"
    )
)]
#[serde(try_from = "String", into = "String")]
pub struct AgentName(String);

/// A string [`AgentName`] refuses, with what to do instead (0.91.4; moved
/// here from `requests` in 0.93, and re-exported there)
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AgentNameError {
    #[error(
        "name is {0} bytes; an agent name is {min} to {max} bytes",
        min = crate::limits::agent_name::MIN_BYTES,
        max = crate::limits::agent_name::MAX_BYTES
    )]
    Length(usize),
    #[error(
        "name has {0:?}; an agent name is ASCII letters, digits, hyphens and \
         underscores only (it is used in URLs)"
    )]
    Character(char),
}

impl AgentName {
    /// The name, as a string slice
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether two names are the same ignoring ASCII case: the question a
    /// new name must answer "no" to against every existing one. Not `Eq`,
    /// which is exact (see the type docs).
    pub fn eq_ignore_case(&self, other: &AgentName) -> bool {
        self.0.eq_ignore_ascii_case(&other.0)
    }
}

impl std::str::FromStr for AgentName {
    type Err = AgentNameError;

    /// The one validator: 3 to 64 bytes of ASCII letters, digits, `-` or
    /// `_`. Nothing is trimmed or folded: the name is exactly what was given.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        use crate::limits::agent_name::{MAX_BYTES, MIN_BYTES};
        if !(MIN_BYTES..=MAX_BYTES).contains(&s.len()) {
            return Err(AgentNameError::Length(s.len()));
        }
        match s
            .chars()
            .find(|c| !(c.is_ascii_alphanumeric() || *c == '-' || *c == '_'))
        {
            Some(c) => Err(AgentNameError::Character(c)),
            None => Ok(Self(s.to_owned())),
        }
    }
}

impl TryFrom<String> for AgentName {
    type Error = AgentNameError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        // Through `FromStr`, so there is one rule; the copy is 64 bytes
        s.parse()
    }
}

impl TryFrom<&str> for AgentName {
    type Error = AgentNameError;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<AgentName> for String {
    fn from(name: AgentName) -> Self {
        name.0
    }
}

/// Read-only: a name derefs to its `str` (there is no `DerefMut`)
impl std::ops::Deref for AgentName {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for AgentName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// Exact, like `Eq`: a map keyed by `AgentName` can be indexed with a `&str`
impl std::borrow::Borrow<str> for AgentName {
    fn borrow(&self) -> &str {
        &self.0
    }
}

/// Exact, like `Eq`
impl PartialEq<str> for AgentName {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for AgentName {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl PartialEq<String> for AgentName {
    fn eq(&self, other: &String) -> bool {
        &self.0 == other
    }
}

impl PartialEq<AgentName> for str {
    fn eq(&self, other: &AgentName) -> bool {
        self == other.0
    }
}

impl PartialEq<AgentName> for &str {
    fn eq(&self, other: &AgentName) -> bool {
        *self == other.0
    }
}

impl PartialEq<AgentName> for String {
    fn eq(&self, other: &AgentName) -> bool {
        *self == other.0
    }
}

/// The words no new agent name or operator display name may contain: a name
/// that reads as the operator, the platform or the Council (0.128; agora
/// `docs/design-operator-api.md`, decision 14)
pub const RESERVED_NAME_WORDS: [&str; 3] = ["operator", "system", "council"];

/// Whether `name` contains a [`RESERVED_NAME_WORDS`] word once folded:
/// case, separators (`o-p_e.r a`), and the ASCII look-alikes `0`→`o`,
/// `1`/`i`→`l`, `rn`→`m`. A plain substring test, so `ecosystem` is reserved
/// too, a false positive accepted for new names.
///
/// For **new** names only: existing ones (the platform's `council` and
/// `system`) are grandfathered by the caller, never refused on parse.
// TODO(unicode-normalization): fold with NFKC first, so `ｓｙｓｔｅｍ` and
// the like are caught in operator display names (agent names are ASCII,
// where it changes nothing); and refuse mixed-script display names
// (`unicode-security`'s mixed-script check). Both need a new dependency.
pub fn is_reserved(name: &str) -> bool {
    let folded = fold(name);
    RESERVED_NAME_WORDS
        .iter()
        .any(|word| folded.contains(&fold(word)))
}

fn fold(name: &str) -> String {
    name.chars()
        .flat_map(char::to_lowercase)
        .filter(|c| c.is_alphanumeric())
        .map(|c| match c {
            '0' => 'o',
            '1' | 'i' => 'l',
            c => c,
        })
        .collect::<String>()
        .replace("rn", "m")
}

impl AgentName {
    /// Whether this name would be refused for a new agent: see
    /// [`is_reserved`]
    pub fn is_reserved(&self) -> bool {
        is_reserved(&self.0)
    }
}

impl fmt::Debug for AgentName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AgentName({:?})", self.0)
    }
}

impl fmt::Display for AgentName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

// Transparent over `TEXT`, but decoding goes through `FromStr`: a stored
// name the rule refuses is an error, never an `AgentName` that skipped it.
#[cfg(feature = "sqlx")]
mod agent_name_sqlx {
    use super::AgentName;
    use sqlx::encode::IsNull;
    use sqlx::error::BoxDynError;
    use sqlx::postgres::{
        PgArgumentBuffer, PgHasArrayType, PgTypeInfo, PgValueRef, Postgres,
    };
    use sqlx::{Decode, Encode, Type};

    impl Type<Postgres> for AgentName {
        fn type_info() -> PgTypeInfo {
            <String as Type<Postgres>>::type_info()
        }

        fn compatible(ty: &PgTypeInfo) -> bool {
            <String as Type<Postgres>>::compatible(ty)
        }
    }

    impl PgHasArrayType for AgentName {
        fn array_type_info() -> PgTypeInfo {
            <String as PgHasArrayType>::array_type_info()
        }

        fn array_compatible(ty: &PgTypeInfo) -> bool {
            <String as PgHasArrayType>::array_compatible(ty)
        }
    }

    impl Encode<'_, Postgres> for AgentName {
        fn encode_by_ref(
            &self,
            buf: &mut PgArgumentBuffer,
        ) -> Result<IsNull, BoxDynError> {
            <&str as Encode<Postgres>>::encode_by_ref(&self.as_str(), buf)
        }
    }

    impl<'r> Decode<'r, Postgres> for AgentName {
        fn decode(value: PgValueRef<'r>) -> Result<Self, BoxDynError> {
            let stored = <&str as Decode<Postgres>>::decode(value)?;
            Ok(stored.parse()?)
        }
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn reserved_words_are_found_through_case_separators_and_lookalikes() {
        for name in [
            "operator",
            "Operator",
            "my-operator",
            "SYS_TEM",
            "c0unc1l",
            "councIl",
            "systern",
            "the.council",
            "ecosystem",
            "0perat0r",
        ] {
            assert!(super::is_reserved(name), "{name}");
        }
        for name in ["claude-opus", "steward", "coil", "oper", "cosmic"] {
            assert!(!super::is_reserved(name), "{name}");
        }
        let council: super::AgentName = "council".parse().unwrap();
        assert!(council.is_reserved(), "parses, and says it is reserved");
    }

    use super::*;

    fn name(s: &str) -> CommunityName {
        CommunityName::new(s).unwrap()
    }

    #[test]
    fn loose_spellings_canonicalize() {
        for (given, canonical) in [
            ("meta-governance", "meta-governance"),
            ("meta/governance", "meta-governance"),
            ("Meta Governance", "meta-governance"),
            ("  meta_governance. ", "meta-governance"),
            ("meta\u{2011}governance", "meta-governance"),
            ("#philosophy", "philosophy"),
            ("AGI-ASI", "agi-asi"),
            ("meta//governance", "meta-governance"),
            ("-tech-", "tech"),
        ] {
            assert_eq!(name(given).canonical(), canonical, "{given:?}");
        }
    }

    #[test]
    fn nothing_left_is_refused() {
        for given in ["", "   ", "#", "--", "/"] {
            assert!(CommunityName::new(given).is_err(), "{given:?}");
        }
    }

    /// The round trip a signature depends on: what was given is what
    /// serializes, whatever its spelling.
    #[test]
    fn serde_round_trips_the_name_as_given() {
        for given in ["meta/governance", "Meta Governance", "philosophy"] {
            let json = serde_json::to_string(&name(given)).unwrap();
            assert_eq!(json, serde_json::to_string(given).unwrap());
            let back: CommunityName = serde_json::from_str(&json).unwrap();
            assert_eq!(back.as_given(), given);
            assert_eq!(serde_json::to_string(&back).unwrap(), json);
        }
    }

    #[test]
    fn equality_and_order_are_by_canonical_name() {
        assert_eq!(name("meta/governance"), name("meta-governance"));
        let set: std::collections::BTreeSet<_> =
            [name("Tech"), name("tech"), name("art")]
                .into_iter()
                .collect();
        assert_eq!(set.len(), 2);
        assert_eq!(set.first().unwrap().canonical(), "art");
    }

    #[test]
    fn into_canonical_gives_the_canonical_spelling() {
        let n = name("Meta/Governance").into_canonical();
        assert!(n.is_canonical());
        assert_eq!(n.as_given(), "meta-governance");
        assert!(!name("Meta/Governance").is_canonical());
    }

    #[cfg(feature = "schemars")]
    #[test]
    fn schema_is_an_inline_string() {
        let schema = schemars::schema_for!(CommunityName);
        let v = serde_json::to_value(&schema).unwrap();
        assert_eq!(v["type"], "string");
        assert!(v.get("pattern").is_none());
        assert!(v.get("$ref").is_none() && v.get("$defs").is_none());
    }
    fn agent(s: &str) -> AgentName {
        s.parse().unwrap()
    }

    #[test]
    fn agent_name_rule() {
        for ok in ["abc", "claude-opus", "Oracle", "a_b-9", &"a".repeat(64)] {
            assert_eq!(agent(ok).as_str(), ok);
        }
        for bad in [
            "", "ab", "   ", "a b c", "@oracle", "oracle ", "naïve", "a/b",
        ] {
            assert!(bad.parse::<AgentName>().is_err(), "{bad:?}");
        }
        assert!("a".repeat(65).parse::<AgentName>().is_err());
        assert_eq!(
            "a b c".parse::<AgentName>(),
            Err(AgentNameError::Character(' '))
        );
        // Every way in is the same rule
        assert!(AgentName::try_from("ab".to_string()).is_err());
        assert!(AgentName::try_from("ab").is_err());
        assert!(serde_json::from_str::<AgentName>("\"ab\"").is_err());
        assert_eq!(String::from(agent("abc")), "abc");
    }

    /// `Oracle` and `oracle` are two agents: equality, hashing and order
    /// are byte-exact, so a map keeps both
    #[test]
    fn agent_name_compares_exactly() {
        assert_ne!(agent("Oracle"), agent("oracle"));
        assert!(agent("Oracle").eq_ignore_case(&agent("oracle")));
        let set: std::collections::HashSet<_> =
            [agent("Oracle"), agent("oracle")].into_iter().collect();
        assert_eq!(set.len(), 2);
        let map: std::collections::BTreeMap<AgentName, u8> =
            [(agent("Oracle"), 1), (agent("oracle"), 2)]
                .into_iter()
                .collect();
        assert_eq!(map.get("oracle"), Some(&2));
        assert_eq!(map.get("ORACLE"), None);
        assert!(agent("Oracle") < agent("oracle"));
        assert_eq!(agent("oracle"), "oracle");
        assert_ne!(agent("oracle"), "Oracle");
    }

    #[test]
    fn agent_name_serializes_as_the_plain_string() {
        for given in ["claude-opus", "Oracle", "a_b-9"] {
            let json = serde_json::to_string(&agent(given)).unwrap();
            assert_eq!(json, serde_json::to_string(given).unwrap());
            let back: AgentName = serde_json::from_str(&json).unwrap();
            assert_eq!(back, agent(given));
        }
    }

    /// Inside a tool input, as the tools use it: no `$ref`, no `$defs`, no
    /// `pattern`, the field a plain string
    #[cfg(feature = "schemars")]
    #[test]
    fn agent_name_field_schema_is_an_inline_string() {
        #[derive(schemars::JsonSchema)]
        #[allow(dead_code)]
        struct Input {
            /// The field's own words
            agent: AgentName,
            maybe: Option<AgentName>,
            many: Vec<AgentName>,
        }
        let v = serde_json::to_value(schemars::schema_for!(Input)).unwrap();
        let text = v.to_string();
        assert!(!text.contains("$ref") && !text.contains("$defs"), "{text}");
        assert!(!text.contains("pattern"), "{text}");
        assert_eq!(v["properties"]["agent"]["type"], "string", "{text}");
        // A field's own doc wins; a bare field gets the type's
        assert_eq!(
            v["properties"]["agent"]["description"], "The field's own words",
            "{text}"
        );
        assert!(
            v["properties"]["many"]["items"]["description"]
                .as_str()
                .is_some_and(|d| d.contains("3 to 64")),
            "{text}"
        );
        assert_eq!(
            v["properties"]["many"]["items"]["type"], "string",
            "{text}"
        );
    }

    #[cfg(feature = "schemars")]
    #[test]
    fn agent_name_schema_is_an_inline_string() {
        let v = serde_json::to_value(schemars::schema_for!(AgentName)).unwrap();
        assert_eq!(v["type"], "string");
        assert!(v.get("pattern").is_none());
        assert!(v.get("$ref").is_none() && v.get("$defs").is_none());
    }
}
