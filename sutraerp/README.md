# SutraERP

Production-grade education ERP — **Batch 0: Foundation** + **Batch 1 slices A, B and C** (durable sessions,
CSRF, authorization policy, session-derived tenant scope; Keycloak OIDC adapter, TOTP MFA + step-up,
login CSRF, cross-site Origin checks; NATS JetStream outbox relay, advisory-locked sweepers, Valkey cache).

A Rust modular monolith (Axum + Tokio, PostgreSQL 18 + SQLx) with a React Router SPA in the
**Soft Bento** design language. Architecture authority: the SutraERP L0–L6 architecture repository
(ADRs under `architecture/L5_Solution_Architecture/adr`). See [`docs/adr-map.md`](docs/adr-map.md)
for how each ADR is realised here and [`docs/batches.md`](docs/batches.md) for the delivery plan.

![Administrator home](docs/screenshots/home-administrator.png)

## Architecture map

```
sutraerp/
├── Cargo.toml                 Cargo workspace
├── crates/                    shared, domain-free crates
│   ├── kernel/                UUIDv7 ids · ExecutionScope (tenant/institution) · Money (rust_decimal)
│   │                          · Role/Permission policy + AuthContext · secret helpers (CSPRNG, SHA-256, HMAC, ct-eq)
│   │                          · Version (optimistic concurrency) · CloudEvents envelope + EventPublisher
│   │                          · AuditEntry/AuditSink · IdempotencyStore · ReadinessCheck · KernelError
│   │                          · MessagePublisher port + subject derivation · advisory Cache port
│   ├── contracts/             public inter-module ports + DTOs (the ONLY way modules talk)
│   ├── web/                   Axum layer: RFC 9457 problems · request id · session auth + CSRF middleware
│   │                          · Authenticated/Scope extractors · Idempotency-Key middleware · /health/live|ready
│   ├── persistence/           SQLx pool (UTC) · embedded migrations · outbox writer + relay · audit sink
│   │                          · PostgreSQL idempotency store · DB readiness · sweeper scheduler (advisory locks)
│   └── infra/                 product adapters behind kernel ports: NATS JetStream publisher, Valkey cache
│                              (only the composition root depends on it)
├── modules/                   one crate per domain (A–J), each owns one PostgreSQL schema
│   ├── platform/      (A)     auth boundary (login/session/MFA hook, dev stub, Keycloak interface),
│   │                          durable sessions (platform.session, idle + absolute expiry) + CSRF tokens,
│   │                          WorkflowPort no-op adapter, workspace catalog, module registry
│   ├── institution/   (B)     reference aggregate: tx + outbox + audit + version/ETag + Idempotency-Key
│   ├── student/       (C)  academic/ (D)  people/ (E)  finance/ (F, money model seed)
│   └── campus/ (G)  services/ (H)  exams/ (I)  resources/ (J)      ← scaffolds
├── apps/
│   ├── api/                   composition root: config · telemetry · router · OpenAPI · graceful shutdown
│   │   └── tests/             architecture fitness tests + router tests (no DB)
│   └── web/                   React Router framework mode (SPA, ssr:false) + TanStack Query
├── packages/
│   ├── ui/                    Soft Bento component library (tokens, Shell, BentoTile, StatTile, DataTable,
│   │                          Drawer, Modal, Toast, CommandPalette, Badge, Button, Input/Select, EmptyState…)
│   └── api-client/            openapi-typescript types + openapi-fetch client
├── catalog/workspaces.json    99 workspaces / 595 workflows (shared by API and web)
├── migrations/                schemas per domain, audit_log, outbox, idempotency_key, institution
├── openapi/openapi.json       generated OpenAPI 3.1 (committed; CI checks drift)
├── deploy/                    Dockerfiles, nginx, Keycloak dev realm, Postgres init
├── docker-compose.yml         postgres 18 · keycloak 26 · valkey · nats (JetStream) · mailpit (+ api/web profile)
└── .github/workflows/ci.yml   fmt · clippy -D warnings · tests · PG18 integration · web · OpenAPI drift
```

