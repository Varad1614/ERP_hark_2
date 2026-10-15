# Domain F: Finance, readiness inventory (Part 1 · Stage 1)

Base: `part1/readiness` = `erp_merge` `fb205860` (CI run 38724169019, every job green).
Scope: `modules/finance` (fee structures, charges, adjustments, aid, payments, gateway, refunds, receivables, ledger and COA, periods, treasury) plus wiring in `apps/api`, web `apps/web/app/features/finance`, migrations `2026103000000x_finance_*`, L2 section F (F00 to F14 and their amendments).
How the work was done: code reading only. Nothing was built, run or fixed. "API-tested" means a named test in `apps/api/tests/db_b5*.rs` (the `Rust integration` job, `cargo test -p sutra-api --features db-tests`). "Browser-tested" means a step in `apps/web/e2e/finance.e2e.mjs` (the `E2E finance (preview)` job runs against the real API binary with `VITE_AUTH_MODE=api`). Customer validation is pending for every row.
Row data: [`F-finance.csv`](F-finance.csv) (40 rows).

## Summary

| Status | Rows |
|---|---|
| implemented | 31 |
| partial | 4 |
| mock-only | 1 |
| missing | 2 |
| deferred | 2 |
| **total** | **40** |

| Release readiness | Rows |
|---|---|
| ready | 4 |
| ready-with-gaps | 21 |
| not-ready | 13 |
| deferred | 2 |

API-tested: 35 rows. Browser-tested: 21 rows.

**What is solid:**
- Money is `rust_decimal` end to end and stored as `NUMERIC(18,2)`. No `f32`/`f64` anywhere in `modules/finance`, and amounts travel as strings on the wire (`shared/money.rs:1-4`). The web does its maths in `BigInt` paise (`features/finance/payments/mock.ts:10-21`).
- Double entry is enforced by the database. Deferred constraint triggers `finance.assert_journal_balanced()` check that debits equal credits, that totals match the header, and that each entry has at least two lines. Journals are append-only. See `migrations/20261030000000_finance_batch5_foundation.sql:195-268`.
- Postings have one write path (`shared/posting.rs`) and are idempotent on `(source, posting_key)` through `journal_entry_source_key`.
- Payments are protected against duplicates:
  - `Idempotency-Key` is required on record and initiate, backed by the unique index `payment_idempotency_unique` (`migrations/20261030000010_finance_payments.sql:70`).
  - Gateway orders are unique (`:68`).
  - Webhook events are deduplicated on `(gateway, event_id)` (`:126`, `payments/http.rs:1031-1065`).
  - The captured amount must equal the payment amount (`payments/http.rs:251`).
  - Reversal is refused while a refund holds the credit (`payments/service.rs:738-753`).

**What blocks release:**
- Every governed decision is auto-approved in every environment (DEF-F-001).
- Separation of duties does not exist (DEF-F-003).
- Online payment is mock-only, and the mock is still selectable in production (DEF-F-002).

## DEFECTS

### DEF-F-001 · P0 · Every governed finance decision is auto-approved in production, including student self-service refunds
- **Evidence:**
  - `modules/finance/src/lib.rs:105` (`into_state()` hard-codes `approvals: ApprovalMode::AutoApprove`).
  - `modules/finance/src/shared/approval.rs:149` (AutoApprove means `Approved` straight away).
  - `apps/api/src/app.rs:863-868`: the production router builds its state through `sutra_finance::router(deps)` → `into_state()`.
  - Auto-apply sites:
    - `refunds/http.rs:717-728`: refund request → approved by `Actor::system`. This includes `my_refund_request` at `refunds/http.rs:1399`, which a student can call.
    - `adjustments/repo.rs:603` (credit/debit notes, write-off adjustments).
    - `receivables/http.rs:1337-1339` (receivable write-off).
    - `ledger/journals.rs:1190-1191` (manual journals).
    - `treasury/http.rs:574-576` (bank accounts).
    - `charges/fees.rs:642` and `charges/repo.rs:628,654,824` (fee structures, charge publication, revision, cancellation).
    - `aid/repo.rs:533` (aid allocation).
  - The e2e confirms this behaviour: `apps/web/e2e/finance.e2e.mjs:342` "manual journal auto-approved and posted".
