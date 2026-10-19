# Part 2 Stage 4.4 — `import` (branch `part2/s4-import`)

Section document: `docs/readiness/part2/s4-import.md`.

| Stage | Task | Defect / feature | Changed files | Tests | Actual result + run id | Status | Blockers | Commit |
|---|---|---|---|---|---|---|---|---|
| 4.4 | Import job engine, CSV/XLSX files, HTTP API, migration | Feature: data migration (upload → mapping → dry run → commit → reconciliation, error report, export) | `modules/platform/src/dataimport/*`, `crates/contracts/src/platform/dataimport.rs`, `crates/persistence` batch runner, `migrations/20261231044001_platform_data_import.sql`, `crates/kernel/src/auth.rs` | unit tests in `files.rs`, `engine.rs`, `auth.rs::part2_data_import_grants` | Rust fmt · clippy · test green, CI 38885818493 | COMPLETED | — | 53615cbc … 08387444 |
| 4.4 | Nine import targets in their owning modules + wiring | Feature: campuses, org units, programmes, curricula, staff, students + guardian, historical results, fee heads, opening balances (through 5A's charge/posting flow) | `modules/{institution,people,student,finance}/src/dataimport.rs`, module writers (`create_in` / `register_in` / `create_head_in` / `import_guardian`), `apps/api/src/app.rs` | per-target unit tests; `db_p2_import` | green, CI 38885818493 | COMPLETED | — | 36f948a0 … a68be36e |
| 4.4 | Clippy `items_after_test_module` | Defect (build): `import_guardian` after the tests module | `modules/student/src/guardian/repo.rs` | clippy | green, CI 38885818493 (was red in 38883875424) | COMPLETED | — | e6729142 |
| 4.4 | Upload / export against another tenant's institution | Defect (tenant isolation): a job could be recorded with a foreign institution id (nothing was written to the domain, but the job row existed) | `modules/platform/src/dataimport/{mod,engine}.rs`, `apps/api/src/app.rs` (InstitutionDirectory port) | `db_p2_import::cross_tenant_jobs_and_institutions_are_invisible` | 404, no job row, CI 38885818493 | COMPLETED | — | a4b64701 |
| 4.4 | DB tests | Dry-run errors, in-file + DB duplicates, failed-batch rollback + resume, idempotent re-import, opening balances SoD + ledger reconciliation, cross-tenant denial, XLSX round trip, formula-safe export | `apps/api/tests/db_p2_import.rs` | 6 DB tests (below) | all ok, CI 38885818493 job "Rust integration" | COMPLETED | — | b0574f90 |
| 4.4 | 2,000-student CSV volume | Performance evidence | `apps/api/tests/db_p2_import.rs`, `.github/workflows/sutraerp-ci.yml` (step with `--nocapture`) | `two_thousand_students_import_timing` | upload 113 ms, dry run 7,552 ms, commit 7,619 ms (4 × 500), total 15,285 ms — CI 38885818493 | COMPLETED | — | b0574f90 |
| 4.4 | OpenAPI + TS client | 14 operations, tag `platform-import` | `openapi/openapi.json`, `packages/api-client/src/schema.d.ts` (artifact `openapi-regenerated` of run 38883875424) | OpenAPI drift | green, CI 38885818493 | COMPLETED | — | 1325bf08 |
| 4.4 | Approval control matrix | `platform_import_job_decision` (126 decision endpoints; finance_approver holds `platform.import.approve`) | `docs/readiness/APPROVAL_CONTROL_MATRIX.md` | `approval_matrix::every_decision_endpoint_is_guarded_or_documented` | green, CI 38885818493 | COMPLETED | — | 1325bf08 |
| 4.4 | Quality gates | gitleaks scans every ref and flagged S4-pay's stub fixture (7fc92f45, not on this branch); same narrow allow-list as S4-pay (SEC-P2-PAY-001) | `sutraerp/.gitleaks.toml` | quality workflow | green, quality 38887796602 (red in 38886758313 before the allow-list) | COMPLETED | — | 7ba1def3 |
| 4.4 | Admin UI | Out of scope for this worker (later task) | — | — | — | NOT_STARTED | assigned to a later worker | — |

## DB tests (`apps/api/tests/db_p2_import.rs`)

* `dry_run_reports_row_errors_and_duplicates`
* `failed_batch_rolls_back_and_reimport_is_idempotent`
* `opening_balances_need_a_second_approver_and_reconcile_to_the_ledger`
* `cross_tenant_jobs_and_institutions_are_invisible`
* `xlsx_round_trip_and_formula_injection_safe_export`
* `two_thousand_students_import_timing`

## Final verification

| Workflow | Head | Run | Result |
|---|---|---|---|
| SutraERP CI | b0574f90 (code + tests) | 38885818493 | success (all jobs; 2,000-row timing: total 15,285 ms) |
| SutraERP CI | fea1e75c (+ gitleaks allow-list + docs; same code) | 38888559376 | success (all jobs; 2,000-row timing: upload 148 ms, dry run 11,034 ms, commit 10,931 ms, total 22,113 ms) |
| SutraERP quality gates | fea1e75c | 38887796602 | success (Security incl. gitleaks, SAST, image scan + ZAP, performance, capacity) |

Later commits after these runs change only this tracker and the section doc.
