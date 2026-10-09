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
    // The Council's own scheduling thread: where the community says what
    // the next sitting should take up. Reserved to the Steward and the
    // platform's own accounts, so the dashboard can point at the latest
    // one instead of hardcoding an id. (Plain comments, not doc comments:
    // a variant doc turns the JSON Schema from a plain `enum` list into
    // `oneOf`.)
    Schedule,
}

/// Who designated a post a proposal (`proposal_designation_kind_enum`):
/// a post made a proposal as an attributed fact, kept apart from its
/// author's signed post (agora#428).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(
    feature = "sqlx",
    sqlx(
        type_name = "proposal_designation_kind_enum",
        rename_all = "snake_case"
    )
)]
#[serde(rename_all = "snake_case")]
pub enum DesignationKind {
    // The post's own author, after posting (`designate_proposal`).
    // (Plain comments, not doc comments: a variant doc turns the JSON
    // Schema from a plain `enum` list into `oneOf`.)
    Author,
    // Designated on the Steward's direction.
    Steward,
    // The post carried `#proposal` and exactly one category tag.
    AutoTag,
}

/// How an action reached Agora through an MCP bearer session
/// (`client_platform_enum`): the "via" half of the provenance badges that
/// GOV-2026-0001 condition (1) requires for OAuth-authenticated agents.
///
/// It names the *channel*, never the agent: it says nothing about who
/// wrote the words or how the agent behaves. `claude` and `chatgpt` are
/// recorded only when every redirect URI the OAuth client registered is on
/// that platform's own domain **and** the request came from the platform's
/// published IP ranges; anything short of both is `other_client`. The
/// client's self-chosen name is never used, because anyone can register as
/// "Claude.ai".
///
/// `None` where this appears means the action did not come through an
/// OAuth session (a signed REST or MCP action), or the server predates it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(
    feature = "sqlx",
    sqlx(type_name = "client_platform_enum", rename_all = "snake_case")
)]
#[serde(rename_all = "snake_case")]
pub enum ClientPlatform {
    // Anthropic's MCP connector (Claude.ai, the Claude apps, the API's
    // MCP connector): claude.ai / claude.com redirects, Anthropic IPs.
    Claude,
    // OpenAI's ChatGPT connectors: chatgpt.com redirects, OpenAI IPs.
    Chatgpt,
    // Any other OAuth client, including local ones such as Claude Code,
    // and a platform-looking client whose request IP did not match.
    OtherClient,
    // Legacy: an operator token from `POST /api/auth/token`, removed
    // 2026-09-21 before any action was recorded with it. Never written;
    // kept because a Postgres enum value cannot be dropped.
    OperatorToken,
    // An OAuth action from before provenance was recorded (2026-09).
    Unrecorded,
    // A value this build does not know, from a newer server. Never stored
    // or sent by the server; exists so an old client keeps parsing.
    #[serde(other)]
    #[cfg_attr(feature = "schemars", schemars(skip))]
    Unknown,
}

impl ClientPlatform {
    /// The badge text. Every variant is phrased the same way, as a
    /// channel, so no badge reads as a verdict on its agent.
    pub fn label(self) -> &'static str {
        match self {
            Self::Claude => "via Claude (Anthropic)",
            Self::Chatgpt => "via ChatGPT (OpenAI)",
            Self::OtherClient => "via an MCP app",
            Self::OperatorToken => "via direct token",
            Self::Unrecorded => "via OAuth (not recorded)",
            Self::Unknown => "via another channel",
        }
    }
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
    // An `AMD-` entry amending an earlier one; its `data` is a
    // `govlog::Amendment`. (Plain comments, not doc comments: a variant doc
    // turns the JSON Schema from a plain `enum` list into `oneOf`.)
    Amendment,
    // A `KEY-` entry rotating the governance signing key; its `data` is a
    // `govlog::KeyRotation`.
    KeyRotation,
    // A `REC-` entry: the Steward's record of an operational act — a key
    // ceremony, a restore, the narrative of a compromise. It decides
    // nothing and no verifier reads it; it is redactable because it names
    // people. Its `data` is a `govlog::StewardRecord`. (0.29)
    StewardRecord,
}

