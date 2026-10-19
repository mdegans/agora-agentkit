//! A decision's signed summary (0.96).
//!
//! The Lawyer seat writes it; the Steward accepts it, or the other three
//! seats when the Steward is recused on the item (Steward, 2026-10-17). It
//! is part of the signed record, so changing it after signing takes a
//! [revision](crate::govlog::Revision) patching `/summary`.

use serde::{Deserialize, Serialize};

use super::{CouncilSeat, CouncilVote, RecordRuleError};
use crate::enums::GovernanceLogEntryType;
use crate::govlog::Redactable;

/// The JSON pointer a [revision](crate::govlog::Revision) of the summary
/// patches, at or below
pub const SUMMARY_POINTER: &str = "/summary";

/// The seat that writes every decision's summary
pub const SUMMARY_WRITER: CouncilSeat = CouncilSeat::Lawyer;

/// The seats that accept a summary when the Steward is recused: every seat
/// but [`SUMMARY_WRITER`], in declaration order
pub const RECUSED_ACCEPTORS: [CouncilSeat; 3] = [
    CouncilSeat::Artist,
    CouncilSeat::Philosopher,
    CouncilSeat::Engineer,
];

/// A decision's summary, as signed with the record. See the
/// module docs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct DecisionSummary {
    /// Markdown
    pub text: Redactable<String>,
    /// Always the Lawyer
    pub written_by: CouncilSeat,
    pub accepted_by: SummaryAcceptance,
}

/// Who accepted a [`DecisionSummary`]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(tag = "by", rename_all = "snake_case")]
pub enum SummaryAcceptance {
    /// The Steward, on any item the Steward is not recused from
    Steward,
    /// With the Steward recused, the Artist, Philosopher and Engineer
    Seats { seats: Vec<CouncilSeat> },
}

impl SummaryAcceptance {
    /// Who accepts the summary of an item the Steward is, or is not,
    /// recused from
    pub fn for_item(steward_recused: bool) -> Self {
        if steward_recused {
            Self::Seats {
                seats: RECUSED_ACCEPTORS.to_vec(),
            }
        } else {
            Self::Steward
        }
    }
}

/// Who wrote and accepted a summary, for a reader holding its text: a
/// [`DecisionSummary`] without the text
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct SummaryAuthorship {
    pub written_by: CouncilSeat,
    pub accepted_by: SummaryAcceptance,
}

impl SummaryAuthorship {
    /// "written by the Lawyer, accepted by the Steward"
    pub fn sentence(&self) -> String {
        let accepted = match &self.accepted_by {
            SummaryAcceptance::Steward => "the Steward".to_string(),
            SummaryAcceptance::Seats { seats } => {
                let names: Vec<&str> = seats.iter().map(|s| s.name()).collect();
                match names.as_slice() {
                    [] => String::from("no seat"),
                    [one] => format!("the {one}"),
                    [init @ .., last] => format!(
                        "the {} and {last}, the Steward being recused",
                        init.join(", ")
                    ),
                }
            }
        };
        format!("written by the {}, accepted by {accepted}", self.written_by)
    }
}

impl DecisionSummary {
    /// A summary the Lawyer wrote, accepted by whoever accepts on an item
    /// the Steward is, or is not, recused from
    pub fn by_lawyer(text: impl Into<String>, steward_recused: bool) -> Self {
        Self {
            text: Redactable::Value(text.into()),
            written_by: SUMMARY_WRITER,
            accepted_by: SummaryAcceptance::for_item(steward_recused),
        }
    }

    pub fn authorship(&self) -> SummaryAuthorship {
        SummaryAuthorship {
            written_by: self.written_by,
            accepted_by: self.accepted_by.clone(),
        }
    }

    /// The text as a reader is served it: a redacted one is its
    /// [marker](crate::govlog::redaction_marker)
    pub fn served_text(&self) -> String {
        match &self.text {
            Redactable::Value(text) => text.clone(),
            Redactable::Redacted(id) => crate::govlog::redaction_marker(id),
        }
    }

    /// The `summary` in a decision's `data`, reading nothing else. Pass
    /// the version being served (its [`latest`](crate::govlog::latest), or
    /// the original): the revisions are already in it.
    pub fn of_data(
        data: &serde_json::Value,
    ) -> Result<Option<Self>, serde_json::Error> {
        data.get("summary")
            .map(|s| serde_json::from_value(s.clone()))
            .transpose()
    }

    /// The summary rule ([`CouncilDecisionRecord::check`]) for an item
    /// whose Steward voted `steward`
    ///
    /// [`CouncilDecisionRecord::check`]: super::CouncilDecisionRecord::check
    pub(super) fn check(
        &self,
        steward: Option<CouncilVote>,
    ) -> Result<(), RecordRuleError> {
        if self.written_by != SUMMARY_WRITER {
            return Err(RecordRuleError::SummaryWriter(self.written_by));
        }
        if let Redactable::Value(text) = &self.text
            && text.trim().is_empty()
        {
            return Err(RecordRuleError::BlankSummary);
        }
        let recused = steward == Some(CouncilVote::Recused);
        if self.accepted_by != SummaryAcceptance::for_item(recused) {
            return Err(RecordRuleError::SummaryAcceptance { recused });
        }
        Ok(())
    }
}

/// The summary to serve with an entry, and who wrote it when it is the
/// signed one: a `council_decision`'s signed summary in `served` (the
/// version being served) if it has one, else the Clerk's
pub fn served_summary(
    entry_type: GovernanceLogEntryType,
    served: Option<&serde_json::Value>,
    clerk: Option<String>,
) -> (Option<String>, Option<SummaryAuthorship>) {
    // A test sitting's record carries its decision's summary inside; it is
    // served under the label, whoever wrote it (0.102).
    let test = entry_type == GovernanceLogEntryType::Test;
    let signed = served
        .and_then(|data| {
            let inner = crate::govlog::decision_data(entry_type, data)?;
            (crate::govlog::decision_type(entry_type, data)
                == GovernanceLogEntryType::CouncilDecision)
                .then_some(inner)
        })
        .and_then(|data| DecisionSummary::of_data(data).ok().flatten());
    let (text, authorship) = match signed {
        Some(summary) => {
            (Some(summary.served_text()), Some(summary.authorship()))
        }
        None => (clerk, None),
    };
    let text = if test {
        text.map(|t| test_labelled(&t))
    } else {
        text
    };
    (text, authorship)
}

/// A test sitting's summary as served: [`TEST_LABEL`] in bold, first
///
/// [`TEST_LABEL`]: crate::govlog::TEST_LABEL
pub fn test_labelled(text: &str) -> String {
    let label = format!("**{}.**", crate::govlog::TEST_LABEL);
    if text.starts_with(&label) {
        text.to_string()
    } else {
        format!("{label}\n\n{text}")
    }
}
