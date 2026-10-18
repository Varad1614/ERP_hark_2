# SutraERP — Part 2 final report (real integrations, infrastructure, pilot, commercial)

Integrated branch **`part2/int`** @ **`7d0cd2d3`** (from `part2/s4-int` @ `fac412ce`; Part 1 base `erp_merge` @ `c31cc487`).
Tracker: [PART2_EXECUTION_TRACKER.md](PART2_EXECUTION_TRACKER.md). `erp_merge` was not changed by Part 2.

> **This report makes no production-readiness claim.** The security, infrastructure, integration and institution-acceptance
> gates of Part 2 have **not** all been met: no live provider credential, no college server and no pilot institution or user
> existed. Everything below that was measured was measured on GitHub-hosted runners (4 vCPU / 15.6 GiB, AMD EPYC / Intel
> variants) acting as a staging host — **NOT college hardware**. Items that need an outside party are marked
> **BLOCKED_EXTERNAL** with exactly what clears them (§13).

## Stage gates at a glance

| Stage | Status |
|---|---|
| 4 Real integrations | **COMPLETED** (implementation, verified against real local equivalents + documented-contract stubs) — live providers **BLOCKED_EXTERNAL** |
| 5 Infrastructure | **READY_FOR_REVIEW** (install / upgrade / rollback / backup / PITR / monitoring proven on a staging runner) — physical college infrastructure **BLOCKED_EXTERNAL**; capacity **FAILED on the runner** (run 8: 30 breaches; run 9 on the integrated head: 3 harness breaches) |
| 6 Institutional pilot | **BLOCKED_EXTERNAL** — preparation **COMPLETED** |
| 7 Commercial | Entitlements **COMPLETED**; commercial documents **READY_FOR_REVIEW** (prices are proposals / assumptions) |

## 1. External integrations implemented and verified

