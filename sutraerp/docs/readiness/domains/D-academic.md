# Domain D — Academic delivery (modules/academic) · Part 1 Stage 1 readiness inventory

Base: `part1/readiness` = `erp_merge` fb205860 · CI evidence: run 38724169019 (all green; DB tests
in job "Rust integration", `apps/web/e2e/academic.e2e.mjs` in job "E2E academic (preview)" against
the real API + PostgreSQL). Scope: L2-D01 Offerings, D02 Faculty assignment, D03 Timetable, D04
Teaching session & attendance, D05 Monitoring, D06 Open electives & MOOCs, D07 Internal
assessment. Rows: [`D-academic.csv`](D-academic.csv). Method: code reading + CI logs only. Customer
validation is `pending` everywhere.

## Summary

| Status | Rows |
|---|---|
| implemented | 28 |
| partial | 16 |
| mock-only | 0 |
| missing | 1 (D-044 D06 analytics/compliance reporting W75–W82) |
| deferred | 0 |
| **total** | **45** |

| Release readiness | Rows |
|---|---|
| ready | 11 |
| ready-with-gaps | 30 |
| not-ready | 3 (D-002 offering approval, D-024 attendance correction, D-034 coursework marking) |
| deferred | 1 |

API-tested passed: 44/45 (all except D-044). Browser-tested passed: 30/45.

### Attendance correction & persistence: what was checked
- Persistence is sound. Each mark is its own row: `academic.attendance_record` with
  `UNIQUE (session_id, student_id)` and a no-delete trigger
  (`migrations/20261025000011_academic_sessions_attendance.sql:44-65`). Capture rejects duplicate
  students and students who are not seated (`attendance/repo.rs:646-700`). Finalisation requires a
  mark for every seated student (`require_complete`, l.740-758) and re-evaluates thresholds in the
  same transaction (l.864). Corrections keep the original fact in an immutable
  `academic.attendance_audit` (trigger, migration l.94-105). Only one open correction is allowed
  per record (`attendance_correction_one_open`). A stale `from_mark` is rejected (repo.rs:1191-1197).
- **Gaps:** corrections are applied without approval (DEF-D-002). There is no UI for them
  (DEF-D-003). The `Submitted` state can be skipped (DEF-D-004). The DB does not stop an UPDATE to
  a finalised record (DEF-D-009).
- Enrolment eligibility (`offering/enrolment.rs:349-547`): checks, in order, offering accepts
  enrolment → section active → active student context (or cross-institution permit) → programme /
  min-term / cross-campus restrictions → open window → seat (capacity is never overridden) → 3B
  registration covers the component. Overrides need `offering.enrolment.manage` plus a reason.
  **Gaps:** no prerequisite check (DEF-C-005); overrides are auto-approved (DEF-D-001) and would
  dead-end under AwaitDecision (DEF-D-005).

## DEFECTS

### DEF-D-001 · P1 · Every Domain D approval is auto-approved in production, Flowable mode included
- Evidence: `modules/academic/src/lib.rs:90` (`AcademicDeps::into_state` hard-codes
  `ApprovalMode::AutoApprove`), `apps/api/src/app.rs:855`,
  `modules/academic/src/shared/approval.rs:153`, `docs/batch9-contracts.md:270-272` (Flowable gets
  `autoApprove=true`).
- Affects: offering approval and revisions, enrolment eligibility override
  (`offering/enrolment.rs:519-539`), faculty assignment and its policy, timetable approval,
  attendance policy, attendance correction (DEF-D-002), assessment finalisation, flexible-learning
  provider/opportunity/policy, credit mapping and recognition.
- Root cause and fix: same as DEF-C-001.

### DEF-D-002 · P1 · Faculty can change finalised attendance on their own; corrections apply instantly as "system"
- Evidence: `attendance/repo.rs:1228-1327` (`request_correction`). It admits assigned faculty
  through `require_teaching(…, Permission::AttendanceCorrect)` (l.1238; `shared/faculty.rs:26-50`
  falls back to `academic.teach`). It then applies the correction in the same request with
  `Actor::system` (l.1312-1322), because of DEF-D-001.
- `AttendanceCorrect` requires MFA step-up (`crates/kernel/src/auth.rs:592`), but the faculty path
  never checks that permission, so step-up is bypassed too. `decide_correction` (l.1329-1359) never
  compares the decider with `requested_by`.
- The test codifies this: `apps/api/tests/db_b4b.rs:1016-1018` expects a faculty-raised correction
  to return `status = "applied"`.
- Impact: official attendance feeds threshold evaluation (`evaluate_section`, l.1224) and exam
  eligibility. A teacher can silently move a student across the threshold.
- Fix: keep faculty-raised corrections `requested`; require a different actor with
  `attendance.correct` + step-up to decide; update the test.

### DEF-D-003 · P2 · No UI to request or decide attendance corrections, though the UI says one exists
- Evidence: `apps/web/app/features/academic/teaching/attendance.tsx:105` tells faculty "changes
  need a correction request". No apps/web module calls `/sessions/{id}/attendance-corrections` or
  `/attendance-corrections/{id}/{decision,withdraw}` (`features/academic/delivery/api.ts`).
- Fix: add a correction drawer to faculty Attendance and an admin decision queue.

### DEF-D-004 · P3 · Attendance can skip `Submitted`; the optional approval stage is not supported
- Evidence: `attendance/domain.rs:44-51` (`can_finalise` accepts `Capturing`). The UI never calls
  `submit-attendance` (`teaching/attendance.tsx:108-114`). L2-D04 §23 defines Recorded → Submitted
  → Approval (optional, configuration-driven) → Finalised.
