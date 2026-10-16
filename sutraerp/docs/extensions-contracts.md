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
| 1A statement import | `modules/finance/src/treasury` | accounting `bankimport.js` | 20270101000100–199 |
| 1B reconciliation | `modules/finance/src/treasury` | accounting `reconcile.js` | 20270101000200–299 |
| 1C financial statements | `modules/finance/src/ledger`, `features/reports` | accounting `reports.js`, `ledger.js` | 20270101000300–399 |
| 1D late fines + reminders | `modules/finance/src/receivables` | invoicing `latefee.js`, `reminders.js`, `aging.js` | 20270101000400–499 |
| 1E periodic billing runs | `modules/finance/src/campus_charges.rs` | childcare `core/billing.js` | 20270101000500–599 |
| 2A admissions funnel | `modules/student/src/admission` | crm `funnel.js`, `forecast.js`, `dupes.js` | 20270102000100–199 |
| 2B service-desk rules/SLA | `modules/campus/src/servicedesk` | help-desk `shared/rules.js` | 20270102000200–299 |
| 2C inventory demand + lots | `modules/resources/src/inventory` | inventory `demand.js`, `reorder.js`, `lots.js` | 20270102000300–399 |
| 2D fleet maintenance | `modules/campus/src/transport` | fleet | 20270102000400–499 |
| 3A duty rosters | `modules/people/src/workload` | shifts `core/rules.js`, `scheduling.js` | 20270103000100–199 |
| 3B hiring scorecards | `modules/people/src/hr` | hiring `core/scorecards.js` | 20270103000200–299 |
| 3C contracts & MoUs | `modules/resources/src/sourcing`, `modules/services/src/accreditation` | contracts `lib/` | 20270103000300–399 |
| 3D expense claims | `modules/people`, `modules/resources/src/payables` | expense `lib/` | 20270103000400–499 |
| 3E fee-counter receipts | `apps/web` (WebUSB/WebSerial) | pos `escpos.js`, `printer.js` | none |

Migration ranges sit after the newest existing migration (20261230000101) so they never collide
with work arriving from `erp_merge`.

## Order
P0 → 1A, 1C, 1D in parallel → 1B (needs 1A) → 1E → Phase 2 (independent) → Phase 3 (3A needs
exam sessions) → integration click-through on `ERP_EXTENSIONS`.
