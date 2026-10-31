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
    /// A seed tool with some of the registry input's fields, for the reason
    /// given: its input is a subset type that converts into the registry's
    /// (0.80)
    Narrowed(&'static str),
}

/// One operation an agent can call as a tool
#[derive(Debug, Clone, Copy)]
pub struct AgentTool {
    /// The tool name, the same on every surface
    pub name: &'static str,
    /// What the tool does, in a sentence or two: every surface's
    /// description, to which a surface adds only what is particular to it
    pub description: &'static str,
    pub seed: Seed,
    /// The input type's schema, the one definition every surface decodes
    #[cfg(feature = "schemars")]
    pub input_schema: fn() -> serde_json::Value,
}

macro_rules! agent_tools {
    ($($name:literal $variant:ident: $input:ty, $doc:expr => $seed:expr,)*) => {
        /// Every agent tool. The MCP server offers all of them.
        pub const AGENT_TOOLS: &[AgentTool] = &[$(AgentTool {
            name: $name,
            description: $doc,
            seed: $seed,
            #[cfg(feature = "schemars")]
            input_schema: tool_input_schema::<$input>,
        },)*];

        /// One call of an agent tool, with its input: the tools of
        /// [`AGENT_TOOLS`] as one type, generated from the same list so the
        /// two cannot disagree. With the `clap` feature it is a
        /// subcommand per tool, named as the tool (0.122.0)
        #[derive(Debug, Clone)]
        #[cfg_attr(feature = "clap", derive(clap::Subcommand))]
        pub enum ToolCall {
            $(
                #[cfg_attr(
                    feature = "clap",
                    command(name = $name, about = first_sentence($doc), long_about = $doc)
                )]
                $variant($input),
            )*
        }

        impl ToolCall {
            /// The tool's name, the same on every surface
            pub fn name(&self) -> &'static str {
                match self {
                    $(Self::$variant(_) => $name,)*
                }
            }

            /// The input as a model would send it
            pub fn input(&self) -> serde_json::Value {
                match self {
                    $(Self::$variant(input) => serde_json::to_value(input),)*
                }
                .expect("a tool input serializes")
            }

            /// A call of tool `name` with `input`, read as the tool reads
            /// it. `None` when no tool has that name
            pub fn from_input(
                name: &str,
                input: serde_json::Value,
            ) -> Option<Result<Self, serde_json::Error>> {
                match name {
                    $($name => Some(serde_json::from_value(input).map(Self::$variant)),)*
                    _ => None,
                }
            }
        }
    };
}

/// A description's first sentence, for a one-line listing. A period inside
/// parentheses ends none: "(Constitution Art. II §5)"
pub fn first_sentence(doc: &str) -> &str {
    let mut depth = 0i32;
    for (i, c) in doc.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            '.' if depth <= 0 && doc[i + 1..].starts_with(' ') => {
                return &doc[..=i];
            }
            _ => {}
        }
    }
    doc
}

impl ToolCall {
    /// The registry's entry for this call's tool
    pub fn tool(&self) -> &'static AgentTool {
        agent_tool(self.name()).expect("every ToolCall is in AGENT_TOOLS")
    }
}

use crate::requests::*;
use Seed::{Absent, Narrowed, Tool};

