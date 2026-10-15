#!/usr/bin/env node
/**
 * Batch 7 (People & workforce) end-to-end click-through in a real Chromium (integrator).
 *
 *   pnpm e2e:people          (from sutraerp/; CI matrix entry `people`, not part of `pnpm -r test`)
 *
 * Reuses the Batch 3 harness (`e2e/lib/harness.mjs`): starts the API binary (dev auth, PostgreSQL)
 * and the web app in API mode on a fresh, migrated database, then walks the Domain E flow across
 * the administrator and faculty roles, one screenshot per step:
 *
 *   seed     institution → `active` (Batch 2 API); nothing else — every People record is created
 *            through the screens.
 *   admin    Staff records: Dr. Meera Shah (linked to the dev faculty login) created → submitted
 *            for verification → verified → active; engagement (Assistant Professor, AP-1)
 *            created → submitted (auto-approved until Batch 9) → activated.
 *   admin    Recruitment: requisition → submitted (opens) → candidate → shortlisted → offer →
 *            accepted.
 *   admin    Leave policy: CL (paid, 12 days) + LWP (unpaid) → submitted → active; opening
 *            entitlements posted.
 *   faculty  home leave shortcut shows the balance; My HR → Leave balance tiles; applies one day
 *            of LWP (auto-approved → reserved).
 *   admin    Leave: the reserved request consumed (`consume:<YYYY-MM>` ledger entry, unpaid);
 *            Attendance: Meera marked present the day before; Workload: a plan drafted.
 *   admin    Payroll: configuration (BASIC fixed + HRA 40 % of basic) → submitted → active;
 *            salary structure AP-1 + Meera's assignment; period → regular run → calculate (LOP
 *            1 day from 7B through LeaveReader) → validate → submit (approved) → finalise → post
 *            (Finance journal) → payslips issued.
 *   faculty  My HR → Payslips lists the payslip; Leave tab shows the balance; dark mode.
 *
 * Screenshots: $E2E_SHOTS (default /workspace/work/out/b7-shots), prefixed `7-`. Every console
 * error, page error and failed /api response fails the run. Requires `psql` on PATH.
 */
import { existsSync, mkdirSync, readdirSync, unlinkSync } from "node:fs";
import path from "node:path";
import { chromium } from "playwright";
import {
  apiBin,
  apiLogin,
  BASE,
  check,
  children,
  failed,
  fillForm as fillDialogForm,
  go,
  log,
  newProblems,
  psql,
  seedInstitution,
  settle,
  shot,
  shotCount,
  SHOTS,
  signIn,
  signOut,
  startApi,
  startWeb,
  step,
  stop,
  tile,
  waitFor,
  watch,
} from "./lib/harness.mjs";

const FACULTY_SUBJECT = "dev|meera.shah@sutra.dev";
const P = "/api/v1/people";

/** Every dialog form closes on success: wait for it so the next dialog mounts fresh. */
async function fillForm(page, name, values) {
  await fillDialogForm(page, name, values);
  await page.locator(`[data-form="${name}"]`).waitFor({ state: "detached", timeout: 15000 });
  await settle(page);
}

/** A list item containing `text`: a table row, a leave approval row or a list row (re-skin v2). */
function row(page, text, scope) {
  return (scope ?? page).locator("tr, [data-approval], .sb-lrow", { hasText: text }).first();
}

/** Opens each visible kebab under `root` until one offers `[data-menu-item=id]`, then clicks it. */
async function viaMenu(page, root, id, { optional = false } = {}) {
  const kebabs = root.locator("[data-row-menu]");
  const n = await kebabs.count();
  for (let i = 0; i < n; i++) {
    const k = kebabs.nth(i);
    if (!(await k.isVisible().catch(() => false))) continue;
    await k.click();
    const it = page.locator(`.sb-menu [data-menu-item="${id}"]`).first();
    if (await it.isVisible().catch(() => false)) {
      await it.click();
      await settle(page);
      return true;
    }
    await k.click(); // toggle closed (Escape would also close a side panel)
  }
  if (optional) return false;
  throw new Error(`action “${id}” is neither a button nor a menu item here`);
}

