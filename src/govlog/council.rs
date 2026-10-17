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

use misanthropic::Model;
use misanthropic::prompt::message::{Content, Thought};
use serde::{Deserialize, Serialize};

use super::{Blind, Redactable};

mod decided;
mod ranking;
mod summary;
mod votes;
use crate::enums::{DecisionOutcome, ProposalCategory, RoundType};
use crate::ids::{CouncilMeetingId, GovernanceLogId, PostId};
pub use decided::{Decided, FinalRoundError, RecordRuleError};
pub use summary::{
    DecisionSummary, RECUSED_ACCEPTORS, SUMMARY_POINTER, SUMMARY_WRITER,
    SummaryAcceptance, SummaryAuthorship, served_summary,
};
pub use votes::{
    BlankStatement, DEFERRING_SEATS, NotASeatChoice, SeatChoice, SeatVote,
    SeatVotes, Statement, StewardVote, Vote, Votes,
};

/// The `data` of a `council_decision` entry. See the [module docs](self).
///
/// The entry's tags are on the entry, not in `data`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// Whether the item limits the Steward's powers, as the four seats
    /// determined it before round 1 (Constitution Art. IV § 3,
    /// GOV-2026-0009). Absent on items decided before it was recorded, and
    /// on `Schedule` and `Emergency` items. (0.48)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steward_recusal: Option<StewardRecusal>,
    /// The Steward's one-sentence rationale for an abstention (Art. IV
    /// § 3). A seat's abstention rationale is its statement in the final
    /// round, recorded there once.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub abstentions: Vec<Abstention>,
    /// Entries this decision retires as precedent. Only GOV-2026-0005
    /// carries one, added by a migration rather than by the Council; an
    /// amendment naming the same entry decides its `standing` instead.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overrules: Vec<GovernanceLogId>,
    /// What the seats were shown beyond the proposal: the Clerk's
    /// summaries and everything a seat had read to it. (0.42)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<CouncilAttachment>,
    /// The model each seat ran on. Absent from entries before 0.64.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seat_models: Option<SeatModels>,
    /// The decision's summary, written by the Lawyer and accepted before
    /// signing. Absent from entries before 0.96, whose summaries are the
    /// Clerk's and unsigned. A [revision](super::Revision) at
    /// [`SUMMARY_POINTER`] changes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<DecisionSummary>,
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
/// (Constitution v0.4 Art. IV § 3).
///
/// An `abstain` or `recused` leaves the denominator. A Steward `veto`
/// rejects any item the Steward is not recused from; a recused Steward has
/// no vote and no veto.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub enum DecisionCategory {
    /// Three `yes`: an absolute count, found among the four seats when the
    /// Steward is recused
    Routine,
    /// Four `yes` of five including the Steward (`yes` or `concur`); with
    /// the Steward recused, all four seats `yes`
    Policy,
    /// Unanimous among those voting, with no fewer than four voting.
    /// Before GOV-2026-0009 (2026-09-26), 5 of 5 `yes`
    Constitutional,
    /// The Steward alone, subject to 72-hour review
    Emergency,
    /// The Council's scheduling thread: the four seats rank the docket
    /// (Borda count, see `agenda_ranking`) and the Steward executes the
    /// order without voting
    Schedule,
}

/// The two spell the same five categories (0.74)
impl From<ProposalCategory> for DecisionCategory {
    fn from(category: ProposalCategory) -> Self {
        match category {
            ProposalCategory::Routine => Self::Routine,
            ProposalCategory::Policy => Self::Policy,
            ProposalCategory::Constitutional => Self::Constitutional,
            ProposalCategory::Emergency => Self::Emergency,
            ProposalCategory::Schedule => Self::Schedule,
        }
    }
}

impl DecisionCategory {
    /// The vote it needs, in words ([`ProposalCategory::threshold`]) (0.76)
    pub const fn threshold(self) -> &'static str {
        ProposalCategory::from_decision(self).threshold()
    }

    /// Whether an item of this category opens with the seats' recusal
    /// determination ([`ProposalCategory::determines_recusal`]) (0.76)
    pub const fn determines_recusal(self) -> bool {
        ProposalCategory::from_decision(self).determines_recusal()
    }

    /// The vote it needs once the Steward is recused, if recusal is ever
    /// determined ([`ProposalCategory::recused_threshold`]) (0.76)
    pub const fn recused_threshold(self) -> Option<&'static str> {
        ProposalCategory::from_decision(self).recused_threshold()
    }

    /// The category's name as the record and prose write it: `"Policy"`
    /// (0.76)
    pub const fn name(self) -> &'static str {
        match self {
            Self::Routine => "Routine",
            Self::Policy => "Policy",
            Self::Constitutional => "Constitutional",
            Self::Emergency => "Emergency",
            Self::Schedule => "Schedule",
        }
    }
}

/// `"Policy"`: [`DecisionCategory::name`], the record's spelling (0.76)
impl std::fmt::Display for DecisionCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

impl ProposalCategory {
    /// [`From<DecisionCategory>`], in a `const fn`
    const fn from_decision(category: DecisionCategory) -> Self {
        match category {
            DecisionCategory::Routine => Self::Routine,
            DecisionCategory::Policy => Self::Policy,
            DecisionCategory::Constitutional => Self::Constitutional,
            DecisionCategory::Emergency => Self::Emergency,
            DecisionCategory::Schedule => Self::Schedule,
        }
    }
}

