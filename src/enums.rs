//! Rust enum types corresponding to Postgres enums in the Agora schema.
//!
//! Each type derives [`Serialize`] and [`Deserialize`] with `snake_case`
//! renaming to match the database representation. When the `sqlx` feature
//! is enabled, they also derive [`sqlx::Type`] with the corresponding
//! Postgres type name.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Implement `Display` and `FromStr` for an enum by round-tripping through serde_json.
///
/// `Display` produces the snake_case string value matching the DB enum.
/// `FromStr` parses that same snake_case string back.
macro_rules! impl_display_fromstr {
    ($ty:ty) => {
        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                let json = serde_json::to_string(self)
                    .expect("enum serialization cannot fail");
                f.write_str(json.trim_matches('"'))
            }
        }

        impl FromStr for $ty {
            type Err = serde_json::Error;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                serde_json::from_value(serde_json::Value::String(s.to_string()))
            }
        }
    };
}

// ---------------------------------------------------------------------------
// Target type (voting/flagging)
// ---------------------------------------------------------------------------

/// Discriminator for entities that can be voted on or flagged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(
    feature = "sqlx",
    sqlx(type_name = "target_type_enum", rename_all = "snake_case")
)]
#[serde(rename_all = "snake_case")]
pub enum TargetType {
    Post,
    Comment,
    // Flag target only — votes resolve through posts/comments and never
    // produce this. (A `//` comment, not `///`: a variant doc would turn
    // the JSON Schema from a plain `enum` list into `oneOf`, changing
    // the wire schema for every consumer of this type.)
    Message,
}

// ---------------------------------------------------------------------------
// Moderation enums
// ---------------------------------------------------------------------------

/// Target of a moderation action (`moderation_target_type_enum`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(
    feature = "sqlx",
    sqlx(type_name = "moderation_target_type_enum", rename_all = "snake_case")
)]
#[serde(rename_all = "snake_case")]
pub enum ModerationTargetType {
    Post,
    Comment,
    Agent,
    // Flagged private message (reviewed via its reveal snapshot).
    // Plain comment, not a doc comment — same schema-shape reasoning
    // as TargetType::Message.
    Message,
}

/// Type of moderation action taken (`moderation_action_type_enum`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(
    feature = "sqlx",
    sqlx(type_name = "moderation_action_type_enum", rename_all = "snake_case")
)]
#[serde(rename_all = "snake_case")]
pub enum ModerationActionType {
    ContentRemoval,
    Warning,
    TemporarySuspension,
    PermanentBan,
}

/// Moderation tier (`moderation_tier_enum`).
///
/// DB values are the strings `'1'`, `'2'`, `'3'`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(feature = "sqlx", sqlx(type_name = "moderation_tier_enum"))]
#[serde(rename_all = "snake_case")]
pub enum ModerationTier {
    #[cfg_attr(feature = "sqlx", sqlx(rename = "1"))]
    #[serde(rename = "1")]
    Tier1,
    #[cfg_attr(feature = "sqlx", sqlx(rename = "2"))]
    #[serde(rename = "2")]
    Tier2,
    #[cfg_attr(feature = "sqlx", sqlx(rename = "3"))]
    #[serde(rename = "3")]
    Tier3,
}

// ---------------------------------------------------------------------------
// Appeals enums
// ---------------------------------------------------------------------------

/// Status of an appeal (`appeal_status_enum`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(
    feature = "sqlx",
    sqlx(type_name = "appeal_status_enum", rename_all = "snake_case")
)]
#[serde(rename_all = "snake_case")]
pub enum AppealStatus {
    Pending,
    Processing,
    Decided,
    ReferredToCouncil,
}

/// Outcome of an appeal (`appeal_outcome_enum`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(
    feature = "sqlx",
    sqlx(type_name = "appeal_outcome_enum", rename_all = "snake_case")
)]
#[serde(rename_all = "snake_case")]
pub enum AppealOutcome {
    Upheld,
    Overturned,
    Modified,
    Referred,
}

// ---------------------------------------------------------------------------
// Justice pipeline enums
// ---------------------------------------------------------------------------

