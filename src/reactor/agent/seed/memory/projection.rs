//! The texts a seed state's JSON is compared by: its memory and its soul.
//! One copy for the runner's notices and the `memory` tool.

use std::borrow::Cow;

/// The memory a state holds (`memory.content`), raw: empty when it holds
/// none
pub fn memory_text(state: &serde_json::Value) -> &str {
    state
        .pointer("/memory/content")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
}

/// The soul a state holds, as the canonical JSON sealing uses (keys
/// sorted), pretty-printed: a soul whose keys were only reordered reads as
/// unchanged. Empty when it holds none
pub fn soul_text(state: &serde_json::Value) -> Cow<'static, str> {
    // serde_json keeps insertion order in this graph (`preserve_order`),
    // so printing the value as it is would show a reordered soul changed
    state
        .get("soul")
        .map(|s| {
            let canonical = crate::govlog::canonical_json(s);
            serde_json::from_slice::<serde_json::Value>(&canonical)
                .and_then(|c| serde_json::to_string_pretty(&c))
                .unwrap_or_default()
                .into()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn memory_and_soul_project() {
        let a = json!({"memory": {"content": "m"}, "soul": {"b": 1, "a": 2}});
        let b = json!({"soul": {"a": 2, "b": 1}, "memory": {"content": "m"}});
        assert_eq!(memory_text(&a), "m");
        assert_eq!(soul_text(&a), soul_text(&b));
        assert!(soul_text(&a).find("\"a\"") < soul_text(&a).find("\"b\""));
        assert_eq!(memory_text(&json!({})), "");
        assert_eq!(soul_text(&json!({})), "");
    }
}
