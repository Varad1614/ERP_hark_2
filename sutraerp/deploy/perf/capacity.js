// capacity gate (docs/batch9-contracts.md §9): ramp 10 → 100 VU over 5 min, hold 100 VU for 3 min.
// Fails the job when http_req_failed ≥ 2 %, overall p95 ≥ 1 s or the sustained rate < 150 req/s;
// deploy/perf/compare.mjs then fails it when p95 regresses > 25 % against deploy/perf/baseline.json.
// Runs on erp_merge, v* tags and workflow_dispatch (job `Capacity (k6)`).
import { mixed, signIn, summary } from "./lib.js";

export const options = {
  scenarios: {
    capacity: {
      executor: "ramping-vus",
      startVUs: 10,
      stages: [
        { duration: "5m", target: 100 },
        { duration: "3m", target: 100 },
      ],
      gracefulRampDown: "10s",
      gracefulStop: "10s",
    },
  },
  thresholds: {
    http_req_failed: ["rate<0.02"],
    http_req_duration: ["p(95)<1000"],
    http_reqs: ["rate>=150"],
    // Always-true entries: they only make k6 record the per-kind sub-metrics for the summary.
    "http_req_duration{kind:read}": ["max>=0"],
    "http_req_duration{kind:write}": ["max>=0"],
  },
  summaryTrendStats: ["avg", "min", "med", "max", "p(90)", "p(95)", "p(99)"],
};

export function setup() {
  return signIn();
}

export default function (session) {
  mixed(session);
}

export function handleSummary(data) {
  return summary("capacity", data);
}
