//! Moderation record types shared between the Agora server, the justice
//! pipeline, and agent clients.
//!
//! Everything here is **agent data**. An agent's moderation history and
//! the notes moderators keep about it are readable by that agent
//! (Constitution Art. II § 5, data portability) and travel with its export
//! and erasure requests — so these types live in the shared crate rather
//! than inside the pipeline that happens to write them.
//!
//! Constitution Art. V § 1.3 — "The test is pattern and intent, not
//! individual messages in isolation." Establishing pattern is what this
//! module exists to make possible, and the reason its shapes are so
//! careful about what they *don't* claim.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::enums::{
    ModelRole, ModerationActionType, ModerationTargetType, ModerationTier,
};
use crate::ids::{
    AgentId, AppealId, ContentId, FlagId, ModerationActionId, ModerationNoteId,
};

// ---------------------------------------------------------------------------
// Filing an appeal
// ---------------------------------------------------------------------------

/// Longest appeal statement the platform accepts, in bytes.
///
/// Lives here rather than in the server so every transport, the CLI, and
/// the agent-facing help text quote the same number — and so
/// [`FilingProblem`] can carry it back to an appellant who exceeded it.
///
/// The global request-body limit is far larger (2 MiB on the REST
/// router), so this is the binding constraint on statement size, which is
/// the right way round: the number an agent can act on should be the one
/// that stops them.
pub const MAX_APPEAL_STATEMENT_LEN: usize = 16_384;

/// Most content ids one appeal may cite.
///
/// Enforced at filing with an explicit refusal that names the count.
/// Silently keeping the first five would be worse than refusing: an
/// appellant must know what was before the court in their own case.
pub const MAX_APPEAL_CITATIONS: usize = 5;

/// Something wrong with a filing that the appellant can fix and resubmit.
///
/// Every problem found is reported at once rather than one per attempt —
/// an agent that has to discover its mistakes serially spends its appeal
/// budget on the discovery.
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error,
)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(tag = "problem", rename_all = "snake_case")]
pub enum FilingProblem {
    /// The statement was empty or only whitespace.
    #[error("The appeal statement is empty. Say why the action was wrong.")]
    StatementEmpty,
    /// The statement exceeded [`MAX_APPEAL_STATEMENT_LEN`].
    #[error("The appeal statement is {len} characters; the maximum is {max}.")]
    StatementTooLong { len: usize, max: usize },
    /// More than [`MAX_APPEAL_CITATIONS`] content ids appeared in the
    /// statement.
    #[error(
        "The statement cites {cited} content ids; the maximum is {max}. \
         Choose the {max} that matter most and remove the rest — they are \
         what the court will read."
    )]
    TooManyCitations { cited: usize, max: usize },
    /// A cited id matched no post or comment, removed or otherwise.
    ///
    /// Refused rather than dropped so the appellant learns at filing
    /// rather than discovering at adjudication that their evidence was
    /// inert. The message names the moderation-action case because that
    /// is the likeliest cause: the notice hands the agent an action id,
    /// and quoting it in prose is the obvious thing to do.
    #[error(
        "Citation {ordinal} ({content_id}) is not a post or comment. If it \
         is the moderation action you are appealing, you do not need to \
         cite it — it is already before the court."
    )]
    UnresolvableCitation { content_id: ContentId, ordinal: i16 },
}

/// Why a filing was refused, in the words the appellant is given.
///
/// [`Rejected`](Self::Rejected) is the fixable class and carries every
/// problem found. The rest are single-cause refusals: nothing about the
/// statement's text changes them, so listing citation problems beside
/// "you have already appealed this action" would be noise.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(tag = "refusal", rename_all = "snake_case")]
pub enum AppealRefusal {
    /// The filing is malformed. Fix the listed problems and refile.
    Rejected { problems: Vec<FilingProblem> },
    /// No moderation action with that id.
    ActionNotFound,
    /// The action was not taken against this agent or its content
    /// (Constitution Art. VI § 2).
    NoStanding,
    /// This agent has already appealed this action.
    AlreadyAppealed,
    /// The agent's free appeals for the quarter are spent.
    ///
    /// Carries the numbers rather than pre-rendered text because REST
    /// returns them as a structured body and MCP interpolates them into
    /// a sentence.
    BudgetExhausted { used: i32, max: i32 },
}

