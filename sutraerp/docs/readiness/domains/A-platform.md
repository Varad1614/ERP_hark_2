# Domain A — Platform: readiness inventory (Part 1 · Stage 1)

Base: `part1/readiness` = `erp_merge` `fb205860`, CI run 38724169019 (all jobs green; "API binary (aarch64)" skipped).
Scope: `modules/platform`, `crates/{kernel,web,persistence,infra,contracts}` (platform parts), auth / Keycloak / OIDC,
workflow adapter, events / NATS, audit, configuration. Rows in [`A-platform.csv`](A-platform.csv) follow the L2 workflows in
`architecture/L2_Domain_Workflow_Architecture/section-a/` (A01–A10 plus the 2026-08/09 amendments) and also cover the Batch 1D
self-service features (notifications, notices, to-dos, profile), which have no L2-A workflow of their own.

Evidence rules: "API-tested passed" = a named test in `apps/api/tests/*.rs`, `modules/platform/tests/*.rs` or an in-crate
`#[cfg(test)]` HTTP test (`modules/platform/src/auth/http_tests.rs`, run by `cargo test --workspace` in the Rust job) that
passed in run 38724169019 (test names checked in the job logs). "Browser-tested passed" = an e2e spec in the CI matrix
(`finance, student, academic, ui, exams, people, campus, reskin2`) drives the flow; screenshot-only page visits are counted as
not tested. Customer-validated = pending everywhere.

## Summary

| Status | Rows |
|---|---|
| implemented | 19 |
| partial | 9 |
| missing | 16 |
| deferred | 1 |
| **total** | **45** |

| Release readiness | Rows |
|---|---|
| ready | 12 |
| ready-with-gaps | 11 |
| not-ready | 19 |
| deferred | 3 |

The **runtime security core is real and well tested**: sessions, CSRF, OIDC BFF, TOTP, WebAuthn (dev mode), back-channel
logout, deny-by-default authz, outbox → JetStream relay, inbox consumer, audit hash chain. The **governance layers L2-A specifies
are mostly absent**: no tenant aggregate, no person/identity model (A02), no role/permission administration, delegation, SoD or
break-glass (A04), no workflow definition governance, human-task inbox or recovery (A05), no audit investigation / retention
(A07), no module registry lifecycle (A08), no master-data service (A09) and no operational-readiness certification (A10).

## Defects

| Id | Sev | Title |
|---|---|---|
| DEF-A-001 | P1 | Approval engine defaults to the auto-completing no-op adapter in every environment, production included |
| DEF-A-002 | P2 | Search API filters by tenant only, not by the session's institution |
| DEF-A-003 | P2 | Fixed three-role model: no role administration, separation of duties impossible |
| DEF-A-004 | P2 | (see DEF-B-004) institution scope taken from the path, not the session |
| DEF-A-005 | P2 | No attempt throttling on sign-in / MFA / step-up endpoints |
| DEF-A-006 | P3 | Catalog and module endpoints are anonymous |
| DEF-A-007 | P2 | Workspace drawer fakes workflow progress ("Advance to … / Approved") client-side |
| DEF-A-008 | P3 | Passkey e2e spec exists but is not in the CI e2e matrix |

**DEF-A-001 · P1 · Approval engine defaults to the no-op adapter in production**
- Evidence: `apps/api/src/config_b9.rs:222` (`None | Some("noop") => WorkflowMode::Noop`, no production check, unlike the payment
  gateway rule at `config_b9.rs:276-301`); `apps/api/src/app.rs:963` wires `NoopWorkflowAdapter`; `modules/platform/src/workflow.rs:12-25`
  returns a synthetic `noop-…` instance and completes nothing; `modules/platform/src/workflow_flowable.rs:10` — even with Flowable the
  BPMN gateway auto-approves unless the caller passes `autoApprove=false`. `docs/batches.md:201` confirms noop "stays the default".
- Root cause: Batch 2–8 shortcut kept through Batch 9; Flowable was made selectable but not mandatory.
- Fix: refuse `SUTRA_WORKFLOW=noop` in `Environment::Production` (same pattern as `PaymentGatewayMode`), and derive each module's
  `ApprovalMode` from the workflow mode (see DEF-B-001).

**DEF-A-002 · P2 · Search API is not institution-scoped**
- Evidence: `modules/platform/src/search/mod.rs:149-152` passes only `ctx.scope.tenant_id` to `index.search`;
  `crates/infra/src/opensearch.rs:119-120` builds a tenant-only filter although documents carry `institutionId` (`opensearch.rs:79-87`).
