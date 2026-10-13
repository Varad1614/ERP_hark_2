# Batch 4 — shared contracts (Academic delivery, Domain D)

**Status:** frozen at `batch4/foundation`. This file is the single source of truth for the three
parallel slices **4A**, **4B** and **4C**. If a slice needs to change anything in §2–§11, it says so
in its PR description ("contract change: …") instead of silently editing a shared file; the
integrator applies it on top of the merge.

Architecture inputs: L2-D00…D07 (+ L2-D05 business-architecture purity amendment 2026-08-24);
L3/L4 D-001…D-057; ADR-L5-0003 (module boundaries), -0004 (scope), -0006/-0007 (owner-local tx +
outbox), -0011/-0035 (event versioning/envelope), -0023/-0042 (WorkflowPort / Flowable), -0034
(API profile), -0036 (schemas, business timezone); L6 screens Student Timetable / Courses /
Assignments / Library, Faculty My classes / Attendance / Timetable / Resources, Admin Academics.

Plan line (Batch 4): *Courses, sections, faculty allocation, timetable generation and changes,
attendance capture and thresholds, coursework, assignments, resources; faculty "My classes".*

**Key lesson from Batch 3 applied here:** every cross-slice and cross-domain dependency is a
**typed port defined up front** (§6) with a real or stub implementation already wired, so no slice
waits for another and nothing is wired late at integration.

---

## 1. Slices and scenario coverage (one wave, three parallel slices)

| Slice | Scope | L3/L4 scenarios |
|-------|-------|-----------------|
| **4A** | Course offerings: establishment → **approval through the WorkflowPort** (the reference approval of Batch 4; auto-approve + `AwaitDecision` test) → publication → enrolment window → activation; suspension, reactivation, administrative cancellation, capacity governance, enrolment eligibility and restriction (**gated on 3B registrations through `RegistrationReader`**), waitlist → seat, cross-campus enrolment, revision/replacement/rollback, archive, merge/split, template-based generation, freeze → governed unfreeze, enrolment windows, quotas, change simulation → impact decision. Sections (create, capacity, activate, cancel). Section enrolment (seat / drop / withdraw / transfer). Faculty academic assignment: establishment → approval → activation, operational change, governance configuration. Screens: Admin → Academic delivery tabs `offerings`, `faculty`; student **Courses** (`/courses`); faculty **My classes** (`/classes`) and **My students** (`/my-students`). | D-001 … D-019 |
| **4B** | Timetable: scheduling requirement → generated draft → validation → approval → publication → operational; governed revision; retirement. Teaching sessions from slots → outcome (start, pause, complete, cancel, abandon, lock). Attendance capture → finalisation, governed correction, **attendance thresholds** (policy + evaluation → `attendance.threshold_breached`). Syllabus coverage → published record. Academic monitoring: requirement → outcome, risk assessment, exceptions, interventions, governance review, certification/closure, reopening. Internal assessment and **coursework/assignments**: blueprint → window/assignment → student submission → moderation → finalisation → archive, governed correction. Screens: Admin tabs `timetable`, `attendance`, `monitoring`, `assessment`; student **Timetable** (`/timetable`) and **Assignments** (`/assignments`); faculty **Timetable** (`/timetable`) and **Attendance** (`/attendance`). | D-020 … D-033, D-051 … D-057 |
| **4C** | Flexible / external learning (open electives, MOOCs, SWAYAM/NPTEL, internships): opportunity establishment → active availability, suspension, archival; provider onboarding; flexible-learning policy + approval configuration; demand → seat allocation → waitlist, allocation conflicts; approved participation → external registration; completion; certificate verification (accepted / rejected / flagged); **credit mapping rules**; credit recognition → **posting into the Domain C ledger through `CreditPostingPort`**; external record reconciliation; session archival; exception cases. **Course resources** (faculty Resources, student Library links). Screens: Admin tabs `flexible`, `recognition`; student **Library** (`/library`); faculty **Resources** (`/resources`). | D-034 … D-050 (+ resources) |

**Decision (resources):** the plan line's "resources" have no L3 scenario of their own; they are
given to **4C** (course resources + library links, a small `course_resource` table in 4C's
migration range), which balances the screen load (4A: 3 screens, 4B: 4, 4C: 2 + admin).
Library catalogue integration (Resources J, Batch 8) is out of scope: links only.

Domains **I** (Exams, Batch 6), **F** (Finance, Batch 5), **G** (Campus, Batch 8) and **E** (People,
Batch 7) are not built yet. Domain D reads their facts only through the ports of §6.3 (stub
adapters) and publishes events they will consume later (`internal_assessment.finalized`,
`attendance.threshold_breached` for Exams). **No Exams or Finance port is needed in Batch 4**: no
Domain D decision depends on an Exams or Finance fact (flexible-learning fees are out of scope).

## 2. Where the code lives: module and file ownership

**Decision:** every Batch 4 aggregate lives in the existing crate `modules/academic`
(`sutra-academic`, domain D) and the PostgreSQL schema `academic`. No new crates
(`apps/api/tests/architecture.rs` fixes exactly ten domain crates). `sutra-academic` depends only on
`sutra-kernel`, `sutra-contracts`, `sutra-web`, `sutra-persistence` and **never** contains
`FROM/JOIN/INTO/UPDATE <other schema>.` (the architecture test fails): Domain B data comes through
`InstitutionReferencePort`, Domain C data through `StudentDirectory` / `RegistrationReader` /
`CreditPostingPort` (§6). Cross-slice reads inside Domain D go through the **offering read port**
(`state.offerings`) for the shared tables, and may query sibling slice tables of the `academic`
schema **read-only** when a port would be pointless (same crate); you never write another slice's
tables.

### 2.1 Ownership map (create/edit freely inside your own paths)

| Path | Owner |
|------|-------|
| `modules/academic/src/offering/**`, `modules/academic/src/faculty/**` | **4A** |
| `modules/academic/src/{timetable,attendance,monitoring,assessment}/**` | **4B** |
| `modules/academic/src/{flexible,recognition,resources}/**` | **4C** |
| `crates/contracts/src/academic/events/offering.rs` | **4A** |
| `crates/contracts/src/academic/events/delivery.rs` | **4B** |
| `crates/contracts/src/academic/events/flexible.rs` | **4C** |
| `modules/platform/src/inbox/rules/offering_4a.rs` / `delivery_4b.rs` / `flexible_4c.rs` | 4A / 4B / 4C |
| `migrations/2026102500000[1-9]_academic_*.sql` | **4A** |
| `migrations/2026102500001[0-9]_academic_*.sql`, `migrations/2026102500002[0-9]_academic_*.sql` | **4B** |
| `migrations/2026102500003[0-9]_academic_*.sql` | **4C** |
| `apps/api/tests/db_b4a.rs` / `db_b4b.rs` / `db_b4c.rs` | 4A / 4B / 4C |
| `apps/web/app/features/academic/offering/**`, `routes/student/courses.tsx`, `routes/faculty/classes.tsx`, `routes/faculty/my-students.tsx` | **4A** |
| `apps/web/app/features/academic/delivery/**`, `routes/shared/timetable.tsx`, `routes/student/assignments.tsx`, `routes/faculty/attendance.tsx` | **4B** |
| `apps/web/app/features/academic/flexible/**`, `routes/student/library.tsx`, `routes/faculty/resources.tsx` | **4C** |
| `apps/web/e2e/academic.e2e.mjs` region `// ==== 4X begin` … `// ==== 4X end` | each slice its own region |
| README region `<!-- batch4:4X:begin … end -->`; `docs/batches.md` status cell `<!-- batch4:4X:status -->` | each slice its own region |

### 2.2 Shared files: foundation-owned, do not edit

