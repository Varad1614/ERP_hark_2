# Payroll disbursement (DEF-E-007)

Payroll calculates money; Finance moves and accounts money (L2-E06 §4.2). Until Part 1 the
payroll workflow stopped at the liability posting (Dr 5410 / Cr 2410 net pay / Cr 2420 deductions
+ employer contributions): net pay was never paid out and the payables were never cleared. The
disbursement run closes that gap.

## Flow

1. **Run posted.** Only a payroll run whose liability is posted to Finance (`posted` or `closed`,
   journal entry present) can be disbursed.
2. **Bank accounts.** Every payee needs a salary bank account
   (`PUT /api/v1/people/staff/{id}/bank-account`, `people.payroll.manage`; account number 6–18
   digits, 11-character IFSC). The API only ever returns the masked number (`XXXXXXXX7890`).
   Missing accounts block the net-pay batch with 422 `bank_account_missing` (listing the staff
   numbers).
3. **Batch.** `POST /api/v1/people/payroll-runs/{id}/disbursements` with
   `{"kind": "net_pay" | "statutory_dues"}` (`people.payroll.manage`, `Idempotency-Key`).
   Idempotent: one live batch per run and kind — 201 the first time, 200 with the same batch
   afterwards. Lines come from the run's **finalised** results only (superseded revisions are
   excluded, so applied adjustments count once):
   * `net_pay` — one line per result with net pay > 0, with a snapshot of the payee's bank account;
   * `statutory_dues` — one line per deduction / employer-contribution component (PF, PF_ER, PT …).
4. **Approval.** The batch goes through the shared approval helper
   (`people.payroll_disbursement_approval`, BPMN in `deploy/flowable/processes`). Pending batches
   are decided with `POST /api/v1/people/payroll-disbursements/{id}/decision`
   (`people.payroll.approve`, step-up, `If-Match`); a rejected batch (reason required) frees the run
   for a new batch.
5. **File.** `GET /api/v1/people/payroll-disbursements/{id}/file` (`people.payroll.manage`,
   approved or paid batches) returns the CSV below and stamps `fileGeneratedAt`.
6. **Bank submission — BLOCKED_EXTERNAL.** No bank API is integrated. The file is uploaded to the
   bank's corporate portal by hand; every batch reports `bankSubmission: "BLOCKED_EXTERNAL"`.
7. **Mark paid.** `POST /api/v1/people/payroll-disbursements/{id}/mark-paid`
   `{"reference": "<UTR or challan ref>", "paidOn": "YYYY-MM-DD"?}` (`people.payroll.approve`,
   step-up, `If-Match`). Rules: the batch is `approved`; a net-pay batch's file has been generated;
   the reference is 1–64 of `A-Z a-z 0-9 - / _ .`; `paidOn` defaults to today in the institution
   timezone and is never in the future. Finance records the payout through the cross-module
   contract `PayrollPostingPort::post_payroll_payout` (`sutra_contracts::finance::ports`):

   | Batch | Dr | Cr |
   |-------|----|----|
   | net pay | 2410 salaries payable | bank (`bank_main`) |
   | statutory dues | 2420 payroll deductions and statutory dues payable | bank (`bank_main`) |

   `source_type = payroll_disbursement`, `source_id` = the batch, `posting_key = paid` (idempotent,
   period-guarded). The batch becomes `paid` with the reference, date, journal entry and actor. A
   second mark-paid is a 409 and posts nothing (also under concurrency: the journal is unique per
   batch and the batch row is locked).

## Generic NEFT bulk file (net pay)

CSV, CRLF line endings, UTF-8 (ASCII content), no quoting: every field is restricted to
bank-safe characters (letters, digits, space, `. & - /`; anything else becomes a space and leading
non-alphanumerics are dropped, so no CSV formula can reach the bank portal). Amounts carry exactly
two decimals; dates are `DD-MM-YYYY`.

```text
H,<batch number>,<payroll run number>,<value date>,<record count>,<total amount>,INR
D,NEFT,<beneficiary account no>,<beneficiary IFSC>,<beneficiary name ≤50>,<amount>,<value date>,<customer reference>,<narration ≤30>
T,<record count>,<total amount>
```

* customer reference = `<batch number>-<line no>` (unique per transfer; matches the bank's return
  file for reconciliation);
* narration = `SALARY <YYYY-MM> <staff number>`;
* value date = the payment date once paid, otherwise the generation date.

Most Indian banks' bulk-upload templates map 1:1 onto the `D` record; a bank-specific template is a
column mapping of this file.

## Statutory remittance advice

Same envelope; `D,<component code>,<component name ≤50>,<amount>` per component, used to prepare
the challans (EPF ECR, professional tax, TDS …). The challan reference is entered at mark-paid.

## Not covered (follow-ups)

* Bank API submission and automatic bank-statement reconciliation (BLOCKED_EXTERNAL).
* An adjustment applied after a batch is created is not added to it; pay the difference with a
  new run (off-cycle) — the batch total is frozen at creation.
* Final-settlement (FNF) payouts were not exercised by the tests; a settlement run is disbursable
  through the same endpoint only when its run is `posted` / `closed` with a journal entry.
