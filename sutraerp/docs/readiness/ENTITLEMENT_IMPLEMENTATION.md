# Module entitlements and licence state — implementation (Part 2 · Stage 7, S7-entitle)

Branch `part2/s7-entitle` (base `part2/s4-int` fac412ce). Commercial packaging and pricing are in
`docs/readiness/COMMERCIAL_PACKAGING.md` (S7-commercial); this document is the technical design,
what is enforced where, and the evidence. Tracker: `docs/readiness/tracker/p2-s7-entitle.md`.

## 1. Design in one paragraph

One canonical registry (`catalog/entitlements.json`) defines 15 licensable modules (each mapped
to its crate, with dependencies), the always-available core, four cumulative packages, the
classification of `/api/v1/<area>/…` request paths onto modules, the data-import entity →
module map, and UI-only mirrors. The API embeds it (`include_str!`, validated at start-up and by a
unit test; an invalid registry fails closed); the web app imports the same file. A tenant's
entitlement (`platform.tenant_entitlement`: package, add-on modules, licence status, validity
window, grace days) is changed only through a maker-checker request decided by a second,
independent administrator. A server-side gate (`sutra_web::entitlement::enforce`) applies the
access rule to **every authenticated request** before any handler runs; cross-module ports and
the import jobs apply the same entitlement where the path alone cannot tell. The UI hides
inactive modules, but hiding is never the control.

## 2. Registry (`catalog/entitlements.json`)

| Module key | Name | Crate | Requires |
|---|---|---|---|
| `students` | Student records | sutra-student | — |
| `admissions` | Admissions | sutra-student | students |
| `academics` | Academics | sutra-academic | students |
| `attendance` | Attendance | sutra-academic | academics, students |
| `examinations` | Examinations | sutra-exams | academics, students |
| `fees` | Fees & finance | sutra-finance | students |
| `hr` | HR | sutra-people | — |
| `payroll` | Payroll | sutra-people | hr |
| `hostel` | Hostel | sutra-campus | students |
| `transport` | Transport | sutra-campus | students |
| `library` | Library | sutra-campus | students |
| `procurement` | Procurement | sutra-resources | — |
| `inventory` | Inventory | sutra-resources | procurement |
| `assets` | Assets | sutra-resources | — |
| `reporting` | Reporting & accreditation | sutra-services | — |

Core (never gated): **platform** (sign-in, identities, audit, inbox, notices, to-dos, documents,
data import / export framework, search, workspace catalogue, entitlements), **institution set-up**
(institution, org units, positions, campuses, curricula, programmes, calendar), **campus
facilities** (infrastructure, spaces, reservations, service desk, maintenance, labs, governance).

| Package | Modules (cumulative) |
|---|---|
| `core_academic` | students, admissions, academics, attendance, examinations |
| `academic_plus` | + fees, library, reporting |
| `institution_operations` | + hr, payroll, hostel, transport |
| `complete_erp` | + procurement, inventory, assets (every module) |

Validation (registry unit tests): unique keys, known dependencies, every package
dependency-closed, packages are supersets of each other, `complete_erp` = every activatable
module, every `api` / `imports` / `web` mapping names a real module. An activation or change
request is validated against the registry (dependency closure: `payroll` without `hr` → 422) when
made **and** when approved.

Path classification (`api` block): `student` → students (`admission*`, `duplicate-cases` →
admissions), `academic` → academics (`attendance` → attendance, `library` → library), `exams` →
examinations, `finance` → fees, `people` → hr (`payroll`, `payslip`, `salary`,
`final-settlement` → payroll), `campus` → core unless hostel / transport / library rules match,
`resources` → procurement (asset → assets; item / store / stock / replenishment → inventory),
`services` → reporting. Areas not listed (auth, platform, institution, profile, notifications,
notices, todos) are core.

