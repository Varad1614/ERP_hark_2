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
| 5 | Finance | F | Fee structures, demands, concessions, payments & receipts (rust_decimal end to end), reconciliation, student fees screen | ✅ complete (slices 5A–5C, see below; online payment mock/disabled only, budgets not delivered) |
| 6 | Examinations | I | Exam scheduling, seating, registration & hall tickets, assessments & marks, gradebook, moderation, result computation (SGPA/CGPA), verification, publication, grade cards, revaluation | ✅ complete (slices F, 6A–6C, see below; 6D deferred) |
| 7 | People & workforce | E | Staff records, recruitment/onboarding, positions, leave & approvals, attendance & workload, payroll engine (Finance postings), payslips, staff directory | ✅ complete (slices F, 7A–7C integrated, see below) |
| 8 | Campus · Services · Resources | G · H · J | Facilities, rooms & labs, maintenance, service desk, hostel, transport, library, dashboards & regulatory reporting, procurement, inventory, assets, Finance AP | ✅ complete (slices F, 8A–8D integrated, see below) |
| 9 | Production hardening | cross-cutting | OTLP exporter + Prometheus/Loki/Tempo/Grafana, tamper-evident audit chaining (ADR-L5-0012), GHCR push + Cosign signing on trusted runners, K8s manifests/Helm, OpenBao secrets, Flowable OSS 8 adapter + BPMN deployment, OpenSearch projections, performance/capacity gates, DR drills | ✅ complete (slices F, 9A–9E integrated, see below) |

Screens in the web app link to their batch (`apps/web/app/lib/roles.ts → SCREEN_BATCH`).

> **Status caveat (Part 1 readiness review, 2026-10-15):** ✅ means the batch scope was delivered and CI is green, not
> that the product is release-ready. In every environment, governed approval steps ("approval", "decision",
> "approve") are completed by `system:auto-approval` (module state hard-codes `ApprovalMode::AutoApprove`; selecting
> Flowable does not change it), and no decision endpoint enforces separation of duties. See
> [`readiness/DEFECT_REGISTER.md`](readiness/DEFECT_REGISTER.md) (DEF-X-001, DEF-X-002) and
> [`readiness/FEATURE_INVENTORY.md`](readiness/FEATURE_INVENTORY.md).

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

## Batch 5 slices

Contracts, scenario coverage and deferrals: [`batch5-contracts.md`](batch5-contracts.md).

| Slice | Scope | Status |
|-------|-------|--------|
| **5A** | Charges, adjustments, aid, configuration: fee heads, versioned fee structures (approval → publication), charge generation → validation → publication (Dr receivables / Cr income), revision / adjustment / cancellation, concessions and aid | ✅ |
| **5B** | Payments, receipts, refunds, receivables: counter and online payments (gateway adapter: mock / disabled only), allocation, unidentified receipts, refunds, receivables, write-off, dunning | ✅ |
| **5C** | Ledger, periods, treasury: balanced double-entry postings, manual journals, reversal, period close / reopen, year close, bank accounts and settlements | ✅ |

Not delivered: budgets and cost centres (F-033…F-038; the contract placed them in "Batch 9 or a later 5D", Batch 9 did
not deliver them), accounting batches and fund transfers (process keys exist, no aggregate), a real payment gateway
adapter. DB tests `db_b5_foundation`, `db_b5a`, `db_b5b`, `db_b5c`; click-through `pnpm e2e:finance`.

## Batch 6 slices

Contracts, scenario coverage and file ownership: [`batch6-contracts.md`](batch6-contracts.md).

| Slice | Scope | Status |
|-------|-------|--------|
| **6·F** | Foundation: Domain I ids / lifecycle states (shared Session, Paper, Registration, Mark, Result machines) / V1 event contracts / permissions (`exam.*`), shared tables (grading schemes, sessions, papers, registrations, paper marks + history, term results + lines), lifecycle / registration / marks / results helpers, pure grading engine (SGPA / CGPA, `rust_decimal`), `ExamDirectory` / `MarksReader` / real `ResultsPort`, Domain D `AcademicExamFacts` and Domain F `ExamFeePort` (`exam_fee` charges), grading-scheme API, pre-mounted routers + OpenAPI tags, inbox rule hooks, web route frames (Admin Examinations, Faculty Assessments, Student Results) | <!-- batch6:6f:status -->✅ |
| **6A** | Exam setup: policy, sessions, papers, scheduling / timetable, venues, seating, invigilation, registration (self, bulk, late, amendments, withdrawal, finalisation), hall tickets (finance-dues and attendance conditions), conduct basics, contingencies | <!-- batch6:6a:status -->✅ |
| **6B** | Assessment & marks: internal assessment import (Batch 4), marks entry by teaching faculty, verification / approval of final marks, evaluation rounds, moderation, withheld marks, gradebook, malpractice basics | <!-- batch6:6b:status -->✅ |
| **6C** | Results: computation engine (SGPA / CGPA), withholding, verification, approval, publication / withdrawal / republication, grade cards, revaluation / rechecking (Finance fee), supplementary eligibility, completion evaluation, session closure | <!-- batch6:6c:status -->✅ |

