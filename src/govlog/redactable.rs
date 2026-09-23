//! A value in an entry's `data` that a redaction may have replaced

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::redaction_marker;
use crate::ids::{GovernanceLogId, GovernanceLogPrefix};

/// The `pattern` of a [`redaction_marker`]
pub const REDACTION_MARKER_PATTERN: &str =
    r"^\[redacted by AMD-[0-9]{4}-[0-9]{4}\]$";

/// A value in an entry's `data`, or the [`redaction_marker`] a redaction
/// left in its place.
///
/// On the wire it is `T` or the marker string; it serializes back to
/// exactly what it parsed from. A `T` that is itself a string reading
/// exactly like a marker parses as `Redacted`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Redactable<T> {
    Value(T),
    /// Replaced by the redaction amendment (`AMD-`) this names
    Redacted(GovernanceLogId),
}

impl<T> Redactable<T> {
    /// The value, unless it was redacted
    pub fn value(&self) -> Option<&T> {
        match self {
            Self::Value(v) => Some(v),
            Self::Redacted(_) => None,
        }
    }

    /// The amendment that redacted it, if one did
    pub fn redacted_by(&self) -> Option<&GovernanceLogId> {
        match self {
            Self::Value(_) => None,
            Self::Redacted(id) => Some(id),
        }
    }

    pub fn is_redacted(&self) -> bool {
        matches!(self, Self::Redacted(_))
    }

    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Redactable<U> {
        match self {
            Self::Value(v) => Redactable::Value(f(v)),
            Self::Redacted(id) => Redactable::Redacted(id),
        }
    }
}

impl<T> From<T> for Redactable<T> {
    fn from(value: T) -> Self {
        Self::Value(value)
    }
}

/// The amendment a [`redaction_marker`] names, if `s` is one
fn parse_marker(s: &str) -> Option<GovernanceLogId> {
    let inner = s.strip_prefix("[redacted by ")?.strip_suffix(']')?;
    // Exact: a marker is written canonically, so lenient citation parsing
    // (`AMD-2026-1`) must not turn look-alike text into a redaction.
    if !GovernanceLogId::is_citation_shaped(inner) {
        return None;
    }
    let id: GovernanceLogId = inner.parse().ok()?;
    (id.prefix() == GovernanceLogPrefix::Amd).then_some(id)
}

impl<T: Serialize> Serialize for Redactable<T> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Value(v) => v.serialize(s),
            Self::Redacted(id) => s.serialize_str(&redaction_marker(id)),
        }
    }
}

impl<'de, T: serde::de::DeserializeOwned> Deserialize<'de> for Redactable<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(d)?;
        if let Some(id) = value.as_str().and_then(parse_marker) {
            return Ok(Self::Redacted(id));
        }
        T::deserialize(value)
            .map(Self::Value)
            .map_err(serde::de::Error::custom)
    }
}

// Hand-written so it inlines: `anyOf [T, marker]`, never a `$ref`.
#[cfg(feature = "schemars")]
impl<T: schemars::JsonSchema> schemars::JsonSchema for Redactable<T> {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> std::borrow::Cow<'static, str> {
        format!("Redactable_{}", T::schema_name()).into()
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        format!("{}::Redactable<{}>", module_path!(), T::schema_id()).into()
    }

    fn json_schema(g: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let value = g.subschema_for::<T>();
        schemars::json_schema!({
            "anyOf": [
                value,
                {
                    "type": "string",
                    "pattern": REDACTION_MARKER_PATTERN,
                    "description": "Removed by the redaction amendment it names",
                },
            ],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn amd() -> GovernanceLogId {
        "AMD-2026-0009".parse().unwrap()
    }

    #[test]
    fn the_marker_parses_as_redacted_and_writes_back_the_same() {
        let marker = serde_json::Value::String(redaction_marker(&amd()));
        let text: Redactable<String> =
            serde_json::from_value(marker.clone()).unwrap();
        assert_eq!(text, Redactable::Redacted(amd()));
        assert_eq!(serde_json::to_value(&text).unwrap(), marker);

        let list: Redactable<Vec<u32>> =
            serde_json::from_value(marker.clone()).unwrap();
        assert_eq!(list.redacted_by(), Some(&amd()));
        assert_eq!(serde_json::to_value(&list).unwrap(), marker);
    }

    #[test]
    fn anything_else_is_a_value() {
        for s in [
            "[redacted by GOV-2026-0001]",
            "[redacted by AMD-2026-001]",
            "redacted by AMD-2026-0001",
            " [redacted by AMD-2026-0001]",
        ] {
            let v: Redactable<String> =
                serde_json::from_value(s.into()).unwrap();
            assert_eq!(v, Redactable::Value(s.to_string()));
        }
        assert!(serde_json::from_value::<Redactable<u32>>("x".into()).is_err());
    }

    #[test]
    fn the_pattern_matches_the_marker() {
        let p = REDACTION_MARKER_PATTERN;
        let body = &p[1..p.len() - 1];
        let marker = redaction_marker(&amd());
        // No regex dependency: the pattern's fixed text plus the id pattern
        assert_eq!(
            body.replace(r"\[", "[")
                .replace(r"\]", "]")
                .replace("[0-9]{4}-[0-9]{4}", "2026-0009"),
            marker
        );
    }

    #[cfg(feature = "schemars")]
    #[test]
    fn schema_is_ref_free() {
        let text = crate::responses::inline_schema_for::<
            Redactable<Vec<Redactable<String>>>,
        >()
        .to_string();
        assert!(!text.contains("$ref"), "{text}");
        assert!(!text.contains("$defs"), "{text}");
        assert!(text.contains("anyOf"), "{text}");
    }
}
