# Backup and restore readiness report — Part 2 · Stage 5

Branch `part2/s5-backup` (base `part2/s4-int` @ fac412ce). Tooling: `deploy/backup/` (README there),
runbook `docs/runbooks/backup-dr.md` § *Single-server (docker compose) installation*, workflow
**SutraERP Part 2 backup and restore drill** (`.github/workflows/sutraerp-part2-backup.yml`, steps in
`deploy/backup/drill/drill.sh`). Tracker: `docs/readiness/tracker/p2-s5-backup.md`.

> **Every number below was measured on a GitHub-hosted runner (@CPU@, @VCPU@ vCPU, @RAM@ RAM,
> @DISK@ disk, kernel @KERNEL@, Docker @DOCKER@) — NOT college hardware.** The runner stands in for a
> clean single "staging host"; the college-scale figures in § 5 are an **ESTIMATE**, not a
> measurement. The college-server drill and a real offsite target are **BLOCKED_EXTERNAL** (§ 8).

## 1. Result

| Measure | Measured (runner) | Target | Result |
|---|---|---|---|
| RTO — restore start on a rebuilt host → service verified (offsite sets, base + WAL replay, documents, Keycloak, application) | **@RTO@** | 7 200 s (2 h) | PASS |
| RPO achieved — base backup + continuous WAL archive (`archive_timeout` 60 s, shipped every 5 s), crash with `kill -9` | **@RPO@** | 86 400 s (24 h) | PASS |
| RPO of the nightly logical set alone (fallback path) | ≤ 24 h by schedule (02:00 daily) | 24 h | by design |
| Point-in-time recovery to a chosen timestamp | @PITR@ | exact | PASS |
| Consistency: fingerprint before disaster = after restore = PITR = logical restore | @FPROWS@ | identical | PASS |
| Damaged backups refused (5 kinds of damage) | 5 / 5 detected, named | all refused | PASS |
| Single-tenant document restore, other tenant untouched | @TENANT@ | isolated | PASS |
| Journey persistence phase + DB integrity on restored data | green | green | PASS |

Green full drill (2,000 students, every step incl. corrupt-backup detection, single-tenant restore,
persistence, DB integrity, systemd units, report): **run @RUN@** on `@SHA@`.
SutraERP CI on the final code head: **run @CIRUN@** (green).

Earlier drill runs (each failure root-caused and fixed in code, not worked around):

| Run | Students | Reached | Root cause → fix |
|---|---|---|---|
| 38914093218 | 2,000 | restore step 7 | `--only-show-errors` passed to `aws s3api` (s3-only flag) → removed (34c68062) |
| 38918071609 | 2,000 | restore step 7 | `aws s3 mb --only-show-errors` invalid → removed (4d2cd149) |
| 38921833431 | 2,000 | restore step 9 | post-restore payment picked charges by status; part-paid charges change status → pick any charge with an outstanding amount (0981267b) |
| 38924105868 | 300 | logical restore | nightly set wrapped an already age-encrypted database tar (double layer) → the set holds the plain tar, encrypted once (498270e7) |
| 38926626943 | 2,000 | logical restore | `backup.env` sourced with `set -a` overrode the caller: the logical restore's explicit target URL was replaced by the live database and the file's `SUTRA_ENV=drill` reached `sutra-api migrate` → `load_backup_env` (caller wins) in every script (cd476458) |
| CI 38931362476 | — | Web lint | `deploy/backup/drill/seed.mjs` failed the repo eslint (Node globals, unused assignments) → fixed (dd5340f8) |

## 2. What the drill does (fresh host, real services)

Production-mode compose stack (`docker-compose.yml` + `docker-compose.demo.yml` +
`deploy/backup/compose.backup.yml`): PostgreSQL 18 with WAL archiving, Keycloak OIDC + TOTP,
MinIO (built from source, random root credentials), NATS, Valkey, Mailpit, API, web. Steps:

1. Part 1 journey (Keycloak OIDC + TOTP, human approvals: marks, attendance, fees, documents, results).
2. **Base backup** (`sutra-fullbackup.sh base`: `pg_basebackup` → age → checksums → offsite) and the
   WAL shipper (`sutra-wal-ship.sh --loop 5`).
3. **Seed @STUDENTS@ students through the public API** (admission → documents uploaded to MinIO →
   approval → student; a fee charge and a payment each; @REQUESTS@ requests, @SEEDS@ s).
4. Tenant-B fixture (25 documents of a second tenant), quiesce writers, **fingerprint before**,
   **nightly set** (database dumps + Keycloak dump + realm export + document objects with sha256
   manifest + configuration inventory → age → checksums → offsite), heartbeat writer (1 commit/s).
