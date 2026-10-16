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

## Admin UI — Data import screen (S4-ui-import, branch `part2/s4-ui-import`)

Built on `part2/s4-int` @ 9b4de991 (all Stage 4 backends merged). Tracker:
`docs/readiness/tracker/p2-s4-ui-import.md`.

**Screen** `/data-import/:tab?` (`apps/web/app/routes/admin/data-import.tsx`,
`apps/web/app/features/imports/`), only `@sutra/ui` prototype components (AreaHeader via
`AreaFrame` with the institution picker, BentoTile, Stepper, FormSelect, FileField, TextField,
TextArea, DataTable, FilterChips, StatCells, StatusBadge, ApprovalCard, Modal, Button):

| Tab | What it does | Endpoints (generated client `@sutra/api-client`) |
|---|---|---|
| Import | entity picker (entities without the caller's domain permission are listed disabled), matched-on key, approval / update hints, column list; **CSV / Excel template** download; file upload showing the limit (**CSV or XLSX · up to 8 MiB · 20,000 rows**; a bigger file is refused in the browser, a 413 from nginx / the API is explained); optional saved mapping applied at upload | `GET /entities`, `GET /entities/{e}/template`, `GET /mapping-profiles`, `POST /` (Idempotency-Key) |
| — mapping | every file header → canonical column (server suggestion, "apply a saved mapping", manual), required columns not mapped are listed, "save this mapping as", on-existing skip / update (entities that support it), first value of each column | `PUT /{id}/mapping`, `GET /{id}/rows` |
| — dry run | counts (create / update / skip / errors, file total for money), rows table with an Errors / All rows filter and per-row problems, **error report CSV** download; Commit disabled while a row has errors | `POST /{id}/dry-run`, `GET /{id}/rows?errorsOnly=`, `GET /{id}/errors.csv` |
| — commit | "Commit N rows", or **"Request approval"** for entities that need a second person (opening balances) | `POST /{id}/commit` |
| — approval | ApprovalCard with Approve / Reject (reason dialog) **only** for holders of `platform.import.approve` who are neither the job's uploader (`createdBy`) nor its commit requester (`commitRequestedBy`); the requester sees "another person with import approval decides it"; step-up handled by the shared `useCommand` dialog | `POST /{id}/decision` |
| — reconciliation | rows in file, created, updated, skipped (+ already in DB), errors, duplicates in file, batches, commit time; for money: file / committed / ledger total + reconciled | job `summary` |
| Approvals | imports waiting for a second person (same maker ≠ checker rule) | `GET /?institutionId=` |
| History | the institution's jobs, newest first, open one to resume it (`?job=`) | `GET /?institutionId=` |
| Export | each entity as CSV / Excel in its import columns | `GET /entities/{e}/export` |

**Navigation**: avatar menu + command palette entry "Data import" only for sessions holding
`platform.import.manage`; sessions holding only `platform.import.approve` (finance checker) get
"Import approvals" (`/data-import/approvals`). The session now carries the principal's `subject`
and every API role (`ClientSession.subject`, `.apiRoles`; `lib/permissions.ts` mirrors the two
kernel grants — it only decides what to offer; the API enforces every request).

**Backend changes the screen needed**

1. **Upload body limit (defect DEF-P2-UIIMP-001)** — the upload carries the file as base64 JSON
   and requires an `Idempotency-Key`; the idempotency middleware buffered at most **1 MiB** of
   body, so the real ceiling was a ≈ 768 KB file (not the documented ≈ 1.5 MiB; above it a 400
   "too large for idempotent processing"), and `SUTRA_MAX_BODY_BYTES` (2 MiB) was next. Fixed:
   `POST /api/v1/platform/imports` gets its own route limit `UPLOAD_BODY_BYTES` = base64 of the
   engine's 8 MiB + 64 KiB (11,250,348 bytes) in `apps/api/src/storage.rs::BodyLimits` (every
   other route, other import routes included, keeps `SUTRA_MAX_BODY_BYTES`); the import router
   lifts the extractor limit (`DefaultBodyLimit`); the body-limit middleware passes the route
   limit to the idempotency middleware (`sutra_web::idempotency::RouteBodyLimit`), which
   fingerprints up to it (only the SHA-256 is stored) and answers **413** above it (was 400);
   nginx (`deploy/docker/nginx.conf`, helm `templates/web/configmap.yaml`): `location =
   /api/v1/platform/imports` with `client_max_body_size 11m`, 120 s timeouts. No new config key.
2. **Approver read scope** — the finance checker holds `platform.import.approve` but not
   `platform.import.manage`, so it could decide a job it could not see. `GET /imports`,
   `GET /imports/{id}`, `GET /imports/{id}/rows` now also admit approve-only callers, restricted
   to jobs that need approval (list filtered, others 404); no step-up for reading (deciding still
   needs it); error report, entities, uploads, dry runs and commits stay `manage`-only.

**Tests**

* vitest `apps/web/app/features/imports/imports.test.tsx` (12): client grants and tabs per role;
  maker ≠ checker (`canDecide` for uploader, commit requester, other admin, finance checker,
  faculty, unknown subject); required-column check; HTTP layer through the generated client
  (path, `Idempotency-Key`, CSRF, base64 body, blob template with its file name, `errorsOnly`);
  413 explained; chunked base64; **mapping** (no suggestion → required columns listed → saved
  profile + manual column → stored mapping); **dry-run row errors** (line 3 with two problems,
  error report button, commit disabled); clean commit → reconciliation; **Approve / Reject hidden
  for the requester**, offered to another approver, rejection needs a reason; requester's
  waiting note on the job page; 9 MiB file refused before upload; history.
* Rust: `sutra-web` `idempotency::fingerprints_bodies_up_to_the_route_limit` (413 without a
  route limit above 1 MiB, processed once + replayed with one, never lowered);
  `apps/api/src/storage.rs::only_the_import_upload_gets_the_import_limit`; DB
  `apps/api/tests/db_p2_ui_import.rs` — `upload_route_accepts_files_above_the_default_body_limit`
  (2.3 MiB file / 3.1 MiB body → 201 through the composition root with its Idempotency-Key; 12
  MiB → 413, no job; 3 MiB on `mapping-profiles` and `/{id}/mapping` → 413) and
  `finance_checker_reads_only_the_imports_it_decides`.
* Playwright `apps/web/e2e/imports.e2e.mjs` (CI job **E2E imports (preview)**, also on
  `part2/**`): avatar menu → template download → CSV with a bad row → suggested mapping → dry run
  with row errors → error CSV → corrected CSV → commit → reconciliation (3 students in the DB) →
  opening balances → "Request approval" (no Approve for the requester, job page and Approvals
  tab, nothing posted) → finance checker lists and approves through the API → committed and
  reconciled (2 charges) → History → Export CSV.
* Part 1 demo journey step **"8 Data import"** (`journey.e2e.mjs`): in the compose stack behind
  nginx, the administrator imports a 2.2 MiB students CSV through the screen (above the 2 MiB
  default) → dry run → commit → reconciliation; DB rows, job, audit
  (`platform.import.upload|dry_run|commit`) and faculty 403 checked.

Evidence (run ids): see the tracker file.
