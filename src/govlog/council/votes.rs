//! The votes that decide a Council item, and the arithmetic that decides
//! it (0.76).
//!
//! Two forms, for two jobs:
//!
//! - [`FinalVotes`] is what a record holds: five bare [`CouncilVote`]s,
//!   with a veto's rationale in `veto_rationale` and each abstention's in
//!   `abstentions` beside it. It reads every entry ever signed, including
//!   those from before a rationale was required, so it cannot require one.
//!   The thresholds ([`FinalVotes::outcome`]) are computed on it, so a
//!   reader can check any signed outcome.
//! - [`Votes`] is what a Council decides with: a [`StewardVote`] that
//!   carries its rationale when it is a veto or an abstention, and a
//!   [`SeatVote`] per seat that carries the seat's statement. A decision
//!   built from it cannot leave out what the Constitution requires (Art.
//!   IV § 3, each abstention's rationale; § 5, each veto's). It converts
//!   into the record's fields with `From`, which cannot fail.
//!
//! There is no conversion back: an entry signed before GOV-2026-0009 may
//! hold an `abstain` with no rationale anywhere, and nothing should
//! pretend it had one.

use std::fmt;
use std::ops::Deref;

use serde::{Deserialize, Serialize};

use super::{
    Abstention, CouncilMember, CouncilSeat, CouncilVote, DecisionCategory,
    FinalVotes,
};
use crate::enums::DecisionOutcome;

/// Words for the record that are never blank: a seat's statement with its
/// final vote, the Steward's rationale for a veto or an abstention.
///
/// Built only by `TryFrom`, which refuses text that is empty once
/// whitespace is trimmed; the text is kept as given. Serialized as a plain
/// string, and its schema is a plain string with no `minLength` or
/// `pattern` (neither is enforced under `strict`; see agora's CLAUDE.md),
/// so a blank one is caught when parsed, not by the grammar.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Statement(String);

/// A [`Statement`] that would have been blank
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("words for the record may not be blank")]
pub struct BlankStatement;

impl TryFrom<String> for Statement {
    type Error = BlankStatement;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        if text.trim().is_empty() {
            Err(BlankStatement)
        } else {
            Ok(Self(text))
        }
    }
}

impl TryFrom<&str> for Statement {
    type Error = BlankStatement;

    fn try_from(text: &str) -> Result<Self, Self::Error> {
        text.to_string().try_into()
    }
}

impl From<Statement> for String {
    fn from(statement: Statement) -> Self {
        statement.0
    }
}

impl Deref for Statement {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for Statement {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Statement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for Statement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

#[cfg(feature = "schemars")]
impl schemars::JsonSchema for Statement {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("Statement")
    }

    fn json_schema(
        generator: &mut schemars::SchemaGenerator,
    ) -> schemars::Schema {
        String::json_schema(generator)
    }
}

/// The Steward's vote on an item, with the rationale the Constitution
/// requires when it is a veto (Art. IV § 5) or an abstention (Art. IV
/// § 3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "vote", rename_all = "snake_case")]
pub enum StewardVote {
    Yes,
    No,
    /// Agreement with the seats' majority, on a Policy item; counts as yes
    Concur,
    Abstain {
        rationale: Statement,
    },
    /// Rejects the item whatever the others voted
    Veto {
        rationale: Statement,
    },
    /// On an item the four seats determined limits the Steward's powers:
    /// no vote and no veto (Art. IV § 3, GOV-2026-0009). Written only from
    /// a recorded determination.
    Recused,
}

impl From<&StewardVote> for CouncilVote {
    fn from(vote: &StewardVote) -> Self {
        match vote {
            StewardVote::Yes => Self::Yes,
            StewardVote::No => Self::No,
            StewardVote::Concur => Self::Concur,
            StewardVote::Abstain { .. } => Self::Abstain,
            StewardVote::Veto { .. } => Self::Veto,
            StewardVote::Recused => Self::Recused,
        }
    }
}

/// What a seat can vote on a voted item
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeatChoice {
    Yes,
    No,
    /// Leaves the denominator (Art. IV § 3)
    Abstain,
    /// A vote to postpone: it participates, so it stays in the denominator
    Defer,
}

impl From<SeatChoice> for CouncilVote {
    fn from(choice: SeatChoice) -> Self {
        match choice {
            SeatChoice::Yes => Self::Yes,
            SeatChoice::No => Self::No,
            SeatChoice::Abstain => Self::Abstain,
            SeatChoice::Defer => Self::Defer,
        }
    }
}

/// A [`CouncilVote`] that no seat casts on a voted item
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("`{0:?}` is not a seat's vote on a voted item")]
pub struct NotASeatChoice(pub CouncilVote);

impl TryFrom<CouncilVote> for SeatChoice {
    type Error = NotASeatChoice;

