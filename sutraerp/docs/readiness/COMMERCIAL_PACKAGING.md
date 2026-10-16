# SutraERP — commercial packaging (Part 2 · Stage 7, S7-commercial)

Branch `part2/s7-commercial` (base `part2/s4-int` @ `fac412ce`). Docs only.
Module classes come from [MODULE_CATALOGUE.md](MODULE_CATALOGUE.md); prices from [PRICING_AND_UNIT_ECONOMICS.md](PRICING_AND_UNIT_ECONOMICS.md);
support terms from [SUPPORT_AND_MAINTENANCE.md](SUPPORT_AND_MAINTENANCE.md).

> **Registry alignment.** The entitlement registry and `ENTITLEMENT_IMPLEMENTATION.md` (branch `part2/s7-entitle`) were **not on
> the remote** when this was written (checked with `git ls-remote` on 2026-10-16 ~22:45 IST). This document therefore uses the
> agreed package keys `core_academic`, `academic_plus`, `institution_operations`, `complete_erp` and module keys `admissions`,
> `students`, `academics`, `attendance`, `examinations`, `fees`, `hr`, `payroll`, `hostel`, `transport`, `library`, `procurement`,
> `inventory`, `assets`, `reporting`. When the registry lands, **the registry is authoritative for keys, contents and
> dependencies**, and this document must be reconciled to it (open item in the tracker).

> **No production claim.** Every package below contains Pilot-ready modules. Packages are offered as **"Pilot-ready; technically
> verified components; institution acceptance pending"**. Sales material must not say "production-ready", "go-live ready" or
> "proven at colleges".

## 1. Packages at a glance

| Package key | Name | Modules | Best for | Readiness of contents |
|---|---|---|---|---|
| `core_academic` | Core Academic | admissions, students, academics, attendance, examinations, fees | a college replacing spreadsheets for the student lifecycle and fee collection | 2 technically verified (admissions, attendance) + 4 Pilot-ready |
| `academic_plus` | Academic Plus | `core_academic` + library, hostel, transport, reporting | residential / larger colleges that also need campus services and NAAC/AISHE reporting | 2 technically verified + 8 Pilot-ready |
| `institution_operations` | Institution Operations (add-on) | hr, payroll, procurement, inventory, assets | the back office (people, pay, purchasing, stores, asset register) | 5 Pilot-ready (assets limited scope) |
| `complete_erp` | Complete ERP | all 15 modules (= `academic_plus` + `institution_operations`) | a college wanting one system for everything above | 2 technically verified + 13 Pilot-ready |

## 2. Platform foundation (included in every package, not licensed separately)

Institution / campus / programme / curriculum / calendar set-up (Domain B); sign-in with Keycloak OIDC, TOTP MFA for privileged
roles, identity provisioning and sync (S4-idp); Flowable human approval engine — production refuses auto-approval
(DEF-X-001 fixed) and maker ≠ checker is enforced on 99 decision endpoints (82 ledger-guarded + 17 status-history-guarded; DEF-X-002
fixed, [SECURITY_AUDIT.md](SECURITY_AUDIT.md)); forced row-level tenant isolation (DEF-X-003 fixed); audit log with chain
verification; in-app + e-mail notifications; document vault with SHA-256 integrity, retention and legal hold (S4-docs); data import
(CSV/XLSX, dry run, approval, reconciliation) and export (CSV/XLSX, formula-injection safe) for 9 targets — campuses, org units,
programmes, curricula, staff, students (+ guardian), historical results, fee heads, opening fee balances (S4-import); on-prem
offline installer, upgrade / rollback, backup + WAL archiving + point-in-time restore, monitoring with 47 alerts (Stage 5, measured
on a GitHub runner — not college hardware).

Platform items that are **not** included / not available: enterprise search UI (DEF-H-004), role administration beyond the shipped
roles (DEF-A-003, L2-A governance gaps), APAAR / DigiLocker / ABC / NSP integrations (Deferred), mobile apps (none exist),
SMS / WhatsApp messaging to students or parents (not implemented — only an Alertmanager SMS *webhook* leg for ops alerts).

