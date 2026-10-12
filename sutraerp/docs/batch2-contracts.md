# Batch 2 — shared contracts (Institution, Domain B)

**Status:** frozen at `batch2/foundation`. This file is the single source of truth for the three
parallel slices **2A**, **2B** and **2C**. If a slice needs to change anything in §2–§8, it says so
in its PR description ("contract change: …") instead of silently editing a shared file; the
integrator applies it on top of the merge.

Architecture inputs: L2-B01 (+ activation-authority amendment 2026-08-24), L2-B02, L2-B03, L2-B04
(+ curriculum-retirement event amendment 2026-08-24), L2-B05; L3/L4 B-001…B-033; ADR-L5-0004
(scope), -0006/-0007 (owner-local tx + outbox), -0011/-0035 (event versioning/envelope),
-0023/-0042 (WorkflowPort / Flowable), -0034 (API profile), -0036 §3.4 (business timezone).

---

## 1. Slices and scenario coverage

| Slice | Scope | L3/L4 scenarios |
|-------|-------|-----------------|
| **2A** | Institution lifecycle (Draft → Configured → activation request → decision → Active → Suspended/Reactivated → Archived), profile revision, institutional policies, business timezone; organisation units tree (register, activate, modify, restructure, type blueprint, governance config, suspend/restore, decommission, archive); effective-dated position definitions | B-001 … B-016 |
| **2B** | Campuses (register, activation decision, modify, suspend/restore, decommission, archive); academic structure, programme catalogue, curriculum framework (Draft → Published → immutable successor version → Retired); school academic model | B-017 … B-026 |
| **2C** | Academic calendar (years, terms, key dates, open academic context, scope assignment, successor revisions, exceptions, year closure, change simulation, retirement); activation readiness checklist endpoint; web screens Admin → Academics and Admin → Campus setup (consuming all three APIs); README Docker click-through | B-027 … B-033 + readiness + web |

## 2. Where the code lives: module and file ownership

**Decision:** every Batch 2 aggregate lives in the existing crate `modules/institution`
(`sutra-institution`) and the PostgreSQL schema `institution`. There are no new crates.
`apps/api/tests/architecture.rs` fixes exactly ten domain crates, and `modules/campus` is
**Domain G** (Batch 8 facilities), not the Domain B institutional campus. Cross-area reads inside
Domain B may query sibling tables of the same schema, but only **read**. You never write another
slice's tables; go through that slice's functions or events.

### 2.1 Ownership map (create/edit freely inside your own paths)

| Path | Owner |
|------|-------|
| `modules/institution/src/domain.rs`, `http.rs`, `repo.rs` (reference aggregate root) | **2A** |
| `modules/institution/src/lifecycle/**` (lifecycle, profile, timezone, policies, `readiness.rs`) | **2A** |
| `modules/institution/src/org/**`, `modules/institution/src/position/**` | **2A** |
| `modules/institution/src/campus/**` (incl. `readiness.rs`, which covers campus **and** curriculum keys) | **2B** |
| `modules/institution/src/curriculum/**` | **2B** |
| `modules/institution/src/calendar/**` (incl. `readiness.rs`), `modules/institution/src/readiness/**` | **2C** |
| `modules/platform/src/inbox/rules/institution_2a.rs` | **2A** |
| `modules/platform/src/inbox/rules/structure_2b.rs` | **2B** |
| `modules/platform/src/inbox/rules/calendar_2c.rs` | **2C** |
| `crates/contracts/src/institution/events/lifecycle.rs`, `events/org.rs` | **2A** |
| `crates/contracts/src/institution/events/campus.rs`, `events/curriculum.rs` | **2B** |
| `crates/contracts/src/institution/events/calendar.rs` | **2C** |
| `migrations/2026101500000[1-9]_*.sql` | **2A** |
| `migrations/2026101500001[0-9]_*.sql` | **2B** |
| `migrations/2026101500002[0-9]_*.sql` | **2C** |
| `apps/api/tests/db_b2a.rs` / `db_b2b.rs` / `db_b2c.rs` | 2A / 2B / 2C |
| `apps/web/app/routes/admin/**`, `apps/web/app/features/institution/**` | **2C** |
| README region `<!-- batch2:2a:… -->` / `2b` / `2c` + `2c-docker`; `docs/batches.md` status cell `<!-- batch2:2X:status -->` | each slice its own region |

### 2.2 Shared files: foundation-owned, do not edit

