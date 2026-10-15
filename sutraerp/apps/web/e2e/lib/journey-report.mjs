#!/usr/bin/env node
/** Renders journey-results.json (journey.e2e.mjs) as the Markdown job summary. */
import { readFileSync } from "node:fs";
const r = JSON.parse(readFileSync(process.argv[2], "utf8"));
const esc = (s) => String(s ?? "").replace(/\|/g, "\\|").replace(/\n/g, "<br>");
const icon = { pass: "✅ pass", fail: "❌ fail", gap: "⚠️ gap", skip: "⏭️ skip" };
const lines = [];
lines.push(`## SutraERP Part 1 — full-stack journey (${r.auth ?? "?"} auth)`);
const steps = r.steps ?? [];
const n = (s) => steps.filter((x) => x.status === s).length;
lines.push("", `**${n("pass")} passed · ${n("fail")} failed · ${n("gap")} gap(s)** — commit \`${(r.sha ?? process.env.GITHUB_SHA ?? "").slice(0, 12)}\`, run ${process.env.GITHUB_RUN_ID ?? r.runId ?? "local"}`, "");
lines.push("| # | Phase | Step | Driven by | Result | Evidence |", "|---|---|---|---|---|---|");
steps.forEach((s, i) => lines.push(`| ${i + 1} | ${esc(s.phase ?? "")} | ${esc(s.name)} | ${esc(s.via ?? "")} | ${icon[s.status] ?? s.status} | ${esc((s.evidence ?? []).slice(0, 12).join("\n"))} |`));
if (r.gaps?.length) {
  lines.push("", "### Steps driven through the API (gaps)", "", "| Step | Why |", "|---|---|");
  for (const g of r.gaps) lines.push(`| ${esc(g.step)} | ${esc(g.why)} |`);
}
if (r.autoApproved?.length) {
  lines.push("", "### Where the journey depends on auto-approval (DEF-X-001)", "", "| Kind | Record | State seen | Step | Real decision endpoint |", "|---|---|---|---|---|");
  for (const a of r.autoApproved) lines.push(`| ${esc(a.kind)} | \`${esc(a.id)}\` | ${esc(a.status)} | ${esc(a.step)} | \`${esc(a.decision)}\` |`);
}
if (r.defects?.length) {
  lines.push("", "### Defects observed", "");
  for (const d of r.defects) lines.push(`- ${esc(d)}`);
}
console.log(lines.join("\n"));
