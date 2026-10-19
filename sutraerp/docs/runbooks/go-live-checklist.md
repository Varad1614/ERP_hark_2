# Go-live checklist (slice 9E)

Tick every box before go / no-go #3 on day T. Procedure and timeline:
[`pilot-go-live.md`](pilot-go-live.md). Attach the evidence named in each line.

## Release

- [ ] Chart + image version frozen (`vX.Y.Z`); release notes written.
- [ ] Both images verified with `cosign verify` (identity = `sutraerp-ci.yml` on `erp_merge` / `v*`); SBOM attestation present.
- [ ] Images deployed by digest (`image.api.digest`, `image.web.digest`).
- [ ] Every CI workflow green on the release commit (SutraERP CI, observability, deploy (Helm + kind), data platform, quality gates, pilot seed).

## Security sign-off (from the 9D findings)

- [ ] `cargo-deny`, `cargo-audit`, `pnpm audit`, gitleaks: no open high / critical finding, or each one accepted in writing.
- [ ] Trivy / Grype image scans: no unaccepted high / critical CVE in the release images.
- [ ] ZAP baseline: no unaccepted high finding; accepted ones listed with the reason.
- [ ] Threat model reviewed; open mitigations have owners and dates.
- [ ] Production secrets written to OpenBao and rotated from staging values; no secret in a values file or the repository.
- [ ] Online payments off until a real gateway adapter exists (SEC-9D-003, fixed in the integration):
      `SUTRA_PAYMENT_GATEWAY=disabled` in the release values (pilot values set it; it is also the
      production default) — check `POST /api/v1/finance/gateway/webhook` answers **503** and the
      student "Pay online" flow reports payments unavailable; fees are collected at the counter.
      If a gateway is ever enabled, `SUTRA_PAYMENT_WEBHOOK_SECRET` comes from OpenBao `payments`
      (≥ 32 chars; the API refuses the dev secret in production).
- [ ] Security reviewer's sign-off attached.

## Disaster recovery

- [ ] DR drill run on staging (`deploy/ci/dr-drill.sh`): backup → restore → row counts + audit heads match → audit chain verify ok.
- [ ] Measured RTO recorded (target 2 h) and RPO confirmed (nightly, 24 h).
- [ ] Production backup CronJob scheduled; one on-demand backup taken and its manifest checked (sha256 per dump).
- [ ] Restore procedure (`sutra-restore.sh`) rehearsed by the platform on-call.

## Observability

- [ ] Dashboards live: API latency / error rate, database, outbox lag, workflow, audit chain.
- [ ] Alerts routed to the on-call channel and tested (one synthetic alert fired and acknowledged).
- [ ] Audit chain sealer running; `POST /platform/audit/chain/verify` returns ok on production.
- [ ] Log retention and trace sampling set for production.

## Data

- [ ] Pilot seed / load script output kept (every `ok …` line, `changes=` count).
- [ ] Re-run of the load script reports `changes=0` (idempotent).
- [ ] Counts validated: students, staff, programmes, fee structures, library titles, hostel beds.
- [ ] 20 students + 5 staff checked field by field by the data owner; fee totals match the fee notice.
- [ ] Data owner's validation sheet signed.

## Accounts and access

- [ ] Named administrator accounts created (no shared accounts); roles reviewed.
- [ ] MFA enrolled for every administrator (TOTP or passkey); step-up tested on an approval.
- [ ] Faculty and student sign-in tested with one real account each.
- [ ] Break-glass account documented, stored in OpenBao, MFA enforced.
- [ ] Dev authentication disabled (`api.authMode` is not `dev`) on production.

## Support

- [ ] Support contacts published to the institution: go-live lead, platform on-call, application on-call, security contact (name, phone, e-mail, hours).
- [ ] Severity levels and response times agreed (S1 immediate call, S2 within 1 h, S3 next business day).
- [ ] Hyper-care rota T … T+7 staffed; daily stand-up booked.
- [ ] Rollback decision owner named; rollback steps (runbook §6) read by the on-call.

## Go / no-go #3

- [ ] Institution sponsor: go.
- [ ] Go-live lead: go.
- [ ] Time of decision recorded: ______