| Integration | What was built | Verified against | Live status |
|---|---|---|---|
| **Payment gateway — Razorpay** (S4-pay, S4-ui-feedocs) | REST client (orders, payments, refunds; idempotent order creation, timeouts, 5xx / lost-answer handling), `RazorpayGateway` behind `PaymentGatewayPort`; fees are marked paid **only** on a server-side verified capture (checkout signature checked server-side; completion on the gateway's capture report or a verified webhook); webhook signature + idempotency under replay and concurrency; amount-mismatch evidence (DEF-F-004); recovery job for late captures (DEF-F-006 failed path); settlement reconciliation import; production refuses the mock gateway, test keys, http and a missing webhook secret. UI: Student Fees → Razorpay Checkout with processing / failed / retry states; `GET /finance/me/payment-gateway`; CSP widened only for checkout.razorpay.com / api.razorpay.com | HTTP contract stub reproducing Razorpay's documented request / response / signature formats (doc URLs cited in the tests); `db_p2_pay` on PostgreSQL 18; Playwright finance pay → receipt | **BLOCKED_EXTERNAL** (no Razorpay account or keys) |
| **Document storage — S3** (S4-docs) | SigV4 S3 adapter, document vault (sha256, size, tenant, state), student document upload / replace / download, private-bucket check at start-up, SSE, 11 MiB upload route (nginx + Helm parity check) | **MinIO built from source** in CI (real S3 API), `db_p2_docs`, browser upload / download in the journey | AWS / college S3 **BLOCKED_EXTERNAL** |
| **Identity — Keycloak** (S4-idp) | Keycloak admin adapter, provisioning / sync jobs, identity admin API with SoD on role requests, activation / recovery e-mail triggers | **Live Keycloak 26.7 container** in CI (3/3 live tests) + `db_p2_idp` (4/4) | College SMTP relay and production realm-admin grant **BLOCKED_EXTERNAL** |
| **Data import** (S4-import, S4-ui-import) | Job engine for CSV / XLSX: upload → mapping → dry run → commit → reconciliation → export; 9 targets; opening balances need an independent approver; admin Data import screen | `db_p2_import` (10 behaviours + 2,000-row timing: 15–22 s), `db_p2_ui_import`, Playwright E2E imports | no external dependency — **COMPLETED** |
| **Private CA trust** (S5-deploy, S5-capacity, unified on `part2/int`) | `SUTRA_TLS_CA_FILE` (documented setting: extra roots for S3 / Keycloak / Flowable / OpenSearch clients, checked at start-up); `SSL_CERT_FILE` via reqwest `rustls-tls-native-roots` still honoured; the on-prem installer sets both to `tls/trust.pem` | `crates/infra/tests/private_ca_trust.rs`, `db_p2_capacity`, on-prem drill (MinIO + Keycloak over https with a local CA) | college CA certificate **BLOCKED_EXTERNAL** |
| **Alert delivery** (S5-monitor) | Alertmanager e-mail / chat / SMS receiver templates + dead-man switch | Mailpit + local sink in the monitoring drill | real college mailbox / chat webhook / SMS gateway **BLOCKED_EXTERNAL** |

## 2. Integration tests and remaining provider dependencies

Test evidence (all on `part2/int` @ `7d0cd2d3` unless noted):

- **SutraERP CI 38967547852** — Rust fmt / clippy / unit tests; Rust integration on PostgreSQL 18 + NATS JetStream + Valkey + live Keycloak + MinIO (all `db_p2_*` suites: pay, docs, idp, import, ui_import, monitor, pilot, entitle, capacity); Web typecheck / lint / test / build; **OpenAPI drift** (spec regenerated from the merged code and compared with the committed `openapi/openapi.json` + `packages/api-client/src/schema.d.ts`: no drift (job green; the regenerated artifact of CI 38961607352 was byte-identical to the committed files)); 8 Playwright E2E suites + E2E imports; container images.
- **Part 1 demo (full stack) 38969072534** — compose + Keycloak OIDC journey **and the Stage 6 UAT synthetic dry run (5 roles, 5/5)**.
- **Quality gates 38969075516** — cargo-deny / cargo-audit / pnpm audit / gitleaks, Semgrep, Trivy / Grype / ZAP baseline, k6 performance gate, k6 capacity gate.
- **Deploy (Helm · kind) 38969078345**, **pilot 38965196203 (`414d118a`)**, **observability 38965198799 (`414d118a`)**, **data platform 38965201884 (`414d118a`)**.
- **Part 2 monitoring 38961558989 (`60471aab`)**, **backup and restore drill 38961558950 (`60471aab`)**, **on-prem install / upgrade / rollback 38961559006 (`60471aab`)**, capacity 38961558841 (`60471aab`, run 9: **failure**, 3 breaches — all the harness 412 on `exams.paper.marks.put`).

Remaining provider dependencies (nothing here was validated live): Razorpay test keys (`rzp_test_…` + secret), dashboard
webhook secret and a public HTTPS callback URL, a real settlement (recon) export, then live keys; an AWS account or the college's
S3-compatible endpoint with a scoped key; the college SMTP relay (host, port, sender, credentials); realm-admin access to the
production Keycloak; the college's CA certificate; an SMS gateway and chat webhook for alerts.

## 3. Actual college-infrastructure test results

**None — BLOCKED_EXTERNAL.** No college server, network, DNS or certificate was available. What exists instead is staging
evidence on GitHub-hosted runners, explicitly labelled "NOT college hardware":

- **On-prem install drills** (INFRASTRUCTURE_REPORT.md, DEPLOYMENT_RUNBOOK.md): offline install from the ~0.9 GB bundle with
  registries black-holed; restarts, reboot simulation, SIGKILL recovery, backup → restore round trip, upgrade `c31cc487` → head
  with data kept, rollback (application + data) and roll-forward — worker run 38931814409; on the integrated head 38961559006 (`60471aab`).
- **Capacity** (CAPACITY_TEST_REPORT.md) — see §4 of the capacity verdict below.
- The exact commands that close the physical gap (host facts, hardware / RAID / SMART / UPS inspection, install, DNS / TLS from
  a campus client, real reboot, backup to the real offsite target, capacity run from a separate load machine) are in
  INFRASTRUCTURE_REPORT.md §6 and CAPACITY_TEST_REPORT.md "To complete on the college server".

**Capacity verdict (honest):** the 2,000-student college-day procedure (seed through the API with real approvals, k6 steady day +
results-day spike + fee-deadline spike, fixed thresholds never edited) **did not pass on the runner**. Runs 5–7
(38930815741, 38939308465, 38947132469): steady day within every threshold; results-day sign-in p95 3.3 / 4.3 / **6.2 s** (limit
5 s, Keycloak CPU-bound on a 4-vCPU host shared with the load generator); fee-deadline spike **failed** (API pool stalls →
503s, DEF-P2-CAP-007). DEF-P2-CAP-007 was root-caused (pool hold-and-wait in `/finance/me/charges` and attendance register
commands — not Keycloak) and **fixed in code** (`018d8766`, small-pool regression tests green); re-measured in run 8 (38960531409, `ae097788`, AMD EPYC 7763): fee-spike errors 1.50 % → 0.08 %, read p99 8.8 s → 551 ms, zero pool errors — but run 8 still **FAILED with 30 breaches**: results sign-in p95 5,011 ms vs 5,000 (Keycloak 213 % CPU), 19 from one office desk refused 403 on all 344 requests after a normal TOTP sign-in (**DEF-P2-CAP-009, open, cause unverified**), 8 heavy office-read latency breaches (0.55–1.1 s) and harness 412s. On the integrated head, run 9 (38961558841, `60471aab`, Intel Xeon Platinum 8573C) **FAILED with 3 breaches**, all harness 412s on marks entry; results sign-in p95 3,983 ms, fees read p99 163 ms; CAP-009 did not reproduce (intermittent). DEF-P2-CAP-008
(93 heuristic hold-and-wait candidates on low-rate office write paths) is **open**. Capacity on college hardware is
**BLOCKED_EXTERNAL** and no capacity figure in this report is a statement of what a college server can carry.

## 4. Backup, restore and monitoring evidence

- **Backup / restore** (BACKUP_RESTORE_REPORT.md, worker drill **38932035861**, 2,000 students): RTO **99 s** (target 2 h), RPO
  **19.4 s** with continuous WAL archiving after `kill -9` (target 24 h), point-in-time recovery to a chosen timestamp exact,
  fingerprints identical (406 tables / 327,651 rows) across disaster restore, PITR and logical restore, 5/5 kinds of damaged
  backup refused, single-tenant document restore isolated, journey persistence + DB integrity green on restored data. Integrated
  head: 38961558950 (`60471aab`). On `part2/int` the on-prem compose stack now archives WAL (S5-backup flags + mounts), and
  `sutra-onprem fullbackup nightly|base` runs the encrypted set through `with-metrics.sh` (also invoked by `sutra-onprem backup`
  once `/etc/sutraerp/backup.env` exists) — wired and documented, **not exercised by the on-prem drill** (the set format itself is
  exercised by the backup drill).
- **Monitoring** (MONITORING_AND_SUPPORT.md, deploy/monitoring/): Prometheus + Alertmanager + Grafana stack, 47 alert rules with
  promtool unit tests (static 93/93), dashboards (API RED, PostgreSQL, host / backups, queues / integrations, security), drill of
  4 injected faults with e-mail / SMS / dead-man delivery through Mailpit (worker runs 38920099573, 38916609615); DEF-P2-MON-001
  (readiness probe swallowed a hung S3) fixed. Integrated head: 38961558989 (`60471aab`); observability 38965198799 (`414d118a`).
- Offsite target, real alert receivers and the college host are **BLOCKED_EXTERNAL**.

## 5. Pilot scope and real UAT findings

- Scope: [PILOT_SCOPE.md](PILOT_SCOPE.md) — **PROPOSED**, awaiting an institution (student records, attendance, academics, fees,
  examinations; one term; success criteria §5).
- UAT cases: [UAT_TEST_CASES.md](UAT_TEST_CASES.md) for 5 roles; synthetic dry run `apps/web/e2e/uat-dryrun.e2e.mjs` 5/5 pass
  (worker demo 38945114014; integrated head 38969072534). The dry run is **not** UAT.
- **Real UAT findings: none — no pilot has taken place (BLOCKED_EXTERNAL).** [UAT_RESULTS.md](UAT_RESULTS.md) and
  [USER_FEEDBACK.md](USER_FEEDBACK.md) are empty templates by design. Synthetic-data reset tooling `deploy/pilot/reset-pilot.sh`
  (refuses production and non-synthetic institutions; pilot workflow).

## 6. Defects fixed during the pilot

No pilot ran, so **no pilot defects exist**. Engineering findings from pilot preparation (PILOT_DEFECT_REGISTER.md, not pilot
defects): **DEF-P2-PILOT-001** fixed — malpractice committee decisions (`POST /exams/malpractice-cases/{id}/approval`) had no
maker ≠ checker guard because the path ends in `/approval`; guard added, the approval-matrix fitness test now enumerates
`/approval`, `db_p2_pilot` + UAT-EXM-01 verify it. Open: PDEF-SYN-002 (Low, UI offers Approve to the maker; API refuses 403),
PDEF-SYN-003 (Medium, no dedicated accounts / examination staff role — needs institution input).

## 7. Institutional acceptance status

**Not accepted, not signed — BLOCKED_EXTERNAL.** [INSTITUTION_ACCEPTANCE.md](INSTITUTION_ACCEPTANCE.md) is an unsigned template;
it must never be pre-filled or completed from synthetic or demo results.

## 8. Commercial module catalogue and package definitions

- [MODULE_CATALOGUE.md](MODULE_CATALOGUE.md): 15 modules; classes from evidence — 2 "Production-ready (technically verified;
  institution acceptance pending)" (admissions, attendance — a catalogue class meaning technically verified, **not** a production
  claim), 13 Pilot-ready (assets limited scope), 0 Under development / Deferred at module level; capabilities that must not be sold
  are listed (§3).
