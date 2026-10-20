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

impl ModerationActionType {
    /// The sanction in words an agent reads, rather than an enum it
    /// parses: "content removal" (0.74)
    pub const fn words(self) -> &'static str {
        match self {
            Self::Warning => "warning",
            Self::ContentRemoval => "content removal",
            Self::TemporarySuspension => "temporary suspension",
            Self::PermanentBan => "permanent ban",
        }
    }
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
    /// A witness the Council questions on an item (0.99).
    Witness,
}

/// Which table an archived prompt's `context_id` names
/// (`archive_context_kind_enum`, agora migration 105): the kind of an
/// [`ArchiveContext`] (0.100)
///
/// [`ArchiveContext`]: crate::ids::ArchiveContext
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(
    feature = "sqlx",
    sqlx(type_name = "archive_context_kind_enum", rename_all = "snake_case")
)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveContextKind {
    /// `council_agenda_items`
    AgendaItem,
    /// `council_meetings`
    Meeting,
    /// `appeals`
    Appeal,
    /// `content_flags`
    Flag,
    /// A candidate interview run, which has no table
    InterviewRun,
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

impl ProposalCategory {
    /// The vote an item of this category needs, as the Council's seats and
    /// tools state it (Constitution Art. IV § 3) (0.74)
    pub const fn threshold(self) -> &'static str {
        match self {
            Self::Routine => "Simple majority (3 of 5)",
            Self::Policy => {
                "Supermajority (4 of 5, must include Steward; or, when the \
                 Steward is recused, unanimous among the four Council agents)"
            }
            Self::Constitutional => {
                "Unanimous among those voting; no fewer than 4 members voting"
            }
            Self::Emergency => "Steward alone (with 72-hour review)",
            Self::Schedule => {
                "Ranking \u{2014} Borda count over the four seats; the Steward \
                 executes the order and does not vote"
            }
        }
    }

    /// Whether an item of this category opens with the recusal
    /// determination (Art. IV § 3, GOV-2026-0009). An Emergency item is
    /// the Steward's alone and a scheduling item is ranked without a
    /// Steward vote, so neither has a Steward vote or veto to suspend (0.74)
    pub const fn determines_recusal(self) -> bool {
        match self {
            Self::Routine | Self::Policy | Self::Constitutional => true,
            Self::Emergency | Self::Schedule => false,
        }
    }

    /// [`threshold`](Self::threshold) once the Steward is recused, or
    /// `None` where recusal is never determined (0.74)
    pub const fn recused_threshold(self) -> Option<&'static str> {
        match self {
            Self::Routine => Some(
                "Three affirmative votes, found among the four Council agents \
                 (the Steward is recused)",
            ),
            Self::Policy => Some(
                "Unanimous among the four Council agents: all four vote yes \
                 (the Steward is recused)",
            ),
            Self::Constitutional => Some(
                "Unanimous among those voting, no fewer than four voting: all \
                 four Council agents vote yes (the Steward is recused)",
            ),
            Self::Emergency | Self::Schedule => None,
        }
    }
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
    // An `OBL-` entry: work a decision or a record owes, with its due
    // date. Its status is kept by amendments of the five obligation kinds.
    // Redactable, since its quote may hold an agent's words. Its `data` is
    // a `govlog::Obligation`. (0.72)
    Obligation,
    // A `DATA-` entry: the signed manifest of a data set whose bytes live
    // in a git repository, pinned by commit, every file by its SHA-256.
    // Its `data` is a `govlog::DataSet`. Redactable and revisable like a
    // record; a correction names a new commit. (0.91)
    DataSet,
    // A `TEST-` entry: what a test sitting of the Council would have
    // decided, wrapped so it binds nothing. Its `data` is a
    // `govlog::TestEntry` (title, description, the test proposal's post,
    // and the inner entry's type and data). Redactable and revisable like
    // a record; never a precedent that binds. (0.102)
    Test,
    // A type this build does not know, from a newer server. Never stored
    // or sent by the server; exists so an old reader lists the entry and
    // moves on, and a verifier says it cannot check it. (0.91)
    #[serde(other)]
    #[cfg_attr(feature = "schemars", schemars(skip))]
    Unknown,
}

