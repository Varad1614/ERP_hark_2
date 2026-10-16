# SutraERP — infrastructure report (on-prem, single college server)

Part 2 · Stage 5 (S5-deploy). Companion to [DEPLOYMENT_RUNBOOK.md](DEPLOYMENT_RUNBOOK.md).

> **Every number in this report was measured on a GitHub-hosted runner acting as a staging host —
> NOT on college hardware.** The user has no college server; physical-infrastructure validation is
> **BLOCKED_EXTERNAL** (§6 lists the exact commands that close it). Runner numbers are evidence
> that the install / upgrade / recovery *process* works and how long each step takes on a known
> machine; they are **not** a statement of college-server capacity. Load capacity (concurrent users)
> is the separate S5-capacity report.

## 1. Staging host (what the numbers were measured on)

Recorded by `deploy/onprem/ci/host-facts.sh` at the start of every drill job (artifacts
`onprem-fresh-results` / `onprem-upgrade-results`, files `host-facts-*.md|json`). GitHub assigns a
different VM per job, so the CPU model varies between jobs (AMD EPYC 7763, 9V74 and 9V45 have all
been observed on this runner class); vCPU / RAM are the same every time.

| Fact | Fresh-install drill (run 38931814409) | Upgrade drill (run 38931814409) |
|---|---|---|
| Label | GitHub-hosted runner (AMD EPYC 9V74 80-Core Processor, 4 vCPU, 15.6 GiB) — NOT college hardware | GitHub-hosted runner (AMD EPYC 9V45 96-Core Processor, 4 vCPU, 15.6 GiB) — NOT college hardware |
| OS / kernel | Ubuntu 24.04.5 LTS / 6.17.0-1022-azure (Hyper-V VM) | same image |
| Disk | 150 GB NVMe-backed Azure disk, ext4; 1 GiB O_DIRECT sequential write 452 MB/s; 8 KiB O_DSYNC write 0.447 ms/write | 87.7 MB/s / 0.256 ms/write |
| Docker | 28.0.4, compose 2.38.2, cgroup v2, overlay2 | same |
| Network | registries black-holed in `/etc/hosts` before install (offline proof: `docker pull busybox` fails, 0 images present) | same |

Reference label requested for the Stage 5 reports: **GitHub-hosted runner (AMD EPYC 7763, 4 vCPU,
15.6 GiB) — NOT college hardware**; the final drills recorded above ran on the EPYC 9V74 / 9V45
variants of the same 4 vCPU / 16 GB runner class, so each table names the CPU it was measured on.

## 2. Measured timings (seconds; staging runner, NOT college hardware)

Source: `timings.tsv` / `results.tsv` / `chaos.tsv` in the run artifacts. The bundle is
~0.9 GB (`sutraerp-onprem-<version>.tar`, 11 images as gzip'd `docker save` + compose + config +
helm + backup scripts + SHA256SUMS + SPDX SBOMs).

### 2.1 Fresh offline install — drill A (`ci/run-fresh.sh`, run 38931814409, release 2026.10.1-a5400f2f)

| Step | Seconds |
|---|---|
| verify bundle checksums (SHA256SUMS) | 0.7 |
| `docker load` of the 11 images | 28.0 |
| first boot of postgres, keycloak (realm import), valkey, nats, minio, flowable, edge → healthy | 40.9 |
| database migrations | 4.7 |
| api + web → every health check green | 13.6 |
| **install total** (`sutra-onprem install`, offline) | 91.1 |

Then: TLS (local CA, TLSv1.3, certificate verifies), http → 301 https, HSTS
`max-age=31536000; includeSubDomains`; only 80/443 published (+ MinIO on 127.0.0.1:9000).

Smoke journey through the public names (Keycloak OIDC + TOTP sign-in of the administrator and two
checkers; institution → student by approved admission → fee structure approved by the finance
approver (maker self-approval refused) → charge → ₹30,000 counter payment + receipt; 3 MiB document
upload over the 11 MiB route, stored in MinIO over TLS with SSE, downloaded byte-identical):

| Phase | Seconds |
|---|---|
| smoke_seed | 65.9 |
| smoke_document | 31.4 |
| smoke_verify | 31.3 |
| smoke_verify_after_reboot_sim | 1.6 |
| smoke_verify_after_chaos | 1.5 |
| smoke_verify_after_restore | 1.5 |

### 2.2 Restarts, reboot simulation, crash recovery (drill A)

`sutra-onprem restart <svc>` = restart + wait until every health check is green again (all
containers healthy, API `/health/ready` ok, SSO discovery, web shell):