## Batch 7 slices

Contracts, scenario coverage and file ownership: [`batch7-contracts.md`](batch7-contracts.md).
43 of 75 L3-E scenarios in scope; 32 deferred to a later 7D (workforce / faculty / HR / profile
depth: E-005, E-010, E-014…017, E-019…025, E-027, E-028, E-032, E-035…038, E-040, E-042,
E-061…063, E-067, E-069…073, E-075).

| Slice | Scope | Status |
|-------|-------|--------|
| **7·F** | Foundation: Domain E ids / lifecycle states (shared StaffStatus, EngagementStatus, leave ledger kinds, attendance days) / V1 event contracts / permissions (`people.*`), shared tables (staff records, engagements, append-only leave ledger, attendance days), staff / lifecycle / leave / attendance helpers, `WorkforceDirectory` / `LeaveReader`, real `StaffDirectory` for Domain D (subject-only fallback), Domain D `FacultyWorkloadPort` and Domain F `PayrollPostingPort` (`payroll_run` journal entries), staff-directory API, pre-mounted routers + OpenAPI tags, inbox rule hooks, web route frames (Admin People, Faculty Leave / My HR) | <!-- batch7:7f:status -->✅ |
| **7A** | Staff records, recruitment & onboarding, positions: profiles + verification, engagements (activation, modification, transfer, suspension, closure, rejoining, duplicates), positions + assignments, reporting managers, faculty establishment / eligibility, onboarding, probation, separation, certificates (25 scenarios) | <!-- batch7:7a:status -->✅ |
| **7B** | Leave, attendance & workload: leave policy, accrual, request → decision (reference approval), reservation / consumption, cancellation, adjustment, carry forward, encashment, exceptions, leave cases, long leave, attendance days, faculty workload plans (12 scenarios) | <!-- batch7:7b:status -->✅ |
| **7C** | Payroll: configuration + statutory rules, salary assignments, runs (calculate → approve → finalise → Finance posting), payslips, adjustments, retro pay, final settlement, off-cycle (6 scenarios) | <!-- batch7:7c:status -->✅ |

## Batch 8 slices

Contracts, scenario coverage and file ownership: [`batch8-contracts.md`](batch8-contracts.md).
120 of 505 L3 scenarios in scope (G 30 + 29 + 18 + G-154 / G-320, H 10, J 31) plus the Batch 5
finance deferrals F-044, F-045, F-076, F-081, F-082; 385 deferred (contract §1 table: G00
governance, G06 health, G07 security, G08 housekeeping, G09, H01…H06, H09, H10, J09 / J10,
depth of every in-scope area, asset accounting F-067…072, statements F-073…079, budget F-080).

| Slice | Scope | Status |
|-------|-------|--------|
| **8·F** | Foundation: Domain G / H / J ids, states, V1 events, permissions (`campus.*`, `services.*`, `resources.*`), shared tables (infrastructure nodes, service tickets, items + store locations, append-only stock ledger, numbering, status history), `TeachingSpacePort` (real), `CampusChargePort` + `ProcurementPostingPort` (Finance), `OperationalMetricsPort` adapters for every domain, directory APIs, pre-mounted routers, web frames (Admin Campus tabs, Admin Reports, Student Library) | <!-- batch8:8f:status -->✅ |
| **8A** | Infrastructure (site → building → floor → space, classification, capabilities, capacity, availability, suspension, decommissioning), room reservations, service desk (requests, complaints, duplicates, SLA, escalation, requester verification), work orders, preventive maintenance, labs (readiness, practical sessions, batches, equipment) — 30 scenarios | <!-- batch8:8a:status -->✅ |
| **8B** | Hostel (policy, beds, applications → waiting list → allocation + hostel fee, leave, transfer, inspection → damage charge, mess, complaints, vacating → clearance) and transport (stops, routes + versions, requests → allocation, passes + fee, renewal / replacement / suspension, vehicles, drivers, trips, breakdowns) — 29 scenarios | <!-- batch8:8b:status -->✅ |
| **8C** | Library (policy, memberships, catalogue, copies, issue / return / renewal, reservations, overdue fines billed through Finance, waivers, lost copies, clearance, restrictions, `/me/library`) and reports (widgets, governed dashboards with versions / rollback / role assignments, library + infrastructure operational reports, F-076 finance KPIs, regulatory frameworks → cycles → validated snapshots → packages → submission → correction / closure) — 30 scenarios + F-076 | <!-- batch8:8c:status -->✅ |
| **8D** | Suppliers, requisitions, sourcing (RFQ / direct), purchase orders, goods receipts, item master, stores, stock ledger, issues / returns / reservations / adjustments, reorder, assets (register, custody, condition, retirement), vendor invoices with three-way match → AP posting, vendor payments — 31 scenarios + F-044, F-045, F-081, F-082 | <!-- batch8:8d:status -->✅ |

