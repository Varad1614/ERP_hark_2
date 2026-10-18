# ERP_EXTENSIONS 5B — Shift swaps, punches, timesheets and payroll inputs

Builds on the 3A duty roster (`people.duty_shift`, `people.roster_settings`). Staff ask to swap,
give away or get cover for a shift, or claim an open one; a colleague agrees and an independent
manager decides. Clock punches per shift are reconciled with 7B daily attendance into a weekly or
monthly timesheet that is submitted, decided, locked and exported to 7C payroll. Logic ported from
HeyPuter/everything `src/shifts/src/core` (`swaps.js`, `attendance.js`, `timesheet.js`,
`payroll.js`, `reports.js`, `ics.js`; MIT, pinned 06802a1) into
`modules/people/src/roster/{swaps,clock,timesheet,reports,ics}.rs`; API in
`modules/people/src/roster/shifts_http.rs`; migration
`migrations/20270104000200_people_shift_swaps_timesheets.sql`.

## Tables (`people` schema, RLS via `platform.apply_tenant_rls()`)

* `roster_settings` (altered): late / early-leave grace (default 10 min), daily / weekly overtime hours (9 / 48), overtime multiplier (2), flat OT rate, standard hours/day (8), night / holiday shift allowances.
* `duty_shift_swap`: `give_away | swap | cover | claim` request, `awaiting_peer → awaiting_manager → approved | declined | cancelled`, shift snapshot, warnings, one live request per shift.
* `duty_shift_punch`: one record per shift — `present | late | absent`, `HH:MM` clock in / out, `source` self | manager.
* `timesheet`: staff × weekly / monthly period (≤ 31 days), `submitted | returned | approved | locked`, reconciled totals + `lines` jsonb, decided / locked / exported stamps.
* `payroll_variable_input`: `overtime_hours | lop_days | night_shifts | holiday_shifts` quantity per staff and work window, sourced from a timesheet (`UNIQUE (source_id, code)`), `active | void`.

## Rules

* **Swap flow**: the requester (`people.self`) asks; for `give_away` / `swap` the named peer
  accepts or declines (a `cover` without a name: any colleague; a `claim` skips the peer) →
  `awaiting_manager`. A manager (`people.staff.manage`) decides with If-Match; the requester, the
  peer and both staff members are refused (`ensure_independent_decider`). Approval re-validates
  the moves against the 3A rules (`rules.rs`: conflicts, availability, weekly maximum, rest /
  daily hours / consecutive days), records warnings and reassigns the shift(s) in one transaction
  under the roster lock. The requester may cancel while live.
* **Punches** (`attendance.js`): self clock-in / clock-out on one's own shift (409 on a double
  punch), manager record / correct (`people.staff.manage`); worked time across midnight net of
  the unpaid break. No punch edits inside an approved or locked timesheet (409).
* **Reconciliation** (`reconcile`): each shift line takes the 7B attendance day (leave, absence,
  holiday), punches and schedule; lateness beyond `late_grace_minutes`, early leave beyond
  `early_leave_grace_minutes`, missed punches, absences, loss-of-pay days not already in daily
  attendance, night shifts (past midnight, Factories Act s.57), holiday shifts.
* **Overtime**: paid hours over 9 h/day or 48 h/week by default (s.51/s.54; either limit may be
  off), paid at `overtime_multiplier` × the ordinary rate ((BASIC + DA) ÷ days ÷ hours/day, s.59)
  unless a flat rate is set.
* **Timesheet**: staff submit a period (→ `submitted`, Idempotency-Key; resubmit after
  `returned`). Decision (`approve | reject`, reason required to reject, If-Match) by
  `people.staff.manage`; the owner and submitter are refused (`ensure_independent_decider`).
  Reject → `returned`; approve → `approved`; lock (`approved → locked`) by a manager.
* **Payroll export** (`people.payroll.manage`): approved / locked, not yet exported timesheets in
  range are locked, written to `people.payroll_variable_input` and stamped `exported_at`. Payroll
  calculation reads active inputs whose `work_from` falls in the salary window and adds
  `OVERTIME`, `NIGHT_SHIFT` and `HOLIDAY_SHIFT` lines (and LOP days). CSV mirror is read-only.
