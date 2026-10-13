# Batch 5 — shared contracts (Finance, Domain F)

**Status:** frozen at `batch5/foundation`. This file is the single source of truth for the three
slices **5A**, **5B** and **5C**. If a slice needs to change anything in §2–§11, it says so in its
PR description ("contract change: …") instead of silently editing a shared file; the integrator
applies it on top of the merge.

Architecture inputs: L2-F00 (+ configuration rollback and financial rule governance amendments
2026-09-01), L2-F01 (+ charge publication authority 2026-09-23, pre-student admission charge
context 2026-10-05), L2-F02, F03, F04, F05 (+ 2026-08-21), F07 (+ 2026-08-21), F09 (+ 2026-08-21),
F10 (+ 2026-08-21), F11; L3/L4 F-001…F-032, F-039…F-043, F-051…F-066, F-084/085; ADR-L5-0003
(module boundaries), -0004 (scope), -0006/-0007 (owner-local tx + outbox), -0011/-0035 (event
versioning/envelope), -0023/-0042 (WorkflowPort / Flowable), -0032 (money: `rust_decimal`),
-0034 (API profile), -0036 (schemas, business timezone); L6 screens Student Fees, Admin Finance.

Plan line (Batch 5): *fee structures, demands/invoices, concessions & scholarships, adjustments;
online payments (gateway adapter), receipts, reconciliation, receivables, GL postings, period
close. Screens: Student Fees; Admin Finance.*

**Key lessons from Batches 3/4 applied here:** every cross-slice and cross-domain dependency is a
**typed port or foundation helper defined up front** (§6) with a real implementation already wired,
so no slice waits for another; the ledger has **one** write path (`shared::posting`) that 5A and 5B
use from day one and 5C extends; money is `rust_decimal` everywhere (no floats, ever).

---

## 1. Slices and scenario coverage (one wave: 5A + 5B overlapping, then 5C)

| Slice | Scope | L3/L4 scenarios |
|-------|-------|-----------------|
| **5A** | **Charges, adjustments, aid, configuration.** Fee heads; fee structures (versioned, approval → publication); business obligation → charge generation → validation → **publication** (Dr receivables / Cr income through `shared::posting`), governed revision / adjustment, cancel / suspend / resume, dispute → resolution, instalments and due schedule, settlement closure → archival. Credit notes, debit notes, write-off adjustment (approval through the WorkflowPort), posted adjustment reversal, corrected successor record. Financial aid: approved Domain C scholarship decision (**`AidDecisionSource`**) → allocation → disbursement / charge adjustment, renewal, revocation → recovery charge, programme reconciliation. Finance configuration + financial rule policy (governed versions with rollback). Screens: Admin → Finance tabs `charges`, `adjustments`, `aid`; Student Fees panel `StudentChargesPanel` (charges, instalments, concessions / aid). | F-001 … F-006, F-020 … F-024, F-062 … F-066, F-084, F-085 |
| **5B** | **Payments, receipts, refunds, receivables.** Student / sponsor payment → receipt + accounting (Dr bank or gateway clearing / Cr receivables), partial and multi-charge allocation (`shared::charge::apply_settlement`), unidentified / unallocated receipts (Cr unapplied receipts), failure / timeout safe recovery, completed payment → reversal / chargeback, cancellation, bulk collection population. **`PaymentGatewayPort`** with the deterministic mock adapter (Razorpay-shaped: orders, webhook HMAC signature verification, idempotent on event id / key). Refund request → approval (**the reference approval of Batch 5**, WorkflowPort) → completion / cancellation / failure + retry / reversal + recovery / closure. AR recognition, overdue → dunning, settlement, write-off decision, reopening on new evidence. Screens: Admin → Finance tabs `collections`, `refunds`, `receivables`; Student Fees panel `StudentPaymentsPanel` (pay, receipts, refund request). | F-007 … F-019, F-039 … F-043 |
| **5C** | **Ledger, periods, treasury.** Balanced double-entry postings (builds on `shared::posting`: ledger views, account management on top of the seeded chart), manual journal request → approval → posting / rejection, accounting batches, journal reversal (`shared::posting::reverse`), source ↔ ledger reconciliation, trial balance, controlled accounting close, auditable history. Financial year / period establishment → open → lock → close → reopen → archive; **fills the period guard** `shared::period::require_open`. Bank accounts, bank execution of payment populations, bank statement import → reconciliation, cash position / liquidity, fund transfers, failed / returned / uncertain bank transactions. Screens: Admin → Finance tabs `ledger`, `periods`, `treasury`. | F-025 … F-032, F-051 … F-061 |

**Deferred (30 scenarios), recorded here and in `docs/batches.md`:**

| Scenarios | Area | Lands in |
|-----------|------|----------|
| F-033 … F-038 | Budget and cost centres (proposal → approved budget, revision / virement, allocation, budget control decision, cost centres, budget period closure) | Batch 9 (or a later 5D) |
| F-044 … F-050 | Accounts payable (vendor invoice → payable, settlement, disputes / holds, adjustments, suspension / write-off, period reconciliation, historical closure) | Batch 8 (procurement / inventory) |
| F-067 … F-072 | Asset financial accounting (capitalisation, depreciation, revaluation, impairment, disposal, write-off) | Batch 8 |
| F-073 … F-079 | Financial reporting, statements, statutory / management reports, KPIs and dashboards, audit reconstruction projections | Batch 8 (reporting) |
| F-080 … F-083 | Procurement finance consumption (budget commitment, three-way match, vendor liability, procurement closure) | Batch 8 |

Domains **I** (Exams, Batch 6), **E** (People, Batch 7), **G** (Campus, Batch 8) are not involved.
Domain C facts come only through the ports of §6.3; Domain C reads Finance facts only through the
real **`FinanceClearancePort`** (§6.3).

## 2. Where the code lives: module and file ownership

**Decision:** every Batch 5 aggregate lives in the existing crate `modules/finance`
(`sutra-finance`, domain F) and the PostgreSQL schema `finance`. No new crates
(`apps/api/tests/architecture.rs` fixes exactly ten domain crates). `sutra-finance` depends only
on `sutra-kernel`, `sutra-contracts`, `sutra-web`, `sutra-persistence` and **never** contains
`FROM/JOIN/INTO/UPDATE <other schema>.` (the architecture test fails): Domain B data comes through
`InstitutionReferencePort`, Domain C data through `StudentDirectory` / `AidDecisionSource` (§6).
Cross-slice reads inside Domain F go through the **foundation ports / helpers** (`state.charges`,
`state.receivables`, `shared::charge::load_*`) for the shared tables, and may query sibling slice
tables of the `finance` schema **read-only** when a port would be pointless (same crate); you never
write another slice's tables.

### 2.1 Ownership map (create/edit freely inside your own paths)

| Path | Owner |
|------|-------|
| `modules/finance/src/{charges,adjustments,aid,config}/**` | **5A** |
| `modules/finance/src/{payments,refunds,receivables}/**` (incl. `payments/gateway.rs`, the mock gateway seeded by the foundation) | **5B** |
| `modules/finance/src/{ledger,periods,treasury}/**` | **5C** |
| `modules/finance/src/shared/period.rs` — **body of `require_open` only** (signature frozen) | **5C** |
| `crates/contracts/src/finance/events/charges.rs` | **5A** |
| `crates/contracts/src/finance/events/payments.rs` | **5B** |
| `crates/contracts/src/finance/events/ledger.rs` | **5C** |
| `modules/platform/src/inbox/rules/charges_5a.rs` / `payments_5b.rs` / `ledger_5c.rs` | 5A / 5B / 5C |
| `migrations/2026103000000[1-9]_finance_*.sql` | **5A** |
| `migrations/2026103000001[0-9]_finance_*.sql`, `migrations/2026103000002[0-9]_finance_*.sql` | **5B** |
| `migrations/2026103000003[0-9]_finance_*.sql` | **5C** |
| `apps/api/tests/db_b5a.rs` / `db_b5b.rs` / `db_b5c.rs` | 5A / 5B / 5C |
| `apps/web/app/features/finance/charges/**` | **5A** |
| `apps/web/app/features/finance/payments/**` | **5B** |
| `apps/web/app/features/finance/ledger/**` | **5C** |
| `apps/web/e2e/finance.e2e.mjs` region `// ==== 5X begin` … `// ==== 5X end` (and `5X student`, `5C closing`) | each slice its own regions |
| README region `<!-- batch5:5X:begin … end -->`; `docs/batches.md` status cell `<!-- batch5:5X:status -->` | each slice its own region |

### 2.2 Shared files: foundation-owned, do not edit

