# Domain E: People (HR, leave, payroll), readiness inventory (Part 1 · Stage 1)

Base: `part1/readiness` = `erp_merge` `fb205860` (CI run 38724169019, every job green).
Scope:
- backend: `modules/people`, covering workforce, positions, faculty, recruitment and onboarding, probation, separation, certificates, profile, leave, attendance, workload, payroll and payslips;
- wiring in `apps/api`;
- web: `apps/web/app/features/people`, plus `routes/admin/people.tsx` and `routes/faculty/leave.tsx`;
- migrations `20261110*_people_*`;
- L2 section E (E01 to E07).

Method: code reading only. Nothing was built, run or fixed.
- **API-tested** means a named test in `apps/api/tests/db_b7*.rs`, run by the `Rust integration` job.
- **Browser-tested** means a step in `apps/web/e2e/people.e2e.mjs`. The `E2E people (preview)` job runs it against the real API.
- **Customer-validated** is pending for every row.

Row data: [`E-people.csv`](E-people.csv) (56 rows). The row count is higher than usual because L2-E01, E03, E04, E05 and E06 each list 10 to 16 named workflows.

## Summary

| Status | Rows |
|---|---|
| implemented | 32 |
| partial | 17 |
| missing | 7 |
| **total** | **56** |

| Release readiness | Rows |
|---|---|
| ready | 3 |
| ready-with-gaps | 27 |
| not-ready | 21 |
| deferred | 5 |

API-tested: 48 rows. Browser-tested: 18 rows.

**What is solid**
- Money, leave days, rates and hours use `rust_decimal`. They are stored as NUMERIC(14,2) for money and NUMERIC(6,2) for days, and sent as strings on the wire (`shared/decimal.rs`, `migrations/20261110000000_people_batch7_foundation.sql:6`). No floats appear in `modules/people`.
- The leave ledger is append-only.
- Payroll results are immutable once finalised. They are revised through revisions and adjustments.
- Payroll posting to Finance is idempotent on `posting_key = finalise` (`payroll/runs.rs:1521-1560`), backed by the unique `journal_entry_source_key`.
- Self-decision of leave is blocked:
  - staff path: `leave/request.rs:1257`;
  - manager path: `leave/request.rs:1555`, which returns 404 unless the caller is the applicant's reporting manager.
- Payslip self-access is restricted to the staff member's own payslips (`payslips/http.rs:253`).

**What blocks release**
- All governed decisions are auto-approved in production, including payroll approval (DEF-E-001).
- Payroll has no separation of duties (DEF-E-002).
- Net pay is never disbursed (DEF-E-007).
- There are no statutory outputs (DEF-E-005).
- About 66 endpoints have no UI (DEF-E-006).

## DEFECTS

### DEF-E-001 · P0 · Every governed People decision is auto-approved in production: payroll run, payroll adjustment, final settlement, leave and more

**Evidence**
- `modules/people/src/lib.rs:89`: `into_state()` hard-codes `ApprovalMode::AutoApprove`.
- `modules/people/src/shared/approval.rs:136`: AutoApprove returns `Approved` at once.
- `apps/api/src/app.rs:859-862`: production mounts `sutra_people::router(people_deps(..))`.
- Auto-apply sites:
  - `payroll/runs.rs:1230`: run submit is approved immediately.
  - `payroll/adjust.rs:349`: adjustments.
  - `payroll/settlement.rs:541`: final settlement.
  - `payroll/config.rs:523`: payroll config.
  - `leave/request.rs:640`: leave requests, including self-service.
  - `leave/balance.rs:1219`, `leave/balance.rs:1719`: leave adjustment and encashment.
  - `leave/policy.rs:861`, `leave/cases.rs:728`.
  - `hr/certificate.rs:324`.
  - Engagement, position, requisition, onboarding, probation and separation via the same helper.
- The e2e only ever sees this path: `people.e2e.mjs:243` "submitted (approved)" and `:425` "validated → approved → finalised".
- The admin "leave requests to review" count (`routes/admin/people.tsx:34`) is therefore always 0.

**Root-cause hypothesis.** Same as DEF-F-001. The Batch 7 default was never made configurable. `SUTRA_WORKFLOW=flowable` still auto-approves, because no `autoApprove:false` is passed (`modules/platform/src/workflow_flowable.rs:10,282-285`).

**Suggested fix**
- Make `ApprovalMode` configurable, with `await` as the production default.
- Pass `autoApprove` explicitly.
- Add the missing decision UIs (see DEF-E-006) before switching.

### DEF-E-002 · P1 · Payroll has no maker-checker: the same administrator prepares, approves, finalises and posts

**Evidence**
- `payroll/runs.rs:1271-1293` (`decide_run`) checks only the status and version, never who submitted or calculated the run.
- `payroll/adjust.rs:377` (`decide_adjustment`) and `payroll/settlement.rs:582` (`decide_settlement`) have no requester check either.
- `people.payroll_run` stores no `submitted_by` (`rg submitted_by modules/people/src/payroll` finds nothing).
- `crates/kernel/src/auth.rs:700-708` grants the single Administrator role `PayrollManage`, `PayrollApprove` and `PeopleApprove` together.

**Root-cause hypothesis.** "HR / payroll office = administrators" (`auth.rs:700`).

**Suggested fix**
- Record the preparer and submitter.
- Refuse the decision when the decider is the same person.
- Split the roles into HR officer, payroll officer and payroll approver.

### DEF-E-003 · P3 · Money precision differs between People and Finance