`modules/academic/src/lib.rs` (router list already mounts every slice), `modules/academic/src/shared/**`,
`modules/academic/src/directory.rs`, `crates/contracts/src/academic/{mod,ids,status,ports}.rs`,
`crates/contracts/src/academic/events/mod.rs`, `crates/contracts/src/{campus,people}.rs`,
`crates/contracts/src/student/ports.rs` (Batch 4 block), `modules/student/src/academic_ports.rs`,
`modules/campus/src/spaces.rs`, `modules/people/src/staff.rs`,
`crates/kernel/src/auth.rs` (all Batch 4 permissions already added),
`modules/platform/src/inbox/rules/mod.rs`, `apps/api/src/{app,openapi}.rs` (tags + `academic_deps`
already added), `apps/api/tests/architecture.rs`, `apps/web/app/routes.ts`,
`apps/web/app/lib/roles.ts`, `apps/web/app/features/academic/{index.ts,Placeholder.tsx}`,
`apps/web/app/routes/admin/academic-delivery.tsx`, `apps/web/e2e/lib/harness.mjs`,
`apps/api/tests/support/{mod,b4}.rs`, `apps/api/tests/db_b4_foundation.rs`,
`migrations/20261025000000_academic_batch4_foundation.sql`.
Domain B and Domain C code (`modules/institution/**`, `modules/student/**`) is **off limits** to all
three slices (Batch 3 is merged; a needed change there is a contract change for the integrator).

If you cannot avoid touching one of these, keep the edit **append-only** at the end of the
relevant list/block, add a `// 4X:` (or `<!-- 4X: -->` / `-- 4X:`) comment, and list it in your
PR description. Examples: a new id at the end of your slice's block in `ids.rs`; a new variant in
`status.rs` (contract change — say so); a new method on a port (contract change — the foundation
adapter must implement it, so coordinate in the PR).

### 2.3 Generated files

`openapi/openapi.json` and `packages/api-client/src/schema.d.ts` are **generated**. Each slice
regenerates them in its own branch (`cargo run -q -p sutra-api -- openapi > openapi/openapi.json
&& pnpm gen:api`) so its drift checks pass. On merge, **never hand-merge them**: take either side,
then regenerate after the merge. `Cargo.lock` / `pnpm-lock.yaml`: no new dependencies are expected
(the foundation already gave `sutra-academic` sqlx/uuid/chrono/serde_json/tracing/persistence). If
you add one, say so in the PR. `CHANGELOG.md`: slices don't edit it.

## 3. Database

### 3.1 Migrations

| Range | Owner |
|-------|-------|
| `20261025000000_academic_batch4_foundation.sql` | foundation (applied): `academic.course_offering`, `academic.offering_section`, `academic.faculty_assignment`, `academic.section_enrolment`, `academic.status_history`, `academic.forbid_history_change()`, no-delete + terminal-offering triggers |
| `20261025000001` … `20261025000009` | 4A |
| `20261025000010` … `20261025000029` | 4B (twenty numbers: timetable, sessions/attendance, syllabus, monitoring, assessment) |
| `20261025000030` … `20261025000039` | 4C |

**Deviation from Batch 3:** 4B gets two decades because it owns five aggregate families. Reserved
names are **not** pre-created as empty files (sqlx checksums applied migrations). Name files
`<version>_academic_<what>.sql`. Never edit a migration after it has been applied anywhere; add the
next number. New tables pick up the runtime-role grants automatically (default privileges of
`20261014000003_runtime_db_roles.sql` cover schema `academic`).

Table conventions: `id uuid PRIMARY KEY DEFAULT uuidv7()`, `tenant_id uuid NOT NULL`,
`institution_id uuid NOT NULL` (plain uuid — **no FK into `institution`**), `offering_id uuid
REFERENCES academic.course_offering(id)` / `section_id uuid REFERENCES academic.offering_section(id)`
where the row belongs to an offering/section (FKs to the **foundation** tables are allowed for
every slice), `student_id uuid` / `student_context_id uuid` **plain** (Domain C, no FK),
`status text NOT NULL CHECK (status IN (...))` with the wire names of §4.2, `version bigint NOT NULL
DEFAULT 1`, `created_at/updated_at timestamptz`, a `process_instance_id text` on every aggregate
with an approval step. Business-local dates are `date` in the institution's timezone
(`shared::time::business_today`); instants are `timestamptz`; weekly slot times are `time` +
`day_of_week smallint` (1 = Monday). Append-only histories (attendance audit, assessment
corrections, recognition postings) get a no-update/no-delete trigger reusing
`academic.forbid_history_change()`.

**No foreign keys across slices.** A column referring to another slice's aggregate (e.g. 4B
`teaching_session.timetable_slot_id` is 4B's own, but 4C `credit_recognition.flexible_enrolment_id`
is 4C's own; 4B `internal_assessment_result` → 4A's section uses the foundation FK) is a plain
`uuid` when it points into another slice's table; the writer validates existence by reading it.

### 3.2 The shared delivery aggregates (foundation tables)

`academic.course_offering` — one curriculum component offered in one academic term
(L2-D01): `id, tenant_id, institution_id, campus_id, org_unit_id, academic_year_id,
academic_term_id, curriculum_version_id, curriculum_component_id, offering_code (unique per
institution+term among live offerings, case-insensitive), title, course_code, category, credits
(snapshot of the component), delivery_mode (in_person|online|hybrid), capacity, status
(CourseOfferingStatus), status_reason, status_changed_at, process_instance_id, merged_into_id,
split_from_id, version, …`. Archived/merged/split rows reject every UPDATE (trigger).

`academic.offering_section` — `id, …, offering_id → offering, section_code (unique per offering
among live sections), campus_id, capacity, status (SectionStatus), merged_into_id, version, …`.

`academic.faculty_assignment` — `id, …, offering_id, section_id → section, faculty_subject_ref
(login subject: the key of faculty-scoped permissions), faculty_name, staff_id (People, Batch 7),
role (FacultyRole), weekly_hours, effective_from/to, status (FacultyAssignmentStatus),
process_instance_id, version, …`. Partial unique: one live (`draft…active`) assignment per
(section, subject, role).

`academic.section_enrolment` — the roster: `id, …, offering_id, section_id, student_id,
student_context_id (Domain C, plain), student_number + display_name + subject_ref (snapshot for
rosters and notifications — no join into `student`), registration_id + registration_line_id (the
3B line that justified the seat), origin (registration|override|waitlist|transfer), status
(SectionEnrolmentStatus), transferred_from_id, enrolled_at, ended_at, version, …`. Partial unique
`section_enrolment_one_seat`: at most one `enrolled` seat per (offering, student).

`academic.status_history` — append-only: every status change of the four tables with
`aggregate_type, aggregate_id, from/to, reason, cause_event_type, process_instance_id, actor,
correlation_id`. No physical DELETE of any of the four tables.

**Write rules for the shared tables**

| Change | Who | How |
|--------|-----|-----|
| INSERT offering / section / faculty assignment / section enrolment; their attributes (capacity, codes, campus, role, hours, snapshot refresh) | **4A** | own repo code; take the row lock (`shared::offering::lock_*`) first; bump `version` |
| `status` of any of the four | **4A** only | **only** `shared::lifecycle::change_{offering,section,assignment,enrolment}_status` |
| Read offerings / sections / rosters / faculty | 4B, 4C | `state.offerings` (`OfferingDirectory`); `shared::offering::load_*` / `seated_roster` inside a transaction (read only, never `lock_*`) |

4B/4C **react** to offering/section/seat changes by reading current state when they act (e.g.
attendance capture uses the seated roster at capture time); there is no in-process event bus.

### 3.3 Slice table names (schema `academic`)

