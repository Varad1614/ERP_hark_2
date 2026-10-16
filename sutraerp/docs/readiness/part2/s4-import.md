# Part 2 · Stage 4.4 — S4-import: CSV / XLSX data migration

Branch `part2/s4-import` (from `erp_merge` c31cc487). Status: **READY_FOR_REVIEW** — API, engine,
nine import targets, migration and DB tests green in CI. The admin UI is a separate later task
(not on this branch).

## What was built

**Platform job (Domain A, `modules/platform/src/dataimport/`)** — owns the job, never another
domain's tables:

| Step | Endpoint (`/api/v1/platform/imports…`) | Behaviour |
|---|---|---|
| entities / template | `GET /entities`, `GET /entities/{entity}/template?format=csv\|xlsx` | columns, natural key, approval need, caller's access; blank template with an example row |
| upload | `POST` (Idempotency-Key) | CSV (RFC 4180: quotes, CRLF, UTF-8 BOM) or XLSX (first sheet; shared/inline strings, numbers, dates by style; formulas never evaluated); ≤ 8 MiB decoded, ≤ 20,000 rows, ≤ 200 columns, zip-bomb guard; idempotent on the job key (default `entity:institution:sha256`): 201 new, 200 same key + same file, 409 same key + other file; **404 unless the institution belongs to the caller's tenant** (InstitutionDirectory port) |
| mapping | `PUT /{id}/mapping`, `GET/POST /mapping-profiles` | header → column (suggested automatically, or a saved profile); on-existing `skip` / `update` |
| dry run | `POST /{id}/dry-run` | column checks (required, date, decimal, integer, boolean, e-mail, choice, length), in-file duplicates by natural key, then every row through the owning module's `ImportTargetPort::apply(dry_run = true)` — the module's own validators/writers in a rolled-back transaction; rows already in the database → `skip` |
| report | `GET /{id}/rows?errorsOnly=`, `GET /{id}/errors.csv` | per-row action / errors / record id; CSV error report (line, column, code, message + source cells) |
| commit | `POST /{id}/commit` | batches of `batchSize` (default 500), one transaction per batch, a savepoint per row; a failing row rolls its whole batch back, earlier batches stay, job `failed` with the line and reason; resuming / re-importing skips committed rows by natural key |
| approval | `POST /{id}/decision` | entities with `requiresApproval` (opening balances) go `pending_approval`; `platform.import.approve` holder who is **not** the uploader / commit requester decides (approval ledger guard + maker columns, 403); rejection needs a reason |
| reconciliation | job `summary` | rows in file, created / updated / skipped / errors, duplicates in file, existing in DB, batches; money entities: Σ file vs Σ committed vs Σ ledger + `reconciled` |
| export | `GET /entities/{entity}/export?institutionId=&format=` | the import columns; formula-injection safe (`=`, `+`, `-`, `@`, TAB, CR → `'` prefix; XLSX inline strings only); re-imports unchanged (the guard is stripped on read); audited |

Every step is audited (`platform.import.upload|mapping|dry_run|commit_request|approve|reject|commit|export`).
Migration `20261231044001_platform_data_import.sql`: `platform.import_mapping_profile`,
`platform.import_job`, `platform.import_job_row`, `finance.charge` source `opening_balance` (unique
per student account), `student.historical_result`; ends with `SELECT platform.apply_tenant_rls();`.

**Import targets (each in its owning module, registered in `apps/api/src/app.rs::import_state`)**:
campuses, org units, programmes, curricula (Domain B); staff (Domain E, 7A writer); students with an
optional guardian (relationship `proposed`; verification stays human) and historical results with
credit-ledger entries (Domain C); fee heads (5A writer) and opening fee balances (a payable charge
created, validated and **published through 5A's charge flow** → Dr student receivables / Cr
accumulated fund via the `OPENING-BAL` fee head) (Domain F).

**Permissions**: `platform.import.manage` (administrator) **plus** the entity's own manage
permission; `platform.import.approve` (administrator, `finance_approver`; step-up permission). Row
in `docs/readiness/APPROVAL_CONTROL_MATRIX.md`: `platform_import_job_decision`.

## Config keys

None new. Request body limit `SUTRA_MAX_BODY_BYTES` (default 2 MiB) bounds the base64 upload
(~1.5 MiB file) below the engine's 8 MiB limit; raise it for bigger single files or split the file.

## Part 1 changes checked in code

* Tenant RLS (DEF-X-003): new tables carry `tenant_id` and get `apply_tenant_rls()`; jobs are read with
  `tenant_id = $tenant` as well — present.
