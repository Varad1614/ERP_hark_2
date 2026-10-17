# UAT test cases — institutional pilot

**Status:** cases written against the product as built (`part2/s6-pilot`); **real UAT not executed —
BLOCKED_EXTERNAL** (no authorised institution, no pilot users). Real results go to
[`UAT_RESULTS.md`](UAT_RESULTS.md) only, entered by named institutional testers.

The last column, **“Synthetic dry run (not UAT)”**, records ONLY the automated walk-through of one
key case per role with synthetic data (`apps/web/e2e/uat-dryrun.e2e.mjs`, step “UAT dry run” of
`.github/workflows/sutraerp-part1-demo.yml`, full compose stack, Keycloak OIDC + TOTP, human
approval mode, on the journey's synthetic institution). It proves the case is executable; it is
not acceptance evidence. Other cases show “—” (not walked by the dry run).

Every case names a real screen (web route in `apps/web/app/routes.ts`) and API endpoint
(`openapi/openapi.json`). Roles are the product's roles (`crates/kernel/src/auth.rs`): there is no
separate “accounts staff” or “examination staff” role — their maker work uses the administrator
role; their decisions use `finance_approver` / `exam_controller` (see PILOT_SCOPE.md §7).

## Administrator

| ID | Role (persona) | Preconditions | Steps | Expected result | Screen · API | Synthetic dry run (not UAT) |
|---|---|---|---|---|---|---|
| UAT-ADM-01 | administrator | institution active; ≥1 enrolled student | 1 Open Students → the student → Enrolment tab. 2 Check name, programme, enrolment/roll number, status. | Student 360 shows the student with an **active** context in the institution; values match the admission/migration source | `/students/record/:id/enrolment` · `GET /api/v1/student/students/{id}` | PASS (synthetic) — run 38945114014 @ cf4c3297: Student 360 GET 200, name shown, DB student+context `active|active` |
| UAT-ADM-02 | administrator (admissions office) → registrar | programme + admission term configured; registrar account | 1 New application (`/students/admissions`). 2 Submit → review → offer (required documents) → accept. 3 Upload + verify documents. 4 Submit for approval. 5 Registrar decides. 6 Financial clearance → confirm → create student. | Application ends `student_created`; the decision is recorded with the **registrar** as decider; the administrator's own decision is refused (403) | `/students/admissions` · `POST /api/v1/student/institutions/{id}/admission-applications`, `…/admission-applications/{id}/submit\|review\|offer`, `POST /api/v1/student/admissions/{id}/decision` | — (not walked by the dry run) |
| UAT-ADM-03 | administrator | admitted student pending activation | Student 360 → Enrolment → Complete enrolment (roll no., batch, section). | Context `active`; roll number stored | `/students/record/:id/enrolment` · `POST /api/v1/student/students/{id}/enrol` | — (not walked by the dry run) |
| UAT-ADM-04 | administrator → second administrator | registration window for the term | Schedule + open the registration window; register a student by office registration; the second administrator decides. | Registration `registered`; requester cannot decide (403) | `/students/registration` · `POST /api/v1/student/registrations/{id}/decision` | — (not walked by the dry run) |
| UAT-ADM-05 | administrator | institutional rules received in writing (PILOT_SCOPE P4) | Configure grading scheme, fee heads/structures, attendance threshold, academic calendar; publish through their approvals. | Configuration matches the signed rules sheet item by item | `/academics/:tab`, `/examinations/sessions`, `/finance/charges`, `/academic-delivery/attendance` · `POST /api/v1/exams/institutions/{id}/grading-schemes`, `POST /api/v1/finance/institutions/{id}/fee-structures`, `POST /api/v1/institution/institutions/{id}/calendars` | — (not walked by the dry run) |
| UAT-ADM-06 | student, faculty (negative) | — | A student tries to create an admission application; faculty tries to confirm an admission. | Both refused (403); nothing written | `POST /api/v1/student/institutions/{id}/admission-applications` · `POST /api/v1/student/admissions/{id}/confirm` | — (not walked by the dry run) |

## Faculty

| ID | Role (persona) | Preconditions | Steps | Expected result | Screen · API | Synthetic dry run (not UAT) |
|---|---|---|---|---|---|---|
| UAT-FAC-01 | faculty | faculty assigned to a section with seated students | 1 Attendance → Schedule a session (date, time, topic). 2 Start. 3 Tap a student to mark absent. 4 Submit → confirm. | Session `finalised`; marks stored per student; student sees the session in their attendance | `/attendance` · `POST /api/v1/academic/sections/{id}/sessions`, `POST /api/v1/academic/sessions/{id}/start`, `POST /api/v1/academic/sessions/{id}/finalise-attendance` | PASS (synthetic) — run 38945114014 @ cf4c3297: session scheduled → started → one student marked absent → finalised; DB attendance `absent` |
| UAT-FAC-02 | faculty | finalised session | Try to re-mark a finalised session; request a correction through the correction flow. | Direct re-mark refused (409); correction recorded in the attendance audit | `/attendance` · `POST /api/v1/academic/sessions/{id}/attendance` (409) | — (not walked by the dry run) |
| UAT-FAC-03 | faculty | mark sheet open for the faculty's paper | Assessments → open the paper → enter marks → submit the sheet. | Marks stored; sheet `evaluated`; values outside 0…max refused (422) | `/assessments` · `PUT /api/v1/exams/exam-papers/{id}/marks`, `POST /api/v1/exams/mark-sheets/{id}/submit` | — (not walked by the dry run) |
| UAT-FAC-04 | faculty (negative) | — | Open Fees; try to record a counter payment through the API. | No Fees screen for faculty; payment refused (403) | `/fees` · `POST /api/v1/finance/student-accounts/{id}/payments` (403) | — (not walked by the dry run) |

## Accounts staff

| ID | Role (persona) | Preconditions | Steps | Expected result | Screen · API | Synthetic dry run (not UAT) |
|---|---|---|---|---|---|---|
| UAT-ACC-01 | accounts staff (administrator role) → finance approver | published charge on a student account; finance-approver account | 1 Finance → Adjustments → + Adjustment (charge, concession, amount, reason). 2 Submit. 3 Maker tries to approve. 4 Finance approver signs in → Adjustments → Approve. | Pending until decided; maker refused (403); after approval `posted`, outstanding reduced by the amount, balanced journal, decider = finance approver | `/finance/adjustments` · `POST /api/v1/finance/charges/{id}/adjustments`, `POST /api/v1/finance/adjustments/{id}/submit`, `POST /api/v1/finance/adjustments/{id}/decision` | PASS (synthetic) — run 38945114014 @ cf4c3297: draft → submit (pending, charge untouched) → maker's decision 403 → finance approver posts; DB decided_by = approver, charge −₹2,000, 1 balanced journal |
| UAT-ACC-02 | accounts staff | published fee structure | Fees & charges → fee structure → Generate charges for selected students. | One charge per student, `published`, amount = structure total; balanced publication journal | `/finance/charges` · `POST /api/v1/finance/fee-structures/{id}/generate-charges` | — (not walked by the dry run) |
| UAT-ACC-03 | accounts staff | student with outstanding charge | Collections → Record payment (cash/cheque/UPI ref) → receipt. | Payment `completed`, receipt number issued, allocation reduces outstanding | `/finance/collections` · `POST /api/v1/finance/student-accounts/{id}/payments` | — (not walked by the dry run) |
| UAT-ACC-04 | accounts staff | as ACC-03 | Re-submit the same payment (network retry). | Same payment returned (idempotent); no double charge | `POST /api/v1/finance/student-accounts/{id}/payments` (same Idempotency-Key) | — (not walked by the dry run) |
| UAT-ACC-05 | student | outstanding charge; gateway configured | Fees → Pay online. | **BLOCKED_EXTERNAL** for live money (no Razorpay keys); with test keys: payment completes only after server-side verification | `/fees` · `POST /api/v1/finance/me/payments` | — (not walked by the dry run) |
| UAT-ACC-06 | finance approver | refund requested by the administrator | Approve the refund. | Refund posted; requester cannot decide (403) | `/finance/refunds` · `POST /api/v1/finance/refunds/{id}/decision` | — (not walked by the dry run) |

## Examination staff

| ID | Role (persona) | Preconditions | Steps | Expected result | Screen · API | Synthetic dry run (not UAT) |
|---|---|---|---|---|---|---|
| UAT-EXM-01 | examination staff (administrator role) → controller of examinations | active exam session with a confirmed registration; controller account | 1 Exam cell reports a malpractice case, investigates, records the committee decision (warning). 2 Proposer tries to approve. 3 Controller → Examinations → Malpractice → Approve decision. | Proposal awaits approval; proposer refused (403); after the controller's approval the case is `decided` with the penalty; audit shows the controller | `/examinations/malpractice` · `POST /api/v1/exams/exam-sessions/{id}/malpractice-cases`, `POST /api/v1/exams/malpractice-cases/{id}/investigate`, `…/decision`, `…/approval` | PASS (synthetic) — run 38945114014 @ cf4c3297: case reported → committee decision proposed → proposer's approval 403 (maker ≠ checker) → controller approves → `decided|warning`, approval audited by the controller |
| UAT-EXM-02 | examination staff | session active, rosters exist | Registrations → register from rosters → issue hall tickets. | Registrations confirmed; hall tickets issued only to students with clear dues | `/examinations/registrations` · `POST /api/v1/exams/exam-sessions/{id}/registrations/bulk`, `POST /api/v1/exams/exam-sessions/{id}/hall-tickets/issue` | — (not walked by the dry run) |
| UAT-EXM-03 | examination staff → controller | submitted mark sheet | Marks → verify → request approval; controller approves; finalise. | Sheet `finalised`; maker cannot approve (403) | `/examinations/marks` · `POST /api/v1/exams/mark-sheets/{id}/verify\|request-approval\|decision\|finalise` | — (not walked by the dry run) |
| UAT-EXM-04 | examination staff → controller | finalised marks | Results → open batch → compute → validate; controller approves; finalise; plan publication → authorise; controller decides; publish. | Results `published`; grades per the institution's scheme; maker refused on both decisions (403) | `/examinations/results` · `POST /api/v1/exams/exam-sessions/{id}/result-batches`, `POST /api/v1/exams/result-batches/{id}/compute\|validate\|decision\|finalise`, `POST /api/v1/exams/publications/{id}/authorise\|decision\|publish` | — (not walked by the dry run) |

## Student

| ID | Role (persona) | Preconditions | Steps | Expected result | Screen · API | Synthetic dry run (not UAT) |
|---|---|---|---|---|---|---|
| UAT-STU-01 | student | published result; fee account | 1 Results. 2 Fees. | Own published result with grade; own fee status (dues or “no dues”); nothing of other students | `/results`, `/fees` · `GET /api/v1/exams/me/results`, `GET /api/v1/finance/me/accounts` | PASS (synthetic) — run 38945114014 @ cf4c3297: own published result 200, own fee account 200, fee status shown |
| UAT-STU-02 | student | registration window open | Registration → choose courses → submit. | Registration submitted, then registered after the decision | `/registration` · `POST /api/v1/student/me/registrations` | — (not walked by the dry run) |
| UAT-STU-03 | student | registered; offering enrolment open | Courses → enrol in a section. | Section enrolment `enrolled` | `/courses` · `POST /api/v1/academic/me/enrolments` | — (not walked by the dry run) |
| UAT-STU-04 | student | finalised attendance | Timetable → attendance. | Attended / held counts match the faculty's sessions | `/timetable` · `GET /api/v1/academic/me/attendance` | — (not walked by the dry run) |
| UAT-STU-05 | student | verified admission documents | Documents → download a verified file. | Own verified files only; other students' files refused | `/documents` · `GET /api/v1/student/me/documents` | — (not walked by the dry run) |
| UAT-STU-06 | student (negative) | — | Request another student's record through the API. | 403 | `GET /api/v1/student/students/{id}` (403) | — (not walked by the dry run) |

## Data-migration validation (Stage 4 import)

| ID | Role (persona) | Preconditions | Steps | Expected result | Screen · API | Synthetic dry run (not UAT) |
|---|---|---|---|---|---|---|
| UAT-MIG-01 | administrator | source extract + control totals (PILOT_SCOPE P5) | Data import → Students → download template → upload the extract → check suggested mapping → **dry run**. | Every invalid row listed with line, column and reason; error report CSV downloadable; commit disabled while errors remain; nothing written by the dry run | `/data-import` · `GET /api/v1/platform/imports/entities/{entity}/template`, `POST /api/v1/platform/imports`, `PUT /api/v1/platform/imports/{id}/mapping`, `POST /api/v1/platform/imports/{id}/dry-run`, `GET /api/v1/platform/imports/{id}/errors.csv` | — (not walked by the dry run) |
| UAT-MIG-02 | administrator | MIG-01 corrected file | Dry run clean → commit → open the reconciliation. | **Reconciliation report vs source totals:** created + updated + skipped = rows in source; error rows = 0; record count in SutraERP = registrar's count | `/data-import` · `POST /api/v1/platform/imports/{id}/commit`, `GET /api/v1/platform/imports/{id}` (summary) | — (not walked by the dry run) |
| UAT-MIG-03 | administrator → finance approver | students migrated; outstanding-fees extract with Σ | Opening balances → dry run → Request approval → finance approver decides. | Nothing posted before approval; uploader cannot decide (403); after approval Σ file = Σ committed = Σ ledger = source Σ outstanding (`reconciled`) | `/data-import`, `/data-import/approvals` · `POST /api/v1/platform/imports/{id}/decision` | — (not walked by the dry run) |
| UAT-MIG-04 | administrator | extracts for staff, programmes/curricula, fee heads | Import each entity (dry run → commit). | Each reconciles to its source count; spot-check 10 records per entity against the source | `/data-import` · `/api/v1/platform/imports` (entities: campuses, org units, programmes, curricula, staff, students, historical results, fee heads, opening balances) | — (not walked by the dry run) |
| UAT-MIG-05 | administrator | MIG-02 committed | Re-upload the same file → dry run → commit. | All rows `skip`; no duplicates created | `/data-import` · `POST /api/v1/platform/imports/{id}/dry-run` | — (not walked by the dry run) |
| UAT-MIG-06 | administrator | migrated entity | History → Export the entity; compare with the source extract. | Same records and values (formula-safe CSV); differences explained | `/data-import/history` · `GET /api/v1/platform/imports/entities/{entity}/export` | — (not walked by the dry run) |

## Reports vs institutional records

| ID | Role (persona) | Preconditions | Steps | Expected result | Screen · API | Synthetic dry run (not UAT) |
|---|---|---|---|---|---|---|
| UAT-RPT-01 | registrar + administrator | migration done; registrar's student register for the same date | Compare student strength by programme/term with the register. | Equal counts per programme; differences = 0 or explained | `/students`, `/reports/dashboards` · `GET /api/v1/student/institutions/{id}/students`, `GET /api/v1/services/institutions/{id}/metrics` (`student.active_students`) | — (not walked by the dry run) |
| UAT-RPT-02 | faculty + administrator | one month of attendance in both systems (parallel run) | Compare attendance % per section/student with the paper registers. | Equal percentages; threshold shortlist equals the institution's | `/academic-delivery/attendance` · `GET /api/v1/academic/sections/{id}/attendance-summary`, `POST …/attendance-summary/evaluate` | — (not walked by the dry run) |
| UAT-RPT-03 | accounts staff | collections for a period in both systems | Compare collections by day/mode with the cash book and bank statement. | Totals equal per day and mode | `/finance/collections`, `/finance/ledger` · `GET /api/v1/finance/institutions/{id}/collections`, `GET /api/v1/finance/institutions/{id}/trial-balance` | — (not walked by the dry run) |
| UAT-RPT-04 | accounts staff | same date in both systems | Compare outstanding receivables per student/programme with the accounts ledger. | Equal outstanding totals; ageing buckets consistent | `/finance/receivables` · `GET /api/v1/finance/institutions/{id}/receivables` | — (not walked by the dry run) |
| UAT-RPT-05 | controller of examinations | a published session tabulated by the exam cell | Compare pass %, grade distribution and SGPA list with the exam cell's tabulation. | Identical per paper and per student | `/examinations/results` · `GET /api/v1/exams/exam-sessions/{id}/results` | — (not walked by the dry run) |

## Synthetic dry-run runs

| Run | Head | Result |
|---|---|---|
| [38945114014](https://github.com/Varad1614/ERP_hark_2/actions/runs/38945114014) (“SutraERP Part 1 demo (full stack)”, step “UAT dry run”) | cf4c3297 | **5/5 pass** (UAT-ADM-01, UAT-FAC-01, UAT-ACC-01, UAT-EXM-01, UAT-STU-01); synthetic data, not UAT |
| 38942486947 | 56fc11b6 | 4/5 — UAT-ACC-01 failed on the test's own DB check (expected `t`; PostgreSQL renders a boolean concatenated with `\|\|` as `true`); test fixed in cf4c3297, product behaviour unchanged |
