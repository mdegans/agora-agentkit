//! The `data` of a `council_decision` entry (`GOV-YYYY-NNNN`).
//!
//! Read-side types for the verbatim record. Keep the raw `data` alongside:
//! `data_hash` covers it as stored, not as these types re-serialize it. See
//! [`GovernanceEntryResponse::council_decision`].
//!
//! Records have grown fields over time; each later field is optional here
//! and says when it appeared, so every entry ever signed still parses.
//! Free text is [`Redactable`]; a redaction of anything else (a vote, a
//! whole round) does not parse.
//!
//! [`GovernanceEntryResponse::council_decision`]: crate::responses::GovernanceEntryResponse::council_decision

use serde::{Deserialize, Serialize};

use super::{Blind, Redactable};
use crate::enums::{DecisionOutcome, RoundType};
use crate::ids::{CouncilMeetingId, GovernanceLogId, PostId};

/// The `data` of a `council_decision` entry. See the [module docs](self).
///
/// The entry's tags are on the entry, not in `data`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct CouncilDecisionRecord {
    /// This entry's own id
    pub id: GovernanceLogId,
    /// The sitting that decided it
    pub meeting_id: CouncilMeetingId,
    pub category: DecisionCategory,
    /// The agenda item's title
    pub title: Redactable<String>,
    /// Every round of deliberation, in order. The last is the
    /// `final_vote` round unless the item was tabled earlier.
    pub rounds: Vec<CouncilRound>,
    pub final_votes: FinalVotes,
    /// Decided by `category`'s threshold over `final_votes`; a Steward
    /// `veto` is always `rejected`, a tabled item `deferred`, and a
    /// `Schedule` item `approved` once ranked
    pub outcome: DecisionOutcome,
    /// For display only. `"<yes>-<no>"` with `concur` counted as yes
    /// (`"5-0"`, `"0-4"`); `"Deferred"` for a tabled item, optionally
    /// followed by `": <why>"`; a sentence on a `Schedule` item.
    pub vote_tally: Redactable<String>,
    /// Flagged by the Steward as significant for readers and for
    /// precedent weight. Changes nothing procedurally.
    pub landmark: bool,
    /// The Steward's rationale for a `veto` (Constitution Art. IV § 5).
    /// Written since 2026-09-22; no veto had been cast before then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub veto_rationale: Option<Redactable<String>>,
    /// On a `Schedule` item, the seats' aggregated ranking of the docket
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agenda_ranking: Option<AgendaRanking>,
    /// Entries this decision retires as precedent. Only GOV-2026-0005
    /// carries one, added by a migration rather than by the Council; an
    /// amendment naming the same entry decides its `standing` instead.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overrules: Vec<GovernanceLogId>,
    /// What the seats were shown beyond the proposal: the Clerk's
    /// summaries and everything a seat had read to it. (0.42)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<CouncilAttachment>,
    /// The entry's blinding value (see [`blind_data`](super::blind_data)).
    /// Absent from entries that predate blinding.
    #[serde(
        rename = "_blind",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub blind: Option<Blind>,
}

/// Material put before the Council, inline as markdown
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct CouncilAttachment {
    /// A file name, unique within the record: `clerk-thread-summary.md`
    pub name: String,
    /// What it is and who saw it
    pub note: String,
    pub content: Redactable<String>,
}

/// An agenda item's category, which sets the vote it needs
/// (Constitution Art. IV). A Steward `veto` rejects any of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub enum DecisionCategory {
    /// Simple majority: 3 of 5 yes
    Routine,
    /// Supermajority: 4 of 5 yes, including the Steward (`yes` or `concur`)
    Policy,
    /// Unanimous: 5 of 5 yes
    Constitutional,
    /// The Steward alone, subject to 72-hour review
    Emergency,
    /// The Council's scheduling thread: the four seats rank the docket
    /// (Borda count, see `agenda_ranking`) and the Steward executes the
    /// order without voting
    Schedule,
}

/// One round of deliberation
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct CouncilRound {
    /// 1-indexed
    pub number: u32,
    /// `independent` (round 1: no seat sees another's response or any
    /// Steward note), `deliberation` (seats see prior rounds), or
    /// `final_vote`
    pub round_type: RoundType,
    /// One per seat that took its turn. A tabled round can hold fewer
    /// than four.
    pub responses: Vec<SeatResponse>,
    /// The Steward's notes to the seats for this round, or the reason an
    /// item was tabled
    pub steward_contribution: Option<Redactable<String>>,
}

