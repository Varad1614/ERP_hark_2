# Part 2 Stage 6 — `pilot` (branch `part2/s6-pilot`)

Pilot preparation: proposed pilot scope, UAT cases, blank real-user templates, a guarded reset of the
synthetic pilot tenant, and a **synthetic** UAT dry run. Base: fac412ce (`part2/s4-int`, Stage 4 integrated).

> **No real pilot has taken place — BLOCKED_EXTERNAL.** There is no authorised institution, no pilot
> users and no institutional authorisation. Nothing below is a UAT result, user feedback or
> acceptance. `UAT_RESULTS.md`, `USER_FEEDBACK.md`, `INSTITUTION_ACCEPTANCE.md` and the real-defect
> part of `PILOT_DEFECT_REGISTER.md` are deliberately blank templates.

| Stage | Task | Defect / feature | Changed files | Tests | Actual result + run id | Status | Blockers | Commit |
|---|---|---|---|---|---|---|---|---|
| 6 | Pilot scope | Feature: PROPOSED pilot scope (modules, roles, data, parallel-run, exit criteria) | `docs/readiness/PILOT_SCOPE.md` | — (document) | written; marked PROPOSED | BLOCKED_EXTERNAL | institution must review, adjust and sign | 95798446 |
| 6 | Real-user templates | Feature: blank UAT results / feedback / defect register / acceptance templates | `docs/readiness/{UAT_RESULTS,USER_FEEDBACK,PILOT_DEFECT_REGISTER,INSTITUTION_ACCEPTANCE}.md` | — | blank by design (no real users) | BLOCKED_EXTERNAL | named institutional testers + sign-off authority | 95798446, 2ef11028 |
| 6 | UAT test cases | Feature: role-based cases (admin, faculty, accounts, exams, student), migration validation (Stage 4 import dry run + reconciliation), reports vs institutional records | `docs/readiness/UAT_TEST_CASES.md` | synthetic dry run of one key case per role | dry-run column + runs table filled from demo 38945114014 (5/5 pass on cf4c3297) | READY_FOR_REVIEW (cases); real execution BLOCKED_EXTERNAL | pilot institution + users | d9b63080, 1fbfa2ab |
| 6 | Synthetic UAT dry run | Feature: Playwright walk-through, Keycloak OIDC + TOTP, human approval mode, DB + audit checks | `apps/web/e2e/uat-dryrun.e2e.mjs`, `.github/workflows/sutraerp-part1-demo.yml`, `apps/web/package.json`, `package.json` | demo workflow step “UAT dry run” | 38942486947 on 56fc11b6: 4/5 (UAT-ACC-01 test assertion bug, below); **38945114014 on cf4c3297: 5/5 pass** | COMPLETED (synthetic only — not UAT) | — | 0eb944a1, 56fc11b6, cf4c3297 |
| 6 | Dry-run test fix | Test defect (not product): UAT-ACC-01 “journal balanced” check expected `t`, but PostgreSQL renders a boolean concatenated with `\|\|` as `true`; check now accepts both (`t` / `true`), also on the bare boolean check | `apps/web/e2e/uat-dryrun.e2e.mjs` | dry run | 38945114014 green, `DB adjustment journal balanced: 1\|true` | COMPLETED | — | cf4c3297 |
| 6 | Dry-run TOTP window | Test defect: first OIDC sign-in reused the TOTP code the persistence phase had just used (Keycloak refuses reuse); waits for a fresh window | `apps/web/e2e/uat-dryrun.e2e.mjs` | dry run | 38945114014 (waited 21.8 s, sign-ins ok) | COMPLETED | — | 56fc11b6 |
| 6 | Malpractice approval SoD | **DEF-P2-PILOT-001** (Critical, engineering): `POST /exams/malpractice-cases/{id}/approval` had no maker ≠ checker guard (path ends in `/approval`, so the approval-matrix fitness test never enumerated it) and the guard sat on the proposal step. **Found by code review while designing UAT case UAT-EXM-01, not by the dry run**; the dry run verifies the fix. Guard moved to the approval endpoint; fitness test enumerates `…/approval` | `modules/exams/src/integrity/http.rs`, `apps/api/tests/approval_matrix.rs`, `apps/api/tests/db_p2_pilot.rs`, `docs/readiness/APPROVAL_CONTROL_MATRIX.md`, `docs/readiness/PILOT_DEFECT_REGISTER.md` | `db_p2_pilot::malpractice_decision_is_approved_by_an_independent_controller` (real PostgreSQL); `approval_matrix::every_decision_endpoint_is_guarded_or_documented`; dry run UAT-EXM-01 | CI 38941294349 green on a11fa9b0; dry run 38945114014: proposer's approval 403, controller approves, audited | COMPLETED | — | a11fa9b0 (fix), 2ef11028 (provenance wording) |
| 6 | Pilot tenant reset | Feature: `deploy/pilot/reset-pilot.sh` — guarded reset of the SYNTHETIC pilot tenant (refuses non-pilot tenants / production), verify empty, re-seed, idempotent | `deploy/pilot/reset-pilot.sh`, `deploy/pilot/README.md`, `.github/workflows/sutraerp-pilot.yml` | pilot workflow (reset → verify empty → re-seed → idempotent; refusals) | pilot 38941533428 green on 33c85fb6 | COMPLETED | — | 33c85fb6 |
| 6 | Adjustments UI for the maker | **PDEF-SYN-002** (Low, UI): Approve / Reject shown to the adjustment's own maker (API correctly 403s). Not fixed: `AdjustmentView` exposes no `createdBy` and the submitter lives in the approval ledger, so hiding needs an API view + `openapi/openapi.json` change — not a trivial UI fix | — | — | API refusal verified in dry run (maker 403) | NOT_STARTED (backlog) | — | — |
| 6 | Least-privilege roles | **PDEF-SYN-003** (Medium): no dedicated accounts-staff / examination-staff role; maker work needs the administrator role | — (PILOT_SCOPE.md §7 records the pilot decision) | — | — | BLOCKED_EXTERNAL | institution's role/duty requirements | — |

## Evidence (GitHub Actions, repo Varad1614/ERP_hark_2)

- Demo (full stack) incl. UAT dry run: **38945114014** green on cf4c3297 (5/5); earlier 38942486947 on 56fc11b6 failed 4/5 on the test assertion fixed in cf4c3297.
- SutraERP CI: 38941294349 green on a11fa9b0 (DEF-P2-PILOT-001 fix + db_p2_pilot); 38945967331 on 2ef11028 (comments/docs only over cf4c3297); final-head run id in the S6-pilot report / `part2-status.txt`.
- Pilot (seeded pilot institution, reset): 38941533428 green on 33c85fb6.

## BLOCKED_EXTERNAL

Real UAT, parallel run, user feedback, pilot defects and institutional acceptance all need: an
authorised pilot institution, named testers per role, the institution's written rules (grading, fees,
attendance, calendar), source data extracts with control totals, a sign-off authority, and a
production-grade host (physical college server, see S5-deploy).
