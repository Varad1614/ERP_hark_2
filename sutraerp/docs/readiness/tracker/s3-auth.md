# Tracker — Part 1 Stage 3, worker `auth`

| ID | Issue | Root cause | Files changed | Status | Tests | CI result + run id | Blockers | Commit |
|----|-------|-----------|---------------|--------|-------|--------------------|----------|--------|
| SEC-P1-AUTH-001 | Privileged roles usable without MFA | MFA only for the step-up permission set | `crates/kernel/src/auth.rs`, `modules/platform/src/auth/session.rs`, `apps/api/src/config.rs`, `apps/api/src/app.rs` | FIXED | kernel unit; `auth_hardening.rs` | CI @@CI@@ green | — | b2e3c57c |
| SEC-P1-AUTH-002 | Weak OIDC token validation | alg from header, JWK use/alg unchecked, no iat/sub, unbounded JWKS refresh | `modules/platform/src/auth/keycloak.rs`, `apps/api/src/config.rs` | FIXED | keycloak.rs unit (3) | CI @@CI@@ green | — | c7a6582a |
| SEC-9D-005 / AUTH-003 | No sign-in / OTP attempt limit | no counter in API, no ingress limit | `modules/platform/src/auth/throttle.rs`, `auth/http.rs`, Helm `ingress-auth.yaml`, `values*.yaml` | FIXED | throttle unit (4), `http_tests.rs` (3), deploy check | CI @@CI@@, deploy @@DEPLOY@@ green | — | b2e3c57c, 0c061cbb |
| SEC-P1-AUTH-004 | Realm brute force / OTP reuse / no prod baseline | realm defaults | `deploy/keycloak/realm-sutra-dev.json`, `realm-production-baseline.json`, `check-realm.py`, `sutraerp-deploy.yml` | FIXED | deploy `keycloak-realm-security-parity` | deploy @@DEPLOY@@ green | — | 0c061cbb |
| SEC-P1-AUTH-005 | Row read before permission check (existence oracle) on academic commands | repo fns fetched the row to find its section, then checked the role | `modules/academic/src/shared/faculty.rs`, `assessment/repo.rs`, `attendance/repo.rs`, `attendance/syllabus.rs`, `resources/repo.rs`; sweep `apps/api/tests/auth_hardening.rs` | FIXED | `every_route_requires_a_session_and_a_permission` | CI @@CI@@ green | — | c658bd3a |
| SEC-P1-AUTH-006 | Prod accepted http OIDC URLs / dev secret | no production validation | `apps/api/src/config.rs` | FIXED | config unit; Helm check | CI @@CI@@ green | — | b2e3c57c |
| SEC-9D-006 / 012 | No API hardening headers / HSTS | headers only on web nginx | `crates/web/src/headers.rs`, `apps/api/src/app.rs`, `deploy/docker/nginx.conf` | FIXED | headers unit; `auth_hardening.rs` | CI @@CI@@ green | — | 0c061cbb |
| Sweep harness | Route sweep reported 1684 false 401s | the role-less run sent `POST /auth/logout` / session revokes with its own probe cookie, ending the session mid-sweep | `apps/api/tests/auth_hardening.rs` | FIXED (test only sends guarded routes in the role-less run; no check weakened) | — | CI 38767003697 red → @@CI@@ green | — | c658bd3a |

Demo `@@DEMO@@`, quality gates `@@QUALITY@@`. Head `@@SHA@@`.
