# Pilot scope — SutraERP institutional pilot (PROPOSED)

**Status: PROPOSED — awaiting institutional approval (BLOCKED_EXTERNAL).**
No institution has authorised a pilot, no pilot users exist and no institutional data has been
received. Everything below is a proposal to be agreed with, and signed by, an institution's
authorised representatives before any real data is loaded. Owner of this document: SutraERP
programme (Part 2 · Stage 6, branch `part2/s6-pilot`).

Related: [`UAT_TEST_CASES.md`](UAT_TEST_CASES.md) · [`UAT_RESULTS.md`](UAT_RESULTS.md) ·
[`USER_FEEDBACK.md`](USER_FEEDBACK.md) · [`PILOT_DEFECT_REGISTER.md`](PILOT_DEFECT_REGISTER.md) ·
[`INSTITUTION_ACCEPTANCE.md`](INSTITUTION_ACCEPTANCE.md) · [`FEATURE_INVENTORY.md`](FEATURE_INVENTORY.md) ·
[`DEFECT_REGISTER.md`](DEFECT_REGISTER.md) · [`E2E_TEST_REPORT.md`](E2E_TEST_REPORT.md) ·
runbooks `docs/runbooks/pilot-go-live.md`, `docs/runbooks/go-live-checklist.md` · synthetic tenant
tooling `deploy/pilot/` (seed + reset).

## 1. Objective

Validate, with one institution's real staff and students, that SutraERP runs the five core
academic-administration areas end to end for one academic term, and that its reports agree with
the institution's own records — before any wider rollout.

## 2. In scope (proposed)

| Area | Included workflows (screen · API) | Roles |
|---|---|---|
| Student records | admission application → offer → documents → verification → approval → student (`/students/admissions`, `POST /api/v1/student/institutions/{id}/admission-applications` …); Student 360 and enrolment (`/students/record/:id/:tab`); student documents (`/documents`); bulk migration of existing students (`/data-import`, `/api/v1/platform/imports`) | administrator (admissions office), registrar (admission decision), student |
| Attendance | teaching sessions, quick-grid marking, submit/finalise, corrections (`/attendance`, `POST /api/v1/academic/sections/{id}/sessions`, `…/sessions/{id}/finalise-attendance`); section summary + threshold evaluation (`/academic-delivery/attendance`, `GET /api/v1/academic/sections/{id}/attendance-summary`, `POST …/attendance-summary/evaluate`); student view (`/timetable`) | faculty, administrator, student |
| Academics | programme / curriculum / calendar set-up (`/academics/:tab`), offerings + sections + faculty assignment (`/academic-delivery/:tab`), student registration (`/registration`, `/students/registration`), course enrolment (`/courses`) | administrator, second administrator (checker), faculty, student |
| Fees | fee heads + structures (`/finance/charges`), charge generation, counter collection with receipt (`/finance/collections`), concessions/adjustments (`/finance/adjustments`, finance-approver decision), student fees + online payment (`/fees`; live gateway BLOCKED_EXTERNAL — see §4), receivables (`/finance/receivables`), opening balances by import (approval required) | administrator / accounts staff (maker), finance approver (checker), student |
| Examinations | grading scheme, policy, session, papers, timetable (`/examinations/sessions`, `/examinations/schedule`), registrations + hall tickets (`/examinations/registrations`), marks entry (`/assessments`), verification and approval (`/examinations/marks`), malpractice (`/examinations/malpractice`), results compute → approve → publish (`/examinations/results`), student results (`/results`) | examination staff (exam cell = administrator role), controller of examinations (checker), faculty, student |

## 3. Out of scope for the pilot (proposed)

* People/HR, payroll and leave (`/people`, `/leave`), campus operations (hostel, library, transport,
  facilities), procurement/resources, accreditation and regulatory reporting, analytics dashboards.
  These modules exist but are not part of the pilot acceptance (FEATURE_INVENTORY rates most of them
  `ready-with-gaps` / `not-ready`).
* Live online payment collection through Razorpay and live institution S3 storage / college SMTP:
  implementation is tested against local equivalents and documented-contract stubs only
  (PART2_EXECUTION_TRACKER: BLOCKED_EXTERNAL until credentials exist). A pilot can run with counter
  collection only, or include online payment after the institution provides gateway test keys.
* Mobile apps, multi-institution groups, ABC / external scholarship sync (mock-only in Part 1).
* Historical data beyond what the institution chooses to migrate (opening balances, current students,
  staff, programme structure).

## 4. Prerequisites (all BLOCKED_EXTERNAL — none exists today)

