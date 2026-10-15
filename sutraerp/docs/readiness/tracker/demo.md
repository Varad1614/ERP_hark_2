# Part 1 · Stage 2 · demo — defect tracker

Branch `part1/s2-demo`. Defects found while building and running the full-stack demonstration
(`.github/workflows/sutraerp-part1-demo.yml`, `apps/web/e2e/journey.e2e.mjs`). P0/P1 functional
and small → fixed here with a regression test; otherwise listed with severity.

| ID | Issue | Root cause | Files changed | Status | Tests | CI result + run id | Blockers | Commit |
|---|---|---|---|---|---|---|---|---|
| DEF-P1-DEMO-002 | `.env.example` (the documented "copy to .env" file) is not a valid dotenv file; with it as `.env` the API silently starts with every variable after line 152 unset | `SUTRA_MAIL_FROM=SutraERP <no-reply@sutra.dev>` unquoted (dotenvy stops at the space); `main.rs` discarded the `dotenvy::dotenv()` error | `.env.example`, `apps/api/src/main.rs` (malformed `.env` now refuses to start; missing `.env` still fine) | fixed | `apps/api/tests/db_p1_demo_config.rs` (`env_example_parses_completely`, `env_example_is_a_valid_development_configuration`); demo workflow runs `sutra-api check-config` with `SUTRA_ENV_FILE=.env.example` | SutraERP CI 38750760063 green (Rust fmt·clippy·test, integration); demo 38750761187 “Validate” step green | — | 488549bb (+ fmt 17075f41) |
| DEF-P1-DEMO-003 | Keycloak/OIDC mode: an administrator who signs in with password + OTP still gets `mfaLevel=none` (`amr=[]`), so every step-up permission (institution register/activate, finance approve, …) answers `403 mfa-step-up-required` — the OIDC deployment cannot be administered | The dev realm used Keycloak's built-in browser flow, whose authenticators carry no *Authentication Method Reference*; the realm's `oidc-amr-mapper` therefore emits no `amr`, and no LoA conditions exist for `acr` (`SUTRA_OIDC_MFA_ACR=2,mfa` never matches). Seen in demo run 38739336672 (`admin@sutra.dev: session mfaLevel=none amr=[]`) | `deploy/keycloak/realm-sutra-dev.json` (browser flow `browser-sutra`: same cookie → password → conditional OTP steps, with `default.reference.value` pwd / otp) | fixed | journey step “Sign-in … (API clients)” asserts the administrator session is `mfa` after password + OTP (`amr ["pwd","otp"]`); the seed then exercises step-up permissions (institution register/activate, fee-structure / exam approvals) | demo 38750761187 green (sign-in, seed, all 6 stages, persistence) | — | 41dafc94, db4eb556 (flow description ≤ 255 chars, realm import failed before) |
| DEF-P1-DEMO-004 | Web container: after `docker compose restart api` every `/api` request through the web app answers **502** until the web container is restarted too (seen in demo run 38745591464: sign-in `oidc/start → 502`, `web→api` never ready again) | `deploy/docker/nginx.conf` `proxy_pass http://api:8080` is resolved once at nginx start-up; the restarted api container came back on a new Docker address and nginx kept the stale one | `deploy/docker/nginx.conf` (Docker embedded DNS `resolver 127.0.0.11 valid=10s`, upstream in a variable → resolved per request; original URI still forwarded) | fixed | demo workflow step “Restart api + postgres” logs the api / postgres addresses before and after (`logs/restart-addresses.txt`), then `demo-wait.sh` requires `web→api /api/v1/auth/csrf` = 200 and the persistence phase signs in through the web proxy. Helm is unaffected (stable Service name/ClusterIP) | demo 38750761187 (api / postgres addresses swapped on restart; web→api 200, persistence 6/6)| an address change on restart is not forced (Docker may reuse the address — it did in 38748076580), so the regression only bites when it changes | f7919e97 |
| DEF-P1-DEMO-005 | Compose Keycloak: the first sign-in after `docker compose restart postgres` fails (`LOGIN_ERROR invalid_user_credentials`, `KC-SERVICES0013 … Database operation failed`, Agroal “connection has been closed”) — demo run 38748076580, persistence phase, admin OTP step → 400 | Keycloak shares the postgres service; its Agroal pool kept the dead connections (background validation only every 2 min, no foreground validation) | `docker-compose.yml` keycloak: `QUARKUS_DATASOURCE_JDBC_VALIDATION_QUERY_SQL=SELECT 1`, `QUARKUS_DATASOURCE_JDBC_FOREGROUND_VALIDATION_INTERVAL=5S` (connections idle > 5 s validated before use) | fixed | persistence phase: the three Keycloak sign-ins (admin password + OTP) right after the postgres restart | demo 38750761187 green (persistence: 3 Keycloak sign-ins incl. admin OTP after the postgres restart; also 38749424353) | production Keycloak (external DB) should carry the same pool validation — noted for the Helm/ops owner | 17027e4d |
| DEF-P1-DEMO-001 | OpenAPI component `LineInput` is defined three times (finance fee-structure line, student registration line, exams grading line); the generated `openapi.json` / TS client describe `POST /finance/institutions/{id}/fee-structures` `lines` as `{curriculumComponentId, kind}` instead of `{feeHeadId, description, amount}` | utoipa `ToSchema` names collide across modules (last registration wins) | — | open (P3: client typing/docs; runtime unaffected) | — | — | needs `#[schema(as = …)]` renames + `openapi.json`/`schema.d.ts` regeneration (openapi-drift job) | — |

