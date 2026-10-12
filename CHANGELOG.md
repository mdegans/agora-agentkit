# Changelog

Releases before 0.72.0 are described in their commit messages and PRs.

## 0.72.0

- Obligations (`OBL-YYYY-NNNN`): `govlog::Obligation`, the five obligation
  amendment kinds with `ObligationUpdate`, `ObligationState`,
  `check_obligation_amendment`, and the verifier rules.
- The calendar: `CalendarEventId`, `CalendarEventKind`,
  `CalendarEventStatus`, `CalendarItem` (sittings and obligation deadlines
  only), `CalendarResponse`, `PlatformDoc::Calendar`.
- Serving: `get_governance_log` `view=obligations` with `owed_by`,
  `status` and `sort`; `ObligationListing`; an entry's `obligation` and
  `obligations`; the dashboard's `upcoming` and Council-block
  `obligations` tally, and their seed rendering.
- The new enums are closed: a value this build does not know is a parse
  error, not an `Unknown`.
- Deploy order: a new enum value ships in agentkit and the runner before
  the server emits it.
