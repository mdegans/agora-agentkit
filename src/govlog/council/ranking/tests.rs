//! The aggregation's tests, moved from the Council program unchanged

use super::*;
use crate::govlog::CouncilSeat;

fn candidates(k: u32) -> BTreeMap<u32, PostId> {
    (1..=k)
        .map(|n| (n, PostId::from(uuid::Uuid::from_u128(n as u128))))
        .collect()
}

fn ballot(seat: CouncilSeat, capacity: u32, ranking: &[u32]) -> Ballot {
    Ballot {
        seat,
        sitting_capacity: capacity,
        ranking: ranking.to_vec(),
    }
}

fn numbers(r: &AgendaRanking) -> Vec<u32> {
    r.order.iter().map(|p| p.number).collect()
}

/// A seat that ranks thirty items does not outweigh a seat that ranks
/// three: the first choice scores K either way.
#[test]
fn borda_scale_is_the_dockets_not_the_ballots() {
    let r = AgendaRanking::aggregate(
        &candidates(10),
        vec![
            ballot(CouncilSeat::Artist, 3, &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]),
            ballot(CouncilSeat::Lawyer, 3, &[2]),
        ],
    );
    let score = |n| r.order.iter().find(|p| p.number == n).unwrap().borda;
    assert_eq!(score(1), 10);
    assert_eq!(score(2), 9 + 10);
    assert_eq!(numbers(&r)[0], 2);
}

#[test]
fn ties_break_on_seats_then_mean_position_then_are_flagged() {
    // 1: two seats at positions 2 and 2 → borda (K-1)*2 = 8 with K=5.
    // 2: one seat at position 1 plus one at 3 → 5 + 3 = 8. Same seats
    //    (2), same position sum (4): a genuine tie, flagged.
    let r = AgendaRanking::aggregate(
        &candidates(5),
        vec![
            ballot(CouncilSeat::Artist, 2, &[3, 1, 2]),
            ballot(CouncilSeat::Lawyer, 2, &[2, 1]),
        ],
    );
    let one = r.order.iter().find(|p| p.number == 1).unwrap();
    let two = r.order.iter().find(|p| p.number == 2).unwrap();
    assert_eq!(one.borda, 8);
    assert_eq!(two.borda, 8);
    assert_eq!((one.seats, one.position_sum), (2, 4));
    assert_eq!((two.seats, two.position_sum), (2, 4));
    assert_eq!(numbers(&r), [1, 2, 3], "exact ties fall back to P-number");
    assert!(r.order[0].tied_with_next);
    assert!(!r.order[1].tied_with_next);

    // Equal Borda, more seats wins. K = 5: item 1 sits 4th on two
    // ballots (2 + 2), item 2 sits 2nd on one (4).
    let r = AgendaRanking::aggregate(
        &candidates(5),
        vec![
            ballot(CouncilSeat::Artist, 1, &[3, 4, 5, 1]),
            ballot(CouncilSeat::Lawyer, 1, &[4, 3, 5, 1]),
            ballot(CouncilSeat::Philosopher, 1, &[3, 2]),
        ],
    );
    let one = r.order.iter().position(|p| p.number == 1).unwrap();
    let two = r.order.iter().position(|p| p.number == 2).unwrap();
    assert_eq!((r.order[one].borda, r.order[two].borda), (4, 4));
    assert_eq!(two, one + 1, "broader support wins the Borda tie");
    assert!(!r.order[one].tied_with_next);
}

#[test]
fn cut_is_the_lower_median() {
    let r = AgendaRanking::aggregate(
        &candidates(9),
        vec![
            ballot(CouncilSeat::Artist, 5, &[1]),
            ballot(CouncilSeat::Philosopher, 2, &[1]),
            ballot(CouncilSeat::Lawyer, 3, &[1]),
            ballot(CouncilSeat::Engineer, 4, &[1]),
        ],
    );
    assert_eq!(r.cut, 3, "sorted 2,3,4,5 → lower middle is 3");
    assert_eq!(r.docket().count(), 1, "the docket never exceeds the order");
}

#[test]
fn condorcet_winner_and_its_absence() {
    let r = AgendaRanking::aggregate(
        &candidates(3),
        vec![
            ballot(CouncilSeat::Artist, 2, &[1, 2, 3]),
            ballot(CouncilSeat::Philosopher, 2, &[1, 3, 2]),
            ballot(CouncilSeat::Lawyer, 2, &[2, 1, 3]),
        ],
    );
    assert_eq!(r.condorcet_winner, Some(1));

    // The classic cycle: 1>2>3, 2>3>1, 3>1>2.
    let r = AgendaRanking::aggregate(
        &candidates(3),
        vec![
            ballot(CouncilSeat::Artist, 2, &[1, 2, 3]),
            ballot(CouncilSeat::Philosopher, 2, &[2, 3, 1]),
            ballot(CouncilSeat::Lawyer, 2, &[3, 1, 2]),
        ],
    );
    assert_eq!(r.condorcet_winner, None);
    assert_eq!(r.order.len(), 3);
}

/// Ranked beats unranked; two unranked items are no preference.
#[test]
fn partial_ballots_in_the_pairwise_report() {
    let r = AgendaRanking::aggregate(
        &candidates(4),
        vec![
            ballot(CouncilSeat::Artist, 1, &[4]),
            ballot(CouncilSeat::Lawyer, 1, &[4, 2]),
        ],
    );
    assert_eq!(r.condorcet_winner, Some(4));
}

/// Nothing in the recorded shape is a float, and the reasoning is not
/// in it — the seats' reasoning is stored once, in the rounds.
#[test]
fn recorded_shape_is_integers_only() {
    let r = AgendaRanking::aggregate(
        &candidates(2),
        vec![ballot(CouncilSeat::Artist, 1, &[2, 1])],
    );
    let json = serde_json::to_string(&r).unwrap();
    assert!(!json.contains('.'), "{json}");
    assert!(json.contains("\"seat\":\"artist\""), "{json}");
}