`modules/finance/src/lib.rs` (router list already mounts every slice), `modules/finance/src/shared/**`
(except the 5C exception above), `modules/finance/src/{directory,clearance,fees}.rs`,
`modules/finance/src/accounts/**`, `crates/contracts/src/finance/{mod,clearance,ids,status,ports}.rs`,
`crates/contracts/src/finance/events/mod.rs`, `crates/contracts/src/student/ports.rs` (Batch 5
block), `modules/student/src/finance_ports.rs`, `crates/kernel/src/auth.rs` (all Batch 5
permissions already added), `modules/platform/src/inbox/rules/mod.rs`,
`apps/api/src/{app,openapi}.rs` (tags + `finance_deps` already added),
`apps/api/tests/architecture.rs`, `apps/web/app/routes.ts`, `apps/web/app/lib/roles.ts`,
`apps/web/app/features/finance/{index.ts,Placeholder.tsx,finance.test.tsx}`,
`apps/web/app/routes/admin/finance.tsx`, `apps/web/app/routes/student/fees.tsx`,
`apps/web/e2e/lib/harness.mjs`, `apps/api/tests/support/{mod,b5}.rs`,
`apps/api/tests/db_b5_foundation.rs`, `migrations/20261030000000_finance_batch5_foundation.sql`.
Domain B, C and D code (`modules/{institution,student,academic}/**`) is **off limits** to all three
slices (a needed change there is a contract change for the integrator).

If you cannot avoid touching one of these, keep the edit **append-only** at the end of the
relevant list/block, add a `// 5X:` (or `<!-- 5X: -->` / `-- 5X:`) comment, and list it in your PR
description. Examples: a new id at the end of your slice's block in `ids.rs`; a new number kind at
the end of your block in `shared::numbering::kind`; a new `source` constant at the end of your
block in `ports::source`; a new variant in `status.rs` (contract change — say so); a new method on
a port (contract change — the foundation adapter must implement it).

### 2.3 Generated files

`openapi/openapi.json` and `packages/api-client/src/schema.d.ts` are **generated**. Each slice
regenerates them in its own branch (`cargo run -q -p sutra-api -- openapi > openapi/openapi.json
&& pnpm gen:api`) so its drift checks pass. On merge, **never hand-merge them**: take either side,
then regenerate after the merge. `Cargo.lock` / `pnpm-lock.yaml`: no new dependencies are expected
(the foundation gave `sutra-finance` sqlx / uuid / chrono / rust_decimal / serde_json / tracing /
persistence / hmac / sha2 and `sutra-contracts` rust_decimal). If you add one, say so in the PR.
`CHANGELOG.md`: slices don't edit it.

## 3. Database

### 3.1 Migrations and money

| Range | Owner |
|-------|-------|
| `20261030000000_finance_batch5_foundation.sql` | foundation (applied): `finance.ledger_account`, `fee_head`, `student_account`, `charge`, `charge_line`, `journal_entry`, `journal_line`, `number_sequence`, `status_history`; `finance.forbid_history_change()`, `finance.assert_journal_balanced()` (deferred constraint triggers), append-only + archived-charge triggers; restated `sutra_finance` grants with `UPDATE/DELETE/TRUNCATE` **revoked** on the journal, charge lines and status history |
| `20261030000001` … `20261030000009` | 5A |
| `20261030000010` … `20261030000029` | 5B (twenty numbers: payments, gateway, receipts, unidentified receipts, bulk collection, refunds, receivables, dunning) |
| `20261030000030` … `20261030000039` | 5C |

Base `20261030…` is after the last Batch 4 migration (`20261025000032_academic_course_resources.sql`).
Reserved names are **not** pre-created as empty files (sqlx checksums applied migrations). Name
files `<version>_finance_<what>.sql`. Never edit a migration after it has been applied anywhere;
add the next number. New tables pick up the `sutra_finance` runtime grants automatically (default
privileges of `20261014000003_runtime_db_roles.sql` cover schema `finance`); an append-only slice
table additionally gets `REVOKE UPDATE, DELETE, TRUNCATE ON finance.<t> FROM sutra_finance;` plus a
`BEFORE UPDATE OR DELETE` trigger reusing `finance.forbid_history_change()`.

**Money rules (all slices):** every amount column is `numeric(18,2)` next to a `currency text NOT
NULL DEFAULT 'INR' CHECK (currency ~ '^[A-Z]{3}$')`; Rust uses `rust_decimal::Decimal` end to end
(`sqlx` decodes `numeric` into it); **no `f32`/`f64` anywhere near money**; validation through
`shared::money::require_positive` (positive, ≤ 2 decimals, fits NUMERIC(18,2)) and `require_inr`
(Batch 5 is INR-only); computed amounts (percentages, pro-rata, instalment splits) are rounded
**once** with `shared::money::round_paise` (half away from zero) and the remainder goes to the last
instalment / line. On the wire amounts are **JSON strings** (`"1250.00"`, `shared::money::fmt`)
with `#[schema(value_type = String, example = "1250.00")]`; the web formats with
`features/finance::formatInr` (string-based, never `parseFloat`).

Table conventions: `id uuid PRIMARY KEY DEFAULT uuidv7()`, `tenant_id uuid NOT NULL`,
`institution_id uuid NOT NULL` (plain uuid — **no FK into `institution`**), `student_account_id
uuid REFERENCES finance.student_account(id)` / `charge_id uuid REFERENCES finance.charge(id)` /
`journal_entry_id uuid REFERENCES finance.journal_entry(id)` / `ledger_account_id uuid REFERENCES
finance.ledger_account(id)` where the row belongs to one (FKs to the **foundation** tables are
allowed for every slice), `student_id` / `admission_id` / `scholarship_eligibility_id` **plain**
(Domain C, no FK), `status text NOT NULL CHECK (status IN (...))` with the wire names of §4.2,
`version bigint NOT NULL DEFAULT 1`, `created_at/updated_at timestamptz`, a `process_instance_id
text` on every aggregate with an approval step, a `journal_entry_id uuid` on every aggregate that
posts. Business dates (`due_date`, `posting_date`, `value_date`, period bounds) are `date` in the
institution's timezone (`shared::time::business_today`); instants are `timestamptz`.

**No foreign keys across slices.** A column referring to another slice's aggregate (e.g. 5B
`payment_allocation.charge_id` → foundation FK is fine; 5C `bank_statement_line.payment_id` → 5B
table) is a plain `uuid` when it points into another slice's table; the writer validates existence
by reading it.

### 3.2 The shared finance tables (foundation)

`finance.ledger_account` — the chart of accounts per institution: `code` (unique per institution),
`name`, `account_type` (`asset|liability|equity|income|expense`), `normal_balance`, `parent_id`,
`system_key` (seeded role, unique per institution), `is_postable`, `currency`, `status`
(`active|inactive`). **Seed:** `shared::coa::ensure_default_chart(conn, tenant, institution)`
(idempotent, run automatically by `shared::posting` / `coa::resolve` on first use; 5A calls it
before creating fee heads):

| `SystemAccount` | Code | Name | Type |
|-----------------|------|------|------|
| `CashOnHand` | 1100 | Cash on hand | asset |
| `BankMain` | 1200 | Bank — main collection account | asset |
| `GatewayClearing` | 1250 | Payment gateway clearing | asset |
| `StudentReceivables` | 1300 | Student fee receivables | asset |
| `UnappliedReceipts` | 2100 | Unapplied receipts and student advances | liability |
| `RefundsPayable` | 2200 | Refunds payable | liability |
| `DepositsHeld` | 2300 | Refundable deposits held | liability |
| `AccumulatedFund` | 3100 | Accumulated fund | equity |
| `FeeIncome` | 4100 | Tuition and academic fee income | income |
| `OtherIncome` | 4900 | Other fee income | income |
| `AidExpense` | 5100 | Scholarships and financial aid | expense |
| `ConcessionExpense` | 5200 | Fee concessions and waivers | expense |
| `BadDebtExpense` | 5300 | Fees written off | expense |
| `BankCharges` | 5900 | Bank and gateway charges | expense |

`finance.fee_head` — `code` (unique per institution, `^[A-Z0-9][A-Z0-9_-]{1,31}$`), `name`,
`category` (`tuition|admission|examination|hostel|transport|library|laboratory|fine|deposit|miscellaneous`),
`income_account_id` → ledger account, `refundable`, `status` (`draft|active|retired`).

`finance.student_account` — the **party** (receivable sub-ledger): `party_kind`
(`student|applicant`), `student_id` (Domain C, plain) **or** `admission_id` (pre-student
applicant, L2-F01 amendment 2026-10-05), snapshots `student_number`, `display_name`,
`subject_ref` (login subject → self-service), `currency`, `status` (`active|on_hold|closed`).
Unique: one per (institution, student); one per (institution, admission) while no student.

