# Part 2 Stage 4.2 — `docs` (branch `part2/s4-docs`)

Section: [`../part2/s4-docs.md`](../part2/s4-docs.md). Base `erp_merge` c31cc487.

Part 1 baseline checked in code: before this stage admission / record documents carried only a
free-text `fileRef` (`dms://…`) with no storage behind it (journey used `dms://admissions/*.pdf`);
no object storage adapter, no file endpoints, no upload size route in nginx (the S4-docs draft's
"nginx limits" were not present in `deploy/docker/nginx.conf` — added in this stage). Part 1
controls kept: forced tenant RLS (new table calls `platform.apply_tenant_rls()`), audit,
production refusal of dev adapters / secrets, 2 MiB global body limit.

| Stage | Task | Defect / feature | Changed files | Tests | Actual result + run id | Status | Blockers | Commit |
|-------|------|------------------|---------------|-------|------------------------|--------|----------|--------|
| 4.2 | S3-compatible storage adapter (SigV4, presign, SSE, privacy proof) | F: object storage (no storage behind `fileRef`) | `crates/infra/src/s3.rs`, `crates/infra/tests/live_s3.rs`, `crates/infra/Cargo.toml` | unit (SigV4 vectors, encoding, errors); `live_s3` on real MinIO | RUN_CI | COMPLETED | — | 20be923b…, b85bc83 |
| 4.2 | Document vault contract + PgDocumentVault (tenant keys, magic bytes, active-PDF refusal, SHA-256 on read, quarantine, retention / legal hold, sweeper, audit) | F | `crates/contracts/src/platform/documents.rs`, `modules/platform/src/documents/*`, `migrations/20261231000400_platform_document_object.sql`, `…000401_student_document_file.sql` | validate unit tests; `db_p2_docs` (7 tests) | RUN_CI | COMPLETED | — | 6057e8fa, 20be923b |
| 4.2 | Student file endpoints (admission, record, self-service; verification re-checks bytes; files move to the student record) | F | `modules/student/src/files.rs`, `admission/flow.rs`, `record/{creation,documents}.rs`, `openapi/openapi.json`, `packages/api-client/src/schema.d.ts` | `db_p2_docs` round trip, isolation, tamper, validation, retention, student retrieval, disabled 503; OpenAPI drift | RUN_CI | COMPLETED | — | 20be923b, dc1f7524 |
| 4.2 | Config + production gate (`SUTRA_DOCUMENT_STORAGE`, `SUTRA_S3_*`, `SUTRA_DOCS_*`) and app wiring (readiness, sweeper, per-route body limit) | F | `apps/api/src/{config_docs,config,config_http,storage,app,main,lib}.rs` | config unit tests (prod refuses disabled / http / SSE none / dev secrets / create-bucket) | RUN_CI | COMPLETED | — | a6c6442f, 90cdc01a, c6025493 |
| 4.2 | MinIO built from archived upstream source (no pullable images; no mirrors) for CI + compose | Infra | `deploy/minio/*`, `deploy/docker/minio*.Dockerfile`, `docker-compose{,.demo}.yml`, `.env.example`, `deploy/ci/demo-wait.sh`, `.github/workflows/sutraerp-ci.yml`, `sutraerp-part1-demo.yml` | rust-db: build + start + bucket + anonymous 403; demo: compose health | RUN_CI / RUN_DEMO | COMPLETED | — | db7c3647, 6cdab885, 2b176499, 5d018a57 |
| 4.2 | Test defects found on first real run: extension-mismatch case also declared a mismatching type; tamper test verified before `documents_submitted` (409 for the wrong reason, missing-object audit never reached) | Test defect | `apps/api/tests/db_p2_docs.rs` | `db_p2_docs` | run 38881775496: 5/7 → fixed, RUN_CI | COMPLETED | — | 22be005 |
| 4.2 | nginx upload route (11 MiB, 120 s) | F | `deploy/docker/nginx.conf` | demo journey (1.5 MiB upload through the web proxy) | RUN_DEMO | COMPLETED | — | f08cc6d |
| 4.2 | Journey: upload + download admission documents, student retrieval, DB + audit | F | `apps/web/e2e/journey.e2e.mjs`, `apps/web/e2e/lib/auth.mjs` | Part 1 demo journey | RUN_DEMO | COMPLETED | Web upload control not built (gap recorded) | f08cc6d |
| 4.2 | Quality gate: gitleaks full-history scan flagged the s4-pay stub fixture (7fc92f45, other branch) | Gate | `.gitleaks.toml` (identical hunk to s4-pay bc06a207) | quality security job | RUN_QUALITY | COMPLETED | — | b85bc83 |
| 4.2 | Live AWS S3 / institution storage | Validation | — | — | not run | BLOCKED_EXTERNAL | AWS account + bucket + scoped key, or the college's TLS S3 endpoint + key | — |