`modules/institution/src/lib.rs` (router list already mounts every slice), `modules/institution/src/shared/**`,
`crates/kernel/src/auth.rs` (all Batch 2 permissions already added), `crates/contracts/src/institution/{mod,ids,status}.rs`,
`crates/contracts/src/institution/events/mod.rs`, `modules/platform/src/inbox/{consumer,mod}.rs` and
`inbox/rules/mod.rs`, `apps/api/src/{app,openapi}.rs` (tags already added), `apps/web/app/routes.ts`,
`apps/web/app/lib/roles.ts`, `apps/api/tests/support/mod.rs`, `apps/api/tests/db_b2_foundation.rs`,
`migrations/20261015000000_institution_batch2_foundation.sql`.

If you cannot avoid touching one of these, keep the edit **append-only** at the end of the
relevant list/block, add a `// 2X:` comment, and list it in your PR description. Examples: a new id
at the end of your slice's block in `ids.rs`, or a new permission appended after the Batch 2
variants in `auth.rs` (with `ALL`, `as_str`, and the Administrator grant).

### 2.3 Generated files

`openapi/openapi.json` and `packages/api-client/src/schema.d.ts` are **generated**. Each slice
regenerates them in its own branch (`make openapi`) so its CI drift check passes. On merge, **never
hand-merge them**: take either side, then regenerate after the merge (`cargo run -q -p sutra-api --
openapi > openapi/openapi.json && pnpm gen:api`). `Cargo.lock` / `pnpm-lock.yaml`: no new
dependencies are expected. If you add one, say so in the PR.

`CHANGELOG.md`: slices don't edit it. The integrator writes the Batch 2 entry from the PR descriptions.

## 3. Database

### 3.1 Migrations

| Range | Owner |
|-------|-------|
| `20261015000000_institution_batch2_foundation.sql` | foundation (applied): `institution.institution.status` (`draft` default, CHECK over `InstitutionStatus`), `status_changed_at`; pre-Batch-2 rows backfilled to `active` |
| `20261015000001` … `20261015000009` | 2A |
| `20261015000010` … `20261015000019` | 2B |
| `20261015000020` … `20261015000029` | 2C |

Reserved names are **not** pre-created as empty files. sqlx checksums applied migrations, so an
empty placeholder that is filled in later breaks every database that already ran it. Name files
`<version>_institution_<what>.sql`. Never edit a migration after it has been applied anywhere;
add the next number instead. New tables pick up the runtime-role grants automatically (default
privileges from `20261014000003_runtime_db_roles.sql`).

Table conventions (copy `20261010000005_institution_institution.sql`): `id uuid PRIMARY KEY DEFAULT
uuidv7()`, `tenant_id uuid NOT NULL`, `institution_id uuid NOT NULL REFERENCES institution.institution(id)`
(for institution-bound rows), `status text NOT NULL CHECK (status IN (...))` using the wire names of
§4.2, `version bigint NOT NULL DEFAULT 1`, `created_at/updated_at timestamptz`. Business-local
dates are `date` and are interpreted in the institution's `timezone` (or a campus override);
instants are `timestamptz`. Unique business codes are scoped `UNIQUE (tenant_id, institution_id, code)`.

**No foreign keys across slices.** A column that refers to another slice's aggregate, such as
`programme.org_unit_id` → 2A or `calendar_assignment.scope_id` → 2B/2A, is a plain `uuid`. The
writing slice validates existence by reading the other table, which is allowed within the schema.
The FK to `institution.institution(id)` is the one exception everyone may use.

### 3.2 Table names (schema `institution`)

| Slice | Tables |
|-------|--------|
| 2A | `institution` (ALTER only: profile columns, `timezone` already exists), `institution_profile_revision`, `activation_request`, `institution_policy`, `institution_policy_revision`, `org_unit` (tree: `parent_id` + materialised `path text`), `org_type_blueprint`, `org_type_blueprint_unit_type`, `governance_config`, `org_restructure`, `org_restructure_move`, `position_definition`, `position_definition_revision` |
| 2B | `campus`, `campus_revision`, `academic_structure`, `academic_structure_node`, `programme`, `curriculum`, `curriculum_version`, `curriculum_component`, `school_academic_model`, `school_grade_level` |
| 2C | `academic_calendar`, `calendar_version`, `academic_year`, `academic_term`, `calendar_key_date`, `calendar_assignment`, `calendar_exception`, `calendar_simulation`, `academic_year_closure` |

A slice may add more tables with its own prefix (2A: `institution_*` / `org_*` / `position_*` /
`governance_*` / `activation_*`; 2B: `campus_*` / `academic_structure_*` / `programme_*` /
`curriculum_*` / `school_*`; 2C: `academic_calendar*` / `calendar_*` / `academic_year*` / `academic_term*`).

## 4. Kernel and contract types (already in the code)

### 4.1 Ids: `sutra_contracts::institution::ids`

