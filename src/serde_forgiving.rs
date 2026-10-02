//! Forgiving serde helpers for LLM tool inputs.
//!
//! Small models occasionally produce inputs where an [`Option`] field is set
//! to the JSON string `"null"` instead of the JSON value `null`, or where a
//! numeric field is set to a JSON string like `"10"` instead of the JSON
//! number `10`. Every other serde error is worth relaying to the agent so
//! it can self-correct, but these particular footguns are common enough and
//! harmless enough that it's cheaper to paper over them than to burn a turn.

use serde::{Deserialize, Deserializer};

/// Deserialize an [`Option<T>`] while tolerating the string `"null"` and the
/// empty string as synonyms for `None`.
///
/// Use with `#[serde(default, deserialize_with = "forgiving_option")]`.
///
/// Valid inputs:
/// - JSON `null` → `None`
/// - field omitted (when paired with `#[serde(default)]`) → `None`
/// - JSON string `"null"` or `""` → `None`
/// - any other value → delegated to `T::deserialize`
pub fn forgiving_option<'de, D, T>(
    deserializer: D,
) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    match value {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(s)) if s == "null" || s.is_empty() => {
            Ok(None)
        }
        Some(other) => T::deserialize(other)
            .map(Some)
            .map_err(serde::de::Error::custom),
    }
}

/// Deserialize an [`Option<u64>`] while tolerating stringified numbers.
///
/// Use with `#[serde(default, deserialize_with = "forgiving_option_u64")]`.
///
/// Valid inputs:
/// - JSON `null` → `None`
/// - field omitted (when paired with `#[serde(default)]`) → `None`
/// - JSON string `"null"` or `""` (possibly whitespace-padded) → `None`
/// - JSON string containing a non-negative integer (e.g. `"10"`, `" 42 "`) → parsed
/// - JSON number that fits in `u64` (including whole-valued floats like `10.0`) → converted
/// - anything else → error
///
/// Observed in the wild from `cogito:14b` and `qwen3.5:35b`, which sometimes
/// emit `{"limit": "10"}` instead of `{"limit": 10}` for tool parameters.
pub fn forgiving_option_u64<'de, D>(
    deserializer: D,
) -> Result<Option<u64>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error;

    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    match value {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(s)) => {
            let trimmed = s.trim();
            if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("null") {
                return Ok(None);
            }
            trimmed.parse::<u64>().map(Some).map_err(|e| {
                D::Error::custom(format!("expected u64, got string {s:?}: {e}"))
            })
        }
        Some(serde_json::Value::Number(n)) => {
            if let Some(u) = n.as_u64() {
                Ok(Some(u))
            } else if let Some(f) = n.as_f64() {
                if f.is_finite()
                    && f >= 0.0
                    && f.fract() == 0.0
                    && f <= u64::MAX as f64
                {
                    Ok(Some(f as u64))
                } else {
                    Err(D::Error::custom(format!(
                        "expected non-negative integer, got {f}"
                    )))
                }
            } else {
                Err(D::Error::custom("expected u64-compatible number"))
            }
        }
        Some(other) => Err(D::Error::custom(format!(
            "expected u64, got {}",
            match other {
                serde_json::Value::Bool(_) => "boolean",
                serde_json::Value::Array(_) => "array",
                serde_json::Value::Object(_) => "object",
                _ => "unknown",
            }
        ))),
    }
}

/// [`forgiving_option_u64`] for a `u32`: out of range is an error, not a
/// truncation.
///
/// Use with `#[serde(default, deserialize_with = "forgiving_option_u32")]`.
pub fn forgiving_option_u32<'de, D>(
    deserializer: D,
) -> Result<Option<u32>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error;

    forgiving_option_u64(deserializer)?
        .map(|n| {
            u32::try_from(n)
                .map_err(|_| D::Error::custom(format!("expected u32, got {n}")))
        })
        .transpose()
}

