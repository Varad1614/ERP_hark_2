# Tracker — S3-config (Part 1 · Stage 3: secrets, configuration, transport)

Branch `part1/s3-config` from `part1/s2-int` @ 1fd0e322. Findings: `docs/readiness/security/config.md`.

| ID | Issue | Root cause | Files changed | Status | Tests | CI result + run id | Blockers | Commit |
|---|---|---|---|---|---|---|---|---|
| SEC-P1-CONFIG-001/003/004/005/011 | Production accepted dev variables, default/weak secrets, shared keys, plain-http URLs, DB without TLS, exposed docs/health detail | No production start-up gate beyond auth mode | `apps/api/src/config_http.rs`, `config.rs`, `app.rs`, `.env.example` | FIXED | `config_http::tests::*` | CI 38775388199 success | — | 2f630ac, d125e0f |
| SEC-P1-CONFIG-002 / DEF-F-002 | Mock payment gateway allowed in production | Rule only demanded a webhook secret | `apps/api/src/config_b9.rs`, `deploy/helm/sutraerp/templates/core/configmap.yaml`, deploy workflow | FIXED | `config_b9::tests::refuses_the_mock_payment_gateway_even_with_a_strong_secret`; deploy `production-refuses-*` | CI 38775388199 success, deploy 38775425281 success | — | 2f630ac, 97009e6 |
| SEC-P1-CONFIG-006/007/008/009 | No API security headers/HSTS, CORS allow-list, rate limiting, transport body limit / timeout | Not implemented in `crates/web` | `crates/web/src/{security_headers,cors,rate_limit}.rs`, `apps/api/src/app.rs` | FIXED | unit tests in each module; `db_p1_config::api_transport_hardening_end_to_end` | CI 38775388199 success | — | 2f630ac |
| SEC-P1-CONFIG-010 | Secret-looking fixtures in history | Test literals matched gitleaks rules | test fixtures, `sutraerp/.gitleaks.toml` | FIXED | quality gitleaks | quality 38775422497 success | — | 76d90fc |
| SEC-P1-CONFIG-012 | SPA CSP `script-src 'unsafe-inline'`, missing nginx limits | Static CSP string | `deploy/docker/{nginx.conf,csp-hashes.mjs,Dockerfile*}`, Helm web configmap | FIXED | image build + deploy chart invariants; demo | demo 38775388189 success, deploy 38775425281 success | — | 97009e6 |
| SEC-P1-CONFIG-013 / DEF-J-004 | Supplier bank details changeable by one user | Bank columns were a plain profile field | migration `20261210000501_resources_supplier_bank_change.sql`, resources suppliers router/service, OpenAPI + TS client | FIXED | `db_p1_config::supplier_bank_details_change_only_through_an_independent_approval` | CI 38775388199 success, demo 38775388189, pilot 38775429092 | — | 320834e, 6a83461, 08aa49a |
| — | clippy / eslint follow-ups | lint findings on new code | config_http.rs, suppliers router, eslint config | DONE | lint jobs | CI 38775388199 success | — | 373f319, 6d6b58a, 0c880f6 |

Behaviour change encoded in tests: approved suppliers' bank fields are no longer editable via PATCH (409); callers use the bank-change request flow.