`InstitutionId`/`TenantId` (re-exported from kernel scope), plus:
- **2A:** `ActivationRequestId`, `InstitutionPolicyId`, `OrgUnitId`, `OrgTypeBlueprintId`, `GovernanceConfigId`, `OrgRestructureId`, `PositionId`
- **2B:** `CampusId`, `AcademicStructureId`, `ProgrammeId`, `CurriculumId`, `CurriculumVersionId`, `SchoolAcademicModelId`
- **2C:** `AcademicCalendarId`, `CalendarVersionId`, `AcademicYearId`, `AcademicTermId`, `KeyDateId`, `CalendarAssignmentId`, `CalendarExceptionId`, `CalendarSimulationId`

All are `typed_id!` newtypes: UUIDv7, serde-transparent, OpenAPI `string/uuid`.

### 4.2 Lifecycle states: `sutra_contracts::institution::status`

These are enums with `ALL`, `as_str()`, `parse()` and serde/OpenAPI `snake_case`. They are also
the DB `CHECK` values. Allowed **transitions** are slice-owned domain rules, written as a pure,
unit-tested `can_transition(from, to)` in the slice's `domain.rs`. An illegal transition returns
409 `conflict`.

| Enum | Values | Used by |
|------|--------|---------|
| `InstitutionStatus` | `draft, configured, pending_approval, returned, rejected, active, suspended, archived` | 2A |
| `PolicyStatus` | `draft, effective, superseded` | 2A policies |
| `OrgUnitStatus` | `draft, active, suspended, decommissioned, archived` | 2A |
| `PositionStatus` | `draft, active, retired, archived` | 2A |
| `CampusStatus` | `registered, pending_activation, active, suspended, decommissioned, archived` | 2B |
| `PublicationStatus` | `draft, in_review, published, superseded, retired` | 2A blueprints/governance; 2B structure, programme, curriculum version, school model; 2C calendar version |
| `AcademicYearStatus` | `planned, open, closing, closed` | 2C |
| `CalendarAssignmentStatus` | `draft, effective, retired` | 2C |
| `CalendarExceptionStatus` | `requested, approved, rejected, in_effect, completed, withdrawn` | 2C |
| `SimulationStatus` | `draft, evaluated, accepted, discarded` | 2C |

Institution lifecycle (L2-B01 §6): `draft → configured → pending_approval → {active | returned |
rejected}`; `returned → configured` (after edit, re-submit); `rejected → configured` (a new request
cycle, never directly active); `active ⇄ suspended`; `active | suspended → archived` (terminal,
read-only). Published versions are **immutable**. A change creates a successor `draft` (with
`supersedes_id`). Publishing it sets the predecessor to `superseded` in the same transaction.

### 4.3 Events: `sutra_contracts::institution::events`

`type = sutraerp.institution.<aggregate>.<verb>`, `dataschema = urn:sutraerp:schema:institution.<aggregate>.<verb>:1`,
`source = /sutraerp/institution`, envelope `subject` = aggregate id, envelope scope =
`ExecutionScope::institution(tenant, institution_id)`. Emit via `shared::publish::event` inside the
owner-local tx, and audit via `shared::publish::audit` with action `institution.<aggregate>.<command>`.
V1 payloads are frozen: additive optional fields only.

Shared payloads (in `events/mod.rs`): **`StatusChangedV1`** `{aggregateId, institutionId, fromStatus,
toStatus, version, reason?, processInstanceId?}` for every lifecycle transition, and **`ModifiedV1`**
`{aggregateId, institutionId, version, changedFields[]}` for attribute changes.

