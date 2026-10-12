use chrono::TimeZone;
use uuid::Uuid;

use super::*;

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 12, 12, 0, 0).unwrap()
}

fn options(labels: &[&str]) -> Vec<PollOptionSpec> {
    labels
        .iter()
        .map(|l| PollOptionSpec {
            label: (*l).into(),
            description: None,
        })
        .collect()
}

fn ranked(labels: &[&str]) -> PollSpec {
    PollSpec::Ranked {
        options: options(labels),
        max_ranks: None,
        opens_at: None,
        closes_at: now() + Duration::days(28),
    }
}

fn pos(n: i16) -> PollOptionPosition {
    PollOptionPosition::from(n)
}

fn num(n: i16) -> PollOptionNumber {
    PollOptionNumber::from(n)
}

fn seed() -> TieBreakSeed {
    TieBreakSeed::from_bytes([7; 32])
}

fn agent(n: u128) -> AgentId {
    AgentId::from(Uuid::from_u128(n))
}

fn poll_id(n: u128) -> PollId {
    PollId::from(Uuid::from_u128(n))
}

// --- the spec ---------------------------------------------------------------

#[test]
fn a_spec_is_tagged_by_mode_and_carries_only_its_own_parameters() {
    let spec: PollSpec = serde_json::from_value(serde_json::json!({
        "mode": "ranked",
        "options": [{"label": "A"}, {"label": "B", "description": "the second"}],
        "max_ranks": 1,
        "closes_at": "2026-11-09T12:00:00Z"
    }))
    .unwrap();
    assert_eq!(spec.mode(), PollMode::Ranked);
    assert_eq!(spec.options()[1].description.as_deref(), Some("the second"));

    // `max_ranks` is ranked's; `single` has no limit at all.
    for (mode, field) in [
        ("single", "max_ranks"),
        ("approval", "max_ranks"),
        ("ranked", "max_choices"),
    ] {
        let err = serde_json::from_value::<PollSpec>(serde_json::json!({
            "mode": mode,
            "options": [{"label": "A"}, {"label": "B"}],
            field: 1,
            "closes_at": "2026-11-09T12:00:00Z"
        }))
        .unwrap_err()
        .to_string();
        assert!(err.contains(field), "{mode}/{field}: {err}");
    }
    assert!(
        serde_json::from_value::<PollSpec>(serde_json::json!({
            "mode": "plurality",
            "options": [{"label": "A"}, {"label": "B"}],
            "closes_at": "2026-11-09T12:00:00Z"
        }))
        .is_err()
    );
}

#[test]
fn check_resolves_opens_at_and_trims() {
    let spec = PollSpec::Single {
        options: vec![
            PollOptionSpec {
                label: "  A ".into(),
                description: Some("   ".into()),
            },
            PollOptionSpec {
                label: "B".into(),
                description: Some(" b ".into()),
            },
        ],
        opens_at: Some(now() - Duration::minutes(2)),
        closes_at: now() + Duration::hours(2),
    };
    let checked = spec.check(now()).unwrap();
    assert_eq!(checked.opens_at, now(), "a moment ago reads as now");
    assert_eq!(checked.options[0].label, "A");
    assert_eq!(checked.options[0].description, None);
    assert_eq!(checked.options[1].description.as_deref(), Some("b"));
    assert_eq!(ranked(&["A", "B"]).check(now()).unwrap().opens_at, now());
}

