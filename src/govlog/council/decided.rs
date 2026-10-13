//! How a Council item was decided, as one value, so the parts of a record
//! that say so cannot disagree (0.76).
//!
//! A record states its decision six times over: `final_votes`, `outcome`,
//! `vote_tally`, `veto_rationale`, `abstentions` and `agenda_ranking`. A
//! writer that fills each by hand can sign a veto with no rationale, an
//! abstention with none, or an outcome its votes do not give. A writer
//! that builds a [`Decided`] and lets
//! [`CouncilDecisionRecord::decided`] fill them cannot.

use super::{
    AgendaRanking, CouncilDecisionRecord, CouncilRound, CouncilSeat,
    CouncilVote, DecisionCategory, FinalVotes, Redactable, SeatChoice,
    SeatVote, SeatVotes, Statement, Votes,
};
use crate::enums::{DecisionOutcome, RoundType};

/// How an item was decided.
///
/// Not every shape a record can hold is a `Decided`: a round tabled by a
/// recused Steward over an API refusal (`tabled_by_recused_steward`) has
/// none, because the Council program does not produce it (a refusal
/// aborts the sitting, 2026-09-29). Should it again, it needs a variant
/// here first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decided {
    /// Voted, under the category's threshold
    Voted(Votes),
    /// A `Schedule` item: the seats' rankings, aggregated. The Steward
    /// executes the order and does not vote; a seat that submitted no
    /// ballot is recorded `abstain`, as is the Steward, as every scheduling
    /// record has been.
    Ranked(AgendaRanking),
    /// Tabled by the Steward: every vote `defer`
    Tabled,
    /// The Steward recused, and `deferring` seats still deferring at the
    /// round cap: it returns to a later sitting, and may not be deferred
    /// again
    RecusedDeferral { deferring: u32 },
}

impl Decided {
    /// The record's `final_votes`
    pub fn final_votes(&self) -> FinalVotes {
        match self {
            Self::Voted(votes) => votes.into(),
            Self::Ranked(ranking) => {
                let ranked = |seat: CouncilSeat| match ranking
                    .ballots
                    .iter()
                    .any(|b| b.seat == seat)
                {
                    true => CouncilVote::Ranked,
                    false => CouncilVote::Abstain,
                };
                FinalVotes {
                    artist: ranked(CouncilSeat::Artist),
                    philosopher: ranked(CouncilSeat::Philosopher),
                    lawyer: ranked(CouncilSeat::Lawyer),
                    engineer: ranked(CouncilSeat::Engineer),
                    steward: CouncilVote::Abstain,
                }
            }
            Self::Tabled => FinalVotes {
                artist: CouncilVote::Defer,
                philosopher: CouncilVote::Defer,
                lawyer: CouncilVote::Defer,
                engineer: CouncilVote::Defer,
                steward: CouncilVote::Defer,
            },
            Self::RecusedDeferral { .. } => FinalVotes {
                artist: CouncilVote::Defer,
                philosopher: CouncilVote::Defer,
                lawyer: CouncilVote::Defer,
                engineer: CouncilVote::Defer,
                steward: CouncilVote::Recused,
            },
        }
    }

    /// The record's `outcome`
    pub fn outcome(&self, category: DecisionCategory) -> DecisionOutcome {
        match self {
            Self::Voted(votes) => votes.outcome(category),
            Self::Ranked(_) => DecisionOutcome::Approved,
            Self::Tabled | Self::RecusedDeferral { .. } => {
                DecisionOutcome::Deferred
            }
        }
    }

    /// The record's `vote_tally`
    pub fn tally(&self) -> String {
        match self {
            Self::Voted(votes) => votes.tally(),
            Self::Ranked(ranking) => format!(
                "ranked {}/{}; the Steward executes the order and does not vote",
                ranking.ballots.len(),
                CouncilSeat::ALL.len()
            ),
            Self::Tabled => "Deferred".to_string(),
            Self::RecusedDeferral { deferring } => format!(
                "Deferred: {deferring} seat(s) still deferring at the round cap, \
                 Steward recused; it returns to a later sitting and may not be \
                 deferred again"
            ),
        }
    }
}

