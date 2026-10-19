# SutraERP — commercial packaging (Part 2 · Stage 7, S7-commercial)

Branch `part2/s7-commercial` (base `part2/s4-int` @ `fac412ce`). Docs only.
Module classes come from [MODULE_CATALOGUE.md](MODULE_CATALOGUE.md); prices from [PRICING_AND_UNIT_ECONOMICS.md](PRICING_AND_UNIT_ECONOMICS.md);
support terms from [SUPPORT_AND_MAINTENANCE.md](SUPPORT_AND_MAINTENANCE.md).

> **Source of truth for packages = the entitlement registry.** Packages, module keys and declared dependencies below are copied
> from `sutraerp/catalog/entitlements.json` on branch `part2/s7-entitle` @ `bf48b7f6` (registry `version: 1`, pushed
> 2026-10-16 17:32 UTC; its SutraERP CI run 38940451926 was still **in progress** when this was written, and
> `ENTITLEMENT_IMPLEMENTATION.md` was not yet on that branch). If the registry changes, the registry wins and this document must
> be updated. Functional dependencies the registry does **not** encode are listed in §4.2 as reconciliation items.

> **No production claim.** Every package contains Pilot-ready modules. Packages are offered as **"Pilot-ready; technically
> verified components; institution acceptance pending"**. Sales material must not say "production-ready", "go-live ready" or
> "proven at colleges".

## 1. Packages at a glance (cumulative tiers, registry `packages`)

| Package key | Name (registry) | Modules (registry) | Adds over the previous tier | Readiness of contents ([MODULE_CATALOGUE.md](MODULE_CATALOGUE.md)) |
|---|---|---|---|---|
| `core_academic` | Core Academic — "Student lifecycle and academic delivery" | students, admissions, academics, attendance, examinations | — | 2 Production-ready (technically verified; institution acceptance pending): admissions, attendance · 3 Pilot-ready |
| `academic_plus` | Academic Plus — "Core Academic plus fees, library and reporting" | `core_academic` + fees, library, reporting | fees, library, reporting | 2 technically verified · 6 Pilot-ready |
| `institution_operations` | Institution Operations — "Academic Plus plus HR, payroll, hostel and transport" | `academic_plus` + hr, payroll, hostel, transport | hr, payroll, hostel, transport | 2 technically verified · 10 Pilot-ready |
| `complete_erp` | Complete ERP — "Every module, including procurement, inventory and assets" | all 15 | procurement, inventory, assets | 2 technically verified · 13 Pilot-ready (assets limited scope) |

Registry `defaultPackage` = `complete_erp`: a tenant with no stored entitlement row gets every module
(`SUTRA_ENTITLEMENT_DEFAULT`, `apps/api/src/config_entitlement.rs`). A commercial deployment should set
`SUTRA_ENTITLEMENT_DEFAULT=none` or store the purchased entitlement before go-live, otherwise the college silently receives
`complete_erp`. *(Integrator note, `part2/int`: the final S7-entitle code no longer falls back silently in production — the API
refuses to start without an explicit `SUTRA_ENTITLEMENT_DEFAULT`; Helm fails the render without `entitlements.default`; the on-prem
`sutraerp.conf.example` ships `complete_erp` and `sutra-onprem render` records `complete_erp` with a warning for installations that
predate entitlements. The packages in §1 are unchanged between `bf48b7f6` and the final registry `0fc56a05`, which only added the
`imports` entity → module map.)* Add-on modules on top of a package are supported by the entitlement model (`addOnModules`, validated against the
dependency closure).

## 2. Always-available core (registry `core`; not licensed separately)

| Core key | Contents (registry description) | Verified basis |
|---|---|---|
| `platform` | sign-in, identities, audit, inbox, notices, to-dos, documents, data import / export, search, workspace catalogue, entitlements | Keycloak OIDC + TOTP MFA (S4-idp), Flowable human approvals with maker ≠ checker on 99 guarded decision endpoints (82 ledger + 17 status-history; [SECURITY_AUDIT.md](SECURITY_AUDIT.md)), forced tenant RLS, document vault (S4-docs), import / export for 9 targets (S4-import), entitlement gate (S7-entitle, in CI) |
| `institution` | institution, organisation units, positions, campuses, curricula, programmes, academic calendar | Domain B inventory (37/43 implemented, Stage 1) + journey seed |
| `campus_facilities` | infrastructure, spaces, room reservations, service desk, maintenance, laboratories, campus governance | `campus.e2e.mjs` infrastructure / booking / service-desk steps; several G10 / G01 / G02 sub-workflows missing (FEATURE_GAPS.md) |

Also included with every installation: the Stage 5 on-prem tooling (offline installer, upgrade / rollback, backup + WAL + PITR,
monitoring with 47 alerts — measured on a GitHub runner, not college hardware).