| Slice | Tables (prefixes a slice may extend) |
|-------|--------------------------------------|
| 4A | `offering_waitlist_entry`, `offering_quota`, `offering_enrolment_window`, `offering_revision`, `offering_template`, `offering_change_simulation`, `offering_reorganisation`, `enrolment_restriction`, `faculty_assignment_policy`, `faculty_assignment_change` (prefixes `offering_*`, `enrolment_*`, `faculty_assignment_*`) |
| 4B | `scheduling_requirement`, `timetable`, `timetable_slot`, `timetable_revision`, `teaching_session`, `attendance_record`, `attendance_correction`, `attendance_policy`, `attendance_evaluation`, `syllabus_coverage`, `syllabus_topic`, `monitoring_case`, `risk_assessment`, `monitoring_exception`, `monitoring_intervention`, `assessment_blueprint`, `assessment_window`, `assessment_submission`, `assessment_moderation`, `internal_assessment_result`, `assessment_correction` (prefixes `scheduling_*`, `timetable_*`, `teaching_session*`, `attendance_*`, `syllabus_*`, `monitoring_*`, `risk_*`, `assessment_*`, `internal_assessment_*`) |
| 4C | `learning_provider`, `learning_opportunity`, `flexible_learning_policy`, `flexible_enrolment`, `flexible_seat_allocation`, `external_registration`, `learning_completion`, `certificate_verification`, `credit_mapping_rule`, `credit_recognition`, `reconciliation_run`, `flexible_exception_case`, `course_resource` (prefixes `learning_*`, `flexible_*`, `external_*`, `certificate_*`, `credit_mapping_*`, `credit_recognition*`, `reconciliation_*`, `course_resource*`) |

## 4. Kernel and contract types (already in the code)

### 4.1 Ids: `sutra_contracts::academic::ids`

`InstitutionId`/`TenantId` re-exported, plus:
- **foundation:** `CourseOfferingId`, `OfferingSectionId`, `FacultyAssignmentId`, `SectionEnrolmentId`
- **4A:** `OfferingWaitlistEntryId`, `OfferingQuotaId`, `OfferingEnrolmentWindowId`, `OfferingRevisionId`, `OfferingTemplateId`, `OfferingChangeSimulationId`, `OfferingReorganisationId`, `FacultyAssignmentPolicyId`, `FacultyAssignmentChangeId`
- **4B:** `SchedulingRequirementId`, `TimetableId`, `TimetableSlotId`, `TimetableRevisionId`, `TeachingSessionId`, `AttendanceRecordId`, `AttendanceCorrectionId`, `AttendancePolicyId`, `SyllabusCoverageId`, `MonitoringCaseId`, `RiskAssessmentId`, `MonitoringInterventionId`, `AssessmentBlueprintId`, `AssessmentWindowId`, `AssessmentSubmissionId`, `InternalAssessmentResultId`, `AssessmentCorrectionId`
- **4C:** `LearningProviderId`, `LearningOpportunityId`, `FlexibleEnrolmentId`, `CertificateVerificationId`, `CreditRecognitionId`, `CreditMappingRuleId`, `FlexibleLearningPolicyId`, `ReconciliationRunId`, `FlexibleExceptionCaseId`, `CourseResourceId`

All are `typed_id!` newtypes: UUIDv7, serde-transparent, OpenAPI `string/uuid`.

### 4.2 Lifecycle states: `sutra_contracts::academic::status`

Enums with `ALL`, `as_str()`, `parse()`, serde/OpenAPI `snake_case`; also the DB `CHECK` values.
The four **shared** machines carry `can_transition` in contracts; only 4A moves them. Every other
machine's transitions are slice-owned, written as a pure, unit-tested `can_transition(from, to)` in
the slice's `domain.rs`. Illegal transition → `409 conflict`.

**Shared machines (foundation, `can_transition` in contracts)**

`CourseOfferingStatus` (L2-D01 §71): `draft, pending_approval, approved, published,
enrollment_open, enrollment_closed, active, suspended, frozen, completed, archived, cancelled,
merged, split`. Helpers: `accepts_enrolment()` (`enrollment_open | active`), `is_deliverable()`
(`enrollment_open | enrollment_closed | active` — 4B/4C delivery gate), `is_terminal()`
(`archived | merged | split`).

| From | Legal to |
|------|----------|
| `draft` | `pending_approval`, `cancelled` |
| `pending_approval` | `approved`, `draft` (returned) |
| `approved` | `published`, `cancelled` |
| `published` | `enrollment_open`, `cancelled` |
| `enrollment_open` | `enrollment_closed` |
| `enrollment_closed` | `enrollment_open` (window reopened), `active` |
| `active` | `suspended`, `frozen`, `completed`, `merged`, `split`, `approved` (governed rollback, L2-D01 §71) |
| `suspended` | `active` (reactivation), `cancelled` |
| `frozen` | `active` (governed unfreeze) |
| `completed`, `cancelled` | `archived` |
| `archived`, `merged`, `split` | — |

`SectionStatus`: `planned → active | cancelled`; `active → completed | cancelled | merged`.

`FacultyAssignmentStatus` (L2-D02 §6.2): `draft → submitted | withdrawn`; `submitted → under_review
| approved | rejected | withdrawn`; `under_review → approved | rejected`; `approved → active |
withdrawn`; `active → completed | revoked`; `completed | revoked | rejected | withdrawn → archived`.
Only `active` grants faculty-scoped rights. `FacultyRole`: `primary_instructor, co_instructor,
tutor, lab_instructor, teaching_assistant, course_coordinator`.

`SectionEnrolmentStatus`: `enrolled → dropped | withdrawn | transferred | completed` (the new seat of
a transfer is a new `enrolled` row with `transferred_from_id`). `is_seated()` = `enrolled`.

**Other enums (transitions slice-owned)**

| Enum | Values | Used by |
|------|--------|---------|
| `PolicyVersionStatus` | `draft, in_review, effective, superseded, retired` | 4A assignment policy; 4B attendance policy; 4C credit mapping rules, flexible-learning policy |
| `ChangeRequestStatus` | `requested, validated, approved, applied, rejected, withdrawn` (L2-D02 operational change) | 4A assignment change, offering revision/simulation; 4B timetable revision, attendance correction, assessment correction |
| `WaitlistStatus` | `waiting, offered, accepted, expired, removed` | 4A |
| `EnrolmentWindowStatus` | `scheduled, open, suspended, closed, closed_permanently` | 4A |
| `TimetableStatus` | `planning, draft, validated, approved, published, active, frozen, revised, retired, archived` (L2-D03 §12.4) | 4B |
| `TeachingSessionStatus` | `scheduled, started, in_progress, paused, completed, cancelled, abandoned, locked, archived` (L2-D04 §21.1) | 4B |
| `AttendanceMark` | `present, absent, late, excused` | 4B |
| `AttendanceCaptureStatus` | `not_started, capturing, submitted, finalised` | 4B |
| `TopicCoverageStatus` / `SyllabusCoverageStatus` | `planned, started, completed, deferred, cancelled` / `draft, submitted, published` | 4B |
| `MonitoringCaseStatus` | `open, under_assessment, intervention, under_review, certified, closed, reopened` | 4B |
| `RiskLevel` / `InterventionStatus` | `low, moderate, high, critical` / `planned, triggered, in_progress, completed, cancelled` | 4B |
| `AssessmentBlueprintStatus` | `draft, published, superseded, archived` (L2-D07 §5) | 4B |
| `AssessmentKind` | `assignment, quiz, test, lab, project, presentation, viva` (coursework = `assignment`) | 4B |
| `AssessmentWindowStatus` | `scheduled, open, extended, closed, cancelled` | 4B |
| `AssessmentSubmissionStatus` | `pending, submitted, late, marked, moderated, finalized, absent` | 4B |
| `InternalAssessmentStatus` | `in_progress, moderated, finalized, corrected, archived` | 4B |
| `LearningProviderStatus` | `proposed, under_review, active, suspended, offboarded` | 4C |
| `LearningOpportunityStatus` / `LearningMode` | `draft, pending_approval, active, suspended, archived` / `open_elective, mooc, swayam, nptel, internship, external_course` | 4C |
| `FlexibleEnrolmentStatus` | `requested, eligible, allocated, waitlisted, registered, in_progress, completed, withdrawn, rejected` | 4C |
| `CertificateVerificationStatus` | `submitted, under_verification, accepted, rejected, flagged` | 4C |
| `CreditRecognitionStatus` | `requested, under_review, approved, posted, rejected` | 4C |
| `CourseResourceStatus` / `CourseResourceKind` | `draft, published, withdrawn` / `document, link, video, reading, library_item` | 4C |