| Slice / file | Constant → `type` suffix | Payload |
|--------------|--------------------------|---------|
| (Batch 0) `mod.rs` | `INSTITUTION_REGISTERED` `institution.registered`, `INSTITUTION_RENAMED` `institution.renamed` | `InstitutionRegisteredV1`, `InstitutionRenamedV1` |
| 2A `lifecycle.rs` | `INSTITUTION_CONFIGURED` `.configured`, `INSTITUTION_SUBMITTED_FOR_APPROVAL` `.submitted_for_approval`, `INSTITUTION_RETURNED_FOR_CORRECTION` `.returned_for_correction`, `INSTITUTION_REJECTED` `.rejected`, `INSTITUTION_ACTIVATED` `.activated`, `INSTITUTION_SUSPENDED` `.suspended`, `INSTITUTION_REACTIVATED` `.reactivated`, `INSTITUTION_ARCHIVED` `.archived` (all `institution.*`) | `StatusChangedV1` |
| | `INSTITUTION_PROFILE_REVISED` `institution.profile_revised` | `ModifiedV1` |
| | `INSTITUTION_TIMEZONE_CHANGED` `institution.timezone_changed` | `InstitutionTimezoneChangedV1 {institutionId, timezone, previousTimezone, version}` |
| | `INSTITUTION_POLICY_REVISED` `institution_policy.revised` | `InstitutionPolicyRevisedV1 {institutionId, policyId, policyKey, revision, effectiveFrom}` |
| 2A `org.rs` | `ORG_UNIT_REGISTERED` `org_unit.registered` | `OrgUnitRegisteredV1 {orgUnitId, institutionId, parentId?, unitType, code, name}` |
| | `ORG_UNIT_ACTIVATED/_SUSPENDED/_RESTORED/_DECOMMISSIONED/_ARCHIVED` `org_unit.<verb>` | `StatusChangedV1` |
| | `ORG_UNIT_MODIFIED` `org_unit.modified` | `ModifiedV1` |
| | `ORG_STRUCTURE_RESTRUCTURED` `org_structure.restructured` | `OrgStructureRestructuredV1 {restructureId, institutionId, moves[{orgUnitId, fromParentId?, toParentId?}], decommissionedUnitIds[], effectiveFrom, processInstanceId?}` |
| | `ORG_TYPE_BLUEPRINT_PUBLISHED` `org_type_blueprint.published` | `OrgTypeBlueprintPublishedV1 {blueprintId, institutionId, versionNo, unitTypes[], effectiveFrom}` |
| | `GOVERNANCE_CONFIG_ACTIVATED` `governance_config.activated` | `GovernanceConfigActivatedV1 {governanceConfigId, orgUnitId, institutionId, revision, effectiveFrom}` |
| | `POSITION_ESTABLISHED` `position.established`, `POSITION_CHANGED` `position.changed` | `PositionDefinitionV1 {positionId, institutionId, orgUnitId, code, title, revision, effectiveFrom, effectiveTo?}` |
| | `POSITION_ACTIVATED/_RETIRED/_ARCHIVED` `position.<verb>` | `StatusChangedV1` |
| 2B `campus.rs` | `CAMPUS_REGISTERED` `campus.registered` | `CampusRegisteredV1 {campusId, institutionId, code, name, timezone?, isMain}` |
| | `CAMPUS_ACTIVATED/_SUSPENDED/_RESTORED/_DECOMMISSIONED/_ARCHIVED` `campus.<verb>` | `StatusChangedV1` |
| | `CAMPUS_MODIFIED` `campus.modified` | `ModifiedV1` |
| 2B `curriculum.rs` | `ACADEMIC_STRUCTURE_PUBLISHED` `academic_structure.published` | `AcademicStructurePublishedV1 {academicStructureId, institutionId, versionNo, supersedesId?, nodeCount}` |
| | `ACADEMIC_STRUCTURE_RETIRED`, `PROGRAMME_RETIRED`, `CURRICULUM_VERSION_RETIRED`, `CURRICULUM_RETIRED` | `StatusChangedV1` |
| | `PROGRAMME_REGISTERED` `programme.registered`, `PROGRAMME_PUBLISHED` `programme.published` | `ProgrammeV1 {programmeId, institutionId, orgUnitId?, code, name, level, durationTerms, version}` |
| | `CURRICULUM_VERSION_PUBLISHED` `curriculum_version.published` | `CurriculumVersionPublishedV1 {curriculumId, curriculumVersionId, programmeId, institutionId, versionNo, supersedesVersionId?, effectiveFrom, totalCredits?, processInstanceId?}` |
| | `SCHOOL_MODEL_PUBLISHED` `school_model.published` | `SchoolAcademicModelPublishedV1 {schoolModelId, institutionId, versionNo, gradeLevels[], board?}` |
| 2C `calendar.rs` | `CALENDAR_VERSION_PUBLISHED` `calendar_version.published` | `CalendarVersionPublishedV1 {calendarId, calendarVersionId, institutionId, academicYearId, versionNo, supersedesVersionId?, startsOn, endsOn, timezone, processInstanceId?}` |
| | `ACADEMIC_YEAR_OPENED` `academic_year.opened`, `ACADEMIC_YEAR_CLOSED` `academic_year.closed` | `AcademicYearV1 {academicYearId, institutionId, calendarId, label, startsOn, endsOn, successorYearId?}` |
| | `CALENDAR_ASSIGNMENT_EFFECTIVE` `calendar_assignment.effective` | `CalendarAssignmentV1 {assignmentId, calendarId, calendarVersionId, institutionId, scopeKind: institution\|campus\|org_unit\|programme, scopeId, effectiveFrom, effectiveTo?}` |
| | `CALENDAR_EXCEPTION_APPROVED` `calendar_exception.approved` | `CalendarExceptionV1 {exceptionId, calendarId, institutionId, kind, startsOn, endsOn, reason, processInstanceId?}` |
| | `CALENDAR_RETIRED` `calendar.retired`, `CALENDAR_VERSION_RETIRED` `calendar_version.retired` (added by 2C, L4-B-033-BH-008: version retired, history kept), `CALENDAR_ASSIGNMENT_RETIRED` `calendar_assignment.retired`, `CALENDAR_EXCEPTION_COMPLETED` `calendar_exception.completed` | `StatusChangedV1` |

