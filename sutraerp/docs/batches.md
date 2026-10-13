# Delivery batches

SutraERP is delivered as a modular monolith in ten batches. Each batch ends green on CI
(fmt · clippy `-D warnings` · tests · web typecheck/lint/test/build · OpenAPI drift).

| # | Batch | Domain(s) | Scope highlights | Status |
|---|-------|-----------|------------------|--------|
| 0 | **Foundation** | all (scaffold) | Cargo + pnpm monorepo, kernel/contracts/web/persistence crates, 10 module crates with enforced boundaries, config, logging + tracing hooks, health, request id, tenant scope extractor, RFC 9457 problems, Idempotency-Key middleware, audit log + writer, transactional outbox, UUIDv7, optimistic concurrency reference aggregate, auth interfaces + dev stub, WorkflowPort + no-op adapter, OpenAPI 3.1 from code, typed TS client, Soft Bento UI library, app shell with role sign-in/switching, per-role Home, route stubs for all 99 workspaces, docker-compose, Dockerfiles, CI | ✅ this batch |
| 1 | Platform | A | Keycloak 26 credential adapter (no ROPC), durable D01 sessions, MFA (WebAuthn/TOTP) UI, CSRF tokens, D02 authorization policy enforcement, session-bound scope, NATS JetStream outbox relay + `EventPublisher`, idempotency/outbox sweepers, Valkey cache adapter, notifications/notices, to-dos inbox, profile | ✅ complete (slices A–D, see below) |
| 2 | Institution | B | Institutions, campuses, departments, programmes catalogue, academic calendar & business timezone, institution configuration; Batch 0 reference aggregate becomes the real model | ✅ complete (slices F, 2A–2C integrated, see below) |
| 3 | Student lifecycle | C | Admissions → enrolment → progression → exit, guardians, documents & certificates, student workspace | ✅ complete (slices F, 3A–3C integrated, see below) |
| 4 | Academic | D | Courses, sections, timetable, attendance, coursework/assignments, faculty "My classes" | ✅ complete (slices F, 4A–4C integrated, see below) |
| 5 | Finance | F | Fee structures, demands, concessions, payments & receipts (rust_decimal end to end), reconciliation, student fees screen | planned |
| 6 | Examinations | I | Exam scheduling, seating, registration & hall tickets, assessments & marks, gradebook, moderation, result computation (SGPA/CGPA), verification, publication, grade cards, revaluation | in progress (6·F foundation, see below) |
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

## Batch 2 slices

Contracts and file ownership: [`batch2-contracts.md`](batch2-contracts.md).

| Slice | Scope | Status |
|-------|-------|--------|
| **2·F** | Foundation: shared ids / lifecycle states / V1 event contracts / permissions, module layout with pre-mounted routers, inbox rule hooks, readiness aggregation, approval helper over the WorkflowPort, institution `status` column, web route placeholders | ✅ |
| **2A** | Institution lifecycle + activation decision, profile, business timezone, policies, organisation units tree, type blueprints, governance, restructuring, position definitions | <!-- batch2:2a:status -->✅ |
| **2B** | Campuses, academic structure, programme catalogue, curriculum framework (immutable versions), school academic model | <!-- batch2:2b:status -->✅ |
| **2C** | Academic calendar (years, terms, key dates, assignments, revisions, exceptions, closure, simulation), activation readiness, Admin → Academics and Campus setup screens, Docker click-through | <!-- batch2:2c:status -->✅ |

Batch 2 is complete: the slices were merged 2A → 2B → 2C onto `erp_merge` and integrated (all three
readiness providers feed `shared::readiness::evaluate`, so `submit` is gated on the full checklist;
campus decommissioning also checks 2C campus-scoped calendar assignments; the web layer uses the
generated OpenAPI types and type-checked paths for every 2A/2B/2C endpoint). Cross-slice test:
`apps/api/tests/db_b2_integration.rs` (register → profile → configure → blueprint + root unit →
campus → structure / programme / curriculum → published calendar + open year + assignment →
readiness all met → submit → active). Container verification runs on the user's machine: README
"Local Docker verification" → *Batch 2 Docker click-through*.

## Batch 3 slices

Contracts and file ownership: [`batch3-contracts.md`](batch3-contracts.md).

| Slice | Scope | Status |
|-------|-------|--------|
| **3·F** | Foundation: Domain C ids / lifecycle states (shared Student + StudentContext machines) / V1 event contracts / permissions, `student` + `student_context` + status-history tables, lifecycle helper, Domain B reference port, Finance/Exams stub ports, ABC + external-scholarship no-op ports, student directory port, pre-mounted routers + OpenAPI tags, inbox rule hooks, web route placeholders | ✅ |
| **3A** | Admissions (application → review → offer → acceptance → documents → approval via WorkflowPort → financial clearance → confirmation → student creation), cancellation/withdrawal/revocation/expiry/readmission, activation + enrolment, assignment, suspension/restoration, transfers, profile, identity correction, duplicates, documents and certificate requests, archival; Admin Students, admission flow, Student 360, Student Documents | <!-- batch3:3a:status -->✅ |
| **3B** | Registration windows, registration, add/drop, cancellation, backlog/repeat, finalisation, holds; progression decisions, credit ledger, conditional promotion, probation, detention, credit transfer, programme migration, academic break/rejoin, qualification eligibility, ABC sync, decision revision, progression policy; registration/progression tabs + student registration | <!-- batch3:3b:status -->✅ |
| **3C** | Qualification completion and conferment, record finalisation, MEME and other exits, exit clearance, closures, deferral, correction/revocation; guardians (relationships, custody, consent, communication, emergency contacts, financial responsibility, pickup); scholarships (definitions, policy, eligibility, renewal, expiry, external sync, bulk recalculation, financial-aid hand-off); alumni transition; exit, guardians and scholarship tabs | <!-- batch3:3c:status -->✅ |