impl CouncilDecisionRecord {
    /// Set every part of the record that states the decision from
    /// `decided`: `final_votes`, `outcome`, `vote_tally`,
    /// `veto_rationale`, `abstentions` and `agenda_ranking`
    pub fn decided(mut self, decided: Decided) -> Self {
        self.final_votes = decided.final_votes();
        self.outcome = decided.outcome(self.category);
        self.vote_tally = decided.tally().into();
        (self.veto_rationale, self.abstentions, self.agenda_ranking) =
            match decided {
                Decided::Voted(votes) => (
                    votes.veto_rationale().map(|r| r.to_string().into()),
                    votes.steward_abstention().into_iter().collect(),
                    None,
                ),
                Decided::Ranked(ranking) => (None, Vec::new(), Some(ranking)),
                Decided::Tabled | Decided::RecusedDeferral { .. } => {
                    (None, Vec::new(), None)
                }
            };
        self
    }

    /// The rules a record must keep whoever wrote it, checked before it is
    /// signed and on every revision of it: a Steward veto carries its
    /// rationale (Constitution Art. IV § 5), and a rationale belongs to a
    /// veto.
    ///
    /// [`StewardVote::Veto`] already cannot be built without one; this is
    /// the same rule for a record that did not come from [`Decided`]. No
    /// signed entry breaks it: no veto has ever been cast.
    pub fn check(&self) -> Result<(), RecordRuleError> {
        let vetoed = self.final_votes.steward == CouncilVote::Veto;
        let rationale = self.veto_rationale.as_ref().map(|r| match r {
            Redactable::Value(text) => !text.trim().is_empty(),
            // Lawfully redacted after it was signed: it was there
            Redactable::Redacted(_) => true,
        });
        match (vetoed, rationale) {
            (true, Some(true)) | (false, None) => Ok(()),
            (true, None | Some(false)) => {
                Err(RecordRuleError::VetoWithoutRationale)
            }
            (false, Some(_)) => Err(RecordRuleError::RationaleWithoutVeto),
        }
    }
}

/// A record that breaks a rule every record must keep
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RecordRuleError {
    #[error(
        "the Steward vetoed with no rationale; a veto is recorded with its \
         rationale (Constitution Art. IV § 5)"
    )]
    VetoWithoutRationale,
    #[error("the record carries a veto rationale but the Steward did not veto")]
    RationaleWithoutVeto,
}

/// Why a final round's responses are not a seat's votes
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FinalRoundError {
    #[error("round {0} is not a final vote")]
    NotFinal(u32),
    #[error("the {} has no response in the final round", .0.name())]
    Missing(CouncilSeat),
    #[error("the {} answered more than once in the final round", .0.name())]
    Twice(CouncilSeat),
    #[error("the {} cast no vote in the final round", .0.name())]
    NoVote(CouncilSeat),
    #[error("the {name}'s final vote, `{vote:?}`, is not a seat's vote", name = .0.name(), vote = .1)]
    NotASeatChoice(CouncilSeat, CouncilVote),
    #[error(
        "the {}'s final-vote statement is blank or withheld; it is the \
         record's words for the vote, and an abstention's rationale (Art. \
         IV § 3)",
        .0.name()
    )]
    NoStatement(CouncilSeat),
}

impl SeatVotes {
    /// The votes the seats cast in `round`, a `final_vote` round, each
    /// with its statement (its `position`)
    pub fn from_final_round(
        round: &CouncilRound,
    ) -> Result<Self, FinalRoundError> {
        if round.round_type != RoundType::FinalVote {
            return Err(FinalRoundError::NotFinal(round.number));
        }
        let seat = |seat: CouncilSeat| -> Result<SeatVote, FinalRoundError> {
            let mut responses =
                round.responses.iter().filter(|r| r.role == seat);
            let response =
                responses.next().ok_or(FinalRoundError::Missing(seat))?;
            if responses.next().is_some() {
                return Err(FinalRoundError::Twice(seat));
            }
            let vote = response.vote.ok_or(FinalRoundError::NoVote(seat))?;
            let choice = SeatChoice::try_from(vote)
                .map_err(|_| FinalRoundError::NotASeatChoice(seat, vote))?;
            let statement = response
                .position
                .value()
                .and_then(|p| Statement::try_from(p.as_str()).ok())
                .ok_or(FinalRoundError::NoStatement(seat))?;
            Ok(SeatVote { choice, statement })
        };
        Ok(Self {
            artist: seat(CouncilSeat::Artist)?,
            philosopher: seat(CouncilSeat::Philosopher)?,
            lawyer: seat(CouncilSeat::Lawyer)?,
            engineer: seat(CouncilSeat::Engineer)?,
        })
    }
}

#[cfg(test)]
mod tests;
