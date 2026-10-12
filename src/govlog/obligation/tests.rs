use super::super::tests::{Chain, amd, at, gov, obl, rec, roots};
use super::super::{
    AmendmentDraft, Blind, KeyAnchor, blind_data, data_hash, redact_data,
    standing, verify_chain,
};
use super::*;
use crate::crypto::generate_keypair;
use crate::enums::Standing;
use serde_json::{Value, json};

fn date(s: &str) -> NaiveDate {
    s.parse().unwrap()
}

fn event() -> CalendarEventId {
    "6dcef9bb-2b3c-4f5e-9a1b-0c2d3e4f5a6b".parse().unwrap()
}

/// One `Until` of every variant. The `match` is exhaustive, so a new
/// variant does not compile until it is listed here; `every_variant` then
/// checks the list against the variants serde knows.
fn untils() -> Vec<Until> {
    let all = vec![
        Until::NextSitting,
        Until::Sitting { event: event() },
        Until::Date {
            date: date("2026-10-24"),
        },
        Until::AfterDecision { decision: gov(12) },
    ];
    for until in &all {
        match until {
            Until::NextSitting
            | Until::Sitting { .. }
            | Until::Date { .. }
            | Until::AfterDecision { .. } => {}
        }
    }
    all
}

/// The `when` tag of each variant, from serde itself: the error for an
/// unknown tag lists every variant the type has
fn every_variant() -> Vec<String> {
    let err = serde_json::from_value::<Until>(json!({"when": "?"}))
        .unwrap_err()
        .to_string();
    let list = err
        .split("expected one of ")
        .nth(1)
        .unwrap_or_else(|| panic!("serde's unknown-variant error: {err}"));
    list.split(", ")
        .map(|v| v.trim().trim_matches('`').to_string())
        .collect()
}

