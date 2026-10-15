# Part 1 · Stage 3 — approval control (S3-appr)

Branch `part1/s3-appr` (from `part1/s2-int` @ 1fd0e322). Findings: `docs/readiness/security/appr.md`.
Matrix: `docs/readiness/APPROVAL_CONTROL_MATRIX.md`. CI = workflow "SutraERP CI" on this branch,
demo = "SutraERP Part 1 demo (full stack)".

| ID | Issue | Root cause | Files changed | Status | Tests | CI result + run id | Blockers | Commit |
|---|---|---|---|---|---|---|---|---|
| SEC-P1-APPR-001 / DEF-X-002 (rest) | ~101 decision endpoints had no separation of duties | Only the 15 Stage-2 money decisions read their makers from module status history; the rest checked the decide permission only and no shared record of who requested a governed decision existed | `migrations/20261202004100_platform_approval_request_ledger.sql`, `crates/kernel/src/approval.rs`, `crates/persistence/src/approval.rs`, `crates/web/src/auth.rs`, every `modules/*/src/shared/approval.rs`, ~60 decision handler files, `apps/api/tests/approval_matrix.rs` | Fixed | `approval_matrix::*` (fitness: all 124 decision ops guarded or allowlisted, matrix lists every op), `db_p1_s3_appr::approval_ledger_refuses_system_and_requesters`, existing DB/e2e suites decide with a second user | see status line | — | 9a74b030, 619dad6f, 5a07b69c, eab0e279 |
| SEC-P1-APPR-002/003 | Ledger tampering; system / anonymous deciders | — (new control) | migration trigger + grants; `ensure_independent_decider` | Fixed | `db_p1_s3_appr::approval_ledger_refuses_system_and_requesters`, `…finance_adjustment…`, `…payroll…`, `…vendor_invoice…` | see status line | — | 9a74b030, 05006c76 |
| DEF-P1-DEMO-006 | Eligibility override undecidable in human mode / self-decided in auto mode | `evaluate` requested an approval with no decision endpoint | `modules/academic/src/offering/enrolment.rs`, `offering/mod.rs`, `migrations/20261202004101_academic_enrolment_override_request.sql`, `modules/platform/src/auth/dev.rs`, `deploy/keycloak/realm-sutra-dev.json`, `apps/web/e2e/lib/{approvals,auth,journey-seed}.mjs`, openapi + api-client | Fixed | `db_p1_s3_appr::enrolment_override_is_decided_by_a_second_administrator`; demo journey | see status line | — | daeeaab5, 483eaaeb, 72a56e80 |
| SEC-P1-APPR-005/006 | Bypass by posting a pending record / editing after approval | Reviewed — no bypass found | `apps/api/tests/db_p1_s3_appr.rs` (regressions) | Verified | `db_p1_s3_appr::*` | see status line | — | 05006c76 |
| Docs | Matrix with every decision endpoint (maker, checker, SoD, tests) | — | `docs/readiness/APPROVAL_CONTROL_MATRIX.md`, `docs/readiness/tools/approval_matrix_table.py`, `security/appr.md`, this file | Done | `approval_matrix::the_approval_control_matrix_lists_every_decision_endpoint` | see status line | — | bd94de56 |

Tests that encoded self-approval were changed to decide with a second user (never by removing a
guard): see the commit diffs above.

## Status

Code head **7f4c129a** (all S3-appr code + tests): SutraERP CI run **38772672703** green (Rust fmt·clippy·test,
Rust integration on PostgreSQL 18 incl. `db_p1_s3_appr` + `approval_matrix`, OpenAPI drift, web,
8 e2e suites, container images); Part 1 demo (full stack, human approval mode) run **38772672672** green.
Coverage confirmed in `db_p1_s3_appr`: posting/applying/completing/reversing a pending record → 409;
amount change after approval refused (settlement recalculation, invoice re-match) or superseded into a
new pending/draft record (finance adjustment correction). This status commit is docs-only (`[skip ci]`).