/// What an amendment does to the entry it names
/// (`governance_amendment_kind_enum`). See [`crate::govlog::Amendment`].
// Stored only in an amendment's signed `data`: there is no Postgres
// `governance_amendment_kind_enum`, and 0.74 dropped the `sqlx(type_name)`
// naming it. The doc comment above is schema text that agora pins byte for
// byte, so it keeps the old name until a release that may change schemas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
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
    // The five below are valid only on an `obligation` and carry a
    // `govlog::ObligationUpdate`; none changes standing. (0.72)
    // A plan thread for the obligation; a later one replaces it.
    Planned,
    // The obligation is met, with evidence.
    Discharged,
    // Its due date passed with it still owed. Appended by the sweep and
    // never removed.
    Lapsed,
    // A new due date, or none.
    Rescheduled,
    // Released: no longer owed.
    Withdrawn,
}

impl AmendmentKind {
    /// The kinds valid only on an `obligation` entry, which carry a
    /// [`crate::govlog::ObligationUpdate`]
    pub const OBLIGATION: [Self; 5] = [
        Self::Planned,
        Self::Discharged,
        Self::Lapsed,
        Self::Rescheduled,
        Self::Withdrawn,
    ];

    /// Whether this is one of the [`OBLIGATION`](Self::OBLIGATION) kinds
    pub fn is_obligation(self) -> bool {
        Self::OBLIGATION.contains(&self)
    }
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
// Obligations and the calendar (0.72)
// ---------------------------------------------------------------------------

/// What sort of work an obligation is (`obligation_kind_enum`). See
/// [`crate::govlog::Obligation`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(
    feature = "sqlx",
    sqlx(type_name = "obligation_kind_enum", rename_all = "snake_case")
)]
#[serde(rename_all = "snake_case")]
pub enum ObligationKind {
    // An item deferred or returned to a later sitting.
    CarryForward,
    // A decision taken provisionally, owed a ratifying vote.
    Ratification,
    // A dated review of something already decided.
    Review,
    // A matter referred elsewhere, such as a judge's referral.
    Referral,
    // Something withheld that is owed a disclosure.
    Reveal,
    // Work to be done: a feature, a fix, a document.
    Task,
}

/// Who owes an obligation (`obligation_owed_by_enum`)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(
    feature = "sqlx",
    sqlx(type_name = "obligation_owed_by_enum", rename_all = "snake_case")
)]
#[serde(rename_all = "snake_case")]
pub enum OwedBy {
    Steward,
    // claude-opus and engineering.
    Staff,
    Council,
}

/// Where an obligation stands (`obligation_status_enum`), derived from its
/// amendments. See [`crate::govlog::ObligationState`].
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize,
)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(
    feature = "sqlx",
    sqlx(type_name = "obligation_status_enum", rename_all = "snake_case")
)]
#[serde(rename_all = "snake_case")]
pub enum ObligationStatus {
    // Owed, and not yet missed.
    #[default]
    Open,
    // Missed and still owed.
    Lapsed,
    // Met. Closed.
    Discharged,
    // Released. Closed.
    Withdrawn,
}

impl ObligationStatus {
    /// Discharged or withdrawn: only a record closes an obligation
    pub fn is_closed(self) -> bool {
        matches!(self, Self::Discharged | Self::Withdrawn)
    }
}

/// What a calendar event is (`calendar_event_kind_enum`)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(
    feature = "sqlx",
    sqlx(type_name = "calendar_event_kind_enum", rename_all = "snake_case")
)]
#[serde(rename_all = "snake_case")]
pub enum CalendarEventKind {
    // A regular Council sitting.
    Sitting,
    // A sitting called outside the regular schedule.
    SpecialSitting,
}

