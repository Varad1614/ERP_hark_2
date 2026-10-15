# Domain I — Examinations: Part 1 Stage 1 readiness inventory

Scope: `modules/exams` (`sutra-exams`, schema `exams`, `/api/v1/exams`), web `apps/web/app/features/exams`
(Admin → Examinations, Faculty → Assessments, Student → Results), migrations `20261105000000…050`,
L2-I00 … L2-I16 (+ the I02 and I11 amendments). Base: `part1/readiness` = `erp_merge` fb205860,
CI run 38724169019 (every job green). Method: code reading only (no local build), CI logs read through the
GitHub API. "API-tested passed" means a named DB test in `apps/api/tests/db_b6*.rs` passed in that run;
"browser-tested passed" means a step of `apps/web/e2e/exams.e2e.mjs` (job "E2E exams (preview)", green)
drives it on screen. Customer validation is `pending` everywhere. Row data: `I-exams.csv`.

## Summary

| Status | Rows |
|---|---|
| implemented | 25 |
| partial | 10 |
| mock-only | 0 |
| missing | 0 |
| deferred | 1 |
| **total** | **36** |

| Release readiness | Rows |
|---|---|
| ready | 4 (I-014 mark validation, I-015 internal import, I-019 gradebook, I-028 student self-service) |
| ready-with-gaps | 18 |
| not-ready | 13 |
| deferred | 1 (I-011 practical / viva conduct) |

API tested: 34 of the 35 non-deferred rows have passing DB-test evidence; the exception is I-022 (multi-term /
supplementary CGPA). Browser tested: 20 rows. Backend breadth is high (182 distinct routes, 216 operations); the
risk is in **governance** (approvals, maker-checker, data scoping) and in **CGPA/withholding rules**, not in
missing endpoints. 78 of 182 route paths have no caller in the web API layer
(`features/exams/{setup,marks,results}/api.ts`).

Headline: results can be approved and published today with **no human approval and no second person**
(DEF-I-001/002), a withheld result can be released straight into the publishable set (DEF-I-003), students
under malpractice investigation get their results published (DEF-I-004), and CGPA is wrong as soon as a
supplementary attempt is published (DEF-I-005). Result publication is **not release-ready**.

## DEFECTS

### DEF-I-001 — P0 — Every governed examination decision is auto-approved in production
- Evidence: `modules/exams/src/lib.rs:97` hardcodes `approvals: ApprovalMode::AutoApprove` in `into_state`,
  used by the production router (`apps/api/src/app.rs:877-880`, `exams_deps`). `modules/exams/src/shared/approval.rs:132-135`
  turns every `request_approval` into `Approved`; callers then apply the decision in the same transaction as
  `system:auto-approval` (e.g. publication authorisation `modules/exams/src/results/repo.rs:2272-2296`, result
  approval `results/repo.rs:1111-1205`). Even with `SUTRA_WORKFLOW=flowable`, the adapter sets
  `autoApprove=true` unless the caller passes `"autoApprove": false` (`modules/platform/src/workflow_flowable.rs:10`,
  `docs/batch9-contracts.md:270-272`), and no exams call site passes it.
- Impact: `RESULT_APPROVAL`, `RESULT_PUBLICATION`, `RESULT_CORRECTION`, `RESULT_WITHDRAWAL`, `MARKS_APPROVAL`,
  `MODERATION_APPROVAL`, `MALPRACTICE_DECISION`, `REVALUATION_DECISION`, `SESSION_CLOSURE` all complete the moment
  they are requested. One `exam.result.manage` user validating a batch approves it; one `exam.result.publish`
  user authorising a publication authorises it. Post-publication marks corrections take effect immediately.
- Root cause: Batch 6 default was never switched; Batch 9 Flowable wiring left module `ApprovalMode` untouched.
- Fix: derive `ApprovalMode` from config (`AwaitDecision` whenever `SUTRA_WORKFLOW=flowable`, and refuse
  `AutoApprove` in production); pass `autoApprove=false` to the adapter; ship UI for the decision endpoints (DEF-I-016).

