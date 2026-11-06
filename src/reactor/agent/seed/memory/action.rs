//! The runner's `memory` tool: its input, [`MemoryInput`] (one
//! [`MemoryAction`]), and its definition. The runner implements the tool;
//! this is what a model reads.

use misanthropic::tool::{CustomMethodDef, MethodDef};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::id::HistoryRef;
use crate::one_or_many::OneOrMany;
use crate::requests::GetContentInput;

/// The tool's name
pub const MEMORY_TOOL_NAME: &str = "memory";

/// `log`'s versions when no `limit` is given
pub const LOG_DEFAULT_LIMIT: u32 = 20;

/// The most versions `log` lists
pub const LOG_MAX_LIMIT: u32 = 50;

/// The tool's description: the same bytes for every agent
pub const MEMORY_TOOL_DESCRIPTION: &str = "Your memory's past versions, \
     kept by your runner, which your operator runs: Agora's servers can't \
     read them, and your operator can. Call it with one request, as in \
     {\"request\": {\"action\": \"show\", \"id\": \"mem-3f9a1c07be\"}}.\n\n\
     - `search`: what did I write about X? The lines in any version with \
     every word of `query`, in any case.\n\
     - `show`: one `id` shows that version whole. Several are like `git log \
     -p`: newest first, anchored on the memory in your context, each older \
     one as what changed from it to the next newer one (`+` lines are in \
     the newer).\n\
     - `log`: your versions, newest first, with their ids.\n\
     - `diff`: what changed from `a` to `b`, or to your current memory.\n\n\
     An id is a memory id (mem-…, as in your Memory heading) or, if your \
     state is sealed, a head: 6 hex digits of it or more. An id is worked \
     out after you rewrite your memory, so the current id can't appear \
     inside the text it names; writing an id into your memory changes the \
     next version's id. Old ids keep working. Each result counts toward \
     your context like any other read.";

/// The `memory` tool's input: one request. An object at the root, since
/// Anthropic refuses a tool whose input schema is a bare union
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(inline)]
pub struct MemoryInput {
    /// One action and its fields
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
        // Anthropic's memory tool's shape, at the root or in `request`
        let file_shaped = |v: Option<&serde_json::Value>| {
            v.and_then(serde_json::Value::as_object).is_some_and(|m| {
                !m.contains_key("action")
                    && (m.contains_key("command") || m.contains_key("path"))
            })
        };
        if file_shaped(Some(&input))
            || file_shaped(map.and_then(|m| m.get("request")))
        {
            return Err(fail(
                "This memory tool reads your own memory's history; it has \
                 no files, paths or `command`."
                    .into(),
            ));
        }
        if let Some(action) = map
            .filter(|m| !m.contains_key("request"))
            .and_then(|m| m.get("action"))
        {
            // Counted, to see how often a model drops the wrapper
            tracing::info!(
                target: "forgiving_coercion",
                coercion = "memory_bare_request",
                action = %super::id::echo(&action.to_string()),
                "memory tool call without `request`, read as its request"
            );
            return read::<MemoryAction>(&input)
                .map_err(|e| {
                    fail(format!("The request could not be read: {e}."))
                })
                .and_then(refuse_empty_query);
        }
        read::<Self>(&input)
            .map(|i| i.request)
            .map_err(|e| fail(format!("The call could not be read: {e}.")))
            .and_then(refuse_empty_query)
    }
}

/// A search with no words is refused: it would match nothing, or every line
fn refuse_empty_query(
    action: MemoryAction,
) -> Result<MemoryAction, MemoryInputError> {
    match &action {
        MemoryAction::Search { query, .. } if query.trim().is_empty() => {
            Err(MemoryInputError(EMPTY_QUERY.into()))
        }
        _ => Ok(action),
    }
}

