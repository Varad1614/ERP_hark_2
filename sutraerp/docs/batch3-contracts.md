# Batch 3 — shared contracts (Student lifecycle, Domain C)

**Status:** frozen at `batch3/foundation`. This file is the single source of truth for the three
parallel slices **3A**, **3B** and **3C**. If a slice needs to change anything in §2–§10, it says so
in its PR description ("contract change: …") instead of silently editing a shared file; the
integrator applies it on top of the merge.

Architecture inputs: L2-C00…C08 (+ amendments: C01 expiry 2026-08-22, C01 finance-clearance
evidence 2026-10-05, C04 event names 2026-08-22, C05 MEME 2026-08-22, C08 alumni boundary
2026-08-26), L1 Person-scope amendment 2026-08-26; L3/L4 C-001…C-075; ADR-L5-0003 (module
boundaries), -0004 (scope), -0006/-0007 (owner-local tx + outbox), -0011/-0035 (event
versioning/envelope), -0023/-0042 (WorkflowPort / Flowable), -0034 (API profile), -0036 (schemas,
business timezone); L6 screens Admin Students, admission flow, Student 360, Student Documents.

Plan line (Batch 3): *Admission application → review → offer → enrolment → activation and
registration. Progression, transfers, exit; guardians; documents and certificates; Student 360.
First BPMN approvals through the WorkflowPort.*

---

## 1. Slices and scenario coverage (one wave, three parallel slices)

| Slice | Scope | L3/L4 scenarios |
|-------|-------|-----------------|
| **3A** | Admissions: application → review → offer (Admission Initiated) → acceptance → documents submitted → verification → **approval decision through the WorkflowPort** (first BPMN-style approval; no-op adapter + auto-approve, `AwaitDecision` covered by tests) → financial clearance (from `FinanceClearancePort` evidence) → confirmation → student created → completed; cancellation, withdrawal, revocation, offer expiry, readmission. Student record: activation + enrolment → active, enrolment update, academic assignment change, suspension / restoration, intra-institute reassignment, inter-institute transfer with context re-establishment, profile, communication preferences, identity correction, duplicate resolution, student documents, document/certificate requests, archival + permanent record closure, Student 360, self-service (`me`). Screens: Admin → Students (directory, admissions tabs + frame), admission flow, Student 360 (frame + overview/enrolment/documents tabs), student-facing Documents. | C-001 … C-019 |
| **3B** | Registration windows (open/close), student registration, add/drop, cancellation, backlog/repeat lines, finalisation + publication, holds (placement/release; finance-sourced holds read through `FinanceClearancePort`); progression decisions, credit ledger, conditional promotion conditions, probation, detention/repeat, credit transfer/recognition, programme migration, academic break + rejoin, qualification eligibility, ABC sync (`AbcRegistryPort`, no-op), decision revision after re-evaluation/appeal, progression policy lifecycle. Screens: Admin → Students registration + progression tabs, Student 360 registration + progression tabs, student-facing Registration. | C-020 … C-038, C-075 |
| **3C** | Qualification completion (eligibility → audit → clearance → approval → award/conferment → record finalisation → exit), NEP MEME intermediate exit, withdrawal / transfer-out / discontinuation / termination exits, exit clearance, administrative closure, deceased record closure, completion deferral, conferred qualification correction/revocation; guardian relationship lifecycle (establish/verify, modify, suspend, reinstate, terminate), custody, consent, communication authorisation, emergency contacts, financial responsibility, pickup authorisation; scholarship definitions, eligibility policy, determination/recommendation, conditional resolution/expiry, reassessment, renewal, expiry/archival, external sync (`ExternalScholarshipPort`, no-op), bulk recalculation, hand-off to financial aid (**event only**); alumni transition, correction, archival. Screens: Admin → Students exits/guardians/scholarships tabs, Student 360 guardians/scholarships/completion tabs. | C-039 … C-074 |

Domains **F** (Finance, Batch 5) and **I** (Exams, Batch 6) are not built yet: Domain C reads their
facts only through the contract ports of §6.3 (stub adapters), and publishes events they will
consume later. Never import `sutra-finance` / `sutra-exams` (the architecture test forbids it).

## 2. Where the code lives: module and file ownership

**Decision:** every Batch 3 aggregate lives in the existing crate `modules/student`
(`sutra-student`, domain C) and the PostgreSQL schema `student`. No new crates
(`apps/api/tests/architecture.rs` fixes exactly ten domain crates). `sutra-student` depends only on
`sutra-kernel`, `sutra-contracts`, `sutra-web`, `sutra-persistence` and **never** contains
`FROM/JOIN/INTO/UPDATE institution.` (or any other foreign schema): Domain B data comes through
`InstitutionReferencePort` (§6.2). Cross-slice reads inside Domain C may query sibling tables of
the `student` schema, but only **read**. You never write another slice's tables; the shared
`student` / `student_context` tables have the write rules of §3.2.

### 2.1 Ownership map (create/edit freely inside your own paths)

| Path | Owner |
|------|-------|
| `modules/student/src/admission/**`, `modules/student/src/record/**` | **3A** |
| `modules/student/src/registration/**`, `modules/student/src/progression/**` | **3B** |
| `modules/student/src/completion/**`, `guardian/**`, `scholarship/**`, `alumni/**` | **3C** |
| `crates/contracts/src/student/events/admission.rs`, `events/record.rs` | **3A** |
| `crates/contracts/src/student/events/registration.rs`, `events/progression.rs` | **3B** |
| `crates/contracts/src/student/events/{completion,guardian,scholarship,alumni}.rs` | **3C** |
| `modules/platform/src/inbox/rules/student_3a.rs` | **3A** |
| `modules/platform/src/inbox/rules/registration_3b.rs` | **3B** |
| `modules/platform/src/inbox/rules/completion_3c.rs` | **3C** |
| `migrations/2026102000000[1-9]_student_*.sql` | **3A** |
| `migrations/2026102000001[0-9]_student_*.sql` | **3B** |
| `migrations/2026102000002[0-9]_student_*.sql` | **3C** |
| `apps/api/tests/db_b3a.rs` / `db_b3b.rs` / `db_b3c.rs` | 3A / 3B / 3C |
| `apps/web/app/features/student/record/**`, `routes/admin/students.tsx`, `routes/admin/student-360.tsx`, `routes/student/documents.tsx` | **3A** (the two admin route files already wire every slice's tab component — 3A keeps those imports/cases) |
| `apps/web/app/features/student/registration/**`, `routes/student/registration.tsx` | **3B** |
| `apps/web/app/features/student/completion/**` | **3C** |
| README region `<!-- batch3:3X:begin … end -->`; `docs/batches.md` status cell `<!-- batch3:3X:status -->` | each slice its own region |

### 2.2 Shared files: foundation-owned, do not edit

`modules/student/src/lib.rs` (router list already mounts every slice), `modules/student/src/shared/**`,
`modules/student/src/{directory,adapters}.rs`, `crates/contracts/src/student/{mod,ids,status,ports}.rs`,
`crates/contracts/src/student/events/mod.rs`, `crates/contracts/src/institution/reference.rs`,
`crates/contracts/src/{finance,exams}.rs`, `modules/institution/src/reference.rs`,
`modules/finance/src/clearance.rs`, `modules/exams/src/results.rs`,
`crates/kernel/src/auth.rs` (all Batch 3 permissions already added),
`modules/platform/src/inbox/rules/mod.rs`, `apps/api/src/{app,openapi}.rs` (tags + `student_deps`
already added), `apps/web/app/routes.ts`, `apps/web/app/lib/roles.ts`,
`apps/web/app/features/student/{index.ts,Placeholder.tsx}`, `apps/api/tests/support/{mod,b3}.rs`,
`apps/api/tests/db_b3_foundation.rs`, `migrations/20261020000000_student_batch3_foundation.sql`.
Domain B code (`modules/institution/**`) is **off limits** to all three slices.

If you cannot avoid touching one of these, keep the edit **append-only** at the end of the
relevant list/block, add a `// 3X:` (or `<!-- 3X: -->` / `-- 3X:`) comment, and list it in your
PR description. Examples: a new id at the end of your slice's block in `ids.rs`; a new variant in
`status.rs` (contract change — say so); a new method on a port (contract change — the foundation
adapter must implement it, so coordinate in the PR).

### 2.3 Generated files

`openapi/openapi.json` and `packages/api-client/src/schema.d.ts` are **generated**. Each slice
regenerates them in its own branch (`cargo run -q -p sutra-api -- openapi > openapi/openapi.json
&& pnpm gen:api`) so its drift checks pass. On merge, **never hand-merge them**: take either side,
then regenerate after the merge. `Cargo.lock` / `pnpm-lock.yaml`: no new dependencies are expected
(the foundation already gave `sutra-student` sqlx/uuid/chrono/serde_json/tracing/persistence). If
you add one, say so in the PR. `CHANGELOG.md`: slices don't edit it.

## 3. Database

### 3.1 Migrations

| Range | Owner |
|-------|-------|
| `20261020000000_student_batch3_foundation.sql` | foundation (applied): `student.student`, `student.student_context`, `student.student_status_history`, immutability triggers |
| `20261020000001` … `20261020000009` | 3A |
| `20261020000010` … `20261020000019` | 3B |
| `20261020000020` … `20261020000029` | 3C |

Reserved names are **not** pre-created as empty files (sqlx checksums applied migrations). Name
files `<version>_student_<what>.sql`. Never edit a migration after it has been applied anywhere;
add the next number. New tables pick up the runtime-role grants automatically (default privileges
of `20261014000003_runtime_db_roles.sql` cover schema `student`).

Table conventions: `id uuid PRIMARY KEY DEFAULT uuidv7()`, `tenant_id uuid NOT NULL`,
`institution_id uuid NOT NULL` (plain uuid — **no FK into `institution`**, ADR-L5-0036 §3.2),
`student_id uuid REFERENCES student.student(id)` and/or `student_context_id uuid REFERENCES
student.student_context(id)` where the row belongs to a student (FKs to the **foundation** tables
are allowed for every slice), `status text NOT NULL CHECK (status IN (...))` with the wire names
of §4.2, `version bigint NOT NULL DEFAULT 1`, `created_at/updated_at timestamptz`, a
`process_instance_id text` on every aggregate with an approval step. Business-local dates are
`date` in the institution's timezone (`shared::time::business_today`); instants are `timestamptz`.
Append-only histories (ledger, status history, decision revisions) get a no-update/no-delete
trigger like `student.forbid_history_change()` (reuse that function).

**No foreign keys across slices.** A column referring to another slice's aggregate (e.g. 3C
`qualification_completion.eligibility_id` → 3B) is a plain `uuid`; the writer validates existence
by reading the other table (allowed within the schema).

