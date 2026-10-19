# Batch 6 — shared contracts (Examinations, Domain I)

**Status:** frozen at `batch6/foundation`. This file is the single source of truth for the three
slices **6A**, **6B** and **6C**. If a slice needs to change anything in §2–§11, it says so in its
PR description ("contract change: …") instead of silently editing a shared file; the integrator
applies it on top of the merge.

Architecture inputs: L2-I01 … L2-I16 (+ L2-I02 contracts / locked decisions amendment 2026-08-24,
L2-I11 exception lifecycle amendment 2026-09-03); L3/L4 I-001 … I-192 (in-scope list §1); ADR-L5-0003
(module boundaries), -0004 (scope), -0006/-0007 (owner-local tx + outbox), -0011/-0035 (event
versioning / envelope), -0023/-0042 (WorkflowPort / Flowable), -0032 (decimals: `rust_decimal`),
-0034 (API profile), -0036 (schemas, business timezone); L6 screens Student Results, Faculty
Assessments, Admin Examinations.

Plan line (Batch 6): *exam scheduling, seating, assessments, marks entry, gradebook, moderation;
result computation engine, verification, publication, grade cards, revaluation. Screens: Student
Results; Faculty Assessments; Admin Examinations.*

**Lessons from Batches 3–5 applied here:** every cross-slice and cross-domain dependency is a
**typed port or foundation helper defined up front** (§6) with a real implementation already wired,
so no slice waits for another; the shared rows (sessions, papers, registrations, marks, results)
have **one** write path each (`shared::{lifecycle, registration, marks, results}`); marks and grade
points are `rust_decimal` everywhere (no floats, ever); **CI-first**: no heavy local builds (§11).

---

## 1. Slices and scenario coverage (one wave: 6A + 6B overlapping, then 6C)

**112 of 192 L3 scenarios in scope (58 %)** — the core flow end to end: policy → session → papers →
schedule / seating / invigilation → registration → hall tickets (finance + attendance conditions) →
conduct attendance → internal assessment import + marks entry → verification / moderation → final
marks → result computation (SGPA / CGPA) → verification / approval → publication → grade cards →
revaluation (Finance fee) → supplementary eligibility → completion / closure.

| Slice | Scope | L3/L4 scenarios (47 / 19 / 46) |
|-------|-------|-------------------------------|
| **6A** | **Exam setup.** Examination policy versions (definition → review → activation, revision → supersession, retirement). Sessions (cycle → governed session: draft → under_review → approved → scheduled → active; governed revision, suspension / reopening, duplicate resolution) and their **papers** (one per offering / curriculum component: credits, mode, max / pass marks, internal weight). Schedule / timetable (plan → approve → publish, revision, conflict detection, withdrawal + notification), venues + allocation, **seating plans**, **invigilation** and practical examiners (shortage, mid-conduct replacement). **Registration** (self-service, bulk / institution-assisted, late, deadline override, amendment, erroneous → corrected, withdrawal / reinstatement, reopening, rejection / ineligibility, scope change after a 3B registration change, special arrangements, supplementary / backlog / improvement registration, duplicates, administrative on behalf, population finalisation → `locked`). **Hall tickets** (issuance to eligible candidates, unsatisfied conditions → withheld — finance dues via `FinanceClearancePort`, attendance via `AcademicExamFacts` —, invalidation after withdrawal, special arrangements, duplicates, population finalisation). Conduct basics (completed conduct record with attendance → registered paper `appeared` / `absent`, failed entry condition → non-participation, seating conflict, invigilator replacement) and contingencies (exception case, suspension / resumption of conduct, postponement + reschedule). Screens: Admin → Examinations tabs `sessions`, `schedule`, `registrations`; Student Results panel `StudentExamPanel` (registration, hall ticket, timetable). | I-001 … I-027, I-029, I-030, I-032, I-034, I-036 … I-043, I-055, I-057, I-059, I-060, I-172, I-174, I-175, I-185 |
| **6B** | **Assessment & marks.** Mark sheets per paper (evaluation lifecycle pending → … → finalised), **internal assessment import** from Batch 4 (`AcademicExamFacts::internal_results` → `paper_mark.internal_*`, `internal_source_id`), **marks entry** by faculty on papers of sections they teach (`OfferingDirectory::teaches`) and by the exam cell, submission → verification → **final marks approval (the reference approval of Batch 6, WorkflowPort)**, panel evaluation, marks submission exceptions, incomplete evaluation → withheld marks, supplementary / improvement marks, **moderation** under policy (grace / scaling through `shared::marks::update` → `moderated`), policy-required second evaluation, first / second difference, adjudicating evaluation; **gradebook** preview per paper (`shared::grading`). Malpractice basics: suspected → investigation → committee decision with penalty (debarment sets the registered paper `debarred`), appeal review (uphold / modify / overturn → restored outcome), reopening, duplicates, closure + archival. Screens: Admin → Examinations tabs `marks`, `malpractice`; Faculty Assessments `FacultyAssessmentsPanel` (marks entry + gradebook). | I-071 … I-077, I-080 … I-082, I-162 … I-168, I-170, I-171 |
| **6C** | **Results.** **Computation engine**: final marks (`MarksReader`, `finalised` only) → graded lines (`shared::grading::grade_line`) → SGPA / credits (`aggregate`) → CGPA over the student's current results (`cgpa`), withheld conditions (missing final marks, pending malpractice, special conditions) and their resolution, recompute before approval, approved marks correction, moderation / grace / conditional policy, malpractice consequence (read 6B's decided cases read-only), duplicates / conflicting outcomes, withheld individual results and release, reopening, pre-publication replacement. **Verification + approval** (WorkflowPort), **publication** (publish, withhold / release, delay / resolution, cancellation, post-publication change → replacement, republication, withdrawal / restoration, revaluation and malpractice outcomes, absence consequence, publication failure recovery, discrepancy correction), **grade cards**. **Revaluation / rechecking** (request → eligibility → **Finance fee** through `ExamFeePort` → settlement read through `ChargeDirectory` → approval → review → outcome verification → revised marks via `shared::marks::revise_final` → result revision; withdrawal, duplicates, conflicting requests, finalised correction). Supplementary basics (eligibility + attempt authorisation, opportunity → supplementary session, duplicate attempts, best-result position, exhausted attempts). Completion evaluation (CGPA / credits vs curriculum, re-evaluation after a revised result, completion evidence), **session closure** (`closing → closed`) and archival. Screens: Admin → Examinations tabs `results`, `revaluation`; Student Results panel `StudentResultsPanel` (results, grade cards, SGPA / CGPA, revaluation requests). | I-083, I-085 … I-090, I-093, I-095, I-097 … I-112, I-114, I-116 … I-121, I-128 … I-130, I-132 … I-134, I-138, I-139, I-142, I-145, I-150, I-153, I-187, I-192 |

**Deferred (80 scenarios), recorded here and in `docs/batches.md`:**

| Scenarios | Area | Lands in |
|-----------|------|----------|
| I-028, I-031, I-033, I-035 | Registration edge cases (special permission, opportunity-change withdrawal, conflicting scope reconciliation, context transfer) | 6D (later) |
| I-044 … I-054, I-056, I-058, I-061 … I-070 | Detailed conduct: practical / viva conduct incidents, identity discrepancies, re-conduct, resource / batch conflicts, multi-centre, question-paper custody / compromise, answer-script custody, medical interruption / withdrawal during conduct, conduct record corrections | 6D (exam conduct & contingency) |
| I-078, I-079 | Evaluation of re-conducted exams / special-arrangement evaluation | 6D |
| I-084, I-091, I-092, I-094, I-096, I-113, I-115 | Results after re-conduct / special arrangements, identity mismatch, conflicting outcomes from exceptions, incomplete-exam outcome publication | 6D |
| I-122 … I-127, I-131 | Practical revaluation / rechecking, evaluator conflict of interest, additional / adjudicated revaluation, interruption, reopening | 6D |
| I-135 … I-137, I-140, I-141, I-143, I-144 | Supplementary schedule revision / reschedule / re-conduct, opportunity cancellation / restoration, attempt-policy change, execution coordination | 6D |
| I-146 … I-149, I-151, I-152, I-154 … I-161 | Completion edge cases (incomplete records, conflicting inputs, identity, duplicates, malpractice / exception re-evaluation, provisional evidence, evidence correction / reissue / withdrawal / restoration, upstream blocking) | Batch 9 (with Domain C conferment) |
| I-169, I-173, I-176 … I-184, I-186 | Conflicting malpractice decisions; exception classification, termination, re-conduct authorisation / execution, communication, escalation, conflicting decisions, reopening / review, closure; contingency plans | 6D |
| I-188 … I-191 | Archived record retrieval, retention classification, disposal, legal hold | Batch 9 (records governance) |

