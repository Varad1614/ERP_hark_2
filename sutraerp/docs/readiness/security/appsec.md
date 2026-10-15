# Part 1 · Stage 3 — application security review (S3-appsec)

Branch `part1/s3-appsec` (from `part1/s2-int` @ 1fd0e322). Code head **4ae0b0e2**; branch head
**6c6db5c1** (adds the gitleaks history allow-list shared with `part1/s3-config`, and these docs).
Scope: OWASP-style review of the API (Rust/axum + sqlx), the SPA (React/TS), the database guards and
the CI security gates. Tenant isolation, authN/session, approvals/SoD and production config are
owned by the S3 tenant / auth / appr / config workers and are not repeated here.

Evidence runs (GitHub Actions, branch `part1/s3-appsec`):

| Workflow | Run | Head | Result |
|---|---|---|---|
| SutraERP CI | 38767258747 | 6c6db5c1 | green (Rust fmt·clippy·test, Rust integration, Web, OpenAPI drift, 9 E2E suites, container images) |
| SutraERP Part 1 demo (full stack) | 38765529815 | 4ae0b0e2 | green |
| SutraERP Part 1 demo (full stack) | 38767258857 | 6c6db5c1 | green |
| SutraERP quality gates (security · performance) | 38767374109 | 6c6db5c1 | green (Security, SAST, Image scan + ZAP, Performance, Capacity) |
| SutraERP quality gates (first run) | 38765548100 | 4ae0b0e2 | Security job red: gitleaks 4 leaks — all in `part1/s3-config` commit 2f630ac7 (gitleaks scans every fetched branch); fixed below |
| SutraERP CI (code head, superseded) | 38765529705 | 4ae0b0e2 | Rust integration (incl. `db_p1_appsec`, `db_b4b`), Web, E2E student/reskin2 green; rest cancelled by the push of the next head |

## Findings

Evidence lines refer to `part1/s2-int` @ 1fd0e322 (before the fix) unless stated.

| ID | Area | Severity | Evidence (file:line) | Exploit sketch | Fix | Test | Status |
|---|---|---|---|---|---|---|---|
| SEC-P1-APPSEC-001 | DB / audit & ledger integrity | High | `migrations/` — append-only tables (`platform.audit_log`, `audit_chain_entry`, `audit_anchor`, `finance.journal_entry`, `journal_line`, `status_history`, `charge_line`, `trial_balance`, `gateway_webhook_event`) guarded only by row-level UPDATE/DELETE triggers | Row triggers do not fire on `TRUNCATE`: the schema owner / migrating role, an operator console or a bad maintenance script wipes the general ledger or the audit trail in one statement, with no trace | `migrations/20261201000470_platform_appsec_ledger_audit_guards.sql`: `platform.forbid_truncate()` + statement-level `BEFORE TRUNCATE` trigger on all 9 tables | `db_p1_appsec::append_only_evidence_cannot_be_truncated` (truncate as owner → refused, rows intact) | FIXED |
| SEC-P1-APPSEC-002 | Finance / manual journals | High | `finance.manual_journal`, `manual_journal_line` (migration `20261030*` finance) — "only drafts can be edited" enforced only in application code | Any SQL `UPDATE` (or a future code path) rewrites amount / date / description of a posted or reversed journal, or edits/deletes its lines, so the journal no longer matches the ledger entry it produced | Same migration: `finance.guard_manual_journal` / line guard — content editable only in draft, ledger links write-once, status only posted→reversed, decision record frozen, no delete after draft, lines change only in draft | `db_p1_appsec::posted_manual_journal_is_immutable_in_the_database` | FIXED |
| SEC-P1-APPSEC-003 | Search / SQL LIKE | Medium | `modules/campus/src/library/catalogue.rs:320`, `library/members.rs:323`, `modules/student/src/guardian/repo.rs:74`, `modules/finance/src/charges/repo.rs:294`, `modules/finance/src/accounts/http.rs:211` — `format!("%{}%", q)` bound into `ILIKE` | Values were bound (no SQLi), but `%` / `_` / `\` in the term were interpreted: `q=%` or `q=_` enumerates every guardian / member / account row the caller's search is meant to narrow, and pathological patterns cost full scans | `sutra_persistence::like_contains` (`crates/persistence/src/lib.rs:36`) escapes `\ % _` and wraps `%…%`; all five call sites use it | `db_p1_appsec::search_terms_are_literal_not_sql_or_wildcards` (`%`, `_`, `' OR 1=1 --`, `\` payloads return only literal matches) | FIXED |
| SEC-P1-APPSEC-004 | Stored XSS via links | High | `modules/academic/src/assessment/repo.rs:888-932` (submission `attachmentUrl` unchecked), `recognition/certificate.rs:188` and `resources/repo.rs:115` (prefix check only) | A student submits `attachmentUrl = "javascript:alert(document.cookie)"`; the teacher's grading screen renders it as a link → script runs in the teacher's session (CSP still allows inline script, SEC-9D-011). Prefix checks also let `http://` + whitespace/control smuggling through | `sutra_kernel::weblink::http_link` / `opt_http_link` (`crates/kernel/src/weblink.rs`): absolute `http(s)` URL with a host (no `/`, `\\`, `@`, `:`, `?`, `#` right after the scheme), no whitespace/control chars, ≤ 2000 chars; used by submissions, certificates, resources | unit `weblink::refuses_script_and_smuggled_schemes`; API attack cases in `db_b4b::internal_assessment_and_coursework` (`javascript:`, `data:`, `//evil` → 422, nothing stored) | FIXED |
| SEC-P1-APPSEC-005 | Error handling / info leak | Low | `crates/web/src/problem.rs:134` — `KernelError::Unavailable(d)` echoed `d` in the 503 problem `detail` | Adapters wrap transport errors as `"<component>: <error>"`; reqwest/lettre/OpenSearch errors carry internal URLs, hosts, ports → an unauthenticated caller who triggers a dependency outage learns the internal topology | `public_unavailable_detail`: only the component label is returned; full text logged at warn | `problem::tests::unavailable_errors_do_not_leak_internal_endpoints` | FIXED |
| SEC-P1-APPSEC-006 | CSV / formula injection | Medium | `apps/web/app/features/reports/Overview.tsx:61` — report CSV escaped only `" , \n` | A label/value beginning with `= + - @` (tab/CR) opens as a formula in Excel/Sheets (`=HYPERLINK(...)` exfiltration, DDE) for the staff member who downloads the report | `apps/web/app/lib/csv.ts` `csvCell`/`csvRow` prefix `'` to formula-leading cells and quote separators; `Overview.tsx` uses it | `apps/web/app/lib/csv.test.ts` (Web job) | FIXED |
| SEC-P1-APPSEC-007 | SAST false positive | Info | `apps/api/src/main.rs:38` — `std::env::args().nth(1)` flagged by Semgrep `rust.lang.security.args.args` | None: argv selects the subcommand (serve / migrate / openapi), never an authorisation or path-trust input | Inline `nosemgrep: rust.lang.security.args.args` with the reason | Semgrep job: 0 findings | ACCEPTED-RISK (false positive; argv is operator-controlled, not a trust input) |
| SEC-P1-APPSEC-008 | Secrets scan (CI) | Info | gitleaks `generic-api-key` ×4: `apps/api/src/config_http.rs:439,447`, `apps/api/tests/db_p1_appr.rs:508,524` in commit 2f630ac7 (`part1/s3-config`, reachable because the job fetches all branches) | None: fake production-config fixtures of the config unit tests (`erp.example.edu`), already replaced by `unit-test-only-*` values on `part1/s3-config` (76d90fc) | Same two-literal allow-list as `part1/s3-config` 76d90fc (identical hunk in `sutraerp/.gitleaks.toml`, documented there as SEC-P1-CONFIG-010) | quality Security job green on 6c6db5c1 | ACCEPTED-RISK (history-only fake fixtures; allow-list matches the two literals only, default rules stay on) |
| SEC-P1-APPSEC-009 | Logging | Info | failed-login audit / log lines carry the submitted username | Username enumeration via logs needs log access; the value is needed for brute-force / lockout investigation | — | — | ACCEPTED-RISK (operational need; logs are access-controlled; passwords never logged) |
| SEC-P1-APPSEC-010 | Payroll bank file download | Medium | payroll bank-file download scoped by URL path institution (readiness `C-010`) | A user with payroll permission in institution A requests institution B's file by path | — (tenant/institution scope is the S3-tenant worker's area) | — | DEFERRED to S3-tenant (C-010) |