| Service | Restart → healthy (s) |
|---|---|
| postgres | 5.8 |
| keycloak | 13.3 |
| valkey | 8.0 |
| nats | 7.8 |
| minio | 5.5 |
| flowable | 11.1 |
| api | 0.8 |
| web | 12.5 |
| edge | 12.7 |
| all containers at once (`docker restart`) | 19.0 |
| Docker daemon restart (host-reboot simulation) | 43.5 |

SIGKILL of the service's main process (no warning); recovered = container restarted by its
`restart: unless-stopped` policy and every health check green again; business row counts identical
before/after each kill (audit rows may only grow):

| Service killed | Recovered in (s) | Restart count | Extra check |
|---|---|---|---|
| postgres | 5.5 | 0→1 | API ready |
| api | 0.9 | 0→1 | API ready |
| valkey | 7.8 | 0→1 | API ready |
| nats | 7.7 | 0→1 | API ready |
| minio | 5.5 | 0→1 | bucket private + reachable over TLS |
| keycloak | 28.1 | 0→1 | OIDC discovery |

Data checks: business counts (`institutions students admissions charges payments payment_total
receipts journal_entries documents document_objects`) **identical** after every restart, after
`docker restart` of all containers, after the Docker daemon restart and after every SIGKILL; the
smoke `verify` phase (sign-in, student, payment, outstanding ₹60,000, document SHA-256) passed after
the reboot simulation and after the crash series.

Backup at the end: `sutra-onprem backup` 1.9 s, 5.7M; `sutra-onprem restore` of it onto the running server 58.2 s (wall 58.2 s), business counts identical and smoke verify green afterwards (pilot-size data set).

### 2.3 Upgrade with data kept, rollback, roll forward — drill B (`ci/run-upgrade.sh`, run 38931814409)

Previous release = Part 1 `erp_merge` **c31cc487** (`2026.10.0-c31cc487`), current = this branch
(`2026.10.1-a5400f2f`). The previous release is installed offline and seeded with the smoke journey,
then upgraded in place.

| Step | Seconds | Result |
|---|---|---|
| install previous release c31cc487 (offline) | 82.4 |  |
| seed (smoke journey) on the previous release | 84.0 |  |
| pre-upgrade backup (inside upgrade) | 1.2 |  |
| migrations of the new release | 1.3 | forward-only |
| **upgrade downtime** (api+web stopped → healthy on the new release) | 34.9 | identical (institutions=1→1 students=1→1 admissions=1→1 charges=1→1 payments=1→1 payment_total=30000.00→30000.00 receipts=1→1 journal_entries=2→2 ) |
| upgrade total (`sutra-onprem upgrade`, incl. image load + backup) | 51.5 |  |
| new capability on migrated data (3 MiB document on the old student) | 31.3 |  |
| rollback A — app only (diagnostic) | 21.0 | previous images started on the migrated schema and served the smoke reads (unsupported: forward-only migrations, new tables ignored) |
| **rollback B — supported** (previous images + restore of the pre-upgrade backup) | 33.4 | identical (institutions=1→1 students=1→1 admissions=1→1 charges=1→1 payments=1→1 payment_total=30000.00→30000.00 receipts=1→1 journal_entries=2→2 ); writes after the upgrade are discarded by design (document rows before rollback: 2, after: 1) |
| roll forward (upgrade again) | 46.7 | identical (institutions=1→1 students=1→1 admissions=1→1 charges=1→1 payments=1→1 payment_total=30000.00→30000.00 receipts=1→1 journal_entries=2→2 ) |

Upgrade downtime includes a restart of Keycloak + Flowable when `compose up` recreates postgres
(fresh connection pools; DEF-P2-S5-DEPLOY-005). Roll-forward after the rollback: upgrade total
46.6 s (downtime 34.4 s), document upload + smoke verify green again.

### 2.4 Footprint (docker stats, staging runner)

| Container | Idle after install | After smoke | Final |
|---|---|---|---|
| api | 73.93MiB | 87.39MiB | 76.04MiB |
| edge | 6.348MiB | 6.637MiB | 6.637MiB |
| flowable | 476.9MiB | 479.1MiB | 415.3MiB |
| keycloak | 611.2MiB | 677.3MiB | 581.6MiB |
| minio | 77.71MiB | 85.81MiB | 77.38MiB |
| nats | 7.461MiB | 8.586MiB | 7.434MiB |
| postgres | 149.9MiB | 196.7MiB | 163.2MiB |
| valkey | 8.664MiB | 8.918MiB | 8.746MiB |
| web | 5.125MiB | 5.277MiB | 5.164MiB |