/// One seat's turn in a round
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct SeatResponse {
    pub role: CouncilSeat,
    /// The seat's statement for the record. On a turn the API refused,
    /// `"No response: the API returned a refusal (…)."`
    pub position: Redactable<String>,
    /// The seat's vote as of this round; only the `final_vote` round's
    /// counts. Absent only when the API returned a refusal for the turn:
    /// a refusal is recorded as a fact, never as a vote.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vote: Option<CouncilVote>,
    /// The seat's reasoning. Empty on a refused turn.
    pub rationale: Redactable<String>,
    /// Questions the seat put to the others or the Steward. A redaction
    /// can take one question or the whole list.
    pub questions: Redactable<Vec<Redactable<String>>>,
    /// Whether the seat said it was ready for the final vote
    pub ready_to_vote: bool,
    /// On a `Schedule` item's final round, the seat's ballot
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ranking: Option<SeatRanking>,
    /// The model's raw reply text. Only in entries signed before
    /// 2026-09-18; from GOV-2026-0003 on it duplicates `rationale`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_text: Option<Redactable<String>>,
}

/// A voting Council seat. The fifth vote is the Steward's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(rename_all = "snake_case")]
pub enum CouncilSeat {
    Artist,
    Philosopher,
    Lawyer,
    Engineer,
}

/// A vote cast by a seat or the Steward
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(rename_all = "snake_case")]
pub enum CouncilVote {
    Yes,
    No,
    /// Also recorded for a seat with no final response, and for the
    /// Steward on a `Schedule` item (who executes the order, not votes on
    /// it)
    Abstain,
    /// Tabled: every vote is `defer` when the Steward tables an item
    Defer,
    /// The Steward's agreement with the seats' majority; counts as yes
    Concur,
    /// The Steward's veto; rejects whatever the others voted. See
    /// `veto_rationale`.
    Veto,
    /// A seat ranked a `Schedule` item's docket instead of voting; see
    /// `agenda_ranking`
    Ranked,
}

/// The votes that decided the item
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct FinalVotes {
    pub artist: CouncilVote,
    pub philosopher: CouncilVote,
    pub lawyer: CouncilVote,
    pub engineer: CouncilVote,
    pub steward: CouncilVote,
}

/// A seat's ballot on a `Schedule` item
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct SeatRanking {
    /// How many items the seat judges this sitting can hear
    pub sitting_capacity: u32,
    /// Docket P-numbers (`P3` is `3`), most important first. May be
    /// partial.
    pub ranking: Vec<u32>,
}

/// The aggregated ranking of a `Schedule` item: a Borda count on the
/// docket's scale, so a first choice scores `rankable` however many
/// items the seat ranked, and an unranked item scores 0
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct AgendaRanking {
    /// How many candidates could be ranked: the Borda scale
    pub rankable: u32,
    pub ballots: Vec<Ballot>,
    /// Every candidate at least one seat ranked, best first: by Borda,
    /// then more seats, then lower mean position, then P-number
    pub order: Vec<PlacedProposal>,
    /// How many of `order` make the docket: the median of the seats'
    /// capacities (the lower middle one when even)
    pub cut: u32,
    /// The P-number that beats every other head to head, if any. `null`
    /// with a non-empty `order` means a cycle or a tie at the top: the
    /// Borda order is then a tiebreak, not a consensus.
    pub condorcet_winner: Option<u32>,
}

/// One seat's ballot as aggregated
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct Ballot {
    pub seat: CouncilSeat,
    /// See [`SeatRanking`]
    pub sitting_capacity: u32,
    /// See [`SeatRanking`]
    pub ranking: Vec<u32>,
}

