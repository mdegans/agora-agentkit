use chrono::{Duration, TimeZone};

use super::*;
use crate::govlog::{
    AmendmentDraft, Evidence, Obligation, ObligationRecord, ObligationUpdate,
    Sha256Hex,
};

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 12, 12, 0, 0).unwrap()
}

fn days(n: i64) -> DateTime<Utc> {
    now() + Duration::days(n)
}

fn event_id(n: u8) -> CalendarEventId {
    format!("8a1f3c2e-0000-4000-8000-0000000000{n:02x}")
        .parse()
        .unwrap()
}

fn obl(n: u32) -> GovernanceLogId {
    format!("OBL-2026-{n:04}").parse().unwrap()
}

fn gov(n: u32) -> GovernanceLogId {
    format!("GOV-2026-{n:04}").parse().unwrap()
}

fn event(
    n: u8,
    kind: CalendarEventKind,
    expected_at: DateTime<Utc>,
    status: CalendarEventStatus,
) -> EventState {
    EventState {
        id: event_id(n),
        kind,
        title: format!("Event {n}"),
        council_meeting_id: None,
        expected_at,
        status,
        notes: None,
        changed_at: days(-5) + Duration::hours(n.into()),
    }
}

fn sitting(n: u8, expected_at: DateTime<Utc>) -> EventState {
    event(
        n,
        CalendarEventKind::Sitting,
        expected_at,
        CalendarEventStatus::Scheduled,
    )
}

fn projection(
    n: u32,
    status: ObligationStatus,
    until: Option<Until>,
) -> ObligationProjection {
    ObligationProjection {
        id: obl(n),
        source: Some(gov(1)),
        kind: ObligationKind::Task,
        owed_by: OwedBy::Staff,
        status,
        until,
        lapsed_at: None,
        plan: None,
        created_at: days(-30) + Duration::hours(n.into()),
    }
}

fn resolved(
    calendar: &Calendar,
    rows: Vec<ObligationProjection>,
) -> Vec<ResolvedObligation> {
    let decided = HashSet::from([gov(1)]);
    rows.into_iter()
        .map(|o| ResolvedObligation::new(o, calendar, &decided))
        .collect()
}

#[test]
fn a_sitting_is_pending_until_held_linked_or_adjourned_past() {
    let mut held = sitting(2, days(3));
    held.status = CalendarEventStatus::Held;
    let mut linked = sitting(3, days(3));
    linked.council_meeting_id =
        Some("8a1f3c2e-0000-4000-8000-0000000000ff".parse().unwrap());
    let stale = sitting(4, days(-20));
    let calendar = Calendar::new(
        vec![sitting(1, days(3)), held, linked, stale],
        Some(days(-10)),
        now(),
    );
    let pending: Vec<bool> = calendar
        .events
        .iter()
        .map(|e| calendar.pending(e))
        .collect();
    assert_eq!(pending, [true, false, false, false]);
    assert_eq!(calendar.today, now().date_naive());
}

#[test]
fn the_next_sitting_is_the_earliest_scheduled_one_of_either_kind() {
    let cancelled = event(
        1,
        CalendarEventKind::SpecialSitting,
        days(2),
        CalendarEventStatus::Cancelled,
    );
    let special = event(
        2,
        CalendarEventKind::SpecialSitting,
        days(5),
        CalendarEventStatus::Scheduled,
    );
    let calendar = Calendar::new(
        vec![sitting(3, days(9)), cancelled, special],
        None,
        now(),
    );
    assert_eq!(calendar.next_sitting().unwrap().id, event_id(2));
    assert_eq!(Calendar::new(vec![], None, now()).next_sitting(), None);
}

