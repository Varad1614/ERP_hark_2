# Batch 8 — shared contracts (Campus · Services · Resources, Domains G, H, J)

**Status:** frozen at `batch8/foundation`. This file is the single source of truth for the four
slices **8A**, **8B**, **8C** and **8D**. If a slice needs to change anything in §2–§11, it says so
in its PR description ("contract change: …") instead of silently editing a shared file; the
integrator applies it on top of the merge.

Architecture inputs: L2-G00 (+ amendments), L2-G01 facilities, L2-G02 laboratories, L2-G03 hostel,
L2-G04 transport, L2-G05 library, L2-G10 infrastructure (+ amendments); L2-H00, L2-H07 dashboards
(+ amendments), L2-H08 regulatory reporting (+ amendments); L2-J00 … L2-J08; L3/L4 G / H / J (in-scope
list §1; one L4 spec per L3 scenario under `architecture/L4_System_Behavior_Specification/01_Production/section-{g,h,j}`);
Batch 5 deferrals F-044, F-045, F-076, F-081, F-082 (`docs/batch5-contracts.md` §1); ADR-L5-0003
(module boundaries), -0004 (scope), -0006/-0007 (owner-local tx + outbox), -0011/-0035 (event
versioning / envelope), -0023/-0042 (WorkflowPort / Flowable), -0032 (decimals: `rust_decimal`), -0034
(API profile), -0036 (schemas, business timezone); L6 workspaces W-G01 … W-G05, W-G10, W-H07, W-H08,
W-J01 … W-J08; screens **Student Library**, **Admin Campus**, **Admin Reports**.

Plan line (Batch 8): *Facilities and rooms, hostel, transport, maintenance, service desk. Library and
learning resources, procurement and inventory, reporting and analytics. Screens: Student Library;
Admin Campus and Reports.*

**Lessons from Batches 3–7 applied here:** every cross-slice and cross-domain dependency is a **typed
port or foundation helper defined up front** (§6) with a real implementation already wired, so no
slice waits for another; the shared rows (infrastructure nodes, service tickets, the stock ledger)
have **one** write path each (`shared::{infrastructure, tickets, stock}`); money and quantities are
`rust_decimal` everywhere (no floats, ever; strings on the wire); **CI-only**: no local cargo at all
(§11).

---

## 1. Slices and scenario coverage (one wave: 8A + 8B + 8C + 8D in parallel)

**120 of 505 L3 scenarios in scope (24 %)** + 5 Batch 5 finance deferrals — the core flows the plan
names, end to end (see `/workspace/work/brief/batch8-plan.md` for the narrative).

| Slice | Scope | Scenarios |
|-------|-------|-----------|
| **8A** (30) | **Campus infrastructure, rooms, labs, maintenance & service desk** (`sutra-campus`). Infrastructure hierarchy campus site → building → floor → space / zone over the foundation table (registration, classification, capabilities, capacity and availability profiles, suspension / restoration, decommissioning), **space reservations / room booking** (request → confirmed, reschedule, cancel, availability; conflicts 409), **service desk** (service requests and complaints from self-service or the office → triage → assignment → in progress → resolved → verified closure; duplicates merged onto a canonical ticket; SLA monitoring + breach flag + escalation; administrative closure; `/me/tickets`), **work orders** (corrective from a ticket, technician assignment / reassignment, completion → verification, reopening), **preventive maintenance** plans → generated work orders per cycle, deferral / rescheduling, **labs** (lab profile of a `lab` space, readiness checklist, equipment status → lab availability, practical sessions planned → ready → completed, batch allocation with capacity check). Screens: Admin → Campus tabs `infrastructure`, `facilities`, `labs`. | G-016, 017, 018, 020, 021, 022, 023, 024, 026, 041, 042, 046, 050, 053, 057, 060, 278, 281, 282, 283, 284, 286, 287, 288, 289, 290, 291, 292, 293, 315 |
| **8B** (29) | **Hostel & transport** (`sutra-campus`). **Hostel**: policy versions (fees, curfew, eligibility; draft → active → superseded, `HOSTEL_POLICY_APPROVAL`), beds of `hostel_room` spaces, admission applications → decision (`HOSTEL_ADMISSION`) or waiting list when full → allocation = residency (active; **hostel fee through `CampusChargePort`**) → resident leave → verified return → room transfer → room inspection (damage → charge) → vacating / clearance → closed occupancy; mess allocation and change; hostel complaints open tickets through `shared::tickets::open_ticket` (source `hostel`). **Transport**: routes with effective-dated versions and ordered stops (draft → active → suspended / retired, `ROUTE_ACTIVATION`), stops (active / suspended → route adjustment), vehicles (availability, breakdown → replacement), driver assignments, passenger requests → allocation (route + stop) → change → withdrawal, **passes** (issue with a **transport fee through `CampusChargePort`**, renewal, lost → replacement, suspension), trips (planned → ready (vehicle + driver + checklist) → in progress → completed / cancelled). Screens: Admin → Campus tabs `hostel`, `transport`. | G-078, 079, 080, 081, 082, 087, 088, 089, 090, 091, 092, 093, 094, 095, 096, 097, 099, 100, 101, 102, 104, 105, 106, 108, 109, 113, 114, 115, 116 |
| **8C** (30 + F-076) | **Library + reports & analytics** (`sutra-campus` `library/` and `sutra-services`). **Library**: borrowing policy versions (loan days, limits per member kind, fine per day, reservation hold days; `LIBRARY_POLICY_APPROVAL`), memberships of students / staff (active, suspended, closed), catalogue records (draft → published, update), copies (accession numbers, available / issued / reserved / lost / damaged / withdrawn), **issue → return → renewal** (limits, restrictions, reservation priority; `library_loan.issued` = `ResourceIssued`), reservations (queue → ready → collected / expired), **overdue → fine** (daily rate × days; **billed to the student through `CampusChargePort`**, fee `LIB-FINE`, category `library`), fine waiver (`FINE_WAIVER`, before billing), lost copy → replacement obligation (charge), clearance (no loans, no unpaid fines → `LIBRARY_CLEARANCE`), borrowing restriction → resolution; `/me/library`. **Dashboards (H07)**: dashboard requirement → governed published dashboard (versions, rollback, `DASHBOARD_PUBLICATION`), widget definitions bound to metric keys, assignments to roles; operational reports G-154 (library) and G-320 (infrastructure portfolio). **Regulatory reporting (H08)**: frameworks (AISHE / NIRF / NAAC data definitions; `REPORTING_FRAMEWORK_APPROVAL`) → cycles + collection plans → snapshots (figures from `ServicesState::all_metrics`, validated) → submission packages (`SUBMISSION_APPROVAL`) → acknowledgement → correction / closure. Screens: **Student Library** `MyLibraryPanel`, Admin → Campus tab `library`, **Admin Reports** tabs `dashboards`, `regulatory`. | G-124 … 139, 149, 152, 154, 320; H-048, 050, 052, 054, 055 … 060; F-076 |
| **8D** (31 + F-044 / 045 / 081 / 082) | **Procurement, inventory, assets + Finance AP** (`sutra-resources`). Suppliers (prospective → registered → approved / active (`SUPPLIER_APPROVAL`) → suspended → restored; duplicates → canonical), purchase requisitions (draft → submitted → approved / released (`REQUISITION_APPROVAL`) / rejected; amendment; withdrawal / cancellation), sourcing (RFQ with quotations → evaluation → award (`AWARD_APPROVAL`); direct / simplified sourcing), purchase orders (issued from an award or released requisition (`PURCHASE_ORDER_APPROVAL`), supplier acknowledgement, amendment, cancellation, closure when fulfilled), goods receipts (delivery → inspection → accepted / rejected lines; accepted stock lines post `receipt` movements, accepted `asset` lines become asset candidates), item master + stores / locations (foundation tables, 8D writes), stock on hand, issues (`STOCK_ISSUE`), reservations, returns to store, approved adjustments (`STOCK_ADJUSTMENT_APPROVAL`), reorder level → replenishment need; asset register (registration from a receipt or manually, assignment / custody to a staff member or space, return, condition change, retirement `ASSET_RETIREMENT`); **vendor invoices** matched three-way against PO + receipt (F-081) → posted through **`ProcurementPostingPort`** (F-044 / F-082, `VENDOR_INVOICE_APPROVAL`) → **vendor payments** (F-045). Screens: Admin → Campus tabs `procurement`, `inventory`, `assets`. | J-004, 007, 020, 021, 030, 035, 038, 039, 040, 041, 043, 044, 045, 046, 047, 049, 051, 053, 054, 057, 058, 059, 061, 064, 065, 066, 074, 075, 076, 079, 083; F-044, F-045, F-081, F-082 |

**Deferred (385 scenarios), recorded here and in `docs/batches.md`:**