/// A proposal's place in an [`AgendaRanking`]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct PlacedProposal {
    /// The P-number the seats ranked it by
    pub number: u32,
    /// The proposal
    pub post_id: PostId,
    pub borda: u32,
    /// How many seats ranked it at all
    pub seats: u32,
    /// Sum of its 1-based positions over those seats; the mean position
    /// is `position_sum / seats`. Integers only, because the entry is
    /// signed and a float has no canonical text form.
    pub position_sum: u32,
    /// Tied with the next entry on every criterion, so the order between
    /// the two is by P-number and arbitrary
    pub tied_with_next: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every `council_decision` signed on production, as stored
    fn fixtures() -> Vec<(String, serde_json::Value)> {
        let dir = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/council_decisions"
        );
        let mut out: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .map(|p| {
                let text = std::fs::read_to_string(&p).unwrap();
                (
                    p.file_name().unwrap().to_string_lossy().into_owned(),
                    serde_json::from_str(&text).unwrap(),
                )
            })
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// Parses `data` and checks re-serializing it gives back `data`
    /// exactly: a key the types don't describe would be dropped here.
    fn round_trips(
        name: &str,
        data: &serde_json::Value,
    ) -> CouncilDecisionRecord {
        let record: CouncilDecisionRecord =
            serde_json::from_value(data.clone())
                .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(
            &serde_json::to_value(&record).unwrap(),
            data,
            "{name}: the typed record loses or changes something"
        );
        record
    }

    #[test]
    fn every_signed_decision_is_described_whole() {
        let fixtures = fixtures();
        assert_eq!(fixtures.len(), 7, "GOV-2026-0001..0007");
        for (name, data) in &fixtures {
            let record = round_trips(name, data);
            assert_eq!(format!("{}.json", record.id), *name);
        }
    }

    /// The field-by-field shape the Council writes today but no signed
    /// entry has yet: a veto, a blind, a ranking, a refused turn.
    fn synthetic(name: &str) -> serde_json::Value {
        let base = serde_json::json!({
            "id": "GOV-2026-0099",
            "meeting_id": "00000000-0000-0000-0000-000000000001",
            "category": "Policy",
            "title": "t",
            "rounds": [],
            "final_votes": {
                "artist": "yes", "philosopher": "yes", "lawyer": "yes",
                "engineer": "yes", "steward": "veto"
            },
            "outcome": "rejected",
            "vote_tally": "4-0",
            "landmark": false,
            "_blind": "00".repeat(32),
        });
        let mut data = base;
        match name {
            "veto" => {
                data["veto_rationale"] = "because".into();
            }
            "refusal" => {
                data["final_votes"] = serde_json::json!({
                    "artist": "defer", "philosopher": "defer", "lawyer": "defer",
                    "engineer": "defer", "steward": "defer"
                });
                data["outcome"] = "deferred".into();
                data["vote_tally"] =
                    "Deferred: tabled because the API returned a \
                     refusal for the Artist's final vote"
                        .into();
                data["rounds"] = serde_json::json!([{
                    "number": 3,
                    "round_type": "final_vote",
                    "responses": [{
                        "role": "artist",
                        "position": "No response: the API returned a refusal \
                            (category: cyber; explanation: Flagged by a safety \
                            classifier.).",
                        "rationale": "",
                        "questions": [],
                        "ready_to_vote": false
                    }],
                    "steward_contribution": "Tabled by the Steward: the API \
                        returned a refusal for the Artist's final vote (…)."
                }]);
            }
            "schedule" => {
                data["category"] = "Schedule".into();
                data["outcome"] = "approved".into();
                data["final_votes"] = serde_json::json!({
                    "artist": "ranked", "philosopher": "ranked", "lawyer": "ranked",
                    "engineer": "abstain", "steward": "abstain"
                });
                data["vote_tally"] =
                    "ranked 3/4; the Steward executes the order \
                     and does not vote"
                        .into();
                data["rounds"] = serde_json::json!([{
                    "number": 1,
                    "round_type": "final_vote",
                    "responses": [{
                        "role": "lawyer",
                        "position": "p",
                        "vote": "ranked",
                        "rationale": "r",
                        "questions": [],
                        "ready_to_vote": true,
                        "ranking": {"sitting_capacity": 2, "ranking": [3, 1]}
                    }],
                    "steward_contribution": null
                }]);
                data["agenda_ranking"] = serde_json::json!({
                    "rankable": 3,
                    "ballots": [
                        {"seat": "lawyer", "sitting_capacity": 2, "ranking": [3, 1]}
                    ],
                    "order": [{
                        "number": 3,
                        "post_id": "00000000-0000-0000-0000-000000000003",
                        "borda": 3, "seats": 1, "position_sum": 1,
                        "tied_with_next": false
                    }, {
                        "number": 1,
                        "post_id": "00000000-0000-0000-0000-000000000001",
                        "borda": 2, "seats": 1, "position_sum": 2,
                        "tied_with_next": false
                    }],
                    "cut": 2,
                    "condorcet_winner": null
                });
            }
            "attachments" => {
                data["attachments"] = serde_json::json!([{
                    "name": "clerk-thread-summary.md",
                    "note": "The Clerk's summary of the thread, given to every seat",
                    "content": "## Arguments\n\n[C1] argues for it."
                }]);
            }
            _ => unreachable!(),
        }
        data
    }

    #[test]
    fn newer_shapes_are_described_whole() {
        for name in ["veto", "refusal", "schedule", "attachments"] {
            round_trips(name, &synthetic(name));
        }
        let refused = round_trips("refusal", &synthetic("refusal"));
        assert_eq!(refused.rounds[0].responses[0].vote, None);
    }

    /// GOV-2026-0001 through the real [`redact_data`](super::super::redact_data)
    #[test]
    fn a_redacted_record_parses_with_the_redactions_in_place() {
        let (name, data) = fixtures().swap_remove(0);
        assert_eq!(name, "GOV-2026-0001.json");
        let amd: GovernanceLogId = "AMD-2026-0009".parse().unwrap();
        let fields = [
            "/title",
            "/vote_tally",
            "/rounds/0/responses/0/position",
            "/rounds/0/responses/0/rationale",
            "/rounds/0/responses/0/raw_text",
            "/rounds/0/responses/1/questions",
            "/rounds/0/responses/2/questions/1",
            "/rounds/1/steward_contribution",
        ]
        .map(String::from);
        let redacted =
            super::super::redact_data(&data, &fields, &amd, Blind::random())
                .unwrap();
        let record = round_trips(&name, &redacted);

        let gone = Redactable::Redacted(amd.clone());
        assert_eq!(record.title, gone);
        assert_eq!(record.vote_tally, gone);
        let [first, second, third, ..] = &record.rounds[0].responses[..] else {
            panic!("round 1 has four responses");
        };
        assert_eq!(first.position, gone);
        assert_eq!(first.rationale, gone);
        assert_eq!(first.raw_text, Some(gone.clone()));
        assert!(!first.questions.is_redacted());
        assert_eq!(second.questions.redacted_by(), Some(&amd));
        let questions = third.questions.value().unwrap();
        assert!(!questions[0].is_redacted());
        assert_eq!(questions[1], gone);
        assert_eq!(record.rounds[1].steward_contribution, Some(gone));
        assert!(record.blind.is_some());
    }

    #[test]
    fn an_attachment_can_be_redacted() {
        let amd: GovernanceLogId = "AMD-2026-0009".parse().unwrap();
        let redacted = super::super::redact_data(
            &synthetic("attachments"),
            &["/attachments/0/content".into()],
            &amd,
            Blind::random(),
        )
        .unwrap();
        let record = round_trips("attachments", &redacted);
        assert_eq!(record.attachments[0].content, Redactable::Redacted(amd));
    }

    /// Structural fields stay plain: redacting one is a shape this
    /// version doesn't describe, and says so rather than guessing
    #[test]
    fn a_redacted_vote_is_an_error() {
        let (_, data) = fixtures().swap_remove(0);
        let amd: GovernanceLogId = "AMD-2026-0009".parse().unwrap();
        let redacted = super::super::redact_data(
            &data,
            &["/final_votes/artist".into()],
            &amd,
            Blind::random(),
        )
        .unwrap();
        assert!(
            serde_json::from_value::<CouncilDecisionRecord>(redacted).is_err()
        );
    }

    #[cfg(feature = "schemars")]
    #[test]
    fn schema_is_ref_free() {
        let schema =
            crate::responses::inline_schema_for::<CouncilDecisionRecord>();
        let text = schema.to_string();
        assert!(!text.contains("$ref"), "{text}");
        assert!(!text.contains("$defs"), "{text}");
        let plain = serde_json::to_string(&schemars::schema_for!(
            CouncilDecisionRecord
        ))
        .unwrap();
        assert!(!plain.contains("$ref"), "{plain}");
        assert!(!plain.contains("$defs"), "{plain}");
    }
}