### Reviewed, no change needed

- **SQL built with `format!`**: every interpolated fragment is a compile-time constant (table/column names, ORDER BY allow-lists); request values are bound. Enforced from now on by the repository Semgrep rule `sutra-no-request-data-in-sql-text`.
- **Raw HTML sinks in the SPA**: none (`dangerouslySetInnerHTML`, `innerHTML`, `eval`, `new Function`); enforced by `sutra-no-raw-html-sink`.
- **Money**: `rust_decimal` / `numeric` end to end, no floats in amounts.
- **Idempotency / replay**: `platform.idempotency` keys on money-moving POSTs already in place.
- **File upload**: no binary uploads exist (links only — covered by 004).

## Scan results (quality run 38767374109 unless stated)

| Scanner | Scope | Result | Exceptions (justified) |
|---|---|---|---|
| cargo-deny (advisories · bans · licenses · sources) | `Cargo.lock`, 390 crates | pass (duplicate-version warnings only) | RUSTSEC-2023-0071 (rsa via unused sqlx-mysql, no upstream fix), RUSTSEC-2024-0436 (paste unmaintained, proc-macro) — pre-existing SEC-9D-001/002, expire 2027-04-14 |
| cargo-audit | `Cargo.lock` | 0 vulnerabilities | same two ignores as above |
| pnpm audit `--prod --audit-level high` | web workspace | 0 (info/low/moderate/high/critical all 0) | none |
| gitleaks 8.30.1 (full history, all branches) | 2,869 commits | green — no leaks (2,869 commits) | 008 above, plus pre-existing allow-lists (OIDC test keys, architecture prose, `dev-only` defaults) |
| Semgrep 1.181.0 (p/rust · p/typescript · p/react · p/javascript · `sutraerp/.semgrep.yml`) | 1,063 files, 87 rules | 0 findings (run 38765548100 and 38767374109) | 007 above (inline `nosemgrep` with reason) |

New CI gate: job **SAST (Semgrep)** in `.github/workflows/sutraerp-quality.yml` — any finding fails
the job; SARIF/JSON uploaded as `sast-reports`.
