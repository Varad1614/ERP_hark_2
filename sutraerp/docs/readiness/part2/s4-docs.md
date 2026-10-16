# Part 2 · Stage 4.2 — Document storage (S4-docs)

Branch `part2/s4-docs` (from `erp_merge` c31cc487). Status: **READY_FOR_REVIEW** — built and
integration-tested against a real S3-compatible server (MinIO built from source) in CI; live
AWS S3 / institution storage **BLOCKED_EXTERNAL**.

## What was built

| Area | Files | What it does |
|------|-------|--------------|
| S3 client | `crates/infra/src/s3.rs` | Dependency-free AWS SigV4 client (reqwest): PUT / GET (size-capped) / DELETE object, bucket create (dev / CI only), bucket policy (ops / tests), presigned GET (≤ 300 s), path-style or virtual-hosted, SSE (`AES256` / `aws:kms` + key id). On first use the bucket is **proved private**: a probe object is written and an anonymous GET and anonymous listing must both be refused; a public bucket is refused for every operation. |
| Contract | `crates/contracts/src/platform/documents.rs` | `ObjectStorePort` + `DocumentVault` ports (owner type/id, actor, `NewDocument`, `StoredDocument`, `DocumentContent`, `DocumentLink`); modules talk to storage only through these. |
| Vault | `modules/platform/src/documents/{mod,vault,validate}.rs` | `PgDocumentVault`: tenant-prefixed keys `<tenant>/<institution>/<owner_type>/<owner_id>/<id>` (no file name in the key); type from **magic bytes** (PDF / PNG / JPEG only), PDFs with `/JavaScript` `/JS` `/Launch` `/EmbeddedFile` refused; name sanitised (path stripped, control chars); size limit; **SHA-256 verified on every read** — a mismatch / missing object quarantines the row (409) and is audited; retention (`retention_until`) and legal hold; retention sweeper (`DocumentRetentionSweep`) expires and purges only what is allowed; audit for upload / download / link / verify_integrity / integrity_failed / delete / hold / expire. `DisabledDocumentVault` answers 503. |
| Schema | `migrations/20261231000400_platform_document_object.sql`, `…000401_student_document_file.sql` | `platform.document_object` (tenant_id + forced RLS via `platform.apply_tenant_rls()`, checks: key inside tenant, retention ≥ upload, hold has reason, purge after delete; guard trigger: no delete / retention shortening while held or retained); `document_object_id` on `student.admission_document` / `student.student_document` (attach once). |
| Student endpoints | `modules/student/src/files.rs` (+ admission flow, record creation) | `PUT/GET /api/v1/student/admissions/{id}/documents/{documentId}/file`, `POST …/file/link`; `PUT/GET /students/{id}/documents/{documentId}/file`; `GET /me/documents/{documentId}/file`, `POST …/file/link`. Authorisation on the owning record first (`admission.manage` / `admission.decide`, `student.manage`, `student.self` own record only), then the vault re-checks tenant / institution / owner. Upload = raw body with the file's own `Content-Type` + `?filename=`; no Idempotency-Key needed (one file per document; a retry is 409). Verification of an admission document re-checks the bytes; verified documents move to the student's record **with** their file. |
| Config | `apps/api/src/config_docs.rs`, `config.rs`, `main.rs` (`check-config` prints `documents=`) | Keys below. **Production gate**: `SUTRA_DOCUMENT_STORAGE=s3` required, `https://` endpoint (and public endpoint), SSE not `none`, dev / CI secrets (`minioadmin`, `sutra-minio-dev-only`, `sutraci-minio-secret`) and secrets under 16 characters refused, `SUTRA_S3_CREATE_BUCKET` refused, retention ≥ 1 day. |
| Wiring | `apps/api/src/app.rs`, `storage.rs`, `config_http.rs` | Vault in `AppContext`, non-critical storage readiness check, retention sweeper job, per-route body limit (`SUTRA_DOCS_MAX_BYTES` on the upload routes only; every other route keeps `SUTRA_MAX_BODY_BYTES`). |
| Web proxy | `deploy/docker/nginx.conf` | Upload routes `…/documents/{id}/file` get `client_max_body_size 11m` and 120 s body / proxy timeouts; everything else stays at 2 MiB / 30 s. |
| MinIO from source | `deploy/minio/build-from-source.sh`, `start-local.sh`, `healthcheck.sh`, `deploy/docker/minio.Dockerfile`, `minio-prebuilt.Dockerfile` | MinIO images are no longer pullable (Docker Hub removed, Quay 401, dl.min.io 410) and upstream is archived. The server is built with `go install github.com/minio/minio@RELEASE.2025-10-15T17-29-55Z` (latest release tag; verified upstream, commit 9e49d5e7; go.mod `go 1.24.0`, no replace directives), `CGO_ENABLED=0`, upstream ldflags; modules checksum-verified by the Go proxy / sum.golang.org. Runtime image: official `debian:13-slim`, non-root uid 10001, bash `/dev/tcp` health check. **No third-party mirror images.** |
| Compose | `docker-compose.yml`, `docker-compose.demo.yml`, `.env.example`, `deploy/ci/demo-wait.sh` | `minio` service (built from source; demo override uses the host-built binary), API configured for it, demo waits on `/minio/health/live`; `.env.example` documents every key and how a college runs storage (AWS S3 / self-built MinIO / SeaweedFS / Garage / Ceph RGW). |
| CI | `.github/workflows/sutraerp-ci.yml` (rust-db), `sutraerp-part1-demo.yml` | `actions/setup-go` 1.24 + `actions/cache` keyed on the tag; MinIO started as a host process (rust-db) or compose service (demo) with a **random root user / secret per run** (masked); bucket created with a SigV4 `curl --aws-sigv4` PUT and anonymous listing proved 403 before the tests. |
| Journey | `apps/web/e2e/journey.e2e.mjs`, `lib/auth.mjs` | Admission step uploads both admission documents (identity proof small, marksheet 1.5 MiB — sent with `application/pdf`, without Idempotency-Key) before verification; new step downloads them back byte-for-byte (SHA-256, attachment headers), checks `platform.document_object` + links + audit, refuses the student on admission files (403), and the created student retrieves both verified files from `/me/documents`; another user is refused. |