* **Reports** (`reports.js`): scheduled vs worked per person, cost by role, attendance, coverage
  gaps and open shifts with the prior period for trends; one formula-safe CSV per section.
* **Staff .ics** (`ics.js`): `GET /me/roster.ics` — one VEVENT per assigned shift, floating local
  times, UIDs from shift ids, RFC 5545 escaping, folding at 74 octets.

## Endpoints (tag `people-roster`, op ids `people_roster_*`)

Generated from `utoipa::path` in `shifts_http.rs`; permission = the handler's `auth.require`.

| Method | Path | Operation id | Permission |
|---|---|---|---|
| POST | `/api/v1/people/me/roster/swaps` | `people_roster_swap_request` | people.self |
| POST | `/api/v1/people/me/roster/shifts/{id}/claim` | `people_roster_shift_claim` | people.self |
| POST | `/api/v1/people/roster/swaps/{id}/respond` | `people_roster_swap_respond` | people.self |
| GET | `/api/v1/people/me/roster/open-shifts` | `people_roster_open_shifts` | people.self |
| POST | `/api/v1/people/roster/swaps/{id}/cancel` | `people_roster_swap_cancel` | people.self |
| GET | `/api/v1/people/me/roster/swaps` | `people_roster_swap_mine` | people.self |
| GET | `/api/v1/people/institutions/{id}/roster/swaps` | `people_roster_swap_list` | people.read |
| POST | `/api/v1/people/roster/swaps/{id}/decision` | `people_roster_swap_decide` | people.staff.manage |
| POST | `/api/v1/people/me/roster/shifts/{id}/clock` | `people_roster_clock` | people.self |
| PUT | `/api/v1/people/roster/shifts/{id}/attendance` | `people_roster_attendance_put` | people.staff.manage |
| GET | `/api/v1/people/me/roster/timesheet` | `people_roster_timesheet_me` | people.self |
| POST | `/api/v1/people/me/roster/timesheets` | `people_roster_timesheet_submit` | people.self |
| GET | `/api/v1/people/institutions/{id}/roster/timesheets` | `people_roster_timesheet_list` | people.read |
| GET | `/api/v1/people/roster/timesheets/{id}` | `people_roster_timesheet_get` | people.read |
| POST | `/api/v1/people/roster/timesheets/{id}/decision` | `people_roster_timesheet_decide` | people.staff.manage |
| POST | `/api/v1/people/roster/timesheets/{id}/lock` | `people_roster_timesheet_lock` | people.staff.manage |
| POST | `/api/v1/people/institutions/{id}/roster/payroll-export` | `people_roster_payroll_export` | people.payroll.manage |
| GET | `/api/v1/people/institutions/{id}/roster/payroll-export.csv` | `people_roster_payroll_export_csv` | people.payroll.manage |
| GET | `/api/v1/people/institutions/{id}/roster/timesheet.csv` | `people_roster_timesheet_csv` | people.read |
| GET | `/api/v1/people/institutions/{id}/roster/reports` | `people_roster_report` | people.read |
| GET | `/api/v1/people/institutions/{id}/roster/reports.csv` | `people_roster_report_csv` | people.read |
| GET | `/api/v1/people/me/roster.ics` | `people_roster_me_ics` | people.self |
| GET | `/api/v1/people/institutions/{id}/roster/timesheet-settings` | `people_roster_timesheet_settings_get` | people.read |
| PUT | `/api/v1/people/institutions/{id}/roster/timesheet-settings` | `people_roster_timesheet_settings_put` | people.staff.manage |
Decision rows in the approval matrix: `people_roster_swap_decide`, `people_roster_timesheet_decide`.
No new permissions; routes are entitled under the `hr` module.

Audit actions: `people.duty_shift_swap.request|claim|respond|cancel|decide`,
`people.duty_shift_punch.clock|record`, `people.timesheet.submit|decide|lock|payroll_export`,
`people.roster_settings.timesheet`.

## Tests

* Unit: `modules/people/src/roster/{swaps,clock,timesheet,reports,ics}.rs`.
* DB: `apps/api/tests/db_e5b_shifts.rs` — `swap_needs_peer_then_an_independent_manager`,
  `timesheet_reconciles_punches_and_feeds_payroll`.
