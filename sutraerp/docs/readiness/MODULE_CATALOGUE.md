# SutraERP — module catalogue (Part 2 · Stage 7, S7-commercial)

Branch `part2/s7-commercial` (base `part2/s4-int` @ `fac412ce`). Docs only — no code was changed to write this.
Companion documents: [COMMERCIAL_PACKAGING.md](COMMERCIAL_PACKAGING.md) · [PRICING_AND_UNIT_ECONOMICS.md](PRICING_AND_UNIT_ECONOMICS.md) ·
[SUPPORT_AND_MAINTENANCE.md](SUPPORT_AND_MAINTENANCE.md) · [COMMERCIAL_READINESS.md](COMMERCIAL_READINESS.md).

## 0. What a class means here (read first)

SutraERP has **no real college pilot, no institutional acceptance, and no live validation of Razorpay, AWS S3 or a
college SMTP relay** (all BLOCKED_EXTERNAL — see [PART2_EXECUTION_TRACKER.md](PART2_EXECUTION_TRACKER.md) § BLOCKED_EXTERNAL).
Therefore **no module is "Production-ready" in the absolute sense** and none may be sold or described that way.

| Class | Rule applied in this catalogue (all conditions must hold) |
|---|---|
| **Production-ready (technically verified; institution acceptance pending)** | (1) every core flow of the module is driven end-to-end in CI — in the human-approval Keycloak journey ([E2E_TEST_REPORT.md](E2E_TEST_REPORT.md): OIDC + TOTP checkers, maker ≠ checker) and/or a browser e2e suite; (2) covered by the Part 1 security audit with no open Critical/High ([SECURITY_AUDIT.md](SECURITY_AUDIT.md)); (3) no open P0/P1 defect ([DEFECT_REGISTER.md](DEFECT_REGISTER.md): 0 P0, 0 P1 open after Part 1); (4) no core flow is missing, mock-only or reachable only through the API (decision with no screen); (5) no core flow depends on an unvalidated external service. |
| **Pilot-ready** | Core flows work and are tested (DB/API tests on real PostgreSQL, browser e2e for the main path), no open P0/P1, but at least one condition above fails: open P2 gaps in core flows, governed decisions with no screen, missing sub-workflows, or an external dependency not validated live. Suitable for a supervised pilot with a written gap list. |
| **Under development** | Capability is partially built or not wired end-to-end; not offerable except as roadmap. |
| **Deferred** | Out of the current release scope by plan; not offerable. |

Evidence sources (all on base `fac412ce` unless stated): [FEATURE_INVENTORY.md](FEATURE_INVENTORY.md) / [.csv](FEATURE_INVENTORY.csv)
(439 rows, **Part 1 Stage 1 baseline** — status/readiness columns predate the Part 1 fixes; rows rated `not-ready` only because of
DEF-X-001/002 auto-approval/SoD are now fixed, so the counts below are a conservative floor), [DEFECT_REGISTER.md](DEFECT_REGISTER.md),
[FEATURE_GAPS.md](FEATURE_GAPS.md), [E2E_TEST_REPORT.md](E2E_TEST_REPORT.md) (journey 29/29 + persistence 6/6, demo run 38755333716),
browser suites `apps/web/e2e/{student,academic,finance,people,campus,exams,imports}.e2e.mjs` (all green on the Stage 4 integrated
head, SutraERP CI 38905792553), [SECURITY_AUDIT.md](SECURITY_AUDIT.md) (44 findings, 0 Critical/High open), Stage 4 rows in
[PART2_EXECUTION_TRACKER.md](PART2_EXECUTION_TRACKER.md); Stage 5 reports on `part2/s5-deploy` (INFRASTRUCTURE_REPORT.md,
DEPLOYMENT_RUNBOOK.md), `part2/s5-backup` (BACKUP_RESTORE_REPORT.md), `part2/s5-monitor` (MONITORING_AND_SUPPORT.md).