### DEF-I-002 — P1 — No segregation of duties / self-approval check on any exam decision
- Evidence: decision handlers check only a permission: `results/http.rs:219` (batch decide), `:547`
  (result-decision decide), `:650`/`:672` (publication authorise/decide), `marks/http.rs:935`/`:1408`
  (`ExamApprove`). No comparison of the decider with the requester / creator / marks enterer exists anywhere in
  `modules/exams/src` (grep for `created_by`/`decided_by` comparisons returns none). The only staff role,
  `Administrator`, holds `MarksEntry`, `MarksManage`, `ResultManage`, `ResultPublish` and `ExamApprove`
  together (`crates/kernel/src/auth.rs:692-699`).
- Impact: under `AwaitDecision` the same person can enter marks, verify, approve final marks, compute,
  validate, approve, authorise and publish results.
- Fix: store the requesting actor on each governed aggregate and reject a decision by the same subject
  (409/403), plus split exam-cell vs controller-of-examinations roles.

### DEF-I-003 — P1 — Releasing a withheld result after batch approval publishes an un-approved recomputation
- Evidence: `modules/exams/src/results/repo.rs:1590-1596` sets `target = Approved` when `release && batch_approved`;
  if the recomputed content differs a **new** result revision is inserted and walked straight to `approved`
  (`results/repo.rs:1615-1630`). `publish_in_tx` publishes every `approved` result of the session
  (`results/repo.rs:2405`). Only `exam.result.manage` is required (`results/http.rs:374`).
- Impact: changed marks/grades reach students without verification or approval (bypasses RESULT_APPROVAL even
  when AwaitDecision is on).
- Fix: when the recomputed content changes, land in `computed` and require a result-decision
  (RESULT_CORRECTION-style) approval; only an unchanged release may return to `approved`.

### DEF-I-004 — P1 — Pending malpractice cases do not withhold the candidate's result
- Evidence: `decided_penalties` counts only `status IN ('decided','upheld','modified','closed','archived')`
  (`results/repo.rs:313-317`); `compute_candidate` withholds only for missing / non-final marks
  (`results/domain.rs:168-191`). No query for `reported | under_investigation | hearing` cases exists in
  `results/`. Contract (`docs/batch6-contracts.md` §1, 6C) lists "pending malpractice" as a withheld condition.
- Impact: a candidate under investigation is computed as Pass and published; a later debarment needs a
  post-publication withdrawal.
- Fix: in `compute_in_tx`, add a withheld reason when an open (`is_open`, `integrity/domain.rs:45`) case exists
  for the registration; add a DB test.

### DEF-I-005 — P1 — CGPA / completion use only the latest-published result per academic term
- Evidence: `cgpa_with` selects `DISTINCT ON (academic_term_id) … ORDER BY academic_term_id, published_at DESC`
  (`results/repo.rs:347-351`); completion does the same (`completion/repo.rs:184-188`). A supplementary /
  improvement session is a separate `exam_session` whose `academic_term_id` is chosen freely
  (`setup/session.rs`, kinds in `migrations/20261105000000_exams_batch6_foundation.sql:76-77`); its result
  carries only the re-attempted papers.
- Impact: if the supplementary session uses the original term, the supplementary result **replaces** the whole
  regular term (credits of passed regular papers vanish from CGPA and from completion credits). If it uses another
  term, the failed attempt's credits stay in "attempted" with 0 points and the pass is added again (double count).
  `best_percent` is computed for eligibility (`revaluation/repo.rs:1184-1198`) but never feeds CGPA.
- Tests: none — `db_b6c.rs:228` only asserts `cgpa == sgpa` for one term; e2e checks only the number format.
- Fix: compute CGPA per curriculum component (best / latest passing attempt per policy) across all published
  results; add multi-term + supplementary DB tests.

### DEF-I-006 — P1 — Faculty can read every unpublished result and every malpractice case in the tenant
- Evidence: `result_list`/`result_get` require only `ExamRead` and apply no section scoping
  (`results/http.rs:295`, `:321`); likewise `case_list`/`case_get` (`integrity/http.rs:258-264`, `:311-316`) and
  batch reads (`results/http.rs:98`, `:140`). `Role::Faculty` holds `ExamRead` (`crates/kernel/src/auth.rs:744`).
  Marks reads, by contrast, are scoped by `authorize_paper` (`marks/http.rs:49-74`).
- Impact: pre-publication results (computed / withheld, with reasons) and malpractice allegations are visible to
  all faculty — a confidentiality breach for students.