### 4.3 Events: `sutra_contracts::academic::events`

`type = sutraerp.academic.<aggregate>.<verb>`, `dataschema = urn:sutraerp:schema:academic.<aggregate>.<verb>:1`,
`source = /sutraerp/academic`, envelope `subject` = aggregate id, envelope scope =
`ExecutionScope::institution(tenant, <offering's institution>)`. Emit via `shared::publish::event`
inside the owner-local tx; audit via `shared::publish::audit` with action
`academic.<aggregate>.<command>`. V1 payloads are frozen: additive optional fields only.

Shared payloads (`events/mod.rs`): **`StatusChangedV1`** `{aggregateId, institutionId, offeringId?,
sectionId?, studentId?, fromStatus?, toStatus, version, reason?, processInstanceId?,
studentSubjectRef?, facultySubjectRef?}` and **`ModifiedV1`** `{aggregateId, institutionId,
offeringId?, sectionId?, version, changedFields[]}`. Subject refs let pure inbox rules notify the
student / faculty member without a lookup. There is no automatic foundation event (unlike
`student.status_changed`): the slice emits its specific event next to every `shared::lifecycle`
call.

| Slice / file | Constant | `type` suffix | Meaning / payload |
|--------------|----------|---------------|-------------------|
| 4A `offering.rs` | `COURSE_OFFERING_SUBMITTED` | `course_offering.submitted` | Draft → pending_approval through the WorkflowPort (StatusChangedV1, D-001). |
| 4A | `COURSE_OFFERING_APPROVED` | `course_offering.approved` | (StatusChangedV1). |
| 4A | `COURSE_OFFERING_PUBLISHED` | `course_offering.published` | [`CourseOfferingV1`]. |
| 4A | `COURSE_OFFERING_ENROLLMENT_OPENED` / `_CLOSED` | `course_offering.enrollment_opened` / `enrollment_closed` | (StatusChangedV1, D-014). |
| 4A | `COURSE_OFFERING_ACTIVATED` | `course_offering.activated` | Teaching starts ([`CourseOfferingV1`]). |
| 4A | `COURSE_OFFERING_SUSPENDED` / `_REACTIVATED` | `course_offering.suspended` / `reactivated` | (StatusChangedV1, D-002/003). |
| 4A | `COURSE_OFFERING_CANCELLED` | `course_offering.cancelled` | (StatusChangedV1, D-004). |
| 4A | `COURSE_OFFERING_FROZEN` / `_UNFROZEN` | `course_offering.frozen` / `unfrozen` | (StatusChangedV1, D-013). |
| 4A | `COURSE_OFFERING_COMPLETED` / `_ARCHIVED` | `course_offering.completed` / `archived` | (StatusChangedV1, D-010). |
| 4A | `COURSE_OFFERING_REVISED` | `course_offering.revised` | (ModifiedV1, D-009). |
| 4A | `COURSE_OFFERING_CAPACITY_CHANGED` | `course_offering.capacity_changed` | (ModifiedV1, D-005/015). |
| 4A | `COURSE_OFFERING_REORGANISED` | `course_offering.reorganised` | Merge/split (ModifiedV1, D-011). |
| 4A | `SECTION_ENROLMENT_ENROLLED` / `_DROPPED` / `_WITHDRAWN` | `section_enrolment.enrolled` / `dropped` / `withdrawn` | [`SectionEnrolmentV1`]. |
| 4A | `OFFERING_WAITLIST_SEAT_OFFERED` | `offering_waitlist.seat_offered` | (StatusChangedV1, D-007). |
| 4A | `FACULTY_ASSIGNMENT_SUBMITTED` / `_APPROVED` | `faculty_assignment.submitted` / `approved` | (StatusChangedV1, D-017). |
| 4A | `FACULTY_ASSIGNMENT_ACTIVATED` / `_ENDED` | `faculty_assignment.activated` / `ended` | [`FacultyAssignmentV1`]. |
| 4A | `FACULTY_ASSIGNMENT_CHANGED` | `faculty_assignment.changed` | (ModifiedV1, D-018). |
| 4A | `FACULTY_ASSIGNMENT_POLICY_EFFECTIVE` | `faculty_assignment_policy.effective` | (StatusChangedV1, D-019). |
| 4B `delivery.rs` | `TIMETABLE_PUBLISHED` / `_ACTIVATED` / `_REVISED` / `_RETIRED` | `timetable.published` / `activated` / `revised` / `retired` | (StatusChangedV1, D-020…022). |
| 4B | `TEACHING_SESSION_STARTED` / `_COMPLETED` / `_CANCELLED` | `teaching_session.started` / `completed` / `cancelled` | (StatusChangedV1, D-023). |
| 4B | `ATTENDANCE_FINALISED` | `attendance.finalised` | (StatusChangedV1, D-024). |
| 4B | `ATTENDANCE_CORRECTED` | `attendance.corrected` | (ModifiedV1, D-025). |
| 4B | `ATTENDANCE_THRESHOLD_BREACHED` | `attendance.threshold_breached` | [`AttendanceThresholdV1`] — consumed by Exams (Batch 6) for eligibility. |
| 4B | `SYLLABUS_COVERAGE_PUBLISHED` | `syllabus_coverage.published` | (StatusChangedV1, D-026). |
| 4B | `MONITORING_CASE_OPENED` / `MONITORING_RISK_ASSESSED` / `MONITORING_CASE_CLOSED` / `MONITORING_CASE_REOPENED` | `monitoring_case.opened` / `risk_assessed` / `closed` / `reopened` | (StatusChangedV1, D-027…033). |
| 4B | `MONITORING_INTERVENTION_TRIGGERED` | `monitoring_intervention.triggered` | (StatusChangedV1, D-030). |
| 4B | `ASSESSMENT_BLUEPRINT_PUBLISHED` | `assessment_blueprint.published` | (StatusChangedV1, D-051). |
| 4B | `ASSESSMENT_WINDOW_OPENED` / `_CLOSED` | `assessment_window.opened` / `closed` | (StatusChangedV1, D-052). |
| 4B | `ASSESSMENT_SUBMISSION_RECEIVED` | `assessment_submission.received` | (StatusChangedV1, D-053). |
| 4B | `ASSESSMENT_OUTCOME_MODERATED` | `assessment_outcome.moderated` | (StatusChangedV1, D-054). |
| 4B | `INTERNAL_ASSESSMENT_FINALIZED` / `_CORRECTED` | `internal_assessment.finalized` / `corrected` | [`InternalAssessmentFinalizedV1`] — consumed by Exams (D-055/057). |
| 4B | `INTERNAL_ASSESSMENT_ARCHIVED` | `internal_assessment.archived` | (StatusChangedV1, D-056). |
| 4C `flexible.rs` | `LEARNING_PROVIDER_ACTIVATED` / `_SUSPENDED` | `learning_provider.activated` / `suspended` | (StatusChangedV1, D-044). |
| 4C | `LEARNING_OPPORTUNITY_ACTIVATED` / `_SUSPENDED` / `_ARCHIVED` | `learning_opportunity.activated` / `suspended` / `archived` | (StatusChangedV1, D-040…042). |
| 4C | `FLEXIBLE_ENROLMENT_REQUESTED` / `_ALLOCATED` / `_WAITLISTED` / `_REGISTERED` / `_COMPLETED` | `flexible_enrolment.requested` / `allocated` / `waitlisted` / `registered` / `completed` | (StatusChangedV1, D-034…038, 048). |
| 4C | `CERTIFICATE_VERIFICATION_DECIDED` | `certificate_verification.decided` | Accepted/rejected/flagged (StatusChangedV1, D-043). |
| 4C | `CREDIT_RECOGNITION_APPROVED` | `credit_recognition.approved` | (StatusChangedV1, D-039). |
| 4C | `CREDIT_RECOGNITION_POSTED` | `credit_recognition.posted` | [`CreditRecognitionPostedV1`] (ledger entry id from `CreditPostingPort`). |
| 4C | `CREDIT_MAPPING_RULE_EFFECTIVE` | `credit_mapping_rule.effective` | (StatusChangedV1, D-046). |
| 4C | `FLEXIBLE_LEARNING_POLICY_EFFECTIVE` | `flexible_learning_policy.effective` | (StatusChangedV1, D-045). |
| 4C | `COURSE_RESOURCE_PUBLISHED` | `course_resource.published` | (StatusChangedV1). |