- **Root-cause hypothesis:** Batch 5 shipped AutoApprove as the default, intending tests and Batch 9 to switch to `AwaitDecision`. No configuration ever selects `AwaitDecision` outside tests. `SUTRA_WORKFLOW=flowable` does not help: the module never passes `autoApprove:false`, and the Flowable adapter defaults to `autoApprove=true` (`modules/platform/src/workflow_flowable.rs:10, 282-285`), so the BPMN takes its auto-approve branch.
- **Suggested fix:**
  - Derive `ApprovalMode` from configuration, for example `SUTRA_APPROVALS=await|auto`.
  - Default to `await` in production and refuse `auto` there.
  - Pass `autoApprove` explicitly into `request_approval` variables.
  - At minimum, never auto-approve the `requested_via = "self"` path.

### DEF-F-002 · P1 · The mock payment gateway can be enabled in production; mock captures settle fees without money
- **Evidence:**
  - `apps/api/src/config_b9.rs:279` accepts `SUTRA_PAYMENT_GATEWAY=mock` in any environment. Production only rejects a missing, development or short webhook secret (`:291-305`).
  - `modules/finance/src/payments/gateway.rs:98-109`: every amount whose paise are not `.13`/`.99` is returned as `Captured`.
  - `gateway.rs:175-189`: `fetch_outcome` reads the mock's in-memory order book.
  - `payments/http.rs:897-934`: the student "confirm" completes the payment, posts Dr gateway clearing / Cr AR and issues a receipt.
  - Mock refunds report `Processed` (`gateway.rs:215-227`).
  - SEC-9D-003 (docs/security/findings.md:11) fixed the default only.
- **Root-cause hypothesis:** the mock was kept selectable "for pilots". No real adapter exists.
- **Suggested fix:** refuse `mock` when `SUTRA_ENV=production`. Build a real Razorpay (or similar) adapter with server-side outcome verification before enabling online payments.

### DEF-F-003 · P1 · No separation of duties: requesters can approve their own refunds, adjustments and write-offs, and one role holds every finance permission
- **Evidence:**
  - `refunds/http.rs:851-905` (`refund_decide`) checks only `FinanceApprove`, `UnderReview` and the version, never `requested_by`.
  - `adjustments/repo.rs:624-660` (`decide`) has no maker check.
  - `receivables/http.rs:1378` onwards (`write-off/decision`) has no maker check.
  - Maker-checker exists only at `ledger/journals.rs:1269` and `treasury/http.rs:637`, and even there it only applies in `AwaitDecision`. The auto path decides as `Actor::system`, which bypasses it.
  - `crates/kernel/src/auth.rs:25-29` defines only Administrator, Faculty and Student. `auth.rs:613-708` gives Administrator every permission: ChargeManage, PaymentManage, RefundManage, AdjustmentManage, LedgerManage, TreasuryManage, FinanceApprove and PeriodClose.
- **Root-cause hypothesis:** "finance office = administrators" was a Batch 5 simplification (comment at `auth.rs:677`).
- **Suggested fix:**
  - Add a `requested_by != decider` guard to every decision endpoint, as journals do.
  - Split the roles: accountant, cashier, finance approver.
  - Treat auto-approval as forbidden (see DEF-F-001).

### DEF-F-004 · P3 · Webhook amount mismatch leaves no trace
- **Evidence:** `payments/http.rs:251-256` returns a 409 `Conflict` from inside the webhook transaction. The event row inserted at `:1031-1045` is rolled back, so there is no exception record or alert, and the gateway retries forever.
- **Root-cause hypothesis:** the error path was not designed for webhooks.
- **Suggested fix:** commit the event with outcome `amount_mismatch`, raise an exception case, and acknowledge with 200.

