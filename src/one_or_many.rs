//! A tool argument that takes one value or several (0.126.0).

use serde::de::{DeserializeOwned, Error as _};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// One `T`, or a non-empty list of them.
///
/// Reads a bare `T` or an array, and always writes an array, which is the
/// only form the schema shows. An empty array is refused, and so is the
/// array a small model sometimes sends as a string (`"[\"ada\"]"`) unless it
/// parses as one, when it is read as the array it spells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OneOrMany<T> {
    One(T),
    Many(Vec<T>),
}

impl<T> OneOrMany<T> {
    /// The values, in order
    pub fn into_vec(self) -> Vec<T> {
        match self {
            Self::One(t) => vec![t],
            Self::Many(v) => v,
        }
    }

    /// The values, in order
    pub fn iter(&self) -> std::slice::Iter<'_, T> {
        self.as_slice().iter()
    }

    /// The values, in order
    pub fn as_slice(&self) -> &[T] {
        match self {
            Self::One(t) => std::slice::from_ref(t),
            Self::Many(v) => v,
        }
    }

    pub fn len(&self) -> usize {
        self.as_slice().len()
    }

    /// Only for a `Many` built empty in code: deserializing refuses one
    pub fn is_empty(&self) -> bool {
        self.as_slice().is_empty()
    }
}

impl<T> From<T> for OneOrMany<T> {
    fn from(t: T) -> Self {
        Self::One(t)
    }
}

impl<T> From<Vec<T>> for OneOrMany<T> {
    fn from(v: Vec<T>) -> Self {
        Self::Many(v)
    }
}

impl<'a, T> IntoIterator for &'a OneOrMany<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<T: Serialize> Serialize for OneOrMany<T> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.as_slice().serialize(s)
    }
}

impl<'de, T: DeserializeOwned> Deserialize<'de> for OneOrMany<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde_json::Value;

        let value = match Value::deserialize(d)? {
            Value::String(s) if s.trim_start().starts_with('[') => {
                match serde_json::from_str::<Value>(&s) {
                    Ok(list @ Value::Array(_)) => list,
                    _ => Value::String(s),
                }
            }
            other => other,
        };
        match value {
            Value::Array(items) if items.is_empty() => {
                Err(D::Error::custom("an empty list; give at least one value"))
            }
            Value::Array(items) => items
                .into_iter()
                .enumerate()
                .map(|(i, item)| {
                    T::deserialize(item)
                        .map_err(|e| D::Error::custom(format!("item {i}: {e}")))
                })
                .collect::<Result<Vec<_>, _>>()
                .map(Self::Many),
            one => T::deserialize(one).map(Self::One).map_err(D::Error::custom),
        }
    }
}

// The array form only, inline: a tool schema never carries a `$ref`
// (agora CLAUDE.md), so `T`'s schema is written out rather than referenced
#[cfg(feature = "schemars")]
impl<T: schemars::JsonSchema> schemars::JsonSchema for OneOrMany<T> {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> std::borrow::Cow<'static, str> {
        format!("OneOrMany_{}", T::schema_name()).into()
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        format!("{}::OneOrMany<{}>", module_path!(), T::schema_id()).into()
    }

    fn json_schema(
        generator: &mut schemars::SchemaGenerator,
    ) -> schemars::Schema {
        let items = T::json_schema(generator);
        schemars::json_schema!({
            "type": "array",
            "items": items,
            "minItems": 1,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::names::AgentName;
    use serde_json::json;

    type Names = OneOrMany<AgentName>;

    fn name(s: &str) -> AgentName {
        s.parse().unwrap()
    }

    #[test]
    fn reads_one_or_a_list_and_writes_a_list() {
        let one: Names = serde_json::from_value(json!("ada")).unwrap();
        assert_eq!(one, OneOrMany::One(name("ada")));
        assert_eq!(serde_json::to_value(&one).unwrap(), json!(["ada"]));

        let many: Names =
            serde_json::from_value(json!(["ada", "bob"])).unwrap();
        assert_eq!(many, OneOrMany::Many(vec![name("ada"), name("bob")]));
        assert_eq!(serde_json::to_value(&many).unwrap(), json!(["ada", "bob"]));
        assert_eq!(many.len(), 2);
        assert_eq!(many.iter().count(), 2);
        assert_eq!(many.into_vec(), vec![name("ada"), name("bob")]);

        // A list written as a string is read as the list it spells
        let quoted: Names =
            serde_json::from_value(json!("[\"ada\", \"bob\"]")).unwrap();
        assert_eq!(quoted.len(), 2);
    }

    #[test]
    fn an_empty_list_and_a_bad_item_are_refused() {
        let err = serde_json::from_value::<Names>(json!([])).unwrap_err();
        assert!(err.to_string().contains("empty list"), "{err}");
        let err =
            serde_json::from_value::<Names>(json!(["ada", "b c"])).unwrap_err();
        assert!(err.to_string().starts_with("item 1:"), "{err}");
        assert!(serde_json::from_value::<Names>(json!("[ada")).is_err());
        assert!(serde_json::from_value::<Names>(json!(7)).is_err());
    }

    #[test]
    fn from_and_empty() {
        assert_eq!(Names::from(name("ada")).len(), 1);
        assert!(Names::from(Vec::new()).is_empty());
    }

    #[cfg(feature = "schemars")]
    #[test]
    fn the_schema_is_an_inline_array() {
        let schema =
            serde_json::to_value(schemars::schema_for!(Names)).unwrap();
        let text = schema.to_string();
        assert!(!text.contains("$ref") && !text.contains("$defs"), "{text}");
        assert_eq!(schema["type"], "array");
        assert_eq!(schema["items"]["type"], "string");
        assert_eq!(schema["minItems"], 1);
    }
}
