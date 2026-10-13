# Batch 7 — shared contracts (People & workforce, Domain E)

**Status:** frozen at `batch7/foundation`. This file is the single source of truth for the three
slices **7A**, **7B** and **7C**. If a slice needs to change anything in §2–§11, it says so in its
PR description ("contract change: …") instead of silently editing a shared file; the integrator
applies it on top of the merge.

Architecture inputs: L2-E00 review, L2-E01 (workforce & engagement), L2-E02 (position & assignment
+ amendment 2026-08-26), L2-E03 (faculty workforce), L2-E04 (HR), L2-E05 (leave), L2-E06 (payroll),
L2-E07 (employee profile); L3/L4 E-001 … E-075 (in-scope list §1); ADR-L5-0003 (module boundaries),
-0004 (scope), -0006/-0007 (owner-local tx + outbox), -0011/-0035 (event versioning / envelope),
-0023/-0042 (WorkflowPort / Flowable), -0032 (decimals: `rust_decimal`), -0034 (API profile), -0036
(schemas, business timezone); L6 workspaces W-E01 … W-E07; screens Admin People, Faculty Leave (My HR).

Plan line (Batch 7): *staff records, recruitment and onboarding, leave and approvals, staff
directory; payroll inputs, then the payroll computation engine. Screens: Faculty Leave; Admin People.*

**Lessons from Batches 3–6 applied here:** every cross-slice and cross-domain dependency is a
**typed port or foundation helper defined up front** (§6) with a real implementation already wired,
so no slice waits for another; the shared rows (staff records, engagements, the leave ledger,
attendance days) have **one** write path each (`shared::{staff, lifecycle, leave, attendance}`);
money and leave days are `rust_decimal` everywhere (no floats, ever; strings on the wire);
**CI-first**: no heavy local builds (§11).

---

## 1. Slices and scenario coverage (one wave: 7A + 7B + 7C in parallel)

**43 of 75 L3 scenarios in scope (57 %)** — the core flow end to end: staff record → engagement →
position + reporting manager → faculty establishment / teaching eligibility → onboarding /
probation / separation → leave policy → accrual → request → approval → reservation → consumption →
cancellation / adjustment / carry forward / encashment / exceptions → long leave, attendance days,
faculty workload → payroll configuration → salary assignment → run → calculation → approval →
finalisation → Finance posting → payslips → adjustments / retro / final settlement / off-cycle.

| Slice | Scope | L3/L4 scenarios (25 / 12 / 6) |
|-------|-------|-------------------------------|
| **7A** | **Staff records, recruitment & onboarding, positions.** Staff record + core profile (draft → pending_verification → verified → active; maintenance, governed correction with history, archive on separation), qualifications and profile documents with verification, **engagements** (pending → active through `ENGAGEMENT_ACTIVATION`; modification, transfer between org units / campuses, suspension → restoration, closure → exit, rejoining = a new engagement on the same staff record, duplicate workforce records → canonical (`merged`)), **positions** (institution positions per org unit, assignment → change / reassignment → completion / termination) and the **reporting manager** (`staff_engagement.reports_to_staff_id`), **faculty** establishment and teaching eligibility (what Domain D sees through `StaffDirectory`), suspension / reactivation, **recruitment → onboarding** (requisition → candidate → offer accepted → onboarding case: documentation → verification → ready → joined, which creates the staff record + pending engagement), **probation** review → confirmation / extension, **separation** case → clearance → exit (closes the engagement, archives the profile), **certificate** requests (service / experience certificate). Screens: Admin → People tabs `staff`, `recruitment`, `positions`; My HR `MyProfilePanel`. | E-001 … E-004, E-007 … E-009, E-011 … E-013, E-018, E-026, E-029, E-031, E-033, E-039, E-041, E-043, E-058 … E-060, E-064 … E-066, E-068 |
| **7B** | **Leave, attendance & workload.** Leave policy versions (leave types with entitlement, accrual frequency, carry-forward cap, expiry, encashment cap, paid / unpaid, `requires_balance`; draft → under_review → approved → active → superseded), periodic **accrual**, **leave request → final decision (the reference approval of Batch 7, `LEAVE_APPROVAL`)** — approver = administrator with `people.leave.decide` **or** the applicant's reporting manager (`EngagementSummary.reports_to_staff_id`), reservation on approval, consumption as the leave dates pass (one ledger entry per calendar-month segment), cancellation of approved / reserved leave (release / reversal), governed balance adjustment, year-end carry forward + expiry, encashment eligibility decision (an `encashment` ledger entry 7C pays), leave exceptions, leave case coordination (HR case around special / long leave), **long leave** (engagement `active ↔ long_leave` through the shared lifecycle helper), **staff attendance days** (present / absent / half_day / on_leave / holiday / weekly_off; bulk capture, correction), **faculty availability + workload plan** (Domain D teaching load through `FacultyWorkloadPort` vs plan / policy maximum). Screens: Admin → People tabs `leave`, `attendance`; My HR `MyLeavePanel`. | E-006, E-030, E-034, E-044 … E-052 |
| **7C** | **Payroll.** **Configuration** (pay components: earning / deduction / employer contribution, calculation basis fixed / % of basic / % of gross, statutory rules PF / ESI / professional tax / TDS as governed rate + ceiling rules, rounding) draft → review → approved → active → superseded; salary structures and **salary assignments** per engagement (effective dated). **Payroll periods** and **runs** (`regular`, `off_cycle`, `final_settlement`, `retro`): created → calculating → calculated → validating → pending_approval → approved → finalized → posted → closed (failed → reprocessing); inputs only from `WorkforceDirectory::payable_engagements` + `LeaveReader::payroll_inputs` (LOP = unpaid leave days + absent days; encashment entries); per-employee **payroll results** (earnings, deductions, employer contributions, net pay, calculation snapshot; immutable once finalised), approval (`PAYROLL_APPROVAL`, `people.payroll.approve`), **Finance posting through `PayrollPostingPort`**, **payslips** (`PS-` numbers, `/me/payslips`). **Adjustments** (requested → validated → pending_approval → approved → calculated → applied → closed; a corrective delta, never an edit of a finalised result), **retro pay** (past-effective salary change → arrears lines), **separation final settlement** (pro-rata pay + leave encashment + recoveries), **off-cycle** runs. Screens: Admin → People tab `payroll`; My HR `MyPayslipsPanel`. | E-053 … E-057, E-074 |

**Deferred (32 scenarios), recorded here and in `docs/batches.md`:**

| Scenarios | Area | Lands in |
|-----------|------|----------|
| E-005, E-010 | Deputation lifecycle; bulk organisational transfer | 7D (workforce depth) |
| E-014 … E-017 | Supplementary occupancy, delegated authority / additional charge, committee membership, responsibility change / withdrawal | 7D |
| E-019 … E-025 | Reporting relationship change, service jurisdiction assignment / change, organisational authority resolution, occupancy conflict, assignment suspension / freeze, expiry → archive | 7D |
| E-071 … E-073 | Position assignment administrative correction, rollback, bulk operation | 7D |
| E-027, E-028, E-032 | Faculty capability lifecycle, qualification → verified academic use, faculty closure / archive | 7D |
| E-035 … E-038, E-040, E-042 | HR request, grievance, promotion case, transfer case, contract renewal, HR compliance case | 7D (HR depth) |
| E-061 … E-063, E-067, E-069, E-070, E-075 | Certifications, skills, languages, HR case outcome → profile, profile suspension / restoration, duplicate **profile** merge, archived profile restoration | 7D (profile depth) |

Domains **G** (Campus rooms, Batch 8) and **H** (services) are not involved. Exams invigilators /
evaluators stay login subjects (Batch 6); they gain staff facts only through `StaffDirectory`.

## 2. Where the code lives: module and file ownership

