# SutraERP — on-prem deployment runbook (single college server)

Part 2 · Stage 5 (S5-deploy). Audience: the college IT administrator. Everything below is done with
the offline **release bundle** and its tool `sutra-onprem`; the server needs no internet access, no
container registry and no git checkout.

Every command in this runbook was executed for real by the workflow **"SutraERP Part 2 on-prem
install"** (`.github/workflows/sutraerp-part2-onprem.yml`) on a clean GitHub-hosted Ubuntu 24.04
runner acting as a staging host — see [INFRASTRUCTURE_REPORT.md](INFRASTRUCTURE_REPORT.md) for the
run ids and measured timings. It has **not** yet been run on college hardware (BLOCKED_EXTERNAL: no
college server is available).

Related: backup / DR runbook `docs/runbooks/backup-dr.md` (format `sutra-backup/v1`), Kubernetes /
Helm installation `deploy/helm/sutraerp` (also inside the bundle under `helm/`), security
configuration `docs/security/config.md`.

---

## 0. What runs on the server

One Docker Compose project `sutraerp` (bundle `compose/compose.yaml`):

| Service | Image (in the bundle) | Role | Exposed |
|---|---|---|---|
| edge | nginxinc/nginx-unprivileged:1.30-alpine | TLS termination for the ERP and SSO host names; http → https | **80, 443** (only published ports) |
| web | sutraerp-web:&lt;version&gt; | SPA + `/api` reverse proxy (per-route body limits: 11 MiB document upload / data import) | internal |
| api | sutraerp-api:&lt;version&gt; (distroless, non-root) | SutraERP API, `SUTRA_ENV=production` | internal |
| postgres | postgres:18 | `sutra` (ERP), `keycloak`, `flowable` databases; TLS on | internal |
| keycloak | quay.io/keycloak/keycloak:26.7 | sign-in (OIDC), TOTP MFA, users | via edge (SSO host) |
| flowable | flowable/flowable-rest:8.0.0 | approval workflow engine (human approvals, production refuses auto-approval) | internal |
| minio | sutraerp-minio:RELEASE.2025-10-15T17-29-55Z (built from source) | student document storage (S3, TLS, SSE) | 127.0.0.1:9000 (administrator only) |
| valkey | valkey/valkey:8 | cache (password, no persistence) | internal |
| nats | nats:2.11-alpine | event stream (JetStream, file storage) | internal |

Data lives in the Docker volumes `sutraerp_pgdata`, `sutraerp_miniodata`, `sutraerp_natsdata`.
Server configuration lives in `/etc/sutraerp/`, releases in `/opt/sutraerp/releases/<version>/`
with `/opt/sutraerp/current` pointing at the running one, backups in `/var/backups/sutraerp/`.

## 1. Prerequisites

* Server per [INFRASTRUCTURE_REPORT.md §3](INFRASTRUCTURE_REPORT.md) (minimum 4 vCPU / 8 GiB RAM /
  100 GiB SSD; recommended 8 vCPU / 16 GiB / 250 GiB SSD + a separate backup disk).
* Ubuntu Server 24.04 LTS (or 22.04) 64-bit, updated, NTP time sync on (`timedatectl` shows
  `System clock synchronized: yes` — TOTP codes and OIDC tokens depend on it).
* Docker Engine ≥ 24 with the compose v2 plugin, installed from Docker's offline `.deb` packages
  (`docker-ce`, `docker-ce-cli`, `containerd.io`, `docker-compose-plugin`) if the server has no
  internet; `systemctl enable --now docker`.