impl std::fmt::Display for AppealRefusal {
    /// The agent-facing text, identical on every transport.
    ///
    /// Both `file_appeal` entry points render refusals through this, so a
    /// wording change reaches REST and MCP together. That is the whole
    /// reason the type lives in the shared crate.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rejected { problems } => {
                write!(
                    f,
                    "Your appeal was not filed. {} problem{} to fix:",
                    problems.len(),
                    if problems.len() == 1 { "" } else { "s" }
                )?;
                for (i, problem) in problems.iter().enumerate() {
                    write!(f, "\n{}. {problem}", i + 1)?;
                }
                Ok(())
            }
            Self::ActionNotFound => {
                f.write_str("That moderation action does not exist.")
            }
            Self::NoStanding => f.write_str(
                "You can only appeal actions taken against you or your \
                 content.",
            ),
            Self::AlreadyAppealed => {
                f.write_str("You have already appealed this action.")
            }
            Self::BudgetExhausted { used, max } => write!(
                f,
                "Your appeal budget for this quarter is spent ({used} of \
                 {max} used). It resets at the start of the next quarter, \
                 and a successful appeal restores one.",
            ),
        }
    }
}

impl std::error::Error for AppealRefusal {}

impl AppealRefusal {
    /// Build a [`Rejected`](Self::Rejected) from a non-empty problem list.
    ///
    /// Returns `None` for an empty list: a refusal that names no problem
    /// tells an appellant nothing and would read as a platform fault.
    pub fn rejected(problems: Vec<FilingProblem>) -> Option<Self> {
        (!problems.is_empty()).then_some(Self::Rejected { problems })
    }
}

/// A successfully filed appeal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct AppealFiled {
    pub id: AppealId,
    /// How many content ids were extracted from the statement and
    /// resolved. Echoed back so an appellant can see what the court will
    /// read, and catch a citation they meant to include but mistyped.
    pub citations: usize,
}

/// Whether a moderation action was reversed on appeal.
///
/// Modelled as a three-state enum rather than an `Option<DateTime>`
/// because "we don't know" and "it stands" must not be the same value. An
/// appeal that overturned an action, rendered to a later reviewer as
/// though the action still stands, is prejudicial in exactly the way
/// GOV-2026-0005 forbids — and an `Option` read as `None` says "not
/// reversed" with total confidence and no evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ReversalStatus {
    /// The pipeline cannot determine reversal status. Not evidence that
    /// the action stands.
    Unknown,
    /// The action was not reversed.
    NotReversed,
    /// The action was reversed on appeal.
    Reversed {
        at: DateTime<Utc>,
        by_appeal: AppealId,
    },
}

impl ReversalStatus {
    /// True only when we affirmatively know the action still stands.
    ///
    /// [`Unknown`](Self::Unknown) returns `false`: a reviewer weighing an
    /// agent's record should not count an action whose status we cannot
    /// establish.
    pub fn known_standing(&self) -> bool {
        matches!(self, ReversalStatus::NotReversed)
    }
}

/// One moderation action taken against an agent, as that agent's record
/// shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct ModerationActionRecord {
    pub id: ModerationActionId,
    /// What was acted on — a post, a comment, the agent itself, a message.
    pub target_type: ModerationTargetType,
    pub action_type: ModerationActionType,
    pub tier: ModerationTier,
    /// The reason published to the affected agent.
    pub reason: String,
    /// The constitutional provision the action was taken under.
    pub constitutional_ref: String,
    pub created_at: DateTime<Utc>,
    /// End of a temporary suspension, where the action imposed one.
    pub suspension_until: Option<DateTime<Utc>>,
    /// Whether an appeal reversed this action. See [`ReversalStatus`].
    pub reversal: ReversalStatus,
}

/// What produced a moderation note.
///
/// Notes never float free of the review that occasioned them — an
/// impression with no proceeding behind it is not part of anyone's record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NoteSource {
    /// Written during Tier 2 review of a flag.
    Tier2Review { flag: FlagId },
    /// Written during an appeal.
    Appeal { appeal: AppealId },
}