fn tag(until: &Until) -> String {
    serde_json::to_value(until).unwrap()["when"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn the_samples_cover_every_variant() {
    let tags: Vec<String> = untils().iter().map(tag).collect();
    assert_eq!(tags, every_variant());
}

#[test]
fn every_until_round_trips() {
    for until in untils() {
        let value = serde_json::to_value(&until).unwrap();
        assert_eq!(serde_json::from_value::<Until>(value).unwrap(), until);
    }
    assert_eq!(
        serde_json::to_value(Until::NextSitting).unwrap(),
        json!({"when": "next_sitting"})
    );
    // A model's lenient citation is read canonically.
    let read: Until = serde_json::from_value(
        json!({"when": "after_decision", "decision": "gov-2026-12"}),
    )
    .unwrap();
    assert_eq!(read, Until::AfterDecision { decision: gov(12) });
    // A date is YYYY-MM-DD and nothing chrono would also read.
    for loose in ["2027-4-1", "+2027-04-01", " 2027-04-01", "2027-02-30"] {
        assert!(
            serde_json::from_value::<Until>(
                json!({"when": "date", "date": loose})
            )
            .is_err(),
            "{loose}"
        );
    }
    // Closed: a stray field is an error, not silently dropped. (Except
    // beside `next_sitting`: serde does not deny unknown fields on a unit
    // variant of an internally tagged enum. The schema does.)
    assert!(
        serde_json::from_value::<Until>(
            json!({"when": "date", "date": "2026-10-24", "event": event()})
        )
        .is_err()
    );
}

/// Whether `value` conforms to `schema`, for the keywords these schemas
/// use. No JSON Schema validator is a dependency, so this is a small one.
#[cfg(feature = "schemars")]
fn conforms(value: &Value, schema: &Value) -> Result<(), String> {
    if let Some(branches) = schema.get("anyOf").and_then(Value::as_array) {
        let matched = branches
            .iter()
            .filter(|b| conforms(value, b).is_ok())
            .count();
        return match matched {
            1 => Ok(()),
            n => Err(format!("{value} matches {n} branches of {schema}")),
        };
    }
    if let Some(allowed) = schema.get("enum").and_then(Value::as_array)
        && !allowed.contains(value)
    {
        return Err(format!("{value} is not one of {allowed:?}"));
    }
    match schema.get("type").and_then(Value::as_str) {
        Some("object") => {
            let object = value.as_object().ok_or("not an object")?;
            let properties = schema["properties"].as_object().unwrap();
            for required in schema["required"].as_array().unwrap() {
                if !object.contains_key(required.as_str().unwrap()) {
                    return Err(format!("{value} lacks {required}"));
                }
            }
            for (key, v) in object {
                match properties.get(key) {
                    Some(s) => conforms(v, s)?,
                    None if schema["additionalProperties"] == false => {
                        return Err(format!("{value} has {key}"));
                    }
                    None => {}
                }
            }
            Ok(())
        }
        Some("string") => {
            let s = value.as_str().ok_or(format!("{value} is not a string"))?;
            match schema.get("format").and_then(Value::as_str) {
                Some("uuid") => s
                    .parse::<uuid::Uuid>()
                    .map(|_| ())
                    .map_err(|e| e.to_string()),
                Some("date") => s
                    .parse::<NaiveDate>()
                    .map(|_| ())
                    .map_err(|e| e.to_string()),
                Some(other) => Err(format!("unchecked format {other}")),
                None => Ok(()),
            }
        }
        Some("null") => value
            .is_null()
            .then_some(())
            .ok_or(format!("{value} is not null")),
        Some(other) => Err(format!("unchecked type {other}")),
        None => Ok(()),
    }
}

/// Strict-safe (agora CLAUDE.md), one closed branch per variant, and
/// every variant's JSON valid against it: a variant without a branch, or a
/// branch the type no longer reads, fails here
#[cfg(feature = "schemars")]
#[test]
fn the_until_schema_is_strict_safe_and_matches_the_type() {
    let schema = crate::responses::inline_schema_for::<Until>();
    assert_eq!(
        crate::tools::strict_schema_violations(&schema),
        Vec::<String>::new(),
        "{schema}"
    );
    let text = schema.to_string();
    for banned in ["$ref", "$defs", "pattern", "oneOf"] {
        assert!(!text.contains(banned), "{banned}: {schema}");
    }
    let branches = schema["anyOf"].as_array().expect("an anyOf");
    let tags: Vec<&str> = branches
        .iter()
        .map(|b| {
            assert_eq!(b["additionalProperties"], false, "{b}");
            let tag = b["properties"]["when"]["enum"].as_array().unwrap();
            assert_eq!(tag.len(), 1, "{b}");
            tag[0].as_str().unwrap()
        })
        .collect();
    assert_eq!(tags, every_variant());
    for until in untils() {
        let value = serde_json::to_value(&until).unwrap();
        conforms(&value, &schema).unwrap();
    }
    // And the validator is not vacuous.
    for bad in [
        json!({"when": "date"}),
        json!({"when": "date", "date": "24 October"}),
        json!({"when": "sitting", "event": "not a uuid"}),
        json!({"when": "next_sitting", "event": event()}),
        json!({"when": "someday"}),
    ] {
        assert!(conforms(&bad, &schema).is_err(), "{bad}");
        if bad["when"] != "next_sitting" {
            assert!(serde_json::from_value::<Until>(bad).is_err());
        }
    }
}

/// As the Lawyer's response format will hold it: optional, inline, still
/// strict-safe
#[cfg(feature = "schemars")]
#[test]
fn an_optional_until_in_a_closed_object_is_strict_safe() {
    #[derive(schemars::JsonSchema, Serialize)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    struct Format {
        kind: ObligationKind,
        owed_by: OwedBy,
        until: Option<Until>,
        text: String,
    }
    let schema = crate::responses::inline_schema_for::<Format>();
    assert_eq!(
        crate::tools::strict_schema_violations(&schema),
        Vec::<String>::new(),
        "{schema}"
    );
    for until in untils().into_iter().map(Some).chain([None]) {
        let value = serde_json::to_value(Format {
            kind: ObligationKind::Review,
            owed_by: OwedBy::Council,
            until,
            text: "t".into(),
        })
        .unwrap();
        conforms(&value["until"], &schema["properties"]["until"]).unwrap();
    }
}

#[cfg(feature = "schemars")]
#[test]
fn obligation_schemas_are_ref_free() {
    use crate::responses::inline_schema_for;
    for (name, schema) in [
        ("Obligation", inline_schema_for::<Obligation>()),
        ("ObligationUpdate", inline_schema_for::<ObligationUpdate>()),
        ("Evidence", inline_schema_for::<Evidence>()),
        ("ObligationState", inline_schema_for::<ObligationState>()),
    ] {
        let text = schema.to_string();
        assert!(!text.contains("$ref"), "{name}: {text}");
        assert!(!text.contains("$defs"), "{name}: {text}");
    }
}

fn review() -> Obligation {
    let mut o = Obligation::new(
        ObligationKind::Review,
        OwedBy::Council,
        "A six-month review of optional signatures",
        "we will review this in six months",
        "round 3, Lawyer",
    );
    o.source = Some(gov(7));
    o.until = Some(Until::Date {
        date: date("2027-04-01"),
    });
    o
}

#[test]
fn an_obligation_round_trips_and_leaves_out_what_is_absent() {
    let o = review();
    let value = serde_json::to_value(&o).unwrap();
    assert_eq!(value["agora_obligation"], 1);
    assert_eq!(
        value["until"],
        json!({"when": "date", "date": "2027-04-01"})
    );
    assert_eq!(serde_json::from_value::<Obligation>(value).unwrap(), o);

    let bare = Obligation::new(
        ObligationKind::Task,
        OwedBy::Staff,
        "t",
        "q",
        "post 91e791c0",
    );
    let value = serde_json::to_value(&bare).unwrap();
    let keys: Vec<&str> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        [
            "agora_obligation",
            "kind",
            "owed_by",
            "quote",
            "quote_location",
            "text"
        ]
    );
}