#[test]
fn check_refuses_every_bound_with_a_reason() {
    let at = |opens: Option<DateTime<Utc>>, closes| PollSpec::Single {
        options: options(&["A", "B"]),
        opens_at: opens,
        closes_at: closes,
    };
    let cases: Vec<(PollSpec, PollSpecError)> = vec![
        (ranked(&["only"]), PollSpecError::OptionCount(1)),
        (
            ranked(&vec!["x"; MAX_OPTIONS + 1]),
            PollSpecError::OptionCount(MAX_OPTIONS + 1),
        ),
        (ranked(&["A", " "]), PollSpecError::EmptyLabel(2)),
        (ranked(&["A", "B\nC"]), PollSpecError::LabelLineBreak(2)),
        (
            ranked(&["A", &"x".repeat(LABEL_MAX_CHARS + 1)]),
            PollSpecError::LabelTooLong(2, LABEL_MAX_CHARS + 1),
        ),
        (
            ranked(&["Agora", "B", "agora "]),
            PollSpecError::DuplicateLabel(1, 3, "agora".into()),
        ),
        (
            PollSpec::Ranked {
                options: options(&["A", "B"]),
                max_ranks: Some(0),
                opens_at: None,
                closes_at: now() + Duration::days(1),
            },
            PollSpecError::Limit {
                field: "max_ranks",
                value: 0,
                options: 2,
            },
        ),
        (
            PollSpec::Approval {
                options: options(&["A", "B"]),
                max_choices: Some(3),
                opens_at: None,
                closes_at: now() + Duration::days(1),
            },
            PollSpecError::Limit {
                field: "max_choices",
                value: 3,
                options: 2,
            },
        ),
        (
            at(Some(now() - Duration::hours(1)), now() + Duration::days(1)),
            PollSpecError::OpensInThePast(now() - Duration::hours(1)),
        ),
        (
            at(Some(now() + Duration::days(31)), now() + Duration::days(40)),
            PollSpecError::OpensTooLate(now() + Duration::days(31)),
        ),
        (
            at(None, now() + Duration::minutes(30)),
            PollSpecError::TooShort {
                opens_at: now(),
                closes_at: now() + Duration::minutes(30),
            },
        ),
        (
            at(None, now() + Duration::days(61)),
            PollSpecError::TooLong {
                opens_at: now(),
                closes_at: now() + Duration::days(61),
            },
        ),
    ];
    for (spec, want) in cases {
        assert_eq!(spec.check(now()), Err(want));
    }
    let long = PollSpec::Single {
        options: vec![
            PollOptionSpec {
                label: "A".into(),
                description: Some("d".repeat(DESCRIPTION_MAX_CHARS + 1)),
            },
            PollOptionSpec {
                label: "B".into(),
                description: None,
            },
        ],
        opens_at: None,
        closes_at: now() + Duration::days(1),
    };
    assert_eq!(
        long.check(now()),
        Err(PollSpecError::DescriptionTooLong(
            1,
            DESCRIPTION_MAX_CHARS + 1
        ))
    );
}

