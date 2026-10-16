# Part 1 · Stage 3 — application security (S3-appsec)

Branch `part1/s3-appsec` (from `part1/s2-int` @ 1fd0e322). Findings, exploit sketches and scan
results: `docs/readiness/security/appsec.md`. CI column = workflow "SutraERP CI" on this branch
unless stated.

| ID | Issue | Root cause | Files changed | Status | Tests | CI result + run id | Blockers | Commit |
|---|---|---|---|---|---|---|---|---|
| SEC-P1-APPSEC-001 | Ledger / audit tables could be TRUNCATEd | Only row-level UPDATE/DELETE triggers; row triggers do not fire on TRUNCATE | `migrations/20261201000470_platform_appsec_ledger_audit_guards.sql` | FIXED | `db_p1_appsec::append_only_evidence_cannot_be_truncated` | Rust integration green (38765529705 job 116528138053 @ 4ae0b0e2; 38767258747 @ 6c6db5c1) | — | c434bf3 / 39423f2 |
| SEC-P1-APPSEC-002 | Posted manual journals and their lines editable in SQL | Draft-only rule enforced only in application code | same migration | FIXED | `db_p1_appsec::posted_manual_journal_is_immutable_in_the_database` | as above | — | c434bf3 / 39423f2 |
| SEC-P1-APPSEC-003 | `%` / `_` in search terms acted as wildcards | `format!("%{}%", q)` without escaping | `crates/persistence/src/lib.rs`; library catalogue/members, guardian repo, finance charges repo, accounts http | FIXED | `db_p1_appsec::search_terms_are_literal_not_sql_or_wildcards` | as above | — | a07868f / 39423f2 |
| SEC-P1-APPSEC-004 | `javascript:` links stored and rendered (stored XSS) | Submission attachment URL unchecked; certificate/resource links prefix-checked only | `crates/kernel/src/weblink.rs`, `kernel/src/lib.rs`, assessment repo, certificate, resources repo, `apps/api/tests/db_b4b.rs` | FIXED | `weblink` unit tests; `db_b4b::internal_assessment_and_coursework` attack cases | as above (+ Rust fmt·clippy·test) | — | a07868f / 9c306ab |
| SEC-P1-APPSEC-005 | 503 problem detail echoed internal URLs/hosts | `KernelError::Unavailable(d)` returned verbatim | `crates/web/src/problem.rs` | FIXED | `problem::tests::unavailable_errors_do_not_leak_internal_endpoints` | Rust (fmt · clippy · test) 38767258747 | — | 9c306ab |
| SEC-P1-APPSEC-006 | CSV formula injection in report download | CSV escaping ignored `= + - @` leading cells | `apps/web/app/lib/csv.ts`, `csv.test.ts`, `features/reports/Overview.tsx` | FIXED | `csv.test.ts` | Web job green (38765529705 job 116528138552; 38767258747) | — | 9c306ab |
| SEC-P1-APPSEC-007 | Semgrep `rust.lang.security.args.args` | False positive (argv = subcommand) | `apps/api/src/main.rs` (annotation), `.semgrep.yml`, `sutraerp-quality.yml` (new SAST job) | ACCEPTED-RISK | Semgrep 0 findings | quality 38765548100 SAST green; 38767374109 | — | ccd18e9 / 4ae0b0e |
| SEC-P1-APPSEC-008 | gitleaks red: fake config fixtures in `part1/s3-config` history | Job scans all fetched branches; fixtures in 2f630ac7 | `sutraerp/.gitleaks.toml` (identical hunk to s3-config 76d90fc) | ACCEPTED-RISK | gitleaks job | quality 38767374109 Security green — no leaks (2,869 commits) | — | 6c6db5c |
| SEC-P1-APPSEC-009 | Username in failed-login logs | Operational need | — | ACCEPTED-RISK | — | — | — | — |
| SEC-P1-APPSEC-010 | Payroll bank-file download institution scope (C-010) | Scope by URL path | — | DEFERRED to S3-tenant | — | — | tenant worker | — |

Behaviour changes encoded in tests: links that were previously accepted with a bare `http(s)://`
prefix but carried whitespace / `//` smuggling are now 422; LIKE searches for `%` / `_` now match
those characters literally.

Demo (full stack): 38765529815 green @ 4ae0b0e2; 38767258857 @ 6c6db5c1.