Domains **E** (People, Batch 7) and **G** (Campus rooms, Batch 8) are not involved: venues are
Exams-owned records (no Campus room FK) and invigilators are login subjects (as Batch 4 faculty).

## 2. Where the code lives: module and file ownership

**Decision:** every Batch 6 aggregate lives in the existing crate `modules/exams` (`sutra-exams`,
domain I) and the PostgreSQL schema `exams`. No new crates (`apps/api/tests/architecture.rs` fixes
exactly ten domain crates). `sutra-exams` depends only on `sutra-kernel`, `sutra-contracts`,
`sutra-web` and `sutra-persistence` and **never** contains `FROM/JOIN/INTO/UPDATE <other schema>.`
(the architecture test fails): Domain B data comes through `InstitutionReferencePort`, Domain C
through `StudentDirectory` / `RegistrationReader`, Domain D through `OfferingDirectory` /
`AcademicExamFacts`, Domain F through `FinanceClearancePort` / `ChargeDirectory` / `ExamFeePort`
(§6). Cross-slice reads inside Domain I go through the **foundation ports / helpers**
(`state.directory`, `state.marks`, `shared::rows::*`, `shared::results::load_result`) for the
shared tables, and may query sibling slice tables of the `exams` schema **read-only** when a port
would be pointless (same crate); you never write another slice's tables.

### 2.1 Ownership map (create/edit freely inside your own paths)

| Path | Owner |
|------|-------|
| `modules/exams/src/{setup,scheduling,registration}/**` | **6A** |
| `modules/exams/src/{marks,integrity}/**` | **6B** |
| `modules/exams/src/{results,revaluation,completion}/**` | **6C** |
| `crates/contracts/src/exams/events/setup.rs` | **6A** |
| `crates/contracts/src/exams/events/marks.rs` | **6B** |
| `crates/contracts/src/exams/events/results.rs` | **6C** |
| `modules/platform/src/inbox/rules/exams_6a.rs` / `exams_6b.rs` / `exams_6c.rs` | 6A / 6B / 6C |
| `migrations/202611050000[1-2][0-9]_exams_*.sql` (`…010` – `…029`) | **6A** |
| `migrations/202611050000[3-4][0-9]_exams_*.sql` (`…030` – `…049`) | **6B** |
| `migrations/202611050000[5-6][0-9]_exams_*.sql` (`…050` – `…069`) | **6C** |
| `apps/api/tests/db_b6a.rs` / `db_b6b.rs` / `db_b6c.rs` | 6A / 6B / 6C |
| `apps/web/app/features/exams/setup/**` | **6A** |
| `apps/web/app/features/exams/marks/**` | **6B** |
| `apps/web/app/features/exams/results/**` | **6C** |
| `apps/web/e2e/exams.e2e.mjs` regions `// ==== 6X begin` … `// ==== 6X end` (created by the first slice that needs it, from `finance.e2e.mjs`; the integrator owns the file header) | each slice its own regions |
| README region `<!-- batch6:6X:begin … end -->`; `docs/batches.md` status cell `<!-- batch6:6X:status -->` | each slice its own region |

### 2.2 Shared files: foundation-owned, do not edit

`modules/exams/src/lib.rs` (router list already mounts every slice), `modules/exams/src/shared/**`,
`modules/exams/src/{directory,results_port}.rs`, `modules/exams/src/grading/**`,
`crates/contracts/src/exams/{mod,ids,status,ports,results}.rs`,
`crates/contracts/src/exams/events/mod.rs`, `crates/contracts/src/academic/ports.rs` (Batch 6
block), `crates/contracts/src/finance/{ports,status}.rs` (Batch 6 block / `ExamFee`),
`modules/academic/src/exam_facts.rs`, `modules/finance/src/exam_fees.rs`,
`crates/kernel/src/auth.rs` (all Batch 6 permissions already added),
`modules/platform/src/inbox/rules/mod.rs`, `apps/api/src/{app,openapi}.rs` (tags + `exams_deps`
already added), `apps/api/tests/architecture.rs`, `apps/web/app/routes.ts`,
`apps/web/app/lib/roles.ts`, `apps/web/app/features/exams/{index.ts,Placeholder.tsx,exams.test.tsx}`,
`apps/web/app/routes/admin/examinations.tsx`, `apps/web/app/routes/faculty/assessments.tsx`,
`apps/web/app/routes/student/results.tsx`, `apps/api/tests/support/{mod,b6}.rs`,
`apps/api/tests/db_b6_foundation.rs`, `migrations/20261105000000_exams_batch6_foundation.sql`,
`migrations/20261105000001_finance_exam_fee_source.sql`, `.github/workflows/sutraerp-ci.yml`.
Domain B, C, D and F code (`modules/{institution,student,academic,finance}/**`) is **off limits**
to all three slices (a needed change there is a contract change for the integrator).

If you cannot avoid touching one of these, keep the edit **append-only** at the end of the
relevant list/block, add a `// 6X:` (or `<!-- 6X: -->` / `-- 6X:`) comment, and list it in your PR
description. Examples: a new id at the end of your slice's block in `ids.rs`; a new number kind at
the end of your block in `shared::numbering::kind`; a new process key in your block of
`shared::approval::process`; a new variant in `status.rs` (contract change — say so); a new port
method (contract change — the foundation adapter must implement it).

### 2.3 Generated files

`openapi/openapi.json` and `packages/api-client/src/schema.d.ts` are **generated**. CI's
`OpenAPI drift` job regenerates them and uploads them as the artifact `openapi-regenerated`;
**commit the artifact's files** (download it through the GitHub API, §11) instead of generating
locally. On merge, **never hand-merge them**: take either side, then regenerate. `Cargo.lock` /
`pnpm-lock.yaml`: no new dependencies are expected (the foundation gave `sutra-exams` sqlx / uuid /
chrono / rust_decimal / serde_json / tracing / persistence; `sutra-academic` got rust_decimal). If
you add one, say so in the PR. `CHANGELOG.md`: slices don't edit it.

## 3. Database

### 3.1 Migrations and numbers

| Range | Owner |
|-------|-------|
| `20261105000000_exams_batch6_foundation.sql` | foundation (applied): `exams.grading_scheme`, `grade_band`, `exam_session`, `exam_paper`, `exam_registration`, `exam_registration_paper`, `paper_mark`, `paper_mark_history`, `term_result`, `term_result_line`, `number_sequence`, `status_history`; `exams.forbid_history_change()`; append-only / no-delete triggers; restated `sutra_exams` grants with `UPDATE/DELETE/TRUNCATE` **revoked** on the history, band and result-line tables |
| `20261105000001_finance_exam_fee_source.sql` | foundation (contract change to Domain F): `finance.charge.source_type` accepts `exam_fee`; unique `(tenant, source_id)` for `exam_fee` charges |
| `20261105000010` … `20261105000029` | 6A (policy, session revisions, schedules, sittings, venues, seating, invigilation, hall tickets, conduct, exceptions) |
| `20261105000030` … `20261105000049` | 6B (mark sheets, evaluations, moderation, marks exceptions, malpractice) |
| `20261105000050` … `20261105000069` | 6C (result batches, verification, publications, grade cards, revaluation, supplementary eligibility, completion, closure) |

Base `20261105…` is after the last Batch 5 migration (`20261030000031_finance_treasury.sql`).
Reserved names are **not** pre-created as empty files (sqlx checksums applied migrations). Name
files `<version>_exams_<what>.sql`. Never edit a migration after it has been applied anywhere; add
the next number. New tables pick up the `sutra_exams` runtime grants automatically (default
privileges of `20261014000003_runtime_db_roles.sql` cover schema `exams`); an append-only slice
table additionally gets `REVOKE UPDATE, DELETE, TRUNCATE ON exams.<t> FROM sutra_exams;` plus a
`BEFORE UPDATE OR DELETE` trigger reusing `exams.forbid_history_change()`.