**Decision:** every Batch 7 aggregate lives in the existing crate `modules/people` (`sutra-people`,
domain E) and the PostgreSQL schema `people`. No new crates (`apps/api/tests/architecture.rs` fixes
exactly ten domain crates). `sutra-people` depends only on `sutra-kernel`, `sutra-contracts`,
`sutra-web` and `sutra-persistence` and **never** contains `FROM/JOIN/INTO/UPDATE <other schema>.`
(the architecture test fails): Domain B data comes through `InstitutionReferencePort`, Domain D
through `FacultyWorkloadPort`, Domain F through `PayrollPostingPort` (§6). Cross-slice reads inside
Domain E go through the **foundation ports / helpers** (`state.directory`, `state.leave`,
`shared::rows::*`, `shared::leave::balance`) for the shared tables, and may query sibling slice
tables of the `people` schema **read-only** when a port would be pointless (same crate); you never
write another slice's tables.

### 2.1 Ownership map (create/edit freely inside your own paths)

| Path | Owner |
|------|-------|
| `modules/people/src/{workforce,positions,hr,profile}/**` | **7A** |
| `modules/people/src/{leave,attendance,workload}/**` | **7B** |
| `modules/people/src/{payroll,payslips}/**` | **7C** |
| `crates/contracts/src/people/events/workforce.rs` | **7A** |
| `crates/contracts/src/people/events/leave.rs` | **7B** |
| `crates/contracts/src/people/events/payroll.rs` | **7C** |
| `modules/platform/src/inbox/rules/people_7a.rs` / `people_7b.rs` / `people_7c.rs` | 7A / 7B / 7C |
| `migrations/202611100000[1-2][0-9]_people_*.sql` (`…010` – `…029`) | **7A** |
| `migrations/202611100000[3-4][0-9]_people_*.sql` (`…030` – `…049`) | **7B** |
| `migrations/202611100000[5-6][0-9]_people_*.sql` (`…050` – `…069`) | **7C** |
| `apps/api/tests/db_b7a.rs` / `db_b7b.rs` / `db_b7c.rs` | 7A / 7B / 7C |
| `apps/web/app/features/people/workforce/**` | **7A** |
| `apps/web/app/features/people/leave/**` | **7B** |
| `apps/web/app/features/people/payroll/**` | **7C** |
| `apps/web/e2e/people.e2e.mjs` regions `// ==== 7X begin` … `// ==== 7X end` (created by the first slice that needs it, from `exams.e2e.mjs`; the integrator owns the file header and the CI matrix entry) | each slice its own regions |
| README region `<!-- batch7:7X:begin … end -->`; `docs/batches.md` status cell `<!-- batch7:7X:status -->` | each slice its own region |

### 2.2 Shared files: foundation-owned, do not edit

`modules/people/src/lib.rs` (router list already mounts every slice), `modules/people/src/shared/**`,
`modules/people/src/{directory,staff_port,staff}.rs`, `modules/people/src/staff_directory/**`,
`crates/contracts/src/people/{mod,ids,status,ports,staff}.rs`,
`crates/contracts/src/people/events/mod.rs`, `crates/contracts/src/academic/ports.rs` (Batch 7
block), `crates/contracts/src/finance/ports.rs` (Batch 7 block + `source::PAYROLL_RUN`),
`modules/academic/src/faculty_workload.rs`, `modules/finance/src/payroll_postings.rs`,
`crates/kernel/src/auth.rs` (all Batch 7 permissions already added),
`modules/platform/src/inbox/rules/mod.rs`, `apps/api/src/{app,openapi}.rs` (tags + `people_deps`
already added), `apps/api/tests/architecture.rs`, `apps/web/app/routes.ts`,
`apps/web/app/lib/{roles,workspace-screens}.ts`, `apps/web/app/features/people/{index.ts,Placeholder.tsx,people.test.tsx}`,
`apps/web/app/routes/admin/people.tsx`, `apps/web/app/routes/faculty/leave.tsx`,
`apps/api/tests/support/{mod,b7}.rs`, `apps/api/tests/db_b7_foundation.rs`,
`migrations/20261110000000_people_batch7_foundation.sql`, `.github/workflows/sutraerp-ci.yml`.
Domain B, C, D, F and I code (`modules/{institution,student,academic,finance,exams}/**`) is **off
limits** to all three slices (a needed change there is a contract change for the integrator).

If you cannot avoid touching one of these, keep the edit **append-only** at the end of the
relevant list/block, add a `// 7X:` (or `<!-- 7X: -->` / `-- 7X:`) comment, and list it in your PR
description. Examples: a new id at the end of your slice's block in `ids.rs`; a new number kind at
the end of your block in `shared::numbering::kind`; a new process key in your block of
`shared::approval::process`; a new variant in `status.rs` (contract change — say so); a new port
method (contract change — the foundation adapter must implement it).

### 2.3 Generated files

`openapi/openapi.json` and `packages/api-client/src/schema.d.ts` are **generated**. CI's
`OpenAPI drift` job regenerates them and uploads them as the artifact `openapi-regenerated`;
**commit the artifact's files** (download it through the GitHub API, §11) instead of generating
locally. On merge, **never hand-merge them**: take either side, then regenerate. `Cargo.lock` /
`pnpm-lock.yaml`: no new dependencies are expected (the foundation gave `sutra-people` sqlx / uuid /
chrono / rust_decimal / serde_json / tracing / persistence). If you add one, say so in the PR.
`CHANGELOG.md`: slices don't edit it.

## 3. Database

### 3.1 Migrations and numbers

| Range | Owner |
|-------|-------|
| `20261110000000_people_batch7_foundation.sql` | foundation (applied): `people.staff_member`, `staff_engagement`, `leave_ledger_entry`, `attendance_day`, `number_sequence`, `status_history`; `people.forbid_history_change()`; append-only / no-delete triggers; restated `sutra_people` grants with `UPDATE/DELETE/TRUNCATE` **revoked** on the ledger and history tables |
| `20261110000010` … `20261110000029` | 7A (positions, assignments, faculty profile / eligibility, requisitions, candidates, onboarding cases, probation reviews, separation cases, certificate requests, qualifications, profile documents, profile corrections) |
| `20261110000030` … `20261110000049` | 7B (leave policies + types, leave requests, accrual / year-end runs, adjustments, encashment decisions, leave exceptions, leave cases, long leave, workload plans) |
| `20261110000050` … `20261110000069` | 7C (payroll configuration + components + statutory rules, salary structures / assignments, payroll periods, runs, results + lines, adjustments, payslips, final settlements, encashment payments) |

Base `20261110…` is after the last Batch 6 migration (`20261105000050_exams_results.sql`).
Reserved names are **not** pre-created as empty files (sqlx checksums applied migrations). Name
files `<version>_people_<what>.sql`. Never edit a migration after it has been applied anywhere; add
the next number. New tables pick up the `sutra_people` runtime grants automatically (default
privileges of `20261014000003_runtime_db_roles.sql` cover schema `people`); an append-only slice
table additionally gets `REVOKE UPDATE, DELETE, TRUNCATE ON people.<t> FROM sutra_people;` plus a
`BEFORE UPDATE OR DELETE` trigger reusing `people.forbid_history_change()`.

**Decimal rules (all slices):** money `numeric(14,2)` (INR, two places), rates / percentages
`numeric(7,4)`, leave days `numeric(6,2)` (half days allowed), hours `numeric(5,2)`; Rust uses
`rust_decimal::Decimal` end to end; **no `f32`/`f64`**. Computed money is rounded **once** per
line with `shared::decimal::round2` (half away from zero); statutory rounding rules are 7C
configuration. On the wire money / days / rates / hours are **JSON strings** (`"45250.00"`,
`shared::decimal::fmt2`) with `#[schema(value_type = String, example = "45250.00")]`.

