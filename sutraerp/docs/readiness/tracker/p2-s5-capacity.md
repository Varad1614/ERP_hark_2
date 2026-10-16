# Part 2 · Stage 5 — capacity (tracker)

Branch `part2/s5-capacity`. Report: [`../CAPACITY_TEST_REPORT.md`](../CAPACITY_TEST_REPORT.md). All capacity numbers come from GitHub-hosted runners, **NOT college hardware**.

| Stage | Task | Defect / feature | Changed files | Tests | Actual result + run id | Status | Blockers | Commit |
|---|---|---|---|---|---|---|---|---|
| S5 | Production-mode capacity stack + procedure | feature | `deploy/capacity/*` (compose, edge, realm, secrets/CA, seed, k6, collect, analyze, thresholds), `.github/workflows/sutraerp-capacity.yml` | capacity workflow | stack up and seeded on the runner (runs 5, 6) | COMPLETED | — | 3fc00796, 6bc18391 |
| S5 | Internal CA for clients + identity provisioning of imports | DEF-P2-CAP-001 | `apps/api` config (`SUTRA_TLS_CA_FILE`), student import | `db_p2_capacity.rs` | 2,000 / 2,000 identities synced (run 6: 86 s) ; CI 38938677888 green | COMPLETED | — | bc0b5982 |
| S5 | Nested pool connection under the offering lock (seats, waitlist, offers, overrides) | DEF-P2-CAP-002 | `modules/academic` enrolment / waitlist / eligibility | `db_p2_capacity.rs` (12 seats; 12 waitlist + 6 overrides; pool 3) | run 3 38922758612: 1,816 × 503 → run 6: 2,000 registrations + 6,000 seats, 0 errors; CI 38938677888 green | COMPLETED | — | f5628d2d, c7506ac3 |
| S5 | Flood guard keyed by the proxy address | DEF-P2-CAP-003 | web nginx conf, `docs/security/config.md`, `run.sh` | run.sh flood-check | runs 5/6: client A 207 × 200 + 1,293 × 429, client B 200 | COMPLETED | — | 71c90e44 |
| S5 | Import commit approval had no Flowable process | DEF-P2-CAP-004 | `deploy/flowable/*`, `modules/platform` dataimport / workflow | `db_b9c.rs` | run 6: opening balances import approved ; CI 38938677888 green | COMPLETED | — | 7feb5318 |
| S5 | Response > 1 MiB → 500 + idempotency key released | DEF-P2-CAP-005 | `crates/web/src/idempotency.rs` | unit test | CI 38938677888 green | COMPLETED | — | 7feb5318 |
| S5 | Nested pool connection in document upload | DEF-P2-CAP-006 | `modules/student/src/files.rs` | `db_p2_docs.rs` (18 uploads, pool 3) | run 6: 2,000 documents stored ; CI 38938677888 green | COMPLETED | — | 7feb5318 |
| S5 | API pool starvation stalls under the fee-deadline spike | DEF-P2-CAP-007 | — | — (to add with the fix) | run 6 38939308465: two 15–25 s stalls, 1,926 × 503, fees read p99 8.7 s | BLOCKED_TECHNICAL | culprit path not identified yet; fix plan in report | — |
| S5 | Harness: disjoint accounts, per-alias session minutes, 429 handling, IA kind, seed response shapes | harness | `deploy/capacity/k6/college-day.js`, `seed.mjs` | node --check, ESLint (CI) | run 6 steady + results 0.00 % errors | COMPLETED | — | 22727f11, 2ab16e69 |
| S5 | Measured runs on the GitHub-hosted runner | evidence | `docs/readiness/CAPACITY_TEST_REPORT.md`, `docs/readiness/tools/cap-report.py` | thresholds.json (unchanged) | run 5 38930815741 (Intel 8370C) 4 breaches; run 6 38939308465 (AMD 7763) 56 breaches, all fees except 2; run 7 38947132469 see report | TESTING | DEF-P2-CAP-007 | aa4c6c43 |
| S5 | Production risk: Keycloak brute-force lockout on concurrent sign-ins | finding | — (recommendation only) | — | run 5: 167 × `user_temporarily_disabled` | READY_FOR_REVIEW | needs a decision and a staging-realm check | — |
| S5 | Capacity run on the college server | — | — | procedure in report | — | BLOCKED_EXTERNAL | no college server, DNS, certs or load machine | — |
| S5 | Razorpay online payment under load | — | — | — | gateway disabled in capacity stack | BLOCKED_EXTERNAL | Razorpay test-mode keys + webhook secret | — |
