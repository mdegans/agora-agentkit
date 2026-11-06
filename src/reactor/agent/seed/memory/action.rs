//! The runner's `memory` tool: its input, [`MemoryInput`] (one
//! [`MemoryAction`]), and its definition. The runner implements the tool;
//! this is what a model reads.

use misanthropic::tool::{CustomMethodDef, MethodDef};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::id::HistoryRef;
use crate::enums::SearchMode;
use crate::one_or_many::OneOrMany;
use crate::requests::GetContentInput;

/// The tool's name
pub const MEMORY_TOOL_NAME: &str = "memory";

/// `log`'s versions when no `limit` is given
pub const LOG_DEFAULT_LIMIT: u32 = 20;

/// The most versions `log` lists
pub const LOG_MAX_LIMIT: u32 = 50;

/// The tool's description: the same bytes for every agent
pub const MEMORY_TOOL_DESCRIPTION: &str = "Read your memory's history: \
     the versions your runner has kept of the memory you rewrite at the end \
     of each session. Each version has a memory id (mem-…), and a sealed \
     version has a head too. `id` takes either: a memory id or a sealed \
     head, at least 6 hex digits of it. A memory id names one memory text; \
     the one in your Memory heading is the memory in your context now. Any \
     change, including writing an id into your memory, gives the next \
     version a new id. Old ids keep working.\n\n\
     `log` lists your versions, newest first. `show` is like `git log -p`: \
     newest first, anchored on the memory in your context, and each older \
     version you ask for is shown as what changed from it to the next newer \
     one (`+` lines were added in the newer). `diff` compares two versions, \
     or one with your current memory. `search` finds the lines in any \
     version that contain your words.\n\n\
     Call it with one request, as in {\"request\": {\"action\": \"show\", \
     \"id\": \"mem-3f9a1c07be\"}}. Each result counts toward your context \
     like any other read: one that would not fit is left out, and you are \
     told its size.";

/// The `memory` tool's input: one request. An object at the root, since
/// Anthropic refuses a tool whose input schema is a bare union
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(inline)]
pub struct MemoryInput {
    /// What to do: `log`, `show`, `diff` or `search`, in `action`, with
    /// that action's fields beside it
    pub request: MemoryAction,
}

/// A `memory` call that could not be read, in words that say what the
/// tool takes
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct MemoryInputError(String);

/// What every [`MemoryInputError`] ends with: the call's shape
const CALL_SHAPE: &str = "Call it with one request, as in {\"request\": \
     {\"action\": \"log\"}} or {\"request\": {\"action\": \"show\", \"id\": \
     \"mem-3f9a1c07be\"}}; the actions are log, show, diff and search.";

impl MemoryInput {
    /// The tool's input schema, as a model reads it: an object whose
    /// `request` is the actions' `anyOf`, with no `oneOf`, `$ref` or `$defs`
    pub fn schema() -> serde_json::Value {
        crate::tools::tool_input_schema::<Self>()
    }

    /// The tool's one definition. Not `strict`
    pub fn definition() -> MethodDef {
        let mut def =
            CustomMethodDef::simple(MEMORY_TOOL_NAME, MEMORY_TOOL_DESCRIPTION);
        def.schema = Self::schema();
        MethodDef::Custom(def)
    }

    /// Read a call's input. A bare request (`{"action": …}`, no
    /// `request`) is read as one. A call shaped like Anthropic's memory
    /// tool (`command`, `path`) is told this tool is not that one
    pub fn parse(
        input: serde_json::Value,
    ) -> Result<MemoryAction, MemoryInputError> {
        let fail =
            |what: String| MemoryInputError(format!("{what} {CALL_SHAPE}"));
        let map = input.as_object();
        if map.is_some_and(|m| {
            !m.contains_key("request")
                && (m.contains_key("command") || m.contains_key("path"))
        }) {
            return Err(fail(
                "This memory tool reads your own memory's history; it has \
                 no files, paths or `command`."
                    .into(),
            ));
        }
        if map.is_some_and(|m| m.contains_key("action")) {
            return serde_json::from_value(input).map_err(|e| {
                fail(format!("The request could not be read: {e}."))
            });
        }
        serde_json::from_value::<Self>(input)
            .map(|i| i.request)
            .map_err(|e| fail(format!("The call could not be read: {e}.")))
    }
}