/// Read `input` as a `T`, or say why: an error echoes the agent's input
/// only as [`echo`](super::id::echo) cuts it, and keeps all of its own
/// words. The error is serde's for a copy of `input` with every string and
/// key cut; that copy fails the same way, since a cut string ends in `…`
fn read<T: serde::de::DeserializeOwned>(
    input: &serde_json::Value,
) -> Result<T, String> {
    use serde_json::Value;
    fn shortened(v: &Value) -> Value {
        match v {
            Value::String(s) => Value::String(super::id::echo(s)),
            Value::Array(items) => {
                Value::Array(items.iter().map(shortened).collect())
            }
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(k, v)| (super::id::echo(k), shortened(v)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }
    T::deserialize(input).map_err(|e| {
        T::deserialize(&shortened(input))
            .err()
            .unwrap_or(e)
            .to_string()
    })
}

/// One request to the `memory` tool, by its `action`. Its docs are what
/// a model reads, every session, so they are short
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
#[schemars(inline)]
pub enum MemoryAction {
    // The tool's description says what each action does; a variant doc
    // would say it again in the schema, every session.
    // No `mode` until semantic search exists (Steward, beta round 16)
    Search {
        /// Words to find
        query: String,
    },
    Show {
        /// One or a list
        id: OneOrMany<HistoryRef>,
        /// Most bytes to show (default 32,768, at least 4,096,
        /// at most 262,144)
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
    Log {
        /// Start below this memory id or head
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::serde_forgiving::forgiving_option"
        )]
        before: Option<HistoryRef>,
        /// Versions to list: 20 if left out, at most 50
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::serde_forgiving::forgiving_option_u32"
        )]
        #[schemars(with = "Option<u32>", range(min = 1, max = LOG_MAX_LIMIT))]
        limit: Option<u32>,
    },
    Diff {
        /// A memory id or head
        a: HistoryRef,
        /// A memory id or head; left out, your current memory
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::serde_forgiving::forgiving_option"
        )]
        b: Option<HistoryRef>,
    },
}

/// A value as used, and what to tell the agent when it is not the one
/// asked for
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Adjusted {
    pub value: usize,
    /// `Limit raised to 1.`, say; `None` when the value is as asked, or
    /// the default
    pub note: Option<String>,
}

/// `value` (or `default`) clamped to `min..=max`, saying so when clamped:
/// "`what` raised to `{min}`" or "lowered to `{max}`"
fn adjust(
    value: Option<u32>,
    default: u32,
    (min, max): (u32, u32),
    raised: impl Fn(&str) -> String,
    lowered: impl Fn(&str) -> String,
) -> Adjusted {
    let asked = value.unwrap_or(default);
    let used = asked.clamp(min, max);
    let note = match asked.cmp(&used) {
        std::cmp::Ordering::Less => {
            Some(raised(&super::render::grouped(used as usize)))
        }
        std::cmp::Ordering::Greater => {
            Some(lowered(&super::render::grouped(used as usize)))
        }
        std::cmp::Ordering::Equal => None,
    };
    Adjusted {
        value: used as usize,
        note,
    }
}

/// `log`'s `limit`, defaulted and clamped to `1..=`[`LOG_MAX_LIMIT`]
pub fn log_limit(limit: Option<u32>) -> Adjusted {
    adjust(
        limit,
        LOG_DEFAULT_LIMIT,
        (1, LOG_MAX_LIMIT),
        |n| format!("Limit raised to {n}."),
        |n| format!("Limit lowered to {n}."),
    )
}

/// `show`'s `budget` in bytes, defaulted and clamped as `get_content`'s
/// `comment_budget` is
pub fn show_budget(budget: Option<u32>) -> Adjusted {
    adjust(
        budget,
        GetContentInput::DEFAULT_COMMENT_BUDGET,
        (
            GetContentInput::MIN_COMMENT_BUDGET,
            GetContentInput::MAX_COMMENT_BUDGET,
        ),
        |n| format!("Budget raised to the minimum of {n} bytes."),
        |n| format!("Budget lowered to the maximum of {n} bytes."),
    )
}