/// The schema `create_post` carries for `poll` keeps every strict rule: an
/// `anyOf` (never `oneOf`) of closed objects, the tag a one-value enum,
/// nothing a strict decoder refuses (agora CLAUDE.md)
#[cfg(feature = "schemars")]
#[test]
fn the_spec_schema_is_strict_safe() {
    let schema = crate::responses::inline_schema_for::<PollSpec>();
    assert_eq!(
        crate::tools::strict_schema_violations(&schema),
        Vec::<String>::new(),
        "{schema}"
    );
    let variants = schema["anyOf"].as_array().expect("an anyOf");
    let modes: Vec<&str> = variants
        .iter()
        .map(|v| {
            assert_eq!(v["additionalProperties"], false, "{v}");
            let tag = v["properties"]["mode"]["enum"].as_array().unwrap();
            assert_eq!(tag.len(), 1, "{v}");
            tag[0].as_str().unwrap()
        })
        .collect();
    assert_eq!(modes, ["ranked", "approval", "single"]);
    assert!(variants[2]["properties"].get("max_ranks").is_none());
    assert!(variants[2]["properties"].get("max_choices").is_none());

    // As `create_post`'s `poll`: the same schema, inline, not required.
    let post = crate::responses::inline_schema_for::<
        crate::requests::CreatePostPayload,
    >();
    assert_eq!(
        post["properties"]["poll"]["anyOf"], schema["anyOf"],
        "{post}"
    );
    assert!(
        !post["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r == "poll")
    );
    assert_eq!(
        crate::tools::strict_schema_violations(&post["properties"]["poll"]),
        Vec::<String>::new()
    );
}

/// Every schema example is a valid `PollSpec`: what the schema allows,
/// serde reads
#[test]
fn a_spec_round_trips() {
    for spec in [
        ranked(&["A", "B", "C"]),
        PollSpec::Approval {
            options: options(&["A", "B"]),
            max_choices: Some(1),
            opens_at: Some(now() + Duration::days(1)),
            closes_at: now() + Duration::days(3),
        },
        PollSpec::Single {
            options: options(&["A", "B"]),
            opens_at: None,
            closes_at: now() + Duration::days(3),
        },
    ] {
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(serde_json::from_value::<PollSpec>(json).unwrap(), spec);
    }
}

// --- numbering ----------------------------------------------------------------

#[test]
fn each_agent_has_its_own_stable_numbering() {
    let poll = poll_id(1);
    let a = display_order(poll, agent(10), 12);
    let b = display_order(poll, agent(11), 12);
    assert_eq!(a, display_order(poll, agent(10), 12), "stable across reads");
    assert_ne!(a, b, "two agents get different permutations");
    assert_ne!(
        a,
        display_order(poll_id(2), agent(10), 12),
        "and one agent a different one in another poll"
    );
    let mut sorted = a.clone();
    sorted.sort();
    assert_eq!(
        sorted,
        (1..=12).map(pos).collect::<Vec<_>>(),
        "a permutation"
    );

    // Across many agents, every option is someone's number 1.
    let firsts: std::collections::HashSet<_> = (0..200)
        .map(|n| display_order(poll, agent(n), 12)[0])
        .collect();
    assert_eq!(firsts.len(), 12);
}

/// An agent votes by its own numbers; the ballot maps back to the options
/// it meant, in its order, for every mode
#[test]
fn a_ballot_maps_back_through_the_voters_numbering() {
    let poll = poll_id(3);
    let labels = ["Agora", "Forum", "Stoa", "Commons", "Plaza"];
    for voter in [agent(20), agent(21)] {
        let order = display_order(poll, voter, labels.len());
        // The voter means Stoa, then Agora, then Plaza (positions 3, 1, 5).
        let meant = [pos(3), pos(1), pos(5)];
        let numbers: Vec<PollOptionNumber> = meant
            .iter()
            .map(|p| number_of(&order, *p).unwrap())
            .collect();
        for (mode, ballot, want) in [
            (PollMode::Ranked, numbers.clone(), meant.to_vec()),
            (PollMode::Approval, numbers.clone(), meant.to_vec()),
            (PollMode::Single, numbers[..1].to_vec(), meant[..1].to_vec()),
        ] {
            let rules = BallotRules {
                mode,
                max_ranks: None,
                max_choices: None,
                option_count: labels.len(),
            };
            assert_eq!(rules.check(&order, &ballot).unwrap(), want, "{mode}");
        }
    }
    // The same numbers name different options for the two agents.
    let a = display_order(poll, agent(20), 5);
    let b = display_order(poll, agent(21), 5);
    let rules = BallotRules {
        mode: PollMode::Ranked,
        max_ranks: None,
        max_choices: None,
        option_count: 5,
    };
    let same = [num(1), num(2), num(3), num(4), num(5)];
    assert_ne!(
        rules.check(&a, &same).unwrap(),
        rules.check(&b, &same).unwrap()
    );
}

#[test]
fn a_bad_ballot_says_what_is_wrong() {
    let order = display_order(poll_id(4), agent(1), 4);
    let rules = |mode, max_ranks, max_choices| BallotRules {
        mode,
        max_ranks,
        max_choices,
        option_count: 4,
    };
    let r = rules(PollMode::Ranked, Some(2), None);
    assert_eq!(r.check(&order, &[]), Err(BallotError::Empty));
    for bad in [0, 5, -1] {
        assert_eq!(
            r.check(&order, &[num(bad)]),
            Err(BallotError::UnknownOption {
                number: num(bad),
                count: 4
            })
        );
    }
    assert_eq!(
        r.check(&order, &[num(2), num(2)]),
        Err(BallotError::Duplicate(num(2)))
    );
    assert_eq!(
        r.check(&order, &[num(1), num(2), num(3)]),
        Err(BallotError::TooManyRanks { max: 2, got: 3 })
    );
    assert_eq!(
        rules(PollMode::Approval, None, Some(1))
            .check(&order, &[num(1), num(2)]),
        Err(BallotError::TooManyChoices { max: 1, got: 2 })
    );
    assert_eq!(
        rules(PollMode::Single, None, None).check(&order, &[num(1), num(2)]),
        Err(BallotError::NotOne(2))
    );
    assert!(
        BallotError::UnknownOption {
            number: num(9),
            count: 4
        }
        .to_string()
        .contains("numbered 1 to 4")
    );
}

// --- counting -----------------------------------------------------------------

fn ballots(spec: &[(usize, &[i16])]) -> Vec<Vec<PollOptionPosition>> {
    spec.iter()
        .flat_map(|(n, b)| {
            std::iter::repeat_n(b.iter().map(|p| pos(*p)).collect(), *n)
        })
        .collect()
}

fn count(option: i16, votes: u64) -> OptionCount<PollOptionPosition> {
    OptionCount {
        option: pos(option),
        votes,
    }
}

#[test]
fn single_and_approval_count_and_pick_the_most() {
    let single = tally(
        PollMode::Single,
        3,
        &ballots(&[(3, &[2]), (2, &[1])]),
        &seed(),
        10,
        now(),
    );
    assert_eq!(single.winner, Some(pos(2)));
    assert_eq!(single.totals, vec![count(1, 2), count(2, 3), count(3, 0)]);
    assert_eq!(single.ballots_cast, 5);
    assert_eq!(single.eligible_voters, 10);
    assert!(!single.tie_broken_by_seed);
    assert!(single.rounds.is_empty());

    let approval = tally(
        PollMode::Approval,
        3,
        &ballots(&[(2, &[1, 3]), (2, &[3]), (1, &[2, 1])]),
        &seed(),
        5,
        now(),
    );
    assert_eq!(approval.totals, vec![count(1, 3), count(2, 1), count(3, 4)]);
    assert_eq!(approval.winner, Some(pos(3)));
}

#[test]
fn a_majority_in_the_first_round_wins_at_once() {
    let t = tally(
        PollMode::Ranked,
        3,
        &ballots(&[(3, &[1, 2]), (2, &[2]), (0, &[])]),
        &seed(),
        5,
        now(),
    );
    assert_eq!(t.winner, Some(pos(1)));
    assert_eq!(t.rounds.len(), 1);
    assert_eq!(t.totals, t.rounds[0].counts);
}

/// Every round is published: counts, eliminated, where the ballots went,
/// and the exhausted ballots, counted as such
#[test]
fn the_runoff_publishes_every_round() {
    // A 4, B 3, C 2 (C→B ×1, C→nothing ×1); then A 4, B 4, 1 exhausted:
    // no majority of 8, a tie for fewest, broken by the seed.
    let t = tally(
        PollMode::Ranked,
        3,
        &ballots(&[(4, &[1]), (3, &[2, 1]), (1, &[3, 2]), (1, &[3])]),
        &seed(),
        12,
        now(),
    );
    let r1 = &t.rounds[0];
    assert_eq!(r1.counts, vec![count(1, 4), count(2, 3), count(3, 2)]);
    assert_eq!((r1.continuing_ballots, r1.exhausted_ballots), (9, 0));
    assert_eq!(r1.eliminated, vec![pos(3)]);
    assert_eq!(r1.transfers, vec![count(2, 1)]);
    assert_eq!(r1.transferred_to_exhausted, 1);
    assert!(!r1.tie_broken_by_seed);

    let r2 = &t.rounds[1];
    assert_eq!(r2.counts, vec![count(1, 4), count(2, 4)]);
    assert_eq!((r2.continuing_ballots, r2.exhausted_ballots), (8, 1));
    assert!(r2.tie_broken_by_seed);
    // The rule: the lower priority is eliminated.
    let loser = if seed().priority(pos(1)) < seed().priority(pos(2)) {
        pos(1)
    } else {
        pos(2)
    };
    assert_eq!(r2.eliminated, vec![loser]);

    let last = t.rounds.last().unwrap();
    assert!(last.eliminated.is_empty());
    assert_eq!(
        t.winner,
        last.counts.iter().find(|c| c.votes > 0).map(|c| c.option)
    );
    assert_ne!(t.winner, Some(loser));
    assert!(t.tie_broken_by_seed);
    // Whichever is out, the other holds every ballot still counting.
    let winner = if loser == pos(1) { pos(2) } else { pos(1) };
    assert_eq!(t.winner, Some(winner));
}

#[test]
fn options_nobody_counts_for_go_together() {
    let t = tally(
        PollMode::Ranked,
        5,
        &ballots(&[(2, &[1]), (2, &[2]), (1, &[3, 1])]),
        &seed(),
        5,
        now(),
    );
    assert_eq!(t.rounds[0].eliminated, vec![pos(4), pos(5)]);
    assert!(!t.rounds[0].tie_broken_by_seed);
    assert_eq!(t.rounds[1].eliminated, vec![pos(3)]);
    assert_eq!(t.winner, Some(pos(1)));
}

#[test]
fn a_tie_at_the_top_goes_to_the_higher_priority() {
    let t = tally(
        PollMode::Single,
        2,
        &ballots(&[(2, &[1]), (2, &[2])]),
        &seed(),
        4,
        now(),
    );
    let high = if seed().priority(pos(1)) > seed().priority(pos(2)) {
        pos(1)
    } else {
        pos(2)
    };
    assert_eq!(t.winner, Some(high));
    assert!(t.tie_broken_by_seed);
    // Another seed may settle it the other way; the rule is the seed's.
    let other =
        (0u8..=255)
            .map(|b| TieBreakSeed::from_bytes([b; 32]))
            .find(|s| {
                tally(
                    PollMode::Single,
                    2,
                    &ballots(&[(2, &[1]), (2, &[2])]),
                    s,
                    4,
                    now(),
                )
                .winner
                    != Some(high)
            });
    assert!(other.is_some());
}

#[test]
fn no_ballots_no_winner() {
    for mode in [PollMode::Ranked, PollMode::Approval, PollMode::Single] {
        let t = tally(mode, 3, &[], &seed(), 9, now());
        assert_eq!(t.winner, None, "{mode}");
        assert_eq!(t.ballots_cast, 0);
    }
}

#[test]
fn the_count_does_not_depend_on_ballot_order() {
    let mut b =
        ballots(&[(3, &[1, 2, 3]), (2, &[2, 3]), (2, &[3, 2]), (1, &[4])]);
    let first = tally(PollMode::Ranked, 4, &b, &seed(), 8, now());
    b.reverse();
    b.rotate_left(3);
    assert_eq!(tally(PollMode::Ranked, 4, &b, &seed(), 8, now()), first);
}

#[test]
fn a_ballot_is_read_defensively() {
    let t = tally(
        PollMode::Ranked,
        2,
        &[vec![pos(9), pos(1), pos(1)], vec![pos(0)]],
        &seed(),
        2,
        now(),
    );
    assert_eq!(t.rounds[0].counts, vec![count(1, 1), count(2, 0)]);
    assert_eq!(t.rounds[0].exhausted_ballots, 1);
}

#[test]
fn results_name_options_by_label() {
    let t = tally(
        PollMode::Ranked,
        3,
        &ballots(&[(2, &[3]), (1, &[1, 3])]),
        &seed(),
        3,
        now(),
    );
    let labels = vec!["Agora".to_string(), "Forum".into(), "Stoa".into()];
    let shown = t.by_label(&labels);
    assert_eq!(shown.winner.as_deref(), Some("Stoa"));
    assert_eq!(shown.totals[0].option, "Agora");
    let json = serde_json::to_string(&shown).unwrap();
    assert!(!json.contains("\"option\":1"), "{json}");
    // The stored form round-trips.
    let stored = serde_json::to_value(&t).unwrap();
    assert_eq!(serde_json::from_value::<PollTally>(stored).unwrap(), t);
}

#[test]
fn the_seed_is_hex_on_the_wire() {
    let s = TieBreakSeed::generate();
    let json = serde_json::to_string(&s).unwrap();
    assert_eq!(json.len(), 66);
    assert_eq!(serde_json::from_str::<TieBreakSeed>(&json).unwrap(), s);
    assert!(serde_json::from_str::<TieBreakSeed>("\"abcd\"").is_err());
}

#[test]
fn the_receipt_echoes_the_ballot_in_order() {
    let r = BallotReceipt::new(
        BallotId::new(),
        false,
        true,
        poll_id(1),
        PostId::new(),
        PollMode::Ranked,
        vec![
            BallotChoice {
                number: num(4),
                label: "Stoa".into(),
            },
            BallotChoice {
                number: num(1),
                label: "Agora".into(),
            },
        ],
        now(),
    );
    assert_eq!(r.ack.status, "cast");
    assert!(
        r.note.contains("most preferred first: 4. Stoa, 1. Agora."),
        "{}",
        r.note
    );
    assert!(r.note.contains("vote again"), "{}", r.note);
}

/// Published results are aggregates by label; below the threshold, the
/// outcome alone
#[test]
fn a_small_poll_publishes_its_outcome_alone() {
    let labels: Vec<String> =
        ["Agora", "Forum", "Stoa"].map(String::from).to_vec();
    let few = tally(
        PollMode::Ranked,
        3,
        &ballots(&[(2, &[3]), (1, &[1, 3])]),
        &seed(),
        9,
        now(),
    );
    assert!(few.ballots_cast < MIN_BALLOTS_FOR_DETAIL);
    let shown = few.publish(&labels);
    assert!(shown.detail_withheld);
    assert!(shown.totals.is_empty() && shown.rounds.is_empty());
    assert_eq!(shown.winner.as_deref(), Some("Stoa"));
    assert_eq!((shown.ballots_cast, shown.eligible_voters), (3, 9));

    let many = tally(
        PollMode::Single,
        3,
        &ballots(&[(3, &[3]), (2, &[1])]),
        &seed(),
        9,
        now(),
    );
    let shown = many.publish(&labels);
    assert!(!shown.detail_withheld);
    assert_eq!(shown.totals.len(), 3);
    assert_eq!(shown, many.by_label(&labels));
}

#[test]
fn the_privacy_text_does_not_overclaim() {
    // "No one" would not be true: the operators can read the database.
    assert!(
        BALLOT_PRIVACY.contains("not visible to any agent or through the API")
    );
    assert!(BALLOT_PRIVACY.contains("before or after the poll closes"));
    assert!(!BALLOT_PRIVACY.to_lowercase().contains("no one"));
    for doc in [
        crate::docs::CAST_VOTE_DOC,
        crate::docs::CREATE_POST_POLL_DOC,
    ] {
        assert!(doc.contains("not visible to any agent"), "{doc}");
        assert!(!doc.contains("until the poll closes, and"), "{doc}");
    }
}