### DEF-F-005 · P2 · No duplicate-instrument protection for counter payments
- **Evidence:**
  - `payments/http.rs:338-433` (`payment_record`): duplicate protection relies only on the client-generated `Idempotency-Key`.
  - `finance.payment.external_ref` (`migrations/20261030000010_finance_payments.sql:37`) has no unique index and is never checked (`rg external_ref modules/finance/src`).
  - The same cheque number or UTR can be recorded twice with two keys, for example after a double click and reload or by two clerks.
- **Root-cause hypothesis:** idempotency was treated as sufficient.
- **Suggested fix:** warn or refuse when `(institution, channel, external_ref)` already belongs to a non-reversed payment. Add a partial unique index, or a soft check plus an override reason.

### DEF-F-006 · P2 · Late gateway capture on a cancelled, failed or expired payment is refused, so captured money has no record
- **Evidence:**
  - `payments/http.rs:244-268`: `apply_outcome(Captured)` calls `complete()`.
  - `payments/service.rs:382`: `complete()` returns `illegal(...)` unless the status can transition to `completed`.
  - The webhook returns an error, and no unidentified receipt or auto-refund is created.
- **Root-cause hypothesis:** the state machine forbids cancelled → completed, and no compensating path exists.
- **Suggested fix:** for a capture on a closed payment, create an unidentified receipt (or completed-with-flag) and open a refund or exception case.

### DEF-F-007 · P2 · The student Fees screen always offers online payment and calls it a "test gateway"
- **Evidence:** `apps/web/app/features/finance/fees.tsx:110` shows the "Pay now" button unconditionally. `:124` and `:276` show the copy "Secure checkout · test gateway". The production default is `SUTRA_PAYMENT_GATEWAY=disabled`, so every click returns 503 `dependency-unavailable` (`payments/gateway.rs:229-289`).
- **Root-cause hypothesis:** the UI has no capability or feature flag for the gateway.
- **Suggested fix:**
  - Expose gateway availability, for example via `/finance/me/accounts` or a config endpoint.
  - Hide or disable online payment when it is off and show counter-payment instructions instead.
  - Remove the "test gateway" copy.

### DEF-F-008 · P2 · Many finance workflows have an API but no UI (dead ends for operators)
- **Evidence:** the web client path tables (`features/finance/payments/api.ts:64-95`, `features/finance/ledger/api.ts`, `features/finance/charges/api.ts`) have no client for:
  - bulk collection `collections/{id}/validate|execute`: the web only lists batches, at `payments/index.tsx:218-224`;
  - `receivables/{id}/write-off/decision`;
  - `institutions/{id}/dunning-runs`, `dunning-cases/{id}/resolve|cancel`;
  - `payments/{id}/cancel|recover|chargeback`;
  - `unidentified-receipts/{id}/investigate|refund`;
  - `bank-accounts/{id}/decision|suspend|resume|close|settlements`, `bank-transactions/{id}/return`;
  - `aid-allocations/{id}/recover`;
  - `disputes/{id}/withdraw`;
  - `fee-structures/{id}/decision`.
- The write-off UI toast also says "Write-off decided" at the moment of the request (`payments/index.tsx:390`). That is only true because of DEF-F-001.
- **Root-cause hypothesis:** the UI was scoped to the auto-approve happy path.
- **Suggested fix:** add the missing screens and actions. Decision screens become mandatory once DEF-F-001 is fixed.

### DEF-F-009 · P3 · Dead seed money module rounds differently from the live code
- **Evidence:** `modules/finance/src/fees.rs:20-37,64` (`RoundingPolicy`, the example uses `MidpointNearestEven`) is still exported (`lib.rs:26`) but unused by the charge code (`charges/fees.rs`). Live rounding is half away from zero (`shared/money.rs:43-45`).
- **Root-cause hypothesis:** a foundation seed that was never removed.
- **Suggested fix:** delete `fees.rs`, or make it the single rounding policy.