`finance.charge` — the canonical receivable obligation (L2-F01): `student_account_id`,
`student_id` / `admission_id` (denormalised), `charge_number` (`CHG-000001`, unique per
institution), `academic_year_id`, `academic_term_id`, `description`, `currency`, **`total_amount`**
(Σ lines, > 0), **`adjustment_amount`** (net of posted adjustments: credit −, debit +),
**`settled_amount`** (net allocated receipts), **`outstanding_amount`** (generated: total +
adjustment − settled; CHECKs keep it within 0 … total + adjustment), `due_date`, `source_type`
(`fee_structure|manual|admission|debit_note|aid_recovery|fine`), `source_id`, `status`
(ChargeStatus), `status_reason`, `status_changed_at`, `process_instance_id`, `published_at`,
`version`. Archived charges reject every UPDATE; no physical DELETE.

`finance.charge_line` — `charge_id`, `line_no`, `fee_head_id`, `description`, `amount` (> 0),
`income_account_id` (snapshot of the fee head's at generation). Immutable (revision = adjustment
or a successor charge).

`finance.journal_entry` / `finance.journal_line` — **the general ledger, append-only** (trigger +
privilege): header `entry_number` (`JE-000001`), `posting_date`, `financial_period_id` (5C's, plain),
`kind` (`standard|reversal|manual|adjusting|opening|closing`), `source_type` (§3.3 constants),
`source_id`, `posting_key`, `description`, `currency`, `total_debit = total_credit`,
`reverses_entry_id` (at most one reversal per entry), `posted_by`, `correlation_id`; unique
`(tenant, source_type, source_id, posting_key)` = **posting idempotency key**. Lines:
`ledger_account_id`, `debit` xor `credit` (> 0), sub-ledger dimensions `student_account_id`,
`charge_id`, `memo`. A **deferred constraint trigger** re-checks at commit that every entry has ≥ 2
lines, Σ debit = Σ credit = header totals.

`finance.number_sequence` — gap-free per-institution counters (`shared::numbering`).

`finance.status_history` — append-only: every status change of the charge (and of slice
aggregates, via `shared::lifecycle::record_history`) with `aggregate_type, aggregate_id,
from/to, reason, cause_event_type, process_instance_id, actor, correlation_id`.

### 3.3 Write rules for the shared tables

| Change | Who | How |
|--------|-----|-----|
| INSERT ledger accounts (default chart) | foundation | `shared::coa::ensure_default_chart` (auto) |
| INSERT / deactivate user ledger accounts | **5C** | own repo code (never rename or deactivate a `system_key` account that has postings) |
| INSERT / edit fee heads | **5A** | own repo code; income account via `coa::resolve` |
| INSERT party accounts | 5A, 5B | **only** `shared::account::ensure_student_account` / `ensure_applicant_account` |
| INSERT charges + lines | **5A** | `shared::charge::insert_charge` (draft, `CHG-` number, lines from active fee heads) |
| `charge.status` | **5A** | **only** `shared::lifecycle::change_charge_status` (on a row locked with `shared::charge::lock_charge`); `apply_settlement` does `published → active` (first settlement) and `closed → active` (reversal re-opens) itself |
| `charge.adjustment_amount` | **5A** | **only** `shared::charge::apply_adjustment` (charge must `accepts_adjustment`; credit never below the settled part) |
| `charge.settled_amount` | **5B** | **only** `shared::charge::apply_settlement` (`+` allocate needs `accepts_settlement`; over-allocation → 409, keep the surplus as an unapplied receipt; `−` reversal) |
| journal entries / lines | 5A, 5B, 5C | **only** `shared::posting::post` / `reverse` in the same transaction as the business change (§6.1) |
| periods guard | **5C** | fills `shared::period::require_open` |
| Read charges / accounts / balances | all | `state.charges` (`ChargeDirectory`), `state.receivables` (`ReceivableReader`), `shared::charge::{load_charge, charge_lines}` (read only, never `lock_charge` outside 5A/5B writes) |

**Posting recipes (every business change that moves money posts in the same transaction):**

| Event | Slice | Dr | Cr | `source_type` / `posting_key` |
|-------|-------|----|----|------------------------------|
| Charge published | 5A | StudentReceivables (party + charge) | each line's `income_account_id` | `charge` / `publish` |
| Charge cancelled / waived after publication | 5A | income / ConcessionExpense | StudentReceivables | `charge` / `cancel` · `waive` (or `reverse` of the publication entry when nothing was settled) |
| Credit note / concession | 5A | ConcessionExpense (or income) | StudentReceivables | `adjustment` / `post` |
| Debit note | 5A | StudentReceivables | income account | `adjustment` / `post` |
| Write-off (adjustment) | 5A | BadDebtExpense | StudentReceivables | `adjustment` / `post` |
| Adjustment reversal | 5A | mirror | mirror | `shared::posting::reverse` |
| Aid applied to a charge | 5A | AidExpense | StudentReceivables | `aid_disbursement` / `apply:<charge>` |
| Aid disbursed in cash (stipend) | 5A | AidExpense | RefundsPayable (paid out by 5B refund/payout) | `aid_disbursement` / `disburse` |
| Gateway payment captured | 5B | GatewayClearing | StudentReceivables per allocated charge; UnappliedReceipts for any surplus | `payment` / `capture` |
| Counter / bank payment | 5B | CashOnHand / BankMain | as above | `payment` / `receive` |
| Unapplied receipt allocated later | 5B | UnappliedReceipts | StudentReceivables | `payment_allocation` / `allocate` |
| Gateway settlement to bank | 5B (or 5C treasury) | BankMain (+ BankCharges) | GatewayClearing | `payment` / `settle` |
| Payment reversal / chargeback | 5B | mirror of the capture | | `shared::posting::reverse` + `apply_settlement(−)` |
| Refund approved / paid | 5B | StudentReceivables or UnappliedReceipts | RefundsPayable → then RefundsPayable / BankMain | `refund` / `approve` · `pay` |
| Receivable write-off | 5B | BadDebtExpense | StudentReceivables | `receivable_write_off` / `post` |
| Manual journal | 5C | any | any | `manual_journal` / `post` |
| Fund transfer | 5C | target bank | source bank | `fund_transfer` / `execute` |

### 3.4 Document numbers (`shared::numbering::kind`)

`charge` `CHG`, `journal` `JE` (foundation); `adjustment` `ADJ`, `credit_note` `CN`, `debit_note`
`DN`, `aid_allocation` `AID` (5A); `payment` `PAY`, `receipt` `RCT`, `refund` `RFD` (5B);
`manual_journal` `MJ`, `fund_transfer` `FT` (5C). Gap-free inside committed transactions.

### 3.5 Period guard (`shared::period`)

`require_open(conn, tenant, institution, posting_date) -> Result<Option<Uuid>, KernelError>` is
called by `shared::posting` **inside** every posting transaction. Foundation stub: always open,
`None`. **5C fills the body** (signature frozen): read the period covering the date with `FOR
SHARE`, return `ensure_accepts_postings(Some((id, status, name)))` — `409` for `planned | locked |
closed | archived`; an institution **with** periods but none covering the date → `409`; an
institution with no financial year configured stays open. 5C's DB tests prove "posting into a
closed period is refused" through 5A/5B-shaped postings (`support::b5::seed_charge` /
`seed_payment` dated into a closed period).

### 3.6 Slice table names (schema `finance`)

| Slice | Tables (prefixes a slice may extend) |
|-------|--------------------------------------|
| 5A | `fee_structure`, `fee_structure_line`, `charge_revision`, `instalment_plan`, `instalment`, `charge_dispute`, `adjustment`, `aid_allocation`, `aid_disbursement`, `aid_recovery`, `aid_programme_reconciliation`, `finance_config_version`, `finance_rule` (prefixes `fee_structure*`, `charge_revision*`, `charge_dispute*`, `instalment*`, `adjustment*`, `aid_*`, `finance_config*`, `finance_rule*`) |
| 5B | `payment`, `payment_allocation`, `receipt`, `gateway_order`, `gateway_webhook_event`, `unidentified_receipt`, `payment_batch`, `payment_batch_item`, `refund`, `receivable`, `dunning_case`, `dunning_notice`, `receivable_write_off` (prefixes `payment*`, `receipt*`, `gateway_*`, `unidentified_*`, `refund*`, `receivable*`, `dunning_*`) |
| 5C | `financial_year`, `financial_period`, `accounting_batch`, `manual_journal`, `manual_journal_line`, `ledger_reconciliation`, `trial_balance`, `bank_account`, `bank_execution_batch`, `bank_statement`, `bank_statement_line`, `bank_reconciliation`, `fund_transfer`, `bank_transaction` (prefixes `financial_year*`, `financial_period*`, `accounting_batch*`, `manual_journal*`, `ledger_*`, `trial_balance*`, `bank_*`, `fund_transfer*`) |

