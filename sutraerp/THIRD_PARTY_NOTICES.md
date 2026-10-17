# Third-party notices

## HeyPuter/everything (MIT)

Parts of SutraERP's business logic are ported from The Everything Project,
<https://github.com/HeyPuter/everything>, pinned at commit
`06802a1e3749202e4db94d39c9ef5ab1a97d0ef1` (see `tools/golden/pin.json`). Ported code is rewritten
in Rust; each port names its original file in its doc comment, and its behaviour is checked
against the original with golden vectors (`tools/golden/`).

Ported so far:

| SutraERP | Original |
|---|---|
| `crates/kernel/src/money.rs` `allocate` | `src/accounting/src/lib/money.js` `allocate` |
| `modules/finance/src/treasury/bankfile.rs` (CSV, dates, amounts, column guess, OFX, de-dupe) | `src/accounting/src/lib/bankimport.js`, `csv.js`, `money.js` `parseMoney` |
| `modules/finance/src/receivables/latefee.rs` (instalment fill, overdue base, due date, late fee, reminder stage) | `src/invoicing/src/lib/money.js` `scheduleRows`, `doc.js` `dueDateFor`, `aging.js` `overdueAmount`, `latefee.js` `lateFeeAmount`, `reminders.js` `daysLate` / `reminderStage` |
| `modules/finance/src/ledger/statements.rs` (income & expenditure, balance sheet, cash flow, general ledger, account tree, period buckets, comparisons) | `src/accounting/src/lib/reports.js` `profitAndLoss` / `balanceSheet` / `cashFlow` / `generalLedger`, `dates.js` buckets / `previousPeriod` / `yearBefore` / `fiscalYearStart`, `accounts.js` `ACCOUNT_TYPES` / `sortChart` |
| `modules/finance/src/treasury/reconcile.rs` (unreconciled candidates, reconcile summary, carried-forward balance, last reconciliation, changes after reconciliation, statement-line match suggestions) | `src/accounting/src/lib/reconcile.js` `candidates` / `summarize` / `beginningBalance` / `finishRecord` / `lastReconciliation`, `reports.js` `reconDiscrepancy`, `banking.js` `findMatches` |
| `modules/finance/src/charges/billing.rs` (billing period of a day, which students a periodic billing run bills with which plan lines, plan totals) | `src/childcare/src/core/billing.js` `periodFor` / `planBillingRun` / `planTotals` |
| `modules/student/src/admission/funnel.rs` (admissions funnel by stage, joining forecast, win rate by source, stage velocity, idle enquiries, duplicate enquirers) | `src/crm/src/lib/funnel.js` `funnelReport`; `src/crm/src/lib/forecast.js` `resolveStages` / `probabilityOf` / `forecastByMonth` / `stageSummary` / `winRateBy` / `closeReasonBreakdown` / `stageVelocity` / `stalledDeals` / `avgSalesCycle`; `src/crm/src/lib/dupes.js` `contactDuplicates` |
| `modules/campus/src/servicedesk/rules.rs` (service-desk condition language for triggers and queues, trigger actions, tags, `{{placeholders}}`, first-response / next-reply / resolution SLA targets) | `src/help-desk/src/shared/rules.js` `testCondition` / `matches` / `inView` / `applyActions` / `renderPlaceholders` / `slaStatus` / `shortDuration`; `src/help-desk/src/shared/model.js` `normTags`; `src/help-desk/src/server/tickets.js` `runTriggers` |
| `modules/resources/src/inventory/demand.rs`, `lots.rs` (units issued per day over the ledger's reach, days of cover, lead time + safety days, suggested reorder level and order quantity in whole packs, units on order; dated lots first-expired-first-out, expiry status and the expiring list) | `src/inventory/src/lib/demand.js` `demandRates` / `coverDays` / `restockDays` / `suggestedThreshold` / `runningOut` / `rateLabel` / `coverLabel`; `reorder.js` `suggestedQty` / `lineRemaining` / `onOrderQty`; `reports.js` `movementByProduct` / `logReach`; `units.js`; `inventory.js` `stockStatus`; `lots.js` `normalizeLots` / `addLot` / `consumeLots` / `consumeLotAt` / `resizeLots` / `expiryStatus` / `expiringProducts` |

```
MIT License

Copyright (c) 2026 Puter Technologies Inc.

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```