Integration (`batch8/integrate`): the cross-slice DB test `db_b8_integration` (room booking, hostel
capacity guard, hostel fee, hostel complaint → 8A ticket → work order → closure, transport pass, library
fine, the student's Finance dues, PO → receipt → stock → asset → issue → vendor invoice → AP → payment,
and the metrics catalogue + a published dashboard reading all of it) and the `pnpm e2e:campus`
click-through (Admin Campus, Admin Reports, Student Library; CI artifact `e2e-shots-campus`).

Integration rules and known inconsistencies:

- **Staff are recorded but never billed.** Staff can reside in the hostel, hold a transport pass and
  borrow from the library, but `CampusChargePort` charges students only: a staff hostel fee / pass fee /
  library fine is kept on the campus row (no `chargeId`), staff library fines are waived, not billed.
- **Hostel capacity.** An 8A capacity change of a `hostel_room` space is refused below the number of
  its beds in service (`available` or `occupied`); take beds out of service first.
- **Complaints.** Hostel, transport and library complaints are opened with `shared::tickets::open_ticket`
  (source `hostel` / `transport` / `library`) and handled in the 8A service desk like any ticket.
- **Metrics.** `PgCampusMetrics` also reports bookings, open work orders, hostel beds / residents /
  waiting list and transport routes / passes / vehicles; `PgResourceMetrics` reports suppliers, open
  purchase orders (count + value), assets (count + acquisition cost) and payables outstanding.
- **Approval-state wire values differ by slice** (not unified, to keep the slice contracts and their
  DB tests stable): 8A maintenance plans use `not_requested → pending → approved / rejected` (same in
  the column); 8C library policies and reporting frameworks show `not_submitted` on the wire while the
  column stores `none`. Hostel, transport and resources aggregates carry an `approvalPending` flag
  instead. A later batch should settle on one vocabulary.
- An asset assigned to a `space` is not checked against Domain G (the resources crate cannot read the
  campus schema); `TeachingSpacePort`-style validation is a follow-up.


## Batch 9 — Production hardening (+ G00 governance, H09 accreditation)

Contracts, scope, deferrals and file ownership: [`batch9-contracts.md`](batch9-contracts.md). No real
cluster: everything is proved in GitHub Actions (kind cluster + containers on the runner); GHCR push +
Cosign keyless signing on `erp_merge` / `v*` tags only; pilot go-live = runbook + checklist + seeded
pilot tenant.

| Slice | Scope | Status |
|-------|-------|--------|
| **9·F** | Foundation: `config_b9` (OTel, audit chain, Flowable, OpenSearch env), audit hash-chain contract + migration, permissions `platform.audit.verify` / `campus.governance.manage` / `services.accreditation.manage`, mount points, Helm chart skeleton, per-slice workflows + kind stage scripts, `SUTRA_ENV_FILE` | <!-- batch9:9f:status -->✅ `667b81ef` |
| **9A** | OTLP export, Prometheus metrics, Loki / Tempo / Grafana dashboards + alerts, audit sealer + verify API + anchors | <!-- batch9:9a:status -->✅ `3133fd94` |
| **9B** | Helm chart + k8s manifests, OpenBao secrets, GHCR push + Cosign + SBOM, kind install-and-smoke, compose boot | <!-- batch9:9b:status -->✅ `19243f4c` |
| **9C** | Flowable deployment + REST adapter + BPMN, OpenSearch projections + search API, backup / restore + DR drill | <!-- batch9:9c:status -->✅ `a2dee3af` |
| **9D** | k6 performance + capacity gates, security review (deny / audit / gitleaks / Trivy / Grype / ZAP), threat model, fixes | <!-- batch9:9d:status -->✅ `9f3f619e` |
| **9E** | G00 governance (15: `campus.governance_*`, tag `campus-governance`, Admin → Campus → Governance), H09 accreditation (11: `services.accreditation_*`, tag `services-accreditation`, Admin → Reports → Accreditation), live home admin KPIs from the services metrics catalogue (fixture numbers removed), `PersonPicker` in the library / hostel / transport forms, approval-state normalisation (§6.4), pilot seed (`deploy/pilot/seed-pilot.mjs`, `deploy/ci/seed-pilot.sh`, workflow `Pilot seed (API + PostgreSQL)`) + `runbooks/pilot-go-live.md` + `runbooks/go-live-checklist.md` | <!-- batch9:9e:status -->✅ `6e5cd8a3` |
| **Integration** | `batch9/integrate` (merge order 9A → 9C → 9B → 9D → 9E): metrics bridge (9C `metrics` facade → 9A prometheus-client registry, one `/metrics`), SEC-9D-003 fix (`SUTRA_PAYMENT_GATEWAY`), BPMN for the 9E approval keys, W-G00 / W-H09 screens, capacity noise floor, kind PG password + seed-pilot container, rustls 0.23.46, NetworkPolicy name clash fix, backup PVC as a pre-install hook (helm `--wait` no longer blocks on the WaitForFirstConsumer claim), kind fixes (PostgreSQL 18 client for the DR drill, idempotent OpenBao re-seed, DR drill re-opens the api port-forward after the restart, release-record credential check ignores `${VAR}` passwords), governance / accreditation e2e screenshots; every workflow green incl. kind with all four stages live (smoke → telemetry → pilot seed → DR; kind DR RTO 43 s against the 2 h target) on `2258a2c8` | <!-- batch9:int:status -->✅ |