Each file exports `EVENT_TYPES`, and `events::all_event_types()` lists every one; a unit test keeps
them unique and well formed. A slice may add an event to its own file (constant + entry in
`EVENT_TYPES` + payload) and must add it to the table above in its PR. Calendar change simulation
publishes **no** event: a simulation is a draft analysis, and only the decision it leads to (a
revision publication) is an event.

### 4.4 Permissions and role mapping (`sutra_kernel::auth::Permission`)

Every **read** of a Domain B resource requires `institution.read` (all roles). The write
permissions are granted to **Administrator only**. Faculty and Student hold none of them; the
unit test `batch2_domain_b_writes_are_administrator_only` checks this.

| Variant | Name | Step-up | Slice: used for |
|---------|------|---------|-----------------|
| `InstitutionManage` | `institution.manage` | no | 2A: profile, timezone, configure, submit for activation |
| `InstitutionLifecycle` | `institution.lifecycle` | **yes** | 2A: suspend, reactivate, archive |
| `InstitutionActivationDecide` | `institution.activation.decide` | **yes** | 2A: approve / reject / return an activation request (Institution Activation Authority) |
| `InstitutionPolicyManage` | `institution.policy.manage` | no | 2A: policies |
| `OrganisationManage` | `organisation.manage` | no | 2A: org units, blueprints, governance, restructure |
| `PositionManage` | `position.manage` | no | 2A: positions |
| `CampusManage` | `campus.manage` | no | 2B: register, activate, modify, suspend, restore |
| `CampusDecommission` | `campus.decommission` | **yes** | 2B: decommission, archive |
| `CurriculumManage` | `curriculum.manage` | no | 2B: drafts (structure, programmes, curricula, school model) |
| `CurriculumPublish` | `curriculum.publish` | no | 2B: publish, supersede, retire |
| `CalendarManage` | `calendar.manage` | no | 2C: drafts (calendars, years, terms, key dates, assignments, exceptions, simulations) |
| `CalendarPublish` | `calendar.publish` | no | 2C: publish, open/close year, approve exceptions, retire |
| `InstitutionReadinessRead` | `institution.readiness.read` | no | 2C: readiness checklist |

Step-up is only enforced when the deployment enables it (`AuthContext::step_up_enforced`). DB
tests run in dev mode without it.

## 5. HTTP API

### 5.1 Conventions (as in the reference aggregate)

- Every handler starts with `auth.require(Permission::…)?`, then `shared::scope::institution_scope(ctx, institution_id)`.
- Create: `POST`, required `Idempotency-Key` (`require_idempotency_key`), `201` + `ETag`.
- Update or state command: `If-Match` required (`shared::http::if_match`): `428` if missing, `412` on stale. The response is the updated resource + `ETag`.
- State commands are `POST …/{id}/<verb>` (`/activate`, `/suspend`, `/restore`, `/decommission`, `/archive`, `/publish`, `/retire`, `/submit`, `/decision`) with an optional `{ "reason": "…" }` body.
- Lists: `GET` collection under the parent, `?status=` filter, no paging in Batch 2 (`{ "items": [...] }`).
- Errors use the kernel taxonomy: illegal transition or structure locked → `409 conflict`; validation → `422`; foreign tenant → `404`.
- JSON members are camelCase. Statuses are the `snake_case` wire names of §4.2. Business dates are `YYYY-MM-DD`.
- **Every `#[utoipa::path]` sets an explicit `operation_id`** with the slice prefix below, plus the slice's `tag`. Utoipa otherwise uses the fn name (`list`, `get`, …), and the slices would collide in the OpenAPI document.
- Before writing to any downstream table, call `shared::guard::lock_institution_for_change(&mut tx, …)` in the transaction (suspended/archived institution → 409).

### 5.2 Path prefixes, operation_id prefixes, tags

