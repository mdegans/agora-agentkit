//! The thresholds as amended by GOV-2026-0009 ("Define unanimous",
//! 2026-09-26, 5-0), moved here from the Council program with their tests
//! unchanged: unanimity is among those voting, with a floor of four
//! voting; an abstention exits the denominator. With the Steward seated,
//! the Policy and Routine rows' arithmetic is unchanged; with the Steward
//! recused, each category's recused threshold holds.

use super::*;

use CouncilVote::{Abstain, Defer, No, Recused, Veto, Yes};
use DecisionCategory::{Constitutional, Policy, Routine};
use DecisionOutcome::{Approved, Rejected};

fn votes(
    artist: CouncilVote,
    philosopher: CouncilVote,
    lawyer: CouncilVote,
    engineer: CouncilVote,
    steward: CouncilVote,
) -> FinalVotes {
    FinalVotes {
        artist,
        philosopher,
        lawyer,
        engineer,
        steward,
    }
}

#[test]
fn constitutional_all_five_yes_still_passes() {
    assert_eq!(
        votes(Yes, Yes, Yes, Yes, Yes).outcome(Constitutional),
        Approved
    );
}

#[test]
fn constitutional_steward_abstention_no_longer_blocks() {
    // Four agents vote, all yes. Before GOV-2026-0009 this was Rejected;
    // the amendment's whole point is that it passes.
    let v = votes(Yes, Yes, Yes, Yes, Abstain);
    assert_eq!(v.outcome(Constitutional), Approved);
}

#[test]
fn constitutional_floor_of_four_holds() {
    // Two abstentions leave three voting, below the floor the Council
    // chose exactly so unanimity cannot hollow out.
    let v = votes(Yes, Yes, Yes, Abstain, Abstain);
    assert_eq!(v.outcome(Constitutional), Rejected);
}

#[test]
fn constitutional_one_no_still_blocks() {
    assert_eq!(
        votes(Yes, Yes, Yes, No, Abstain).outcome(Constitutional),
        Rejected
    );
    assert_eq!(
        votes(Yes, Yes, Yes, Yes, No).outcome(Constitutional),
        Rejected
    );
}

#[test]
fn constitutional_defer_is_a_vote_and_blocks() {
    // A defer participates (asks to postpone); it is not a stand-aside, so
    // it blocks unanimity rather than exiting the denominator.
    let v = votes(Yes, Yes, Yes, Defer, Yes);
    assert_eq!(v.outcome(Constitutional), Rejected);
}

#[test]
fn veto_still_rejects_everything() {
    let v = votes(Yes, Yes, Yes, Yes, Veto);
    for category in [Constitutional, Policy, Routine] {
        assert_eq!(v.outcome(category), Rejected, "{category:?}");
    }
}

#[test]
fn policy_with_the_steward_seated() {
    // 4 yes including the Steward passes; a Steward abstention still
    // rejects (§ 4.4's pocket veto) because an abstention is not a
    // recusal: only the seats' determination makes one.
    assert_eq!(votes(Yes, Yes, Yes, No, Yes).outcome(Policy), Approved);
    assert_eq!(votes(Yes, Yes, Yes, Yes, Abstain).outcome(Policy), Rejected);
}

/// Policy, Steward recused: unanimous among the four Council agents. The
/// Steward-inclusion condition falls away; one seat short of four yes, by
/// a no or an abstention, rejects.
#[test]
fn policy_with_the_steward_recused_needs_all_four_seats() {
    assert_eq!(votes(Yes, Yes, Yes, Yes, Recused).outcome(Policy), Approved);
    assert_eq!(votes(Yes, Yes, Yes, No, Recused).outcome(Policy), Rejected);
    assert_eq!(
        votes(Yes, Yes, Yes, Abstain, Recused).outcome(Policy),
        Rejected
    );
}

/// Constitutional, Steward recused: unanimous among those voting with no
/// fewer than four voting, so all four seats yes; a seat's abstention
/// drops below the floor.
#[test]
fn constitutional_with_the_steward_recused() {
    let v = votes(Yes, Yes, Yes, Yes, Recused);
    assert_eq!(v.voting_count(), 4, "the recusal leaves the denominator");
    assert_eq!(v.outcome(Constitutional), Approved);
    assert_eq!(
        votes(Yes, Yes, Yes, Abstain, Recused).outcome(Constitutional),
        Rejected
    );
    assert_eq!(
        votes(Yes, Yes, Yes, No, Recused).outcome(Constitutional),
        Rejected
    );
}

/// Routine, Steward recused: three affirmative votes, found among the
/// four. Abstentions are tolerated as before.
#[test]
fn routine_with_the_steward_recused() {
    assert_eq!(votes(Yes, Yes, Yes, No, Recused).outcome(Routine), Approved);
    assert_eq!(
        votes(Yes, Yes, Yes, Abstain, Recused).outcome(Routine),
        Approved
    );
    assert_eq!(votes(Yes, Yes, No, No, Recused).outcome(Routine), Rejected);
}

#[test]
fn routine_absolute_count_unchanged() {
    // 3 affirmative of 5, abstentions tolerated: the count is absolute,
    // achievable with the Steward standing aside.
    assert_eq!(votes(Yes, Yes, Yes, No, Abstain).outcome(Routine), Approved);
    assert_eq!(
        votes(Yes, Yes, Abstain, Abstain, Abstain).outcome(Routine),
        Rejected
    );
}

#[test]
fn emergency_is_the_stewards_alone() {
    assert_eq!(
        votes(No, No, No, No, Yes).outcome(DecisionCategory::Emergency),
        Approved
    );
    assert_eq!(
        votes(Yes, Yes, Yes, Yes, Abstain).outcome(DecisionCategory::Emergency),
        Rejected
    );
}