**Boundary rules** (enforced by `cargo test`, see `apps/api/tests/architecture.rs`):

- a module crate depends only on `sutra-kernel`, `sutra-contracts`, `sutra-web`, `sutra-persistence`;
- NATS / Valkey SDKs appear only in `sutra-infra` (never in modules or the other shared crates);
- `kernel` and `contracts` never depend on Axum/SQLx/Tower;
- no module SQL references another module's schema;
- every module crate is registered in the composition root.

## Running locally

Prerequisites: Rust stable (see `rust-toolchain.toml`), Node 22, pnpm (`corepack enable`), Docker.

```sh
make setup        # pnpm install + .env from .env.example
make up           # postgres 18, keycloak 26, valkey, nats (JetStream), mailpit
make migrate      # apply migrations (separate step, ADR-L5-0008)
make api          # http://localhost:8080   (OpenAPI: /api/openapi.json)
make web          # http://localhost:5173   (proxies /api to :8080)
```

The web app runs without the API by default (`VITE_AUTH_MODE=mock`): pick a role on the sign-in
page. Set `VITE_AUTH_MODE=api` to sign in through `POST /api/v1/auth/login` (dev authenticator,
password `sutra`). With `SUTRA_DEV_MFA=true` administrators are asked to enrol an authenticator app
(QR code on `/mfa/enrol`) and then enter a real TOTP code at every sign-in. `VITE_AUTH_MODE=oidc`
pairs with `SUTRA_AUTH_MODE=keycloak` and shows a single sign-on button (`GET /api/v1/auth/oidc/start`).

The API also starts with no database at all for UI work:

```sh
SUTRA_SESSION_STORE=memory SUTRA_IDEMPOTENCY_STORE=memory SUTRA_AUDIT_SINK=memory cargo run -p sutra-api
# /health/ready reports 503 (postgres unreachable) — liveness, auth and catalog still work
```

Whole stack in containers: `docker compose --profile app up --build` (web on :3000).

## Quality gates

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace                       # no database needed
make test-db                                 # + live PostgreSQL 18 tests: persistence, PG session store,
                                             #   end-to-end cross-tenant isolation, MFA factors,
                                             #   pre-auth records, TOTP + step-up, outbox relay,
                                             #   sweepers, institution cache (DATABASE_URL)
make test-live                               # + live NATS JetStream / Valkey adapter tests
                                             #   (nats-server -js and valkey/redis on localhost)