- [COMMERCIAL_PACKAGING.md](COMMERCIAL_PACKAGING.md): four cumulative packages from the entitlement registry
  (`catalog/entitlements.json`) — `core_academic` (students, admissions, academics, attendance, examinations) → `academic_plus`
  (+ fees, library, reporting) → `institution_operations` (+ hr, payroll, hostel, transport) → `complete_erp` (+ procurement,
  inventory, assets); add-on modules validated against declared dependencies; functional dependencies the registry does not encode
  are listed as reconciliation items (R1 / R2). Registry packages unchanged in the final entitlement code.

## 9. Module-entitlement implementation results

[ENTITLEMENT_IMPLEMENTATION.md](ENTITLEMENT_IMPLEMENTATION.md): per-tenant entitlement (package + add-ons, licence state, validity)
in `platform.tenant_entitlement` (RLS); API gate on every module route (403 problem with a licence code) plus enforcement inside
cross-module ports (fees-aware Finance ports) and per-entity data-import gate; entitlement changes are requested and decided by
different administrators (step-up, DB CHECK `decided_by <> requested_by`, audited; approval matrix row); web shell hides
unlicensed modules and shows the licence state (UI mirror only). Production refuses to start without `SUTRA_ENTITLEMENT_DEFAULT`;
Helm fails the render without `entitlements.default`; on `part2/int` the on-prem `sutraerp.conf.example` and api.env writer carry
it (`complete_erp`, recorded with a warning for installations that predate entitlements) and the capacity production stack sets it.
Evidence: `db_p2_entitle` 8/8 (worker CI 38950392871) and on the integrated head CI 38967547852; deploy 38950396576, demo 38950399731,
quality 38952022358 on the worker branch. Reviewed security exception: session-only entitlement reads.