## Configuration keys

| Key | Default | Notes |
|-----|---------|-------|
| `SUTRA_DOCUMENT_STORAGE` | `disabled` | `s3` required in production |
| `SUTRA_S3_ENDPOINT` / `SUTRA_S3_PUBLIC_ENDPOINT` | — / endpoint | `https://` in production |
| `SUTRA_S3_REGION` | `us-east-1` | |
| `SUTRA_S3_BUCKET` | — | private bucket |
| `SUTRA_S3_ACCESS_KEY_ID` / `SUTRA_S3_SECRET_ACCESS_KEY` | — | OpenBao `kv/sutraerp/<env>/storage`; dev / CI secrets refused in production |
| `SUTRA_S3_PATH_STYLE` | `true` | `false` for AWS virtual-hosted |
| `SUTRA_S3_SSE` / `SUTRA_S3_SSE_KMS_KEY_ID` | `none` dev, `AES256` prod | `none` refused in production |
| `SUTRA_S3_CREATE_BUCKET` | `false` | dev / CI only |
| `SUTRA_S3_TIMEOUT_MS` | `15000` | |
| `SUTRA_DOCS_MAX_BYTES` | `10485760` | 1 KiB ..= 64 MiB; nginx upload location allows 11 MiB (raise both together) |
| `SUTRA_DOCS_RETENTION_DAYS` | `2920` | |
| `SUTRA_DOCS_PRESIGN_TTL_SECS` | `0` (off) | 1 ..= 300 |
| `SUTRA_DOCS_SWEEP_SECS` | `3600` | `0` = off |

