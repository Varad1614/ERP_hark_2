# Domain J — Resources (procurement, inventory, assets, AP): Part 1 Stage 1 readiness inventory

Scope: `modules/resources` (`sutra-resources`, schema `resources`, `/api/v1/resources`), web
`apps/web/app/features/campus/resources` (Admin → Campus tabs Procurement / Inventory / Assets), migrations
`20261115000002`, `…070`, `…071`, L2-J00 … L2-J10. **Library** was requested under J but is owned by Domain G
(`modules/campus/src/library`, L2-G05); it gets one cross-reference row (J-027) with a light review only.
Base: `part1/readiness` = `erp_merge` fb205860, CI run 38724169019 (all jobs green). "API-tested passed" = a named
test in `apps/api/tests/db_b8d.rs` / `db_b8_foundation.rs` / `db_b8_integration.rs` (/ `db_b8c.rs` for library)
passed in that run; "browser-tested passed" = a step of `apps/web/e2e/campus.e2e.mjs` (job "E2E campus (preview)",
green) drives it on screen. Customer validation is `pending` everywhere. Row data: `J-resources.csv`.

## Summary

| Status | Rows |
|---|---|
| implemented | 18 |
| partial | 1 |
| mock-only | 0 |
| missing | 1 (J-001 procurement planning) |
| deferred | 8 (supplier qualification/risk, emergency PR, receipt correction, expiry/valuation, transfers, asset transfer/missing/repair, **J09 physical verification**, **J10 disposal & write-off**) |
| **total** | **28** |

| Release readiness | Rows |
|---|---|
| ready | 3 (J-012 stock ledger, J-017 replenishment, J-028 metrics) |
| ready-with-gaps | 9 |
| not-ready | 8 |
| deferred | 8 |

API tested: 19 rows. Browser tested: 6 rows, and only partly on screen. The campus e2e creates supplier,
requisition, sourcing, PO, receipt, invoice and asset **through the API** and uses the screens only to accept the
receipt, match/post the invoice and view stock and the asset register (`apps/web/e2e/campus.e2e.mjs:321-391`).
18 of 70 route paths have no caller in `features/campus/resources/api.ts`.

Headline: procure-to-pay works end to end, but **no financial control holds**: every approval is automatic
(DEF-J-001), there is no maker-checker (DEF-J-002), vendor payments need no approval (DEF-J-003), an approved
supplier's bank account can be changed silently (DEF-J-004), and the ₹1,00,000 direct-purchase limit can be
bypassed (DEF-J-005). L2-J09 and L2-J10 are entirely deferred. Procurement is **not release-ready**.

## DEFECTS

### DEF-J-001 — P0 — Every governed resource decision is auto-approved in production
- Evidence: `modules/resources/src/lib.rs:76` hardcodes `approvals: ApprovalMode::AutoApprove` (production router
  `apps/api/src/app.rs:881-884`); `shared/approval.rs` resolves every request to `Approved`. Submit then applies the
  decision as the system actor in the same transaction: requisition `requisitions/mod.rs:719-770` (`approve(... Actor::system ...)`
  at :769), purchase order `orders/mod.rs:816`, vendor invoice `payables/mod.rs:759`; same pattern for supplier,
  award, stock issue/adjustment and asset retirement. The Flowable adapter defaults `autoApprove=true`
  (`modules/platform/src/workflow_flowable.rs:10`) and no resources call passes `false`. The campus e2e shows it:
  supplier and requisition are submitted and immediately usable with no decision step (`campus.e2e.mjs:337-347`).
- Impact: requisitions, awards, purchase orders, stock write-offs (adjustments), invoice approval for AP posting and
  asset retirement are approved by `system:auto-approval` the instant they are submitted.
- Fix: derive `ApprovalMode` from config (AwaitDecision with Flowable; refuse AutoApprove in production), pass
  `autoApprove=false`, add value-based approval routing.