Import entities (`imports` block, new in the finisher): `students`, `historical_results` →
students; `staff` → hr; `fee_heads`, `opening_balances` → fees; `campuses`, `org_units`,
`programmes`, `curricula` → core.

## 3. Behaviour by licence state

`effective_status(stored, valid_from, valid_until, grace_days, now)`: `suspended` and `expired`
are kept; before `valid_from` → expired; up to `valid_until` → active; up to
`valid_until + grace_days` → grace; after → expired. The clock is injectable (`Clock`,
`ManualClock` in tests).

| State | Active modules | Inactive module | Core | Data export | Entitlement status / admin |
|---|---|---|---|---|---|
| active | full access | `403 entitlement.module_inactive` (whatever the roles) | full | yes | yes |
| grace | full access + `Sutra-Licence-Status: grace` header (UI renewal banner) | 403 module_inactive | full | yes | yes |
| expired | reads only; writes `403 entitlement.licence_expired` | 403 module_inactive | reads only | yes (GET export) | yes (renewal) |
| suspended | `403 entitlement.licence_suspended` | same | same, except `/api/v1/platform/*` for holders of `platform.entitlement.manage` | yes | yes |

Always reachable: `/api/v1/auth/*` (sign-in / sign-out / session), the entitlement status,
catalogue and requests (so a suspended or expired tenant can see why and be renewed), and
`GET /api/v1/platform/imports/entities/{entity}/export` (no data hostage). Renewal, upgrade or
reinstatement applies to the very next request (the gate reads the state per request).

A tenant **without a stored row** gets `SUTRA_ENTITLEMENT_DEFAULT` (perpetual, active).

## 4. Configuration and production safety

| Variable | Development / test | Production |
|---|---|---|
| `SUTRA_ENTITLEMENT_STORE` | `postgres` (`memory` when `SUTRA_ENV=test`) | `postgres` only (`memory` refused) |
| `SUTRA_ENTITLEMENT_DEFAULT` | `complete_erp` when unset (dev seed, demo, CI keep every module) | **required**: the API refuses to start without it (`SUTRA_ENTITLEMENT_DEFAULT is required in production`) |

Accepted values: a package key or `none` (core only until a licence is approved — strict SaaS).
The production rule is checked last in `Config::validate` (`config_entitlement::validate_production`),
so earlier production rules keep reporting first. Rationale: what an institution without a
stored licence may use is a commercial decision the operator states; it must never silently
become `complete_erp`.

Helm: `entitlements.default` (values.yaml `complete_erp`, schema enum of the five values) is
rendered as `SUTRA_ENTITLEMENT_DEFAULT`; the render **fails** when `global.environment=production`
and the value is empty / null. Deploy workflow checks `entitlement-default-rendered-pilot` and
`production-refuses-missing-entitlement-default`.

On-premises bundle (branch `part2/s5-deploy`, `deploy/onprem/bin/sutra-onprem` writes the env
file with `SUTRA_ENV=production`): **the integrator must add `SUTRA_ENTITLEMENT_DEFAULT=<package>`
there and to `deploy/onprem/config/sutraerp.conf.example` when merging this branch**, or the
on-prem API will (correctly) refuse to start.

## 5. Enforcement points

