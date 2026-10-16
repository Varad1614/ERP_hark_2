# Part 2 · Stage 4.3 — Keycloak identity onboarding (S4-idp)

Branch `part2/s4-idp` (from `erp_merge` c31cc487). Status: **READY_FOR_REVIEW** — see "Test evidence"
for the green run ids. Tracker rows: [`tracker/p2-s4-idp.md`](../tracker/p2-s4-idp.md).

SutraERP decides **who** may sign in and **with which roles**; Keycloak keeps credentials, second
factors and the sign-in itself. This stage connects the two so that a student or staff member who
becomes active gets exactly one Keycloak identity in the right tenant / institution, follows the
record's lifecycle (suspension, exit, separation), and never needs a hand-made account.

## What was built

| Part | Where | What |
|---|---|---|
| Ports | `crates/contracts/src/platform/identity.rs` | `IdentityProvisioningPort` (IdP user administration, idempotent desired-state reconciliation), `IdentitySubjectSource` (implemented by the owning modules), `ScopeGroup` (`/sutra-tenant-<t>[/institution-<i>]`), `generated_username` (e-mail, or `<number>.<tenant suffix>`), `RequiredAction`, `AccountState`. |
| Schema | `migrations/20261231004300_platform_identity_link.sql` | `platform.identity_link` (duplicate guard + sync queue: unique per subject, per `(idp, idp_user_id)`, per `(idp, lower(username))`, per `(idp, lower(email))`; back-off columns), `platform.identity_role_request` (maker-checker for privileged roles), `platform.identity_role_mapping` (ERP role → extra realm role / client role / group, per tenant / institution). Tenant tables with `platform.apply_tenant_rls()`. |
| Keycloak adapter | `modules/platform/src/identity/keycloak_admin.rs` | Admin REST over a confidential **service-account** client (`client_credentials`, token cached until 30 s before expiry, one retry on 401). `check()` reads the token's grants and refuses an over- or under-privileged client (required `manage-users`, `view-users`, `query-users`, `query-groups`; optional `query-clients`; anything else — `realm-admin`, `manage-realm`, `view-realm`, `view-clients`, `impersonation`, … — or any SutraERP realm role is refused). Roles are looked up through `role-mappings/realm/available` / `…/clients/{c}/available` (no `view-realm` needed). |
| In-memory adapter | `modules/platform/src/identity/memory.rs` | Tests only; production configuration refuses it. |
| Owning-module sources | `modules/student/src/identity_source.rs`, `modules/people/src/identity_source.rs` | Facts (status → account state, institution, e-mail, base role), event → subject mapping, `subject_ref` recording on the owning record. The platform never queries a foreign schema. |
| Jobs | `modules/platform/src/identity/sync.rs` | `idp-events` (outbox lifecycle events → links, exactly once via `platform.event_receipt`) and `idp-sync` (reconciles enabled flag, realm / client roles, groups, required actions; adopts a pre-existing account with the same e-mail, never one of another tenant or with a different e-mail; disabling ends Keycloak sessions **and** SutraERP sessions; failures back off exponentially; conflicts stop for an administrator). Every step audited. |
| Admin API (14 operations) | `modules/platform/src/identity/http.rs` | list / get, provision, create platform administrator, required actions (activation, password recovery, MFA enrolment, optional e-mail), retry, administrative suspend / reactivate, role requests (+ list, decision with `ensure_independent_decider`), role mappings (list / create / retire). Permissions `platform.identity.read|manage|lifecycle|role.approve`; step-up and SoD; never on one's own identity. |
| Configuration | `apps/api/src/config_idp.rs`, `apps/api/src/idp_wiring.rs`, `main.rs` | `SUTRA_IDP_*` (below); start-up least-privilege check, fatal in production. |
| Realm | `deploy/keycloak/realm-sutra-dev.json`, `realm-production-baseline.json`, `check-realm.py` | `sutra-idp-admin` service-account client with exactly the least-privilege roles; the `tenant_id` claim is now a **user-attribute mapper resolved from the group attributes** (`/sutra-tenant-<t>` carries `tenant_id`, institution sub-groups also `institution_id`) instead of a hard-coded claim; seeded dev users are members of the dev tenant group. |
| CI | `.github/workflows/sutraerp-ci.yml` (`rust-db` job) | Keycloak 26.7 container with the dev realm import; step "Live Keycloak identity provisioning (S4-idp)" runs `live_keycloak_idp.rs`. |

## Configuration (`SUTRA_IDP_*`)

| Variable | Default | Meaning |
|---|---|---|
| `SUTRA_IDP_PROVISIONING` | `off` | `off` · `keycloak` · `memory` (tests; refused in production) |
| `SUTRA_IDP_URL` | root of `SUTRA_OIDC_BACKCHANNEL_URL` / `SUTRA_OIDC_ISSUER` | Keycloak base URL the API calls |
| `SUTRA_IDP_REALM` | realm of the issuer | realm |
| `SUTRA_IDP_CLIENT_ID` | `sutra-idp-admin` | service-account client (never the sign-in client) |
| `SUTRA_IDP_CLIENT_SECRET` | — (required with `keycloak`) | its secret |
| `SUTRA_IDP_TIMEOUT_MS` | `10000` | per call |
| `SUTRA_IDP_AUTO_PROVISION` | `true` | create identities when a subject becomes active |
| `SUTRA_IDP_SEND_ACTIONS_EMAIL` | `false` | Keycloak "execute actions" e-mails (needs realm SMTP) |
| `SUTRA_IDP_ACTIONS_CLIENT_ID` / `SUTRA_IDP_ACTIONS_REDIRECT_URI` | — | landing page of the e-mailed link |
| `SUTRA_IDP_POLL_MS` · `SUTRA_IDP_BATCH_SIZE` | `5000` · `50` | job cadence |
| `SUTRA_IDP_RETRY_BASE_SECS` · `SUTRA_IDP_RETRY_MAX_SECS` | `10` · `3600` | back-off |
| `SUTRA_IDP_TENANTS` | all | staged roll-out (comma-separated tenant UUIDs) |
| `SUTRA_IDP_STARTUP_CHECK` | `true` | least-privilege check at start-up (cannot be switched off in production) |

