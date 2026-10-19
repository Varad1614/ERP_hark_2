# SutraERP — commercial readiness gate (Part 2 · Stage 7, S7-commercial)

Branch `part2/s7-commercial` (base `part2/s4-int` @ `fac412ce`). Docs only. Assessed 2026-10-16 (IST).

## Verdict

**SutraERP is NOT production-ready and no document, quote or sales conversation may claim it is.** It is commercially offerable
only as a **supervised design-partner pilot** — "Pilot-ready; technically verified components; institution acceptance pending" —
on the college's own server, with the gap list from [MODULE_CATALOGUE.md](MODULE_CATALOGUE.md) attached to the order. Reasons:
no real college has run it, no institution has accepted it, the live third-party integrations (Razorpay, AWS / college S3, college
SMTP) and the college hardware have never been exercised, and 13 of 15 modules carry open functional gaps.

Status values: VERIFIED (evidence from a real run, cited) · VERIFIED_ON_RUNNER (process proven on a GitHub-hosted staging runner,
not college hardware) · IN_PROGRESS · BLOCKED_EXTERNAL (needs an outside party / credential / hardware) · NOT_STARTED · OPEN_RISK.

## Gate table

| # | Gate | Status | Evidence / what is missing |
|---|---|---|---|
| G1 | Code builds, all tests and quality gates green on the integrated head | VERIFIED | `part2/s4-int` @ `4ae6a52e`: SutraERP CI 38905792553 (Rust, integration on PG 18 · NATS · Valkey · Keycloak · MinIO, Web, OpenAPI drift, 8 E2E suites + imports), quality 38905833063, demo 38905830060, deploy 38905836130, pilot 38905839210 ([PART2_EXECUTION_TRACKER.md](PART2_EXECUTION_TRACKER.md)) |
| G2 | No open P0 / P1 defects | VERIFIED (with register hygiene caveat) | [DEFECT_REGISTER.md](DEFECT_REGISTER.md): 5 P0 + 24 P1 raised, all fixed; 40 P2 + 22 P3 open. Several rows look stale (dups of fixed DEF-X-002, DEF-F-004) — reconcile before sharing externally. Stage 5 capacity work found DEF-P2-CAP-002/004/005 on `part2/s5-capacity` (fixed there, **not yet integrated**) |
| G3 | Security review, no open Critical / High | VERIFIED (internal) | [SECURITY_AUDIT.md](SECURITY_AUDIT.md): 44 findings, 37 fixed, 13 accepted risks, 0 Critical/High open; scans green (gitleaks, Semgrep, cargo-deny/audit, pnpm audit, Trivy/Grype, ZAP). **Third-party penetration test: NOT_STARTED** (out of scope of Part 1) |
| G4 | End-to-end business journey with human approvals | VERIFIED | [E2E_TEST_REPORT.md](E2E_TEST_REPORT.md): journey 29/29 + persistence 6/6, Keycloak OIDC + TOTP checkers, maker self-decision 403 ×4, DB integrity 0 violations (demo run 38755333716); Stage 4 journey with documents + Checkout UI (demo 38886172253, 38898287708) |
| G5 | Module classification on evidence | VERIFIED (this stage) | [MODULE_CATALOGUE.md](MODULE_CATALOGUE.md): 2 Production-ready (technically verified; institution acceptance pending) — admissions, attendance; 13 Pilot-ready; capability-level Under development / Deferred list |
| G6 | Offline install, restart, crash recovery, upgrade with data kept, rollback | VERIFIED_ON_RUNNER | on-prem run 38931814409: install 91 s, upgrade downtime 35 s (counts identical), supported rollback 33 s, roll-forward; CI 38935075136, deploy 38935077985 (`part2/s5-deploy`, INFRASTRUCTURE_REPORT.md) |
| G7 | Backup / restore / PITR | VERIFIED_ON_RUNNER | drill 38932035861 (2,000 students): restore 99 s, RPO 19.4 s, PITR exact, 5/5 damaged backups refused, single-tenant document restore (`part2/s5-backup`, BACKUP_RESTORE_REPORT.md) |
| G8 | College-scale restore within the 2 h RTO | OPEN_RISK | **ESTIMATE ≈ 418 min (~7 h)** with ~57 GiB documents (5,000 students × 5 years, assumed) — exceeds target; mitigations listed in BACKUP_RESTORE_REPORT.md §5 but none timed |
| G9 | Monitoring and alerting | VERIFIED_ON_RUNNER | 47 alerts, 93/93 promtool tests, fault-injection drill 38916609615 / 38918984735 with e-mail (Mailpit), SMS webhook and dead-man delivery (`part2/s5-monitor`, MONITORING_AND_SUPPORT.md) |
| G10 | Real alert channels (college SMTP, chat, SMS gateway, dead-man service) | BLOCKED_EXTERNAL | MONITORING_AND_SUPPORT.md §9 |
| G11 | Capacity at college load (2,000 students, concurrent staff) | IN_PROGRESS | `part2/s5-capacity` (head `07f9b6a`, "run 6") has no published report yet; server minimum / recommended spec are **sizing estimates** (INFRASTRUCTURE_REPORT.md §3) |
| G12 | College hardware (CPU, disks, RAID, UPS, DNS, real reboot, college CA) | BLOCKED_EXTERNAL | INFRASTRUCTURE_REPORT.md §6 lists the exact commands that close it |
| G13 | Live Razorpay (test then live) incl. webhooks and settlement export | BLOCKED_EXTERNAL | needs `rzp_test_…` + secret, dashboard webhook secret + public HTTPS URL, later `rzp_live_…`; adapter verified only against the documented contract (`db_p2_pay`, CI 38882654896) |
| G14 | Live S3 / institution storage | BLOCKED_EXTERNAL (on-prem alternative VERIFIED_ON_RUNNER) | AWS / college S3 endpoint + scoped key; the bundle's MinIO over TLS + SSE is verified in the on-prem smoke |
| G15 | College SMTP + production Keycloak realm-admin grant | BLOCKED_EXTERNAL | Stage 4.3 |
| G16 | Module entitlements / package enforcement | IN_PROGRESS | `part2/s7-entitle` @ `bf48b7f6`: registry `catalog/entitlements.json`, server-side gate, maker-checker licence changes, expiry read-only; SutraERP CI 38940451926 **in progress**, `ENTITLEMENT_IMPLEMENTATION.md` not yet pushed, not integrated. Packaging is aligned to the registry; reconciliation items R1–R5 open (COMMERCIAL_PACKAGING.md §4.2) — **R1 / R2 (admissions / exams on `core_academic` without `fees`) untested** |
| G17 | Licence enforcement on self-hosted installs | OPEN_RISK (by design) | entitlement changes are made by two tenant administrators; no vendor-signed licence token → compliance is contractual (COMMERCIAL_PACKAGING.md §6) |
| G18 | Pilot / UAT with a real institution and real data | BLOCKED_EXTERNAL | no college, server or pilot users (Stage 6) |
| G19 | Institutional acceptance sign-off | BLOCKED_EXTERNAL | depends on G18 |
| G20 | Packaging, pricing, support policy documented | VERIFIED (documents) / NOT validated in market | COMMERCIAL_PACKAGING.md, PRICING_AND_UNIT_ECONOMICS.md (all prices PROPOSED — ASSUMPTION; competitor prices UNVERIFIED), SUPPORT_AND_MAINTENANCE.md |
| G21 | Unit economics | OPEN_RISK | 2,000-student `complete_erp`: Y1 margin 13.7 % (list) / −10.5 % (pilot terms), 5-year 33.5 % — every input an ASSUMPTION; institutions < ~1,500 students barely cover support |
| G22 | Legal: EULA / subscription terms, DPA under the DPDP Act 2023, CERT-In incident clause, offboarding & deletion terms | NOT_STARTED | drafts needed; SUPPORT_AND_MAINTENANCE.md §8 states the intended terms |
| G23 | Open-source licence compliance of the bundle | NOT_STARTED / OPEN_RISK | MinIO (AGPL-3.0) shipped built from source inside the on-prem bundle; SBOMs exist (SPDX in the bundle) but no licence review was done |
| G24 | Tax and invoicing (GST rate / SAC, e-invoicing) | NOT_STARTED | chartered-accountant review |
| G25 | Managed hosting offer | NOT_STARTED | no cloud drill; proposed price only (PRICING §3.5 / §6.5) |
| G26 | Complete "export everything" offboarding package | OPEN_RISK | entity export covers the 9 import targets; the rest leaves via DB dump / module CSVs — document and test a full export before the first contract ends |