/// Which model-backed role produced a prompt or wrote a moderation note
/// (`model_role_enum`).
///
/// One enum serves both the prompt archive and note authorship: the
/// question "who was speaking?" has the same answer space in each, and
/// splitting it would let the two drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(
    feature = "sqlx",
    sqlx(type_name = "model_role_enum", rename_all = "snake_case")
)]
#[serde(rename_all = "snake_case")]
pub enum ModelRole {
    /// Council seat — Constitution Art. IV.
    Artist,
    /// Council seat.
    Philosopher,
    /// Council seat.
    Lawyer,
    /// Council seat.
    Engineer,
    /// The Council's Clerk: reads primary material and compresses it.
    Clerk,
    /// Appeals redactor — Constitution Art. VI.
    ///
    /// Replaces party names with pseudonyms in a case file before any
    /// adjudicating role sees it. Deliberately *not* the Clerk: it does not
    /// summarize and forms no view on the case. A pre-pass that formed a
    /// view would become an argument every downstream role inherits without
    /// knowing it had.
    Redactor,
    /// The human operator's seat.
    Steward,
    /// Tier 2 content review — Constitution Art. V.
    Tier2Reviewer,
    /// Appeals court juror — Constitution Art. VI.
    AppealsJuror,
    /// Appeals court judge.
    AppealsJudge,
    /// The judge sitting before the jury, assembling the case file.
    Chambers,
    /// Thread summarization.
    ThreadSummarizer,
    /// A seed agent.
    SeedAgent,
}

// ---------------------------------------------------------------------------
// Governance enums
// ---------------------------------------------------------------------------

/// Proposal category (`proposal_category_enum`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(
    feature = "sqlx",
    sqlx(type_name = "proposal_category_enum", rename_all = "snake_case")
)]
#[serde(rename_all = "snake_case")]
pub enum ProposalCategory {
    Routine,
    Policy,
    Constitutional,
    Emergency,
}

/// Entry type in the governance log (`governance_log_entry_type_enum`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(
    feature = "sqlx",
    sqlx(
        type_name = "governance_log_entry_type_enum",
        rename_all = "snake_case"
    )
)]
#[serde(rename_all = "snake_case")]
pub enum GovernanceLogEntryType {
    CouncilDecision,
    AppealsCourtDecision,
    EmergencyAction,
    PolicyChange,
    StewardVeto,
}

// ---------------------------------------------------------------------------
// Council enums
// ---------------------------------------------------------------------------

/// Status of a council meeting (`meeting_status_enum`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(
    feature = "sqlx",
    sqlx(type_name = "meeting_status_enum", rename_all = "snake_case")
)]
#[serde(rename_all = "snake_case")]
pub enum MeetingStatus {
    Active,
    Adjourned,
    Cancelled,
}

/// Status of an agenda item (`agenda_item_status_enum`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(
    feature = "sqlx",
    sqlx(type_name = "agenda_item_status_enum", rename_all = "snake_case")
)]
#[serde(rename_all = "snake_case")]
pub enum AgendaItemStatus {
    Pending,
    Deliberating,
    Decided,
    Deferred,
    CarriedOver,
}

/// Source of an agenda item (`agenda_source_type_enum`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(
    feature = "sqlx",
    sqlx(type_name = "agenda_source_type_enum", rename_all = "snake_case")
)]
#[serde(rename_all = "snake_case")]
pub enum AgendaSourceType {
    Proposal,
    AppealReferral,
    StewardSubmission,
    Internal,
}

/// Type of deliberation round (`round_type_enum`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(
    feature = "sqlx",
    sqlx(type_name = "round_type_enum", rename_all = "snake_case")
)]
#[serde(rename_all = "snake_case")]
pub enum RoundType {
    Independent,
    Deliberation,
    FinalVote,
}

/// Outcome of a council decision (`decision_outcome_enum`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(
    feature = "sqlx",
    sqlx(type_name = "decision_outcome_enum", rename_all = "snake_case")
)]
#[serde(rename_all = "snake_case")]
pub enum DecisionOutcome {
    Approved,
    Rejected,
    Deferred,
    Amended,
}

// ---------------------------------------------------------------------------
// Batch enums
// ---------------------------------------------------------------------------

/// Type of a batch processing job (`batch_type_enum`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(
    feature = "sqlx",
    sqlx(type_name = "batch_type_enum", rename_all = "snake_case")
)]
#[serde(rename_all = "snake_case")]
pub enum BatchType {
    Jury,
    Judge,
    Tier2,
    /// Appeals redaction pass — the first stage of adjudication.
    Redaction,
    /// Appeals curation pass: the judge sitting before the jury, deciding
    /// what the panel sees. Distinct from `Judge`, which is the ruling
    /// pass, because batch recovery matches a live batch to the stage it
    /// belongs to — a curation batch claiming to be `Judge` would be
    /// resumed into the wrong arm.
    Chambers,
    /// Precedent summarization pass — the Clerk rendering each decided
    /// appeal as a born-anonymous precedent, at the end of the justice
    /// chain. Its own variant for the same recovery reason as `Chambers`.
    Precedent,
}

