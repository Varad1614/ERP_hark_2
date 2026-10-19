# Domain G — Campus operations: readiness inventory (Part 1 · Stage 1)

Scope: `modules/campus` (infrastructure/spaces, reservations, service desk, maintenance, labs, hostel,
transport, library, G00 governance), the Admin → Campus and Reports → Governance screens, the student
Library screen, and every L2 workflow in `architecture/L2_Domain_Workflow_Architecture/section-g`
(G00–G10). Base `part1/readiness` = `erp_merge` fb205860; CI evidence = run 38724169019 (all jobs green).
Row-level data: [`G-campus.csv`](G-campus.csv). Read-only review: no code was built or changed.

Method: 319 utoipa routes (G + H) extracted and matched against `apps/api/tests/*.rs`, the web data layer
(`features/*/api.ts`) and `apps/web/e2e/*.e2e.mjs`; every `auth.require(..)` per handler listed; every
SQL statement touching `campus.*` without `tenant_id` listed; passing test names taken from the CI logs.
"api_tested = passed" means a named DB test that calls the endpoint passed in that run. "browser_tested =
partial" means the e2e only views the screen while the data is created through the API.
Customer validation is `pending` everywhere.

## Summary

62 rows (one per L2 workflow or workflow group).

| Status | Rows |
|---|---|
| implemented | 35 |
| partial | 13 |
| missing | 9 |
| deferred | 4 (G06 medical, G07 security, G08 housekeeping, G09 TGS — whole sub-domains) |
| mock-only | 1 (workspace drawers W-G00…W-G10) |

| Release readiness | Rows |
|---|---|
| ready | 3 |
| ready-with-gaps | 37 |
| not-ready | 16 |
| deferred | 6 |

Headline: the 8A/8B/8C/9E backend is broad and well tested at the API level (all 13 G-related DB tests
passed), but **every approval in production is auto-approved** (DEF-G-001). Student/staff self-service
for hostel, transport, service desk and room booking exists **only as an API** (no screens), and
G06–G09 plus a large part of the G01/G02/G03/G10 L2 workflows don't exist.

## DEFECTS

### DEF-G-001 · P1 · Approvals are hard-wired to auto-approve in production, even with Flowable
- Evidence: `modules/campus/src/lib.rs:89` (`approvals: ApprovalMode::AutoApprove` in `CampusDeps::into_state`);
  `apps/api/src/app.rs:869` mounts `sutra_campus::router(campus_deps(..))`, which always calls `into_state`;
  `modules/campus/src/shared/approval.rs:125-132` turns AutoApprove into `Approved` immediately.
  `SUTRA_WORKFLOW=flowable` (`apps/api/src/app.rs:962-985`) only swaps the WorkflowPort, so the domain still
  applies the decision in the submit transaction (actor `system:auto-approval`).
- Impact: node decommissioning, reservations, maintenance plans, hostel admission/policy/leave/transfer,
  route activation, library policy, **fine waivers**, library clearance and G00 governance decisions never
  wait for a human. The `AwaitDecision` path is exercised only by tests (`db_b8a/b/c`, `db_b9e`).
- Root cause: Batch 8 default never got tied to `config_b9::WorkflowMode`. The same line exists in every
  module (`modules/{academic,exams,finance,people,resources,services,student}/src/lib.rs`), so this is systemic.
- Fix: derive `ApprovalMode` from config (`AwaitDecision` when `SUTRA_WORKFLOW=flowable` or a new
  `SUTRA_APPROVALS=human` flag); add a DB test that builds the production router and asserts a pending state.

### DEF-G-002 · P1 · A librarian can waive any library fine alone (financial control bypass)
- Evidence: `modules/campus/src/library/fines.rs:475` (waiver needs only `LibraryManage`), `fines.rs:509`
  (`request_approval(FINE_WAIVER)`) → auto-approved through DEF-G-001; the decision endpoint `fines.rs:563`
  is never reached in production.
- Root cause: DEF-G-001 plus no amount threshold or second approver.
- Fix: once DEF-G-001 is fixed, also make the decider ≠ requester (DEF-G-004) and add a waiver audit report.

