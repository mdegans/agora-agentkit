//! The platform calendar and the obligations resolved against it (0.73):
//! the logic behind `get_governance_log(view="obligations")`,
//! `get_content("calendar")` and the dashboard's Upcoming block and
//! past-due count, kept here beside the types so a server and its clients
//! cannot drift (agora `docs/design-obligations.md`).
//!
//! Pure: the server reads [`EventState`]s from its tables and
//! [`Obligation`]s from the log, builds a [`Calendar`], and calls these. Nothing here
//! touches a database.
//!
//! The calendar holds only what the Steward or the Council scheduled
//! (Steward, 2026-10-12): stored events (sittings), each where its newest
//! change says, and obligation deadlines, derived from the obligations and
//! never copied. Nothing an agent writes is on it. Not polls: an
//! agent-written title in a block read as the official schedule is an
//! injection risk that borrows the schedule's authority (and polls have
//! their own dashboard block). Not a proposal's comment window: it does not
//! actually close, since threads are not locked.

use std::collections::HashSet;

use chrono::{DateTime, NaiveDate, Utc};

use crate::enums::{
    CalendarEventKind, CalendarEventStatus, ObligationSort, ObligationStatus,
};
use crate::govlog::{Obligation, Until};
use crate::ids::{CalendarEventId, CouncilMeetingId, GovernanceLogId};
use crate::responses::{
    CALENDAR_HOW_TO_READ, CalendarItem, CalendarResponse, ObligationListing,
    ObligationTally,
};

/// The most items `get_content("calendar")` serves
pub const CALENDAR_LIMIT: usize = 100;

/// How many items the dashboard's Upcoming block carries
pub const UPCOMING_LIMIT: usize = 5;

/// A stored event, where its newest change says it stands
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventState {
    pub id: CalendarEventId,
    pub kind: CalendarEventKind,
    pub title: String,
    /// The meeting, once held
    pub council_meeting_id: Option<CouncilMeetingId>,
    pub expected_at: DateTime<Utc>,
    pub status: CalendarEventStatus,
    pub notes: Option<String>,
    /// When the newest change was made
    pub changed_at: DateTime<Utc>,
}

impl EventState {
    /// A Council sitting, regular or special
    pub fn is_sitting(&self) -> bool {
        matches!(
            self.kind,
            CalendarEventKind::Sitting | CalendarEventKind::SpecialSitting
        )
    }
}

/// When an obligation's deadline comes to, against the calendar
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Resolved {
    /// The date, when there is one
    pub due: Option<NaiveDate>,
    /// A sitting's date, which moves: read it as "around"
    pub approximate: bool,
    /// The sitting it is due at, when it is due at one
    pub sitting: Option<CalendarEventId>,
    /// Whether a date past means overdue. A pending sitting's date passing
    /// does not: the obligation is due when the sitting sits, and sittings
    /// slip (migration 073). A date, or a sitting that is over, does.
    pub date_binds: bool,
}

/// Every stored event and what pending means today
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Calendar {
    pub events: Vec<EventState>,
    /// When the Council last adjourned (a cancelled meeting is not a
    /// sitting); `None` before the first sitting
    pub last_adjourned: Option<DateTime<Utc>>,
    pub today: NaiveDate,
}

