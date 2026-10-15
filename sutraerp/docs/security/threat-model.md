# SutraERP threat model (Batch 9 · slice 9D)

**Scope:** the production-hardening target of Batch 9 (`docs/batch9-contracts.md` §1): browser SPA
behind nginx, the Rust API (`sutra-api`) with PostgreSQL 18, NATS JetStream, Valkey, the optional
Flowable runtime (9C), OpenSearch projections (9C), OpenBao-injected secrets (9B), the tamper-evident
audit chain (9A) and the CI → GHCR → Cosign supply chain (9B). Method: STRIDE per trust boundary.
Every open risk has an id in [`findings.md`](findings.md); the response process is
[`docs/runbooks/security.md`](../runbooks/security.md).

**Status of the evidence:** reviewed from the code at `batch9/foundation` (667b81ef) plus the 9D CI
gates (`.github/workflows/sutraerp-quality.yml`). There is no real cluster: Kubernetes / OpenBao /
ingress controls are as designed in the contract and proved only in the throwaway kind job (9B).
Statements marked *(design)* are not yet exercised against a running production deployment.

## 1. System context and trust boundaries

```
 [Browser] ──TB1── [Ingress / TLS] ──TB2── [web: nginx SPA + /api proxy] ──TB3── [API sutra-api]
                                                                                  │
         TB4 PostgreSQL (owner / runtime role) · TB5 NATS · TB6 Valkey · TB7 Flowable · TB8 OpenSearch
         TB9 OpenBao agent (secrets file) · TB10 Keycloak (OIDC) · TB11 payment gateway webhook
 [GitHub Actions] ──TB12── [GHCR images + Cosign signatures + SBOM] ──── [cluster pulls by digest]
```

| Asset | Why it matters |
|-------|----------------|
| Student, staff, guardian personal data | DPDP Act 2023 personal data; minors among students |
| Marks, attendance, results, conferred qualifications | integrity is the product |
| Fees, payments, receipts, refunds, ledger | money; regulator and audit exposure |
| Sessions, CSRF tokens, MFA secrets (TOTP keys, passkeys) | account takeover |
| Audit log + chain + anchors | non-repudiation; evidence for disputes and accreditation |
| Secrets (DB passwords, CSRF / MFA / anchor keys, OIDC client secret) | full compromise |
| Images and the release pipeline | supply-chain compromise of every tenant |

## 2. STRIDE by boundary

### TB1/TB2 — Browser ↔ web (nginx) / BFF

| STRIDE | Threat | Controls in place | Residual / finding |
|--------|--------|-------------------|--------------------|
| S | Session theft via XSS or network | Opaque 256-bit session token in an `HttpOnly; SameSite=Strict` cookie (`__Host-` + `Secure` when `SUTRA_SESSION_COOKIE_SECURE`, which production enforces); only `SHA-256(token)` stored (`session_pg.rs`); new token on every sign-in, presented session revoked (fixation defence, `auth/http.rs::issue_session`) | XSS still acts *as* the user; CSP needs `'unsafe-inline'` for scripts — SEC-9D-011 |
| T | Clickjacking, MIME sniffing, content injection | nginx: `X-Frame-Options DENY` + CSP `frame-ancestors 'none'`, `nosniff`, `object-src 'none'`, `base-uri 'self'` (fixed in 9D: SEC-9D-009/010; header test in the `Image scan + ZAP baseline` job) | — |
| R | User denies an action | Every state change writes `platform.audit_log` with actor, correlation id, outcome; chained + anchored (audit chain below) | — |
| I | Version / stack disclosure | `server_tokens off` (9D); problem+json errors without stack traces (`CatchPanicLayer`) | — |
| D | Request floods, large bodies | 2 MiB body limit, request timeout (`app.rs`) | No rate limiting in the API or nginx — SEC-9D-005 (ingress) |
| E | CSRF / login CSRF / cross-site writes | Synchronizer token per session on every unsafe method with a cookie (`crates/web/src/auth.rs::authenticate`); signed double-submit login-CSRF token on pre-session paths; `origin_guard` rejects `Sec-Fetch-Site: cross-site` and foreign `Origin`; cookies `SameSite=Strict` | — |
| E | Step-up bypass | Administrators' sensitive operations need an MFA-verified session (`SUTRA_MFA_STEP_UP`, forced on in production by `config.rs::validate`) | Step-up TOTP attempts are not throttled — SEC-9D-005 |

HSTS must be set where TLS terminates (ingress), not in the http-only nginx container — SEC-9D-012.

