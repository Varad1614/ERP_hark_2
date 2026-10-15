# Part 1 execution tracker

One row per work item. Seeded at Stage 1 with every open P0/P1 from [`DEFECT_REGISTER.md`](DEFECT_REGISTER.md). Update the row
(status, files changed, tests executed, result, commit) when an item is worked; add rows for P2/P3 items when they are picked up.
Status: open | in progress | done | blocked | won't fix.

| ID | Issue | Root cause | Files changed | Status | Tests executed | Result | Blockers | Commit |
|---|---|---|---|---|---|---|---|---|
| S1 | Stage 1: merge domain inventories (s1-ab, s1-cd, s1-ef, s1-gh, s1-ij) + aggregate inventory, defect register, gaps, test matrix, tracker; correct stale docs | — | `sutraerp/docs/readiness/**`, `sutraerp/README.md`, `sutraerp/docs/batches.md`, `sutraerp/docs/security/findings.md` | done | none (docs only; CI-first) | merged no-ff at `f6ffff7` | — | `f6ffff7` (merges); aggregate + doc commits on `part1/readiness` |
| DEF-X-001 (P0) | Governed financial / exam / payroll / admission (and every other) decisions are approved without any human, in every environment | Batch 2-8 'AutoApprove until Flowable' placeholder never switched when Flowable shipped in 9C; module ApprovalMode is not derived from config; Noop stays the default and production accepts it | — | open | — | — | needs decision UIs (B-006, C-007, E-006, F-008, G-006, I-016, J-008) first | — |
| DEF-E-001 (P0) | Every governed People decision is auto-approved in production: payroll run, adjustment, final settlement, leave and more | Batch 7 default never configurable | — | open | — | — | closes with DEF-X-001 | — |
| DEF-F-001 (P0) | Every governed finance decision is auto-approved in production, incl. student self-service refunds | Batch 5 default; Flowable defaults autoApprove=true | — | open | — | — | closes with DEF-X-001 | — |
| DEF-I-001 (P0) | Every governed examination decision (result approval/publication, marks, malpractice, revaluation) is auto-approved in production | Batch 6 default; Flowable left untouched | — | open | — | — | closes with DEF-X-001 | — |
| DEF-J-001 (P0) | Every governed resource decision (requisition, PO, invoice, stock write-off, asset retirement) is auto-approved in production | Batch 8 default | — | open | — | — | closes with DEF-X-001 | — |
| DEF-X-002 (P1) | No separation of duties: requesters can decide their own governed requests; one Administrator role holds every manage + approve permission | Fixed three-role model (L2-A04 role admin, SoD, delegation never built); maker-checker implemented only in a few places (finance journals/treasury, some academic) and only on the AwaitDecision path | — | open | — | — |  | — |
| DEF-A-001 (P1) | Approval engine defaults to the auto-completing no-op adapter in every environment, production included | Batch 2-8 shortcut kept through Batch 9; Flowable selectable, not mandatory | — | open | — | — | closes with DEF-X-001 | — |
| DEF-B-001 (P1) | Every Domain B approval is auto-approved; decision endpoints unreachable in a deployed system | Batch 2 shortcut never revisited | — | open | — | — | closes with DEF-X-001 | — |
| DEF-B-002 (P1) | submit (institution.manage, no step-up) activates the institution, bypassing the Activation Authority | Consequence of auto-approval | — | open | — | — | consequence of DEF-X-001 | — |
| DEF-C-001 (P1) | Every Domain C approval (21 process keys incl. admission) is auto-approved in production, Flowable included | Batch 3 placeholder | — | open | — | — | closes with DEF-X-001 | — |
| DEF-D-001 (P1) | Every Domain D approval is auto-approved in production, Flowable included | Same as C | — | open | — | — | closes with DEF-X-001 | — |
| DEF-D-002 (P1) | Faculty can change finalised attendance alone; corrections apply instantly as 'system' (step-up bypassed) | Auto-approval + faculty path skips AttendanceCorrect | — | open | — | — | consequence of DEF-X-001; also SoD (DEF-X-002) | — |
| DEF-E-002 (P1) | Payroll has no maker-checker: same administrator prepares, approves, finalises and posts | HR/payroll = administrators | — | open | — | — | closes with DEF-X-002 | — |
| DEF-E-007 (P1) | Net pay is never disbursed; salaries payable (2410) never cleared | Workflow stops at posting | — | open | — | — |  | — |
| DEF-F-002 (P1) | Mock payment gateway can be enabled in production; mock captures settle fees without money | Mock kept selectable 'for pilots' | — | open | — | — |  | — |
| DEF-F-003 (P1) | No separation of duties: requesters approve own refunds, adjustments, write-offs; one role holds every finance permission | Finance office = administrators | — | open | — | — | closes with DEF-X-002 | — |
| DEF-G-001 (P1) | Approvals hard-wired to auto-approve in production, even with Flowable | Batch 8 default | — | open | — | — | closes with DEF-X-001 | — |
| DEF-G-002 (P1) | A librarian can waive any library fine alone | Auto-approval + no threshold/second approver | — | open | — | — | consequence of DEF-X-001/002 | — |
| DEF-H-001 (P1) | Dashboard, regulatory-package and accreditation approvals auto-complete in production | Systemic | — | open | — | — | closes with DEF-X-001 | — |
| DEF-I-002 (P1) | No segregation of duties / self-approval check on any exam decision | Not built | — | open | — | — | closes with DEF-X-002 | — |
| DEF-I-003 (P1) | Releasing a withheld result after batch approval publishes an un-approved recomputation | Release path walks to approved | — | open | — | — |  | — |
| DEF-I-004 (P1) | Pending malpractice cases do not withhold the candidate's result | Only decided cases counted | — | open | — | — |  | — |
| DEF-I-005 (P1) | CGPA / completion use only the latest-published result per academic term (supplementary breaks CGPA) | DISTINCT ON term | — | open | — | — |  | — |
| DEF-I-006 (P1) | Faculty can read every unpublished result and every malpractice case in the tenant | ExamRead without scoping | — | open | — | — |  | — |
| DEF-I-007 (P1) | Exam conduct, invigilation, seating publication and exceptions have no UI (marks entry cannot start) | UI not built | — | open | — | — |  | — |
| DEF-J-002 (P1) | No segregation of duties: requester/creator can approve own procurement documents | Not built | — | open | — | — | closes with DEF-X-002 | — |
| DEF-J-003 (P1) | Vendor payments created and posted without any approval | No VENDOR_PAYMENT key | — | open | — | — |  | — |
| DEF-J-004 (P1) | Approved supplier's bank details changeable without re-approval or field audit | Profile patch overwrites bank fields | — | open | — | — |  | — |
| DEF-J-005 (P1) | Direct-purchase limit checked on the requester's estimate, not the ordered value | Limit on estimate only | — | open | — | — |  | — |
