# Domain B — Institution & Structure: readiness inventory (Part 1 · Stage 1)

Base: `part1/readiness` = `erp_merge` `fb205860`, CI run 38724169019 (all jobs green).
Scope: `modules/institution` (2A lifecycle / policies / organisation / positions, 2B campus / curriculum, 2C calendar / readiness),
web `apps/web/app/features/institution` (Admin → Campus setup, Admin → Academics), migrations `20261015*_institution_*`.
Rows in [`B-institution.csv`](B-institution.csv) follow `architecture/L2_Domain_Workflow_Architecture/section-b/` (B01 + activation
authority amendment, B02 WF01–WF08, B03 sections 102–112, B04 operational / governance / compliance workflows + retirement event
amendment, B05 sections 38–49).

Evidence rules as in the Domain A file: API = named test in `apps/api/tests/db_b2*.rs` that passed in run 38724169019 (all 31
Domain B DB tests are listed as `ok` in the "Rust integration" job log); browser = e2e spec in the CI matrix that drives the
flow. **No Domain B screen is browser-tested**: the e2e harness (`apps/web/e2e/lib/harness.mjs:164-279`) builds the institution
through API calls, and `campus.e2e.mjs:581` only screenshots `/campus/institution`, `/campus/campuses`, `/campus/organisation`,
`/campus/positions`, `/campus/policies` and `/campus/readiness`.

## Summary

| Status | Rows |
|---|---|
| implemented | 37 |
| partial | 5 |
| missing | 0 |
| deferred | 1 |
| **total** | **43** |

| Release readiness | Rows |
|---|---|
| ready | 17 |
| ready-with-gaps | 23 |
| not-ready | 2 |
| deferred | 1 |

The backend covers every L2-B workflow listed, with tenant-filtered SQL, `If-Match` / idempotency, per-handler permission
checks (every one of the 140 Domain B operations in `openapi/openapi.json` requires a permission) and an integration test per slice. The two release blockers are governance ones:
**every approval step is auto-approved in every configuration** and **submit activates an institution without the Activation
Authority**. Roughly a third of the API has no web screen.

## Defects

| Id | Sev | Title |
|---|---|---|
| DEF-B-001 | P1 | Every Domain B approval is auto-approved; decision endpoints can't be reached in a deployed system |
| DEF-B-002 | P1 | `submit` (institution.manage, no step-up) activates the institution, bypassing the Activation Authority |
| DEF-B-003 | P2 | Decision endpoints reuse the requester's permission (no separate decision authority) |
| DEF-B-004 | P2 | Institution scope comes from the URL path, not the session |
| DEF-B-005 | P2 | Curriculum draft / successor version content cannot be edited in the web |
| DEF-B-006 | P2 | Large API surface with no web screen |
| DEF-B-007 | P3 | Module descriptor still says `Scaffolded` |
| DEF-B-008 | P3 | Domain B admin screens have no browser test |
| DEF-B-009 | P3 | Web build silently falls back to in-browser mock data when `VITE_AUTH_MODE` is unset |

**DEF-B-001 · P1 · All approvals auto-approved**
- Evidence: `modules/institution/src/shared/state.rs:21-27` (`InstitutionState::new` hard-codes `ApprovalMode::AutoApprove`);
  `modules/institution/src/lib.rs:76-77` (router always uses `new`, nothing sets `AwaitDecision`); `shared/approval.rs:47-51,104-106`;
  applied at `lifecycle/repo.rs:288`, `campus/repo.rs:650,987`, `curriculum/publication.rs:246`, `org/blueprint.rs:375`,
  `org/restructure.rs:557`, `calendar/repo.rs:1053,2012,3027`. Switching `SUTRA_WORKFLOW=flowable` does not change this (the mode is
  not derived from the adapter; the BPMN also defaults `autoApprove=true`, `modules/platform/src/workflow_flowable.rs:10`). Only tests set
  `st.approvals = ApprovalMode::AwaitDecision` (e.g. `apps/api/tests/db_b2b.rs:998`).
- Root cause: Batch 2 shortcut ("Batch 2 runs AutoApprove", `approval.rs:18`) never revisited when 9C landed.
- Fix: add `approvals` to `InstitutionDeps`, set it from config (`AwaitDecision` whenever `SUTRA_WORKFLOW=flowable`, and in production),
  pass `autoApprove=false` in the process variables; e2e harness then calls the decision endpoints.

**DEF-B-002 · P1 · Activation without the Activation Authority**
- Evidence: `modules/institution/src/lifecycle/http.rs:88` (`submit` requires only `InstitutionManage`, which is not in
  `crates/kernel/src/auth.rs:577-583` `requires_step_up`); `lifecycle/repo.rs:288-297` then applies `ActivationDecision::Approve` as
  `system:auto-approval`. The explicit decision path needs `InstitutionActivationDecide` + step-up (`lifecycle/http.rs:117`).
  Contradicts `L2-B01_Institution_Activation_Authority_Amendment_2026-08-24.md` §1 ("does not gain universal final activation
  authority merely by administering Institution configuration").
- Fix: resolved by DEF-B-001 (pending until a decision); until then require `InstitutionActivationDecide` + step-up on `submit`.

**DEF-B-003 · P2 · No separate decision authority**
- Evidence: decision handlers require the same permission as the request: org restructure `org/restructure.rs:772`
  (`OrganisationManage`), blueprint decision `org/blueprint.rs:633` (`OrganisationManage`), curriculum version decision
  `curriculum/framework.rs:1288` (`CurriculumPublish`), calendar version decision `calendar/http.rs:352` (`CalendarPublish`).
  Only institution activation (`InstitutionActivationDecide`) and campus decommission have a distinct permission.
- Fix: add `*.decide` permissions (or an approver check `decided_by <> submitted_by` where institution policy demands it).

**DEF-B-004 · P2 · Path-selected institution scope**
- Evidence: `modules/institution/src/shared/scope.rs:14-16` builds the scope from `ctx.scope.tenant_id` + the path id and ignores
  the session's institution; documented as "a later-batch hardening" (`scope.rs:7-8`). Any role with `institution.read` (faculty
  included) reads every institution of the tenant; any administrator writes to any of them.