| # | Where | What |
|---|---|---|
| 1 | `crates/web/src/entitlement.rs` `enforce` (global middleware, inside session auth, outside idempotency) | Path → module (registry) + licence state → 403 / header; store or registry failure → 503 (fail closed); a refusal is never stored as an idempotent replay |
| 2 | `crates/kernel/src/entitlement.rs` `decide` | The access rule (pure, unit-tested) |
| 3 | `modules/platform/src/entitlement.rs` | Maker-checker API; RLS-forced tables; `decided_by <> requested_by` CHECK; audit in the same transaction; version check (a request made against an older entitlement is refused) |
| 4 | `apps/api/src/entitlement_ports.rs` (**finisher**) | `EntitledFinanceClearance`: without `fees`, admission clearance and student dues are `NotApplicable`; `EntitledExamFees`: without `fees`, no examination fee is raised (`403 entitlement.module_inactive` to the port caller; the late-registration fee is skipped, the registration stands). Wired in `app::build` for the student and exams routers |
| 5 | `modules/platform/src/dataimport/http.rs` `require_entitled` (**finisher**) | Import upload, mapping, mapping-profile save, dry run, commit and commit decision of an entity whose module is inactive → `403 entitlement.module_inactive`; the entity list hides those entities; template / job read / export stay available |
| 6 | `crates/kernel/src/error.rs` `KernelError::Entitlement` (**finisher**) | The same `403` / `errorCode` / title as the gate for refusals raised inside handlers and ports |
| 7 | `apps/web/app/lib/entitlements.ts`, `app-shell.tsx`, `LicenceBanner.tsx` | UI only: navigation / palette / routes filtered by active modules; grace / expired / suspended banner |

### Modules that read another module's facts (gap closed by the finisher)

Admissions (financial clearance before confirmation), student registration holds, document
release, exit clearance and the examination hall-ticket entry condition ("dues cleared") read
Finance facts through `FinanceClearancePort`, and examinations raise late-registration /
revaluation fees through `ExamFeePort`. The path gate never sees these calls. On a Core
Academic tenant, dues left in Finance from before a downgrade (or raised by another path) would
have blocked admission confirmation and withheld hall tickets with no way to settle them (the
fees API is refused).

**Decision:** correct behaviour, not a registry dependency. Making `fees` a dependency of
`admissions` / `examinations` would make Core Academic unsellable to institutions that collect
fees outside SutraERP. Without `fees`, *no Finance obligation applies*: the ports answer
`NotApplicable` (the evidence an institution that never raised a charge already gets), so the
unchanged Domain C / Domain I rules let the student through; the evidence stored on the
admission / hall ticket records `notApplicable`. With `fees` active nothing changes. The
entitlement lookup fails closed (a store error is returned, never read as "no fees").

## 6. Known limits

1. **Self-hosted self-licensing.** On an on-premises install the operator controls the database
   and the environment, so an administrator pair (or the DBA) can grant any package. Licence
   enforcement on self-hosted installs needs a **signed licence file** (vendor-signed package,
   validity and installation id, verified by the API at start-up and per request) **or a
   contractual control** (audit rights). Not built; SaaS / vendor-hosted tenants are fully
   enforced because the operator is the vendor.
2. **Campus sub-tabs are UI-only.** The `campus` area defaults to core; hostel, transport and
   library are gated server-side by path rules. Campus *facility* screens that embed module
   data in sub-tabs are filtered in the UI only; any API they call is still classified and gated.
3. **Revaluation without fees.** On a tenant without `fees`, a revaluation / rechecking request
   cannot raise its fee and stays `eligibility_verified` (no fee-waived path yet). Late
   registration is unaffected (the fee is skipped). Follow-up: a "no fee applies" transition.
4. **Other fee-raising ports.** Campus charges (hostel / transport / library fines →
   `PgCampusCharges`) are reachable only when those modules are active, and every package that
   contains them contains `fees`; an add-on combination without `fees` (e.g. core_academic +
   hostel) would raise charges the tenant cannot see. The registry does not forbid that
   combination; commercial rule: sell hostel / transport / library with `fees`.
5. Suspended tenants: platform administrators keep `/api/v1/platform/*`, which includes the
   import job endpoints of *active* modules (the import gate checks the module, not the
   suspension; imports are writes and expired tenants are refused by the path gate).
6. Default for tenants without a row is a deployment setting, not per tenant; a per-tenant
   default needs a stored row (approve one through the API).

## 7. Test evidence

