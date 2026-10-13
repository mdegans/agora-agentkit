//! Aggregating the seats' rankings on a `Schedule` item (0.76, moved from
//! the Council program).
//!
//! Each seat ranks the docket by P-number (partial rankings allowed) and
//! says how many items fit this sitting. The aggregate is a Borda count on
//! a **fixed scale**: with `K` rankable candidates, a seat's 1st choice
//! scores `K`, its 2nd `K - 1`, … and anything it left out scores 0. The
//! scale is the docket's, not the ballot's, so a seat's first choice weighs
//! the same whether it ranked three items or thirty.
//!
//! Everything recorded is an integer: the governance log carries no floats
//! (a float's text form is not canonical, and the entry is signed). The
//! mean-position tie-break is therefore compared by cross-multiplying
//! position sums, and stored as the sum and the count.
//!
//! The pairwise report is a check on the Borda order, not a second rule:
//! with four voters a Borda tiebreak can hide the absence of any real
//! consensus, and the Steward should see that before executing the order.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use super::{AgendaRanking, Ballot, PlacedProposal, SeatRanking};
use crate::ids::PostId;

impl SeatRanking {
    /// "Ranking: P3, P1, P7 — this sitting can hear 2.", for the round
    /// record and the console
    pub fn render(&self) -> String {
        let list: Vec<String> =
            self.ranking.iter().map(|n| format!("P{n}")).collect();
        format!(
            "Ranking: {} — this sitting can hear {}.",
            list.join(", "),
            self.sitting_capacity
        )
    }
}

impl AgendaRanking {
    /// The chosen docket: the first `cut` entries of the order
    pub fn docket(&self) -> impl Iterator<Item = &PlacedProposal> {
        self.order.iter().take(self.cut as usize)
    }

    /// Aggregate validated ballots.
    ///
    /// `candidates` maps each rankable P-number to its proposal; the ballots
    /// are assumed validated against it (the `rank` method refuses unknown and
    /// duplicate numbers), and a number missing here is skipped rather than
    /// trusted.
    pub fn aggregate(
        candidates: &BTreeMap<u32, PostId>,
        ballots: Vec<Ballot>,
    ) -> Self {
        let k = candidates.len() as u32;

        #[derive(Default)]
        struct Tally {
            borda: u32,
            seats: u32,
            position_sum: u32,
        }
        let mut tallies: BTreeMap<u32, Tally> = BTreeMap::new();
        for ballot in &ballots {
            for (i, number) in ballot.ranking.iter().enumerate() {
                if !candidates.contains_key(number) {
                    continue;
                }
                let position = i as u32 + 1;
                let t = tallies.entry(*number).or_default();
                t.borda += k.saturating_sub(position) + 1;
                t.seats += 1;
                t.position_sum += position;
            }
        }

        // Best first: Borda, then more seats, then lower mean position
        // (a/b < c/d  ⇔  a·d < c·b, all positive), then P-number for a
        // stable, disclosed arbitrary order.
        let compare = |a: &(u32, &Tally), b: &(u32, &Tally)| -> Ordering {
            b.1.borda
                .cmp(&a.1.borda)
                .then(b.1.seats.cmp(&a.1.seats))
                .then(
                    (a.1.position_sum * b.1.seats)
                        .cmp(&(b.1.position_sum * a.1.seats)),
                )
        };
        let mut ranked: Vec<(u32, &Tally)> =
            tallies.iter().map(|(n, t)| (*n, t)).collect();
        ranked.sort_by(|a, b| compare(a, b).then(a.0.cmp(&b.0)));

        let order: Vec<PlacedProposal> = ranked
            .iter()
            .enumerate()
            .map(|(i, (number, t))| PlacedProposal {
                number: *number,
                post_id: candidates[number],
                borda: t.borda,
                seats: t.seats,
                position_sum: t.position_sum,
                tied_with_next: ranked.get(i + 1).is_some_and(|next| {
                    compare(&ranked[i], next) == Ordering::Equal
                }),
            })
            .collect();

        let mut capacities: Vec<u32> =
            ballots.iter().map(|b| b.sitting_capacity).collect();
        capacities.sort_unstable();
        let cut = capacities
            .get(capacities.len().saturating_sub(1) / 2)
            .copied()
            .unwrap_or(0);

        let contenders: Vec<u32> = order.iter().map(|p| p.number).collect();
        let condorcet_winner = condorcet_winner(&contenders, &ballots);

        Self {
            rankable: k,
            ballots,
            order,
            cut,
            condorcet_winner,
        }
    }
}

/// The candidate that a majority of expressed preferences favours over
/// every other, head to head. A seat prefers `a` to `b` when it ranked `a`
/// above `b`, or ranked `a` and left `b` out; a seat that ranked neither
/// expresses no preference.
fn condorcet_winner(contenders: &[u32], ballots: &[Ballot]) -> Option<u32> {
    let position =
        |ballot: &Ballot, n: u32| ballot.ranking.iter().position(|&x| x == n);
    let prefers = |a: u32, b: u32| -> u32 {
        ballots
            .iter()
            .filter(|ballot| match (position(ballot, a), position(ballot, b)) {
                (Some(pa), Some(pb)) => pa < pb,
                (Some(_), None) => true,
                (None, Some(_)) | (None, None) => false,
            })
            .count() as u32
    };
    contenders.iter().copied().find(|&a| {
        contenders
            .iter()
            .filter(|&&b| b != a)
            .all(|&b| prefers(a, b) > prefers(b, a))
    })
}

#[cfg(test)]
mod tests;
