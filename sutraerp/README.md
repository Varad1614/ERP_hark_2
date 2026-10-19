# SutraERP

Production-grade education ERP — **Batch 0: Foundation** + **Batch 1: Platform** (complete: slices A–D —
durable sessions, CSRF, authorization policy, session-derived tenant scope; Keycloak OIDC adapter, TOTP MFA +
step-up, login CSRF, cross-site Origin checks; NATS JetStream outbox relay, advisory-locked sweepers, Valkey
cache; notifications + e-mail channel, notices, to-dos, profile and security screens, per-schema runtime DB
roles, dev-mode WebAuthn passkeys, Keycloak back-channel logout).
**Batch 2: Institution** (Domain B, slices 2A–2C) and **Batch 3: Student lifecycle** (Domain C: admissions,
student record, registration, progression, completion/exit, guardians, scholarships, alumni; slices 3A–3C)
are complete — see the sections below.

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
│   │                          WorkflowPort (no-op default; Flowable adapter since 9C), workspace catalog, module registry
│   ├── institution/   (B)     reference aggregate: tx + outbox + audit + version/ETag + Idempotency-Key
│   ├── student/       (C)  academic/ (D)  people/ (E)  finance/ (F)
│   └── campus/ (G)  services/ (H)  exams/ (I)  resources/ (J)      ← all implemented (Batches 2–9);
│                              readiness, defects and gaps: docs/readiness/
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

`make web` (the dev server) runs without the API (`VITE_AUTH_MODE` unset = `mock`): pick a role on
the sign-in page. A build without `VITE_AUTH_MODE` is `oidc` (audit 2026-10). Set
`VITE_AUTH_MODE=api` to sign in through `POST /api/v1/auth/login` (dev authenticator: login id +
password form; `VITE_DEV_PERSONAS=true` adds the persona picker). The dev authenticator has **no
default password**: start the API with `SUTRA_DEV_AUTH_PASSWORD=<at least 12 characters>` (it
refuses to start otherwise) and type that password on the sign-in page. With `SUTRA_DEV_MFA=true` administrators are asked to enrol an authenticator app
(QR code on `/mfa/enrol`) and then enter a real TOTP code at every sign-in. `VITE_AUTH_MODE=oidc`
pairs with `SUTRA_AUTH_MODE=keycloak` and shows a single sign-on button (`GET /api/v1/auth/oidc/start`).

The API also starts with no database at all for UI work:

```sh
SUTRA_SESSION_STORE=memory SUTRA_IDEMPOTENCY_STORE=memory SUTRA_AUDIT_SINK=memory cargo run -p sutra-api
# /health/ready reports 503 (postgres unreachable) — liveness, auth and catalog still work
```