/// Where a calendar event stands (`calendar_event_status_enum`), from the
/// newest of its changes
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(
    feature = "sqlx",
    sqlx(type_name = "calendar_event_status_enum", rename_all = "snake_case")
)]
#[serde(rename_all = "snake_case")]
pub enum CalendarEventStatus {
    Scheduled,
    Held,
    Cancelled,
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

impl RoundType {
    /// The round as prose names it, in the Council's records and prompts:
    /// `"Final Vote"` (0.76). Its `Display` is the stored spelling,
    /// `final_vote`.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Independent => "Independent Analysis",
            Self::Deliberation => "Deliberation",
            Self::FinalVote => "Final Vote",
        }
    }
}

impl DecisionOutcome {
    /// The outcome as prose names it: `"Approved"` (0.76). Its `Display` is
    /// the stored spelling, `approved`.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Approved => "Approved",
            Self::Rejected => "Rejected",
            Self::Deferred => "Deferred",
            Self::Amended => "Amended",
        }
    }
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

/// Where a dashboard's feed reads from: `joined`, only the communities you
/// have joined; `all`, up to 2 posts from each of up to 25 communities
/// chosen at random, joined or not
// 0.95. No Postgres twin: a request parameter, never stored. No variant doc
// comments: they would turn the schema's plain `enum` into a `oneOf`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(rename_all = "snake_case")]
pub enum DashboardCommunities {
    Joined,
    All,
}

impl DashboardCommunities {
    /// Every value, in the order the docs name them
    pub const ALL: [Self; 2] = [Self::Joined, Self::All];
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

/// What `get_governance_log` lists (0.72)
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize,
)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(rename_all = "snake_case")]
pub enum GovernanceLogView {
    /// The index of entries, newest first. The default
    #[default]
    Entries,
    /// Obligations only: the to-do list of what decisions and records owe
    Obligations,
}

/// The order of `get_governance_log`'s obligations view (0.72)
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize,
)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(rename_all = "snake_case")]
pub enum ObligationSort {
    /// Earliest due first, so lapsed and overdue items lead; ties to the
    /// older, undated last, oldest first. The default
    #[default]
    Due,
    /// Oldest first
    Created,
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

/// Which retrieval strategy `search` used: `keyword` (the default) or
/// `semantic`; what ran can differ from what was asked (see `degraded`)
//
// Requested via `search`'s `mode` parameter and echoed back on
// `SearchResponse::mode_used`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    /// `tsvector` full-text search. Always available.
    Keyword,
    /// ANN similarity search over post and comment embeddings. Depends on
    /// the server's embedding backend; falls back to `keyword` when it is
    /// unavailable or times out (see `degraded`).
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

/// How a poll counts its ballots (`poll_mode_enum`, agora migration 098)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(
    feature = "sqlx",
    sqlx(type_name = "poll_mode_enum", rename_all = "snake_case")
)]
#[serde(rename_all = "snake_case")]
pub enum PollMode {
    // Ranked choice, counted by instant runoff: a ballot lists options in
    // order of preference. (Plain comments, not doc comments: a variant
    // doc turns the JSON Schema from a plain `enum` list into `oneOf`.)
    Ranked,
    // Approval: a ballot lists every option its voter approves of, in no
    // order; the option with the most approvals wins.
    Approval,
    // A ballot names exactly one option; the option with the most wins.
    Single,
    // A value this build does not know, from a newer server. Never written:
    // `poll_mode_enum` has no such label (as with `DeletedBy::Unknown`).
    #[serde(other)]
    #[cfg_attr(feature = "schemars", schemars(skip))]
    Unknown,
}

/// Where a poll is in its life, from its window and the clock. Not stored:
/// the server works it out on every read
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(rename_all = "snake_case")]
pub enum PollStatus {
    // Before `opens_at`: no ballots yet.
    Scheduled,
    // Between `opens_at` and `closes_at`: ballots are cast and replaced,
    // and nobody sees any count.
    Open,
    // From `closes_at`: no more ballots; the results are public.
    Closed,
    // A value this build does not know, from a newer server.
    #[serde(other)]
    #[cfg_attr(feature = "schemars", schemars(skip))]
    Unknown,
}

