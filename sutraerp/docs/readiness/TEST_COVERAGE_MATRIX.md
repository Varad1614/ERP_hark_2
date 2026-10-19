# SutraERP test coverage matrix (Part 1 · Stage 1)

Counts are feature rows from [`FEATURE_INVENTORY.csv`](FEATURE_INVENTORY.csv) (`f6ffff7`). **Passed** = evidence that a named test
passed in CI run 38724169019 (API: Rust DB/HTTP test; browser: e2e spec in the CI matrix `finance, student, academic, ui, exams,
people, campus, reskin2`). **Partial** = only part of the feature is driven. Screenshot-only visits count as not tested.

| Domain | Rows | API passed | API partial | API not tested | Browser passed | Browser partial | Browser not tested |
|---|---:|---:|---:|---:|---:|---:|---:|
| A-platform | 45 | 26 | 0 | 19 | 3 | 0 | 42 |
| B-institution | 43 | 41 | 0 | 2 | 0 | 0 | 43 |
| C-student | 50 | 50 | 0 | 0 | 26 | 0 | 24 |
| D-academic | 45 | 44 | 0 | 1 | 30 | 0 | 15 |
| E-people | 56 | 48 | 0 | 8 | 18 | 0 | 38 |
| F-finance | 40 | 35 | 0 | 5 | 21 | 0 | 19 |
| G-campus | 62 | 47 | 1 | 14 | 14 | 4 | 44 |
| H-services | 34 | 21 | 0 | 13 | 8 | 5 | 21 |
| I-exams | 36 | 34 | 0 | 2 | 20 | 0 | 16 |
| J-resources | 28 | 19 | 0 | 9 | 6 | 0 | 22 |
| **Total** | **439** | **365** | **1** | **73** | **146** | **9** | **284** |

Note: `missing` / `deferred` rows are counted as not tested (nothing to test). Customer validation: pending for all rows.

## Tests that encode wrong behaviour

These pass today **because** of a defect; they must change when the defect is fixed.

| Test | What it asserts | Defect |
|---|---|---|
| `apps/web/e2e/finance.e2e.mjs:342` step "5c: chart of accounts, manual journal auto-approved and posted" | A manual journal is approved by the system with no approver | DEF-F-001 / DEF-X-001 |
| `apps/api/tests/db_b4b.rs:1016-1018` (`sessions_attendance_thresholds_corrections_and_syllabus`) | A faculty-raised attendance correction comes back `status = "applied"` (applied as system, no decision, no step-up) | DEF-D-002 |
| `apps/web/e2e/student.e2e.mjs:656` ("Send for approval" → row "Approved") | Admission approval completes instantly | DEF-C-001 |
| `apps/web/e2e/people.e2e.mjs:243` "submitted (approved)", `:425` "validated → approved → finalised" | Staff engagement and payroll run approve themselves | DEF-E-001 |
| `apps/web/e2e/campus.e2e.mjs:269` (self-service hostel application recorded as approved right after the POST); `:337-347` supplier/requisition usable without a decision | Hostel self-service and procurement auto-approve | DEF-G-003, DEF-J-001 |
| `apps/web/e2e/campus.e2e.mjs:522` "submitted to the regulator and acknowledged" | Names a manual reference entry as a regulator submission | DEF-H-003 |
| `apps/api/tests/api.rs:166-181` | Catalog/module endpoints answer 200 without a session | DEF-A-006 |
| All `*await_decision*` DB tests (`db_b2b.rs:998`, `db_b3c`, `db_b5a.rs:1395`, `db_b5b.rs:1676`, `db_b5c.rs:1617`, `db_b7a.rs:1933`, `db_b7b.rs:1400`, `db_b7c.rs:1121`, `db_b8a/b/c`, `db_b9e`) | Build `AwaitDecision` state by hand; no test boots the production router and asserts approvals stay pending, so the production mode is untested | DEF-X-001 |

## CI wiring gaps

