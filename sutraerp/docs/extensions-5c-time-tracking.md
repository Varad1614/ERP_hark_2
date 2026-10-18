# ERP_EXTENSIONS 5C — Staff time tracking (projects, grants, committees)

Staff record time against research projects / sponsored grants, internal projects and committees;
a week of one project is submitted as a timesheet and decided by the project's PI or the staff
member's reporting manager; effort reports give hours, % effort, person-months and cost for
utilisation certificates. Logic ported from HeyPuter/everything `src/time-tracking/src/lib`
(`time.js`, `util.js`, `csv.js`, `importer.js`, `store.js`; MIT, pinned 06802a1) into
`modules/people/src/timetrack/logic.rs`; API in `modules/people/src/timetrack/http.rs`; migration
`migrations/20270104000300_people_time_tracking.sql`.

## Reuse decisions

| Concept | Existing | Decision |
|---|---|---|
| Research project / grant | none (Resources keeps research **MoUs** as `resources.contract` kind `mou_research`) | new `people.time_project`; optional `contract_id` links the MoU / sanction contract by plain id (no cross-schema FK) |
| Cost centre / fund | none in Finance | `cost_centre` free-text code on the project (to be replaced by a Finance master when one exists) |
| Hourly cost rate | payroll results (`people.payroll_result`) | derived: last **finalised** result's gross, scaled to a full month (gross × period_days ÷ payable_days), ÷ `standard_monthly_hours` |
| Staff, PI, manager | `people.staff_member`, `staff_engagement.reports_to_staff_id` | reused; PI = `time_project.pi_staff_id` |
| Teaching workload | `people.workload_plan` (Domain D load) | untouched; the admin tile sits on the **Workload** tab next to it |
| Shift timesheets (5B, roster) | `roster/timesheet.rs` (shift attendance hours for payroll) | different concept (rostered shift hours vs project effort); not merged |

## Tables (`people` schema, RLS via `platform.apply_tenant_rls()`)

* `time_settings` (tenant, institution): `week_start` (1 Mon | 0 Sun), `rounding_minutes` (0,5,6,10,15,30),
  `standard_monthly_hours` (default 176 = 22 × 8), `default_hourly_rate` (jsonb number or periods),
  `timer_cap_hours` (default 12).
* `time_project`: `code` (unique per institution, upper-cased), `name`, `kind`
  (`sponsored_research | consultancy | internal_project | committee | other`), `funding_agency`
  (DST, SERB/ANRF, UGC, ICSSR, AICTE, DBT, CSIR, ICMR…), `scheme`, `sanction_number`, `sanction_date`,
  `sanctioned_amount`, `manpower_budget` (₹, salary head), `start_date`/`end_date` (required for
  sponsored), `pi_staff_id`, `cost_centre`, `contract_id`, budget (`budget_kind` hours|amount,
  `budget_period` total|monthly, `budget_value`), `rounding_minutes`/`minimum_minutes` overrides,
  `billable_default`, `open_to_all`, `status` (`active | closed | archived`), `notes`.
* `time_project_member`: (project, staff) with `role` (`pi | co_pi | member | jrf | srf |
  research_associate | project_assistant | technical_staff`) and `commitment_percent`. The PI is
  always a member.
* `time_entry`: staff, project, `entry_date`, `seconds` (1…86 400, the duration of record),
  optional `started_at`/`ended_at` (`timestamp`, institution-local wall clock; both or neither),
  `task`, `notes`, `billable` (= chargeable to the project), `source` (`manual | timer | import`).
* `time_timer`: one running timer per staff member (local wall-clock start).
* `time_timesheet`: (staff, project, `week_start`) unique; `status` `draft | submitted | approved |
  rejected`; `total_seconds`, `billed_seconds` snapshot; submitted / decided by + at; CHECK
  `decided_by <> submitted_by`.
* `time_staff_rate`: a staff member's own rate (jsonb number or effective-dated periods).

## Rules

* **Durations** (`parseDuration`): `1:30`, `1:30:00`, `90m`, `1.5h`, `1h30`, `1h 30m`, `:45`; bare
  numbers are hours (`1.5`, `8`) except integers above 24 = minutes (`45`, `90`). With `start` and
  `end` (`HH:MM`) the duration may be omitted; an end at or before the start is the next morning;
  with all three the start stays and the end moves (`durationPatch`).
* **Who may log**: active projects that are `open_to_all` or where the staff member is PI / member,
  dated inside the project's period. ≤ 24 h per entry.
* **Overlap**: an entry with clock times may not overlap another of the same person
  (`overlappingEntries`, day before / after included; touching ends allowed) → 409.
* **Timer**: one per person (409); stop records on the start day with a 1-minute floor and the
  `timer_cap_hours` (or body `capHours`) trim (`stopTimer`); discard records nothing.
* **Rounding / minimum** (`billedSeconds`): project `rounding_minutes` else the institution's;
  project `minimum_minutes` (a floor on the rounded time). Only chargeable (`billable`) entries cost.
* **Rate cascade** (`resolveRate` mapped): staff's own effective-dated rate on the entry date →
  payroll-derived hourly rate → institution default (effective-dated) → 0. Rate edits with a date
  start a new period (`withRateFrom`), so earlier work keeps its rate.