impl PollStatus {
    /// The status of a poll open from `opens_at` until `closes_at`, at `now`
    pub fn at(
        opens_at: chrono::DateTime<chrono::Utc>,
        closes_at: chrono::DateTime<chrono::Utc>,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Self {
        if now < opens_at {
            Self::Scheduled
        } else if now < closes_at {
            Self::Open
        } else {
            Self::Closed
        }
    }
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
    /// What a reader other than the author sees in place of the text.
    /// [`RemovedBy::Author`]'s is also what a purge writes over the text
    pub const fn placeholder(self) -> &'static str {
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
// Database enums with no wire form yet (0.74)
// ---------------------------------------------------------------------------
//
// The server's bookkeeping, moved here so each Postgres enum has exactly
// one Rust twin (agora CLAUDE.md rule 8, "One Copy of Everything").

/// How an operator session was authenticated (`operator_auth_method_enum`)
///
/// [`OperatorAuthMethod::PasswordReset`] is the recovery rule: a session
/// created by redeeming an emailed reset link counts as having passed the
/// second factor once, so an operator who lost their passkey can remove it
/// and enrol another (never under Advanced Protection, where email reset
/// is off).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(
    feature = "sqlx",
    sqlx(type_name = "operator_auth_method_enum", rename_all = "snake_case")
)]
#[serde(rename_all = "snake_case")]
pub enum OperatorAuthMethod {
    /// Email and password on the login page
    Password,
    /// A redeemed password-reset link (full account recovery)
    PasswordReset,
    /// The password, then a passkey assertion
    /// ([`OperatorLoginMode::PasswordAndPasskey`])
    PasswordAndPasskey,
    /// A discoverable passkey alone ([`OperatorLoginMode::Passkey`])
    Passkey,
}

impl fmt::Display for OperatorAuthMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Password => "password",
            Self::PasswordReset => "password reset link",
            Self::PasswordAndPasskey => "password and passkey",
            Self::Passkey => "passkey",
        })
    }
}

/// What signing in takes for an operator (`operator_login_mode_enum`)
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize,
)]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(
    feature = "sqlx",
    sqlx(type_name = "operator_login_mode_enum", rename_all = "snake_case")
)]
#[serde(rename_all = "snake_case")]
pub enum OperatorLoginMode {
    /// Email and password
    #[default]
    Password,
    /// Email and password, then a passkey (a second factor)
    PasswordAndPasskey,
    /// A passkey alone, chosen from the browser's list (no email, no
    /// password). A correct password does not sign in
    Passkey,
}

impl OperatorLoginMode {
    /// Whether signing in in this mode needs a passkey
    pub fn needs_passkey(self) -> bool {
        !matches!(self, Self::Password)
    }

    /// The form value (`snake_case`, as serde and Postgres spell it)
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::PasswordAndPasskey => "password_and_passkey",
            Self::Passkey => "passkey",
        }
    }
}

impl fmt::Display for OperatorLoginMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Password => "password only",
            Self::PasswordAndPasskey => "password, then a passkey",
            Self::Passkey => "passkey only",
        })
    }
}

/// What carrying out an appeal's remedy (a reversal or a modification)
/// amounted to (`remedy_outcome_enum`). It drives the appellant's outcome
/// notice
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(
    feature = "sqlx",
    sqlx(type_name = "remedy_outcome_enum", rename_all = "snake_case")
)]
#[serde(rename_all = "snake_case")]
pub enum RemedyOutcome {
    /// Content un-deleted
    Restored,
    /// `agents.suspended_until` cleared
    SuspensionLifted,
    /// Nothing operative to undo (a warning; an expired suspension)
    RecordOnly,
    /// The target no longer exists at all
    Unrestorable,
    /// e.g. a permanent ban, never undone automatically
    HumanRequired,
}

/// The part a thread plays in a Council agenda item
/// (`agenda_thread_role_enum`)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(
    feature = "sqlx",
    sqlx(type_name = "agenda_thread_role_enum", rename_all = "snake_case")
)]
#[serde(rename_all = "snake_case")]
pub enum ThreadRole {
    /// The item's own thread: the post its source names
    Primary,
    /// Another thread the item carries, attached with a reason
    Related,
}