**Decimal rules (all slices):** marks `numeric(7,2)`, percentages `numeric(5,2)`, credits
`numeric(5,2)` (`numeric(6,2)` for term totals), grade points `numeric(4,2)`, Σ credit × grade point
`numeric(8,2)`; Rust uses `rust_decimal::Decimal` end to end; **no `f32`/`f64`** except the frozen
Batch 3 `TermResultSummary` (converted only in `PgResultsPort`). Computed values are rounded **once**
with `shared::grading::round2` (half away from zero). On the wire marks / percentages / grade points
/ SGPA / CGPA are **JSON strings** (`"72.50"`, `shared::grading::fmt2`) with
`#[schema(value_type = String, example = "72.50")]`.

Table conventions: `id uuid PRIMARY KEY DEFAULT uuidv7()`, `tenant_id uuid NOT NULL`,
`institution_id uuid NOT NULL` (plain uuid — **no FK into `institution`**), `session_id uuid
REFERENCES exams.exam_session(id)` / `paper_id … exams.exam_paper(id)` / `registration_id …
exams.exam_registration(id)` / `paper_mark_id … exams.paper_mark(id)` / `result_id …
exams.term_result(id)` where the row belongs to one (FKs to the **foundation** tables are allowed
for every slice), `student_id` / `offering_id` / `section_id` / `charge_id` **plain** (other
domains, no FK), `status text NOT NULL CHECK (status IN (...))` with the wire names of §4.2,
`version bigint NOT NULL DEFAULT 1`, `created_at/updated_at timestamptz`, `process_instance_id
text` on every aggregate with an approval step. Business dates (exam dates, registration windows,
publication dates) are `date` in the institution's timezone (`shared::time::business_today`);
instants are `timestamptz`. **No foreign keys across slices** (plain uuid + validate by reading).

### 3.2 The shared examination tables (foundation)

- `exams.grading_scheme` / `grade_band` — per-institution schemes (`code` unique), bands
  (`grade` `^[A-Z][A-Z+-]{0,3}$`, `min_percent`, `grade_point` 0 … 10, `is_pass`), immutable
  bands; `status active | retired`. Foundation API (§5.2).
- `exams.exam_session` — `academic_term_id`, `code` (unique per institution), `name`, `kind`
  (ExamSessionKind), `policy_id` (6A, plain), `grading_scheme_id`, `starts_on`/`ends_on`,
  `registration_opens_on`/`closes_on`, `status` (ExamSessionStatus), `status_reason`,
  `process_instance_id`, `version`. No physical delete.
- `exams.exam_paper` — `session_id`, `offering_id` (Domain D, plain; one live paper per offering and
  session), `curriculum_component_id`, `academic_term_id`, `paper_code` (unique per session),
  `title`, `credits`, `mode` (ExamMode), `max_marks` (external max), `pass_percent`,
  `internal_weight` (% of the final from internal assessment), `duration_minutes`, `status`
  (ExamPaperStatus).
- `exams.exam_registration` — `session_id`, `student_id` / `student_context_id` (Domain C, plain)
  + snapshots `student_number`, `display_name`, `subject_ref` (login → self-service),
  `registration_number` (`EXR-000001`), `channel` (RegistrationChannel), `status`
  (ExamRegistrationStatus). **One live registration per candidate and session** (unique index).
- `exams.exam_registration_paper` — `registration_id`, `paper_id`, `attempt_kind`, `attempt_no`,
  `status` (RegistrationPaperStatus: registered / withdrawn / absent / appeared / debarred).
- `exams.paper_mark` — exactly one per registered paper (created with it): `internal_marks` /
  `internal_max` / `internal_source_id` (Domain D result), `external_marks` / `external_max`,
  `grace_marks`, `absent`, `status` (MarkStatus), `revision`, `finalised_at`. History:
  `exams.paper_mark_history` (append-only; cause, reason, actor, values).
- `exams.term_result` — one **current** row per candidate and session (unique index; revisions
  supersede): snapshots, `grading_scheme_id`, `credits_attempted`, `credits_earned`,
  `grade_points`, `sgpa`, `cgpa`, `outcome` (ResultOutcome), `status` (ResultStatus), `revision`,
  `supersedes_id`, `result_batch_id` (6C, plain), `published_at`. Lines:
  `exams.term_result_line` (immutable): paper snapshot, credits, attempt kind, internal / external /
  grace / total / max marks, `percent`, `grade`, `grade_point`, `passed`, `remark`.
- `exams.number_sequence` — gap-free per-institution counters (`shared::numbering`).
- `exams.status_history` — append-only: every status change of the shared aggregates (and of
  slice aggregates, via `shared::lifecycle::record_history`).

### 3.3 Write rules for the shared tables

| Change | Who | How |
|--------|-----|-----|
| grading schemes / bands | foundation | the `exam_grading_*` API only (create, retire) |
| INSERT / edit sessions | **6A** | own repo code (code, name, dates, policy, scheme while `is_configurable`) |
| `exam_session.status` | **6A** draft … reopened; **6C** `closing → closed → archived` | **only** `shared::lifecycle::change_session_status` on a row locked with `shared::rows::lock_session` |
| INSERT / edit papers | **6A** | own repo code while the paper is `draft | scheduled` |
| `exam_paper.status` | **6A** → `scheduled / ready / conducted / cancelled`; **6B** `conducted → evaluation → marks_finalised` (and back) | **only** `change_paper_status` (`lock_paper`) |
| INSERT registrations / registered papers (+ their `paper_mark` rows) | **6A** | **only** `shared::registration::{insert_registration, add_paper}` |
| `exam_registration.status` | **6A** (6C → `completed` at closure) | **only** `change_registration_status` (`lock_registration`) |
| `exam_registration_paper.status` | **6A** (withdrawal, attendance), **6B** (`debarred` by malpractice) | **only** `shared::registration::set_paper_status` |
| `paper_mark` values + status | **6B** | **only** `shared::marks::update` (on `lock_mark`); values change only on `→ entered`, `→ moderated` or the internal `import` |
| final marks revision | **6C** | **only** `shared::marks::revise_final` (revaluation / rechecking outcome, approved correction; reason required) |
| `term_result(_line)` | **6C** | **only** `shared::results::{insert_result, change_result_status}` |
| Read sessions / papers / registrations / marks / results | all | `state.directory` (`ExamDirectory`), `state.marks` (`MarksReader`), `shared::rows::*`, `shared::results::load_result` |

### 3.4 Document numbers (`shared::numbering::kind`)

`registration` `EXR` (foundation); `hall_ticket` `HT`, `exam_exception` `EXC` (6A);
`malpractice_case` `MAL` (6B); `grade_card` `GC`, `revaluation` `REV` (6C). Gap-free inside
committed transactions.

### 3.5 Grading engine (`shared::grading`, pure)

`paper_percent` = internal share (`internal / internal_max × internal_weight`, missing internal = 0)
+ external share (`min(external + grace, external_max) / external_max × (100 − internal_weight)`),
capped at 100, rounded half-up to 2 places; absent / no external marks → not gradable. `band_for`
= highest `min_percent ≤ %`; **passed** = band `is_pass` **and** `% ≥ pass_percent` (otherwise the
highest failing band). `aggregate` → credits attempted / earned, Σ credit × grade point, **SGPA** =
Σ(credits × gp) / Σ credits over every registered line (ungraded = 0 gp). **CGPA** = Σ grade points
/ Σ credits attempted over the student's current results of every term. `validate_bands` (one band
from 0 %, ≥ 1 pass and ≥ 1 failing band, unique grades / minimums, monotonic grade points).
6B's gradebook preview and 6C's official computation **must** both use these functions.
6A's examination policy may later make rounding / grace configurable — that is a contract change.

### 3.6 Slice table names (schema `exams`)

| Slice | Tables (prefixes a slice may extend) |
|-------|--------------------------------------|
| 6A | `exam_policy`, `exam_session_revision`, `exam_schedule`, `exam_sitting`, `exam_venue`, `venue_allocation`, `seating_plan`, `seat_assignment`, `invigilation_duty`, `hall_ticket`, `conduct_record`, `conduct_attendance`, `exam_exception`, `registration_override` (prefixes `exam_policy*`, `exam_session_*`, `exam_schedule*`, `exam_sitting*`, `exam_venue*`, `venue_*`, `seating_*`, `seat_*`, `invigilation_*`, `hall_ticket*`, `conduct_*`, `exam_exception*`, `registration_*`) |
| 6B | `mark_sheet`, `evaluation`, `evaluation_assignment`, `moderation`, `marks_exception`, `malpractice_case`, `malpractice_decision` (prefixes `mark_sheet*`, `evaluation*`, `moderation*`, `marks_*`, `malpractice_*`) |
| 6C | `result_batch`, `result_verification`, `publication`, `publication_item`, `grade_card`, `revaluation_request`, `supplementary_eligibility`, `completion_evaluation`, `session_closure` (prefixes `result_*`, `publication*`, `grade_card*`, `revaluation_*`, `supplementary_*`, `completion_*`, `session_closure*`) |

