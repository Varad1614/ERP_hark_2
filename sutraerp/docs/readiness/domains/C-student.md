# Domain C — Student (modules/student) · Part 1 Stage 1 readiness inventory

Base: `part1/readiness` = `erp_merge` fb205860 · CI evidence: run 38724169019 (all jobs green; job
"Rust integration" ran `cargo test -p sutra-api --features db-tests`, job "E2E student (preview)" ran
`apps/web/e2e/student.e2e.mjs` against the real API + PostgreSQL with `VITE_AUTH_MODE=api`).
Scope: L2-C01 Admissions, C02 Lifecycle & activation, C03 Registration, C04 Progression, C05
Completion & exit, C06 Guardian, C07 Scholarship, C08 Alumni. Rows: [`C-student.csv`](C-student.csv).
Method: code reading + CI logs only (no local build). Customer validation is `pending` for every row.

## Summary

| Status | Rows |
|---|---|
| implemented | 30 |
| partial | 18 |
| mock-only | 2 |
| missing | 0 |
| deferred | 0 |
| **total** | **50** |

| Release readiness | Rows |
|---|---|
| ready | 20 |
| ready-with-gaps | 23 |
| not-ready | 5 (C-004 admission approval, C-019 duplicate resolution, C-026 registration approval, C-033 credit transfer, C-034 programme migration) |
| deferred | 2 (C-037 ABC sync, C-049 external scholarship sync) |

API-tested passed: 50/50 rows have a named DB test that passed in CI. Browser-tested passed: 26/50.

Headline: the backend is broad and well tested, but **every governed decision in Domain C is
auto-approved in every deployment** (DEF-C-001). Most rows marked `partial` are partial only for
that reason, or because the workflow has no UI.

### Admission state machine: what was checked
- `admission_can_transition` (`modules/student/src/admission/domain.rs:35-58`) follows the L2-C01
  §6.9.1 happy path with no skips. Verification and approval cannot be bypassed: unit test
  `admission_machine_follows_l2`, and every command goes through `transition()`
  (`flow.rs:280-358`), which re-checks the move under `FOR UPDATE` + `If-Match` + a version
  predicate. Cancel is blocked after confirmation, withdraw is allowed only from `confirmed`,
  expiry only from an un-accepted `initiated` offer, and history rows are immutable (DB trigger).
- Duplicate admissions: one admission per application (`admission_application_unique`), and one
  open admission per *existing* student per institution (`admission_open_per_student`,
  migration `20261020000001_student_admissions.sql:94-96`). **Gaps:** new applicants with no
  `student_id` are not protected (DEF-C-002, DEF-C-003).
- Enrolment (`record/student.rs:441-530`): only a `pending_activation` context can be enrolled. It
  completes the admission behind the context. Section-level enrolment eligibility is covered in
  Domain D (D-008).

## DEFECTS

### DEF-C-001 · P1 · Every Domain C approval is auto-approved in production, Flowable mode included
- Evidence: `modules/student/src/lib.rs:87` (`StudentDeps::into_state` hard-codes
  `ApprovalMode::AutoApprove`), `apps/api/src/app.rs:851` (production router uses `into_state`),
  `modules/student/src/shared/approval.rs:140` (AutoApprove → `Approved`),
  `modules/student/src/admission/flow.rs:1052-1062` (the admission is approved in the same
  transaction by `system:auto-approval`), `apps/api/src/config_b9.rs:222` and
  `docs/batch9-contracts.md:270-272` (the Flowable adapter sends `autoApprove=true` unless the
  caller sends `false`, and no module does). The browser shows it too: `apps/web/e2e/student.e2e.mjs`
  goes from "Send for approval" straight to "Approved".
- Affects 21 process keys (`shared/approval.rs:29-55`): admission approval and revocation,
  suspension, transfer, identity correction, duplicate resolution, archival, registration approval,
  progression decision/revision/policy, credit transfer, programme migration, academic break,
  qualification completion/revocation, exit, guardian verification, scholarship
  policy/recommendation, alumni transition.
- Root cause: Batch 3 placeholder ("AwaitDecision unused until Flowable lands") was never switched
  when Flowable shipped in Batch 9.
- Fix: derive `ApprovalMode` from config (`SUTRA_WORKFLOW=flowable` → `AwaitDecision`, or set it per
  process key); have the modules pass `autoApprove:false`; add a production-config test proving
  that approvals stay pending.