## What can be said to a prospect today (and what cannot)

| May say (evidenced) | Must not say |
|---|---|
| "Every approval in SutraERP needs a second person; the system refuses self-approval" (G4, DEF-X-001/002 fixed) | "Production-ready", "go-live ready", "proven at colleges", "used by N institutions" |
| "Your data stays on your server; offline install, backups with point-in-time restore, tested restore" — *on a staging runner* (G6, G7) | "Restores in under two hours at your scale" (G8 estimate ~7 h) |
| "Admissions and attendance are technically verified end-to-end; the other modules are pilot-ready with a written gap list" (G5) | "Online fee payment is live-tested" (G13), "statutory payroll (PF/ESI/TDS)" (not built), "ABC / DigiLocker / NSP integration" (Deferred) |
| "Security reviewed internally, no open critical or high findings" (G3) | "Independently penetration-tested" or any certification (ISO 27001 etc.) |

## Path from here to a production claim

1. Close G16 (entitlements integrated, CI green, R1 / R2 tested) and G11 (capacity report).
2. Reconcile the defect register (G2) and close the P2 gaps that block the next modules' promotion (decision screens: DEF-C-007,
   DEF-F-008, DEF-I-016, DEF-E-006; exam conduct screens; multi-term CGPA test).
3. Sign a design-partner college; run G12 + G13 + G14 + G15 + G10 on its server with its credentials; time a real restore (G8).
4. Stage 6 pilot with UAT scripts and acceptance criteria agreed up front (G18) → written acceptance (G19).
5. Legal / tax / licence compliance (G22–G24) before the first invoice.
6. Only then: modules that passed the pilot may drop the "(technically verified; institution acceptance pending)" qualifier — and
   only for the scope the institution accepted.