| Scenarios | Area | Lands in |
|-----------|------|----------|
| G-001 … G-015 | Campus operations governance (W-G00) | Batch 9 (with H09) |
| G-019, 025, 027 … 040, 043 … 045, 047 … 049 | Facilities depth: multi work order, missed PM, root cause, repeat failure, emergency, vendors / AMC, utilities, inspections, consumables, costs, invalid references, conflicts, configuration | 8E later |
| G-051, 052, 054 … 056, 058, 059, 061 … 077 | Laboratory depth (warnings, unavailability, postponement, cancellation, conflicts, shared labs, equipment restriction / usage, safety, incidents, inspections, compliance, consumables, emergencies, configuration) | 8E later |
| G-083 … 086, G-098, 103, 107, 110 … 112, 117 … 119, 121 … 123 | Hostel curfew / visitors / discipline; transport sequencing, driver replacement, deviations, vehicle issues, checklists, diversions, permanent revisions, complaint follow-ups, configuration | 8E later |
| G-140 … 148, 150, 151, 153 | Library damaged copy, acquisitions (→ J), receipt cataloguing, reading rooms, stock verification, clearance consumption, conflicts, configuration | 8E later |
| G-155 … G-185 | Health / medical services (G06) | later batch |
| G-186 … G-215 | Security operations (G07) | later batch |
| G-216 … G-251 | Housekeeping (G08) | later batch |
| G-252 … G-277 | Teacher guardian scheme (G09) | later batch |
| G-279, 280, 285, 294 … 314, 316 … 319 | Infrastructure depth (campus association, effective-dated attribute changes, zones, allocations, responsibilities, validation, compliance, maintenance governance, renovation, sustainability, DR, lifecycle, archival, boundary, correction) | 8E later |
| H-001 … H-047 | APAAR / DigiLocker, notifications (platform inbox covers delivery), search, placement, research, alumni | later batches |
| H-049, 051, 053 | Dashboard templates, sharing, retirement | 8E later |
| H-061 … H-081 | Accreditation & compliance (H09), external education (H10) | Batch 9 / later |
| J-001 … 003, 005, 006, 008 … 019 | Procurement plans, RFP / tender, consolidation / split, multi-supplier awards, rate contracts, single source, emergency, external, insufficient competition, cancellation | 8E later |
| J-022 … 029, 031 … 034, 036, 037, 042, 050, 052, 055, 056, 060, 062, 063, 067 … 073, 077, 078, 080 … 082, 084 … 104 | Supplier qualification / risk / blacklisting / closure, emergency PR, receipt correction, item archival, expiry / controlled stock, discrepancy, valuation, transfers / distribution / custody, asset location / transfer / missing / repair / verification / disposal / archival, physical verification (J09), disposal & write-off (J10) | 8E later |
| F-046 … F-050, F-067 … F-075, F-077 … F-080, F-083 | AP disputes / holds / adjustments / write-off / reconciliation / closure, asset accounting, financial statements and statutory reports, audit reconstruction, budget commitment, procurement closure | later batch |

Consciously deferred from the plan text: *learning resources* beyond the library are the Batch 4
course resources (already live under Student Library); *reporting* is operational dashboards +
regulatory cycles over metric projections, not financial statements (F-073 … 079 deferred).

## 2. Where the code lives: module and file ownership

**Decision:** Batch 8 fills the three Batch 0 scaffolds — **no new crate** (`apps/api/tests/architecture.rs`
fixes exactly ten domain crates A … J):

| Domain | Crate | Schema | Routes | Slices |
|--------|-------|--------|--------|--------|
| G | `modules/campus` (`sutra-campus`) | `campus` | `/api/v1/campus` | 8A, 8B, 8C (library) |
| H | `modules/services` (`sutra-services`) | `services` | `/api/v1/services` | 8C (reports) |
| J | `modules/resources` (`sutra-resources`) | `resources` | `/api/v1/resources` | 8D |

Each crate depends only on `sutra-kernel`, `sutra-contracts`, `sutra-web` and `sutra-persistence`
and **never** contains `FROM/JOIN/INTO/UPDATE <other schema>.` (architecture test). Domain B data
comes through `InstitutionReferencePort`, students through `StudentDirectory`, staff through
`WorkforceDirectory`, Finance through `CampusChargePort` / `ProcurementPostingPort`, other domains'
figures through `OperationalMetricsPort` (§6). Inside `campus`, a slice may read another slice's
tables **read-only** when a port would be pointless (same crate); it never writes them.

### 2.1 Ownership map (create/edit freely inside your own paths)

| Path | Owner |
|------|-------|
| `modules/campus/src/{infrastructure,reservations,labs,maintenance,servicedesk}/**` | **8A** |
| `modules/campus/src/{hostel,transport}/**` | **8B** |
| `modules/campus/src/library/**`, `modules/services/src/{dashboards,regulatory}/**` | **8C** |
| `modules/resources/src/{suppliers,requisitions,sourcing,orders,receipts,inventory,assets,payables}/**` | **8D** |
| `crates/contracts/src/campus/events/facilities.rs` | **8A** |
| `crates/contracts/src/campus/events/residence.rs` | **8B** |
| `crates/contracts/src/campus/events/library.rs`, `crates/contracts/src/services/events/reporting.rs` | **8C** |
| `crates/contracts/src/resources/events/procurement.rs` | **8D** |
| `modules/platform/src/inbox/rules/campus_8a.rs` / `campus_8b.rs` / `campus_8c.rs` + `services_8c.rs` / `resources_8d.rs` | 8A / 8B / 8C / 8D |
| `migrations/202611150000[1-2][0-9]_campus_*.sql` (`…010` – `…029`) | **8A** |
| `migrations/202611150000[3-4][0-9]_campus_*.sql` (`…030` – `…049`) | **8B** |
| `migrations/202611150000[5-6][0-9]_{campus,services}_*.sql` (`…050` – `…069`) | **8C** |
| `migrations/202611150000[7-8][0-9]_resources_*.sql` (`…070` – `…089`) | **8D** |
| `apps/api/tests/db_b8a.rs` / `db_b8b.rs` / `db_b8c.rs` / `db_b8d.rs` | 8A / 8B / 8C / 8D |
| `apps/web/app/features/campus/facilities/**` | **8A** |
| `apps/web/app/features/campus/residence/**` | **8B** |
| `apps/web/app/features/campus/library/**`, `apps/web/app/features/reports/analytics/**` | **8C** |
| `apps/web/app/features/campus/resources/**` | **8D** |
| `apps/web/e2e/campus.e2e.mjs` regions `// ==== 8X begin` … `// ==== 8X end` (created by the integrator from `people.e2e.mjs`; the integrator owns the header and the CI matrix entry) | each slice its own regions |
| README region `<!-- batch8:8X:begin … end -->`; `docs/batches.md` status cell `<!-- batch8:8X:status -->` | each slice its own region |

### 2.2 Shared files: foundation-owned, do not edit

`modules/{campus,services,resources}/src/lib.rs` (router lists already mount every slice),
`modules/{campus,services,resources}/src/shared/**`, `modules/{campus,services,resources}/src/directory/**`,
`modules/campus/src/{spaces,metrics}.rs`, `modules/resources/src/metrics.rs`,
`crates/contracts/src/{campus,services,resources}.rs`, `crates/contracts/src/{campus,resources}/{ids,status}.rs`,
`crates/contracts/src/services/{ids,ports}.rs`, `crates/contracts/src/{campus,services,resources}/events/mod.rs`,
`crates/contracts/src/finance/{ports,status}.rs` (Batch 8 blocks), `modules/finance/src/{campus_charges,procurement_postings,metrics}.rs`,
`modules/student/src/metrics.rs`, `modules/people/src/metrics.rs`, `crates/kernel/src/auth.rs` (all Batch 8
permissions already added), `modules/platform/src/inbox/rules/mod.rs`, `apps/api/src/{app,openapi}.rs`
(tags + `campus_deps` / `services_deps` / `resources_deps` already added), `apps/api/tests/architecture.rs`,
`apps/api/tests/support/{mod,b8}.rs`, `apps/api/tests/db_b8_foundation.rs`, the four foundation
migrations `20261115000000 … 003`, `apps/web/app/routes.ts`, `apps/web/app/lib/{roles,workspace-screens}.ts`,
`apps/web/app/features/campus/{index.ts,Placeholder.tsx,campus.test.tsx}`, `apps/web/app/features/reports/index.ts`,
`apps/web/app/routes/admin/{campus-setup,reports}.tsx`, `apps/web/app/routes/student/library.tsx`,
`apps/web/e2e/ui.e2e.mjs`, `.github/workflows/sutraerp-ci.yml`. Domain B, C, D, E, F and I code
(`modules/{institution,student,academic,people,finance,exams}/**`) is **off limits** to all four slices
(a needed change there is a contract change for the integrator).

If you cannot avoid touching one of these, keep the edit **append-only** at the end of the relevant
list/block, add a `// 8X:` (or `<!-- 8X: -->` / `-- 8X:`) comment, and list it in your PR description.
Examples: a new id at the end of your slice's block in `ids.rs`; a new number kind at the end of your
block in `shared::numbering::kind` (also add it to `kind::ALL`); a new process key in your block of
`shared::approval::process` (and `process::ALL`); a new variant in `status.rs` (contract change); a new
metric in a foundation metrics adapter (contract change).

### 2.3 Generated files