### DEF-C-002 · P2 · Repeated readmission creates several open admissions for one applicant
- Evidence: `modules/student/src/admission/flow.rs:1669-1700`. The only duplicate guard is
  `if let Some(sid) = src.student_id` (l.1691). The unique index
  `20261020000001_student_admissions.sql:94-96` applies only when `student_id IS NOT NULL`. Nothing
  checks whether the source admission was already readmitted (`previous_admission_id`).
- Result: a cancelled, withdrawn or expired admission with no student yet can be readmitted again
  and again, each time with a new Idempotency-Key. Each call opens a new `initiated` admission for
  the same person, which can lead to two students.
- Fix: unique partial index on `previous_admission_id` over open statuses, plus a person-key
  (name + DOB + contact) check on readmission.

### DEF-C-003 · P2 · No duplicate-applicant detection at intake or offer; it happens late, at student creation, and only by login
- Evidence: `admission/application.rs:258` (`create`) and `:766-860` (`offer`) do no duplicate
  lookup. `record/creation.rs:91-106` checks only `subject_ref` (login), and only after approval
  and financial clearance. Name + DOB + contact matching exists only as a read endpoint on an
  existing student (`record/student.rs:1447`, `duplicate_candidates`).
- L2-C01 l.924 says student creation "shall never duplicate an existing institutional Student
  identity". `docs/batch3-contracts.md:710` claims detection "works on name + DOB + contact".
- Fix: run the candidate query at application submit, offer and `create_student`. Block the step,
  or route it to a duplicate case.

### DEF-C-004 · P2 · Duplicate resolution has no UI
- Evidence: the path table in `apps/web/app/features/student/record/api.ts:66-83` (and the whole of
  apps/web) has no caller of `/duplicate-cases*` or `/students/{id}/duplicate-candidates`.
  Backend: `record/identity.rs:631+`.
- Fix: add a Duplicates tab to Student 360 (candidates → case → submit → decision).

### DEF-C-005 · P2 · Course prerequisites and co-requisites are not validated anywhere
- Evidence: `registration/domain.rs:74-160` (`validate_selection`) checks curriculum membership,
  term, backlog/repeat rules and the credit limit only. Section enrolment
  (`academic/src/offering/enrolment.rs:349-547`) has no prerequisite gate either. No migration
  defines prerequisite data (`grep -i prerequisite migrations/` → none).
- L2-C03 l.859: "Prerequisites and co-requisites shall be validated before approval."
- Fix: add a prerequisite/co-requisite model to the curriculum (Domain B). Check it in
  `validate_selection`, with an override reason.

### DEF-C-006 · P2 · No maker-checker on Domain C decision endpoints
- Evidence: `admission/flow.rs:1081-1112` (`decide`) and `:926-995` (`apply_decision`) never
  compare the decider with the requester. The same holds for the other C decision endpoints. Domain
  D does enforce it in places (`academic/src/assessment/repo.rs:1324`,
  `academic/src/monitoring/repo.rs:1053`).
- Latent today because of DEF-C-001. It becomes live as soon as approvals are human.
- Fix: store `requested_by` and reject a decision by the same subject.

### DEF-C-007 · P2 · Governed workflows that exist only in the API (no UI)
- Evidence: no apps/web caller for `/credit-transfers*`, `/programme-migrations*`,
  `/academic-breaks/{id}/{start,decision,withdraw}`, `/conferred-qualifications/{id}/{corrections,revoke}`,
  `/students/{id}/financial-responsibilities`, `/guardian-relationships/{id}/consents/{cid}/withdraw`,
  `/…/communications/{cid}/revoke`, `/transfers/{id}/{decision,withdraw}`,
  `/identity-corrections/{id}/{decision,withdraw}`, `/registrations/{id}/decision`,
  `/progression-decisions/{id}/{decision,revisions}`.
  Compare `features/student/registration/api.ts`, `features/student/completion/api.ts:118-160`
  and `features/student/record/api.ts:66-120` with `openapi/openapi.json`.
- Fix: add the screens, or explicitly descope them for release.

### DEF-C-008 · P3 · Inconsistent and duplicated admission state logic
- Evidence: `admission/domain.rs:62-65` treats `Completed` as terminal ("no further transition"),
  yet `:56` allows `Completed → Revoked`. The web client re-implements the machine in
  `features/student/record/api.ts:286-312`. It does not offer cancel from `approval_pending`, which
  the backend allows (`domain.rs:47-53`), and does not offer readmit from `completed`, which the
  backend allows (`domain.rs:72`).
- Fix: drop `Completed` from `admission_is_terminal` (or rename the helper). Expose the allowed
  actions from the API instead of duplicating them in TS.