* `curl`, `openssl`, `python3`, `tar`, `sha256sum` (all in a default Ubuntu Server install).
* Two DNS names for the server, e.g. `erp.college.edu.in` and `sso.college.edu.in` (A records in
  the college DNS → the server's LAN IP). For a pilot without DNS: an `/etc/hosts` line on every
  client and on the server: `<server-ip> erp.college.edu.in sso.college.edu.in`.
* A TLS certificate covering both names (subjectAltName), with its private key and — when issued by
  a college / private CA — that CA's certificate. Without any CA: `--local-ca` (§3.2).
* Firewall: allow inbound 443 (and 80 for the redirect) from the campus network only; nothing else.

Check the server before installing: `sudo ./bin/sutra-onprem preflight` (tools, Docker, ports 80/443
free, CPU / RAM / disk against the minimum, NTP).

## 2. Get and verify the bundle

The bundle `sutraerp-onprem-<version>.tar` (≈ 1 GiB) and `sutraerp-onprem-<version>.tar.sha256`
come from the release (CI artifact `onprem-bundle-current`). Copy both to the server (USB disk /
scp), then:

```bash
sha256sum -c sutraerp-onprem-<version>.tar.sha256
tar -xf sutraerp-onprem-<version>.tar && cd sutraerp-onprem-<version>
sudo ./bin/sutra-onprem verify           # every file against SHA256SUMS
cat RELEASE.json                          # version, git sha, image ids/digests, SBOM list
```

`sbom/*.spdx.json` is the SPDX software bill of materials of every image (for the college's
vulnerability scanner).

## 3. Install

### 3.1 With the college's certificate

```bash
sudo ./bin/sutra-onprem install --domain erp.college.edu.in --sso-domain sso.college.edu.in \
     --cert /root/tls/fullchain.pem --key /root/tls/privkey.pem [--ca /root/tls/college-ca.pem]
```

### 3.2 Without a CA (pilot)

```bash
sudo ./bin/sutra-onprem install --domain erp.college.edu.in --sso-domain sso.college.edu.in --local-ca
```

creates a local CA in `/etc/sutraerp/tls/local-ca/` and a certificate for both names (825 days).
Distribute `/etc/sutraerp/tls/local-ca/ca.crt` to every client as a trusted root (group policy /
MDM / manual import), otherwise browsers show a certificate warning. **Keep `ca.key` secret.**

### 3.3 What `install` does (in order; each step is also a sub-command)

1. `preflight`, `verify` (bundle checksums), copies the bundle to `/opt/sutraerp/releases/<version>`,
   `load` (docker load of every image — no registry is contacted; `pull_policy: never`).
2. `secrets` — generates `/etc/sutraerp/secrets.env` (mode 600): database owner / runtime / Keycloak /
   Flowable passwords, Keycloak bootstrap admin, OIDC client secrets, Valkey and MinIO credentials,
   MinIO SSE key, `SUTRA_CSRF_KEY`, `SUTRA_MFA_ENCRYPTION_KEY`, `SUTRA_AUDIT_ANCHOR_KEY` (32 random
   bytes each, distinct). Nothing secret ships in the bundle; production refuses development values.
3. `tls-internal` — internal CA + certificates for PostgreSQL (`postgres`) and MinIO (`minio`).
4. TLS for the public names (§3.1 / §3.2) and `render`: `/etc/sutraerp/{compose.env, api.env,
   edge/default.conf, keycloak/realm-sutra.json, tls/trust.pem}`. `trust.pem` = the college CA (if
   any) + the internal CA; the API trusts it for outbound HTTPS (`SUTRA_TLS_CA_FILE` — the documented setting, checked at start-up — and `SSL_CERT_FILE` for the native-roots path; MinIO and Keycloak)
   and Keycloak trusts it (`KC_TRUSTSTORE_PATHS`).
5. Starts postgres, keycloak (imports the production-baseline realm with your host names and the
   generated client secrets), valkey, nats, minio, flowable, edge; waits for every health check.
6. `bucket` — creates the private document bucket (anonymous listing must answer 403).
7. `migrate` — database migrations as the schema owner; provisions the API's runtime login role
   (row-level security applies to it).
8. Starts api + web and waits for `health` (§6).

The API runs with `SUTRA_ENV=production`: Keycloak sign-in only, privileged MFA, human approvals on
Flowable, https everywhere, PostgreSQL `sslmode=verify-full`, S3 over https with SSE, rate limits on,
no API docs, no health detail. It refuses to start if any of that is missing (`docker compose logs
api` names the setting).

## 4. Configure

* **Settings**: `/etc/sutraerp/sutraerp.conf` (host names, ports, TLS file paths, SMTP, backup
  directory, tuning). After editing: `sudo sutra-onprem render && sudo sutra-onprem start`.
* **Secrets**: `/etc/sutraerp/secrets.env` (generated; root only). To rotate one (e.g.
  `VALKEY_PASSWORD`): edit, `render`, `start`. Database passwords additionally need `ALTER ROLE` in
  PostgreSQL first (`sutra-onprem` does not rotate them).
* **E-mail**: set `SUTRA_SMTP_HOST`, `SUTRA_SMTP_PORT`, `SUTRA_SMTP_TLS=starttls|tls`,
  `SUTRA_SMTP_USER`, `SUTRA_SMTP_PASSWORD`, `SUTRA_MAIL_FROM` → `render`, `start`. Empty host = e-mail
  off (production refuses `SUTRA_SMTP_TLS=none`).
* **Online fee payments (Razorpay)**: off by default in production (`SUTRA_PAYMENT_GATEWAY=disabled`;
  counter payments work). Live keys are BLOCKED_EXTERNAL (Stage 4 S4-pay); when the college has them,
  put `SUTRA_PAYMENT_GATEWAY=razorpay`, `SUTRA_RAZORPAY_KEY_ID`, `SUTRA_RAZORPAY_KEY_SECRET`,
  `SUTRA_PAYMENT_WEBHOOK_SECRET` in a file referenced by `SUTRA_EXTRA_API_ENV=/etc/sutraerp/extra-api.env`
  (mode 600) in `sutraerp.conf`, then `render`, `start`.
* **Module entitlement** (Part 2 · S7): `SUTRA_ENTITLEMENT_DEFAULT` in `sutraerp.conf` — the licence of
  an institution without a stored entitlement row: a package key from `catalog/entitlements.json` or
  `none` (core only until the purchased entitlement is approved). Required in production (the API
  refuses to start without it); `render` records `complete_erp` with a warning when an installation
  that predates entitlements has none. A commercial installation sets the purchased package or `none`
  (COMMERCIAL_PACKAGING.md, ENTITLEMENT_IMPLEMENTATION.md).
* **Resource tuning**: `SUTRA_PG_SHARED_BUFFERS` (≈ 25 % of the RAM given to PostgreSQL),
  `SUTRA_KC_HEAP` / `SUTRA_FLOWABLE_HEAP` (JVM max heap), `SUTRA_VALKEY_MAXMEMORY`.

## 5. First tenant (institution) and first administrator

A tenant is a UUID carried in every user's token (`tenant_id` claim from the tenant's Keycloak
group). Create it and the first administrator; the administrator then sets up the institution in the
ERP (Institution → profile → configure → activate) and everything else through the UI.