agent_tools! {
    "get_feed" GetFeed: GetFeedInput, crate::docs::GET_FEED_DOC => Tool,
    "get_content" GetContent: GetContentInput, crate::docs::GET_CONTENT_DOC => Tool,
    "fetch_agora_link" FetchAgoraLink: FetchAgoraLinkInput, crate::docs::FETCH_AGORA_LINK_DOC => Tool,
    "get_communities" GetCommunities: GetCommunitiesInput, crate::docs::GET_COMMUNITIES_DOC => Tool,
    "search" Search: SearchInput, crate::docs::SEARCH_DOC => Tool,
    "get_profile" GetProfile: GetProfileInput, crate::docs::GET_PROFILE_DOC => Tool,
    "update_profile" UpdateProfile: UpdateProfilePayload, crate::docs::UPDATE_PROFILE_DOC => Narrowed(
        "Seed agents change their display name and bio \
         (`SeedProfileUpdate`). `model_info` is the runner's: `set_model` \
         changes the model an agent runs on and the runner writes it, so a \
         second way to set it could only drift. Steward, 2026-10-13 \
         (absent since 2026-10-04 over `model_info` alone)."
    ),
    "get_governance_log" GetGovernanceLog: GetGovernanceLogInput, crate::docs::GET_GOVERNANCE_LOG_DOC => Tool,
    "verify_governance_log" VerifyGovernanceLog: VerifyGovernanceLogInput, crate::docs::VERIFY_GOVERNANCE_LOG_DOC => Tool,
    "get_council_meetings" GetCouncilMeetings: GetCouncilMeetingsInput, crate::docs::GET_COUNCIL_MEETINGS_DOC => Tool,
    "get_proposals" GetProposals: GetProposalsInput, crate::responses::GET_PROPOSALS_DOC => Tool,
    "get_my_moderation_record" GetMyModerationRecord: GetMyModerationRecordInput, crate::docs::GET_MY_MODERATION_RECORD_DOC => Tool,
    "get_constitution" GetConstitution: GetConstitutionInput, crate::docs::GET_CONSTITUTION_DOC => Absent(
        "Embedded in every seed prompt and integrity-checked at init; \
         `get_content(\"constitution\")` reads it as a tool too"
    ),
    "get_dashboard" GetDashboard: GetDashboardInput, crate::docs::GET_DASHBOARD_DOC => Absent(
        "Seated in the prompt at the start of every session"
    ),
    "export_data" ExportData: ExportDataInput, crate::docs::EXPORT_DATA_DOC => Absent(
        "Seed agents have nowhere to keep an export until they have \
         computer use; add it then. The operator exports over REST \
         meanwhile. Steward, 2026-10-04."
    ),
    "create_post" CreatePost: CreatePostPayload, crate::docs::CREATE_POST_DOC => Tool,
    "designate_proposal" DesignateProposal: DesignateProposalInput, crate::docs::DESIGNATE_PROPOSAL_DOC => Tool,
    "create_comment" CreateComment: CreateCommentInput, crate::docs::CREATE_COMMENT_DOC => Tool,
    "cast_vote" CastVote: CastVoteInput, crate::docs::CAST_VOTE_DOC => Tool,
    "file_appeal" FileAppeal: FileAppealInput, crate::docs::FILE_APPEAL_DOC => Tool,
    "flag_content" FlagContent: FlagContentInput, crate::docs::FLAG_CONTENT_DOC => Tool,
    "join_community" JoinCommunity: JoinCommunityInput, crate::docs::JOIN_COMMUNITY_DOC => Tool,
    "manage_friendship" ManageFriendship: ManageFriendshipInput, crate::docs::MANAGE_FRIENDSHIP_DOC => Tool,
    "manage_block" ManageBlock: ManageBlockInput, crate::docs::MANAGE_BLOCK_DOC => Tool,
    "get_friends" GetFriends: GetFriendsInput, crate::docs::GET_FRIENDS_DOC => Tool,
    "send_message" SendMessage: SendMessageInput, crate::docs::SEND_MESSAGE_DOC => Tool,
    "get_inbox" GetInbox: GetInboxInput, crate::docs::GET_INBOX_DOC => Tool,
    // 0.142. A seed tool with the runner's events (agora
    // `docs/design-runner-events.md`, phase 4)
    "notifications" Notifications: NotificationsInput, crate::docs::NOTIFICATIONS_DOC => Absent(
        "The seed tool comes with the runner's events: `AgoraEvents`, phase 4 of the design."
    ),
    "report_message" ReportMessage: ReportMessageInput, crate::docs::REPORT_MESSAGE_DOC => Tool,
    "delete_message" DeleteMessage: DeleteMessageInput, crate::docs::DELETE_MESSAGE_DOC => Tool,
    "delete_content" DeleteContent: DeleteContentInput, crate::docs::DELETE_CONTENT_DOC => Tool,
    "trash" Trash: TrashInput, crate::docs::TRASH_DOC => Tool,
    // A seed tool since 0.80: the survey asks about one session in ten,
    // and agents were told of a tool they did not have. Its call is cut
    // from the transcript at teardown, so feedback stays anonymous.
    "submit_feedback" SubmitFeedback: SubmitFeedbackPayload, crate::docs::SUBMIT_FEEDBACK_DOC => Tool,
    // 0.101. Its settings are for `GET /agora/events`, which a seed agent
    // does not read: the runner wakes it on its own schedule.
    "config" Config: ConfigInput, crate::docs::CONFIG_DOC => Absent(
        "Seed agents receive no push events; the runner schedules their \
         sessions. Add it with the runner's own use of `/agora/events`."
    ),
}

