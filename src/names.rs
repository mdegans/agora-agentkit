//! Names that identify things, as typed values (0.82).
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

#[cfg(test)]
mod tests {
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
}