Each slice file exports `EVENT_TYPES`; `events::all_event_types()` lists every one and a unit test
keeps them unique and well formed (4 dot-separated parts, lowercase). A slice may add an event to
its own file (constant + `EVENT_TYPES` entry + payload) and must add it to the table above in its
PR. Consumed Domain C events: Domain D does **not** subscribe in Batch 4 (no inbox projection
into `academic`); it reads Student/Registration state through ports when it acts.

### 4.4 Permissions and role mapping (`sutra_kernel::auth::Permission`)

Staff reads of every Domain D resource require `academic.read` (Administrator **and Faculty**).
Students act only on their **own** delivery through `academic.self`: the handler resolves the
student from the session subject (`shared::me::me` / `require_seated`, 404 when the account has no
linked student or no seat) and never takes a student id from the client. **Faculty-scoped
permissions:** faculty hold `academic.teach`, valid only on sections where they hold an **active**
faculty assignment — every teaching handler starts with
`shared::faculty::require_teaching(&state, &auth, section_id, Permission::<SliceManage>)`, which
lets administrators in through the slice's manage permission and faculty only through an active
assignment (403 otherwise). Every manage/decide permission is **Administrator only** (unit test
`batch4_domain_d_grants`).

| Variant | Name | Step-up | Roles | Slice: used for |
|---------|------|---------|-------|-----------------|
| `AcademicRead` | `academic.read` | no | Admin, Faculty | all slices: staff reads |
| `AcademicSelfService` | `academic.self` | no | Student | 4A own courses/enrolment; 4B own timetable, attendance, assignments + submissions; 4C flexible-learning requests, certificate upload, library links |
| `AcademicTeach` | `academic.teach` | no | Faculty | 4A My classes / My students; 4B sessions, attendance capture, syllabus coverage, coursework creation + marking on own sections; 4C resources on own sections |
| `OfferingManage` | `offering.manage` | no | Admin | 4A drafts, sections, capacity, quotas, windows, templates, simulation, revisions |
| `OfferingDecide` | `offering.decide` | no | Admin | 4A approval decision, publish, enrolment open/close, activate, suspend/reactivate, freeze |
| `OfferingLifecycle` | `offering.lifecycle` | **yes** | Admin | 4A cancel, archive, merge/split, governed unfreeze, rollback |
| `OfferingEnrolmentManage` | `offering.enrolment.manage` | no | Admin | 4A seat / withdraw / transfer, waitlist resolution, eligibility overrides |
| `FacultyAssignmentManage` | `faculty_assignment.manage` | no | Admin | 4A assignment drafts, operational change, governance configuration |
| `FacultyAssignmentDecide` | `faculty_assignment.decide` | no | Admin | 4A approve / reject / activate / revoke |
| `TimetableManage` | `timetable.manage` | no | Admin | 4B requirements, drafts, slots, validation, revision requests |
| `TimetablePublish` | `timetable.publish` | no | Admin | 4B approve, publish, activate, freeze, revise, retire |
| `AttendanceManage` | `attendance.manage` | no | Admin | 4B policy, capture/finalisation on any section, threshold evaluation, syllabus publication |
| `AttendanceCorrect` | `attendance.correct` | **yes** | Admin | 4B governed correction of finalised attendance |
| `MonitoringManage` | `monitoring.manage` | no | Admin | 4B monitoring cases end to end |
| `AssessmentManage` | `assessment.manage` | no | Admin | 4B blueprints, windows/assignments, moderation on any section |
| `AssessmentFinalize` | `assessment.finalize` | **yes** | Admin | 4B finalise, archive, correct internal assessment |
| `FlexibleLearningManage` | `flexible_learning.manage` | no | Admin | 4C providers, opportunities, policy, mapping rules, allocation, registration, completion, verification, reconciliation, exceptions |
| `CreditRecognitionDecide` | `credit_recognition.decide` | **yes** | Admin | 4C approve recognition + post credits |
| `CourseResourceManage` | `course_resource.manage` | no | Admin | 4C resources on any offering/section |

Step-up is enforced only when the deployment enables it; DB tests run in dev mode without it.

## 5. HTTP API

### 5.1 Conventions (as Domains B and C)

- Every handler starts with `auth.require(Permission::…)?` (or `shared::faculty::require_teaching`
  / `shared::me::me`). Tenant from the session; build `shared::scope::institution_scope(ctx,
  offering.institution_id)` for events/audit.
- Before a write: `shared::guard::require_institution_writable`; 4A uses
  `require_offering_changeable`; 4B/4C delivery commands use `require_deliverable(offering.status,
  section.status)`.
- Create: `POST`, required `Idempotency-Key`, `201` + `ETag`. Update or state command: `If-Match`
  required (`shared::http::if_match`: 428 missing, 412 stale); response = updated resource + `ETag`.
- State commands are `POST …/{id}/<verb>` with an optional `{ "reason": "…" }`; literal paths per
  command, no generic `/{command}` route.
- Lists: `GET` collection under the parent, `?status=` filter, `{ "items": [...] }`; `?limit=` ≤ 200
  (default 50) and `nextCursor` where lists can grow (rosters, attendance, submissions).
- Errors: illegal transition → `409`; validation → `422`; foreign tenant / unknown id → `404`;
  Domain B/C reference not found → `422` on the field; faculty not assigned → `403`.
- JSON members camelCase, statuses snake_case (§4.2), business dates `YYYY-MM-DD`, weekly times
  `HH:MM`.
- **Every `#[utoipa::path]` sets an explicit `operation_id`** with the slice prefix below and the
  slice's `tag` (enforced by `architecture.rs::academic_operations_follow_the_slice_prefix_and_tag_rules`).

### 5.2 Path prefixes, operation_id prefixes, tags (all under `/api/v1/academic`)

**Path parameter rule (Batch 2/3 lesson):** the first parameter after any collection segment is
always `{id}`; deeper parameters get names (`/offerings/{id}/sections/{sectionId}`,
`/sessions/{id}/attendance/{recordId}`). Self-service lives under `/me/…`, faculty "my" views under
`/my/…`.