/// Its quote may hold an agent's words: it is stored blinded, the quote
/// can be redacted, and what is left still reads as an obligation
#[test]
fn a_stored_and_redacted_obligation_still_reads() {
    use super::super::{is_redactable, is_revisable};
    assert!(is_redactable(GovernanceLogEntryType::Obligation));
    assert!(is_revisable(GovernanceLogEntryType::Obligation));
    let stored =
        blind_data(&serde_json::to_value(review()).unwrap(), Blind::random())
            .unwrap();
    assert_eq!(
        serde_json::from_value::<Obligation>(stored.clone()).unwrap(),
        review()
    );
    let redacted =
        redact_data(&stored, &["/quote".into()], &amd(9), Blind::random())
            .unwrap();
    let read: Obligation = serde_json::from_value(redacted).unwrap();
    assert_eq!(read.quote, "[redacted by AMD-2026-0009]");
    assert_eq!(read.text, review().text);
}

// -- amendments and state --

fn update(target: &GovernanceLogId, update: ObligationUpdate) -> Amendment {
    AmendmentDraft::obligation(
        target.clone(),
        data_hash(&json!("x")),
        update,
        "b",
        "n",
    )
    .amendment
}

fn plan(n: u128) -> PostId {
    uuid::Uuid::from_u128(n).into()
}

fn deploy() -> Evidence {
    Evidence::Deploy {
        date: date("2026-10-20"),
        note: "the credit ledger".into(),
    }
}