impl From<DecisionCategory> for ProposalCategory {
    fn from(category: DecisionCategory) -> Self {
        Self::from_decision(category)
    }
}

/// The tag a recused deferral carries on the record, which the next
/// hearing's guard looks for (0.74)
pub const RECUSED_DEFERRED_TAG: &str = "recused-deferred";

/// The tag every decision with the Steward recused carries (0.74)
pub const STEWARD_RECUSED_TAG: &str = "steward-recused";

/// One round of deliberation
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// Present when `steward_contribution` came from a recused Steward
    /// under emergency authority (Art. IV § 2), which puts the item under
    /// mandatory Council review within 72 hours. (0.48)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steward_emergency: Option<StewardEmergency>,
    /// The item was tabled here by a recused Steward, over an API refusal
    /// on a seat's final vote: refusal handling, the one tabling open to
    /// a recused Steward. `steward_contribution` says which refusal.
    /// (0.48)
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub tabled_by_recused_steward: bool,
}

/// A recused Steward's note to the seats, made as an emergency act
/// (Constitution Art. IV § 2)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct StewardEmergency {
    /// The Steward's one-sentence reason, given before the note was read
    pub reason: Redactable<String>,
}

/// The four seats' determination of whether an item limits the Steward's
/// powers (Constitution Art. IV § 3, GOV-2026-0009)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct StewardRecusal {
    /// One per seat that answered in round 1; a seat whose turn the API
    /// refused has none
    pub votes: Vec<RecusalVote>,
    /// Three of the four seats said it does. The Steward then has no
    /// deliberation, vote or veto on the item, and `final_votes` records
    /// `recused`.
    pub recused: bool,
}

/// One seat's answer to whether an item limits the Steward's powers
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct RecusalVote {
    pub seat: CouncilSeat,
    /// The seat's explanation for the record; before 0.64, its reasoning,
    /// written before its answer
    pub reason: Redactable<String>,
    /// The seat's thinking before it answered (0.64)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<Redactable<Rationale>>,
    pub limits_steward_powers: bool,
}

/// An abstention and its one-sentence rationale (Art. IV § 3)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct Abstention {
    pub member: CouncilMember,
    pub rationale: Redactable<String>,
}

/// Any of the five Council members: a [`CouncilSeat`] or the Steward
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(rename_all = "snake_case")]
pub enum CouncilMember {
    Artist,
    Philosopher,
    Lawyer,
    Engineer,
    Steward,
}

impl From<CouncilSeat> for CouncilMember {
    fn from(seat: CouncilSeat) -> Self {
        match seat {
            CouncilSeat::Artist => Self::Artist,
            CouncilSeat::Philosopher => Self::Philosopher,
            CouncilSeat::Lawyer => Self::Lawyer,
            CouncilSeat::Engineer => Self::Engineer,
        }
    }
}

/// One seat's turn in a round
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// Why the seat said it. Empty on a refused turn.
    pub rationale: Redactable<Rationale>,
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

/// Why a seat said what it said.
///
/// Until 0.64 the seats wrote their reasoning into a `reasoning` field of
/// the decision tool, which is not something a model should be asked to
/// do: Anthropic's `reasoning_extraction` safeguard declines exactly that
/// ([docs]). The seats now think before they answer, and the record keeps
/// what the API returns for that thinking.
///
/// [docs]: https://platform.claude.com/docs/en/build-with-claude/refusals-and-fallback#keep-reasoning-in-thinking-blocks
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(untagged)]
pub enum Rationale {
    /// What the seat wrote in the decision tool's `reasoning` field
    Legacy(String),
    /// The thinking blocks of the seat's turn, in order and as the API
    /// returned them. On Anthropic models each is a summary of the model's
    /// reasoning, written by Anthropic, with a `signature` over the full
    /// reasoning; the [`SeatModels`] entry says which model to replay it
    /// against. Local models' blocks are their own reasoning, unsigned.
    Thinking(Vec<Thought>),
}

impl Rationale {
    /// The text of every thinking block, in order. A legacy rationale is
    /// one item.
    pub fn texts(&self) -> Vec<&str> {
        match self {
            Self::Legacy(text) => vec![text.as_str()],
            Self::Thinking(thoughts) => {
                thoughts.iter().filter_map(Thought::text).collect()
            }
        }
    }

    /// Thinking the API withheld: `redacted_thinking` blocks
    pub fn redacted_blocks(&self) -> usize {
        match self {
            Self::Legacy(_) => 0,
            Self::Thinking(thoughts) => {
                thoughts.iter().filter(|t| t.text().is_none()).count()
            }
        }
    }

    /// No words: an empty legacy string, or no thinking at all
    pub fn is_empty(&self) -> bool {
        self.texts().iter().all(|t| t.trim().is_empty())
            && self.redacted_blocks() == 0
    }
}

/// The text of each block, a blank line apart; withheld blocks say so
impl std::fmt::Display for Rationale {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self::Thinking(thoughts) = self else {
            return f.write_str(&self.texts().concat());
        };
        for (i, thought) in thoughts.iter().enumerate() {
            if i > 0 {
                f.write_str("\n\n")?;
            }
            f.write_str(
                thought.text().unwrap_or("[reasoning withheld]").trim_end(),
            )?;
        }
        Ok(())
    }
}

impl From<String> for Rationale {
    fn from(text: String) -> Self {
        Self::Legacy(text)
    }
}

impl From<&str> for Rationale {
    fn from(text: &str) -> Self {
        Self::Legacy(text.to_owned())
    }
}

