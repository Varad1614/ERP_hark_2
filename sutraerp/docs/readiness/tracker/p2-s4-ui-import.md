# Part 2 Stage 4.4 — `ui-import` (branch `part2/s4-ui-import`)

Admin Data import screen. Section: `docs/readiness/part2/s4-import.md` § "Admin UI — Data import
screen". Base: `part2/s4-int` @ 9b4de991.

| Stage | Task | Defect / feature | Changed files | Tests | Actual result + run id | Status | Blockers | Commit |
|---|---|---|---|---|---|---|---|---|
| 4.4 | Admin → Data import screen | Feature: entity picker + CSV/XLSX template, upload (limit shown, 413 explained), mapping (suggested, saved profiles, save-as), dry run (counts, row errors, error CSV), commit / request approval, approve / reject (maker ≠ checker), reconciliation, history, export — `@sutra/ui` components only | `apps/web/app/features/imports/{api,hooks,mock,index}.ts(x)`, `apps/web/app/routes/admin/data-import.tsx`, `apps/web/app/routes.ts` | vitest `features/imports/imports.test.tsx` (12) | Web job green — CI 38901963467 (34b2a730) | COMPLETED | — | 42b1eac9, 081fe4a8, bc7ed353 |
| 4.4 | Navigation by permission | Feature: avatar menu + palette "Data import" only with `platform.import.manage`; "Import approvals" for approve-only sessions; session carries `subject` + API roles | `apps/web/app/routes/app-shell.tsx`, `apps/web/app/lib/{auth.tsx,permissions.ts}` | vitest (grants / tabs per role), e2e (menu entry) | Web job green — CI 38901963467 (34b2a730) | COMPLETED | — | 42b1eac9 |
| 4.4 | Upload body limit | **DEF-P2-UIIMP-001** (functional): the required `Idempotency-Key` made the idempotency middleware buffer the upload with a 1 MiB cap → files above ≈ 768 KB failed with 400 (docs claimed ≈ 1.5 MiB; `SUTRA_MAX_BODY_BYTES` 2 MiB next). Fix: import upload route limit = base64 of 8 MiB + 64 KiB (`UPLOAD_BODY_BYTES`), extractor limit lifted on the import router, route limit passed to the idempotency middleware (`RouteBodyLimit`, fingerprint only), 413 (not 400) above it; nginx + helm `location = /api/v1/platform/imports` 11m | `apps/api/src/storage.rs`, `crates/web/src/idempotency.rs`, `modules/platform/src/dataimport/http.rs`, `deploy/docker/nginx.conf`, `deploy/helm/sutraerp/templates/web/configmap.yaml` | `idempotency::fingerprints_bodies_up_to_the_route_limit`, `storage::only_the_import_upload_gets_the_import_limit`, DB `db_p2_ui_import::upload_route_accepts_files_above_the_default_body_limit`, demo step 8 (2.2 MiB through nginx) | Rust unit (`fingerprints_bodies_up_to_the_route_limit`, `only_the_import_upload_gets_the_import_limit`) + Rust integration (`db_p2_ui_import` 2/2, 2.4 MiB file / 3.2 MiB body → 201, 12 MiB → 413, 3 MiB on other import routes → 413) green — CI 38901963467 (34b2a730); demo step 8 green — Part 1 demo 38901966528 (34b2a730) | COMPLETED | — | 5bc7fd07, cf1679f3, 1b2fa0d2, 3c92d8c0, 56d678ad, 34b2a730 |
| 4.4 | Approver read scope | Defect (usability of SoD): the finance checker (`platform.import.approve` only) could decide a job it could not read. List / get / rows now admit approve-only callers for approval-needing jobs only (others 404, no step-up for reads; everything else stays manage-only) | `modules/platform/src/dataimport/http.rs` | DB `db_p2_ui_import::finance_checker_reads_only_the_imports_it_decides`; e2e (checker lists the pending job) | `db_p2_ui_import::finance_checker_reads_only_the_imports_it_decides` green — CI 38901963467 (34b2a730); e2e green (E2E imports job, same run) | COMPLETED | — | 5bc7fd07 |
| 4.4 | Playwright e2e | small CSV → row errors → error CSV → corrected CSV → dry run → commit → reconciliation; opening balances → request approval (requester sees no Approve) → checker approves → reconciled; history; export | `apps/web/e2e/imports.e2e.mjs`, `apps/web/package.json`, `package.json`, `.github/workflows/sutraerp-ci.yml` (job `e2e-imports`, runs on `part2/**` too) | `pnpm e2e:imports` | **E2E imports (preview)** green — CI 38898351220 (0f172df6) and CI 38901963467 (34b2a730) | COMPLETED | — | e1c21657 |
| 4.4 | Part 1 demo journey | Step "8 Data import": 2.2 MiB students CSV through the screen behind nginx (compose, Keycloak) → dry run → commit → reconciliation; DB, audit, faculty 403 | `apps/web/e2e/journey.e2e.mjs` | Part 1 demo | green — Part 1 demo 38898353897 (0f172df6), 38900237745 (56d678ad), 38901966528 (34b2a730) | COMPLETED | — | 0f172df6 |
| 4.4 | Docs | UI section in `s4-import.md`; this tracker | `docs/readiness/part2/s4-import.md`, `docs/readiness/tracker/p2-s4-ui-import.md` | — | — | COMPLETED | — | 98968638, 56d678ad, this commit |

## Final verification (head 34b2a730)

| Workflow | Run | Result |
|---|---|---|
| SutraERP CI (Rust fmt/clippy/test, Rust integration incl. `db_p2_ui_import`, Web, OpenAPI drift, E2E imports) | 38901963467 | green |
| SutraERP Part 1 demo (full stack) | 38901966528 | green |
| SutraERP quality gates (security · performance) | 38902714112 | green |

Finisher fixes (tests only, no product change): `db_p2_ui_import` built a 330-row file whose
upload body was 3.0 MiB, under its own `> 3 MiB` assertion → 360 rows (2.4 MiB file, 3.2 MiB
body); the idempotency unit-test handler echoed the 1.5 MiB request body, exceeding the 1 MiB
replay buffer (500) → it now returns the body length, asserted to equal the large body.

## Notes

* No new configuration key. No new dependency (no cargo-deny / npm change).
* Part 1 checks in code: SoD (`guard_decision` + engine maker columns) unchanged and still the
  control; the UI hides the decision from the maker in addition. CSRF via the shared client
  middleware; tenant scope from the session only; formula-safe exports stay server-side.
* The dev sign-in page offers the three personas only, so the finance checker's decision is driven
  through the API in the e2e (its read scope is asserted there and in `db_p2_ui_import`).
* BLOCKED_EXTERNAL: none.