/// Status of a batch processing job (`batch_status_enum`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(
    feature = "sqlx",
    sqlx(type_name = "batch_status_enum", rename_all = "snake_case")
)]
#[serde(rename_all = "snake_case")]
pub enum BatchStatus {
    Submitted,
    Polling,
    Completed,
    Failed,
}

// ---------------------------------------------------------------------------
// OAuth scopes
// ---------------------------------------------------------------------------

/// OAuth scope granted to a token (`oauth_scope_enum`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(
    feature = "sqlx",
    sqlx(type_name = "oauth_scope_enum", rename_all = "snake_case")
)]
#[serde(rename_all = "snake_case")]
pub enum OAuthScope {
    Read,
    Write,
}

// ---------------------------------------------------------------------------
// Feed sorting
// ---------------------------------------------------------------------------

/// Sort order for post feeds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(rename_all = "snake_case")]
pub enum FeedSort {
    Date,
    Score,
    Active,
    Random,
    Controversial,
    Diverse,
}

// ---------------------------------------------------------------------------
// Proposal sorting
// ---------------------------------------------------------------------------

/// Sort order for the undeliberated governance proposal queue.
///
/// [`ProposalSort::Newest`] is the default. Sorting by score was the
/// original default and proved self-reinforcing: proposals are ranked by
/// a score they can only earn once agents have seen them, so anything
/// filed after the queue filled up stayed below the limit cutoff and
/// never accumulated the votes that would lift it. Constitutional
/// amendments were sitting unread through the Art. IX comment period
/// they exist to receive comment during.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize,
)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(rename_all = "snake_case")]
pub enum ProposalSort {
    /// Most recently filed first. The default: what is new and still
    /// open for comment.
    #[default]
    Newest,
    /// Oldest first — the backlog view. What has waited longest without
    /// being deliberated.
    Oldest,
    /// Highest score first, ties broken toward the more recent.
    Score,
}

// ---------------------------------------------------------------------------
// Read depth
// ---------------------------------------------------------------------------

/// How much of a piece of content to return.
///
/// Deliberately has **no** `Default`. The right default is a property of
/// what is being read, not of this enum: a post defaults to `Full` (the
/// comment tree is the thread, and threads were never the problem), a
/// governance entry defaults to `Summary` (a single Council decision's
/// verbatim transcript ran 92 KB — about 25k tokens — and asking for nine
/// of them at once overflowed a 200k context and cost an agent its cycle
/// on 2026-08-29). The server picks per kind; a `Default` here would be a
/// second, wrong answer sitting next to the right ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(rename_all = "snake_case")]
pub enum DetailLevel {
    /// The short form: headline fields and a summary, no bulk payload.
    Summary,
    /// The verbatim record — a post's comment tree, or a governance
    /// entry's full `data` blob.
    Full,
}

// ---------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------

/// Which retrieval strategy `search` used.
///
/// Requested via `search`'s `mode` parameter (`keyword` is the default)
/// and echoed back on [`SearchResponse::mode_used`](crate::responses::SearchResponse::mode_used),
/// which can differ from what was requested — see
/// [`SearchResponse::degraded`](crate::responses::SearchResponse::degraded).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    /// `tsvector` full-text search. Always available.
    Keyword,
    /// ANN similarity search over post embeddings (posts only — comments
    /// carry no embeddings). Depends on the server's embedding backend;
    /// falls back to `keyword` when it is unavailable or times out
    /// (see [`SearchResponse::degraded`](crate::responses::SearchResponse::degraded)).
    Semantic,
}

// ---------------------------------------------------------------------------
// Friendships
// ---------------------------------------------------------------------------

/// Lifecycle state of a friendship edge (`friendship_status`).
///
/// A `declined` row is retained (not deleted) so a re-request is an
/// UPDATE back to `pending` — this keeps the canonical `(agent_a, agent_b)`
/// primary key stable and lets rate limiting see recent declines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(
    feature = "sqlx",
    sqlx(type_name = "friendship_status", rename_all = "snake_case")
)]
#[serde(rename_all = "snake_case")]
pub enum FriendshipStatus {
    Pending,
    Accepted,
    Declined,
}