## 4. Kernel and contract types (already in the code)

### 4.1 Ids: `sutra_contracts::finance::ids`

`InstitutionId`/`TenantId` re-exported, plus:
- **foundation:** `LedgerAccountId`, `FeeHeadId`, `StudentAccountId`, `ChargeId`, `ChargeLineId`, `JournalEntryId`, `JournalLineId`
- **5A:** `FeeStructureId`, `FeeStructureLineId`, `ChargeRevisionId`, `InstalmentPlanId`, `InstalmentId`, `ChargeDisputeId`, `AdjustmentId`, `AidAllocationId`, `AidDisbursementId`, `AidRecoveryId`, `AidProgrammeReconciliationId`, `FinanceConfigVersionId`, `FinanceRuleId`
- **5B:** `PaymentId`, `PaymentAllocationId`, `ReceiptId`, `UnidentifiedReceiptId`, `PaymentBatchId`, `GatewayOrderId`, `RefundId`, `ReceivableId`, `DunningCaseId`, `ReceivableWriteOffId`
- **5C:** `FinancialYearId`, `FinancialPeriodId`, `AccountingBatchId`, `ManualJournalId`, `LedgerReconciliationId`, `TrialBalanceId`, `BankAccountId`, `BankExecutionBatchId`, `BankStatementId`, `BankStatementLineId`, `BankReconciliationId`, `FundTransferId`, `BankTransactionId`

All are `typed_id!` newtypes: UUIDv7, serde-transparent, OpenAPI `string/uuid`.

### 4.2 Lifecycle states: `sutra_contracts::finance::status`

Enums with `ALL`, `as_str()`, `parse()`, serde/OpenAPI `snake_case`; also the DB `CHECK` values.
The **shared** machine `ChargeStatus` carries `can_transition` in contracts; only 5A moves it.
Every other machine's transitions are slice-owned, written as a pure, unit-tested
`can_transition(from, to)` in the slice's `domain.rs`. Illegal transition → `409`.

**Shared machine (foundation): `ChargeStatus`** (L2-F01 §18–19): `draft, pending_validation,
validated, published, active, suspended, disputed, cancelled, waived, written_off, closed, archived`.
Helpers: `is_receivable()` (`published | active | disputed` — dues, clearance, dunning),
`accepts_settlement()` (`published | active`), `accepts_adjustment()` (`published | active |
disputed`), `is_visible_to_student()` (not `draft | pending_validation | validated`),
`is_terminal()` (`archived`).

| From | Legal to |
|------|----------|
| `draft` | `pending_validation`, `cancelled` |
| `pending_validation` | `validated`, `draft` (returned) |
| `validated` | `published`, `draft`, `cancelled` |
| `published` | `active`, `suspended`, `disputed`, `cancelled`, `waived`, `written_off`, `closed` |
| `active` | `suspended`, `disputed`, `cancelled`, `waived`, `written_off`, `closed` |
| `suspended` | `active` (resumed), `cancelled` |
| `disputed` | `active` (resolved), `cancelled`, `waived` |
| `closed` | `active` (only `apply_settlement` on a reversal), `archived` |
| `cancelled`, `waived`, `written_off` | `archived` |
| `archived` | — |

**Classification enums (foundation):** `ChargeSourceType`, `PartyKind`, `StudentAccountStatus`,
`FeeHeadStatus`, `FeeCategory`, `LedgerAccountType` (+ `normal_balance()`), `NormalBalance`
(= `ports::EntrySide`), `LedgerAccountStatus`, `JournalEntryKind`.

**Other enums (transitions slice-owned)**

| Enum | Values | Used by |
|------|--------|---------|
| `FeeStructureStatus` | `draft, in_review, approved, published, superseded, retired` | 5A |
| `InstalmentStatus` | `scheduled, due, overdue, paid, waived, cancelled` | 5A (5B reads) |
| `ChargeDisputeStatus` | `raised, under_review, upheld, rejected, withdrawn` | 5A |
| `AdjustmentKind` / `AdjustmentStatus` | `credit_note, debit_note, write_off, waiver, concession` / `draft, pending_approval, approved, posted, rejected, reversed, superseded` | 5A |
| `AidAllocationStatus` | `proposed, approved, allocated, disbursed, suspended, revoked, recovered, closed` | 5A |
| `FinancePolicyStatus` | `draft, in_review, effective, superseded, rolled_back, retired` | 5A config + rules |
| `PaymentStatus` / `PaymentChannel` | `initiated, pending, completed, failed, expired, cancelled, reversed, charged_back` / `gateway, cash, cheque, bank_transfer, demand_draft, sponsor` | 5B |
| `AllocationStatus` / `ReceiptStatus` | `allocated, reversed` / `issued, cancelled` | 5B |
| `UnidentifiedReceiptStatus` | `unidentified, under_investigation, identified, allocated, refunded` | 5B |
| `PaymentBatchStatus` | `draft, validated, executing, completed, partially_completed, failed` | 5B |
| `RefundStatus` | `requested, under_review, approved, rejected, processing, completed, failed, cancelled, reversed, closed` | 5B |
| `ReceivableStatus` / `DunningStatus` | `open, partially_settled, overdue, settled, written_off, reopened` / `open, notice_sent, escalated, resolved, cancelled` | 5B |
| `FinancialYearStatus` | `draft, open, closing, closed, archived` | 5C |
| `FinancialPeriodStatus` (+ `accepts_postings()` = `open | reopened`) | `planned, open, locked, closed, reopened, archived` | 5C (period guard) |
| `AccountingBatchStatus` / `ManualJournalStatus` | `draft, validated, posted, rejected, failed` / `draft, pending_approval, approved, posted, rejected, reversed` | 5C |
| `ReconciliationStatus` | `open, matched, exceptions, resolved, closed` | 5C ledger + bank reconciliation |
| `BankAccountStatus` / `BankStatementStatus` | `requested, approved, active, suspended, closed` / `imported, reconciling, reconciled, failed` | 5C |
| `BankTransactionStatus` / `FundTransferStatus` | `pending, executed, failed, returned, uncertain, resolved` / `requested, approved, executed, settled, failed, cancelled` | 5C |

### 4.3 Events: `sutra_contracts::finance::events`

`type = sutraerp.finance.<aggregate>.<verb>`, `dataschema = urn:sutraerp:schema:finance.<aggregate>.<verb>:1`,
`source = /sutraerp/finance`, envelope `subject` = aggregate id, envelope scope =
`ExecutionScope::institution(tenant, <institution>)`. Emit via `shared::publish::event` inside the
owner-local tx; audit via `shared::publish::audit` with action `finance.<aggregate>.<command>`. V1
payloads are frozen: additive optional fields only. Money in payloads is a decimal string +
`currency`.

Shared payloads (`events/mod.rs`): **`StatusChangedV1`** `{aggregateId, institutionId,
studentAccountId?, studentId?, chargeId?, fromStatus?, toStatus, version, amount?, currency?,
reason?, processInstanceId?, journalEntryId?, studentSubjectRef?}`, **`ModifiedV1`**
`{aggregateId, institutionId, version, changedFields[]}`, **`StudentAccountOpenedV1`**,
**`JournalEntryPostedV1`** `{journalEntryId, institutionId, entryNumber, postingDate, kind,
sourceType, sourceId, postingKey, amount, currency, financialPeriodId?, reversesEntryId?}`.
Subject refs let pure inbox rules notify the student without a lookup.