## 4. Kernel and contract types (already in the code)

### 4.1 Ids: `sutra_contracts::exams::ids`

`InstitutionId`/`TenantId` re-exported, plus:
- **foundation:** `GradingSchemeId`, `GradeBandId`, `ExamSessionId`, `ExamPaperId`, `ExamRegistrationId`, `ExamRegistrationPaperId`, `PaperMarkId`, `TermResultId`, `TermResultLineId`
- **6A:** `ExamPolicyId`, `ExamScheduleId`, `ExamSittingId`, `ExamVenueId`, `VenueAllocationId`, `SeatingPlanId`, `SeatAssignmentId`, `InvigilationDutyId`, `HallTicketId`, `ConductRecordId`, `ExamExceptionId`
- **6B:** `MarkSheetId`, `EvaluationId`, `ModerationId`, `MarksExceptionId`, `MalpracticeCaseId`, `MalpracticeDecisionId`
- **6C:** `ResultBatchId`, `ResultVerificationId`, `PublicationId`, `GradeCardId`, `RevaluationRequestId`, `SupplementaryEligibilityId`, `CompletionEvaluationId`, `SessionClosureId`

All are `typed_id!` newtypes: UUIDv7, serde-transparent, OpenAPI `string/uuid`.

### 4.2 Lifecycle states: `sutra_contracts::exams::status`

Enums with `ALL`, `as_str()`, `parse()`, serde/OpenAPI `snake_case`; also the DB `CHECK` values.
The **shared** machines carry `can_transition` in contracts (illegal → `409`):

| Machine | Values | Legal moves (summary) | Helpers |
|---------|--------|----------------------|---------|
| `ExamSessionStatus` (L2-I02 §20 + amendment, L2-I16) | `draft, under_review, approved, scheduled, active, suspended, reopened, closing, closed, archived` | draft → under_review → approved → scheduled → active; under_review / approved → draft; active / reopened → suspended → reopened; active / reopened → closing → closed → archived; closing → reopened | `is_operational()` (`active | reopened` — the only states operational workflows consume, L2-I02 decision 4), `is_configurable()`, `is_terminal()` |
| `ExamPaperStatus` | `draft, scheduled, ready, conducted, evaluation, marks_finalised, cancelled` | draft → scheduled → ready → conducted → evaluation → marks_finalised (each back one step allowed where listed); draft / scheduled / ready → cancelled | `accepts_marks()` (`conducted | evaluation`) |
| `ExamRegistrationStatus` (L2-I04 §20 + `rejected`) | `draft, pending_validation, pending_review, approved, confirmed, locked, amended, withdrawn, rejected, completed, archived` | draft → pending_validation → (pending_review →) approved | rejected; approved → confirmed → locked; confirmed / locked ↔ amended; confirmed / amended / locked / approved → withdrawn → confirmed (reinstatement); locked → completed; rejected / withdrawn / completed → archived | `is_effective()` (`approved | confirmed | locked | amended | completed`), `is_live()` |
| `MarkStatus` | `pending, entered, submitted, verified, moderated, approved, withheld, finalised` | pending → entered → submitted → verified → (moderated →) approved → finalised; entered ↔ submitted; review steps → withheld → entered / verified | `is_final()` (`finalised` — the only computation input), `is_editable()` |
| `ResultStatus` (L2-I09/I10 condensed) | `pending, computed, validated, approved, withheld, published, withdrawn, superseded, archived` | pending → computed → validated → approved → published; validated → computed; any pre-publication step → withheld → computed / approved; published ↔ withdrawn; non-archived → superseded (revision); superseded / published → archived | `is_published()`, `is_current()` |

**Classification enums (foundation):** `ExamSessionKind` (`regular, supplementary, improvement,
makeup, special`), `ExamMode` (`theory, practical, viva, project`), `RegistrationPaperStatus`,
`AttemptKind` (`regular, backlog, supplementary, improvement`), `RegistrationChannel`
(`self_service, bulk, administrative, late`), `GradingSchemeStatus`, `ResultOutcome` (`pass, fail,
withheld, incomplete, malpractice`).