### 3.2 The shared Student aggregate (foundation tables)

`student.student` — tenant-scoped persona (L2-C02 §6; L1 amendment: one Person per tenant):
`id, tenant_id, person_id (A02 later), student_number (UNIQUE per tenant, immutable), given_name,
middle_name, family_name, preferred_name, date_of_birth, gender, primary_email, primary_phone,
subject_ref (login subject for self-service; unique per tenant among non-archived), origin_admission_id,
merged_into_id, status (StudentStatus), status_changed_at, version, created_at, updated_at`.

`student.student_context` — per-Institute relationship (L2-C02 §11.10): `id, tenant_id,
student_id → student, institution_id, campus_id, programme_id, curriculum_version_id, org_unit_id,
admission_term_id, admission_year_id, batch_code, section_code, current_term_no, enrolment_number
(unique per institution), roll_number, enrolment_type (regular | lateral_entry | readmission |
transfer_in | re_entry), enrolled_on, admission_id, predecessor_context_id → context, status
(StudentContextStatus), status_reason, status_changed_at, closed_on, version, …`.
Partial unique index `student_context_one_live`: at most one live context
(`pending_activation | active | suspended`) per (tenant, student, institution) — SC-INV-001/004.
Trigger: terminal contexts reject every UPDATE (SC-INV-003); no physical DELETE of students or
contexts.

`student.student_status_history` — append-only: every persona/context status change with
`from/to, reason, cause_event_type, process_instance_id, actor, correlation_id`.

**Write rules for the shared tables**

| Change | Who | How |
|--------|-----|-----|
| INSERT persona / context | **3A** (student creation, readmission, inter-institute transfer) | own repo code |
| Persona attributes (names, contact, DOB, subject link, merge pointer) | **3A** (profile, identity correction, duplicates) | own repo code; bump `version` |
| Context placement (campus, org unit, batch, section, roll/enrolment numbers, enrolment type/date) | **3A** (enrolment update, assignment change, intra transfer) | own repo code; bump `version` |
| `current_term_no`, `programme_id`, `curriculum_version_id` of a **live** context | **3B** (progression, programme migration) | `shared::student::set_context_progress` only |
| `status` of persona or context | anyone, per §4.2 | **only** `shared::lifecycle::{change_context_status, change_student_status}` |

Every write takes the row lock (`shared::student::lock_student` / `lock_context`) inside the
owner-local transaction first.

### 3.3 Slice table names (schema `student`)

| Slice | Tables (prefixes a slice may extend) |
|-------|--------------------------------------|
| 3A | `admission_application`, `admission`, `admission_document`, `admission_status_history`, `student_document`, `document_request`, `identity_correction`, `duplicate_case`, `student_transfer`, `student_suspension`, `communication_preference`, `student_number_sequence` (prefixes `admission_*`, `student_*`, `document_*`, `identity_*`, `duplicate_*`) |
| 3B | `registration_window`, `registration`, `registration_line`, `registration_hold`, `progression_policy`, `progression_decision`, `progression_condition`, `credit_ledger_entry`, `probation_case`, `detention_case`, `credit_transfer`, `programme_migration`, `academic_break`, `qualification_eligibility`, `abc_sync`, `academic_standing` (prefixes `registration_*`, `progression_*`, `credit_*`, `probation_*`, `detention_*`, `programme_*`, `academic_*`, `qualification_eligibility*`, `abc_*`) |
| 3C | `qualification_completion`, `conferred_qualification`, `completion_deferral`, `qualification_correction`, `student_exit`, `exit_clearance`, `guardian`, `guardian_relationship`, `guardian_consent`, `guardian_communication`, `emergency_contact`, `financial_responsibility`, `pickup_authorisation`, `scholarship`, `scholarship_policy`, `scholarship_eligibility`, `scholarship_recalculation`, `scholarship_external_sync`, `alumni_transition` (prefixes `qualification_completion*`, `conferred_*`, `completion_*`, `exit_*`, `student_exit*`, `guardian_*`, `emergency_*`, `financial_responsibility*`, `pickup_*`, `scholarship_*`, `alumni_*`) |

## 4. Kernel and contract types (already in the code)

### 4.1 Ids: `sutra_contracts::student::ids`

`InstitutionId`/`TenantId` re-exported, plus:
- **foundation:** `StudentId`, `StudentContextId`
- **3A:** `AdmissionApplicationId`, `AdmissionId`, `AdmissionDocumentId`, `StudentDocumentId`, `DocumentRequestId`, `IdentityCorrectionId`, `DuplicateCaseId`, `StudentTransferId`, `StudentSuspensionId`
- **3B:** `RegistrationWindowId`, `RegistrationId`, `RegistrationLineId`, `RegistrationHoldId`, `ProgressionPolicyId`, `ProgressionDecisionId`, `CreditLedgerEntryId`, `ProgressionConditionId`, `ProbationCaseId`, `DetentionCaseId`, `CreditTransferId`, `ProgrammeMigrationId`, `AcademicBreakId`, `QualificationEligibilityId`, `AbcSyncId`, `ProgressionRevisionId`
- **3C:** `QualificationCompletionId`, `StudentExitId`, `ExitClearanceId`, `CompletionDeferralId`, `QualificationCorrectionId`, `GuardianId`, `GuardianRelationshipId`, `GuardianConsentId`, `EmergencyContactId`, `PickupAuthorisationId`, `ScholarshipDefinitionId`, `ScholarshipPolicyId`, `ScholarshipEligibilityId`, `ScholarshipRecalculationId`, `AlumniTransitionId`

All are `typed_id!` newtypes: UUIDv7, serde-transparent, OpenAPI `string/uuid`.

### 4.2 Lifecycle states: `sutra_contracts::student::status`