| Test | Kind | What it proves |
|---|---|---|
| `sutra_kernel::entitlement::tests` (registry valid & complete, invalid registries rejected, dependency validation, status machine, decide matrix, **import entities map onto modules**) | Rust unit | registry, licence state machine, access rule |
| `sutra_web::entitlement::tests` (4) | Rust unit (axum) | default complete_erp, inactive module 403 for an administrator, grace → expired → renewal on an injected clock, suspended leaves export + status |
| `sutra_api::config_entitlement::tests` (2, **production requires an explicit default**) | Rust unit | config parsing, memory store refused in production, production refuses to start without `SUTRA_ENTITLEMENT_DEFAULT`, dev keeps complete_erp |
| `sutra_api::entitlement_ports::tests` (3) | Rust unit | no Finance obligation without fees (Finance never consulted), real evidence with fees, fails closed, no exam fee raised without fees |
| `db_p2_entitle` (8) | Rust integration, real PostgreSQL | module gate vs role; dependency / validation rejections; expiry read-only + export + renewal; suspension; tenant isolation + forced RLS; SoD + audit; **core_academic admission → confirmation → student and hall-ticket release with outstanding Finance charges (control: both blocked while fees is active)**; **import jobs gated per target module (upload / mapping / profile / dry-run / commit / decision 403, entity list filtered, students / set-up imports and exports available, HR add-on reopens)** |
| `auth_hardening::every_route_requires_a_session_and_a_permission` | Rust integration | every route needs a session; `GET /platform/entitlement` and `/catalogue` are session-only by design (own tenant's state / public catalogue) |
| `apps/web/app/lib/entitlements.test.ts` | vitest | nav / route / workspace filtering, licence banner model |
| `apps/web/e2e/ui.e2e.mjs` (entitlement step) | Playwright | the shell reads the entitlement and shows every module on the default |
| helm render checks in `sutraerp-deploy.yml` | CI | production render refuses a missing default |

### Run evidence (branch `part2/s7-entitle`)

| Workflow | Run | Commit | Result |
|---|---|---|---|
| SutraERP CI (Rust fmt·clippy·test, Rust integration incl. all 8 `db_p2_entitle`, Web, OpenAPI drift, 9 E2E suites) | 38950392871 | `40fdba67` | green |
| SutraERP deploy (Helm lint/template incl. production default render check, compose boot, kind install·migrate·smoke·telemetry·DR) | 38950396576 | `40fdba67` | green |
| SutraERP Part 1 demo (full stack, Keycloak) | 38950399731 | `40fdba67` | green |
| SutraERP quality gates (cargo-deny/audit, pnpm audit, gitleaks, Semgrep, Trivy/Grype/ZAP, k6) | 38952022358 | `4d59b467` | green |

`4d59b467` differs from `40fdba67` only by `.gitleaks.toml` + `docs/security/findings.md`
(SEC-P2-ENT-001: gitleaks scans every ref and flagged a shell variable reference in
`part2/s5-monitor`'s drill script; narrow path AND literal allow-list). Earlier red runs and
their causes: CI 38949593345 (`7f175623`, unused binding in `db_p2_entitle` under
`-D warnings`, fixed in `40fdba67`), quality 38950403058 (`40fdba67`, the gitleaks finding above).

### Reviewed security exception — session-only entitlement reads

`GET /api/v1/platform/entitlement` and `GET /api/v1/platform/entitlement/catalogue` are on the
`auth_hardening` **session-only** list (`apps/api/tests/auth_hardening.rs`): any signed-in user
may read them without a permission. Reviewed and accepted because both are **read-only** and
**tenant-scoped**: the status endpoint returns only the caller's own tenant's active modules and
licence state (the tenant comes from the session, never from the request), and the catalogue is
the public module/package registry with no tenant data. Every signed-in user needs them so the
shell can hide inactive modules and explain a `403 entitlement.*`. All changes (requests,
decisions, withdrawals) stay behind `platform.entitlement.manage` / `platform.entitlement.approve`
with maker-checker SoD. The test still requires a session for both routes.

Tracker: `docs/readiness/tracker/p2-s7-entitle.md`.
