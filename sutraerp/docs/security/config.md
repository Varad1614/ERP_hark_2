# Secrets, configuration and transport hardening (Part 1 · Stage 3 · S3-config)

What the API refuses at start-up, which HTTP protections every response carries, where secrets
live, and how the supplier bank-detail change is controlled. Findings / evidence:
`docs/readiness/security/config.md`. Code: `apps/api/src/config_http.rs` (production gate),
`apps/api/src/config_b9.rs` (payments), `crates/web/src/{security_headers,cors,rate_limit}.rs`,
`apps/api/src/app.rs::router`, `deploy/docker/nginx.conf`, `deploy/docker/csp-hashes.mjs`,
`deploy/helm/sutraerp/templates/{web/configmap,core/configmap}.yaml`.

## 1. Production start-up gate

`Config::load` (used by `serve`, `migrate` and `sutra-api check-config`) exits with code 2 when
`SUTRA_ENV=production` and any rule below fails. The message names the variable; secrets are never
printed. Each rule has a unit test in `config_http::tests` (or the named module).

| Area | Rule (production) | Test |
|---|---|---|
| Debug / dev switches | `SUTRA_AUTH_MODE=dev`, `SUTRA_SESSION_COOKIE_SECURE=false`, `SUTRA_MFA_STEP_UP=false` refused (earlier batches) | `config::tests::production_requires_database_and_forbids_dev_auth` |
| | any of `SUTRA_DEV_AUTH_PASSWORD`, `SUTRA_DEV_MFA`, `SUTRA_DEV_TENANT_ID`, `SUTRA_DEV_INSTITUTION_ID` set | `refuses_development_only_variables` |
| Auto / noop adapters | `SUTRA_APPROVAL_MODE=auto`, `SUTRA_WORKFLOW=noop` (also the default), `SUTRA_FLOWABLE_AUTO_APPROVE=true` (DEF-X-001), in-memory session / idempotency / audit stores, relay off without `SUTRA_OUTBOX_RELAY_ALLOW_OFF` | `config::tests::*`, `db_p1_appr` |
| Payments (DEF-F-002) | `SUTRA_PAYMENT_GATEWAY=mock` refused **whatever the webhook secret** (the mock "captures" without money); only `disabled` until a real adapter exists | `config_b9::tests::payment_gateway_defaults_and_production_rules`, `refuses_the_mock_payment_gateway_even_with_a_strong_secret` |
| Default / weak secrets | `SUTRA_OIDC_CLIENT_SECRET`, `SUTRA_DB_RUNTIME_PASSWORD`, `SUTRA_SMTP_PASSWORD`, `SUTRA_FLOWABLE_PASSWORD`, `SUTRA_OPENSEARCH_PASSWORD` and the password inside `DATABASE_URL` / `SUTRA_MIGRATION_DATABASE_URL`: refused when shorter than 16 characters or one of the published values (`sutra`, `sutra-api-dev-only`, `dev-only-change-me`, `sutra-dev-gateway-secret`, `change-me`, `password`, `admin`, `test`, `rest-admin`, …, `config_http::KNOWN_DEV_SECRETS`); Flowable / OpenSearch passwords required when those adapters are on | `refuses_default_and_short_secrets` |
| Keys | `SUTRA_CSRF_KEY` required, built-in dev `SUTRA_MFA_ENCRYPTION_KEY` refused (earlier); now also: the three keys (CSRF, MFA, audit anchor) must be **distinct** and random-looking (≥ 12 distinct bytes; `openssl rand -hex 32`) | `refuses_reused_low_entropy_and_published_keys` |
| TLS | `SUTRA_PUBLIC_URL`, `SUTRA_OIDC_ISSUER`, `SUTRA_OIDC_REDIRECT_URI`, `SUTRA_OIDC_POST_LOGOUT_REDIRECT_URI`, every `SUTRA_WEBAUTHN_ORIGINS` / `SUTRA_ALLOWED_ORIGINS` entry must be `https://`; `DATABASE_URL` (and the migration URL) must carry `sslmode=require\|verify-ca\|verify-full`; `SUTRA_SMTP_TLS=none` refused with SMTP on; `SUTRA_HSTS_MAX_AGE_SECS` ≥ 15552000 | `refuses_plain_http_urls_and_missing_database_tls` |
| Public surface | `SUTRA_RATE_LIMIT=false`, `SUTRA_API_DOCS=true`, `SUTRA_HEALTH_DETAIL=true` refused | `refuses_disabled_rate_limit_and_exposed_ops_endpoints` |
| Every environment | `SUTRA_ALLOWED_ORIGINS` exact origins only (no `*`, `null`, wildcard host, path); body limit 1 KiB…64 MiB; request timeout 1…300 s; rate limits > 0; trusted proxy hops ≤ 5; metrics never on the API port | `wildcard_cors_and_bad_limits_are_refused_everywhere` |