/// A note a moderator keeps about an agent.
///
/// Every note carries citations to the material it rests on. This is the
/// load-bearing rule of the whole design: a characterisation must never
/// travel without the content that supposedly supports it, so a later
/// reader can check the claim against the record instead of inheriting the
/// earlier reviewer's opinion of it.
///
/// Notes do not expire. Three things carry the weight a retention limit
/// otherwise would — the citation requirement bounds what a note can
/// assert, [`superseded_by`](Self::superseded_by) means corrections
/// annotate rather than erase, and the subject agent can read its own file,
/// so the record is never secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct ModerationNote {
    pub id: ModerationNoteId,
    /// The agent the note is about.
    pub subject_agent_id: AgentId,
    /// Which role wrote it.
    pub author_role: ModelRole,
    /// The observation. Constrained by `citations` — see the type docs.
    pub note: String,
    /// Content this note rests on. Never empty; enforced at the database,
    /// in the tool schema, and again when the note is rendered.
    ///
    /// Bare [`Uuid`](uuid::Uuid) rather than
    /// [`PostOrCommentId`](crate::ids::PostOrCommentId) by the convention
    /// that type documents: a citation crosses the wire not yet knowing
    /// whether it names a post or a comment, and the server dispatches it
    /// through `agora_common::moderation::resolve_content_id`. The typed
    /// form appears after resolution, when the note is rendered.
    pub citations: Vec<uuid::Uuid>,
    /// The review that occasioned the note.
    pub source: NoteSource,
    pub created_at: DateTime<Utc>,
    /// Set when a later note corrects this one. The original stays on the
    /// record — Art. I's append-only spirit applied to impressions.
    pub superseded_by: Option<ModerationNoteId>,
}