Enums with `ALL`, `as_str()`, `parse()`, serde/OpenAPI `snake_case`; also the DB `CHECK` values.
The two **shared** machines carry `can_transition` in contracts (several slices move them); every
other machine's transitions are slice-owned, written as a pure, unit-tested `can_transition(from,
to)` in the slice's `domain.rs`. Illegal transition → `409 conflict`.

**Shared machines (foundation, `can_transition` in contracts)**

`StudentContextStatus` (L2-C02 §11.10.1): `pending_activation, active, suspended,
qualification_completed, withdrawn, transferred, discontinued, terminated`. Live =
`pending_activation | active | suspended`; the rest are terminal and immutable.

| From | Legal to |
|------|----------|
| `pending_activation` | `active` (3A enrolment), `withdrawn` (3A withdrawal after creation), `discontinued` (no-show / deceased, 3C), `terminated` (3A revocation after creation), `transferred` (3A inter-institute) |
| `active` | `suspended` (3A), `qualification_completed` (3C award/exit, also MEME), `withdrawn` / `transferred` / `discontinued` / `terminated` (3C exits; `transferred` also 3A inter-institute transfer) |
| `suspended` | `active` (3A restoration), `withdrawn` / `transferred` / `discontinued` / `terminated` (3C exits). **Not** `qualification_completed` (L2-C02 §11.6) |
| terminal | — |

`StudentStatus` (persona, L2-C02 §11.2 **condensed**): `created, activated, enrolled,
academically_assigned, active, suspended, qualification_completed, archived`. **Decision:** L2's
`Restored` and `Transferred` are transient ("Restored → Active: restoration complete",
"Transferred → Active: transfer completed"), so they are **events** (`student.restored`,
`student.transferred`) that land in `active`, not stored states.

| From | Legal to |
|------|----------|
| `created` | `activated` |
| `activated` | `enrolled` |
| `enrolled` | `academically_assigned` |
| `academically_assigned` | `active` |
| `active` | `suspended`, `qualification_completed` |
| `suspended` | `active` |
| any non-archived | `archived` (guard: no live context — checked by the helper) |
| `archived`, `qualification_completed` | `activated` (governed readmission / re-entry, 3A C-005) |

Persona ↔ context mapping (L2-C02 §11.10.3), applied by the slice that changes the context:
3A creation → persona `created`, context `pending_activation` → persona `activated`; 3A enrolment →
context `active`, persona `enrolled` → (assignment) `academically_assigned` → `active` (3A may
pass through these in one transaction); suspension/restoration mirror on the persona when it is
the persona's only live context; 3C `qualification_completed` context → persona
`qualification_completed`; any terminal context with no other live context → persona `archived`
only through 3A archival (C-019), 3C deceased closure (C-048) or 3C alumni completion (C-072).
Withdrawal/discontinuation/termination leave the persona as is until archival.

**Exit kind → terminal context state (`StudentExitKind`, 3C):** `completion`, `meme_exit` →
`qualification_completed`; `withdrawal` → `withdrawn`; `transfer` (transfer-out, external) →
`transferred`; `discontinuation`, `administrative_closure`, `deceased` → `discontinued`;
`termination` → `terminated`. 3A's inter-institute transfer inside the tenant (C-013) closes the
source context `transferred` and opens a new `pending_activation` context at the destination with
`predecessor_context_id` and `enrolment_type = transfer_in`; it does not create a 3C exit record.

**Other enums (transitions slice-owned)**

| Enum | Values | Used by |
|------|--------|---------|
| `CaseStatus` | `requested, under_review, approved, rejected, completed, withdrawn` | generic governed case: 3A identity correction, duplicate case, transfer; 3B credit transfer, programme migration, progression revision; 3C completion deferral, qualification correction |
| `PolicyVersionStatus` | `draft, in_review, effective, superseded, retired` | 3B progression policy; 3C scholarship eligibility policy (L2-C07's `active` = `effective`) |
| `AssignmentStatus` | `active, suspended, revoked, expired` | 3C emergency contact, pickup authorisation, financial responsibility, communication authorisation |
| `DocumentStatus` | `submitted, verified, rejected, waived, superseded` | 3A admission documents, student documents |
| `AdmissionApplicationStatus` | `draft, submitted, under_review, offered, rejected, withdrawn` | 3A |
| `AdmissionStatus` | `initiated, documents_submitted, verification_completed, approval_pending, approved, financially_cleared, confirmed, student_created, completed, cancelled, withdrawn, revoked, expired` | 3A |
| `DocumentRequestStatus` | `requested, in_progress, issued, rejected, cancelled` | 3A |
| `StudentTransferKind` | `intra_institute, inter_institute` | 3A |
| `RegistrationWindowStatus` | `scheduled, open, closed, cancelled` | 3B |
| `RegistrationStatus` | `draft, submitted, returned, approved, registered, finalised, cancelled, archived` | 3B |
| `RegistrationLineKind` / `RegistrationLineStatus` | `regular, backlog, repeat, audit, elective` / `selected, registered, dropped, withdrawn` | 3B |
| `RegistrationHoldStatus` / `HoldSource` | `active, released` / `academic, administrative, discipline, documents, finance` | 3B |
| `ProgressionDecisionStatus` | `draft, pending_approval, effective, superseded, rejected` | 3B |
| `ProgressionOutcome` | `progressed, conditionally_progressed, probation, detained, repeat, qualification_eligible` | 3B |
| `AcademicStanding` | `good_standing, conditional, probation, detained, academic_break` | 3B (projection) |
| `CreditEntryKind` | `earned, transferred_in, recognised, adjusted, revoked, expired` | 3B (append-only ledger) |
| `ProgressionConditionStatus` | `pending, fulfilled, failed, waived` | 3B |
| `ProbationStatus` | `active, cleared, escalated` | 3B |
| `AcademicBreakStatus` | `requested, approved, in_effect, rejoined, lapsed, rejected, withdrawn` | 3B |
| `AbcSyncStatus` | `pending, submitted, acknowledged, failed` | 3B |
| `QualificationCompletionStatus` | `eligibility_evaluation, degree_audit, institutional_clearance, approval_pending, approved, awarded, record_finalized, exit_completed, deferred, cancelled` | 3C |
| `QualificationOutcome` | `certificate, diploma, advanced_diploma, degree, degree_with_research` | 3B eligibility, 3C award |
| `ConferredQualificationStatus` | `conferred, corrected, revoked` | 3C |
| `StudentExitKind` / `StudentExitStatus` | see above / `initiated, clearance_pending, approval_pending, approved, completed, rejected, cancelled` | 3C |
| `ExitClearanceStatus` | `pending, cleared, waived, blocked` | 3C |
| `GuardianRelationshipStatus` | `proposed, pending_verification, verified, active, suspended, terminated, historical` (L2 `Modified` is an event) | 3C |
| `GuardianConsentStatus` | `granted, withdrawn, expired` | 3C |
| `ScholarshipDefinitionStatus` | `draft, published, active, suspended, expired, archived` | 3C |
| `ScholarshipEligibilityStatus` | `created, assessment_pending, under_evaluation, eligible, not_eligible, conditionally_eligible, recommendation_generated, active, reassessment, renewal, expired, archived` | 3C |
| `AlumniTransitionStatus` | `created, pending_verification, verified, pending_approval, approved, transitioning, activated, completed, archived, rejected` | 3C |

Reference transition sets for the slice-owned machines are the L2 matrices: Admission L2-C01
§6.9.1 + expiry amendment (`initiated → expired`; `approval_pending` sits between
`verification_completed` and `approved`; a rejected approval → `cancelled` with the decision
reason); Registration L2-C03 §6.9; Qualification completion L2-C05 §9 + MEME amendment §4;
Guardian L2-C06 §6; Scholarship L2-C07 §12; Alumni L2-C08 §12.4.

### 4.3 Events: `sutra_contracts::student::events`

`type = sutraerp.student.<aggregate>.<verb>`, `dataschema = urn:sutraerp:schema:student.<aggregate>.<verb>:1`,
`source = /sutraerp/student`, envelope `subject` = aggregate id, envelope scope =
`ExecutionScope::institution(tenant, <context's institution>)`. Emit via `shared::publish::event`
inside the owner-local tx; audit via `shared::publish::audit` with action
`student.<aggregate>.<command>`. V1 payloads are frozen: additive optional fields only.

Shared payloads (`events/mod.rs`): **`StatusChangedV1`** `{aggregateId, institutionId, studentId?,
fromStatus?, toStatus, version, reason?, processInstanceId?, studentSubjectRef?}`, **`ModifiedV1`**
`{aggregateId, institutionId, studentId?, version, changedFields[], studentSubjectRef?}`, and the
foundation event **`STUDENT_STATUS_CHANGED`** `student.status_changed`
(`StudentStatusChangedV1 {studentId, studentContextId?, institutionId, scope: student|context,
fromStatus?, toStatus, causeEventType, reason?, version}`) emitted **automatically** by
`shared::lifecycle` on every persona/context status change — downstream domains (D/F/I/…)
subscribe to that one type. `studentSubjectRef` lets pure inbox rules notify the student.

Canonical L2 names are kept as doc comments (AdmissionExpired, ConditionalPromotionGranted,
AcademicProbationAssigned, AcademicDetentionApplied — the C04 amendment synonyms must not be
published —, QualificationAwarded, StudentExitCompleted, AlumniRelationshipActivated, …).

| Slice / file | Constant | `type` suffix | Meaning / payload |
|--------------|----------|---------------|-------------------|
| foundation `mod.rs` | `STUDENT_STATUS_CHANGED` | `student.status_changed` | `StudentStatusChangedV1` (emitted by `shared::lifecycle`) |
| 3A `admission.rs` | `ADMISSION_APPLICATION_SUBMITTED` | `admission_application.submitted` | Draft → submitted (StatusChangedV1). |
| 3A `admission.rs` | `ADMISSION_APPLICATION_OFFERED` | `admission_application.offered` | Application offered; an Admission Record was initiated (StatusChangedV1). |
| 3A `admission.rs` | `ADMISSION_APPLICATION_REJECTED` | `admission_application.rejected` | (StatusChangedV1). |
| 3A `admission.rs` | `ADMISSION_APPLICATION_WITHDRAWN` | `admission_application.withdrawn` | (StatusChangedV1). |
| 3A `admission.rs` | `ADMISSION_INITIATED` | `admission.initiated` | AdmissionInitiated: Admission Record created with an offer ([`AdmissionInitiatedV1`]). Also readmission (L3-C-005). |
| 3A `admission.rs` | `ADMISSION_OFFER_ACCEPTED` | `admission.offer_accepted` | Applicant accepted the offer (StatusChangedV1, status unchanged). |
| 3A `admission.rs` | `ADMISSION_DOCUMENTS_SUBMITTED` | `admission.documents_submitted` | DocumentsSubmitted (StatusChangedV1). |
| 3A `admission.rs` | `ADMISSION_DOCUMENTS_VERIFIED` | `admission.documents_verified` | DocumentsVerified → verification_completed (StatusChangedV1). |
| 3A `admission.rs` | `ADMISSION_APPROVAL_REQUESTED` | `admission.approval_requested` | Approval step started through the WorkflowPort (StatusChangedV1, carries processInstanceId). |
| 3A `admission.rs` | `ADMISSION_APPROVED` | `admission.approved` | AdmissionApproved (StatusChangedV1). |
| 3A `admission.rs` | `ADMISSION_FINANCIALLY_CLEARED` | `admission.financially_cleared` | FinancialCleared: Admissions' own clearance decision from Finance evidence (amendment 2026-10-05; StatusChangedV1). |
| 3A `admission.rs` | `ADMISSION_CONFIRMED` | `admission.confirmed` | AdmissionConfirmed (StatusChangedV1). |
| 3A `admission.rs` | `ADMISSION_STUDENT_CREATED` | `admission.student_created` | AdmissionStudentCreated (StatusChangedV1; studentId set). |
| 3A `admission.rs` | `ADMISSION_COMPLETED` | `admission.completed` | AdmissionCompleted (StatusChangedV1). |
| 3A `admission.rs` | `ADMISSION_CANCELLED` | `admission.cancelled` | AdmissionCancelled (StatusChangedV1). |
| 3A `admission.rs` | `ADMISSION_WITHDRAWN` | `admission.withdrawn` | AdmissionWithdrawn (StatusChangedV1). |
| 3A `admission.rs` | `ADMISSION_REVOKED` | `admission.revoked` | AdmissionRevoked (StatusChangedV1). |
| 3A `admission.rs` | `ADMISSION_EXPIRED` | `admission.expired` | AdmissionExpired (StatusChangedV1). Publisher: Admissions only (ADR-STU-ADM-EXP-001). |
| 3A `record.rs` | `STUDENT_CREATED` | `student.created` | StudentCreated ([`StudentCreatedV1`]). |
| 3A `record.rs` | `STUDENT_ACTIVATED` | `student.activated` | StudentActivated: context pending_activation (StatusChangedV1). |
| 3A `record.rs` | `STUDENT_ENROLLED` | `student.enrolled` | Enrolment completed; context → active (StatusChangedV1). |
| 3A `record.rs` | `STUDENT_ENROLMENT_UPDATED` | `student.enrolment_updated` | Enrolment attributes changed (ModifiedV1, L3-C-008). |
| 3A `record.rs` | `STUDENT_ACADEMIC_ASSIGNMENT_CHANGED` | `student.academic_assignment_changed` | Programme/department/batch/section changed (ModifiedV1, L3-C-009). |
| 3A `record.rs` | `STUDENT_SUSPENDED` | `student.suspended` | Context active → suspended (StatusChangedV1, L3-C-010). |
| 3A `record.rs` | `STUDENT_RESTORED` | `student.restored` | Context suspended → active (StatusChangedV1, L3-C-011). |
| 3A `record.rs` | `STUDENT_TRANSFERRED` | `student.transferred` | Intra- or inter-institute transfer completed ([`StudentTransferredV1`], L3-C-012/013). |
| 3A `record.rs` | `STUDENT_PROFILE_UPDATED` | `student.profile_updated` | (ModifiedV1, L3-C-014). |
| 3A `record.rs` | `STUDENT_COMMUNICATION_PREFERENCES_UPDATED` | `student.communication_preferences_updated` | (ModifiedV1, L3-C-015). |
| 3A `record.rs` | `STUDENT_IDENTITY_CORRECTED` | `student.identity_corrected` | Governed identity correction applied (ModifiedV1, L3-C-016). |
| 3A `record.rs` | `STUDENT_DUPLICATE_RESOLVED` | `student.duplicate_resolved` | Duplicate resolved; the duplicate persona is archived and points at the survivor ([`StudentDuplicateResolvedV1`], L3-C-017). |
| 3A `record.rs` | `STUDENT_ARCHIVED` | `student.archived` | Persona archived / permanent record closure (StatusChangedV1, L3-C-019). |
| 3A `record.rs` | `DOCUMENT_REQUEST_SUBMITTED` | `document_request.submitted` | Student document/certificate request raised (StatusChangedV1, L3-C-018). |
| 3A `record.rs` | `DOCUMENT_REQUEST_ISSUED` | `document_request.issued` | (StatusChangedV1). |
| 3A `record.rs` | `DOCUMENT_REQUEST_REJECTED` | `document_request.rejected` | (StatusChangedV1). |
| 3B `registration.rs` | `REGISTRATION_WINDOW_OPENED` | `registration_window.opened` | (StatusChangedV1, L3-C-020). |
| 3B `registration.rs` | `REGISTRATION_WINDOW_CLOSED` | `registration_window.closed` | (StatusChangedV1). |
| 3B `registration.rs` | `REGISTRATION_SUBMITTED` | `registration.submitted` | (StatusChangedV1). |
| 3B `registration.rs` | `REGISTRATION_APPROVED` | `registration.approved` | (StatusChangedV1). |
| 3B `registration.rs` | `REGISTRATION_REGISTERED` | `registration.registered` | StudentRegistered: effective registration ([`RegistrationV1`]). |
| 3B `registration.rs` | `REGISTRATION_MODIFIED` | `registration.modified` | Add/drop applied ([`RegistrationV1`], L3-C-022). |
| 3B `registration.rs` | `REGISTRATION_CANCELLED` | `registration.cancelled` | (StatusChangedV1, L3-C-023). |
| 3B `registration.rs` | `REGISTRATION_FINALISED` | `registration.finalised` | Frozen + published ([`RegistrationV1`], L3-C-025). |
| 3B `registration.rs` | `REGISTRATION_HOLD_PLACED` | `registration_hold.placed` | ([`RegistrationHoldV1`], L3-C-026). |
| 3B `registration.rs` | `REGISTRATION_HOLD_RELEASED` | `registration_hold.released` | ([`RegistrationHoldV1`]). |
| 3B `progression.rs` | `PROGRESSION_EVALUATED` | `progression.evaluated` | ProgressionEvaluated ([`ProgressionDecisionV1`]). |
| 3B `progression.rs` | `PROGRESSION_STUDENT_PROGRESSED` | `progression.student_progressed` | StudentProgressed ([`ProgressionDecisionV1`]). |
| 3B `progression.rs` | `PROGRESSION_CONDITIONAL_PROMOTION_GRANTED` | `progression.conditional_promotion_granted` | ConditionalPromotionGranted ([`ProgressionDecisionV1`]). |
| 3B `progression.rs` | `PROGRESSION_ACADEMIC_PROBATION_ASSIGNED` | `progression.academic_probation_assigned` | AcademicProbationAssigned ([`ProgressionDecisionV1`]). |
| 3B `progression.rs` | `PROGRESSION_ACADEMIC_DETENTION_APPLIED` | `progression.academic_detention_applied` | AcademicDetentionApplied ([`ProgressionDecisionV1`]). |
| 3B `progression.rs` | `PROGRESSION_DECISION_REVISED` | `progression.decision_revised` | Decision superseded by a revision ([`ProgressionDecisionV1`], L3-C-038). |
| 3B `progression.rs` | `PROGRESSION_CONDITION_FULFILLED` | `progression_condition.fulfilled` | (StatusChangedV1, L3-C-029). |
| 3B `progression.rs` | `PROGRESSION_CONDITION_FAILED` | `progression_condition.failed` | (StatusChangedV1). |
| 3B `progression.rs` | `PROBATION_RESOLVED` | `probation.resolved` | Probation cleared or escalated (StatusChangedV1, L3-C-030). |
| 3B `progression.rs` | `CREDIT_LEDGER_UPDATED` | `credit_ledger.updated` | Ledger entries appended ([`CreditLedgerUpdatedV1`], L3-C-028). |
| 3B `progression.rs` | `CREDIT_TRANSFER_APPROVED` | `credit_transfer.approved` | Credits recognised (StatusChangedV1, L3-C-032). |
| 3B `progression.rs` | `PROGRAMME_MIGRATION_COMPLETED` | `programme_migration.completed` | ([`ProgrammeMigratedV1`], L3-C-033). |
| 3B `progression.rs` | `ACADEMIC_BREAK_STARTED` | `academic_break.started` | (StatusChangedV1, L3-C-034). |
| 3B `progression.rs` | `ACADEMIC_BREAK_REJOINED` | `academic_break.rejoined` | (StatusChangedV1, L3-C-035). |
| 3B `progression.rs` | `QUALIFICATION_ELIGIBILITY_UPDATED` | `qualification_eligibility.updated` | QualificationEligibilityUpdated ([`QualificationEligibilityV1`], L3-C-036). Consumed by 3C. |
| 3B `progression.rs` | `ABC_SYNC_COMPLETED` | `abc_sync.completed` | (StatusChangedV1, L3-C-037). |
| 3B `progression.rs` | `ABC_SYNC_FAILED` | `abc_sync.failed` | (StatusChangedV1). |
| 3B `progression.rs` | `PROGRESSION_POLICY_EFFECTIVE` | `progression_policy.effective` | (StatusChangedV1, L3-C-075). |
| 3B `progression.rs` | `PROGRESSION_POLICY_RETIRED` | `progression_policy.retired` | (StatusChangedV1). |
| 3C `completion.rs` | `QUALIFICATION_COMPLETION_ELIGIBILITY_EVALUATED` | `qualification_completion.eligibility_evaluated` | QualificationEligibilityEvaluated (StatusChangedV1). |
| 3C `completion.rs` | `QUALIFICATION_COMPLETION_DEGREE_AUDIT_COMPLETED` | `qualification_completion.degree_audit_completed` | DegreeAuditCompleted (StatusChangedV1). |
| 3C `completion.rs` | `QUALIFICATION_COMPLETION_INSTITUTIONAL_CLEARANCE_COMPLETED` | `qualification_completion.institutional_clearance_completed` | InstitutionalClearanceCompleted (StatusChangedV1). |
| 3C `completion.rs` | `QUALIFICATION_COMPLETION_APPROVED` | `qualification_completion.approved` | QualificationCompletionApproved (StatusChangedV1). |
| 3C `completion.rs` | `QUALIFICATION_COMPLETION_QUALIFICATION_AWARDED` | `qualification_completion.qualification_awarded` | QualificationAwarded ([`QualificationAwardedV1`]). |
| 3C `completion.rs` | `QUALIFICATION_COMPLETION_ACADEMIC_RECORD_FINALIZED` | `qualification_completion.academic_record_finalized` | AcademicRecordFinalized (StatusChangedV1). |
| 3C `completion.rs` | `QUALIFICATION_COMPLETION_DEFERRED` | `qualification_completion.deferred` | (StatusChangedV1, L3-C-049). |
| 3C `completion.rs` | `QUALIFICATION_CORRECTED` | `qualification.corrected` | ([`QualificationAwardedV1`] of the corrected version, L3-C-050). |
| 3C `completion.rs` | `QUALIFICATION_REVOKED` | `qualification.revoked` | (StatusChangedV1, L3-C-050). |
| 3C `completion.rs` | `STUDENT_EXIT_INITIATED` | `student_exit.initiated` | ([`StudentExitV1`]). |
| 3C `completion.rs` | `STUDENT_EXIT_COMPLETED` | `student_exit.completed` | StudentExitCompleted ([`StudentExitV1`]); the context is terminal. |
| 3C `completion.rs` | `STUDENT_EXIT_CANCELLED` | `student_exit.cancelled` | ([`StudentExitV1`]). |
| 3C `completion.rs` | `EXIT_CLEARANCE_COMPLETED` | `exit_clearance.completed` | All clearance items cleared/waived (StatusChangedV1, L3-C-046). |
| 3C `guardian.rs` | `GUARDIAN_RELATIONSHIP_PROPOSED` | `guardian_relationship.proposed` | ([`GuardianRelationshipV1`]). |
| 3C `guardian.rs` | `GUARDIAN_RELATIONSHIP_VERIFIED` | `guardian_relationship.verified` | (StatusChangedV1). |
| 3C `guardian.rs` | `GUARDIAN_RELATIONSHIP_ACTIVATED` | `guardian_relationship.activated` | (StatusChangedV1). |
| 3C `guardian.rs` | `GUARDIAN_RELATIONSHIP_MODIFIED` | `guardian_relationship.modified` | (ModifiedV1, L3-C-052). |
| 3C `guardian.rs` | `GUARDIAN_RELATIONSHIP_SUSPENDED` | `guardian_relationship.suspended` | (StatusChangedV1, L3-C-059). |
| 3C `guardian.rs` | `GUARDIAN_RELATIONSHIP_REINSTATED` | `guardian_relationship.reinstated` | (StatusChangedV1, L3-C-060). |
| 3C `guardian.rs` | `GUARDIAN_RELATIONSHIP_TERMINATED` | `guardian_relationship.terminated` | (StatusChangedV1, L3-C-061). |
| 3C `guardian.rs` | `GUARDIAN_CUSTODY_CHANGED` | `guardian_custody.changed` | ([`GuardianRelationshipV1`], L3-C-053). |
| 3C `guardian.rs` | `GUARDIAN_CONSENT_GRANTED` | `guardian_consent.granted` | (StatusChangedV1, L3-C-054). |
| 3C `guardian.rs` | `GUARDIAN_CONSENT_WITHDRAWN` | `guardian_consent.withdrawn` | (StatusChangedV1). |
| 3C `guardian.rs` | `GUARDIAN_COMMUNICATION_AUTHORISED` | `guardian_communication.authorised` | (StatusChangedV1, L3-C-055). |
| 3C `guardian.rs` | `GUARDIAN_COMMUNICATION_REVOKED` | `guardian_communication.revoked` | (StatusChangedV1). |
| 3C `guardian.rs` | `EMERGENCY_CONTACT_ASSIGNED` | `emergency_contact.assigned` | (StatusChangedV1, L3-C-056). |
| 3C `guardian.rs` | `EMERGENCY_CONTACT_REMOVED` | `emergency_contact.removed` | (StatusChangedV1). |
| 3C `guardian.rs` | `FINANCIAL_RESPONSIBILITY_ASSIGNED` | `financial_responsibility.assigned` | (StatusChangedV1, L3-C-057). |
| 3C `guardian.rs` | `FINANCIAL_RESPONSIBILITY_ENDED` | `financial_responsibility.ended` | (StatusChangedV1). |
| 3C `guardian.rs` | `PICKUP_AUTHORISATION_GRANTED` | `pickup_authorisation.granted` | (StatusChangedV1, L3-C-058). |
| 3C `guardian.rs` | `PICKUP_AUTHORISATION_REVOKED` | `pickup_authorisation.revoked` | (StatusChangedV1). |
| 3C `scholarship.rs` | `SCHOLARSHIP_PUBLISHED` | `scholarship.published` | (StatusChangedV1, L3-C-062). |
| 3C `scholarship.rs` | `SCHOLARSHIP_ACTIVATED` | `scholarship.activated` | (StatusChangedV1). |
| 3C `scholarship.rs` | `SCHOLARSHIP_SUSPENDED` | `scholarship.suspended` | (StatusChangedV1). |
| 3C `scholarship.rs` | `SCHOLARSHIP_EXPIRED` | `scholarship.expired` | (StatusChangedV1). |
| 3C `scholarship.rs` | `SCHOLARSHIP_ARCHIVED` | `scholarship.archived` | (StatusChangedV1). |
| 3C `scholarship.rs` | `SCHOLARSHIP_POLICY_ACTIVATED` | `scholarship_policy.activated` | (StatusChangedV1, L3-C-063). |
| 3C `scholarship.rs` | `SCHOLARSHIP_POLICY_RETIRED` | `scholarship_policy.retired` | (StatusChangedV1). |
| 3C `scholarship.rs` | `SCHOLARSHIP_ELIGIBILITY_DETERMINED` | `scholarship_eligibility.determined` | Eligible / not eligible / conditional ([`ScholarshipEligibilityV1`], L3-C-064). |
| 3C `scholarship.rs` | `SCHOLARSHIP_ELIGIBILITY_RECOMMENDED` | `scholarship_eligibility.recommended` | Recommendation generated ([`ScholarshipEligibilityV1`]). |
| 3C `scholarship.rs` | `SCHOLARSHIP_ELIGIBILITY_CONDITION_RESOLVED` | `scholarship_eligibility.condition_resolved` | (StatusChangedV1, L3-C-065). |
| 3C `scholarship.rs` | `SCHOLARSHIP_ELIGIBILITY_REASSESSED` | `scholarship_eligibility.reassessed` | ([`ScholarshipEligibilityV1`], L3-C-066). |
| 3C `scholarship.rs` | `SCHOLARSHIP_ELIGIBILITY_RENEWED` | `scholarship_eligibility.renewed` | ([`ScholarshipEligibilityV1`], L3-C-067). |
| 3C `scholarship.rs` | `SCHOLARSHIP_ELIGIBILITY_EXPIRED` | `scholarship_eligibility.expired` | (StatusChangedV1, L3-C-068). |
| 3C `scholarship.rs` | `SCHOLARSHIP_ELIGIBILITY_HANDED_OFF` | `scholarship_eligibility.handed_off` | Hand-off to Financial Aid (Domain F, Batch 5) — **event only**, no Finance call ([`ScholarshipEligibilityV1`], L3-C-071). |
| 3C `scholarship.rs` | `SCHOLARSHIP_EXTERNAL_SYNCHRONISED` | `scholarship_external.synchronised` | External ecosystem sync result (StatusChangedV1, L3-C-069; port, no-op adapter). |
| 3C `scholarship.rs` | `SCHOLARSHIP_RECALCULATION_COMPLETED` | `scholarship_recalculation.completed` | Bulk recalculation run finished ([`ScholarshipRecalculationV1`], L3-C-070). |
| 3C `alumni.rs` | `ALUMNI_TRANSITION_INITIATED` | `alumni_transition.initiated` | AlumniTransitionInitiated (StatusChangedV1). |
| 3C `alumni.rs` | `ALUMNI_TRANSITION_ELIGIBILITY_VALIDATED` | `alumni_transition.eligibility_validated` | TransitionEligibilityValidated (StatusChangedV1). |
| 3C `alumni.rs` | `ALUMNI_TRANSITION_POLICY_VALIDATED` | `alumni_transition.policy_validated` | TransitionPolicyValidated (StatusChangedV1). |
| 3C `alumni.rs` | `ALUMNI_TRANSITION_APPROVED` | `alumni_transition.approved` | AlumniTransitionApproved (StatusChangedV1). |
| 3C `alumni.rs` | `ALUMNI_TRANSITION_STUDENT_RELATIONSHIP_CLOSED` | `alumni_transition.student_relationship_closed` | StudentRelationshipClosed (StatusChangedV1). |
| 3C `alumni.rs` | `ALUMNI_TRANSITION_ALUMNI_RELATIONSHIP_ACTIVATED` | `alumni_transition.alumni_relationship_activated` | AlumniRelationshipActivated (StatusChangedV1). |
| 3C `alumni.rs` | `ALUMNI_TRANSITION_COMPLETED` | `alumni_transition.completed` | AlumniTransitionCompleted (StatusChangedV1). |
| 3C `alumni.rs` | `ALUMNI_TRANSITION_CORRECTED` | `alumni_transition.corrected` | (ModifiedV1, L3-C-073). |
| 3C `alumni.rs` | `ALUMNI_TRANSITION_ARCHIVED` | `alumni_transition.archived` | AlumniTransitionArchived (StatusChangedV1, L3-C-074). |
Each slice file exports `EVENT_TYPES`; `events::all_event_types()` lists every one and a unit test
keeps them unique and well formed (4 dot-separated parts, lowercase). A slice may add an event to
its own file (constant + `EVENT_TYPES` entry + payload) and must add it to the table above in its
PR. Specific payloads already defined: `AdmissionInitiatedV1`, `StudentCreatedV1`,
`StudentTransferredV1`, `StudentDuplicateResolvedV1`, `RegistrationV1`, `RegistrationHoldV1`,
`ProgressionDecisionV1`, `CreditLedgerUpdatedV1`, `ProgrammeMigratedV1`,
`QualificationEligibilityV1`, `QualificationAwardedV1`, `StudentExitV1`, `GuardianRelationshipV1`,
`ScholarshipEligibilityV1`, `ScholarshipRecalculationV1`.

**Cross-domain events:** `scholarship_eligibility.handed_off` (3C, L3-C-071) is the **only**
hand-off to Financial Aid — no Finance call, no Finance table. Finance (Batch 5) and Academic
(Batch 4) will subscribe to `student.status_changed`, `registration.finalised`,
`admission.financially_cleared` etc.

### 4.4 Permissions and role mapping (`sutra_kernel::auth::Permission`)

Staff reads of every Domain C resource require `student.read` (Administrator **and Faculty** —
read-only directory / Student 360 for mentors and advisors). Students act only on their **own**
record through `student.self` / `registration.self`; the handler resolves the student from the
session subject (`shared::student::me`, 404 when the account has no linked student) and never
takes a student id from the client for self-service. Every write permission below is
**Administrator only** (unit test `batch3_domain_c_grants`). Faculty and Student hold no write.

| Variant | Name | Step-up | Roles | Slice: used for |
|---------|------|---------|-------|-----------------|
| `StudentRead` | `student.read` | no | Admin, Faculty | all slices: staff reads |
| `StudentSelfService` | `student.self` | no | Student | 3A: own 360, own documents + certificate requests, communication preferences; 3C may expose own guardians/scholarships read-only |
| `AdmissionManage` | `admission.manage` | no | Admin | 3A: applications, review, offers, admission documents + verification, cancellation, withdrawal, expiry, readmission initiation |
| `AdmissionDecide` | `admission.decide` | no | Admin | 3A: approval decision endpoint, financial-clearance decision, confirmation |
| `AdmissionRevoke` | `admission.revoke` | **yes** | Admin | 3A: revocation |
| `StudentManage` | `student.manage` | no | Admin | 3A: creation/activation/enrolment, enrolment update, assignment, intra-institute transfer, profile, communication preferences, document requests processing, student documents |
| `StudentLifecycle` | `student.lifecycle` | **yes** | Admin | 3A: suspend, restore, inter-institute transfer |
| `StudentIdentityManage` | `student.identity.manage` | **yes** | Admin | 3A: identity correction, duplicate resolution |
| `StudentArchive` | `student.archive` | **yes** | Admin | 3A: archival / permanent record closure |
| `RegistrationManage` | `registration.manage` | no | Admin | 3B: windows, holds, staff registration actions, approval, cancellation, finalisation |
| `RegistrationSelf` | `registration.self` | no | Student | 3B: own registration, add/drop, cancellation request in an open window |
| `ProgressionManage` | `progression.manage` | no | Admin | 3B: decision drafts, ledger, conditions, probation, detention, credit transfer, migration, break, eligibility, ABC sync |
| `ProgressionDecide` | `progression.decide` | no | Admin | 3B: make decisions effective, revisions |
| `ProgressionPolicyManage` | `progression.policy.manage` | no | Admin | 3B: progression policy lifecycle |
| `CompletionManage` | `completion.manage` | no | Admin | 3C: completion evaluation/audit/clearance/deferral, exit cases, exit clearance, administrative closure |
| `QualificationConfer` | `qualification.confer` | **yes** | Admin | 3C: approve + award, record finalisation, correction |
| `QualificationRevoke` | `qualification.revoke` | **yes** | Admin | 3C: revocation of a conferred qualification |
| `ExitDecide` | `exit.decide` | **yes** | Admin | 3C: exit approval incl. termination, deceased closure |
| `GuardianManage` | `guardian.manage` | no | Admin | 3C: guardians, relationships, custody, consent, communication, emergency contacts, financial responsibility, pickup |
| `ScholarshipManage` | `scholarship.manage` | no | Admin | 3C: drafts, determinations, reassessment, renewal, recalculation, external sync, hand-off |
| `ScholarshipPublish` | `scholarship.publish` | no | Admin | 3C: publish/activate/suspend/expire/archive definitions and policies |
| `AlumniManage` | `alumni.manage` | no | Admin | 3C: alumni transition, correction, archival |

Step-up is enforced only when the deployment enables it; DB tests run in dev mode without it.
The test `administrator_holds_institution_admin_permissions` deliberately excludes the two
self-service permissions (an administrator has no own student record).

## 5. HTTP API

### 5.1 Conventions (as Domain B)

- Every handler starts with `auth.require(Permission::…)?`. Resolve the student / context first
  (`shared::student::load_student` / `load_context`, tenant from the session), then build
  `shared::scope::institution_scope(ctx, context.institution_id)` for events/audit.
- Before a write: `shared::guard::require_institution_writable` (institution of the path or of the
  context: 404 missing, 409 when `suspended | archived`), `require_student_writable` (archived
  persona → 409, except 3A readmission), `require_live_context` for 3B/3C commands.
- Create: `POST`, required `Idempotency-Key`, `201` + `ETag`. Update or state command: `If-Match`
  required (`shared::http::if_match`: 428 missing, 412 stale); response = updated resource + `ETag`.
- State commands are `POST …/{id}/<verb>` (`/submit`, `/decision`, `/approve`, `/cancel`,
  `/withdraw`, `/revoke`, `/suspend`, `/restore`, `/finalise`, `/release`, `/publish`, `/activate`,
  `/archive`, …) with an optional `{ "reason": "…" }`. Literal paths per command, no generic
  `/{command}` route.
- Lists: `GET` collection under the parent, `?status=` filter, `{ "items": [...] }`. The student
  directory (3A) additionally supports `?q=` (name/number) and `?programmeId=`; paging is optional
  in Batch 3 (`?limit=` ≤ 200, default 50, `nextCursor` when more).
- Errors: illegal transition → `409 conflict`; validation → `422`; foreign tenant / unknown id →
  `404`; Domain B reference not found → `422` on the field (e.g. `programmeId`).
- JSON members camelCase, statuses snake_case (§4.2), business dates `YYYY-MM-DD`.
- **Every `#[utoipa::path]` sets an explicit `operation_id`** with the slice prefix below and the
  slice's `tag`.

### 5.2 Path prefixes, operation_id prefixes, tags (all under `/api/v1/student`)

**Path parameter rule (Batch 2 lesson):** axum refuses two different parameter names at the same
position, and all three slices hang paths under `/students/{…}` and `/institutions/{…}`. So **the
first parameter after any collection segment is always `{id}`**; deeper parameters get names
(`/registrations/{id}/lines/{lineId}`, `/exits/{id}/clearances/{clearanceId}`).

| Slice | Paths | operation_id prefixes | tag |
|-------|-------|-----------------------|-----|
| 3A | `/institutions/{id}/admission-applications`, `/admission-applications/{id}[/…]`, `/institutions/{id}/admissions`, `/admissions/{id}[/…]` (incl. `/admissions/{id}/documents[/{documentId}/verify]`, `/decision`) | `admission_application_`, `admission_` | `admissions` |
| 3A | `/institutions/{id}/students` (directory), `/students/{id}` (Student 360 read model), `/students/{id}/{enrolment,assignment,profile,communication-preferences,suspend,restore,archive}`, `/students/{id}/documents`, `/students/{id}/document-requests`, `/document-requests/{id}[/…]`, `/students/{id}/identity-corrections`, `/identity-corrections/{id}[/…]`, `/duplicate-cases[/{id}[/…]]`, `/students/{id}/transfers`, `/transfers/{id}[/…]`, `/students/{id}/status-history`, `/me/student`, `/me/document-requests` | `student_`, `student_document_`, `document_request_`, `identity_correction_`, `duplicate_case_`, `student_transfer_`, `student_self_` | `students` |
| 3B | `/institutions/{id}/registration-windows`, `/registration-windows/{id}[/…]`, `/students/{id}/registrations`, `/registrations/{id}[/…]` (lines `/registrations/{id}/lines[/{lineId}]`), `/students/{id}/holds`, `/registration-holds/{id}[/release]`, `/me/registrations[/…]`, `/me/registration-windows` | `registration_window_`, `registration_`, `registration_hold_`, `registration_self_` | `registration` |
| 3B | `/institutions/{id}/progression-policies`, `/progression-policies/{id}[/…]`, `/students/{id}/progression-decisions`, `/progression-decisions/{id}[/…]` (incl. `/revisions`), `/students/{id}/credit-ledger`, `/progression-conditions/{id}[/…]`, `/probation-cases/{id}[/…]`, `/detention-cases/{id}[/…]`, `/students/{id}/credit-transfers`, `/credit-transfers/{id}[/…]`, `/students/{id}/programme-migrations`, `/programme-migrations/{id}[/…]`, `/students/{id}/academic-breaks`, `/academic-breaks/{id}[/…]`, `/students/{id}/qualification-eligibility`, `/students/{id}/abc-syncs`, `/abc-syncs/{id}` | `progression_policy_`, `progression_`, `credit_ledger_`, `progression_condition_`, `probation_`, `detention_`, `credit_transfer_`, `programme_migration_`, `academic_break_`, `qualification_eligibility_`, `abc_sync_` | `progression` |
| 3C | `/students/{id}/qualification-completions`, `/qualification-completions/{id}[/…]`, `/conferred-qualifications/{id}[/{corrections,revoke}]`, `/students/{id}/exits`, `/exits/{id}[/…]` (clearance `/exits/{id}/clearances[/{clearanceId}]`) | `qualification_completion_`, `conferred_qualification_`, `exit_`, `exit_clearance_` | `completion` |
| 3C | `/institutions/{id}/guardians`, `/guardians/{id}[/…]`, `/students/{id}/guardian-relationships`, `/guardian-relationships/{id}[/…]` (consent / communication nested), `/students/{id}/emergency-contacts`, `/emergency-contacts/{id}[/…]`, `/students/{id}/pickup-authorisations`, `/pickup-authorisations/{id}[/…]`, `/students/{id}/financial-responsibilities`, `/financial-responsibilities/{id}[/…]` | `guardian_`, `guardian_relationship_`, `guardian_consent_`, `emergency_contact_`, `pickup_authorisation_`, `financial_responsibility_` | `guardians` |
| 3C | `/institutions/{id}/scholarships`, `/scholarships/{id}[/…]` (policies `/scholarships/{id}/policies`), `/scholarship-policies/{id}[/…]`, `/students/{id}/scholarship-eligibilities`, `/scholarship-eligibilities/{id}[/…]`, `/institutions/{id}/scholarship-recalculations`, `/scholarship-recalculations/{id}` | `scholarship_`, `scholarship_policy_`, `scholarship_eligibility_`, `scholarship_recalculation_` | `scholarships` |
| 3C | `/students/{id}/alumni-transitions`, `/alumni-transitions/{id}[/…]` | `alumni_transition_` | `alumni` |

Prefix hygiene: 3A must not start an operation_id with `exit_` / `registration_` / `qualification_`;
3C uses `exit_` (not `student_exit_`) and `conferred_qualification_` (not `qualification_` alone)
so no prefix of one slice is a prefix of another slice's names. The tags are already in
`apps/api/src/openapi.rs`; the router mount points are already registered in
`sutra_student::router_with_state` (`admission`, `record` → 3A; `registration`, `progression` → 3B;
`completion`, `guardian`, `scholarship`, `alumni` → 3C). Each slice only fills
`pub fn router(state: StudentState) -> OpenApiRouter` in its own `mod.rs`.

### 5.3 Minimum response fields other slices / screens rely on

- **Student** (3A, `GET /students/{id}`, directory items): `id, studentNumber, displayName, givenName, familyName, status, subjectLinked (bool), version` + **`currentContext`** `{id, institutionId, programmeId, curriculumVersionId, campusId, orgUnitId, batchCode, sectionCode, currentTermNo, enrolmentNumber, status, version}` (nullable). Student 360 tabs of 3B/3C load their own endpoints with the student id; they only need this DTO for the header.
- **Admission** (3A): `id, institutionId, applicationId, programmeId, status, offerExpiresOn, studentId, processInstanceId, version`.
- **Registration** (3B): `id, studentId, studentContextId, academicTermId, status, totalCredits, lines[{id, curriculumComponentId, kind, status, credits}], version`; **RegistrationWindow**: `id, institutionId, academicTermId, opensOn, closesOn, status`.
- **ProgressionDecision** (3B): `id, studentId, academicTermId, outcome, status, supersedesDecisionId, version`; **credit ledger**: `{totalCredits, entries[{id, kind, credits, curriculumComponentId?, academicTermId?, recordedAt}]}`.
- **QualificationEligibility** (3B, read by 3C): `studentId, eligible, outcomes[], totalCredits, evaluatedAt`.
- **QualificationCompletion** (3C): `id, studentId, outcome, status, conferredOn, version`; **StudentExit**: `id, studentId, kind, status, effectiveOn, version`.
- **GuardianRelationship** (3C): `id, guardianId, studentId, relationshipType, hasLegalCustody, status, version`; **ScholarshipEligibility**: `id, scholarshipId, studentId, status, validUntil, version`.
- Collections: `{ "items": [ … ] }`.

## 6. Cross-slice runtime rules

### 6.1 Foundation helpers (`modules/student/src/shared/`)

- `state::StudentState` — `pool, workflow, approvals, references, finance, results, abc,
  external_scholarships`. Built by `StudentDeps::into_state()` (auto-approve) or by hand in tests.
- `scope` — tenant from the session; staff administration is tenant-wide (as Domain B); the
  context's institution is the event/audit scope; every query filters on `tenant_id`.
- `guard` — institution writable rule (§5.1), archived persona, live context.
- `student` — row types `StudentRow` / `StudentContextRow`, `load_*` / `lock_*`, `contexts_of`,
  `live_context`, `current_context`, `me(ctx)` (self-service), `set_context_progress` (3B).
- `lifecycle` — `change_context_status` / `change_student_status` (§3.2, §4.2).
- `publish` — `event` + `audit` in the owner-local tx (actor `system:*` → `ActorKind::System`).
- `approval` — §9. `http` — `with_etag`, `if_match`, `actor`. `time::business_today` — the
  institution's business date (IANA zone from Domain B).

### 6.2 Domain B references (`sutra_contracts::institution::reference::InstitutionReferencePort`)

Implemented by `sutra_institution::PgInstitutionReferences`, wired by `apps/api::app::student_deps`.
Methods (tenant-scoped, missing/foreign → `None`): `institution` (status + timezone), `campus`,
`org_unit`, `programme`, `curriculum_version`, `current_curriculum_version(programme, on)`
(published, latest effective ≤ date), `curriculum_components(version)` (term_no, credits,
category, course code), `academic_year`, `academic_term` (+ calendar version status),
`key_dates(calendar_version)` (`registration_window` key dates feed 3B windows).
Validation rules: a new admission/enrolment needs a **published** programme and curriculum version
of the same institution; registration needs a term of a **published** calendar version; campus /
org unit must be `active`. Domain C stores the ids as plain uuids and never caches Domain B rows.
Academic offerings/sections (Domain D, Batch 4) do not exist yet: 3B registers **curriculum
components** (`curriculumComponentId`) for a term; Batch 4 will add offering ids additively.

### 6.3 Finance, Exams and external ports (stubs in Batch 3)

| Port | Adapter wired now | Behaviour | Consumer |
|------|-------------------|-----------|----------|
| `sutra_contracts::finance::FinanceClearancePort` (`admission_clearance`, `student_dues`) | `sutra_finance::StubFinanceClearance` | always `FinanceEvidence::NotApplicable` | 3A financial clearance (`permits_clearance()` → may decide `financially_cleared`; Admissions still records its own decision + event, amendment CE-001/002); 3B finance holds; 3C exit clearance `finance` item (auto-`cleared` with note "no Finance obligation configured") |
| `sutra_contracts::exams::ResultsPort` (`term_result`) | `sutra_exams::NoPublishedResults` | always `None` | 3B progression: when `None`, the decision draft carries manually entered evidence (`creditsEarned`, failed components, note) by `progression.manage` |
| `sutra_contracts::student::ports::AbcRegistryPort` | `sutra_student::adapters::NoopAbcRegistry` | `ExternalSyncOutcome::NotConfigured` | 3B ABC sync (record as `failed` with reason `not_configured`, or a dedicated skipped note — 3B decides, document it) |
| `sutra_contracts::student::ports::ExternalScholarshipPort` | `sutra_student::adapters::NoopExternalScholarships` | `NotConfigured` | 3C external sync |
| `sutra_contracts::student::ports::StudentDirectory` | `sutra_student::PgStudentDirectory` | reads the foundation tables | other domains later (not used by 3A/3B/3C themselves) |

Tests can swap any port by building a `StudentState` by hand (e.g. a finance double returning
`Outstanding`) and serving `sutra_student::router_with_state(state)`.

## 7. Cross-slice dependencies (3B/3C need 3A's Student) and readiness

- **Student aggregate is foundation-owned**, so 3B/3C never wait for 3A: they read
  `student`/`student_context` through `shared::student` and seed test students with
  `apps/api/tests/support/b3.rs::seed_student` (active persona + active context in a published
  programme/curriculum, term 1). 3A's real flow creates the same shape, so behaviour after merge
  is identical. 3A must keep the foundation columns' meaning (§3.2).
- **3B → 3C:** 3C's qualification completion reads 3B's `qualification_eligibility` table
  (read-only, plain uuid reference) **if present**; until 3B is merged 3C develops against the
  `QualificationEligibilityV1` shape and an injectable read function in its own code, defaulting to
  "eligible when the curriculum's `min_total_credits` is met by the 3B ledger, else evaluated
  manually". At integration the integrator points 3C at 3B's table (contract change noted in 3C's PR).
- **3B registration preconditions** (read-only checks): live context `active` (not
  `pending_activation` / `suspended`), no active hold, standing not `academic_break` /
  `detained` (unless repeat registration), window `open` and business date inside it.
- **3C exits** close the context through `shared::lifecycle`; 3B must treat a terminal context as
  read-only (guard), and 3A's archival requires no live context (enforced by the helper).
- **Student 360** (3A frame) composes tabs from all slices; there is no server-side aggregate
  endpoint spanning slices in Batch 3 (each tab calls its slice's endpoints).
- There is **no activation-readiness checklist** in Domain C. Readiness-like gates are plain
  409 preconditions listed above. Admissions may run while the institution is not yet `active`
  (same "editable" rule as Batch 2 structure).

## 8. Inbox consumer hooks (`modules/platform/src/inbox/rules/`)

`rules::plan` dispatches by event type to `student_3a.rs` (3A), `registration_3b.rs` (3B),
`completion_3c.rs` (3C); each declares `EVENT_TYPES` (only types from its own event files — test
`slices_never_claim_the_same_type`) and a pure `plan`. Add a type only **with** its rule. Category:
reuse `institution` (no new notification category in Batch 3, so no platform migration). Notify
students with `Recipient::Subject(studentSubjectRef)` when the payload carries it; otherwise
`Recipient::Role(Role::Administrator)`. To-dos with `todos::SystemTodo` for administrators.
Nobody projects `student.status_changed`.

| Slice | Types | Projection |
|-------|-------|------------|
| 3A | `admission.approval_requested` | to-do (admins) "Decide admission of …" (only meaningful in `AwaitDecision`) |
| 3A | `admission.confirmed`, `admission.expired`, `admission.revoked` | notification (admins) |
| 3A | `student.created` (with subject) / `student.enrolled`, `student.suspended`, `student.restored` | notification to the student (subject) + admins for suspension |
| 3A | `document_request.submitted` → to-do (admins); `document_request.issued` / `rejected` → notification to the student |
| 3B | `registration_window.opened` | notification to `Role::Student` (institution-scoped) |
| 3B | `registration.finalised`, `registration_hold.placed/released` | notification to the student |
| 3B | `progression.academic_probation_assigned`, `progression.academic_detention_applied`, `progression.conditional_promotion_granted` | notification to the student + admins |
| 3C | `qualification_completion.qualification_awarded`, `student_exit.completed` | notification to the student + admins |
| 3C | `exit_clearance.completed`, `guardian_relationship.proposed` (verify) | to-do / notification for admins |
| 3C | `scholarship_eligibility.determined`, `renewed`, `expired` | notification to the student |

## 9. WorkflowPort usage for approval steps (first BPMN approvals)

`shared::approval` wraps `WorkflowPort` exactly like Domain B: the composition root passes the
platform `NoopWorkflowAdapter` and `ApprovalMode::AutoApprove`.

1. Validate the command and current state (read only).
2. `request_approval(&state, &scope, process::<KEY>, business_key(aggregate, id, version), vars)`
   **before** opening the tx (deterministic key → retry-safe).
3. One owner-local tx: re-check the version; set the pending state (`approval_pending`,
   `under_review`, `pending_approval`, `submitted`, …); store `process_instance_id`; emit the
   *requested/submitted* event. If the outcome is `ApprovalOutcome::Approved`, **also** apply the
   decision in the same tx (final state + decision event with `processInstanceId`, audit actor
   `system:auto-approval`).
4. Always expose the explicit decision endpoint (e.g. `POST /admissions/{id}/decision {decision:
   approve|reject, reason}` with `admission.decide`) so `AwaitDecision` (Flowable, Batch 9) needs no
   API change. Each slice has at least one DB test building a `StudentState` with
   `ApprovalMode::AwaitDecision` (via `sutra_student::router_with_state` and
   `sutra_api::app::student_deps`).

Process keys (`shared::approval::process`): 3A `ADMISSION_APPROVAL` (**the reference BPMN approval
of Batch 3**: verification_completed → approval_pending → approved | cancelled),
`ADMISSION_REVOCATION`, `STUDENT_SUSPENSION`, `STUDENT_TRANSFER`, `IDENTITY_CORRECTION`,
`DUPLICATE_RESOLUTION`, `STUDENT_ARCHIVAL`; 3B `REGISTRATION_APPROVAL`, `PROGRESSION_DECISION`,
`PROGRESSION_REVISION`, `CREDIT_TRANSFER`, `PROGRAMME_MIGRATION`, `ACADEMIC_BREAK`,
`PROGRESSION_POLICY`; 3C `QUALIFICATION_COMPLETION`, `QUALIFICATION_REVOCATION`, `STUDENT_EXIT`,
`GUARDIAN_VERIFICATION`, `SCHOLARSHIP_POLICY`, `SCHOLARSHIP_RECOMMENDATION`, `ALUMNI_TRANSITION`.
Use only the keys of your slice; a new key is a contract change.

## 10. Web

| Route | File (owner) | Content |
|-------|--------------|---------|
| `/students/:tab?` | `routes/admin/students.tsx` (3A frame, pre-wired) | Admin → Students tabs (`STUDENTS_TABS` in `features/student/index.ts`): `directory`, `admissions` (3A), `registration`, `progression` (3B), `exits`, `guardians`, `scholarships` (3C). Uses `features/institution/AdminScreen` (admin gate + institution picker) |
| `/students/record/:studentId/:tab?` | `routes/admin/student-360.tsx` (3A frame, pre-wired) | Student 360 tabs (`STUDENT_360_TABS`): `overview`, `enrolment`, `documents` (3A), `registration`, `progression` (3B), `guardians`, `scholarships`, `completion` (3C). Staff only (faculty read-only) |
| `/documents` | `routes/student/documents.tsx` (3A) | student-facing Student Documents + certificate requests (`me` endpoints) |
| `/registration` | `routes/student/registration.tsx` (3B) | student-facing registration / add-drop (`me` endpoints); student nav entry "Registration" added |

Each slice fills the placeholder components exported from its folder — keep the export names
(`features/student/record/index.tsx`: `StudentsDirectoryTab, AdmissionsTab, Student360Overview,
Student360Enrolment, Student360Documents, StudentDocumentsScreen`;
`features/student/registration/index.tsx`: `RegistrationAdminTab, ProgressionAdminTab,
Student360Registration, Student360Progression, StudentRegistrationScreen`;
`features/student/completion/index.tsx`: `ExitsAdminTab, GuardiansAdminTab, ScholarshipsAdminTab,
Student360Guardians, Student360Scholarships, Student360Completion`). Put API hooks in your own
folder (`features/student/<slice>/api.ts`), use `@sutra/api-client` after regeneration, and a mock
mode like `features/institution/mock.ts`. Until a sibling slice is merged, type its calls locally
in your folder and switch to generated types at integration. Never edit another slice's folder;
3A keeps the frame files' imports of 3B/3C components intact.

## 11. Worker environment: separate DB and target dir per slice

| Slice | Worktree | Branch | Database | `CARGO_TARGET_DIR` |
|-------|----------|--------|----------|--------------------|
| 3A | `/workspace/work/erp-3a` | `batch3/3a` | `postgres://sutra:sutra@localhost:5432/sutra_3a` | `/tmp/target-3a` |
| 3B | `/workspace/work/erp-3b` | `batch3/3b` | `postgres://sutra:sutra@localhost:5432/sutra_3b` | `/workspace/work/targets/3b` |
| 3C | `/workspace/work/erp-3c` | `batch3/3c` | `postgres://sutra:sutra@localhost:5432/sutra_3c` | `/workspace/work/targets/3c` |

The databases exist (owner `sutra`); migrations run on first test. The runtime login role
`sutra_it_runtime` is cluster-wide and idempotently provisioned. Build with `-j 1` when the other
slices are building, `CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0`, the low-memory linker
(`CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=/workspace/work/tools/2b/cc-lowmem`; the sandbox is
**aarch64**), and retry on `signal: 9`. **Disk:** `/` (incl. `/tmp`) is ~6.3 GB, RAM-backed, with
~4.2 GB free after the foundation; one full debug target dir (workspace + db tests) is **~3 GB**, so
only one fits in `/tmp`. Hence 3A builds in `/tmp/target-3a` and 3B/3C in
`/workspace/work/targets/3b|3c` (JuiceFS: slower, effectively unlimited). If `df -h /` drops below
~1 GB, 3A moves to `/workspace/work/targets/3a` too; never delete another worker's target dir.

```sh
cd /workspace/work/erp-3b/sutraerp
export CARGO_TARGET_DIR=/workspace/work/targets/3b DATABASE_URL=postgres://sutra:sutra@localhost:5432/sutra_3b
cargo test -j 1 -p sutra-api --features db-tests --test db_b3b
```

Quality gates per slice before pushing: `cargo fmt --all -- --check`, `cargo clippy --workspace
--all-targets --all-features -- -D warnings`, `cargo test --workspace --locked --no-fail-fast`,
DB tests on a freshly recreated DB (`-p sutra-persistence`, `-p sutra-platform`, `-p sutra-api
--features db-tests --no-fail-fast`), OpenAPI regenerated + `pnpm gen:api` with no drift after
commit, `pnpm -r typecheck && pnpm lint && pnpm -r test`. Commit `[skip ci]` as
`Varad1614 <Varad1614@users.noreply.github.com>`.

## 12. Merge order and known gaps

- Merge order: **3A → 3B → 3C** (3C's completion consumes 3B eligibility; all rely on 3A for the
  real student flow). Regenerate OpenAPI/TS after each merge (§2.3). After 3A merges, the
  integrator adds a cross-slice test (admission → student → registration → progression →
  completion/exit) in `apps/api/tests/db_b3_integration.rs`.
- Session pinning to one institution is not enforced for Domain C (tenant-wide staff admin, as
  Batch 2). Guardians are data in Batch 3, not a sign-in role (no `guardian` role yet).
- Person (Platform A02) does not exist: `student.person_id` / guardian person ids stay NULL;
  duplicate detection (3A) works on name + DOB + contact.
- Domain D offerings/sections, Finance (F) and Exams (I) data are stubbed per §6.3.
- `ApprovalMode::AwaitDecision` is wired but unused in production until the Flowable adapter lands
  (Batch 9).
- Institution status is checked through the reference port without a cross-schema row lock (a
  concurrent institution suspension may interleave with one in-flight Domain C write).