| Slice | Paths (all under `/api/v1/institution`) | operation_id prefixes | tag |
|-------|------------------------------------------|-----------------------|-----|
| 2A | `/institutions` (root CRUD, existing), `/institutions/{id}/{configure,submit,decision,suspend,reactivate,archive}`, `/institutions/{id}/profile`, `/institutions/{id}/timezone`, `/institutions/{id}/activation-requests`, `/institutions/{id}/policies`, `/policies/{policyId}[/…]` | `institution_`, `institution_policy_` (existing `register`, `get_institution`, `rename` stay) | `institution` |
| 2A | `/institutions/{id}/org-units`, `/org-units/{orgUnitId}[/…]`, `/institutions/{id}/org-type-blueprints`, `/org-type-blueprints/{id}[/…]`, `/org-units/{id}/governance-configs`, `/governance-configs/{id}[/…]`, `/institutions/{id}/org-restructures`, `/org-restructures/{id}[/…]`, `/institutions/{id}/positions`, `/positions/{positionId}[/…]` | `org_unit_`, `org_type_blueprint_`, `governance_config_`, `org_restructure_`, `position_` | `organisation` |
| 2B | `/institutions/{id}/campuses`, `/campuses/{campusId}[/…]` | `campus_` | `campuses` |
| 2B | `/institutions/{id}/academic-structures`, `/academic-structures/{id}[/…]`, `/institutions/{id}/programmes`, `/programmes/{programmeId}[/…]`, `/programmes/{id}/curricula`, `/curricula/{curriculumId}[/…]`, `/curriculum-versions/{versionId}[/…]`, `/institutions/{id}/school-models`, `/school-models/{id}[/…]` | `academic_structure_`, `programme_`, `curriculum_`, `school_model_` | `curriculum` |
| 2C | `/institutions/{id}/calendars`, `/calendars/{calendarId}[/…]`, `/calendar-versions/{versionId}[/…]`, `/institutions/{id}/academic-years`, `/academic-years/{yearId}[/…]` (terms + key dates nested under the calendar version), `/calendar-assignments/{id}[/…]`, `/calendar-exceptions/{id}[/…]`, `/calendar-simulations/{id}[/…]` | `calendar_`, `academic_year_`, `calendar_assignment_`, `calendar_exception_`, `calendar_simulation_` | `calendar` |
| 2C | `GET /institutions/{id}/activation-readiness` → `ReadinessReport` | `readiness_` | `readiness` |

The router mount points are already registered: `sutra_institution::router` merges
`http`, `lifecycle`, `org`, `position` (2A), `campus`, `curriculum` (2B), `calendar`, `readiness` (2C).
Each slice only fills `pub fn router(state: InstitutionState) -> OpenApiRouter` in its own `mod.rs`.

**As integrated:** 2A names every single-resource path parameter `{id}` (`/policies/{id}`,
`/org-units/{id}`, `/positions/{id}`, …) instead of `{policyId}` / `{orgUnitId}` / `{positionId}`:
axum refuses two different parameter names on sibling segments (`/institutions/{id}/…` vs
`/institutions/{institutionId}/…`). 2B/2C keep the named parameters. Curriculum successor versions are
created with `POST /curricula/{curriculumId}/revisions`; every state command is its own literal path
(`…/{id}/activate`, `…/suspend`, …), there is no generic `/{command}` route.

### 5.3 Minimum response fields the 2C screens rely on

Slices may add fields but must keep these:
- **Institution** (2A extends the existing DTO): `id, tenantId, code, name, timezone, status, version, createdAt, updatedAt` (+ profile fields).
- **OrgUnit:** `id, institutionId, parentId, unitType, code, name, status, version`. **Position:** `id, institutionId, orgUnitId, code, title, status, revision, effectiveFrom, effectiveTo, version`. **Policy:** `id, institutionId, key, title, status, revision, effectiveFrom, version`.
- **Campus:** `id, institutionId, code, name, isMain, timezone, status, version`.
- **AcademicStructure / Programme / CurriculumVersion / SchoolModel:** `id, institutionId, status, versionNo` (programme: `code, name, level, durationTerms, orgUnitId`; curriculum version: `curriculumId, programmeId, effectiveFrom, supersedesVersionId`).
- **CalendarVersion:** `id, calendarId, institutionId, academicYearId, versionNo, status, startsOn, endsOn, timezone`. **AcademicYear:** `id, label, status, startsOn, endsOn`.
- Collections: `{ "items": [ … ] }`.

## 6. Cross-slice runtime rules (`modules/institution/src/shared/`)

- **Status guard** (`shared::guard`): downstream structure may be created or changed while the institution is `draft | configured | pending_approval | returned | rejected | active`. Setup happens **before** activation and readiness aggregates it. It is read-only while `suspended | archived`.
- **Scope** (`shared::scope`): tenant always comes from the session. Domain B administration is tenant-wide (an admin acts on any institution of their tenant, as in Batch 0). The path's institution id becomes the event/audit scope. Every query filters on `tenant_id`.
- **Business timezone:** `institution.timezone` (IANA, default `Asia/Kolkata`) governs every business-local date. 2A owns changing it (`institution.timezone_changed`). A campus may carry an override (2B). 2C snapshots the zone into each published calendar version.
- **Helpers:** `shared::http::{with_etag, if_match, actor}`, `shared::publish::{event, audit}`, `shared::Actor`.

## 7. Activation readiness (2C reads 2A/2B/2C state)