The Helm chart fails the render first for the same production mistakes passed through
`api.extraEnv` (`SUTRA_DEV_*`, `SUTRA_PAYMENT_GATEWAY=mock`, `SUTRA_RATE_LIMIT=false`,
`SUTRA_SESSION_COOKIE_SECURE=false`, `SUTRA_API_DOCS=true`, `SUTRA_HEALTH_DETAIL=true`) —
deploy workflow checks `production-refuses-*`.

Reference valid production environment: `config_http::tests::prod()`. Operators: generate keys with
`openssl rand -hex 32` (one per purpose), passwords with `openssl rand -base64 24`, and put the
`DATABASE_URL` with `?sslmode=verify-full` into OpenBao `kv/sutraerp/<env>/database`.

## 2. Secrets in the repository

* Scanned with gitleaks 8.30.1 (default rules + `sutraerp/.gitleaks.toml`) over the working tree and
  the full history: no finding outside the documented allow-list (throwaway OIDC test keys, two
  architecture prose false positives, the documented dev defaults `dev-only-change-me` /
  `sutra-api-dev-only`, and the two replaced S3-config test fixtures — SEC-P1-CONFIG-010).
* Helm values carry no credential (deploy workflow `chart invariants` greps every rendered object);
  every credential comes from OpenBao (`secrets.mode=openbao`) or an existing Secret.
* `docker-compose.yml`, `.env.example`, `deploy/keycloak/realm-sutra-dev.json` contain **only
  development values** (`sutra`, `dev-only-change-me`, `sutra-api-dev-only`, `admin`, `keycloak`);
  each of them is now refused by the production gate, and `.env.example` says so.
* `deploy/openbao/seed-dev.sh` generates random values (`openssl rand`) for kind.

## 3. HTTP hardening

### API (every response, `crates/web/src/security_headers.rs`, outermost layer)

`X-Content-Type-Options: nosniff`, `X-Frame-Options: DENY`,
`Content-Security-Policy: default-src 'none'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'`,
`Referrer-Policy: no-referrer`, `Cross-Origin-Resource-Policy: same-origin`, `Permissions-Policy`,
`Cache-Control: no-store` (unless the handler set its own), and
`Strict-Transport-Security: max-age=<SUTRA_HSTS_MAX_AGE_SECS>; includeSubDomains` (production default
one year). Applies to 404 / 413 / 429 / timeout / panic answers too.

* **CORS** (`cors.rs`): only origins listed in `SUTRA_ALLOWED_ORIGINS` get a credentialed answer echoing
  that origin (`Vary: Origin`); never `*`; preflight only for listed origins. Unsafe cross-site
  requests are still refused by the Origin / `Sec-Fetch-Site` guard.
* **Body limit**: `SUTRA_MAX_BODY_BYTES` (default 2 MiB) for both the transport layer and axum
  extractors → 413.
* **Timeouts**: `SUTRA_REQUEST_TIMEOUT_SECS` (default 30, max 300) → 504; DB acquire 5 s.
* **Rate limiting** (`rate_limit.rs`, inside the session layer): token bucket per client and class —
  `credentials` (login / MFA verify / step-up / WebAuthn assertion, always, 30/min burst 10),
  `auth` (other `/api/v1/auth/*` without session, 300/min burst 100), `public` (any other `/api/*`
  request without a live session, incl. the payment webhook, 600/min burst 200). `/health/*` never.
  429 problem+json with `Retry-After`. Client = peer address, or the address seen by the outermost
  of `SUTRA_TRUSTED_PROXY_HOPS` proxies (production default 1 = the ingress); spoofed left-most
  `X-Forwarded-For` entries are ignored. Bounded memory (`SUTRA_RATE_LIMIT_MAX_CLIENTS`). Limits are
  per replica; tune per campus NAT size.

### Web (nginx image + Helm web config)

* CSP `script-src 'self' <sha256 of each inline bootstrap script>` — hashes generated at image build by
  `deploy/docker/csp-hashes.mjs` into `/etc/nginx/sutra-csp.conf` (fails the build on inline event
  handlers). Replaces `'unsafe-inline'` (SEC-9D-011). `style-src` keeps `'unsafe-inline'` (React style
  attributes; no script execution — accepted).