    fn try_from(vote: CouncilVote) -> Result<Self, Self::Error> {
        match vote {
            CouncilVote::Yes => Ok(Self::Yes),
            CouncilVote::No => Ok(Self::No),
            CouncilVote::Abstain => Ok(Self::Abstain),
            CouncilVote::Defer => Ok(Self::Defer),
            other @ (CouncilVote::Concur
            | CouncilVote::Veto
            | CouncilVote::Ranked
            | CouncilVote::Recused) => Err(NotASeatChoice(other)),
        }
    }
}

/// A seat's final vote, with its statement for the record. The statement
/// is the seat's `position` in the final round, and an abstaining seat's
/// rationale (Art. IV § 3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatVote {
    pub choice: SeatChoice,
    pub statement: Statement,
}

/// One member's vote, seat or Steward: what [`Votes::iter`] yields
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vote<'a> {
    Seat(&'a SeatVote),
    Steward(&'a StewardVote),
}

impl Vote<'_> {
    /// The vote as a record holds it
    pub fn cast(&self) -> CouncilVote {
        match self {
            Self::Seat(v) => v.choice.into(),
            Self::Steward(v) => (*v).into(),
        }
    }

    /// The rationale an abstention carries, if this is one
    pub fn abstention(&self) -> Option<&Statement> {
        match self {
            Self::Seat(SeatVote {
                choice: SeatChoice::Abstain,
                statement,
            }) => Some(statement),
            Self::Steward(StewardVote::Abstain { rationale }) => {
                Some(rationale)
            }
            Self::Seat(_) | Self::Steward(_) => None,
        }
    }
}

/// The four seats' final votes, each with its statement: what the
/// Steward is shown before voting, and the seats' part of [`Votes`]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatVotes {
    pub artist: SeatVote,
    pub philosopher: SeatVote,
    pub lawyer: SeatVote,
    pub engineer: SeatVote,
}

impl SeatVotes {
    /// One seat's vote
    pub fn seat(&self, seat: CouncilSeat) -> &SeatVote {
        match seat {
            CouncilSeat::Artist => &self.artist,
            CouncilSeat::Philosopher => &self.philosopher,
            CouncilSeat::Lawyer => &self.lawyer,
            CouncilSeat::Engineer => &self.engineer,
        }
    }

    /// Every seat's vote, in seating order
    pub fn iter(&self) -> impl Iterator<Item = (CouncilSeat, &SeatVote)> {
        CouncilSeat::ALL
            .into_iter()
            .map(|seat| (seat, self.seat(seat)))
    }

    /// Seats voting yes
    pub fn yes_count(&self) -> usize {
        self.iter()
            .filter(|(_, v)| v.choice == SeatChoice::Yes)
            .count()
    }

    /// Seats voting no
    pub fn no_count(&self) -> usize {
        self.iter()
            .filter(|(_, v)| v.choice == SeatChoice::No)
            .count()
    }
}

/// The five votes that decide a voted item, each with the words the
/// Constitution requires of it. See the [module docs](self).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Votes {
    #[serde(flatten)]
    pub seats: SeatVotes,
    pub steward: StewardVote,
}

impl Votes {
    /// One seat's vote
    pub fn seat(&self, seat: CouncilSeat) -> &SeatVote {
        self.seats.seat(seat)
    }

    /// Every member's vote, the seats in seating order and the Steward
    /// last
    pub fn iter(&self) -> impl Iterator<Item = (CouncilMember, Vote<'_>)> {
        self.seats
            .iter()
            .map(|(seat, vote)| (seat.into(), Vote::Seat(vote)))
            .chain([(CouncilMember::Steward, Vote::Steward(&self.steward))])
    }

    /// Each abstention with its rationale, in [`iter`](Self::iter)'s
    /// order: the record's `abstentions`
    pub fn abstentions(&self) -> Vec<Abstention> {
        self.iter()
            .filter_map(|(member, vote)| {
                Some(Abstention {
                    member,
                    rationale: vote.abstention()?.to_string().into(),
                })
            })
            .collect()
    }

    /// The Steward's rationale, when the Steward vetoed: the record's
    /// `veto_rationale`
    pub fn veto_rationale(&self) -> Option<&Statement> {
        match &self.steward {
            StewardVote::Veto { rationale } => Some(rationale),
            _ => None,
        }
    }

    /// The outcome under `category`'s threshold
    /// ([`FinalVotes::outcome`])
    pub fn outcome(&self, category: DecisionCategory) -> DecisionOutcome {
        FinalVotes::from(self).outcome(category)
    }

    /// The record's `vote_tally` ([`FinalVotes::tally`])
    pub fn tally(&self) -> String {
        FinalVotes::from(self).tally()
    }
}