Readiness is **computed on read** (L2-B01: activation creates no authoritative readiness state).
`shared::readiness::evaluate(pool, scope, institution_id)` (foundation) concatenates three
slice-owned providers. Each provider returns **all of its keys every time** as `ReadinessItem
{key, area, label, required, status: met|unmet|not_applicable, detail?, link?}`:

| Provider | Keys (required unless marked advisory) |
|----------|-----------------------------------------|
| 2A `lifecycle/readiness.rs` | `institution.profile_complete`, `institution.timezone_set`, `institution.policies_effective` (advisory), `organisation.root_unit_active`, `organisation.governance_active` (advisory), `position.definitions_active` (advisory) |
| 2B `campus/readiness.rs` | `campus.active_campus` (≥ 1 active campus), `curriculum.academic_structure_published`, `curriculum.programme_published` (≥ 1), `curriculum.curriculum_published` (≥ 1 published version), `curriculum.school_model_published` (`not_applicable` unless the institution is a school) |
| 2C `calendar/readiness.rs` | `calendar.version_published`, `calendar.academic_year_open` (open, or planned and starting within the version), `calendar.assignment_effective` (institution-level assignment effective) |

- **2C** serves `GET /institutions/{id}/activation-readiness` (`institution.readiness.read`) returning `ReadinessReport {institutionId, institutionStatus, ready, items[], evaluatedAt}`, and renders it in Campus setup → *Activation readiness*.
- **2A**'s `submit` (configured → pending_approval) calls `evaluate` and refuses with `409 conflict` listing `blocking_keys()` when `ready` is false.
- **Events that change readiness:** `institution.{configured, profile_revised, timezone_changed, activated, suspended, reactivated, archived}`, `institution_policy.revised`, `org_unit.{registered, activated, suspended, restored, decommissioned}`, `governance_config.activated`, `position.{activated, retired}`, `campus.{registered, activated, suspended, restored, decommissioned}`, `academic_structure.{published, retired}`, `programme.{published, retired}`, `curriculum_version.{published, retired}`, `curriculum.retired`, `school_model.published`, `calendar_version.{published, retired}`, `calendar.retired`, `academic_year.{opened, closed}`, `calendar_assignment.{effective, retired}`. These types are listed for reference: readiness is read live, so nothing has to subscribe to them, and the web refetches the checklist after any of its own mutations. Inbox subscriptions stay **one owner per type**: only the slice that publishes an event lists it in its inbox rule file (unit test `slices_never_claim_the_same_type`). 2C therefore does not subscribe to 2A/2B types.

## 8. Inbox consumer hooks (`modules/platform/src/inbox/rules/`)

`consumer::plan` first calls `rules::plan(env)`, which dispatches by event type to the owning
file. The subscribed types are `consumer::EVENT_TYPES` + `rules::event_types()`. Each rule file
declares `EVENT_TYPES` (add a type only **with** its rule: the consumer writes a receipt for every
listed type, so a type listed without a rule is marked `ignored` for good) and a pure `plan` built
with `rules::notify(...)` / `todos::SystemTodo`. Add unit tests in the same file.

Suggested projections (all optional, category `institution`, recipients `Role::Administrator`):

| Slice | Types | Projection |
|-------|-------|------------|
| 2A | `institution.submitted_for_approval` | to-do for the activation authority (admins) "Decide activation of X" |
| 2A | `institution.activated / returned_for_correction / rejected / suspended / archived`, `org_structure.restructured` | notification |
| 2B | `campus.activated / decommissioned`, `curriculum_version.published` | notification |
| 2C | `calendar_version.published`, `academic_year.opened / closed`, `calendar_exception.approved` | notification (exceptions also to Faculty + Student) |

## 9. WorkflowPort usage for approval steps

`shared::approval` (foundation) wraps `sutra_contracts::platform::workflow::WorkflowPort`. The
composition root passes the platform `NoopWorkflowAdapter`, and `InstitutionState.approvals =
ApprovalMode::AutoApprove`.

1. Validate the command and current state (read only).
2. `request_approval(&state, &scope, process::<KEY>, business_key(aggregate, id, version), vars)` **before** opening the tx (deterministic business key → retry-safe).
3. One owner-local tx: re-check the version; set the pending state (`pending_approval` / `pending_activation` / `in_review` / `requested`); store `process_instance_id`; emit the *submitted* event. If the outcome is `ApprovalOutcome::Approved`, **also** apply the decision in the same tx: final state + decision event with `processInstanceId`, audit actor `system:auto-approval` (`shared::Actor::system`).
4. Also expose the explicit decision endpoint (for example `POST /institutions/{id}/decision {decision: approve|reject|return, reason}` with `institution.activation.decide`) so that `ApprovalMode::AwaitDecision` (Flowable, Batch 9) needs no API change. Tests may build an `InstitutionState` with `AwaitDecision` to cover it.