**Evidence.** People stores money as NUMERIC(14,2) (`migrations/20261110000000_people_batch7_foundation.sql:6`). Finance journals use NUMERIC(18,2) (`migrations/20261030000000_finance_batch5_foundation.sql:6`). Payroll totals that are posted to Finance are therefore capped at the smaller range.

**Suggested fix.** Align on NUMERIC(18,2), or document the cap and validate against it.

### DEF-E-005 · P2 · No statutory compliance outputs

**Evidence**
- Statutory rules exist only as configuration and calculation inputs (`payroll/views.rs:52-131`, `payroll/rows.rs:121`).
- There are no PF, ESI, PT or TDS challans or returns, and no Form 16 (`rg -i 'tds|provident|form.?16' modules/people/src` finds nothing).
- Deductions are credited to `2420 payroll deductions and statutory dues payable` (`modules/finance/src/payroll_postings.rs:55`), and nothing ever settles that account.

**Suggested fix.** Add remittance runs, which also clear account 2420, and statutory report exports.

### DEF-E-006 · P2 · About 66 People endpoints have no web client, so many governed workflows are API-only

**Evidence.** These endpoints are missing from the path tables in `features/people/{workforce,leave,payroll}/api.ts`:
- separation case decision, clearance and complete, and the list;
- probation reviews;
- faculty profile, eligibility, suspend and reactivate;
- certificate decision and issue;
- onboarding decision and cancel;
- engagement modify and transfer;
- position-assignment change;
- staff merge and corrections;
- qualification and document verification;
- leave-adjustment, encashment and leave-case decisions;
- payroll adjustments (`payroll-results/{id}/adjustments`, `payroll-adjustments/*`);
- payslip reissue and withdraw;
- payroll statutory-rules maintenance and config supersede;
- salary-assignment end.

In addition, `payroll/api.ts:18` only offers `"regular" | "retro"` runs, so off-cycle payroll is unreachable from the UI.

**Root-cause hypothesis.** The 7A–7C UI was scoped to the e2e happy path. Decision steps were not needed because they auto-approve (DEF-E-001).

**Suggested fix.** Build the missing admin screens, and decision inboxes in particular. They are mandatory before DEF-E-001 is fixed.

### DEF-E-007 · P1 · Net pay is never disbursed; salaries payable is never cleared

**Evidence**
- `modules/finance/src/payroll_postings.rs:5-14` posts Dr 5410 / Cr 2410 (net pay) / Cr 2420. There is no payment run, no bank advice or NEFT file, and no Dr 2410 / Cr bank posting anywhere (`rg -i "disburs|bank file|salary payment" modules` finds nothing).
- L2-E06 Workflow 7 stops at the posting. Treasury (L2-F10) has no payroll payout either.

**Suggested fix.** Add a payroll disbursement workflow: bank file, Finance payment posting, and reconciliation with the bank statement.

### DEF-E-008 · P3 · Primary-key-only UPDATEs without `tenant_id` (defence in depth)

**Evidence.** Several writes run after a tenant-scoped locked load but match on the primary key alone:
- `hr/onboarding.rs:195,220,541,600,813`;
- `hr/certificate.rs:187,563`;
- `hr/separation.rs:175,580,695`;
- `payroll/runs.rs:273,1558`;
- `payroll/settlement.rs:256,506,731,764`;
- `workforce/staff.rs:854,892`;
- `leave/balance.rs:1029,1319,1533`.

No cross-tenant path was found, but row-level security is absent (SEC-9D-004).

**Suggested fix.** Add the tenant predicate or row-level security.

## TEST GAPS

- No test runs the production approval mode. The `governed_decisions_await_a_decision` (`db_b7a.rs:1933`), `await_decision_routes_leave_to_the_reporting_manager` (`db_b7b.rs:1400`) and `await_decision_keeps_payroll_steps_pending` (`db_b7c.rs:1121`) tests build `AwaitDecision` state by hand (DEF-E-001).
- There are no negative tests for payroll self-approval, because the guard does not exist (DEF-E-002).
- The browser never exercises:
  - separation, probation, certificates (admin side), onboarding decision;
  - engagement modify and transfer, faculty profiles;
  - leave adjustment, encashment, cases and exceptions, accrual and year-end;
  - payroll adjustments, retro and off-cycle runs, final settlement;
  - payslip reissue and withdraw.
- `db_b7c.rs::payroll_lifecycle_over_http` covers the main payroll cases, but there is no property or regression suite for the payroll engine. Only unit tests exist in `payroll/domain.rs`, covering proration rounding, LOP across month boundaries and the slab edges of statutory rules.
- There are no concurrency tests for:
  - two leave requests racing on the same balance;
  - parallel payroll calculate and finalise;
  - double payslip issue. The unique `payslip_one_live` index protects this, but no test covers it.

## DOC DISCREPANCIES

- `docs/batches.md:112-114` marks 7A, 7B and 7C ✅ for "approval", "request → decision (reference approval)" and "calculate → approve → finalise". In every deployed configuration these approvals are automatic (DEF-E-001), and most decision steps have no UI (DEF-E-006).
- `docs/batches.md:105-107` defers 32 L3-E scenarios to "7D". The L2 workflows found missing here (E01 deputation, E03 availability and archival, E04 HR case, promotion and compliance, E07 contact, address, emergency contact, skills, languages and certifications) are not listed by name. The E-0xx to L2 mapping should be stated so these rows can be marked `deferred` rather than `missing`.
- `modules/people/src/shared/approval.rs:15-19` says AwaitDecision arrives with Batch 9 and Flowable. Batch 9 shipped Flowable without changing the module's approval mode.
- L2-E06 lists finance posting as the last payroll step. No document records where salary disbursement and statutory remittance live (DEF-E-005, DEF-E-007).
