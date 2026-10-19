# Pilot go-live runbook (slice 9E)

How the first institution moves onto SutraERP: who does what, when, the cut-over on a real
Kubernetes cluster with the Helm chart and OpenBao, loading data from the institution's
spreadsheets, and how to roll back. The tick-box version is
[`go-live-checklist.md`](go-live-checklist.md). Contract: `docs/batch9-contracts.md` §12.4.

> Batch 9 is proved in GitHub Actions only (kind + containers on the runner). The steps below are
> the procedure for a real cluster; every command is the one CI runs, so a rehearsal on a staging
> cluster follows exactly the same path.

## 1. Roles

| Role | Who | Responsible for |
|---|---|---|
| Go-live lead | SutraERP delivery lead | Plan, go / no-go calls, communication |
| Platform engineer | SutraERP | Cluster, Helm release, OpenBao, backups, monitoring |
| Application engineer | SutraERP | Data migration scripts, seed, smoke tests, defect triage |
| Institution sponsor | Principal / registrar | Business sign-off, final go / no-go |
| Institution data owner | Registrar's office, accounts, HR | Spreadsheet exports, data validation |
| Institution administrators | 2–3 named staff | Accounts with MFA, first-line support |
| Security reviewer | SutraERP | Sign-off on the 9D findings, secret rotation |

## 2. Timeline (T = go-live day)

| When | Step |
|---|---|
| T-14 | Kick-off. Confirm scope (Domains A–J as delivered), named administrators, support hours. Freeze the chart + image version (`vX.Y.Z`, signed). Request spreadsheet exports (§4). |
| T-12 | Staging cluster: install the chart with `values-pilot.yaml` (§3), run `deploy/ci/smoke.sh` and `deploy/ci/seed-pilot.sh` (`SUTRA_CI_MODE=kind`). |
| T-10 | First data load into staging from the exports; the data owner validates counts and samples (§4.3). |
| T-7 | Security sign-off on the 9D findings; OpenBao secrets rotated for production; MFA enrolment for administrators on staging. |
| T-5 | DR drill on staging (`deploy/ci/dr-drill.sh`): backup → restore → audit chain verify; record the measured RTO. |
| T-3 | Dashboards + alerts reviewed with the on-call engineer (runbook `observability.md`); support contacts published. |
| T-2 | Dress rehearsal: full cut-over on staging from fresh exports, timed. Go / no-go #1. |
| T-1 | Production install (§3), empty database migrated, smoke test. Final exports requested for the evening (data freeze at the institution). Go / no-go #2. |
| T | Cut-over (§5). Go / no-go #3 before users are let in. |
| T+1 | Hyper-care: daily stand-up, defect triage, backup verification. |
| T+3 | First fee collection / attendance day reviewed with the data owner. |
| T+7 | Hyper-care exit review: open defects, backup + restore evidence, alert noise, sign-off. |

## 3. Install on a real cluster (Helm + OpenBao)

1. **Images.** Use the signed release tag. Verify before installing:
   ```bash
   cosign verify ghcr.io/varad1614/sutraerp-api:vX.Y.Z \
     --certificate-identity-regexp '^https://github.com/Varad1614/ERP_hark_2/.github/workflows/sutraerp-ci.yml@refs/(heads/erp_merge|tags/v.*)$' \
     --certificate-oidc-issuer https://token.actions.githubusercontent.com
   ```
   Repeat for `sutraerp-web`; pin both by digest (`image.api.digest`, `image.web.digest`).
2. **Secrets.** In OpenBao, write the production paths of contract §5 (database URL, session keys,
   audit anchor key, Flowable admin, OpenSearch, backup target). The API pod's OpenBao agent renders
   them into the dotenv file named by `SUTRA_ENV_FILE`; nothing secret goes into values files.
3. **Install.**
   ```bash
   helm upgrade --install sutraerp deploy/helm/sutraerp -n sutraerp --create-namespace \
     -f deploy/helm/sutraerp/values-pilot.yaml --set image.api.digest=sha256:… --set image.web.digest=sha256:…
   ```
   The migrations job runs `sutra-api migrate` (idempotent; re-grants the roles) before the API
   rolls out.
4. **Smoke.** `SUTRA_CI_API_URL=https://<host> bash deploy/ci/smoke.sh` and
   `bash deploy/ci/check-telemetry.sh` — every check prints `ok …`.