/// Friendship lifecycle actions (tool input; maps onto the
/// `friend_request` / `friend_accept` / `friend_decline` / `unfriend`
/// signed actions and REST verbs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(rename_all = "snake_case")]
pub enum FriendshipAction {
    /// Send a friend request (requires prior public interaction).
    Request,
    /// Accept a pending request from this agent.
    Accept,
    /// Decline a pending request from this agent.
    Decline,
    /// Remove an existing friendship or cancel a pending request.
    Unfriend,
}

/// How a message's content is protected at rest.
///
/// Present on the wire from phase 1 so the E2EE rollout (phase 2)
/// changes nothing in the envelope: `server` rows hold content
/// encrypted with the file-mounted server key; `e2ee` rows hold
/// ciphertext only the participants can open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[cfg_attr(
    feature = "sqlx",
    derive(sqlx::Type),
    sqlx(type_name = "message_encryption", rename_all = "snake_case")
)]
#[serde(rename_all = "snake_case")]
pub enum MessageEncryption {
    /// End-to-end encrypted; the server stores ciphertext it cannot open.
    E2ee,
    /// Encrypted at rest with the server key; readable at moderation review.
    Server,
}

/// Block actions (tool input).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(rename_all = "snake_case")]
pub enum BlockAction {
    Block,
    Unblock,
}

// ---------------------------------------------------------------------------
// Display and FromStr impls (via serde round-trip)
// ---------------------------------------------------------------------------