| # | Prerequisite | Provided by | Status |
|---|---|---|---|
| P1 | Signed pilot authorisation (scope, duration, named sponsor) | institution management | BLOCKED_EXTERNAL |
| P2 | DPDP Act 2023 compliance: data-processing agreement (institution = Data Fiduciary, SutraERP operator = Data Processor), purpose limitation, notice and **verifiable consent** of students/staff (and parental consent for anyone under 18), retention + erasure terms, breach notification contacts, data-localisation/hosting decision | institution legal/DPO + operator | BLOCKED_EXTERNAL |
| P3 | Named user accounts per role (administrator ×2 for maker-checker, registrar, accounts staff, finance approver, examination staff, controller of examinations, faculty sample, student sample) and their identity-provider onboarding (Keycloak realm / college SSO) | institution IT | BLOCKED_EXTERNAL |
| P4 | Institutional rules to configure, each confirmed in writing: **grading scheme** (bands, grade points, pass %, credit framework), **fee heads and fee structures** (amounts, instalments, due dates, concessions policy), **attendance thresholds** (minimum %, condonation rules), **academic calendar** (year, terms, holidays, exam windows), programme/curriculum structure | registrar, accounts, exam cell | BLOCKED_EXTERNAL |
| P5 | Source extracts for migration (students, staff, programmes, fee balances) with control totals (row counts, Σ outstanding fees) for reconciliation | institution records office | BLOCKED_EXTERNAL |
| P6 | Hosting decision + production environment (on-prem drills exist: Stage 5 deploy) with backups, monitoring contacts, support hours | institution IT + operator | BLOCKED_EXTERNAL |
| P7 | Optional: Razorpay test keys + webhook URL; institution SMTP relay; S3 bucket/keys | institution | BLOCKED_EXTERNAL |

## 5. Success criteria (proposed; to be agreed and signed)

1. 100 % of UAT cases in `UAT_TEST_CASES.md` marked in scope executed by the named role holders;
   ≥ 95 % pass, and **no open Critical or High** defect in `PILOT_DEFECT_REGISTER.md`.
2. Data migration: every imported entity reconciles with the source control totals (row counts
   created + updated + skipped = rows in file; opening balances Σ file = Σ committed = Σ ledger).
3. Reports vs institutional records (UAT-RPT-*): student strength, attendance %, fee collection and
   outstanding, and result statistics match the institution's own figures for the same period
   (zero unexplained differences).
4. Every governed decision (admissions, concessions, marks, results, malpractice, opening balances)
   made by an independent checker — never by its maker — and visible in the audit trail.
5. No personal data incident; DPDP obligations of P2 met; availability during agreed hours ≥ 99 %.
6. Written acceptance in `INSTITUTION_ACCEPTANCE.md` by the authorised representatives.

## 6. Timeline template (dates to be set at authorisation — none are committed)

| Phase | Duration (proposed) | Exit gate |
|---|---|---|
| 0. Authorisation + DPA + consent design | 2–4 weeks | P1, P2 signed |
| 1. Configuration (rules P4) + accounts (P3) | 2 weeks | rules confirmed in writing |
| 2. Migration dry runs + reconciliation (UAT-MIG-*) | 1–2 weeks | totals reconciled |
| 3. Training + UAT round 1 | 2 weeks | UAT_RESULTS round 1 recorded |
| 4. Fixes + retest (UAT round 2) | 1–2 weeks | no open Critical/High |
| 5. Parallel run for one term (shadow the existing process) | one term | report comparisons (UAT-RPT-*) clean |
| 6. Acceptance | 1 week | INSTITUTION_ACCEPTANCE signed |

## 7. Roles and responsibilities

| Role | Institution | SutraERP operator |
|---|---|---|
| Pilot sponsor (signs authorisation + acceptance) | Principal / Director | — |
| Data Protection Officer / grievance officer (DPDP) | institution | processor contact |
| Pilot coordinator | registrar's office | implementation lead |
| UAT testers by role | administrator, registrar, accounts staff, finance approver, examination staff, controller of examinations, faculty, students | — |
| Configuration + migration | provides rules + extracts | configures, imports, reconciles |
| Defect triage | reports in PILOT_DEFECT_REGISTER | fixes, retests, reports |
| Support during pilot | first-line contact | Stage 5 monitoring + on-call |

**Role mapping to the product (as built):** the product has no separate “accounts staff” or
“examination staff” role. Fee collection and fee set-up are performed with the **administrator**
role (maker); fee decisions (concessions, refunds, write-offs, opening balances) by the
**finance approver**. Examination set-up, marks verification and malpractice handling use the
**administrator** role (exam cell, maker); approvals and result publication by the **controller of
examinations**. Whether the institution accepts administrator-level access for its accounts and exam
cell staff is an open pilot decision (see PILOT_DEFECT_REGISTER.md, PDEF-SYN-003).

## 8. Synthetic preparation already done (not pilot evidence)

* `deploy/pilot/seed-pilot.mjs` seeds the synthetic `PILOT` institution; `deploy/pilot/reset-pilot.sh`
  empties that synthetic tenant (guarded; proven in `sutraerp-pilot.yml`).
* `apps/web/e2e/uat-dryrun.e2e.mjs` walks one key UAT case per role with synthetic data on the full
  demo stack (`sutraerp-part1-demo.yml`). Results: `UAT_TEST_CASES.md` → “Synthetic dry run (not UAT)”.
