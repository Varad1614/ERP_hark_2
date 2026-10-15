# Backup and restore (slice 9C)

`sutra-backup.sh` / `sutra-restore.sh` (+ `lib.sh`, `roles.sql`): format `sutra-backup/v1`
(docs/batch9-contracts.md §11): `pg_dump -Fc --no-owner` (ACLs kept) of the `sutra` and `flowable`
databases + `manifest.json` (sha256 per file, migration version, audit chain heads, row counts —
all from the dump's snapshot). NATS / Valkey are rebuilt, OpenSearch is re-projected.
After editing a script run `gen-chart.py` (embeds the scripts into the Helm chart).
Runbook: docs/runbooks/backup-dr.md.