`openapi/openapi.json` and `packages/api-client/src/schema.d.ts` are **generated**. CI's `OpenAPI
drift` job regenerates them and uploads the artifact `openapi-regenerated`; **commit the artifact's
files** (§11) instead of generating locally. On merge never hand-merge them: take either side, then
regenerate. `Cargo.lock`: the foundation gave `sutra-campus`, `sutra-services` and `sutra-resources`
the same dependency set as `sutra-people` (sqlx, uuid, chrono, rust_decimal, serde_json, tracing,
persistence); a slice that adds a dependency must edit `Cargo.lock` by hand (the `dependencies`
list of its crate's `[[package]]` entry) because nobody can run cargo locally — say so in the PR.

## 3. Database

### 3.1 Migrations and numbers

| Range | Owner |
|-------|-------|
| `20261115000000_campus_batch8_foundation.sql` | foundation: `campus.infrastructure_node`, `campus.service_ticket`, `campus.number_sequence`, `campus.status_history`, `campus.forbid_history_change()`, no-delete / append-only triggers, restated `sutra_campus` grants |
| `20261115000001_services_batch8_foundation.sql` | foundation: `services.number_sequence`, `services.status_history`, `services.forbid_history_change()` |
| `20261115000002_resources_batch8_foundation.sql` | foundation: `resources.item`, `resources.store_location`, `resources.stock_ledger_entry` (append-only), `resources.number_sequence`, `resources.status_history` |
| `20261115000003_finance_campus_charge_source.sql` | foundation (contract change to Domain F): charge `source_type` `campus_charge` + unique index per source |
| `20261115000010` … `029` | 8A (`campus.` space capability, availability profile, reservation, work order (+ assignment history), maintenance plan / cycle, lab, lab equipment, practical session, lab batch allocation, ticket escalation / SLA events) |
| `20261115000030` … `049` | 8B (`campus.` hostel policy, bed, application, allocation, resident leave, room transfer, inspection, mess, mess allocation; transport route, route version, route stop, stop, vehicle, driver assignment, allocation, pass, trip) |
| `20261115000050` … `069` | 8C (`campus.` library policy, membership, catalogue record, copy, loan, reservation, fine, clearance; `services.` dashboard, dashboard version, widget definition, dashboard assignment, reporting framework, cycle, snapshot (+ values), submission package) |
| `20261115000070` … `089` | 8D (`resources.` supplier, requisition (+ lines), sourcing event, quotation, purchase order (+ lines), goods receipt (+ lines), stock issue, stock reservation, stock return, stock adjustment, replenishment need, asset, asset assignment, vendor invoice (+ lines), vendor payment (+ allocations)) |

Base `20261115…` is after the last Batch 7 migration (`20261110000050_people_payroll.sql`). Reserved
names are **not** pre-created (sqlx checksums applied migrations). Name files
`<version>_<schema>_<what>.sql`. Never edit a migration after it has been applied anywhere; add the next
number. New tables pick up the `sutra_<schema>` runtime grants automatically (default privileges of
`20261014000003_runtime_db_roles.sql`); an append-only slice table additionally gets `REVOKE UPDATE,
DELETE, TRUNCATE ON <schema>.<t> FROM sutra_<schema>;` plus a `BEFORE UPDATE OR DELETE` trigger reusing
`<schema>.forbid_history_change()`.

**Decimal rules (all slices, ADR-L5-0032):** money `numeric(14,2)` (INR), rates / percentages
`numeric(7,4)`, quantities `numeric(14,3)`, hours `numeric(5,2)`, area `numeric(10,2)`; Rust uses
`rust_decimal::Decimal` end to end; **no `f32`/`f64`**. Computed money is rounded **once** per line
with `shared::decimal::round2` (half away from zero). On the wire money / quantities / rates are
**JSON strings** (`"30.00"`, `shared::decimal::fmt2`; quantities `resources::directory::http::fmt3`
`"12.500"`) with `#[schema(value_type = String, example = "30.00")]`.

Table conventions: `id uuid PRIMARY KEY DEFAULT uuidv7()`, `tenant_id uuid NOT NULL`,
`institution_id uuid NOT NULL` (plain uuid — **no FK into `institution`**), FKs to the **foundation**
tables of the same schema are allowed for every slice (`node_id … REFERENCES campus.infrastructure_node`,
`ticket_id … campus.service_ticket`, `item_id … resources.item`, `location_id …
resources.store_location`), other domains' ids (`campus_id`, `student_id`, `staff_id`, `charge_id`,
`journal_entry_id`) **plain** (validate by reading through the port), persons as `(person_kind,
person_id)` (`PersonKind` + Domain C student id / Domain E staff id), `status text NOT NULL CHECK (status
IN (...))` with the wire names of §4.2, `version bigint NOT NULL DEFAULT 1`, `created_at/updated_at
timestamptz`, `process_instance_id text` on every aggregate with an approval step. Business dates
(reservation dates, due dates, trip dates, receipt dates) are `date` / `timestamptz` per meaning,
"today" via `shared::time::business_today` in the institution timezone. **No foreign keys across
slices** (plain uuid + validate by reading).

### 3.2 The shared foundation tables

- `campus.infrastructure_node` — the physical hierarchy: `campus_id` (Domain B, plain), `parent_id`
  (self FK), `node_type` (`site | building | floor | space | zone`; allowed parents
  `InfrastructureNodeType::allowed_parents`: building under site / zone, floor under building, space
  under floor / building / zone, zone under site / building / floor; only a site has no parent),
  `code` (unique per institution, `^[A-Z0-9][A-Z0-9_.-]{0,31}$`), `name`, `space_type` (spaces only:
  `classroom, lab, seminar_room, auditorium, office, hostel_room, library, reading_room, store,
  workshop, sports, other`), `capacity` (seats / beds), `floor_number`, `area_sq_m`, `attributes`
  jsonb, `status` (`InfrastructureStatus`), `effective_from`, `version`. No physical delete.
  **Who uses it:** 8A owns writes; 8B hostel rooms = `hostel_room` spaces (beds are 8B rows per
  room), 8B stops / garages may reference nodes; 8C reading rooms / library spaces; 8D stores
  (`store_location.campus_node_id`); Domain D timetable rooms via `TeachingSpacePort`.
- `campus.service_ticket` — the service desk: `number` (`SR-000001`), `category`
  (`TicketCategory`), `source` (`TicketSource`: `self_service, admin, hostel, transport, library,
  laboratory, preventive`), `source_ref_id` (the slice aggregate behind it), `is_complaint`,
  `node_id`, requester `(requester_kind, requester_id, requester_subject)`, `title`, `description`,
  `priority` (`TicketPriority`, default SLA 4 / 24 / 72 / 168 h), `status` (`TicketStatus`),
  `assignee`, `sla_due_at`, `sla_breached`, `escalation_level`, `resolved_at`, `closed_at`,
  `resolution`, `canonical_ticket_id` (duplicates), `version`.
- `resources.item` (item master: `code`, `kind` `consumable | asset | service`, `unit`,
  `reorder_level`, `reorder_quantity`, `standard_cost`, `status` `draft | active | inactive |
  archived`), `resources.store_location` (store = `parent_id IS NULL`, else a location inside it;
  `campus_node_id`; `active | inactive`), **append-only** `resources.stock_ledger_entry` (`item_id`,
  `location_id`, `kind` (`StockMovementKind`), signed `quantity numeric(14,3)` (sign by kind),
  `unit_cost`, `effective_on`, `source_type` / `source_id` / `posting_key` = **idempotency key**,
  `reverses_entry_id`, `reason`, actor). On hand = Σ quantity (`shared::stock::on_hand`).
- `<schema>.number_sequence` — gap-free per-institution counters (`shared::numbering`).
- `<schema>.status_history` — append-only: every status change of the shared aggregates and of
  slice aggregates (`shared::lifecycle::record_history`).

### 3.3 Write rules for the shared tables