pnpm install && pnpm -r typecheck && pnpm lint && pnpm -r test && pnpm -r build
make openapi                                 # regenerate spec + TS client after API changes
make ci                                      # everything CI runs, incl. OpenAPI drift
```

## API conventions (ADR-L5-0034)

- `/api/v1/...` URL-major versioning; JSON members are camelCase.
- Errors are `application/problem+json` (RFC 9457) with `errorCode`, `correlationId`
  (= `X-Request-Id`) and `validationErrors`.
- Retry-sensitive commands accept `Idempotency-Key`; same key + different body → 422, in flight → 409,
  replay carries `Idempotency-Replayed: true`.
- Authentication: opaque server-side session in an `HttpOnly; SameSite=Strict` (`Secure`, `__Host-`)
  cookie. No session → `401`. Missing permission → `403` (deny-by-default role→permission policy).
- CSRF: login and `GET /api/v1/auth/session` return `csrfToken`; every POST/PUT/PATCH/DELETE carrying the
  session cookie must send it as `X-CSRF-Token` → otherwise `403` `csrf-token-missing` / `csrf-token-invalid`.
  Login and MFA verify (no session yet) need the signed pre-session token from `GET /api/v1/auth/csrf`
  (double submit: `__Host-sutra_login_csrf` cookie + `X-CSRF-Token`).
- Cross-site unsafe requests (`Sec-Fetch-Site: cross-site`, or an `Origin` that is neither the API's own
  host nor in `SUTRA_ALLOWED_ORIGINS`) → `403`.
- Step-up: admin-sensitive permissions (`institution.register`) need an MFA-verified session when step-up
  is enforced → `403` `mfa-step-up-required`.
- Tenant scope comes **only** from the session (ADR-L5-0004). `X-Sutra-Tenant-Id` / `X-Sutra-Institution-Id`
  are no longer read; if a client still sends one that contradicts the session it gets `403`.
- Mutable aggregates expose `ETag: "<version>"`; updates require `If-Match` (412 on conflict, 428 if absent).

## Endpoints

| Method | Path | Notes |
|--------|------|-------|
| GET | `/health/live`, `/health/ready` | readiness: PostgreSQL (critical) + NATS / Valkey when enabled (non-critical → `degraded`, still 200); 503 while draining |
| GET | `/api/v1/auth/csrf` | pre-session (login) CSRF token: cookie + body |
| POST | `/api/v1/auth/login` | dev mode: login id + password (login CSRF required); `authenticated` or `mfaRequired`; `501` in Keycloak mode |
| POST | `/api/v1/auth/mfa/verify` | TOTP code for a pending challenge (login CSRF required) |
| GET | `/api/v1/auth/session` | current principal, `expiresAt`, `idleExpiresAt`, `csrfToken`, `mfaLevel`, `amr` |
| POST | `/api/v1/auth/logout` | revokes the server-side session; Keycloak mode returns the RP-initiated `endSessionUrl` |
| POST | `/api/v1/auth/mfa/totp/enrolment`, `/api/v1/auth/mfa/totp/confirm` | dev-mode TOTP enrolment (secret + `otpauth://` URI, first code activates) |
| POST | `/api/v1/auth/mfa/step-up` | TOTP code → session rotated to `mfaLevel = mfa` |
| POST | `/api/v1/auth/mfa/webauthn/registration`, `/assertion` | port only: `501` problem (deferred) |
| GET | `/api/v1/auth/oidc/start`, `/api/v1/auth/oidc/callback` | Keycloak mode: Authorization Code + PKCE BFF flow |
| GET | `/api/v1/platform/workspaces[?role=&group=]`, `/api/v1/platform/workspaces/{key}` | navigation catalog |
| GET | `/api/v1/platform/modules` | module registry |
| POST/GET/PATCH | `/api/v1/institution/institutions[/{id}]` | reference aggregate (needs PostgreSQL); register/rename: administrator, read: any role, always in the session tenant |
| GET | `/api/openapi.json` | generated OpenAPI 3.1 |

## Batch 1 slice A — sessions, CSRF, policy, scope

| Concern | Implementation |
|---------|----------------|
| Durable sessions | `platform.session` (`migrations/20261011000001_platform_session.sql`), `PgSessionStore` in `modules/platform/src/auth/session_pg.rs`; `InMemorySessionStore` for no-DB dev/tests. Only `SHA-256(token)` is stored; 256-bit tokens; revocation sets `revoked_at`. |
| Expiry | Absolute `SUTRA_SESSION_TTL_SECS` (default 8 h, also cookie `Max-Age`) + sliding idle `SUTRA_SESSION_IDLE_SECS` (default 30 min). Checked atomically with the DB clock in one `UPDATE … RETURNING`. |
| Store selection | `SUTRA_SESSION_STORE=postgres\|memory` (default `postgres`, `memory` when `SUTRA_ENV=test`; `memory` refused in production). |
| CSRF | Per-session random secret; token = `HMAC-SHA256(secret, "sutraerp.csrf.v1\|" ‖ session token)`, so it is session-bound, re-derivable for `GET /auth/session`, and not computable from a DB dump. Enforced by `sutra_web::auth::authenticate` with a constant-time compare. |
| Authorization (D02) | `sutra_kernel::auth::{Role, Permission, role_permissions}` — exhaustive, deny-by-default map. Handlers take `sutra_web::Authenticated` and call `.require(Permission::…)`. |
| Scope | `AuthContext.scope` from the session is the only scope authority; the idempotency store is keyed by `<tenant>/<subject>` of the session. |