## 3. Package contents and licensed functionality

"Licensed functionality" = what the entitlement makes writable for the tenant while the licence is active. Every licensed module is
also **readable** (and exportable) after expiry — see §6.

### 3.1 `core_academic`

| Module | Licensed functionality (verified) | Not included / known gaps (from the catalogue) |
|---|---|---|
| admissions | intake, review, offer, acceptance, documents, governed approval, financial clearance, confirmation, student creation | duplicate-case resolution UI |
| students | enrolment, Student 360, documents & certificates, registration windows + (self-)registration, holds, credit ledger, guardians, scholarships, completion, alumni transition | decision screens for transfers / corrections / progression (API only); ABC & NSP sync (Deferred) |
| academics | offerings, sections, enrolment, waitlists, faculty assignment, timetable, monitoring, flexible learning & credit recognition | coursework marking in browser; offering-governance screens |
| attendance | sessions, capture / finalise, policy & thresholds, governed corrections, self-views | device / biometric capture |
| examinations | policy → session → papers → schedule, registration, eligibility, hall tickets, marks, verification, results, SGPA, publication, grade cards, revaluation | exam-conduct screens, CGPA across terms (partial), practical / viva (Deferred) |
| fees | fee heads / structures, charges, instalments, counter collection & receipts, refunds, adjustments, ledger, trial balance, periods, receivables / dunning, aid, bank statement import; **online payment (Razorpay) code path** | live Razorpay validation (BLOCKED_EXTERNAL — enabled per college only after the go-live validation add-on, §5); fee-structure and other finance decision screens; budgets (Deferred) |

### 3.2 `academic_plus` = `core_academic` +

| Module | Licensed functionality (verified) | Not included / known gaps |
|---|---|---|
| library | policy, membership, catalogue / copies, circulation, overdue, reservations, fines via Finance, lost resources, clearance, student self-service | acquisition, stock verification, reading room; barcode/RFID hardware |
| hostel | policy, rooms / beds, application → allocation + fee, leave, inspection → damage charge, mess, complaints, exit clearance | check-in, visitors, discipline |
| transport | stops, routes with approval, passengers, passes + fee, vehicles, drivers, trips | rider self-service (partial); GPS tracking |
| reporting | dashboards with approval, KPI reports, AISHE-style reporting cycle → submission package, NAAC accreditation frameworks / cycles / records, governance cases | regulator API submission; evidence mapping; role-assigned dashboard delivery |

### 3.3 `institution_operations` (add-on; requires `core_academic`)

| Module | Licensed functionality (verified) | Not included / known gaps |
|---|---|---|
| hr | staff records, engagements, positions, recruitment, onboarding, leave (policy, requests, accrual, carry-forward), staff attendance, workload, My HR | HR cases, promotion, deputation, compliance; ~66 endpoints without screens |
| payroll | configuration, structures, runs (calc incl. loss of pay), validation, approval, finalisation, Finance posting, payslips, NEFT disbursement file, final settlement, retro pay | **statutory outputs (PF/ESI/PT/TDS, Form 16) — not available**; bank transmission manual |
| procurement | suppliers (masked bank, governed bank change), RFQ → award, requisition, PO, goods receipt, three-way match, vendor invoice approval, AP posting, vendor payments | procurement planning, budget check, award screen; supplier qualification (Deferred) |
| inventory | items, stores, stock ledger, issues, returns, reservations, adjustments, reorder → requisition | valuation, expiry, transfers (Deferred) |
| assets | register (from goods receipt or manual), custody, return, retirement | transfers, verification, disposal, depreciation (Deferred) |

### 3.4 `complete_erp`

All of 3.1–3.3 under one licence, at a bundle price below the sum of `academic_plus` + `institution_operations`.

## 4. Dependencies (what the entitlement must enforce)

Module → module dependencies observed in the inventory and evidence (a module cannot be licensed without the modules it depends on):