Table conventions: `id uuid PRIMARY KEY DEFAULT uuidv7()`, `tenant_id uuid NOT NULL`,
`institution_id uuid NOT NULL` (plain uuid — **no FK into `institution`**), `staff_id uuid
REFERENCES people.staff_member(id)` / `engagement_id … people.staff_engagement(id)` where the row
belongs to one (FKs to the **foundation** tables are allowed for every slice), `org_unit_id` /
`campus_id` / `offering_id` / `journal_entry_id` **plain** (other domains, no FK), `status text NOT
NULL CHECK (status IN (...))` with the wire names of §4.2, `version bigint NOT NULL DEFAULT 1`,
`created_at/updated_at timestamptz`, `process_instance_id text` on every aggregate with an approval
step. Business dates (joining, leave dates, payroll periods) are `date` in the institution's
timezone (`shared::time::business_today`); instants are `timestamptz`. **No foreign keys across
slices** (plain uuid + validate by reading).

### 3.2 The shared People tables (foundation)

- `people.staff_member` — the canonical workforce member + core profile: `staff_number`
  (`EMP-000001`), `subject_ref` (login subject, **unique per tenant** among live records — the key
  Domain D / self-service resolve), `given_name`, `family_name`, `display_name`, `work_email`,
  `phone`, `date_of_birth`, `gender`, `staff_category` (StaffCategory), `canonical_staff_id`
  (duplicate resolution, 7A), `status` (StaffStatus), `status_reason`, `verified_at`,
  `process_instance_id`, `version`. No physical delete.
- `people.staff_engagement` — one employment / engagement of a staff member: `engagement_number`
  (`ENG-000001`), `org_unit_id` / `campus_id` (Domain B, plain), `staff_category`,
  `employment_type` (EmploymentType), `designation`, `grade` (pay level code 7C maps to a salary
  structure), `start_date`, `end_date` (planned end of a contract), `last_working_date` (exit),
  `probation_end_date`, `reports_to_staff_id` (reporting manager, 7A E-018; 7B routes manager
  approvals with it), `rejoin_of_engagement_id` (E-008), `status` (EngagementStatus). **One live
  engagement per staff member** (unique index on `staff_id` where status ∉ `closed, cancelled`).
