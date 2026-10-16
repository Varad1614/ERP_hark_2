# UAT results — institutional pilot

> **Status: No real-user results recorded — BLOCKED_EXTERNAL.** There is no authorised pilot institution, no pilot users and no institutional authorisation. Nothing in this file may be filled from synthetic data, demo runs or assumptions; the synthetic UAT dry run is recorded separately in [`UAT_TEST_CASES.md`](UAT_TEST_CASES.md) (column “Synthetic dry run (not UAT)”) and is not a UAT result.

Template. One row per executed UAT case per round, entered by (or witnessed with) the named
institutional tester. Case ids come from [`UAT_TEST_CASES.md`](UAT_TEST_CASES.md).

## Round header (copy per round)

| Field | Value |
|---|---|
| Institution | _(legal name — blank until authorised)_ |
| Pilot authorisation reference | _(from PILOT_SCOPE.md P1)_ |
| UAT round | _(1, 2, …)_ |
| Environment + build | _(URL, git sha, deploy run id)_ |
| Data set | _(migration job ids / date of extract)_ |
| Period | _(from – to)_ |
| Coordinator | _(name, role)_ |

## Results

| Case id | Round | Tester (name, role) | Date | Build sha | Result (Pass / Fail / Blocked / Not run) | Actual result (what happened) | Evidence (screenshot / export / record id) | Defect id(s) | Retest of (round/case) | Witness |
|---|---|---|---|---|---|---|---|---|---|---|
| | | | | | | | | | | |

## Round summary (fill when the round closes)

| Metric | Value |
|---|---|
| Cases planned / executed / passed / failed / blocked | |
| Open Critical / High / Medium / Low defects | |
| Migration reconciliation (UAT-MIG-*) | |
| Report comparisons (UAT-RPT-*) | |
| Coordinator sign-off (name, date) | |