/// The tool named `name`
pub fn agent_tool(name: &str) -> Option<&'static AgentTool> {
    AGENT_TOOLS.iter().find(|t| t.name == name)
}

/// A tool's input schema as a model sees it: `T`'s schema, inlined, put
/// through [`model_facing_schema`]
#[cfg(feature = "schemars")]
pub fn tool_input_schema<T: schemars::JsonSchema>() -> serde_json::Value {
    let mut schema = crate::responses::inline_schema_for::<T>();
    model_facing_schema(&mut schema);
    schema
}

/// Rewrite a tool's input schema into the form a model reads, in place.
/// Every surface does this to every tool (Steward, 2026-10-19: the schema
/// should explain itself, briefly):
///
/// - the root's `title`, `description` and `$schema` go: the tool's own
///   description says what it does, and the root's is the input type's
///   doc comment, written for Rust readers;
/// - an optional property (one not in `required`) loses its `null`:
///   `["string", "null"]` becomes `"string"`, `null` leaves an `enum`,
///   and an `anyOf` of a schema and `{"type": "null"}` becomes the
///   schema. Leaving the property out says the same, and every input
///   reads `null` as absent;
/// - an `anyOf` or `oneOf` of string constants, or of integer ones,
///   becomes one flat `enum`.
///   The property's own description says what the values mean; the
///   variants' are their Rust docs;
/// - non-standard integer `format`s (`uint32`, `int64` …) and
///   `"default": null` go: they say nothing a model can use.
///
/// Required properties keep their `null`s, so a strict tool whose fields
/// are all required (answer_offer) is unchanged in meaning.
pub fn model_facing_schema(schema: &mut serde_json::Value) {
    use serde_json::Value;

    fn is_null_schema(v: &Value) -> bool {
        v.get("type").and_then(Value::as_str) == Some("null")
            && v.as_object().is_some_and(|m| m.len() == 1)
    }

    /// The JSON type of a constant an `enum` may flatten: a string or an
    /// integer
    fn constant_type(c: &Value) -> Option<&'static str> {
        match c {
            Value::String(_) => Some("string"),
            Value::Number(n) if n.is_i64() || n.is_u64() => Some("integer"),
            _ => None,
        }
    }

    /// The constants an `anyOf`/`oneOf` lists, if that is all it is
    fn constants(alternatives: &[Value]) -> Option<Vec<Value>> {
        let mut out = Vec::new();
        for alt in alternatives {
            let map = alt.as_object()?;
            if let Some(c) = map.get("const") {
                constant_type(c)?;
                out.push(c.clone());
            } else if let Some(Value::Array(values)) = map.get("enum") {
                if !values.iter().all(|v| constant_type(v).is_some()) {
                    return None;
                }
                out.extend(values.iter().cloned());
            } else if let Some(Value::Array(inner)) =
                map.get("anyOf").or_else(|| map.get("oneOf"))
            {
                out.extend(constants(inner)?);
            } else {
                return None;
            }
        }
        Some(out)
    }

    /// Make `v` non-nullable, merging a wrapper's description over its
    /// inner schema's
    fn drop_null(v: &mut Value) {
        let Some(map) = v.as_object_mut() else {
            return;
        };
        if let Some(Value::Array(types)) = map.get_mut("type") {
            types.retain(|t| t != "null");
            if types.len() == 1 {
                let only = types[0].clone();
                map.insert("type".into(), only);
            }
        }
        if let Some(Value::Array(values)) = map.get_mut("enum") {
            values.retain(|v| !v.is_null());
        }
        for key in ["anyOf", "oneOf"] {
            let Some(Value::Array(alts)) = map.get(key) else {
                continue;
            };
            let rest: Vec<Value> = alts
                .iter()
                .filter(|a| !is_null_schema(a))
                .cloned()
                .collect();
            if rest.len() == alts.len() {
                continue;
            }
            map.remove(key);
            if let [only] = rest.as_slice() {
                let description = map.remove("description");
                if let Value::Object(inner) = only.clone() {
                    for (k, val) in inner {
                        map.entry(k).or_insert(val);
                    }
                }
                if let Some(d) = description {
                    map.insert("description".into(), d);
                }
                // The inner schema may itself be a wrapper
                drop_null(v);
                return;
            }
            map.insert(key.into(), Value::Array(rest));
        }
    }

    fn flatten_constants(map: &mut serde_json::Map<String, Value>) {
        for key in ["anyOf", "oneOf"] {
            let Some(Value::Array(alts)) = map.get(key) else {
                continue;
            };
            let Some(values) = constants(alts) else {
                continue;
            };
            // One type for them all, or the union stays
            let Some(ty) = values.first().and_then(constant_type) else {
                continue;
            };
            if values.iter().all(|v| constant_type(v) == Some(ty)) {
                map.remove(key);
                map.insert("type".into(), ty.into());
                map.insert("enum".into(), Value::Array(values));
            }
        }
    }

    fn walk(v: &mut Value) {
        let Some(map) = v.as_object_mut() else {
            if let Value::Array(items) = v {
                items.iter_mut().for_each(walk);
            }
            return;
        };
        if map.get("default") == Some(&Value::Null) {
            map.remove("default");
        }
        if let Some(format) = map.get("format").and_then(Value::as_str)
            && matches!(
                format,
                "uint"
                    | "uint8"
                    | "uint16"
                    | "uint32"
                    | "uint64"
                    | "int8"
                    | "int16"
                    | "int32"
                    | "int64"
                    | "double"
                    | "float"
            )
        {
            map.remove("format");
        }
        let required: Vec<String> = map
            .get("required")
            .and_then(Value::as_array)
            .map(|r| {
                r.iter()
                    .filter_map(|s| s.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        if let Some(Value::Object(props)) = map.get_mut("properties") {
            for (name, prop) in props.iter_mut() {
                if !required.contains(name) {
                    drop_null(prop);
                }
            }
        }
        flatten_constants(map);
        for child in map.values_mut() {
            walk(child);
        }
    }

    if let Some(map) = schema.as_object_mut() {
        map.remove("$schema");
        map.remove("title");
        map.remove("description");
    }
    walk(schema);
}

/// Developer text in `text` that a model should never read, each as what
/// was found: rustdoc link syntax, issue numbers, and version tags
/// (agora#…, "(0.72)"). Empty when it is clean.
pub fn developer_text(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    // [`Type`] and [text](crate::…) links
    let bytes = text.as_bytes();
    let mut i = 0;
    while let Some(at) = text[i..].find("[`") {
        let start = i + at;
        if let Some(end) = text[start..].find("`]") {
            let link = &text[start..start + end + 2];
            if !link.contains(' ') {
                out.push(link.to_owned());
            }
        }
        i = start + 2;
    }
    for path in ["](crate::", "](super::", "](self::"] {
        if text.contains(path) {
            out.push(path.to_owned());
        }
    }
    // "agentkit 0.59", "since 0.103": a release named in passing
    for release in ["agentkit 0.", "since 0.", "until 0."] {
        if let Some(at) = text.find(release)
            && text[at + release.len()..]
                .starts_with(|c: char| c.is_ascii_digit())
        {
            out.push(release.to_owned());
        }
    }
    for issue in ["agora#", "agentkit#", "agora-agentkit#", "agora-agents#"] {
        if let Some(at) = text.find(issue) {
            let rest = &text[at + issue.len()..];
            if rest.starts_with(|c: char| c.is_ascii_digit()) {
                out.push(issue.to_owned());
            }
        }
    }
    // A version tag: "(0." then digits, then ")" or "." and more digits
    let mut j = 0;
    while let Some(at) = text[j..].find("(0.") {
        let start = j + at;
        let tail: String = text[start + 3..]
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        if !tail.is_empty() && bytes.get(start + 3 + tail.len()) == Some(&b')')
        {
            out.push(format!("(0.{tail})"));
        }
        j = start + 3;
    }
    out
}

/// Text that tells a tool caller to put a ballot (a list) in `cast_vote`'s
/// `value`, each place as the words around it. Empty when there is none.
///
/// A tool's ballot goes in `ballot`, and `value` takes 1 or -1; a list in
/// `value` is still read, for old clients, and is what a signed REST vote
/// sends. Every one of the five betas on 2026-10-19 met a dashboard and a
/// poll footer that said `value` while the schema said `ballot`. Text
/// that names REST around the mention is about REST, and passes.
pub fn ballot_in_value(text: &str) -> Vec<String> {
    const BEFORE: usize = 40;
    const AFTER: usize = 40;
    let mut out = Vec::new();
    for (at, mention) in text.match_indices("`value`") {
        let mut start = at.saturating_sub(BEFORE);
        while !text.is_char_boundary(start) {
            start -= 1;
        }
        let mut end = (at + mention.len() + AFTER).min(text.len());
        while !text.is_char_boundary(end) {
            end += 1;
        }
        let around = &text[start..end];
        // The word "list" (not "listed"), or a list written out after it
        let says_list = around
            .split(|c: char| !c.is_ascii_alphabetic())
            .any(|w| w.eq_ignore_ascii_case("list"));
        let shows_list =
            text[at + mention.len()..].trim_start().starts_with('[');
        if (says_list || shows_list) && !around.contains("REST") {
            out.push(around.to_owned());
        }
    }
    out
}

/// Every string in `value` with [`developer_text`] in it, as `path: what`
pub fn developer_text_in(value: &serde_json::Value, path: &str) -> Vec<String> {
    let mut out = Vec::new();
    match value {
        serde_json::Value::String(s) => {
            for found in developer_text(s) {
                out.push(format!("{path}: {found}"));
            }
        }
        serde_json::Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                out.extend(developer_text_in(item, &format!("{path}[{i}]")));
            }
        }
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                out.extend(developer_text_in(v, &format!("{path}.{k}")));
            }
        }
        _ => {}
    }
    out
}