/// One request to the `memory` tool, by its `action`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case")]
#[schemars(inline)]
pub enum MemoryAction {
    /// List your memory's versions, newest first: date, memory id, head,
    /// and size
    Log {
        /// Start below this version, a memory id or a head; leave it out
        /// to start from the newest
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::serde_forgiving::forgiving_option"
        )]
        before: Option<HistoryRef>,
        /// How many versions to list: 20 if left out, at most 50
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::serde_forgiving::forgiving_option_u32"
        )]
        #[schemars(with = "Option<u32>", range(min = 1, max = LOG_MAX_LIMIT))]
        limit: Option<u32>,
    },
    /// Show versions like `git log -p`: one id shows that version whole;
    /// several show the newest first and each older one as what changed
    /// from it to the next newer one
    Show {
        /// A memory id or a head, or a list of them
        id: OneOrMany<HistoryRef>,
        /// The most to show, in bytes (default 32,768, at least 4,096, at
        /// most 262,144). Versions past it are listed by id and size, never
        /// cut
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::serde_forgiving::forgiving_option_u32"
        )]
        #[schemars(
            with = "Option<u32>",
            range(
                min = GetContentInput::MIN_COMMENT_BUDGET,
                max = GetContentInput::MAX_COMMENT_BUDGET
            )
        )]
        budget: Option<u32>,
    },
    /// What changed from version `a` to version `b`, whole
    Diff {
        /// The version to compare from, a memory id or a head
        a: HistoryRef,
        /// The version to compare to; leave it out to compare with your
        /// current memory
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::serde_forgiving::forgiving_option"
        )]
        b: Option<HistoryRef>,
    },
    /// Find the lines, in any version, that contain every word of `query`,
    /// in any case
    Search {
        /// The words to find
        query: String,
        /// `keyword`, the default; `semantic` searches by keyword too, for
        /// now
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::serde_forgiving::forgiving_option"
        )]
        mode: Option<SearchMode>,
    },
}

/// `log`'s `limit`, defaulted and clamped to `1..=`[`LOG_MAX_LIMIT`]
pub fn log_limit(limit: Option<u32>) -> usize {
    limit.unwrap_or(LOG_DEFAULT_LIMIT).clamp(1, LOG_MAX_LIMIT) as usize
}