### TB3 — web/ingress ↔ API

| STRIDE | Threat | Controls | Residual |
|--------|--------|----------|----------|
| S | Forged client identity / scope | Execution scope comes **only** from the session; `X-Tenant-Id` / `X-Institution-Id` headers can never select or widen it and are rejected when they contradict it (`check_scope_headers`) | — |
| T | Replay of writes | `Idempotency-Key` store keyed by the authenticated owner; optimistic concurrency with `If-Match` versions | — |
| I | API served without browser hardening headers when routed directly | The API sets `Cache-Control: no-store` on auth responses only; generic security headers come from nginx | Ingress must route `/api` through the web tier or add the headers — SEC-9D-006 |
| D | Slow / heavy queries | Timeouts; paginated directory reads (`limit` + cursor); k6 gates (§9) measure p95 under 20 VU (every push) and 100 VU (capacity) | Some institution lists are unpaginated (e.g. fee heads, guardians) and grow without bound — API review item, not a gate failure |
| E | Missing authorization on a handler | `Authenticated::require(Permission)` deny-by-default with typed permissions (`sutra_kernel::auth`); architecture tests over the router | — |

### TB4 — API ↔ PostgreSQL (roles, tenant isolation)

| STRIDE | Threat | Controls | Residual |
|--------|--------|----------|----------|
| T/E | API compromise alters schema or audit history | Migrations run as the schema owner; the API connects as a runtime LOGIN role that owns nothing (DML only, ADR-L5-0005 / -0036); `audit_chain_entry` / `audit_anchor` append-only (trigger + privileges) | — |
| I | Cross-tenant read through a query missing `tenant_id` | Module queries are written `WHERE tenant_id = $1 …` with the session tenant; institution path ids are checked by per-module `require_institution*` guards | **No row-level security**: one forgotten predicate leaks across tenants — SEC-9D-004 |
| I | SQL injection | sqlx bind parameters; dynamic SQL only concatenates internal column lists / static fragments | — |
| D | Connection exhaustion | Pool size + acquire timeout from config | — |

### TB5/TB6 — API ↔ NATS JetStream / Valkey

| STRIDE | Threat | Controls | Residual |
|--------|--------|----------|----------|
| S/T | Forged or altered domain events | Transactional outbox (events committed with the change), JetStream de-duplication window; inbox consumers re-read state from PostgreSQL | NATS / Valkey have no auth in dev compose; production relies on NetworkPolicy (9B, *design*) |
| I | Cache poisoning / disclosure | Valkey is an advisory cache only (ADR-L5-0037): authorization and money never read from it; namespace per environment | — |

### TB7/TB8 — API ↔ Flowable / OpenSearch (9C)

Flowable REST and OpenSearch use per-environment credentials from OpenBao (`kv/…/flowable`,
`kv/…/opensearch`); Flowable sits behind `WorkflowPort`, decisions are synced back into PostgreSQL,
which stays the source of truth, and the no-op adapter remains the default. Projections carry
`tenant_id` and the search API filters by the session tenant *(design; 9C proves it in
`Data platform`)*. Residual: a projection or query without tenant filtering would be an
information-disclosure bug — 9C's tests are the control; re-check after merge.

### TB9 — Secrets lifecycle (OpenBao)

Secrets live in OpenBao KV v2 `kv/sutraerp/<env>/…` (§5); the agent renders one dotenv file read via
`SUTRA_ENV_FILE`; the Kubernetes auth role is bound to three ServiceAccounts with a read-only policy
on the environment path. Nothing secret is a Helm value or a committed file — enforced in CI by
gitleaks over the full history (`.gitleaks.toml`; allow-list = test fixtures + documented dev
defaults only). Production refuses to start with dev auth, insecure cookies, no `SUTRA_CSRF_KEY`, the
built-in dev MFA key, in-memory stores or no OIDC client secret (`config.rs`). Rotation and
compromise handling: runbook §4–§5.

### TB10 — Keycloak (OIDC)

Authorization Code + PKCE (S256) through the BFF; `state`, `nonce` and code verifier kept in a
single-use pre-auth record bound to a browser cookie; ID-token validation incl. issuer, audience,
expiry and constant-time `nonce` check (`auth/keycloak.rs`); back-channel logout with signed logout
tokens and a `jti` replay guard (`auth/replay.rs`). Password brute-force protection is Keycloak's
(realm setting, *design*).

### TB11 — Payment gateway webhook

