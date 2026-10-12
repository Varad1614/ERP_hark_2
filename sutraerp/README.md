# SutraERP

Production-grade education ERP — **Batch 0: Foundation** + **Batch 1: Platform** (complete: slices A–D —
durable sessions, CSRF, authorization policy, session-derived tenant scope; Keycloak OIDC adapter, TOTP MFA +
step-up, login CSRF, cross-site Origin checks; NATS JetStream outbox relay, advisory-locked sweepers, Valkey
cache; notifications + e-mail channel, notices, to-dos, profile and security screens, per-schema runtime DB
roles, dev-mode WebAuthn passkeys, Keycloak back-channel logout).

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
VITE_AUTH_MODE=api pnpm --filter @sutra/web build && E2E_WEB=preview pnpm e2e:passkey   # built SPA on :3000
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
3. **Register an institution** — `POST` through the API (no screen until Batch 2), from a signed-in browser
   console on http://localhost:3000:
   ```js
   const s = await (await fetch("/api/v1/auth/session")).json();
   await fetch("/api/v1/institution/institutions", { method: "POST",
     headers: { "Content-Type": "application/json", "X-CSRF-Token": s.csrfToken, "Idempotency-Key": crypto.randomUUID() },
     body: JSON.stringify({ code: "SIT-" + Date.now() % 100000, name: "Sutra Institute of Technology", timezone: "Asia/Kolkata" }) }).then(r => r.status) // 201
   ```
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