**Slice enums (transitions slice-owned, in the slice's `domain.rs`):**

| Enum | Values | Used by |
|------|--------|---------|
| `ExamPolicyStatus` (L2-I01 §20) | `draft, under_review, approved, scheduled, active, superseded, retired, archived` | 6A |
| `ExamScheduleStatus` (L2-I03 §20) | `draft, under_review, approved, published, active, revised, cancelled, completed, archived` | 6A |
| `HallTicketStatus` (L2-I05 §20 + `withheld`) | `draft, under_review, approved, published, active, withheld, reissued, replaced, withdrawn, cancelled, completed, archived` | 6A |
| `SeatingPlanStatus` / `InvigilationDutyStatus` | `draft, published, revised, cancelled` / `assigned, confirmed, replaced, released, completed` | 6A |
| `ConductStatus` / `ExamExceptionStatus` | `scheduled, in_progress, suspended, completed, terminated, postponed` / `raised, classified, under_review, resolved, closed` | 6A |
| `EvaluationStatus` (L2-I08 §20) / `EvaluationRound` | `pending, planned, assigned, in_evaluation, evaluated, moderation, validated, approved, finalised, closed, archived` / `first, second, adjudication` | 6B |
| `ModerationStatus` | `requested, validated, approved, applied, rejected, withdrawn` | 6B |
| `MalpracticeCaseStatus` / `MalpracticePenalty` | `reported, under_investigation, hearing, decided, appealed, under_review, upheld, modified, overturned, closed, archived` / `none, warning, marks_deduction, paper_cancelled, session_cancelled, debarred` | 6B (6C reads) |
| `ResultBatchStatus` (L2-I09 §20) | `pending, aggregated, rule_processing, computed, validated, approved, finalised, ready_for_publication, closed, archived` | 6C |
| `PublicationStatus` (L2-I10 §20 + L3) | `planned, scheduled, authorised, ready, published, accessible, delayed, cancelled, failed, withdrawn, closed, archived` | 6C |
| `GradeCardStatus` / `ReviewKind` | `issued, reissued, withdrawn` / `revaluation, rechecking` | 6C |
| `RevaluationStatus` (L2-I11 §20 + amendment) | `submitted, eligibility_verified, fee_pending, fee_verified, approved, review_in_progress, review_completed, outcome_verified, revised_marks_approved, result_revision_initiated, correction_review, withdrawn, rejected, closed, archived` | 6C |
| `SupplementaryEligibilityStatus` / `CompletionEvaluationStatus` | `eligible, applied, authorised, exhausted, cancelled` / `pending, eligible, not_eligible, blocked, superseded` | 6C |

### 4.3 Events: `sutra_contracts::exams::events`

`type = sutraerp.exams.<aggregate>.<verb>`, `dataschema = urn:sutraerp:schema:exams.<aggregate>.<verb>:1`,
`source = /sutraerp/exams`, envelope `subject` = aggregate id, envelope scope =
`ExecutionScope::institution(tenant, <institution>)`. Emit via `shared::publish::event` inside the
owner-local tx; audit via `shared::publish::audit` with action `exams.<aggregate>.<command>`. V1
payloads are frozen: additive optional fields only. Decimals in payloads are strings.

Shared payloads (`events/mod.rs`): **`StatusChangedV1`** `{aggregateId, institutionId, sessionId?,
paperId?, registrationId?, studentId?, fromStatus?, toStatus, version, reason?, processInstanceId?,
studentSubjectRef?}`, **`ModifiedV1`** `{aggregateId, institutionId, version, changedFields[]}`,
**`ResultPublishedV1`** `{resultId, institutionId, sessionId, studentId, academicTermId, sgpa?, cgpa?,
creditsAttempted, creditsEarned, outcome, revision, supersedesResultId?, studentSubjectRef?}`.

| Owner / file | Constants | `type` suffixes |
|--------------|-----------|-----------------|
| foundation `mod.rs` | `GRADING_SCHEME_CREATED`, `GRADING_SCHEME_RETIRED` | `grading_scheme.created` / `retired` |
| 6A `setup.rs` | `EXAM_POLICY_ACTIVATED`, `_SUPERSEDED`, `_RETIRED`; `EXAM_SESSION_SUBMITTED`, `_APPROVED`, `_ACTIVATED`, `_SUSPENDED`, `_REOPENED`; `EXAM_SCHEDULE_APPROVED`, `EXAM_TIMETABLE_PUBLISHED`, `EXAM_TIMETABLE_WITHDRAWN`; `SEATING_PLAN_PUBLISHED`; `INVIGILATION_ASSIGNED`; `EXAM_REGISTRATION_CONFIRMED`, `_REJECTED`, `_WITHDRAWN`, `_LOCKED`; `HALL_TICKET_ISSUED`, `_WITHHELD`, `_WITHDRAWN`; `EXAM_CONDUCT_COMPLETED`, `EXAM_CONDUCT_SUSPENDED`, `EXAM_POSTPONED` | `exam_policy.*`, `exam_session.*`, `exam_schedule.approved/published/withdrawn`, `seating_plan.published`, `invigilation_duty.assigned`, `exam_registration.*`, `hall_ticket.*`, `conduct_record.completed/suspended/postponed` |
| 6B `marks.rs` | `MARKS_SUBMITTED`, `MARKS_VERIFIED`, `MARKS_MODERATED`, `MARKS_APPROVED`, `MARKS_FINALISED`, `MARKS_WITHHELD`, `SECOND_EVALUATION_REQUIRED`, `MALPRACTICE_REPORTED`, `MALPRACTICE_DECIDED`, `MALPRACTICE_REVIEWED`, `MALPRACTICE_CLOSED` | `mark_sheet.submitted/verified/approved/finalised`, `moderation.applied`, `paper_mark.withheld`, `evaluation.second_required`, `malpractice_case.*` |
| 6C `results.rs` | `RESULT_COMPUTED`, `RESULT_WITHHELD`, `RESULT_APPROVED`, `RESULT_PUBLISHED` (ResultPublishedV1), `RESULT_WITHDRAWN`, `RESULT_REVISED` (ResultPublishedV1), `PUBLICATION_DELAYED`, `GRADE_CARD_ISSUED`, `REVALUATION_REQUESTED`, `REVALUATION_DECIDED`, `REVALUATION_COMPLETED`, `SUPPLEMENTARY_ELIGIBILITY_DETERMINED`, `COMPLETION_EVALUATED`, `EXAM_SESSION_CLOSED` | `result_batch.*`, `term_result.*`, `publication.delayed`, `grade_card.issued`, `revaluation_request.*`, `supplementary_eligibility.determined`, `completion_evaluation.evaluated`, `exam_session.closed` |

Each slice file exports `EVENT_TYPES`; `events::all_event_types()` lists every one and a unit test
keeps them unique and well formed. A slice may add an event to its own file and must add it to the
table above in its PR. **Consumed events:** none required — Domain D / F facts are read through
ports when the user acts (no inbox projection into `exams`). Domain C may later consume
`term_result.published` / `revised` (not in Batch 6; 3B reads `ResultsPort`).

### 4.4 Permissions and role mapping (`sutra_kernel::auth::Permission`)

Staff reads of every Domain I resource require `exam.read` (Administrator **and Faculty**). Students
act only on their **own** examinations through `exam.self`: the handler resolves the registrations
from the session subject (`shared::me::{my_registrations, require_own_registration}`) and never
takes a student or registration id from the client. Faculty enter marks with `exam.marks.enter`
**only on papers whose offering has a section they teach** (`state.offerings.teaches(...)` on any
section of the paper's offering, else 404 — they never learn about other papers). Governed
**decisions** use `exam.approve` (step-up); result approval / publication / withdrawal and grade
cards use `exam.result.publish` (step-up). Unit test `batch6_domain_i_grants`.

| Variant | Name | Step-up | Roles | Slice: used for |
|---------|------|---------|-------|-----------------|
| `ExamRead` | `exam.read` | no | Admin, Faculty | all: staff reads |
| `ExamSelfService` | `exam.self` | no | Student | 6A own registration / hall ticket / timetable; 6C own results, grade cards, revaluation requests |
| `ExamSetupManage` | `exam.setup.manage` | no | Admin | foundation grading schemes; 6A policy, sessions, papers, schedules, venues, seating, invigilation, conduct, exceptions |
| `ExamRegistrationManage` | `exam.registration.manage` | no | Admin | 6A registrations (bulk, late, administrative, amendments, withdrawal, finalisation) and hall tickets |
| `MarksEntry` | `exam.marks.enter` | no | Admin, Faculty | 6B marks entry / submission (faculty: own sections only) |
| `MarksManage` | `exam.marks.manage` | no | Admin | 6B internal import, verification, mark sheets, evaluation rounds, moderation requests, withheld marks, malpractice cases |
| `ResultManage` | `exam.result.manage` | no | Admin | 6C computation, verification, withholding, revaluation processing, supplementary eligibility, completion, closure |
| `ResultPublish` | `exam.result.publish` | **yes** | Admin | 6C approve / publish / delay / withdraw / republish results, grade cards |
| `ExamApprove` | `exam.approve` | **yes** | Admin | all: approval **decisions** (§9) |

Step-up is enforced only when the deployment enables it; DB tests run in dev mode without it.

## 5. HTTP API

### 5.1 Conventions (as Domains B, C, D and F)

- Every handler starts with `auth.require(Permission::…)?` (or `shared::me::*`). Tenant from the
  session; build `shared::scope::institution_scope(ctx, <institution>)` for events/audit.
- Before a write: `shared::guard::require_institution_writable`; operational commands
  (registration, conduct, marks, results) additionally `shared::guard::require_operational(&session)`.
- Create: `POST`, required `Idempotency-Key` (`sutra_web::idempotency::require_idempotency_key`),
  `201` + `ETag`. Update or state command: `If-Match` required (`shared::http::if_match`: 428
  missing, 412 stale); response = updated resource + `ETag`. Bulk commands (bulk registration,
  hall ticket issuance, internal import, result computation, publication) also require
  `Idempotency-Key` and return a summary `{processed, skipped, failed[]}`.
- State commands are `POST …/{id}/<verb>` with an optional `{ "reason": "…" }`; literal paths per
  command, no generic `/{command}` route.
- Lists: `GET` collection under the parent, `?status=` filter, `{ "items": [...] }`; `?limit=` ≤ 200
  (default 50) and `nextCursor` where lists grow (registrations, marks, results).
- Errors: illegal transition / session not operational / duplicate → `409`; validation (marks out of
  range, dates) → `422`; foreign tenant / unknown id / not your section → `404`; Domain B/C/D
  reference not found → `422` on the field.
- JSON members camelCase, statuses snake_case (§4.2), business dates `YYYY-MM-DD`, **decimals as
  strings**.
- **Every `#[utoipa::path]` sets an explicit `operation_id`** with the slice prefix below and the
  slice's `tag` (enforced by `architecture.rs::exam_operations_follow_the_slice_prefix_and_tag_rules`,
  which also reserves the `exam_` prefix for Domain I and checks the `{id}` rule).

### 5.2 Path prefixes, operation_id prefixes, tags (all under `/api/v1/exams`)

**Path parameter rule:** the first parameter after any collection segment is always `{id}`;
deeper parameters get names (`/sessions/{id}/papers/{paperId}`). Self-service lives under `/me/…`.

| Owner | Paths | operation_id prefixes | tag |
|-------|-------|-----------------------|-----|
| foundation | `GET/POST /institutions/{id}/grading-schemes`, `GET /grading-schemes/{id}`, `POST /grading-schemes/{id}/retire` | `exam_grading_` | `exam-grading` |
| 6A | `/institutions/{id}/exam-policies`, `/exam-policies/{id}[/submit,/decision,/activate,/supersede,/retire]`, `/institutions/{id}/exam-sessions`, `/exam-sessions/{id}[/submit,/decision,/schedule,/activate,/suspend,/reopen,/merge]`, `/exam-sessions/{id}/papers`, `/exam-papers/{id}[/schedule,/ready,/conducted,/cancel]` | `exam_policy_`, `exam_session_`, `exam_paper_` | `exam-setup` |
| 6A | `/exam-sessions/{id}/schedules`, `/exam-schedules/{id}[/submit,/decision,/publish,/revise,/withdraw]`, `/exam-schedules/{id}/sittings`, `/exam-sittings/{id}[/…]`, `/institutions/{id}/exam-venues`, `/exam-venues/{id}`, `/exam-sittings/{id}/seating`, `/seating-plans/{id}[/publish,/revise]`, `/exam-sittings/{id}/invigilation`, `/invigilation-duties/{id}[/replace,/release]`, `/exam-sittings/{id}/conduct`, `/conduct-records/{id}[/start,/suspend,/resume,/complete,/postpone]`, `/exam-sessions/{id}/exceptions`, `/exam-exceptions/{id}[/…]`, `/me/exam-timetable`, `/me/invigilation` | `exam_schedule_`, `exam_sitting_`, `exam_venue_`, `exam_seating_`, `exam_invigilation_`, `exam_conduct_`, `exam_exception_` | `exam-scheduling` |
| 6A | `/exam-sessions/{id}/registrations[/bulk]`, `/exam-registrations/{id}[/submit,/decision,/confirm,/amend,/withdraw,/reinstate]`, `/exam-registrations/{id}/papers`, `/exam-sessions/{id}/registration-population/finalise`, `/exam-sessions/{id}/hall-tickets[/issue]`, `/hall-tickets/{id}[/withhold,/release,/withdraw,/reissue]`, `/me/exam-registrations[/…]`, `/me/hall-tickets` | `exam_registration_` (incl. `exam_registration_self_`), `exam_hall_ticket_` (incl. `exam_hall_ticket_self_`) | `exam-registration` |
| 6B | `/exam-papers/{id}/mark-sheet`, `/mark-sheets/{id}[/import-internal,/submit,/verify,/decision,/finalise]`, `/exam-papers/{id}/marks`, `/paper-marks/{id}[/withhold,/release]`, `/exam-papers/{id}/evaluations`, `/evaluations/{id}[/…]`, `/exam-papers/{id}/moderation`, `/moderations/{id}[/decision,/apply]`, `/exam-papers/{id}/gradebook`, `/me/assessments` (faculty: papers of their sections), `/me/assessments/{id}/marks` | `exam_mark_sheet_`, `exam_marks_` (incl. `exam_marks_self_`), `exam_evaluation_`, `exam_moderation_`, `exam_gradebook_` | `exam-marks` |
| 6B | `/exam-sessions/{id}/malpractice-cases`, `/malpractice-cases/{id}[/investigate,/hearing,/decision,/appeal,/review,/reopen,/merge,/close,/archive]` | `exam_malpractice_` | `exam-integrity` |
| 6C | `/exam-sessions/{id}/result-batches`, `/result-batches/{id}[/compute,/validate,/decision,/finalise]`, `/exam-sessions/{id}/results`, `/term-results/{id}[/withhold,/release,/recompute,/correct,/withdraw,/restore]`, `/exam-sessions/{id}/publications`, `/publications/{id}[/authorise,/publish,/delay,/resume,/cancel,/retry]`, `/term-results/{id}/grade-card`, `/grade-cards/{id}[/reissue,/withdraw]`, `/me/results`, `/me/grade-cards` | `exam_result_` (incl. `exam_result_self_`), `exam_publication_`, `exam_grade_card_` (incl. `exam_grade_card_self_`) | `exam-results` |
| 6C | `/exam-sessions/{id}/revaluation-requests`, `/revaluation-requests/{id}[/verify,/decision,/start-review,/complete,/verify-outcome,/approve-marks,/withdraw,/reopen,/correct]`, `/me/revaluation-requests`, `/exam-sessions/{id}/supplementary-eligibility`, `/supplementary-eligibility/{id}[/authorise]` | `exam_revaluation_` (incl. `exam_revaluation_self_`), `exam_supplementary_` | `exam-revaluation` |
| 6C | `/institutions/{id}/completion-evaluations`, `/completion-evaluations/{id}[/…]`, `/exam-sessions/{id}/closure[/initiate,/close,/archive]` | `exam_completion_`, `exam_closure_` | `exam-completion` |

**Additional paths implemented by the slices (integrator, after 6A → 6B → 6C merged)** — same
prefix / tag rules, all covered by the architecture test:

| Owner | Extra paths |
|-------|-------------|
| 6A setup / scheduling | `POST /exam-sessions/{id}/papers/generate`, `GET /exam-schedules/{id}/conflicts`, `POST /exam-sittings/{id}/venues/{venueId}/release`, `POST /invigilation-duties/{id}/confirm`, `POST /conduct-records/{id}/decision` |
| 6A registration | `POST /exam-registrations/{id}/arrangements`, `POST /exam-registrations/{id}/scope-sync`, `GET/POST /exam-sessions/{id}/registration-overrides`, `POST /registration-overrides/{id}/decision`, `POST /exam-sessions/{id}/registration-window/reopen`, `POST /exam-sessions/{id}/hall-tickets/finalise`, `POST /me/exam-registrations/{id}/withdraw` |
| 6B marks / integrity | `GET /institutions/{id}/mark-sheets`, `POST /mark-sheets/{id}/return`, `POST /mark-sheets/{id}/request-approval`, `GET/POST /mark-sheets/{id}/exceptions`, `POST /marks-exceptions/{id}/resolve`, `GET/POST /mark-sheets/{id}/assignments`, `POST /moderations/{id}/validate`, `POST /moderations/{id}/withdraw`, `POST /malpractice-cases/{id}/approval` |
| 6C results / revaluation / completion | `POST /result-batches/{id}/reopen`, `POST /result-decisions/{id}/decision`, `POST /publications/{id}/decision`, `POST /me/revaluation-requests/{id}/withdraw`, `POST /exam-sessions/{id}/closure/decision` |

The tags are already in `apps/api/src/openapi.rs`; the router mount points are already registered
in `sutra_exams::router_with_state` (`grading` → foundation; `setup`, `scheduling`,
`registration` → 6A; `marks`, `integrity` → 6B; `results`, `revaluation`, `completion` → 6C). Each
slice only fills `pub fn router(state: ExamsState) -> OpenApiRouter` in its own `mod.rs`.

### 5.3 Minimum response fields other slices / screens rely on

- **Session** (6A): `{id, institutionId, academicTermId, code, name, kind, status, policyId?,
  gradingSchemeId?, startsOn?, endsOn?, registrationOpensOn?, registrationClosesOn?, papers[{id,
  paperCode, title, credits, mode, maxMarks, passPercent, internalWeight, status}], version}`.
- **Registration** (6A): `{id, registrationNumber, sessionId, studentId, studentNumber,
  displayName, channel, status, papers[{registrationPaperId, paperId, paperCode, attemptKind,
  attemptNo, status}], hallTicket?{id, number, status}, version}`.
- **Marks row** (6B): `{id, paperId, registrationId, studentNumber, displayName, internalMarks?,
  internalMax?, externalMarks?, externalMax, graceMarks, absent, status, preview?{percent, grade,
  gradePoint, passed}, version}`.
- **Term result** (6C): `{id, sessionId, registrationId, studentId, studentNumber, displayName,
  academicTermId, creditsAttempted, creditsEarned, gradePoints, sgpa?, cgpa?, outcome, status,
  revision, supersedesId?, publishedAt?, lines[{lineNo, paperId, paperCode, title, credits,
  attemptKind, internalMarks?, externalMarks?, graceMarks, totalMarks?, maxMarks, percent?, grade?,
  gradePoint?, passed, remark?}], version}` (student view: only `published` / `withdrawn` notice).
- **Revaluation request** (6C): `{id, number, kind, registrationId, paperId, status, feeChargeId?,
  feeChargeNumber?, feeSettled, outcome?{fromMarks, toMarks, changed}, processInstanceId?, version}`.
- Collections: `{ "items": [ … ] }`.

## 6. Cross-slice runtime rules and ports

### 6.1 Foundation helpers (`modules/exams/src/shared/`)

- `state::ExamsState` — `pool, workflow, approvals, references, students, registrations,
  offerings, academic, finance, charges, fees, directory, marks`. Built by
  `ExamsDeps::into_state()` (auto-approve, Postgres adapters) or by hand in tests (port doubles,
  `AwaitDecision`).
- `scope`, `guard` (`require_institution_writable`, `require_institution`, `require_operational`),
  `http` (`with_etag`, `if_match`, `actor`), `publish` (`event` + `audit`), `approval` (§9),
  `time::business_today`, `numbering::next` (§3.4).
- `rows` — column lists + decoders (`SESSION_COLUMNS`/`session_from_row`, `PAPER_*`,
  `REGISTRATION_*`, `MARK_*`), `lock_session`, `lock_paper`, `lock_registration`,
  `attach_papers`, `load_scheme`, `decode`.
- `lifecycle` — `change_session_status`, `change_paper_status`, `change_registration_status`,
  `record_history` (any slice, own aggregates), `StatusChange`, `HistoryEntry`.
- `registration` — `insert_registration(NewRegistration)`, `add_paper`, `set_paper_status`.
- `marks` — `lock_mark`, `update(values?, to, cause, reason)`, `revise_final`, `validate`,
  `MarkValues::of`.
- `results` — `insert_result(NewResult)` (supersedes the current one), `change_result_status`,
  `load_result`, `ResultRow` / `ResultLineRow`.
- `grading` — `paper_percent`, `band_for`, `grade_line`, `aggregate`, `cgpa`, `validate_bands`,
  `round2`, `fmt2`, `ten_point_default`.
- `me` — `my_registrations`, `require_own_registration`.

### 6.2 Typed ports (all defined and wired by the foundation)

| Port | Defined in | Implementation wired now | Consumer | Notes |
|------|-----------|--------------------------|----------|-------|
| **`ExamDirectory`** (`session`, `sessions_of_institution`, `grading_scheme`, `paper`, `papers_of_session`, `registration`, `registrations_of_session`, `registration_of_student`, `registrations_of_subject`) — "6A → 6B / 6C" | `sutra_contracts::exams::ports` | `sutra_exams::PgExamDirectory` (**real**) | 6B (candidates of a paper), 6C (candidates of a session), self-service, later domains | 6B/6C never wait for 6A: the shape is foundation-owned and seeded in tests by `support::b6`. |
| **`MarksReader`** (`marks_of_paper`, `marks_of_registration`) — "6B → 6C" | same | `sutra_exams::PgMarksReader` (**real**) | 6C computation (only `finalised` rows count), Student Results detail | 6C seeds final marks with `support::b6::seed_final_marks` (same helper path as 6B). |
| **`ResultsPort`** (Batch 3, `term_result`) — Exams → Domain C | `sutra_contracts::exams::results` | `sutra_exams::PgResultsPort` (**real**, replaces `NoPublishedResults` in `student_deps`) | 3B progression evidence | Only `published` rows; `None` before 6C publishes (Batch 3 behaviour unchanged: db_b3b stays green). Frozen `f64` members converted only here. |
| **`AcademicExamFacts`** (`internal_results(offering)`, `attendance_standing(offering, student)`) — Domain D → 6B / 6A | `sutra_contracts::academic::ports` (Batch 6 block) | `sutra_academic::PgAcademicExamFacts` over 4B `internal_assessment_result` (`finalized | archived`) + `attendance_evaluation` (**real**) | 6B internal import (`paper_mark.internal_*`, `internal_source_id` = `result_id`), 6A hall-ticket attendance condition (`below_threshold`) | **No cross-schema SQL** in `sutra-exams`. Contract change to Domain D (adapter file foundation-owned). |
| **`OfferingDirectory`** (Batch 4) | `sutra_contracts::academic::ports` | `sutra_academic::PgOfferingDirectory` | 6A papers / candidates per offering, 6B `teaches` (faculty marks entry) | |
| **`RegistrationReader`** / **`StudentDirectory`** (Batch 3) | `sutra_contracts::student::ports` | `sutra_student::{PgRegistrationReader, PgStudentDirectory}` | 6A registration scope and eligibility (`TermRegistration::covers(component)`, live context), snapshots | |
| **`FinanceClearancePort`** (Batch 3, real since Batch 5) | `sutra_contracts::finance` | `sutra_finance::PgFinanceClearance` | 6A hall tickets: `student_dues` must `permits_clearance()` (else `withheld`, reason = the evidence detail) | Policy may waive the condition (6A policy flag). |
| **`ExamFeePort`** (`raise_exam_fee`) — Exams → Domain F | `sutra_contracts::finance::ports` (Batch 6 block) | `sutra_finance::PgExamFees` (**real**: fee head created on first use, party account, charge `source_type = exam_fee` + `source_id`, validated + **published** with its ledger posting, `charge.published`) | 6C revaluation / rechecking fee (L2-I11 "Fee Verified"); 6A may use it for late-registration fees | Idempotent on `source_id`. Settlement: `state.charges.charge(id)` → `outstanding_amount == 0` (paid through 5B on the Student Fees screen). Contract change to Domain F (+ `ChargeSourceType::ExamFee`, migration `…000001`). |
| **`ChargeDirectory`** (Batch 5) | `sutra_contracts::finance::ports` | `sutra_finance::PgChargeDirectory` | 6C fee settlement check | |
| Domain B reference | `sutra_contracts::institution::reference` | `PgInstitutionReferences` | all | institution + timezone, academic years / terms, curriculum components (credits). |

Tests can swap any port by building an `ExamsState` by hand and serving
`sutra_exams::router_with_state(state)`; `sutra_api::app::exams_deps(pool, workflow)` gives the
production adapters.

## 7. Cross-slice dependencies and readiness

- **Sessions, papers, registrations, marks and results are foundation-owned tables**, so 6B / 6C
  never wait for 6A and 6C never waits for 6B: tests seed through `apps/api/tests/support/b6.rs`
  (`exam_setup` → active institution + enrolled student linked to a login + finance account +
  CS101 offering + ten-point scheme + session + paper; `seed_registration` →
  `insert_registration`; `seed_final_marks` → `marks::update` through the machine;
  `seed_internal_result` → a finalised 4B result). 6A's / 6B's real flows create the same shape.
- **6A → 6B:** 6B enters marks only for **effective** registrations (`is_effective`) on papers that
  `accepts_marks()` (6A moves the paper to `conducted`; conduct attendance sets registered papers
  `absent` / `appeared` — an `absent` paper's mark row is stored `absent = true`).
- **6B → 6C:** 6C computes only from `finalised` marks; a paper with any non-final effective
  candidate makes that candidate's result `withheld` (reason) — never computed from drafts. 6B
  moves the paper to `marks_finalised` when every candidate is final; 6C may require it per policy.
- **6B malpractice → 6C:** 6C reads 6B's `malpractice_case` / `malpractice_decision` rows
  read-only (`decided | upheld | modified` with a penalty) when computing (outcome `malpractice`,
  paper / session cancelled → not passed; `marks_deduction` reduces external marks in the
  computation input only, never in `paper_mark`).
- **6C revaluation → marks:** revised marks go through `shared::marks::revise_final`, then 6C
  recomputes (new result revision, `term_result.revised`).
- **Finance:** hall-ticket dues through `FinanceClearancePort`; fees through `ExamFeePort` +
  `ChargeDirectory`; never `finance.*` SQL.
- Merge order: **6A → 6B → 6C** (§12).

## 8. Inbox consumer hooks (`modules/platform/src/inbox/rules/`)

`rules::plan` dispatches by event type to `exams_6a.rs` (6A), `exams_6b.rs` (6B), `exams_6c.rs`
(6C); each declares `EVENT_TYPES` (only types from its own event file — test
`slices_never_claim_the_same_type`) and a pure `plan`. The foundation files start with **empty**
`EVENT_TYPES`; add a type only **with** its rule. Category: reuse `institution` (no new notification
category, so no platform migration). Notify students with `Recipient::Subject(studentSubjectRef)`;
otherwise `Recipient::Role(…)`. To-dos with `todos::SystemTodo`.

| Slice | Types | Projection |
|-------|-------|------------|
| 6A | `exam_schedule.published`, `hall_ticket.issued`, `hall_ticket.withheld`, `exam_registration.rejected` | notification to the student (link `/results`) / faculty role for the timetable |
| 6A | `exam_session.submitted` (AwaitDecision) | to-do (admins) "Approve examination session …" |
| 6B | `mark_sheet.submitted` | to-do (admins) "Verify marks …" |
| 6B | `paper_mark.withheld`, `malpractice_case.decided`, `malpractice_case.reviewed` | notification to the student |
| 6C | `term_result.published`, `term_result.revised`, `term_result.withdrawn`, `revaluation_request.decided`, `revaluation_request.completed` | notification to the student (link `/results`) |
| 6C | `result_batch.computed` (AwaitDecision) | to-do (admins) "Approve results …" |

## 9. WorkflowPort usage for approval steps

`shared::approval` wraps `WorkflowPort` exactly like Domains B, C, D and F: the composition root
passes the platform `NoopWorkflowAdapter` and `ApprovalMode::AutoApprove`.

1. Validate the command and current state (read only).
2. `request_approval(&state, &scope, process::<KEY>, business_key(aggregate, id, version), vars)`
   **before** opening the tx (deterministic key → retry-safe).
3. One owner-local tx: re-check the version; set the pending state; store `process_instance_id`;
   emit the *submitted* event. If the outcome is `ApprovalOutcome::Approved`, **also** apply the
   decision in the same tx (final state + decision event with `processInstanceId`, audit actor
   `system:auto-approval`).
4. Always expose the explicit decision endpoint (`POST …/{id}/decision {decision: approve|reject,
   reason}` with `exam.approve` — or `exam.result.publish` for results) so `AwaitDecision`
   (Flowable, Batch 9) needs no API change. Each slice has at least one DB test building an
   `ExamsState` with `ApprovalMode::AwaitDecision` (via `sutra_exams::router_with_state` and
   `sutra_api::app::exams_deps`).

Process keys (`shared::approval::process`): 6A `POLICY_APPROVAL`, `SESSION_APPROVAL`,
`SCHEDULE_APPROVAL`, `REGISTRATION_REVIEW`, `DEADLINE_OVERRIDE`, `HALL_TICKET_APPROVAL`,
`CONTINGENCY_DECISION`; 6B **`MARKS_APPROVAL` (the reference approval of Batch 6: mark sheet
verified → approved | returned)**, `MODERATION_APPROVAL`, `MALPRACTICE_DECISION`,
`MALPRACTICE_REVIEW`; 6C `RESULT_APPROVAL`, `RESULT_PUBLICATION`, `RESULT_WITHDRAWAL`,
`RESULT_CORRECTION`, `REVALUATION_DECISION`, `SESSION_CLOSURE`. Use only the keys of your slice; a
new key is a contract change.

## 10. Web

**The UI is being re-skinned** (branch `ui/reskin`, new design from the prototype in
`/workspace/work/ui-proto`). Batch 6 screens use **only `@sutra/ui` components** (BentoTile,
DataTable, Tabs, Button, Badge, Input/Select, Modal/Drawer, EmptyState, StatTile, Steps) and **no
custom CSS / class names / inline styles**, so the re-skin restyles them without edits. Do not add
stylesheets; if a component is missing, compose existing ones and note it for the integrator.

| Route | File (owner) | Content |
|-------|--------------|---------|
| `/examinations/:tab?` | `routes/admin/examinations.tsx` (foundation frame, pre-wired) | Admin → Examinations tabs (`EXAM_TABS` in `features/exams/index.ts`): `sessions`, `schedule`, `registrations` (6A), `marks`, `malpractice` (6B), `results`, `revaluation` (6C). Uses `features/institution/AdminScreen` (admin gate + institution picker). Linked from the admin nav ("Examinations", already present). |
| `/assessments` | `routes/faculty/assessments.tsx` (foundation frame) | Faculty Assessments: `FacultyAssessmentsPanel` (6B: papers of the faculty member's sections, marks entry, gradebook). Linked from the faculty nav ("Assessments"). Uses `features/academic/RoleScreen` (faculty only). |
| `/results` | `routes/student/results.tsx` (foundation frame) | Student Results: `StudentResultsPanel` (6C, span 8: results, grade cards, SGPA / CGPA, revaluation requests) + `StudentExamPanel` (6A, span 4: registration, hall ticket, timetable). Linked from the student nav ("Results"). Uses `RoleScreen` (student only). |

Each slice fills the placeholder components exported from its folder — keep the export names:
`features/exams/setup/index.tsx`: `SessionsAdminTab, ScheduleAdminTab, RegistrationsAdminTab,
StudentExamPanel`; `features/exams/marks/index.tsx`: `MarksAdminTab, MalpracticeAdminTab,
FacultyAssessmentsPanel`; `features/exams/results/index.tsx`: `ResultsAdminTab,
RevaluationAdminTab, StudentResultsPanel`. Mark real screens with `data-slice="6X"` (the foundation
test accepts either the placeholder or `data-slice`). Put API hooks in your own folder
(`features/exams/<slice>/api.ts`), use `@sutra/api-client` after committing the regenerated client,
decimals as strings (never `parseFloat` a mark for display), and a mock mode like
`features/institution/mock.ts`. Until a sibling slice is merged, type its calls locally in your
folder. Never edit another slice's folder.

**E2E:** `apps/web/e2e/exams.e2e.mjs` (`pnpm e2e:exams`, added by the integrator / first slice that
needs it, reusing `e2e/lib/harness.mjs` and the `finance.e2e.mjs` pattern) runs **in CI** (a job
`e2e-exams` like `e2e-finance`, screenshots uploaded as an artifact) — never locally.

## 11. Worker environment: CI-first

| Slice | Worktree | Branch | Local check target dir |
|-------|----------|--------|------------------------|
| 6A | `/workspace/work/erp-6a` | `batch6/6a` | `/workspace/work/targets/6a` |
| 6B | `/workspace/work/erp-6b` | `batch6/6b` | `/workspace/work/targets/6b` |
| 6C | `/workspace/work/erp-6c` | `batch6/6c` | `/workspace/work/targets/6c` (or a finished sibling's) |

**CI-first, no heavy local builds** (~2 GB free in the sandbox and a web worker running): the only
local Rust allowed is `cargo check -j1 -p sutra-exams [--tests]` (or `-p sutra-contracts`)
through `/workspace/work/tools/cargo-locked.sh`, plus `cargo fmt --all`. **No local `cargo test`,
`clippy --workspace`, DB tests, `pnpm build` or e2e**: push (without `[skip ci]`) and poll the
`SutraERP CI` workflow through the GitHub API (`/workspace/work/tools/b6-worker-common.md` has the
exact commands): fmt · clippy · workspace tests (`Rust`), full DB suite incl. `db_b6_foundation`
+ your `db_b6X` + every regression (`Rust integration`), `OpenAPI drift` (commit the regenerated
spec + TS client from the `openapi-regenerated` artifact), `Web`. Fix until all are green.

## 12. Merge order and known gaps

- Merge order: **6A → 6B → 6C** (6B depends on 6A only through the foundation tables; 6C on 6B only
  through `paper_mark` / `MarksReader` and read-only malpractice rows). Regenerate OpenAPI/TS after
  each merge (§2.3) — from the CI artifact. After 6C merges, the integrator adds a cross-slice test
  (session active → registration → hall ticket (dues cleared) → conduct → internal import + marks
  entry → final marks approval → computation (SGPA / CGPA) → approval → publication → `ResultsPort`
  sees it → revaluation with an `exam_fee` charge paid through 5B → revised result → closure) in
  `apps/api/tests/db_b6_integration.rs`, and the `e2e-exams` CI job.
- Session pinning to one institution is not enforced for Domain I (tenant-wide staff admin, as
  Batches 2–5).
- Venues are Exams-owned (no Campus room FK until Batch 8); invigilators / evaluators are login
  subjects (no People staff record until Batch 7).
- `ApprovalMode::AwaitDecision` is wired but unused in production until the Flowable adapter lands
  (Batch 9).
- The Batch 3 `TermResultSummary` keeps `f64` members (frozen contract); Exams converts at the port.
- Institution status is checked through the reference port without a cross-schema row lock.

### 12.1 Integrator wiring (batch6/integrate)

- 6C `results::repo::decided_penalties` reads 6B's `exams.malpractice_case` read-only: cases with an
  effective penalty in `decided | upheld | modified` (and after `closed | archived` with that
  decision in place), skipping merged duplicates; overturned findings carry `none`; a case reopened
  for investigation stops counting until decided again (`db_b6_integration`).
- 6B treats 6A's registered paper status `absent` (conduct) as absent on mark sheets and in the
  gradebook: no external marks can be stored, and a still-pending absent row is submitted as absent
  instead of blocking the sheet (`db_b6_integration`).
- 6C supplementary eligibility takes the attempt limit from 6A's policy (`exam_policy.max_attempts`
  of the session's policy, else the institution's active policy; 6C default 4 otherwise). The
  revaluation review window (15 days) and the revaluation fee stay 6C constants: the 6A policy has
  no such fields.