Not available in any package (Deferred / not built): enterprise search UI (DEF-H-004), role administration beyond the shipped roles
(DEF-A-003), APAAR / DigiLocker / ABC / NSP integrations, mobile apps, SMS / WhatsApp messaging to students or parents.

## 3. Licensed functionality per tier

"Licensed functionality" = what the entitlement gate makes writable while the licence is active or in grace. Requests to an
unlicensed module answer `403 entitlement.module_inactive` (server-side gate `sutra_web::entitlement`; the web app only mirrors it).

### 3.1 `core_academic`

| Module | Licensed functionality (verified) | Known gaps (catalogue) |
|---|---|---|
| students | enrolment, Student 360, documents & certificates, registration windows + (self-)registration, holds, credit ledger, guardians, scholarships, completion, alumni transition | decision screens for transfers / corrections / progression (API only); ABC & NSP sync (Deferred) |
| admissions | intake, review, offer, acceptance, documents, governed approval, financial clearance, confirmation, student creation | duplicate-case resolution UI; **financial clearance reads Finance evidence — see §4.2 R1** |
| academics | offerings, sections, enrolment, waitlists, faculty assignment, timetable, monitoring, flexible learning & credit recognition | coursework marking in browser; offering-governance screens |
| attendance | sessions, capture / finalise, policy & thresholds, governed corrections, self-views | device / biometric capture |
| examinations | policy → session → papers → schedule, registration, eligibility, hall tickets, marks, verification, results, SGPA, publication, grade cards, revaluation | exam-conduct screens; CGPA across terms (partial); practical / viva (Deferred); **fee-dues eligibility and revaluation fees use Finance — §4.2 R2** |

### 3.2 `academic_plus` adds

| Module | Licensed functionality (verified) | Known gaps |
|---|---|---|
| fees | fee heads / structures, charges, instalments, counter collection & receipts, refunds, adjustments, ledger, trial balance, periods, receivables / dunning, aid, bank statement import; **online payment (Razorpay) code path** | live Razorpay validation BLOCKED_EXTERNAL — online collection enabled per college only after the go-live add-on (§5); fee-structure and other finance decision screens (DEF-F-008); budgets (Deferred) |
| library | policy, membership, catalogue / copies, circulation, overdue, reservations, fines via Finance, lost resources, clearance, student self-service | acquisition, stock verification, reading room; barcode / RFID hardware |
| reporting | dashboards with approval, KPI reports, AISHE-style reporting cycle → submission package, NAAC accreditation frameworks / cycles / records | regulator API submission; evidence mapping; role-assigned dashboard delivery (DEF-H-002) |

### 3.3 `institution_operations` adds

| Module | Licensed functionality (verified) | Known gaps |
|---|---|---|
| hr | staff records, engagements, positions, recruitment, onboarding, leave (policy, requests, accrual, carry-forward), staff attendance, workload, My HR | HR cases, promotion, deputation, compliance; ~66 endpoints without screens (DEF-E-006) |
| payroll | configuration, structures, runs (calc incl. loss of pay), validation, approval, finalisation, Finance posting, payslips, NEFT disbursement file, final settlement, retro pay | **statutory outputs (PF/ESI/PT/TDS, Form 16) not available** (DEF-E-005); bank transmission manual |
| hostel | policy, rooms / beds, application → allocation + fee, leave, inspection → damage charge, mess, complaints, exit clearance | check-in, visitors, discipline (G-035) |
| transport | stops, routes with approval, passengers, passes + fee, vehicles, drivers, trips | rider self-service (partial); GPS tracking |

### 3.4 `complete_erp` adds

| Module | Licensed functionality (verified) | Known gaps |
|---|---|---|
| procurement | suppliers (masked bank, governed bank change), RFQ → award, requisition, PO, goods receipt, three-way match, vendor invoice approval, AP posting, vendor payments | procurement planning, budget check (DEF-J-010), award screen (DEF-J-008); supplier qualification (Deferred) |
| inventory | items, stores, stock ledger, issues, returns, reservations, adjustments, reorder → requisition | valuation, expiry, transfers (Deferred) |
| assets | register (from goods receipt or manual), custody, return, retirement | transfers, verification, disposal, depreciation (Deferred) — the registry description "verification, disposal" overstates what is built |

## 4. Dependencies

### 4.1 Declared in the registry (enforced: a package or add-on set that lacks a dependency is rejected)

| Module | `requires` (registry) |
|---|---|
| students, hr, procurement, assets, reporting | — |
| admissions, academics, fees, hostel, transport, library | students |
| attendance, examinations | academics, students |
| payroll | hr |
| inventory | procurement |

### 4.2 Functional dependencies NOT in the registry — reconciliation items

Observed in the feature inventory; the registry treats them as optional. The entitlement gate classifies HTTP paths only; in-process
port calls between modules are not gated, so these cases need a test before they are sold:

| Id | Module (package) | Uses | Evidence | Risk if the dependency is not licensed | Recommendation |
|---|---|---|---|---|---|
| R1 | admissions (`core_academic`) | fees | C-005 "Financial clearance from Finance evidence", C-002 pre-student charge context F-008 | a `core_academic` college cannot raise or collect fee charges, so admission financial clearance may block confirmation — **untested** | test admission confirmation with `fees` inactive; either add `fees` to `core_academic` or document / build a no-fee clearance path. Until tested, sell `core_academic` only with the `fees` add-on |
| R2 | examinations (`core_academic`) | fees, attendance | I-009 eligibility = fee dues + attendance; I-008 late fee; I-029 revaluation fee | without `fees`, dues checks and fee-bearing steps behave untested | same test; attendance is already required |
| R3 | hostel, transport, library | fees | G-027, G-037 raise Finance charges; G-047 fines billed through Finance | within `institution_operations` fees is always present; as add-ons to `core_academic` they need `fees` | make `fees` a prerequisite for these add-ons commercially (§5) |
| R4 | payroll, procurement | fees (ledger) | E-049 payroll posting; J-025 AP posting via the procurement posting port | always satisfied inside the tiers; matters only for add-on combinations | add-ons require `academic_plus` or `fees` |
| R5 | assets | procurement | J-019 assets from goods-receipt lines (manual entry also possible) | works manually without procurement | none |

## 5. Add-ons

| Add-on | Type | Commercial prerequisite | Status / note |
|---|---|---|---|
| Single module on a lower tier (registry `addOnModules`), e.g. hostel on `academic_plus`, hr on `academic_plus` | annual licence | registry dependencies **plus** §4.2 (fees for hostel / transport / library / payroll / procurement) | modules Pilot-ready |
| `fees` on `core_academic` | annual licence | — | recommended until R1 / R2 are tested |
| Online fee collection go-live (Razorpay) | one-time service | `fees` | performs the BLOCKED_EXTERNAL validation with the college's test then live keys, webhook URL and a real settlement export; gateway fees are paid by the college to Razorpay |
| Additional institution / campus (same tenant) | annual licence uplift | any | multi-institution supported by the tenant model |
| Data migration beyond the included scope | one-time service | any | import engine (9 targets); other entities by quotation |
| Additional training days | one-time service | any | onsite / remote |
| Priority or Critical 24×7 support | annual | any | SUPPORT_AND_MAINTENANCE.md |
| Managed hosting (optional) | monthly | any | **not drilled on a cloud provider** — not offerable until a hosting drill passes (COMMERCIAL_READINESS.md) |
| Monitoring alert channels (college SMTP, chat, SMS gateway, dead-man) | one-time service | any | BLOCKED_EXTERNAL until the college provides relay / webhook / SMS provider |
| Custom reports, integrations, customisation | time & material | any | e.g. Tally export, university result-portal export, LDAP/AD via Keycloak (not tested — scoped on request) |
| Staging / training environment | annual | any | second install of the same bundle |

**Not offerable** (Deferred / not built): ABC, NSP, APAAR / DigiLocker, placement, research, alumni engagement, health / security /
housekeeping operations, mobile apps, SMS / WhatsApp messaging, budgets, asset depreciation.

## 6. Licence lifecycle (as implemented on `part2/s7-entitle`, CI in progress)

| Status | Behaviour (kernel `decide`, `crates/kernel/src/entitlement.rs`) |
|---|---|
| `active` | licensed modules read + write per role permissions; unlicensed modules `403 entitlement.module_inactive` |
| `grace` | past `validUntil` but within `graceDays` (0–365): full access + renewal banner |
| `expired` | after the grace period (or before `validFrom`): **reads and data export kept, writes refused** (`403 entitlement.licence_expired`) |
| `suspended` | administrative: only the platform administration area (platform administrators) and data export (`403 entitlement.licence_suspended`) |

Data export (`GET /api/v1/platform/imports/entities/{entity}/export`) and sign-in / sign-out / entitlement status are always
reachable. Every entitlement change (activate · change · renew · suspend · reinstate) is a **maker-checker request between two
administrators of the tenant** (`platform.entitlement.manage` / `.approve`, step-up, audited), applied on the next request.

**Commercial consequence for self-hosted installations:** there is no vendor-signed licence key — the college's own administrators
apply the purchased entitlement. Licence compliance is therefore **contractual** (order form, entitlement statement, annual
student-count true-up, audit log of entitlement changes available to the vendor on request), not technically enforced against the
customer. Proposed commercial terms: grace period 30 days on renewal; the vendor's renewal invoice states the exact entitlement
(package, add-ons, `validFrom` / `validUntil`, `graceDays`) the college must apply. A signed licence token would be a product
change (not planned in this stage).