- Fix: when the session scope carries an institution, refuse other ids (404) unless the role is tenant-wide.

**DEF-B-005 · P2 · Curriculum drafts not editable in the web**
- Evidence: `apps/web/app/features/institution/api.ts:157-230` `PATHS` has no `curriculum-versions/{versionId}` (PATCH) entry;
  `CurriculumTabs.tsx:332` creates a successor draft (`reviseCurriculum`) and `:336` can only publish or retire it. A revision
  published from the web is therefore identical to its predecessor.
- Fix: add the version editor (components, credit rules) on top of `PATCH /curriculum-versions/{versionId}`.

**DEF-B-006 · P2 · API without screens**
- Evidence: `api.ts:157-230` `PATHS` (the only endpoint list the web uses) lacks: governance configs (B-012), org restructures
  (B-014), type migration (B-013), org-unit PATCH and history (B-011), campus PATCH and revisions (B-020), `PUT …/timezone` and
  `GET …/profile/revisions` (B-005), policy revisions (B-006), position PATCH / changes / revisions (B-017), calendar resolution
  (B-037), school-model PATCH / revise (B-031), structure revise, activation-requests list, and every `…/decision` endpoint except
  institution activation and simulation.
- Fix: build the missing forms, or mark them API-only in the release notes.

**DEF-B-007 · P3 · Stale module status**
- Evidence: `modules/institution/src/lib.rs:61` `status: ModuleStatus::Scaffolded`, reported by `GET /api/v1/platform/modules`,
  although `docs/batches.md:10` says Batch 2 is complete.
- Fix: `ModuleStatus::Active`.

**DEF-B-008 · P3 · No browser test of Domain B screens**
- Evidence: `apps/web/e2e/campus.e2e.mjs:581-588` only visits and screenshots the Domain B tabs; `e2e/lib/harness.mjs:174-279` drives
  registration → activation through `admin.call` (API). The 356-line `institution.test.tsx` runs against the in-memory mock.
- Fix: a click-through step for register → profile → configure → readiness → submit, and the calendar publish/open flow.

**DEF-B-009 · P3 · Silent mock fallback**
- Evidence: `apps/web/app/lib/api.ts:12-13` — any `VITE_AUTH_MODE` other than `api`/`oidc` (including unset) selects `mock`, and
  `features/institution/hooks.tsx:19-21` then serves `mock.ts` (702 lines of in-memory data). Mitigated: `deploy/docker/web.Dockerfile:15`
  defaults `VITE_AUTH_MODE=api`.
- Fix: fail the production build when the mode is unset.

## Test gaps

- Every decision path (`AwaitDecision`) is tested only by building router state by hand in the test (`db_b2a.rs`
  `activation_authority_decisions_return_reject_and_stale_approval`, `db_b2b.rs:998`, `db_b2c.rs`
  `await_decision_mode_keeps_steps_pending_until_decided`); no test covers the deployed composition with approvals pending.
- No test asserts that `submit` cannot activate without the decide permission (DEF-B-002).
- No cross-institution (same tenant) isolation test; existing `tenant_isolation` tests cover tenants only (DEF-B-004).
- No browser test of any Domain B screen (DEF-B-008).
- Calendar simulation impact on D/I is not tested against real downstream data (reported as `downstream.unknown`, `calendar/domain.rs:1342`).

## Doc discrepancies

- `README.md:240,262,279` and `shared/approval.rs:15-18` say approval is auto-approved "in Batch 2" and that `AwaitDecision` comes
  "later batches, Flowable"; after 9C it is still auto-approved in every configuration (DEF-B-001). `docs/batch9-contracts.md:270`
  says modules pass `autoApprove = (ApprovalMode == AutoApprove)`, but Domain B never runs `AwaitDecision` outside tests.
- `modules/institution/src/lib.rs:70` comment "`NoopWorkflowAdapter` in Batch 2" and `ModuleStatus::Scaffolded` (`lib.rs:61`) are stale.
- `docs/batches.md:43` (2C) lists a "Docker click-through" and `:48-52` puts container verification "on the user's machine"; no
  evidence in CI that it was run.
- `docs/batches.md:10` says Batch 2 covers "departments"; the implementation has generic blueprint-typed org units (no dedicated
  department aggregate). That is consistent with L2-B02, but the wording differs.
- L2-B03 assigns physical campus attributes to L2-G10; the README campus row (`README.md:262`) does not mention the hand-off.
