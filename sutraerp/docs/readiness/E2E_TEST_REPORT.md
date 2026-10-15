# SutraERP Part 1 — end-to-end journey test report

Source: demo workflow run [38750761187](https://github.com/Varad1614/ERP_hark_2/actions/runs/38750761187) on `part1/s2-demo` @ `04674047` (Keycloak / OIDC),
`journey-results.json` in the `part1-demo-logs` artifact. Spec: `apps/web/e2e/journey.e2e.mjs`
(phases `journey` and `persistence`); seed `lib/journey-seed.mjs`; sign-in `lib/auth.mjs`;
approvals `lib/approvals.mjs`; summary renderer `lib/journey-report.mjs`.

**Journey: 25/26 pass, 0 fail, 1 known open defect · Persistence: 6/6 pass.**

## 1. How each step is checked

* **Action** — in Chromium (Playwright) where a screen exists, with the same `data-*` selectors as the
  CI e2e suites; otherwise through the API as the signed-in role (listed in §3).
* **API status** — the response the *page itself* sent is captured (`page.waitForResponse`) and its
  status and body asserted; API-driven steps assert the call's status.
* **DB check** — `psql` as the schema owner against the compose postgres: the rows the step should have
  written (status, amounts, marks, counts).
* **Audit check** — `platform.audit_log` rows for the record (count, action names, actors). Required
  action names are asserted where the step depends on them.
* **Role visibility** — what admin / faculty / student can and cannot read (expected 200 with the data,
  or 403), plus a screenshot of the role's screen.
* Any 5xx or page error seen by a browser during a step fails that step.

## 2. Per-step results

| # | Stage | Step | Driven by | API status (asserted) | DB check | Audit check | Role visibility | Result |
|---|---|---|---|---|---|---|---|---|
| 1 | journey | Sign-in: administrator, faculty and student (API clients) | API · Keycloak OIDC (code + PKCE) | — | — | — | — | ✅ pass |
| 2 | journey | Sign-in: administrator, faculty and student (browser) | browser · Keycloak login page | — | — | — | — | ✅ pass |
| 3 | seed | Synthetic institution through the API (institution, campus, departments, programmes, year, courses, sections, faculty, students, fee structure, exam session) | API | — | institution active: active<br>departments: CSE:active,MATH:active<br>offerings: CS101-T1:enrollment_open,EL101-T1:enrollment_open,MD101-T1:enrollment_open<br>faculty assignment: active\|f49e4eb6-4e67-4ac6-9cb3-62683c5d9040<br>seed students: 2<br>fee structure: published\|90000.00<br>(+1 more) | institution: 5 row(s) — actions [institution.institution.activate,institution.institution.configure,institution.institution.register,institution.institution.rev | — | ✅ pass |
| 4 | 1 Admission | Application → review → offer → acceptance → documents → verification → approval → clearance → confirmation → student | browser (admin /students/admissions) | POST /student/institutions/{id}/admission-applications → 201<br>POST /student/admission-applications/{id}/submit → 200<br>POST /student/admission-applications/{id}/review → 200<br>POST /student/admission-applications/{id}/offer → 200<br>POST /student/admissions/{id}/accept → 200<br>POST /student/admissions/{id}/documents → 201<br>POST /student/admissions/{id}/documents → 201<br>POST /student/admissions/{id}/submit-documents → 200<br>(+6 more) | application: offered<br>admission: student_created\|01a13ff8-159b-7aed-b2c6-d444a2f654cb<br>documents: identity_proof:verified,qualifying_marksheet:verified<br>student: activated\|S202600003\|d572e68f-14a1-4142-a6eb-4fd6f50a2f77 | admission: 13 row(s) — actions [student.admission.accept_offer,student.admission.add_document,student.admission.approve,student.admission.clear_financially,stud | faculty: GET /student/admissions/{id} → 200 (student.read: read-only)<br>student: GET /student/admissions/{id} → 403<br>admin sees the admission linked to the new student (04-1-admission-student-created.png) | ✅ pass |
| 5 | 1 Admission | Negative: wrong role — faculty cannot decide an admission, student cannot create an application (403) | API | faculty POST /admissions/{id}/confirm → 403<br>student POST /institutions/{id}/admission-applications → 403 | no application by the student: 0 | — | — | ✅ pass |
| 6 | 1 Admission | Negative: invalid transition — confirming an admission that already created its student (409) | API | admin POST /admissions/{id}/confirm on student_created → 409 admission is student_created; it cannot become confirmed | admission unchanged: student_created | — | — | ✅ pass |
| 7 | 1 Admission | Negative: duplicate admission — a second application for the same applicant (same login, name, date of birth, e-mail) is refused | API | create duplicate application → 201 <br>duplicate submit → 200 <br>duplicate review → 200 <br>duplicate offer → 200 <br>cleanup: duplicate admission 01a13ff8-18ab-7084-9791-8823b2835cae (initiated) cancel → 200 | applications for this applicant: 2 | — | — | ⚠️ known open defect |
| 8 | 2 Enrolment | Student 360: complete enrolment (pending_activation → active) | browser (admin /students/record/:id/enrolment) | POST /student/students/{id}/enrol → 200 | context: active\|CSE-26-001<br>admission completed: completed<br>student active: active | student: 2 row(s) — actions [student.student.create,student.student.enrol] | faculty: GET /student/students/{id} → 200 (Student 360 read-only)<br>student: GET /student/students/{id} → 403<br>admin sees the active context (05-2-student360-enrolled.png) | ✅ pass |
| 9 | 2 Enrolment | Registration window for term 1 scheduled and opened | API (screen exists: /students/registration — not driven) | — | window: open | registration window: 2 row(s) — actions [student.registration_window.create,student.registration_window.open] | — | ✅ pass |
| 10 | 2 Enrolment | Student self-registration (CS101, MD101, EL101) → submitted → registered | browser (student /registration) | POST /student/me/registrations → 201<br>POST /student/me/registrations/{id}/submit → 200 | registration: registered<br>lines: CS101:registered,EL101:registered,MD101:registered | registration: 3 row(s) — actions [student.registration.approve,student.registration.create,student.registration.submit] | admin: GET /student/students/{id} → 200<br>student: GET /student/me/registrations/{id} → 200<br>faculty: GET /student/me/registrations/{id} → 403<br>student sees the registration (06-2-student-registration.png) | ✅ pass |
| 11 | 2 Enrolment | Course enrolment: the student enrols in CS101-T1 / A from Courses | browser (student /courses) | POST /academic/me/enrolments → 201 | section enrolment: enrolled\|01a13ff8-159b-7aed-b2c6-d444a2f654cb<br>CS101/A roster: 2 | section enrolment: 1 row(s) — actions [academic.section_enrolment.enrol] | admin: GET /academic/sections/{id}/enrolments → 200 (roster)<br>faculty: GET /academic/sections/{id}/enrolments → 200 (own section roster)<br>student: GET /academic/me/courses → 200<br>student sees CS101-T1 in My courses (07-2-student-courses.png)<br>faculty sees CS101 in My classes (08-2-faculty-classes.png) | ✅ pass |
| 12 | 3 Attendance | Faculty: session → quick-grid attendance (Isha absent) → submit → finalised | browser (faculty /attendance) | POST /academic/sections/{id}/sessions → 201<br>POST /academic/sessions/{id}/start → 200<br>POST /academic/sessions/{id}/finalise-attendance → 200 | teaching session: completed\|finalised<br>attendance records: S202600001:absent,S202600003:present<br>attendance_audit (finalised-mark changes): 0 | teaching session: 5 row(s) — actions [academic.attendance.capture,academic.attendance.finalise,academic.teaching_session.complete,academic.teaching_session.sche | admin: GET /academic/sections/{id}/attendance-summary → 200 (section summary)<br>student: GET /academic/me/attendance → 200 (own attendance)<br>student: GET /academic/sessions/{id} → 403<br>faculty sees the session Finalised (10-3-faculty-attendance-finalised.png)<br>student sees 1 / 1 attended (11-3-student-attendance.png) | ✅ pass |
| 13 | 3 Attendance | Negatives: re-starting a finalised session (409); a student cannot record attendance (403) | API | faculty POST /sessions/{id}/start on completed/finalised → 409 the teaching session is completed; it cannot become started<br>faculty re-marks after finalisation → 409 attendance is finalised; use a governed correction<br>student POST /sessions/{id}/attendance → 403 | Isha still absent: absent | — | — | ✅ pass |
| 14 | 4 Fees | Charges generated from the published fee structure BTECH-Y1 for the student | browser (admin /finance/charges) | POST /finance/fee-structures/{id}/generate-charges → 200 | charge: published\|90000.00\|90000.00\|01a13ff8-4170-755e-9753-2d6080bfb095<br>publication journal balanced: 90000.00=90000.00 | charge: 3 row(s) — actions [finance.charge.create,finance.charge.publish,finance.charge.validate] | student: GET /finance/me/accounts → 200 (own charge)<br>faculty: GET /finance/student-accounts/{id} → 403<br>admin sees CHG-000001 (12-4-admin-charge.png) | ✅ pass |
| 15 | 4 Fees | Counter payment ₹30,000 recorded with receipt (allocation + posting) | browser (admin /finance/collections) | POST /finance/student-accounts/{id}/payments → 201 | payment: completed\|30000.00\|cash<br>receipt: 1<br>charge outstanding: 60000.00 | payment: 1 row(s) — actions [finance.payment.complete] | admin sees PAY-000001 (13-4-admin-counter-payment.png) | ✅ pass |
| 16 | 4 Fees | Negative: duplicate payment Idempotency-Key — replay returns the same payment, a different body with the key is rejected (422), nothing is double-charged | API (admin, explicit key) | first POST (key journey-pay-8b2f87c6…) → 201 PAY-000002 completed<br>replay same key + same body → 201, Idempotency-Replayed: true, payment 01a13ff8-4a4c-7e88-92f2-d91490c35632<br>same key + different body → 422 Idempotency key reused the Idempotency-Key was already used with a different request | payments on the account: 2\|40000.00<br>charge outstanding: 50000.00 | — | — | ✅ pass |
| 17 | 4 Fees | Negative: wrong role — student and faculty cannot record counter payments (403) | API | student POST /student-accounts/{id}/payments → 403<br>faculty POST /student-accounts/{id}/payments → 403 | no extra payment: 0 | — | — | ✅ pass |
| 18 | 4 Fees | Student pays the remaining ₹50,000 online (mock gateway) → receipt → nothing outstanding | browser (student /fees) | POST /finance/me/payments → 201<br>POST /finance/me/payments/{id}/confirm → 200 | charge settled: active\|0.00\|90000.00<br>payments: cash:30000.00:completed,bank_transfer:10000.00:completed,gateway:50000.00:completed<br>receipts: 3<br>ledger balanced (institution): 180000.00=180000.00 | online payment: 2 row(s) — actions [finance.payment.complete,finance.payment.initiate] | admin: GET /finance/student-accounts/{id} → 200 (balance 0)<br>student sees outstanding ₹50,000<br>student sees “You have no dues right now” (15-4-student-fees-paid.png)<br>faculty has no Fees screen (16-4-faculty-fees-unavailable.png) | ✅ pass |
| 19 | 5 Examination | Registrations from the rosters and hall tickets (dues clear after the payments) | browser (admin /examinations/registrations) | POST /exams/exam-sessions/{id}/registrations/bulk → 200<br>POST /exams/exam-sessions/{id}/hall-tickets/issue → 200 | exam registration: confirmed<br>hall ticket: active | exam registration: 1 row(s) — actions [exams.exam_registration.confirm] | student: GET /exams/me/hall-tickets → 200 (own hall ticket)<br>faculty: GET /exams/exam-sessions/{id}/registrations → 200 (exam.read)<br>admin sees registrations + hall tickets (17-5-admin-registrations.png) | ✅ pass |
| 20 | 5 Examination | Conduct: sitting started and completed — the student present, paper conducted | API (no conduct screen — gap) | — | conduct attendance: present | conduct record: 3 row(s) — actions [exams.conduct_record.complete,exams.conduct_record.open,exams.conduct_record.start] | — | ✅ pass |
| 21 | 5 Examination | Mark sheet opened by the exam cell | API (screen exists: /examinations/marks — not driven) | — | paper in evaluation: evaluation | mark sheet: 1 row(s) — actions [exams.mark_sheet.open] | — | ✅ pass |
| 22 | 5 Examination | Negative: invalid marks (above the maximum, negative) are rejected (422) and nothing is stored | API (faculty) | faculty PUT marks externalMarks=150 → 422 Validation failed<br>faculty PUT marks externalMarks=-5 → 422 Validation failed<br>student PUT marks → 403 | no marks stored: null | — | — | ✅ pass |
| 23 | 5 Examination | Faculty enters 78 / 100 and submits the mark sheet | browser (faculty /assessments) | PUT /exams/exam-papers/{id}/marks → 200<br>POST /exams/mark-sheets/{id}/submit → 200 | paper mark: 78.00\|submitted<br>mark history: 2 | mark sheet: 2 row(s) — actions [exams.mark_sheet.open,exams.mark_sheet.submit] | faculty sees the submitted sheet (18-5-faculty-marks-submitted.png) | ✅ pass |
| 24 | 5 Examination | Exam cell verifies → final marks approval → finalised | API (screen exists: /examinations/marks — not driven) | — | paper: marks_finalised<br>paper mark final: finalised | mark sheet: 6 row(s) — actions [exams.mark_sheet.approve,exams.mark_sheet.finalise,exams.mark_sheet.open,exams.mark_sheet.request_approval,exams.mark_sheet.subm | faculty: GET /exams/exam-papers/{id}/gradebook → 200 (gradebook)<br>student: GET /exams/exam-papers/{id}/marks → 403 | ✅ pass |
| 25 | 6 Result | Results computed → validated (approved) → finalised → publication authorised → published | browser (admin /examinations/results) | POST /exams/exam-sessions/{id}/result-batches → 201<br>POST /exams/result-batches/{id}/compute → 200<br>POST /exams/result-batches/{id}/validate → 200<br>POST /exams/result-batches/{id}/finalise → 200<br>POST /exams/exam-sessions/{id}/publications → 201<br>POST /exams/publications/{id}/authorise → 200<br>POST /exams/publications/{id}/publish → 200 | term result: 01a13ff8-6c18-7f36-a688-4e561a33b35b\|published\|8.00<br>result line: CS101-T1:A:78.00 | result batch: 5 row(s) — actions [exams.result_batch.approve,exams.result_batch.compute,exams.result_batch.create,exams.result_batch.finalise,exams.result_batch<br>publication: 3 row(s) — actions [exams.publication.authorise,exams.publication.create,exams.publication.publish] | admin sees published results (19-6-admin-results-published.png) | ✅ pass |
| 26 | 6 Result | Visibility: student sees the published result, faculty the gradebook, admin the session results | browser (student /results) + API | — | — | — | student: GET /exams/me/results → 200 (own published result)<br>admin: GET /exams/exam-sessions/{id}/results → 200<br>faculty: GET /exams/exam-papers/{id}/gradebook → 200<br>faculty: GET /exams/me/results → 403<br>student Results screen (20-6-student-results.png) | ✅ pass |
| 27 | persistence | Sign-in: administrator, faculty and student (API clients) | API · Keycloak OIDC (code + PKCE) | — | — | — | — | ✅ pass |
| 28 | persistence | Sign-in: administrator, faculty and student (browser) | browser · Keycloak login page | — | — | — | — | ✅ pass |
| 29 | 7 Persistence | The stack really restarted (postgres start time moved, API healthy) | DB + API | — | — | — | — | ✅ pass |
| 30 | 7 Persistence | Attendance persists (API + DB + student screen) | API + DB + browser | — | attendance records: 01a13ff7-f3ca-7b59-8153-ea624d2f5fc3:absent,01a13ff8-159b-7aed-b2c6-d444a2f654cb:present | — | student: GET /academic/me/attendance → 200<br>student still sees 1 / 1 (104-7-student-attendance-after-restart.png) | ✅ pass |
| 31 | 7 Persistence | Fees persist (charge settled, 3 completed payments, receipts) | API + DB + browser | — | charge + payments: 0.00\|90000.00\|3 | — | student: GET /finance/me/accounts → 200 | ✅ pass |
| 32 | 7 Persistence | Results persist (published term result and grade) | API + DB + browser | — | term result: published\|8.00\|A:78.00 | — | student: GET /exams/me/results → 200 | ✅ pass |

Screenshots (26): `01-journey-home-administrator.png`, `02-journey-home-faculty.png`, `03-journey-home-student.png`, `04-1-admission-student-created.png`, `05-2-student360-enrolled.png`, `06-2-student-registration.png`, `07-2-student-courses.png`, `08-2-faculty-classes.png`, `09-3-faculty-attendance-grid.png`, `10-3-faculty-attendance-finalised.png`, `101-persistence-home-administrator.png`, `102-persistence-home-faculty.png`, `103-persistence-home-student.png`, `104-7-student-attendance-after-restart.png`, `105-7-student-fees-after-restart.png`, `106-7-student-results-after-restart.png`, `11-3-student-attendance.png`, `12-4-admin-charge.png`, `13-4-admin-counter-payment.png`, `14-4-student-checkout.png`, `15-4-student-fees-paid.png`, `16-4-faculty-fees-unavailable.png`, `17-5-admin-registrations.png`, `18-5-faculty-marks-submitted.png`, `19-6-admin-results-published.png`, `20-6-student-results.png`.

## 3. Gaps — API used instead of a screen

| Step | Why |
|---|---|
| Seed (institution, campus, departments, programmes, curriculum, year/terms, courses, offerings, sections, faculty assignment, 2 seed students, fee structure, exam policy / session / timetable) | setup data, not part of the student journey; all through the API as the administrator (and faculty where the API requires it) |
| 2 Enrolment: registration window | screen exists (/students/registration); scheduled + opened through the API to keep the journey on one browser path |
| 5 Examination: exam conduct (start / attendance / complete) | no screen: conduct records are API-only (exams e2e does the same) |
| 5 Examination: open mark sheet | screen exists (/examinations/marks “Open sheet”); opened through the API so marks entry stays the faculty's browser step |
| 5 Examination: verify / approve / finalise marks | screen exists (/examinations overview card + Marks tab); driven through the API — the faculty's entry is the browser step |

No step uses a mock of SutraERP functionality. The only simulated external party is the payment
gateway's **mock checkout** (the gateway adapter the dev stack ships); the payment, receipt,
allocation and ledger posting it triggers are real.

## 4. Where the run depends on auto-approval (DEF-X-001)

| Kind | Step | State seen | Real decision endpoint (for the integrator) |
|---|---|---|---|
| institution_activation | seed: institution submit → active | active | `POST /api/v1/institution/activation-requests/{id}/decision` |
| offering | seed: offering CS101-T1 submitted | approved | `POST /api/v1/academic/offerings/{id}/decision` |
| offering | seed: offering MD101-T1 submitted | approved | `POST /api/v1/academic/offerings/{id}/decision` |
| offering | seed: offering EL101-T1 submitted | approved | `POST /api/v1/academic/offerings/{id}/decision` |
| faculty_assignment | seed: faculty assignment submitted | approved | `POST /api/v1/academic/faculty-assignments/{id}/decision` |
| admission | seed: admission of Isha Kulkarni | approved | `POST /api/v1/student/admissions/{id}/decision` |
| admission | seed: admission of Kiran Joshi | approved | `POST /api/v1/student/admissions/{id}/decision` |
| fee_structure | seed: fee structure submitted | approved | `POST /api/v1/finance/fee-structures/{id}/decision` |
| exam_policy | seed: exam policy submitted | approved | `POST /api/v1/exams/exam-policies/{id}/decision` |
| exam_session | seed: exam session submitted | approved | `POST /api/v1/exams/exam-sessions/{id}/decision` |
| exam_schedule | seed: exam timetable submitted | approved | `POST /api/v1/exams/exam-schedules/{id}/decision` |
| admission | 1 Admission: submit-for-approval | approved | `POST /api/v1/student/admissions/{id}/decision` |
| registration | 2 Enrolment: registration submitted | registered | `POST /api/v1/student/registrations/{id}/decision` |
| mark_sheet | 5 Examination: final marks approval | approved | `POST /api/v1/exams/mark-sheets/{id}/decision` |
| result_batch | 6 Result: result approval | approved | `POST /api/v1/exams/result-batches/{id}/decision` |
| publication | 6 Result: publication authorisation | authorised | `POST /api/v1/exams/publications/{id}/decision` |

`approve(kind, id)` is the single seam: once `part1/s2-appr` is merged it must sign in an approver
other than the requester and POST the decision endpoint above.

## 5. Negative tests

| Negative | Expected | Result |
|---|---|---|
| Faculty confirms an admission; student creates an application | 403 / 403, no row written | pass |
| Confirm an admission that already created its student | 409, unchanged | pass |
| Duplicate applicant (same login, name, DoB, e-mail) | refused at intake or offer (409/422) | **known open defect DEF-C-003** — accepted through offer; fixed on `part1/s2-acad`; listed in `E2E_KNOWN_DEFECTS` so it is reported but not counted. The journey cancels the duplicate admission it created |
| Re-start a finalised teaching session; re-mark finalised attendance; student records attendance | 409 / 409 / 403 | pass |
| Payment replay with the same Idempotency-Key + body; same key, different body | same payment, `Idempotency-Replayed: true` / 422; exactly 2 payments, ₹40,000 | pass |
| Student / faculty record a counter payment | 403 / 403, no payment | pass |
| Marks 150 / −5 on a 100-mark paper; student enters marks | 422 / 422 / 403, nothing stored | pass |

## 6. Persistence (after `docker compose restart postgres api`)

Postgres start time moved, the API answered through the web proxy, and the attendance records, the
charge (outstanding 0.00, settled 90,000.00, 3 completed payments) and the published term result
(SGPA 8.00, grade A, 78.00) were **identical** to the snapshot taken before the restart — in the DB,
through the API and on the student's screens. Restart addresses: `before: api 172.18.0.6 · postgres 172.18.0.5  / after:  api 172.18.0.5 · postgres 172.18.0.6`.

## 7. Iterations that led to the green run

| Run | Outcome | Cause | Fix |
|---|---|---|---|
| [38739336672](https://github.com/Varad1614/ERP_hark_2/actions/runs/38739336672) | stack healthy, 3 roles signed in; admin `mfa none` | DEF-P1-DEMO-003 (no AMR in the dev realm) | realm flow `browser-sutra` (41dafc94, db4eb556) |
| [38744032264](https://github.com/Varad1614/ERP_hark_2/actions/runs/38744032264) | 17/26 journey steps pass | 5 wrong test assumptions (audit action name, attendance_audit semantics, no-dues marker, mark-row key) | journey corrections (964c4af8) |
| [38745591464](https://github.com/Varad1614/ERP_hark_2/actions/runs/38745591464) | journey 24/26; persistence 0/6 | results audited on batch/publication, not the term result (test); **web → api 502 after the api restart (DEF-P1-DEMO-004)** | f7919e97 |
| [38748076580](https://github.com/Varad1614/ERP_hark_2/actions/runs/38748076580) | journey 25/26 (+1 known); persistence 2/6 | **Keycloak's first login after the postgres restart failed (DEF-P1-DEMO-005)** | 17027e4d |
| [38749424353](https://github.com/Varad1614/ERP_hark_2/actions/runs/38749424353) | **green** (25 + 1 known / 6 of 6) | — | — |
| [38750761187](https://github.com/Varad1614/ERP_hark_2/actions/runs/38750761187) | **green** — this report | duplicate-admission cleanup now cancels (withdraw is only for confirmed admissions) | 04674047 |
