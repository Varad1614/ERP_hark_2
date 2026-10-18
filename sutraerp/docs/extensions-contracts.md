# ERP_EXTENSIONS contracts: porting Everything logic into SutraERP

Branch: **`ERP_EXTENSIONS`** (created 2026-10-16 from `erp_merge` c31cc48). All work for this
programme is pushed only here; nothing goes to `erp_merge` from it.

Source: HeyPuter/everything @ `06802a1` (MIT). Credit in `THIRD_PARTY_NOTICES.md`.

## Rules for every port
1. **Port, don't import.** Rewrite the pure JS module as Rust inside the module that owns the
   domain. No Everything JS ships in the web bundle; the server is the source of truth (tenant
   scope, RBAC, audit chain, outbox unchanged).
2. **Golden vectors.** Every port adds a spec in `tools/golden/specs/` that runs the original JS
   and a Rust test replaying its fixture. The `golden` CI job fails on stale fixtures.
3. **Money is `rust_decimal`**, never floats; split amounts with `kernel::money::allocate`.
   Rounding policies stay explicit (ADR-L5-0032).
4. **Indianise:** ₹ lakh/crore grouping on screens, GST where it applies, Indian bank statement
   formats (CSV/XLS/MT940), NEP/UGC vocabulary.
5. Each slice ships migration + API + regenerated OpenAPI/TS client + reskin2-compliant screen
   + golden tests + an e2e step, with all workflows green on `ERP_EXTENSIONS`.

