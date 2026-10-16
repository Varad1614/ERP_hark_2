# Part 2 Stage 5 — `backup` (branch `part2/s5-backup`)

Report: [`../BACKUP_RESTORE_REPORT.md`](../BACKUP_RESTORE_REPORT.md). Base `part2/s4-int` @ fac412ce.

Baseline checked in code before this stage: slice 9C `deploy/backup/sutra-backup.sh` /
`sutra-restore.sh` (logical `pg_dump -Fc` of `sutra` + `flowable`, manifest with sha256, row counts
and audit heads from the dump's snapshot, optional `age` of the tarball) and `deploy/ci/dr-drill.sh`
(compose / kind, in `sutraerp-data.yml` and `sutraerp-deploy.yml`) are present and match
`docs/runbooks/backup-dr.md`. Gaps found (not covered before this stage): no backup of the Keycloak
database / realm, none of the MinIO document objects (added in S4-docs), no WAL archiving / PITR
(contract §11 says "logical backups only"), no config inventory, encryption optional and only of the
whole tarball, no offsite copy, no scheduled backup outside Kubernetes (the chart CronJob only),
retention = `find -mtime` only, RPO never measured.

| Stage | Task | Defect / feature | Changed files | Tests | Actual result + run id | Status | Blockers | Commit |
|---|---|---|---|---|---|---|---|---|
| S5 | Inspect existing backup tooling | — | — | read sutra-backup.sh, sutra-restore.sh, dr-drill.sh, sutraerp-data.yml, runbook | 9C tooling present; gaps listed above | COMPLETED | — | — |
| S5 | Encrypted, checksummed backup sets | FEAT: age per artefact, SHA256SUMS + PLAIN.SHA256, set.json | `deploy/backup/set-lib.sh`, `sutra-fullbackup.sh`, `sutra-backup-verify.sh` | drill `nightly`, `base`, `corrupt` | see report § Evidence | TESTING | — | 37a19aa2 |
| S5 | PostgreSQL base backup + WAL archiving + shipping (PITR) | FEAT | `compose.backup.yml`, `host/postgresql-archive.conf`, `sutra-wal-ship.sh`, `sutra-fullrestore.sh` (`wal`, `pgdata`, `wait-recovery`) | drill `restore` (replay to latest), `pitr` (target time), `rpo` | see report | TESTING | — | 37a19aa2 |
| S5 | Document objects backup + sha256 manifest + selective restore | FEAT | `sutra-objects.sh`, `sutra-fullrestore.sh objects` | drill `nightly`, `restore` step 7, `tenant-restore`, `corrupt` | see report | TESTING | — | 93bfcee8 |
| S5 | Keycloak DB + realm export | FEAT | `sutra-fullbackup.sh`, `sutra-fullrestore.sh logical`, `fingerprint.sh` (kc-*) | drill fingerprints (kc-users / credentials), `verify.mjs` Keycloak sign-ins, journey persistence | see report | TESTING | — | 37a19aa2 |
| S5 | Config / secrets inventory (names only) | FEAT | `inventory.sh` | drill `nightly` (inventory.json.age) | see report | TESTING | — | 37a19aa2 |
| S5 | Consistency fingerprint (rows, trial balance, party balances, audit heads, documents, Keycloak) | FEAT | `fingerprint.sh` | compare before ↔ after-restore / PITR / logical | see report | TESTING | — | 93bfcee8 |
| S5 | Scheduled backups, rotation, offsite | FEAT | `systemd/*`, `systemd/install.sh`, `systemd/crontab.example`, `rotate.sh`, `offsite.sh`, `host/backup.env.example` | drill `units` (install into scratch root, `systemd-analyze verify` + calendar), rotation dry run on the offsite sets, offsite push / pull in every drill restore | see report | TESTING | college server + offsite target | 37a19aa2 |
| S5 | Drill workflow | FEAT | `.github/workflows/sutraerp-part2-backup.yml`, `deploy/backup/drill/{drill.sh,seed.mjs,verify.mjs,report.py}` | the workflow | see report | TESTING | — | 04d14439 |
| S5 | Runbook | DOC | `docs/runbooks/backup-dr.md` § Single-server, `deploy/backup/README.md` | — | — | READY_FOR_REVIEW | — | 92b7c79d |
| S5 | College-server drill | — | — | report § To complete on the college server | — | BLOCKED_EXTERNAL | no college server / offsite target | — |
