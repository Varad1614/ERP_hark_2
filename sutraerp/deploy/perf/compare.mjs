#!/usr/bin/env node
/**
 * Gate summary + baseline comparison (Batch 9 · 9D, docs/batch9-contracts.md §9).
 *
 *   node deploy/perf/compare.mjs <gate> <summary.json> [baseline.json]
 *
 * Prints a Markdown table (appended to $GITHUB_STEP_SUMMARY when set) of the gate's p95s, rate,
 * error and check rates next to the committed baseline. For the `capacity` gate a p95 regression
 * of more than 25 % (read, write or overall) against `baseline.json` → exit 1, but only when the
 * p95 also grew by more than MIN_DELTA_MS (INT: noise floor — on shared GitHub runners a p95 in
 * the 10–25 ms range moves by ±8 ms between runs of the SAME code: 9D's capacity run 38564995071
 * on 9f3f619e read 23.0 ms vs the 15.1 ms baseline of c6ae5964 with no API change in between and
 * the same 460 req/s). A capacity throughput drop below 75 % of the baseline rps also fails. For `smoke` the
 * comparison is informational (its own thresholds are enforced by k6). A missing baseline entry
 * is reported, never invented. Exit 1 also when the summary says a k6 threshold failed.
 */
/* global process, console, URL */
import { appendFileSync, existsSync, readFileSync } from "node:fs";

const [gate, summaryPath, baselinePath = new URL("./baseline.json", import.meta.url).pathname] = process.argv.slice(2);
if (!gate || !summaryPath) {
  console.error("usage: compare.mjs <gate> <summary.json> [baseline.json]");
  process.exit(2);
}
const s = JSON.parse(readFileSync(summaryPath, "utf8"));
const baseline = existsSync(baselinePath) ? JSON.parse(readFileSync(baselinePath, "utf8")) : {};
const b = baseline[gate] ?? null;
const LIMIT = 1.25;
// INT: absolute noise floor (ms) a p95 must also exceed before a relative regression counts.
const MIN_DELTA_MS = Number(process.env.SUTRA_PERF_MIN_DELTA_MS ?? 10);
// INT: throughput floor relative to the baseline (capacity).
const RPS_FLOOR = 0.75;
const f = (v, d = 1) => (v === null || v === undefined ? "—" : Number(v).toFixed(d));
const rows = [];
const regressions = [];
for (const k of ["read", "write", "overall"]) {
  const now = s.p95?.[k];
  const base = b?.p95?.[k];
  let verdict = "no baseline";
  if (base && now !== null && now !== undefined) {
    const ratio = now / base;
    verdict = `${ratio >= 1 ? "+" : ""}${f((ratio - 1) * 100)} %`;
    if (ratio > LIMIT && now - base > MIN_DELTA_MS) {
      verdict += ` ✗ (> 25 % and > ${MIN_DELTA_MS} ms)`;
      regressions.push(`${k} p95 ${f(now)} ms vs baseline ${f(base)} ms`);
    } else if (ratio > LIMIT) {
      verdict += ` (within the ${MIN_DELTA_MS} ms noise floor)`;
    }
  }
  rows.push(`| p95 ${k} (ms) | ${f(now)} | ${f(base)} | ${verdict} |`);
}
let rpsVerdict = "";
if (b?.rps && s.rps !== null && s.rps !== undefined) {
  const r = s.rps / b.rps;
  rpsVerdict = `${r >= 1 ? "+" : ""}${f((r - 1) * 100)} %`;
  if (r < RPS_FLOOR) {
    rpsVerdict += " ✗ (< 75 % of baseline)";
    regressions.push(`throughput ${f(s.rps)} req/s vs baseline ${f(b.rps)} req/s`);
  }
}
rows.push(`| requests/s | ${f(s.rps)} | ${f(b?.rps)} | ${rpsVerdict} |`);
rows.push(`| failed requests | ${f((s.failedRate ?? 0) * 100, 2)} % | ${b ? f((b.failedRate ?? 0) * 100, 2) + " %" : "—"} | |`);
rows.push(`| checks | ${f((s.checksRate ?? 0) * 100, 2)} % | | |`);
rows.push(`| requests | ${s.requests} | ${b?.requests ?? "—"} | |`);
const thresholdRows = Object.entries(s.thresholds ?? {}).map(
  ([metric, t]) => `| \`${metric}\` | ${Object.keys(t).map((x) => `\`${x}\``).join(", ")} | ${Object.values(t).every((x) => x.ok) ? "✅ pass" : "❌ fail"} |`,
);
const md = [
  `### Performance gate \`${gate}\` (k6)`,
  "",
  "| Metric | This run | Baseline | Δ |",
  "|---|---|---|---|",
  ...rows,
  "",
  "| Threshold (contract §9) | Expression | Result |",
  "|---|---|---|",
  ...thresholdRows,
  "",
  b ? `Baseline: \`deploy/perf/baseline.json\` (${b.source ?? "committed"}).` : "Baseline: none committed for this gate yet.",
  gate === "capacity"
    ? `Capacity fails on a p95 regression > 25 % that is also > ${MIN_DELTA_MS} ms, or on throughput < 75 % of the baseline.`
    : "Smoke: baseline comparison is informational.",
  "",
].join("\n");
console.log(md);
if (process.env.GITHUB_STEP_SUMMARY) appendFileSync(process.env.GITHUB_STEP_SUMMARY, md + "\n");
let fail = false;
if (s.thresholdsPassed === false) {
  console.error(`✗ ${gate}: a k6 threshold failed`);
  fail = true;
}
if (gate === "capacity" && regressions.length) {
  console.error(`✗ capacity regression: ${regressions.join("; ")}`);
  fail = true;
}
process.exit(fail ? 1 : 0);