impl From<&Votes> for FinalVotes {
    fn from(votes: &Votes) -> Self {
        Self {
            artist: votes.seats.artist.choice.into(),
            philosopher: votes.seats.philosopher.choice.into(),
            lawyer: votes.seats.lawyer.choice.into(),
            engineer: votes.seats.engineer.choice.into(),
            steward: (&votes.steward).into(),
        }
    }
}

impl FinalVotes {
    /// Every member's vote, in seating order and the Steward last
    fn all(&self) -> [CouncilVote; 5] {
        [
            self.artist,
            self.philosopher,
            self.lawyer,
            self.engineer,
            self.steward,
        ]
    }

    /// The four seats' votes
    fn seats(&self) -> [CouncilVote; 4] {
        [self.artist, self.philosopher, self.lawyer, self.engineer]
    }

    /// Yes votes, `concur` counted as yes
    pub fn yes_count(&self) -> usize {
        self.all()
            .iter()
            .filter(|v| matches!(v, CouncilVote::Yes | CouncilVote::Concur))
            .count()
    }

    /// No votes
    pub fn no_count(&self) -> usize {
        self.all()
            .iter()
            .filter(|v| matches!(v, CouncilVote::No))
            .count()
    }

    /// Whether the Steward is recused on the item
    pub fn steward_recused(&self) -> bool {
        self.steward == CouncilVote::Recused
    }

    /// Members voting: everyone but abstainers and a recused Steward.
    ///
    /// Constitution Art. IV § 3, "Abstention and Recusal" (GOV-2026-0009):
    /// an abstention or recusal removes the member from the denominator;
    /// it is neither a vote for nor against. A `defer` is a vote, a member
    /// asking to postpone is participating, not standing aside, so it
    /// stays in the denominator and blocks unanimity.
    pub fn voting_count(&self) -> usize {
        self.all()
            .iter()
            .filter(|v| {
                !matches!(v, CouncilVote::Abstain | CouncilVote::Recused)
            })
            .count()
    }

    /// `"<yes>-<no>"`, noting a recused Steward: the record's `vote_tally`
    /// on a voted item
    pub fn tally(&self) -> String {
        if self.steward_recused() {
            format!(
                "{}-{} (Steward recused)",
                self.yes_count(),
                self.no_count()
            )
        } else {
            format!("{}-{}", self.yes_count(), self.no_count())
        }
    }

    /// The outcome under `category`'s threshold (Constitution Art. IV § 3,
    /// as amended by GOV-2026-0009).
    ///
    /// A recused Steward puts the recused thresholds into effect, with no
    /// separate vote on the arithmetic. A `Schedule` item is ranked, not
    /// voted, and is recorded set without a tally; reaching this with one
    /// means a caller routed it through the yes/no path, and it is
    /// `Deferred`.
    pub fn outcome(&self, category: DecisionCategory) -> DecisionOutcome {
        let recused = self.steward_recused();
        let passes = |pass: bool| match pass {
            true => DecisionOutcome::Approved,
            false => DecisionOutcome::Rejected,
        };

        // A veto rejects regardless of tally (Art. IV § 2). A recused
        // Steward holds no veto (GOV-2026-0009), and recusal and veto are
        // the same field, so a recused Steward's vote cannot also be one.
        if self.steward == CouncilVote::Veto {
            return DecisionOutcome::Rejected;
        }

        match category {
            // Three affirmative votes, an absolute count. With the Steward
            // recused they must be found among the four seats, which is
            // the same count: a recusal is never a yes.
            DecisionCategory::Routine => passes(self.yes_count() >= 3),
            // "Unanimous among the four Council agents": all four yes. The
            // row names its denominator, so a seat's abstention does not
            // shrink it here.
            DecisionCategory::Policy if recused => {
                passes(self.seats().iter().all(|v| *v == CouncilVote::Yes))
            }
            // Four of five including the Steward. A Steward abstention
            // still rejects (§ 4.4's pocket veto): an abstention is not a
            // recusal, which only the seats' determination makes.
            DecisionCategory::Policy => passes(
                self.yes_count() >= 4
                    && matches!(
                        self.steward,
                        CouncilVote::Yes | CouncilVote::Concur
                    ),
            ),
            // GOV-2026-0009 ("Define unanimous"): unanimous among those
            // voting, and no fewer than four voting. The floor keeps a
            // shrunken denominator from hollowing out unanimity (the
            // Council chose 4 so that one member may stand aside, and so a
            // recused-Steward item tolerates no seat abstention).
            DecisionCategory::Constitutional => {
                let voting = self.voting_count();
                passes(voting >= 4 && self.yes_count() == voting)
            }
            // The Steward alone
            DecisionCategory::Emergency => passes(matches!(
                self.steward,
                CouncilVote::Yes | CouncilVote::Concur
            )),
            DecisionCategory::Schedule => DecisionOutcome::Deferred,
        }
    }
}

#[cfg(test)]
mod tests;