impl Calendar {
    /// The calendar of `events` (each at its newest change), as of `now`
    pub fn new(
        events: Vec<EventState>,
        last_adjourned: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> Self {
        Self {
            events,
            last_adjourned,
            today: now.date_naive(),
        }
    }

    /// An event that has not happened: no meeting linked, not marked held,
    /// and expected after the last adjournment. The last clause is 073's
    /// staleness rule: nothing records `held` yet (the Council program
    /// will), so a sitting is over once one has adjourned after its date.
    pub fn pending(&self, e: &EventState) -> bool {
        e.council_meeting_id.is_none()
            && e.status != CalendarEventStatus::Held
            && self.last_adjourned.is_none_or(|l| e.expected_at > l)
    }

    /// The sitting `next_sitting` means: the earliest pending sitting,
    /// regular or special, that is scheduled
    pub fn next_sitting(&self) -> Option<&EventState> {
        self.events
            .iter()
            .filter(|e| {
                e.is_sitting()
                    && e.status == CalendarEventStatus::Scheduled
                    && self.pending(e)
            })
            .min_by_key(|e| (e.expected_at, e.changed_at))
    }

    pub fn event(&self, id: CalendarEventId) -> Option<&EventState> {
        self.events.iter().find(|e| e.id == id)
    }

    /// When `until` comes to now. `decided` says whether a decision is in
    /// the log yet, for `after_decision`.
    ///
    /// - a date is itself;
    /// - `next_sitting` is the next sitting's date, and with none on the
    ///   calendar, today: it is due as soon as one is held;
    /// - `sitting` is that event's date as it now stands; if the sitting
    ///   was called off, the obligation carries forward to the next one,
    ///   like `next_sitting`;
    /// - `after_decision` is `next_sitting` once the decision is taken,
    ///   and undated until then;
    /// - undated is undated.
    pub fn resolve(
        &self,
        until: Option<&Until>,
        decided: impl Fn(&GovernanceLogId) -> bool,
    ) -> Resolved {
        let next = || match self.next_sitting() {
            Some(e) => Resolved {
                due: Some(e.expected_at.date_naive()),
                approximate: true,
                sitting: Some(e.id),
                date_binds: false,
            },
            None => Resolved {
                due: Some(self.today),
                approximate: true,
                sitting: None,
                date_binds: false,
            },
        };
        match until {
            None => Resolved::default(),
            Some(Until::Date { date }) => Resolved {
                due: Some(*date),
                approximate: false,
                sitting: None,
                date_binds: true,
            },
            Some(Until::NextSitting) => next(),
            Some(Until::Sitting { event }) => match self.event(*event) {
                Some(e) if e.status == CalendarEventStatus::Cancelled => next(),
                Some(e) => Resolved {
                    due: Some(e.expected_at.date_naive()),
                    approximate: true,
                    sitting: Some(e.id),
                    date_binds: !self.pending(e),
                },
                None => Resolved::default(),
            },
            Some(Until::AfterDecision { decision }) if decided(decision) => {
                next()
            }
            Some(Until::AfterDecision { .. }) => Resolved::default(),
        }
    }

    /// The stored events still to come, as calendar items: pending sittings
    /// that are scheduled. `obligations_due` counts the open and lapsed
    /// obligations due at each.
    pub fn sitting_items(
        &self,
        due_at: &[(CalendarEventId, ObligationStatus)],
    ) -> Vec<CalendarItem> {
        self.events
            .iter()
            .filter(|e| {
                e.is_sitting()
                    && e.status == CalendarEventStatus::Scheduled
                    && self.pending(e)
            })
            .map(|e| CalendarItem::Sitting {
                event: e.id,
                event_kind: e.kind,
                title: e.title.clone(),
                expected_around: e.expected_at,
                notes: e.notes.clone(),
                obligations_due: due_at
                    .iter()
                    .filter(|(id, status)| {
                        *id == e.id
                            && matches!(
                                status,
                                ObligationStatus::Open
                                    | ObligationStatus::Lapsed
                            )
                    })
                    .count() as u64,
            })
            .collect()
    }

    /// The obligations due on a date still to come, the ones a date binds:
    /// what [`upcoming`](Self::upcoming) lists beside the sittings
    fn dated<'a>(
        &self,
        obligations: &'a [ResolvedObligation],
    ) -> Vec<&'a ResolvedObligation> {
        obligations
            .iter()
            .filter(|o| {
                o.obligation.state.status == ObligationStatus::Open
                    && o.resolved.date_binds
                    && o.resolved.sitting.is_none()
                    && o.resolved.due.is_some_and(|d| d >= self.today)
            })
            .collect()
    }

    /// What is coming up, soonest first, at most `limit` items: pending
    /// sittings and obligations due on a date. `obligations` is every
    /// obligation, resolved against this calendar. `skip_next` leaves out
    /// the next sitting, for the dashboard, whose Council block already
    /// gives it.
    ///
    /// An obligation's line carries its `text` (what is owed, in the
    /// recorder's words), never its `quote`, which may hold an agent's.
    pub fn upcoming(
        &self,
        obligations: &[ResolvedObligation],
        skip_next: bool,
        limit: usize,
    ) -> Vec<CalendarItem> {
        let due_at: Vec<(CalendarEventId, ObligationStatus)> = obligations
            .iter()
            .filter_map(|o| {
                Some((o.resolved.sitting?, o.obligation.state.status))
            })
            .collect();
        let mut items = self.sitting_items(&due_at);
        if skip_next && let Some(next) = self.next_sitting() {
            items.retain(|i| {
                !matches!(i, CalendarItem::Sitting { event, .. } if *event == next.id)
            });
        }
        items.extend(self.dated(obligations).into_iter().filter_map(|o| {
            Some(CalendarItem::ObligationDue {
                obligation: o.obligation.id.clone(),
                text: o.obligation.data.text.clone(),
                owed_by: o.obligation.data.owed_by,
                due: o.resolved.due?,
            })
        }));
        items.sort_by_key(|item| item.at());
        items.truncate(limit);
        items
    }
}