## Journey test corrections (test assumptions that were wrong — no app change, no assertion weakened)

| Step | Wrong assumption | Correct behaviour now asserted | Commit |
|---|---|---|---|
| 1 Admission | audit action `student.admission.create-student` | the API's action is `student.admission.create_student`; the step now also requires `student.admission.approve` and `student.admission.confirm` | 964c4af8 |
| 3 Attendance | `academic.attendance_audit` has ≥ 2 rows after finalisation | that table is the append-only history of changes to **finalised** marks (corrections, migration 20261025000011); a fresh finalisation must have **0** rows — asserted exactly; the platform audit trail of the session is still required | 964c4af8 |
| 4 Fees / 7 Persistence | `[data-outstanding]` shows ₹0 when everything is paid | the Invoices tile replaces the amount with `[data-outstanding-none]` “You have no dues right now”; the step asserts that text **and** that no `[data-outstanding]` is rendered | 964c4af8 |
| 5 Examination | mark rows carry `studentId` | `MarkRowView` carries `studentNumber` (no `studentId`); rows are matched by the admitted student's number | 964c4af8 |
| 6 Result | a `platform.audit_log` row targets the term result | results are audited per command on the **result batch** (create / compute / validate / approve / finalise) and the **publication** (create / authorise / publish, with counts); both are now required | f7919e97 |

## Open items owned elsewhere

| ID | Where it shows | Owner | Journey handling |
|---|---|---|---|
| DEF-C-003 | duplicate admission (same login, name, DoB, e-mail) accepted through offer | fixed on `part1/s2-acad` | `E2E_KNOWN_DEFECTS: DEF-C-003` — reported as a known failure in the summary, not counted; the integrator removes it after merging s2-acad |
| DEF-X-001 | every approval is automatic (`system:auto-approval` in the audit trail) | fixed on `part1/s2-appr` | all approval points go through `approve(kind, id)` in `apps/web/e2e/lib/approvals.mjs` (single seam; today it reads + asserts the approved state and lists the dependency) |

## Status

Demo workflow green end to end on `04674047`: run 38750761187 (journey 25 pass + 1 known open
DEF-C-003; persistence 6/6; api / postgres addresses swapped on restart). SutraERP CI on the same
commit: 38750760063. Reports: `../LOCAL_DEMO_REPORT.md`, `../E2E_TEST_REPORT.md`; guide
`../DEMO_RUN_GUIDE.md`. Approvals still auto-approved on this base — `approve(kind, id)` in
`apps/web/e2e/lib/approvals.mjs` is the single seam the integrator switches after `part1/s2-appr`.

## Integrator (part1/s2-int, phase B)

| ID | Issue | Root cause | Files changed | Status | Tests | CI result + run id | Blockers | Commit |
|---|---|---|---|---|---|---|---|---|
| DEF-P1-DEMO-006 | Eligibility override for a section seat returns 409 “awaits a decision” in human mode; nothing can decide it | `academic/offering/enrolment.rs::evaluate` starts `ENROLMENT_ELIGIBILITY_OVERRIDE` but no decision endpoint exists | `journey.e2e.mjs`, `lib/journey-seed.mjs` (office registers the seed student, seats her without override) | test fixed; product gap open (P2) | journey step “Seed student Isha registered …” | demo 38755333716 green | override decision endpoint (academic) | 7c26665f |
| DEF-P1-DEMO-007 | Journey/harness assumed auto-approval; no checker users in the realm | written before DEF-X-001 merged | `lib/approvals.mjs`, `lib/auth.mjs`, `lib/harness.mjs`, `lib/journey-seed.mjs`, `journey.e2e.mjs`, `lib/journey-report.mjs`, `deploy/keycloak/realm-sutra-dev.json` | fixed | journey 29/29, persistence 6/6 (human mode); self-decision 403 ×4 | demo 38755333716 green | — | 1b6d4276, 7c26665f |
| DEF-P1-DEMO-008 | No DB integrity gate | — | `deploy/ci/db-integrity.{sh,sql}`, workflow step | fixed | 0 violations | demo 38755333716 green | — | 7c594e49 |