| Slice | Paths | operation_id prefixes | tag |
|-------|-------|-----------------------|-----|
| 4A | `/institutions/{id}/offerings`, `/offerings/{id}[/…]` (`/submit`, `/decision`, `/publish`, `/open-enrolment`, `/close-enrolment`, `/activate`, `/suspend`, `/reactivate`, `/cancel`, `/freeze`, `/unfreeze`, `/complete`, `/archive`, `/revisions`, `/merge`, `/split`, `/capacity`, `/quotas`, `/windows`, `/waitlist`), `/offerings/{id}/sections`, `/sections/{id}[/…]` (incl. `/sections/{id}/enrolments`), `/enrolments/{id}[/drop,/withdraw,/transfer]`, `/institutions/{id}/offering-templates`, `/offering-templates/{id}[/generate]`, `/offerings/{id}/simulations`, `/me/courses`, `/me/enrolments` | `offering_`, `offering_section_`, `section_enrolment_`, `offering_waitlist_`, `offering_window_`, `offering_quota_`, `offering_template_`, `offering_simulation_`, `offering_self_` | `offerings` |
| 4A | `/sections/{id}/faculty-assignments`, `/faculty-assignments/{id}[/…]` (`/submit`, `/decision`, `/activate`, `/changes`, `/revoke`, `/complete`), `/institutions/{id}/faculty-assignment-policies`, `/faculty-assignment-policies/{id}[/…]`, `/my/classes`, `/my/students` | `faculty_assignment_`, `faculty_policy_`, `faculty_self_` | `faculty-assignments` |
| 4B | `/institutions/{id}/scheduling-requirements`, `/scheduling-requirements/{id}[/…]`, `/institutions/{id}/timetables`, `/timetables/{id}[/…]` (slots `/timetables/{id}/slots[/{slotId}]`, `/validate`, `/submit`, `/decision`, `/publish`, `/activate`, `/freeze`, `/revisions`, `/retire`), `/me/timetable`, `/my/timetable` | `timetable_`, `scheduling_requirement_`, `timetable_self_` | `timetable` |
| 4B | `/sections/{id}/sessions`, `/sessions/{id}[/…]` (`/start`, `/pause`, `/resume`, `/complete`, `/cancel`, `/abandon`, `/lock`, attendance `/sessions/{id}/attendance[/{recordId}]`, `/finalise-attendance`), `/attendance-corrections/{id}[/…]`, `/institutions/{id}/attendance-policies`, `/attendance-policies/{id}[/…]`, `/sections/{id}/attendance-summary`, `/sections/{id}/syllabus-coverage`, `/syllabus-coverage/{id}[/…]`, `/me/attendance`, `/my/attendance` | `teaching_session_`, `attendance_`, `syllabus_coverage_` | `attendance` |
| 4B | `/institutions/{id}/monitoring-cases`, `/monitoring-cases/{id}[/…]` (risk assessments, exceptions, interventions nested) | `monitoring_` | `monitoring` |
| 4B | `/sections/{id}/assessment-blueprints`, `/assessment-blueprints/{id}[/…]`, `/sections/{id}/assessment-windows`, `/assessment-windows/{id}[/…]` (submissions `/assessment-windows/{id}/submissions[/{submissionId}]`, `/moderation`), `/sections/{id}/internal-assessment[/finalize]`, `/internal-assessment-results/{id}[/corrections]`, `/me/assignments[/{id}/submission]` | `assessment_`, `internal_assessment_`, `assessment_self_` | `assessment` |
| 4C | `/institutions/{id}/learning-providers`, `/learning-providers/{id}[/…]`, `/institutions/{id}/learning-opportunities`, `/learning-opportunities/{id}[/…]` (demand, allocation, waitlist nested), `/flexible-enrolments/{id}[/…]`, `/institutions/{id}/flexible-learning-policies`, `/flexible-learning-policies/{id}[/…]`, `/institutions/{id}/flexible-exceptions`, `/flexible-exceptions/{id}[/…]`, `/me/flexible-learning[/…]` | `learning_provider_`, `learning_opportunity_`, `flexible_enrolment_`, `flexible_policy_`, `flexible_exception_`, `flexible_self_` | `flexible-learning` |
| 4C | `/flexible-enrolments/{id}/certificates`, `/certificate-verifications/{id}[/…]`, `/institutions/{id}/credit-mapping-rules`, `/credit-mapping-rules/{id}[/…]`, `/credit-recognitions/{id}[/…]` (`/decision`, `/post`), `/institutions/{id}/reconciliation-runs`, `/reconciliation-runs/{id}` | `certificate_verification_`, `credit_mapping_`, `credit_recognition_`, `reconciliation_` | `credit-recognition` |
| 4C | `/sections/{id}/resources`, `/offerings/{id}/resources`, `/resources/{id}[/publish,/withdraw]`, `/me/library` | `course_resource_`, `resource_self_` | `resources` |