```bash
TENANT=$(sudo sutra-onprem create-tenant --name "College of Engineering, Pune" | tail -1)
sudo sutra-onprem create-user --tenant "$TENANT" --email principal@college.edu.in \
     --role administrator --first Asha --last Kulkarni
# prints a one-time initial password; the user must change it and enrol TOTP at first sign-in
```

Roles (Keycloak realm roles): `administrator`, `registrar`, `finance_approver`, `exam_controller`,
`payroll_approver`, `procurement_approver` (privileged → TOTP enrolment is required at first sign-in),
`faculty`, `student`. Approvals need a **second person** (maker-checker): create at least one
`registrar` and one `finance_approver` besides the administrator. Day-to-day user management happens
in the ERP (identity provisioning) or the Keycloak admin console `https://sso.college.edu.in/admin/`
(sign in to realm `master` with `sutra-bootstrap-admin` / `KC_ADMIN_PASSWORD` from `secrets.env`;
create a named admin there and delete the bootstrap admin afterwards).

## 6. Health, status, logs

```bash
sudo sutra-onprem health [--wait 600]   # every container running/healthy, API ready, SSO discovery, web
sudo sutra-onprem status                # docker compose ps
sudo sutra-onprem logs api [--tail]     # SUTRA_LOG_TAIL=2000 sutra-onprem logs keycloak
sudo sutra-onprem counts                # key business row counts (institutions, students, payments, …)
curl -fsS https://erp.college.edu.in/health/ready   # monitoring probe (status only)
```