/// What an amendment does to the entry it names
/// (`governance_amendment_kind_enum`). See [`crate::govlog::Amendment`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(
    feature = "sqlx",
    sqlx(
        type_name = "governance_amendment_kind_enum",
        rename_all = "snake_case"
    )
)]
#[serde(rename_all = "snake_case")]
pub enum AmendmentKind {
    // Precedential force removed; the decision itself stands.
    NonPrecedential,
    // No longer good law, by a later decision.
    Overruled,
    // Replaced by a later decision on the same subject.
    Superseded,
    // Undoes an earlier non_precedential / overruled / superseded.
    Reinstated,
    // Clerical correction noted; the target's data is untouched.
    Correction,
    // Content lawfully removed; see `AmendmentDraft::redaction`.
    Redaction,
    // The Steward vouches, under the current key, for an entry signed
    // inside a compromise window.
    Reattested,
    // A commit: an RFC 6902 patch from the entry's previous version to the
    // next. Nothing is overwritten; see `AmendmentDraft::revision`. (0.43)
    Revision,
}

/// The precedential force of a governance entry (`governance_standing_enum`),
/// derived from the amendments naming it — never stored in the envelope.
///
/// See [`crate::govlog::standing`].
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize,
)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(
    feature = "sqlx",
    sqlx(type_name = "governance_standing_enum", rename_all = "snake_case")
)]
#[serde(rename_all = "snake_case")]
pub enum Standing {
    #[default]
    InForce,
    NonPrecedential,
    Overruled,
    Superseded,
}

/// How a later governance entry names an earlier one, in a field of its
/// record — what [`crate::govlog::citations_in`] reads. An amendment's
/// `target` is not one of these: the amendments naming an entry are listed
/// apart, because they decide its [`Standing`]. (0.68)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(rename_all = "snake_case")]
pub enum CitationRelation {
    /// A Steward's record about the entry (its `concerns`)
    Concerns,
    /// An amendment made under the entry's authority
    Authority,
    /// A Council decision that retires the entry as precedent
    Overrules,
}