#[test]
fn update_and_kind_must_agree() {
    use AmendmentError::*;
    let target = obl(1);
    for u in [
        ObligationUpdate::Planned { plan: plan(1) },
        ObligationUpdate::Discharged { evidence: deploy() },
        ObligationUpdate::Lapsed,
        ObligationUpdate::Rescheduled { until: None },
        ObligationUpdate::Withdrawn,
    ] {
        let a = update(&target, u.clone());
        assert_eq!(a.kind, u.kind());
        assert!(a.kind.is_obligation());
        assert_eq!(a.validate(), Ok(()));
        let value = serde_json::to_value(&a).unwrap();
        assert_eq!(value["update"]["kind"], value["kind"], "{value}");
        assert_eq!(serde_json::from_value::<Amendment>(value).unwrap(), a);
        assert_eq!(
            AmendmentDraft::new(
                target.clone(),
                data_hash(&json!("x")),
                u.kind(),
                "b",
                "n"
            ),
            Err(MissingUpdate(u.kind()))
        );
    }
    let mut a = update(&target, ObligationUpdate::Lapsed);
    a.kind = AmendmentKind::Withdrawn;
    assert_eq!(
        a.validate(),
        Err(UpdateMismatch {
            kind: AmendmentKind::Withdrawn,
            update: AmendmentKind::Lapsed
        })
    );
    a.kind = AmendmentKind::Correction;
    assert_eq!(a.validate(), Err(UnexpectedUpdate));
    a.kind = AmendmentKind::Lapsed;
    a.update = None;
    assert_eq!(a.validate(), Err(MissingUpdate(AmendmentKind::Lapsed)));
}

#[test]
fn obligation_kinds_do_not_change_standing() {
    for kind in AmendmentKind::OBLIGATION {
        assert_eq!(super::super::kind_standing(kind), None);
    }
    use AmendmentKind::*;
    assert_eq!(
        standing([NonPrecedential, Planned, Lapsed, Discharged]),
        Standing::NonPrecedential
    );
    assert_eq!(standing([Withdrawn, Rescheduled]), Standing::InForce);
}

fn state_after(
    o: &Obligation,
    updates: &[(i64, ObligationUpdate)],
) -> ObligationState {
    let amendments: Vec<(DateTime<Utc>, Amendment)> = updates
        .iter()
        .map(|(t, u)| (at(*t), update(&obl(1), u.clone())))
        .collect();
    ObligationState::from_amendments(o, amendments.iter().map(|(t, a)| (*t, a)))
}

#[test]
fn a_late_discharge_keeps_its_lapse() {
    let state = state_after(
        &review(),
        &[
            (10, ObligationUpdate::Lapsed),
            (20, ObligationUpdate::Discharged { evidence: deploy() }),
        ],
    );
    assert_eq!(state.status, ObligationStatus::Discharged);
    assert_eq!(state.lapsed_at, Some(at(10)));
    assert_eq!(state.until, review().until);
    // A lapse after it is closed records the date and moves nothing.
    let state = state_after(
        &review(),
        &[
            (10, ObligationUpdate::Withdrawn),
            (20, ObligationUpdate::Lapsed),
        ],
    );
    assert_eq!(state.status, ObligationStatus::Withdrawn);
}

#[test]
fn a_reschedule_reopens_a_lapsed_obligation() {
    let later = Until::Date {
        date: date("2027-06-01"),
    };
    let state = state_after(
        &review(),
        &[
            (10, ObligationUpdate::Lapsed),
            (
                20,
                ObligationUpdate::Rescheduled {
                    until: Some(later.clone()),
                },
            ),
        ],
    );
    assert_eq!(state.status, ObligationStatus::Open);
    assert_eq!(state.until, Some(later));
    assert_eq!(state.lapsed_at, Some(at(10)), "the lapse stays on record");
    // Lapsing again moves the date.
    let state = state_after(
        &review(),
        &[
            (10, ObligationUpdate::Lapsed),
            (20, ObligationUpdate::Rescheduled { until: None }),
            (30, ObligationUpdate::Lapsed),
        ],
    );
    assert_eq!(state.status, ObligationStatus::Lapsed);
    assert_eq!(state.lapsed_at, Some(at(30)));
    assert_eq!(state.until, None);
}

#[test]
fn a_later_plan_replaces_an_earlier_one() {
    let state = state_after(
        &review(),
        &[
            (10, ObligationUpdate::Planned { plan: plan(1) }),
            (20, ObligationUpdate::Planned { plan: plan(2) }),
        ],
    );
    assert_eq!(state.plan, Some(plan(2)));
    assert_eq!(state.status, ObligationStatus::Open);
    assert_eq!(
        ObligationState::from_amendments(&review(), []),
        ObligationState {
            until: review().until,
            ..Default::default()
        }
    );
}