- Root cause: projections designed tenant-wide; institution pinning deferred (DEF-B-004).
- Fix: add an `institutionId` term filter when the session scope has an institution; add a test with two institutions in one tenant.
  Mitigation today: search is off by default (`db_b9c.rs::search_api_is_disabled_by_default_and_checks_permissions`).

**DEF-A-003 · P2 · Fixed three-role model**
- Evidence: `crates/kernel/src/auth.rs:25-29` (`enum Role { Administrator, Faculty, Student }`), `auth.rs:609-613` static
  `role_permissions`; administrator holds e.g. both `InstitutionManage` and `InstitutionActivationDecide`.
- Root cause: L2-A04 role/permission management, delegation, SoD and break-glass were never built.
- Fix: persisted roles + permission assignment per tenant/institution, then SoD constraints on decision permissions.

**DEF-A-004 · P2** — recorded once as DEF-B-004 (the code is in `modules/institution/src/shared/scope.rs:14-16`); it is the
context-resolution gap of row A-002.

**DEF-A-005 · P2 · No auth attempt throttling**
- Evidence: `modules/platform/src/auth/http.rs:451` (`login`), `:555` (`verify_mfa`), `:771` (`step_up`) have no attempt counter;
  `docs/security/findings.md` SEC-9D-005 is still `open`.
- Fix: per-account + per-IP counters (Valkey) on these three handlers and ingress rate limits on `/api/v1/auth/*`.

**DEF-A-006 · P3 · Anonymous catalog endpoints**
- Evidence: `modules/platform/src/catalog.rs:44,79,95` take no `Authenticated`; `apps/api/tests/api.rs:166-181` calls them without a
  session and expects 200. Discloses the module list and the per-role workflow catalog.
- Fix: require an authenticated session (the web only calls them after sign-in).

**DEF-A-007 · P2 · Fake workflow progress in the workspace drawer**
- Evidence: `apps/web/app/routes/workspace.tsx:11` (`STAGES = ["Draft","Submitted","In review","Approved"]`), `:150-160` — the
  "Advance to …" button only updates React state and toasts "moved to Approved"; nothing is persisted or sent to the API.
- Root cause: prototype placeholder for the 99 catalog workspaces kept after the domain screens landed.
- Fix: remove the button (read-only catalog) or link each workflow to its real domain screen.

**DEF-A-008 · P3 · Passkey e2e not in CI**
- Evidence: `apps/web/e2e/passkey.e2e.mjs` and `package.json` `e2e:passkey` exist, but `.github/workflows/sutraerp-ci.yml:174`
  matrix is `[finance, student, academic, ui, exams, people, campus, reskin2]`.
- Fix: add `passkey` to the matrix (virtual authenticator) or delete the stale spec.

## Test gaps

- No browser test drives TOTP enrolment (`/account/mfa`), step-up dialog, passkey sign-in, OIDC redirect sign-in, notifications,
  notices or to-do transitions; reskin2 only screenshots `/notifications`, `/notices`, `/todos`, `/profile`, `/account/security`.
- Flowable adapter is only proved in `sutraerp-data.yml` (`live_flowable.rs`, `live_opensearch.rs`); run 38724169019 covers only the
  BPMN-file and wiring tests in `db_b9c.rs`.
- No test asserts that production refuses the no-op workflow adapter (it does not — DEF-A-001).
- No test of search with two institutions in one tenant (DEF-A-002).
- No rate-limit / brute-force tests (feature absent).
- No architecture test that flags tenant-table SQL without a `tenant_id` predicate (SEC-9D-004 follow-up still open).

## Doc discrepancies

- `modules/platform/src/lib.rs:15-16` says the workflow port is `NoopWorkflowAdapter` "(Flowable adapter later)"; the Flowable
  adapter shipped in 9C (`workflow_flowable.rs`).
- `docs/batches.md:26` (1B) says the WebAuthn port is "501, deferred"; 1D implemented dev-mode passkeys, but it is still 501 in
  Keycloak mode (the production mode), which `docs/batches.md:28` does not say.
- `docs/batches.md:9` marks Batch 1 Platform "✅ complete"; that is true only for the batch scope. Against L2-A, 16 of 45 rows are
  missing (tenant provisioning, person/identity, role management, delegation/SoD, workflow governance, audit investigation and
  retention, module registry, master data, operational readiness).
- `docs/batches.md:30` says container verification could not be run for Batch 1; nothing records it being done later.
- `docs/security/findings.md` SEC-9D-004 (no RLS) and SEC-9D-005 (no throttling) are still `open`, while `docs/batches.md` reports
  Batch 9 hardening as ✅.
