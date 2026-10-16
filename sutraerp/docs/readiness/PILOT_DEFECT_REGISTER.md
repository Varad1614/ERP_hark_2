# Pilot defect register — institutional pilot

> **Status: No real-user results recorded — BLOCKED_EXTERNAL.** There is no authorised pilot institution, no pilot users and no institutional authorisation. Nothing in this file may be filled from synthetic data, demo runs or assumptions; the synthetic UAT dry run is recorded separately in [`UAT_TEST_CASES.md`](UAT_TEST_CASES.md) (column “Synthetic dry run (not UAT)”) and is not a UAT result.

## Real pilot defects (from institutional UAT / the parallel run)

None — no pilot has taken place. Fields to capture per defect:

| Defect id (PDEF-nnn) | Raised (date) | Raised by (tester id, role) | UAT case / feedback id | Area | Summary | Steps to reproduce | Expected | Actual | Severity (Critical / High / Medium / Low) | Data affected (personal data? DPDP breach assessment) | Root cause | Fix (commit / PR) | Regression test | Retested in round / by | Status (Open / Fixing / Ready for retest / Closed / Won't fix + reason) |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| | | | | | | | | | | | | | | | |

Severity guide: **Critical** — wrong money, wrong grade/result, data loss, personal-data exposure or
a governed decision taken without an independent checker; **High** — a core workflow blocked with no
workaround; **Medium** — workaround exists; **Low** — cosmetic.

## Findings of the synthetic UAT dry run (NOT pilot defects — engineering evidence only)

Found by `apps/web/e2e/uat-dryrun.e2e.mjs` with synthetic data on the demo stack. Listed so the
pilot starts from a known state; they are not institutional findings.

| Id | Found by | Summary | Severity (engineering) | Fix / disposition | Test | Status |
|---|---|---|---|---|---|---|
| DEF-P2-PILOT-001 | dry-run case UAT-EXM-01 | Malpractice committee decisions could be approved by the exam official who proposed them: `POST /api/v1/exams/malpractice-cases/{id}/approval` had no maker ≠ checker guard (its path ends in `/approval`, so the approval-matrix fitness test never enumerated it), and the guard sat on the proposal (maker) step, where it wrongly refused a re-proposal after a rejection. | Critical (governed decision without an independent checker) | guard moved to the approval endpoint; fitness test enumerates `…/approval`; matrix doc updated (`modules/exams/src/integrity/http.rs`, `apps/api/tests/approval_matrix.rs`) | `db_p2_pilot::malpractice_decision_is_approved_by_an_independent_controller`; dry-run UAT-EXM-01 | see tracker `tracker/p2-s6-pilot.md` |
| PDEF-SYN-002 | dry-run case UAT-ACC-01 (observation) | The Adjustments screen offers **Approve / Reject** on a pending adjustment to its own maker; the API correctly refuses (403). UI-only: misleading, not a control failure. | Low | not fixed in S6 (UI change outside the dry-run scope); the import screen already hides decisions from the requester (`can`/`canDecide`) — apply the same pattern to finance decisions | — | Open (backlog) |
| PDEF-SYN-003 | dry-run design (role mapping) | No dedicated “accounts staff” or “examination staff” role: their maker work needs the full administrator role. | Medium (least privilege) | pilot decision recorded in PILOT_SCOPE.md §7; role model change needs institutional requirements | — | Open (needs institution input — BLOCKED_EXTERNAL) |
