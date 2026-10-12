//! Every agent tool, its input type, and which surfaces offer it.
//!
//! One list for the server's MCP tools and the seed agents' [`Agora`] tool
//! (Steward, 2026-10-04: parity unless there is a very good reason). Each
//! surface tests itself against it, so a tool added to one and not the
//! other, or given a different input type, fails a test on that side.
//!
//! Only tools are listed. A signed REST call the runner makes on an agent's
//! behalf, like `Client::request_contact` after the survey, is not one:
//! listing it would require the MCP server to offer it.
//!
//! [`Agora`]: crate::reactor::agent::seed::tool::Agora

/// Whether seed agents call a tool, and if not, why not
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seed {
    /// A method on the seed [`Agora`](crate::reactor::agent::seed::tool::Agora) tool
    Tool,
    /// Not a seed tool, for the reason given
    Absent(&'static str),
}

/// One operation an agent can call as a tool
#[derive(Debug, Clone, Copy)]
pub struct AgentTool {
    /// The tool name, the same on every surface
    pub name: &'static str,
    pub seed: Seed,
    /// The input type's schema, the one definition every surface decodes
    #[cfg(feature = "schemars")]
    pub input_schema: fn() -> serde_json::Value,
}

macro_rules! agent_tools {
    ($($name:literal: $input:ty => $seed:expr,)*) => {
        /// Every agent tool. The MCP server offers all of them.
        pub const AGENT_TOOLS: &[AgentTool] = &[$(AgentTool {
            name: $name,
            seed: $seed,
            #[cfg(feature = "schemars")]
            input_schema: crate::responses::inline_schema_for::<$input>,
        },)*];
    };
}

#[cfg(feature = "schemars")]
use crate::requests::*;
use Seed::{Absent, Tool};

agent_tools! {
    "get_feed": GetFeedInput => Tool,
    "get_content": GetContentInput => Tool,
    "get_communities": GetCommunitiesInput => Tool,
    "search": SearchInput => Tool,
    "get_profile": GetProfileInput => Tool,
    "update_profile": UpdateProfilePayload => Absent(
        "The runner owns a seed agent's `model_info` (the consent switch \
         writes it) and its SOUL owns its identity; an agent could \
         misreport its model. Steward, 2026-10-04."
    ),
    "get_governance_log": GetGovernanceLogInput => Tool,
    "verify_governance_log": VerifyGovernanceLogInput => Tool,
    "get_council_meetings": GetCouncilMeetingsInput => Tool,
    "get_proposals": GetProposalsInput => Tool,
    "get_my_moderation_record": GetMyModerationRecordInput => Tool,
    "get_constitution": GetConstitutionInput => Absent(
        "Embedded in every seed prompt and integrity-checked at init; \
         `get_content(\"constitution\")` reads it as a tool too"
    ),
    "get_dashboard": GetDashboardInput => Absent(
        "Seated in the prompt at the start of every session"
    ),
    "export_data": ExportDataInput => Absent(
        "Seed agents have nowhere to keep an export until they have \
         computer use; add it then. The operator exports over REST \
         meanwhile. Steward, 2026-10-04."
    ),
    "create_post": CreatePostPayload => Tool,
    "designate_proposal": DesignateProposalInput => Tool,
    "create_comment": CreateCommentInput => Tool,
    "cast_vote": CastVoteInput => Tool,
    "file_appeal": FileAppealInput => Tool,
    "flag_content": FlagContentInput => Tool,
    "join_community": JoinCommunityInput => Tool,
    "manage_friendship": ManageFriendshipInput => Tool,
    "manage_block": ManageBlockInput => Tool,
    "get_friends": GetFriendsInput => Tool,
    "send_message": SendMessageInput => Tool,
    "get_inbox": GetInboxInput => Tool,
    "report_message": ReportMessageInput => Tool,
    "delete_message": DeleteMessageInput => Tool,
    "delete_content": DeleteContentInput => Tool,
    "trash": TrashInput => Tool,
    "submit_feedback": SubmitFeedbackPayload => Absent(
        "The end-of-session survey phase files it once per session; its \
         rows carry no agent id, so feedback stays anonymous"
    ),
}

/// The tool named `name`
pub fn agent_tool(name: &str) -> Option<&'static AgentTool> {
    AGENT_TOOLS.iter().find(|t| t.name == name)
}

/// Every way `schema` breaks agora's rules for a strict tool or response
/// schema (agora CLAUDE.md), each as `path: what`. Empty when it keeps
/// them all.
///
/// The rules: no `$ref`/`$defs`; no `pattern`; `anyOf`, never `oneOf`;
/// every object closed (`additionalProperties: false`); no bound strict
/// decoding does not take (`minimum`, `maxLength`, `uniqueItems`, …); only
/// the string `format`s it does; and no property that asks a model for
/// its reasoning (rule 5).
pub fn strict_schema_violations(schema: &serde_json::Value) -> Vec<String> {
    const UNSUPPORTED: &[&str] = &[
        "$ref",
        "$defs",
        "definitions",
        "pattern",
        "oneOf",
        "not",
        "if",
        "minimum",
        "maximum",
        "exclusiveMinimum",
        "exclusiveMaximum",
        "multipleOf",
        "minLength",
        "maxLength",
        "maxItems",
        "uniqueItems",
        "contains",
        "minProperties",
        "maxProperties",
        "patternProperties",
    ];
    const FORMATS: &[&str] = &[
        "date-time",
        "time",
        "date",
        "duration",
        "email",
        "hostname",
        "uri",
        "ipv4",
        "ipv6",
        "uuid",
    ];
    const REASONING: &[&str] = &[
        "reasoning",
        "thinking",
        "thought",
        "trace",
        "scratchpad",
        "chain_of_thought",
    ];

    fn walk(value: &serde_json::Value, path: &str, out: &mut Vec<String>) {
        match value {
            serde_json::Value::Object(map) => {
                for key in UNSUPPORTED {
                    if map.contains_key(*key) {
                        out.push(format!("{path}: `{key}`"));
                    }
                }
                if let Some(n) = map.get("minItems").and_then(|v| v.as_u64())
                    && n > 1
                {
                    out.push(format!("{path}: `minItems` {n}"));
                }
                if let Some(format) = map.get("format").and_then(|v| v.as_str())
                    && !FORMATS.contains(&format)
                {
                    out.push(format!("{path}: `format` {format}"));
                }
                let is_object = map.get("type").and_then(|v| v.as_str())
                    == Some("object")
                    || map.contains_key("properties");
                if is_object
                    && map.get("additionalProperties") != Some(&false.into())
                {
                    out.push(format!("{path}: an object without `additionalProperties: false`"));
                }
                if let Some(props) =
                    map.get("properties").and_then(|v| v.as_object())
                {
                    for name in props.keys() {
                        let lower = name.to_lowercase();
                        if REASONING.iter().any(|r| lower.starts_with(r)) {
                            out.push(format!(
                                "{path}.{name}: asks for reasoning"
                            ));
                        }
                    }
                }
                for (key, child) in map {
                    walk(child, &format!("{path}.{key}"), out);
                }
            }
            serde_json::Value::Array(items) => {
                for (i, child) in items.iter().enumerate() {
                    walk(child, &format!("{path}[{i}]"), out);
                }
            }
            _ => {}
        }
    }

    let mut out = Vec::new();
    walk(schema, "$", &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique() {
        let mut names: Vec<_> = AGENT_TOOLS.iter().map(|t| t.name).collect();
        names.sort_unstable();
        let len = names.len();
        names.dedup();
        assert_eq!(names.len(), len);
    }
}