* Part 2 S4 (UI fees, DEF-F-007): Razorpay Checkout needs exactly three additions, nothing else is
  loosened — `script-src … https://checkout.razorpay.com` (the SPA injects `checkout.js` only when
  `GET /api/v1/finance/me/payment-gateway` reports `razorpay`), `frame-src 'self' https://api.razorpay.com
  https://checkout.razorpay.com` (the Checkout iframe) and `connect-src 'self' https://api.razorpay.com`.
  No SRI on `checkout.js` (Razorpay serves one unversioned, changing file). `Permissions-Policy
  payment=()` and COOP `same-origin` are unchanged; whether some bank / 3-D Secure flows need
  `payment=(self "https://api.razorpay.com")` or COOP `same-origin-allow-popups` can only be seen
  against a live Razorpay test account (BLOCKED_EXTERNAL, docs/readiness/part2/s4-pay.md).
* Same header set inherited by every location (no per-location `add_header`), `frame-ancestors 'none'`,
  `X-Frame-Options DENY`, `Referrer-Policy strict-origin-when-cross-origin`, `Permissions-Policy`,
  COOP / CORP, `server_tokens off`; HSTS added by the Helm config when the ingress terminates TLS.
* The API's duplicates are hidden behind the proxy (`proxy_hide_header`) so each header appears once.
* `client_max_body_size 2m`, client / proxy timeouts, `limit_req` zones (credentials 10 r/s burst 50,
  API 100 r/s burst 400 per client; 429).
* Part 2 · S5-capacity (DEF-P2-CAP-003): "per client" means the real client behind the edge /
  ingress. When the peer is on a private network (the proxy) the zone key is the last
  `X-Forwarded-For` entry that proxy appended; otherwise the peer's own address. `$remote_addr` is
  not rewritten, so the API's `SUTRA_TRUSTED_PROXY_HOPS` rule is unchanged. Expose the web port only
  to the edge: a client reaching it directly from a private network could choose its bucket via
  `X-Forwarded-For` (the API limiter, keyed by the trusted-hop rule, still applies). Regression:
  `deploy/capacity/run.sh flood-check` (capacity workflow).

## 4. Admin / ops endpoints

| Endpoint | Production |
|---|---|
| `/metrics` | separate listener `SUTRA_METRICS_ADDR` (:9464), never the API port (config refuses the same port), separate `sutraerp-api-metrics` Service, NetworkPolicy allows only the metrics namespace; the ingress routes only `/api` and `/` |
| `/health/live`, `/health/ready` | unauthenticated for probes; readiness reports check names and status only — dependency error texts (hosts, drivers) are hidden (`SUTRA_HEALTH_DETAIL`, refused in production) |
| `/api/openapi.json` | not served (`SUTRA_API_DOCS`, refused in production); the document is published with the release (`openapi/openapi.json`) |
| Debug endpoints | none exist (`sutra-api openapi` / `check-config` are CLI sub-commands) |
| Audit chain / platform admin APIs | session + permission (`platform.*`), unchanged |

## 5. Supplier bank-detail change (DEF-J-004)

Once a supplier is approved its bank account is no longer a profile field:

1. `PATCH /api/v1/resources/suppliers/{id}` with different bank values → **409** (also while the
   registration awaits approval); unchanged values are a no-op.
2. `POST /api/v1/resources/suppliers/{id}/bank-change` (`resources.procurement.manage`, If-Match,
   reason required) → pending request (`pending_bank_*`, `bank_change_request_id`); live details keep
   being used; `SUPPLIER_BANK_CHANGE_APPROVAL` started; `supplier.bank_change_requested` notifies
   procurement and finance approvers; audit with masked old → new account. One open request at a time.
3. `POST /api/v1/resources/suppliers/{id}/bank-change/decision` (`resources.approve` or
   `finance.approve`, step-up): the requester (or a system actor) → **403**; approve → details go live,
   `supplier.bank_change_approved` warns finance approvers and administrators (call-back check before
   the next payment); reject (reason required) → discarded. Status history `supplier_bank_change`
   keyed by the request id; audit `resources.supplier.bank_change.decide` with masked accounts and the
   maker.
4. Database trigger `supplier_bank_details_guard` refuses any other change of an approved supplier's
   bank columns (direct SQL, future code).

Full account numbers never enter the audit log, the outbox or notifications (test
`db_p1_config::supplier_bank_details_change_only_through_an_independent_approval`).

## 6. Accepted residual risks

* NATS / Valkey / Flowable / OpenSearch URLs are not forced to TLS: in-cluster or private-subnet
  services behind the default-deny NetworkPolicy (`networkPolicy.externalCidrs`). Revisit when any of
  them leaves the private network.
* `style-src 'unsafe-inline'` on the SPA (see above).
* Rate limits are per process (no shared store); with N replicas the effective limit is N×. The
  ingress `limit_req` and Keycloak brute-force detection are the outer layers.