Process keys (`shared::approval::process`): 2A `INSTITUTION_ACTIVATION`, `ORG_RESTRUCTURE`,
`ORG_TYPE_BLUEPRINT_PUBLICATION`; 2B `CAMPUS_ACTIVATION`, `CAMPUS_DECOMMISSION`,
`CURRICULUM_PUBLICATION`, `ACADEMIC_STRUCTURE_PUBLICATION`, `SCHOOL_MODEL_PUBLICATION`; 2C
`CALENDAR_PUBLICATION`, `CALENDAR_EXCEPTION`, `ACADEMIC_YEAR_CLOSURE`.

## 10. Web (slice 2C)

| Route | File | Content |
|-------|------|---------|
| `/academics/:tab?` | `apps/web/app/routes/admin/academics.tsx` | tabs `structure`, `programmes`, `curriculum`, `school-model`, `calendar` (2B + 2C APIs) |
| `/campus/:tab?` | `apps/web/app/routes/admin/campus-setup.tsx` | tabs `institution` (2A lifecycle/profile/timezone), `readiness`, `campuses` (2B), `organisation`, `positions`, `policies` (2A) |

Both routes are registered in `routes.ts` (before `:screen`). Tab definitions are in
`apps/web/app/features/institution/index.ts`. Put one component per tab in that folder (for
example `CampusesTab.tsx`), with API hooks in `features/institution/api.ts`, and use the typed client
`@sutra/api-client` after regeneration. 2C develops against the §5 contract. Until 2A/2B are
merged, it types those calls locally in `features/institution/types.ts`, then switches to the
generated types at integration. Screens are administrator-only. Other roles see "not available".
`/campus` stays the admin Campus entry; Domain G (Batch 8) adds its facilities tabs later.

## 11. Worker environment: separate DB and target dir per slice

| Slice | Worktree | Branch | Database | `CARGO_TARGET_DIR` |
|-------|----------|--------|----------|--------------------|
| 2A | `/workspace/work/erp-2a` | `batch2/2a` | `postgres://sutra:sutra@localhost:5432/sutra_2a` | `/tmp/target-2a` |
| 2B | `/workspace/work/erp-2b` | `batch2/2b` | `postgres://sutra:sutra@localhost:5432/sutra_2b` | `/tmp/target-2b` |
| 2C | `/workspace/work/erp-2c` | `batch2/2c` | `postgres://sutra:sutra@localhost:5432/sutra_2c` | `/tmp/target-2c` |

The three databases already exist (owner `sutra`) and are migrated to the foundation. The runtime
login role `sutra_it_runtime` used by the DB tests is cluster-wide, and its provisioning is
idempotent, so it is safe across the three databases. Run DB tests with `-j 2` (linking several
test binaries at once can OOM a 7 GB sandbox):

```sh
cd /workspace/work/erp-2a/sutraerp
export CARGO_TARGET_DIR=/tmp/target-2a DATABASE_URL=postgres://sutra:sutra@localhost:5432/sutra_2a
cargo test -j 2 -p sutra-api --features db-tests --test db_b2a
```

**Disk:** the sandbox root (`/`, which holds `/tmp`) is about 6.3 GB, and one debug target dir is
about 2–2.5 GB. Three `/tmp/target-2x` dirs at once do **not** fit next to `/tmp/target-main`.
If the slices share this sandbox, either run them one after another, or point
`CARGO_TARGET_DIR` at `/workspace/work/targets/2x` (large but slower JuiceFS). Delete stale
target dirs first.

Quality gates per slice before pushing: `cargo fmt --all -- --check`, `cargo clippy --workspace
--all-targets --all-features -- -D warnings`, `cargo test --workspace --locked`, the DB tests above
plus `--test db` (existing), `make openapi` with no drift after commit, `pnpm -r typecheck && pnpm lint
&& pnpm -r test`. Commit `[skip ci]` as `Varad1614 <Varad1614@users.noreply.github.com>`.

## 12. Merge order and known gaps

- Merge order: 2A → 2B → 2C (2C's readiness and screens consume the other two). Regenerate OpenAPI/TS after each merge (§2.3). **Done** (Batch 2 integration): all three providers are live in `evaluate`; `apps/api/tests/support/b2.rs` sets up the 2B/2C readiness items through the API for `db_b2a.rs` and `db_b2_integration.rs`.
- Integration additions: campus decommissioning (2B) also refuses while a non-retired campus-scoped `calendar_assignment` (2C table, read only) governs the campus; calendar assignment scopes are validated against the real `campus` / `org_unit` / `programme` tables.
- Session pinning to one institution is **not** enforced for Domain B in Batch 2 (dev sessions carry a placeholder institution id). This is hardening for a later batch.
- `ApprovalMode::AwaitDecision` is wired but unused until the Flowable adapter lands (Batch 9).
