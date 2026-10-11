# Delivery batches

SutraERP is delivered as a modular monolith in ten batches. Each batch ends green on CI
(fmt · clippy `-D warnings` · tests · web typecheck/lint/test/build · OpenAPI drift).

| # | Batch | Domain(s) | Scope highlights | Status |
|---|-------|-----------|------------------|--------|
| 0 | **Foundation** | all (scaffold) | Cargo + pnpm monorepo, kernel/contracts/web/persistence crates, 10 module crates with enforced boundaries, config, logging + tracing hooks, health, request id, tenant scope extractor, RFC 9457 problems, Idempotency-Key middleware, audit log + writer, transactional outbox, UUIDv7, optimistic concurrency reference aggregate, auth interfaces + dev stub, WorkflowPort + no-op adapter, OpenAPI 3.1 from code, typed TS client, Soft Bento UI library, app shell with role sign-in/switching, per-role Home, route stubs for all 99 workspaces, docker-compose, Dockerfiles, CI | ✅ this batch |
| 1 | Platform | A | Keycloak 26 credential adapter (no ROPC), durable D01 sessions, MFA (WebAuthn/TOTP) UI, CSRF tokens, D02 authorization policy enforcement, session-bound scope, NATS JetStream outbox relay + `EventPublisher`, idempotency/outbox sweepers, Valkey cache adapter, notifications/notices, to-dos inbox, profile | ✅ complete (slices A–D, see below) |
| 2 | Institution | B | Institutions, campuses, departments, programmes catalogue, academic calendar & business timezone, institution configuration; Batch 0 reference aggregate becomes the real model | planned |
| 3 | Student lifecycle | C | Admissions → enrolment → progression → exit, guardians, documents & certificates, student workspace | planned |
| 4 | Academic | D | Courses, sections, timetable, attendance, coursework/assignments, faculty "My classes" | planned |
| 5 | Finance | F | Fee structures, demands, concessions, payments & receipts (rust_decimal end to end), reconciliation, student fees screen | planned |
| 6 | Examinations | I | Exam scheduling, assessments & gradebook, moderation, results publication, grade cards | planned |
| 7 | People & workforce | E | Staff records, recruitment/onboarding, leave, payroll inputs, staff directory | planned |
| 8 | Campus · Services · Resources | G · H · J | Facilities, hostel, transport, service desk, library & learning resources | planned |
| 9 | Production hardening | cross-cutting | OTLP exporter + Prometheus/Loki/Tempo/Grafana, tamper-evident audit chaining (ADR-L5-0012), GHCR push + Cosign signing on trusted runners, K8s manifests/Helm, OpenBao secrets, Flowable OSS 8 adapter + BPMN deployment, OpenSearch projections, performance/capacity gates, DR drills | planned |

Screens in the web app link to their batch (`apps/web/app/lib/roles.ts → SCREEN_BATCH`).

## Batch 1 slices

| Slice | Scope | Status |
|-------|-------|--------|
| **1A** | Durable PostgreSQL sessions (`platform.session`, SHA-256 token digests, idle + absolute expiry, revocation; `SUTRA_SESSION_STORE`), session-bound CSRF synchronizer tokens (`X-CSRF-Token`, 403 problem), deny-by-default role→permission policy + `Authenticated` extractor (401/403 problems), institution endpoints scoped from the session (client scope headers no longer select scope; contradicting headers → 403), idempotency records keyed by the session owner, web client sends `X-CSRF-Token` | ✅ |
| **1B** | Keycloak 26 OIDC adapter via BFF (Authorization Code + PKCE, `platform.pre_auth`, JWKS-cached ID-token validation, role/tenant claim mapping, `sid` + RP-initiated logout, `SUTRA_AUTH_MODE=dev\|keycloak`), dev-mode TOTP MFA (enrolment + QR, login challenge, replay refusal, AES-GCM secrets in `platform.mfa_factor`), session `mfa_level`/`amr` + step-up 403, WebAuthn port (501, deferred), signed login CSRF (`GET /auth/csrf`), cross-site `Origin`/`Sec-Fetch-Site` guard, throttled `last_seen_at` | ✅ |
| **1C** | `MessagePublisher` port + NATS JetStream adapter (stream ensured from config, `Nats-Msg-Id` = outbox id, ack-then-mark), outbox relay (`FOR UPDATE SKIP LOCKED` leases, exponential backoff + jitter, dead-letter, per-aggregate ordering, cancellation; `SUTRA_OUTBOX_RELAY=off\|nats`), advisory-locked sweeper scheduler (idempotency keys, published outbox retention, sessions, pre-auth records), advisory `Cache` port + Valkey/in-memory adapters (`SUTRA_CACHE=off\|memory\|valkey`) with an institution GET read-through cache, non-critical NATS/Valkey readiness, compose `nats` service | ✅ |
| **1D** | Notifications (subject/role addressed, receipts, unread count, per-category in-app/e-mail preferences) + SMTP e-mail channel with queued, retried delivery (Mailpit in dev), notices (audience roles, scheduled/expiring, pinned, archive), to-dos (own/assigned, complete/dismiss/reopen) and an outbox-driven event consumer (`institution.registered` → admin notification + setup to-do), profile + security self-service (sessions list/revoke, MFA status), web screens (notifications, notices, to-dos, profile, account/security; live bell; passkey option at sign-in), per-schema runtime DB roles (`sutra_<schema>`, `sutra_app`; migrate as owner, serve as runtime login role), dev-mode WebAuthn passkeys (registration, sign-in, step-up, sign-counter check), Keycloak back-channel logout (`logout_token`, `jti` replay guard, revokes by IdP `sid`; registered in the dev realm), retention sweepers, compose Mailpit + runtime-role wiring | ✅ |

Batch 1 is complete. Container verification (`docker compose --profile app up --build`) could not be run in
the build sandbox (no Docker); the Dockerfiles and compose file were checked by reading, and the README section
"Local Docker verification" lists the commands and the 1C/1D click-through checklist to run locally.