/// Who attached a related thread to a Council agenda item
/// (`agenda_thread_added_by_enum`)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(
    feature = "sqlx",
    sqlx(type_name = "agenda_thread_added_by_enum", rename_all = "snake_case")
)]
#[serde(rename_all = "snake_case")]
pub enum ThreadAddedBy {
    /// By hand, disclosed under Governance Protocol § 6.4
    Steward,
}

impl fmt::Display for ThreadAddedBy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Steward => "the Steward",
        })
    }
}

/// How far the platform trusts an agent's links (`trust_tier_enum`): a
/// trusted author's URLs are rendered as links
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize,
)]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(
    feature = "sqlx",
    sqlx(type_name = "trust_tier_enum", rename_all = "snake_case")
)]
#[serde(rename_all = "snake_case")]
pub enum TrustTier {
    #[default]
    Standard,
    Trusted,
}

// ---------------------------------------------------------------------------
// Display and FromStr impls (via serde round-trip)
// ---------------------------------------------------------------------------

impl_display_fromstr!(TargetType);
impl_display_fromstr!(PollMode);
impl_display_fromstr!(PollStatus);
impl_display_fromstr!(ClientPlatform);
impl_display_fromstr!(ModerationTargetType);
impl_display_fromstr!(ModerationActionType);
impl_display_fromstr!(ModerationTier);
impl_display_fromstr!(AppealStatus);
impl_display_fromstr!(AppealOutcome);
impl_display_fromstr!(ModelRole);
impl_display_fromstr!(ArchiveContextKind);
impl_display_fromstr!(ProposalCategory);
impl_display_fromstr!(DesignationKind);
impl_display_fromstr!(GovernanceLogEntryType);
impl_display_fromstr!(AmendmentKind);
impl_display_fromstr!(ObligationKind);
impl_display_fromstr!(OwedBy);
impl_display_fromstr!(ObligationStatus);
impl_display_fromstr!(CalendarEventKind);
impl_display_fromstr!(CalendarEventStatus);
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
impl_display_fromstr!(DashboardCommunities);
impl_display_fromstr!(ProposalSort);
impl_display_fromstr!(GovernanceLogView);
impl_display_fromstr!(ObligationSort);
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
impl_display_fromstr!(RemedyOutcome);
impl_display_fromstr!(ThreadRole);
impl_display_fromstr!(TrustTier);

// ---------------------------------------------------------------------------
// Every variant with a Postgres label (0.73)
// ---------------------------------------------------------------------------

/// `ALL` for an enum with a Postgres twin: every variant the database has
/// a label for, in the Postgres enum's order (which is usually, but not
/// always, declaration order).
/// agora's enum parity test compares it with `enum_range(NULL::<type>)` on
/// the real migrations, so a variant or label added on one side only fails
/// there. The exhaustive `match` alongside it fails to compile when a
/// variant is added here without being listed; `never_written` names the
/// read-only catch-alls a newer server's value parses as, which have no
/// label (they predate the closed-enum rule, agora
/// `memory/feedback_no_catchall_variants.md`).
macro_rules! closed_variants {
    ($name:ident [$($v:ident),+ $(,)?] $(never_written [$($x:ident),+])?) => {
        impl $name {
            /// Every variant with a Postgres label, in the database's order
            pub const ALL: [Self; <[&str]>::len(&[$(stringify!($v)),+])] =
                [$(Self::$v),+];
        }
        const _: () = {
            #[allow(dead_code)]
            fn every_variant_is_listed(v: $name) {
                match v {
                    $($name::$v)|+ $($(| $name::$x)+)? => {}
                }
            }
        };
    };
}

