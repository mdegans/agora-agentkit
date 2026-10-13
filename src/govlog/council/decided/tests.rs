use super::*;
use crate::govlog::StewardVote;
use crate::govlog::{Rationale, Redactable, SeatResponse};

fn statement(text: &str) -> Statement {
    text.try_into().unwrap()
}

fn response(
    seat: CouncilSeat,
    vote: Option<CouncilVote>,
    position: &str,
) -> SeatResponse {
    SeatResponse {
        role: seat,
        position: position.to_string().into(),
        vote,
        rationale: Redactable::from(Rationale::from("thinking")),
        questions: Vec::new().into(),
        ready_to_vote: true,
        ranking: None,
        raw_text: None,
    }
}

fn final_round(responses: Vec<SeatResponse>) -> CouncilRound {
    CouncilRound {
        number: 3,
        round_type: RoundType::FinalVote,
        responses,
        steward_contribution: None,
        steward_emergency: None,
        tabled_by_recused_steward: false,
    }
}

fn four(votes: [(CouncilVote, &str); 4]) -> CouncilRound {
    final_round(
        CouncilSeat::ALL
            .into_iter()
            .zip(votes)
            .map(|(seat, (vote, position))| {
                response(seat, Some(vote), position)
            })
            .collect(),
    )
}

fn record() -> CouncilDecisionRecord {
    serde_json::from_value(serde_json::json!({
        "id": "GOV-2026-0099",
        "meeting_id": "00000000-0000-0000-0000-000000000001",
        "category": "Policy",
        "title": "t",
        "rounds": [],
        "final_votes": {
            "artist": "yes", "philosopher": "yes", "lawyer": "yes",
            "engineer": "yes", "steward": "yes"
        },
        "outcome": "approved",
        "vote_tally": "5-0",
        "landmark": false,
    }))
    .unwrap()
}

#[test]
fn the_votes_are_the_final_rounds() {
    use CouncilVote::{Abstain, No, Yes};
    let round = four([
        (Yes, "Yes, for the record."),
        (Abstain, "I stand aside: I wrote it."),
        (Yes, "Yes."),
        (No, "No."),
    ]);
    let votes = Votes {
        seats: SeatVotes::from_final_round(&round).unwrap(),
        steward: StewardVote::Abstain {
            rationale: statement("I'll let the Council decide."),
        },
    };
    assert_eq!(votes.seats.philosopher.choice, SeatChoice::Abstain);
    assert_eq!(
        &*votes.seats.philosopher.statement,
        "I stand aside: I wrote it."
    );

    let r = record().decided(Decided::Voted(votes));
    assert_eq!(
        r.final_votes,
        FinalVotes {
            artist: Yes,
            philosopher: Abstain,
            lawyer: Yes,
            engineer: No,
            steward: Abstain,
        }
    );
    // Policy, the Steward abstaining: § 4.4's pocket veto
    assert_eq!(r.outcome, DecisionOutcome::Rejected);
    assert_eq!(r.vote_tally.value().unwrap(), "2-1");
    assert_eq!(r.veto_rationale, None);
    assert_eq!(
        serde_json::to_value(&r.abstentions).unwrap(),
        serde_json::json!([
            {"member": "steward", "rationale": "I'll let the Council decide."},
        ]),
        "the Philosopher's rationale is its final-round statement, not copied"
    );
}

#[test]
fn a_veto_is_recorded_with_its_rationale() {
    use CouncilVote::Yes;
    let round = four([(Yes, "a"), (Yes, "b"), (Yes, "c"), (Yes, "d")]);
    let votes = Votes {
        seats: SeatVotes::from_final_round(&round).unwrap(),
        steward: StewardVote::Veto {
            rationale: statement("It limits the appeals court."),
        },
    };
    let r = record().decided(Decided::Voted(votes));
    assert_eq!(r.final_votes.steward, CouncilVote::Veto);
    assert_eq!(r.outcome, DecisionOutcome::Rejected);
    assert_eq!(
        r.veto_rationale
            .as_ref()
            .and_then(|v| v.value())
            .map(String::as_str),
        Some("It limits the appeals court.")
    );
    assert!(r.abstentions.is_empty());
}

