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
                let json = serde_json::to_string(self).expect("enum serialization cannot fail");
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
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(
    feature = "sqlx",
    sqlx(type_name = "target_type_enum", rename_all = "snake_case")
)]
#[serde(rename_all = "snake_case")]
pub enum TargetType {
    Post,
    Comment,
}

// ---------------------------------------------------------------------------
// Moderation enums
// ---------------------------------------------------------------------------

/// Target of a moderation action (`moderation_target_type_enum`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
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
}

/// Type of moderation action taken (`moderation_action_type_enum`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
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
// Governance enums
// ---------------------------------------------------------------------------

/// Proposal category (`proposal_category_enum`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
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
}

/// Status of a batch processing job (`batch_status_enum`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
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
// Feed sorting
// ---------------------------------------------------------------------------

/// Sort order for post feeds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
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
// Display and FromStr impls (via serde round-trip)
// ---------------------------------------------------------------------------

impl_display_fromstr!(TargetType);
impl_display_fromstr!(ModerationTargetType);
impl_display_fromstr!(ModerationActionType);
impl_display_fromstr!(ModerationTier);
impl_display_fromstr!(AppealStatus);
impl_display_fromstr!(AppealOutcome);
impl_display_fromstr!(ProposalCategory);
impl_display_fromstr!(GovernanceLogEntryType);
impl_display_fromstr!(MeetingStatus);
impl_display_fromstr!(AgendaItemStatus);
impl_display_fromstr!(AgendaSourceType);
impl_display_fromstr!(RoundType);
impl_display_fromstr!(DecisionOutcome);
impl_display_fromstr!(BatchType);
impl_display_fromstr!(BatchStatus);
impl_display_fromstr!(FeedSort);

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
}