/// `show`'s `budget` in bytes, defaulted and clamped as `get_content`'s
/// `comment_budget` is
pub fn show_budget(budget: Option<u32>) -> usize {
    budget
        .unwrap_or(GetContentInput::DEFAULT_COMMENT_BUDGET)
        .clamp(
            GetContentInput::MIN_COMMENT_BUDGET,
            GetContentInput::MAX_COMMENT_BUDGET,
        ) as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_schema_has_no_one_of_ref_or_defs() {
        let schema = MemoryInput::schema();
        let text = schema.to_string();
        for banned in ["oneOf", "$ref", "$defs", "definitions", "pattern"] {
            assert!(
                !text.contains(&format!("\"{banned}\"")),
                "{banned}: {text}"
            );
        }
        // Anthropic refuses a bare union at the root
        assert_eq!(schema["type"], "object");
        for union in ["anyOf", "oneOf", "allOf"] {
            assert!(schema.get(union).is_none(), "{union}: {schema}");
        }
        assert_eq!(schema["required"], json!(["request"]));
        assert_eq!(schema["additionalProperties"], false);
        let actions = schema["properties"]["request"]["anyOf"]
            .as_array()
            .expect("anyOf");
        let names: Vec<&str> = actions
            .iter()
            .map(|a| {
                a["properties"]["action"]["const"]
                    .as_str()
                    .or_else(|| a["properties"]["action"]["enum"][0].as_str())
                    .expect("an action constant")
            })
            .collect();
        assert_eq!(names, ["log", "show", "diff", "search"]);
        for a in actions {
            let required = a["required"].as_array().unwrap();
            assert!(required.contains(&json!("action")), "{a}");
        }
        assert!(crate::tools::reasoning_keys_in(&schema).is_empty());
        // `id` is one or a list; `mode` the search modes
        let show = &actions[1]["properties"];
        assert_eq!(show["id"]["type"], "array");
        assert_eq!(show["id"]["items"]["type"], "string");
        assert_eq!(
            actions[3]["properties"]["mode"]["enum"],
            json!(["keyword", "semantic"])
        );
    }

    #[test]
    fn the_definition_is_not_strict() {
        let MethodDef::Custom(def) = MemoryInput::definition() else {
            panic!("custom");
        };
        assert_eq!(def.name, MEMORY_TOOL_NAME);
        assert_ne!(def.strict, Some(true));
        assert!(
            crate::tools::developer_text(MEMORY_TOOL_DESCRIPTION).is_empty()
        );
        for must in [
            "memory id (mem-…)",
            "sealed head",
            "names one memory text",
            "writing an id into your memory",
            "Old ids keep working",
            "like `git log -p`: newest first, anchored on the memory in your context",
            "{\"request\": {\"action\": \"show\"",
            "counts toward your context",
        ] {
            assert!(MEMORY_TOOL_DESCRIPTION.contains(must), "{must}");
        }
    }

    #[test]
    fn actions_read_and_round_trip() {
        let cases = [
            (
                json!({"action": "log"}),
                MemoryAction::Log {
                    before: None,
                    limit: None,
                },
            ),
            (
                json!({"action": "log", "before": "mem-3f9a1c", "limit": "10"}),
                MemoryAction::Log {
                    before: Some("mem-3f9a1c".parse().unwrap()),
                    limit: Some(10),
                },
            ),
            (
                json!({"action": "show", "id": "346ac9543d0c"}),
                MemoryAction::Show {
                    id: OneOrMany::One("346ac9543d0c".parse().unwrap()),
                    budget: None,
                },
            ),
            (
                json!({"action": "show", "id": ["mem-aaaaaa", "mem-bbbbbb"], "budget": 4096}),
                MemoryAction::Show {
                    id: OneOrMany::Many(vec![
                        "mem-aaaaaa".parse().unwrap(),
                        "mem-bbbbbb".parse().unwrap(),
                    ]),
                    budget: Some(4096),
                },
            ),
            (
                json!({"action": "diff", "a": "mem-aaaaaa", "b": "null"}),
                MemoryAction::Diff {
                    a: "mem-aaaaaa".parse().unwrap(),
                    b: None,
                },
            ),
            (
                json!({"action": "search", "query": "anchor", "mode": "keyword"}),
                MemoryAction::Search {
                    query: "anchor".into(),
                    mode: Some(SearchMode::Keyword),
                },
            ),
        ];
        for (input, want) in cases {
            let got: MemoryAction =
                serde_json::from_value(input.clone()).unwrap();
            assert_eq!(got, want, "{input}");
            // `id` always writes a list
            let written = serde_json::to_value(&got).unwrap();
            let back: MemoryAction =
                serde_json::from_value(written.clone()).unwrap();
            assert_eq!(serde_json::to_value(&back).unwrap(), written);
        }
        assert!(
            serde_json::from_value::<MemoryAction>(
                json!({"action": "show", "id": "abc"})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<MemoryAction>(json!({"action": "rm"}))
                .is_err()
        );
    }

    /// The nested call, a bare request, and Anthropic's memory tool's shape
    #[test]
    fn calls_parse_or_say_what_the_tool_takes() {
        let log = MemoryAction::Log {
            before: None,
            limit: None,
        };
        assert_eq!(
            MemoryInput::parse(json!({"request": {"action": "log"}})).unwrap(),
            log
        );
        assert_eq!(MemoryInput::parse(json!({"action": "log"})).unwrap(), log);
        for shaped in [
            json!({"command": "view", "path": "/memories"}),
            json!({"command": "create", "path": "/memories/a.md", "file_text": "x"}),
        ] {
            let err = MemoryInput::parse(shaped).unwrap_err().to_string();
            assert!(err.contains("no files, paths or `command`"), "{err}");
            assert!(err.ends_with(CALL_SHAPE), "{err}");
        }
        for bad in [
            json!({"request": {"action": "rm"}}),
            json!({"action": "show"}),
            json!({"request": {"action": "log"}, "extra": 1}),
            json!("log"),
        ] {
            let err = MemoryInput::parse(bad.clone()).unwrap_err().to_string();
            assert!(err.ends_with(CALL_SHAPE), "{bad}: {err}");
        }
    }

    #[test]
    fn limits_and_budgets_clamp() {
        assert_eq!(log_limit(None), 20);
        assert_eq!(log_limit(Some(0)), 1);
        assert_eq!(log_limit(Some(500)), 50);
        assert_eq!(show_budget(None), 32_768);
        assert_eq!(show_budget(Some(1)), 4_096);
        assert_eq!(show_budget(Some(u32::MAX)), 262_144);
    }

    /// The numbers the field docs state are the constants'
    #[test]
    fn the_docs_state_the_constants() {
        let text = MemoryInput::schema().to_string();
        let n = |n: u32| match n {
            0..1000 => n.to_string(),
            n if n < 1_000_000 => format!("{},{:03}", n / 1000, n % 1000),
            n => n.to_string(),
        };
        for want in [
            format!("{LOG_DEFAULT_LIMIT} if left out, at most {LOG_MAX_LIMIT}"),
            format!("default {}", n(GetContentInput::DEFAULT_COMMENT_BUDGET)),
            format!("at least {}", n(GetContentInput::MIN_COMMENT_BUDGET)),
            format!("most {}", n(GetContentInput::MAX_COMMENT_BUDGET)),
        ] {
            assert!(text.contains(&want), "{want}: {text}");
        }
    }
}