### DEF-J-002 — P1 — No segregation of duties: requester / creator can approve their own documents
- Evidence: decision handlers check only `Permission::ResourcesApprove` and the status: `requisitions/mod.rs:810-831`,
  `orders/mod.rs:831-838`, `payables/mod.rs:772-779`, `suppliers/mod.rs:824-831`, `sourcing/mod.rs:942-949`,
  `inventory/stock.rs:520` and `:1396`, `assets/mod.rs:935-942`. `requester_subject` / `created_by` are stored
  (`requisitions/mod.rs:139`, `:445`) but never compared with the decider. `Administrator` holds both
  `ProcurementManage` and `ResourcesApprove` (`crates/kernel/src/auth.rs:722-725`); there is no separate approver role.
- Impact: even after DEF-J-001 is fixed, one person can raise, approve, order, receive, match, approve the invoice
  and pay (L2-J10 ADR-J10-008 and GD-J01-11 require policy-driven SoD).
- Fix: reject decisions where the decider equals the requester/creator/submitter (and, for invoices, the receiver);
  introduce procurement-approver roles and amount thresholds.

### DEF-J-003 — P1 — Vendor payments are created and posted without any approval
- Evidence: `create_payment` (`payables/mod.rs:1077-1225`) and `post_payment` (`payables/mod.rs:1271-1400`) require
  only `ProcurementManage` and never call `request_approval`; `VENDOR_PAYMENT` is not among the process keys
  (`shared/approval.rs:30-50`). Posting books Dr 2310 / Cr bank through `ProcurementPostingPort`.
- Impact: cash leaves (in the books) on one user's action.
- Fix: add `VENDOR_PAYMENT_APPROVAL` (draft → pending → approved → posted) with maker-checker.

### DEF-J-004 — P1 — An approved supplier's bank details can be changed without re-approval or field audit
- Evidence: `suppliers/mod.rs:615-660` (`PATCH /suppliers/{id}`, `ProcurementManage`) allows any status except
  merged/inactive and calls `apply_profile`, which overwrites `bank_account_name/number/ifsc`
  (`suppliers/mod.rs:318-320`); the audit entry records only the supplier number (`suppliers/mod.rs:652-660`), not the
  changed fields. Status stays `active`.
- Impact: classic payment-diversion fraud vector: change the bank account, then pay a posted invoice (DEF-J-003).
- Fix: bank-detail changes on a non-prospective supplier create a pending change that needs `SUPPLIER_APPROVAL`
  (by someone else); audit old/new masked values; notify.

### DEF-J-005 — P1 — Direct-purchase limit is checked on the requester's estimate, not the ordered value
- Evidence: direct sourcing checks `r.estimated_total > direct_limit()` only (`sourcing/mod.rs:488`); the direct PO
  path checks the same estimate (`orders/mod.rs:551-558`) and then takes **caller-supplied unit prices** without any cap
  (`orders/mod.rs:559-571`). The quotation in a direct event is likewise unbounded.
- Impact: under-estimate a requisition (≤ ₹1,00,000), then order at any price without an RFQ; with DEF-J-001 the PO
  is auto-approved. No check that a PO total stays within the approved requisition / award value.
- Fix: enforce the limit on the quotation/PO total; require re-approval when the PO total exceeds the approved
  requisition estimate by a policy tolerance.

### DEF-J-006 — P2 — Three-way match is not concurrency-safe
- Evidence: `match_invoice` computes `claimed` quantities of other live invoices on a plain pool connection with no
  row lock (`payables/mod.rs:670-690`) before the status update transaction; two invoices for the same PO line matched
  concurrently can each pass `quantity + invoiced + claimed ≤ received` (`payables/mod.rs:596`).
- Fix: lock the purchase-order lines (`FOR UPDATE`) inside the match transaction.

### DEF-J-007 — P2 — Price tolerance and direct-sourcing limit are code constants
- Evidence: `PRICE_TOLERANCE_PERCENT = 1` (`payables/mod.rs:59`), `DIRECT_SOURCING_LIMIT = "100000.00"`
  (`sourcing/mod.rs:52`). L2-J01 GD-J01-04 requires institution-specific procurement rules (public vs private
  institutions, GFR thresholds).
- Fix: move to an institution procurement policy (versioned, governed).