| Owner / file | Constant | `type` suffix | Meaning |
|--------------|----------|---------------|---------|
| foundation `mod.rs` | `STUDENT_ACCOUNT_OPENED` | `student_account.opened` | emitted by `shared::account::ensure_*` |
| foundation | `JOURNAL_ENTRY_POSTED` / `JOURNAL_ENTRY_REVERSED` | `journal_entry.posted` / `reversed` | emitted by `shared::posting` |
| 5A `charges.rs` | `FEE_STRUCTURE_PUBLISHED` | `fee_structure.published` | F-001 |
| 5A | `CHARGE_PUBLISHED`, `CHARGE_REVISED` (ModifiedV1), `CHARGE_SUSPENDED`, `CHARGE_RESUMED`, `CHARGE_CANCELLED`, `CHARGE_DISPUTED`, `CHARGE_DISPUTE_RESOLVED`, `CHARGE_CLOSED`, `CHARGE_ARCHIVED` | `charge.*`, `charge_dispute.resolved` | F-001…006 |
| 5A | `INSTALMENT_PLAN_CREATED`, `INSTALMENT_DUE` | `instalment_plan.created`, `instalment.due` | F-005 |
| 5A | `ADJUSTMENT_SUBMITTED`, `ADJUSTMENT_POSTED`, `ADJUSTMENT_REVERSED`, `ADJUSTMENT_SUPERSEDED` | `adjustment.*` | F-020…024 |
| 5A | `AID_ALLOCATED`, `AID_DISBURSED`, `AID_RENEWED`, `AID_REVOKED` | `aid_allocation.*` | F-062…065 |
| 5A | `FINANCE_CONFIG_EFFECTIVE`, `FINANCE_RULE_EFFECTIVE` | `finance_config.effective`, `finance_rule.effective` | F-084/085 |
| 5B `payments.rs` | `PAYMENT_INITIATED`, `PAYMENT_COMPLETED`, `PAYMENT_FAILED`, `PAYMENT_CANCELLED`, `PAYMENT_REVERSED`, `RECEIPT_ISSUED`, `UNIDENTIFIED_RECEIPT_RESOLVED`, `PAYMENT_BATCH_COMPLETED` | `payment.*`, `receipt.issued`, … | F-007…013 |
| 5B | `REFUND_REQUESTED`, `REFUND_DECIDED`, `REFUND_COMPLETED`, `REFUND_CANCELLED`, `REFUND_FAILED`, `REFUND_REVERSED` | `refund.*` | F-014…019 |
| 5B | `RECEIVABLE_RECOGNISED`, `RECEIVABLE_OVERDUE`, `DUNNING_NOTICE_SENT`, `RECEIVABLE_SETTLED`, `RECEIVABLE_WRITTEN_OFF`, `RECEIVABLE_REOPENED` | `receivable.*`, `dunning.notice_sent` | F-039…043 |
| 5C `ledger.rs` | `MANUAL_JOURNAL_SUBMITTED`, `MANUAL_JOURNAL_POSTED`, `ACCOUNTING_BATCH_POSTED`, `LEDGER_RECONCILIATION_COMPLETED`, `TRIAL_BALANCE_GENERATED` | `manual_journal.*`, … | F-025…032 |
| 5C | `FINANCIAL_PERIOD_OPENED`, `_LOCKED`, `_CLOSED`, `_REOPENED`, `FINANCIAL_YEAR_CLOSED` | `financial_period.*`, `financial_year.closed` | F-051…055 |
| 5C | `BANK_ACCOUNT_ACTIVATED`, `BANK_STATEMENT_RECONCILED`, `FUND_TRANSFER_SETTLED`, `BANK_TRANSACTION_FAILED` | `bank_account.activated`, … | F-056…061 |

Each slice file exports `EVENT_TYPES`; `events::all_event_types()` lists every one and a unit test
keeps them unique and well formed (4 dot-separated parts, lowercase). A slice may add an event to
its own file (constant + `EVENT_TYPES` entry + payload) and must add it to the table above in its
PR. **Consumed Domain C event:** `sutraerp.student.scholarship_eligibility.handed_off` (3C) — only
the 5A inbox rule may claim it (to-do "Allocate financial aid"); the aid data itself is read through
`AidDecisionSource` when the admin acts (no inbox projection into `finance`).

### 4.4 Permissions and role mapping (`sutra_kernel::auth::Permission`)

Staff reads of every Domain F resource require `finance.read` (Administrator only — the finance
office; faculty have none). Students act only on their **own** fees through `finance.self`: the
handler resolves the party accounts from the session subject (`shared::me::my_accounts` /
`require_own_account`, empty / 404 otherwise) and never takes a student or account id from the
client. Every manage permission is **Administrator only**; governed **decisions** (refund
approval, adjustment / write-off approval, manual journal approval, payment reversal, aid
revocation, fund transfer approval) use `finance.approve` (step-up) and period close / reopen /
archive uses `period.close` (step-up). Unit test `batch5_domain_f_grants`.

| Variant | Name | Step-up | Roles | Slice: used for |
|---------|------|---------|-------|-----------------|
| `FinanceRead` | `finance.read` | no | Admin | all slices: staff reads |
| `FinanceSelfService` | `finance.self` | no | Student | 5A own charges / instalments / aid; 5B pay, receipts, refund requests |
| `ChargeManage` | `charge.manage` | no | Admin | 5A fee heads, structures, charges, revision, suspend / resume / cancel, disputes, instalments, closure |
| `AdjustmentManage` | `adjustment.manage` | no | Admin | 5A adjustment drafts, submit, reversal requests, successors |
| `AidManage` | `aid.manage` | no | Admin | 5A aid allocation, disbursement, renewal, recovery, reconciliation |
| `FinanceConfigManage` | `finance.config.manage` | no | Admin | 5A configuration + rule versions |
| `PaymentManage` | `payment.manage` | no | Admin | 5B counter / bank payments, allocation, receipts, unidentified receipts, recovery, cancellation, bulk collection |
| `RefundManage` | `refund.manage` | no | Admin | 5B refund requests on behalf, processing, retry, closure |
| `ReceivableManage` | `receivable.manage` | no | Admin | 5B recognition, dunning, write-off requests, reopening |
| `LedgerManage` | `ledger.manage` | no | Admin | 5C chart of accounts, manual journal drafts, batches, reconciliation, trial balance |
| `PeriodManage` | `period.manage` | no | Admin | 5C years / periods: establish, open, lock |
| `TreasuryManage` | `treasury.manage` | no | Admin | 5C bank accounts, execution, statements, reconciliation, cash position, transfer requests, failed transactions |
| `FinanceApprove` | `finance.approve` | **yes** | Admin | all slices: approval **decisions** (§9) |
| `PeriodClose` | `period.close` | **yes** | Admin | 5C close / reopen / archive periods and years, accounting close |

Step-up is enforced only when the deployment enables it; DB tests run in dev mode without it.

## 5. HTTP API

### 5.1 Conventions (as Domains B, C and D)

- Every handler starts with `auth.require(Permission::…)?` (or `shared::me::my_accounts`). Tenant
  from the session; build `shared::scope::institution_scope(ctx, <institution>)` for events/audit.
- Before a write: `shared::guard::require_institution_writable`.
- Create: `POST`, required `Idempotency-Key` (`sutra_web::idempotency::require_idempotency_key`),
  `201` + `ETag`. Update or state command: `If-Match` required (`shared::http::if_match`: 428
  missing, 412 stale); response = updated resource + `ETag`. **Money-moving commands** (publish,
  post adjustment, pay, allocate, refund, journal post) additionally require `Idempotency-Key`.
- State commands are `POST …/{id}/<verb>` with an optional `{ "reason": "…" }`; literal paths per
  command, no generic `/{command}` route.
- Lists: `GET` collection under the parent, `?status=` filter, `{ "items": [...] }`; `?limit=` ≤ 200
  (default 50) and `nextCursor` where lists can grow (charges, payments, journal entries).
- Errors: illegal transition / unbalanced / over-allocation / closed period → `409`; validation
  (amount, currency, dates) → `422`; foreign tenant / unknown id → `404`; Domain B/C reference not
  found → `422` on the field; webhook signature invalid → `401`.
- JSON members camelCase, statuses snake_case (§4.2), business dates `YYYY-MM-DD`, **amounts as
  decimal strings**.
- **Every `#[utoipa::path]` sets an explicit `operation_id`** with the slice prefix below and the
  slice's `tag` (enforced by `architecture.rs::finance_operations_follow_the_slice_prefix_and_tag_rules`,
  which also reserves the `finance_` prefix for Domain F and checks the `{id}` rule).

### 5.2 Path prefixes, operation_id prefixes, tags (all under `/api/v1/finance`)

**Path parameter rule (Batch 2/3/4 lesson):** the first parameter after any collection segment is
always `{id}`; deeper parameters get names (`/charges/{id}/instalments/{instalmentId}`).
Self-service lives under `/me/…`. The **gateway webhook** is the one unauthenticated route:
`POST /api/v1/finance/gateway/webhook` (5B). It needs no auth change: a request without a session
cookie passes `sutra_web::auth::authenticate` anonymously (no CSRF check without a session) and a
server-to-server call carries no `Origin`; the handler takes **no** `Authenticated` extractor,
verifies `X-Razorpay-Signature` with `state.gateway.verify_webhook` over the raw body (401 on
mismatch) and is idempotent on the gateway event id.