## 10. Pricing assumptions and support model

- [PRICING_AND_UNIT_ECONOMICS.md](PRICING_AND_UNIT_ECONOMICS.md): every SutraERP price is a **PROPOSED list price (ASSUMPTION)**;
  competitor prices UNVERIFIED (public pages); costs ASSUMPTION; GST 18 % to be confirmed by a chartered accountant. Annual licence
  per active student: ₹150 / 220 / 270 / 300 by package (minimums ₹1.25–3 lakh); 2,000-student Year-1 list totals ₹7.25 lakh
  (`core_academic` + fees) to ₹12.45 lakh (`complete_erp`) incl. implementation, migration and integration go-live; design-partner
  pilot at 50 % off licence.
- [SUPPORT_AND_MAINTENANCE.md](SUPPORT_AND_MAINTENANCE.md): self-hosted on the college server (managed hosting proposed, not yet
  offerable); RACI; Standard (included, business hours) / Priority / Critical 24×7 tiers; severity response targets; patching and
  upgrade policy; backup responsibility; licence expiry, offboarding and data export (export never gated).
- [COMMERCIAL_READINESS.md](COMMERCIAL_READINESS.md) verdict: **NOT production-ready**; offerable only as a supervised design-partner
  pilot ("Pilot-ready; technically verified components; institution acceptance pending").