Batch 3 is complete: the slices were integrated 3A → 3B → 3C on top of `batch3/foundation` and
squashed onto `erp_merge` (3C completion reads the newest 3B `qualification_eligibility` per student
context, with a credit-ledger fallback; the OpenAPI spec and generated api-client types cover every
3A/3B/3C endpoint). Cross-slice coverage: `apps/api/tests/db_b3_foundation.rs`, `db_b3a.rs`,
`db_b3b.rs` and `db_b3c.rs` (the latter also exercises the 3B eligibility wiring).

## Batch 4 slices

Contracts and file ownership: [`batch4-contracts.md`](batch4-contracts.md).

| Slice | Scope | Status |
|-------|-------|--------|
| **4·F** | Foundation: Domain D ids / lifecycle states (shared Course Offering, Section, Faculty Assignment, Section Enrolment machines) / V1 event contracts / permissions incl. faculty-scoped `academic.teach`, `course_offering` + `offering_section` + `faculty_assignment` + `section_enrolment` + status-history tables, lifecycle helper, offering read port (`OfferingDirectory`), Domain C `RegistrationReader` + `CreditPostingPort`, Campus teaching-space and People staff stub ports, pre-mounted routers + OpenAPI tags + operation-id rule, inbox rule hooks, web frames (admin Academic delivery, student Courses/Timetable/Assignments/Library, faculty My classes/My students/Timetable/Attendance/Resources; faculty nav no longer links the admin-only Students page), `pnpm e2e:academic` skeleton | ✅ |
| **4A** | Course offerings (establishment → approval via WorkflowPort → publication → enrolment → activation), suspension/reactivation, cancellation, capacity, eligibility (gated on 3B registrations), waitlist, cross-campus, revision/rollback, archive, merge/split, templates, freeze/unfreeze, windows, quotas, change simulation; sections + enrolment; faculty assignment, operational change, governance; Offerings/Faculty tabs, student Courses, faculty My classes + My students | <!-- batch4:4a:status -->✅ |
| **4B** | Timetable (requirement → generation → validation → publication, revision, retirement), teaching sessions, attendance capture/finalisation/correction/thresholds, syllabus coverage, academic monitoring, internal assessment + coursework/assignments; Timetable/Attendance/Monitoring/Assessment tabs, student Timetable + Assignments, faculty Timetable + Attendance | <!-- batch4:4b:status -->✅ |
| **4C** | Flexible/external learning (opportunities, providers, policy, allocation, waitlist, external registration, completion, archival, conflicts, exceptions), certificate verification, credit mapping, credit recognition posted to the Domain C ledger, reconciliation; course resources; Flexible/Recognition tabs, student Library, faculty Resources | <!-- batch4:4c:status -->✅ |

Batch 4 is complete: the foundation and slices 4A → 4B → 4C were integrated on `batch4/integrate` and
squashed onto `erp_merge` (4B and 4C read 4A's offerings, sections, rosters and faculty through
`OfferingDirectory`; recognised flexible-learning credits post to the Domain C ledger only through
`CreditPostingPort`; the OpenAPI spec and generated api-client types cover every 4A/4B/4C endpoint).
Gates at integration: fmt, clippy `-D warnings`, API DB tests 131/131 (`db_b4_foundation`, `db_b4a`,
`db_b4b`, `db_b4c` plus the Batch 2/3 suites), `cargo test --workspace` 366, web typecheck/lint/test/build,
and the `pnpm e2e:academic` click-through (admin, student and faculty) with `pnpm e2e:student` as the
Batch 3 regression.

## Batch 6 slices

Contracts, scenario coverage and file ownership: [`batch6-contracts.md`](batch6-contracts.md).

| Slice | Scope | Status |
|-------|-------|--------|
| **6·F** | Foundation: Domain I ids / lifecycle states (shared Session, Paper, Registration, Mark, Result machines) / V1 event contracts / permissions (`exam.*`), shared tables (grading schemes, sessions, papers, registrations, paper marks + history, term results + lines), lifecycle / registration / marks / results helpers, pure grading engine (SGPA / CGPA, `rust_decimal`), `ExamDirectory` / `MarksReader` / real `ResultsPort`, Domain D `AcademicExamFacts` and Domain F `ExamFeePort` (`exam_fee` charges), grading-scheme API, pre-mounted routers + OpenAPI tags, inbox rule hooks, web route frames (Admin Examinations, Faculty Assessments, Student Results) | <!-- batch6:6f:status -->✅ |
| **6A** | Exam setup: policy, sessions, papers, scheduling / timetable, venues, seating, invigilation, registration (self, bulk, late, amendments, withdrawal, finalisation), hall tickets (finance-dues and attendance conditions), conduct basics, contingencies | <!-- batch6:6a:status -->✅ |
| **6B** | Assessment & marks: internal assessment import (Batch 4), marks entry by teaching faculty, verification / approval of final marks, evaluation rounds, moderation, withheld marks, gradebook, malpractice basics | <!-- batch6:6b:status -->✅ |
| **6C** | Results: computation engine (SGPA / CGPA), withholding, verification, approval, publication / withdrawal / republication, grade cards, revaluation / rechecking (Finance fee), supplementary eligibility, completion evaluation, session closure | <!-- batch6:6c:status -->✅ |