### DEF-C-009 · P3 · Offer expiry runs only when an admin presses a button
- Evidence: `admission/flow.rs:1599` (`expire_offers`) is reachable only through the
  "Expire lapsed offers" button (`features/student/record/AdminTabs.tsx:398`). No sweep job is
  registered (no other reference in `apps/api/src`).
- Lapsed offers stay `initiated`; acceptance is still blocked by date (`flow.rs:586-588`).
- Fix: register a daily `SweepJob` per institution.

### DEF-C-010 · P2 · Staff administration is tenant-wide: no institution-level authorisation
- Evidence: `modules/student/src/shared/scope.rs:1-14` ("Staff administration is tenant-wide …
  Session pinning to one institution is later hardening"). `prepare` (`admission/flow.rs:365-377`)
  takes the institution from the record, not from the caller. List endpoints accept any
  institution id in the tenant (`flow.rs:439`).
- In a multi-college tenant, an admin of one college can admit, revoke or archive students of
  another. Documented in `docs/batch3-contracts.md` §12 but not in `docs/security/findings.md`.
- Fix: carry institution grants in `AuthContext`, enforce them in `institution_scope`, and add the
  item to the security register.

### DEF-C-011 · P3 · Child-row SQL without a tenant predicate (defence in depth)
- Evidence: e.g. `admission/flow.rs:590, 935, 1163, 1267, 1466`, `admission/application.rs:857`,
  `registration/repo.rs:965`, `progression/repo.rs:890`. These are safe today because each runs
  after a tenant-filtered lock of the parent. A scan finds 50+ such statements in modules/student.
  There is no RLS (SEC-9D-004).
- Fix: add `AND tenant_id = $n` to these statements, or apply the RLS plan in SEC-9D-004.

### DEF-C-012 · P3 · Offer does not check the intake / academic session precondition
- Evidence: `admission/application.rs:766-830` validates programme, curriculum and expiry only.
  L2-C01 §2.2 (l.1190-1197) requires that "Intake exists" and "Academic Session exists".
- Fix: validate the admission term/year (`refs::term`) on offer, as readmission already does
  (`flow.rs:1727`).

## TEST GAPS
- No test readmits the same no-student source twice (would expose DEF-C-002). No test submits two
  applications for the same person (DEF-C-003).
- The human approval path is exercised only with a hand-built `AwaitDecision` state
  (`db_b3a.rs::admission_approval_await_decision_and_finance_evidence`,
  `db_b3b.rs::approvals_await_a_decision`, `db_b3c.rs::await_decision_mode_keeps_approvals_pending`).
  No test boots the production `router()` / `SUTRA_WORKFLOW=flowable` and asserts that approvals
  stay pending.
- No test asserts that the decider must differ from the requester (DEF-C-006).
- The full illegal-transition matrix is tested only at unit level (`admission/domain.rs` tests).
  API-level negative cases cover a subset (cancel / withdraw / expiry / readmit conflicts).
- Browser coverage is missing for: cancellation, withdrawal, revocation, expiry, readmission,
  suspension/restoration, transfers, identity correction, duplicates, registration holds,
  probation/detention, academic break, credit transfer, programme migration, guardian
  suspension/termination, conferred-qualification correction/revocation.
- ABC and external scholarship sync are tested only against Noop adapters.
- The e2e suite runs with dev auth only (no OIDC or step-up path for `admission.revoke`,
  `student.archive`, etc.).

## DOC DISCREPANCIES
- `docs/batches.md:61-65` marks 3A/3B/3C ✅ and "Batch 3 is complete". Approvals are auto in every
  environment (DEF-C-001), and several workflows have no UI (DEF-C-004, DEF-C-007).
- `docs/batch3-contracts.md:712` says AwaitDecision is "unused in production until the Flowable
  adapter lands (Batch 9)". Flowable has landed (`app.rs:963-985`), yet approvals still
  auto-complete (`lib.rs:87`; batch9-contracts §7 `autoApprove=true` default).
- `docs/batch3-contracts.md:710` claims duplicate detection "works on name + DOB + contact". It is
  a post-creation read endpoint only (DEF-C-003).
- `docs/batch3-contracts.md:711` says "Domain D offerings/sections, Finance and Exams data are
  stubbed". This is stale: real readers are wired (`apps/api/src/app.rs:677-679`).
- `admission/domain.rs:60` doc comment ("Terminal admissions (no further transition)") contradicts
  `Completed → Revoked`.
- `docs/security/findings.md` has no entry for system auto-approval of governed decisions or for
  tenant-wide institution access (DEF-C-001, DEF-C-010).