#[test]
fn a_final_round_that_is_not_four_seats_voting_is_refused() {
    use CouncilVote::{Abstain, Ranked, Yes};
    let mut round = four([(Yes, "a"), (Abstain, "  "), (Yes, "c"), (Yes, "d")]);
    assert_eq!(
        SeatVotes::from_final_round(&round),
        Err(FinalRoundError::NoStatement(CouncilSeat::Philosopher))
    );
    round.responses[1].position =
        Redactable::Redacted("AMD-2026-0099".parse().unwrap());
    assert_eq!(
        SeatVotes::from_final_round(&round),
        Err(FinalRoundError::NoStatement(CouncilSeat::Philosopher))
    );
    round.responses[1] = response(CouncilSeat::Philosopher, None, "Refused.");
    assert_eq!(
        SeatVotes::from_final_round(&round),
        Err(FinalRoundError::NoVote(CouncilSeat::Philosopher))
    );
    round.responses[1] = response(CouncilSeat::Philosopher, Some(Ranked), "P1");
    assert_eq!(
        SeatVotes::from_final_round(&round),
        Err(FinalRoundError::NotASeatChoice(
            CouncilSeat::Philosopher,
            Ranked
        ))
    );
    round.responses.remove(1);
    assert_eq!(
        SeatVotes::from_final_round(&round),
        Err(FinalRoundError::Missing(CouncilSeat::Philosopher))
    );
    round
        .responses
        .push(response(CouncilSeat::Artist, Some(Yes), "again"));
    assert_eq!(
        SeatVotes::from_final_round(&round),
        Err(FinalRoundError::Twice(CouncilSeat::Artist))
    );
    round.round_type = RoundType::Deliberation;
    assert_eq!(
        SeatVotes::from_final_round(&round),
        Err(FinalRoundError::NotFinal(3))
    );
}

#[test]
fn a_recused_deferral_says_why() {
    let r = record().decided(Decided::RecusedDeferral { deferring: 2 });
    assert_eq!(r.final_votes.steward, CouncilVote::Recused);
    assert_eq!(r.final_votes.artist, CouncilVote::Defer);
    assert_eq!(r.outcome, DecisionOutcome::Deferred);
    assert!(
        r.vote_tally
            .value()
            .unwrap()
            .starts_with("Deferred: 2 seat(s)")
    );
}

#[test]
fn a_veto_without_its_rationale_is_refused_however_the_record_was_built() {
    let mut r = record();
    r.final_votes.steward = CouncilVote::Veto;
    assert_eq!(r.check(), Err(RecordRuleError::VetoWithoutRationale));
    r.veto_rationale = Some("  ".to_string().into());
    assert_eq!(r.check(), Err(RecordRuleError::VetoWithoutRationale));
    r.veto_rationale = Some("It limits the appeals court.".to_string().into());
    assert_eq!(r.check(), Ok(()));
    // Redacted after signing: it was there
    r.veto_rationale =
        Some(Redactable::Redacted("AMD-2026-0099".parse().unwrap()));
    assert_eq!(r.check(), Ok(()));
    r.final_votes.steward = CouncilVote::Yes;
    assert_eq!(r.check(), Err(RecordRuleError::RationaleWithoutVeto));
    // What Decided writes always passes
    let round = four([
        (CouncilVote::Yes, "a"),
        (CouncilVote::Yes, "b"),
        (CouncilVote::Yes, "c"),
        (CouncilVote::Yes, "d"),
    ]);
    let votes = Votes {
        seats: SeatVotes::from_final_round(&round).unwrap(),
        steward: StewardVote::Veto {
            rationale: statement("Because."),
        },
    };
    assert_eq!(record().decided(Decided::Voted(votes)).check(), Ok(()));
}

#[test]
fn the_rule_reads_raw_data_the_same_way() {
    use serde_json::json;
    let check = CouncilDecisionRecord::check_data;
    assert_eq!(check(&json!({"title": "no votes"})), Ok(()));
    assert_eq!(
        check(&json!({"final_votes": {"steward": "veto"}})),
        Err(RecordRuleError::VetoWithoutRationale)
    );
    assert_eq!(
        check(
            &json!({"final_votes": {"steward": "veto"}, "veto_rationale": " "})
        ),
        Err(RecordRuleError::VetoWithoutRationale)
    );
    assert_eq!(
        check(
            &json!({"final_votes": {"steward": "veto"}, "veto_rationale": "Because."})
        ),
        Ok(())
    );
    assert_eq!(
        check(
            &json!({"final_votes": {"steward": "yes"}, "veto_rationale": "Because."})
        ),
        Err(RecordRuleError::RationaleWithoutVeto)
    );
    assert_eq!(
        check(&json!({"final_votes": {"steward": "vetoed"}})),
        Err(RecordRuleError::Unreadable)
    );
}