## 11. All remaining P0 / P1 / P2 defects

- **P0: none open. P1: none open** (Part 1 register: 5 P0 + 24 P1 raised, all fixed; no Part 2 defect is P0/P1-open).
- **P2 (Part 1 register):** 40 open at Part 1 close (DEFECT_REGISTER.md); Part 2 closed DEF-F-007 (gateway availability in Student Fees) and
  P3 DEF-F-004 (webhook amount-mismatch evidence); DEF-F-006 is fixed for the *failed* payment path only (late capture after a
  cancelled / expired attempt not separately tested — kept open) — so **39 P2 remain open**, all functional gaps listed per
  module in MODULE_CATALOGUE.md §1 (e.g. DEF-C-004/005/007, D-005/007/008, I-011/012/016, F-005/008, E-005/006, G-003/006/007,
  J-006/007/008/010, H-002/010). None is a security or data-integrity defect.
- **Part 2 defects:** fixed — DEF-P2-CAP-001…007, DEF-P2-MON-001, DEF-P2-UIIMP-001, DEF-P2-PILOT-001; **open** — DEF-P2-CAP-008
  (93 heuristic hold-and-wait candidates on office write paths, lower risk), **DEF-P2-CAP-009** (office desk 403 on every request
  after a normal TOTP sign-in in capacity run 8; not reproduced in run 9; cause unverified), PDEF-SYN-002 (Low), PDEF-SYN-003
  (Medium, needs institution input); test flake — E2E student "progression: policy" (seen once in CI 38941353466, green since,
  cause not established); harness — k6 marks entry does not retry on 412; capacity findings READY_FOR_REVIEW — Keycloak brute-force lockout on concurrent sign-ins, Keycloak CPU sizing.

## 12. Changed files, commits and CI evidence

- Range `c31cc487..7d0cd2d3`: 228 commits; 320 files changed, 88,056 insertions, 22,730 deletions (to `7d0cd2d3`). Areas: `apps/api` (config: payments, documents, idp,
  entitlement, TLS; wiring; metrics), `crates/{infra,kernel,web,contracts}` (Razorpay, S3, TLS roots, entitlement gate,
  idempotency, signals), `modules/{finance,platform,student,academic,exams,…}`, `apps/web` (fees checkout, documents, data import,
  licence banner), `deploy/{onprem,backup,monitoring,capacity,pilot,helm,docker,flowable}`, 8 workflows, 38 readiness docs.
- Migrations (append-only, all after 20261230000101, tenant tables with RLS): `20261231000400_platform_document_object`,
  `20261231000401_student_document_file`, `20261231000410_finance_payment_gateway_razorpay`, `20261231004300_platform_identity_link`,
  `20261231044001_platform_data_import`, `20261231070100_platform_tenant_entitlement`.
