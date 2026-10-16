# SutraERP — Part 1 security audit (Stage 3)

Integration branch `part1/s3-int` (from `part1/s2-int` @ `1fd0e322`, Stage 2 complete) with the five Stage 3
security workstreams merged (no-ff, in this order): `part1/s3-appsec` @ `0689003b`, `part1/s3-tenant` @ `7acc932d`,
`part1/s3-appr` @ `1f3ea7cd`, `part1/s3-auth` @ `1b842403`, `part1/s3-config` @ `6b2d3444`.
Final integrated head and its runs: see [Evidence](#evidence-runs). Fix-by-fix detail: [SECURITY_FIXES.md](SECURITY_FIXES.md).
Per-area findings tables (source of this report): [security/tenant.md](security/tenant.md), [security/auth.md](security/auth.md),
[security/appr.md](security/appr.md), [security/config.md](security/config.md), [security/appsec.md](security/appsec.md).

## Scope

| Area | Owner | What was reviewed |
|---|---|---|
| Tenant / institution isolation | S3-tenant | every table with `tenant_id` (386), id-only SQL, institution scope from URL path (DEF-X-003, C-010), runtime DB role, faculty access to unpublished exam data (I-006) |
| Authentication / session | S3-auth | MFA for privileged roles, OIDC token validation, sign-in / OTP guessing (SEC-9D-005), Keycloak realm, permission-before-read on every route (sweep of all OpenAPI operations), production OIDC config, API headers / HSTS (SEC-9D-006 / 012) |
| Approvals / separation of duties | S3-appr | all 125 decision endpoints (maker ≠ checker), ledger integrity, system / anonymous deciders, approval bypass by posting or editing, enrolment eligibility override (DEMO-006) |
| Secrets / configuration / transport | S3-config | production start-up gate (dev switches, weak / published secrets, key reuse, TLS), mock payment gateway (F-002), CORS, rate limiting, body / time limits, ops endpoints, SPA CSP (SEC-9D-011), supplier bank-detail change (J-004) |
| Application security (OWASP) | S3-appsec | SQL construction, LIKE injection, stored XSS via links, error-detail leaks, CSV formula injection, DB-level immutability of ledger / audit tables, CI security gates (SAST) |

Out of scope (Part 2 / external): real bank / payment-gateway APIs, penetration test by a third party, infrastructure (cloud IAM,
cluster hardening beyond the Helm chart), mobile clients.

## Method

1. Inputs: `DEFECT_REGISTER.md` (Stage 1), `docs/security/findings.md` (open SEC-9D-004/005/006/012), `APPROVAL_CONTROL_MATRIX.md`.
2. Code review per area with exploit sketches; each fix lands with an **attack-style regression test** that fails before and passes
   after (Rust DB tests on real PostgreSQL 18, API tests through the full router, unit tests, Playwright e2e, Helm render checks).
3. Fitness tests that keep the control from regressing: `approval_matrix` (every decision endpoint guarded or documented),
   `auth_hardening::every_route_requires_a_session_and_a_permission` (all OpenAPI operations, anonymous + role-less session),
   `db_p1_s3_tenant::rls_catalog_covers_every_tenant_table` + `deploy/ci/db-integrity.sql` (`platform.tenant_rls_gaps`).
4. Automated scans in `sutraerp-quality.yml` (gitleaks full history, cargo-deny, cargo-audit, pnpm audit, Semgrep SAST, image scan + ZAP).
5. Full-stack verification: Part 1 demo (compose · Keycloak OIDC · human approval mode · journey · DB integrity), Helm/kind deploy, seeded pilot —
   the API serves as the non-superuser runtime role everywhere (RLS cannot be bypassed in tests).
6. CI-first: no local builds; every result below is a GitHub Actions run.

## Scans

| Scanner | Result | Run(s) | Exceptions (justified) |
|---|---|---|---|
| gitleaks 8.30.1 (full history, all branches) | no leaks | quality 38767374109 (appsec), 38774948589 (auth), 38775422497 (config), final: 38862951620 | two history-only fake config-test fixtures in `2f630ac7` allow-listed by exact literal (SEC-P1-CONFIG-010 / APPSEC-008); pre-existing dev-default / OIDC test-key allow-lists |
| Semgrep 1.181.0 (p/rust, p/typescript, p/react, p/javascript, repo rules) | 0 findings (87 rules, 1,063 files) | 38765548100, 38767374109, final: 38862951620 | one inline `nosemgrep` (APPSEC-007, argv subcommand) |
| cargo-deny / cargo-audit | pass / 0 vulnerabilities | same quality runs | RUSTSEC-2023-0071 (rsa via unused sqlx-mysql), RUSTSEC-2024-0436 (paste, proc-macro) — pre-existing SEC-9D-001/002, expire 2027-04-14 |
| pnpm audit `--prod --audit-level high` | 0 | same quality runs | none |
| Image scan + ZAP baseline | green | same quality runs | — |
| Route authorisation sweep | 1,706 operations checked; 24 read-before-permission routes fixed; 0 open | CI 38774880706 (auth), final CI | 15 public routes, each justified in security/auth.md |
| Approval matrix | 125 decision endpoints: 82 ledger-guarded, 17 status-history-guarded, 26 allowlisted with reasons | final CI | APPR-007 |
| Tenant RLS catalogue / DB integrity | 0 tenant tables without forced RLS + policy; runtime roles not superuser / BYPASSRLS | final CI + demo | AR3 / AR4 below |

## Findings by area

Severity counts per area (rows of the area tables; accepted-risk-only tables counted under Accepted risks):

| Area | Critical | High | Medium | Low | Info | Fixed | Accepted risk | Other |
|---|---|---|---|---|---|---|---|---|
| Tenant (SEC-P1-TENANT-*) | 0 | 2 | 2 | 0 | 0 | 4 | 4 (AR1–AR4) | — |
| Auth (SEC-P1-AUTH-*, SEC-9D-006/012) | 0 | 1 | 5 | 4 | 0 | 8 | 2 (R1, R2) | — |
| Approvals (SEC-P1-APPR-*) | 0 | 4 | 2 | 1 | 0 | 6 | 1 (APPR-007) | — |
| Config (SEC-P1-CONFIG-*) | 2 | 5 | 6 | 0 | 0 | 13 | 3 (table) | — |
| AppSec (SEC-P1-APPSEC-*) | 0 | 3 | 3 | 1 | 3 | 6 | 3 (007–009) | APPSEC-010 resolved by TENANT-003 |
| **Total** | **2** | **15** | **18** | **6** | **3** | **37** | **13** | 1 |

44 findings; **no Critical or High finding is open**. The two Critical findings (mock payment gateway in production, supplier
bank-detail change by one user) and every High finding are fixed with regression tests.

### Tenant isolation
- **TENANT-001 (High, DEF-X-003 / SEC-9D-004)** — no RLS; any id-only statement could cross tenants. Forced RLS + one policy on all 386
  tenant tables, context from the server-side session only (task-local, cleared on pool release), fail-closed. FIXED.
- **TENANT-002 (High)** — API could run as superuser (RLS bypassed). Production refuses superuser / BYPASSRLS; e2e, pilot and demo run as
  the runtime role. FIXED.
- **TENANT-003 (Medium, DEF-C-010 / B-004 / A-004)** — institution from URL path. Pinned sessions are DB-enforced to their institution;
  tenant-wide staff remain tenant-wide by design. FIXED (pinned sessions).
- **TENANT-004 (Medium, DEF-I-006)** — faculty read unpublished results / malpractice. `require_exam_official`. FIXED.

### Authentication / session
- **AUTH-001 (High)** — privileged roles without MFA → `SUTRA_PRIVILEGED_MFA` (refused off in production). FIXED.
- **AUTH-002..006 (Medium)** — OIDC alg pinning + JWK checks; per-account lockout (SEC-9D-005) + rate-limited auth ingress; realm
  brute force / OTP reuse + production baseline parity; permission before first read on 24 routes; production OIDC https + no dev secret. FIXED.
- **SEC-9D-006 / 012 (Low)** — API hardening headers and HSTS: one middleware (`crates/web/src/security_headers.rs`), HSTS on by
  default in production (`SUTRA_HSTS` / `SUTRA_HSTS_MAX_AGE_SECS`, refused off in production). FIXED.

### Approvals / separation of duties
- **APPR-001 (High, DEF-X-002 rest)** — ~101 decision endpoints without SoD → append-only approval ledger + `guard_decision*` / status-history
  guards on 99 endpoints; fitness test fails on any new unguarded decision. FIXED.
- **APPR-004 (High, DEMO-006)** — undecidable / self-decided eligibility override → governed override request + guarded decision. FIXED.
- **APPR-005/006 (High if present)** — bypass by posting / editing after approval: none found; regressions added. FIXED (verified).
- **APPR-002/003 (Medium)** — ledger tampering, system / anonymous deciders. FIXED.

### Secrets / configuration / transport
- **CONFIG-002 (Critical, DEF-F-002)** — mock payment gateway accepted in production → refused at start-up and at Helm render. FIXED.
- **CONFIG-013 (Critical, DEF-J-004)** — approved supplier's bank account changeable by one user → request + independent decision
  (step-up, maker ≠ checker), masked audit, DB trigger `supplier_bank_details_guard`, RLS-covered table. FIXED.
- **CONFIG-001/003/004/005/008 (High)** — dev variables, weak / published secrets, key reuse, plain-http / no-TLS DB, no rate limiting. FIXED.
- **CONFIG-006/007/009/010/011/012 (Medium)** — headers, CORS allow-list, body / time limits, secrets in history, exposed ops
  endpoints, SPA CSP without `'unsafe-inline'` scripts (SEC-9D-011). FIXED.

### Application security
- **APPSEC-001/002 (High)** — TRUNCATE of ledger / audit tables; posted manual journals editable in SQL → DB guards. FIXED.
- **APPSEC-004 (High)** — stored XSS via `javascript:` links → `weblink::http_link`. FIXED.
- **APPSEC-003/006 (Medium)** — LIKE wildcard injection; CSV formula injection. FIXED. **APPSEC-005 (Low)** — 503 detail leak. FIXED.

## Accepted risks

| ID | Risk | Why accepted | Plan |
|---|---|---|---|
| SEC-P1-TENANT-AR1 | `sutra.rls_scope` / `sutra.tenant_id` are session settings; SQL injection as the runtime role could switch them | all SQL parameterised (Semgrep rule + sqlx binds); settings never from client input | separate `sutra_system` login role or signed context (Part 2) |
| SEC-P1-TENANT-AR2 | backups need a BYPASSRLS role | backups must contain every tenant | dedicated `sutra_backup` role, flagged by db-integrity if misused |
| SEC-P1-TENANT-AR3 | two child tables without `tenant_id` (scoped via parent) | every access path joins the RLS parent | append-only migration adding `tenant_id` |
| SEC-P1-TENANT-AR4 | non-tenant platform tables outside RLS | server-generated keys; idempotency scope includes tenant | Part 2 platform review |
| SEC-P1-AUTH-R1 | lockout counter per replica | ingress per-IP limit + Keycloak brute-force detection | shared counter (Valkey) |
| SEC-P1-AUTH-R2 | role revocation applies at next sign-in | 30-min idle / 8-h absolute expiry; back-channel logout for immediate cut-off | — |
| SEC-P1-APPR-007 | 26 configuration-only decisions without SoD | no money / grade / status / access effect; reasons enforced by the fitness test | review per release |
| CONFIG (table) | NATS / Valkey / Flowable / OpenSearch not forced to TLS | private network + default-deny NetworkPolicy, non-default credentials | when any leaves the private network |
| CONFIG (table) | API rate limits per replica | outer nginx / ingress limits + Keycloak | shared store if replicas grow |
| CONFIG (table) | SPA CSP `style-src 'unsafe-inline'` | inline styles cannot run script; `script-src` hash-only | when the UI drops inline style attributes |
| SEC-P1-APPSEC-007 | Semgrep argv false positive | argv is operator-controlled | — |
| SEC-P1-APPSEC-008 | history-only fake fixtures allow-listed in gitleaks | exact-literal allow-list, default rules on | — |
| SEC-P1-APPSEC-009 | username in failed-login logs | lockout investigation; logs access-controlled | — |

## BLOCKED_EXTERNAL

| Item | Why blocked | What exists now |
|---|---|---|
| Real bank API (payouts, payroll disbursement file transmission, supplier payments) | needs a bank sandbox / contract and credentials; no real bank API is available to this project | payouts post to the ledger and produce bank files; transmission and confirmation are manual |
| Bank statement reconciliation | needs real bank statement feeds (MT940 / CAMT.053 / bank API) | treasury bank accounts and ledger postings; reconciliation against real statements not implemented |
| Real payment-gateway capture | needs a live gateway merchant account | the mock gateway is refused in production (CONFIG-002); a production deployment must configure a real gateway |

## Evidence runs

Worker branches (green before integration): tenant CI 38772242990 · demo 38772242954 · deploy 38772331224 · pilot 38772335139;
auth CI 38774880706 · demo 38774880984 · deploy 38774944294 · quality 38774948589; appr CI 38772672703 · demo 38772672672;
config CI 38775388199 · demo 38775388189 · deploy 38775425281 · pilot 38775429092 · quality 38775422497;
appsec CI 38767258747 · demo 38765529815 / 38767258857 · quality 38767374109.

Integrated head 453bc21d: SutraERP CI 38862254488 · Part 1 demo 38862254558 · deploy 38862954655 · pilot 38862956343 · quality 38862951620 — all green.
