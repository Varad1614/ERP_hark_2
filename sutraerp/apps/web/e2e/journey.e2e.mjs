#!/usr/bin/env node
// Bootstrap version (auth smoke) — replaced by the full journey in the next commit.
import { chromium } from "playwright";
import { writeFileSync, mkdirSync } from "node:fs";
import { login, browserSignIn, AUTH, BASE } from "./lib/auth.mjs";
const results = { auth: AUTH, steps: [] };
const out = process.env.E2E_RESULTS ?? "/tmp/journey-results.json";
let code = 0;
try {
  for (const r of ["administrator", "faculty", "student"]) {
    const c = await login(r);
    results.steps.push({ id: `auth-${r}`, name: `API sign-in ${r}`, status: "pass", evidence: [`subject ${c.subject} mfa ${c.session.mfaLevel} amr ${c.session.amr}`] });
  }
  const shots = process.env.E2E_SHOTS ?? "/tmp/shots";
  mkdirSync(shots, { recursive: true });
  const browser = await chromium.launch({ args: ["--no-sandbox"] });
  for (const r of ["administrator", "faculty", "student"]) {
    const ctx = await browser.newContext({ viewport: { width: 1440, height: 960 } });
    const page = await ctx.newPage();
    try {
      await browserSignIn(page, r);
      await page.screenshot({ path: `${shots}/smoke-${r}.png` });
      results.steps.push({ id: `browser-${r}`, name: `browser sign-in ${r}`, status: "pass", evidence: [page.url()] });
    } catch (e) {
      await page.screenshot({ path: `${shots}/smoke-${r}-fail.png` }).catch(() => {});
      results.steps.push({ id: `browser-${r}`, name: `browser sign-in ${r}`, status: "fail", evidence: [String(e.message)] });
      code = 1;
    }
    await ctx.close();
  }
  await browser.close();
} catch (e) {
  console.error(e);
  results.steps.push({ id: "setup", name: "setup", status: "fail", evidence: [String(e.message)] });
  code = 1;
}
writeFileSync(out, JSON.stringify(results, null, 2));
console.log(JSON.stringify(results, null, 2));
process.exit(code);