| Owner | Paths | operation_id prefixes | tag |
|-------|-------|-----------------------|-----|
| foundation | `GET /institutions/{id}/student-accounts`, `GET /student-accounts/{id}`, `GET /me/accounts` | `finance_account_` | `finance-accounts` |
| 5A | `/institutions/{id}/fee-heads`, `/fee-heads/{id}`, `/institutions/{id}/fee-structures`, `/fee-structures/{id}[/submit,/decision,/publish,/supersede,/generate-charges]`, `/institutions/{id}/charges`, `/student-accounts/{id}/charges`, `/charges/{id}[/validate,/publish,/revise,/suspend,/resume,/cancel,/close,/archive]`, `/charges/{id}/instalments[/{instalmentId}]`, `/charges/{id}/disputes`, `/disputes/{id}[/decision,/withdraw]`, `/me/charges` | `finance_fee_head_`, `finance_fee_structure_`, `finance_charge_` (incl. `finance_charge_self_`), `finance_instalment_`, `finance_dispute_` | `finance-charges` |
| 5A | `/charges/{id}/adjustments`, `/institutions/{id}/adjustments`, `/adjustments/{id}[/submit,/decision,/reverse,/correct]` | `finance_adjustment_` | `finance-adjustments` |
| 5A | `/institutions/{id}/aid-decisions` (handed-off Domain C decisions), `/institutions/{id}/aid-allocations`, `/aid-allocations/{id}[/approve,/disburse,/renew,/revoke,/recover]`, `/institutions/{id}/aid-reconciliations`, `/me/aid` | `finance_aid_` | `financial-aid` |
| 5A | `/institutions/{id}/finance-config`, `/finance-config/{id}[/submit,/decision,/activate,/rollback]`, `/institutions/{id}/finance-rules`, `/finance-rules/{id}[/…]` | `finance_config_`, `finance_rule_` | `finance-config` |
| 5B | `/student-accounts/{id}/payments`, `/institutions/{id}/payments`, `/payments/{id}[/allocate,/cancel,/recover,/reverse,/chargeback]`, `/payments/{id}/receipt`, `/receipts/{id}`, `/institutions/{id}/unidentified-receipts`, `/unidentified-receipts/{id}[/identify,/allocate,/refund]`, `/institutions/{id}/collections`, `/collections/{id}[/…]`, `/gateway/webhook`, `/me/payments[/…]` (initiate with the mock gateway), `/me/receipts` | `finance_payment_` (incl. `finance_payment_self_`), `finance_receipt_`, `finance_unidentified_`, `finance_collection_`, `finance_gateway_` | `finance-payments` |
| 5B | `/student-accounts/{id}/refunds`, `/institutions/{id}/refunds`, `/refunds/{id}[/decision,/process,/complete,/cancel,/retry,/reverse,/close]`, `/me/refunds` | `finance_refund_` | `finance-refunds` |
| 5B | `/institutions/{id}/receivables`, `/receivables/{id}[/dunning,/write-off,/reopen]`, `/institutions/{id}/dunning-runs`, `/dunning-cases/{id}[/…]` | `finance_receivable_`, `finance_dunning_` | `finance-receivables` |
| 5C | `/institutions/{id}/ledger-accounts`, `/ledger-accounts/{id}`, `/institutions/{id}/journal-entries`, `/journal-entries/{id}[/reverse]`, `/institutions/{id}/manual-journals`, `/manual-journals/{id}[/submit,/decision,/post]`, `/institutions/{id}/accounting-batches`, `/accounting-batches/{id}[/…]`, `/institutions/{id}/reconciliations`, `/reconciliations/{id}`, `/institutions/{id}/trial-balance` | `finance_ledger_`, `finance_journal_`, `finance_batch_`, `finance_reconciliation_`, `finance_trial_balance_` | `finance-ledger` |
| 5C | `/institutions/{id}/financial-years`, `/financial-years/{id}[/open,/close,/archive]`, `/financial-years/{id}/periods`, `/financial-periods/{id}[/open,/lock,/close,/reopen,/archive]` | `finance_year_`, `finance_period_` | `finance-periods` |
| 5C | `/institutions/{id}/bank-accounts`, `/bank-accounts/{id}[/…]`, `/bank-accounts/{id}/statements`, `/bank-statements/{id}[/reconcile]`, `/institutions/{id}/fund-transfers`, `/fund-transfers/{id}[/…]`, `/institutions/{id}/cash-position`, `/institutions/{id}/bank-transactions`, `/bank-transactions/{id}[/resolve]` | `finance_bank_`, `finance_statement_`, `finance_transfer_`, `finance_cash_` | `finance-treasury` |

The tags are already in `apps/api/src/openapi.rs`; the router mount points are already registered
in `sutra_finance::router_with_state` (`accounts` → foundation; `charges`, `adjustments`, `aid`,
`config` → 5A; `payments`, `refunds`, `receivables` → 5B; `ledger`, `periods`, `treasury` → 5C).
Each slice only fills `pub fn router(state: FinanceState) -> OpenApiRouter` in its own `mod.rs`.

### 5.3 Minimum response fields other slices / screens rely on

- **Account** (foundation, `GET /student-accounts/{id}`, `/me/accounts`): `{account: {id,
  institutionId, partyKind, studentId?, admissionId?, studentNumber?, displayName, currency,
  status, balance: {totalCharged, totalAdjusted, totalSettled, outstanding, disputedOutstanding,
  overdue, openCharges, chargesRaised, currency}}, charges: [{id, chargeNumber, description,
  currency, totalAmount, adjustmentAmount, settledAmount, outstandingAmount, dueDate?,
  academicTermId?, status, version}]}` (student view hides `draft | pending_validation | validated`).
- **Charge** (5A): the charge view above + `studentAccountId, studentId?, lines[{lineNo, feeHeadId,
  feeHeadCode, description, amount}], instalments[{id, sequence, dueDate, amount, status}],
  publishedAt?, journalEntryId?`.
- **Payment** (5B): `{id, paymentNumber, studentAccountId, channel, amount, currency, status,
  gatewayOrderRef?, gatewayPaymentRef?, receivedOn, allocations[{chargeId, chargeNumber, amount,
  status}], unappliedAmount, receipt?{id, receiptNumber, issuedAt}, journalEntryId?, version}`.
- **Refund** (5B): `{id, refundNumber, studentAccountId, paymentId?, amount, currency, reason,
  status, processInstanceId?, journalEntryId?, version}`.
- **Journal entry** (5C): `{id, entryNumber, postingDate, kind, sourceType, sourceId, description,
  currency, totalDebit, totalCredit, financialPeriodId?, reversesEntryId?, lines[{lineNo,
  ledgerAccountId, accountCode, accountName, debit, credit, studentAccountId?, chargeId?, memo?}]}`.
- **Trial balance** (5C): `{asOf, periodId?, rows[{ledgerAccountId, code, name, accountType,
  debit, credit, balance}], totalDebit, totalCredit, balanced}`.
- Collections: `{ "items": [ … ] }`.

## 6. Cross-slice runtime rules and ports

### 6.1 Foundation helpers (`modules/finance/src/shared/`)

- `state::FinanceState` — `pool, workflow, approvals, references, students, aid, gateway,
  charges, receivables, posting`. Built by `FinanceDeps::into_state()` (auto-approve, Postgres
  directories) or by hand in tests (port doubles, `AwaitDecision`).
- `scope`, `guard` (`require_institution_writable`, `require_institution`), `http` (`with_etag`,
  `if_match`, `actor`), `publish` (`event` + `audit`), `approval` (§9), `time::business_today`.
- `money` — `require_positive`, `require_inr`, `round_paise`, `fmt`, `display_inr`.
- `numbering::next(conn, tenant, institution, kind::…)` (§3.4).
- `coa` — `DEFAULT_CHART`, `ensure_default_chart`, `resolve(conn, tenant, institution, AccountRef)`.
- `account` — `ensure_student_account(conn, tenant, institution, &StudentSummary, actor)`,
  `ensure_applicant_account(…)`, `load_account`.
- `charge` — `ChargeRow` (+ `summary()`), `load_charge`, `lock_charge`, `charge_lines`,
  `insert_charge(NewCharge)`, `apply_settlement` (5B), `apply_adjustment` (5A), pure checks
  `settlement_after` / `adjustment_after`.
