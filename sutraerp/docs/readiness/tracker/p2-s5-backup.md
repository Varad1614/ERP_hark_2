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
| S5 | Encrypted, checksummed backup sets | FEAT: age per artefact, SHA256SUMS + PLAIN.SHA256, set.json | `deploy/backup/set-lib.sh`, `sutra-fullbackup.sh`, `sutra-backup-verify.sh` | drill `nightly`, `base`, `corrupt` | nightly set 24.7 s / 138.9 MiB, base 2.3 s; 5/5 damaged inputs refused — run 38932035861 (2,000 students) green | COMPLETED | — | 37a19aa2, 498270e7 |
| S5 | PostgreSQL base backup + WAL archiving + shipping (PITR) | FEAT | `compose.backup.yml`, `host/postgresql-archive.conf`, `sutra-wal-ship.sh`, `sutra-fullrestore.sh` (`wal`, `pgdata`, `wait-recovery`) | drill `restore` (replay to latest), `pitr` (target time), `rpo` | RPO 19.4 s (19 commits lost after kill -9) vs 24 h; PITR exact (heartbeat #75, nothing after target) — run 38932035861 (2,000 students) green | COMPLETED | — | 37a19aa2 |
| S5 | Document objects backup + sha256 manifest + selective restore | FEAT | `sutra-objects.sh`, `sutra-fullrestore.sh objects` | drill `nightly`, `restore` step 7, `tenant-restore`, `corrupt` | 4,027/4,027 sha256 verified after restore; tenant B 25 restored alone, tenant A 4,002 keys + ETags identical — run 38932035861 (2,000 students) green | COMPLETED | — | 93bfcee8, 34c68062, 4d2cd149 |
| S5 | Keycloak DB + realm export | FEAT | `sutra-fullbackup.sh`, `sutra-fullrestore.sh logical`, `fingerprint.sh` (kc-*) | drill fingerprints (kc-users / credentials), `verify.mjs` Keycloak sign-ins, journey persistence | kc fingerprints equal (physical + logical); 4 OIDC + TOTP sign-ins; persistence phase green — run 38932035861 (2,000 students) green | COMPLETED | — | 37a19aa2 |
| S5 | Config / secrets inventory (names only) | FEAT | `inventory.sh` | drill `nightly` (inventory.json.age) | in every nightly set — run 38932035861 (2,000 students) green | COMPLETED | — | 37a19aa2 |
| S5 | Consistency fingerprint (rows, trial balance, party balances, audit heads, documents, Keycloak) | FEAT | `fingerprint.sh` | compare before ↔ after-restore / PITR / logical | 406 tables / 327,651 rows, TB 155,275,000.00 balanced; all 3 compares empty — run 38932035861 (2,000 students) green | COMPLETED | — | 93bfcee8 |
| S5 | Logical restore into an empty server | DEFECT: double age layer in the nightly set (run 38924105868) | `sutra-fullbackup.sh` | drill `logical` | fixed; logical fingerprint equal — run 38932035861 (2,000 students) green | COMPLETED | — | 498270e7 |
| S5 | backup.env overrode the caller's environment | DEFECT: `set -a; . backup.env` replaced an explicit SUTRA_BACKUP_DATABASE_URL (logical restore aimed at the LIVE database) and passed SUTRA_ENV=drill to `sutra-api migrate` (run 38926626943) | `set-lib.sh` (`load_backup_env`), `offsite.sh`, `sutra-backup-verify.sh`, `sutra-fullrestore.sh`, `sutra-wal-ship.sh` | drill `logical` | restore went to the empty server, counts / audit heads / grants vs manifest ok — run 38932035861 (2,000 students) green | COMPLETED | — | cd476458 |
| S5 | Logical restore on a compose host without a sutra-api binary | FEAT: `SUTRA_RESTORE_SKIP_MIGRATE=true` | `sutra-fullrestore.sh`, `host/backup.env.example` | — (option; the drill path migrates with the binary) | not exercised by the drill | READY_FOR_REVIEW | — | 659527ca |
| S5 | Post-restore write picked charges by status | DEFECT (drill): part-paid charges change status (run 38921833431) | `drill/verify.mjs` | drill restore step 9 | payment + receipt, ledger balanced — run 38932035861 (2,000 students) green | COMPLETED | — | 0981267b |
| S5 | seed.mjs eslint | DEFECT (build): Node globals undeclared, unused assignments — Web job of CI 38931362476 | `drill/seed.mjs` | CI Web lint | CI 38932074059 green | COMPLETED | — | dd5340f8 |
| S5 | Scheduled backups, rotation, offsite | FEAT | `systemd/*`, `systemd/install.sh`, `systemd/crontab.example`, `rotate.sh`, `offsite.sh`, `host/backup.env.example`, `with-metrics.sh` | drill `units` (install into scratch root, `systemd-analyze verify` + calendar), rotation dry run, metrics hook, offsite push / pull in every restore | units parsed (scratch-root artefacts only), calendars ok, rotation keep/delete + WAL prune dry run, textfile metrics success/failure/WAL ok — run 38932035861 (2,000 students) green | COMPLETED (runner); real host BLOCKED_EXTERNAL | college server + real offsite target | 37a19aa2, 5af9851a |
| S5 | Drill workflow | FEAT | `.github/workflows/sutraerp-part2-backup.yml`, `deploy/backup/drill/{drill.sh,seed.mjs,verify.mjs,report.py}` | the workflow | every step green incl. corrupt, tenant-restore, persistence, db-integrity (0 violations), units, report — run 38932035861 (2,000 students) green on dd5340f8 | COMPLETED | — | 04d14439 … dd5340f8 |
| S5 | Measured RTO / RPO vs targets | EVIDENCE | `docs/readiness/BACKUP_RESTORE_REPORT.md` | drill | RTO 99 s vs 7,200 s; RPO 19.4 s vs 86,400 s — runner (AMD EPYC 7763, 4 vCPU, 15.6 GiB) NOT college hardware | COMPLETED | — | db30ac0a |
| S5 | College-scale RTO | ESTIMATE | `BACKUP_RESTORE_REPORT.md` § 5 | — | ≈ 418 min whole stack (documents dominate) / ≈ 11 min core service — ESTIMATE, exceeds 2 h with 57 GiB documents: measure on college server | BLOCKED_EXTERNAL | college data volume + hardware | — |
| S5 | Runbook | DOC | `docs/runbooks/backup-dr.md` § Single-server, `deploy/backup/README.md` | — | — | READY_FOR_REVIEW | — | 92b7c79d |
| S5 | SutraERP CI on final code head | CI | — | workflow_dispatch | CI 38932074059 green on dd5340f8 | COMPLETED | — | dd5340f8 |
| S5 | College-server drill | — | — | report § 8 B1 | — | BLOCKED_EXTERNAL | no college server | — |
| S5 | Real offsite target | — | — | report § 8 B2 | — | BLOCKED_EXTERNAL | no second location / credential | — |