**Register note.** Some per-domain rows in DEFECT_REGISTER.md are still `open` although their root cause is marked fixed
(e.g. DEF-A-003, B-003, C-006, G-004, H-007 are "dup of DEF-X-002", which is fixed; DEF-F-004 is fixed by S4-pay `e76dfe0`
per the Stage 4 tracker; DEF-F-007 is addressed by S4-ui-feedocs' gateway capability endpoint). This catalogue lists them as the
register shows them and flags the likely-stale ones with "(stale?)"; the register owner should reconcile them. None is P0/P1.

Module keys match the entitlement registry `sutraerp/catalog/entitlements.json` (`part2/s7-entitle` @ `bf48b7f6`, registry
version 1). Package membership is in [COMMERCIAL_PACKAGING.md](COMMERCIAL_PACKAGING.md). Note: the registry's `assets` description
("asset register, lifecycle, verification, disposal") is broader than what is built — verification and disposal are Deferred (§2.14).

## 1. Summary

| # | Module key | Module | Class | Inventory rows (impl / partial / mock / missing / deferred) | API-tested · browser-tested (Stage 1) | Open defects (register) |
|---|---|---|---|---|---|---|
| 1 | `admissions` | Admissions | **Production-ready (technically verified; institution acceptance pending)** | 12 (10 / 2 / 0 / 0 / 0) | 12 · 7 | P2 DEF-C-004 · P3 DEF-C-008, C-009, C-012 |
| 2 | `students` | Student records, registration & progression | **Pilot-ready** | 38 (20 / 16 / 2 / 0 / 0) | 38 · 19 | P2 DEF-C-005, C-006 (stale?), C-007 |
| 3 | `academics` | Academics (offerings, sections, timetable, faculty assignment, internal assessment, flexible learning) | **Pilot-ready** | 40 (24 / 15 / 0 / 1 / 0) | 39 · 26 | P2 DEF-D-005, D-007, D-008 |
| 4 | `attendance` | Attendance (teaching sessions, capture, policy, corrections, self-views) | **Production-ready (technically verified; institution acceptance pending)** | 5 (4 / 1 / 0 / 0 / 0) | 5 · 4 | P3 DEF-D-006, D-010 |
| 5 | `examinations` | Examinations & results | **Pilot-ready** | 36 (25 / 10 / 0 / 0 / 1) | 34 · 20 | P2 DEF-I-011, I-012, I-016 · P3 DEF-I-013, I-014 |
| 6 | `fees` | Fees & finance (fee structures, charges, collection, refunds, ledger, receivables, bank, aid) | **Pilot-ready** | 38 (31 / 2 / 1 / 2 / 2) | 33 · 21 | P2 DEF-F-005, F-006, F-007 (stale?), F-008 · P3 DEF-F-004 (stale?), F-009, F-011 |
| 7 | `hr` | HR (staff records, engagements, positions, recruitment, onboarding, leave, staff attendance) | **Pilot-ready** | 41 (21 / 15 / 0 / 5 / 0) | 35 · 9 | P2 DEF-E-006 |
| 8 | `payroll` | Payroll | **Pilot-ready** | 15 (11 / 2 / 0 / 2 / 0) | 13 · 9 | P2 DEF-E-005, E-006 · P3 DEF-E-003 |
| 9 | `hostel` | Hostel | **Pilot-ready** | 11 (8 / 2 / 0 / 1 / 0) | 10 · 2 | P2 DEF-G-003, G-006, G-007 |
| 10 | `transport` | Transport | **Pilot-ready** | 6 (4 / 2 / 0 / 0 / 0) | 5 · 0 | P2 DEF-G-007 · P3 DEF-G-010 |
| 11 | `library` | Library | **Pilot-ready** | 11 (10 / 0 / 0 / 1 / 0) | 10 · 8 | P2 DEF-G-007 |
| 12 | `procurement` | Procurement (sourcing, requisition, PO, goods receipt, vendor invoice / payment) | **Pilot-ready** | 15 (8 / 3 / 0 / 1 / 3) | 11 · 3 | P2 DEF-J-006, J-007, J-008, J-010 |
| 13 | `inventory` | Inventory (stores, stock ledger, issues, reservations, adjustments, reorder) | **Pilot-ready** | 8 (6 / 0 / 0 / 0 / 2) | 6 · 1 | — (shares DEF-J-008 UI gaps) |
| 14 | `assets` | Assets (register, custody, retirement) | **Pilot-ready (limited scope)** | 5 (2 / 0 / 0 / 0 / 3) | 2 · 1 | — (asset accounting F-037 deferred) |
| 15 | `reporting` | Reporting, dashboards, regulatory (AISHE) & accreditation (NAAC) | **Pilot-ready** | 19 (13 / 3 / 0 / 3 / 0) | 16 · 9 | P2 DEF-H-002, H-007 (stale?), H-010 · P3 DEF-H-003, H-011 |

**Totals: 2 modules Production-ready (technically verified; institution acceptance pending) — `admissions`, `attendance`;
13 Pilot-ready; 0 Under development; 0 Deferred** at module level. Under-development and deferred *capabilities inside*
modules are listed per module and in §3. Row mapping to inventory ids is in §4 (reproducible).

Platform foundation (not sold separately, included in every package): institution & structure (Domain B), identity / SSO /
MFA (Keycloak, S4-idp), approval engine (Flowable human approvals, maker ≠ checker on 99 guarded decision endpoints),
tenant isolation (forced RLS), audit, notifications (in-app + e-mail), document vault (S4-docs), data import / export
(S4-import: 9 targets), on-prem install / upgrade / backup / monitoring tooling (Stage 5).

## 2. Module detail

### 2.1 `admissions` — Production-ready (technically verified; institution acceptance pending)

- **Verified:** application intake → review → offer → acceptance → documents (upload / verify, real object storage) →
  governed approval (registrar, maker self-decision 403) → financial clearance → confirmation → student creation;
  duplicate applicant refused at intake (409, DEF-C-003 fixed); wrong-role and invalid-transition negatives.
  Decision screen exists (`features/student/record/api.ts` → `/admissions/{id}/decision`).
- **Evidence:** [E2E_TEST_REPORT.md](E2E_TEST_REPORT.md) steps 5–8 (human mode, Keycloak OIDC + TOTP);
  `apps/web/e2e/student.e2e.mjs`; documents: `apps/api/tests/db_p2_docs.rs` (7/7), `crates/infra/tests/live_s3.rs` on real MinIO,
  CI 38885937911; journey with browser upload/download demo 38886172253; on-prem smoke (3 MiB document over TLS, SSE) in on-prem run
  38931814409 (`part2/s5-deploy`); inventory C-001…C-012.
- **Open:** DEF-C-004 duplicate *resolution* (post-creation duplicate cases) has no UI (P2); P3 DEF-C-008 (duplicated state logic),
  C-009 (offer expiry only on button press), C-012 (offer does not check intake/session precondition).
- **Caveats:** document storage on AWS / a college S3 endpoint is BLOCKED_EXTERNAL; the on-prem bundle's MinIO (TLS + SSE) is the
  verified store. No real applicant has used the flow.

### 2.2 `students` — Pilot-ready

- **Verified:** student activation / enrolment, Student 360, documents & certificate requests, archival, registration windows,
  self-registration and office registration, holds, credit ledger, guardians, scholarships, qualification completion
  (inventory C-013…C-050; journey steps 9–12; `student.e2e.mjs`).
- **Gaps that keep it Pilot-ready:** governed student changes — transfer, identity correction, registration decision,
  progression, credit transfer, programme migration, academic break, conferred-qualification correction — are **API-only**
  (DEF-C-007; the journey decided registrations through the API); prerequisites / co-requisites not validated (DEF-C-005);
  Academic Bank of Credits sync (C-037) and NSP scholarship sync (C-049) are **mock-only → Deferred**.

### 2.3 `academics` — Pilot-ready

- **Verified:** offerings (publish, enrol windows), sections, section enrolment, waitlist, faculty assignment with decision
  screen, timetable planning → generation → approval → publication, monitoring/governance, flexible learning and credit
  recognition, course resources (inventory; `academic.e2e.mjs`; journey seed).
- **Gaps:** faculty cannot mark or moderate coursework in the browser (DEF-D-007; D-034 `not-ready`); large parts of offering
  governance (quotas, revisions, merge/split, templates, simulations) are API-only (DEF-D-008); enrolment eligibility override
  dead-ends in human mode (DEF-D-005; the governed override request was added by S3-appr — reconcile); flexible-learning
  analytics D-044 **missing**.

### 2.4 `attendance` — Production-ready (technically verified; institution acceptance pending)

- **Verified:** teaching session lifecycle, attendance capture → submit → finalise in the faculty browser, policy / threshold
  evaluation, student & faculty self-views, governed correction after finalisation with a decision screen
  (`/attendance-corrections/{id}/decision`), persistence across a stack restart, DB immutability of finalised attendance
  (journey steps on attendance + persistence 33; [DATABASE_INTEGRITY_REPORT.md](DATABASE_INTEGRITY_REPORT.md)); D-022…D-025, D-027.
- **Open (P3 only):** DEF-D-006 faculty "Submit attendance" is a non-atomic four-call chain; DEF-D-010 corrections accepted on
  locked/archived sessions. Staff (HR) attendance days are part of `hr`.
- **Caveats:** biometric / device capture is not part of the product (no integration exists).

### 2.5 `examinations` — Pilot-ready

- **Verified:** exam policy, session, papers, schedule (approved by the controller of examinations), registration, eligibility
  (fees + attendance), hall tickets, marks entry (faculty browser), validation, internal-assessment import, verification →
  approval → finalisation, result computation, SGPA, result approval and publication (maker ≠ checker, 403 on self-decision),
  grade cards, revaluation, student results view (journey steps on exams/results; `exams.e2e.mjs`; DEF-I-003…I-010 fixed).
- **Gaps:** exam **conduct** (start / attendance / complete) has no screen — API only (E2E report §3); venues/seating (I-006)
  and invigilation (I-007) partial; **CGPA across terms incl. supplementary/improvement (I-022) partial and not tested**;
  supplementary execution (I-031) partial; hall-ticket eligibility fails open when attendance is missing (DEF-I-011);
  moderation bounds hard-coded (DEF-I-012); governed corrections unreachable from UI (DEF-I-016).
  Practical & viva conduct (I-011) **Deferred**.

### 2.6 `fees` — Pilot-ready

- **Verified:** fee heads / structures, charge generation and publication (Dr AR / Cr income), instalments, counter
  payment → allocation → receipt → ledger posting, idempotent payment replay, refunds and adjustments with independent
  finance approver (403 on self-decision), double-entry engine with DB immutability, trial balance, financial periods,
  receivables / dunning, scholarship aid; persistence after restart (journey 4 Fees + 34; `finance.e2e.mjs`).
  Razorpay adapter: server-side verification, idempotent webhooks, recovery job, refunds, settlement reconciliation —
  tested against Razorpay's documented contract (`db_p2_pay`, CI 38882654896) and Checkout UI (CI 38899332909).
- **Gaps that keep it Pilot-ready:** **live / test-mode Razorpay is BLOCKED_EXTERNAL** (needs `rzp_test_…`/`rzp_live_…` keys,
  webhook secret, public HTTPS URL) — online collection must not be switched on for a college until that validation is done;
  fee-structure decision, bulk collection, write-off decision, bank-account decision and several other finance decisions have
  **no screen** (DEF-F-008; the journey approved the fee structure through the API); no duplicate-instrument protection for
  counter payments (DEF-F-005); late capture on cancelled/expired payments refused (DEF-F-006, failed path fixed by S4-pay).
  **Missing:** accounting batches (F-026), fund transfers (F-035). **Deferred:** budgets & cost centres (F-027), asset
  accounting (F-037). Financial statements (F-038) partial. Real bank API / statement feeds BLOCKED_EXTERNAL ([SECURITY_AUDIT.md](SECURITY_AUDIT.md#blocked_external)).

### 2.7 `hr` — Pilot-ready

- **Verified:** staff record create → verify → activate, engagement with approval, recruitment requisition → offer,
  leave policy → request (self-service) → consumption, staff attendance, workload, My HR self-service
  (`people.e2e.mjs`, green in CI 38905792553).
- **Gaps:** ~66 People endpoints have no web client (DEF-E-006: separation, probation, certificate, onboarding decision,
  engagement transfer, staff merge, leave adjustment / encashment decisions …). **Missing:** deputation (E-005), faculty
  availability (E-015), HR case / grievance / disciplinary (E-021), promotion (E-022), HR compliance (E-025).

### 2.8 `payroll` — Pilot-ready

- **Verified:** payroll configuration → approval, salary structures and assignments, run → calculation (loss of pay from unpaid
  leave) → validation → approval → finalisation → posting to Finance (Dr 5410 / Cr 2410, 2420) → payslips, net-pay
  disbursement with NEFT file and salaries-payable clearing (DEF-E-007 fixed), faculty payslip view (`people.e2e.mjs`).
- **Gaps:** **no statutory outputs — PF / ESI / PT / TDS challans, Form 16, returns (DEF-E-005, E-056)**; statutory filing must
  stay outside SutraERP during a pilot; off-cycle payroll unreachable from the UI (DEF-E-006); bank file transmission is manual
  (no bank API — BLOCKED_EXTERNAL).

### 2.9 `hostel` — Pilot-ready

- **Verified:** hostel policy with approval, rooms & beds (capacity guard), application → waiting list → allocation + hostel fee
  charge, leave workflow, room inspection → damage charge, mess allocation, complaints → service desk, vacating & exit clearance
  (`campus.e2e.mjs` hostel step).
- **Gaps:** resident self-service partial (G-028); student self-service hostel application / leave approved automatically per the
  register (DEF-G-003 — reconcile against the DEF-X-001 fix before a pilot); **missing** check-in, visitor and discipline
  workflows (G-035).

### 2.10 `transport` — Pilot-ready

- **Verified:** stops, routes, route versions with activation approval, passenger allocation, pass + transport fee, vehicles,
  driver assignment, trips (`campus.e2e.mjs` transport step).
- **Gaps:** rider self-service partial (G-038); transport complaints partial; driver-assignment end untested and not in UI
  (DEF-G-010). No GPS / vehicle tracking integration exists.

### 2.11 `library` — Pilot-ready

- **Verified:** library policy, membership, catalogue & copies, issue / return / renew, overdue sweep, reservations, fines billed
  through Finance with waiver decision, lost-resource workflow, library clearance, student library self-service incl. reservation
  (`campus.e2e.mjs` library + student steps).
- **Gaps:** **missing** acquisition, reading room and stock verification (G-051); some admin actions not reachable from the web
  (DEF-G-007). No OPAC / barcode / RFID hardware integration.

### 2.12 `procurement` — Pilot-ready

- **Verified:** supplier registration (GSTIN/PAN, masked bank details; bank-detail change needs an independent decision,
  SEC-P1-CONFIG-013), RFQ → quotation → award, requisition → approval → PO → goods receipt with inspection, vendor invoice with
  three-way match → approval → AP posting, vendor payments (`campus.e2e.mjs` procurement step; DEF-J-003/004/005 fixed).
- **Gaps:** award decision has no screen (DEF-J-008); three-way match not concurrency-safe (DEF-J-006); price tolerance and
  direct-sourcing limit are code constants (DEF-J-007); **no budget / commitment check** in procure-to-pay (DEF-J-010).
  **Missing:** procurement planning (J-001). **Deferred:** supplier qualification / blacklisting (J-005), emergency requisition
  (J-007), receipt correction (J-010).

### 2.13 `inventory` — Pilot-ready

- **Verified:** item master, stores and locations, stock ledger with no direct balance edit and no negative stock, stock issues
  with approval, returns, reservations, adjustments with approval, reorder level → draft requisition (`campus.e2e.mjs` inventory step).
- **Gaps:** **Deferred:** expiry / controlled stock and valuation (J-013), stock transfers / distribution / custody (J-018).

### 2.14 `assets` — Pilot-ready (limited scope)

- **Verified:** asset register from a goods-receipt line or manually, assignment / custody to staff or space, return, retirement with
  approval (`campus.e2e.mjs` assets step).
- **Gaps:** **Deferred:** location transfer / missing asset / repair (J-021), physical verification (J-022), disposal & write-off
  (J-023), asset financial accounting — capitalisation, depreciation (F-037). Sell only as an asset *register*.

### 2.15 `reporting` — Pilot-ready

- **Verified:** widget registry, dashboard create → version → decision → publish, operational reports & KPI catalogue, reporting
  framework (AISHE) → cycle → data collection → validation → snapshot → approval → submission package → acknowledgement,
  accreditation frameworks (NAAC weighted criteria), governance cases, campus / resource metrics (`campus.e2e.mjs` reports,
  regulatory and accreditation steps).
- **Gaps:** role-assigned dashboards never shown to assignees (DEF-H-002); "submitted to the regulator" is a typed reference — no
  regulator API (DEF-H-003); **missing** compliance evidence mapping (H-022) and expiry reconciliation (H-030); enterprise search off
  by default with no UI (DEF-H-004, platform).

## 3. Capabilities that are Under development or Deferred (must not be sold)

| Capability | Module | Class | Source |
|---|---|---|---|
| Statutory payroll outputs (PF/ESI/PT/TDS, Form 16) | payroll | Under development (not started in code) | DEF-E-005, E-056 |
| Multi-term CGPA incl. supplementary/improvement (tested end-to-end) | examinations | Under development (partial, untested) | I-022 |
| Exam conduct screens | examinations | Under development (API only) | E2E report §3 |
| Accounting batches; fund transfers | fees | Under development (missing) | F-026, F-035 |
| HR cases, promotion, deputation, HR compliance, faculty availability | hr | Under development (missing) | E-005, E-015, E-021, E-022, E-025 |
| Library acquisition / stock verification; hostel check-in / visitor / discipline | library, hostel | Under development (missing) | G-051, G-035 |
| Live Razorpay collection; AWS/college S3; college SMTP | fees, platform | Implemented — **live validation BLOCKED_EXTERNAL** | Stage 4 tracker |
| Budgets & cost centres; asset accounting | fees, assets | Deferred | F-027, F-037 |
| Practical & viva conduct | examinations | Deferred | I-011 |
| ABC sync, NSP scholarships, APAAR / DigiLocker | students, platform | Deferred (mock / not built) | C-037, C-049, H-001, H-002 |
| Placement, research, alumni engagement, external education integration | (not in the 15) | Deferred | H-011…H-013, H-031 |
| Health services, security operations, housekeeping, teacher-guardian scheme | (not in the 15) | Deferred (workspace drawer is a client-side fake, DEF-X-004 / G-011) | G-052…G-055 |
| Inventory valuation / transfers; asset verification / disposal; supplier qualification | inventory, assets, procurement | Deferred | J-005, J-013, J-018, J-021…J-023 |

## 4. Inventory row mapping (reproducible)

Counts in §1 come from `FEATURE_INVENTORY.csv` with this mapping: admissions C-001…C-012; students C-013…C-050;
academics D-001…D-021, D-026, D-028…D-045; attendance D-022…D-025, D-027; examinations I-001…I-036; fees F-001…F-029,
F-031…F-038, F-040; hr E-001…E-041; payroll E-042…E-056; hostel G-025…G-035; transport G-036…G-041; library G-042…G-051, J-027;
procurement J-001…J-010, J-024…J-026, F-030, F-039; inventory J-011…J-018; assets J-019…J-023; reporting H-014…H-030, J-028, G-060.
Not mapped to a sellable module (platform foundation): A-platform 45, B-institution 43, G-campus infrastructure / service desk /
labs / governance / G06–G09 34, H-services notifications / search / H01 / H04–H06 / H10 / H00 17.

## 5. What would move a module up a class

- **Pilot-ready → Production-ready (technically verified …):** close the module's "Gaps" bullets that touch core flows (decision
  screens, missing core sub-workflows), add the flow to the human-approval journey, clear its BLOCKED_EXTERNAL item where relevant
  (fees: Razorpay test-mode run with the college's keys).
- **Any class → Production-ready (absolute):** a real institution runs the module in a pilot on its own server with its own data
  and signs an acceptance (Stage 6 pilot, BLOCKED_EXTERNAL today). Until then the parenthetical qualifier is mandatory.