`/var/lib/docker` after the fresh drill: 3.8G (images of one release + volumes).

## 3. Derived college server specification

Derived from the footprint above, the fixed JVM heaps / PostgreSQL buffers in
`config/sutraerp.conf.example` and headroom for an office-hours peak. These are **sizing estimates**
until the college server runs the checks in §6 and the S5-capacity load test is repeated on it.

| | Minimum (pilot: one college, ≤ ~2,000 students, ≤ 50 concurrent staff) | Recommended (production, one college) |
|---|---|---|
| CPU | 4 vCPU / cores x86-64 (AVX2), ≥ 2.0 GHz | 8 vCPU / cores |
| RAM | 8 GiB (stack idles at ~1.3 GiB; Keycloak + Flowable JVMs capped at 768 MiB heap each, PostgreSQL shared_buffers 512 MB, page cache for PostgreSQL) | 16 GiB (raise `SUTRA_PG_SHARED_BUFFERS=2GB`, `SUTRA_KC_HEAP=1g`) |
| System disk | 100 GiB SSD (Docker root: ~4 GB per installed release; keep two releases + volumes + growth) | 250 GiB SSD / NVMe, RAID 1 |
| Disk latency | 8 KiB O_DSYNC write ≤ 2 ms (PostgreSQL commit path; the runner measured ~0.3 ms) | ≤ 1 ms |
| Backup disk | separate disk or NAS share ≥ 2× the system data, plus an off-site copy | same + encrypted off-site rotation |
| Network | 1 Gbit/s LAN; static IP; inbound 443 (+80 redirect) from campus only | same + UPS-backed switch |
| Power | UPS with ≥ 15 min runtime and clean shutdown | same |

`sutra-onprem preflight` warns below 4 vCPU, 8 GiB RAM or 60 GiB free disk for Docker.

## 4. Operations checklist (before go-live)

**OS**
- [ ] Ubuntu Server 24.04 LTS (or 22.04) 64-bit, fully updated; unattended security updates on.
- [ ] NTP synchronised (`timedatectl` → `System clock synchronized: yes`) — TOTP and OIDC need it.
- [ ] Docker Engine ≥ 24 + compose v2 plugin from Docker's `.deb` packages; `systemctl enable docker`.
- [ ] Host firewall (ufw/nftables): allow 22 from the admin VLAN only, 80/443 from campus; deny the rest.
- [ ] SSH key-only login, no root login; administrators in the `sudo` group only.
- [ ] Disk monitoring: alert below 20 % free on `/var/lib/docker` and the backup disk.

**TLS**
- [ ] Certificate with SAN for both names (`erp.<college>`, `sso.<college>`), full chain, key 0600.
- [ ] Private / college CA: pass `--ca college-ca.pem` so the API and Keycloak trust it
      (outbound HTTPS via `SSL_CERT_FILE`, test `crates/infra/tests/private_ca_trust.rs`).
- [ ] No CA at all: `--local-ca` and distribute `/etc/sutraerp/tls/local-ca/ca.crt` to every client.
- [ ] Calendar reminder 30 days before expiry; renew = replace files + `sutra-onprem render && sutra-onprem restart edge`.
- [ ] Internal CA (PostgreSQL / MinIO) renewal: `sutra-onprem tls-internal --renew` (825-day certificates).

**DNS**
- [ ] A records `erp.<college>` and `sso.<college>` → the server's static LAN IP (or `/etc/hosts` on
      every client for a pilot).
- [ ] `getent hosts erp.<college> sso.<college>` on the server and on a client resolve to that IP.

**Backup**
- [ ] `sutraerp-backup.timer` enabled (runbook §7): nightly → RPO 24 h.
- [ ] Copy of `/var/backups/sutraerp` to a second disk/NAS daily + an off-site copy weekly; backups
      contain `etc-sutraerp.tar.gz` with secrets → store encrypted.
- [ ] Restore test each term on a spare machine (`sutra-onprem restore <dir>`); target RTO 2 h.
- [ ] Keep the previous release bundle in `/opt/sutraerp/releases/` for rollback.

## 5. What the drills proved / did not prove

Proved on the staging runner: offline install from the bundle only; production mode with real
Keycloak OIDC + TOTP, Flowable human approvals, MinIO over TLS with SSE, PostgreSQL over TLS;
idempotent restarts; recovery from SIGKILL of every stateful service and from a Docker daemon
restart without data loss; upgrade from the Part 1 release with data kept; the supported rollback
(previous images + pre-upgrade backup) and roll-forward.