// -- the verifier --

/// A chain with a decision (GOV-2026-0001) and an obligation it created
/// (OBL-2026-0001), signed by `key`
fn with_obligation(key: &crate::crypto::SigningKey) -> Chain {
    let mut c = Chain::new();
    c.decision(key);
    let mut o = review();
    o.source = Some(gov(1));
    c.push(
        key,
        obl(1),
        GovernanceLogEntryType::Obligation,
        serde_json::to_value(o).unwrap(),
        false,
    );
    c
}

#[test]
fn obligation_kinds_amend_an_obligation_and_nothing_else() {
    let (key, pk) = generate_keypair();
    let anchor = KeyAnchor::pinned((&pk).into());
    let mut c = with_obligation(&key);
    for u in [
        ObligationUpdate::Planned { plan: plan(1) },
        ObligationUpdate::Lapsed,
        ObligationUpdate::Rescheduled {
            until: Some(Until::NextSitting),
        },
        ObligationUpdate::Discharged {
            evidence: Evidence::Governance { id: gov(1) },
        },
    ] {
        let draft =
            AmendmentDraft::obligation(obl(1), c.hash_at(2), u, "b", "n");
        c.amend(&key, &draft);
    }
    // And the ordinary kinds still do too.
    let correction = AmendmentDraft::new(
        obl(1),
        c.hash_at(2),
        AmendmentKind::Correction,
        "clerical",
        "location corrected",
    )
    .unwrap();
    c.amend(&key, &correction);
    let v = verify_chain(&c.links, &pk, &anchor, &roots());
    assert!(v.ok, "{v:#?}");
    assert_eq!(v.entries[1].amended_by.len(), 5);

    for u in [
        ObligationUpdate::Lapsed,
        ObligationUpdate::Discharged { evidence: deploy() },
    ] {
        let mut c = with_obligation(&key);
        let draft =
            AmendmentDraft::obligation(gov(1), c.hash_at(1), u, "b", "n");
        c.amend(&key, &draft);
        let v = verify_chain(&c.links, &pk, &anchor, &roots());
        assert!(!v.ok);
        let problem = v.entries[2].problem.clone().unwrap_or_default();
        assert!(problem.contains("only valid on an obligation"), "{problem}");
        assert!(v.entries[0].amended_by.is_empty(), "and it takes no effect");
    }
}

#[test]
fn an_update_that_disagrees_with_its_kind_fails_verification() {
    let (key, pk) = generate_keypair();
    let anchor = KeyAnchor::pinned((&pk).into());
    let amend_with = |mutate: &dyn Fn(&mut Amendment)| -> String {
        let mut c = with_obligation(&key);
        let mut draft = AmendmentDraft::obligation(
            obl(1),
            c.hash_at(2),
            ObligationUpdate::Lapsed,
            "b",
            "n",
        );
        mutate(&mut draft.amendment);
        c.amend(&key, &draft);
        let v = verify_chain(&c.links, &pk, &anchor, &roots());
        assert!(!v.ok, "{v:#?}");
        v.entries[2].problem.clone().unwrap_or_default()
    };
    let p = amend_with(&|a| a.kind = AmendmentKind::Withdrawn);
    assert!(
        p.contains("of kind `lapsed` on an amendment of kind"),
        "{p}"
    );
    let p = amend_with(&|a| a.update = None);
    assert!(p.contains("requires an `update`"), "{p}");
    let p = amend_with(&|a| a.kind = AmendmentKind::Correction);
    assert!(p.contains("only valid on the obligation kinds"), "{p}");
}

