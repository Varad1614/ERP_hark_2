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
| `modules/finance/src/printing/escpos.rs` (code page 437 encoding and transliteration, printed column width, word wrap, ESC/POS builder, receipt and day-close report slips, cash-drawer pulse) | `src/pos/src/lib/escpos.js` `encodeCp437` / `printWidth` / `wrap` / `EscposBuilder` / `escposReceipt` / `escposReport` / `drawerKickBytes` (`₹` prints as `Rs.` instead of `INR`) |
| `modules/finance/src/gst/rules.rs` (document totals with named taxes per line, tax-inclusive carve-out, prorated discount, paisa rounding; zero-tax notes and their presets; document numbering; Indianised as CGST + SGST / IGST by place of supply, GST slabs, April–March series, GSTR-1 summary) | `src/invoicing/src/lib/money.js` `lineTotal` / `docTotals`; `taxnote.js` `taxNoteValue` / `taxNoteText` / `chargesUntaxed` / presets; `doc.js` `formatDocNumber` |
| `modules/finance/src/charges/billing.rs` (billing period of a day, which students a periodic billing run bills with which plan lines, plan totals) | `src/childcare/src/core/billing.js` `periodFor` / `planBillingRun` / `planTotals` |
| `modules/student/src/admission/funnel.rs` (admissions funnel by stage, joining forecast, win rate by source, stage velocity, idle enquiries, duplicate enquirers) | `src/crm/src/lib/funnel.js` `funnelReport`; `src/crm/src/lib/forecast.js` `resolveStages` / `probabilityOf` / `forecastByMonth` / `stageSummary` / `winRateBy` / `closeReasonBreakdown` / `stageVelocity` / `stalledDeals` / `avgSalesCycle`; `src/crm/src/lib/dupes.js` `contactDuplicates` |
| `modules/campus/src/servicedesk/rules.rs` (service-desk condition language for triggers and queues, trigger actions, tags, `{{placeholders}}`, first-response / next-reply / resolution SLA targets) | `src/help-desk/src/shared/rules.js` `testCondition` / `matches` / `inView` / `applyActions` / `renderPlaceholders` / `slaStatus` / `shortDuration`; `src/help-desk/src/shared/model.js` `normTags`; `src/help-desk/src/server/tickets.js` `runTriggers` |
| `modules/resources/src/inventory/demand.rs`, `lots.rs` (units issued per day over the ledger's reach, days of cover, lead time + safety days, suggested reorder level and order quantity in whole packs, units on order; dated lots first-expired-first-out, expiry status and the expiring list) | `src/inventory/src/lib/demand.js` `demandRates` / `coverDays` / `restockDays` / `suggestedThreshold` / `runningOut` / `rateLabel` / `coverLabel`; `reorder.js` `suggestedQty` / `lineRemaining` / `onOrderQty`; `reports.js` `movementByProduct` / `logReach`; `units.js`; `inventory.js` `stockStatus`; `lots.js` `normalizeLots` / `addLot` / `consumeLots` / `consumeLotAt` / `resizeLots` / `expiryStatus` / `expiringProducts` |
| `modules/campus/src/transport/maintenance.rs` (service reminders by time / meter / whichever first with on-time grading, renewals with anchored repeats, fuel economy between full fills, fuel totals, operating cost lines, per-vehicle costs, meter usage, calendar intervals) | `src/fleet/src/core/reminders.js` `reminderNextDue` / `reminderState` / `completeReminder` / `compliance` / `describeSchedule` / `renewalState` / `completeRenewal`; `fuel.js` `fuelTotal` / `unitPrice` / `fuelDerived` / `fuelSummary`; `costs.js` `costsByVehicle` / `usageByVehicle`; `format.js` `economyValue`; `money.js` `roundCents`; `dates.js` `addDays` / `addMonths` / `diffDays` / `addInterval` |
| `modules/people/src/roster/rules.rs` (shift validation, overnight conflicts, weekly availability and time off, weekly hours and limits, move checks, ranked suggestions, coverage gaps, minimum rest / daily hours / consecutive days, bulk fill of open shifts, time helpers) | `src/shifts/src/core/scheduling.js` `validateShift` / `findConflicts` / `availabilityStatus` / `weeklyMinutes` / `hoursStatus` / `evaluateMove` / `suggestAssignees` / `coverageGaps`; `rules.js` `normalizeRules` / `ruleViolations` / `scheduleRuleViolations`; `fill.js` `planFill`; `shift-time.js`; `lib/time.js` `toMinutes` / `durationMinutes` / `startOfWeek` / `formatDate` / `formatHours` |
| `modules/people/src/roster/swaps.rs`, `clock.rs`, `timesheet.rs`, `reports.rs`, `ics.rs` (swap / give-away / cover / claim lifecycle and roster moves, clock punches with lateness and worked time across midnight, timesheet rows and summary with the formula-safe CSV, punch reconciliation with daily attendance, overtime and labour cost for the payroll export, roster reports with prior-period trends, staff iCalendar with stable UIDs and 74-octet folding) | `src/shifts/src/core/swaps.js`; `attendance.js`; `timesheet.js`; `payroll.js`; `reports.js`; `ics.js` |
| `modules/people/src/timetrack/logic.rs` (duration parsing and formatting, billing rounding and minimums, calendar weeks / months, clock times on an entry, money parsing, CSV parse / escape with the formula guard, Toggl / Clockify / Harvest import mapping, import dates / durations / rows and duplicate signature, effective-dated rates and the rate cascade, entry amounts, totals, grouping, overlap detection, timer elapsed / stop, duration patch, budget progress) | `src/time-tracking/src/lib/time.js` `parseDuration` / `fmtClock` / `fmtTimer` / `fmtDuration` / `toHours` / `roundSeconds` / `addDays` / `daysApart` / `weekStartOf` / `weekDays` / `monthDays` / `shiftMonth` / `monthKeysBetween` / `msAt` / `timeValueOf` / `timePartOf`; `util.js` `round2` / `parseAmount` / `textMatches`; `csv.js` `parseCSV` / `csvField` / `toCSVLine`; `importer.js` `IMPORT_TARGETS` / `detectMapping` / `parseImportDate` / `parseImportDuration` / `rowsToItems`; `store.js` `rateAt` / `withRateFrom` / `resolveRate` / `resolveRounding` / `resolveMinimumMinutes` / `billedSeconds` / `entryAmount` / `totals` / `groupEntries` / `overlappingEntries` / `filterEntries` / `elapsedSeconds` / `stopTimer` / `durationPatch` / `budgetProgress` / `importEntries` (signature) |
| `modules/people/src/hr/scorecards.rs` (rating rubric, scorecard validation, averages, panel summary and leaning, interview guides and the guide picked for an interview, screening questions and answers, must-have mismatches, interviews owed feedback) | `src/hiring/src/core/scorecards.js` `normalizeRatingFields` / `validateScorecard` / `averageRating` / `summarizeScorecards`; `guides.js` `normalizeGuides` / `defaultGuideFor` / `normalizeGuideAnswers`; `screening.js` `normalizeScreeningQuestions` / `normalizeAnswers` / `screeningMismatches`; `reminders.js` `missingScorecards` |
| `modules/people/src/hr/offers.rs` (offer status machine, offer validation, expiring offers, letter templates with optional lines, onboarding checklist templates and the template picked for a hire, due offsets from the joining date, due / overdue onboarding tasks, checklist progress and grouping, follow-ups due / overdue and their order) | `src/hiring/src/core/offers.js` `canTransition` / `isOpenOffer` / `validateOffer` / `expiringOffers` / `renderOfferLetter`; `emails.js` `renderTemplate`; `onboard.js` `normalizeOnboardingTemplates` / `templateForJob` / `buildOnboardingPlan` / `taskDueDate` / `offsetLabel` / `offsetShort` / `hireStartDate` / `dueOnboardingTasks` / `onboardingProgress` / `groupTasks`; `followups.js` `validateFollowup` / `followupDue` / `dueFollowups` / `sortFollowups` |
| `modules/people/src/hr/hiring_analytics.rs` (stage visits, reached stages, stage conversion, time to hire, average days in stage, source breakdown, offer outcomes, configurable stages, funnel counts, days in stage, stale candidates, retention due, offer / follow-up / onboarding reminders) | `src/hiring/src/core/analytics.js` `stageVisits` / `reachedStages` / `stageConversion` / `timeToHire` / `averageDaysInStage` / `sourceBreakdown` / `offerOutcomes` / `median`; `pipeline.js` `stagesFrom` / `funnelCounts` / `daysInStage` / `staleCandidates`; `retention.js` `archivedAt` / `retentionDue`; `reminders.js` `computeReminders` (offers, follow-ups, onboarding) |
| `modules/people/src/hr/careers.rs` (public careers page: escaping, description paragraphs and bullets, slug from the institution name) | `src/hiring/src/core/jobpage.js` `escapeHtml` / `descriptionHtml` / `subdomainBase` |
| `modules/resources/src/contracts/rules.rs`, `contracts/ical.rs` (term ends and auto-renewal roll-forward, agreement end, finished terms, renewal phase and notice deadline, recorded decisions and when they stop applying, term reconciliation, repeating obligations anchored to their first date, obligation lists, dashboard numbers per currency, iCalendar output with 75-octet folding and stable UIDs) | `src/contracts/src/lib/dates.js` `isTermEnd` / `nextTermEnd` / `agreementEnd` / `completedTerms` / `renewalInfo` / `needsAttention` / `decisionFor` / `decisionHandles`; `store.js` `termReconciliation` / `endedOn` / `occurrenceDue` / `repeatAnchor` / `withinSeries` / `advanceOccurrence` / `reopenedSeries` / `rewindOccurrence` / `allObligations` / `dueObligations` / `annualizedValue` / `contractStats` / `upcomingRenewals`; `ical.js` `escapeText` / `foldLine` / `buildCalendar` / `contractEvents` |
| `modules/resources/src/expenses/rules.rs` (amount parsing, totals per currency, text normalising and tokens, two-place rounding, loose dates, currency resolution, receipt-reading JSON extraction, category matching and normalising a reading, claim lifecycle) | `src/expense/src/lib/money.js` `parseAmount` / `totalsByCurrency`; `util.js` `normalizeText` / `tokens` / `round2`; `dates.js` `parseLooseDate`; `currencies.js` `resolveCurrency`; `ocr.js` `extractJson` / `matchCategory` / `normalizeOcrResult`; `reports.js` lifecycle |

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

## PaddleOCR (Apache-2.0)

Receipt reading for staff expense claims (`tools/ocr-sidecar`) uses
[PaddleOCR](https://github.com/PaddlePaddle/PaddleOCR) 3.x and its PP-OCRv6 models, with
[PaddlePaddle](https://github.com/PaddlePaddle/Paddle), both Copyright PaddlePaddle Authors,
licensed under the Apache License, Version 2.0
(<https://www.apache.org/licenses/LICENSE-2.0>). PaddleOCR runs as a **separate sidecar service**
called over HTTP; it is not linked into or distributed with the SutraERP binaries, and SutraERP's
own code is not derived from it. The sidecar image installs the unmodified packages from PyPI
(`tools/ocr-sidecar/requirements.txt`), which carry their own license and NOTICE files.