### DEF-F-010 · P3 · Primary-key-only UPDATEs without `tenant_id` (defence in depth)
- **Evidence:**
  - `refunds/http.rs:460,492,544,920`
  - `receivables/http.rs:622,812,823,1014`
  - `payments/service.rs:447,681,767`
  - `ledger/journals.rs:340,974,1086`
  - `treasury/http.rs:301,557,1279`
- Each runs after a tenant-scoped locked load, so no cross-tenant path was found, but a refactor could open one. SEC-9D-004 notes that there is no row-level security.
- **Suggested fix:** add `AND tenant_id = $n` to every write, or enable PostgreSQL row-level security (RLS).

### DEF-F-011 · P3 · Approval process keys exist for workflows that do not exist
- **Evidence:** `shared/approval.rs:52-58` defines `ACCOUNTING_BATCH_APPROVAL`, `YEAR_CLOSE` and `FUND_TRANSFER_APPROVAL`, and BPMN is generated for them (docs/batch9-contracts.md:260-263). There is no accounting-batch or fund-transfer aggregate or endpoint (L2-F05 posting batches, L2-F10 fund transfer).
- **Suggested fix:** implement them or mark them deferred in the contracts.

## TEST GAPS
- **No production-mode approval test.** Every DB test passes because AutoApprove is the default. The `approvals_await_a_decision*` tests (`db_b5a.rs:1395`, `db_b5b.rs:1676`, `db_b5c.rs:1617`) build `AwaitDecision` by hand, so nothing asserts the mode the production router actually uses (DEF-F-001).
- **No self-approval negative tests** for refunds, adjustments or write-offs (only journals and bank accounts, `db_b5c.rs:1617`).
- **No test for a duplicate `external_ref`** on counter payments, and none for a capture after cancel (DEF-F-005, DEF-F-006).
- **No test that production refuses `SUTRA_PAYMENT_GATEWAY=mock`.** The config tests only check the secret (`config_b9::payment_gateway_defaults_and_production_rules`).
- **No concurrency tests:**
  - two refunds racing on the same unapplied credit (only the DB `CHECK unapplied_amount >= 0` protects);
  - parallel webhook plus confirm on one payment.
- **Not exercised by the browser:**
  - chargeback, recovery, bulk collection, dunning runs, write-off decision, payment reversal;
  - aid revoke and recover;
  - period reopen and year close;
  - bank settlements and returns;
  - charge revision, cancellation and disputes from the admin side.
- **Accounts payable / procurement postings** are covered only by `db_b8d.rs::procure_to_pay_end_to_end`. No finance-side browser test.

## DOC DISCREPANCIES
- `docs/batches.md:13` lists Batch 5 Finance as **planned**, and the file has no Batch 5 slice table. Yet `docs/batch5-contracts.md:35` says deferrals are "recorded here and in `docs/batches.md`", and 5A to 5C are on `erp_merge`.
- `docs/batch5-contracts.md:39` puts budget F-033…F-038 in "Batch 9 (or a later 5D)". Batch 9 delivered none of it (`rg -i budget modules/finance/src` finds nothing), and `docs/batches.md:205` does not list F-033…038 among the remaining deferrals.
- `docs/batches.md:201` and `docs/batch9-contracts.md:270-276` say Flowable is "proved and switched on per environment". Switching it on does not stop auto-approval, because module state is hard-coded to AutoApprove (DEF-F-001).
- `docs/security/findings.md:11` marks SEC-9D-003 "fixed". Production can still run the mock adapter (DEF-F-002), and auto-approval is not in the register at all.
- `modules/finance/src/shared/approval.rs:18-19` says "Batch 5 runs AutoApprove… (Batch 9, Flowable) the aggregate stays pending". No Batch 9 change implements that.
- The UI copy promises behaviour the backend only has because of auto-approval: "Write-off decided" (`payments/index.tsx:390`), and the e2e step names "refund approved (reference approval)" and "manual journal auto-approved".