#[test]
fn deadlines_resolve_against_the_calendar() {
    let cancelled = event(
        2,
        CalendarEventKind::SpecialSitting,
        days(3),
        CalendarEventStatus::Cancelled,
    );
    let over = sitting(3, days(-20));
    let calendar = Calendar::new(
        vec![sitting(1, days(10)), cancelled, over],
        Some(days(-15)),
        now(),
    );
    let decided = |id: &GovernanceLogId| id == &gov(1);
    let next = Resolved {
        due: Some(days(10).date_naive()),
        approximate: true,
        sitting: Some(event_id(1)),
        date_binds: false,
    };
    let date = days(4).date_naive();
    assert_eq!(calendar.resolve(None, decided), Resolved::default());
    assert_eq!(
        calendar.resolve(Some(&Until::Date { date }), decided),
        Resolved {
            due: Some(date),
            approximate: false,
            sitting: None,
            date_binds: true,
        }
    );
    assert_eq!(calendar.resolve(Some(&Until::NextSitting), decided), next);
    // A called-off sitting carries forward
    assert_eq!(
        calendar.resolve(Some(&Until::Sitting { event: event_id(2) }), decided),
        next
    );
    // A sitting that is over binds its date
    assert_eq!(
        calendar.resolve(Some(&Until::Sitting { event: event_id(3) }), decided),
        Resolved {
            due: Some(days(-20).date_naive()),
            approximate: true,
            sitting: Some(event_id(3)),
            date_binds: true,
        }
    );
    // An event not on the calendar resolves to nothing
    assert_eq!(
        calendar.resolve(Some(&Until::Sitting { event: event_id(9) }), decided),
        Resolved::default()
    );
    assert_eq!(
        calendar
            .resolve(Some(&Until::AfterDecision { decision: gov(1) }), decided),
        next
    );
    assert_eq!(
        calendar
            .resolve(Some(&Until::AfterDecision { decision: gov(2) }), decided),
        Resolved::default()
    );
    // With no sitting on the calendar, the next one is due today
    let empty = Calendar::new(vec![], None, now());
    assert_eq!(
        empty.resolve(Some(&Until::NextSitting), decided),
        Resolved {
            due: Some(now().date_naive()),
            approximate: true,
            sitting: None,
            date_binds: false,
        }
    );
}

#[test]
fn overdue_is_open_and_past_a_date_that_binds() {
    let calendar = Calendar::new(vec![sitting(1, days(-1))], None, now());
    let past = Some(Until::Date {
        date: days(-3).date_naive(),
    });
    let rows = resolved(
        &calendar,
        vec![
            projection(1, ObligationStatus::Open, past.clone()),
            projection(2, ObligationStatus::Lapsed, past.clone()),
            projection(3, ObligationStatus::Discharged, past),
            // A pending sitting whose date passed: not overdue
            projection(4, ObligationStatus::Open, Some(Until::NextSitting)),
        ],
    );
    let overdue: Vec<bool> = rows.iter().map(|r| r.overdue).collect();
    assert_eq!(overdue, [true, false, false, false]);
    assert_eq!(
        ObligationTally::from(rows.as_slice()),
        ObligationTally {
            lapsed: 1,
            overdue: 1
        }
    );
}

#[test]
fn the_due_order_puts_what_is_owed_first_and_undated_last() {
    let calendar = Calendar::new(vec![sitting(1, days(10))], None, now());
    let date = |n| {
        Some(Until::Date {
            date: days(n).date_naive(),
        })
    };
    let mut rows = resolved(
        &calendar,
        vec![
            projection(1, ObligationStatus::Open, None),
            projection(2, ObligationStatus::Open, date(2)),
            projection(3, ObligationStatus::Open, Some(Until::NextSitting)),
            projection(4, ObligationStatus::Lapsed, date(-3)),
            projection(5, ObligationStatus::Discharged, date(-60)),
            projection(6, ObligationStatus::Open, None),
            projection(7, ObligationStatus::Open, date(10)),
        ],
    );
    ObligationSort::Due.sort(&mut rows);
    let order: Vec<GovernanceLogId> =
        rows.iter().map(|r| r.obligation.id.clone()).collect();
    assert_eq!(order, [4, 2, 3, 7, 1, 6, 5].map(obl));
    ObligationSort::Created.sort(&mut rows);
    let order: Vec<GovernanceLogId> =
        rows.iter().map(|r| r.obligation.id.clone()).collect();
    assert_eq!(order, (1..=7).map(obl).collect::<Vec<_>>());
}