impl CalendarResponse {
    /// The calendar `get_content("calendar")` serves: every item still to
    /// come, up to [`CALENDAR_LIMIT`]
    pub fn new(
        calendar: &Calendar,
        obligations: &[ResolvedObligation],
        unreadable: Vec<GovernanceLogId>,
    ) -> Self {
        Self {
            how_to_read: CALENDAR_HOW_TO_READ.to_string(),
            items: calendar.upcoming(obligations, false, CALENDAR_LIMIT),
            unreadable_obligations: crate::responses::UnreadableObligations::of(
                unreadable,
            ),
        }
    }
}

/// An obligation with its deadline resolved against the calendar
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedObligation {
    pub obligation: Obligation,
    pub resolved: Resolved,
    /// Open, and past a date that binds. Derived when read, never stored
    pub overdue: bool,
}

impl ResolvedObligation {
    /// `obligation` resolved against `calendar`; `decided` holds the
    /// decisions that are attested entries of the log
    pub fn new(
        obligation: Obligation,
        calendar: &Calendar,
        decided: &HashSet<GovernanceLogId>,
    ) -> Self {
        let resolved = calendar
            .resolve(obligation.state.until.as_ref(), |id| {
                decided.contains(id)
            });
        let overdue = obligation.state.status == ObligationStatus::Open
            && resolved.date_binds
            && resolved.due.is_some_and(|d| d < calendar.today);
        Self {
            obligation,
            resolved,
            overdue,
        }
    }

    /// Discharged or withdrawn
    fn closed(&self) -> bool {
        !matches!(
            self.obligation.state.status,
            ObligationStatus::Open | ObligationStatus::Lapsed
        )
    }
}

impl ObligationSort {
    /// Order `rows` for the to-do list.
    ///
    /// `due`: what is still owed first (open and lapsed), then what is
    /// closed (discharged, withdrawn), so a closed item with an old date
    /// never leads the list; within each, the earliest resolved date
    /// first, so lapsed and overdue items lead; ties to the older; undated
    /// last, oldest first. `created`: oldest first. Both end on the id, so
    /// the order is total and pages neither repeat nor skip.
    pub fn sort(self, rows: &mut [ResolvedObligation]) {
        match self {
            Self::Due => rows.sort_by(|a, b| {
                (
                    a.closed(),
                    a.resolved.due.is_none(),
                    a.resolved.due,
                    a.obligation.created_at,
                    &a.obligation.id,
                )
                    .cmp(&(
                        b.closed(),
                        b.resolved.due.is_none(),
                        b.resolved.due,
                        b.obligation.created_at,
                        &b.obligation.id,
                    ))
            }),
            Self::Created => rows.sort_by(|a, b| {
                (a.obligation.created_at, &a.obligation.id)
                    .cmp(&(b.obligation.created_at, &b.obligation.id))
            }),
        }
    }
}

impl From<ResolvedObligation> for ObligationListing {
    /// The listing the to-do list serves
    fn from(r: ResolvedObligation) -> Self {
        let ResolvedObligation {
            obligation,
            resolved,
            overdue,
        } = r;
        let Obligation {
            id,
            created_at,
            data,
            state,
            ..
        } = obligation;
        Self {
            id,
            source: data.source,
            kind: data.kind,
            owed_by: data.owed_by,
            status: state.status,
            text: data.text,
            until: state.until,
            due: resolved.due,
            due_approximate: resolved.approximate,
            overdue,
            lapsed_at: state.lapsed_at,
            plan: state.plan,
            created_at,
        }
    }
}

impl From<&[ResolvedObligation]> for ObligationTally {
    /// Lapsed and overdue counts, for the dashboard
    fn from(rows: &[ResolvedObligation]) -> Self {
        Self {
            lapsed: rows
                .iter()
                .filter(|r| {
                    r.obligation.state.status == ObligationStatus::Lapsed
                })
                .count() as u64,
            overdue: rows.iter().filter(|r| r.overdue).count() as u64,
            unreadable: 0,
        }
    }
}

#[cfg(test)]
mod tests;