- Fix: restrict these reads to `MarksManage`/`ResultManage`, or scope faculty to papers they teach.

### DEF-I-007 — P1 — Exam conduct, invigilation, seating publication and exceptions have no UI
- Evidence: `features/exams/setup/api.ts` defines no path for `/exam-sittings/{id}/conduct`, `/conduct-records/*`,
  `/exam-papers/{id}/conducted`, `/exam-sittings/{id}/invigilation`, `/invigilation-duties/*`,
  `/seating-plans/{id}/publish|revise`, `/exam-exceptions/*`; the timetable tab only offers "Generate seating"
  (`features/exams/setup/index.tsx:337`). The e2e does conduct through the API (`apps/web/e2e/exams.e2e.mjs:400-403`).
  Opening a mark sheet requires a conducted paper (`marks/http.rs:730-731`).
- Impact: an exam cell using only the product cannot record attendance or mark papers conducted, so marks entry
  cannot start; students never see seat numbers; invigilator duties cannot be managed.
- Fix: add Conduct, Invigilation and Exceptions panels (and seating publish) to Admin → Examinations.

### DEF-I-008 — P2 — Pass/fail is decided on a percentage already rounded to 2 dp; rounding not configurable
- Evidence: `paper_percent` returns `round2(...)` (`shared/grading.rs:79`) and `grade_line` compares the rounded
  value with `pass_percent` and band minimums (`shared/grading.rs:100-101`). `round2` is half-away-from-zero
  (`shared/grading.rs:24-28`). SGPA/CGPA round to 2 dp (`:148`, `:161`).
- Impact: 39.995 % passes a 40 % paper and can move a grade band; many university ordinances truncate or round
  marks (not percentages) to integers. Not wrong per contract §3.5, but a policy decision hidden in code.
- Fix: make mark/percent rounding (truncate vs round, integer vs 2 dp) part of the examination policy; compare on
  the unrounded value unless policy says otherwise; document in the grade card.

### DEF-I-009 — P2 — Publication `scheduled_on` / `delayed_until` are never enforced
- Evidence: `scheduled_on` is only inserted (`results/repo.rs:2199-2209`); `publish_publication` checks only
  status (`results/repo.rs:2554-2562`). No sweep job publishes on the scheduled date.
- Fix: refuse publish before `scheduled_on`/`delayed_until` (or add a scheduled publisher job).

### DEF-I-010 — P2 — Publication authorisation is not bound to a result snapshot
- Evidence: `publish_in_tx` publishes whatever is `approved` at publish time (`results/repo.rs:2405`), including
  results approved after authorisation (e.g. via DEF-I-003). `exams.publication_item` exists but the authorised
  set is not frozen.
- Fix: freeze the result ids at authorisation; publish only those (new approvals need a new publication).

### DEF-I-011 — P2 — Hall-ticket eligibility fails open when attendance data is missing
- Evidence: `registration/hall_ticket.rs:196-204` skips papers without an offering and `continue`s when
  `attendance_standing` returns `None`; the ticket is then issued as eligible.
- Fix: treat missing attendance as unsatisfied (or a policy switch), and record it in the evidence.

### DEF-I-012 — P2 — Moderation bounds are hardcoded, not taken from the examination policy
- Evidence: `marks/domain.rs:145-163` (grace ≤ 10 % of max, scaling 50–120 %); `setup/policy.rs` has no moderation
  fields. Docs say "moderation under policy" (`docs/batch6-contracts.md` §1 6B).
- Fix: move bounds into policy rules; validate against the active policy version.

### DEF-I-013 — P3 — State machines allow shortcuts
- Evidence: `results/domain.rs:30` allows `Pending → Computed` (skips aggregation states); `results/domain.rs:73,84`
  allow `Authorised → Published` (skips `Ready`) and `Failed → Published` without re-authorisation.
- Fix: confirm against L2-I09/I10 §20; require re-authorisation after a failure that changed the result set.

### DEF-I-014 — P3 — Results cross into Domain C as `f64`, with silent 0.0 on conversion failure
- Evidence: `modules/exams/src/results_port.rs:43-44` (`to_f64().unwrap_or_default()`); `crates/contracts/src/exams/results.rs:31-34`.
- Fix: move the frozen contract to decimal strings in the next contract revision.