/// What a search with no words is told
pub const EMPTY_QUERY: &str = "A search needs words to find, and `query` \
     was empty. Nothing was searched.";

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
        // As the description leads: search, then show, log and diff
        assert_eq!(names, ["search", "show", "log", "diff"]);
        for a in actions {
            let required = a["required"].as_array().unwrap();
            assert!(required.contains(&json!("action")), "{a}");
        }
        assert!(crate::tools::reasoning_keys_in(&schema).is_empty());
        // A string or a list, as it reads
        let show = &actions[1]["properties"];
        let id = show["id"]["anyOf"].as_array().expect("id anyOf");
        assert_eq!(id[0]["type"], "string");
        assert_eq!(id[1]["type"], "array");
        assert_eq!(id[1]["items"]["type"], "string");
        assert_eq!(id[1]["minItems"], 1);
        assert!(
            show["id"]["description"]
                .as_str()
                .unwrap()
                .contains("a list")
        );
        // No `mode` until semantic search exists
        assert_eq!(
            actions[0]["properties"]
                .as_object()
                .unwrap()
                .keys()
                .collect::<Vec<_>>(),
            ["action", "query"]
        );
    }

    /// Every agent reads the definition every session: keep it short
    #[test]
    fn the_definition_is_short() {
        let size = MEMORY_TOOL_DESCRIPTION.len()
            + MemoryInput::schema().to_string().len();
        assert!(size < 2_500, "{size}");
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
            "memory id (mem-…",
            "if your state is sealed, a head",
            "An id is worked out after you rewrite your memory, so the \
             current id can't appear inside the text it names; writing an \
             id into your memory changes the next version's id. Old ids \
             keep working.",
            "like `git log -p`: newest first, anchored on the memory in your context",
            "{\"request\": {\"action\": \"show\", \"id\": \"mem-3f9a1c07be\"}}",
            "counts toward your context",
            "kept by your runner, which your operator runs: Agora's servers \
             can't read them, and your operator can.",
        ] {
            assert!(MEMORY_TOOL_DESCRIPTION.contains(must), "{must}");
        }
        // Search leads, then show, log and diff
        let at = |a: &str| MEMORY_TOOL_DESCRIPTION.find(a).unwrap();
        assert!(at("- `search`") < at("- `show`"));
        assert!(at("- `show`") < at("- `log`"));
        assert!(at("- `log`") < at("- `diff`"));
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
                json!({"action": "search", "query": "anchor"}),
                MemoryAction::Search {
                    query: "anchor".into(),
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
        // `mode` is gone until semantic search exists
        assert!(
            serde_json::from_value::<MemoryAction>(
                json!({"action": "search", "query": "x", "mode": "keyword"})
            )
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
            json!({"request": {"command": "view", "path": "/memories"}}),
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
            // A misspelled field is refused, not dropped
            json!({"request": {"action": "log", "befor": "mem-abcdef"}}),
            json!({"action": "log", "befor": "mem-abcdef"}),
            json!({"request": {"action": "show", "id": "mem-abcdef", "budgt": 4096}}),
            json!("log"),
        ] {
            let err = MemoryInput::parse(bad.clone()).unwrap_err().to_string();
            assert!(err.ends_with(CALL_SHAPE), "{bad}: {err}");
        }
    }

    /// An error cuts the agent's input, never its own guidance (beta
    /// round 16: "give a memory id (mem- and hex dig…")
    #[test]
    fn errors_cut_the_input_not_the_guidance() {
        let huge = "x".repeat(100_000);
        let err = MemoryInput::parse(
            json!({"request": {"action": "show", "id": huge}}),
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains(
                "is not a memory id or a head: give a memory id (mem- and \
                 hex digits, as in your Memory heading) or a head, at least \
                 6 hex digits of either"
            ),
            "{err}"
        );
        assert!(err.ends_with(CALL_SHAPE), "{err}");
        let err = MemoryInput::parse(json!({"request": {"action": huge}}))
            .unwrap_err()
            .to_string();
        assert!(err.contains("expected one of"), "{err}");
        let err = MemoryInput::parse(
            json!({"request": {"action": "log", (huge.clone()): 1}}),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("expected `before` or `limit`"), "{err}");
        assert!(err.len() < 1_000, "{}", err.len());
    }

    /// An error echoes a bounded part of a huge input
    #[test]
    fn errors_echo_little() {
        let huge = "x".repeat(100_000);
        for input in [
            json!({"request": {"action": huge}}),
            json!({"request": {"action": "show", "id": huge}}),
            json!({"action": "diff", "a": huge}),
        ] {
            let err = MemoryInput::parse(input).unwrap_err().to_string();
            assert!(err.len() < 1_000, "{}", err.len());
        }
    }

    #[test]
    fn limits_and_budgets_clamp_and_say_so() {
        let a = |value, note: Option<&str>| Adjusted {
            value,
            note: note.map(Into::into),
        };
        assert_eq!(log_limit(None), a(20, None));
        assert_eq!(log_limit(Some(50)), a(50, None));
        assert_eq!(log_limit(Some(0)), a(1, Some("Limit raised to 1.")));
        assert_eq!(log_limit(Some(500)), a(50, Some("Limit lowered to 50.")));
        assert_eq!(show_budget(None), a(32_768, None));
        assert_eq!(
            show_budget(Some(1)),
            a(4_096, Some("Budget raised to the minimum of 4,096 bytes."))
        );
        assert_eq!(
            show_budget(Some(u32::MAX)),
            a(
                262_144,
                Some("Budget lowered to the maximum of 262,144 bytes.")
            )
        );
    }

    #[test]
    fn an_empty_query_is_refused() {
        for query in ["", "   ", "\n\t"] {
            for input in [
                json!({"request": {"action": "search", "query": query}}),
                json!({"action": "search", "query": query}),
            ] {
                let err = MemoryInput::parse(input).unwrap_err().to_string();
                assert_eq!(err, EMPTY_QUERY);
            }
        }
        assert!(
            MemoryInput::parse(
                json!({"request": {"action": "search", "query": " x "}})
            )
            .is_ok()
        );
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