/** Clicks `[data-action=id]` (the row / tile primary pill) or the kebab menu item `id`, then waits for the network. */
async function action(page, id, scope) {
  const root = scope ?? page;
  const b = root.locator(`[data-action="${id}"]`).first();
  await b.waitFor({ state: "visible", timeout: 4000 }).catch(() => {});
  if (await b.isVisible().catch(() => false)) {
    await b.click();
    await settle(page);
    return;
  }
  await viaMenu(page, root, id);
}

/** Same as `action`, inside the row containing `text`. */
async function rowAction(page, text, id, scope) {
  const r = row(page, text, scope);
  await r.waitFor({ state: "visible", timeout: 10000 });
  await action(page, id, r);
}

/** Clicks `id` in the row containing `text` when it is offered (auto-approval may skip a step). */
async function maybeRowAction(page, text, id, scope) {
  const r = row(page, text, scope);
  const btn = r.locator(`[data-action="${id}"]`).first();
  if (await btn.isVisible().catch(() => false)) {
    await btn.click();
    await settle(page);
    return true;
  }
  return viaMenu(page, r, id, { optional: true });
}

async function expectRowStatus(page, text, status, scope) {
  const r = row(page, text, scope);
  await r.filter({ hasText: status }).waitFor({ timeout: 10000 }).catch(() => {});
  const content = ((await r.textContent().catch(() => "")) ?? "").replace(/\s+/g, " ").trim();
  check(content.includes(status), `row “${text}” shows “${status}” (row: ${content.slice(0, 160)})`);
}

/** Clicks `id` in the open staff side panel (footer pill or its kebab). */
async function panelAction(page, id) {
  const panel = page.locator(".sb-drawer, [role=dialog]").filter({ has: page.locator("[data-staff-drawer]") }).last();
  await panel.waitFor();
  await action(page, id, panel);
}

async function display(page, setting) {
  await page.click('[aria-label="Account menu"]');
  // Re-skin v2 (R1): display settings live in the Theme drawer (avatar menu → Personalization → Theme).
  if (!(await page.locator("[data-pz-group]").count())) await page.click("[data-pz-toggle]");
  await page.click('[data-pz="theme"]');
  await page.locator(`[data-display="${setting}"]`).click();
  await page.keyboard.press("Escape");
  await page.mouse.click(4, 700);
  await settle(page);
}