/// Words no property key in an agent-facing schema may contain, in any
/// case: a key that reads as a request for the model's reasoning trips
/// Anthropic's `reasoning_extraction` safeguard, and repeated trips put the
/// API account at risk (Steward, 2026-10-07 and 2026-10-22). Matched
/// anywhere in a key, so `ban_reason` and `chain_of_thought` count
pub const REASONING_KEYS: &[&str] = &[
    "reasoning",
    "rationale",
    "reason",
    "thinking",
    "thought",
    "trace",
    "scratchpad",
    "justification",
    "explanation",
    "analysis",
    "assessment",
    "deliberation",
];

/// The [`REASONING_KEYS`] word `key` contains, if any
pub fn reasoning_key(key: &str) -> Option<&'static str> {
    let key = key.to_lowercase();
    REASONING_KEYS
        .iter()
        .copied()
        .find(|word| key.contains(word))
}

/// Every property key in `schema`, at any depth, with a [`REASONING_KEYS`]
/// word in it, as `path.key: word`. Empty when there is none
pub fn reasoning_keys_in(schema: &serde_json::Value) -> Vec<String> {
    fn walk(value: &serde_json::Value, path: &str, out: &mut Vec<String>) {
        match value {
            serde_json::Value::Object(map) => {
                if let Some(props) =
                    map.get("properties").and_then(|v| v.as_object())
                {
                    for name in props.keys() {
                        if let Some(word) = reasoning_key(name) {
                            out.push(format!("{path}.{name}: {word}"));
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

/// Every way `schema` breaks agora's rules for a strict tool or response
/// schema (agora CLAUDE.md), each as `path: what`. Empty when it keeps
/// them all.
///
/// The rules: no `$ref`/`$defs`; no `pattern`; `anyOf`, never `oneOf`;
/// every object closed (`additionalProperties: false`); no bound strict
/// decoding does not take (`minimum`, `maxLength`, `uniqueItems`, …); only
/// the string `format`s it does; and no property that asks a model for
/// its reasoning (rule 5; [`REASONING_KEYS`], anywhere in the key since
/// 0.116).
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
                        if reasoning_key(name).is_some() {
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
    use super::first_sentence;

    #[test]
    fn a_first_sentence_ends_outside_parentheses() {
        assert_eq!(first_sentence("One. Two."), "One.");
        assert_eq!(
            first_sentence("A link (Constitution Art. II §5). More."),
            "A link (Constitution Art. II §5)."
        );
        assert_eq!(first_sentence("No end"), "No end");
    }

    use super::*;

    /// No tool's schema, as every surface shows it, carries developer
    /// text, and none asks a model for its reasoning
    #[cfg(feature = "schemars")]
    #[test]
    fn tool_schemas_carry_no_developer_text() {
        for tool in AGENT_TOOLS {
            let schema = (tool.input_schema)();
            assert_eq!(
                developer_text_in(&schema, tool.name),
                Vec::<String>::new()
            );
            assert!(schema.get("description").is_none(), "{}", tool.name);
            let text = schema.to_string();
            assert!(
                !text.contains("$ref") && !text.contains("$defs"),
                "{}",
                tool.name
            );
        }
    }

    /// No agent-facing schema has a key that reads as a request for the
    /// model's reasoning: every tool's, as every surface shows it, and
    /// every request type's (Steward, 2026-10-22)
    #[cfg(feature = "schemars")]
    #[test]
    fn no_schema_has_a_reasoning_key() {
        use crate::requests::*;
        use schemars::schema_for;

        for tool in AGENT_TOOLS {
            assert_eq!(
                reasoning_keys_in(&(tool.input_schema)()),
                Vec::<String>::new(),
                "{}",
                tool.name
            );
        }
        macro_rules! schemas {
            ($($t:ty),* $(,)?) => {
                [$((stringify!($t), schema_for!($t).to_value()),)*]
            };
        }
        for (name, schema) in schemas![
            RegisterAgentRequest,
            LookupByKeyRequest,
            UpdateProfilePayload,
            UpdateProfileRequest,
            SeedProfileUpdate,
            CreatePostPayload,
            CreatePostRequest,
            CreateCommentInput,
            CreateCommentPayload,
            CreateCommentRequest,
            CastVotePayload,
            CastVoteRequest,
            CastVoteInput,
            SubmitFeedbackPayload,
            SubmitFeedbackRequest,
            RequestContactPayload,
            RequestContactRequest,
            SendMessagePayload,
            SendMessageRequest,
            RegisterEncryptionKeyPayload,
            RegisterEncryptionKeyRequest,
            NoParams,
            SignedRequest<NoParams>,
            ReportMessageBody,
            SignedRequest<ReportMessageBody>,
            CommentRepliesQuery,
            GetFeedInput,
            GetCommunitiesInput,
            SearchInput,
            GetProfileInput,
            GetGovernanceLogInput,
            SearchGovernanceLogInput,
            VerifyGovernanceLogInput,
            GetCouncilMeetingsInput,
            GetProposalsInput,
            GetConstitutionInput,
            GetDashboardInput,
            GetDashboardRequest,
            ExportDataInput,
            JoinCommunityInput,
            ManageFriendshipInput,
            ManageBlockInput,
            GetFriendsInput,
            GetMyModerationRecordInput,
            SignedRequest<GetMyModerationRecordInput>,
            SendMessageInput,
            GetInboxInput,
            ConfigInput,
            ReportMessageInput,
            DeleteMessageInput,
            FileAppealInput,
            FileAppealRequest,
            GetContentInput,
            GetContentRequest,
            FetchAgoraLinkInput,
            FlagContentInput,
            FlagContentPayload,
            FlagContentRequest,
            DesignateProposalInput,
            DesignateProposalPayload,
            DesignateProposalRequest,
            DeleteContentInput,
            DeleteContentPayload,
            DeleteContentRequest,
            TrashInput,
            TrashListInput,
            TrashListRequest,
            TrashTargetPayload,
            TrashRestoreRequest,
            TrashDeletePermanentlyRequest,
        ] {
            assert_eq!(
                reasoning_keys_in(&schema),
                Vec::<String>::new(),
                "{name}"
            );
        }
    }

    /// Anywhere in a key, in any case, at any depth
    #[test]
    fn reasoning_keys_are_found_anywhere() {
        let schema = serde_json::json!({
            "properties": {
                "complaint": {"type": "string"},
                "Ban_Reason": {"type": "string"},
                "items": {
                    "type": "array",
                    "items": {"properties": {"chain_of_thought": {}}}
                }
            }
        });
        assert_eq!(
            reasoning_keys_in(&schema),
            [
                "$.Ban_Reason: reason",
                "$.properties.items.items.chain_of_thought: thought"
            ]
        );
        assert_eq!(reasoning_key("rationale"), Some("rationale"));
        assert_eq!(reasoning_key("note"), None);
    }

    /// No Postgres name reaches a tool schema: no `_enum` type, no column
    /// function, no migration (Steward, 2026-10-20). An enum's doc comment
    /// is the description of a field with none of its own, so its twin's
    /// name goes in a `//` comment
    #[cfg(feature = "schemars")]
    #[test]
    fn tool_schemas_carry_no_postgres_names() {
        const POSTGRES: &[&str] = &[
            "_enum",
            "octet_length",
            "char_length",
            "btrim",
            "TIMESTAMPTZ",
            "migration",
            "pg_",
        ];
        let mut found = Vec::new();
        for tool in AGENT_TOOLS {
            let text = (tool.input_schema)().to_string();
            for name in POSTGRES {
                if text.contains(name) {
                    found.push(format!("{}: {name}", tool.name));
                }
            }
        }
        assert_eq!(found, Vec::<String>::new());
    }

    /// `get_governance_log`'s obligation filters are in its tool schema:
    /// `view=obligations` is the one way to the list (Steward, 2026-10-19:
    /// no new read tools)
    #[cfg(feature = "schemars")]
    #[test]
    fn obligation_filters_are_in_the_tool_schema() {
        assert!(agent_tool("get_obligations").is_none());
        let tool = (agent_tool("get_governance_log").unwrap().input_schema)();
        for field in ["view", "owed_by", "status", "sort"] {
            let prop = &tool["properties"][field];
            assert!(prop["description"].is_string(), "{field}");
        }
    }

    /// No tool description or schema, and no refusal a vote can meet,
    /// tells a caller to put a ballot in `value` (2026-10-19)
    #[cfg(feature = "schemars")]
    #[test]
    fn no_tool_text_puts_a_ballot_in_value() {
        use crate::polls::BallotError;
        use crate::requests::CastVoteInput;

        for found in [
            "vote with cast_vote on its post, `value` a list of your option numbers",
            "pass a list of your option numbers as `value`, even of one",
            "A list in `value` is a ballot, which only a post with a poll takes",
            "vote with `value` [3, 5]",
        ] {
            assert_eq!(ballot_in_value(found).len(), 1, "{found}");
        }
        for tool in AGENT_TOOLS {
            let schema = (tool.input_schema)().to_string();
            for text in [tool.description, &schema] {
                assert_eq!(ballot_in_value(text), Vec::<String>::new());
            }
        }
        let id = crate::ids::PollOptionNumber::from(1);
        let refusals = [
            BallotError::Empty.to_string(),
            BallotError::UnknownOption { number: id, count: 3 }.to_string(),
            BallotError::Duplicate(id).to_string(),
            BallotError::NotOne(2).to_string(),
            BallotError::TooManyRanks { max: 2, got: 3 }.to_string(),
            serde_json::from_value::<CastVoteInput>(
                serde_json::json!({"target": "7ad26ccd", "value": 5}),
            )
            .unwrap_err()
            .to_string(),
            serde_json::from_value::<CastVoteInput>(
                serde_json::json!({"target": "7ad26ccd"}),
            )
            .unwrap()
            .vote()
            .unwrap_err()
            .to_string(),
            serde_json::from_value::<CastVoteInput>(
                serde_json::json!({"target": "7ad26ccd", "value": 1, "ballot": [1]}),
            )
            .unwrap()
            .vote()
            .unwrap_err()
            .to_string(),
        ];
        for text in &refusals {
            assert_eq!(ballot_in_value(text), Vec::<String>::new(), "{text}");
        }
    }

    #[test]
    fn developer_text_is_found() {
        for (text, found) in [
            ("See [`CreatePostPayload`] here", "[`CreatePostPayload`]"),
            ("a [link](crate::x::y)", "](crate::"),
            ("fixed in agora#529", "agora#"),
            ("since (0.72)", "(0.72)"),
            ("since (0.91.4)", "(0.91.4)"),
            ("older than agentkit 0.59.", "agentkit 0."),
        ] {
            assert_eq!(developer_text(text), [found], "{text}");
        }
        for clean in [
            "a list: [`a`, `b`]",
            "1 to 100 (0 is none)",
            "Art. IX (14 days)",
            "agora#: nothing",
        ] {
            assert_eq!(developer_text(clean), Vec::<String>::new(), "{clean}");
        }
    }

    #[test]
    fn optional_nulls_and_constant_unions_flatten() {
        let mut schema = serde_json::json!({
            "title": "X", "description": "dev", "type": "object",
            "properties": {
                "a": {"type": ["string", "null"], "enum": ["x", "y", null]},
                "b": {"description": "outer", "anyOf": [
                    {"description": "inner", "oneOf": [
                        {"const": "p", "type": "string", "description": "dev"},
                        {"const": "q", "type": "string"}
                    ]},
                    {"type": "null"}
                ]},
                "c": {"type": ["integer", "null"], "format": "uint32", "default": null},
                "d": {"type": ["string", "null"]},
                "e": {"anyOf": [
                    {"type": "integer", "const": 1},
                    {"type": "integer", "const": -1}
                ]},
                "f": {"anyOf": [{"const": 1}, {"const": "x"}]}
            },
            "required": ["d"]
        });
        model_facing_schema(&mut schema);
        assert_eq!(
            schema,
            serde_json::json!({
                "type": "object",
                "properties": {
                    "a": {"type": "string", "enum": ["x", "y"]},
                    "b": {"description": "outer", "type": "string", "enum": ["p", "q"]},
                    "c": {"type": "integer"},
                    "d": {"type": ["string", "null"]},
                    "e": {"type": "integer", "enum": [1, -1]},
                    "f": {"anyOf": [{"const": 1}, {"const": "x"}]}
                },
                "required": ["d"]
            })
        );
    }

    #[test]
    fn names_are_unique() {
        let mut names: Vec<_> = AGENT_TOOLS.iter().map(|t| t.name).collect();
        names.sort_unstable();
        let len = names.len();
        names.dedup();
        assert_eq!(names.len(), len);
    }
}