Web (`VITE_AUTH_MODE=api`) keeps the CSRF token in memory from the login/session responses and the typed
client adds `X-CSRF-Token` to unsafe requests; mock mode is unchanged.

## Batch 1 slice B — Keycloak OIDC, MFA, login CSRF, Origin checks

| Concern | Implementation |
|---------|----------------|
| Auth mode | `SUTRA_AUTH_MODE=dev\|keycloak` (default `dev`, refused in production). Keycloak settings: `SUTRA_OIDC_ISSUER` (or `SUTRA_KEYCLOAK_URL` + `SUTRA_KEYCLOAK_REALM`), `SUTRA_OIDC_CLIENT_ID`, `SUTRA_OIDC_CLIENT_SECRET`, `SUTRA_OIDC_REDIRECT_URI`, `SUTRA_OIDC_POST_LOGOUT_REDIRECT_URI`, tenant/institution claim names, MFA `acr` values. See `.env.example`. |
| OIDC BFF | `modules/platform/src/auth/keycloak.rs`. `/oidc/start` stores `state` → nonce, PKCE `S256` verifier, browser-binding cookie digest and return path in `platform.pre_auth` (single use, 10 min). `/oidc/callback` exchanges the code (confidential client), validates the ID token (JWKS cached, refreshed once on unknown `kid`; RS/ES/PS algorithms only; `iss`, `aud`/`azp`, `exp`/`nbf`, `nonce`), maps `sub`/`preferred_username`/`name`, realm roles → `Role`, `tenant_id`/`institution_id` → scope (fail closed), `amr`/`acr` → MFA level, stores `sid`, creates the session and redirects to the SPA. Logout returns the RP-initiated `end_session_endpoint` URL (`client_id` + `post_logout_redirect_uri`; no `id_token_hint` because ID tokens are not retained). Keycloak tokens never reach the browser. |
| Dev-realm | `deploy/keycloak/realm-sutra-dev.json`: confidential `sutra-web` client (PKCE S256), realm roles = SutraERP roles, `tenant_id`/`institution_id` user-attribute mappers, `CONFIGURE_TOTP` / `webauthn-register` required actions. |
| TOTP MFA (dev mode) | `modules/platform/src/auth/mfa.rs`: RFC 6238 (SHA-1, 6 digits, 30 s, ±1 step), enrolment → `otpauth://` URI + QR, first code activates; last accepted step stored so a code cannot be replayed. Secret encrypted at rest with AES-256-GCM (`SUTRA_MFA_ENCRYPTION_KEY`, AAD = tenant\|subject\|kind) in `platform.mfa_factor`. Administrators require MFA when `SUTRA_DEV_MFA=true`. |
| Session assurance | `platform.session.mfa_level` / `amr` / `idp_sid` (`migrations/20261012000001_platform_auth_mfa.sql`). `Permission::requires_step_up` (institution register) → `403` problem unless `mfa_level = mfa`; `POST /auth/mfa/step-up` rotates the session. |
| WebAuthn | Port (`WebauthnPort`) + endpoints answering `501` (deferred; available in Keycloak mode via the `webauthn-register` required action). |
| Login CSRF | `GET /api/v1/auth/csrf`: HMAC-signed, expiring double-submit token (`SUTRA_CSRF_KEY`, `SUTRA_LOGIN_CSRF_TTL_SECS`); required on login and MFA verify. MFA challenges are bound to the token they were issued with. |
| Origin guard | `sutra_web::auth::origin_guard` on every unsafe request (`SUTRA_ALLOWED_ORIGINS` for extra origins). |
| `last_seen_at` throttle | Written at most once per `SUTRA_SESSION_TOUCH_SECS` (default 60 s) per session. |

Web: sign-in fetches the login CSRF token first, shows a TOTP step on `mfaRequired`, routes to the MFA
enrolment screen (QR code) when `mfaEnrollmentRequired`; `oidc` mode shows an SSO button.

## Batch 1 slice C — JetStream outbox relay, sweepers, Valkey cache