const iso = (d) => `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
/** The latest Monday–Friday on or before today (the LWP day: started, so it can be consumed). */
function leaveDay() {
  const d = new Date();
  while (d.getDay() === 0 || d.getDay() === 6) d.setDate(d.getDate() - 1);
  return d;
}
const LEAVE = leaveDay();
const LEAVE_ISO = iso(LEAVE);
/** Engagement / salary start: the first day of the month before the leave month (full payroll month). */
const ENG_START = iso(new Date(LEAVE.getFullYear(), LEAVE.getMonth() - 1, 1));
/** Attendance day: the day before the leave day (inside the engagement). */
const ATT_ISO = iso(new Date(LEAVE.getFullYear(), LEAVE.getMonth(), LEAVE.getDate() - 1));
const PAY_YEAR = LEAVE.getFullYear();
const PAY_MONTH = LEAVE.getMonth() + 1;
const YEAR = new Date().getFullYear();

// ---------------------------------------------------------------- run
let api;
let web;
let browser;
const ids = {};
try {
  if (!existsSync(apiBin)) throw new Error(`API binary not found at ${apiBin} — run \`cargo build -p sutra-api\` or set SUTRA_API_BIN`);
  mkdirSync(SHOTS, { recursive: true });
  for (const f of readdirSync(SHOTS)) if (/^(\d\d|fail)-.*\.png$/.test(f)) unlinkSync(path.join(SHOTS, f));
  const existing = psql("SELECT count(*) FROM people.staff_member");
  if (existing !== "0") throw new Error(`DATABASE_URL must be a fresh migrated database (found ${existing} staff records)`);

  log(`API (dev auth), web on ${BASE}, shots → ${SHOTS}; leave day ${LEAVE_ISO}, engagement from ${ENG_START}, payroll ${PAY_YEAR}-${PAY_MONTH}`);
  api = await startApi();
  web = startWeb();

  log("— seed: institution");
  const admin = await apiLogin("admin@sutra.dev");
  const setup = await seedInstitution(admin);
  Object.assign(ids, setup);
  log(`  ✓ institution ${setup.id}`);

  await waitFor(`${BASE}/sign-in`, web);
  browser = await chromium.launch({ headless: !process.env.E2E_HEADED, executablePath: process.env.CHROMIUM_PATH || undefined, args: ["--no-sandbox", ...(process.env.E2E_CHROMIUM_ARGS ?? "").split(" ").filter(Boolean)] });
  const context = await browser.newContext({ viewport: { width: 1440, height: 960 }, locale: "en-IN", timezoneId: "Asia/Kolkata" });
  const page = await context.newPage();
  page.setDefaultTimeout(15000);
  watch(page);
  page.setDefaultNavigationTimeout(180000);
  for (const p of ["/sign-in", "/sign-in"]) await go(page, p);

  // ------------------------------------------------------------ admin: staff records
  await signIn(page, "administrator");
  for (const p of ["/people", "/people/leave", "/people/payroll"]) await go(page, p);
  page.setDefaultNavigationTimeout(30000);
  newProblems();

  await step(page, "admin: People is live in the re-skinned UI (area header, ≤5 tabs + More, directory)", async () => {
    await go(page, "/");
    await page.click('nav.sb-pills a[href="/people"]');
    await settle(page);
    await page.locator('[data-screen="people"]').waitFor();
    check((await page.locator("[data-coming]").count()) === 0, "no “Coming in Batch 7” page");
    const head = page.locator('[data-page-head="people"]');
    const tabs = (await head.locator("nav.sb-tabs > a").allTextContents()).map((t) => t.replace(/ · .*$/, "").trim());
    check(JSON.stringify(tabs) === JSON.stringify(["Directory", "Leave requests", "Payroll", "Recruitment & onboarding"]), `four header tabs (${tabs.join(", ")})`);
    check((await head.locator("[data-tab-more]").count()) === 1, "More menu for positions, attendance and workload");
    check(((await head.locator("[data-area-summary]").textContent()) ?? "").includes("faculty on roll"), "live summary line");
    check((await page.locator("[data-staff-filter]").count()) === 1, "department chips + search");
    await shot(page, "7-admin-people-staff-empty");
  });

  await step(page, "admin: staff record created (linked to the faculty login) → verified → active", async () => {
    await action(page, "staff-new");
    await fillForm(page, "staff-create", { givenName: "Meera", familyName: "Shah", workEmail: "meera.shah@sit.edu.in", staffCategory: "teaching", subjectRef: FACULTY_SUBJECT });
    const card = page.locator('[data-person="Meera Shah"]');
    await card.waitFor();
    await card.click();
    await page.locator("[data-staff-drawer]").waitFor();
    await panelAction(page, "staff-submit-verification");
    await panelAction(page, "staff-verify");
    await panelAction(page, "staff-activate");
    await page.locator('[data-staff-drawer][data-staff-status="active"]').waitFor({ timeout: 10000 }).catch(() => {});
    check((await page.locator('[data-staff-drawer][data-staff-status="active"]').count()) === 1, "staff panel shows the record active");
    const s = await admin.get(`${P}/institutions/${setup.id}/staff`);
    ids.meera = s.items.find((x) => x.displayName === "Meera Shah");
    check(ids.meera?.status === "active", `staff record active (${ids.meera?.status})`);
    await shot(page, "7-admin-staff-active");
  });

  await step(page, "admin: engagement created → submitted (approved) → activated", async () => {
    const drawer = page.locator("[data-staff-drawer]");
    await drawer.waitFor();
    await panelAction(page, "engagement-new");
    await fillForm(page, "engagement-create", { employmentType: "permanent", designation: "Assistant Professor", grade: "AP-1", startDate: ENG_START });
    await row(page, "Assistant Professor", drawer).waitFor();
    await rowAction(page, "Assistant Professor", "engagement-submit", drawer);
    await maybeRowAction(page, "Assistant Professor", "engagement-approve", drawer);
    await maybeRowAction(page, "Assistant Professor", "engagement-activate", drawer);
    await expectRowStatus(page, "Assistant Professor", "Active", drawer);
    await shot(page, "7-admin-engagement-active");
    await page.keyboard.press("Escape");
    await settle(page);
    check(((await page.locator('[data-person="Meera Shah"]').textContent()) ?? "").includes("Assistant Professor"), "person card shows the designation");
    const engs = await admin.get(`${P}/staff/${ids.meera.id}/engagements`);
    ids.engagement = engs.items.find((e) => e.status === "active");
    check(!!ids.engagement, "engagement active");
  });

  // ------------------------------------------------------------ admin: recruitment
  await step(page, "admin: requisition → open, candidate → shortlisted → offer → accepted", async () => {
    await go(page, "/people/recruitment");
    await page.locator("[data-requisitions]").waitFor();
    await action(page, "requisition-new");
    await fillForm(page, "requisition-create", { title: "Assistant Professor (Computer Engineering)", designation: "Assistant Professor", staffCategory: "teaching", employmentType: "probation", openings: "1" });
    await rowAction(page, "Computer Engineering", "requisition-submit", tile(page, "Requisitions"));
    await maybeRowAction(page, "Computer Engineering", "requisition-approve", tile(page, "Requisitions"));
    await expectRowStatus(page, "Computer Engineering", "Open", tile(page, "Requisitions"));
    await action(page, "candidate-new");
    await fillForm(page, "candidate-create", { givenName: "Kavya", familyName: "Nair", email: "kavya.nair@example.org" });
    const cands = tile(page, "Candidates");
    await row(page, "Kavya", cands).waitFor();
    await rowAction(page, "Kavya", "candidate-shortlist", cands);
    await rowAction(page, "Kavya", "candidate-offer", cands);
    await fillForm(page, "candidate-offer", { joining: iso(new Date(Date.now() + 14 * 864e5)) });
    await rowAction(page, "Kavya", "candidate-accept", cands);
    await expectRowStatus(page, "Kavya", "Offer accepted", cands);
    await shot(page, "7-admin-recruitment-offer-accepted");
  });

  // ------------------------------------------------------------ admin: leave policy
  await step(page, "admin: leave policy (CL paid, LWP unpaid) → active, opening entitlements posted", async () => {
    await go(page, "/people/leave");
    await action(page, "policy-new");
    await fillForm(page, "leave-policy", { name: `Leave policy ${YEAR}`, effectiveFrom: `${YEAR}-01-01` });
    const policies = tile(page, "Leave policies");
    await row(page, `Leave policy ${YEAR}`, policies).waitFor();
    await rowAction(page, `Leave policy ${YEAR}`, "policy-type", policies);
    await fillForm(page, "leave-type", { code: "CL", name: "Casual leave", annualEntitlement: "12.00", accrualFrequency: "yearly", isPaid: true, requiresBalance: true });
    await rowAction(page, `Leave policy ${YEAR}`, "policy-type", policies);
    await fillForm(page, "leave-type", { code: "LWP", name: "Leave without pay", accrualFrequency: "none", isPaid: false, requiresBalance: false });
    await rowAction(page, `Leave policy ${YEAR}`, "policy-submit", policies);
    await maybeRowAction(page, `Leave policy ${YEAR}`, "policy-approve", policies);
    await rowAction(page, `Leave policy ${YEAR}`, "policy-activate", policies);
    await expectRowStatus(page, `Leave policy ${YEAR}`, "Active", policies);
    await action(page, "accrual-opening");
    await tile(page, "Accrual & year end").getByText(`Opening entitlements ${YEAR}`).first().waitFor();
    const bal = await admin.get(`${P}/staff/${ids.meera.id}/leave-balances`);
    const cl = bal.items.find((b) => b.leaveTypeCode === "CL");
    check(cl?.entitled === "12.00", `CL opening entitlement 12.00 (${cl?.entitled})`);
    await shot(page, "7-admin-leave-policy-active");
  });

  // ------------------------------------------------------------ faculty: apply for leave
  await signOut(page);
  await signIn(page, "faculty");
  await step(page, "faculty: home leave shortcut shows the CL balance", async () => {
    await go(page, "/");
    const sc = page.locator('[data-shortcut="leave"]');
    await sc.waitFor();
    await page.waitForFunction(() => document.querySelector('[data-shortcut="leave"]')?.textContent?.includes("left"), null, { timeout: 15000 }).catch(() => {});
    const text = ((await sc.textContent()) ?? "").trim();
    check(/12 days casual leave left/.test(text), `shortcut shows the balance in whole days (${text})`);
    await shot(page, "7-faculty-home");
  });

  await step(page, "faculty: Leave = balance tiles + inline apply form; one day of unpaid leave applied (reserved)", async () => {
    await page.click('nav.sb-pills a[href="/leave"]');
    await settle(page);
    await page.locator('[data-page-head="leave"]').waitFor();
    const tabs = await page.locator('[data-page-head="leave"] nav a').allTextContents();
    check(JSON.stringify(tabs) === JSON.stringify(["Leave", "My record", "Attendance", "Workload", "Payslips"]), `My HR tabs (${tabs.join(", ")})`);
    const kpi = page.locator('[data-kpi="balance-CL"]');
    await kpi.waitFor();
    const bal = ((await kpi.locator("b").textContent()) ?? "").replace(/\s+/g, " ").trim();
    check(/^12 \/ 12 days$/.test(bal), `casual leave 12 of 12 days (${bal})`);
    check(((await kpi.textContent()) ?? "").includes("Casual leave"), "balance tile names the leave type");
    await fillDialogForm(page, "leave-apply", { leaveTypeId: "Leave without pay", startDate: LEAVE_ISO, endDate: LEAVE_ISO, reason: "Family function" });
    await settle(page);
    const history = tile(page, "History");
    await row(page, "Leave without pay", history).waitFor();
    await expectRowStatus(page, "Leave without pay", "Reserved", history);
    check(!((await history.textContent()) ?? "").includes(".00"), "history shows whole days (no 1.00)");
    await shot(page, "7-faculty-leave-applied");
  });

  await step(page, "faculty: My HR → My record shows the staff record and engagement", async () => {
    await go(page, "/leave/profile");
    await page.locator("[data-my-profile]").getByText("Meera Shah").first().waitFor();
    check((await page.locator("[data-my-profile]").getByText("Assistant Professor").count()) >= 1, "engagement on My record");
    await shot(page, "7-faculty-my-record");
  });

  // ------------------------------------------------------------ admin: leave consumed, attendance, workload
  await signOut(page);
  await signIn(page, "administrator");
  await step(page, "admin: Leave — the reserved request is consumed (unpaid ledger entry)", async () => {
    await go(page, "/people/leave");
    const requests = tile(page, "Leave requests");
    await action(page, "request-filter-reserved", requests);
    await row(page, "Meera Shah", requests).waitFor();
    await rowAction(page, "Meera Shah", "request-consume", requests);
    await action(page, "request-filter-consumed", requests);
    await expectRowStatus(page, "Meera Shah", "Consumed", requests);
    const ledger = await admin.get(`${P}/staff/${ids.meera.id}/leave-ledger`);
    const entry = ledger.items.find((e) => e.kind === "consumption");
    check(entry?.leaveTypeCode === "LWP" && entry.isPaid === false && entry.postingKey === `consume:${LEAVE_ISO.slice(0, 7)}`, `unpaid consumption ${entry?.postingKey} (${entry?.days})`);
    await shot(page, "7-admin-leave-consumed");
  });

  await step(page, "admin: Attendance — Meera marked present the day before", async () => {
    await go(page, "/people/attendance");
    const att = tile(page, "Staff attendance");
    await att.waitFor();
    await att.locator('input[type="date"]').first().fill(ATT_ISO);
    await settle(page);
    await row(page, "Meera Shah", att).waitFor();
    await rowAction(page, "Meera Shah", "attendance-present", att);
    await expectRowStatus(page, "Meera Shah", "Present", att);
    await shot(page, "7-admin-attendance");
  });

  await step(page, "admin: Workload — teaching load and a drafted workload plan", async () => {
    await go(page, "/people/workload");
    const wl = tile(page, "Faculty workload");
    await wl.waitFor();
    await action(page, "workload-plan", wl);
    await fillForm(page, "workload-plan", { termLabel: "Term 1", availability: "available", plannedWeeklyHours: "12.00", maxWeeklyHours: "16.00" });
    await row(page, "Term 1", wl).waitFor();
    await expectRowStatus(page, "Term 1", "Draft", wl);
    await shot(page, "7-admin-workload");
  });

  // ------------------------------------------------------------ admin: payroll
  await step(page, "admin: payroll configuration (BASIC + HRA) → active, structure AP-1, Meera's salary assignment", async () => {
    await go(page, "/people/payroll");
    await action(page, "new-config");
    await fillForm(page, "config-create", { name: `Payroll ${YEAR}`, effectiveFrom: ENG_START });
    const cfg = tile(page, "Payroll configuration");
    await row(page, `Payroll ${YEAR}`, cfg).waitFor();
    await rowAction(page, `Payroll ${YEAR}`, "config-component", cfg);
    await fillForm(page, "config-component", { code: "BASIC", name: "Basic pay", kind: "earning", basis: "fixed" });
    await rowAction(page, `Payroll ${YEAR}`, "config-component", cfg);
    await fillForm(page, "config-component", { code: "HRA", name: "House rent allowance", kind: "earning", basis: "percent_of_basic", rate: "40" });
    await rowAction(page, `Payroll ${YEAR}`, "config-submit", cfg);
    await maybeRowAction(page, `Payroll ${YEAR}`, "config-approve", cfg);
    await rowAction(page, `Payroll ${YEAR}`, "config-activate", cfg);
    await expectRowStatus(page, `Payroll ${YEAR}`, "Active", cfg);
    await action(page, "structure-new");
    await fillForm(page, "structure-create", { code: "AP-1", name: "Assistant Professor 1", grade: "AP-1", basic: "30000.00" });
    await row(page, "AP-1", tile(page, "Salary structures")).waitFor();
    await action(page, "salary-assign-new");
    await fillForm(page, "salary-assign", { engagementId: "Meera Shah", structureId: "AP-1", effectiveFrom: ENG_START });
    await row(page, "Meera Shah", tile(page, "Salary assignments")).waitFor();
    await shot(page, "7-admin-payroll-config-assignment");
  });

  await step(page, "admin: payroll run calculated — loss of pay from the unpaid leave", async () => {
    await action(page, "new-period");
    await fillForm(page, "period-create", { year: String(PAY_YEAR), month: String(PAY_MONTH) });
    await action(page, "new-run");
    const runs = tile(page, "Payroll runs");
    await row(page, "PR-", runs).waitFor();
    await rowAction(page, "PR-", "run-calculate", runs);
    await expectRowStatus(page, "PR-", "Calculated", runs);
    const results = tile(page, "Results");
    await row(page, "Meera Shah", results).waitFor();
    const text = ((await row(page, "Meera Shah", results).textContent()) ?? "").replace(/\s+/g, " ");
    check(text.includes("1 day"), `result shows 1 day of loss of pay (${text.slice(0, 160)})`);
    await shot(page, "7-admin-payroll-calculated");
  });

  await step(page, "admin: run validated → approved → finalised → posted to Finance → payslips issued", async () => {
    const runs = tile(page, "Payroll runs");
    await rowAction(page, "PR-", "run-validate", runs);
    await rowAction(page, "PR-", "run-submit", runs);
    await maybeRowAction(page, "PR-", "run-approve", runs);
    await rowAction(page, "PR-", "run-finalise", runs);
    await expectRowStatus(page, "PR-", "Finalized", runs);
    await rowAction(page, "PR-", "run-post", runs);
    await expectRowStatus(page, "PR-", "Posted", runs);
    await rowAction(page, "PR-", "run-issue", runs);
    const je = psql(`SELECT count(*) FROM finance.journal_entry WHERE source_type = 'payroll_run'`);
    check(je === "1", `one payroll journal entry in Finance (${je})`);
    const slips = psql(`SELECT count(*) FROM people.payslip WHERE status = 'issued'`);
    check(slips === "1", `one payslip issued (${slips})`);
    await shot(page, "7-admin-payroll-posted");
  });

  await step(page, "admin: home “Faculty on roll” is live", async () => {
    await go(page, "/");
    const kpi = page.locator('[data-kpi="faculty"]');
    await kpi.waitFor();
    await page.waitForFunction(() => !document.querySelector('[data-kpi="faculty"] b')?.textContent?.includes("…"));
    check(!(await kpi.evaluate((n) => n.classList.contains("is-pending"))), "Faculty on roll is no longer pending");
    check(((await kpi.locator("b").textContent()) ?? "").trim() === "1", `one faculty member on roll (${await kpi.locator("b").textContent()})`);
  });

  // ------------------------------------------------------------ faculty: payslip + balance
  await signOut(page);
  await signIn(page, "faculty");
  await step(page, "faculty: My HR → Payslips lists the issued payslip", async () => {
    await go(page, "/leave/payslips");
    const slips = tile(page, "My payslips");
    await row(page, "PS-", slips).waitFor();
    await expectRowStatus(page, "PS-", "Issued", slips);
    await shot(page, "7-faculty-payslips");
  });

  await step(page, "faculty: My HR → Leave shows the balances and the consumed request", async () => {
    await go(page, "/leave");
    await page.locator('[data-kpi="balance-CL"]').waitFor();
    await expectRowStatus(page, "Leave without pay", "Consumed", tile(page, "History"));
    await shot(page, "7-faculty-leave-balance");
  });

  await step(page, "faculty: My HR → Attendance lists the recorded days", async () => {
    await go(page, "/leave/attendance");
    await page.locator("[data-my-attendance]").waitFor();
    await shot(page, "7-faculty-attendance");
  });

  await step(page, "faculty: My HR in dark mode", async () => {
    await go(page, "/leave");
    await display(page, "theme:dark");
    check(await page.evaluate(() => document.documentElement.classList.contains("dk")), "dark mode on <html>");
    await shot(page, "7-faculty-leave-dark");
    await display(page, "theme:light");
  });

  const late = newProblems();
  if (late.length) failed.push(`errors outside the steps:\n    ${late.join("\n    ")}`);
  else log("  ✓ no unexpected console/page/network errors");
} catch (e) {
  failed.push(`setup: ${e instanceof Error ? e.message : e}`);
  console.error("[e2e] FAIL:", e instanceof Error ? e.message : e);
  if (api) console.error(`[e2e] api log tail:\n${api.tail()}`);
} finally {
  await browser?.close().catch(() => {});
  await stop(web);
  await stop(api);
  for (const c of children) await stop(c);
}
if (failed.length) {
  console.error(`[e2e] ${failed.length} step(s) failed:\n  - ${failed.join("\n  - ")}`);
  process.exit(1);
}
log(`PASS — ${shotCount()} screenshots in ${SHOTS}`);
process.exit(0);