## Test evidence

All on code commit `837db1c3` (later commits on the branch are documentation only).

| Workflow | Run | Result |
|----------|-----|--------|
| SutraERP CI (workflow_dispatch) | [38885937911](https://github.com/Varad1614/ERP_hark_2/actions/runs/38885937911) | fmt · clippy · test (incl. S3 SigV4 vectors, retry regression tests, config gate), Rust integration: full DB suite + MinIO built from source → `live_s3` 3/3, `db_p2_docs` 7/7; OpenAPI drift; Web — green (Container images: see report) |
| SutraERP Part 1 demo (full stack, keycloak) | [38886172253](https://github.com/Varad1614/ERP_hark_2/actions/runs/38886172253) | compose with MinIO (built from source, random root credentials), `check-config` → `documents=S3`; journey 30/30 incl. upload 184 B + 1 573 054 B (no Idempotency-Key) → download byte-identical → student retrieval; persistence 6/6; DB integrity 0 violations — green |
| SutraERP Part 1 demo (previous head 4b02508f) | [38884030283](https://github.com/Varad1614/ERP_hark_2/actions/runs/38884030283) | green (same journey) |
| SutraERP quality gates | [38885968170](https://github.com/Varad1614/ERP_hark_2/actions/runs/38885968170) | QUALITY_RESULT |

Defects found by the first real runs and fixed at the root:

* `db_p2_docs` (run 38881775496): the extension-mismatch case also declared a mismatching type
  (validator correctly reports `type_mismatch` first) — the case now declares the content's own
  type; the tamper test verified documents before `documents_submitted`, so its 409s came from
  the admission state and the missing-object audit was never exercised — documents are now
  submitted first and each 409 is asserted by reason.
* S3 client (run 38884027027, intermittent): a request failed with a bare `error sending request`
  (transport). Every S3 request is idempotent, so the client now retries transport failures
  (connect / connection closed before the response; never timeouts) up to 3 attempts with
  back-off and reports the full cause chain (no URL). Regression tests: a server that drops the
  first connection (retried, 2 connections) and one that drops all (bounded at 3, cause chain).
* Journey: `psql` renders booleans as `true`, the quarantine check compared with `t` — now a
  `CASE` text.
* Web unit test `home.test.tsx` "loading state" (flaky, run 38883433353): requests issued after
  the release stayed pending forever — they are now answered at once.
* Quality gate gitleaks (full history) flagged the `part2/s4-pay` stub fixture (commit 7fc92f45,
  not on this branch): the identical narrow allowlist of s4-pay bc06a207 (commit + path + literal).


## BLOCKED_EXTERNAL

| Item | What clears it |
|------|----------------|
| Live AWS S3 (virtual-hosted addressing, SSE-S3 / SSE-KMS, IAM-scoped key, public-access block) | An AWS account + bucket in the college's region and an access key limited to that bucket (`s3:ListBucket`, `s3:GetObject`, `s3:PutObject`, `s3:DeleteObject`, plus `kms:GenerateDataKey`/`kms:Decrypt` for `aws:kms`); then run `live_s3` and `db_p2_docs` with `SUTRA_TEST_S3_ENDPOINT=https://s3.<region>.amazonaws.com`, path-style off. |
| Institution storage (the college's own MinIO / SeaweedFS / Garage / Ceph behind TLS) | The college's endpoint URL, CA if private, a bucket and a dedicated access key; same two test suites against it. |
| TLS to the object store | Exercised only by the production config gate (https required); CI uses plain http on 127.0.0.1 / the compose network. Clears with either item above. |

## Notes / limits

* MinIO is AGPL-3.0; SutraERP talks to it over the S3 API only (no linking). Colleges that prefer
  a permissive licence can use SeaweedFS (Apache-2.0); Garage is AGPL-3.0 as well.
* No web upload control yet on the admission drawer (journey records the gap); the API and the
  generated TS client (`packages/api-client`) carry the endpoints.