Whole stack in containers: `docker compose --profile app up --build` (web on :3000) — see
[Local Docker verification](#local-docker-verification) for the exact checks.

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
| GET | `/api/v1/auth/mfa` | second-factor status: TOTP, passkeys, whether WebAuthn is available, session level |
| POST | `/api/v1/auth/mfa/webauthn/registration`, `…/registration/complete` | dev mode: enrol a passkey (step-up required once a factor exists); `501` in Keycloak mode |
| POST | `/api/v1/auth/mfa/webauthn/challenge`, `…/webauthn/verify` | sign-in challenge answered with a passkey (login CSRF required) |
| POST | `/api/v1/auth/mfa/webauthn/assertion`, `…/webauthn/step-up` | passkey step-up of the current session |
| DELETE | `/api/v1/auth/mfa/webauthn/credentials/{id}`, `/api/v1/auth/mfa/totp` | remove a passkey / the authenticator app (step-up) |
| GET/DELETE/POST | `/api/v1/auth/sessions`, `/sessions/{id}`, `/sessions/revoke-others` | own active sessions; revoke one / all others |
| POST | `/api/v1/auth/oidc/backchannel-logout` | Keycloak back-channel logout (`logout_token`, revokes sessions of the IdP `sid`) |
| GET/POST | `/api/v1/notifications[?unreadOnly&cursor&limit]`, `/unread-count`, `/{id}/read`, `/read-all` | own notification inbox |
| GET/POST/PATCH | `/api/v1/notices[/{id}]`, `/notices/{id}/archive` | notices (read: audience; manage: administrators) |
| GET/POST | `/api/v1/todos[?status]`, `/todos/{id}/complete|dismiss|reopen` | to-dos (own, assigned, created from domain events) |
| GET/PATCH | `/api/v1/profile` | own profile + notification preferences (`If-Match`) |
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
| WebAuthn | Port (`WebauthnPort`); implemented in dev mode by Batch 1D (below). In Keycloak mode the endpoints answer `501` and Keycloak handles passkeys via the `webauthn-register` required action. |
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

## Batch 1 slice D — notifications, notices, to-dos, profile/security, DB roles, WebAuthn, back-channel logout

| Concern | Implementation |
|---------|----------------|
| Schema | `migrations/20261014000001_platform_inbox.sql` (`platform.profile`, `notification`, `notification_receipt`, `notification_delivery`, `notice`, `todo`, `event_receipt`), `…000002_platform_webauthn_logout.sql` (`webauthn_credential`, `replay_guard`), `…000003_runtime_db_roles.sql` (per-schema roles). Every query is filtered by the session tenant (and institution where the row has one); other tenants' rows are 404. |
| Notifications | `modules/platform/src/inbox/notifications.rs`: addressed to a subject or to a role (tenant-wide), per-reader receipts, cursor paging, unread count, mark read / all read. Categories `institution · notice · todo · security · system`; per-category in-app / e-mail preferences in the profile. |
| E-mail channel | `sutra_kernel::mail::MailSender` port, SMTP adapter `crates/infra/src/smtp.rs` (lettre, rustls; `SUTRA_NOTIFY_EMAIL=off\|smtp`). Rows queued in `notification_delivery` in the notification's transaction (only for recipients with a contact e-mail and e-mail enabled for the category); `inbox/delivery.rs` sends them from an advisory-locked job with exponential backoff, `failed` after `SUTRA_NOTIFY_MAX_ATTEMPTS`. Mailpit in development. |
| Event consumer | `inbox/consumer.rs`: reads committed `platform.outbox` events by type (independent of the NATS relay), exactly-once per consumer via `platform.event_receipt` in the projection's transaction, savepoint per event (poison → `failed`, never blocks). `institution.registered` → notification to every administrator of the tenant + "Complete the setup of …" to-do for the registering user; `institution.renamed` → administrators; `platform.notice.published` → audience roles from `publishAt`; `platform.todo.assigned` → assignee. |
| Notices | `inbox/notices.rs`: administrators create/update/archive (Idempotency-Key, `If-Match`), audience roles, pinned, scheduled `publishAt`, `expiresAt`; everyone reads the live notices of their role. |
| To-dos | `inbox/todos.rs`: own and assigned to-dos (administrators/faculty may assign by login id), complete / dismiss / reopen with versions, overdue flag, open count. |
| Profile | `inbox/profile.rs`: display name, locale, time zone, contact e-mail/phone, notification preferences; created on first sign-in; `PATCH` with `If-Match`. |
| Security | `auth/http_security.rs`: own sessions (current flagged), revoke one / all others, MFA status, remove TOTP (step-up). |
| Dev-mode WebAuthn | `auth/webauthn.rs` (`DevWebauthn`, webauthn-rs; `SUTRA_WEBAUTHN_RP_ID`, `SUTRA_WEBAUTHN_ORIGINS`): passkey registration, sign-in challenge (`…/webauthn/challenge` → `…/webauthn/verify`, the renewed challenge still accepts TOTP), step-up; ceremony state single use in `platform.pre_auth`. Creation options ask for `residentKey: preferred`, `userVerification: required`, `attestation: none`, no `authenticatorAttachment`, `hints: ["client-device"]` (Windows Hello / Touch ID offered first) and a 2-minute prompt timeout. Sign counter starts at the registration value and advances atomically; authenticators without a counter (always `0`, e.g. synced passkeys) are accepted, a counter that stops advancing is refused. Session `amr` = `pwd, hwk, mfa`. Keycloak mode keeps `501` (Keycloak owns passkeys). See [Passkeys (dev mode)](#passkeys-dev-mode). |
| Back-channel logout | `POST /api/v1/auth/oidc/backchannel-logout`: Keycloak's signed `logout_token` (JWKS, `iss`, `aud`, `iat`, back-channel event, `sid` required, no `nonce`), `jti` single use via `ReplayGuard` (`platform.replay_guard`), revokes every session with that IdP `sid`. Registered on the `sutra-web` client in the dev realm (`backchannel.logout.url` = `http://api:8080/…`, session required). |
| Runtime DB roles | `sutra_<schema>` NOLOGIN roles with DML on their own schema only (+ INSERT on `platform.outbox`/`audit_log`), `sutra_app` member of all; `sutra-api migrate` runs as the owner (`SUTRA_MIGRATION_DATABASE_URL`) and provisions `SUTRA_DB_RUNTIME_USER` as a member of `sutra_app`. The API serves as that role: no DDL, no ownership, audit log append-only. The db-tests run the API as such a role. |
| Sweepers | Read notifications, archived/expired notices, closed to-dos (`SUTRA_*_RETENTION_SECS`), consumer receipts and replay-guard entries (`inbox/jobs.rs`). |
| Web | `/notifications`, `/notices`, `/todos`, `/profile`, `/account/security`; the bell shows the live unread count and the latest notifications; account menu → Profile, Notices, To-dos, Security, Two-step verification; sign-in MFA step offers **Use a passkey** when the account has one; the home **Your to-dos** card shows the first four open to-dos from `GET /api/v1/todos` (overdue / due badges, the checkbox completes the to-do, *See all* → `/todos`) with loading, empty and error states. Mock mode keeps an in-memory inbox per role and the persona's sample to-dos on home. |

## Batch 2 — Institution (Domain B)

Built in three parallel slices on top of `batch2/foundation`; the shared contracts (ids, states,
events, permissions, paths, file ownership, migration ranges) are in
[`docs/batch2-contracts.md`](docs/batch2-contracts.md).

<!-- batch2:2a:begin (slice 2A edits only inside this region) -->
#### 2A — Institution lifecycle, policies, organisation, positions

All under `/api/v1/institution` (tags `institution` and `organisation`); writes are
administrator-only, reads need `institution.read`. Creates need `Idempotency-Key`, updates and
state commands `If-Match` (428 / 412). Business dates are read in the institution's IANA
timezone.

| Area | Endpoints | Rules |
|------|-----------|-------|
| Lifecycle | `POST /institutions/{id}/configure · submit · decision · suspend · reactivate · archive`, `GET …/activation-requests`, `GET /institutions` | `draft → configured → pending_approval → active / returned / rejected`, `returned / rejected → configured`, `active ⇄ suspended`, `→ archived` (terminal). `submit` is refused with 409 while a required activation-readiness item is unmet (detail lists the keys); approval goes through the WorkflowPort (auto-approved in Batch 2, actor `system:auto-approval`). `decision` = Institution Activation Authority (`institution.activation.decide`, step-up), re-validates readiness on approve; `return`/`reject` need a reason. Suspend / reactivate / archive need `institution.lifecycle` (step-up) and a reason. Suspended / archived institutions are read-only (also for every downstream write). |
| Profile & timezone | `PUT /institutions/{id}/profile`, `GET …/profile/revisions`, `PUT …/timezone` | L2-B01 §7 master information (`configure` 422 lists what is missing); every revision keeps previous/new values; timezone must be in PostgreSQL's IANA catalogue. Locked while pending approval. |
| Policies | `POST/GET /institutions/{id}/policies`, `GET /policies/{id}[?asOf=]`, `POST /policies/{id}/revisions · publish`, `GET …/revisions` | Versioned revisions; drafts never govern; publication not retroactive and strictly after the current revision's date; predecessor `superseded` but in force until the successor's date. |
| Type blueprints | `POST/GET /institutions/{id}/org-type-blueprints`, `GET/PUT /org-type-blueprints/{id}`, `POST …/publish · decision` | Unit types + allowed parent types; published versions immutable; publication through the WorkflowPort supersedes the previous version. |
| Organisation units | `POST/GET /institutions/{id}/org-units`, `GET/PATCH /org-units/{id}`, `POST …/activate · suspend · restore · decommission · archive · migrate-type`, `GET …/history` | Tree with materialised path, one live root, placement by the published blueprint; activate needs the effective blueprint version and an active parent; `PATCH` changes name/description only (reparenting = restructure, type = migration); decommission refused while live children or draft/active positions remain; append-only history. |
| Restructure | `POST/GET /institutions/{id}/org-restructures`, `GET /org-restructures/{id}`, `POST …/decision` | `move` / `merge`, cycle and blueprint checks, WorkflowPort approval, stale structure baseline → 409, takes effect today. |
| Governance | `POST/GET /org-units/{id}/governance-configs`, `GET /org-units/{id}/effective-governance[?asOf=]`, `GET/PUT /governance-configs/{id}`, `POST …/activate` | Revisions per unit; activation for active units only, not retroactive, no overlapping authority (predecessor closed the day before). |
| Positions | `POST/GET /institutions/{id}/positions`, `GET/PATCH /positions/{id}[?asOf=]`, `POST …/activate · changes · retire · archive`, `GET …/revisions` | `draft → active → retired → archived`; effective-dated successor revisions (predecessor in force until the day before); never creates an occupant or assignment; codes never reused. |

Activation readiness keys provided by 2A: `institution.profile_complete`,
`institution.timezone_set`, `organisation.root_unit_active` (required),
`institution.policies_effective`, `organisation.governance_active`,
`position.definitions_active` (advisory). Inbox: activation submission → to-do + warning;
activation outcome, suspension, restoration, archival and restructures → administrator
notifications. Tests: `cargo test -p sutra-api --features db-tests --test db_b2a`.
<!-- batch2:2a:end -->
<!-- batch2:2b:begin (slice 2B edits only inside this region) -->
### Slice 2B — campuses, academic structure, programmes, curriculum framework, school model

| Concern | Implementation |
|---------|----------------|
| Schema | `migrations/20261015000010_institution_campus.sql` (`institution.campus`, `campus_revision`), `…000011_institution_curriculum.sql` (`academic_structure`, `academic_structure_node`, `programme`, `curriculum`, `curriculum_version`, `curriculum_component`, `school_academic_model`, `school_grade_level`). Codes unique per institution and never reused; one live main campus, one published + one open draft per lineage (partial unique indexes). Triggers make published / superseded / retired content immutable even to direct SQL (only status bookkeeping may move). |
| Campuses (L3-B-017…022) | `modules/institution/src/campus/`: register (`registered`, code, name, type, address, geo, timezone override, establishment date, declared capacity, contacts, working hours) → `POST …/activate` (address required; WorkflowPort `CAMPUS_ACTIVATION`: `pending_activation`, then `active` under auto-approval, actor `system:auto-approval`) → governed `PATCH` (name, address, geo, timezone, contacts, description, working hours, main flag; code / type / declared capacity immutable; every change snapshotted in `GET …/revisions`) → `suspend` (reason) / `restore` → `decommission` (`campus.decommission` + step-up; reason + `effectiveOn`; WorkflowPort `CAMPUS_DECOMMISSION`; refused while a non-retired programme is offered there) → `archive` (step-up, terminal). `POST …/decision` completes a pending activation / decommissioning when approvals await a human (`ApprovalMode::AwaitDecision`). |
| Academic structure | `curriculum/structure.rs`: versioned tree of `level` / `degree_type` / `stream` / `discipline` nodes with NHEQF levels, model `higher_education` / `school` / `mixed`, NEP flag. Draft → `publish` (WorkflowPort `ACADEMIC_STRUCTURE_PUBLICATION`; `in_review`, then `published` superseding the previous version) → `revise` (successor draft) → `retire` (blocked while programmes are published). |
| Programme catalogue | `curriculum/programme.rs`: code, name, level + degree-type keys, duration in terms, owning org unit (validated against 2A's table when present), offering campuses (live campuses only). `draft → published` (keys must exist in the published structure) `→ retired` (blocked while a curriculum is published). |
| Curriculum framework (L3-B-023…025) | `curriculum/framework.rs`: a curriculum per programme with immutable versions: term structure + count, credit framework (`nep2020` / `cbcs` / `custom`), min total / per-term credit rules, NEP fields (NHEQF level, multiple entry / exit with exit awards, Academic Bank of Credits), cohort applicability, course slots (term, NEP category, credits in half steps, L-T-P hours, basket, course code). Publish (WorkflowPort `CURRICULUM_PUBLICATION`) checks the coherent set (programme published), the credit rules, term count = programme duration and refuses a stale successor; `POST /curricula/{id}/revisions` creates the successor linked to the published version, publishing it supersedes the predecessor in the same transaction. `curriculum-versions/{id}/retire` retires one version; `curricula/{id}/retire` retires the curriculum and, as its explicit cascade, its published version, emitting the canonical `curriculum.retired` only. |
| School academic model (L3-B-026) | `curriculum/school.rs`: board, NEP 5+3+3+4 stage alignment, grade levels with sections and subjects (not a programme). Draft → publish (WorkflowPort `SCHOOL_MODEL_PUBLICATION`, needs a published school / mixed structure) → `revise` successor; a rejected proposal leaves the published model authoritative. |
| Events / inbox | `campus.{registered, activated, modified, suspended, restored, decommissioned, archived}`, `academic_structure.{published, retired}`, `programme.{registered, published, retired}`, `curriculum_version.{published, retired}`, `curriculum.retired`, `school_model.published` (outbox + audit in the owner transaction). `inbox/rules/structure_2b.rs`: `campus.activated`, `campus.decommissioned`, `curriculum_version.published` → administrator notifications. |
| Readiness | `campus/readiness.rs`: `campus.active_campus`, `curriculum.academic_structure_published`, `curriculum.programme_published`, `curriculum.curriculum_published`, `curriculum.school_model_published`; programme / curriculum keys are `not_applicable` for a school-only model, the school-model key unless the model is `school` / `mixed`. |
| Guard + API profile | Writes take `shared::guard::lock_institution_for_change` (allowed in draft / configured / pending_approval / returned / rejected / active, 409 when suspended / archived); creates need `Idempotency-Key`, updates and commands `If-Match` (428 / 412); illegal transitions 409; foreign tenant 404. Tags `campuses` and `curriculum`; operation ids `campus_*`, `academic_structure_*`, `programme_*`, `curriculum_*`, `school_model_*`. DB tests: `apps/api/tests/db_b2b.rs`. |
<!-- batch2:2b:end -->
<!-- batch2:2c:begin (slice 2C edits only inside this region) -->
### Slice 2C — academic calendar, activation readiness, admin screens

| Concern | Implementation |
|---------|----------------|
| Schema | `migrations/20261015000020_institution_academic_calendar.sql`: `academic_calendar`, `academic_year`, `calendar_version` (+ `academic_term`, `calendar_key_date`), `calendar_assignment`, `calendar_exception`, `calendar_simulation`, `academic_year_closure`. Business dates are `date` in the institution business timezone; a published version snapshots it (`calendar_version.timezone`). Partial unique indexes: one open year per calendar, one draft and one published version per calendar + year. Nothing is deleted — retirement, supersession, closure and completion are states. |
| Domain | `modules/institution/src/calendar/domain.rs` — pure, unit-tested transitions (`version_can_transition`, `year_can_transition`, `assignment_can_transition`, `exception_can_transition`, `simulation_can_transition`), definition validation (terms inside the version, no overlap, unique sequence, version inside its year), closure evidence, the change-simulation impact model (instructional days, uncovered assignments, `uncertain` instead of assuming downstream impact). |
| API (`tag calendar`) | `/institutions/{id}/calendars`, `/calendars/{id}` (`/retire`, `/versions`, `/assignments`, `/exceptions`, `/simulations`), `/calendar-versions/{id}` (`PATCH` draft, `/terms`, `/key-dates`, `/publish`, `/decision`, `/revise`, `/retire`), `/institutions/{id}/academic-years`, `/academic-years/{id}` (`/open`, `/close`, `/decision`), `/calendar-assignments/{id}` (`/activate`, `/retire`), `/institutions/{id}/calendar-assignments`, `/calendar-exceptions/{id}` (`/decision`, `/apply`, `/complete`, `/withdraw`), `/calendar-simulations/{id}` (`/decision`), `/institutions/{id}/calendar-resolution?scopeKind&scopeId&on`. Creates need `Idempotency-Key` (201 + `ETag`), commands `If-Match` (428/412); illegal transitions 409; reads `institution.read`, drafts `calendar.manage`, publish/open/close/activate/apply/retire `calendar.publish`. Every write locks the institution (`shared::guard`, read-only while suspended/archived) and writes outbox + audit in the same transaction. |
| Scenarios | B-027 establish → publish (approval step via `WorkflowPort`, auto-approved) → open academic year (needs a published version; creates no assignment). B-028 draft assignment → effective (published version, scope exists in 2A/2B tables, no overlapping effective assignment for the scope). B-029 `revise` = successor draft with copied structure, `supersedesVersionId`; publishing it supersedes the predecessor in the same tx; assignments are not migrated. B-030 exceptions are approved through the approval step, `apply` only inside their period in the institution timezone, `complete` ends them. B-031 closure evaluates evidence (year ended or a governed early-closure reason, no pending exception decisions), completes temporary exceptions and prepares the next year as **planned** + draft version (never opened). B-032 simulations bind a point-in-time baseline, return the impact report, write only the simulation row + audit (no event, no calendar change); the decision records `accepted`/`discarded` and applies nothing. B-033 version / calendar retirement refused while an assignment is effective or the year is open; retired never returns. |
| Events | `calendar_version.published`, `calendar_version.retired` (**added by 2C**, `StatusChangedV1`), `calendar.retired`, `academic_year.opened/closed`, `calendar_assignment.effective/retired`, `calendar_exception.approved/completed`. Inbox (`inbox/rules/calendar_2c.rs`): administrators are notified of publications and year open/close; approved exceptions reach every role. |
| Readiness | `calendar/readiness.rs` returns `calendar.version_published`, `calendar.academic_year_open` (open, or planned with a published version), `calendar.assignment_effective` (institution-level, not ended). `GET /institutions/{id}/activation-readiness` (`institution.readiness.read`, tag `readiness`) serves `shared::readiness::evaluate` over all three slice providers. |
| Web | Admin → **Campus setup** (`/campus/:tab?`): Institution (Register institution, profile, Mark configured / Submit / activation decision / Suspend / Restore / Archive with the shared step-up dialog, rename), Activation readiness (grouped checklist with *Fix* links, submit when ready), Campuses, Organisation (unit tree), Positions, Policies. Admin → **Academics** (`/academics/:tab?`): Academic structure, Programmes, Curriculum (versions, successor revisions), School model, Academic calendar (calendars, years open/close, versions with terms / key dates, publish / revise / retire, assignments, exceptions, simulation preview). Data layer `features/institution/api.ts` (one interface; HTTP with CSRF, `Idempotency-Key`, `If-Match`, `ApiProblem`; every 2A/2B/2C path type-checked against the generated OpenAPI `paths`, all types generated) and `mock.ts` for mock mode. The step-up dialog moved to `components/StepUpDialog.tsx` (shared with Security). |
| Tests | `calendar/domain.rs` + `calendar/readiness.rs` unit tests; `inbox/rules/calendar_2c.rs`; `apps/api/tests/db_b2c.rs` (8 PostgreSQL tests: lifecycle + illegal transitions, invalid/overlapping dates, successor revision, timezone (Pacific/Kiritimati vs Pacific/Pago_Pago), side-effect-free simulation, closure + retirement, tenant isolation / permissions / `If-Match` / idempotency replay / suspended guard, `AwaitDecision` mode); `features/institution/institution.test.tsx` (screens in mock mode + HTTP conventions). |
<!-- batch2:2c:end -->

## Batch 3 — Student lifecycle (Domain C)

Built in three parallel slices on top of `batch3/foundation`; the shared contracts (ids, states,
events, permissions, paths, file ownership, migration ranges) are in
[`docs/batch3-contracts.md`](docs/batch3-contracts.md).

<!-- batch3:3a:begin (slice 3A edits only inside this region) -->
### 3A — Admissions and the student record (L3-C-001 … C-019)

| Concern | Implementation |
|---------|----------------|
| Schema | `migrations/20261020000001_student_admissions.sql`: `admission_application`, `admission` (applicant snapshot, offer expiry, required documents, `process_instance_id`, Finance evidence, `previous_admission_id`), `admission_document`, append-only `admission_status_history`. `migrations/20261020000002_student_record.sql`: `student_number_sequence`, append-only `student_record_history` (before/after of enrolment, assignment, transfer, profile, identity, merge, preferences), `student_suspension`, `communication_preference`, `student_document`, `document_request`, `identity_correction`, `duplicate_case`, `student_transfer`. Partial unique indexes: one admission per application, one open admission per student and institution, one active request per document type, one open transfer per context, one open duplicate case per pair. Domain B ids are plain uuids validated through `InstitutionReferencePort`. |
| Domain | `admission/domain.rs` — application and Admission Record machines (L2-C01 §6.9.1 + expiry amendment: `approval_pending` between verification and approval, rejected approval → `cancelled`, withdrawal only from `confirmed`, revocation from `confirmed` and — policy-controlled — after student creation, `initiated → expired`), documentary verification evaluation. `record/identity.rs` — the governed case machine (`requested → under_review → approved → completed`, `rejected`, `withdrawn`) shared by identity corrections, duplicate cases and transfers. `record/documents.rs` — request machine. All unit-tested. Persona/context status changes go only through `shared::lifecycle`. |
| Admissions API (`tag admissions`) | `/institutions/{id}/admission-applications`, `/admission-applications/{id}` (`PATCH`, `/submit`, `/review`, `/offer`, `/reject`, `/withdraw`); `/institutions/{id}/admissions` (+ `/expire-offers` sweep), `/admissions/{id}` (`/history`, `/documents`, `/documents/{documentId}/verify`, `/accept`, `/submit-documents`, `/submit-for-approval`, `/decision`, `/financial-clearance`, `/confirm`, `/create-student`, `/cancel`, `/withdraw`, `/revoke`, `/expire`, `/readmit`). The offer initiates the Admission Record (`AdmissionInitiated`); the last verified/waived required document completes verification (`DocumentsVerified`); `submit-for-approval` is **the reference BPMN approval** (`student.admission_approval` through the WorkflowPort, deterministic business key, auto-approved in Batch 3, `/decision` for `AwaitDecision`); `financial-clearance` reads `FinanceClearancePort` evidence and records Admissions' own decision (outstanding/pending → 409); `create-student` establishes the Student (or reuses it on readmission) + a `pending_activation` context, activates the persona and files verified documents on the record. |
| Record API (`tag students`) | `/institutions/{id}/students` (directory: `q`, `status`, `programmeId`, `limit`/`cursor`), `/students/{id}` (Student 360 read model: persona + `currentContext` + `contexts`), `/students/{id}/enrol` (context → `active`, persona `activated → enrolled → academically_assigned → active`, admission → `completed`), `PATCH /students/{id}/enrolment` (C-008), `PATCH /students/{id}/assignment` (C-009; a programme change is refused with `programme-migration-required`), `/suspend` + `/restore` + `/suspensions` (C-010/011; a `financial` suspension needs Finance evidence to restore), `PATCH /profile` (C-014; identity members rejected), `/communication-preferences` (C-015), `/archive` (C-019; every context terminal), `/status-history`, `/record-history`, `/duplicate-candidates`, `/documents` (+ `/{documentId}/verify`), `/document-requests`; `/document-requests/{id}` (`/start`, `/issue`, `/reject`, `/cancel`), `/institutions/{id}/document-requests` (queue); `/students/{id}/identity-corrections`, `/identity-corrections/{id}` (`/submit`, `/decision`, `/withdraw`); `/duplicate-cases[/{id}]` (`/submit`, `/decision`, `/withdraw`; merge moves live contexts, documents and requests to the survivor, archives the duplicate with `mergedIntoId`); `/students/{id}/transfers`, `/transfers/{id}` (`/submit`, `/decision`, `/withdraw`; intra = placement move, inter = source context `transferred` + destination `pending_activation` context with `transfer_in` and `predecessorContextId`). Self-service: `/me/student`, `/me/documents`, `/me/document-requests` (+ `/{id}/cancel`), `/me/communication-preferences`. Context commands use the **context's** version for `If-Match`/`ETag` (`currentContext.version`); persona commands the persona's. |
| Conventions | Creates need `Idempotency-Key` (201 + `ETag`), updates / commands `If-Match` (428/412); illegal transitions 409, validation 422, unknown or foreign-tenant ids 404 (problem+json). Every write runs in one owner-local transaction with audit (`student.<aggregate>.<command>`) and outbox events. Step-up permissions: `admission.revoke`, `student.lifecycle` (suspend/restore/inter-institute transfer), `student.identity.manage`, `student.archive`. Revocation, suspension and archival start their process (`ADMISSION_REVOCATION`, `STUDENT_SUSPENSION`, `STUDENT_ARCHIVAL`) for traceability and are decided by the step-up-authorised caller; admission approval, identity correction, duplicate resolution and transfers wait in `AwaitDecision`. |
| Events + inbox | All 3A events of contract §4.3 (no new types). `inbox/rules/student_3a.rs`: approval requested → to-do for the requester + admins notified; confirmed / expired / revoked → admins; student created / enrolled / restored / document issued / rejected → the student (when a login is linked); suspended → student + admins; document request raised → admins (+ to-do for the staff member who raised it). |
| Web | Admin → **Students** (`/students`): directory (search, link to Student 360) and Admissions (applications with submit / review / offer / reject / withdraw, admission records with the next actions per status, documents drawer with verify / reject / waive, approval decision, clearance, confirmation, student creation, cancellation / withdrawal / revocation / readmission, "Expire lapsed offers"). **Student 360** overview (persona, current enrolment, profile, identity correction, suspend / restore, archive, status history), enrolment (enrol, update enrolment, batch/section, transfer, all contexts, transfers, record history) and documents (documents on record + certificate requests with start / issue / reject). Student **Documents** (`/documents`): own documents, request / cancel certificates. Data layer `features/student/record/api.ts` (paths type-checked against the generated OpenAPI) + `mock.ts`. |
| Tests | Unit: `admission/domain.rs`, `record/{support,creation,common,documents,identity}.rs`, `inbox/rules/student_3a.rs`. DB (`apps/api/tests/db_b3a.rs`, 8 PostgreSQL tests): C-001/C-007 full flow with events/audit/history/directory/self-service/roles/tenant isolation/idempotency replay; `AwaitDecision` approval + Finance-evidence rejection; cancellation / withdrawal / expiry (explicit + sweep); revocation → readmission → archival → re-entry; enrolment / assignment / suspension / restoration / profile / preferences; intra + inter-institute transfer (incl. `AwaitDecision`); identity correction (auto + rejected) and duplicate merge; document requests (self + staff, Finance-gated) and record documents. Web: `features/student/record/record.test.tsx`. |
<!-- batch3:3a:end -->
<!-- batch3:3b:begin (slice 3B edits only inside this region) -->
### Slice 3B — registration and progression (L3-C-020 … C-038, C-075)

`modules/student/src/{registration,progression}` (tags `registration`, `progression`), migrations
`20261020000010_student_registration.sql` and `20261020000011_student_progression.sql`, inbox rules
`modules/platform/src/inbox/rules/registration_3b.rs`, DB tests `apps/api/tests/db_b3b.rs`, web
`apps/web/app/features/student/registration/` (Admin → Students registration/progression tabs,
Student 360 registration/progression tabs, student-facing `/registration`).

- **Registration windows** per academic term (`scheduled → open → closed`, `scheduled → cancelled`):
  dates given or taken from the calendar's `registration_window` key date; opening needs a
  published calendar term and today (institution timezone) inside the window. A closed window's
  `registered` registrations are finalised in one call (`/registration-windows/{id}/finalise`).
- **Registration** (`draft → submitted → approved → registered → finalised`, `returned`,
  `cancelled`): staff or self-service (`/me/registrations`); lines are curriculum components
  (`regular`/`elective` of the current term, `backlog` of earlier unearned terms, `repeat` of
  components registered in an earlier finalised registration, `audit`), credit limits from the
  window or the curriculum. Blocked by an active hold, Finance dues (`FinanceClearancePort`),
  an academic break or an inactive context; a detained student registers repeat/backlog only.
  Approval `student.registration_approval` (auto-approved in Batch 3). Add/drop after
  registration while the window is open (`registration.modified`).
- **Progression**: versioned policies (`draft → in_review → effective → superseded | retired`)
  with ratio thresholds; decisions are drafted with evidence from `ResultsPort` or entered by
  hand (`creditsEarned`), the outcome defaulting to the policy recommendation. An effective
  decision moves the context term (`set_context_progress`), appends earned credits to the
  append-only ledger, sets the academic standing and opens probation / detention cases and
  conditions. Revisions after re-evaluation or appeal supersede (never mutate) the decision and
  reverse its ledger entries.
- **Mobility**: credit transfer/recognition (approve → ledger, reject, revise), programme
  migration (published destination programme/curriculum, placement, mapping; blocked by an open
  registration), academic break (`in_effect` → standing `academic_break`) and rejoining.
- **Qualification eligibility** (read by 3C): the newest `student.qualification_eligibility` row
  per context; eligible when the curriculum's `min_total_credits` is met by the ledger and every
  `*_core` component is earned; MEME outcomes at ¼/½/¾ of the minimum. **ABC**: the no-op
  adapter's `NotConfigured` is recorded as `failed` with `failureReason = not_configured`.
<!-- batch3:3b:end -->
<!-- batch3:3c:begin (slice 3C edits only inside this region) -->
### 3C — Completion, exits, guardians, scholarships and alumni (L3-C-039 … C-074)

| Concern | Implementation |
|---------|----------------|
| Schema | `migrations/20261020000020_student_completion_exit.sql` (`qualification_completion`, `conferred_qualification`, `completion_deferral`, `qualification_correction`, `student_exit`, `exit_clearance`), `…021_student_guardian.sql` (`guardian`, `guardian_relationship`, consents, communication authorisations, emergency contacts, financial responsibility, pickup authorisation), `…022_student_scholarship.sql` (definitions, versioned eligibility policies, eligibilities, recalculations, external syncs), `…023_student_alumni.sql` (`alumni_transition`). References to 3B aggregates (`eligibility_id`) are plain uuids (no cross-slice FK). |
| Completion + exits (`tag completion`) | `/students/{id}/qualification-completions`, `/qualification-completions/{id}` (`/eligibility-evaluation`, `/degree-audit`, `/clearances/{clearanceId}/resolve` + `/refresh`, `/decision`, `/award`, `/finalise`, `/complete-exit`, `/defer`, `/resume`, `/cancel`); `/students/{id}/conferred-qualifications`, `/conferred-qualifications/{id}` (`/corrections`, `/corrections/{correctionId}/decision`, `/revoke`); `/students/{id}/exits`, `/institutions/{id}/exits`, `/exits/{id}` (`/clearances`, `/clearances/{clearanceId}/resolve` + `/refresh`, `/decision`, `/complete`, `/cancel`). Eligibility reads 3B's **newest `student.qualification_eligibility` row of the completion's context** (`eligible`, `outcomes`, `total_credits`), else the context's credit ledger, else the evaluator's evidence; the requirement is the curriculum's `min_total_credits` or, for a NEP MEME exit, the policy requirement supplied by the evaluator (curriculum must allow multiple entry/exit). Finance clearance via `FinanceClearancePort`; withdrawal, transfer-out, discontinuation, termination, administrative and deceased closures; context/persona status only through `shared::lifecycle`. |
| Guardians (`tag guardians`) | `/institutions/{id}/guardians`, `/guardians/{id}[/relationships]`, `/students/{id}/guardian-relationships`, `/guardian-relationships/{id}` (`/submit-verification`, `/verification-decision`, `/activate`, `/custody`, `/consents[/{consentId}/withdraw]`, `/communications[/{communicationId}/revoke]`), emergency contacts, financial responsibilities and pickup authorisations per student. |
| Scholarships (`tag scholarships`) | `/institutions/{id}/scholarships`, `/scholarships/{id}` (`/policies`, `/eligibilities`, `/external-syncs`), `/scholarship-policies/{id}` (`/submit`, `/decision`, `/retire`), `/students/{id}/scholarship-eligibilities`, `/scholarship-eligibilities/{id}` (`/resolve-condition`, `/recommendation-decision`, `/reassess`, `/renew`), `/institutions/{id}/scholarship-recalculations`, `/scholarship-recalculations/{id}`. External sync goes through the no-op `ExternalScholarshipPort`; the hand-off to Financial Aid is the `scholarship_eligibility.handed_off` event only. |
| Alumni (`tag alumni`) | `/students/{id}/alumni-transitions`, `/alumni-transitions/{id}` (`/validate-policy`, `/decision`, `/corrections`, `/archive`). |
| Events + inbox | 3C events of contract §4.3. `inbox/rules/completion_3c.rs`: qualification awarded and exit completed → student + admins; exit clearance completed and guardian proposed → admins; scholarship determined / renewed / expired → student. |
| Web | Admin → Students exits / guardians / scholarships tabs and Student 360 guardians / scholarships / completion tabs (`features/student/completion/`, mock + typed API). |
| Tests | Unit tests in `completion/{domain,eligibility}.rs`, guardian / scholarship / alumni domains and `inbox/rules/completion_3c.rs`. DB (`apps/api/tests/db_b3c.rs`, 18 PostgreSQL tests): completion → conferment → exit, MEME exits, withdrawal with clearance, Finance-blocked clearance, other exits, deceased closure, deferral, correction/revocation, guardian lifecycle + consents/responsibilities, scholarship policy/determination/hand-off, conditions/renewal/expiry, external sync + bulk recalculation, alumni, permissions + tenant isolation, `AwaitDecision`, and the 3B → 3C eligibility wiring. Web: `features/student/completion/completion.test.tsx`. |
<!-- batch3:3c:end -->

Batch 3 click-through: `pnpm e2e:student` (needs a fresh migrated DB in `DATABASE_URL`, default `sutra_e2e`; not part of `pnpm -r test`) seeds an active institution through the API, walks admissions → enrolment → Student 360 → registration (add/drop) → progression → guardians → scholarships → completion/alumni → exits as admin, student and faculty in Chromium, and writes numbered full-page screenshots to `$E2E_SHOTS`.

## Batch 4 — Academic delivery (Domain D)

Built in three parallel slices on top of `batch4/foundation`; the shared contracts (ids, states,
events, permissions, ports, tables, routes, web frames) are fixed in
[`docs/batch4-contracts.md`](docs/batch4-contracts.md). Domain D reads Domain B through
`InstitutionReferencePort`, Domain C through `StudentDirectory` / `RegistrationReader` (offering
enrolment gating) / `CreditPostingPort` (recognised flexible-learning credits), and 4B/4C read 4A's
offerings, sections, rosters and faculty through the offering read port `OfferingDirectory`.

<!-- batch4:4a:begin (slice 4A edits only inside this region) -->
### 4A — Course offerings, sections, enrolment and faculty assignment (L3-D-001 … D-019)

| Concern | Implementation |
|---------|----------------|
| Schema | Offerings, sections, faculty assignments and section enrolments live in the foundation tables (`20261025000000`; only 4A writes them, status only through `shared::lifecycle`). `migrations/20261025000001_academic_offering_governance.sql` adds 4A's own aggregates: `offering_revision` (governed revision / replacement / rollback / cancellation / unfreeze requests with baseline + after snapshots, one open request per kind), `offering_reorganisation` (merge / split), `offering_template` + `offering_template_generation` (provenance, one generation per offering), `offering_change_simulation`, `offering_capacity_change`, `offering_quota`, `offering_enrolment_window`, `offering_waitlist_entry` (one live entry per student), `enrolment_restriction` (incl. cross-campus / cross-institution permits), `faculty_assignment_policy` (versioned, one effective) and `faculty_assignment_change` (one open change per assignment). Domain B/C references are plain uuids validated through the ports. |
| Offering API (`tag offerings`) | `/institutions/{id}/offerings` (create with sections, list), `/offerings/{id}` (read, `PATCH`), `/submit` (**WorkflowPort approval**, deterministic business key; `/decision` for `AwaitDecision`), `/publish` → `/open-enrolment` (`/close-enrolment`) → `/activate` → `/suspend` / `/reactivate` → `/complete` / `/archive`, `/readiness`, `/capacity` (+ history), `/quotas`, `/windows`, `/restrictions`, `/waitlist` (+ `/promote`, `/{entryId}/accept`, `/{entryId}/remove`), `/revisions` (revision / replacement / rollback, `/decision`, `/withdraw`), `/cancel` and `/unfreeze` (governed requests), `/freeze`, `/merge`, `/split`, `/reorganisations/{id}/decision`, `/simulations` (+ `/decision`); `/institutions/{id}/offering-templates`, `/offering-templates/{id}` (`/activate`, `/generate` partial or atomic, `/retire`). Sections: `/offerings/{id}/sections`, `/sections/{id}`, `/sections/{id}/enrolments` (enrolment gated on 3B registrations through `RegistrationReader`, capacity / quota / window / restriction checks, overflow → waitlist), `/enrolments/{id}` (+ `/transfer`). |
| Faculty API (`tag faculty`) | `/sections/{id}/faculty-assignments`, `/faculty-assignments/{id}` (`PATCH`, `/submit`, `/decision`), `/faculty-assignments/{id}/changes` (operational change: substitution / reassignment / load change, `/decision`), `/institutions/{id}/faculty-assignment-policies`, `/faculty-assignment-policies/{id}` (`/submit`, `/review`, `/decision`, `/retire`). |
| Self-service | Student: `/me/courses`, `/me/courses/available`, `/me/enrolments` (+ `/{id}/drop`), `/me/waitlist` (+ `/{id}/accept`) — the student is always the session's (`shared::me`). Faculty: `/my/classes`, `/my/students` (sections the login actively teaches). |
| Conventions | Creates need `Idempotency-Key` (201 + `ETag`), commands `If-Match` (428/412); illegal transitions 409, validation 422 (incl. an unknown rollback target), unknown / foreign-tenant ids 404 (problem+json). One owner-local transaction per write with audit (`academic.<aggregate>.<command>`), status history and outbox events (`sutra_contracts::academic` 4A events, no new types). |
| Inbox | `modules/platform/src/inbox/rules/offering_4a.rs`: submission awaiting a decision → to-do for the requester; published / enrolment opened → admins; enrolled / dropped / withdrawn → the student; faculty assignment activated / ended → the faculty member. |
| Web | Admin → Academic delivery → **Offerings** (lifecycle actions per status, sections + rosters, capacity, waitlist, revisions) and **Faculty** (teaching sections, assignments, changes, policies); student **Courses** (`/courses`: my courses, available offerings, enrol / drop, waitlist); faculty **My classes** + **My students**. Data layer `features/academic/offering/{api,hooks,mock,types}.ts`; screens carry `data-slice="4A"`. |
| Tests | DB (`apps/api/tests/db_b4a.rs`, 4 PostgreSQL scenario tests covering L4-D-001 … D-019): offering lifecycle + enrolment gating + waitlist + archive; `AwaitDecision` approval, cancellation and revisions / rollback / replacement; restrictions, templates, simulation, merge / split; faculty assignment, operational changes and policy. Unit tests in `modules/academic/src/offering` + `faculty` and the inbox rule; web `features/academic/offering/offering.test.tsx`; e2e region `4A` in `apps/web/e2e/academic.e2e.mjs` (screenshots `4a-*`). |
<!-- batch4:4a:end -->
<!-- batch4:4b:begin (slice 4B edits only inside this region) -->
### 4B — Timetable, sessions & attendance, academic monitoring, internal assessment (L3-D-020 … D-057)

Module `modules/academic/src/{timetable,attendance,monitoring,assessment}` (4B-wide persistence + HTTP kit in `timetable/kit.rs`), migrations `20261025000010…014`. All paths below are under `/api/v1/academic`.

| Concern | Implementation |
|---------|----------------|
| Timetable (`tag timetable`) | `/institutions/{id}/scheduling-requirements` (+ `/scheduling-requirements/{id}/withdraw`), `/institutions/{id}/teaching-sections`, `/institutions/{id}/timetables`, `/timetables/{id}` → `/generate` (draft slots from the requirements, rooms through the Campus teaching-space stub) → `/validate` (clash checks) → `/submit` → `/decision` (**WorkflowPort approval**) → `/publish` → `/activate`; `/freeze`, `/withdraw`, `/revisions` (governed revision), `/retire`, `/archive`; slots `/timetables/{id}/slots[/{slotId}]`. Views: student `/me/timetable`, faculty `/my/timetable`. |
| Sessions & attendance (`tag attendance`) | `/sections/{id}/sessions` (from a timetable slot or ad hoc), `/sessions/{id}` → `/start` / `/pause` / `/resume` → `/attendance` (marks: present / absent / late / excused, roster = the seated roster at capture time) → `/submit-attendance` → `/complete` → `/finalise-attendance` → `/lock`; `/cancel`, `/abandon`; governed corrections `/sessions/{id}/attendance-corrections`, `/attendance-corrections/{id}[/decision\|/withdraw]`. Policy: `/institutions/{id}/attendance-policies`, `/attendance-policies/{id}/submit\|decision\|retire` (threshold %, minimum sessions; one effective). `/sections/{id}/attendance-summary` (+ `/evaluate` raises `attendance.threshold_breached`); student `/me/attendance`, faculty `/my/attendance`. Faculty act only on sections they actively teach (`shared::faculty::require_teaching`). |
| Syllabus coverage | `/sections/{id}/syllabus-coverage`, `/syllabus-coverage/{id}[/topics\|/submit\|/publish]` → published coverage record. |
| Academic monitoring (`tag monitoring`) | `/institutions/{id}/monitoring-cases`, `/monitoring-cases/{id}` → `/risk-assessments` → `/decision` → `/publish-outcome` → `/submit-review` → `/certify` → `/close`, `/reopen` (a new cycle); governance exceptions `/exceptions[/{exceptionId}/plan\|validate\|resolve\|escalate\|withdraw]`; interventions from approved decisions `/interventions/{interventionId}/trigger\|assign\|start\|complete\|cancel`. Evidence (finalised attendance) is read, never re-owned; a repeated `sourceKey` never creates a second outcome. |
| Internal assessment & coursework (`tag assessment`) | `/sections/{id}/assessment-blueprints`, `/assessment-blueprints/{id}[/publish]`; `/sections/{id}/assessment-windows` (coursework = window kind `assignment`), `/assessment-windows/{id}/open\|extend\|close\|cancel`, `/submissions` (+ `/{submissionId}/mark`), `/moderation` (+ `/{moderationId}/decision`); `/sections/{id}/internal-assessment` (+ `/decision`, `/finalize` → `internal_assessment.finalized`, consumed by Examinations in Batch 6); `/internal-assessment-results/{id}/archive`, governed `/corrections[/{correctionId}/decision]`. Student `/me/assignments` and `/me/assignments/{id}/submission` (always the session's student). |
| Conventions | `Idempotency-Key` on creates, `If-Match` on commands, 409 / 422 / 404 problem+json; one owner-local transaction per write with audit, status history and outbox events (`sutra_contracts::academic::events::delivery`). |
| Inbox | `modules/platform/src/inbox/rules/delivery_4b.rs`: `timetable.published` / `timetable.revised` → students + faculty; `attendance.threshold_breached` → the student + administrators; `assessment_window.opened` → students; `monitoring_intervention.triggered` → to-do for the owner + notification to administrators. |
| Web | Admin → Academic delivery → **Timetable**, **Attendance** (policy + section summary), **Monitoring** (case lifecycle, risk), **Internal assessment**; student **Timetable** (week grid + my attendance) and **Assignments** (submit); faculty **Timetable** and **Attendance** (session → marks → complete → finalise). Data layer `features/academic/delivery/{api,hooks,mock,types}.ts`; screens carry `data-slice="4B"`. |
| Tests | DB (`apps/api/tests/db_b4b.rs`, 5 PostgreSQL scenario tests): timetable generation / publication / revision / retirement; sessions, attendance, thresholds, corrections and syllabus; monitoring case lifecycle and reopening; internal assessment and coursework; approval steps awaiting a decision. Unit tests in the 4B module and inbox rule; web delivery tests; e2e region `4B` in `apps/web/e2e/academic.e2e.mjs` (screenshots `4b-*`). |
<!-- batch4:4b:end -->
<!-- batch4:4c:begin (slice 4C edits only inside this region) -->
**Slice 4C — flexible / external learning, credit recognition, course resources** (`modules/academic/src/{flexible,recognition,resources}`, migrations `20261025000030…032`):

- *Providers* (`/institutions/{id}/learning-providers`, `/learning-providers/{id}/submit|decision|suspend|reactivate|offboard`): onboarding with accreditation evidence + approved modes, `LEARNING_PROVIDER_ONBOARDING`; suspended providers receive no new enrolments.
- *Opportunities* (`/institutions/{id}/learning-opportunities`, `/learning-opportunities/{id}/submit|decision|suspend|reactivate|archive|capacity|allocate|promote-waitlist|resolve-conflicts|enrolments`, `…/archive-session`): open electives on a 4A offering (offering read port) or external courses through a provider; activation through `LEARNING_OPPORTUNITY_APPROVAL`; seat allocation by priority never above governed capacity, waitlist with expiry and revalidation, conflict resolution keeping released allocations as evidence; archival / D06 session archival refused while obligations remain.
- *Policy* (`/institutions/{id}/flexible-learning-policies`): versioned rules (concurrency, credits per term, human review, waitlist validity, withdrawal limit), `FLEXIBLE_LEARNING_POLICY`, predecessors superseded.
- *Enrolments* (`/flexible-enrolments/{id}/decision|withdraw|register|registration-outcome|progress|completion`, `/me/flexible-learning[/requests|/{id}/withdraw|/{id}/certificates|/recognitions]`): request → eligible (policy path or `FLEXIBLE_ENROLMENT_APPROVAL`) → allocated / waitlisted → provider registration (acknowledgement is evidence, idempotent on `evidenceKey`) → in progress → recorded completion (no automatic credit).
- *Recognition* (`/flexible-enrolments/{id}/certificates|credit-recognitions`, `/certificate-verifications/{id}/verify|decision`, `/institutions/{id}/credit-mapping-rules`, `/credit-recognitions/{id}/decision|post`, `/institutions/{id}/reconciliation-runs`, `/reconciliation-runs/{id}/resolve`): certificates accepted / rejected / flagged, mapping rules against Domain B curriculum components, recognition → posting into the Domain C ledger **only** through `CreditPostingPort` (idempotent on the recognition id, receipts in an append-only history), provider record reconciliation (uphold or escalate to an exception case).
- *Exceptions* (`/institutions/{id}/flexible-exceptions`, `/flexible-exceptions/{id}/plan|decision|resolve|reopen-correction|hold|close`): approved plan before correction, holds keep a case open, closure with evidence.
- *Resources* (`/sections/{id}/resources`, `/offerings/{id}/resources`, `/resources/{id}[/publish|/withdraw]`, `/my/resources`, `/me/library`): faculty manage resources of sections they actively teach; students see published resources of their seated sections.
- Web: Admin → Academic delivery → *Flexible learning* and *Credit recognition*; student *Library* (resources + flexible-learning requests); faculty *Resources*. DB tests: `cargo test -p sutra-api --features db-tests --test db_b4c`.
<!-- batch4:4c:end -->

Batch 4 click-through: `pnpm e2e:academic` (fresh migrated DB in `DATABASE_URL`, default `sutra_e2e`) seeds an active institution, two students and an active offering taught by the dev faculty persona, then walks every Academic delivery screen as admin, student and faculty and writes screenshots to `$E2E_SHOTS` (default `/workspace/work/out/b4-shots`).

## Batches 5–7 — Finance (F), Examinations (I), People (E)

> **Approval caveat (all batches):** every governed approval step in these modules is currently completed by
> `system:auto-approval` in every environment, `SUTRA_WORKFLOW=flowable` included (module state hard-codes
> `ApprovalMode::AutoApprove`). See [`docs/readiness/DEFECT_REGISTER.md`](docs/readiness/DEFECT_REGISTER.md) DEF-X-001
> (auto-approval) and DEF-X-002 (no separation of duties) before relying on any "approve" step below.

### Batch 5 — Finance (Domain F)

Slices 5A (charges, adjustments, aid, configuration), 5B (payments, receipts, refunds, receivables) and 5C (ledger,
periods, treasury); contracts and deferrals: [`docs/batch5-contracts.md`](docs/batch5-contracts.md). Money is
`rust_decimal` / `NUMERIC(18,2)` end to end; double entry is enforced by deferred constraint triggers. Online payment
has only the mock and disabled adapters (production default `disabled`; see `docs/security/findings.md` SEC-9D-003 and
SEC-P1-003). Budgets (F-033…F-038) are not delivered. DB tests: `db_b5_foundation`, `db_b5a`, `db_b5b`, `db_b5c`;
click-through: `pnpm e2e:finance`.

### Batch 6 — Examinations (Domain I)

Slices 6·F (foundation, grading engine), 6A (exam setup: policy, sessions, papers, schedule, venues, seating,
invigilation, registration, hall tickets, conduct basics), 6B (marks entry, verification and approval, moderation,
gradebook, malpractice basics) and 6C (result computation SGPA / CGPA, withholding, approval, publication, grade cards,
revaluation, supplementary eligibility, completion, session closure); contracts, scenario coverage and the 6D
deferrals (practical / viva conduct, detailed contingencies): [`docs/batch6-contracts.md`](docs/batch6-contracts.md).
Web: Admin → Examinations, Faculty Assessments, Student Results. DB tests: `db_b6_foundation`, `db_b6a`, `db_b6b`,
`db_b6c`, `db_b6_integration`; click-through: `pnpm e2e:exams`. Known gaps (DEF-I-003…I-007, I-016): conduct,
invigilation and seating publication have no screen; CGPA ignores supplementary attempts; pending malpractice does not
withhold a result.

### Batch 7 — People & workforce (Domain E)

Slices 7·F, 7A (staff records, onboarding, positions, probation, separation, certificates), 7B (leave, attendance days,
workload) and 7C (payroll configuration, runs → Finance posting, payslips, adjustments, retro, final settlement);
contracts: [`docs/batch7-contracts.md`](docs/batch7-contracts.md). DB tests: `db_b7_foundation`, `db_b7a`, `db_b7b`,
`db_b7c`, `db_b7_integration`; click-through: `pnpm e2e:people`. Known gaps: net pay is never disbursed and there are no
statutory outputs (DEF-E-005, E-007); about 66 endpoints have no screen (DEF-E-006).

## Batch 8 — Campus · Services · Resources (Domains G, H, J)

Built in four parallel slices on top of `batch8/foundation`; the shared contracts (ids, states,
events, permissions, ports, foundation tables, routes, web frames) are fixed in
[`docs/batch8-contracts.md`](docs/batch8-contracts.md) and the scope / deferrals in
[`docs/batches.md`](docs/batches.md#batch-8-slices). Three crates: `sutra-campus` (schema `campus`,
`/api/v1/campus`), `sutra-services` (`services`, `/api/v1/services`, read-only projections through
`OperationalMetricsPort`) and `sutra-resources` (`resources`, `/api/v1/resources`). Campus charges
reach Finance only through `CampusChargePort` (students only — staff are recorded but never
billed); vendor invoices and payments only through `ProcurementPostingPort` (Dr 1410 / 5510 — Cr 2310,
Dr 2310 — Cr bank).

<!-- batch8:8a:begin (slice 8A edits only inside this region) -->
### 8A — Infrastructure, rooms, labs, maintenance, service desk
- *Infrastructure* (`/institutions/{id}/infrastructure-nodes`, `/infrastructure-nodes/{id}[/activate|suspend|restore|decommission|archive|capacity|capabilities|availability]`): site → building → floor → space, classification, capacity (refused below a booking's attendees or a hostel room's beds in service), availability profile + blackout dates.
- *Reservations* (`/institutions/{id}/reservations`, `/me/reservations`, `/reservations/{id}/…`): capacity / opening hours / overlap checks, reschedule, cancel, `RESERVATION_APPROVAL`.
- *Service desk + maintenance* (`/institutions/{id}/tickets`, `/me/tickets[/{id}/confirm]`, `/tickets/{id}/triage|assign|start|hold|resolve|close|reopen|cancel|merge|escalate|work-orders`, `/work-orders/{id}/…`, `/institutions/{id}/maintenance-plans`): SLA by priority, escalation, duplicates, requester verification, corrective / preventive work orders. Labs: readiness, practical sessions, batch allocation, equipment status.
- Web: Admin → Campus tabs *Infrastructure*, *Facilities*, *Labs*. DB tests: `db_b8a`.
<!-- batch8:8a:end -->
<!-- batch8:8b:begin (slice 8B edits only inside this region) -->
### 8B — Hostel and transport
- *Hostel* (`/institutions/{id}/hostel-policies|hostel-applications|hostel-beds`, `/hostel-rooms/{id}/beds`, `/hostel-applications/{id}/allocate`, `/hostel-allocations/{id}/…`, `/me/hostel[/application|/complaints]`): `HOSTEL_ADMISSION`, waiting list, `HOSTEL-FEE` / `HOSTEL-DAMAGE` charges, leave, transfer, inspection, mess, vacating → clearance.
- *Transport* (`/institutions/{id}/transport-stops|transport-routes|transport-allocations|vehicles|trips`, `/route-versions/{id}/stops`, `/transport-allocations/{id}/pass`, `/transport-passes/{id}/renew|replace|suspend|restore`, `/me/transport[/request|/complaints]`): route versions + activation, seat capacity, `TRANSPORT-PASS` charges, drivers, trips, breakdowns.
- Web: Admin → Campus tabs *Hostel*, *Transport*. DB tests: `db_b8b`.
<!-- batch8:8b:end -->
<!-- batch8:8c:begin (slice 8C edits only inside this region) -->
### 8C — Library, dashboards and regulatory reporting
- *Library* (`/institutions/{id}/library-policies|library-memberships|catalogue|library-loans|library-fines|library-clearances`, `/catalogue-records/{id}/copies|reservations`, `/library-loans/{id}/return|renew`, `/library-loans/overdue-sweep`, `/library-fines/{id}/bill|waiver`, `/me/library[/…]`): issue / return / renewal, reservations, overdue fines billed as `LIB-FINE`, lost copies (`LIB-LOST`), clearance.
- *Reports* (`/services/institutions/{id}/metrics|widgets|dashboards|reports/library|reports/infrastructure|reporting-frameworks`, `/services/dashboards/{id}/versions|submit|publish|rollback|assignments|view`, `/services/me/dashboards`): governed dashboards over the metrics catalogue and AISHE / NIRF / NAAC style cycles → snapshots → packages → acknowledgement.
- Web: Student *Library* (`MyLibraryPanel`), Admin → Campus tab *Library*, Admin *Reports* (*Dashboards*, *Regulatory*). DB tests: `db_b8c`.
<!-- batch8:8c:end -->
<!-- batch8:8d:begin (slice 8D edits only inside this region) -->
### 8D — Procurement, inventory, assets, Finance AP
- *Procurement* (`/institutions/{id}/suppliers|requisitions|sourcing-events|purchase-orders|vendor-invoices|vendor-payments`, `/purchase-orders/{id}/issue|acknowledge|amend|cancel|close|receipts`, `/goods-receipts/{id}/inspect|accept`, `/vendor-invoices/{id}/match|post`, `/vendor-payments/{id}/post`): RFQ or direct sourcing, three-way match (PO / receipt / invoice) before AP posting.
- *Inventory + assets* (`/institutions/{id}/items|stores|stock-on-hand|stock-issues|stock-adjustments|assets`, `/stock-issues/{id}/decision|issue|return`, `/assets/{id}/assign|return|condition|retire`): append-only stock ledger, reorder level → replenishment need, custody to staff or a space.
- Web: Admin → Campus tabs *Procurement*, *Inventory*, *Assets*. DB tests: `db_b8d`.
<!-- batch8:8d:end -->

Batch 8 integration: `db_b8_integration` (booking → hostel fee + complaint → work order → closure →
transport pass → library fine → the student's Finance dues → PO → receipt → stock → asset → issue → AP
→ payment → metrics + published dashboard). Click-through: `pnpm e2e:campus` (fresh migrated DB in
`DATABASE_URL`) seeds the campus through the API and walks Admin Campus (infrastructure, facilities,
hostel, transport, library, procurement, inventory, assets), Admin Reports and the Student Library,
writing screenshots to `$E2E_SHOTS` (CI artifact `e2e-shots-campus`).

## Passkeys (dev mode)

The browser half lives in `apps/web/app/lib/webauthn.ts`: it unwraps the API's `{ publicKey: … }`
options, uses the browser's `PublicKeyCredential.parseCreationOptionsFromJSON` /
`parseRequestOptionsFromJSON` and `credential.toJSON()` when present (manual base64url conversion
otherwise), refuses to prompt when the page host is not the relying-party id (for example the app
opened as `http://127.0.0.1:3000` while `SUTRA_WEBAUTHN_RP_ID=localhost` — open `http://localhost:3000`),
and passes an `AbortSignal`, so **Add a passkey**, the sign-in **Use a passkey** step and the step-up
dialog all show a **Cancel** button while the browser / Windows Security prompt is open and end
with a plain message on cancel, timeout or rejection (they used to stay on “Waiting for your device…”).

Requirements: the page origin must be listed in `SUTRA_WEBAUTHN_ORIGINS` (compose:
`http://localhost:3000,http://localhost:5173`) and its host must equal `SUTRA_WEBAUTHN_RP_ID`
(`localhost`). On Windows, **Windows Hello** is offered only when a PIN, fingerprint or face is set
up in *Settings → Accounts → Sign-in options*; without it Windows Security lists only
“iPhone, iPad, or Android device” and “Security key”.

### Passkey end-to-end check (Playwright)

`pnpm e2e:passkey` (not part of `pnpm -r test`) drives Chromium with a CDP virtual authenticator
(CTAP2, internal, resident keys, user verification — what Chrome DevTools → *WebAuthn* offers) through:
sign in as Administrator → *Security* → name + **Add a passkey** → listed; an unanswered prompt →
**Cancel** → idle with a message; password-only session → **Remove** → step-up dialog →
**Use a passkey**; then restarts the API with `SUTRA_DEV_MFA=true` → sign out → sign in →
**Use a passkey** (Cancel once, then succeed) → signed in with `mfaLevel = mfa`. Unexpected console
errors, page errors and failed `/api` calls fail the run. It starts the API binary and the web app
itself; it needs a migrated PostgreSQL and `psql` (to clear the administrator's factors before and after):

```sh
cargo build -p sutra-api && cargo run -p sutra-api -- migrate        # DATABASE_URL as in .env
pnpm --filter @sutra/web exec playwright install --with-deps chromium  # once
pnpm e2e:passkey                                                     # web: Vite dev server on :5173
VITE_AUTH_MODE=api VITE_DEV_PERSONAS=true pnpm --filter @sutra/web build && E2E_WEB=preview pnpm e2e:passkey   # built SPA on :3000
```

Options: `SUTRA_API_BIN` (default `$CARGO_TARGET_DIR/debug/sutra-api`), `CHROMIUM_PATH` (use an installed
Chrome/Chromium instead of Playwright's), `E2E_HEADED=1`, `E2E_API_PORT` (default 8080, must be free).

## Local Docker verification

Docker is not available in the CI sandbox that produced Batch 1, so the container stack is verified by
reading the Dockerfiles/compose file; run these on your own machine (Docker 24+ with Compose v2). From
`sutraerp/`:

```sh
# 1. Build and start everything: postgres 18, keycloak 26, valkey, nats (JetStream), mailpit,
#    migrate (as the schema owner, provisions the runtime role) → api (runtime role) → web.
docker compose --profile app up --build -d

# 2. Migrations ran as the owner and exited 0; the API runs as `sutra_api`.
docker compose ps -a migrate                      # STATUS: Exited (0)
docker compose logs migrate | grep -E "migrations applied|runtime login role provisioned"
docker compose exec postgres psql -U sutra -d sutra -c \
  "SELECT usename, application_name FROM pg_stat_activity WHERE datname = 'sutra' AND usename = 'sutra_api' LIMIT 1;"
#   → one row: sutra_api

# (re-run migrations at any time — idempotent)
docker compose --profile app run --rm migrate

# 3. Health
curl -s http://localhost:8080/health/live         # {"status":"ok"}
curl -s http://localhost:8080/health/ready        # HTTP 200, status "ok", checks postgres/nats/valkey all ok:true
curl -s http://localhost:8180/realms/sutra/.well-known/openid-configuration | head -c 200   # Keycloak realm
curl -s -o /dev/null -w "%{http_code}\n" http://localhost:8025/                              # Mailpit UI → 200
curl -s -o /dev/null -w "%{http_code}\n" http://localhost:3000/                              # web → 200
curl -s http://localhost:3000/api/v1/auth/csrf    # web → API proxy: {"csrfToken":"…","expiresAt":"…"}
```

Expected: `/health/ready` returns **200** with `"status":"ok"` and `postgres` (critical), `nats` and
`valkey` checks `ok: true`; Keycloak serves the `sutra` realm on http://localhost:8180 (admin/admin);
Mailpit UI on http://localhost:8025; the web app on http://localhost:3000 (API mode, dev sign-in).

Keycloak (SSO) mode for the logout check — rebuild the API and web with:

```sh
SUTRA_COMPOSE_AUTH_MODE=keycloak SUTRA_COMPOSE_WEB_AUTH_MODE=oidc docker compose --profile app up --build -d
```

Stop with `docker compose --profile app down` (add `-v` to wipe the database and JetStream volumes).

**Restarts.** postgres, keycloak, valkey, nats, mailpit, api and web have `restart: unless-stopped`:
after a Docker Desktop or PC restart the stack comes back by itself (Docker starts the containers
directly, without compose's `depends_on` ordering — the API tolerates a database that is still
starting and reports `/health/ready` 503 until it is up). `migrate` is a one-shot job without a restart
policy; re-run it after pulling new migrations. `docker compose --profile app stop` (or `down`) keeps
the stack stopped across restarts.

### Click-through checklist (1C / 1D)

Dev sign-in (default compose, http://localhost:3000, password `sutra` is built in):

1. **Sign in** — choose *Administrator* (Dhruv Naik). The bell shows the unread count from
   `GET /api/v1/notifications/unread-count`.
2. **Profile edit** — account menu → *My profile*: set contact e-mail `admin@sutra.dev`, turn **e-mail on**
   for *Institution*, *Save changes*; a reload keeps the values (a stale second tab gets a conflict message).
3. **Register an institution** — *Campus setup* → *Institution* → **Register institution** (code `SIT`, name
   `Sutra Institute of Technology`, timezone `Asia/Kolkata`) → *Register*.
4. **Notification + setup to-do** — within a few seconds (consumer polls every 2 s) the bell shows
   "Institution registered …"; *To-dos* lists "Complete the setup of Sutra Institute of Technology".
   The outbox event also reaches JetStream: `curl -s localhost:8222/jsz?streams=1` shows `SUTRA_EVENTS` with messages.
5. **E-mail in Mailpit** — http://localhost:8025 shows the institution notification e-mail to `admin@sutra.dev`
   (delivery job runs every 5 s).
6. **Post a notice** — account menu → *Notices* → *New notice*, audience *Students*, publish now. Switch role to
   *Student* (account menu): the notice is on the Notices screen and in the student's bell.
7. **Security sessions revoke** — sign in as Administrator in a second browser (or private window); in the first,
   account menu → *Security* lists two sessions → *Sign out* on the other one (or *Sign out others*); the second
   browser is sent to sign-in on its next request.
8. **Passkey enrol** — open the app at **http://localhost:3000** (not 127.0.0.1). *Security* → *Add a passkey*
   (name it) → the browser/OS passkey prompt. On Windows choose **Windows Hello** (needs a PIN or
   biometric in *Settings → Accounts → Sign-in options*); a phone or security key works too, and Chrome
   DevTools → *WebAuthn* → *Enable virtual authenticator environment* (ctap2, internal, resident keys,
   user verification) replaces the OS prompt. **Cancel** next to “Waiting for your device…” closes the
   prompt. The passkey is listed. Then require a second factor for administrators and sign in again —
   the second-factor step offers **Use a passkey**:
   ```sh
   SUTRA_COMPOSE_DEV_MFA=true docker compose --profile app up -d api
   ```
9. **Keycloak logout revokes the session** — start in Keycloak mode (command above), sign in at
   http://localhost:3000 via *Continue with SutraERP SSO* as `meera.shah@sutra.dev` / `sutra`. In the Keycloak
   admin console (http://localhost:8180/admin, admin/admin) → realm *sutra* → *Sessions* → sign out Meera's
   session (or *Users → meera.shah → Sessions → Sign out*). Keycloak POSTs the logout token to
   `http://api:8080/api/v1/auth/oidc/backchannel-logout` (`docker compose logs api | grep -i backchannel`);
   the next click in the SutraERP tab returns to sign-in (`GET /api/v1/auth/session` → 401).

### Click-through checklist (Batch 2)

<!-- batch2:2c-docker:begin -->
#### Batch 2 Docker click-through (Windows / PowerShell)

Covers 2A + 2B + 2C end to end. From `sutraerp\` in PowerShell (Docker Desktop running). The
`migrate` image embeds `migrations/` at build time, so `--build` is what picks up the Batch 2
migrations (`20261015000000` … `20261015000020`):

```powershell
# 1. Rebuild and start the stack; migrate runs as a one-shot job before the API.
docker compose --profile app up --build -d
docker compose ps -a migrate                 # STATUS: Exited (0)
docker compose logs migrate | Select-String "migrations applied"
# (re-run at any time — idempotent)
docker compose --profile app run --rm migrate

# 2. Schema check: the Batch 2 migrations and tables exist.
docker compose exec postgres psql -U sutra -d sutra -c "SELECT version, description FROM _sqlx_migrations WHERE version >= 20261015000000 ORDER BY version;"
#   → 8 rows: 20261015000000 … 000004 (foundation + 2A), 000010 / 000011 (2B), 000020 (2C)
docker compose exec postgres psql -U sutra -d sutra -c "\dt institution.*"
#   → institution, org_unit, org_type_blueprint, position_definition, campus, academic_structure, programme,
#     curriculum_version, academic_calendar, calendar_version, academic_year, calendar_assignment, …
curl.exe -s http://localhost:8080/health/ready   # HTTP 200, "status":"ok"
```

Open **http://localhost:3000** and sign in as **Administrator** (Dhruv Naik). Then:

1. **Register institution** — navigation *Campus* (Campus setup) → *Institution* → **Register institution**:
   code `SIT`, name `Sutra Institute of Technology`, timezone `Asia/Kolkata` → *Register*. Status **Draft**.
2. **Profile → configured** — **Mark configured** first: refused with "… missing: shortName, institutionType, …"
   (422). **Edit profile**: short name `SIT`, type `college`, registration number `REG-001`, address
   `1 Campus Road, Pune`, phone `+91 20 1234 5678`, official e-mail `office@sit.edu`, administrator e-mail
   `admin@sit.edu`, pattern `semester`, working days `mon, tue, wed, thu, fri` → *Save profile* (Profile shows
   "revision 1") → **Mark configured** → status **Configured**.
3. **Organisation** — *Organisation* → *Unit types*: keep `university, faculty, department` → **Publish unit types**
   (shows "Blueprint v1"). **Add unit**: parent *— root unit —*, type `university`, code `SIT`, name
   `Sutra Institute of Technology` → *Add* → **Activate**. **Add unit** again: parent *Sutra Institute of Technology*,
   type `department`, code `CSE`, name `Computer Science` → *Add* (indented under the root).
4. **Campus** — *Campuses* → **Register campus**: code `MAIN`, name `Main campus`, address `1 University Road`,
   `Pune`, `Maharashtra`, PIN `411007`, *Main campus* ticked → *Register* → **Activate** (status *Active*).
5. **Structure, programme, curriculum** — navigation *Academics* → *Academic structure*: keep the levels
   `ug:Undergraduate:6, pg:Postgraduate:6.5` (key : name : NHEQF level) → *Create draft structure* → **Publish** (status *Published*).
   *Programmes* → **Register programme**: `BTECH-CSE`, `B.Tech Computer Science`, level *Undergraduate (ug)*,
   `4` terms, owning unit *Computer Science* → *Register* → **Publish**. *Curriculum* → programme
   *B.Tech Computer Science* → *Add curriculum*: code `CSE-2026`, title `CSE scheme 2026`, effective from
   `2026-07-01`, course slots `1:CS101:20, 2:CS201:20, 3:CS301:20, 4:CS401:20` → *Add curriculum* (version 1,
   *Draft*) → **Publish** (*Published*). **Revise** (what changes `BoS 2027`, effective `2027-07-01`) creates
   version 2 as a draft successor; publishing it later supersedes version 1.
6. **Calendar** — *Academics* → *Academic calendar*: *Create calendar* (`MAIN`, `Main academic calendar`) →
   **Plan academic year** (`2026-27`, `2026-06-01` → `2027-05-31`) → *Create draft version* → add terms
   (`T1` Semester 1 `2026-06-15`–`2026-11-30`, `T2` Semester 2 `2026-12-01`–`2027-04-30`) and a key date
   (holiday *Independence Day* `2026-08-15`) → **Publish** (*Published*, "dates in Asia/Kolkata") → **Open year**
   (*Open*) → *Create assignment* with scope *Whole institution*, effective from `2026-06-01` → **Make effective**
   (*Effective · governing*). Optional: a second assignment with scope *Campus · Main campus* shows that campus
   scopes come from the real campus register; *Change simulation* → proposed end `2026-10-31` → *Run simulation*
   shows **Unfavourable** and changes nothing.
7. **Readiness all green** — *Campus setup* → *Activation readiness* → *Refresh*: every **required** item is *Met*
   (profile, timezone, root unit, active campus, structure / programme / curriculum published, calendar version
   published, year open, institution assignment effective); *School academic model* is *Not applicable*;
   advisory items (policies, governance, positions) may stay open. Header: "Every required item is met".
8. **Submit / activate** — *Institution* → **Submit for activation**. With the Batch 2 auto-approval (no
   activation authority configured) the institution goes straight to **Active**, and the bell shows the
   activation notification. Submitting before step 7 is green is refused with "activation readiness incomplete: …".
9. **Suspend / restore** — **Suspend** (reason `Audit`) → *Suspended*; every setup screen now refuses changes
   (e.g. *Register campus* → "… is suspended …"). **Restore** (reason `Audit closed`) → *Active* again. To see the
   step-up dialog, enforce MFA for sensitive admin actions and repeat:
   ```powershell
   $env:SUTRA_COMPOSE_DEV_MFA = "true"; docker compose --profile app up -d api
   ```
   *Suspend* then opens **Verify it’s you** (passkey or authenticator code) and completes after verification.
   Campus **Decommission** asks for a reason and effective date, and is refused while a programme is offered at the
   campus or a campus-scoped calendar assignment still governs it.
10. **Evidence in the database**:
   ```powershell
   docker compose exec postgres psql -U sutra -d sutra -c "SELECT event_type, created_at FROM platform.outbox WHERE event_type LIKE 'sutraerp.institution.%' ORDER BY created_at DESC LIMIT 20;"
   docker compose exec postgres psql -U sutra -d sutra -c "SELECT action, actor_id FROM platform.audit_log WHERE action LIKE 'institution.%' ORDER BY occurred_at DESC LIMIT 20;"
   ```
   → `institution.configured`, `org_type_blueprint.published`, `org_unit.activated`, `campus.activated`,
   `academic_structure.published`, `programme.published`, `curriculum_version.published`,
   `calendar_version.published`, `academic_year.opened`, `calendar_assignment.effective`,
   `institution.submitted_for_approval`, `institution.activated`, `institution.suspended`,
   `institution.reactivated`, with matching audit actions (approval steps by `system:auto-approval`).

Mock mode (`VITE_AUTH_MODE` unset, `pnpm dev`) runs the same screens against an in-memory store.
<!-- batch2:2c-docker:end -->

### Batch 3 + 4 Docker click-through (Windows / PowerShell)

Student lifecycle (Batch 3) and academic delivery (Batch 4) need an **active** institution with
students, and Batch 4 also needs a course offering with a section, a teacher and seated students.
The demo seed makes all of that in a **new** institution, `DEMO` "Sutra Demo College". It does not
change any institution you already have (for example a half-set-up `SIT` from the Batch 2 walk).
It runs in a small container (Node + psql), so you don't need Rust, Node or psql on Windows. From
`sutraerp\` in PowerShell, with Docker Desktop running:

```powershell
# 1. Pull, rebuild and start (migrate embeds migrations/ at build time, so --build matters).
git pull
docker compose --profile app up --build -d
docker compose ps -a migrate                 # STATUS: Exited (0)

# 2. Migrations check: 9 Batch 3 rows (20261020000000 … 000023) + 10 Batch 4 rows (20261025000000 … 000032).
docker compose exec postgres psql -U sutra -d sutra -c "SELECT version, description FROM _sqlx_migrations WHERE version >= 20261020000000 ORDER BY version;"
#   Batch 3: 20261020000000 foundation, 000001 admissions, 000002 record, 000010 registration, 000011 progression,
#            000020 completion/exit, 000021 guardian, 000022 scholarship, 000023 alumni
#   Batch 4: 20261025000000 foundation, 000001 offering governance, 000010 timetable, 000011 sessions/attendance,
#            000012 syllabus, 000013 monitoring, 000014 assessment, 000030 flexible learning,
#            000031 credit recognition, 000032 course resources
curl.exe -s http://localhost:8080/health/ready  # HTTP 200, "status":"ok"

# 3. Seed the demo data (ONE command; safe to re-run: a second run only adds what is missing).
docker compose --profile demo run --rm --build demo-seed
#   → "Demo data ready — institution DEMO “Sutra Demo College” …"
```

What the seed creates:

| | |
|---|---|
| Institution | `DEMO` **Sutra Demo College**, **Active**. It is taken through the same Batch 2 API calls as the e2e harness: profile, organisation, main campus, structure, programme `BTECH-CSE`, curriculum `CSE-2026` (CS/MD/EL slots, terms 1–4) and calendar `2026-27` (T1 15 Jun–30 Nov, T2 1 Dec–30 Apr). |
| Students | `DEMO-0001` **Aarav Desai** (term 1, linked to the **Student** sign-in), `DEMO-0002` **Isha Kulkarni** (term 1), `DEMO-0003` **Kiran Joshi** (term 4, for completion/alumni), `DEMO-0004` **Rohan Patil** (term 4, for an exit) |
| Delivery | Offering **CS101-T1** (Programming 1, 12 cr, *Active*), section **A** taught by **Dr. Meera Shah** (`dev|meera.shah@sutra.dev`, primary instructor), with Aarav and Isha seated |

If it stops with "institution DEMO already exists with status …", an earlier run stopped half-way
and nothing was changed. Use another code:
`$env:DEMO_CODE = "DEMO2"; docker compose --profile demo run --rm demo-seed` (students are then `DEMO2-0001` …).

Open **http://localhost:3000** and use the development role cards on the sign-in page (password `sutra`
is built in): **Administrator** (`admin@sutra.dev`), **Student** (`aarav.desai@sutra.dev` = Aarav
Desai) and **Faculty** (`meera.shah@sutra.dev` = Dr. Meera Shah). To switch role, go to account menu →
*Security* → **Sign out of this device**, then pick another card. On every admin screen, choose
**Sutra Demo College (DEMO)** in the **Institution** picker in the header once. The browser remembers
the choice.

**Batch 3: student lifecycle** (Administrator unless noted)

1. **Admissions** — *Students* → *Admissions* → **New application**: `Kabir Mehta`, date of birth
   `2007-03-14`, e-mail `kabir@example.in`. Leave the login field empty, because the Student login
   already belongs to Aarav. On the row, **Submit** → **Start review** → **Offer admission** (offer
   expires `2026-12-31`, required documents `identity_proof, qualifying_marksheet`). A second
   application (`Kavya Joshi`) → **Submit** → **Reject** with a reason shows the rejection path.
2. **Acceptance and documents** — in *Admission records*: **Record acceptance** → **Documents** →
   **Add document** twice (`identity_proof` "Aadhaar card", `qualifying_marksheet` "HSC marksheet
   2025", any file ref such as `dms://admissions/id.pdf`) → close → **Documents submitted** →
   **Documents** → **Verify** on each. The record reads *Verification completed*.
3. **Approval to student** — **Send for approval** (auto-approved: *Approved*) → **Financial clearance** →
   **Confirm** (*Confirmed*) → **Create student**. The name becomes a link to Student 360.
4. **Student 360** — open **Kabir Mehta** → *Enrolment* → **Complete enrolment** (roll `CSE-26-001`, batch
   `2026`, section `A`). *Overview* → **Edit profile** (preferred name). *Documents* → **Raise request** (bonafide
   certificate, purpose `Bank account opening`) → **Start** → **Issue** (reference `BON/2026/0001`). *Students* lists Kabir next to
   the four seeded students.
5. **Registration window and progression policy** — *Students* → *Registration* → **Schedule window**
   (term **T1**, name `Term 1 registration`, closes `2026-11-15`, credits `16`–`28`) → **Open**.
   *Progression* → **Define policy** (`UG`, `UG progression rules`) → **Make effective**.
6. **Student self-registration, add and drop** — sign in as **Student** (Aarav) → *Registration*
   (`/registration`): tick **CS101** and **MD101** → **Start registration** → **Add component** `EL101`
   → **Drop** on EL101 → **Submit** (*Registered*, 20 cr). *Documents* (`/documents`): **Request a
   document** (transcript copy), then **Cancel** it.
7. **Finalise and progress** — as Administrator: *Students* → *Registration* → **Close** → **Finalise
   registrations**. Aarav's Student 360 → *Registration* shows *Finalised*. *Progression & credits* →
   **Record decision** (term T1, credits `20`/`20`) → **Make effective** → **Evaluate** (qualification
   eligibility).
8. **Guardians and scholarships** — *Students* → *Guardians* → **Register guardian** (`Sunita Desai`,
   `+91 98220 44556`). Aarav → *Guardians* → **Link guardian** (Sunita Desai, mother, primary) →
   **Submit verification** (document, `Birth certificate BC-2007-1123`) → **Approve verification** (if
   shown) → **Activate**. Under emergency contacts and pickups, **Add** one of each. *Students* →
   *Scholarships* → **New scholarship** (`MERIT-2026`, fee percentage `50`, valid from `2026-06-01`) →
   **New policy** (min score `75`, max income `800000`) → **Publish** → **Activate**. Aarav →
   *Scholarships* → **Determine** (Merit scholarship 2026, score `88`, income `600000`) → **Recommend**
   → **Hand off** (*Active*).
9. **Completion and alumni (Kiran Joshi)** — Kiran's Student 360 → *Completion & exit* → **Start completion**
   → **Eligibility evaluation** (credits `96`) → **Degree audit** → resolve each clearance item → **Decision**
   (if shown) → **Award** → **Finalise** → **Complete exit**. Under alumni, **Initiate** and step through
   (**Validate policy** with graduation year `2026`).
10. **Exit (Rohan Patil)** — Rohan → *Completion & exit* → **Initiate exit** (Withdrawal, reason `Relocating`).
    *Students* → *Completion & exits*: resolve the clearances (*Cleared*) → **Approve** (if shown) →
    **Complete**. Rohan's status changes. Signed in as **Faculty**, a Student 360 is read-only.

**Batch 4: academic delivery** (Administrator → *Delivery*, `/academic-delivery`)

11. **Offerings and allocation** — *Offerings*: **CS101-T1** is *Active*. **Sections** → **Roster** on section A
    lists Aarav and Isha. **New offering** (term T1, component `MD101`, code `MD101-T1`, section `A`, `40`) →
    **Submit for approval** → **Publish**. *Faculty assignment* → **Assign faculty** on `MD101-T1 · A`
    (`dev|meera.shah@sutra.dev`, Dr. Meera Shah, primary instructor, `3` h) → **Submit** → **Activate**. Back in
    *Offerings*, **Open enrolment** on MD101-T1.
12. **Timetable** — *Timetable*: **Set requirement** on CS101-T1 (`3` sessions × `60` min, room `R101`) →
    **New timetable** (`Term 1 timetable`) → **Generate** → **Validate** → **Submit for approval** →
    **Publish** → **Activate**.
13. **Attendance policy, monitoring, assessments** — *Attendance* → **New policy** (`75` %, min `1` session) →
    **Submit for approval** (*Effective*). *Monitoring* → **Open case** (section CS101-T1/A, `CS101 attendance
    watch`) → **Assess risk** → **Publish outcome** → **Governance review** → **Certify** → **Close**.
    *Internal assessment* → **New blueprint** (`CS101 internal`, `50` marks, assignments `40` %) → **Publish** →
    **New window** (`a1`, `Essay on recursion`, `10` marks) → **Open**.
14. **Flexible learning and credit recognition** — *Flexible learning*: **New version** (policy) → **Publish** →
    **Onboard provider** (`NPTEL`, government platform, mode nptel, accreditation `MoE/2026/17`) → **Submit for
    onboarding** → **New opportunity** (`NPTEL-ML`, Machine Learning (NPTEL), mode nptel, `4` cr, `20` h,
    capacity `2`, enrolment opens today) → **Activate**. Sign in as **Student** → *Library* → **Request** on
    Machine Learning (NPTEL) (*Eligible*). Back as Administrator: **Allocate seats** → **Demand** → on Aarav's
    row **Register** / **Provider reply** (accepted, `NPTEL-0001`) → **Record completion** (`25` h, passed) →
    **Add certificate** (`NPTEL/CERT/0001`). *Credit recognition* → **Verify** the certificate → **New mapping**
    (NPTEL-ML, `4` cr) → **Publish**. *Flexible learning* → **Demand** → **Request credit**; *Credit
    recognition* → **Post to credit record** (*Posted*) → **Reconcile**.
15. **Faculty (Dr. Meera Shah)** — sign in as **Faculty**: *My classes* (`/classes`) shows CS101-T1/A (and
    MD101-T1/A), *My students* lists Aarav and Isha, *Timetable* shows the 3 weekly slots. *Attendance* →
    **New session** (topic `Recursion`) → open it → **Start session** → mark *present* / *absent* →
    **Save marks** → **Complete session** → **Finalise attendance**. *Resources* → **Add resource** →
    **Publish**.
16. **Student (Aarav)** — *Courses* (`/courses`) shows CS101-T1 with Dr. Meera Shah, and MD101-T1 as "Not on
    your registration" unless it was registered in step 6. *Timetable* shows the weekly classes and the
    finalised attendance. *Assignments* → **Submit** on "Essay on recursion". *Library* lists the published
    resources.
17. **Evidence in the database**:
    ```powershell
    docker compose exec postgres psql -U sutra -d sutra -c "SELECT event_type, count(*) FROM platform.outbox WHERE event_type LIKE 'sutraerp.student.%' OR event_type LIKE 'sutraerp.academic.%' GROUP BY 1 ORDER BY 1;"
    docker compose exec postgres psql -U sutra -d sutra -c "SELECT action, count(*) FROM platform.audit_log WHERE action LIKE 'student.%' OR action LIKE 'academic.%' GROUP BY 1 ORDER BY 1;"
    docker compose exec postgres psql -U sutra -d sutra -c "SELECT code, name, status FROM institution.institution ORDER BY code;"
    #   → DEMO active; your other institutions are unchanged
    ```
    You should see student events (`sutraerp.student.admission…`, `…registration.submitted`,
    `…registration_window.opened` and so on) and academic events (`sutraerp.academic.course_offering…`,
    timetable, attendance and so on), with matching audit actions (`student.registration.create`,
    `student.registration.add_line`, `…drop_line`, `academic.course_offering.create` and so on).

The seed's own rows (students, CS101-T1, its section, teacher and seats) are written directly into the
foundation tables, like the e2e suites do, so they create no outbox or audit rows. Only the
institution set-up goes through the API.