- Fix: require `Submitted` before finalisation. Add an institution setting for register approval.

### DEF-D-005 · P2 · Enrolment eligibility override dead-ends once approvals are human
- Evidence: `offering/enrolment.rs:519-539` returns 409 "the eligibility override awaits a
  decision". Process `academic.enrolment_eligibility_override` (`shared/approval.rs:40`) is used
  nowhere else. There is no decision endpoint and no pending record to resume from.
- Fix: persist a pending override request with a decision endpoint that seats the student when
  approved.

### DEF-D-006 · P3 · The faculty "Submit attendance" button is a non-atomic four-call chain
- Evidence: `teaching/attendance.tsx:108-114` runs start → capture → complete → finalise as
  separate requests. If one fails midway (e.g. a student is still unmarked), the session is left
  `completed` with an unfinalised register. The session is also marked completed at submit time,
  whatever the real end time.
- Fix: a server-side "submit register" command, or a resume-aware UI.

### DEF-D-007 · P2 · Faculty cannot mark or moderate coursework in the browser
- Evidence: no apps/web caller of `/assessment-windows/{id}/submissions` (GET/POST),
  `/…/submissions/{sid}/mark`, `/…/moderation*` or `/internal-assessment-results/{id}/corrections*`.
  Students can submit (`academic.e2e.mjs` "the essay moved to Submitted"), but no screen lets
  anyone mark the work. Backend: `assessment/repo.rs`.
- Fix: add a submissions/marking view to faculty Assessments (`routes/faculty/assessments.tsx`).

### DEF-D-008 · P2 · Large parts of 4A/4B governance are API-only (no UI)
- Evidence: no apps/web caller for offering windows, restrictions, quotas, revisions, merge/split,
  reorganisations, templates, simulations, `/sections/{id}/{activate,cancel}`,
  `/enrolments/{id}/transfer`, `/offerings/{id}/waitlist/{eid}/{accept,remove}`,
  `/me/waitlist/{id}/accept`, faculty assignment changes, faculty policy decision/retire,
  timetable slot add/remove, syllabus coverage writes, monitoring exceptions and reopen. Compare
  `features/academic/offering/api.ts:60-100` and `features/academic/delivery/api.ts` with
  `openapi/openapi.json`.
- Notable: a section added after enrolment opens stays `planned` (`offering/lifecycle.rs:1052-1059`
  activates only at open-enrolment) and cannot be activated from the UI.
- Fix: build the screens, or descope them explicitly.

### DEF-D-009 · P3 · Finalised attendance is protected only by application code; pre-finalisation edits lose per-mark history
- Evidence: the migration (`20261025000011…sql:63-65`) forbids DELETE on `attendance_record` but
  not UPDATE. Before finalisation, `capture` overwrites the mark in place (`repo.rs:704-706`). The
  audit row stores only a count (`{"marks": n}`, l.732).
- Fix: a trigger that blocks mark updates when the session is `finalised`, except through the
  correction function. Optionally store per-mark diffs in the capture audit.

### DEF-D-010 · P3 · Corrections are accepted on locked or archived sessions
- Evidence: `attendance/repo.rs:1246-1251` checks only `attendance_status = finalised`, not the
  session status (`Locked`/`Archived`).
- Fix: decide the policy (L2-D04 §21.8 "Locked"). Block corrections, or require elevated approval
  after lock.

## TEST GAPS
- `db_b4b.rs::sessions_attendance_thresholds_corrections_and_syllabus` asserts the auto-applied
  faculty correction (l.1016-1018). No test covers: a pending correction, a requester deciding
  their own correction, step-up enforcement on corrections, or corrections on locked sessions.
- No test of an enrolment override under `AwaitDecision` (DEF-D-005). The
  `approval_steps_await_a_decision` tests do not include it.
- No test covers concurrent capture by two faculty on the same session (row lock + If-Match exist;
  untested).
- Browser coverage is missing for: offering suspend/cancel/archive, sections
  activate/cancel/capacity, windows, restrictions, quotas, revisions, merge/split, templates,
  simulations, waitlist, enrolment transfer, faculty changes/policy, timetable revision/retirement,
  session abandon/lock, corrections, syllabus coverage writes, monitoring exceptions/reopen,
  moderation/marking/corrections, certificate decisions, flexible exceptions.
- No prerequisite tests (the feature is absent: DEF-C-005).

## DOC DISCREPANCIES
- `docs/batches.md:78-82` marks 4A/4B/4C ✅ ("attendance … correction", "internal assessment +
  coursework") although corrections and marking have no UI and every approval is automatic.
- `docs/batch4-contracts.md:616` says AwaitDecision is unused "until the Flowable adapter lands
  (Batch 9)". After Batch 9 it is still unused (`lib.rs:90`).
- `docs/batch4-contracts.md:463-464` lists `TeachingSpacePort` and `StaffDirectory` as stubs.
  `apps/api/src/app.rs:703-704` wires the real `PgTeachingSpaces` / `PgStaffDirectory`.
- L2-D04 §23 attendance lifecycle (Submitted, optional Approval) does not match the
  implementation (`Capturing → Finalised` allowed; no approval stage).
- UI copy `teaching/attendance.tsx:105` promises a correction-request flow that does not exist.
- `docs/security/findings.md` has no entry for system auto-approval or the step-up bypass on
  faculty attendance corrections (DEF-D-001/002).
