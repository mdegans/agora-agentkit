//! `VoteValue`'s variants, written and read by hand. Reading is lenient
//! where the schema is exact: `"up"`, `"1"` and `1.0` are `1`, and a
//! ballot's numbers may be strings (`"#4"`) or integral floats

use serde::{Deserialize, Deserializer, Serializer};

use crate::ids::PollOptionNumber;

pub(crate) fn one<S: Serializer>(s: S) -> Result<S::Ok, S::Error> {
    s.serialize_i8(1)
}

pub(crate) fn minus_one<S: Serializer>(s: S) -> Result<S::Ok, S::Error> {
    s.serialize_i8(-1)
}

/// `value` as an integer, from a number or a string of one
fn integer(value: &serde_json::Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| {
            value
                .as_f64()
                .filter(|f| f.fract() == 0.0 && f.abs() < 1e9)
                .map(|f| f as i64)
        })
        .or_else(|| {
            value
                .as_str()
                .and_then(|s| s.trim().trim_start_matches('#').parse().ok())
        })
}

fn exactly<'de, D: Deserializer<'de>>(
    d: D,
    want: i64,
    word: &str,
) -> Result<(), D::Error> {
    use serde::de::Error; // needed for custom

    let value = serde_json::Value::deserialize(d)?;
    let is_word = value
        .as_str()
        .is_some_and(|s| s.trim().eq_ignore_ascii_case(word));
    if is_word || integer(&value) == Some(want) {
        Ok(())
    } else {
        Err(D::Error::custom("not this vote"))
    }
}

pub(crate) fn is_one<'de, D: Deserializer<'de>>(d: D) -> Result<(), D::Error> {
    exactly(d, 1, "up")
}

pub(crate) fn is_minus_one<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<(), D::Error> {
    exactly(d, -1, "down")
}

pub(crate) fn ballot<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<Vec<PollOptionNumber>, D::Error> {
    use serde::de::Error;

    Vec::<serde_json::Value>::deserialize(d)?
        .iter()
        .map(|n| {
            integer(n)
                .and_then(|n| i16::try_from(n).ok())
                .map(PollOptionNumber::from)
                .ok_or_else(|| D::Error::custom("not an option number"))
        })
        .collect()
}