impl From<Vec<Thought>> for Rationale {
    fn from(thoughts: Vec<Thought>) -> Self {
        Self::Thinking(thoughts)
    }
}

/// The thinking blocks of `content`; anything else in it is not thinking
impl From<&Content> for Rationale {
    fn from(content: &Content) -> Self {
        Self::Thinking(content.thoughts().collect())
    }
}

/// The model each seat ran on
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct SeatModels {
    pub artist: Model,
    pub philosopher: Model,
    pub lawyer: Model,
    pub engineer: Model,
}

impl SeatModels {
    pub fn get(&self, seat: CouncilSeat) -> &Model {
        match seat {
            CouncilSeat::Artist => &self.artist,
            CouncilSeat::Philosopher => &self.philosopher,
            CouncilSeat::Lawyer => &self.lawyer,
            CouncilSeat::Engineer => &self.engineer,
        }
    }
}

/// A voting Council seat. The fifth vote is the Steward's.
// Ordered as declared (0.74). A plain comment: the doc comment is schema
// text, which agora pins byte for byte.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(rename_all = "snake_case")]
pub enum CouncilSeat {
    Artist,
    Philosopher,
    Lawyer,
    Engineer,
}

impl CouncilSeat {
    /// Every seat, in declaration order (0.74)
    pub const ALL: [Self; 4] = [
        Self::Artist,
        Self::Philosopher,
        Self::Lawyer,
        Self::Engineer,
    ];

    /// The `snake_case` form, as serde spells it: `"artist"` (0.74)
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Artist => "artist",
            Self::Philosopher => "philosopher",
            Self::Lawyer => "lawyer",
            Self::Engineer => "engineer",
        }
    }

    /// The seat's name as prose writes it: `"Artist"` (0.74). Also its
    /// `Display` (0.76).
    pub const fn name(self) -> &'static str {
        match self {
            Self::Artist => "Artist",
            Self::Philosopher => "Philosopher",
            Self::Lawyer => "Lawyer",
            Self::Engineer => "Engineer",
        }
    }
}

/// `"Artist"`: [`CouncilSeat::name`] (0.76)
impl std::fmt::Display for CouncilSeat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// A seat is the model role that speaks for it (0.76)
impl From<CouncilSeat> for crate::enums::ModelRole {
    fn from(seat: CouncilSeat) -> Self {
        match seat {
            CouncilSeat::Artist => Self::Artist,
            CouncilSeat::Philosopher => Self::Philosopher,
            CouncilSeat::Lawyer => Self::Lawyer,
            CouncilSeat::Engineer => Self::Engineer,
        }
    }
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
    /// The Steward, on an item the seats found limits the Steward's
    /// powers: no vote and no veto. See `steward_recusal`. (0.48)
    Recused,
}

impl CouncilVote {
    /// The vote as prose writes it: `"Yes"`, `"Abstain"` (0.76). Also its
    /// `Display`, and how the Council stores a seat's vote in its
    /// `deliberation_rounds` rows.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Yes => "Yes",
            Self::No => "No",
            Self::Abstain => "Abstain",
            Self::Defer => "Defer",
            Self::Concur => "Concur",
            Self::Veto => "Veto",
            Self::Ranked => "Ranked",
            Self::Recused => "Recused",
        }
    }
}

/// `"Yes"`: [`CouncilVote::name`] (0.76)
impl std::fmt::Display for CouncilVote {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
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
    use crate::enums::AmendmentKind;

