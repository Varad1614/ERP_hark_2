# Delivery batches

SutraERP is delivered as a modular monolith in ten batches. Each batch ends green on CI
(fmt · clippy `-D warnings` · tests · web typecheck/lint/test/build · OpenAPI drift).

| # | Batch | Domain(s) | Scope highlights | Status |
|---|-------|-----------|------------------|--------|
| 0 | **Foundation** | all (scaffold) | Cargo + pnpm monorepo, kernel/contracts/web/persistence crates, 10 module crates with enforced boundaries, config, logging + tracing hooks, health, request id, tenant scope extractor, RFC 9457 problems, Idempotency-Key middleware, audit log + writer, transactional outbox, UUIDv7, optimistic concurrency reference aggregate, auth interfaces + dev stub, WorkflowPort + no-op adapter, OpenAPI 3.1 from code, typed TS client, Soft Bento UI library, app shell with role sign-in/switching, per-role Home, route stubs for all 99 workspaces, docker-compose, Dockerfiles, CI | ✅ this batch |
| 1 | Platform | A | Keycloak 26 credential adapter (no ROPC), durable D01 sessions, MFA (WebAuthn/TOTP) UI, CSRF tokens, D02 authorization policy enforcement, session-bound scope, NATS JetStream outbox relay + `EventPublisher`, idempotency/outbox sweepers, Valkey cache adapter, notifications/notices, to-dos inbox, profile | 🟡 slice A done (see below) |
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
| 1B+ | Keycloak 26 credential adapter, MFA (WebAuthn/TOTP) UI, NATS JetStream outbox relay, idempotency/outbox/session sweepers, Valkey cache adapter, notifications/notices, to-dos inbox, profile, per-schema runtime DB roles | planned |