- `lifecycle` — `change_charge_status` (5A), `record_history` (any slice, own aggregates).
- `posting` — **`post(conn, tenant, &PostingRequest, actor)`** and **`reverse(conn, tenant,
  entry, posting_date, reason, actor)`**: validate (≥ 2 lines, positive, scale ≤ 2, balanced),
  idempotent on `(source_type, source_id, posting_key)` (`already_posted`), period guard, account
  resolution (default chart seeded on demand; inactive / non-postable → 409), `JE-` number,
  header + lines, `journal_entry.posted|reversed` event + audit. **Call it in the same transaction
  as the business change** (charge publication, adjustment posting, payment capture, refund, …) so
  the business record and the ledger commit or roll back together; store the returned
  `journal_entry_id` on your aggregate.
- `period::require_open` (stub; 5C fills it), `period::ensure_accepts_postings` (pure rule).
- `me` — `my_accounts`, `require_own_account` (student self-service).

### 6.2 Typed ports (all defined and wired by the foundation)

| Port | Defined in | Implementation wired now | Consumer | Notes |
|------|-----------|--------------------------|----------|-------|
| **`ChargeDirectory`** (`charge`, `charges_of_account`, `open_charges_of_account` (allocation order: due date, number), `student_account`, `account_of_student`, `account_of_admission`, `accounts_of_subject`) — "5A → 5B" | `sutra_contracts::finance::ports` | `sutra_finance::PgChargeDirectory` over the foundation tables (**real**) | **5B** allocation + Student Fees pay panel, **5C** reconciliation, `shared::me`, later domains | 5B never waits for 5A: the shape is foundation-owned and seeded in tests by `support::b5::seed_charge`. |
| **`ReceivableReader`** (`account_balance(account, as_of)` → total charged / adjusted / settled, outstanding, disputed, overdue, counts) | same | `sutra_finance::PgReceivableReader` (**real**) | 5B receivables / dunning, Student Fees summary, foundation account endpoints | Overdue uses the institution business date the caller passes. |
| **`PostingPort`** (`post`, `reverse`) — "5A/5B → 5C" | same | `sutra_finance::PgPostingPort` (**real**, own tx around `shared::posting`) | code **outside** a finance transaction (other domains later, background jobs) | Inside Domain F call `shared::posting::post` on your transaction instead (same validation, same idempotency). 5C owns the journal / period logic **on top of** this single write path; it never adds a second one. |
| **`PaymentGatewayPort`** (`name`, `create_order`, `fetch_outcome`, `verify_webhook`, `refund`) | same | `sutra_finance::MockPaymentGateway` (**deterministic mock**, `payments/gateway.rs`, 5B owns and may extend) | **5B** | Razorpay-shaped. Paise `.13` → failed, `.99` → pending (timeout), else captured; orders / refunds idempotent on the key; webhook = HMAC-SHA256 hex over the raw body (`MockPaymentGateway::sign` for tests, dev secret `DEV_WEBHOOK_SECRET`). 5B stores the amount with the payment and resolves recovery with `outcome_for` (or extends the adapter with an order book). |
| **`AidDecisionSource`** (`handed_off_decisions(institution, student?)`, `decision(eligibility_id)`) — Domain C → 5A | `sutra_contracts::student::ports` (Batch 5 block) | `sutra_student::PgAidDecisionSource` over 3C's `scholarship_eligibility` (`handed_off_at` set) + `scholarship` (**real**) | **5A** aid allocation / renewal / revocation | `AidDecision {eligibilityId, scholarshipId, code, name, sponsorType, benefitType (fixed_amount / fee_percentage / fee_waiver / stipend), benefitValue: Decimal?, currency, institutionId, studentId, studentContextId, academicYearId?, status, validFrom?, validUntil?, renewalCount, handedOffAt}`; `is_live()` = `active | renewal`. Idempotency: one allocation per `eligibility_id` (unique index in 5A's table). **No cross-schema SQL.** |
| **`FinanceClearancePort`** (Batch 3: `admission_clearance`, `student_dues`) — Finance → Domain C | `sutra_contracts::finance` (`clearance.rs`) | `sutra_finance::PgFinanceClearance` (**real**, replaces `StubFinanceClearance`) via `sutra_api::app::student_deps` | 3A admission financial clearance, 3B finance holds, 3C exit clearance | No charge ever raised for the subject → `NotApplicable` (Batch 3 behaviour unchanged; db_b3a/b3c stay green); unpaid `published | active` → `Outstanding {"₹1,25,000.00 outstanding on 1 charge"}`; only disputed dues → `Pending`; all settled → `Cleared {reference: "finance:student_account:<id>"}`. Admission queries match `admission_id` (applicant charges) or the student id when given. |
| Domain B reference | `sutra_contracts::institution::reference` | `PgInstitutionReferences` (**real**) | all slices | institution + timezone (business dates), academic years / terms (charge term, financial year alignment). |
| Students | `sutra_contracts::student::ports::StudentDirectory` | `PgStudentDirectory` (**real**) | 5A party accounts (snapshot), self-service | A charge needs a student with a live context in the institution (or an admission for applicant charges). |

Tests can swap any port by building a `FinanceState` by hand (e.g. a gateway double) and serving
`sutra_finance::router_with_state(state)`; `sutra_api::app::finance_deps(pool, workflow)` gives the
production adapters.

## 7. Cross-slice dependencies and readiness