| Change | Who | How |
|--------|-----|-----|
| INSERT infrastructure nodes | **8A** (8B / 8C / 8D never; tests seed with `support::b8`) | **only** `sutra_campus::shared::infrastructure::insert_node` (hierarchy rule, duplicate code → 409) |
| Edit node attributes (name, capacity, attributes, classification) | **8A** | own repo code with `infrastructure_node.modified` (`ModifiedV1`) while not `decommissioned | archived` |
| `infrastructure_node.status` | **8A** | **only** `shared::infrastructure::change_node_status` on a row locked with `shared::rows::lock_node` |
| INSERT service tickets | **every Campus slice** (8A self-service / office, 8B hostel + transport complaints, 8C library complaints) | **only** `shared::tickets::open_ticket` (numbering, SLA, history, `service_ticket.opened`, audit in the caller's tx) |
| `service_ticket.status`, assignee, escalation, resolution | **8A** | status **only** `shared::tickets::change_ticket_status` (`lock_ticket`); other columns in 8A repo code |
| `resources.item`, `resources.store_location` | **8D** | own repo code (tests seed with `support::b8::{seed_item, seed_store}`) |
| `resources.stock_ledger_entry` | **8D** | **only** `sutra_resources::shared::stock::{post_movement, reverse_movement}` (append-only, sign + scale + on-hand checks, idempotent) |
| Read nodes / tickets / stock | all | `shared::rows::{load_node, lock_node, load_ticket, lock_ticket, NODE_COLUMNS, node_from_row, TICKET_COLUMNS, ticket_from_row}`, `shared::stock::on_hand` |

### 3.4 Document numbers (`shared::numbering::kind`)

Campus: `service_ticket` `SR` (foundation); `reservation` `RSV`, `work_order` `WO` (8A);
`hostel_application` `HAP`, `transport_pass` `TP` (8B); `library_membership` `LM`, `library_copy`
`ACC`, `library_loan` `LN` (8C). Services: `reporting_cycle` `RC`, `submission_package` `SUB` (8C).
Resources: `supplier` `SUP`, `requisition` `PR`, `sourcing_event` `RFQ`, `purchase_order` `PO`,
`goods_receipt` `GRN`, `stock_issue` `SIV`, `asset` `AST`, `vendor_invoice` `VINV`, `vendor_payment`
`VPAY` (8D). Format `PREFIX-000001`, gap-free inside committed transactions.

### 3.5 Slice table names

| Slice | Tables (prefixes a slice may extend) |
|-------|--------------------------------------|
| 8A | `campus.space_capability`, `availability_profile`, `space_reservation`, `work_order`, `work_order_assignment`, `maintenance_plan`, `maintenance_cycle`, `lab`, `lab_equipment`, `practical_session`, `lab_batch_allocation`, `ticket_*` (prefixes `space_*`, `availability_*`, `reservation_*`, `work_order*`, `maintenance_*`, `lab*`, `practical_*`, `ticket_*`, `infrastructure_*` except the foundation table) |
| 8B | `campus.hostel_policy`, `hostel_bed`, `hostel_application`, `hostel_allocation`, `resident_leave`, `room_transfer`, `room_inspection`, `mess`, `mess_allocation`, `transport_route`, `route_version`, `route_stop`, `transport_stop`, `vehicle`, `driver_assignment`, `transport_allocation`, `transport_pass`, `trip` (prefixes `hostel_*`, `resident_*`, `room_*`, `mess*`, `transport_*`, `route_*`, `vehicle*`, `driver_*`, `trip*`) |
| 8C | `campus.library_policy`, `library_membership`, `catalogue_record`, `library_copy`, `library_loan`, `library_reservation`, `library_fine`, `library_clearance`; `services.dashboard`, `dashboard_version`, `widget_definition`, `dashboard_assignment`, `reporting_framework`, `reporting_cycle`, `report_snapshot`, `report_snapshot_value`, `submission_package` (prefixes `library_*`, `catalogue_*`; `dashboard*`, `widget_*`, `reporting_*`, `report_*`, `submission_*`) |
| 8D | `resources.supplier`, `purchase_requisition`, `purchase_requisition_line`, `sourcing_event`, `quotation`, `purchase_order`, `purchase_order_line`, `goods_receipt`, `goods_receipt_line`, `stock_issue`, `stock_reservation`, `stock_return`, `stock_adjustment`, `replenishment_need`, `asset`, `asset_assignment`, `vendor_invoice`, `vendor_invoice_line`, `vendor_payment` (prefixes `supplier*`, `purchase_*`, `sourcing_*`, `quotation*`, `goods_receipt*`, `stock_*` except the foundation ledger, `replenishment_*`, `asset*`, `vendor_*`) |

## 4. Kernel and contract types (already in the code)

### 4.1 Ids

`sutra_contracts::campus::ids` — foundation `InfrastructureNodeId`, `ServiceTicketId`; 8A
`SpaceCapabilityId`, `AvailabilityProfileId`, `SpaceReservationId`, `WorkOrderId`, `MaintenancePlanId`,
`LabId`, `LabEquipmentId`, `PracticalSessionId`, `LabBatchAllocationId`; 8B `HostelPolicyId`,
`HostelBedId`, `HostelApplicationId`, `HostelAllocationId`, `ResidentLeaveId`, `RoomTransferId`,
`RoomInspectionId`, `MessId`, `MessAllocationId`, `TransportRouteId`, `RouteVersionId`, `TransportStopId`,
`VehicleId`, `DriverAssignmentId`, `TransportAllocationId`, `TransportPassId`, `TripId`; 8C
`LibraryPolicyId`, `LibraryMembershipId`, `CatalogueRecordId`, `LibraryCopyId`, `LibraryLoanId`,
`LibraryReservationId`, `LibraryFineId`, `LibraryClearanceId`.
`sutra_contracts::services::ids` — 8C `DashboardId`, `DashboardVersionId`, `WidgetDefinitionId`,
`DashboardAssignmentId`, `ReportingFrameworkId`, `ReportingCycleId`, `ReportSnapshotId`,
`SubmissionPackageId`.
`sutra_contracts::resources::ids` — foundation `ItemId`, `StoreLocationId`, `StockLedgerEntryId`; 8D
`SupplierId`, `PurchaseRequisitionId`, `SourcingEventId`, `QuotationId`, `PurchaseOrderId`,
`GoodsReceiptId`, `StockIssueId`, `StockReservationId`, `StockReturnId`, `StockAdjustmentId`,
`ReplenishmentNeedId`, `AssetId`, `AssetAssignmentId`, `VendorInvoiceId`, `VendorPaymentId`.
All are `typed_id!` newtypes (UUIDv7, serde-transparent, OpenAPI `string/uuid`).

### 4.2 Lifecycle states

Enums with `ALL`, `as_str()`, `parse()`, serde/OpenAPI `snake_case`; also the DB `CHECK` values. The
**shared** machines carry `can_transition` in contracts (illegal → `409`):

| Machine | Values | Legal moves | Helpers |
|---------|--------|-------------|---------|
| `campus::status::InfrastructureStatus` (L2-G10) | `planned, active, suspended, decommissioned, archived` | planned → active / archived; active ↔ suspended; active / suspended → decommissioned; decommissioned → archived | `is_operational()` (active) |
| `campus::status::TicketStatus` (L2-G01) | `open, triaged, assigned, in_progress, on_hold, resolved, closed, reopened, cancelled, merged` | open → triaged / assigned; triaged → assigned; assigned → in_progress / on_hold / resolved; in_progress ↔ on_hold; on_hold → assigned; in_progress → resolved; resolved → closed / reopened; reopened → assigned / in_progress; open / triaged / assigned / reopened → cancelled; open / triaged / assigned / on_hold / reopened → merged | `is_open()` |
| `resources::status::StockMovementKind` (L3-J-058) | `receipt, issue, return_to_store, transfer_out, transfer_in, adjustment, reversal` | — (append-only) | `sign()`: `+` receipt / return_to_store / transfer_in; `−` issue / transfer_out; `±` adjustment / reversal |

**Classification enums (foundation):** `InfrastructureNodeType` (+ `allowed_parents`), `SpaceType`
(+ `is_teaching`), `TicketCategory`, `TicketPriority` (+ `default_sla_hours`), `TicketSource`,
`PersonKind` (`student, staff`) — campus; `ItemStatus`, `ItemKind`, `StoreLocationStatus` — resources.

**Slice enums (transitions slice-owned, in the slice's `domain.rs`; names follow the L2 state models):**

| Enum | Values | Slice |
|------|--------|-------|
| `ReservationStatus` (L2-G10) | `requested, confirmed, rescheduled, cancelled, completed, rejected` | 8A |
| `WorkOrderStatus` (L2-G01) | `created, assigned, in_progress, on_hold, completed, verified, reopened, cancelled` | 8A |
| `MaintenancePlanStatus` / `MaintenanceCycleStatus` | `draft, active, suspended, retired` / `scheduled, due, deferred, completed, missed` | 8A |
| `LabStatus` / `PracticalSessionStatus` / `EquipmentStatus` | `not_ready, ready, restricted, unavailable` / `planned, ready, in_progress, completed, postponed, cancelled` / `operational, restricted, under_maintenance, out_of_service` | 8A |
| `HostelPolicyStatus` | `draft, approved, active, superseded` | 8B |
| `HostelApplicationStatus` (L2-G03) | `submitted, under_review, approved, waitlisted, rejected, withdrawn, allocated` | 8B |
| `HostelAllocationStatus` | `active, on_leave, transfer_pending, vacating, closed` | 8B |
| `ResidentLeaveStatus` / `RoomTransferStatus` / `InspectionStatus` | `requested, approved, rejected, out, returned, overdue` / `requested, approved, rejected, completed` / `scheduled, completed, damage_found, charged, closed` | 8B |
| `RouteStatus` / `StopStatus` / `VehicleStatus` | `draft, active, suspended, retired` / `active, suspended, retired` / `available, assigned, under_maintenance, broken_down, retired` | 8B |
| `TransportAllocationStatus` / `PassStatus` / `TripStatus` | `requested, active, changed, withdrawn` / `issued, active, expired, suspended, replaced, cancelled` / `planned, ready, in_progress, completed, cancelled` | 8B |
| `LibraryPolicyStatus` / `MembershipStatus` | `draft, active, superseded` / `requested, active, suspended, closed` | 8C |
| `CatalogueStatus` / `CopyStatus` | `draft, published, withdrawn` / `available, issued, reserved, lost, damaged, withdrawn` | 8C |
| `LoanStatus` / `LibraryReservationStatus` / `FineStatus` / `ClearanceStatus` | `active, overdue, returned, lost` / `queued, ready, collected, expired, cancelled` / `assessed, waiver_requested, waived, billed, paid` / `requested, approved, rejected` | 8C |
| `DashboardStatus` / `WidgetStatus` | `draft, in_review, published, retired` / `draft, active, retired` | 8C |
| `FrameworkStatus` / `CycleStatus` / `SnapshotStatus` / `PackageStatus` (L2-H08) | `draft, active, superseded` / `planned, collecting, validating, packaged, submitted, closed` / `collected, validated, rejected` / `draft, pending_approval, approved, submitted, acknowledged, corrected` | 8C |
| `SupplierStatus` (L2-J02) | `prospective, registered, approved, active, suspended, merged, inactive` | 8D |
| `RequisitionStatus` (L2-J03) | `draft, submitted, approved, released, rejected, amended, withdrawn, cancelled, ordered, closed` | 8D |
| `SourcingStatus` / `QuotationStatus` | `draft, open, evaluating, awarded, cancelled` / `received, shortlisted, accepted, rejected` | 8D |
| `PurchaseOrderStatus` (L2-J04) | `draft, issued, acknowledged, partially_received, received, closed, cancelled` | 8D |
| `GoodsReceiptStatus` (L2-J05) | `received, inspecting, accepted, partially_accepted, rejected` | 8D |
| `StockIssueStatus` / `AdjustmentStatus` | `requested, approved, issued, returned, cancelled` / `requested, approved, applied, rejected` | 8D |
| `AssetStatus` (L2-J08) | `registered, available, assigned, under_repair, retired` | 8D |
| `VendorInvoiceStatus` / `VendorPaymentStatus` | `received, matched, mismatch, approved, posted, paid, cancelled` / `draft, posted` | 8D |

### 4.3 Events

`type = sutraerp.<campus|services|resources>.<aggregate>.<verb>`, `dataschema =
urn:sutraerp:schema:<domain>.<aggregate>.<verb>:1`, `source = /sutraerp/<domain>`, envelope `subject`
= aggregate id, scope `ExecutionScope::institution(tenant, institution)`. Emit via
`shared::publish::event` inside the owner-local tx; audit via `shared::publish::audit` with action
`<domain>.<aggregate>.<command>`. V1 payloads are frozen: additive optional fields only. Shared
payloads per domain (`events/mod.rs`): **`StatusChangedV1`** `{aggregateId, institutionId,
fromStatus?, toStatus, version, reason?, processInstanceId?, number?, subjectRef?}` and **`ModifiedV1`**
`{aggregateId, institutionId, version, changedFields[]}`.

| Owner / file | Constants (payload `StatusChangedV1` unless noted) |
|--------------|-----------|
| foundation `campus/events/mod.rs` | `SERVICE_TICKET_OPENED` (`service_ticket.opened`, emitted by `open_ticket` for every slice; `subjectRef` = requester, `number`, `reason` = title) |
| 8A `campus/events/facilities.rs` | `INFRASTRUCTURE_NODE_ACTIVATED`, `_SUSPENDED`, `_RESTORED`, `_DECOMMISSIONED`, `_MODIFIED` (ModifiedV1); `SPACE_RESERVATION_CONFIRMED`, `_RESCHEDULED`, `_CANCELLED`; `SERVICE_TICKET_ASSIGNED`, `_RESOLVED`, `_CLOSED`, `_ESCALATED`; `WORK_ORDER_COMPLETED`; `MAINTENANCE_PLAN_ACTIVATED`; `PRACTICAL_SESSION_COMPLETED` |
| 8B `campus/events/residence.rs` | `HOSTEL_APPLICATION_DECIDED`, `HOSTEL_ALLOCATION_ACTIVATED`, `HOSTEL_ALLOCATION_CLOSED`, `RESIDENT_LEAVE_DECIDED`, `ROOM_TRANSFER_COMPLETED`, `ROOM_INSPECTION_COMPLETED`; `TRANSPORT_ROUTE_ACTIVATED`, `TRANSPORT_ROUTE_SUSPENDED`, `TRANSPORT_ALLOCATION_ACTIVATED`, `TRANSPORT_PASS_ISSUED`, `TRANSPORT_PASS_SUSPENDED`, `TRIP_COMPLETED`, `TRIP_CANCELLED` |
| 8C `campus/events/library.rs` | `LIBRARY_MEMBERSHIP_ACTIVATED`, `_SUSPENDED`; `CATALOGUE_RECORD_PUBLISHED`; `LIBRARY_LOAN_ISSUED`, `_RETURNED`, `_OVERDUE`; `LIBRARY_RESERVATION_READY`; `LIBRARY_FINE_RAISED`, `_WAIVED`; `LIBRARY_CLEARANCE_DECIDED` |
| 8C `services/events/reporting.rs` | `DASHBOARD_PUBLISHED`, `DASHBOARD_ROLLED_BACK`, `WIDGET_DEFINITION_ACTIVATED`, `REPORTING_FRAMEWORK_ACTIVATED`, `REPORT_SNAPSHOT_VALIDATED`, `SUBMISSION_PACKAGE_APPROVED`, `SUBMISSION_PACKAGE_ACKNOWLEDGED`, `REPORTING_CYCLE_CLOSED` |
| 8D `resources/events/procurement.rs` | `SUPPLIER_APPROVED`, `SUPPLIER_SUSPENDED`; `PURCHASE_REQUISITION_SUBMITTED`, `_APPROVED`, `_REJECTED`; `SOURCING_EVENT_AWARDED`; `PURCHASE_ORDER_ISSUED`, `_CLOSED`; `GOODS_RECEIPT_ACCEPTED`; `STOCK_ISSUED`; `REPLENISHMENT_NEEDED`; `ASSET_REGISTERED`, `ASSET_ASSIGNED`, `ASSET_RETIRED`; `VENDOR_INVOICE_POSTED`, `VENDOR_PAYMENT_POSTED` |

Each slice file exports `EVENT_TYPES`; `events::all_event_types()` per domain lists every one and a
unit test keeps them unique and well formed. A slice adds events only to its own file and lists them
here in its PR. **Consumed events:** none required (cross-slice facts are read through foundation
helpers / ports when a user acts).

### 4.4 Permissions and role mapping (`sutra_kernel::auth::Permission`)

Office reads require the domain read permission (Administrator). **Everyone** (administrator,
faculty, student) acts on their own campus services through `campus.self`: handlers resolve the
person from the session subject (`sutra_campus::shared::me::my_person` → student record, else staff
record, else 404) and never take a person id from the client. Governed decisions use the step-up
`*.approve` permissions. Unit test `batch8_domains_g_h_j_grants`.

| Variant | Name | Step-up | Roles | Used for |
|---------|------|---------|-------|----------|
| `CampusRead` | `campus.read` | no | Admin | all Campus office reads (8A / 8B / 8C) |
| `CampusSelfService` | `campus.self` | no | Admin, Faculty, Student | own tickets, room booking requests (8A), hostel application / residency / leave, transport request / pass (8B), library loans / reservations / fines / clearance (8C), `GET /campus/me/person` |
| `FacilityManage` | `campus.facility.manage` | no | Admin | 8A |
| `HostelManage` | `campus.hostel.manage` | no | Admin | 8B hostel |
| `TransportManage` | `campus.transport.manage` | no | Admin | 8B transport |
| `LibraryManage` | `campus.library.manage` | no | Admin | 8C library |
| `CampusApprove` | `campus.approve` | **yes** | Admin | decommissioning, hostel admission, policy activation, fine waivers, library clearance (§9) |
| `ReportRead` | `services.report.read` | no | Admin | metrics catalogue, dashboards, cycles (8C) |
| `ReportManage` | `services.report.manage` | no | Admin | 8C dashboards / widgets / frameworks / cycles / snapshots |
| `ReportApprove` | `services.report.approve` | **yes** | Admin | 8C publish dashboards, approve / submit packages |
| `ResourcesRead` | `resources.read` | no | Admin | all 8D reads, stock on hand |
| `ProcurementManage` | `resources.procurement.manage` | no | Admin | 8D suppliers, requisitions, sourcing, POs, receipts, invoices, payments |
| `InventoryManage` | `resources.inventory.manage` | no | Admin | 8D items, stores, movements, issues, reservations, returns, adjustments, replenishment |
| `AssetManage` | `resources.asset.manage` | no | Admin | 8D assets |
| `ResourcesApprove` | `resources.approve` | **yes** | Admin | 8D governed decisions (§9) |

Step-up is enforced only when the deployment enables it; DB tests run in dev mode without it.

## 5. HTTP API

### 5.1 Conventions (as Domains B … F, I, E)

- Every handler starts with `auth.require(Permission::…)?` (or `shared::me::my_person`). Tenant from
  the session; `shared::scope::institution_scope(ctx, <institution>)` for events / audit.
- Before a write: `shared::guard::require_institution_writable`.
- Create: `POST`, required `Idempotency-Key` (`sutra_web::idempotency::require_idempotency_key`),
  `201` + `ETag`. Update or state command: `If-Match` required (`shared::http::if_match`: 428 missing,
  412 stale); response = updated resource + `ETag`. Bulk commands (generate PM cycles, overdue sweep,
  pass renewal batches, snapshot collection, replenishment run) also require `Idempotency-Key` and
  return `{processed, skipped, failed[]}`.
- State commands are `POST …/{id}/<verb>` with optional `{ "reason": "…" }`; literal paths per command,
  no generic `/{command}` route.
- Lists: `GET` collection under the parent, `?status=` filter, `{ "items": [...] }`; `?limit=` ≤ 200
  (default 50) and `nextCursor` where lists grow (tickets, loans, stock movements).
- Errors: illegal transition / duplicate / capacity or availability conflict / insufficient stock →
  `409`; validation → `422`; foreign tenant / unknown id / not yours → `404`; Domain B/C/E reference not
  found → `422` on the field.
- JSON members camelCase, statuses snake_case (§4.2), dates `YYYY-MM-DD`, instants RFC 3339,
  **decimals as strings**.
- **Every `#[utoipa::path]` sets an explicit `operation_id`** with the slice prefix below and the
  slice's `tag`, enforced by `architecture.rs::batch8_operations_follow_the_slice_prefix_and_tag_rules`
  (Batch 8 tags only on `/api/v1/{campus,services,resources}/` paths, no other path uses a Batch 8
  prefix, first path parameter after a collection is `{id}`). Note: Domain B's campus operations use
  bare `campus_*` ids (`campus_list` …) on `/api/v1/institution/campuses`; Batch 8 ids always use the
  longer prefixes below.
- OpenAPI schema names are global: prefix ambiguous names with your area (`HostelPolicyView`, not
  `PolicyView`) or use `#[schema(as = …)]`.

### 5.2 Path prefixes, operation_id prefixes, tags

**Path parameter rule:** the first parameter after any collection segment is always `{id}`; deeper
parameters get names (`/routes/{id}/stops/{stopId}`). Self-service lives under `/me/…`.

| Owner | Paths (under the domain root) | operation_id prefixes | tag |
|-------|-------------------------------|-----------------------|-----|
| foundation | `GET /api/v1/campus/institutions/{id}/infrastructure-tree` (`campus.read`, filters `campusId`, `nodeType`, `status`, `spaceType`), `GET /api/v1/campus/me/person` (`campus.self`) | `campus_directory_` (incl. `campus_directory_self_`) | `campus-directory` |
| foundation | `GET /api/v1/services/institutions/{id}/metrics` (`services.report.read`, `?domain=`) | `services_directory_` | `services-directory` |
| foundation | `GET /api/v1/resources/institutions/{id}/stock-on-hand` (`resources.read`, `?itemId=&locationId=`) | `resources_directory_` | `resources-directory` |
| 8A | `/campus/institutions/{id}/infrastructure-nodes`, `/infrastructure-nodes/{id}[/activate,/suspend,/restore,/decommission,/archive,/capabilities,/availability]`, `/infrastructure-nodes/{id}/capacity`, `/spaces/{id}/availability` (free / busy), `/institutions/{id}/reservations`, `/reservations/{id}[/confirm,/reschedule,/cancel,/decision]`, `/me/reservations` | `campus_infra_`, `campus_space_`, `campus_reservation_` (incl. `campus_reservation_self_`) | `campus-infrastructure` |
| 8A | `/campus/institutions/{id}/tickets`, `/tickets/{id}[/triage,/assign,/start,/hold,/resolve,/close,/reopen,/cancel,/merge,/escalate]`, `/institutions/{id}/tickets/sla-check`, `/me/tickets[/{id}/confirm]`, `/tickets/{id}/work-orders`, `/work-orders/{id}[/assign,/start,/complete,/verify,/reopen,/cancel]`, `/institutions/{id}/maintenance-plans`, `/maintenance-plans/{id}[/submit,/decision,/activate,/suspend,/generate]`, `/maintenance-cycles/{id}[/defer,/complete]`, `/institutions/{id}/labs`, `/labs/{id}[/readiness,/equipment,/batch-allocations,/sessions]`, `/lab-equipment/{id}/status`, `/practical-sessions/{id}[/ready,/start,/complete,/postpone,/cancel]` | `campus_ticket_` (incl. `campus_ticket_self_`), `campus_workorder_`, `campus_maintenance_`, `campus_lab_` | `campus-facilities` |
| 8B | `/campus/institutions/{id}/hostel-policies`, `/hostel-policies/{id}[/submit,/decision,/activate]`, `/hostel-rooms/{id}/beds`, `/institutions/{id}/hostel-applications`, `/hostel-applications/{id}[/decision,/allocate,/withdraw]`, `/hostel-allocations/{id}[/leave,/transfer,/vacate,/close]`, `/resident-leaves/{id}[/decision,/return]`, `/room-transfers/{id}/decision`, `/hostel-rooms/{id}/inspections`, `/room-inspections/{id}/complete`, `/institutions/{id}/messes`, `/messes/{id}/allocations`, `/mess-allocations/{id}/change`, `/hostel-allocations/{id}/complaints`, `/me/hostel[/application,/leave,/complaints]` | `campus_hostel_` (incl. `campus_hostel_self_`) | `campus-hostel` |
| 8B | `/campus/institutions/{id}/transport-routes`, `/transport-routes/{id}[/versions,/activate,/suspend,/retire]`, `/route-versions/{id}/stops`, `/institutions/{id}/transport-stops`, `/transport-stops/{id}[/suspend,/restore]`, `/institutions/{id}/vehicles`, `/vehicles/{id}[/status,/breakdown]`, `/transport-routes/{id}/drivers`, `/institutions/{id}/transport-allocations`, `/transport-allocations/{id}[/change,/withdraw,/pass]`, `/transport-passes/{id}[/renew,/replace,/suspend,/restore]`, `/transport-routes/{id}/trips`, `/trips/{id}[/ready,/start,/complete,/cancel,/replace-vehicle]`, `/me/transport[/request,/pass,/complaints]` | `campus_transport_` (incl. `campus_transport_self_`) | `campus-transport` |
| 8C | `/campus/institutions/{id}/library-policies`, `/library-policies/{id}[/submit,/decision,/activate]`, `/institutions/{id}/library-memberships`, `/library-memberships/{id}[/suspend,/restore,/close]`, `/institutions/{id}/catalogue`, `/catalogue-records/{id}[/publish,/copies]`, `/library-copies/{id}[/condition,/withdraw,/lost]`, `/institutions/{id}/library-loans` (issue), `/library-loans/{id}[/return,/renew]`, `/institutions/{id}/library-loans/overdue-sweep`, `/catalogue-records/{id}/reservations`, `/library-reservations/{id}[/collect,/cancel,/expire]`, `/library-fines/{id}[/waiver,/decision,/bill]`, `/institutions/{id}/library-clearances`, `/library-clearances/{id}/decision`, `/me/library[/catalogue,/loans,/reservations,/fines,/clearance]` | `campus_library_` (incl. `campus_library_self_`) | `campus-library` |
| 8C | `/services/institutions/{id}/dashboards`, `/dashboards/{id}[/versions,/submit,/decision,/publish,/rollback,/assignments]`, `/institutions/{id}/widgets`, `/widgets/{id}[/activate,/retire]`, `/me/dashboards`, `/institutions/{id}/reports/library`, `/institutions/{id}/reports/infrastructure` | `services_dashboard_` | `services-dashboards` |
| 8C | `/services/institutions/{id}/reporting-frameworks`, `/reporting-frameworks/{id}[/submit,/decision,/activate]`, `/reporting-frameworks/{id}/cycles`, `/reporting-cycles/{id}[/collect,/close]`, `/report-snapshots/{id}[/validate,/reject]`, `/reporting-cycles/{id}/packages`, `/submission-packages/{id}[/submit,/decision,/acknowledge,/correct]` | `services_regulatory_` | `services-regulatory` |
| 8D | `/resources/institutions/{id}/suppliers`, `/suppliers/{id}[/register,/submit,/decision,/suspend,/restore,/merge]`, `/institutions/{id}/requisitions`, `/requisitions/{id}[/submit,/decision,/amend,/withdraw,/cancel]`, `/institutions/{id}/sourcing-events`, `/sourcing-events/{id}[/quotations,/evaluate,/award,/decision,/cancel]`, `/institutions/{id}/purchase-orders`, `/purchase-orders/{id}[/issue,/acknowledge,/amend,/cancel,/close,/receipts]`, `/goods-receipts/{id}[/inspect,/accept,/reject]`, `/institutions/{id}/vendor-invoices`, `/vendor-invoices/{id}[/match,/decision,/post]`, `/institutions/{id}/vendor-payments`, `/vendor-payments/{id}/post` | `resources_supplier_`, `resources_requisition_`, `resources_sourcing_`, `resources_po_`, `resources_receipt_`, `resources_payable_` | `resources-procurement` |
| 8D | `/resources/institutions/{id}/items`, `/items/{id}[/activate,/deactivate]`, `/institutions/{id}/stores`, `/stores/{id}/locations`, `/institutions/{id}/stock-movements`, `/institutions/{id}/stock-issues`, `/stock-issues/{id}[/decision,/issue,/return,/cancel]`, `/institutions/{id}/stock-reservations`, `/stock-reservations/{id}[/release,/fulfil]`, `/institutions/{id}/stock-adjustments`, `/stock-adjustments/{id}/decision`, `/institutions/{id}/replenishment` (run + list) | `resources_item_`, `resources_store_`, `resources_stock_` | `resources-inventory` |
| 8D | `/resources/institutions/{id}/assets`, `/assets/{id}[/assign,/return,/condition,/retire,/decision]` | `resources_asset_` | `resources-assets` |

The tags are in `apps/api/src/openapi.rs`; router mount points are registered in each crate's
`router_with_state`. Each slice only fills `pub fn router(state: <State>) -> OpenApiRouter` in its own
`mod.rs` (rename `_state` to `state`).

### 5.3 Minimum response fields other slices / screens rely on

- **Infrastructure node** (foundation `InfrastructureNodeView`, reusable): `{id, institutionId,
  campusId, parentId?, nodeType, code, name, spaceType?, capacity?, floorNumber?, areaSqM?,
  attributes, status, statusReason?, effectiveFrom?, version}`.
- **Ticket** (8A): `{id, number, category, source, sourceRefId?, isComplaint, nodeId?,
  requesterKind?, requesterId?, title, description?, priority, status, assignee?, slaDueAt,
  slaBreached, escalationLevel, resolvedAt?, closedAt?, resolution?, canonicalTicketId?, createdAt,
  version}`.
- **Loan** (8C): `{id, number, copyId, accessionNumber, title, memberId, personKind, personId,
  issuedOn, dueOn, returnedOn?, renewals, status, fineAmount?, version}`; **fine**: `{id, loanId?,
  amount, days, status, chargeId?, chargeNumber?}` (strings for money).
- **Stock on hand** (foundation): `{itemId, itemCode, itemName, unit, locationId, locationCode,
  onHand, reorderLevel?, atReorderLevel}`.
- **Metric** (foundation): `{domain, key, label, value, unit}` (`value` a decimal string).
- Collections: `{ "items": [ … ] }`.

## 6. Cross-slice runtime rules and ports

### 6.1 Foundation helpers

`sutra_campus::shared` — `state::CampusState` (`pool, workflow, approvals, references, students,
workforce, charges, finance`; built by `CampusDeps::into_state()` or by hand in tests), `scope`, `guard`
(`require_institution_writable`, `require_institution`), `http` (`with_etag`, `if_match`, `actor`),
`publish` (`event`, `audit`), `approval` (§9), `time::business_today`, `numbering::next` (§3.4),
`decimal` (`round2`, `fmt2`, `parse_decimal`), `rows` (node / ticket decoders + loads + locks),
`infrastructure` (`insert_node`, `change_node_status`, `validate_new_node`), `tickets` (`open_ticket`,
`change_ticket_status`, `validate_new_ticket`), `lifecycle` (`record_history`, `StatusChange`,
`HistoryEntry`, `illegal`), `me` (`PersonRef`, `person`, `require_person`, `my_person`).
`sutra_services::shared` — same set with `ServicesState` (`pool, workflow, approvals, references,
metrics`) and `ServicesState::all_metrics(scope, institution)`.
`sutra_resources::shared` — same set with `ResourcesState` (`pool, workflow, approvals, references,
workforce, postings`) and `stock` (`post_movement`, `reverse_movement`, `on_hand`, `validate_movement`,
`NewMovement`, `MovementReceipt`).

### 6.2 Typed ports (all defined and wired by the foundation)

| Port | Defined in | Implementation wired now | Consumer | Notes |
|------|-----------|--------------------------|----------|-------|
| **`TeachingSpacePort`** (`space`, `spaces_of_campus`) — Campus → Domain D | `sutra_contracts::campus` (Batch 4) | `sutra_campus::PgTeachingSpaces` (**real**, replaces `NoTeachingSpaces` in `academic_deps`) | 4B timetable | active / suspended `space` nodes of a teaching type (classroom, lab, seminar_room, auditorium); `active = status == active`; unknown → `None` (free-text room label keeps working). |
| **`StudentDirectory`** (`student`, `live_context`, `by_subject`) — Domain C → Campus | `sutra_contracts::student::ports` | `sutra_student::PgStudentDirectory` | 8A/8B/8C persons, `my_person` | |
| **`WorkforceDirectory`** — Domain E → Campus / Resources | `sutra_contracts::people::ports` | `sutra_people::PgWorkforceDirectory` | staff persons, asset custodians, requesters | |
| **`CampusChargePort`** (`raise_campus_charge`) — Campus → Domain F | `sutra_contracts::finance::ports` (Batch 8 block) | `sutra_finance::PgCampusCharges` (**real**: fee head created on first use with the request's category `library | hostel | transport | laboratory | fine` and income `OtherIncome`, charge `source_type = campus_charge`, `source_id` = the campus aggregate, validated + **published** (Dr receivables / Cr other income), `charge.published`; idempotent on `source_id`) | 8C fines / lost copies, 8B hostel fees / damage and pass fees | Contract change to Domain F (+ `ChargeSourceType::CampusCharge`, migration `…003`). Settlement is read through `state.finance` (`ChargeDirectory::charge`, `sutra_finance::PgChargeDirectory`; `outstanding_amount == 0` = paid). A waiver happens **before** billing (no Finance credit note in Batch 8). |
| **`ProcurementPostingPort`** (`post_procurement`) — Resources → Domain F | `sutra_contracts::finance::ports` (Batch 8 block) | `sutra_finance::PgProcurementPostings` (**real**: accounts `1410` inventory, `5510` procurement expense, `2310` accounts payable seeded on first use outside the Batch 5 chart; vendor invoice Dr 1410 (stock) + Dr 5510 (other) / Cr 2310; vendor payment Dr 2310 / Cr `bank_main`; signed amounts flip sides (credit notes); journal `source_type = procurement_invoice | procurement_payment`, idempotent on `(source, posting_key)`, period-guarded) | 8D payables | Contract change to Domain F (+ `source::PROCUREMENT_INVOICE / PAYMENT`). Batch 5 F-044 / 045 / 082 delivered through it; three-way match F-081 is 8D logic. |
| **`OperationalMetricsPort`** (`domain`, `metrics(scope, institution)`) — every domain → Services | `sutra_contracts::services::ports` | `PgStudentMetrics` (active / enrolled students), `PgPeopleMetrics` (working / teaching staff), `PgFinanceMetrics` (open charges, receivables outstanding, payments collected), `PgCampusMetrics` (active buildings / spaces, capacity, open tickets, past SLA), `PgResourceMetrics` (active items / stores, items at reorder level) | 8C dashboards / snapshots (`ServicesState::all_metrics`) | Keys `<domain>.<key>`. A slice wanting a figure of its own tables asks the integrator to extend its domain's adapter (contract change); 8C never reads another schema. |
| Domain B reference | `sutra_contracts::institution::reference` | `PgInstitutionReferences` | all | institution + timezone, campuses (validate `campus_id`). |

Tests swap any port by building the state by hand and serving `sutra_<crate>::router_with_state(state)`;
`sutra_api::app::{campus_deps, services_deps, resources_deps}(pool, workflow)` give the production adapters.

## 7. Cross-slice dependencies and readiness

- **Nodes and tickets are foundation-owned**, so 8B / 8C never wait for 8A: tests seed through
  `apps/api/tests/support/b8.rs`: `b8::campus_setup(&app, &admin, &pool, TENANT, "Name")` →
  `CampusSetup {people, campus_id}` (active institution + published programme / calendar + default
  chart of accounts + an **active Domain B campus**; `.tenant()`, `.institution()`, `.scope()`);
  `b8::seed_hierarchy(&pool, &setup, "HST", &[("101", SpaceType::HostelRoom, 2)])` →
  `SeededHierarchy {site, building, floor, spaces}` (codes `<prefix>`, `<prefix>-B1`,
  `<prefix>-B1-F1`, `<prefix>-B1-<suffix>`, all active); `b8::seed_node(…)` for one node;
  `b8::seed_ticket(&pool, &setup, category, source, requester, node, title)`;
  `b8::seed_student(&pool, &setup, "Aarav", Some(b4::AARAV))` (active student + active context) and
  `b8::student_person(&student, Some(b4::AARAV))`; `b8::seed_staff(&pool, &setup, "Meera",
  Some(b4::MEERA))` (active staff + active engagement) and `b8::staff_person(&staff)`;
  `b8::seed_item(&pool, &setup, "PAPER-A4", ItemKind::Consumable, Some("12"))`, `b8::seed_store(&pool,
  &setup, "CENTRAL", None)`, `b8::seed_stock(&pool, &setup, item, store, "10", "line:1")`; `b8::dec`,
  `b8::date`, `b8::campus_actor`, `b8::resources_actor`.
- **8A → 8B / 8C:** hostel rooms / library spaces are nodes; a node that is not `active` takes no new
  allocation / booking (check `status.is_operational()`). 8A owns ticket status, so a hostel /
  transport / library complaint is opened by 8B / 8C with `open_ticket` (source `hostel` /
  `transport` / `library`, `source_ref_id` = their aggregate) and then followed in 8A's service desk.
- **8B / 8C → Finance:** charges only through `state.charges.raise_campus_charge` (fee codes:
  `HOSTEL-FEE`, `HOSTEL-DAMAGE`, `TRANSPORT-PASS`, `LIB-FINE`, `LIB-LOST`); store `charge_id` /
  `charge_number` on your row; never `finance.*` SQL. Only **students** can be charged (staff fines
  are recorded but not billed in Batch 8).
- **8C → every domain:** figures only through `state.all_metrics(…)`. `sutra-services` cannot read
  the `campus` schema, so the library and infrastructure operational reports (G-154 / G-320) are built
  from the `campus.*` metric keys; the 8C library half lives in `sutra-campus` and may add library
  figures to `PgCampusMetrics` as an append-only contract change (marked `// 8C:`), so the report reads
  them through the port.
- **8D → Finance:** postings only through `state.postings.post_procurement`.
- Merge order: **8A → 8B → 8C → 8D** (§12).

## 8. Inbox consumer hooks (`modules/platform/src/inbox/rules/`)

`rules::plan` dispatches by event type to `campus_8a.rs` (8A, also claims the foundation
`service_ticket.opened`), `campus_8b.rs` (8B), `campus_8c.rs` + `services_8c.rs` (8C),
`resources_8d.rs` (8D); each declares `EVENT_TYPES` (only types from its own event file — test
`slices_never_claim_the_same_type`) and a pure `plan`. The foundation files start **empty**; add a type
only **with** its rule. Category: reuse `institution`. Notify persons with
`Recipient::Subject(subjectRef)`, otherwise `Recipient::Role(…)`; to-dos with `todos::SystemTodo`.

| Slice | Types | Projection |
|-------|-------|------------|
| 8A | `service_ticket.opened` | to-do (administrators) "Triage SR-…" (link `/campus/facilities`) |
| 8A | `service_ticket.resolved`, `service_ticket.closed`, `space_reservation.confirmed`, `space_reservation.cancelled` | notification to the requester (`subjectRef`) |
| 8A | `service_ticket.escalated` | notification to administrators |
| 8B | `hostel_application.decided`, `hostel_allocation.activated`, `resident_leave.decided`, `transport_pass.issued`, `trip.cancelled` | notification to the person (`subjectRef`; trips → passengers is optional) |
| 8C | `library_loan.overdue`, `library_reservation.ready`, `library_fine.raised`, `library_clearance.decided` | notification to the member (link `/library`) |
| 8C | `submission_package.approved` | notification to administrators (link `/reports/regulatory`) |
| 8D | `purchase_requisition.submitted` (AwaitDecision), `replenishment_need.raised` | to-do (administrators) (link `/campus/procurement` / `/campus/inventory`) |

## 9. WorkflowPort usage for approval steps

`shared::approval` wraps `WorkflowPort` exactly like Domains B … F, I, E: the composition root passes
the platform `NoopWorkflowAdapter` and `ApprovalMode::AutoApprove`.

1. Validate the command and current state (read only).
2. `request_approval(&state, &scope, process::<KEY>, business_key(aggregate, id, version), vars)`
   **before** opening the tx (deterministic key → retry-safe).
3. One owner-local tx: re-check the version; set the pending state; store `process_instance_id`; emit
   the *submitted* event. If the outcome is `ApprovalOutcome::Approved`, **also** apply the decision in
   the same tx (final state + decision event, audit actor `system:auto-approval`).
4. Always expose the explicit decision endpoint (`POST …/{id}/decision {decision: approve|reject,
   reason}` with the slice's approve permission) so `AwaitDecision` (Flowable, Batch 9) needs no API
   change. Each slice has at least one DB test building its state with `ApprovalMode::AwaitDecision`
   (via `router_with_state` and `sutra_api::app::*_deps`).

Process keys: Campus (`sutra_campus::shared::approval::process`) 8A `NODE_DECOMMISSION`,
`RESERVATION_APPROVAL`, `WORK_ORDER_APPROVAL`, `MAINTENANCE_PLAN_APPROVAL`; 8B **`HOSTEL_ADMISSION`
(the reference approval of Batch 8)**, `HOSTEL_POLICY_APPROVAL`, `RESIDENT_LEAVE_APPROVAL`,
`ROOM_TRANSFER_APPROVAL`, `ROUTE_ACTIVATION`; 8C `LIBRARY_POLICY_APPROVAL`, `FINE_WAIVER`,
`LIBRARY_CLEARANCE`. Services 8C `DASHBOARD_PUBLICATION`, `REPORTING_FRAMEWORK_APPROVAL`,
`SUBMISSION_APPROVAL`. Resources 8D `SUPPLIER_APPROVAL`, `REQUISITION_APPROVAL`, `AWARD_APPROVAL`,
`PURCHASE_ORDER_APPROVAL`, `STOCK_ADJUSTMENT_APPROVAL`, `VENDOR_INVOICE_APPROVAL`, `ASSET_RETIREMENT`.
Use only your slice's keys; a new key is a contract change. Decisions use `campus.approve` /
`services.report.approve` / `resources.approve`; routine reservations / resident leave may be decided
with the slice's manage permission (say so in the PR).

## 10. Web

Batch 8 screens use **only `@sutra/ui` components** (BentoTile, DataTable, Tabs, Button, Badge,
Input/Select, Modal/Drawer, EmptyState, StatTile, Steps, PageHeader) and **no custom CSS / class
names / inline styles**. Frames render the `PageHeader` and the tabs.

| Route | File (owner) | Content |
|-------|--------------|---------|
| `/campus/:tab?` | `routes/admin/campus-setup.tsx` (foundation frame) | **Admin Campus**: the Batch 2 setup tabs first (institution, readiness, campuses, organisation, positions, policies — 2C), then `CAMPUS_OPS_TABS` (`features/campus/index.ts`): `infrastructure`, `facilities`, `labs` (8A), `hostel`, `transport` (8B), `library` (8C), `procurement`, `inventory`, `assets` (8D). Admin nav "Campus" (already present). |
| `/reports/:tab?` | `routes/admin/reports.tsx` (foundation frame, new) | **Admin Reports**: `REPORTS_TABS` `dashboards`, `regulatory` (8C). Admin nav "Reports" (already present; now live, `SCREEN_BATCH.reports.batch = 8`). |
| `/library` | `routes/student/library.tsx` (foundation frame since Batch 8) | **Student Library**: `MyLibraryPanel` (8C) above the Batch 4 `StudentLibraryScreen` (4C course resources). Student nav "Library". |

Each slice fills the placeholder components exported from its folder — keep the export names:
`features/campus/facilities/index.tsx`: `InfrastructureAdminTab, FacilitiesAdminTab, LabsAdminTab`;
`features/campus/residence/index.tsx`: `HostelAdminTab, TransportAdminTab`;
`features/campus/library/index.tsx`: `LibraryAdminTab, MyLibraryPanel`;
`features/reports/analytics/index.tsx`: `DashboardsAdminTab, RegulatoryAdminTab`;
`features/campus/resources/index.tsx`: `ProcurementAdminTab, InventoryAdminTab, AssetsAdminTab`.
Admin tabs take `{ institution: Institution | null }`. Mark real screens with `data-slice="8X"` (the
foundation test `features/campus/campus.test.tsx` accepts either the placeholder or `data-slice`).
API hooks in your own folder (`api.ts`, `hooks.ts`, `types.ts`, a `mock.ts` like
`features/institution/mock.ts`), `@sutra/api-client` after committing the regenerated client, decimals
as strings. `lib/workspace-screens.ts` already maps W-G01 … W-G05, W-G10, W-H07, W-H08, W-J01 … W-J08 to
these routes (they show as live).

**E2E:** the `ui` e2e already treats `/campus`, `/reports` (admin) and `/library` (student) as live
screens. `apps/web/e2e/campus.e2e.mjs` (`pnpm e2e:campus`) and the `campus` CI matrix entry are added
by the **integrator** (reusing `e2e/lib/harness.mjs` and the `people.e2e.mjs` pattern) — never locally.

## 11. Worker environment: CI-only

| Slice | Worktree | Branch (off `batch8/foundation`) |
|-------|----------|--------|
| 8A | `/workspace/work/erp-8a` | `batch8/8a` |
| 8B | `/workspace/work/erp-8b` | `batch8/8b` |
| 8C | `/workspace/work/erp-8c` | `batch8/8c` |
| 8D | `/workspace/work/erp-8d` | `batch8/8d` |

**NO local cargo at all** (the sandbox was OOM-killed on a single `cargo check -j1`): the only local
Rust is `cargo fmt --all` (rustfmt only); no `cargo check/build/test/clippy`, no `pnpm
build/typecheck/test`, no e2e / chromium. Push (without `[skip ci]`) and poll the `SutraERP CI`
workflow through the GitHub API (`bash /workspace/work/tools/b8-ci.sh batch8/8x`;
`/workspace/work/tools/b8-worker-common.md` has the exact commands): fmt · clippy · workspace tests
(`Rust`), full DB suite incl. `db_b8_foundation` + your `db_b8X` + every regression (`Rust
integration`), `OpenAPI drift` (commit the regenerated spec + TS client from the `openapi-regenerated`
artifact), `Web`, the e2e matrix, `Container images`. Fix until all are green. The workflow triggers on
`batch8/**`.

## 12. Merge order and known gaps

- Merge order: **8A → 8B → 8C → 8D** (independent crates / folders; 8B and 8C depend on 8A only
  through the foundation node / ticket tables; 8D is a separate crate). Regenerate OpenAPI / TS after
  each merge from the CI artifact. After 8D the integrator adds `apps/api/tests/db_b8_integration.rs`
  (building → room booking → complaint ticket → work order → closure; hostel application → allocation →
  hostel fee charge in Finance; route → pass → transport fee; catalogue → copy → issue → overdue →
  fine billed → student dues; requisition → PO → receipt → stock → issue; vendor invoice → AP entry →
  payment; dashboard + regulatory snapshot showing those figures), `campus.e2e.mjs` and the `e2e
  (campus)` CI job.
- `TeachingSpacePort` is real now, but 4B still stores a free-text room label when a slot has no
  space; linking slots to spaces is the integrator's / a later batch's choice.
- Staff can borrow / reside / travel, but only students are **billed** (`CampusChargePort` charges
  students); staff charges are recorded on the campus row only.
- Fine waivers happen before billing; reversing a billed charge needs Finance adjustments (Batch 5
  `adjustments`, not wired to campus in Batch 8).
- Asset capitalisation / depreciation (F-067 …) is deferred: assets are operational records only; vendor
  invoices for asset lines post to inventory (1410) until asset accounting lands.
- `ApprovalMode::AwaitDecision` is wired but unused in production until the Flowable adapter lands (Batch 9).
- Session pinning to one institution is not enforced for Domains G / H / J (tenant-wide office, as before).