5. **Disaster**: `kill -9` PostgreSQL, then every container, volume (pgdata, minio, nats), the WAL
   archive, the staging area and the local backup disk are deleted. Only the offsite copy and the
   operator's age identity survive.
6. **Restore** from offsite onto the empty host (timed = RTO), **RPO** from the lost heartbeats,
   **PITR** to a timestamp between two heartbeats, **logical restore** of the nightly set into a
   second, empty PostgreSQL, **partial failures**, **single-tenant document restore**, journey
   **persistence** phase, **DB integrity** (`deploy/ci/db-integrity.sh`), **systemd units** +
   rotation dry run + monitoring textfile metrics, **report** (`drill-metrics.json`, job summary).

Data at backup time (run @RUN@): @DATA@.

## 3. Measured restore (RTO), run @RUN@

@RESTORE_TABLE@

Backup side: base backup @BASE_S@ s; nightly set @NIGHTLY_S@ s (encrypted set @NIGHTLY_MB@).

## 4. Consistency evidence

`fingerprint.sh` writes every application table's exact row count (406 tables), the trial balance per
ledger account and in total, every student account's receivable balance (md5), the audit chain head
per tenant, `platform.audit_log` count / max id, the document rows (count, bytes, md5 of id / key /
sha256 / size / state) and Keycloak users / credentials (incl. TOTP) / clients / roles / groups.
`fingerprint.sh --compare` (plain `diff`) must be empty.

@FP_LINES@

Application level after the restore (`drill/verify.mjs`, through the public web origin): Keycloak
OIDC + TOTP sign-ins of the administrator, registrar, finance approver and student; audit chain
verification (`POST /api/v1/platform/audit/chain/verify`) ok; 40 random documents downloaded through
the API with sha256 = `platform.document_object.sha256`; a new ₹100 counter payment → receipt, ledger
still balanced. Restore step 7 verifies **every** document object's sha256 after upload (downloaded
back). Logical restore (`sutra-restore.sh`): row counts + audit heads equal the manifest, append-only
audit grants present.

Partial failures (each damaged copy refused, naming what is wrong): encrypted file checksum
(`SHA256SUMS`); age authentication when `SHA256SUMS` was rewritten too; database dump sha256 inside a
decrypted set (refused before touching the database); one document object's sha256; one offsite WAL
segment's checksum.

## 5. College-scale ESTIMATE (not a measurement)

Method (`drill/report.py`): the runner's measured throughput of each restore step applied to an
**assumed** college data volume — 5,000 students × 5 years of history, 6 documents × 400 KiB per
student-year (≈ 57 GiB of documents), database scaled linearly per student from this run. None of
this is college data; replace the assumptions with the college's real sizes.

@EST_TABLE@

Reading of the estimate: the database path (base backup + WAL replay) stays in minutes; the document
objects dominate. Restore step 7 uploads every object and downloads it back to verify its sha256 —
on the runner that is ≈ @OBJ_RATE@. The drill's documents average ≈ 25 KiB, so per-object
overhead dominates that rate and scaling it by bytes to 400 KiB documents is pessimistic — but it is
the only measured figure, so it is what the estimate uses. At 57 GiB the **whole-stack** estimate
exceeds the 2 h target; this is a risk to measure on the college server, not a pass.
Mitigations already supported by the tooling, to be measured on the college server:
- bring the service up before the documents finish (restore steps 1–6, 8, 9 first; run
  `sutra-fullrestore.sh objects` afterwards — documents return progressively; ≈ @CORE_EST@ for the
  core service in this estimate);
- skip the read-back verification at restore time and run `sutra-objects.sh verify-live` after;
- keep the document bucket replicated to the offsite store (S3 replication / `mc mirror --watch`)
  so a restore re-points instead of copying;
- restore from a local second disk (no network transfer).

## 6. Restore procedure (operator)

Exact commands: `docs/runbooks/backup-dr.md` § *Restore after losing the server*. Summary:

1. New server with Docker, the release bundle, `/etc/sutraerp/backup.env`
   (`deploy/backup/systemd/install.sh` creates the directories); bring the age identity from the safe.
2. `offsite.sh pull <newest sutra-base-…> in/base`, `offsite.sh pull <newest sutra-nightly-…>
   in/nightly`, `offsite.sh pull-wal in/wal` — checksums verified.
3. `SUTRA_BACKUP_AGE_IDENTITY=… sutra-fullrestore.sh decrypt in/base base` (same for nightly),
   `sutra-fullrestore.sh wal in/wal wal`.
4. `docker compose create postgres`; `sutra-fullrestore.sh pgdata sutraerp_pgdata base wal
   [--target-time '…']`; `docker compose up -d --no-deps postgres`; `sutra-fullrestore.sh
   wait-recovery $(docker compose ps -q postgres)`. Fallback without base / WAL: empty PostgreSQL,
   `SUTRA_RESTORE_SKIP_MIGRATE=true sutra-fullrestore.sh logical nightly` (data as of 02:00).