5. **Backups.** Confirm the nightly backup CronJob and run one on demand; check the manifest
   (`sutra-backup/v1`, sha256 per dump) — RPO 24 h, RTO 2 h (contract §11).

## 4. Data migration from spreadsheets

### 4.1 Exports requested from the institution

| Sheet | Columns (minimum) | Loaded into |
|---|---|---|
| Institution profile | legal name, code, address, contacts, academic pattern | Batch 2 institution API (profile → configure) |
| Campuses + buildings + rooms | campus, block, floor, room, type, capacity | Batch 2 campus API, 8A infrastructure nodes |
| Programmes + curricula | programme code, name, terms, components with credits | Batch 2 programme / curriculum API |
| Academic calendar | year, terms with dates | Batch 2 calendar API |
| Students | number, names, DOB, e-mail, phone, programme, term, section, batch | Batch 3 student records |
| Staff | number, names, category, designation, start date, e-mail | Batch 7 staff + engagements API |
| Fee structure | heads, amounts per programme / year, due dates | Batch 5 fee heads + structures API |
| Library catalogue + copies | title, authors, ISBN, accession, shelf mark | 8C catalogue + copies API |
| Hostel rooms + beds | block, room, beds | 8A nodes + 8B beds API |

### 4.2 Load

- Use `deploy/pilot/seed-pilot.mjs` as the template: every object is looked up first and only
  created when missing, so a load can be re-run after a fix. Replace its fixed lists with the CSV
  rows (one CSV per sheet, UTF-8, ISO dates `YYYY-MM-DD`, amounts as decimal strings).
- Writes go through the public API (audit + outbox + approvals in one transaction). Students are
  the one exception, as in the demo seed: they are inserted into the Batch 3 foundation tables with
  an active context, because admissions are not replayed for an existing student body.
- Load order: institution → campuses / nodes → programmes / curricula → calendar → **activate the
  institution** → staff + engagements → students → fee heads + structures → library → hostel.

### 4.3 Validate

- Counts per sheet vs. the API (`GET …/students`, `…/staff`, `…/fee-structures`, metrics catalogue
  `GET /api/v1/services/institutions/{id}/metrics`).
- 20 random students and 5 staff checked field by field by the data owner.
- Fee totals per programme vs. the institution's fee notice.
- The data owner signs the validation sheet; the load log (script output) is attached to it.

## 5. Cut-over (day T)

1. 08:00 — Data freeze confirmed at the institution; final exports received.
2. 08:30 — Backup of the (empty, migrated) production database; note its manifest.
3. 09:00 — Run the load (§4.2) against production; keep the output.
4. 11:00 — Validation (§4.3) by the data owner; `seed-pilot.sh`-style read checks.
5. 12:00 — Administrators sign in, enrol MFA, open their home page (live KPIs: students enrolled,
   faculty on roll, fee collection, open work orders).
6. 12:30 — Go / no-go #3 (sponsor + go-live lead). Go → announce to users; No-go → §6.
7. Afternoon — Hyper-care desk open; dashboards watched (error rate, latency, audit chain).

## 6. Rollback

Roll back when a go / no-go fails, data validation fails beyond a same-day fix, or a severity-1
defect has no workaround.

- **Before users were let in:** `helm rollback sutraerp <previous revision> -n sutraerp` if the
  release itself is faulty; otherwise restore the step-2 backup (`sutra-restore.sh <dir>`: verify
  sha256 → `pg_restore --clean --if-exists` → `sutra-api migrate` → audit chain verify) and repeat
  the load after the fix. The institution keeps working on its spreadsheets.
- **After users were let in:** freeze writes (scale the API to 0 or switch the ingress to the
  maintenance page), export what was entered since go-live from the audit log, restore the last good
  backup, re-apply the exported changes after review, re-open. Communicate the outage window.
- Every rollback is recorded with the reason, the backup used and the measured recovery time.

## 7. Contacts and escalation

Fill in before T-3 (also in the checklist): go-live lead, platform on-call, application on-call,
institution sponsor, institution administrators, security contact — name, phone, e-mail, hours.
Severity 1 (no sign-in, data loss, wrong fees posted): phone the platform on-call immediately;
severity 2: ticket + call within 1 h; severity 3: ticket, next business day.