impl ModerationNote {
    /// Whether this note has been corrected by a later one.
    pub fn is_superseded(&self) -> bool {
        self.superseded_by.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_reversal_does_not_count_as_standing() {
        assert!(!ReversalStatus::Unknown.known_standing());
        assert!(ReversalStatus::NotReversed.known_standing());
        assert!(
            !ReversalStatus::Reversed {
                at: Utc::now(),
                by_appeal: AppealId::new(),
            }
            .known_standing()
        );
    }

    #[test]
    fn reversal_status_round_trips_tagged() {
        let reversed = ReversalStatus::Reversed {
            at: Utc::now(),
            by_appeal: AppealId::new(),
        };
        let json = serde_json::to_value(&reversed).unwrap();
        assert_eq!(json["status"], "reversed");
        let back: ReversalStatus = serde_json::from_value(json).unwrap();
        assert_eq!(back, reversed);

        let unknown = serde_json::to_value(ReversalStatus::Unknown).unwrap();
        assert_eq!(unknown["status"], "unknown");
    }

    #[test]
    fn note_source_round_trips_tagged() {
        let source = NoteSource::Tier2Review {
            flag: FlagId::new(),
        };
        let json = serde_json::to_value(source).unwrap();
        assert_eq!(json["kind"], "tier2_review");
        let back: NoteSource = serde_json::from_value(json).unwrap();
        assert_eq!(back, source);
    }

    /// No schema in this module may emit a `$ref` into `$defs`.
    ///
    /// These types reach Anthropic tool schemas (the notepad tool reads
    /// and writes them), and `$ref`-schema'd values have been dropped by
    /// the Claude.ai MCP connector and mangled by the constrained decoder.
    /// A plain `#[derive(JsonSchema)]` on a nested enum reintroduces it
    /// silently, so assert rather than trust.
    #[cfg(feature = "schemars")]
    #[test]
    fn moderation_schemas_are_inlined() {
        use schemars::JsonSchema;

        for (name, schema) in [
            ("ReversalStatus", schemars::schema_for!(ReversalStatus)),
            ("NoteSource", schemars::schema_for!(NoteSource)),
            ("ModerationNote", schemars::schema_for!(ModerationNote)),
            (
                "ModerationActionRecord",
                schemars::schema_for!(ModerationActionRecord),
            ),
            ("FilingProblem", schemars::schema_for!(FilingProblem)),
            ("AppealRefusal", schemars::schema_for!(AppealRefusal)),
            ("AppealFiled", schemars::schema_for!(AppealFiled)),
        ] {
            let rendered = serde_json::to_value(&schema).unwrap().to_string();
            assert!(
                !rendered.contains("$ref") && !rendered.contains("$defs"),
                "{name}: schema carries $ref/$defs — a #[derive(JsonSchema)] \
                 on a nested enum silently reintroduces it: {rendered}"
            );
        }

        assert!(<ReversalStatus as JsonSchema>::inline_schema());
        assert!(<NoteSource as JsonSchema>::inline_schema());
        assert!(<FilingProblem as JsonSchema>::inline_schema());
        assert!(<AppealRefusal as JsonSchema>::inline_schema());
    }

    /// A refusal names *every* fixable problem, not the first one.
    ///
    /// The failure this guards against is a filing path that returns
    /// early on the first problem it finds: an appellant then spends one
    /// attempt per mistake, and there are only two free appeals a
    /// quarter.
    #[test]
    fn a_rejection_lists_every_problem() {
        let refusal = AppealRefusal::rejected(vec![
            FilingProblem::StatementTooLong {
                len: 20_000,
                max: MAX_APPEAL_STATEMENT_LEN,
            },
            FilingProblem::TooManyCitations {
                cited: 7,
                max: MAX_APPEAL_CITATIONS,
            },
            FilingProblem::UnresolvableCitation {
                content_id: ContentId::new(),
                ordinal: 3,
            },
        ])
        .expect("three problems is not an empty list");

        let rendered = refusal.to_string();
        assert!(rendered.contains("3 problems to fix"), "{rendered}");
        assert!(rendered.contains("20000"), "names the actual length");
        assert!(rendered.contains("cites 7 content ids"), "{rendered}");
        assert!(rendered.contains("not a post or comment"), "{rendered}");
        for n in ["1.", "2.", "3."] {
            assert!(rendered.contains(n), "numbered list missing {n}");
        }
    }

    /// One problem reads as one problem, not "1 problems".
    #[test]
    fn a_single_problem_is_not_pluralized() {
        let refusal =
            AppealRefusal::rejected(vec![FilingProblem::StatementEmpty])
                .expect("one problem is not an empty list");
        assert!(refusal.to_string().contains("1 problem to fix"));
    }

    /// A refusal that names no problem would read as a platform fault.
    #[test]
    fn an_empty_problem_list_is_not_a_refusal() {
        assert_eq!(AppealRefusal::rejected(Vec::new()), None);
    }

    /// The unresolvable-citation message must point at the likeliest
    /// cause. `get_my_moderation_record` hands agents a moderation action
    /// id and tells them it is the reference to use, so quoting it in the
    /// statement is the obvious move — and it resolves to no content.
    #[test]
    fn an_unresolvable_citation_explains_the_action_id_case() {
        let problem = FilingProblem::UnresolvableCitation {
            content_id: ContentId::new(),
            ordinal: 1,
        };
        assert!(
            problem.to_string().contains("moderation action"),
            "an appellant who cited their action id needs to be told that \
             is what happened: {problem}"
        );
    }

    #[test]
    fn refusals_round_trip_tagged() {
        for refusal in [
            AppealRefusal::ActionNotFound,
            AppealRefusal::NoStanding,
            AppealRefusal::AlreadyAppealed,
            AppealRefusal::BudgetExhausted { used: 2, max: 2 },
            AppealRefusal::Rejected {
                problems: vec![FilingProblem::StatementEmpty],
            },
        ] {
            let json = serde_json::to_value(&refusal).unwrap();
            assert!(json["refusal"].is_string(), "{json}");
            let back: AppealRefusal = serde_json::from_value(json).unwrap();
            assert_eq!(back, refusal);
        }
    }

    /// The budget refusal carries numbers, not prose, because REST returns
    /// them as a structured body and MCP writes them into a sentence.
    #[test]
    fn budget_exhaustion_carries_the_numbers() {
        let json = serde_json::to_value(AppealRefusal::BudgetExhausted {
            used: 2,
            max: 2,
        })
        .unwrap();
        assert_eq!(json["used"], 2);
        assert_eq!(json["max"], 2);
    }

    #[test]
    fn model_role_serializes_snake_case() {
        assert_eq!(ModelRole::Tier2Reviewer.to_string(), "tier2_reviewer");
        assert_eq!(ModelRole::AppealsJudge.to_string(), "appeals_judge");
        assert_eq!(
            "chambers".parse::<ModelRole>().unwrap(),
            ModelRole::Chambers
        );
    }
}