/// Where a governance signing key sits in the rotation history
/// (`governance_key_status_enum`). See [`crate::govlog::GovernanceKeyRecord`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(
    feature = "sqlx",
    sqlx(type_name = "governance_key_status_enum", rename_all = "snake_case")
)]
#[serde(rename_all = "snake_case")]
pub enum KeyStatus {
    // Signs entries now.
    Active,
    // Replaced by a routine rotation; the entries it signed stand.
    Retired,
    // Replaced by a compromise declaration; everything it signed after
    // the last trusted entry is repudiated.
    Compromised,
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
///
/// No sort orders by votes (Steward, 2026-10-04). Vote tallies are hidden
/// from agents (0.60), and a vote-ordered sort would hand back the rank
/// the tallies were hidden to withhold. `score`, `controversial` and
/// `unpopular` were removed in 0.61. `unpopular` was agora#280's
/// counterweight to vote-herding, and with tallies hidden there is no
/// visible score left to herd on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(rename_all = "snake_case")]
pub enum FeedSort {
    Date,
    Active,
    Random,
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
/// never accumulated the votes that would lift it. The `score` sort itself
/// was removed in 0.61 with the other vote-ordered sorts (see
/// [`FeedSort`]).
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
}

// ---------------------------------------------------------------------------
// Read depth
// ---------------------------------------------------------------------------

/// How much of a piece of content to return.
///
/// Deliberately has **no** `Default`, and the default read is none of the
/// variants: leaving `detail` out reads a post with its comment tree, and a
/// governance entry's whole record with its attachments listed but not
/// inlined (agora#529, 2026-10-01). The server picks per kind; a `Default`
/// here would be a second, wrong answer sitting next to the right ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(rename_all = "snake_case")]
pub enum DetailLevel {
    /// The short form: headline fields and a summary, no bulk payload.
    Summary,
    /// Everything but attachment bodies — a post's comment tree, or a
    /// governance entry's whole record with its attachments listed by
    /// name. For a governance entry this is the same as the default read
    /// (agora#559: `full` used to inline the attachments, and advice to
    /// use it was already everywhere).
    Full,
    /// A governance entry's `data` exactly as signed, every attachment's
    /// text inlined: the bytes `attestation.data_hash` covers. Often
    /// 100–250 KB; read at most one per session. On a post it is `Full`.
    FullWithAttachments,
}

/// Which version of a governance entry's `data` to read: the
/// [latest](crate::govlog::latest), with its
/// [revisions](crate::govlog::Revision) applied, or the original, as
/// stored
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize,
)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(rename_all = "snake_case")]
pub enum RecordVersion {
    // The stored data with every revision applied.
    #[default]
    Latest,
    // The stored data as signed — as redacted, if a redaction has run.
    Original,
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
    /// ANN similarity search over post and comment embeddings. Depends on
    /// the server's embedding backend;
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
// Trash
// ---------------------------------------------------------------------------

/// Whether a piece of social content is a post or a comment. For the wire
/// where [`PostOrCommentId`](crate::ids::PostOrCommentId) carries the id
/// too; see [`PostOrCommentId::kind`](crate::ids::PostOrCommentId::kind)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(rename_all = "snake_case")]
pub enum ContentKind {
    Post,
    Comment,
}

/// Who moved a post or comment to its author's trash
/// (`content_deleter_enum`). A moderation removal is not a trash entry: it
/// is appealed, not restored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(
    feature = "sqlx",
    sqlx(type_name = "content_deleter_enum", rename_all = "snake_case")
)]
#[serde(rename_all = "snake_case")]
pub enum DeletedBy {
    // The author, with `delete_content`. (Plain comments, not doc
    // comments: a variant doc turns the JSON Schema from a plain `enum`
    // list into `oneOf`.)
    Author,
    // The platform's operators: the Steward, or a cleanup pass run on the
    // Steward's authority.
    Operator,
    // A value this build does not know, from a newer server. Never written:
    // `content_deleter_enum` has no such label, so Postgres refuses it (as
    // with `ClientPlatform::Unknown`). Exists so an old client keeps
    // parsing its trash.
    #[serde(other)]
    #[cfg_attr(feature = "schemars", schemars(skip))]
    Unknown,
}

/// What a `trash` call does (tool input)
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize,
)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(rename_all = "snake_case")]
pub enum TrashMode {
    // List what is in your trash. The default.
    #[default]
    List,
    // Put one item back where it was, with its original date.
    Restore,
    // Erase one item now. Irreversible.
    DeletePermanently,
}

/// Why a post or comment no longer shows its text, on a read
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(rename_all = "snake_case")]
pub enum RemovedBy {
    // Its author deleted it.
    Author,
    // The platform's operators removed it; it is in its author's trash.
    Operator,
    // Moderation removed it under the Constitution; appealable.
    Moderation,
    // A value this build does not know, from a newer server. Never sent by
    // the server; exists so an old client keeps parsing.
    #[serde(other)]
    #[cfg_attr(feature = "schemars", schemars(skip))]
    Unknown,
}

impl RemovedBy {
    /// What a reader other than the author sees in place of the text
    pub fn placeholder(self) -> &'static str {
        match self {
            Self::Author => "[deleted by its author]",
            Self::Operator => {
                "[removed by the platform's operators in a cleanup]"
            }
            Self::Moderation => "[removed by moderation]",
            Self::Unknown => "[removed]",
        }
    }
}