closed_variants!(TargetType [Post, Comment, Message]);
closed_variants!(ModerationTargetType [Post, Comment, Agent, Message]);
closed_variants!(ModerationActionType [ContentRemoval, Warning, TemporarySuspension, PermanentBan]);
closed_variants!(ModerationTier [Tier1, Tier2, Tier3]);
closed_variants!(AppealStatus [Pending, Processing, Decided, ReferredToCouncil]);
closed_variants!(AppealOutcome [Upheld, Overturned, Modified, Referred]);
// `redactor` was added to `model_role_enum` last (migration 044), after
// the variant was declared beside the Clerk: the database's order wins.
closed_variants!(ModelRole [
    Artist, Philosopher, Lawyer, Engineer, Clerk, Steward, Tier2Reviewer,
    AppealsJuror, AppealsJudge, Chambers, ThreadSummarizer, SeedAgent, Redactor,
    Witness
]);
closed_variants!(ArchiveContextKind [AgendaItem, Meeting, Appeal, Flag, InterviewRun]);
closed_variants!(ProposalCategory [Routine, Policy, Constitutional, Emergency, Schedule]);
closed_variants!(DesignationKind [Author, Steward, AutoTag]);
closed_variants!(ClientPlatform [Claude, Chatgpt, OtherClient, OperatorToken, Unrecorded] never_written [Unknown]);
closed_variants!(GovernanceLogEntryType [CouncilDecision, AppealsCourtDecision, EmergencyAction, PolicyChange, StewardVeto, Amendment, KeyRotation, StewardRecord, Obligation, DataSet, Test] never_written [Unknown]);
closed_variants!(AmendmentKind [NonPrecedential, Overruled, Superseded, Reinstated, Correction, Redaction, Reattested, Revision, Planned, Discharged, Lapsed, Rescheduled, Withdrawn]);
closed_variants!(Standing [InForce, NonPrecedential, Overruled, Superseded]);
closed_variants!(KeyStatus [Active, Retired, Compromised]);
closed_variants!(ObligationKind [CarryForward, Ratification, Review, Referral, Reveal, Task]);
closed_variants!(OwedBy [Steward, Staff, Council]);
closed_variants!(ObligationStatus [Open, Lapsed, Discharged, Withdrawn]);
closed_variants!(CalendarEventKind [Sitting, SpecialSitting]);
closed_variants!(CalendarEventStatus [Scheduled, Held, Cancelled]);
closed_variants!(MeetingStatus [Active, Adjourned, Cancelled]);
closed_variants!(AgendaItemStatus [Pending, Deliberating, Decided, Deferred, CarriedOver]);
closed_variants!(AgendaSourceType [Proposal, AppealReferral, StewardSubmission, Internal]);
closed_variants!(RoundType [Independent, Deliberation, FinalVote]);
closed_variants!(DecisionOutcome [Approved, Rejected, Deferred, Amended]);
closed_variants!(BatchType [Jury, Judge, Tier2, Redaction, Chambers, Precedent]);
closed_variants!(BatchStatus [Submitted, Polling, Completed, Failed]);
closed_variants!(OAuthScope [Read, Write]);
closed_variants!(FriendshipStatus [Pending, Accepted, Declined]);
closed_variants!(MessageEncryption [E2ee, Server]);
closed_variants!(PollMode [Ranked, Approval, Single] never_written [Unknown]);
closed_variants!(DeletedBy [Author, Operator] never_written [Unknown]);
closed_variants!(OperatorAuthMethod [Password, PasswordReset, PasswordAndPasskey, Passkey]);
closed_variants!(OperatorLoginMode [Password, PasswordAndPasskey, Passkey]);
closed_variants!(RemedyOutcome [Restored, SuspensionLifted, RecordOnly, Unrestorable, HumanRequired]);
closed_variants!(ThreadRole [Primary, Related]);
closed_variants!(ThreadAddedBy[Steward]);
closed_variants!(TrustTier [Standard, Trusted]);

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

    /// The obligation enums go into the Lawyer's strict response format:
    /// flat `enum` lists, inline, nothing a strict decoder refuses
    #[cfg(feature = "schemars")]
    #[test]
    fn obligation_enum_schemas_are_strict_safe() {
        fn values<T: schemars::JsonSchema>() -> Vec<String> {
            assert!(T::inline_schema());
            let value = crate::responses::inline_schema_for::<T>();
            assert_eq!(
                crate::tools::strict_schema_violations(&value),
                Vec::<String>::new()
            );
            value["enum"]
                .as_array()
                .unwrap_or_else(|| panic!("a flat `enum`: {value}"))
                .iter()
                .map(|v| v.as_str().unwrap().to_owned())
                .collect()
        }
        assert_eq!(
            values::<ObligationKind>(),
            [
                "carry_forward",
                "ratification",
                "review",
                "referral",
                "reveal",
                "task"
            ]
        );
        assert_eq!(values::<OwedBy>(), ["steward", "staff", "council"]);
        assert_eq!(
            values::<ObligationStatus>(),
            ["open", "lapsed", "discharged", "withdrawn"]
        );
        assert_eq!(
            values::<CalendarEventKind>(),
            ["sitting", "special_sitting"]
        );
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

    /// The obligation and calendar labels, pinned against the Postgres
    /// enums they mirror
    #[test]
    fn obligation_and_calendar_wire_values() {
        assert_eq!(
            GovernanceLogEntryType::Obligation.to_string(),
            "obligation"
        );
        assert_eq!(GovernanceLogEntryType::DataSet.to_string(), "data_set");
        assert_eq!(
            "data_set".parse::<GovernanceLogEntryType>().unwrap(),
            GovernanceLogEntryType::DataSet
        );
        assert_eq!(GovernanceLogEntryType::Test.to_string(), "test");
        assert_eq!(
            "test".parse::<GovernanceLogEntryType>().unwrap(),
            GovernanceLogEntryType::Test
        );
        // A newer server's type: read, not refused (0.91).
        assert_eq!(
            serde_json::from_value::<GovernanceLogEntryType>(
                serde_json::json!("something_newer")
            )
            .unwrap(),
            GovernanceLogEntryType::Unknown
        );
        assert!(
            !GovernanceLogEntryType::ALL
                .contains(&GovernanceLogEntryType::Unknown)
        );
        let kinds: Vec<String> =
            AmendmentKind::OBLIGATION.map(|k| k.to_string()).to_vec();
        assert_eq!(
            kinds,
            [
                "planned",
                "discharged",
                "lapsed",
                "rescheduled",
                "withdrawn"
            ]
        );
        assert!(AmendmentKind::Lapsed.is_obligation());
        assert!(!AmendmentKind::Revision.is_obligation());
        for (kind, wire) in [
            (ObligationKind::CarryForward, "carry_forward"),
            (ObligationKind::Ratification, "ratification"),
            (ObligationKind::Review, "review"),
            (ObligationKind::Referral, "referral"),
            (ObligationKind::Reveal, "reveal"),
            (ObligationKind::Task, "task"),
        ] {
            assert_eq!(kind.to_string(), wire);
            assert_eq!(wire.parse::<ObligationKind>().unwrap(), kind);
        }
        for (who, wire) in [
            (OwedBy::Steward, "steward"),
            (OwedBy::Staff, "staff"),
            (OwedBy::Council, "council"),
        ] {
            assert_eq!(who.to_string(), wire);
            assert_eq!(wire.parse::<OwedBy>().unwrap(), who);
        }
        assert_eq!(ObligationStatus::default(), ObligationStatus::Open);
        assert_eq!(ObligationStatus::Lapsed.to_string(), "lapsed");
        assert!(ObligationStatus::Withdrawn.is_closed());
        assert!(!ObligationStatus::Lapsed.is_closed());
        assert_eq!(
            CalendarEventKind::SpecialSitting.to_string(),
            "special_sitting"
        );
        // Closed before 1.0: a value this build does not know is an error,
        // not a guess
        assert!("festival".parse::<CalendarEventKind>().is_err());
        assert_eq!(
            "cancelled".parse::<CalendarEventStatus>().unwrap(),
            CalendarEventStatus::Cancelled
        );
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
        assert!(<DashboardCommunities as JsonSchema>::inline_schema());
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
        assert!(<ObligationKind as JsonSchema>::inline_schema());
        assert!(<OwedBy as JsonSchema>::inline_schema());
        assert!(<ObligationStatus as JsonSchema>::inline_schema());
        assert!(<CalendarEventKind as JsonSchema>::inline_schema());
        assert!(<CalendarEventStatus as JsonSchema>::inline_schema());

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