- **Charges, accounts, the chart and the ledger are foundation-owned**, so 5B/5C never wait for
  5A and 5A/5C never wait for 5B: tests seed through `apps/api/tests/support/b5.rs`
  (`finance_setup` → active institution + seeded student linked to a login + default chart +
  `TUITION` fee head + the student's account; `seed_charge(owner, setup, "125000.00", "published",
  due)` → charge + publication posting; `seed_payment(owner, setup, charge, "100000.00")` →
  `apply_settlement` + Dr bank / Cr receivables posting exactly like 5B). 5A's real flow creates
  the same shape, so behaviour after merge is identical.
- **5A ↔ 5B:** 5B allocates only to charges where `accepts_settlement()`; 5A's credit notes never
  push `total + adjustment` below `settled` (`apply_adjustment` refuses; refund the settled part
  through 5B instead). Instalments (5A) are a schedule over a charge; 5B allocates to the
  **charge** (oldest due first) and 5A derives instalment `paid` from `settled_amount`.
- **5A ↔ 5B (aid stipends / refunds):** cash aid is posted by 5A to RefundsPayable; 5B pays it out
  (refund / payout flow) — 5B owns the bank movement.
- **5C ← 5A/5B:** 5C reads every entry they post (source_type / source_id link back); 5C's
  reconciliation compares `finance.charge` / 5B tables with the journal. 5C fills the period guard;
  until it merges every period is open. 5C **never** writes 5A/5B tables and never posts on their
  behalf (manual journals use `source_type = manual_journal`).
- **Finance ↔ Domain C:** clearance through `FinanceClearancePort` (real); aid through
  `AidDecisionSource`; never `student.*` SQL.
- Merge order: **5A → 5B → 5C** (§12).

## 8. Inbox consumer hooks (`modules/platform/src/inbox/rules/`)

`rules::plan` dispatches by event type to `charges_5a.rs` (5A), `payments_5b.rs` (5B),
`ledger_5c.rs` (5C); each declares `EVENT_TYPES` (only types from its own event file — test
`slices_never_claim_the_same_type`; **exception:** 5A may claim
`student::events::scholarship::SCHOLARSHIP_ELIGIBILITY_HANDED_OFF`) and a pure `plan`. The
foundation files start with **empty** `EVENT_TYPES`; add a type only **with** its rule. Category:
reuse `institution` (no new notification category in Batch 5, so no platform migration). Notify
students with `Recipient::Subject(studentSubjectRef)`; otherwise `Recipient::Role(…)`. To-dos with
`todos::SystemTodo`.

| Slice | Types | Projection |
|-------|-------|------------|
| 5A | `charge.published`, `instalment.due`, `aid_allocation.allocated` | notification to the student (link `/fees`) |
| 5A | `adjustment.submitted` (AwaitDecision), Domain C `scholarship_eligibility.handed_off` | to-do (admins) "Approve adjustment …" / "Allocate financial aid …" |
| 5B | `payment.completed`, `payment.failed`, `refund.decided`, `refund.completed`, `receivable.overdue`, `dunning.notice_sent` | notification to the student |
| 5B | `refund.requested` (AwaitDecision) | to-do (admins) "Decide refund …" |
| 5C | `manual_journal.submitted` (AwaitDecision), `bank_transaction.failed` | to-do (admins) |
| 5C | `financial_period.closed` | notification to `Role::Administrator` |

## 9. WorkflowPort usage for approval steps

`shared::approval` wraps `WorkflowPort` exactly like Domains B, C and D: the composition root passes
the platform `NoopWorkflowAdapter` and `ApprovalMode::AutoApprove`.

1. Validate the command and current state (read only).
2. `request_approval(&state, &scope, process::<KEY>, business_key(aggregate, id, version), vars)`
   **before** opening the tx (deterministic key → retry-safe).
3. One owner-local tx: re-check the version; set the pending state (`under_review`,
   `pending_approval`, …); store `process_instance_id`; emit the *submitted/requested* event. If
   the outcome is `ApprovalOutcome::Approved`, **also** apply the decision in the same tx (final
   state + decision event with `processInstanceId`, **its ledger posting**, audit actor
   `system:auto-approval`).
4. Always expose the explicit decision endpoint (`POST …/{id}/decision {decision: approve|reject,
   reason}` with `finance.approve`) so `AwaitDecision` (Flowable, Batch 9) needs no API change.
   Each slice has at least one DB test building a `FinanceState` with `ApprovalMode::AwaitDecision`
   (via `sutra_finance::router_with_state` and `sutra_api::app::finance_deps`).

Process keys (`shared::approval::process`): 5A `FEE_STRUCTURE_APPROVAL`, `CHARGE_PUBLICATION`,
`CHARGE_REVISION`, `CHARGE_CANCELLATION`, `CHARGE_DISPUTE_RESOLUTION`, `ADJUSTMENT_APPROVAL`
(credit / debit notes and **write-offs**), `ADJUSTMENT_REVERSAL`, `AID_ALLOCATION`,
`AID_REVOCATION`, `FINANCE_CONFIG_CHANGE`, `FINANCE_RULE_CHANGE`; 5B `PAYMENT_REVERSAL`,
`PAYMENT_BATCH_APPROVAL`, **`REFUND_APPROVAL` (the reference approval of Batch 5:
requested → under_review → approved | rejected)**, `REFUND_REVERSAL`, `RECEIVABLE_WRITE_OFF`,
`RECEIVABLE_REOPEN`; 5C **`MANUAL_JOURNAL_APPROVAL`**, `JOURNAL_REVERSAL`,
`ACCOUNTING_BATCH_APPROVAL`, `PERIOD_CLOSE`, `PERIOD_REOPEN`, `YEAR_CLOSE`,
`BANK_ACCOUNT_APPROVAL`, `FUND_TRANSFER_APPROVAL`. Use only the keys of your slice; a new key is a
contract change.

## 10. Web

| Route | File (owner) | Content |
|-------|--------------|---------|
| `/finance/:tab?` | `routes/admin/finance.tsx` (foundation frame, pre-wired) | Admin → Finance tabs (`FINANCE_TABS` in `features/finance/index.ts`): `charges`, `adjustments`, `aid` (5A), `collections`, `refunds`, `receivables` (5B), `ledger`, `periods`, `treasury` (5C). Uses `features/institution/AdminScreen` (admin gate + institution picker). Linked from the admin nav ("Finance", already present). |
| `/fees` | `routes/student/fees.tsx` (foundation frame, pre-wired) | Student Fees: `StudentChargesPanel` (5A, span 8: charges, instalments, concessions / aid) + `StudentPaymentsPanel` (5B, span 4: pay with the mock gateway, receipts, refund request). Linked from the student nav ("Fees", already present). Uses `features/academic/RoleScreen` (student only). |

Each slice fills the placeholder components exported from its folder — keep the export names:
`features/finance/charges/index.tsx`: `ChargesAdminTab, AdjustmentsAdminTab, AidAdminTab,
StudentChargesPanel`; `features/finance/payments/index.tsx`: `CollectionsAdminTab,
RefundsAdminTab, ReceivablesAdminTab, StudentPaymentsPanel`; `features/finance/ledger/index.tsx`:
`LedgerAdminTab, PeriodsAdminTab, TreasuryAdminTab`. Mark real screens with `data-slice="5X"` (the
foundation tests and e2e accept either the placeholder or `data-slice`). Put API hooks in your own
folder (`features/finance/<slice>/api.ts`), use `@sutra/api-client` after regeneration, money via
`formatInr` (string in, string out), and a mock mode like `features/institution/mock.ts`. Until a
sibling slice is merged, type its calls locally in your folder. Never edit another slice's folder.
The foundation account endpoints (`/me/accounts`, `/student-accounts/{id}`) are available to
every slice for summaries.

**E2E:** `apps/web/e2e/finance.e2e.mjs` (`pnpm e2e:finance`) reuses the Batch 3 harness
(`e2e/lib/harness.mjs`). The foundation seeds an institution to `active`, two students (Aarav linked
to the dev student login), the default chart, a `TUITION` fee head, Aarav's finance account and a
published ₹1,25,000 charge `CHG-000001` with its journal entry, and walks every frame. Each slice
adds its steps **only inside its regions** (`// ==== 5A begin` … `// ==== 5A end`, `5A student`,
`5B`, `5B student`, `5C`, `5C closing`) and writes screenshots to `$E2E_SHOTS` (default
`/workspace/work/out/b5-shots`) with names prefixed `5a-`/`5b-`/`5c-`. Run:
`/workspace/work/tools/run-e2e.sh <log> /workspace/work/out/b5-shots/5X finance <worktree>`.

## 11. Worker environment: separate DB, target dir and worktree per slice

| Slice | Worktree | Branch | Database | `CARGO_TARGET_DIR` |
|-------|----------|--------|----------|--------------------|
| 5A | `/workspace/work/erp-5a` | `batch5/5a` | `postgres://sutra:sutra@localhost:5432/sutra_5a` | `/workspace/work/targets/5a` (may start from a copy-free reuse of `targets/4b` when no one else builds there) |
| 5B | `/workspace/work/erp-5b` | `batch5/5b` | `…/sutra_5b` | `/workspace/work/targets/5b` |
| 5C | `/workspace/work/erp-5c` | `batch5/5c` | `…/sutra_5c` | `/workspace/work/targets/5c` (or whichever of 5a/5b finished) |

**One heavy Rust job in the whole sandbox at a time** (`/workspace/work/tools/cargo-locked.sh`;
2 CPUs, ~7.9 GB RAM of which ~2.5 GB is the RAM-backed root fs and ~2.5 GB container-server).
At most two workers alive (authoring may overlap; builds serialize on the lock). See
`/workspace/work/tools/b5-worker-common.md` for the full worker rules (setup script, build flags,
low-memory linker, retry on SIGKILL, checkpoint pushes every 30 minutes, status lines in
`b5-status.txt`).

```sh
. /workspace/work/tools/b5f-env.sh   # template; workers copy it with their own target dir / DB
cd /workspace/work/erp-5b/sutraerp
export CARGO_TARGET_DIR=/workspace/work/targets/5b DATABASE_URL=postgres://sutra:sutra@localhost:5432/sutra_5b
/workspace/work/tools/cargo-locked.sh cargo test -j 1 -p sutra-api --features db-tests --test db_b5b
```

Quality gates per slice before pushing: `cargo fmt --all -- --check`, `cargo clippy --workspace
--all-targets --all-features -- -D warnings`, `cargo test -p sutra-finance` + the architecture
test, DB tests on a freshly recreated DB (`db_b5_foundation`, your `db_b5X`, and `db_b3a` / `db_b3c`
when you touch anything clearance-related), OpenAPI regenerated + `pnpm gen:api` with no drift after
commit, `pnpm --filter @sutra/web typecheck && pnpm lint && pnpm --filter @sutra/web test`, and the
slice's e2e regions green. Commit `[skip ci]` as `Varad1614 <Varad1614@users.noreply.github.com>`.

## 12. Merge order and known gaps

- Merge order: **5A → 5B → 5C** (5B depends on 5A only through the foundation tables; 5C only
  through `shared::posting` and the journal). Regenerate OpenAPI/TS after each merge (§2.3). After
  5B merges, the integrator adds a cross-slice test (fee structure → charge published (journal) →
  instalments → mock gateway payment → allocation + receipt (journal) → refund approval → trial
  balance balances → period close refuses a back-dated posting → clearance Cleared) in
  `apps/api/tests/db_b5_integration.rs`.
- Session pinning to one institution is not enforced for Domain F (tenant-wide staff admin, as
  Batches 2–4).
- The payment gateway is the deterministic mock until a real adapter (Razorpay) and its secrets are
  provisioned; the webhook route is the only unauthenticated Finance route.
- GST / tax on fees is out of scope (education services; no tax lines in Batch 5).
- `ApprovalMode::AwaitDecision` is wired but unused in production until the Flowable adapter lands
  (Batch 9).
- Institution status is checked through the reference port without a cross-schema row lock.
- The e2e foundation seed duplicates `DEFAULT_CHART` in `finance.e2e.mjs` (keep in sync).