- `people.leave_ledger_entry` — **append-only** leave ledger (L2-E05 "no direct balance
  mutation"): `staff_id`, `engagement_id`, `leave_type_id` (7B, plain) + `leave_type_code`
  snapshot, `is_paid`, `kind` (LeaveEntryKind), signed `days numeric(6,2)` (sign fixed by the kind,
  §4.2), `effective_on`, `leave_year`, `source_type` / `source_id` / `posting_key` (**idempotency
  key**, unique per tenant), `reverses_entry_id`, `reason`, actor. Balances are projections
  (`shared::leave::balance`, `LeaveReader::balances`).
- `people.attendance_day` — one row per staff member and work date: `status`
  (AttendanceDayStatus), `source` (AttendanceSource), `leave_request_id` (7B, plain), `remark`,
  `version`; every change appends `people.status_history` (`attendance_day`).
- `people.number_sequence` — gap-free per-institution counters (`shared::numbering`).
- `people.status_history` — append-only: every status change of the shared aggregates (and of
  slice aggregates, via `shared::lifecycle::record_history`).

### 3.3 Write rules for the shared tables

| Change | Who | How |
|--------|-----|-----|
| INSERT staff records | **7A** (onboarding, manual creation, rejoining reuses the record) | **only** `shared::staff::insert_staff` (numbering, duplicate subject → 409) |
| Edit staff profile fields | **7A** | own repo code (name, contact, category …) with `ModifiedV1`; corrections keep history (7A table) |
| `staff_member.status` | **7A** | **only** `shared::lifecycle::change_staff_status` on a row locked with `shared::rows::lock_staff` |
| INSERT engagements | **7A** | **only** `shared::staff::insert_engagement` (one live engagement → 409) |
| Edit engagement attributes (org unit, designation, grade, dates, reporting manager) | **7A** | own repo code while the engagement is not `closed | cancelled` |
| `staff_engagement.status` | **7A** every move; **7B** only `active → long_leave` and `long_leave → active` | **only** `change_engagement_status` (`lock_engagement`) |
| `leave_ledger_entry` | **7B** | **only** `shared::leave::{post_entry, reverse_entry}` (append-only; idempotent on `(source_type, source_id, posting_key)`) |
| `attendance_day` | **7B** (7B's leave flow marks `on_leave` days too) | **only** `shared::attendance::record_day` |
| Read staff / engagements / leave / attendance | all | `state.directory` (`WorkforceDirectory`), `state.leave` (`LeaveReader`), `shared::rows::*`, `shared::leave::balance` |

### 3.4 Document numbers (`shared::numbering::kind`)

`staff` `EMP`, `engagement` `ENG` (foundation); `requisition` `REQ`, `onboarding` `ONB`,
`separation` `SEP`, `certificate` `CERT` (7A); `leave_request` `LV`, `leave_exception` `LEX`
(7B); `payroll_run` `PR`, `payslip` `PS`, `settlement` `FNF` (7C). Gap-free inside committed
transactions.

### 3.5 Slice table names (schema `people`)

| Slice | Tables (prefixes a slice may extend) |
|-------|--------------------------------------|
| 7A | `position`, `position_assignment`, `faculty_profile`, `requisition`, `candidate`, `onboarding_case`, `probation_review`, `separation_case`, `certificate_request`, `staff_qualification`, `staff_document`, `profile_correction` (prefixes `position*`, `faculty_*`, `requisition*`, `candidate*`, `onboarding_*`, `probation_*`, `separation_*`, `certificate_*`, `staff_qualification*`, `staff_document*`, `profile_*`, `engagement_*`) |
| 7B | `leave_policy`, `leave_type`, `leave_request`, `leave_request_segment`, `leave_accrual_run`, `leave_year_end_run`, `leave_adjustment`, `leave_encashment`, `leave_exception`, `leave_case`, `long_leave`, `workload_plan` (prefixes `leave_*`, `long_leave*`, `workload_*`, `attendance_*` except the foundation `attendance_day`) |
| 7C | `payroll_config`, `pay_component`, `statutory_rule`, `salary_structure`, `salary_structure_line`, `salary_assignment`, `payroll_period`, `payroll_run`, `payroll_result`, `payroll_result_line`, `payroll_adjustment`, `payslip`, `final_settlement`, `encashment_payment` (prefixes `payroll_*`, `pay_*`, `statutory_*`, `salary_*`, `payslip*`, `final_settlement*`, `encashment_payment*`) |

## 4. Kernel and contract types (already in the code)

### 4.1 Ids: `sutra_contracts::people::ids`

`InstitutionId`/`TenantId` re-exported, plus:
- **foundation:** `StaffMemberId`, `StaffEngagementId`, `LeaveLedgerEntryId`, `AttendanceDayId`
- **7A:** `PositionId`, `PositionAssignmentId`, `FacultyProfileId`, `RequisitionId`, `CandidateId`, `OnboardingCaseId`, `ProbationReviewId`, `SeparationCaseId`, `CertificateRequestId`, `StaffQualificationId`, `StaffDocumentId`, `ProfileCorrectionId`
- **7B:** `LeavePolicyId`, `LeaveTypeId`, `LeaveRequestId`, `LeaveAccrualRunId`, `LeaveYearEndRunId`, `LeaveAdjustmentId`, `LeaveEncashmentId`, `LeaveExceptionId`, `LeaveCaseId`, `LongLeaveId`, `WorkloadPlanId`
- **7C:** `PayrollConfigId`, `PayComponentId`, `StatutoryRuleId`, `SalaryStructureId`, `SalaryAssignmentId`, `PayrollPeriodId`, `PayrollRunId`, `PayrollResultId`, `PayrollAdjustmentId`, `PayslipId`, `FinalSettlementId`

All are `typed_id!` newtypes: UUIDv7, serde-transparent, OpenAPI `string/uuid`.

### 4.2 Lifecycle states: `sutra_contracts::people::status`

Enums with `ALL`, `as_str()`, `parse()`, serde/OpenAPI `snake_case`; also the DB `CHECK` values.
The **shared** machines carry `can_transition` in contracts (illegal → `409`):

| Machine | Values | Legal moves (summary) | Helpers |
|---------|--------|----------------------|---------|
| `StaffStatus` (L2-E07 §10) | `draft, pending_verification, verified, active, suspended, merged, archived` | draft → pending_verification → verified → active; pending_verification → draft; draft → active (direct creation by HR with verified identity); active ↔ suspended; active / suspended → archived; archived → active (restoration); any live state → merged (duplicate, E-009) | `is_live()` (not merged / archived) |
| `EngagementStatus` (L2-E01) | `pending, active, suspended, deputed, long_leave, closed, cancelled` | pending → active / cancelled; active ↔ suspended; active ↔ deputed; active ↔ long_leave (7B); active / suspended / deputed / long_leave → closed | `is_live()` (not closed / cancelled), `is_working()` (`active` — the state Domain D sees as an active staff member), `is_payable()` (`active, suspended, deputed, long_leave`) |
| `LeaveEntryKind` (L2-E05 §6.6) | `opening, accrual, carry_forward, adjustment, reservation, release, consumption, expiry, encashment, reversal` | — (append-only) | `sign()`: `+` opening / accrual / carry_forward / release; `−` reservation / consumption / expiry / encashment; `±` adjustment / reversal |
| `AttendanceDayStatus` | `present, absent, half_day, on_leave, holiday, weekly_off` | any → any (correction, history kept) | `absent_weight()` (absent 1, half_day 0.5) |

**Classification enums (foundation):** `StaffCategory` (`teaching, non_teaching,
administrative`), `EmploymentType` (`permanent, probation, contract, visiting, temporary`),
`Gender` (`female, male, other, undisclosed`), `AttendanceSource` (`manual, bulk, leave,
correction`).

**Slice enums (transitions slice-owned, in the slice's `domain.rs`):**

| Enum | Values | Used by |
|------|--------|---------|
| `PositionStatus` (L2-E02) | `draft, active, frozen, retired, archived` | 7A |
| `PositionAssignmentStatus` (L2-E02) | `draft, pending_approval, approved, scheduled, active, suspended, closed, superseded, archived` | 7A |
| `FacultyEligibilityStatus` (L2-E03) | `requested, under_review, approved, suspended, expired, rejected` | 7A |
| `RequisitionStatus` / `CandidateStatus` | `draft, pending_approval, approved, open, filled, cancelled` / `applied, shortlisted, interviewed, offered, offer_accepted, offer_declined, rejected, withdrawn` | 7A |
| `OnboardingStatus` (L2-E04) | `initiated, documentation_pending, verification_pending, ready_for_joining, joined, cancelled` | 7A |
| `HrCaseStatus` (L2-E04 generic case) | `initiated, pending_approval, approved, rejected, processing, completed, failed, closed, archived` | 7A (probation, certificate, separation share it; 7B leave case reuses it) |
| `ProbationOutcome` | `confirmed, extended, terminated` | 7A |
| `SeparationKind` | `resignation, retirement, termination, contract_end, death, other` | 7A (7C reads for settlement) |
| `VerificationStatus` | `pending, verified, rejected, expired` | 7A (qualifications, documents) |
| `LeavePolicyStatus` (L2-E05 §6.3) | `draft, under_review, approved, scheduled, active, superseded, archived` | 7B |
| `LeaveRequestStatus` (L2-E05 §6.4) | `draft, submitted, validated, pending_approval, approved, rejected, reserved, consumed, cancelled, closed` | 7B |
| `LeaveAdjustmentStatus` / `EncashmentStatus` / `LeaveExceptionStatus` | `requested, pending_approval, approved, applied, rejected` / `requested, eligible, not_eligible, approved, paid, rejected` / `raised, under_review, resolved, closed` | 7B |
| `WorkloadPlanStatus` | `draft, submitted, approved, active, superseded` | 7B |
| `PayrollConfigStatus` (L2-E06 §7.3) | `draft, review, approved, active, superseded, archived` | 7C |
| `PayComponentKind` / `CalculationBasis` | `earning, deduction, employer_contribution` / `fixed, percent_of_basic, percent_of_gross, statutory` | 7C |
| `PayrollRunStatus` (L2-E06 §7.1) / `PayrollRunKind` | `created, calculating, calculated, validating, pending_approval, approved, finalized, posted, closed, failed, reprocessing` / `regular, off_cycle, final_settlement, retro` | 7C |
| `PayrollResultStatus` / `PayrollAdjustmentStatus` (§7.3) / `PayslipStatus` | `calculated, validated, finalized, superseded` / `requested, validated, pending_approval, approved, calculated, applied, closed, rejected` / `issued, reissued, withdrawn` | 7C |

### 4.3 Events: `sutra_contracts::people::events`

`type = sutraerp.people.<aggregate>.<verb>`, `dataschema = urn:sutraerp:schema:people.<aggregate>.<verb>:1`,
`source = /sutraerp/people`, envelope `subject` = aggregate id, envelope scope =
`ExecutionScope::institution(tenant, <institution>)`. Emit via `shared::publish::event` inside the
owner-local tx; audit via `shared::publish::audit` with action `people.<aggregate>.<command>`. V1
payloads are frozen: additive optional fields only. Money and days in payloads are strings.

Shared payloads (`events/mod.rs`): **`StatusChangedV1`** `{aggregateId, institutionId, staffId?,
engagementId?, fromStatus?, toStatus, version, reason?, processInstanceId?, staffSubjectRef?,
managerSubjectRef?}`, **`ModifiedV1`** `{aggregateId, institutionId, version, changedFields[]}`,
**`PayrollPostedV1`** `{payrollRunId, institutionId, journalEntryId, entryNumber, grossEarnings,
employeeDeductions, employerContributions, netPay, postingKey}`.

| Owner / file | Constants (payload `StatusChangedV1` unless noted) | `type` suffixes |
|--------------|-----------|-----------------|
| foundation `mod.rs` | — (the directory API is read-only) | — |
| 7A `workforce.rs` | `STAFF_CREATED` (ModifiedV1), `STAFF_VERIFIED`, `STAFF_ACTIVATED`, `STAFF_CORRECTED` (ModifiedV1), `STAFF_MERGED`, `STAFF_ARCHIVED`; `ENGAGEMENT_SUBMITTED`, `ENGAGEMENT_ACTIVATED`, `ENGAGEMENT_MODIFIED` (ModifiedV1), `ENGAGEMENT_TRANSFERRED` (ModifiedV1), `ENGAGEMENT_SUSPENDED`, `ENGAGEMENT_RESTORED`, `ENGAGEMENT_CLOSED`, `REPORTING_LINE_SET` (ModifiedV1); `POSITION_ASSIGNED`, `POSITION_ASSIGNMENT_ENDED`; `FACULTY_ESTABLISHED`, `FACULTY_ELIGIBILITY_CHANGED`; `ONBOARDING_SUBMITTED`, `ONBOARDING_COMPLETED`; `PROBATION_DECIDED`; `SEPARATION_SUBMITTED`, `SEPARATION_COMPLETED`; `CERTIFICATE_ISSUED`; `QUALIFICATION_VERIFIED`, `DOCUMENT_VERIFIED` | `staff_member.*`, `staff_engagement.*`, `position_assignment.activated/ended`, `faculty_profile.*`, `onboarding_case.*`, `probation_review.decided`, `separation_case.*`, `certificate_request.issued`, `staff_qualification.verified`, `staff_document.verified` |
| 7B `leave.rs` | `LEAVE_POLICY_ACTIVATED`; `LEAVE_REQUEST_SUBMITTED`, `LEAVE_REQUEST_APPROVED`, `LEAVE_REQUEST_REJECTED`, `LEAVE_REQUEST_CANCELLED`, `LEAVE_REQUEST_CONSUMED`; `LEAVE_ACCRUED`; `LEAVE_BALANCE_ADJUSTED`; `LEAVE_CARRIED_FORWARD`; `LEAVE_ENCASHMENT_DECIDED`; `LEAVE_EXCEPTION_RESOLVED`; `LEAVE_CASE_DECIDED`; `LONG_LEAVE_STARTED`, `LONG_LEAVE_ENDED`; `ATTENDANCE_CORRECTED`; `WORKLOAD_PLAN_APPROVED` | `leave_policy.activated`, `leave_request.*`, `leave_accrual_run.completed`, `leave_adjustment.applied`, `leave_year_end_run.completed`, `leave_encashment.decided`, `leave_exception.resolved`, `leave_case.decided`, `long_leave.started/ended`, `attendance_day.corrected`, `workload_plan.approved` |
| 7C `payroll.rs` | `PAYROLL_CONFIG_ACTIVATED`; `PAYROLL_RUN_CALCULATED`, `PAYROLL_RUN_SUBMITTED`, `PAYROLL_RUN_APPROVED`, `PAYROLL_RUN_FINALIZED`, `PAYROLL_RUN_POSTED` (PayrollPostedV1), `PAYROLL_RUN_FAILED`; `PAYSLIP_ISSUED`; `PAYROLL_ADJUSTMENT_APPLIED`; `FINAL_SETTLEMENT_COMPLETED` | `payroll_config.activated`, `payroll_run.*`, `payslip.issued`, `payroll_adjustment.applied`, `final_settlement.completed` |

Each slice file exports `EVENT_TYPES`; `events::all_event_types()` lists every one and a unit test
keeps them unique and well formed. A slice may add an event to its own file and must add it to the
table above in its PR. **Consumed events:** none required — engagements / leave facts are read
through the foundation ports when a user acts (no inbox projection into `people`).

### 4.4 Permissions and role mapping (`sutra_kernel::auth::Permission`)

HR / payroll staff reads of every Domain E resource require `people.read` (Administrator). Staff
(Administrator **and Faculty**) act on their **own** records through `people.self`: handlers resolve
the staff record from the session subject (`shared::me::{my_staff, my_engagement}`) and never take a
staff id from the client; `people.self` also reads the basic staff directory. A **reporting
manager** decides the leave of their reports with `people.self` (7B checks
`EngagementSummary.reports_to_staff_id == my staff id`, else 404). Governed **HR decisions** use
`people.approve` (step-up); payroll approval / finalisation / posting uses `people.payroll.approve`
(step-up). Unit test `batch7_domain_e_grants`.

| Variant | Name | Step-up | Roles | Slice: used for |
|---------|------|---------|-------|-----------------|
| `PeopleRead` | `people.read` | no | Admin | all: staff, leave, payroll reads |
| `PeopleSelfService` | `people.self` | no | Admin, Faculty | own staff record / profile (7A), own leave + manager approvals (7B), own payslips (7C), staff directory (foundation) |
| `StaffManage` | `people.staff.manage` | no | Admin | 7A staff records, engagements, positions, reporting lines, faculty, recruitment, onboarding, probation, separation, certificates, profile verification |
| `LeaveManage` | `people.leave.manage` | no | Admin | 7B leave policy, accrual / year-end runs, adjustments, encashment, exceptions, long leave, attendance, workload plans |
| `LeaveDecide` | `people.leave.decide` | no | Admin | 7B leave request decisions on behalf of any manager |
| `PayrollManage` | `people.payroll.manage` | no | Admin | 7C payroll configuration, salary structures / assignments, periods, runs (create / calculate / validate), adjustments, settlements |
| `PayrollApprove` | `people.payroll.approve` | **yes** | Admin | 7C approve / finalise / post runs, approve adjustments, issue payslips |
| `PeopleApprove` | `people.approve` | **yes** | Admin | all: governed HR **decisions** (§9) — engagement activation / change, separation, probation, policy and configuration activation |

Step-up is enforced only when the deployment enables it; DB tests run in dev mode without it.

## 5. HTTP API

### 5.1 Conventions (as Domains B, C, D, F and I)

- Every handler starts with `auth.require(Permission::…)?` (or `shared::me::*`). Tenant from the
  session; build `shared::scope::institution_scope(ctx, <institution>)` for events/audit.
- Before a write: `shared::guard::require_institution_writable`; commands on an engagement that
  must be live use `shared::guard::require_live_engagement`.
- Create: `POST`, required `Idempotency-Key` (`sutra_web::idempotency::require_idempotency_key`),
  `201` + `ETag`. Update or state command: `If-Match` required (`shared::http::if_match`: 428
  missing, 412 stale); response = updated resource + `ETag`. Bulk commands (bulk attendance,
  accrual / year-end runs, payroll calculation, payslip issue) also require `Idempotency-Key` and
  return a summary `{processed, skipped, failed[]}`.
- State commands are `POST …/{id}/<verb>` with an optional `{ "reason": "…" }`; literal paths per
  command, no generic `/{command}` route.
- Lists: `GET` collection under the parent, `?status=` filter, `{ "items": [...] }`; `?limit=` ≤ 200
  (default 50) and `nextCursor` where lists grow (staff, leave requests, payroll results).
- Errors: illegal transition / duplicate / insufficient balance → `409`; validation (dates, amounts,
  days) → `422`; foreign tenant / unknown id / not your report → `404`; Domain B/D reference not
  found → `422` on the field.
- JSON members camelCase, statuses snake_case (§4.2), business dates `YYYY-MM-DD`, **decimals as
  strings**.
- **Every `#[utoipa::path]` sets an explicit `operation_id`** with the slice prefix below and the
  slice's `tag` (enforced by `architecture.rs::people_operations_follow_the_slice_prefix_and_tag_rules`,
  which also reserves the `people_` prefix for Domain E and checks the `{id}` rule).

### 5.2 Path prefixes, operation_id prefixes, tags (all under `/api/v1/people`)

**Path parameter rule:** the first parameter after any collection segment is always `{id}`;
deeper parameters get names (`/positions/{id}/assignments/{assignmentId}`). Self-service lives
under `/me/…`.

| Owner | Paths | operation_id prefixes | tag |
|-------|-------|-----------------------|-----|
| foundation | `GET /institutions/{id}/staff` (HR list), `GET /staff/{id}` (staff + current engagement), `GET /institutions/{id}/directory` (staff directory, `people.self`), `GET /me/staff` (own record) | `people_directory_` (incl. `people_directory_self_`) | `people-directory` |
| 7A | `POST /institutions/{id}/staff`, `PUT /staff/{id}`, `/staff/{id}[/submit-verification,/verify,/activate,/suspend,/restore,/archive,/merge,/corrections]`, `/staff/{id}/engagements`, `/engagements/{id}[/submit,/decision,/activate,/modify,/transfer,/suspend,/restore,/close,/cancel,/reporting]`, `/institutions/{id}/positions`, `/positions/{id}[/freeze,/retire]`, `/positions/{id}/assignments`, `/position-assignments/{id}[/decision,/change,/end]`, `/staff/{id}/faculty-profile`, `/faculty-profiles/{id}[/eligibility,/suspend,/reactivate]`, `/staff/{id}/qualifications`, `/staff-qualifications/{id}/verify`, `/staff/{id}/documents`, `/staff-documents/{id}/verify`, `/me/profile` | `people_staff_`, `people_engagement_`, `people_position_`, `people_reporting_`, `people_faculty_`, `people_profile_` (incl. `people_profile_self_`) | `people-workforce` |
| 7A | `/institutions/{id}/requisitions`, `/requisitions/{id}[/submit,/decision,/close]`, `/requisitions/{id}/candidates`, `/candidates/{id}[/shortlist,/offer,/accept,/decline,/reject]`, `/candidates/{id}/onboarding`, `/onboarding-cases/{id}[/documents,/verify,/ready,/join,/cancel]`, `/engagements/{id}/probation-reviews`, `/probation-reviews/{id}/decision`, `/engagements/{id}/separation`, `/separation-cases/{id}[/decision,/clearance,/complete]`, `/staff/{id}/certificate-requests`, `/certificate-requests/{id}[/decision,/issue]`, `/me/certificate-requests` | `people_recruitment_`, `people_onboarding_`, `people_probation_`, `people_separation_`, `people_certificate_` (incl. `people_certificate_self_`) | `people-hr` |
| 7B | `/institutions/{id}/leave-policies`, `/leave-policies/{id}[/submit,/decision,/activate,/supersede]`, `/leave-policies/{id}/leave-types`, `/institutions/{id}/leave-requests`, `/leave-requests/{id}[/decision,/cancel,/consume]`, `/institutions/{id}/leave-accruals`, `/institutions/{id}/leave-year-end`, `/staff/{id}/leave-balances`, `/staff/{id}/leave-ledger`, `/staff/{id}/leave-adjustments`, `/leave-adjustments/{id}/decision`, `/staff/{id}/leave-encashments`, `/leave-encashments/{id}/decision`, `/institutions/{id}/leave-exceptions`, `/leave-exceptions/{id}[/review,/resolve]`, `/staff/{id}/leave-cases`, `/leave-cases/{id}/decision`, `/engagements/{id}/long-leave`, `/long-leaves/{id}[/start,/end]`, `/me/leave-balances`, `/me/leave-requests[/{id}/cancel]`, `/me/leave-approvals[/{id}/decision]` | `people_leave_` (incl. `people_leave_self_`) | `people-leave` |
| 7B | `/institutions/{id}/attendance[/bulk]`, `/staff/{id}/attendance`, `/attendance-days/{id}/correct`, `/staff/{id}/workload-plans`, `/workload-plans/{id}[/submit,/decision]`, `/staff/{id}/workload`, `/me/attendance`, `/me/workload` | `people_attendance_` (incl. `people_attendance_self_`), `people_workload_` (incl. `people_workload_self_`) | `people-attendance` |
| 7C | `/institutions/{id}/payroll-configs`, `/payroll-configs/{id}[/submit,/decision,/activate,/supersede]`, `/payroll-configs/{id}/components`, `/payroll-configs/{id}/statutory-rules`, `/institutions/{id}/salary-structures`, `/salary-structures/{id}`, `/engagements/{id}/salary-assignments`, `/salary-assignments/{id}[/end]`, `/institutions/{id}/payroll-periods`, `/payroll-periods/{id}/runs`, `/payroll-runs/{id}[/calculate,/validate,/submit,/decision,/finalise,/post,/close,/reprocess]`, `/payroll-runs/{id}/results`, `/payroll-results/{id}`, `/payroll-runs/{id}/payslips[/issue]`, `/payslips/{id}[/reissue,/withdraw]`, `/payroll-results/{id}/adjustments`, `/payroll-adjustments/{id}[/validate,/decision,/apply]`, `/engagements/{id}/final-settlement`, `/final-settlements/{id}[/calculate,/decision,/complete]`, `/me/payslips[/{id}]` | `people_payroll_`, `people_pay_`, `people_salary_`, `people_payslip_` (incl. `people_payslip_self_`), `people_settlement_` | `people-payroll` |

The tags are already in `apps/api/src/openapi.rs`; the router mount points are already registered
in `sutra_people::router_with_state` (`staff_directory` → foundation; `workforce`, `positions`,
`hr`, `profile` → 7A; `leave`, `attendance`, `workload` → 7B; `payroll`, `payslips` → 7C). Each
slice only fills `pub fn router(state: PeopleState) -> OpenApiRouter` in its own `mod.rs`.

**Integration notes (recorded by the integrator):**
- `GET /me/profile` (7A) answers **200 with `staff: null`** for a login without a staff record (My HR
  renders its empty state without a 404). The other self-service reads (`/me/leave-*`, `/me/attendance`,
  `/me/workload`, `/me/payslips`) still answer **404 "staff record not found"** for such a login; the
  `ui` e2e accepts exactly those 404s.
- Endpoints 7B added beyond the table above: `GET /leave-policies/{id}`, `GET /leave-requests/{id}`,
  `POST /institutions/{id}/leave-consumption` (bulk consumption of reserved leave up to a date),
  `POST /leave-cases/{id}/complete`, `GET /me/leave-types`.
- Cross-slice wiring added by the integrator: closing an engagement (7A `close_engagement`, also
  reached from separation and probation termination) cancels in the same transaction every open
  leave request of it that starts after the last working date (7B
  `leave::request::cancel_after_exit`: reservation released, `leave_request.cancelled`). 7C reads
  loss of pay (`consume:<YYYY-MM>` unpaid entries + absences) and unpaid `encashment` entries only
  through `LeaveReader::payroll_inputs`; leave requests need a working (`active`) engagement, so a
  suspended / closed engagement takes no new leave; payroll pays `WorkforceDirectory::payable_engagements`.
  Covered end to end by `apps/api/tests/db_b7_integration.rs` and `apps/web/e2e/people.e2e.mjs`.
- Web (integrator): Admin → People has a separate `workload` tab (7B `WorkloadAdminTab`); Faculty
  Leave is My HR at `/leave/:tab?` with tabs leave / profile / attendance / workload / payslips
  (`MY_HR_TABS`; 7B adds `MyLeaveStats`, `MyAttendancePanel`, `MyWorkloadPanel`); 7C's tab gained
  pay components, salary structures, salary assignments (staff picker) and final settlements.
- OpenAPI schema names are global across the workspace: a people `ToSchema` type whose name already
  exists in another domain carries `#[schema(as = …)]` (`StaffPositionList`, `ProfileVerifyBody`,
  `SeparationClearanceItem`, `StaffCorrectionBody`).

### 5.3 Minimum response fields other slices / screens rely on

- **Staff** (foundation view, 7A extends): `{id, institutionId, staffNumber, subjectRef?, givenName,
  familyName?, displayName, workEmail?, staffCategory, status, engagement?{id, engagementNumber,
  orgUnitId?, campusId?, staffCategory, employmentType, designation, grade?, startDate, endDate?,
  lastWorkingDate?, probationEndDate?, reportsToStaffId?, status, version}, version}`.
- **Leave request** (7B): `{id, number, staffId, staffNumber, displayName, leaveTypeId,
  leaveTypeCode, startDate, endDate, halfDayStart, halfDayEnd, days, reason?, status,
  approverStaffId?, processInstanceId?, version}`; **leave balance**: `{leaveTypeId, leaveTypeCode,
  leaveYear, entitled, reserved, consumed, available}` (strings).
- **Payroll run** (7C): `{id, number, periodId, kind, status, periodStart, periodEnd, headcount,
  grossEarnings, employeeDeductions, employerContributions, netPay, journalEntryId?,
  journalEntryNumber?, processInstanceId?, version}`; **payslip**: `{id, number, payrollRunId,
  staffId, staffNumber, displayName, periodStart, periodEnd, earnings[{code, name, amount}],
  deductions[{code, name, amount}], grossEarnings, totalDeductions, netPay, lopDays, status}`.
- Collections: `{ "items": [ … ] }`.

## 6. Cross-slice runtime rules and ports

### 6.1 Foundation helpers (`modules/people/src/shared/`)

- `state::PeopleState` — `pool, workflow, approvals, references, directory, leave, workload,
  postings`. Built by `PeopleDeps::into_state()` (auto-approve, Postgres adapters) or by hand in
  tests (port doubles, `AwaitDecision`).
- `scope`, `guard` (`require_institution_writable`, `require_institution`,
  `require_live_engagement`), `http` (`with_etag`, `if_match`, `actor`), `publish` (`event` +
  `audit`), `approval` (§9), `time::business_today`, `numbering::next` (§3.4), `decimal`
  (`round2`, `fmt2`, `parse_decimal`).
- `rows` — column lists + decoders (`STAFF_COLUMNS`/`staff_from_row`, `ENGAGEMENT_*`),
  `lock_staff`, `lock_engagement`, `load_staff`, `load_engagement`, `current_engagement`.
- `lifecycle` — `change_staff_status`, `change_engagement_status`, `record_history` (any slice,
  own aggregates), `StatusChange`, `HistoryEntry`.
- `staff` — `insert_staff(NewStaff)`, `insert_engagement(NewEngagement)`.
- `leave` — `post_entry(NewLeaveEntry)` (sign check, idempotent), `reverse_entry`, `balance`
  (projection inside a tx), `LeaveEntryReceipt`.
- `attendance` — `record_day(DayRecord)` (upsert + history).
- `me` — `my_staff`, `my_engagement`.

### 6.2 Typed ports (all defined and wired by the foundation)

| Port | Defined in | Implementation wired now | Consumer | Notes |
|------|-----------|--------------------------|----------|-------|
| **`WorkforceDirectory`** (`staff`, `staff_by_subject`, `staff_of_institution`, `engagement`, `current_engagement`, `engagements_of_staff`, `payable_engagements(institution, from, to)`, `reports_of(manager)`) — "7A → 7B / 7C" | `sutra_contracts::people::ports` | `sutra_people::PgWorkforceDirectory` (**real**) | 7B (applicant, manager, long leave), 7C (payroll population, settlement), self-service | 7B/7C never wait for 7A: the shape is foundation-owned and seeded in tests by `support::b7`. `payable_engagements` = engagements whose status `is_payable()` overlapping the period, plus engagements closed with `last_working_date` inside it. |
| **`LeaveReader`** (`balances(staff, leave_year)`, `payroll_inputs(staff, from, to)`) — "7B → 7C" | same | `sutra_people::PgLeaveReader` (**real**, over `leave_ledger_entry` + `attendance_day`) | 7C LOP and encashment, My HR balances | `unpaid_leave_days` = consumption days of `is_paid = false` entries effective in the period (net of reversals); `absent_days` = absent + ½ half_day attendance days; `encashments` = `encashment` entries effective in the period. 7C records what it paid in its own `encashment_payment` table (unique on the ledger entry id). |
| **`StaffDirectory`** (Batch 4, `staff_by_subject`) — People → Domain D | `sutra_contracts::people` | `sutra_people::PgStaffDirectory` (**real**, replaces `SubjectOnlyStaff` in `academic_deps`) | 4A faculty assignment, Exams via Domain D | Known subject → `staff_id`, display name, engagement org unit, `active = engagement.is_working()`; **no staff record → the Batch 4 subject-only answer** (`staff_id = None`, `active = true`) so Batch 4/6 data and tests keep working. |
| **`FacultyWorkloadPort`** (`teaching_load(institution, subject, term?)`) — Domain D → 7B | `sutra_contracts::academic::ports` (Batch 7 block) | `sutra_academic::PgFacultyWorkload` (**real**, over 4A `faculty_assignment` `approved / active` + the effective `faculty_assignment_policy.max_weekly_hours`) | 7B workload plan (E-030) | **No cross-schema SQL** in `sutra-people`. Contract change to Domain D (adapter file foundation-owned). |
| **`PayrollPostingPort`** (`post_payroll`) — 7C → Domain F | `sutra_contracts::finance::ports` (Batch 7 block) | `sutra_finance::PgPayrollPostings` (**real**: one balanced journal entry `source_type = payroll_run`, `source_id` = run id, `posting_key` (e.g. `finalise`, `adjustment:<id>`, `settlement:<id>`) — Dr `5410` salaries & wages (gross + employer contributions) / Cr `2410` salaries payable (net) / Cr `2420` payroll deductions & statutory dues payable (deductions + employer contributions); signed amounts allowed for corrective postings (negative → opposite side); period-guarded; idempotent) | 7C finalisation / posting, adjustments, final settlement | Payroll accounts (`system_key` `salary_expense`, `payroll_payable`, `payroll_deductions_payable`) are seeded on first use **outside** Batch 5's default chart (the Batch 5 chart test keeps 14 accounts). Contract change to Domain F (+ `source::PAYROLL_RUN`). Payment execution of salaries stays Finance (later batch). |
| Domain B reference | `sutra_contracts::institution::reference` | `PgInstitutionReferences` | all | institution + timezone, org units, campuses. |

Tests can swap any port by building a `PeopleState` by hand and serving
`sutra_people::router_with_state(state)`; `sutra_api::app::people_deps(pool, workflow)` gives the
production adapters.

## 7. Cross-slice dependencies and readiness

- **Staff records and engagements are foundation-owned tables**, so 7B / 7C never wait for 7A:
  tests seed through `apps/api/tests/support/b7.rs` (`people_setup` → active institution with a
  published programme / calendar and the default chart of accounts; `seed_staff` →
  `insert_staff` + `insert_engagement` in any engagement status, optionally linked to a login such
  as `b4::MEERA`; `seed_leave_entry` → `post_entry`; `seed_attendance` → `record_day`). 7A's real
  flows create the same shape.
- **7A → 7B:** leave is requested only on a **live** engagement (`require_live_engagement`);
  manager approval uses `reports_to_staff_id` (7A sets it; tests seed it with
  `b7::set_manager`). Long leave (7B) moves the engagement `active → long_leave → active` only.
- **7A → 7C:** the payroll population is `WorkforceDirectory::payable_engagements`; a separation
  (7A) closes the engagement with `last_working_date` → 7C final settlement reads it (7C seeds a
  closed engagement with `b7::seed_staff(…, Some(EngagementStatus::Closed))`; the last argument
  is `Option<EngagementStatus>`, `None` = no engagement; `Closed` gets `last_working_date = 2026-11-15`;
  `Cancelled` is not seedable — seed `Pending` and cancel through the lifecycle).
- **7B → 7C:** 7C reads leave facts only through `LeaveReader::payroll_inputs`; 7B's consumption
  is posted per calendar-month segment (`posting_key = consume:<YYYY-MM>`, `effective_on` = first
  leave day of the month) so period sums are exact; unpaid leave types post `is_paid = false`.
  Encashment: 7B posts an `encashment` entry (negative days) on approval; 7C pays it and records
  the ledger entry id in `encashment_payment` (unique) — 7C never writes the ledger.
- **Domain D:** faculty workload only through `FacultyWorkloadPort`; Domain D sees staff facts only
  through `StaffDirectory` (now real).
- **Finance:** payroll postings only through `PayrollPostingPort`; never `finance.*` SQL.
- Merge order: **7A → 7B → 7C** (§12).

## 8. Inbox consumer hooks (`modules/platform/src/inbox/rules/`)

`rules::plan` dispatches by event type to `people_7a.rs` (7A), `people_7b.rs` (7B), `people_7c.rs`
(7C); each declares `EVENT_TYPES` (only types from its own event file — test
`slices_never_claim_the_same_type`) and a pure `plan`. The foundation files start with **empty**
`EVENT_TYPES`; add a type only **with** its rule. Category: reuse `institution` (no new notification
category, so no platform migration). Notify staff with `Recipient::Subject(staffSubjectRef)`
(managers: `managerSubjectRef`); otherwise `Recipient::Role(…)`. To-dos with `todos::SystemTodo`.

| Slice | Types | Projection |
|-------|-------|------------|
| 7A | `staff_engagement.activated`, `onboarding_case.completed`, `probation_review.decided`, `certificate_request.issued` | notification to the staff member (link `/leave`) |
| 7A | `staff_engagement.submitted`, `separation_case.submitted` (AwaitDecision) | to-do (admins) "Approve engagement …" / "Approve separation …" |
| 7B | `leave_request.submitted` | notification + to-do to the reporting manager (`managerSubjectRef`, link `/leave`), else administrators (link `/people/leave`) |
| 7B | `leave_request.approved`, `leave_request.rejected`, `leave_request.cancelled`, `leave_encashment.decided` | notification to the applicant (link `/leave`) |
| 7C | `payslip.issued` | notification to the staff member (link `/leave`) |
| 7C | `payroll_run.submitted` (AwaitDecision) | to-do (admins) "Approve payroll …" |

## 9. WorkflowPort usage for approval steps

`shared::approval` wraps `WorkflowPort` exactly like Domains B, C, D, F and I: the composition root
passes the platform `NoopWorkflowAdapter` and `ApprovalMode::AutoApprove`.

1. Validate the command and current state (read only).
2. `request_approval(&state, &scope, process::<KEY>, business_key(aggregate, id, version), vars)`
   **before** opening the tx (deterministic key → retry-safe).
3. One owner-local tx: re-check the version; set the pending state; store `process_instance_id`;
   emit the *submitted* event. If the outcome is `ApprovalOutcome::Approved`, **also** apply the
   decision in the same tx (final state + decision event with `processInstanceId`, audit actor
   `system:auto-approval`).
4. Always expose the explicit decision endpoint (`POST …/{id}/decision {decision: approve|reject,
   reason}` with the slice's decide permission) so `AwaitDecision` (Flowable, Batch 9) needs no API
   change. Each slice has at least one DB test building a `PeopleState` with
   `ApprovalMode::AwaitDecision` (via `sutra_people::router_with_state` and
   `sutra_api::app::people_deps`).

Process keys (`shared::approval::process`): 7A `ENGAGEMENT_ACTIVATION`, `ENGAGEMENT_CHANGE`,
`POSITION_ASSIGNMENT`, `REQUISITION_APPROVAL`, `ONBOARDING_ACCEPTANCE`, `PROBATION_DECISION`,
`SEPARATION_APPROVAL`, `CERTIFICATE_APPROVAL`; 7B **`LEAVE_APPROVAL` (the reference approval of
Batch 7: leave request pending_approval → approved | rejected; decided by `people.leave.decide` or
the reporting manager)**, `LEAVE_POLICY_APPROVAL`, `LEAVE_ADJUSTMENT_APPROVAL`,
`ENCASHMENT_DECISION`, `LEAVE_CASE_DECISION`, `WORKLOAD_PLAN_APPROVAL`; 7C
`PAYROLL_CONFIG_APPROVAL`, `PAYROLL_APPROVAL`, `PAYROLL_ADJUSTMENT_APPROVAL`, `OFF_CYCLE_APPROVAL`,
`FINAL_SETTLEMENT_APPROVAL`. Use only the keys of your slice; a new key is a contract change.

## 10. Web

Batch 7 screens use **only `@sutra/ui` components** (BentoTile, DataTable, Tabs, Button, Badge,
Input/Select, Modal/Drawer, EmptyState, StatTile, Steps, PageHeader) and **no custom CSS / class
names / inline styles**. The frames already render the `PageHeader` and the tabs. Do not add
stylesheets; if a component is missing, compose existing ones and note it for the integrator.

| Route | File (owner) | Content |
|-------|--------------|---------|
| `/people/:tab?` | `routes/admin/people.tsx` (foundation frame, pre-wired) | Admin → People tabs (`PEOPLE_TABS` in `features/people/index.ts`): `staff`, `recruitment`, `positions` (7A), `leave`, `attendance` (7B), `payroll` (7C). Uses `features/institution/AdminScreen` (admin gate + institution picker + PageHeader + Tabs). Linked from the admin nav ("People", already present). |
| `/leave` | `routes/faculty/leave.tsx` (foundation frame) | Faculty **Leave** (My HR): `MyLeavePanel` (7B, span 8: balances, apply, my requests, approvals for managers), `MyProfilePanel` (7A, span 4: own record, engagement, qualifications, certificate requests), `MyPayslipsPanel` (7C, span 12: payslips). Uses `features/academic/RoleScreen` (faculty only). Linked from the faculty nav ("Leave", already present). |
| — | — | No student screen in Batch 7. |

Each slice fills the placeholder components exported from its folder — keep the export names:
`features/people/workforce/index.tsx`: `StaffAdminTab, RecruitmentAdminTab, PositionsAdminTab,
MyProfilePanel`; `features/people/leave/index.tsx`: `LeaveAdminTab, AttendanceAdminTab,
MyLeavePanel`; `features/people/payroll/index.tsx`: `PayrollAdminTab, MyPayslipsPanel`. Mark real
screens with `data-slice="7X"` (the foundation test accepts either the placeholder or
`data-slice`). Put API hooks in your own folder (`features/people/<slice>/api.ts`), use
`@sutra/api-client` after committing the regenerated client, decimals as strings (never
`parseFloat` money or days for display), and a mock mode like `features/institution/mock.ts`.
Until a sibling slice is merged, type its calls locally in your folder. Never edit another slice's
folder. `lib/workspace-screens.ts` already maps W-E01 … W-E07 to these routes.

**E2E:** `apps/web/e2e/people.e2e.mjs` (`pnpm e2e:people`, added by the integrator / first slice that
needs it, reusing `e2e/lib/harness.mjs` and the `exams.e2e.mjs` pattern) runs **in CI** (the
integrator adds `people` to the e2e matrix) — never locally. The `ui` e2e already treats `/people`
and `/leave` as live screens.

## 11. Worker environment: CI-first

| Slice | Worktree | Branch | Local check target dir |
|-------|----------|--------|------------------------|
| 7A | `/workspace/work/erp-7a` | `batch7/7a` | `/workspace/work/targets/7a` |
| 7B | `/workspace/work/erp-7b` | `batch7/7b` | `/workspace/work/targets/7b` |
| 7C | `/workspace/work/erp-7c` | `batch7/7c` | `/workspace/work/targets/7c` |

**CI-first, NO local cargo at all** (the sandbox was OOM-killed on a single `cargo check -j1`):
the only local Rust is `cargo fmt --all` (rustfmt only); no `cargo check/build/test/clippy`, not even
`-p sutra-people`. **No local `cargo test`, `clippy --workspace`, DB tests, `pnpm build` or e2e**: push (without
`[skip ci]`) and poll the `SutraERP CI` workflow through the GitHub API
(`/workspace/work/tools/b7-worker-common.md` has the exact commands): fmt · clippy · workspace
tests (`Rust`), full DB suite incl. `db_b7_foundation` + your `db_b7X` + every regression (`Rust
integration`), `OpenAPI drift` (commit the regenerated spec + TS client from the
`openapi-regenerated` artifact), `Web`, the e2e matrix. Fix until all are green.

## 12. Merge order and known gaps

- Merge order: **7A → 7B → 7C** (7B depends on 7A only through the foundation tables and
  `reports_to_staff_id`; 7C on 7A/7B only through `WorkforceDirectory` / `LeaveReader`).
  Regenerate OpenAPI/TS after each merge (§2.3) — from the CI artifact. After 7C merges, the
  integrator adds a cross-slice test (onboarding → staff record + engagement activated → reporting
  manager → leave policy + accrual → leave request decided by the manager → consumption (unpaid
  leave) → salary assignment → payroll run → LOP deduction → approval → finalisation → journal entry
  in Finance → payslip on `/me/payslips` → separation → final settlement with encashment) in
  `apps/api/tests/db_b7_integration.rs`, plus `people.e2e.mjs` and the `e2e (people)` CI job.
- Session pinning to one institution is not enforced for Domain E (tenant-wide HR admin, as
  Batches 2–6).
- Salary **payment execution** (bank transfer of net pay) is Finance treasury work (later batch);
  Batch 7 posts the payroll liability only.
- `ApprovalMode::AwaitDecision` is wired but unused in production until the Flowable adapter lands
  (Batch 9).
- Statutory rules (PF / ESI / PT / TDS) are simplified governed rate + ceiling rules, not a tax
  engine; the configuration lifecycle makes them replaceable.
- Institution status is checked through the reference port without a cross-schema row lock.
