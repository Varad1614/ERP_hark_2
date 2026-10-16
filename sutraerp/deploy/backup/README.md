# Backup and restore (slice 9C)

`sutra-backup.sh` / `sutra-restore.sh` (+ `lib.sh`, `roles.sql`): format `sutra-backup/v1`
(docs/batch9-contracts.md §11): `pg_dump -Fc --no-owner` (ACLs kept) of the `sutra` and `flowable`
databases + `manifest.json` (sha256 per file, migration version, audit chain heads, row counts —
all from the dump's snapshot). NATS / Valkey are rebuilt, OpenSearch is re-projected.
After editing a script run `gen-chart.py` (embeds the scripts into the Helm chart).
Runbook: docs/runbooks/backup-dr.md.

## Single-server installation (Part 2 · Stage 5)

| File | Purpose |
|---|---|
| `sutra-fullbackup.sh nightly\|base` | encrypted, checksummed backup set: database (this format) + Keycloak dump + realm export + document objects with sha256 manifest + config inventory; or a `pg_basebackup` |
| `sutra-wal-ship.sh` | WAL archive → encrypted offsite copy (PITR), `ship.log` for RPO evidence |
| `sutra-fullrestore.sh` | verify / decrypt / wal / pgdata (base + WAL, PITR target) / wait-recovery / logical / objects |
| `sutra-objects.sh` | document manifest, mirror, verify (sha256 per object), restore (all or one tenant), verify-live |
| `sutra-backup-verify.sh` | checksums of every stored set + WAL (weekly timer) |
| `fingerprint.sh` | row counts, trial balance, party balances, audit heads, documents, Keycloak — compare before / after |
| `inventory.sh`, `rotate.sh`, `offsite.sh`, `set-lib.sh` | config inventory (names only), GFS retention + WAL pruning, offsite push / pull, age + checksum helpers |
| `compose.backup.yml`, `host/` | WAL archiving for the compose postgres; `backup.env.example`, `postgresql-archive.conf` |
| `systemd/` | timers (nightly 02:00, WAL every minute, weekly verify), alert unit, `install.sh`, cron alternative |
| `drill/` | the CI drill (`drill.sh`, `seed.mjs`, `verify.mjs`, `report.py`) |

Runbook: `docs/runbooks/backup-dr.md` § Single-server. Evidence: `docs/readiness/BACKUP_RESTORE_REPORT.md`.