    /// Every `council_decision` signed on production, as signed
    fn fixtures() -> Vec<(String, serde_json::Value)> {
        fixtures_in(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/council_decisions"
        ))
    }

    /// The latest version of each one a revision amended
    fn revised_fixtures() -> Vec<(String, serde_json::Value)> {
        fixtures_in(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/council_decisions/revised"
        ))
    }

    /// Each signed entry's `data_hash`, and the `resulting_data_hash` of
    /// its last revision when one revised it, as the production log has
    /// them (2026-10-13): the fixtures are the signed bytes.
    const SIGNED: [(&str, &str, Option<&str>); 13] = [
        (
            "GOV-2026-0001",
            "fca67b28737d613e3a98cdb3b1ad686944b36f1654ad5c237da6ec0291ee4574",
            None,
        ),
        (
            "GOV-2026-0002",
            "b1bb43d611d75e1d3b03df8ff64381781b3d75d934a1f9d7ec75fe2174716c47",
            None,
        ),
        (
            "GOV-2026-0003",
            "7c75af86d9b3930b5d14cd9e1784c2ecb32483427ba7149dffb20d84510c1b09",
            Some(
                "e1db85357dbe3b865129a53836a16c33730c7ebb37a8412f84afe5fbac4aa64d",
            ),
        ),
        (
            "GOV-2026-0004",
            "0004e84733236dca2ab4dfb16757236ad0d76e27efbf47bbec0e00c6d90a7ed0",
            Some(
                "b2a9ec584f49eebf0103a72986440a326fcb5f68c83a7f1eddba8228046c342f",
            ),
        ),
        (
            "GOV-2026-0005",
            "8257faade7548f15c010f2a5bb7ff22e1e37f7034eac2c9726445002fa593f0e",
            Some(
                "514a020a95669da78dfadcee58729a99402f9b2c2f750a557a8b8ce8512f451d",
            ),
        ),
        (
            "GOV-2026-0006",
            "6520bb4cba556c4ba251584d836d1efb8a33227b83573f74c34c32362762a6f7",
            Some(
                "58f3fafc031ca29c796ac08fa69bb9994bf40fef9114189d6f820d050c66eedc",
            ),
        ),
        (
            "GOV-2026-0007",
            "39a273c711d3f5b4744bbfcb1a1b2978e26b5b48629fabd5d0f136241167f07c",
            Some(
                "56c1036a8ee1eb852d831082f36ef9c22bc2a103a9a0d56c778fa040f9c6917e",
            ),
        ),
        (
            "GOV-2026-0008",
            "89ac0d32809a3baf07a28ab6a511c64aaf1a31741cb3c5630d23fde484cf5cf3",
            None,
        ),
        (
            "GOV-2026-0009",
            "32a86a94646ed886085895ad92e76285acced67b76c1853bc0e8b04626b6487d",
            None,
        ),
        (
            "GOV-2026-0010",
            "aa4031caaf9e2851f70ed83008954869388cacb03a8119a2996a281da9e4a0dd",
            None,
        ),
        (
            "GOV-2026-0011",
            "590b29357ae1acd425f64abb8e2aeeee2eec71d36dbb9afc99ec33ce4e5791d7",
            None,
        ),
        (
            "GOV-2026-0012",
            "625d7f8b7c504def1fa332db68b06518d34b247edd5f2225be370a392e1a0973",
            None,
        ),
        (
            "GOV-2026-0013",
            "c3d0a7f2c157e15f73031621148c1f3ff543418e31684d12fc6048a6c657dcb7",
            None,
        ),
    ];

    fn fixtures_in(dir: &str) -> Vec<(String, serde_json::Value)> {
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
        assert_eq!(fixtures.len(), 13, "GOV-2026-0001..0013");
        let revised = revised_fixtures();
        assert_eq!(revised.len(), 5, "GOV-2026-0003..0007, revised");
        for (name, data) in fixtures.iter().chain(&revised) {
            let record = round_trips(name, data);
            assert_eq!(format!("{}.json", record.id), *name);
        }
    }

    /// Every signed outcome on a voted item is what the thresholds give
    /// for its votes, as signed and as revised. Tabled items (every vote
    /// `defer`, GOV-2026-0002) and `Schedule` items (0008, 0013) are
    /// decided otherwise; 0001..0008 predate GOV-2026-0009's thresholds,
    /// and agree with them anyway.
    #[test]
    fn every_signed_outcome_follows_from_its_votes() {
        let check = |fixtures: Vec<(String, serde_json::Value)>| {
            let mut checked = 0;
            for (name, data) in fixtures {
                let record: CouncilDecisionRecord =
                    serde_json::from_value(data).unwrap();
                let tabled = record.final_votes.steward == CouncilVote::Defer;
                if tabled || record.category == DecisionCategory::Schedule {
                    continue;
                }
                assert_eq!(
                    record.final_votes.outcome(record.category),
                    record.outcome,
                    "{name}"
                );
                assert_eq!(
                    record.vote_tally.value().map(String::as_str),
                    Some(record.final_votes.tally().as_str()),
                    "{name}"
                );
                checked += 1;
            }
            checked
        };
        assert_eq!(
            check(fixtures()),
            10,
            "GOV-2026-0001, 0003..0007, 0009..0012"
        );
        assert_eq!(
            check(revised_fixtures()),
            5,
            "GOV-2026-0003..0007, revised"
        );
    }

    /// Aggregating a signed scheduling item's ballots again gives its
    /// signed ranking: the order, the ties, the cut and the Condorcet
    /// winner
    #[test]
    fn every_signed_ranking_follows_from_its_ballots() {
        let mut checked = 0;
        for (name, data) in fixtures().into_iter().chain(revised_fixtures()) {
            let record: CouncilDecisionRecord =
                serde_json::from_value(data).unwrap();
            let Some(signed) = record.agenda_ranking else {
                continue;
            };
            // The P-numbers run 1..=rankable; a candidate no seat ranked is
            // not in `order`, and its proposal does not matter here
            let candidates: std::collections::BTreeMap<u32, PostId> = (1
                ..=signed.rankable)
                .map(|n| {
                    let post = signed
                        .order
                        .iter()
                        .find(|p| p.number == n)
                        .map(|p| p.post_id)
                        .unwrap_or_else(|| {
                            PostId::from(uuid::Uuid::from_u128(n.into()))
                        });
                    (n, post)
                })
                .collect();
            let again =
                AgendaRanking::aggregate(&candidates, signed.ballots.clone());
            assert_eq!(again, signed, "{name}");
            checked += 1;
        }
        assert_eq!(checked, 2, "GOV-2026-0008 and 0013");
    }

    /// What [`Decided`] writes is what the signed records say, for the
    /// kinds the archive has: tabled (GOV-2026-0002) and ranked
    /// (GOV-2026-0008, 0013). No voted record can be read back into
    /// [`Votes`]: before GOV-2026-0013 the seats' words were in
    /// `rationale` and `position` was empty, which is why there is no
    /// conversion back.
    #[test]
    fn decided_writes_what_the_archive_says() {
        let mut checked = 0;
        for (name, data) in fixtures() {
            let record: CouncilDecisionRecord =
                serde_json::from_value(data).unwrap();
            let decided =
                match (&record.agenda_ranking, record.final_votes.steward) {
                    (Some(ranking), _) => Decided::Ranked(ranking.clone()),
                    (None, CouncilVote::Defer) => Decided::Tabled,
                    (None, _) => {
                        let last = record.rounds.last().unwrap();
                        let votes = SeatVotes::from_final_round(last);
                        assert!(
                            matches!(
                                votes,
                                Err(FinalRoundError::NoStatement(_))
                            ),
                            "{name}: {votes:?}"
                        );
                        continue;
                    }
                };
            let again = record.clone().decided(decided);
            assert_eq!(again, record, "{name}");
            checked += 1;
        }
        assert_eq!(checked, 3, "GOV-2026-0002, 0008, 0013");
    }

    /// The prose names: a category's is its serde form; a seat's and a
    /// vote's are their serde forms capitalized (0.76)
    #[test]
    fn the_prose_names() {
        use DecisionCategory as C;
        for c in [
            C::Routine,
            C::Policy,
            C::Constitutional,
            C::Emergency,
            C::Schedule,
        ] {
            assert_eq!(
                serde_json::to_value(c).unwrap(),
                c.to_string().as_str()
            );
            assert_eq!(c.threshold(), ProposalCategory::from(c).threshold());
        }
        let capitalized = |s: &str| {
            let mut c = s.chars();
            c.next()
                .unwrap()
                .to_uppercase()
                .chain(c)
                .collect::<String>()
        };
        for seat in CouncilSeat::ALL {
            assert_eq!(seat.to_string(), capitalized(seat.as_str()));
        }
        use CouncilVote as V;
        for v in [
            V::Yes,
            V::No,
            V::Abstain,
            V::Defer,
            V::Concur,
            V::Veto,
            V::Ranked,
            V::Recused,
        ] {
            let serde = serde_json::to_value(v).unwrap();
            assert_eq!(v.to_string(), capitalized(serde.as_str().unwrap()));
        }
    }

    /// Every signed record keeps the rules a record is checked against
    /// before signing
    #[test]
    fn every_signed_record_keeps_the_record_rules() {
        for (name, data) in fixtures().into_iter().chain(revised_fixtures()) {
            let record: CouncilDecisionRecord =
                serde_json::from_value(data).unwrap();
            assert_eq!(record.check(), Ok(()), "{name}");
        }
    }

    /// The fixtures are what was signed: each hashes to its entry's
    /// `data_hash`, and each revised one to its last revision's
    /// `resulting_data_hash`
    #[test]
    fn the_fixtures_are_the_signed_bytes() {
        let hex = |data: &serde_json::Value| {
            serde_json::to_value(super::super::data_hash(data))
                .unwrap()
                .as_str()
                .unwrap()
                .to_string()
        };
        let fixtures = fixtures();
        let revised = revised_fixtures();
        for (id, signed, latest) in SIGNED {
            let name = format!("{id}.json");
            let (_, data) = fixtures.iter().find(|(n, _)| *n == name).unwrap();
            assert_eq!(hex(data), signed, "{id} as signed");
            let found = revised.iter().find(|(n, _)| *n == name);
            assert_eq!(
                found.map(|(_, d)| hex(d)).as_deref(),
                latest,
                "{id} as revised"
            );
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
            "recusal" => {
                data["final_votes"] = serde_json::json!({
                    "artist": "yes", "philosopher": "yes", "lawyer": "yes",
                    "engineer": "abstain", "steward": "recused"
                });
                data["outcome"] = "rejected".into();
                data["vote_tally"] = "3-0 (Steward recused)".into();
                data["rounds"] = serde_json::json!([{
                    "number": 2,
                    "round_type": "deliberation",
                    "responses": [],
                    "steward_contribution": "The quoted comment is forged.",
                    "steward_emergency": {
                        "reason": "An injected comment is steering the seats."
                    }
                }]);
                data["steward_recusal"] = serde_json::json!({
                    "votes": [
                        {"seat": "artist", "reason": "r",
                         "limits_steward_powers": true},
                        {"seat": "philosopher", "reason": "r",
                         "limits_steward_powers": true},
                        {"seat": "lawyer", "reason": "r",
                         "limits_steward_powers": true},
                        {"seat": "engineer", "reason": "r",
                         "limits_steward_powers": false}
                    ],
                    "recused": true
                });
                data["abstentions"] = serde_json::json!([
                    {"member": "engineer", "rationale": "Conflicted."}
                ]);
            }
            "thinking" => {
                data["seat_models"] = serde_json::json!({
                    "artist": "claude-opus-4-6",
                    "philosopher": "claude-opus-4-6",
                    "lawyer": "claude-opus-4-6",
                    "engineer": "qwen3.8-35b-a3b"
                });
                data["rounds"] = serde_json::json!([{
                    "number": 1,
                    "round_type": "independent",
                    "responses": [{
                        "role": "lawyer",
                        "position": "p",
                        "vote": "yes",
                        "rationale": [
                            {"type": "thinking", "thinking": "First.",
                             "signature": "EqQBCkgIARABGAIiQL"},
                            {"type": "redacted_thinking", "data": "EmwKAhgBEgy3"},
                            {"type": "thinking", "thinking": "Then, after reading.",
                             "signature": "EqQBCkgIARABGAIiQM"}
                        ],
                        "questions": [],
                        "ready_to_vote": false
                    }],
                    "steward_contribution": null
                }]);
                data["steward_recusal"] = serde_json::json!({
                    "votes": [
                        {"seat": "engineer", "reason": "Art. IV § 2 is untouched.",
                         "rationale": [{"type": "thinking",
                            "thinking": "Unsigned, from a local model.",
                            "signature": ""}],
                         "limits_steward_powers": false}
                    ],
                    "recused": false
                });
            }
            "summary" => {
                data["final_votes"]["steward"] = "yes".into();
                data["outcome"] = "approved".into();
                data["vote_tally"] = "5-0".into();
                data["summary"] = serde_json::json!({
                    "text": "## Decision\n\nApproved 5-0.",
                    "written_by": "lawyer",
                    "accepted_by": {"by": "steward"}
                });
            }
            "recused_summary" => {
                data = synthetic("recusal");
                data["summary"] = serde_json::json!({
                    "text": "Rejected with the Steward recused.",
                    "written_by": "lawyer",
                    "accepted_by": {
                        "by": "seats",
                        "seats": ["artist", "philosopher", "engineer"]
                    }
                });
            }
            "recused_refusal" => {
                data = synthetic("refusal");
                data["final_votes"]["steward"] = "recused".into();
                data["rounds"][0]["tabled_by_recused_steward"] = true.into();
            }
            _ => unreachable!(),
        }
        data
    }

    #[test]
    fn newer_shapes_are_described_whole() {
        for name in [
            "veto",
            "refusal",
            "schedule",
            "attachments",
            "recusal",
            "recused_refusal",
            "thinking",
            "summary",
            "recused_summary",
        ] {
            round_trips(name, &synthetic(name));
        }
        let tabled =
            round_trips("recused_refusal", &synthetic("recused_refusal"));
        assert!(tabled.rounds[0].tabled_by_recused_steward);
        assert!(
            !round_trips("refusal", &synthetic("refusal")).rounds[0]
                .tabled_by_recused_steward
        );
        let refused = round_trips("refusal", &synthetic("refusal"));
        assert_eq!(refused.rounds[0].responses[0].vote, None);
    }

    /// A seat's rationale is its turn's thinking blocks from 0.64, each
    /// kept as the API returned it so a signed one can be replayed
    #[test]
    fn a_thinking_rationale_reads_back() {
        let record = round_trips("thinking", &synthetic("thinking"));
        let models = record.seat_models.as_ref().unwrap();
        assert_eq!(models.get(CouncilSeat::Lawyer).name(), "claude-opus-4-6");
        assert_eq!(models.get(CouncilSeat::Engineer).name(), "qwen3.8-35b-a3b");

        let response = &record.rounds[0].responses[0];
        let rationale = response.rationale.value().unwrap();
        assert!(matches!(rationale, Rationale::Thinking(_)));
        assert_eq!(rationale.texts(), ["First.", "Then, after reading."]);
        assert_eq!(rationale.redacted_blocks(), 1);

        let vote = &record.steward_recusal.unwrap().votes[0];
        let thought = vote.rationale.as_ref().unwrap().value().unwrap();
        assert_eq!(thought.texts(), ["Unsigned, from a local model."]);

        assert_eq!(
            rationale.to_string(),
            "First.\n\n[reasoning withheld]\n\nThen, after reading."
        );
        assert!(!rationale.is_empty());
        assert!(Rationale::Legacy(" ".into()).is_empty());

        let legacy = round_trips("schedule", &synthetic("schedule"));
        let legacy = legacy.rounds[0].responses[0].rationale.value().unwrap();
        assert_eq!(legacy, &Rationale::Legacy("r".into()));
    }

    #[test]
    fn a_thinking_rationale_can_be_redacted() {
        let amd: GovernanceLogId = "AMD-2026-0009".parse().unwrap();
        let redacted = super::super::redact_data(
            &synthetic("thinking"),
            &["/rounds/0/responses/0/rationale".into()],
            &amd,
            Blind::random(),
        )
        .unwrap();
        let record = round_trips("thinking", &redacted);
        assert_eq!(
            record.rounds[0].responses[0].rationale.redacted_by(),
            Some(&amd)
        );
    }

    /// Recusal, abstention rationales and a recused Steward's emergency
    /// note, as the Council writes them from 0.48
    #[test]
    fn a_recused_decision_reads_back() {
        let record = round_trips("recusal", &synthetic("recusal"));
        assert_eq!(record.final_votes.steward, CouncilVote::Recused);
        let recusal = record.steward_recusal.unwrap();
        assert!(recusal.recused);
        assert_eq!(
            recusal
                .votes
                .iter()
                .filter(|v| v.limits_steward_powers)
                .count(),
            3
        );
        assert_eq!(record.abstentions[0].member, CouncilMember::Engineer);
        let emergency = record.rounds[0].steward_emergency.as_ref().unwrap();
        assert_eq!(
            emergency.reason.value().map(String::as_str),
            Some("An injected comment is steering the seats.")
        );
        // Absent everywhere they are not written: earlier records keep
        // their bytes.
        let veto = round_trips("veto", &synthetic("veto"));
        assert_eq!(veto.steward_recusal, None);
        assert!(veto.abstentions.is_empty());
    }

    #[test]
    fn a_recused_decision_reads_in_order() {
        let data = synthetic("recusal");
        let keys = |v: &serde_json::Value| -> Vec<String> {
            super::super::reading::ordered(v.as_object().unwrap())
                .into_iter()
                .map(|(k, _)| k.clone())
                .collect()
        };
        let top = keys(&data);
        let at = |k: &str| top.iter().position(|x| x == k).unwrap();
        assert!(at("rounds") < at("steward_recusal"));
        assert!(at("steward_recusal") < at("final_votes"));
        assert!(at("final_votes") < at("abstentions"));
        assert!(at("abstentions") < at("vote_tally"));
        assert_eq!(
            keys(&data["steward_recusal"]["votes"][0]),
            ["seat", "reason", "limits_steward_powers"]
        );
        assert_eq!(keys(&data["steward_recusal"]), ["votes", "recused"]);
        assert_eq!(
            keys(&data["rounds"][0]),
            [
                "number",
                "round_type",
                "steward_emergency",
                "steward_contribution",
                "responses"
            ]
        );
        assert_eq!(keys(&data["abstentions"][0]), ["member", "rationale"]);
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
        assert_eq!(first.rationale.redacted_by(), Some(&amd));
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

    /// Every signed record, typed and written back, has the bytes it was
    /// signed with: the 0.96 `summary` adds nothing to a record without one
    #[test]
    fn a_record_without_a_summary_keeps_its_signed_bytes() {
        let fixtures = fixtures();
        for (id, signed, _) in SIGNED {
            let name = format!("{id}.json");
            let (_, data) = fixtures.iter().find(|(n, _)| *n == name).unwrap();
            let record: CouncilDecisionRecord =
                serde_json::from_value(data.clone()).unwrap();
            assert_eq!(record.summary, None, "{id}");
            let again = serde_json::to_value(&record).unwrap();
            assert!(again.get("summary").is_none(), "{id}");
            assert_eq!(
                super::super::canonical_json(&again),
                super::super::canonical_json(data),
                "{id}"
            );
            assert_eq!(
                super::super::data_hash(&again).to_hex(),
                signed,
                "{id}"
            );
        }
    }

    #[test]
    fn a_signed_summary_reads_back() {
        let record = round_trips("summary", &synthetic("summary"));
        let summary = record.summary.as_ref().unwrap();
        assert_eq!(summary.written_by, CouncilSeat::Lawyer);
        assert_eq!(summary.accepted_by, SummaryAcceptance::Steward);
        assert_eq!(summary.served_text(), "## Decision\n\nApproved 5-0.");
        assert_eq!(record.check(), Ok(()));
        assert_eq!(
            CouncilDecisionRecord::check_data(&synthetic("summary")),
            Ok(())
        );
        assert_eq!(
            summary.authorship().sentence(),
            "written by the Lawyer, accepted by the Steward"
        );

        let recused =
            round_trips("recused_summary", &synthetic("recused_summary"));
        let summary = recused.summary.as_ref().unwrap();
        assert_eq!(
            summary.accepted_by,
            SummaryAcceptance::Seats {
                seats: RECUSED_ACCEPTORS.to_vec()
            }
        );
        assert_eq!(recused.check(), Ok(()));
        assert_eq!(
            summary.authorship().sentence(),
            "written by the Lawyer, accepted by the Artist, Philosopher and \
             Engineer, the Steward being recused"
        );
    }

    /// What `with_summary` writes is what the rules accept, recused or not
    #[test]
    fn with_summary_writes_the_acceptance_the_item_needs() {
        for name in ["summary", "recused_summary"] {
            let mut record: CouncilDecisionRecord =
                serde_json::from_value(synthetic(name)).unwrap();
            let signed = record.summary.take().unwrap();
            let written = record.with_summary(signed.served_text());
            assert_eq!(written.summary.as_ref(), Some(&signed), "{name}");
            assert_eq!(written.check(), Ok(()), "{name}");
        }
    }

    #[test]
    fn a_summary_breaking_the_rules_is_refused() {
        let check = |data: &serde_json::Value| {
            let record: CouncilDecisionRecord =
                serde_json::from_value(data.clone()).unwrap();
            let typed = record.check();
            assert_eq!(typed, CouncilDecisionRecord::check_data(data));
            typed
        };
        let mut data = synthetic("summary");
        data["summary"]["written_by"] = "engineer".into();
        assert_eq!(
            check(&data),
            Err(RecordRuleError::SummaryWriter(CouncilSeat::Engineer))
        );

        let mut data = synthetic("summary");
        data["summary"]["text"] = " \n".into();
        assert_eq!(check(&data), Err(RecordRuleError::BlankSummary));

        // Seats accepting where the Steward is not recused
        let mut data = synthetic("summary");
        data["summary"]["accepted_by"] =
            synthetic("recused_summary")["summary"]["accepted_by"].clone();
        assert_eq!(
            check(&data),
            Err(RecordRuleError::SummaryAcceptance { recused: false })
        );

        // The Steward accepting where the Steward is recused, and too few
        // seats
        let mut data = synthetic("recused_summary");
        data["summary"]["accepted_by"] = serde_json::json!({"by": "steward"});
        assert_eq!(
            check(&data),
            Err(RecordRuleError::SummaryAcceptance { recused: true })
        );
        data["summary"]["accepted_by"] =
            serde_json::json!({"by": "seats", "seats": ["artist", "engineer"]});
        assert_eq!(
            check(&data),
            Err(RecordRuleError::SummaryAcceptance { recused: true })
        );

        // A redacted summary was there
        let amd: GovernanceLogId = "AMD-2026-0009".parse().unwrap();
        let redacted = super::super::redact_data(
            &synthetic("summary"),
            &["/summary/text".into()],
            &amd,
            Blind::random(),
        )
        .unwrap();
        let record = round_trips("summary", &redacted);
        assert_eq!(record.check(), Ok(()));
        let summary = record.summary.unwrap();
        assert_eq!(summary.text, Redactable::Redacted(amd.clone()));
        assert_eq!(summary.served_text(), super::super::redaction_marker(&amd));

        // Not a summary at all
        let mut data = synthetic("summary");
        data["summary"] = "just text".into();
        assert_eq!(
            CouncilDecisionRecord::check_data(&data),
            Err(RecordRuleError::Unreadable)
        );
    }

    /// A summary changes by a revision at `/summary`: the patch applies,
    /// the result keeps the rules, and the revised summary is what is
    /// served, the original still reading as signed
    #[test]
    fn a_revision_of_the_summary_applies_and_serves() {
        use crate::enums::GovernanceLogEntryType::CouncilDecision;
        use json_patch::{Patch, PatchOperation, ReplaceOperation};
        let original = synthetic("summary");
        let patch = Patch(vec![PatchOperation::Replace(ReplaceOperation {
            path: format!("{SUMMARY_POINTER}/text").parse().unwrap(),
            value: "## Decision\n\nApproved 5-0, as corrected.".into(),
        })]);
        let (draft, revised) = super::super::AmendmentDraft::revision(
            "GOV-2026-0099".parse().unwrap(),
            CouncilDecision,
            super::super::data_hash(&serde_json::json!(null)),
            "Steward's record",
            "the summary corrected",
            &original,
            patch,
        )
        .unwrap();
        let revision = draft.amendment.revision.as_ref().unwrap();
        assert_eq!(draft.amendment.kind, AmendmentKind::Revision);
        assert_eq!(
            super::super::revision_touches(revision, &[SUMMARY_POINTER]),
            Some(SUMMARY_POINTER)
        );
        assert_eq!(super::super::kind_standing(draft.amendment.kind), None);
        let latest = super::super::latest(&original, [revision]).unwrap();
        assert_eq!(latest, revised);
        assert_eq!(
            super::super::data_hash(&latest),
            revision.resulting_data_hash
        );
        assert_eq!(CouncilDecisionRecord::check_data(&latest), Ok(()));
        round_trips("revised summary", &latest);

        let clerk = Some("The Clerk's digest.".to_string());
        let (text, by) =
            served_summary(CouncilDecision, Some(&latest), clerk.clone());
        assert_eq!(
            text.as_deref(),
            Some("## Decision\n\nApproved 5-0, as corrected.")
        );
        assert_eq!(by.unwrap().written_by, CouncilSeat::Lawyer);
        let (text, _) =
            served_summary(CouncilDecision, Some(&original), clerk.clone());
        assert_eq!(text.as_deref(), Some("## Decision\n\nApproved 5-0."));

        // A record signed without one is served the Clerk's
        let (_, old) = fixtures().swap_remove(0);
        let served = served_summary(CouncilDecision, Some(&old), clerk.clone());
        assert_eq!(served, (clerk.clone(), None));
        // as is a read without `data`, or any other entry
        assert_eq!(
            served_summary(CouncilDecision, None, clerk.clone()),
            (clerk.clone(), None)
        );
        assert_eq!(
            served_summary(
                crate::enums::GovernanceLogEntryType::AppealsCourtDecision,
                Some(&latest),
                clerk.clone()
            ),
            (clerk, None)
        );

        // A revision handing acceptance to the seats breaks the rules,
        // which the writer checks on every revision
        let wrong = Patch(vec![PatchOperation::Replace(ReplaceOperation {
            path: format!("{SUMMARY_POINTER}/accepted_by").parse().unwrap(),
            value: serde_json::json!({
                "by": "seats", "seats": ["artist", "philosopher", "engineer"]
            }),
        })]);
        let wrong = super::super::apply_patch(&latest, &wrong).unwrap();
        assert_eq!(
            CouncilDecisionRecord::check_data(&wrong),
            Err(RecordRuleError::SummaryAcceptance { recused: false })
        );
    }

    /// A summary added by revision to a record signed without one
    #[test]
    fn a_revision_can_add_a_summary_to_an_older_record() {
        use json_patch::{AddOperation, Patch, PatchOperation};
        let (name, old) = fixtures().pop().unwrap();
        assert_eq!(name, "GOV-2026-0013.json");
        let record: CouncilDecisionRecord =
            serde_json::from_value(old.clone()).unwrap();
        let summary = record.clone().with_summary("Ranked.").summary.unwrap();
        let patch = Patch(vec![PatchOperation::Add(AddOperation {
            path: SUMMARY_POINTER.parse().unwrap(),
            value: serde_json::to_value(&summary).unwrap(),
        })]);
        let (_, revised) = super::super::AmendmentDraft::revision(
            record.id.clone(),
            crate::enums::GovernanceLogEntryType::CouncilDecision,
            super::super::data_hash(&serde_json::json!(null)),
            "b",
            "n",
            &old,
            patch,
        )
        .unwrap();
        assert_eq!(CouncilDecisionRecord::check_data(&revised), Ok(()));
        let again = round_trips(&name, &revised);
        assert_eq!(again.summary, Some(summary));
    }

    #[test]
    fn a_summary_reads_after_the_outcome() {
        let data = synthetic("recused_summary");
        let keys = |v: &serde_json::Value| -> Vec<String> {
            super::super::reading::ordered(v.as_object().unwrap())
                .into_iter()
                .map(|(k, _)| k.clone())
                .collect()
        };
        let top = keys(&data);
        let at = |k: &str| top.iter().position(|x| x == k).unwrap();
        assert!(at("outcome") < at("summary"));
        assert_eq!(
            keys(&data["summary"]),
            ["written_by", "text", "accepted_by"]
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