- Merge commits on `part2/int`: s5-monitor `c4eef2af`, s5-backup `2467405f`, s5-deploy `061e7d06`, s6-pilot `8af960d2`,
  s7-entitle `7adea21a`, s7-commercial `ab4dc2de`, integration fix `74eb99d2`, Helm backup-scripts regeneration `bf8e49f0`,
  s5-capacity (incl. CAP-007 fix) `45c51f0e`, Part 2 drill triggers `60471aab`, Semgrep test fix `414d118a`, session-purge test serialization `7d0cd2d3` (final code head); docs: capacity runs 8–9 `b2e38121`, final tracker, this report.
- Green runs on the integrated head: see §2 and the tracker. Pre-capacity head `74eb99d2`: CI 38957616473, demo 38957619116,
  quality 38957621604, deploy 38957624397, pilot 38957627514, observability 38957822336; data 38958674107 (`bf8e49f0`).
  Stage 4 head `4ae6a52e`: CI 38905792553, demo 38905830060, quality 38905833063, deploy 38905836130, pilot 38905839210.

## 13. Final release-readiness assessment and unresolved external approvals

**Assessment: not production-ready; not releasable to a college for live use.** The software is technically integrated and its
processes are proven on staging runners, but the gates that define production readiness are open:

| Gate | Status | Exactly what clears it |
|---|---|---|
| Live payments | BLOCKED_EXTERNAL | Razorpay account; test keys + webhook secret + public HTTPS URL → test-mode run (order → Checkout → capture → webhook → receipt → refund → recon export); then live keys |
| Live document storage | BLOCKED_EXTERNAL | AWS bucket + scoped key, or the college's S3-compatible endpoint + key (+ its CA) → `db_p2_docs`-equivalent smoke against it |
| Identity e-mail / production IdP | BLOCKED_EXTERNAL | college SMTP relay (host, port, sender, credentials); realm-admin grant on the production Keycloak |
| College infrastructure | BLOCKED_EXTERNAL | the college server + DNS + TLS certificate (or college CA) → INFRASTRUCTURE_REPORT.md §6 commands; a real offsite target → backup drill on it |
| Capacity | FAILED on the runner (run 8: 30 breaches; run 9 on the integrated head: 3 harness breaches); college run BLOCKED_EXTERNAL | fix DEF-P2-CAP-009 and the k6 412 retry, a clean runner run, then the college server with a separate load machine (CAPACITY_TEST_REPORT.md "To complete on the college server"); size Keycloak (≈5 vCPU steady, ≈12 limit per the keycloak.org sizing guide) and review brute-force settings |
| Alert delivery | BLOCKED_EXTERNAL | ops mailbox + SMTP, chat webhook URL, SMS gateway, dead-man URL |
| Institutional pilot / UAT | BLOCKED_EXTERNAL | an authorised institution, DPDP data-processing agreement, named testers, signed PILOT_SCOPE.md, UAT rounds recorded in UAT_RESULTS.md |
| Institution acceptance | BLOCKED_EXTERNAL | signatures of the authorised representatives on INSTITUTION_ACCEPTANCE.md after the success criteria are met |
| Commercial | READY_FOR_REVIEW | company approval of prices, CA confirmation of GST, legal review of licence / support terms |
| Security sign-off | READY_FOR_REVIEW | independent review of Part 2 changes (gitleaks allow-list SEC-P2-ENT-001, CSP for Razorpay, session-only entitlement reads exception), penetration test on the college deployment |
| Merge to `erp_merge` | not done | the owner's review and explicit approval of `part2/int` |

**Unresolved external approvals / inputs** (nothing here can be produced by engineering alone): Razorpay account and keys; S3 /
college object storage and key; college SMTP relay; production Keycloak realm-admin grant; college server, DNS, TLS certificate /
college CA; real offsite backup target; alert receivers (mailbox, chat webhook, SMS gateway, dead-man URL); an authorised pilot
institution with DPDP agreement, testers and signed scope; institution acceptance signatures; company price approval, CA GST
confirmation and legal review; independent security review / penetration test; the owner's approval to merge `part2/int` into
`erp_merge` (not merged, not pushed by Part 2).

**Final code head `7d0cd2d3`**; later commits on `part2/int` change `docs/readiness/**` only, which no workflow builds or tests, so
CI was not re-run on them.