| Concern | Implementation |
|---------|----------------|
| Ports | `sutra_kernel::messaging::MessagePublisher` (+ `InMemoryPublisher` test adapter, `subject_for`) and `sutra_kernel::cache::{Cache, AdvisoryCache}` (+ `InMemoryCache`, `NoCache`). Adapters in `crates/infra`: `JetStreamPublisher` (async-nats, rustls) and `ValkeyCache` (redis crate, rustls for `rediss://`). |
| Outbox relay | `crates/persistence/src/outbox_relay.rs`, enabled by `SUTRA_OUTBOX_RELAY=nats` (default `off` in dev/test, `nats` in production; `off` in production needs `SUTRA_OUTBOX_RELAY_ALLOW_OFF=true`). Claim: short transaction, relay-claim advisory lock, `FOR UPDATE SKIP LOCKED`, rows leased via `claim_token` + `next_attempt_at = now() + lease`. Publish in `created_at` order to `sutra.<tenant>.<domain>.<aggregate>.<event>` with `Nats-Msg-Id` = outbox id and CloudEvents headers; mark `published_at` **only after the JetStream ack**. Transient failure → `attempts+1`, `last_error`, exponential backoff with jitter, pass stops (broker likely down); permanent failure or `SUTRA_OUTBOX_MAX_ATTEMPTS` → `dead_lettered_at` (kept for operators). Later events of a failed aggregate (same tenant + subject) are held back to keep per-aggregate order. Cancellation token for graceful shutdown. Stream ensured at startup (`SUTRA_NATS_STREAM`, subjects `<prefix>.>`, duplicate window > lease). |
| Migration | `migrations/20261013000001_platform_outbox_relay.sql`: `claim_token`, `last_attempt_at`, `dead_lettered_at`, published-xor-dead-lettered check, pending / per-subject / retention / dead-letter partial indexes. |
| Sweepers | `crates/persistence/src/jobs.rs`: `Scheduler` (one tokio loop per job, first run spread over one interval, ±10 % jitter) and `SweepJob` run inside a transaction holding `pg_try_advisory_xact_lock(fnv1a(name))` — one replica per job per tick, others skip. Jobs (bounded by `SUTRA_SWEEP_BATCH_LIMIT` rows per run): expired idempotency keys, published outbox rows older than `SUTRA_OUTBOX_RETENTION_SECS` (dead-lettered kept), sessions expired/revoked/idle beyond `SUTRA_SESSION_PURGE_GRACE_SECS` and expired pre-auth records beyond `SUTRA_PREAUTH_PURGE_GRACE_SECS` (`modules/platform/src/sweep.rs`). `SUTRA_SWEEPERS=false` or a zero interval disables. Counts are logged on the `sutra::metrics` target. |
| Cache | `SUTRA_CACHE=off\|memory\|valkey` (default `memory` in dev/test, `off` in production because a process-local cache is incoherent across replicas). Keys `sutra:<env>:<owner>:t:<tenant>[:i:<institution>]:…` (`SUTRA_CACHE_NAMESPACE`), TTL `SUTRA_CACHE_TTL_SECS`, per-command timeout `SUTRA_CACHE_TIMEOUT_MS`, 5 s fail-fast cool-down after a Valkey error. Every error is a logged miss. Real use: read-through cache for `GET /api/v1/institution/institutions/{id}` keyed by the session tenant (checked after authorization), invalidated by the owner after register/rename — also when a rename fails the `If-Match` check, so the client's refetch is fresh. The cache is never session or authorization authority (ADR-L5-0037 §2.8). |
| Readiness | NATS (connected + stream ensured) and Valkey (`PING`) are reported when enabled but are **non-critical**: the outbox buffers events while NATS is down and a cache outage is a miss, so a failure returns `200` with `status: degraded` instead of taking the replica out of rotation. PostgreSQL stays critical. |
| Shutdown | After HTTP draining, the relay finishes its current pass and the sweepers their current run, then stop (10 s bound). |