Not proved (BLOCKED_EXTERNAL): behaviour on the college's CPU, disks, RAID controller, network,
power and DNS; a real power-cut reboot (the drill restarts the Docker daemon, not the kernel);
the college's own certificate / CA; restore onto rebuilt hardware.

## 6. To complete on the college server (BLOCKED_EXTERNAL — physical inspection)

Status: **BLOCKED_EXTERNAL** — no college server is available. Run, as root, on the server:

```bash
# 1. Host facts + micro-benchmarks (same script as the runner; copy it from the source release
#    deploy/onprem/ci/host-facts.sh). PASS: ≥ 4 vCPU, ≥ 8 GiB RAM, O_DSYNC 8 KiB write ≤ 2 ms.
bash host-facts.sh /root/sutra-facts college-server && less /root/sutra-facts/host-facts-college-server.md

# 2. Hardware inspection. PASS: no failed disks, RAID optimal, ECC on, SMART "PASSED".
lscpu; dmidecode -t memory | grep -E 'Size|Type:|Error Correction'; lsblk -o NAME,SIZE,ROTA,MODEL
smartctl -H /dev/sda          # each disk (apt install smartmontools)
cat /proc/mdstat              # software RAID, or the vendor tool (ssacli / perccli / storcli)
upsc <ups>@localhost 2>/dev/null || echo "check the UPS manually: runtime ≥ 15 min, shutdown signal wired"

# 3. Preflight + install from the bundle (runbook §2–§3). PASS: "ok preflight", "installed".
sha256sum -c sutraerp-onprem-<version>.tar.sha256 && tar -xf sutraerp-onprem-<version>.tar
cd sutraerp-onprem-<version> && ./bin/sutra-onprem preflight
SUTRA_TIMINGS_FILE=/root/sutra-timings.tsv ./bin/sutra-onprem install --domain erp.<college> \
  --sso-domain sso.<college> --cert fullchain.pem --key privkey.pem [--ca college-ca.pem]

# 4. DNS / TLS from a campus client. PASS: both names → server IP; "Verify return code: 0"; 301.
getent hosts erp.<college> sso.<college>
echo | openssl s_client -connect erp.<college>:443 -servername erp.<college> 2>/dev/null | grep 'Verify return code'
curl -sI http://erp.<college>/ | head -1

# 5. Health, restart and a REAL reboot. PASS: health ok within 900 s after the reboot.
sutra-onprem health && sutra-onprem restart && reboot
# after the server is back:
sutra-onprem health --wait 900

# 6. Backup → restore round trip. PASS: "ok restored", counts identical to before.
sutra-onprem counts > /root/counts-before.txt
dir=$(sutra-onprem backup --label acceptance | tail -1) && sutra-onprem restore "$dir"
sutra-onprem counts | diff - /root/counts-before.txt && echo "restore round trip OK"

# 7. Record /root/sutra-timings.tsv and host-facts-college-server.md in this report (new column
#    "college server" next to the runner numbers) and re-run the S5-capacity load test there.
```

Pass criteria for go-live: steps 1–6 all PASS; install ≤ 15 min; health after reboot ≤ 15 min;
restore of the production-size backup ≤ 2 h (RTO).

## 7. Evidence

| Run | What | Result |
|---|---|---|
| 38931814409 (a5400f2f) | **on-prem: bundles (current + previous c31cc487), drill A fresh install + restarts + reboot sim + SIGKILL recovery + backup/restore, drill B upgrade / rollback / roll forward** | **all jobs green** |
| 38928531972 (e015e419) | on-prem drills | fresh green (incl. backup → restore round trip 58.6 s); upgrade red: first sign-in after upgrade "Database operation failed" (Keycloak pool after postgres recreate) → fixed a5400f2f |
| 38925488115 (f4454d5e) | on-prem drills | fresh green; upgrade red: rollback restore — sutra-restore.sh exit 1 after success (EXIT-trap status) → fixed e015e419 |
| 38922326019 (58e61292) | on-prem drills | fresh green (first full pass); upgrade red: rollback `pg_restore --clean` blocked by later-migration tables → fixed f4454d5e |
| 38919074621 (a8256755) | on-prem drills | both red at create-tenant: Keycloak admin token HTTP 405 (token POST sent as GET) → fixed 58e61292 |
| 38923598486 (db068807) | SutraERP CI | green (ESLint fix) |
| 38922359236 (58e61292) | SutraERP deploy (Helm · kind), incl. `web-nginx-parity-with-image` | green |
