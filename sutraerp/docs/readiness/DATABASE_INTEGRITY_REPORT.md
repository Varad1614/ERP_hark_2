# SutraERP Part 1 — database integrity report

**Result: PASS — 0 violations.** Every check below ran against the PostgreSQL 18 database of the
full-stack demo (`docker compose`, Keycloak/OIDC, `SUTRA_APPROVAL_MODE=human`) **after** the whole
journey (seed → Admission → Enrolment → Attendance → Fees incl. a fee adjustment → Examination →
Result) **and after** `docker compose restart postgres api` + the persistence phase.

| | |
|---|---|
| Branch / commit | `part1/s2-int` @ `7c26665f` (code; later commits on the branch are docs / report wiring) |
| Workflow run | [38755333716](https://github.com/Varad1614/ERP_hark_2/actions/runs/38755333716) — *SutraERP Part 1 demo (full stack)*, step “Database integrity (SQL checks against the demo DB)”: **success** |
| Script | `deploy/ci/db-integrity.sh` → `deploy/ci/db-integrity.sql` (one transaction, **rolled back**; exits 1 on any violation) |
| Raw output | [`evidence/db-integrity-run-38755333716.txt`](evidence/db-integrity-run-38755333716.txt) (also artifact `part1-demo-logs/db-integrity.txt` and the job summary) |
| Totals | `INTEGRITY_VIOLATIONS=0`, `migrations: 92 files, 92 applied`, `INTEGRITY TOTAL VIOLATIONS: 0` |

The step is wired into `.github/workflows/sutraerp-part1-demo.yml` (after the persistence phase,
`if: always()` once the journey ran) and fails the job on any violation, so every demo run re-proves
this report.

## 1. Migrations — count and order

| Check | Result |
|---|---|
| Migration files in `migrations/` | **92** (`20261010000001_create_domain_schemas` … `20261201000302_student_admission_single_readmission`) |
| Applied (`_sqlx_migrations`) | **92**, all `success = true`, latest `20261201000302` |
| Files ↔ applied versions | identical set, identical order (wrapper `diff` empty) |
| Applied in version order (`installed_on` order = version order) | 0 out of order |
| Checksums present | 0 missing |
| Duplicate versions | none |

Part 1 added **8 migrations** (84 → 92) (append-only; no existing file edited):

| Version | Purpose | Defect |
|---|---|---|
| `20261125000100_platform_approver_roles` | widen role CHECKs (notifications, notices, dashboards) to the 5 checker roles | DEF-X-002 |
| `20261125000101_exams_publication_requester` | `exams.publication.authorisation_requested_by` (SoD on publication) | DEF-X-002 |
| `20261125000102_resources_po_approval_requester` | `resources.purchase_order.approval_requested_by` | DEF-X-002 |
| `20261125001000_exams_release_decision_publication_snapshot` | `release` result-decision kind; **`exams.publication_snapshot`** (append-only trigger + REVOKE UPDATE/DELETE) | DEF-I-003, DEF-I-010 |
| `20261125004700_people_payroll_disbursement` | staff bank account, payroll disbursement + lines (CHECKs, `payroll_disbursement_one_live` unique index) | DEF-E-007 |
| `20261126000310_resources_vendor_payment_approval` | vendor payment `pending_approval → approved/rejected → posted`, requester/decider columns | DEF-J-003 (s2-ops) |
| `20261201000301_academic_attendance_finalised_guard` | **trigger `attendance_record_finalised_guard`** + CHECK `attendance_correction_maker_checker` (NOT VALID: new/updated rows) | DEF-D-009, DEF-D-002 |
| `20261201000302_student_admission_single_readmission` | **partial unique index `admission_one_live_readmission`** | DEF-C-002 |

Ordering note: `20261126000310` (merged from `part1/s2-ops`) sorts *before* the two `20261201…`
migrations (from `part1/s2-acad`). On a fresh database (the demo, CI, a new pilot) they apply in
version order — verified above. A database that had already applied the `20261201…` pair from the
acad branch alone would get `20261126000310` applied after them (sqlx applies missing versions);
the "applied in version order" check would then report it. No shared environment ran the acad
branch alone, so no action is needed; keep new Part 1 migrations above `20261201000302`.

## 2. Constraints, triggers and indexes that guard Part 1 behaviour

Inventory of the demo DB: **700** foreign keys (all validated), **1461** CHECK, **106** UNIQUE
constraints, **182** partial unique indexes, **162** user triggers.

| Guard | Where defined | Verified in the demo DB |
|---|---|---|
| Finalised attendance cannot be edited except through a governed correction (`attendance_record_finalised_guard`, BEFORE UPDATE) | Part 1 `20261201000301` | present + enabled; **probe refused**: “attendance record 01a14016-0e9c-… belongs to a finalised register; change it through a governed correction” |
| Applied attendance correction decided by someone other than its requester (`attendance_correction_maker_checker`) | Part 1 `20261201000301` | present; 0 violating rows |
| One live readmission per closed admission (`admission_one_live_readmission`, partial unique) | Part 1 `20261201000302` | present, valid, unique |
| One open admission per student (`admission_open_per_student`, partial unique) | Batch 3 | present, valid, unique |
| Duplicate applicant refused at intake (application code, advisory lock per institution) | Part 1 DEF-C-003 (`student/admission/duplicates.rs`) | journey: duplicate → **409**; DB: 0 applicants with > 1 live admission |
| Double entry at commit (`journal_entry_balanced`, `journal_line_balanced`, DEFERRABLE constraint triggers) + `CHECK (total_debit = total_credit)` | Batch 5 | present + enabled; **probe refused**: header copied without lines → “journal entry … is unbalanced (lines: 0.00 / 0.00 over 0 lines; header 90000.00 / 90000.00)” |
| Append-only ledger (`journal_entry_append_only`, `journal_line_append_only`) | Batch 5 | **probes refused** (UPDATE entry, DELETE line) |
| Append-only history: `admission_status_history_immutable`, `paper_mark_history_append_only`, `attendance_audit_immutable`, `publication_snapshot_append_only` (Part 1), `audit_log_no_update_delete` | Batches 1/3/4/6 + Part 1 | all present + enabled; **probes refused** (status history UPDATE, paper mark history UPDATE, audit log DELETE) |
| Idempotent posting: one journal entry per (tenant, source_type, source_id, posting_key) | Batch 5 unique index | 0 duplicates |

## 3. Referential integrity (FK / orphans)

| Check | Violations |
|---|---|
| Foreign keys not validated (`convalidated = false`) | 0 of 700 |
| Orphan rows behind any foreign key (dynamic scan of all 700 FKs: child value set, parent row missing) | 0 |
| Cross-module references (plain uuids by module boundary, no FK): section enrolments → student, → student context; attendance records → student; student accounts → student; charges → student; exam registrations → student; term results → student; student contexts → institution | 0 each |
| Journal entries → their source record (charge / payment / adjustment) | 0 |

## 4. Balances

| Check | Violations |
|---|---|
| Every journal entry: ≥ 2 lines, Σdebit = Σcredit = header totals | 0 |
| Ledger balanced per institution | 0 |
| Charge `settled_amount` = Σ live allocations | 0 |
| Charge `adjustment_amount` = Σ posted adjustments (debit note +, others −) — includes the journey's ₹5,000 concession (charge 90,000 → 85,000 outstanding, journal 5000.00 = 5000.00) | 0 |
| Charge outstanding within [0, total + adjustments] | 0 |
| Completed payment = Σ live allocations + unapplied | 0 |
| Receipt amount = payment amount | 0 |
| Completed payment has its journal entry | 0 |

## 5. Duplicates and idempotency

| Check | Violations / evidence |
|---|---|
| Live admissions per applicant (normalised name + date of birth, per institution) ≤ 1 | 0 |
| One receipt per payment · one journal entry per (source, posting key) · payments per idempotency key ≤ 1 | 0 · 0 · 0 |
| One live enrolment per student and section · one attendance record per session and student | 0 · 0 |
| One live exam registration per session and student · one published term result per session and student | 0 · 0 |
| One charge per student and fee structure | 0 |
| Completed idempotency records carry the stored response | 0 (161 records) |
| The journey's replayed payment key (`journey-pay-*`: same body replayed → same payment, different body → 422) | 1 payment, 1 idempotency record |

## 6. Maker-checker in the data (human approval mode)

| Check | Violations |
|---|---|
| No `system:auto-approval` actor anywhere in `platform.audit_log` | 0 |
| Admission approved by someone other than the user who requested approval | 0 |
| Posted fee adjustments decided by someone other than their creator | 0 (journey: `posted · decided_by = finance approver · created_by = administrator`) |
| Publication authorised by someone other than its requester | 0 |
| Applied attendance corrections decided by someone other than the requester | 0 |

## 7. Limits (what this does not prove)

* The probes run as the schema owner inside a rolled-back transaction; they prove the database
  refuses the writes, not who may connect. Row-level grants per module role are covered by the
  batch DB suites, not re-checked here.
* The orphan scan covers the FKs and the cross-module references listed; other plain-uuid
  references (e.g. `platform.audit_log.target_id`, outbox subjects) are deliberately untyped.
* Balance checks cover the demo's flows (charges, payments, allocations, adjustments); aid,
  refunds and treasury postings exist in the schema but carry no rows in the demo DB.
