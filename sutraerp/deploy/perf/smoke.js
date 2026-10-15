// perf-smoke gate (docs/batch9-contracts.md §9): 20 VU for 60 s, mixed read 70 % / write 30 %.
// Fails the job when http_req_failed ≥ 1 %, read p95 ≥ 300 ms, write p95 ≥ 800 ms or checks ≤ 99 %.
// Run: k6 run deploy/perf/smoke.js (env: see lib.js). Thresholds change only by a contract change.
import { mixed, signIn, summary } from "./lib.js";

export const options = {
  scenarios: {
    smoke: { executor: "constant-vus", vus: 20, duration: "60s", gracefulStop: "10s" },
  },
  thresholds: {
    http_req_failed: ["rate<0.01"],
    "http_req_duration{kind:read}": ["p(95)<300"],
    "http_req_duration{kind:write}": ["p(95)<800"],
    checks: ["rate>0.99"],
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
  return summary("smoke", data);
}