### DEF-J-008 — P2 — Procurement UI gaps
- Evidence: no caller in `features/campus/resources/api.ts` for `requisitions/{id}/amend|withdraw`,
  `purchase-orders/{id}/amend`, `goods-receipts/{id}/inspect`, `sourcing-events/{id}/decision|cancel`,
  `suppliers/{id}/merge`, `stock-reservations/{id}/fulfil`, `stock-issues/{id}/cancel`, `stores/{id}/locations`,
  `institutions/{id}/stock-movements`, detail GETs. Award decision has no screen, so under AwaitDecision an award
  cannot be approved from the UI.
- Fix: add the actions; add an approvals inbox before enabling AwaitDecision.

### DEF-J-009 — P3 — By-id statements without a tenant predicate; no RLS
- Evidence: `orders/mod.rs:665,1127`; `payables/mod.rs:635,807`; `requisitions/mod.rs:387,413`; `suppliers/mod.rs:442,533`;
  `sourcing/mod.rs:728,1043`; `assets/mod.rs:360,819`; `inventory/items.rs:400`; `inventory/stock.rs:277,878`. All follow
  a tenant-checked load in the same transaction (not exploitable today); no RLS in the resources migrations.
- Fix: add `AND tenant_id = $n` for defence in depth.

### DEF-J-010 — P2 — No budget / commitment check anywhere in procure-to-pay
- Evidence: requisition submit and PO issue carry no budget head or commitment (`requisitions/mod.rs:684-800`,
  `orders/mod.rs:742-826`); Finance budget commitment is deferred (`docs/batch8-contracts.md:62`, F-083).
- Impact: spending is unconstrained by budget; documented deferral, but a release blocker for most institutions.
- Fix: add commitment on PO approval / release on invoice through a Finance port.

## TEST GAPS

- **Self-approval**: no test attempts a decision by the requester (`db_b8d::permissions_and_preconditions` only checks
  student → 403, 404, 412/428, 422).
- **Supplier bank change**: no test of PATCH on an active supplier.
- **Direct-limit bypass**: `db_b8d::sourcing_variants_and_cancellations` tests the estimate limit, not a high quotation /
  PO price on a low estimate.
- **Concurrent invoice matching**: none.
- **Decisions**: `vendor-invoices/{id}/decision` and `sourcing-events/{id}/decision` not found in the DB tests (only the
  auto-approved path runs); `goods-receipts/{id}/reject`, `items/{id}/deactivate` and the institution receipt list also
  not found.
- **Browser**: no e2e step creates a supplier, requisition, sourcing event, PO, stock issue, reservation, adjustment or
  payment through the screens; Faculty has no procurement self-service (requisitions are admin-only).
- No resources e2e job of its own; coverage rides on `campus.e2e.mjs`.

## DOC DISCREPANCIES

- `sutraerp/README.md:43` labels `resources/ (J)` "← scaffolds" although Batch 8D is implemented (README §8D at
  `README.md:446-449` describes it).
- `docs/batch8-contracts.md` §1 (8D) and the requisition/PO handler doc-comments describe decisions as
  "step-up" governed approvals; in production they are auto-approved (DEF-J-001). `docs/security/findings.md` lists
  neither the auto-approval nor the missing maker-checker / payment approval.
- `docs/batch9-contracts.md:263-276` implies Flowable human tasks for approval keys; resources never requests them.
- L2-J00 BR-J00-014 ("Disposal Requires Authority") and AC-J00 criteria on physical verification are unmet — J09/J10 are
  deferred to an "8E later" (`docs/batch8-contracts.md:61`) that is not scheduled in `docs/batches.md`.
- The user-facing scope "J Resources: library, procurement, inventory, assets" differs from the architecture:
  library is L2-G05 / `modules/campus`; the G inventory should own its assessment.

## Library (cross-reference, light review)
`modules/campus/src/library` (6.5k lines) is covered by `db_b8c::library_policy_membership_catalogue_circulation_fines_and_clearance`
and `library_await_decision_policy_fine_waiver_and_clearance`, plus campus e2e library steps (admin catalogue/issue,
student loans/reserve). Fines are decimal, capped and rounded once (`modules/campus/src/library/domain.rs:253-260`).
Its approvals (`LIBRARY_POLICY_APPROVAL`, `FINE_WAIVER`, `LIBRARY_CLEARANCE`) are subject to the same production
auto-approval as DEF-J-001 (`modules/campus/src/lib.rs:89`). Not audited further here.
