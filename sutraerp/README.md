# SutraERP

Production-grade education ERP — **Batch 0: Foundation** + **Batch 1 slice A** (durable sessions, CSRF,
authorization policy, session-derived tenant scope).

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
│   ├── contracts/             public inter-module ports + DTOs (the ONLY way modules talk)
│   ├── web/                   Axum layer: RFC 9457 problems · request id · session auth + CSRF middleware
│   │                          · Authenticated/Scope extractors · Idempotency-Key middleware · /health/live|ready
│   └── persistence/           SQLx pool (UTC) · embedded migrations · outbox writer · audit sink
│                              · PostgreSQL idempotency store · DB readiness
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
├── docker-compose.yml         postgres 18 · keycloak 26 · valkey · mailpit (+ api/web profile)
└── .github/workflows/ci.yml   fmt · clippy -D warnings · tests · PG18 integration · web · OpenAPI drift
```

**Boundary rules** (enforced by `cargo test`, see `apps/api/tests/architecture.rs`):

- a module crate depends only on `sutra-kernel`, `sutra-contracts`, `sutra-web`, `sutra-persistence`;
- `kernel` and `contracts` never depend on Axum/SQLx/Tower;
- no module SQL references another module's schema;
- every module crate is registered in the composition root.

## Running locally

Prerequisites: Rust stable (see `rust-toolchain.toml`), Node 22, pnpm (`corepack enable`), Docker.

```sh
make setup        # pnpm install + .env from .env.example
make up           # postgres 18, keycloak 26, valkey, mailpit
make migrate      # apply migrations (separate step, ADR-L5-0008)
make api          # http://localhost:8080   (OpenAPI: /api/openapi.json)
make web          # http://localhost:5173   (proxies /api to :8080)
```

The web app runs without the API by default (`VITE_AUTH_MODE=mock`): pick a role on the sign-in
page. Set `VITE_AUTH_MODE=api` to sign in through `POST /api/v1/auth/login` (dev authenticator,
password `sutra`; set `SUTRA_DEV_MFA=true` to exercise the MFA challenge, code `000000`).

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
                                             #   end-to-end cross-tenant isolation (DATABASE_URL)
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
  session cookie must send it as `X-CSRF-Token` (login and MFA verify excepted) → otherwise `403`
  `csrf-token-missing` / `csrf-token-invalid`.
- Tenant scope comes **only** from the session (ADR-L5-0004). `X-Sutra-Tenant-Id` / `X-Sutra-Institution-Id`
  are no longer read; if a client still sends one that contradicts the session it gets `403`.
- Mutable aggregates expose `ETag: "<version>"`; updates require `If-Match` (412 on conflict, 428 if absent).

## Endpoints

| Method | Path | Notes |
|--------|------|-------|
| GET | `/health/live`, `/health/ready` | readiness checks PostgreSQL, flips to 503 while draining |
| POST | `/api/v1/auth/login` | dev authenticator now, Keycloak delegation later in Batch 1; returns `csrfToken` |
| POST | `/api/v1/auth/mfa/verify` | MFA continuation hook; returns `csrfToken` |
| GET | `/api/v1/auth/session` | current principal, `expiresAt`, `idleExpiresAt`, `csrfToken` |
| POST | `/api/v1/auth/logout` | revokes the server-side session (CSRF token required) |
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