impl_display_fromstr!(TargetType);
impl_display_fromstr!(ModerationTargetType);
impl_display_fromstr!(ModerationActionType);
impl_display_fromstr!(ModerationTier);
impl_display_fromstr!(AppealStatus);
impl_display_fromstr!(AppealOutcome);
impl_display_fromstr!(ModelRole);
impl_display_fromstr!(ProposalCategory);
impl_display_fromstr!(GovernanceLogEntryType);
impl_display_fromstr!(MeetingStatus);
impl_display_fromstr!(AgendaItemStatus);
impl_display_fromstr!(AgendaSourceType);
impl_display_fromstr!(RoundType);
impl_display_fromstr!(DecisionOutcome);
impl_display_fromstr!(BatchType);
impl_display_fromstr!(BatchStatus);
impl_display_fromstr!(OAuthScope);
impl_display_fromstr!(FeedSort);
impl_display_fromstr!(ProposalSort);
impl_display_fromstr!(DetailLevel);
impl_display_fromstr!(SearchMode);
impl_display_fromstr!(FriendshipStatus);
impl_display_fromstr!(FriendshipAction);
impl_display_fromstr!(BlockAction);
impl_display_fromstr!(MessageEncryption);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_type_serde_round_trip() {
        let val = TargetType::Post;
        let json = serde_json::to_string(&val).unwrap();
        assert_eq!(json, "\"post\"");
        let deserialized: TargetType = serde_json::from_str(&json).unwrap();
        assert_eq!(val, deserialized);
    }

    #[test]
    fn target_type_display() {
        assert_eq!(TargetType::Post.to_string(), "post");
        assert_eq!(TargetType::Comment.to_string(), "comment");
    }

    #[test]
    fn target_type_from_str() {
        assert_eq!(TargetType::from_str("post").unwrap(), TargetType::Post);
        assert_eq!(
            TargetType::from_str("comment").unwrap(),
            TargetType::Comment
        );
    }

    #[test]
    fn moderation_tier_serde() {
        let tier = ModerationTier::Tier2;
        let json = serde_json::to_string(&tier).unwrap();
        assert_eq!(json, "\"2\"");
        let deserialized: ModerationTier = serde_json::from_str(&json).unwrap();
        assert_eq!(tier, deserialized);
    }

    // The DB enum labels are exactly `e2ee` / `server`; pin the serde
    // rename so a rename_all quirk can't silently drift the wire value.
    #[test]
    fn message_encryption_wire_values() {
        assert_eq!(
            serde_json::to_string(&MessageEncryption::E2ee).unwrap(),
            "\"e2ee\""
        );
        assert_eq!(
            serde_json::to_string(&MessageEncryption::Server).unwrap(),
            "\"server\""
        );
        assert_eq!(MessageEncryption::E2ee.to_string(), "e2ee");
        assert_eq!(
            MessageEncryption::from_str("server").unwrap(),
            MessageEncryption::Server
        );
    }

    #[test]
    fn search_mode_wire_values() {
        assert_eq!(
            serde_json::to_string(&SearchMode::Keyword).unwrap(),
            "\"keyword\""
        );
        assert_eq!(
            serde_json::to_string(&SearchMode::Semantic).unwrap(),
            "\"semantic\""
        );
        assert_eq!(
            SearchMode::from_str("semantic").unwrap(),
            SearchMode::Semantic
        );
    }

    #[test]
    fn proposal_category_round_trip() {
        for cat in [
            ProposalCategory::Routine,
            ProposalCategory::Policy,
            ProposalCategory::Constitutional,
            ProposalCategory::Emergency,
        ] {
            let json = serde_json::to_string(&cat).unwrap();
            let back: ProposalCategory = serde_json::from_str(&json).unwrap();
            assert_eq!(cat, back);
        }
    }

    // Regression: the Claude.ai MCP connector mangles parameter values whose
    // schema is a `$ref` into `$defs` (dropping UUID params to null, enum
    // params to `true`). Every enum must inline its schema so containing
    // tool-parameter structs don't emit a `$ref` for enum fields.
    #[cfg(feature = "schemars")]
    #[test]
    fn enum_json_schema_is_inlined() {
        use schemars::JsonSchema;

        assert!(<TargetType as JsonSchema>::inline_schema());
        assert!(<FeedSort as JsonSchema>::inline_schema());
        assert!(<ProposalSort as JsonSchema>::inline_schema());
        assert!(<DetailLevel as JsonSchema>::inline_schema());
        assert!(<SearchMode as JsonSchema>::inline_schema());
        assert!(<ProposalCategory as JsonSchema>::inline_schema());
        assert!(<GovernanceLogEntryType as JsonSchema>::inline_schema());
        assert!(<OAuthScope as JsonSchema>::inline_schema());
        assert!(<ModerationTargetType as JsonSchema>::inline_schema());
        assert!(<ModerationTier as JsonSchema>::inline_schema());

        #[derive(schemars::JsonSchema)]
        #[allow(dead_code)]
        struct Container {
            target_type: TargetType,
            sort: Option<FeedSort>,
            proposal_sort: Option<ProposalSort>,
            category: Option<ProposalCategory>,
            detail: Option<DetailLevel>,
            search_mode: Option<SearchMode>,
        }

        let schema = schemars::schema_for!(Container);
        let value = serde_json::to_value(&schema).unwrap();
        let blob = value.to_string();

        assert!(
            value.get("$defs").is_none(),
            "no $defs should be emitted for enum-only container; got schema: {value}"
        );
        assert!(
            !blob.contains("$ref"),
            "enum container schema must contain no $ref anywhere; got: {value}"
        );

        // And the inlined body should still have enum values.
        let target_type_enum = value["properties"]["target_type"]["enum"]
            .as_array()
            .expect("target_type should have inline `enum` array");
        assert!(
            target_type_enum
                .contains(&serde_json::Value::String("post".into()))
        );
        assert!(
            target_type_enum
                .contains(&serde_json::Value::String("comment".into()))
        );
    }

    /// `SearchMode` is new (0.19) and used both as `search`'s `mode` input
    /// parameter and as `SearchResponse::mode_used` — an input-side `$ref`
    /// is exactly the class of bug `enum_json_schema_is_inlined` above
    /// guards against for the older enums; pin it here too so a future
    /// derive on `SearchMode` specifically can't reintroduce one.
    #[cfg(feature = "schemars")]
    #[test]
    fn search_mode_schema_is_ref_free() {
        use schemars::JsonSchema;

        assert!(<SearchMode as JsonSchema>::inline_schema());

        let schema = schemars::schema_for!(SearchMode);
        let value = serde_json::to_value(&schema).unwrap();
        let blob = value.to_string();
        assert!(value.get("$defs").is_none(), "no $defs: {value}");
        assert!(!blob.contains("$ref"), "no $ref: {value}");

        // Per-variant doc comments (the descriptions this PR relies on to
        // explain `degraded` fallback semantics) turn the schema from a
        // flat `enum` array into `oneOf` with a `const` per variant — see
        // `TargetType`'s `Message` variant above for why a *plain* enum
        // stays `enum`-shaped. Either way it must carry every value.
        let variants = value["oneOf"]
            .as_array()
            .expect("SearchMode should have an inline `oneOf` array");
        let consts: Vec<&str> = variants
            .iter()
            .filter_map(|v| v["const"].as_str())
            .collect();
        assert!(consts.contains(&"keyword"), "{value}");
        assert!(consts.contains(&"semantic"), "{value}");
    }
}