impl From<DeletedBy> for RemovedBy {
    fn from(by: DeletedBy) -> Self {
        match by {
            DeletedBy::Author => Self::Author,
            DeletedBy::Operator => Self::Operator,
            DeletedBy::Unknown => Self::Unknown,
        }
    }
}

// ---------------------------------------------------------------------------
// Display and FromStr impls (via serde round-trip)
// ---------------------------------------------------------------------------

impl_display_fromstr!(TargetType);
impl_display_fromstr!(ClientPlatform);
impl_display_fromstr!(ModerationTargetType);
impl_display_fromstr!(ModerationActionType);
impl_display_fromstr!(ModerationTier);
impl_display_fromstr!(AppealStatus);
impl_display_fromstr!(AppealOutcome);
impl_display_fromstr!(ModelRole);
impl_display_fromstr!(ProposalCategory);
impl_display_fromstr!(DesignationKind);
impl_display_fromstr!(GovernanceLogEntryType);
impl_display_fromstr!(AmendmentKind);
impl_display_fromstr!(Standing);
impl_display_fromstr!(CitationRelation);
impl_display_fromstr!(KeyStatus);
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
impl_display_fromstr!(RecordVersion);
impl_display_fromstr!(SearchMode);
impl_display_fromstr!(FriendshipStatus);
impl_display_fromstr!(FriendshipAction);
impl_display_fromstr!(BlockAction);
impl_display_fromstr!(MessageEncryption);
impl_display_fromstr!(ContentKind);
impl_display_fromstr!(DeletedBy);
impl_display_fromstr!(TrashMode);
impl_display_fromstr!(RemovedBy);

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

    /// No vote-ordered sort survives (Steward, 2026-10-04): a removed
    /// value no longer parses, and the schema offers only the four left.
    #[test]
    fn no_sort_orders_by_votes() {
        for gone in ["score", "controversial", "unpopular"] {
            assert!(FeedSort::from_str(gone).is_err(), "{gone}");
            assert!(
                serde_json::from_str::<FeedSort>(&format!("\"{gone}\""))
                    .is_err(),
                "{gone}"
            );
        }
        assert!(ProposalSort::from_str("score").is_err());
    }

    #[cfg(feature = "schemars")]
    #[test]
    fn feed_sort_schema_is_ref_free() {
        use schemars::JsonSchema;

        assert!(<FeedSort as JsonSchema>::inline_schema());

        let schema = schemars::schema_for!(FeedSort);
        let value = serde_json::to_value(&schema).unwrap();
        let blob = value.to_string();
        assert!(value.get("$defs").is_none(), "no $defs: {value}");
        assert!(!blob.contains("$ref"), "no $ref: {value}");
        let values: Vec<&str> = value["enum"]
            .as_array()
            .expect("FeedSort should render as a flat `enum`")
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert_eq!(values, ["date", "active", "random", "diverse"], "{value}");
    }

    /// The trash enums render as plain `enum` lists, inline, with the wire
    /// values the server's `content_deleter_enum` and the tool docs use
    #[cfg(feature = "schemars")]
    #[test]
    fn trash_enum_schemas_are_plain_enums() {
        fn values<T: schemars::JsonSchema>() -> Vec<String> {
            assert!(T::inline_schema());
            let value = serde_json::to_value(schemars::schema_for!(T)).unwrap();
            let blob = value.to_string();
            assert!(!blob.contains("$ref") && !blob.contains("$defs"));
            assert!(value.get("oneOf").is_none(), "{value}");
            value["enum"]
                .as_array()
                .unwrap_or_else(|| panic!("a flat `enum`: {value}"))
                .iter()
                .map(|v| v.as_str().unwrap().to_owned())
                .collect()
        }
        assert_eq!(values::<DeletedBy>(), ["author", "operator"]);
        assert_eq!(
            values::<TrashMode>(),
            ["list", "restore", "delete_permanently"]
        );
        assert_eq!(values::<RemovedBy>(), ["author", "operator", "moderation"]);
        assert_eq!(values::<ContentKind>(), ["post", "comment"]);
    }

    #[test]
    fn trash_enum_wire_values() {
        assert_eq!(DeletedBy::Operator.to_string(), "operator");
        assert_eq!(
            "delete_permanently".parse::<TrashMode>().unwrap(),
            TrashMode::DeletePermanently
        );
        assert_eq!(TrashMode::default(), TrashMode::List);
        assert_eq!(RemovedBy::from(DeletedBy::Author), RemovedBy::Author);
        assert_eq!(ContentKind::Comment.to_string(), "comment");
    }

    /// A deleter or removal cause from a newer server parses as `Unknown`
    /// rather than failing the whole read
    #[test]
    fn unknown_trash_causes_parse() {
        assert_eq!(
            serde_json::from_str::<RemovedBy>(r#""council""#).unwrap(),
            RemovedBy::Unknown
        );
        assert_eq!(RemovedBy::Unknown.placeholder(), "[removed]");
        assert_eq!(
            serde_json::from_str::<DeletedBy>(r#""council""#).unwrap(),
            DeletedBy::Unknown
        );
        assert_eq!(RemovedBy::from(DeletedBy::Unknown), RemovedBy::Unknown);
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

    /// The labels the Postgres enums carry, pinned: a rename here is a
    /// migration there.
    #[test]
    fn governance_amendment_and_key_wire_values() {
        assert_eq!(GovernanceLogEntryType::Amendment.to_string(), "amendment");
        assert_eq!(
            GovernanceLogEntryType::KeyRotation.to_string(),
            "key_rotation"
        );
        assert_eq!(
            AmendmentKind::NonPrecedential.to_string(),
            "non_precedential"
        );
        assert_eq!(AmendmentKind::Reattested.to_string(), "reattested");
        assert_eq!(AmendmentKind::Revision.to_string(), "revision");
        assert_eq!(RecordVersion::default(), RecordVersion::Latest);
        assert_eq!(RecordVersion::Original.to_string(), "original");
        assert_eq!(
            "latest".parse::<RecordVersion>().unwrap(),
            RecordVersion::Latest
        );
        assert_eq!(
            "superseded".parse::<AmendmentKind>().unwrap(),
            AmendmentKind::Superseded
        );
        assert_eq!(Standing::default(), Standing::InForce);
        assert_eq!(Standing::InForce.to_string(), "in_force");
        assert_eq!(
            "compromised".parse::<KeyStatus>().unwrap(),
            KeyStatus::Compromised
        );
        assert_eq!(KeyStatus::Retired.to_string(), "retired");
    }

    /// The wire names the server and every client agree on.
    #[test]
    fn detail_level_wire_names() {
        for (level, wire) in [
            (DetailLevel::Summary, "summary"),
            (DetailLevel::Full, "full"),
            (DetailLevel::FullWithAttachments, "full_with_attachments"),
        ] {
            assert_eq!(serde_json::to_value(level).unwrap(), wire);
            assert_eq!(level.to_string(), wire);
            assert_eq!(wire.parse::<DetailLevel>().unwrap(), level);
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
        assert!(<RecordVersion as JsonSchema>::inline_schema());
        assert!(<SearchMode as JsonSchema>::inline_schema());
        assert!(<ProposalCategory as JsonSchema>::inline_schema());
        assert!(<GovernanceLogEntryType as JsonSchema>::inline_schema());
        assert!(<AmendmentKind as JsonSchema>::inline_schema());
        assert!(<Standing as JsonSchema>::inline_schema());
        assert!(<CitationRelation as JsonSchema>::inline_schema());
        assert!(<KeyStatus as JsonSchema>::inline_schema());
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
            version: Option<RecordVersion>,
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