#[test]
fn an_obligation_lives_in_the_obl_series() {
    let (key, pk) = generate_keypair();
    let anchor = KeyAnchor::pinned((&pk).into());
    let mut c = Chain::new();
    c.push(
        &key,
        rec(1),
        GovernanceLogEntryType::Obligation,
        serde_json::to_value(review()).unwrap(),
        false,
    );
    c.push(
        &key,
        obl(1),
        GovernanceLogEntryType::CouncilDecision,
        json!({"title": "a decision wearing an OBL- id"}),
        false,
    );
    let v = verify_chain(&c.links, &pk, &anchor, &roots());
    assert!(!v.ok);
    let p = v.entries[0].problem.clone().unwrap_or_default();
    assert!(p.contains("must be in the OBL- series"), "{p}");
    let p = v.entries[1].problem.clone().unwrap_or_default();
    assert!(p.contains("OBL- ids are reserved for obligation"), "{p}");
}

// -- the append rules --

/// GOV-1 a Council decision at seq 1, GOV-2 a Council decision at seq 5,
/// GOV-3 a policy change at seq 6, REC-1 a Steward's record at seq 2
fn known(id: &GovernanceLogId) -> Option<KnownEntry> {
    use GovernanceLogEntryType::*;
    let (entry_type, chain_seq) = match id.as_str() {
        "GOV-2026-0001" => (CouncilDecision, 1),
        "GOV-2026-0002" => (CouncilDecision, 5),
        "GOV-2026-0003" => (PolicyChange, 6),
        "REC-2026-0001" => (StewardRecord, 2),
        _ => return None,
    };
    Some(KnownEntry {
        entry_type,
        chain_seq,
    })
}

fn check(
    o: &Obligation,
    state: &ObligationState,
    u: ObligationUpdate,
    authority: Option<GovernanceLogId>,
) -> Result<(), ObligationRuleError> {
    let mut a = update(&obl(1), u);
    a.authority = authority;
    check_obligation_amendment(o, state, &a, known)
}

fn open(o: &Obligation) -> ObligationState {
    ObligationState::from_amendments(o, [])
}

#[test]
fn a_council_obligation_moves_only_with_a_later_council_decision() {
    use ObligationRuleError::*;
    let o = Obligation {
        source: Some(gov(1)),
        ..review()
    };
    let s = open(&o);
    let resched = || ObligationUpdate::Rescheduled {
        until: Some(Until::NextSitting),
    };
    assert_eq!(
        check(&o, &s, resched(), None),
        Err(NeedsCouncil(AmendmentKind::Rescheduled))
    );
    assert_eq!(
        check(&o, &s, resched(), Some(gov(3))),
        Err(NeedsCouncil(AmendmentKind::Rescheduled)),
        "a policy change is not a Council decision"
    );
    assert_eq!(
        check(&o, &s, resched(), Some(gov(1))),
        Err(AuthorityNotLater {
            authority: gov(1),
            earlier: gov(1)
        })
    );
    assert_eq!(check(&o, &s, resched(), Some(gov(2))), Ok(()));
    assert_eq!(
        check(&o, &s, ObligationUpdate::Withdrawn, Some(gov(2))),
        Ok(())
    );
    assert_eq!(
        check(&o, &s, ObligationUpdate::Withdrawn, Some(gov(9))),
        Err(UnknownEntry(gov(9)))
    );
    // Council-owed: a discharge needs a Council decision too.
    let done = || ObligationUpdate::Discharged {
        evidence: Evidence::Governance { id: gov(2) },
    };
    assert_eq!(
        check(&o, &s, done(), None),
        Err(NeedsCouncil(AmendmentKind::Discharged))
    );
    assert_eq!(check(&o, &s, done(), Some(gov(2))), Ok(()));
    // Anyone may plan it.
    assert_eq!(
        check(&o, &s, ObligationUpdate::Planned { plan: plan(1) }, None),
        Ok(())
    );
}

