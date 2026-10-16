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