## Slices, ownership and migration ranges
| Slice | Owns | Original | Migrations |
|---|---|---|---|
| P0 groundwork | `crates/kernel/src/money.rs`, `tools/golden/` | accounting `money.js` | none |
| 1A statement import | `modules/finance/src/treasury` | accounting `bankimport.js` | 20270101000100–199 (used: 100) — **done**: `bankfile.rs`, `POST …/statements/preview`, import `format` csv/ofx/mt940/auto, golden `modules/finance/tests/golden/bankimport_*.json` |
| 1B reconciliation | `modules/finance/src/treasury` | accounting `reconcile.js` | 20270101000200–299 (done: 200 `bank_account_reconciliation` + `_entry`; endpoints `finance_bank_recon_*`, `finance_statement_line_suggestions`) |
| 1C financial statements | `modules/finance/src/ledger`, `features/reports` | accounting `reports.js`, `ledger.js` | 20270101000300–399 (used: 300) — **done**: `ledger/statements.rs` + `statements_http.rs`; `GET …/financial-statements/{income-expenditure,balance-sheet,cash-flow,general-ledger}` (`finance_ledger_*`, `finance.read`); period columns month / quarter / financial year, compare previous period / year; FY from the institution's financial year (April default), prior surplus folded into the accumulated fund; new `ledger_account.statement_group` (derived from role / type unless set; `statementGroup` on account create / update); web Statements panel in Admin → Finance → Ledger with CSV; golden `modules/finance/tests/golden/statements.json` |
| 1D late fines + reminders | `modules/finance/src/receivables` | invoicing `latefee.js`, `reminders.js`, `aging.js` | 20270101000400–499 (used: 400) — **done**: `latefee.rs` + `latefee_http.rs`; configurable `late_fee` rule (flat / percent once or monthly / per_day / slabs, graceDays, cap, capPercent, minOverdue, roundToRupee, feeHeadCode); new `reminder_schedule` rule (beforeDays, afterDays drive the dunning run); `POST …/late-fees/preview`, `…/late-fees`, `…/fee-reminders`; events `late_fee.charged`, `fee_reminder.sent`; golden `modules/finance/tests/golden/latefee.json` |
| 1E periodic billing runs | `modules/finance/src/charges/billing*.rs` (beside `campus_charges.rs`) | childcare `core/billing.js` | 20270101000500–599 (done: 500 `billing_plan`, `billing_plan_enrolment`, `billing_run`, `billing_run_item`, `billing_schedule`, charge source `billing_run`; endpoints `finance_charge_billing_*`; sweeper job `finance_billing_schedule`, hourly) |
| 2A admissions funnel | `modules/student/src/admission/funnel*.rs` | crm `funnel.js`, `forecast.js`, `dupes.js` | 20270102000100–199 (done: 100 `admission_enquiry`, `admission_funnel_settings`, `admission_intake_target`; endpoints `admission_funnel_*`: enquiries list/create/get/update/link-application, funnel, forecast with seat fill, duplicates, settings; linked enquiries follow their application to offered / admitted / lost; golden `modules/student/tests/golden/admission_funnel.json`) |
| 2B service-desk rules/SLA | `modules/campus/src/servicedesk/{rules,automation,automation_http}.rs` | help-desk `shared/rules.js`, `server/tickets.js` `runTriggers` | 20270102000200–299 (done: 200 `service_ticket.tags` + `first_response_at`, `ticket_rule`, `ticket_rule_run`, `ticket_view`, `ticket_sla_policy`; endpoints `campus_servicedesk_*`: rules list/create/update/delete/preview, views list/create/delete/tickets (built-in `sys-*`), sla-board, sla-policy GET/PUT, ticket tags, ticket automation log; triggers run on `open_ticket` and every office ticket command; the SLA sweep also breaches a missed first response when a policy is set; golden `modules/campus/tests/golden/servicedesk_rules.json`) |
| 2C inventory demand + lots | `modules/resources/src/inventory` | inventory `demand.js`, `reorder.js`, `lots.js` | 20270102000300–399 |
| 2D fleet maintenance | `modules/campus/src/transport` | fleet | 20270102000400–499 |
| 3A duty rosters | `modules/people/src/workload` | shifts `core/rules.js`, `scheduling.js` | 20270103000100–199 |
| 3B hiring scorecards | `modules/people/src/hr` | hiring `core/scorecards.js` | 20270103000200–299 |
| 3C contracts & MoUs | `modules/resources/src/contracts` (MoU evidence also reaches accreditation through the `resources.functional_mous` metric) | contracts `lib/dates.js`, `store.js`, `ical.js` | 20270103000300–399 (done: 300 `contract`, `contract_term`, `contract_obligation`, `contract_activity`, `contract_settings`; endpoints `resources_contract_*`: settings, list/create/get/update, decision, obligations create/update/complete/undo/delete, activities, dashboard, reconcile-terms, calendar.ics (book and one contract), mou-evidence per AQAR year; golden `modules/resources/tests/golden/contracts_renewals.json`) |
| 3D expense claims | `modules/resources/src/expenses` (`rules.rs`, `receipt_text.rs`, `ocr.rs`, `http.rs`) + `tools/ocr-sidecar` | expense `lib/` (`money.js`, `util.js`, `dates.js`, `currencies.js`, `ocr.js`, `reports.js`) | 20270103000400–499 (used: 400 `expense_settings`, `expense_receipt`, `expense_claim`, `expense`; endpoints `resources_expense_*`: settings GET/PUT, receipt upload + file, me/expenses list/create/update/delete, me/expense-claims create/update/delete/submit/withdraw, expense-claims get/decision/reimburse, institution claims list, dashboard; approval books Dr 5510 / Cr 2310 through the procurement posting port (key `expense_claim:approve`), reimbursement Dr 2310 / Cr bank (key `expense_claim:reimburse`); receipt OCR through the PaddleOCR sidecar (`SUTRA_OCR_URL`), disabled otherwise — OCR only pre-fills, the claimant confirms; golden `modules/resources/tests/golden/expense_claims.json`. Deferred: card-statement reconciliation from Everything's `reconcile.js`) |
| 3E fee-counter receipts | `apps/web` (WebUSB/WebSerial) | pos `escpos.js`, `printer.js` | none |

Migration ranges sit after the newest existing migration (20261230000101) so they never collide
with work arriving from `erp_merge`.

## Order
P0 → 1A, 1C, 1D in parallel → 1B (needs 1A) → 1E → Phase 2 (independent) → Phase 3 (3A needs
exam sessions) → integration click-through on `ERP_EXTENSIONS`.

## Integration click-through (P4)
`apps/web/e2e/extensions.e2e.mjs` — `pnpm --filter @sutra/web e2e:extensions` (fresh migrated
DB in `DATABASE_URL`, API binary from `cargo build -p sutra-api`; same harness as the other
suites). Seeds an active institution, two students, the finance foundation rows, Meera's staff
record and an open requisition, then as administrator opens every extension tile — 3E counter
printing (day close, printer setup, receipt layout, Print / Duplicate copy with `window.print`
stubbed), 1A/1B treasury file import + reconcile, 1C statements, 1E billing runs, 1D late fees +
reminders, 2A funnel, 2B service-desk rules, 2C stock demand, 2D fleet, 3A roster, 3B scorecards,
3C contracts, 3D expense claims — and as faculty My HR → Payslips → My expenses. Page errors,
console errors and `/api` ≥ 500 fail; 4xx are listed. Screenshots `NN-area-screen.png` go to
`$SHOTS_DIR` (else `$E2E_SHOTS`, else `apps/web/e2e-shots/extensions`).