### DEF-I-015 — P3 — By-id statements without a tenant predicate; no RLS
- Evidence: `registration/hall_ticket.rs:628,702,746`; `scheduling/schedule.rs:631,991,1486`; `scheduling/seating.rs:219,436`;
  `scheduling/conduct.rs:445,843`; `registration/base.rs:136`; `shared/results.rs:202`. Each runs after a
  tenant-checked load in the same transaction, so not exploitable today; no `ENABLE ROW LEVEL SECURITY` in the
  exams migrations.
- Fix: add `AND tenant_id = $n` for defence in depth (or RLS).

### DEF-I-016 — P2 — Governed decision and correction endpoints are unreachable from the UI
- Evidence: no web caller for `/exam-policies/{id}/decision|supersede`, `/exam-sessions/{id}/decision|merge`,
  `/exam-schedules/{id}/decision`, `/exam-registrations/{id}/decision|amend|arrangements|scope-sync`,
  `/registration-overrides/{id}/decision`, `/moderations/{id}/decision`, `/result-batches/{id}/reopen`,
  `/result-decisions/{id}/decision`, `/exam-sessions/{id}/closure/decision`, `/supplementary-eligibility/{id}/authorise`,
  `/grade-cards/{id}/reissue|withdraw`, `/revaluation-requests/{id}/correct` (path scan of `features/exams/*/api.ts`).
- Impact: harmless only while DEF-I-001 auto-approves; once fixed, these workflows stall without a screen.
- Fix: add an approvals inbox / decision buttons before switching to AwaitDecision.

## TEST GAPS

- **Multi-term / supplementary CGPA**: no test computes CGPA over two terms or after a supplementary result
  (`db_b6c.rs:226-228` single term only). Required before DEF-I-005 is fixed.
- **Malpractice pending → withheld**: `db_b6_integration::decided_malpractice_feeds_the_result_outcome` covers decided /
  overturned / reopened cases, never a pending case at compute time.
- **Release after approval**: no test asserts that a released result with changed content needs approval (DEF-I-003).
- **Maker-checker**: no test tries a decision by the requester; `tests/api.rs::non_admin_roles_are_forbidden_to_mutate_but_may_read`
  confirms faculty may read (DEF-I-006 is by design, untested as a privacy rule).
- **Rounding boundaries**: unit tests cover typical values; no test at .xx5 boundaries or pass-mark edge (DEF-I-008).
- **Scheduled publication**: no test for `scheduled_on` in the future.
- **Browser**: e2e has no step for malpractice, moderation, withheld/release, correction/withdrawal, supplementary,
  completion or closure; conduct is done through the API. All Batch 6 approvals in e2e run auto-approved.
- Endpoints with no DB-test reference found by path scan (helpers may hide some): `exam-papers/{id}/papers/generate`,
  `exam-sittings/{id}/venues/{venueId}/release`, `grade-cards/{id}/withdraw`, `hall-tickets/{id}/withdraw`,
  `invigilation-duties/{id}/confirm|release`.

## DOC DISCREPANCIES

- `sutraerp/README.md:43` still labels `exams/ (I)` and `resources/ (J)` as "← scaffolds"; there is **no Batch 6
  section** in the README (sections jump from Batch 4 to Batch 8, `README.md:359,414`).
- `docs/batch6-contracts.md` §1 (6C) lists "pending malpractice" as a withheld condition — not implemented (DEF-I-004).
- `docs/batch6-contracts.md` §1 (6B) says moderation is "under policy" — bounds are constants (DEF-I-012).
- `docs/batch9-contracts.md:263-276` presents Flowable as the approval runtime, but module `ApprovalMode` is still
  `AutoApprove`, so human tasks are never created for Domain I (DEF-I-001). `docs/security/findings.md` does not list
  the auto-approval or the missing maker-checker.
- `modules/exams/src/lib.rs:67` / `shared/approval.rs:19` comments describe auto-approval as a Batch 5/6 interim;
  still the production behaviour after Batch 9.
- Module header `shared/grading.rs:6` says the ten-point rules are "the default until 6A's examination policy makes
  them configurable" — the policy never made them configurable.
- L2-I06 (practical/viva) and most of L2-I15 are deferred to a "6D" that is not scheduled in `docs/batches.md`.
