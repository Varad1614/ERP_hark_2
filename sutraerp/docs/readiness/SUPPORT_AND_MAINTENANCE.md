# SutraERP — support and maintenance (Part 2 · Stage 7, S7-commercial)

Branch `part2/s7-commercial` (base `part2/s4-int` @ `fac412ce`). Docs only. Proposed commercial policy (ASSUMPTION where marked)
built on the operational evidence of Stage 5: [DEPLOYMENT_RUNBOOK.md / INFRASTRUCTURE_REPORT.md](https://github.com/Varad1614/ERP_hark_2/tree/part2/s5-deploy/sutraerp/docs/readiness)
(`part2/s5-deploy`), [BACKUP_RESTORE_REPORT.md](https://github.com/Varad1614/ERP_hark_2/blob/part2/s5-backup/sutraerp/docs/readiness/BACKUP_RESTORE_REPORT.md)
(`part2/s5-backup`), [MONITORING_AND_SUPPORT.md](https://github.com/Varad1614/ERP_hark_2/blob/part2/s5-monitor/sutraerp/docs/readiness/MONITORING_AND_SUPPORT.md)
(`part2/s5-monitor`). Every timing quoted below was **measured on a GitHub-hosted runner, not college hardware**.
Prices for the tiers are in [PRICING_AND_UNIT_ECONOMICS.md](PRICING_AND_UNIT_ECONOMICS.md) §3.4.

## 1. Deployment models

| | Self-hosted (default) | Managed hosting (optional) |
|---|---|---|
| Where it runs | the college's own server on campus (single-server Docker Compose bundle, offline install) | a VM operated by the vendor in an Indian cloud region |
| Verified? | **Process verified on a staging runner**: offline install 91 s, idempotent restarts, SIGKILL recovery of every stateful service, upgrade with data kept (downtime 35 s), supported rollback, roll-forward (on-prem run 38931814409). **On college hardware: BLOCKED_EXTERNAL** (INFRASTRUCTURE_REPORT.md §6) | **Not drilled.** Same bundle is expected to run on a VM, but no cloud drill exists → not offerable until one passes (COMMERCIAL_READINESS.md) |
| Data location / ownership | college premises; college owns hardware and data | vendor-operated VM in India; college owns the data (contract + DPA) |
| Minimum server | 4 vCPU, 8 GiB RAM, 100 GiB SSD, 8 KiB O_DSYNC write ≤ 2 ms (pilot, ≤ ~2,000 students) | vendor sizes |
| Recommended | 8 vCPU, 16 GiB RAM, 250 GiB SSD/NVMe RAID 1; backup disk/NAS ≥ 2× data + offsite copy; UPS ≥ 15 min | vendor sizes |
| Who runs backups | college IT (vendor tooling + verification, §6) | vendor |

## 2. RACI — infrastructure and operations

R = responsible (does it), A = accountable (owns the outcome), C = consulted, I = informed.

| Activity | Self-hosted: College IT | Self-hosted: Vendor | Managed: College | Managed: Vendor |
|---|---|---|---|---|
| Server hardware, RAID, UPS, power, network, DNS, firewall | **A/R** | C | I | **A/R** |
| OS install, OS security updates (unattended-upgrades), NTP, SSH hardening | **A/R** | C (checklist INFRASTRUCTURE_REPORT.md §4) | I | **A/R** |
| TLS certificate (college CA / public) and renewal | **A/R** | C | C (domain) | **A/R** |
| SutraERP install (offline bundle) | C | **A/R** (implementation) | I | **A/R** |
| SutraERP upgrades and security patches | R (runs `sutra-onprem upgrade` in the window) — or vendor under Priority/Critical tier | **A** (ships signed bundle with SHA256SUMS + SBOM, release notes, upgrade support) | I/C (approves window) | **A/R** |
| Nightly backup timer, WAL archiving, offsite copy, encryption-key (age identity) custody | **A/R** | C (verifies evidence quarterly, §6) | I | **A/R** |
| Restore drill each term on a spare machine | **A/R** | C (supports) | I | **A/R** (evidence to college) |
| Monitoring stack (Prometheus/Alertmanager), alert receivers, on-call rota | **A/R** (first line) | C; R for escalations | I | **A/R** |
| Application incidents / defects | R (first line, triage with runbooks) | **A/R** (diagnosis, hotfix) | R (report) | **A/R** |
| Security incident (contain, preserve evidence, CERT-In 6 h report, DPDP duties) | **A/R** (data fiduciary) | R (technical analysis, fix) | **A** (data fiduciary) | R |
| User and role administration (Keycloak), MFA enrolment | **A/R** | C | **A/R** | C |
| Master data, configuration, approvals (maker ≠ checker assignments) | **A/R** | C | **A/R** | C |
| Payment gateway account (Razorpay keys, webhook secret, settlement) | **A/R** | C (go-live validation) | **A/R** | C |
| SMTP relay, chat / SMS channels for alerts | **A/R** | C | C | **A/R** |
| Licence renewal; applying the purchased entitlement (maker-checker request + decision by two tenant administrators, `platform.entitlement.manage` / `.approve`) | **A/R** (pays; applies the entitlement stated on the vendor's order) | R (issues order + entitlement statement; may audit the entitlement-change log) | **A** (pays) | **R** (applies) |
| Data export at offboarding | **A** | R (assists) | **A** | **R** |

## 3. Support tiers

Vendor support covers the SutraERP application, the bundle, upgrades and the documented runbooks. It does not cover the college's
hardware, OS, network or third-party accounts except under managed hosting. Business hours = Mon–Sat 09:30–18:30 IST excluding
national holidays (ASSUMPTION).

| | Standard (included) | Priority | Critical 24×7 |
|---|---|---|---|
| Channels | ticket portal / e-mail | + phone, shared chat channel | + phone bridge, named engineer |
| Coverage | business hours | 08:00–20:00 IST, 7 days | 24×7 for Sev-1; Priority hours otherwise |
| Upgrades | college runs, vendor supports | vendor performs (remote) in the agreed window | vendor performs |
| Backup / restore verification | annual review of restore evidence | quarterly review + annual assisted restore drill | quarterly assisted restore drill |
| Health check | annual | quarterly | monthly |

## 4. Severity definitions and response targets

Severities align with MONITORING_AND_SUPPORT.md §8 (Sev-1…Sev-3); Sev-4 added for requests. Targets are **vendor** commitments
(PROPOSED — ASSUMPTION, no operating history); the college's own on-call targets in MONITORING_AND_SUPPORT.md (Sev-1 acknowledge
≤ 15 min 24×7) remain the college's / managed-hosting operator's responsibility.

| Sev | Definition | Examples | Standard: response / update | Priority: response / update | Critical 24×7: response / update | Resolution target (workaround or fix) |
|---|---|---|---|---|---|---|
| Sev-1 | ERP unavailable or data at risk for everyone | API / PostgreSQL down, DB disk critical, suspected breach, audit chain broken | 2 business h / every 2 h | 30 min (in hours) / 60 min | 15 min (24×7) / 30 min | workaround ≤ 8 h (Critical: ≤ 4 h) |
| Sev-2 | major function degraded or a safety net gone | payments failing, backups stale (RPO breached), WAL archiving failing, sign-in failing for a group | 4 business h / daily | 2 h / every 4 h | 1 h / every 2 h | ≤ 2 business days |
| Sev-3 | minor, one user, workaround exists | one slow report, idp-sync conflict, warning host alert | next business day / at resolution | 1 business day | 4 h (in hours) | next scheduled release |
| Sev-4 | question, how-to, enhancement | configuration help, new report | 3 business days | 2 business days | 1 business day | roadmap / quotation |

Service credits (PROPOSED): for managed hosting only, monthly availability target 99.5 % excluding announced maintenance; credit
5 % of the monthly hosting fee per 0.5 % below target, capped at 25 %. No availability SLA is offered for self-hosted installations
(the vendor does not control the hardware).

## 5. Security patching, updates and maintenance windows

| Item | Policy (PROPOSED) | Basis in evidence |
|---|---|---|
| Release cadence | quarterly feature release; monthly patch release when there are fixes | — (ASSUMPTION) |
| Security fixes | Critical: patched bundle within 7 days of a fix being available; High: within 30 days; Medium/Low: next patch release. Advisories to customers with CVSS and affected component | CI security gates (cargo-deny/audit, pnpm audit, gitleaks, Semgrep, Trivy/Grype, ZAP) run on every change ([SECURITY_AUDIT.md](SECURITY_AUDIT.md)); two accepted RUSTSEC exceptions expire 2027-04-14 |
| Bundle integrity | every bundle ships SHA256SUMS and SPDX SBOMs; the installer verifies checksums before loading images | INFRASTRUCTURE_REPORT.md §2 (bundle contents) |
| Supported versions | current release (N) and previous (N-1); security fixes only for N and N-1 | ASSUMPTION |
| Upgrade path | in place with `sutra-onprem upgrade`: pre-upgrade backup → forward-only migrations → health; measured downtime 35 s on the runner (data kept, counts identical) | INFRASTRUCTURE_REPORT.md §2.3 |
| Rollback | supported rollback = previous images **+ restore of the pre-upgrade backup**; every change made after the upgrade is lost (runbook §9); app-only rollback is diagnostic, not supported | DEPLOYMENT_RUNBOOK.md §9, drill B |
| Maintenance window | Sunday 06:00–10:00 IST (or a college holiday), announced ≥ 5 working days ahead; emergency security window with 24 h notice; never during admission, fee-deadline or examination-result weeks without the college's written consent | ASSUMPTION; runbook "plan upgrades outside office hours and announce them" |
| OS / Docker / PostgreSQL host patches | self-hosted: college (unattended security updates on); managed: vendor | INFRASTRUCTURE_REPORT.md §4 |
| Certificates | college / operator renews TLS 30 days before expiry; internal CA via `sutra-onprem tls-internal --renew` (825-day certificates) | INFRASTRUCTURE_REPORT.md §4 |

## 6. Backup responsibility

| Item | Policy | Evidence |
|---|---|---|
| What is backed up | ERP, Keycloak and Flowable databases, document objects, `/etc/sutraerp` (incl. secrets — encrypted) | BACKUP_RESTORE_REPORT.md, runbook §7 |
| Schedule / RPO | nightly logical set (02:00) → RPO 24 h; with continuous WAL archiving the measured RPO was **19.4 s** after `kill -9` (runner) | BACKUP_RESTORE_REPORT.md §1 |
| RTO target | 2 h. Measured **99 s** restore on a rebuilt host with 2,000 students (runner). **College-scale ESTIMATE ≈ 418 min (~7 h) with ~57 GiB documents — exceeds the target; a risk to measure on the college server**, mitigations: bring service up before documents finish (~11 min core), defer read-back verification, replicate the document bucket offsite, restore from a local second disk | BACKUP_RESTORE_REPORT.md §5 |
| Offsite copy | daily copy to second disk / NAS + weekly offsite (S3 or rsync target implemented; **not exercised against a real remote store** — BLOCKED_EXTERNAL) | BACKUP_RESTORE_REPORT.md §7–§8 |
| Damaged backups | 5 kinds of damage detected and refused in the drill | BACKUP_RESTORE_REPORT.md §1 |
| Self-hosted responsibility | **the college** keeps the timer enabled, the offsite rotation, the age key in custody (printed copy in a safe + second administrator) and runs a restore test each term; SutraBackupFailed / Stale / Missing and SutraWalArchive* alerts page the college on-call | MONITORING_AND_SUPPORT.md §6 |
| Vendor responsibility (self-hosted) | provide and maintain the tooling, review restore evidence per tier (§3), assist restores as Sev-1 | — |
| Managed | vendor runs all of the above and sends the college the term restore-drill evidence | — |
| Limitation | the vendor is not liable for data loss caused by backups the college disabled, did not copy offsite, or whose key was lost | ASSUMPTION (contract clause) |

## 7. Upgrade policy

1. Customers on a valid licence / AMC receive every release (N) and patch.
2. Each release states: migrations included (forward-only), expected downtime measured on the staging drill, new secrets / config,
   rollback impact.
3. Pre-upgrade backup is taken automatically by `sutra-onprem upgrade`; the college verifies a recent successful nightly set before
   the window.
4. Skipping releases is supported within N-1 → N; older installations upgrade stepwise (drill B covered Part 1 release `c31cc487` →
   Part 2 head).
5. After licence expiry (grace ended), no new releases are provided except security fixes needed to keep the read-only system safe
   for the export period (§8).

## 8. Licence expiry, offboarding and data export

As implemented on `part2/s7-entitle` @ `bf48b7f6` (kernel `decide`; CI in progress when written): **after expiry, licensed modules
become read-only; data is never deleted or locked away by the licence.** Statuses: `active` → `grace` (past `validUntil`, within
`graceDays` 0–365: full access + renewal banner) → `expired` (reads + data export, writes refused `403 entitlement.licence_expired`);
`suspended` (administrative) leaves only platform administration and data export.

| Phase | What the college can do | Vendor obligations |
|---|---|---|
| Renewal reminder | — | notices at 60, 30 and 7 days before `validUntil` (ASSUMPTION) |
| Grace (proposed 30 days) | everything, with a renewal banner | — |
| Expired (read-only) | sign in; view, search and export records; no new transactions, approvals or imports | keep the read-only system supported for security fixes for 90 days (ASSUMPTION) |
| Reinstatement | full function on the next request after the renewal entitlement is approved; no data re-entry | issue the renewal order / entitlement statement |
| Suspension | used only for breach of contract / non-payment after notice (ASSUMPTION); platform administration + data export stay available | written notice ≥ 15 days before suspending (ASSUMPTION) |
| Termination / offboarding | (a) export every entity in the import columns as CSV / XLSX through the data export endpoint (`GET /api/v1/platform/imports/entities/{entity}/export` — always allowed by the gate, formula-injection safe, audited); (b) full PostgreSQL logical dump + document objects from the nightly backup set (open formats: PostgreSQL dump, S3 objects with SHA-256); (c) audit-log export for the retention period | self-hosted: assist the export (up to 3 person-days included, ASSUMPTION); managed: hand over the latest full backup set + decryption key and CSV exports within 15 working days, then **delete** all copies (incl. offsite) within 30 days and certify deletion in writing (DPDP Act 2023 — legal review pending) |
| Records the college must retain | statutory retention of academic, fee and payroll records is the college's obligation; read-only access or the exports satisfy it | — |

Self-hosted note: the entitlement is stored in the college's own database and changed by its own administrators (maker-checker);
the vendor does not hold a kill switch. Licence compliance is contractual (order form, annual student-count true-up, entitlement-change
audit log) — see COMMERCIAL_PACKAGING.md §6.

Export coverage note: the entity export covers the 9 import targets; other records (e.g. results history, ledgers, audit) leave
through the database dump and the modules' own CSV / report downloads. A complete, documented "export everything" package is an
open item (COMMERCIAL_READINESS.md).

## 9. Not covered by support (without a separate order)

Custom development, changes to the college's legacy systems, hardware faults, OS / network administration on self-hosted servers,
third-party service accounts (Razorpay, SMTP, SMS, cloud), restores made necessary by college-side backup failures (assisted on a T&M
basis), training beyond the contracted days, and any Deferred capability listed in [MODULE_CATALOGUE.md](MODULE_CATALOGUE.md) §3.