#[test]
fn the_steward_moves_only_deadlines_the_steward_set() {
    use ObligationRuleError::*;
    let o = Obligation {
        source: Some(rec(1)),
        owed_by: OwedBy::Staff,
        ..review()
    };
    let s = open(&o);
    let resched = || ObligationUpdate::Rescheduled {
        until: Some(Until::NextSitting),
    };
    assert_eq!(check(&o, &s, resched(), None), Ok(()));
    assert_eq!(check(&o, &s, ObligationUpdate::Withdrawn, None), Ok(()));
    assert_eq!(
        check(
            &o,
            &s,
            ObligationUpdate::Discharged {
                evidence: Evidence::Commit {
                    repo: "mdegans/agora".into(),
                    sha: "58e5540".into()
                }
            },
            None
        ),
        Ok(())
    );
    // Once the Council has set its deadline, it is the Council's.
    let mut by_council = update(&obl(1), resched());
    by_council.authority = Some(gov(1));
    let s = ObligationState::from_amendments(&o, [(at(1), &by_council)]);
    assert_eq!(s.until_authority, Some(gov(1)));
    assert_eq!(
        check(&o, &s, resched(), None),
        Err(NeedsCouncil(AmendmentKind::Rescheduled))
    );
    assert_eq!(check(&o, &s, resched(), Some(gov(2))), Ok(()));
    // A top-level Council-owed obligation is released only by the Council.
    let o = Obligation {
        source: None,
        owed_by: OwedBy::Council,
        ..review()
    };
    assert_eq!(check(&o, &open(&o), resched(), None), Ok(()));
    assert_eq!(
        check(&o, &open(&o), ObligationUpdate::Withdrawn, None),
        Err(NeedsCouncil(AmendmentKind::Withdrawn))
    );
}

#[test]
fn lapses_closures_and_evidence_are_checked() {
    use ObligationRuleError::*;
    let o = Obligation {
        source: Some(rec(1)),
        owed_by: OwedBy::Steward,
        ..review()
    };
    assert_eq!(check(&o, &open(&o), ObligationUpdate::Lapsed, None), Ok(()));
    let lapsed = state_after(&o, &[(1, ObligationUpdate::Lapsed)]);
    assert_eq!(
        check(&o, &lapsed, ObligationUpdate::Lapsed, None),
        Err(AlreadyLapsed)
    );
    let undated = Obligation {
        until: None,
        ..o.clone()
    };
    assert_eq!(
        check(&undated, &open(&undated), ObligationUpdate::Lapsed, None),
        Err(Undated)
    );
    let closed = state_after(&o, &[(1, ObligationUpdate::Withdrawn)]);
    assert_eq!(
        check(
            &o,
            &closed,
            ObligationUpdate::Planned { plan: plan(1) },
            None
        ),
        Err(Closed(ObligationStatus::Withdrawn))
    );
    let discharge = |evidence| ObligationUpdate::Discharged { evidence };
    assert_eq!(
        check(
            &o,
            &open(&o),
            discharge(Evidence::Commit {
                repo: "mdegans/agora".into(),
                sha: "xyz".into()
            }),
            None
        ),
        Err(BadSha("xyz".into()))
    );
    assert_eq!(
        check(
            &o,
            &open(&o),
            discharge(Evidence::PullRequest {
                repo: "agora".into(),
                number: 706
            }),
            None
        ),
        Err(BadRepo("agora".into()))
    );
    assert_eq!(
        check(
            &o,
            &open(&o),
            discharge(Evidence::Governance { id: gov(8) }),
            None
        ),
        Err(UnknownEntry(gov(8)))
    );
    // A mismatched amendment is refused before any rule is read.
    let mut a = update(&obl(1), ObligationUpdate::Lapsed);
    a.kind = AmendmentKind::Planned;
    assert!(matches!(
        check_obligation_amendment(&o, &open(&o), &a, known),
        Err(Shape(AmendmentError::UpdateMismatch { .. }))
    ));
    // Other kinds are not this function's business.
    let correction = AmendmentDraft::new(
        obl(1),
        data_hash(&json!("x")),
        AmendmentKind::Correction,
        "b",
        "n",
    )
    .unwrap()
    .amendment;
    assert_eq!(
        check_obligation_amendment(&o, &closed, &correction, known),
        Ok(())
    );
}