5. `fingerprint.sh postgres://…/sutra postgres://…/keycloak` before anyone writes; compare with the
   last known fingerprint / trial balance.
6. `docker compose up -d --wait keycloak minio nats valkey mailpit`; `sutra-fullrestore.sh objects nightly`.
7. `docker compose up -d`; sign in through Keycloak; verify the audit chain; open documents; record a receipt.
8. Re-enable the timers, take a fresh base backup, record the incident.

Single tenant's documents: `sutra-objects.sh restore <dir> --tenant <uuid>` then
`sutra-objects.sh verify-live documents.tsv --tenant <uuid>`.

Schedule installed by `systemd/install.sh` (validated in the drill with `systemd-analyze verify` +
`calendar`): nightly 02:00 (+ base backup Sundays, rotation), WAL ship every minute, verification
Saturday 06:00, `OnFailure=` alert unit; textfile metrics (`with-metrics.sh`) feed the S5-monitor
backup alerts (`sutra_backup_last_run_success`, `sutra_backup_last_success_timestamp_seconds`,
`sutra_wal_*`). Retention (`rotate.sh`): 14 daily / 8 weekly / 12 monthly nightly sets, 2 base
backups, WAL behind the oldest kept base pruned.

## 7. Limits of this evidence

- Runner, not college hardware: one 4-vCPU VM, local NVMe, no real network between server and
  offsite (the offsite target was a directory on the same VM — `offsite.sh` S3 / rsync targets are
  implemented but not exercised against a real remote store).
- Data volume is the seeded 2,000 students + journey (≈ 300 MB database, ≈ 100 MB documents), not
  years of college history.
- The age identity was generated per run; key custody (printed copy in the safe, second
  administrator) is procedure only.

## 8. To complete on the college server — BLOCKED_EXTERNAL

**B1. Restore drill on the college server.** Needs: the production server (or an identical spare),
the release bundle, a copy of a real nightly + base set and the WAL archive, the age identity.

```sh
sudo bash deploy/backup/systemd/install.sh && sudo editor /etc/sutraerp/backup.env
sudo systemctl start sutraerp-backup.service && sudo /opt/sutraerp/backup/sutra-fullbackup.sh base
sudo /opt/sutraerp/backup/sutra-backup-verify.sh
sudo /opt/sutraerp/backup/fingerprint.sh "$SUTRA_BACKUP_DATABASE_URL" "$SUTRA_BACKUP_KEYCLOAK_DATABASE_URL" > fp-before.txt
# on the spare server: runbook § Restore after losing the server, steps 1–7, timing each step
sudo /opt/sutraerp/backup/fingerprint.sh postgres://…/sutra postgres://…/keycloak > fp-after.txt
/opt/sutraerp/backup/fingerprint.sh --compare fp-before.txt fp-after.txt
```

Pass: RTO (restore start → users can sign in and open documents) ≤ 2 h with the college's real data
volume; fingerprints identical; audit chain verify ok; Keycloak sign-in with TOTP works; a test
receipt can be recorded; `sutra-backup-verify.sh` exit 0; the timers listed by
`systemctl list-timers 'sutraerp-*'`; a deliberately failed backup raises the alert.

**B2. Real offsite target.** Needs: a second location (S3-compatible bucket in another region /
provider, or an SSH host at another campus) and a write-but-not-delete credential.

```sh
# backup.env: SUTRA_OFFSITE_TARGET=s3://<bucket>/sutraerp (+ SUTRA_OFFSITE_S3_ENDPOINT / _ACCESS_KEY_ID /
#             _SECRET_ACCESS_KEY) or SUTRA_OFFSITE_TARGET=backup@dr-host:/srv/sutraerp
sudo systemctl start sutraerp-backup.service
sudo /opt/sutraerp/backup/offsite.sh pull <set> /tmp/pull-test && sudo /opt/sutraerp/backup/sutra-backup-verify.sh /tmp/pull-test
# with the server's credential, try to delete an object: must be refused (object lock / append-only)
```

Pass: nightly set and WAL land offsite within the backup window; a pull verifies; deletion with the
server's credential is refused; the store's lifecycle rule expires copies after 13 months; WAL
offsite lag (`sutra_wal_last_shipped_timestamp_seconds`) stays < 5 min.

**B3. Key custody.** Print the age identity, seal it in the college safe, give a second administrator
a copy in a password manager; quarterly: decrypt-test one set with the custody copy
(`SUTRA_BACKUP_AGE_IDENTITY=… sutra-backup-verify.sh <set>`).