## 7. Backup and restore

```bash
sudo sutra-onprem backup [--label nightly]      # → /var/backups/sutraerp/sutra-backup-production-<UTC>/
sudo sutra-onprem restore /var/backups/sutraerp/sutra-backup-production-<UTC>
```

A backup holds: `sutra.dump` + `flowable.dump` + `manifest.json` (format `sutra-backup/v1`: sha256,
migration version, row counts and audit-chain heads from the dump's own snapshot), `keycloak.dump`,
`minio-data.tar.gz` (document objects), `etc-sutraerp.tar.gz` (configuration **including secrets** —
mode 600; store backups encrypted / offline), `onprem.sha256`, `onprem.release`. Restore stops the
application, **recreates the `sutra` and `flowable` databases empty** (a restore replaces the data;
after an upgrade, `pg_restore --clean` alone cannot drop tables that newer migrations made other
tables depend on), restores all three databases and the objects, re-runs migrations, checks row
counts and audit grants against the manifest and starts everything again.

Schedule (targets RPO 24 h / RTO 2 h, `docs/runbooks/backup-dr.md`): a systemd timer, e.g.

```ini
# /etc/systemd/system/sutraerp-backup.service
[Service]
Type=oneshot
ExecStart=/opt/sutraerp/current/bin/sutra-onprem backup --label nightly
# /etc/systemd/system/sutraerp-backup.timer
[Timer]
OnCalendar=*-*-* 01:30:00
Persistent=true
[Install]
WantedBy=timers.target
```

**Encrypted offsite sets, point-in-time recovery, monitoring metrics** (Part 2 · S5-backup / S5-monitor):
PostgreSQL archives every WAL segment to `SUTRA_WAL_ARCHIVE_DIR` (default `/var/lib/sutraerp/wal-archive`,
created by `render`, owner uid 999). Copy `backup/host/backup.env.example` from the bundle to
`/etc/sutraerp/backup.env` (mode 600; database URLs use host `postgres`, e.g.
`postgres://sutra:<POSTGRES_PASSWORD>@postgres:5432/sutra?sslmode=require`; age **public** key; offsite
target; MinIO on `https://127.0.0.1:9000`) and install `age`, `jq` and the AWS CLI. From then on
`sutra-onprem backup` also runs `sutra-onprem fullbackup nightly` — `backup/sutra-fullbackup.sh` through
`backup/with-metrics.sh`, which writes `sutra_backup_*{kind="nightly"}` textfile metrics for
node_exporter (alerts SutraBackupFailed / SutraBackupStale, MONITORING_AND_SUPPORT.md). Weekly base
backup for PITR: `sutra-onprem fullbackup base`; WAL shipping: `backup/systemd/sutraerp-wal-ship.timer`
(`backup/systemd/install.sh`). Restore of a set: docs/runbooks/backup-dr.md. Status: the
`fullbackup` integration of the on-prem tool is wired on part2/int but is **not** exercised by the
on-prem drill (which covers the restore-point backup above); the set format itself is exercised by
the backup drill (BACKUP_RESTORE_REPORT.md).

`systemctl enable --now sutraerp-backup.timer`; copy `/var/backups/sutraerp` to a second disk / NAS
daily and prune old copies (e.g. keep 14 daily, 8 weekly). Test a restore every term on a spare
machine.

## 8. Upgrade

```bash
sha256sum -c sutraerp-onprem-<new>.tar.sha256
sudo sutra-onprem upgrade /path/sutraerp-onprem-<new>.tar
```

Steps: verify the new bundle → copy to `/opt/sutraerp/releases/<new>` → load its images →
**pre-upgrade backup** (recorded in `/etc/sutraerp/state.env`) → stop api + web (downtime starts) →
switch `/opt/sutraerp/current` → add any new secrets, render → start infrastructure → bucket →
migrations (forward-only) → start everything → health (downtime ends). Measured on the staging
runner: see INFRASTRUCTURE_REPORT.md §2. Plan upgrades outside office hours and announce them.

## 9. Rollback

Migrations are **forward-only** (no down migrations). The supported rollback restores the
pre-upgrade backup together with the previous images:

```bash
sudo sutra-onprem rollback
```

stops api + web, switches back to the previous release, restores the `sutra`, `flowable` and
`keycloak` databases and the document objects from the pre-upgrade backup, re-runs the previous
release's migrations (no-op) and starts it. **Every change made after the upgrade is lost**
(payments, documents, users created since) — export or re-enter them; decide quickly.

`sutra-onprem rollback --app-only` only switches the images back on the already-migrated schema. It
is a diagnostic, **not a supported state** (old code ignores new tables/columns, constraints added by
new migrations still apply). The staging drill records whether the previous release even starts on
the migrated schema (INFRASTRUCTURE_REPORT.md §2); after it, always run the full `rollback`.

## 10. Restart, reboot, crash behaviour

* `sudo sutra-onprem restart [service…]` restarts and waits for health; `stop` / `start` for the
  whole stack (`stop` is remembered: stopped services stay down across reboots until `start`).
* Every long-running service has `restart: unless-stopped`: after a crash, a Docker restart or a
  host reboot (with `systemctl enable docker`) the stack comes back by itself. The staging drill
  killed (SIGKILL) postgres, api, valkey, nats, minio and keycloak one at a time and restarted the
  Docker daemon; recovery times are in INFRASTRUCTURE_REPORT.md §2. No committed data was lost
  (business row counts identical, documents byte-identical).
* After a host reboot: `sudo sutra-onprem health --wait 900`.

## 11. Troubleshooting

| Symptom | Check | Fix |
|---|---|---|
| `install` stops at "infrastructure did not become healthy" | `sutra-onprem status`, `logs <svc>` | keycloak first start takes 1–3 min; postgres: TLS key permissions (`/etc/sutraerp/tls/internal/postgres` owned by 999, key 600) |
| api container restarts in a loop | `sutra-onprem logs api` → `configuration error: …` | the message names the setting production refused; fix `sutraerp.conf`, `render`, `start` |
| Sign-in fails "invalid redirect" | Keycloak client `sutra-web` redirect URIs | host name changed after the first start: update the client in the Keycloak console (the realm import runs only once) |
| Sign-in loops / "HTTPS required" | edge headers | always reach Keycloak through `https://<sso host>`; never publish port 8080 |
| Browser certificate warning | CA trust on the client | import the college / local CA; check `openssl s_client -connect <erp host>:443 -servername <erp host>` |
| Document upload 413 | edge / web body limits | 11 MiB document route (`SUTRA_DOCS_MAX_BYTES` ≤ 64 MiB); larger needs edge `client_max_body_size` too |
| Document upload 503 "storage unavailable" | `sutra-onprem bucket`; MinIO TLS | `trust.pem` must contain the internal CA (`render`); MinIO certificate SAN `minio` |
| Approvals stay pending | Flowable health, approver accounts | a second person with the checker role must decide (maker-checker); `logs flowable` |
| TOTP codes rejected | server clock | NTP (`timedatectl set-ntp true`) |
| Disk filling | `docker system df`, `/var/backups/sutraerp` | prune old backups; logs rotate at 5 × 20 MiB per container |
| Unknown state after a failed upgrade | `/etc/sutraerp/state.env` | `sutra-onprem rollback` (restores the pre-upgrade backup) |

Collect a support bundle: `sudo sutra-onprem status; sudo SUTRA_LOG_TAIL=5000 sutra-onprem logs >
sutraerp-logs.txt` (contains no secrets; `secrets.env` and `etc-sutraerp.tar.gz` must never be sent).
