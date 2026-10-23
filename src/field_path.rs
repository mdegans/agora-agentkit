//! A deserialization error as an agent reads it: the path to the field it
//! is about, then serde's own message, without the parser's position
//! (0.117.2).
//!
//! serde_json says *what* went wrong in its own words, which the Steward
//! wants agents to read, but a nested field's error names no field
//! ("unknown variant `bogus`, expected …"), and one read from text ends
//! "at line 1 column 214", which a model cannot count. [`FieldError`]
//! keeps the message, drops only that position, and adds the path
//! `serde_path_to_error` tracked (`options.mode`, `items[2].id`). One copy,
//! for the runner's model-facing parses and the server's request bodies,
//! query strings and MCP tool arguments alike.

use std::fmt;

use serde::de::DeserializeOwned;

/// Where a value failed to deserialize, and serde's message for it
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldError {
    path: Option<String>,
    message: String,
}

impl FieldError {
    /// From a [`serde_path_to_error`] error over serde_json
    pub fn new(e: serde_path_to_error::Error<serde_json::Error>) -> Self {
        let path = e.path().to_string();
        Self {
            // `?` is a segment serde_path_to_error could not name: alone,
            // it says nothing a missing path does not
            path: (!matches!(path.as_str(), "" | "." | "?")).then_some(path),
            message: message(e.inner()),
        }
    }

    /// The field the error is about (`options.mode`), or `None` for the
    /// value as a whole
    pub fn path(&self) -> Option<&str> {
        self.path.as_deref()
    }

    /// serde's message, without the position
    pub fn message(&self) -> &str {
        &self.message
    }

    /// `lead`, then the path when there is one, then the message:
    /// "Invalid JSON at `soul.voice`: invalid type: …", or "Invalid JSON:
    /// …" for the value as a whole
    pub fn after(&self, lead: &str) -> String {
        match &self.path {
            Some(path) => format!("{lead} at `{path}`: {}", self.message),
            None => format!("{lead}: {}", self.message),
        }
    }
}

/// "at `path`: message", or the message alone for the value as a whole
impl fmt::Display for FieldError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.path {
            Some(path) => write!(f, "at `{path}`: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for FieldError {}

/// serde_json's message for `e` without the " at line L column C" it
/// appends when the error came from text (`line()` is 0 for one from a
/// `Value`, which has no position)
pub fn message(e: &serde_json::Error) -> String {
    let text = e.to_string();
    if e.line() == 0 {
        return text;
    }
    let position = format!(" at line {} column {}", e.line(), e.column());
    match text.strip_suffix(&position) {
        Some(message) => message.to_string(),
        None => text,
    }
}

/// [`serde_json::from_value`], failing with the field's path
pub fn from_value<T: DeserializeOwned>(
    value: serde_json::Value,
) -> Result<T, FieldError> {
    serde_path_to_error::deserialize(value).map_err(FieldError::new)
}

/// [`serde_json::from_str`], failing with the field's path and no
/// position
pub fn from_str<T: DeserializeOwned>(text: &str) -> Result<T, FieldError> {
    from_slice(text.as_bytes())
}

/// [`serde_json::from_slice`], failing with the field's path and no
/// position. Trailing characters after the value are an error, as there
pub fn from_slice<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, FieldError> {
    let mut de = serde_json::Deserializer::from_slice(bytes);
    let value =
        serde_path_to_error::deserialize(&mut de).map_err(FieldError::new)?;
    de.end().map_err(|e| FieldError {
        path: None,
        message: message(&e),
    })?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    struct Outer {
        options: Options,
        #[serde(default)]
        items: Vec<Item>,
    }

    #[derive(Debug, serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    struct Options {
        mode: Mode,
    }

    #[derive(Debug, serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum Mode {
        Full,
        Summary,
    }

    #[derive(Debug, serde::Deserialize)]
    #[allow(dead_code)]
    struct Item {
        id: u32,
    }

    /// A nested field's error names the field, in serde's own words, and
    /// text's errors carry no line or column (Steward, 2026-10-23)
    #[test]
    fn a_nested_error_names_its_path_and_no_position() {
        let text = r#"{"options": {"mode": "bogus"}}"#;
        let before = serde_json::from_str::<Outer>(text).unwrap_err();
        assert_eq!(
            before.to_string(),
            "unknown variant `bogus`, expected `full` or `summary` at line 1 \
             column 28"
        );
        let after = from_str::<Outer>(text).unwrap_err();
        assert_eq!(
            after.to_string(),
            "at `options.mode`: unknown variant `bogus`, expected `full` or \
             `summary`"
        );
        assert_eq!(after.path(), Some("options.mode"));
        // From a Value: no position to drop, the same path
        let value: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(from_value::<Outer>(value).unwrap_err(), after);
        // A sequence's index is in the path
        let err = from_str::<Outer>(
            r#"{"options": {"mode": "full"}, "items": [{"id": 1}, {"id": "x"}]}"#,
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "at `items[1].id`: invalid type: string \"x\", expected u32"
        );
    }

    /// The value as a whole has no path; syntax and trailing characters
    /// keep serde's message without the position
    #[test]
    fn whole_value_errors_keep_the_message_alone() {
        let err =
            from_str::<Outer>(r#"{"options": {"mode": "full"}"#).unwrap_err();
        assert_eq!(err.path(), None);
        assert_eq!(err.message(), "EOF while parsing an object");
        let err = from_str::<Outer>("7").unwrap_err();
        assert_eq!(err.path(), None);
        assert_eq!(
            err.message(),
            "invalid type: integer `7`, expected struct Outer"
        );
        assert_eq!(
            err.after("Invalid JSON"),
            format!("Invalid JSON: {}", err.message())
        );
        let err = from_str::<serde_json::Value>("{} x").unwrap_err();
        assert_eq!(err.to_string(), "trailing characters");
    }
}