- `apps/web/e2e/passkey.e2e.mjs` (and `package.json` `e2e:passkey`) is **not in the CI e2e matrix** (`.github/workflows/sutraerp-ci.yml:174`) — DEF-A-008.
- Live Flowable and OpenSearch tests (`apps/api/tests/live_flowable.rs`, `live_opensearch.rs`) run **only in `.github/workflows/sutraerp-data.yml`**
  (lines 124, 138-141), not in the main CI run 38724169019; the main run covers only the BPMN-file and wiring tests in `db_b9c.rs`.
- Domain J has no e2e job of its own (coverage rides on `campus.e2e.mjs`); Domain B screens are only screenshotted.
- The e2e suite runs with dev auth only: no OIDC or step-up path is exercised in the browser.

## Notable untested areas per domain

- **A-platform**: TOTP enrolment, step-up dialog, passkey and OIDC sign-in, notifications/notices/to-dos transitions (screenshots only); production refusal of the no-op adapter; two-institution search; brute-force/rate limits; no architecture test for tenant predicates.
- **B-institution**: No browser test of any Domain B screen (harness drives registration → activation via API); `submit` without decide permission; cross-institution isolation; AwaitDecision only with hand-built state.
- **C-student**: Readmitting the same source twice; duplicate applications; decider ≠ requester; browser: cancellation, withdrawal, revocation, expiry, readmission, suspension, transfers, identity correction, duplicates, holds, probation, academic break, credit transfer, programme migration, guardian lifecycle, qualification correction; OIDC/step-up paths; ABC/scholarship sync only against Noop.
- **D-academic**: Pending / self-decided / step-up / locked-session attendance corrections; enrolment override under AwaitDecision; concurrent capture; browser: offering lifecycle beyond happy path, sections, windows, quotas, waitlist, faculty changes, timetable revision, corrections, marking/moderation, monitoring exceptions.
- **E-people**: Production approval mode; payroll self-approval; browser: separation, probation, certificates, onboarding decision, leave adjustment/encashment/cases, payroll adjustments, retro/off-cycle, final settlement, payslip reissue; no payroll regression/property suite; no concurrency tests (leave balance race, parallel payroll, double payslip).
- **F-finance**: Production approval mode; self-approval of refunds/adjustments/write-offs; duplicate `external_ref`; capture after cancel; production refusal of `SUTRA_PAYMENT_GATEWAY=mock`; concurrency (refund race, webhook + confirm); browser: chargeback, recovery, bulk collection, dunning, write-off decision, reversal, aid recover, period reopen/year close, bank settlements.
- **G-campus**: Production approval wiring; decider == submitter; reschedule without re-approval; `/driver-assignments/{id}/end` (no test at all); `/me/*` self-service only at API level; SLA/overdue sweeps as scheduled jobs; no G-wide isolation matrix; browser: labs, maintenance plans, hostel leave/transfer/inspection, mess, trips, vehicles, clearance, governance.
- **H-services**: Production wiring leaves H07–H09 pending; self-approval; browser: H08 correction/closure, accreditation, dashboard rollback/assignments, `/me/dashboards`; search UI absent; `live_opensearch` not in the main CI run; no browser assertion for notifications.
- **I-exams**: Multi-term / supplementary CGPA (I-022 not tested); pending malpractice at compute time; release-after-approval with changed content; maker-checker; rounding boundaries; scheduled publication; browser: malpractice, moderation, withheld/release, correction/withdrawal, supplementary, completion, closure; conduct done via API; several endpoints with no DB-test reference (paper generate, venue release, grade-card/hall-ticket withdraw, invigilation confirm/release).
- **J-resources**: Self-approval; PATCH on an active supplier's bank details; high PO price on a low estimate; concurrent invoice matching; `vendor-invoices/{id}/decision`, `sourcing-events/{id}/decision`, goods-receipt reject; browser creates supplier/requisition/PO/stock/payment via API, screens only accept/match/view; no resources e2e job of its own.