#[test]
fn a_recused_tally_says_so() {
    assert_eq!(
        votes(Yes, Yes, Yes, No, Recused).tally(),
        "3-1 (Steward recused)"
    );
    assert_eq!(votes(Yes, Yes, Yes, No, Yes).tally(), "4-1");
    assert_eq!(
        votes(Yes, Yes, Yes, Yes, CouncilVote::Concur).tally(),
        "5-0"
    );
}

fn statement(text: &str) -> Statement {
    text.try_into().unwrap()
}

fn seat(choice: SeatChoice, text: &str) -> SeatVote {
    SeatVote {
        choice,
        statement: statement(text),
    }
}

#[test]
fn a_statement_is_never_blank() {
    assert_eq!(Statement::try_from(""), Err(BlankStatement));
    assert_eq!(Statement::try_from(" \n\t"), Err(BlankStatement));
    // Kept as given
    assert_eq!(&*statement("  I abstain.\n"), "  I abstain.\n");
    // The same rule when it is read
    assert!(
        serde_json::from_value::<Statement>(serde_json::json!("  ")).is_err()
    );
    assert_eq!(
        serde_json::to_value(statement("Yes.")).unwrap(),
        serde_json::json!("Yes.")
    );
}

#[test]
fn a_rationale_cannot_be_left_out() {
    let blank = serde_json::json!({"vote": "veto", "rationale": " "});
    assert!(serde_json::from_value::<StewardVote>(blank).is_err());
    let missing = serde_json::json!({"vote": "abstain"});
    assert!(serde_json::from_value::<StewardVote>(missing).is_err());
    let veto = serde_json::json!({"vote": "veto", "rationale": "Because."});
    assert_eq!(
        serde_json::from_value::<StewardVote>(veto).unwrap(),
        StewardVote::Veto {
            rationale: statement("Because.")
        }
    );
}

fn decided(steward: StewardVote) -> Votes {
    Votes {
        seats: SeatVotes {
            artist: seat(SeatChoice::Yes, "The Artist's statement."),
            philosopher: seat(
                SeatChoice::Abstain,
                "I stand aside: I wrote it.",
            ),
            lawyer: seat(SeatChoice::Yes, "The Lawyer's statement."),
            engineer: seat(SeatChoice::Defer, "Not yet."),
        },
        steward,
    }
}

/// The record's fields come from the typed votes: the five bare votes, the
/// Steward's abstention with its rationale (a seat's is its final-round
/// statement, never copied), and a veto's rationale
#[test]
fn votes_become_the_records_fields() {
    let v = decided(StewardVote::Abstain {
        rationale: statement("I'll let the Council decide."),
    });
    assert_eq!(
        FinalVotes::from(&v),
        votes(Yes, Abstain, Yes, Defer, Abstain)
    );
    let abstention = serde_json::to_value(v.steward_abstention()).unwrap();
    assert_eq!(
        abstention,
        serde_json::json!({"member": "steward", "rationale": "I'll let the Council decide."})
    );
    assert_eq!(v.veto_rationale(), None);

    let v = decided(StewardVote::Veto {
        rationale: statement("It limits the appeals court."),
    });
    assert_eq!(
        v.veto_rationale().map(|r| &**r),
        Some("It limits the appeals court.")
    );
    assert_eq!(v.outcome(Routine), Rejected);
    assert_eq!(v.steward_abstention(), None, "a veto is not an abstention");
    assert_eq!(v.tally(), "2-0");
}

#[test]
fn iter_is_seating_order_then_the_steward() {
    let v = decided(StewardVote::Recused);
    let members: Vec<CouncilMember> = v.iter().map(|(m, _)| m).collect();
    assert_eq!(
        members,
        [
            CouncilMember::Artist,
            CouncilMember::Philosopher,
            CouncilMember::Lawyer,
            CouncilMember::Engineer,
            CouncilMember::Steward,
        ]
    );
    let cast: Vec<CouncilVote> =
        v.iter().map(|(_, vote)| vote.cast()).collect();
    assert_eq!(cast, [Yes, Abstain, Yes, Defer, Recused]);
}

#[test]
fn the_seats_alone_are_counted_before_the_steward_votes() {
    let v = decided(StewardVote::Recused);
    assert_eq!((v.seats.yes_count(), v.seats.no_count()), (2, 0));
    let seats: Vec<CouncilSeat> = v.seats.iter().map(|(s, _)| s).collect();
    assert_eq!(seats, CouncilSeat::ALL);
}

#[test]
fn the_prose_names_of_rounds_and_outcomes() {
    use crate::enums::RoundType;
    assert_eq!(RoundType::FinalVote.name(), "Final Vote");
    assert_eq!(RoundType::Independent.name(), "Independent Analysis");
    assert_eq!(
        RoundType::FinalVote.to_string(),
        "final_vote",
        "the stored spelling stands"
    );
    assert_eq!(DecisionOutcome::Approved.name(), "Approved");
    assert_eq!(DecisionOutcome::Approved.to_string(), "approved");
}

#[test]
fn only_a_seats_votes_are_seat_choices() {
    for vote in [Yes, No, Abstain, Defer] {
        assert_eq!(
            CouncilVote::from(SeatChoice::try_from(vote).unwrap()),
            vote
        );
    }
    for vote in [CouncilVote::Concur, Veto, CouncilVote::Ranked, Recused] {
        assert_eq!(SeatChoice::try_from(vote), Err(NotASeatChoice(vote)));
    }
}