`POST /api/v1/finance/gateway/webhook` verifies an HMAC-SHA256 signature in constant time and is
idempotent per `event_id`. **But** the only adapter is `MockPaymentGateway`, wired in every
environment with a public constant secret, and its checkout captures any payment the student
confirms — SEC-9D-003 (**High**; fixed in the integration: `SUTRA_PAYMENT_GATEWAY` defaults to
`disabled` in production → checkout / webhook 503, and production refuses the dev webhook secret).

### Audit chain (9A)

Tampering with `audit_log` rows is detected by recomputing `entry_hash` (§4.2) along the chain;
rewriting the whole chain is detected by anchors (HMAC with the anchor key held in OpenBao, never in
the database). Threats: (a) database-owner compromise rewrites rows *and* chain → caught by anchors
as long as the anchor key is safe; (b) anchor-key compromise → forged anchors for a rewritten chain
→ runbook §5 (freeze evidence, rotate to a new `SUTRA_AUDIT_ANCHOR_KEY_ID`, re-anchor). The verify
API is permission-gated (`platform.audit.verify`) and itself audited.

### TB12 — CI ↔ GHCR + Cosign (supply chain)

| Threat | Controls |
|--------|----------|
| Malicious / vulnerable dependency | `cargo-deny` (RustSec advisories, licence allow-list, crates.io only) + `cargo-audit` + `pnpm audit --prod` on every push; lockfiles frozen (`--locked`, `--frozen-lockfile`) |
| Vulnerable base images | Trivy (CRITICAL/HIGH, fixed) **and** Grype (high, fixed) on the built api + web images on every push — the two DBs disagree (SEC-9D-015 was seen by Grype only) |
| Compromised CI action / scanner | Third-party actions pinned to commit SHAs; scanners from pinned image tags / a checksummed binary (the March 2026 `trivy-action` tag hijack is why 9D runs the Trivy image directly instead of the action) |
| Image substitution | Push + keyless Cosign signing only on `erp_merge` / `v*` (9B), SBOM attestation, deploy by digest, verify identity regex pinned to workflow + ref (§10.2) |
| Secret exfiltration from CI | `permissions: contents: read` by default; `packages: write` / `id-token: write` only on the signing job; throwaway CI credentials generated per job (`openssl rand`) |

## 3. Review notes — auth / session / CSRF / tenant scope

Reviewed: `crates/web/src/auth.rs` (session middleware, CSRF, origin guard, scope headers),
`modules/platform/src/auth/{http,session,session_pg,preauth,replay,keycloak,http_security}.rs`,
`apps/api/src/config.rs::validate`, `modules/finance/src/payments/{http,gateway}.rs`, a sample of
module queries and the `shared/guard.rs` institution guards. Results:

- CSRF: correct for cookie sessions (synchronizer token, constant-time compare) and pre-session paths
  (signed double-submit, MAC + expiry). Unsafe requests without a cookie are not CSRF-checked —
  correct, they carry no ambient authority.
- Session: tokens hashed at rest, fixation defence, idle + absolute expiry, revoke-others,
  back-channel logout with replay guard. No change needed.
- Production guards: dev auth, insecure cookie, step-up off, missing CSRF key, dev MFA key, memory
  stores and a missing OIDC secret all refuse to start. No change needed.
- Tenant scope: session-bound, headers cannot widen; isolation relies on query predicates (SEC-9D-004).
- Payments: SEC-9D-003. Not patched in 9D (needed foundation-owned `Config` + `app.rs` wiring);
  fixed by the integrator (`config_b9.rs` payments settings, `DisabledPaymentGateway`, pilot values).

## 4. What CI proves (9D)

| Job | Proves |
|-----|--------|
| `Security (cargo-deny · cargo-audit · pnpm audit · gitleaks)` | no unaccepted RustSec advisory, licences within the allow-list, crates.io-only sources, no high npm advisory in production deps, no secret in the full git history |
| `Image scan + ZAP baseline (Trivy · Grype · ZAP)` | the api + web images build, have no fixable CRITICAL/HIGH CVEs (two scanners), boot as a stack, serve the security headers, and a ZAP passive baseline on the SPA and `/api/v1` finds no High alert |
| `Performance gate (k6)` | §9 smoke thresholds on a release build with 2 000 students seeded through the admission API |
| `Capacity (k6)` | §9 capacity thresholds + p95 regression ≤ 25 % vs `deploy/perf/baseline.json` (erp_merge / tags / dispatch) |