| Module | Depends on | Why (evidence, inventory ids) |
|---|---|---|
| admissions | students, fees | admission confirmation needs financial clearance from Finance evidence (C-005, F-008 clearance port) and creates the student (C-007) |
| students | academics | registration selects offerings (C-024) |
| attendance | academics | sessions belong to sections / offerings (D-022) |
| examinations | academics, attendance, fees | papers per offering; eligibility uses attendance thresholds and fee dues (I-009); late registration and revaluation fees charge via Finance (I-008, I-029) |
| hostel, transport | fees, students | allocation raises a Finance charge (G-027, G-037) |
| library | fees, students | fines billed through Finance (G-047); student members |
| reporting | (platform) | reads the metrics ports of the modules that are licensed (J-028, G-060) |
| payroll | hr, fees (ledger) | runs use staff / engagements / leave; posting to the Finance ledger (E-049) |
| procurement | fees (ledger), inventory | AP posting via the procurement posting port (J-025); goods receipt lands stock (J-009) |
| inventory | procurement (for receipts) | receipts come from procurement |
| assets | procurement | assets created from goods-receipt lines (J-019) |

Consequences for packaging: `core_academic` is the minimum sellable unit (fees is inside it, so the ledger is always present);
`institution_operations` therefore **requires** `core_academic`; à-la-carte modules (§5) require `core_academic`; `inventory` and
`assets` are only sold together with `procurement`.

## 5. Add-ons

| Add-on | Type | Requires | Status / note |
|---|---|---|---|
| Single module à la carte: library, hostel, transport, reporting | annual licence | `core_academic` | modules Pilot-ready |
| HR + Payroll only (without procurement/inventory/assets) | annual licence | `core_academic` | Pilot-ready; statutory outputs not available |
| Procurement + Inventory + Assets only | annual licence | `core_academic` | Pilot-ready |
| Online fee collection go-live (Razorpay) | one-time service | `fees` | performs the BLOCKED_EXTERNAL validation with the college's own test then live keys, webhook URL and a real settlement export; gateway fees are paid by the college to Razorpay |
| Additional institution / campus (same tenant) | annual licence uplift | any package | multi-institution is supported by the tenant model; per-institution pricing |
| Data migration beyond the included scope | one-time service | any | uses the import engine (9 targets); other entities by quotation |
| Additional training days | one-time service | any | onsite / remote |
| Priority or 24×7 support | annual | any | see SUPPORT_AND_MAINTENANCE.md |
| Managed hosting (optional) | monthly | any | **not yet drilled on a cloud provider** — offered only after a hosting drill (COMMERCIAL_READINESS.md) |
| Monitoring alert channels (college SMTP, chat, SMS gateway, dead-man service) | one-time service | any | BLOCKED_EXTERNAL until the college provides relay / webhook / SMS provider |
| Custom reports, integrations, customisation | time & material | any | e.g. Tally export, university result-portal export, LDAP/AD federation via Keycloak (not tested — scoped on request) |
| Staging / training environment | annual | any | second install of the same bundle |

**Not offerable as add-ons today** (Deferred / not built): ABC, NSP, APAAR / DigiLocker, placement, research, alumni engagement,
health / security / housekeeping operations, mobile apps, SMS / WhatsApp messaging to students and parents, budgets, asset
depreciation.

## 6. Licence lifecycle (aligned with the entitlement design)

| State | Behaviour of licensed modules | Notes |
|---|---|---|
| Active | read + write per role permissions | — |
| Expired | **read-only**: users can sign in, view and export records of the expired modules; no new transactions, no approvals, no imports | as specified for the entitlement work (`part2/s7-entitle`, in progress); exact grace period / banner behaviour to be confirmed against ENTITLEMENT_IMPLEMENTATION.md |
| Module removed from the licence | same as expired, for that module only; dependent modules stay licensed only if their dependencies are licensed (§4) | — |
| Terminated | data export (offboarding in SUPPORT_AND_MAINTENANCE.md); self-hosted data never leaves the college server | — |

Read-only-after-expiry means the vendor never holds a college's records hostage: the commercial and offboarding terms must keep it
that way.