* **Cost** (`entryAmount`): chargeable hours (4 places) × rate, to the paisa.
* **Timesheets**: submit one project's week (≥ 1 entry; from none / draft / rejected) → `submitted`;
  entries of submitted or approved weeks cannot be added, changed, deleted or imported (409). The
  owner may withdraw while submitted (→ `draft`). **Decision** (`approve | reject`, reason required
  to reject, If-Match): the project's PI or the owner's reporting manager (`people.self`), or an
  administrator with `people.timesheet.decide`. Separation of duties: the owner / submitter is
  refused (403, `ensure_independent_decider`). Approved = locked; rejected = open for correction.
* **Effort** per staff × project × month: `percent` = project seconds ÷ the staff member's tracked
  seconds that month over all projects (2 places); `personMonths` = share × 1 (3 places);
  `cost` = Σ entry costs. Project summary: hours, person-months, cost, `utilisationPercent` = cost ÷
  manpower budget. `approvedOnly=true` counts approved weeks only.
* **Budget progress** (`budgetProgress`): hours budgets compare all tracked seconds, amount budgets
  chargeable cost; monthly budgets look at the current month.
* **CSV export**: `Date, Start, End, Staff no., Staff, Project code, Project, Funding agency, Task,
  Notes, Duration, Hours, Chargeable, Rate (INR/h), Amount (INR)` with the formula guard
  (`csvField`). **CSV import** (`detectMapping` / `rowsToItems`): Toggl, Clockify, Harvest or a
  given mapping; projects match loggable projects by code or name (never created); per-line
  problems (unknown project, outside period, submitted week, overlap); duplicates (same day,
  project, task, duration, notes) skipped; `dryRun` defaults to true.

## Endpoints (tag `people-time`, op ids `people_time_*`)

| Method | Path | Permission | Notes |
|---|---|---|---|
| GET / PUT | `/api/v1/people/institutions/{id}/time-settings` | people.read / people.time.manage | |
| GET / POST | `/api/v1/people/institutions/{id}/time-projects` | people.read / people.time.manage | POST: Idempotency-Key; `?status=` |
| GET / PUT | `/api/v1/people/time-projects/{id}` | people.read / people.time.manage | PUT: If-Match; replaces members |
| GET | `/api/v1/people/me/time?week=` | people.self | week view, projects × days, timer |
| POST | `/api/v1/people/me/time/entries` | people.self | Idempotency-Key; 409 overlap / locked |
| PUT / DELETE | `/api/v1/people/me/time/entries/{id}` | people.self | If-Match |
| POST / DELETE | `/api/v1/people/me/time/timer` | people.self | start / discard |
| POST | `/api/v1/people/me/time/timer/stop` | people.self | records an entry |
| POST | `/api/v1/people/me/time/timesheets` | people.self | submit `{projectId, week}` |
| POST | `/api/v1/people/me/time/timesheets/{id}/withdraw` | people.self | If-Match |
| GET | `/api/v1/people/me/time-approvals` | people.self | weeks I decide as PI / manager |
| GET | `/api/v1/people/institutions/{id}/timesheets?status=` | people.read | approval queue |
| GET | `/api/v1/people/timesheets/{id}` | owner / PI / manager / people.read | with entries, `canDecide` |
| POST | `/api/v1/people/timesheets/{id}/decision` | PI / manager (people.self) or people.timesheet.decide | If-Match; SoD; matrix row `people_time_timesheet_decide` |
| GET | `/api/v1/people/institutions/{id}/time-rates` | people.read | own / payroll / effective rate |
| PUT | `/api/v1/people/staff/{id}/time-rate` | people.time.manage | `{rate, from}` |
| GET | `/api/v1/people/institutions/{id}/effort-report?from=&to=&projectId=&approvedOnly=` | people.read | months `YYYY-MM` (≤ 60) |
| GET | `/api/v1/people/institutions/{id}/effort-report/csv` | people.read | text/csv |
| GET | `/api/v1/people/institutions/{id}/time-entries/csv?from=&to=&projectId=` | people.read | text/csv (≤ 2 years) |
| GET | `/api/v1/people/me/time/csv?from=&to=` | people.self | text/csv |
| POST | `/api/v1/people/me/time/import` | people.self | `{csv, mapping?, dryRun?}` (≤ 2 MB, 5 000 rows) |

New permissions (administrator): `people.time.manage` (`TimeTrackingManage`),
`people.timesheet.decide` (`TimesheetDecide`); neither needs step-up. Routes are entitled under
the `hr` module (`/api/v1/people/...` default).

Audit actions: `people.time_settings.update`, `people.time_project.create|update`,
`people.time_entry.create|update|delete|import`, `people.time_timer.start|stop|discard`,
`people.time_timesheet.submit|withdraw|approve|reject`, `people.time_rate.update`.

## Tests

* Golden: `tools/golden/specs/time-tracking.mjs` → `modules/people/tests/golden/time_tracking.json`
  (200 cases) replayed by `modules/people/tests/golden_time_tracking.rs`.
* DB: `apps/api/tests/db_e5c_time_tracking.rs`.
* Web: `apps/web/app/features/people/timetrack/timetrack.test.ts`; e2e screens 60–6x in
  `apps/web/e2e/extensions.e2e.mjs`.

## Web

* Admin → People → **Workload** tab: **Time tracking** tile (projects & grants with members and
  budget bars, approval queue, effort report with CSV, staff rates, settings).
* My HR (`/leave/workload`): **My time** — timer, week grid per project, add entry (duration or
  start/end), submit week, approvals I owe as PI / manager, CSV import / export.