Production refuses: `memory`; a plain-http non-loopback URL; a missing / published / short client
secret; the sign-in client reused as admin client; the start-up check off; a failed start-up check.

## The two risks, verified against a real Keycloak 26.7

1. **Tenant claim from group attributes.** `live_keycloak_idp::provisioned_student_signs_in_and_suspension_locks_them_out`
   asserts the provisioned user has **no** `tenant_id` attribute of its own, signs in through
   SutraERP's real `/api/v1/auth/oidc/start` → Keycloak login + `UPDATE_PASSWORD` pages →
   `/api/v1/auth/oidc/callback`, and checks the resulting SutraERP session's `scope.tenantId` and
   `scope.institutionId` equal the institution sub-group's attributes (SutraERP refuses a token
   without the claim, so the session itself proves the mapper resolved the group attribute). The
   Part 1 full-stack demo (Keycloak/OIDC journey with the seeded users, whose tenant now also comes
   from the dev tenant group) stays green.
2. **Role lookup without `view-realm`.** The same test first runs the adapter's `check()` (refuses
   `view-realm` / `view-clients`) against the realm's `sutra-idp-admin` account, then maps an
   ERP role to a newly created realm role and grants an extra ERP role: both are applied through
   `role-mappings/realm/available` and are present in the ID token of the next sign-in (session
   roles `faculty` + `student`).

## Test evidence

| Suite | What | Result |
|---|---|---|
| unit (`sutra-contracts`, `sutra-platform`, `sutra-api` config) | username generation, desired state, role diff, foreign-tenant detection, least-privilege grant rules, Keycloak error mapping, production refusals | CI `Rust (fmt · clippy · test)` green — run RUN_CI |
| `apps/api/tests/db_p2_idp.rs` (PostgreSQL 18, memory IdP) | lifecycle (activation event → provisioned, suspension → disabled + IdP/SutraERP sessions ended, restore), duplicates (409, adoption, foreign tenant never adopted, DB unique), retry with back-off, admin API (permissions, maker-checker, no self-service, TOTP on privileged grants, suspend/reactivate, mappings, tenant isolation, audit) | 4/4 green — run RUN_CI |
| `apps/api/tests/live_keycloak_idp.rs` (PostgreSQL 18 + **Keycloak 26.7**) | provision → real OIDC login (group-attribute tenant claim); role mapping + grant effective at next login; suspension → Keycloak login refused, Keycloak sessions = 0, SutraERP session 401; restore → login works; duplicate prevention (adoption, Keycloak 409 on taken username, foreign-tenant account untouched); retry while Keycloak is unreachable (TCP proxy cut) → back-off → converges to exactly one account | 3/3 green — run RUN_CI |
| `approval_matrix` | `platform_identity_role_request_decision` guarded and listed in `APPROVAL_CONTROL_MATRIX.md` | green — run RUN_CI |
| OpenAPI drift | committed `openapi/openapi.json` + `packages/api-client/src/schema.d.ts` regenerated | green — run RUN_CI |
| SutraERP Part 1 demo (full stack, Keycloak) | realm tenant mapper changed — journey + persistence | green — run RUN_DEMO |
| SutraERP quality gates | cargo-deny / audit / gitleaks / Semgrep / ZAP / k6 | green — run RUN_QUALITY |

## BLOCKED_EXTERNAL

| Item | Why | What clears it |
|---|---|---|
| Activation / password-recovery / MFA **e-mails** (`execute-actions-email`) | needs SMTP configured in the college's realm; the CI realm has none, so the live test marks the e-mail verified and sets a temporary password (the `UPDATE_PASSWORD` step itself runs on Keycloak's real page) | realm SMTP host, port, sender and credentials of the college's mail relay; then set `SUTRA_IDP_SEND_ACTIONS_EMAIL=true` and run one activation end-to-end |
| Service-account roles on a **real server** | the dev realm import grants them; on the college's Keycloak an administrator must create `sutra-idp-admin` (confidential, service accounts on, no browser flows) and grant exactly `realm-management` `manage-users`, `view-users`, `query-users`, `query-groups` (+ `query-clients` if client-role mappings are used), then put the secret in `SUTRA_IDP_CLIENT_SECRET` | realm-admin access to the production Keycloak (or the college IT team doing the grant); the API start-up check then verifies the grants |

## Notes for the integrator

* Files other branches may touch: `apps/api/Cargo.toml` (dev-deps `reqwest`, `tokio` net/io-util),
  `apps/api/src/{app.rs,config.rs,lib.rs,main.rs}` (wiring lines marked `Part 2 · S4-idp`),
  `.github/workflows/sutraerp-ci.yml` (Keycloak container + live step in `rust-db`),
  `deploy/keycloak/realm-*.json`, `openapi/openapi.json`, `packages/api-client/src/schema.d.ts`,
  `docs/readiness/APPROVAL_CONTROL_MATRIX.md`, `.gitleaks.toml` (S4-pay's 7fc92f45 allowlist block
  mirrored byte-identically: gitleaks scans every ref's history, so every branch needs it until
  S4-pay merges).
* `part2/**` is not in the CI push triggers: runs were dispatched (`workflow_dispatch`).