#[test]
fn upcoming_lists_sittings_and_dated_obligations_by_text() {
    let later = event(
        2,
        CalendarEventKind::SpecialSitting,
        days(20),
        CalendarEventStatus::Scheduled,
    );
    let calendar = Calendar::new(vec![sitting(1, days(9)), later], None, now());
    let soon = Some(Until::Date {
        date: days(3).date_naive(),
    });
    let rows = resolved(
        &calendar,
        vec![
            projection(1, ObligationStatus::Open, soon.clone()),
            projection(2, ObligationStatus::Open, Some(Until::NextSitting)),
            // Closed, or past, or with no text: not listed
            projection(3, ObligationStatus::Discharged, soon.clone()),
            projection(
                4,
                ObligationStatus::Open,
                Some(Until::Date {
                    date: days(-1).date_naive(),
                }),
            ),
            projection(5, ObligationStatus::Open, soon),
        ],
    );
    let dated: Vec<GovernanceLogId> = calendar
        .dated(&rows)
        .iter()
        .map(|o| o.obligation.id.clone())
        .collect();
    assert_eq!(dated, [obl(1), obl(5)]);
    let texts = HashMap::from([(obl(1), "Ship the ledger".to_string())]);

    let all = calendar.upcoming(&rows, &texts, false, CALENDAR_LIMIT);
    let shape: Vec<String> = all
        .iter()
        .map(|i| match i {
            CalendarItem::Sitting {
                event,
                obligations_due,
                ..
            } => format!("{event} ({obligations_due})"),
            CalendarItem::ObligationDue { text, .. } => text.clone(),
        })
        .collect();
    assert_eq!(
        shape,
        [
            "Ship the ledger".to_string(),
            format!("{} (1)", event_id(1)),
            format!("{} (0)", event_id(2)),
        ]
    );
    // The dashboard leaves out the next sitting, and takes a few
    let dash = calendar.upcoming(&rows, &texts, true, 1);
    assert_eq!(dash.len(), 1);
    assert!(matches!(&dash[0], CalendarItem::ObligationDue { .. }));
    let dash = calendar.upcoming(&rows, &texts, true, UPCOMING_LIMIT);
    assert!(
        matches!(&dash[1], CalendarItem::Sitting { event, .. } if *event == event_id(2))
    );

    let response = CalendarResponse::new(&calendar, &rows, &texts);
    assert_eq!(response.items, all);
    assert_eq!(response.how_to_read, CALENDAR_HOW_TO_READ);
}

#[test]
fn a_listing_carries_the_resolved_deadline() {
    let calendar = Calendar::new(vec![sitting(1, days(9))], None, now());
    let row = resolved(
        &calendar,
        vec![projection(
            1,
            ObligationStatus::Open,
            Some(Until::NextSitting),
        )],
    )
    .remove(0);
    let listing = row.into_listing("Ship it".to_string());
    assert_eq!(listing.text, "Ship it");
    assert_eq!(listing.due, Some(days(9).date_naive()));
    assert!(listing.due_approximate && !listing.overdue);
    assert_eq!(listing.until, Some(Until::NextSitting));
}

#[test]
fn awaited_decision_is_only_after_decision() {
    let mut row = projection(1, ObligationStatus::Open, None);
    assert_eq!(row.awaited_decision(), None);
    row.until = Some(Until::AfterDecision { decision: gov(3) });
    assert_eq!(row.awaited_decision(), Some(&gov(3)));
}

#[test]
fn fixed_date_is_only_a_date() {
    let date = days(1).date_naive();
    assert_eq!(Until::Date { date }.fixed_date(), Some(date));
    assert_eq!(Until::NextSitting.fixed_date(), None);
    assert_eq!(Until::Sitting { event: event_id(1) }.fixed_date(), None);
    assert_eq!(Until::AfterDecision { decision: gov(1) }.fixed_date(), None);
}

#[test]
fn a_record_is_its_latest_version_and_its_state() {
    let o = Obligation::new(
        ObligationKind::Task,
        OwedBy::Staff,
        "Ship the ledger and the form",
        "the words",
        "round 2",
    );
    let data = serde_json::to_value(&o).unwrap();
    let hash = Sha256Hex::from([0u8; 32]);
    let plan: PostId = "8a1f3c2e-0000-4000-8000-000000000001".parse().unwrap();
    let planned = AmendmentDraft::obligation(
        obl(1),
        hash,
        ObligationUpdate::Planned { plan },
        "b",
        "n",
    )
    .amendment;
    let discharged = AmendmentDraft::obligation(
        obl(1),
        hash,
        ObligationUpdate::Discharged {
            evidence: Evidence::Governance { id: gov(1) },
        },
        "b",
        "n",
    )
    .amendment;
    let mut revision = planned.clone();
    revision.kind = crate::enums::AmendmentKind::Revision;
    revision.update = None;
    revision.revision = Some(crate::govlog::Revision {
        patch: serde_json::from_value(serde_json::json!([
            {"op": "replace", "path": "/text", "value": "Ship the ledger"}
        ]))
        .unwrap(),
        duplicates: vec![],
        resulting_data_hash: hash,
    });
    let record = ObligationRecord::from_log(
        obl(1),
        now(),
        7,
        &data,
        &[
            (days(1), planned),
            (days(2), revision),
            (days(3), discharged),
        ],
    )
    .unwrap();
    assert_eq!(record.obligation.text, "Ship the ledger");
    assert_eq!(record.state.status, ObligationStatus::Discharged);
    assert_eq!(record.state.plan, Some(plan));
    assert_eq!(record.chain_seq, 7);

    assert!(
        ObligationRecord::from_log(
            obl(1),
            now(),
            7,
            &serde_json::json!({"not": "an obligation"}),
            &[]
        )
        .is_err()
    );
}
