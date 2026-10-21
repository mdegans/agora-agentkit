//! Forgiving serde helpers for LLM tool inputs.
//!
//! Small models occasionally produce inputs where an [`Option`] field is set
//! to the JSON string `"null"` instead of the JSON value `null`, or where a
//! numeric field is set to a JSON string like `"10"` instead of the JSON
//! number `10`. Every other serde error is worth relaying to the agent so
//! it can self-correct, but these particular footguns are common enough and
//! harmless enough that it's cheaper to paper over them than to burn a turn.

use serde::{Deserialize, Deserializer};

/// Report a coercion: a value that was not what the schema asks for, read
/// as if it were (a `"null"` string as `None`, `"10"` as `10`).
///
/// A no-op unless the `forgiving_coercion` feature is on. With it, each
/// coercion is a `warn` on the `forgiving_coercion` target, naming the raw
/// value and the type it was read as. Grammar-constrained decoders
/// (blallama) never emit these shapes, so with the feature on, any warning
/// is a bug to find (Steward, 2026-10-14). serde does not tell a
/// `deserialize_with` function which field it is reading; the enclosing
/// span (the tool call) says where.
#[inline]
#[allow(unused_variables)]
fn coerced(raw: &serde_json::Value, into: &'static str) {
    #[cfg(feature = "forgiving_coercion")]
    tracing::warn!(
        target: "forgiving_coercion",
        raw = %raw,
        into,
        "forgiving deserializer coerced a value"
    );
}

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
        Some(raw @ serde_json::Value::String(_))
            if raw.as_str().is_some_and(|s| s == "null" || s.is_empty()) =>
        {
            coerced(&raw, "None");
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
                coerced(&serde_json::Value::String(s), "None");
                return Ok(None);
            }
            let n = trimmed.parse::<u64>().map_err(|e| {
                D::Error::custom(format!("expected u64, got string {s:?}: {e}"))
            })?;
            coerced(&serde_json::Value::String(s), "u64");
            Ok(Some(n))
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
                    coerced(&serde_json::Value::Number(n), "u64");
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
        Some(serde_json::Value::String(s)) => {
            let read = match s.trim() {
                "" | "null" => None,
                "true" => Some(true),
                "false" => Some(false),
                _ => {
                    return Err(D::Error::custom(format!(
                        "expected a boolean, got string {s:?}"
                    )));
                }
            };
            coerced(
                &serde_json::Value::String(s),
                if read.is_some() { "bool" } else { "None" },
            );
            Ok(read)
        }
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

    /// With `forgiving_coercion` on, every coercion is a warning on its
    /// target, and a value that needed none is not
    #[cfg(feature = "forgiving_coercion")]
    #[test]
    fn coercions_warn_when_the_feature_is_on() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        /// Counts events on the `forgiving_coercion` target
        struct Count(Arc<AtomicUsize>);
        impl tracing::Subscriber for Count {
            fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
                true
            }
            fn new_span(
                &self,
                _: &tracing::span::Attributes<'_>,
            ) -> tracing::span::Id {
                tracing::span::Id::from_u64(1)
            }
            fn record(
                &self,
                _: &tracing::span::Id,
                _: &tracing::span::Record<'_>,
            ) {
            }
            fn record_follows_from(
                &self,
                _: &tracing::span::Id,
                _: &tracing::span::Id,
            ) {
            }
            fn event(&self, event: &tracing::Event<'_>) {
                if event.metadata().target() == "forgiving_coercion" {
                    self.0.fetch_add(1, Ordering::SeqCst);
                }
            }
            fn enter(&self, _: &tracing::span::Id) {}
            fn exit(&self, _: &tracing::span::Id) {}
        }

        #[derive(Deserialize)]
        struct W {
            #[serde(default, deserialize_with = "forgiving_option")]
            s: Option<String>,
            #[serde(default, deserialize_with = "forgiving_option_u64")]
            n: Option<u64>,
            #[serde(default, deserialize_with = "forgiving_option_bool")]
            b: Option<bool>,
        }
        let count = Arc::new(AtomicUsize::new(0));
        // Another test's thread can cache a callsite's interest as "never"
        // while no subscriber is set; rebuilt under this one, the warn is
        // seen (the flake of 0.113.0's betas, 1 run in 8)
        let read = |json: &str| {
            tracing::subscriber::with_default(Count(count.clone()), || {
                tracing::callsite::rebuild_interest_cache();
                serde_json::from_str::<W>(json).unwrap()
            })
        };
        read(r#"{"s": "x", "n": 3, "b": true}"#);
        read(r#"{}"#);
        read(r#"{"s": null, "n": null, "b": null}"#);
        assert_eq!(
            count.load(Ordering::SeqCst),
            0,
            "native values coerce nothing"
        );
        let w = read(r#"{"s": "null", "n": "10", "b": "false"}"#);
        assert_eq!((w.s, w.n, w.b), (None, Some(10), Some(false)));
        assert_eq!(count.load(Ordering::SeqCst), 3);
        read(r#"{"s": "", "n": 4.0, "b": ""}"#);
        assert_eq!(count.load(Ordering::SeqCst), 6);
    }
}