* SoD (DEF-X-001/002): the decision handler calls `guard_decision` and the engine
  `guard_decision_by` with the job's maker columns — present; fitness test `approval_matrix` passes
  with the new row (126 decision endpoints).
* Formula injection (SEC-P1-APPSEC-006, `apps/web/app/lib/csv.ts`): the server export uses the same
  prefix set. Difference noted (not a defect): the server also guards plain negative / `+` numbers
  (`'-12.50`), the web leaves them as numbers; the import strips the guard, so round trips are exact.

## Test evidence

`apps/api/tests/db_p2_import.rs` (PostgreSQL 18, real composition root, runtime role under RLS):

| Test | Proves |
|---|---|
| `dry_run_reports_row_errors_and_duplicates` | required / date / e-mail column errors, unknown programme and guardian-without-contact domain errors, in-file duplicate, DB duplicate (`skip`), error rows + `errors.csv`, commit of an invalid job → 409, nothing written, faculty → 403, audit |
| `failed_batch_rolls_back_and_reimport_is_idempotent` | batch 2 fails at commit (enrolment-number conflict invisible to the per-batch dry run) → whole batch rolled back, batch 1 kept, job `failed` with line + reason; re-dry-run sees the conflict; corrected file resumes (2 skip + 2 create); same file → same job (200); same key + other file → 409; re-import under a new key creates nothing; committed job → 409 on commit / dry run |
| `opening_balances_need_a_second_approver_and_reconcile_to_the_ledger` | checker cannot upload (403); decision before request → 409; commit → `pending_approval`, nothing posted; maker → 403, faculty → 403; rejection needs a reason, then `rejected`; re-request + finance checker approval → committed; file = committed = ledger total (17,599.75), `reconciled`; receivables −17,599.75 / accumulated fund +17,599.75, entries balanced; audit trail; re-import posts nothing |
| `cross_tenant_jobs_and_institutions_are_invisible` | another tenant: GET / rows / errors.csv / dry-run / commit / decision / mapping → 404, not listed; upload or export against the foreign institution → 404, no job recorded |
| `xlsx_round_trip_and_formula_injection_safe_export` | XLSX template + upload; `=HYPERLINK(…)`, `@SUM(…)`, `-Dash`, `+91…` stored as text; CSV export guards them (`'=…`); XLSX export re-imports with 0 created / 0 errors |
| `two_thousand_students_import_timing` | 2,000 students + 2,000 guardians from one CSV (303,034 bytes) |

Unit tests in the modules: CSV/XLSX parsing, formula guards, header / size validation (`files.rs`), batching
and grouping (`engine.rs`), each target's spec / natural key / cell validation, permission grants
(`crates/kernel/src/auth.rs::part2_data_import_grants`).

**2,000-student timing** (CI run 38885818493, GitHub-hosted runner, debug build, PostgreSQL 18
service container): upload 113 ms, dry run 7,552 ms (server 7,538 ms), commit 7,619 ms (server
7,592 ms, 4 batches of 500), total 15,285 ms — about 3.8 ms per student per phase, including the
guardian, two domain events and the audit record per row. Second run on the final code head
(CI 38888559376): upload 148 ms, dry run 11,034 ms, commit 10,931 ms, total 22,113 ms (runner
variance).

CI evidence: SutraERP CI 38885818493 (b0574f90) and 38888559376 (fea1e75c) green; SutraERP
quality gates 38887796602 (fea1e75c) success (Security incl. gitleaks, SAST, image scan + ZAP, performance, capacity). Per-step run ids in
`docs/readiness/tracker/p2-s4-import.md`.

## BLOCKED_EXTERNAL

None. Everything this stage needs (PostgreSQL, the modules' own writers) runs in CI. Validation with
a real college's legacy export needs a sample file from the college (no data available to the
project); the mapping profiles exist for that.

## Files other workers may conflict on

`apps/api/src/app.rs` (`import_state`, router merge), `crates/kernel/src/auth.rs` (two
permissions + grants), `Cargo.toml` / `Cargo.lock` (`zip` 9, `rust_decimal` in student),
`.github/workflows/sutraerp-ci.yml` (one extra step after the api DB suite), `sutraerp/.gitleaks.toml`
(identical hunk to S4-pay's SEC-P2-PAY-001 allow-list), `openapi/openapi.json`,
`packages/api-client/src/schema.d.ts`, `docs/readiness/APPROVAL_CONTROL_MATRIX.md`.