### DEF-G-003 · P2 · Student self-service hostel application and leave are approved automatically
- Evidence: `modules/campus/src/hostel/selfservice.rs:148` → `hostel/admission.rs:287` (`HOSTEL_ADMISSION`);
  `selfservice.rs:214` → `hostel/residency.rs:350` (`RESIDENT_LEAVE_APPROVAL`). The CI e2e records
  "self-service application HAP-000001 (approved)" right after the student POSTs it (`apps/web/e2e/campus.e2e.mjs:269`).
- Fix: as DEF-G-001. Until then, document that warden approval is not enforced.

### DEF-G-004 · P2 · No segregation of duties on decision endpoints
- Evidence: decision handlers check only the permission, never `decider != submitter`:
  `governance/cases.rs:1294`, `library/fines.rs:563`, `library/policy.rs:554`, `maintenance/plans.rs:631`,
  `hostel/policy.rs:450`, `transport/network.rs:971`. `grep` finds no requester/decider comparison in `modules/campus/src`.
- Fix: store `submitted_by` and refuse a decision by the same actor (403/409), with a test per decision type.

### DEF-G-005 · P2 · Approval permissions are inconsistent: some decisions skip `campus.approve`
- Evidence: `reservations/http.rs:771` (reservation decision needs `FacilityManage`) and
  `hostel/residency.rs:595` (leave decision needs `HostelManage`). Other decisions need `CampusApprove`
  (e.g. `hostel/residency.rs:1118`, `infrastructure/nodes.rs:1648`).
- Impact: the operator who manages bookings or leave approves their own queue.
- Fix: use `Permission::CampusApprove` (plus step-up where the contract asks) for every `/decision`.

### DEF-G-006 · P2 · Decision endpoints with no UI (would deadlock once human approval is on)
- Evidence: no web caller for `POST /maintenance-plans/{id}/decision` (`maintenance/plans.rs:620`),
  `/room-transfers/{id}/decision` (`hostel/residency.rs:1107`), `/reservations/{id}/decision`
  (`reservations/http.rs:760`), `/infrastructure-nodes/{id}/decommission-decision` (`infrastructure/nodes.rs:1637`).
  `apps/web/app/features/campus/facilities/api.ts` and `residence/api.ts` contain no such paths.
- Fix: add Approve/Reject row actions (like `library/api.ts:82` policyDecision) and e2e steps.

### DEF-G-007 · P2 · Backend-only features: self-service and several admin actions are not reachable from the web
- Evidence: 57 campus routes have no frontend caller. Examples: `/me/hostel*` (`hostel/selfservice.rs:109-246`),
  `/me/transport*` (`transport/selfservice.rs:85-212`), `/me/tickets*` (`servicedesk/http.rs:703,767,784`),
  `/me/reservations` (`reservations/http.rs:937,969`), `/tickets/{id}/merge` (`servicedesk/http.rs:1211`),
  `/labs/{id}/batch-allocations` (`labs/http.rs:1367`), `/practical-sessions/{id}/postpone` (`labs/http.rs:1262`),
  `/maintenance-cycles/{id}/defer|complete` (`maintenance/plans.rs:990,1067`), capabilities/availability PUT
  (`infrastructure/nodes.rs:948,1086`), transport complaints (`transport/passengers.rs:1270,1288`),
  `PATCH /catalogue-records/{id}` (`library/catalogue.rs:400`). The router (`apps/web/app/routes.ts`) has no
  student hostel/transport/helpdesk route; the student nav only has Library.
- Fix: add student/faculty pages (Hostel, Transport, Help desk, Room booking) and the missing admin actions.

### DEF-G-008 · P3 · Work-order approval process key defined but never used
- Evidence: `modules/campus/src/shared/approval.rs:34` `WORK_ORDER_APPROVAL` (also in `process::ALL` and a BPMN
  file), no caller in `modules/campus/src` (`grep process::WORK_ORDER_APPROVAL` returns nothing).
  `maintenance/workorders.rs` creates and assigns work orders with no approval.
- Fix: either wire approval for cost-bearing/vendor work orders (L2-G01 Work Order Workflow) or remove the key and BPMN.