Prefix hygiene: 4A never starts an operation_id with `assessment_`/`attendance_`/`timetable_`; 4B
never with `offering_`/`credit_`; 4C uses `credit_mapping_`/`credit_recognition_` (never `credit_`
alone, which would collide with 3B's `credit_ledger_`/`credit_transfer_`). The tags are already in
`apps/api/src/openapi.rs`; the router mount points are already registered in
`sutra_academic::router_with_state` (`offering`, `faculty` → 4A; `timetable`, `attendance`,
`monitoring`, `assessment` → 4B; `flexible`, `recognition`, `resources` → 4C). Each slice only
fills `pub fn router(state: AcademicState) -> OpenApiRouter` in its own `mod.rs`.

### 5.3 Minimum response fields other slices / screens rely on

- **Offering** (4A): `id, institutionId, campusId, academicTermId, curriculumComponentId, offeringCode, title, courseCode, credits, capacity, seatsTaken, status, version` + `sections[{id, sectionCode, capacity, seatsTaken, status, faculty[{assignmentId, facultySubjectRef, facultyName, role, status}]}]`.
- **My classes** (4A, `GET /my/classes`): `items[{sectionId, offeringId, offeringCode, title, sectionCode, academicTermId, role, seatedCount}]`. **My students**: `items[{studentId, studentNumber, displayName, sectionId, offeringCode, sectionCode}]` (links to the read-only Student 360).
- **Student courses** (4A, `GET /me/courses`): `items[{offeringId, sectionId, offeringCode, title, credits, sectionCode, faculty[], status}]`.
- **Timetable** (4B): slots `{id, sectionId, offeringCode, title, sectionCode, dayOfWeek, startsAt, endsAt, roomLabel, teachingSpaceId, facultyNames[]}`; `GET /me/timetable` and `/my/timetable` return `{items: [...slots], weekOf}`.
- **Session + attendance** (4B): `{id, sectionId, sessionDate, startsAt, status, attendanceStatus, records[{studentId, studentNumber, displayName, mark}]}`; attendance summary `{studentId, attended, total, percent, belowThreshold}`.
- **Assignment** (4B, window of kind `assignment`): `{id, sectionId, title, kind, opensAt, dueAt, maxMarks, status}`; student view adds `submission {status, submittedAt, marks?}`.
- **Resource** (4C): `{id, offeringId?, sectionId?, title, kind, url, status}`; `GET /me/library` = published resources of the student's sections.
- **CreditRecognition** (4C): `{id, studentId, flexibleEnrolmentId, credits, curriculumComponentId?, status, ledgerEntryId?, version}`.
- Collections: `{ "items": [ … ] }`.

## 6. Cross-slice runtime rules and ports

### 6.1 Foundation helpers (`modules/academic/src/shared/`)

- `state::AcademicState` — `pool, workflow, approvals, references, students, registrations,
  credits, offerings, spaces, staff`. Built by `AcademicDeps::into_state()` (auto-approve, Postgres
  offering directory) or by hand in tests (port doubles, `AwaitDecision`).
- `scope` — tenant from the session; the offering's institution is the event/audit scope.
- `guard` — institution writable, `require_offering_changeable`, `require_deliverable`.
- `offering` — row types `OfferingRow`, `SectionRow`, `FacultyAssignmentRow`,
  `SectionEnrolmentRow`, `load_*` / `lock_*` (4A), `seated_roster`.
- `lifecycle` — `change_offering_status`, `change_section_status`, `change_assignment_status`,
  `change_enrolment_status` (4A only; history row in `academic.status_history`).
- `faculty` — `require_teaching` (§4.4), `my_sections`.
- `me` — `me`, `my_sections`, `require_seated` (student self-service).
- `publish` — `event` + `audit` in the owner-local tx. `approval` — §9. `http` — `with_etag`,
  `if_match`, `actor`. `time::business_today`.

### 6.2 Typed ports (all defined and wired by the foundation)

| Port | Defined in | Implementation wired now | Consumer | Notes |
|------|-----------|--------------------------|----------|-------|
| **Offering read port** `OfferingDirectory` (`offering`, `offerings_in_term`, `section`, `sections_of_offering`, `roster`, `faculty_of_section`, `sections_taught_by`, `sections_of_student`, `teaches`) | `sutra_contracts::academic::ports` | `sutra_academic::PgOfferingDirectory` over the foundation tables (**real**, not a stub) | **4B, 4C** (`state.offerings`), `shared::faculty`, `shared::me`; Exams later | 4B/4C never read 4A's own tables and never wait for 4A: the shape is foundation-owned and seeded in tests by `support::b4`. |
| **Registration read** `RegistrationReader::term_registration` → `TermRegistration { status, lines[] }` + `is_effective()` / `covers(component)` | `sutra_contracts::student::ports` | `sutra_student::PgRegistrationReader` over 3B's tables (**real**) | **4A** enrolment gating | Seat a student in an offering only when `covers(offering.curriculum_component_id)` (registration `approved | registered | finalised`, line `selected | registered`); otherwise 409 unless `offering.enrolment.manage` records an `override` with a reason. No cross-schema SQL. |
| **Credit posting** `CreditPostingPort::post_credits(CreditPostingRequest)` → `CreditPostingReceipt { ledgerEntryId, alreadyPosted }` | `sutra_contracts::student::ports` | `sutra_student::PgCreditPosting` (**real**): `recognised` entry in `student.credit_ledger_entry` + `credit_ledger.updated` + audit in a Domain C tx | **4C** credit recognition | Idempotent on `(source_type, source_id)`: use `CREDIT_SOURCE_FLEXIBLE_LEARNING` and the recognition id. Call it **after** the decision, outside your own tx, then record the receipt (`posted`, `ledgerEntryId`) in your tx; a retry is safe. Needs a live context in the institution (409 otherwise). |
| **Domain B reference** `InstitutionReferencePort` (`institution` + timezone, `campus`, `org_unit`, `programme`, `curriculum_version`, `curriculum_components`, `academic_year`, `academic_term`, `key_dates`) | `sutra_contracts::institution::reference` | `sutra_institution::PgInstitutionReferences` (**real**) | all slices | Offerings need a **published** curriculum version and a term of a **published** calendar version of the same institution; campus/org unit `active`. Store ids as plain uuids; snapshot component title/credits on the offering. |
| **Students** `StudentDirectory` (`student`, `live_context`, `by_subject`) | `sutra_contracts::student::ports` | `sutra_student::PgStudentDirectory` (**real**) | 4A seating (snapshot name/number), `shared::me` | A seat needs a **live** `active` context in the offering's institution. |
| **Rooms** `TeachingSpacePort` (`space`, `spaces_of_campus`) | `sutra_contracts::campus` | `sutra_campus::NoTeachingSpaces` (**stub**: none) | 4B timetable | Store `room_label text` + nullable `teaching_space_id`; validate the id only when the port returns spaces (Batch 8). |
| **Staff** `StaffDirectory::staff_by_subject` | `sutra_contracts::people` | `sutra_people::SubjectOnlyStaff` (**stub**: any non-empty subject is an active staff member, no name) | 4A faculty assignment | Faculty are identified by login subject (dev faculty: `dev|meera.shah@sutra.dev`); 4A stores `faculty_name` typed by the admin until Batch 7. |
| Exams / Finance | — | — | — | **Not needed in Batch 4** (§1): Domain D publishes `attendance.threshold_breached` and `internal_assessment.finalized` for Exams; no Finance fact gates any D decision. |

Tests can swap any port by building an `AcademicState` by hand (e.g. a `RegistrationReader` double
returning `None`) and serving `sutra_academic::router_with_state(state)`.

## 7. Cross-slice dependencies and readiness

- **The offering, section, faculty assignment and roster are foundation-owned**, so 4B/4C never
  wait for 4A: they read them through `state.offerings` and seed tests with
  `apps/api/tests/support/b4.rs` (`seed_offering`, `seed_section`, `seed_faculty`, `seed_student`,
  `seed_enrolment`, `seed_registration`). 4A's real flow creates the same shape, so behaviour after
  merge is identical. 4A must keep the foundation columns' meaning (§3.2).
- **4B delivery preconditions** (read-only checks): `require_deliverable(offering, section)`; faculty
  through `require_teaching`; attendance rosters = seated roster at capture time (a later drop does
  not rewrite finalised attendance). Timetable slots reference sections of offerings in the same
  term (plain FK to `offering_section` is allowed).
- **4C → Domain C:** credits only through `CreditPostingPort`; never write `student.*`. A flexible
  enrolment may map to an offering of 4A (open elective) by plain `offering_id` FK (foundation
  table) — reading it through `state.offerings`.
- **4C resources** attach to `offering_id` / `section_id` (foundation FKs); students see published
  resources of sections they are seated in (`shared::me::my_sections`).
- **4A ↔ 3B:** enrolment gating reads registrations through `RegistrationReader` only. Batch 4 does
  not change 3B (registration lines stay keyed by `curriculumComponentId`; "Batch 4 will add
  offering ids additively" in batch3 §6.2 is **deferred**: the section enrolment carries
  `registration_line_id` instead, so no Domain C change is needed).
- No activation-readiness checklist in Domain D. Readiness-like gates are plain 409 preconditions.

## 8. Inbox consumer hooks (`modules/platform/src/inbox/rules/`)

`rules::plan` dispatches by event type to `offering_4a.rs` (4A), `delivery_4b.rs` (4B),
`flexible_4c.rs` (4C); each declares `EVENT_TYPES` (only types from its own event file — test
`slices_never_claim_the_same_type`) and a pure `plan`. The foundation files start with **empty**
`EVENT_TYPES`; add a type only **with** its rule. Category: reuse `institution` (no new
notification category in Batch 4, so no platform migration). Notify students with
`Recipient::Subject(studentSubjectRef)`, faculty with `Recipient::Subject(facultySubjectRef)` when
the payload carries it; otherwise `Recipient::Role(…)`. To-dos with `todos::SystemTodo`.

| Slice | Types | Projection |
|-------|-------|------------|
| 4A | `course_offering.submitted` (AwaitDecision) | to-do (admins) "Decide offering …" |
| 4A | `course_offering.published`, `course_offering.enrollment_opened` | notification to `Role::Student` (institution-scoped) |
| 4A | `section_enrolment.enrolled` / `dropped` / `withdrawn` | notification to the student |
| 4A | `faculty_assignment.activated` / `ended` | notification to the faculty member |
| 4B | `timetable.published` / `revised` | notification to `Role::Student` + `Role::Faculty` |
| 4B | `attendance.threshold_breached` | notification to the student + admins |
| 4B | `assessment_window.opened` | notification to `Role::Student` |
| 4B | `monitoring_intervention.triggered` | to-do (admins) |
| 4C | `flexible_enrolment.allocated` / `waitlisted`, `certificate_verification.decided`, `credit_recognition.posted` | notification to the student |
| 4C | `course_resource.published` | notification to `Role::Student` |

## 9. WorkflowPort usage for approval steps

`shared::approval` wraps `WorkflowPort` exactly like Domains B and C: the composition root passes
the platform `NoopWorkflowAdapter` and `ApprovalMode::AutoApprove`.

1. Validate the command and current state (read only).
2. `request_approval(&state, &scope, process::<KEY>, business_key(aggregate, id, version), vars)`
   **before** opening the tx (deterministic key → retry-safe).
3. One owner-local tx: re-check the version; set the pending state (`pending_approval`,
   `submitted`, `under_review`, `requested`, …); store `process_instance_id`; emit the
   *submitted/requested* event. If the outcome is `ApprovalOutcome::Approved`, **also** apply the
   decision in the same tx (final state + decision event with `processInstanceId`, audit actor
   `system:auto-approval`).
4. Always expose the explicit decision endpoint (`POST …/{id}/decision {decision: approve|reject,
   reason}` with the slice's decide permission) so `AwaitDecision` (Flowable, Batch 9) needs no API
   change. Each slice has at least one DB test building an `AcademicState` with
   `ApprovalMode::AwaitDecision` (via `sutra_academic::router_with_state` and
   `sutra_api::app::academic_deps`).

Process keys (`shared::approval::process`): 4A `COURSE_OFFERING_APPROVAL` (**the reference approval
of Batch 4**: draft → pending_approval → approved | draft), `COURSE_OFFERING_CANCELLATION`,
`COURSE_OFFERING_REVISION`, `COURSE_OFFERING_REORGANISATION`, `COURSE_OFFERING_UNFREEZE`,
`OFFERING_CHANGE_IMPACT`, `ENROLMENT_ELIGIBILITY_OVERRIDE`, `FACULTY_ASSIGNMENT_APPROVAL`,
`FACULTY_ASSIGNMENT_CHANGE`, `FACULTY_ASSIGNMENT_POLICY`; 4B `TIMETABLE_APPROVAL`,
`TIMETABLE_REVISION`, `ATTENDANCE_CORRECTION`, `ATTENDANCE_POLICY`, `MONITORING_DECISION`,
`MONITORING_CLOSURE`, `ASSESSMENT_MODERATION`, `ASSESSMENT_FINALIZATION`, `ASSESSMENT_CORRECTION`;
4C `LEARNING_PROVIDER_ONBOARDING`, `LEARNING_OPPORTUNITY_APPROVAL`, `FLEXIBLE_ENROLMENT_APPROVAL`,
`CERTIFICATE_VERIFICATION`, `CREDIT_RECOGNITION`, `CREDIT_MAPPING_RULE`, `FLEXIBLE_LEARNING_POLICY`,
`FLEXIBLE_EXCEPTION`. Use only the keys of your slice; a new key is a contract change.

## 10. Web

| Route | File (owner) | Content |
|-------|--------------|---------|
| `/academic-delivery/:tab?` | `routes/admin/academic-delivery.tsx` (foundation frame, pre-wired) | Admin → Academic delivery tabs (`ACADEMIC_TABS` in `features/academic/index.ts`): `offerings`, `faculty` (4A), `timetable`, `attendance`, `monitoring`, `assessment` (4B), `flexible`, `recognition` (4C). Uses `features/institution/AdminScreen` (admin gate + institution picker). Linked from the admin nav ("Delivery"). |
| `/courses` | `routes/student/courses.tsx` (4A) | student Courses |
| `/classes` | `routes/faculty/classes.tsx` (4A) | faculty My classes (sections taught; per-section tabs may embed 4B/4C components) |
| `/my-students` | `routes/faculty/my-students.tsx` (4A) | faculty My students (rosters of taught sections → read-only Student 360). **Fixes the Batch 3 bug**: the faculty nav "Students" entry pointed at the admin-only `/students`. |
| `/timetable` | `routes/shared/timetable.tsx` (4B) | student and faculty Timetable (role-switched: `StudentTimetableScreen` / `FacultyTimetableScreen`) |
| `/assignments` | `routes/student/assignments.tsx` (4B) | student Assignments |
| `/attendance` | `routes/faculty/attendance.tsx` (4B) | faculty Attendance |
| `/library` | `routes/student/library.tsx` (4C) | student Library links |
| `/resources` | `routes/faculty/resources.tsx` (4C) | faculty Resources |

Each slice fills the placeholder components exported from its folder — keep the export names:
`features/academic/offering/index.tsx`: `OfferingsAdminTab, FacultyAssignmentsAdminTab,
StudentCoursesScreen, FacultyClassesScreen, FacultyStudentsScreen`;
`features/academic/delivery/index.tsx`: `TimetableAdminTab, AttendanceAdminTab, MonitoringAdminTab,
AssessmentAdminTab, StudentTimetableScreen, FacultyTimetableScreen, StudentAssignmentsScreen,
FacultyAttendanceScreen`; `features/academic/flexible/index.tsx`: `FlexibleLearningAdminTab,
CreditRecognitionAdminTab, StudentLibraryScreen, FacultyResourcesScreen`. Put API hooks in your
own folder (`features/academic/<slice>/api.ts`), use `@sutra/api-client` after regeneration, and a
mock mode like `features/institution/mock.ts`. Until a sibling slice is merged, type its calls
locally in your folder. Never edit another slice's folder. Role gating is in the route frames
(`RoleGate`); the frames are thin and owned as listed above (only the owning slice edits them).

**E2E:** `apps/web/e2e/academic.e2e.mjs` (`pnpm e2e:academic`) reuses the Batch 3 harness
(`e2e/lib/harness.mjs`: processes, API seeding of an active institution, psql seeding, browser
helpers, screenshots). The foundation seeds an institution, two students, one active offering with
section A taught by Meera and both students seated, and walks every frame. Each slice adds its
steps **only inside its region** (`// ==== 4A begin` … `// ==== 4A end`, same for 4B/4C) and
writes screenshots to `$E2E_SHOTS` (default `/workspace/work/out/b4-shots`) with names prefixed
`4a-`/`4b-`/`4c-`. Run: `/workspace/work/tools/run-e2e.sh <log> /workspace/work/out/b4-shots
academic <worktree>`.

## 11. Worker environment: separate DB, target dir and worktree per slice

| Slice | Worktree | Branch | Database | `CARGO_TARGET_DIR` |
|-------|----------|--------|----------|--------------------|
| 4A | `/workspace/work/erp-4a` | `batch4/4a` | `postgres://sutra:sutra@localhost:5432/sutra_4a` | `/workspace/work/targets/4a` |
| 4B | `/workspace/work/erp-4b` | `batch4/4b` | `postgres://sutra:sutra@localhost:5432/sutra_4b` | `/workspace/work/targets/4b` |
| 4C | `/workspace/work/erp-4c` | `batch4/4c` | `postgres://sutra:sutra@localhost:5432/sutra_4c` | reuses `4a` or `4b`, whichever slice finished first |

**At most two slice builds at once** (2 CPUs, ~7.9 GB RAM). See
`/workspace/work/tools/b4-worker-common.md` for the full worker rules (setup script, build flags,
low-memory linker, checkpoint pushes every 30 minutes, status lines in `b4-status.txt`).

```sh
cd /workspace/work/erp-4b/sutraerp
export CARGO_TARGET_DIR=/workspace/work/targets/4b DATABASE_URL=postgres://sutra:sutra@localhost:5432/sutra_4b
cargo test -j 1 -p sutra-api --features db-tests --test db_b4b
```

Quality gates per slice before pushing: `cargo fmt --all -- --check`, `cargo clippy --workspace
--all-targets --all-features -- -D warnings`, `cargo test --workspace --locked --no-fail-fast`,
DB tests on a freshly recreated DB (`-p sutra-persistence`, `-p sutra-platform`, `-p sutra-api
--features db-tests --no-fail-fast -- --test-threads 4`), OpenAPI regenerated + `pnpm gen:api` with
no drift after commit, `pnpm -r typecheck && pnpm lint && pnpm -r test && pnpm -r build`, and the
slice's e2e region green. Commit `[skip ci]` as `Varad1614 <Varad1614@users.noreply.github.com>`.

## 12. Merge order and known gaps

- Merge order: **4A → 4B → 4C** (4B/4C are independent of each other; both rely on 4A only through
  the foundation tables). Regenerate OpenAPI/TS after each merge (§2.3). After 4A merges, the
  integrator adds a cross-slice test (offering → enrolment gated by registration → timetable →
  session + attendance → assignment → flexible credit recognition posted to the ledger) in
  `apps/api/tests/db_b4_integration.rs`.
- Session pinning to one institution is not enforced for Domain D (tenant-wide staff admin, as
  Batches 2/3).
- Faculty identity is the login subject until People (Batch 7); rooms are free text until Campus
  (Batch 8); Exams consumes D events from Batch 6.
- `ApprovalMode::AwaitDecision` is wired but unused in production until the Flowable adapter lands
  (Batch 9).
- Institution status is checked through the reference port without a cross-schema row lock.