/// Deserialize an [`Option<bool>`] while tolerating `"true"` and `"false"`
/// as strings, as a query string carries them.
///
/// Use with `#[serde(default, deserialize_with = "forgiving_option_bool")]`.
/// `"null"` and `""` are `None`, like [`forgiving_option`]; any other
/// string is an error.
pub fn forgiving_option_bool<'de, D>(
    deserializer: D,
) -> Result<Option<bool>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error;

    match Option::<serde_json::Value>::deserialize(deserializer)? {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Bool(b)) => Ok(Some(b)),
        Some(serde_json::Value::String(s)) => match s.trim() {
            "" | "null" => Ok(None),
            "true" => Ok(Some(true)),
            "false" => Ok(Some(false)),
            _ => Err(D::Error::custom(format!(
                "expected a boolean, got string {s:?}"
            ))),
        },
        Some(other) => {
            Err(D::Error::custom(format!("expected a boolean, got {other}")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize, PartialEq)]
    struct Wrapper {
        #[serde(default, deserialize_with = "forgiving_option")]
        value: Option<u32>,
    }

    #[test]
    fn accepts_null() {
        let w: Wrapper = serde_json::from_str(r#"{"value": null}"#).unwrap();
        assert_eq!(w.value, None);
    }

    #[test]
    fn accepts_missing() {
        let w: Wrapper = serde_json::from_str(r#"{}"#).unwrap();
        assert_eq!(w.value, None);
    }

    #[test]
    fn accepts_string_null() {
        let w: Wrapper = serde_json::from_str(r#"{"value": "null"}"#).unwrap();
        assert_eq!(w.value, None);
    }

    #[test]
    fn accepts_empty_string() {
        let w: Wrapper = serde_json::from_str(r#"{"value": ""}"#).unwrap();
        assert_eq!(w.value, None);
    }

    #[test]
    fn accepts_valid_value() {
        let w: Wrapper = serde_json::from_str(r#"{"value": 42}"#).unwrap();
        assert_eq!(w.value, Some(42));
    }

    #[test]
    fn rejects_garbage() {
        let err: Result<Wrapper, _> =
            serde_json::from_str(r#"{"value": "not a number"}"#);
        assert!(err.is_err());
    }

    #[derive(Debug, Deserialize, PartialEq)]
    struct U64Wrapper {
        #[serde(default, deserialize_with = "forgiving_option_u64")]
        limit: Option<u64>,
    }

    #[test]
    fn u64_accepts_native_number() {
        let w: U64Wrapper = serde_json::from_str(r#"{"limit": 10}"#).unwrap();
        assert_eq!(w.limit, Some(10));
    }

    #[test]
    fn u64_accepts_stringified_number() {
        let w: U64Wrapper = serde_json::from_str(r#"{"limit": "10"}"#).unwrap();
        assert_eq!(w.limit, Some(10));
    }

    #[test]
    fn u64_accepts_padded_stringified_number() {
        let w: U64Wrapper =
            serde_json::from_str(r#"{"limit": " 42 "}"#).unwrap();
        assert_eq!(w.limit, Some(42));
    }

    #[test]
    fn u64_accepts_whole_float() {
        let w: U64Wrapper = serde_json::from_str(r#"{"limit": 10.0}"#).unwrap();
        assert_eq!(w.limit, Some(10));
    }

    #[test]
    fn u64_accepts_null() {
        let w: U64Wrapper = serde_json::from_str(r#"{"limit": null}"#).unwrap();
        assert_eq!(w.limit, None);
    }

    #[test]
    fn u64_accepts_missing() {
        let w: U64Wrapper = serde_json::from_str(r#"{}"#).unwrap();
        assert_eq!(w.limit, None);
    }

    #[test]
    fn u64_accepts_string_null() {
        let w: U64Wrapper =
            serde_json::from_str(r#"{"limit": "null"}"#).unwrap();
        assert_eq!(w.limit, None);
    }

    #[test]
    fn u64_accepts_string_null_mixed_case() {
        let w: U64Wrapper =
            serde_json::from_str(r#"{"limit": "NULL"}"#).unwrap();
        assert_eq!(w.limit, None);
    }

    #[test]
    fn u64_accepts_empty_string() {
        let w: U64Wrapper = serde_json::from_str(r#"{"limit": ""}"#).unwrap();
        assert_eq!(w.limit, None);
    }

    #[test]
    fn u64_accepts_whitespace_string() {
        let w: U64Wrapper =
            serde_json::from_str(r#"{"limit": "   "}"#).unwrap();
        assert_eq!(w.limit, None);
    }

    #[test]
    fn u64_accepts_zero() {
        let w: U64Wrapper = serde_json::from_str(r#"{"limit": 0}"#).unwrap();
        assert_eq!(w.limit, Some(0));
    }

    #[test]
    fn u64_accepts_large_number() {
        let w: U64Wrapper =
            serde_json::from_str(r#"{"limit": 18446744073709551615}"#).unwrap();
        assert_eq!(w.limit, Some(u64::MAX));
    }

    #[test]
    fn u64_rejects_negative_number() {
        let err: Result<U64Wrapper, _> =
            serde_json::from_str(r#"{"limit": -1}"#);
        assert!(err.is_err());
    }

    #[test]
    fn u64_rejects_negative_string() {
        let err: Result<U64Wrapper, _> =
            serde_json::from_str(r#"{"limit": "-1"}"#);
        assert!(err.is_err());
    }

    #[test]
    fn u64_rejects_fractional_number() {
        let err: Result<U64Wrapper, _> =
            serde_json::from_str(r#"{"limit": 1.5}"#);
        assert!(err.is_err());
    }

    #[test]
    fn u64_rejects_non_numeric_string() {
        let err: Result<U64Wrapper, _> =
            serde_json::from_str(r#"{"limit": "ten"}"#);
        assert!(err.is_err());
    }

    #[test]
    fn u64_rejects_boolean() {
        let err: Result<U64Wrapper, _> =
            serde_json::from_str(r#"{"limit": true}"#);
        assert!(err.is_err());
    }

    #[test]
    fn u64_rejects_array() {
        let err: Result<U64Wrapper, _> =
            serde_json::from_str(r#"{"limit": [10]}"#);
        assert!(err.is_err());
    }

    #[derive(Debug, Deserialize, PartialEq)]
    struct BoolWrapper {
        #[serde(default, deserialize_with = "forgiving_option_bool")]
        flag: Option<bool>,
    }

    #[test]
    fn bool_accepts_native_and_stringified() {
        let read =
            |s: &str| serde_json::from_str::<BoolWrapper>(s).map(|w| w.flag);
        assert_eq!(read(r#"{"flag": true}"#).unwrap(), Some(true));
        assert_eq!(read(r#"{"flag": "false"}"#).unwrap(), Some(false));
        assert_eq!(read(r#"{"flag": ""}"#).unwrap(), None);
        assert_eq!(read(r#"{}"#).unwrap(), None);
        assert!(read(r#"{"flag": "yes"}"#).is_err());
        assert!(read(r#"{"flag": 1}"#).is_err());
    }
}