### DEF-G-009 · P3 · Follow-up SQL statements without a tenant filter (defense-in-depth)
- Evidence: 141 statements on `campus.*` filter by `id` only, e.g. `hostel/residency.rs:645`,
  `transport/network.rs:825` (`DELETE FROM campus.route_stop WHERE route_version_id = $1`),
  `library/catalogue.rs:558`, `maintenance/plans.rs:937`. Each one runs after a tenant-checked `head()/load()`
  in the same transaction, so I found **no exploitable cross-tenant path**. There is also no RLS on the `campus` schema.
- Fix: add `AND tenant_id = $n` to id-only statements (cheap), or enable RLS on the schema.

### DEF-G-010 · P3 · `POST /driver-assignments/{id}/end` untested and not in UI
- Evidence: `modules/campus/src/transport/fleet.rs:474`. It's the only campus route with no reference in
  any API test or the web app.
- Fix: add it to `db_b8b::transport_routes_passengers_passes_and_trips` and a row action.

### DEF-G-011 · P2 · Workspace drawer fakes workflow approval for unbuilt sub-domains
- Evidence: `apps/web/app/routes/workspace.tsx:150-160` — "Advance to …" changes React state only and
  toasts "`<workflow>`: moved to Approved". Its "Records" tab is always empty (`workspace.tsx:126-138`).
  It's shown for W-G06 medical, W-G07 security, W-G08 housekeeping and W-G09 TGS, which have no backend.
- Impact: a user can believe a medical/security request was approved.
- Fix: hide the advance button (or the drawer) for workspaces without a module, labelled "coming soon".

### DEF-G-012 · P3 · Approval-state vocabulary still split (deprecated `approvalPending`)
- Evidence: `docs/batches.md:207` (8B hostel/transport keep `approvalPending`); `apps/web/app/features/campus/approval.ts`
  maps legacy values.
- Fix: drop `approvalPending` from 8B views after the web reads only `approvalState`.

### DEF-G-013 · P2 · Rescheduling a confirmed booking bypasses approval (self-service included)
- Evidence: `modules/campus/src/reservations/http.rs:780-850` — `reschedule` (permission `CampusSelfService`,
  owner check `may_act`) moves `confirmed → rescheduled` with a new date, time and attendees without a new
  `RESERVATION_APPROVAL`. Creation does request approval (`reservations/http.rs:467-478`).
- Fix: re-request approval when date, space or attendees change on an approval-requiring booking.

## TEST GAPS
- Production approval wiring: no test builds the real router and asserts that a decision stays pending (DEF-G-001).
- Browser: labs, maintenance plans, leave, transfer, inspection, mess, trips, vehicles/drivers, clearance,
  governance config/evidence, lost copies — no e2e step. Transport and governance are only viewed (data seeded by API).
- Self-service `/me/*` hostel/transport/tickets/reservations are tested only at the API level, because no UI exists.
- `POST /driver-assignments/{id}/end` — no test (DEF-G-010).
- No negative test for decider == submitter (DEF-G-004) or for rescheduling without re-approval (DEF-G-013).
- No test for the SLA breach sweep or the overdue library sweep as scheduled jobs (both are manual triggers).
- Cross-tenant tests: `campus_tenant_isolation_permissions_and_idempotency` is in `db_b2b.rs` (Domain B campus).
  For Domain G, isolation is checked inside the slice tests but there's no dedicated G-wide isolation matrix.

## DOC DISCREPANCIES
- `README.md:43` still labels `campus/ (G)  services/ (H)` as "scaffolds"; both are active Batch 8/9 modules.
- `docs/batches.md` / `docs/batch9-contracts.md:45` say Flowable "is proved and switched on per environment".
  In fact switching it on doesn't stop auto-approval for Domain G (DEF-G-001).
- `docs/batches.md:127` claims 8A covers "service desk (… requester verification)" and room reservations. Both are
  API-only for requesters (DEF-G-007).
- `docs/batches.md:128` claims transport "routes + versions … drivers". `driver-assignments/{id}/end` has no test or UI.
- Process key `campus.work_order_approval` and its BPMN exist without any use (DEF-G-008). The docs imply work-order approval.
- L2-G01/G02/G03/G05/G10 list workflows (emergency, vendor/AMC, utility, inspection, lab safety/incident/consumables,
  hostel visitor/discipline/check-in, library acquisition/reading room/verification, G10 compliance/renovation/DR).
  `docs/batches.md` only says "depth of every in-scope area" is deferred, without naming them.