Integration deviations from the contract (also in [`batch9-contracts.md`](batch9-contracts.md) §14):

- **Backups keep ACLs** (§11): `pg_dump -Fc --no-owner` without `--no-privileges`; `sutra-restore.sh`
  re-creates the cluster roles from `deploy/backup/roles.sql` before `pg_restore`, so the append-only
  audit grants (ADR-L5-0012) survive a restore.
- **Capacity regression check** (§9) has a 10 ms absolute p95 noise floor and a throughput floor
  (75 % of the baseline req/s).
- **One metrics registry:** 9C's `metrics` facade families are bridged into 9A's registry
  (`apps/api/src/metrics_bridge.rs`).
- **SEC-9D-003 (High) fixed:** `SUTRA_PAYMENT_GATEWAY=disabled|mock` (production default `disabled`,
  development / test `mock`); disabled → online checkout, recovery polling, gateway refunds and the
  webhook answer 503 `dependency-unavailable`; `SUTRA_PAYMENT_WEBHOOK_SECRET` from OpenBao `payments`,
  production refuses unset / the dev constant / < 32 characters; pilot values set `disabled`.

Known gaps and deferrals (contract §1 / §14):

- The kind DR drill covers PostgreSQL (dump → wipe StatefulSet + PVC → restore → grants / heads / chain
  verify); Flowable and OpenSearch backup → restore → rebuild are proved in the `Data platform` job.
- No real cluster: Helm, OpenBao, Flowable, OpenSearch, backup / DR and telemetry are proved in GitHub
  Actions only (kind + containers on the runner). Real cloud / DNS / TLS / IdP federation for the
  pilot are operator steps in `runbooks/pilot-go-live.md`.
- GHCR push + Cosign keyless signing + SBOM attestation run only on `erp_merge` / `v*` tags; the first
  real signed images appear on the erp_merge merge.
- Logical backups only: no PG streaming replication, WAL-archive PITR or multi-region; pilot RPO 24 h,
  RTO target 2 h (the DR drill records the measured RTO).
- `NoopWorkflowAdapter` stays the default approval engine; Flowable is proved and switched on per
  environment after the pilot.
  **Caveat:** switching Flowable on does not stop auto-approval: every module hard-codes
  `ApprovalMode::AutoApprove` and the adapter sends `autoApprove=true` unless the caller passes `false`
  (no module does); production also does not refuse `noop` (readiness DEF-X-001).
- OpenSearch projections cover students, staff and the library catalogue only; no global search UI.
- Online payments stay off in production until a real payment gateway adapter exists (SEC-9D-003).
- Every Batch 8 deferral remains (H01, H03…H06, H10, G06…G09, J09 / J10, F-046…050, F-067…075,
  F-077…080, F-083); OLED theme and customisable home widgets not done.
- Approval-state legacy fields: `approvalPending` stays on 8B / 8D views (deprecated).

### Approval-state rule (9E, contract §6.4)

Every Batch 8 / 9 aggregate view with an approval step carries the canonical wire field
`approvalState` ∈ `not_requested | pending | approved | rejected`:

- 8A maintenance plans already complied; 9E G00 cases / configuration versions and H09 frameworks /
  cycles / records use it natively.
- 8C library policies and reporting frameworks: the stored value `none` stays in the database and is
  mapped to `not_requested` in the view (the old wire value `not_submitted` is gone).
- 8B hostel / transport and 8D supplier / purchase order / vendor invoice views add `approvalState`
  derived from their status (+ `approvalPending`); `approvalPending` stays, documented as deprecated,
  so existing clients keep working.
- The web reads the field only through `approvalStateLabel` / `approvalFrom` in
  `apps/web/app/features/campus/approval.ts` (legacy `not_submitted` / `none` read as "Not submitted").
- New aggregates with an approval step must expose `approvalState` with exactly these four values.
