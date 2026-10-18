#!/usr/bin/env node
/**
 * ERP_EXTENSIONS (P4 step 2) click-through in a real Chromium: every screen the Everything logic
 * port added (docs/extensions-contracts.md, docs/extensions-3e-printing.md).
 *
 *   pnpm e2e:extensions          (from sutraerp/ or apps/web; not part of `pnpm -r test`)
 *
 * Reuses the shared harness (`e2e/lib/harness.mjs`): starts the API binary (dev auth, PostgreSQL,
 * runtime DB role, dev auto-approval) and the web app in API mode on a FRESH migrated database,
 * seeds through the API (plus the finance foundation rows by psql, exactly like
 * `finance.e2e.mjs`), then opens each extension tile, checks it renders, does one cheap
 * representative action and takes a full-page screenshot:
 *
 *   seed     institution → active (Batch 2 API); students Aarav (dev student login) + Isha; finance
 *            chart / TUITION fee head / Aarav's account / one published ₹1,25,000 charge (psql,
 *            like tests/support/b5.rs); a ledger account placed in statement group `fixed_asset`
 *            (1C, API); Meera Shah's active staff record linked to the dev faculty login + active
 *            engagement (API); an open requisition with one candidate (API).
 *   admin    Finance → Collections: cash counter payment → RCT-000001; Counter printing section:
 *            day close card (count ≥ 1, Print), printer setup (paper width, Test print), receipt
 *            layout form saved and re-read; receipt dialog Print + Duplicate copy, row Print
 *            (no printer configured → the print-dialog iframe; window.print is stubbed);
 *            Student 360 → Fees reprint.                                                    (3E)
 *            Finance → Bank & treasury: bank account, bank CSV uploaded → previewed → imported;
 *            Reconcile with bank (reconcile / BRS / history views).                     (1A, 1B)
 *            Finance → Ledger: Financial statements (I&E, balance sheet, cash flow).        (1C)
 *            Finance → Fees & charges: billing plan created, student enrolled, run previewed (1E);
 *            late-fee rule + reminder schedule drafted → effective; Receivables: pre-due
 *            reminders sent, late fees previewed (and charged when any are due).            (1D)
 *            Students → Admissions: Admissions funnel — enquiry recorded, funnel view.       (2A)
 *            Campus → Service desk: automation — SLA board, rule saved.                      (2B)
 *            Campus → Inventory: Stock demand & expiry — settings saved.                     (2C)
 *            Campus → Transport: Fleet maintenance — settings saved.                         (2D)
 *            People → Attendance: Duty roster — shift planned.                               (3A)
 *            People → Recruitment: Interview scorecards — faculty rubric saved.              (3B)
 *            Campus → Procurement: Contracts and MoUs — contract added (3C); Expense claims —
 *            settings saved (3D admin).
 *   faculty  My HR → Payslips: My expenses — expense typed in and saved, claimed, submitted (3D).
 *   admin    Procurement → Expense claims → Approvals lists the submitted claim.             (3D)
 *
 * Failure policy: every `pageerror`, console error and /api/ response ≥ 500 fails the step it
 * happened in; /api/ 4xx answers are recorded (printed at the end) and only fail when the step's
 * own awaited action got one.
 *
 * Environment (see lib/harness.mjs for the rest: DATABASE_URL, SUTRA_API_BIN, E2E_WEB,
 * E2E_API_PORT, CHROMIUM_PATH, E2E_HEADED):
 *   SHOTS_DIR   screenshot directory (else $E2E_SHOTS, else ./e2e-shots/extensions relative to the
 *               working directory — apps/web when run through pnpm). Files: NN-area-screen.png,
 *               failures fail-NN-<step>.png.
 * Requires `psql` on PATH.
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
  problems,
  psql,
  seedInstitution,
  seedStudent,
  settle,
  signIn,
  signOut,
  startApi,
  startWeb,
  STUDENT_SUBJECT,
  stop,
  TENANT,
  waitFor,
} from "./lib/harness.mjs";

const SHOTS_DIR = path.resolve(process.env.SHOTS_DIR ?? process.env.E2E_SHOTS ?? "./e2e-shots/extensions");
const FACULTY_SUBJECT = "dev|meera.shah@sutra.dev";
const P = "/api/v1/people";
const F = "/api/v1/finance";

// ---------------------------------------------------------------- dates (device = Asia/Kolkata)
const pad = (n) => String(n).padStart(2, "0");
const iso = (d) => `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
const dmy = (d) => `${pad(d.getDate())}/${pad(d.getMonth() + 1)}/${d.getFullYear()}`;
const daysFromNow = (n) => new Date(Date.now() + n * 864e5);
const TODAY = iso(new Date());

// ---------------------------------------------------------------- finance foundation (as finance.e2e.mjs)
/** The default chart of accounts of `sutra_finance::shared::coa::DEFAULT_CHART` (keep in sync with finance.e2e.mjs). */
const CHART = [
  ["cash_on_hand", "1100", "Cash on hand", "asset"],
  ["bank_main", "1200", "Bank — main collection account", "asset"],
  ["gateway_clearing", "1250", "Payment gateway clearing", "asset"],
  ["student_receivables", "1300", "Student fee receivables", "asset"],
  ["unapplied_receipts", "2100", "Unapplied receipts and student advances", "liability"],
  ["refunds_payable", "2200", "Refunds payable", "liability"],
  ["deposits_held", "2300", "Refundable deposits held", "liability"],
  ["accumulated_fund", "3100", "Accumulated fund", "equity"],
  ["fee_income", "4100", "Tuition and academic fee income", "income"],
  ["other_income", "4900", "Other fee income", "income"],
  ["aid_expense", "5100", "Scholarships and financial aid", "expense"],
  ["concession_expense", "5200", "Fee concessions and waivers", "expense"],
  ["bad_debt_expense", "5300", "Fees written off", "expense"],
  ["bank_charges", "5900", "Bank and gateway charges", "expense"],
];
const NORMAL = { asset: "debit", expense: "debit", liability: "credit", equity: "credit", income: "credit" };

/** Chart + fee head + Aarav's finance account + one published (overdue) tuition charge. */
function seedFinance(setup, studentId, studentNumber) {
  for (const [key, code, name, type] of CHART)
    psql(
      `INSERT INTO finance.ledger_account (tenant_id, institution_id, code, name, account_type, normal_balance, system_key) ` +
        `VALUES ('${TENANT}', '${setup.id}', '${code}', '${name}', '${type}', '${NORMAL[type]}', '${key}') RETURNING id`,
    );
  const account = (key) => psql(`SELECT id FROM finance.ledger_account WHERE institution_id = '${setup.id}' AND system_key = '${key}'`);
  const feeHeadId = psql(
    `INSERT INTO finance.fee_head (tenant_id, institution_id, code, name, category, income_account_id) ` +
      `VALUES ('${TENANT}', '${setup.id}', 'TUITION', 'Tuition fee', 'tuition', '${account("fee_income")}') RETURNING id`,
  );
  const studentAccountId = psql(
    `INSERT INTO finance.student_account (tenant_id, institution_id, party_kind, student_id, student_number, display_name, subject_ref) ` +
      `VALUES ('${TENANT}', '${setup.id}', 'student', '${studentId}', '${studentNumber}', 'Aarav Desai', '${STUDENT_SUBJECT}') RETURNING id`,
  );
  const chargeId = psql(
    `INSERT INTO finance.charge (tenant_id, institution_id, student_account_id, student_id, charge_number, academic_year_id, ` +
      `academic_term_id, description, total_amount, due_date, source_type, status, published_at) VALUES ('${TENANT}', '${setup.id}', ` +
      `'${studentAccountId}', '${studentId}', 'CHG-000001', '${setup.yearId}', '${setup.termIds[0]}', 'Tuition fee — term 1', ` +
      `125000.00, DATE '2026-07-15', 'manual', 'published', now()) RETURNING id`,
  );
  psql(
    `INSERT INTO finance.charge_line (tenant_id, charge_id, line_no, fee_head_id, description, amount, income_account_id) ` +
      `VALUES ('${TENANT}', '${chargeId}', 1, '${feeHeadId}', 'Tuition', 125000.00, '${account("fee_income")}') RETURNING id`,
  );
  psql(`INSERT INTO finance.number_sequence (tenant_id, institution_id, kind, next_value) VALUES ('${TENANT}', '${setup.id}', 'charge', 2), ('${TENANT}', '${setup.id}', 'journal', 2) RETURNING kind`);
  psql(
    `WITH e AS (INSERT INTO finance.journal_entry (tenant_id, institution_id, entry_number, posting_date, kind, source_type, source_id, ` +
      `posting_key, description, total_debit, total_credit) VALUES ('${TENANT}', '${setup.id}', 'JE-000001', DATE '2026-07-01', 'standard', ` +
      `'charge', '${chargeId}', 'publish', 'Charge CHG-000001 published', 125000.00, 125000.00) RETURNING id) ` +
      `INSERT INTO finance.journal_line (tenant_id, journal_entry_id, line_no, ledger_account_id, debit, credit, student_account_id, charge_id) ` +
      `SELECT '${TENANT}'::uuid, e.id, 1, '${account("student_receivables")}'::uuid, 125000.00, 0, '${studentAccountId}'::uuid, '${chargeId}'::uuid FROM e ` +
      `UNION ALL SELECT '${TENANT}'::uuid, e.id, 2, '${account("fee_income")}'::uuid, 0, 125000.00, NULL::uuid, NULL::uuid FROM e RETURNING journal_entry_id`,
  );
  return { feeHeadId, studentAccountId, chargeId };
}

/** Best-effort API seeding: a failure is logged and the dependent step shows what is missing. */
async function attempt(what, fn) {
  try {
    const r = await fn();
    log(`  ✓ seed: ${what}`);
    return r;
  } catch (e) {
    log(`  ! seed: ${what} skipped (${e instanceof Error ? e.message.slice(0, 300) : e})`);
    return null;
  }
}

// ---------------------------------------------------------------- browser: watch, step, shot
/** /api/ 4xx answers seen (recorded, not failing on their own). */
const clientErrors = [];
let currentStep = "setup";

function watchExtensions(page) {
  page.on("console", (m) => {
    if (m.type() !== "error") return;
    const text = m.text();
    // A 4xx/5xx answer also logs "Failed to load resource": the response handler judges those.
    if (/Failed to load resource|Outdated Optimize Dep/.test(text)) return;
    problems.push(`console: ${text}`);
    log(`  console error: ${text}`);
  });
  page.on("pageerror", (e) => {
    if (/dynamically imported module/.test(e.message)) return;
    problems.push(`pageerror: ${e.message}`);
    log(`  page error: ${e.message}`);
  });
  page.on("response", async (r) => {
    const u = new URL(r.url());
    if (!u.pathname.startsWith("/api/") || r.status() < 400) return;
    const body = await r.text().catch(() => "");
    const line = `${r.request().method()} ${u.pathname} → ${r.status()} ${body.slice(0, 300)}`;
    if (r.status() >= 500) {
      problems.push(line);
      log(`  SERVER ERROR ${line}`);
    } else {
      clientErrors.push(`[${currentStep}] ${line}`);
      log(`  4xx (recorded) ${line}`);
    }
  });
}

let shotNo = 0;
/** Full-page screenshot `NN-area-screen.png`; visibly broken text fails the step like the other suites. */
async function shot(page, area, screen) {
  shotNo++;
  const slug = (s) => s.replace(/[^a-z0-9]+/gi, "-").replace(/^-|-$/g, "").toLowerCase();
  const file = path.join(SHOTS_DIR, `${pad(shotNo)}-${slug(area)}-${slug(screen)}.png`);
  await settle(page);
  await page.waitForFunction(() => document.querySelectorAll(".sb-toast").length === 0, null, { timeout: 6000 }).catch(() => {});
  const broken = await page.evaluate(() => {
    const main = document.querySelector("main") ?? document.body;
    const leaked = main.innerText.match(/\bundefined\b|\bNaN\b|\[object Object\]/);
    return leaked ? `text “${leaked[0]}” on the page` : null;
  });
  if (broken) problems.push(`${area}/${screen}: ${broken}`);
  const tag = await page.addStyleTag({ content: ".sb-env-tag { visibility: hidden !important; }" });
  await page.screenshot({ path: file, fullPage: true });
  await tag.evaluate((n) => n.remove()).catch(() => {});
  log(`  📸 ${path.basename(file)}`);
}

async function step(page, name, fn) {
  currentStep = name;
  log(`— ${name}`);
  newProblems();
  try {
    await fn();
    const p = newProblems();
    if (p.length) throw new Error(`errors during step:\n    ${p.join("\n    ")}`);
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    failed.push(`${name}: ${msg}`);
    console.error(`[e2e]   ✗ ${name}: ${msg}`);
    const file = path.join(SHOTS_DIR, `fail-${pad(failed.length)}-${name.replace(/[^a-z0-9]+/gi, "-").toLowerCase().slice(0, 50)}.png`);
    await page.screenshot({ path: file, fullPage: true }).catch(() => {});
    await page.keyboard.press("Escape").catch(() => {});
  }
}

// ---------------------------------------------------------------- browser: actions
/**
 * Runs `trigger` and waits for the API answer `method` + pathname regex; throws on ≥ 400.
 * Returns the response. `query` (optional) must be contained in the request's search string.
 */
async function apiAction(page, method, re, trigger, { query, timeout = 20000 } = {}) {
  const [res] = await Promise.all([
    page.waitForResponse((r) => {
      if (r.request().method() !== method) return false;
      const u = new URL(r.url());
      return re.test(u.pathname) && (!query || u.search.includes(query));
    }, { timeout }),
    trigger(),
  ]);
  const u = new URL(res.url());
  if (res.status() >= 400) throw new Error(`${method} ${u.pathname}${u.search} → ${res.status()} ${(await res.text().catch(() => "")).slice(0, 300)}`);
  log(`  ✓ ${method} ${u.pathname} → ${res.status()}`);
  await settle(page);
  return res;
}

/** The input / select labelled exactly `label` inside `scope`. */
function field(scope, label) {
  return scope.getByLabel(label, { exact: true }).first();
}

async function fill(scope, label, value) {
  const el = field(scope, label);
  await el.waitFor({ state: "visible", timeout: 10000 });
  await el.fill(String(value));
}

/** Selects the option whose value is `text`, else the first whose label contains it. */
async function choose(select, text) {
  await select.waitFor({ state: "visible", timeout: 10000 });
  // Options can arrive after the select renders (e.g. the PersonPicker shows "Loading…" first).
  let opts = [];
  let hit;
  for (let i = 0; i < 40 && !hit; i++) {
    if (i) await new Promise((r) => setTimeout(r, 250));
    opts = await select.locator("option").evaluateAll((os) => os.map((o) => ({ value: o.value, label: o.textContent ?? "", disabled: o.disabled })));
    hit = opts.find((o) => o.value === text) ?? opts.find((o) => o.label.includes(text));
  }
  if (!hit) throw new Error(`no option “${text}” (${opts.map((o) => o.label).join(" | ")})`);
  await select.selectOption(hit.value);
}

/** A tile by its data attribute, checked visible with its heading. */
async function openTile(page, attr, title) {
  const t = page.locator(`[${attr}]`).first();
  await t.waitFor({ state: "visible", timeout: 20000 });
  await t.scrollIntoViewIfNeeded().catch(() => {});
  const heading = t.locator("h1, h2, h3, h4", { hasText: title }).first();
  await heading.waitFor({ state: "visible", timeout: 10000 }).catch(() => {});
  check(await heading.isVisible().catch(() => false), `tile “${title}” renders (${attr})`);
  return t;
}

async function tab(page, scope, attr, value) {
  await scope.locator(`[${attr}="${value}"]`).first().click();
  await settle(page);
}

/** A list row containing `text` (table row or re-skin list row). */
function row(page, text, scope) {
  return (scope ?? page).locator("tr, .sb-lrow", { hasText: text }).first();
}

/** Opens each visible kebab under `root` until one offers `[data-menu-item=id]`, then clicks it. */
async function viaMenu(page, root, id, { optional = false } = {}) {
  const kebabs = root.locator("[data-row-menu]");
  const n = await kebabs.count();
  for (let i = 0; i < n; i++) {
    const k = kebabs.nth(i);
    if (!(await k.isVisible().catch(() => false))) continue;
    await k.click();
    const it = page.locator(`.sb-menu [data-menu-item="${id}"], [data-menu-item="${id}"]`).first();
    if (await it.isVisible().catch(() => false)) {
      await it.click();
      await settle(page);
      return true;
    }
    await k.click();
  }
  if (optional) return false;
  throw new Error(`action “${id}” is neither a button nor a menu item here`);
}

/** Clicks the pill `[data-action=id]` or the kebab item `id` in the row containing `text`. */
async function rowAction(page, text, id, scope, { optional = false } = {}) {
  const r = row(page, text, scope);
  await r.waitFor({ state: "visible", timeout: 10000 });
  const pill = r.locator(`[data-action="${id}"]`).first();
  if (await pill.isVisible().catch(() => false)) {
    await pill.click();
    await settle(page);
    return true;
  }
  return viaMenu(page, r, id, { optional });
}

async function expectRowText(page, text, want, scope) {
  const r = row(page, text, scope);
  await r.filter({ hasText: want }).waitFor({ timeout: 10000 }).catch(() => {});
  const content = ((await r.textContent().catch(() => "")) ?? "").replace(/\s+/g, " ").trim();
  check(content.includes(want), `row “${text}” shows “${want}” (row: ${content.slice(0, 160)})`);
}

/** Dialog forms close on success: wait so the next one mounts fresh. */
async function fillDialog(page, name, values) {
  await fillDialogForm(page, name, values);
  await page.locator(`[data-form="${name}"]`).waitFor({ state: "detached", timeout: 15000 });
  await settle(page);
}

/** Print dialog stub (installed by addInitScript): prints recorded so far. */
const prints = (page) => page.evaluate(() => window.__prints ?? []);
async function expectPrint(page, before, what, contains = []) {
  await page.waitForFunction((n) => (window.__prints ?? []).length > n, before, { timeout: 10000 }).catch(() => {});
  let all = await prints(page);
  if (all.length <= before) {
    // Fallback: the stub did not catch the iframe's print(); read the print-dialog iframe itself.
    const frameText = await page.evaluate(() => {
      const frames = window.__printFrames ?? [];
      if (frames.length <= (window.__framesUsed ?? 0)) return null;
      window.__framesUsed = frames.length;
      const f = frames.at(-1);
      const b = f?.contentDocument?.body;
      return b ? (b.innerText || b.textContent || "") : null;
    });
    if (frameText !== null) {
      await page.evaluate((t) => window.__prints.push({ src: "iframe-content", text: t }), frameText);
      all = await prints(page);
    }
  }
  check(all.length > before, `${what}: the print dialog opened (${all.length - before} print call(s), via ${all.at(-1)?.src ?? "—"})`);
  const text = all.at(-1)?.text ?? "";
  for (const c of contains) check(text.includes(c), `${what}: the printed slip says “${c}”`);
  return text;
}

// ---------------------------------------------------------------- run
let api;
let web;
let browser;
const ids = {};
try {
  if (!existsSync(apiBin)) throw new Error(`API binary not found at ${apiBin} — run \`cargo build -p sutra-api\` or set SUTRA_API_BIN`);
  mkdirSync(SHOTS_DIR, { recursive: true });
  for (const f of readdirSync(SHOTS_DIR)) if (/^(\d\d|fail)-.*\.png$/.test(f)) unlinkSync(path.join(SHOTS_DIR, f));
  const existing = psql("SELECT count(*) FROM finance.charge");
  if (existing !== "0") throw new Error(`DATABASE_URL must be a fresh migrated database (found ${existing} charges)`);

  log(`API (dev auth), web on ${BASE}, shots → ${SHOTS_DIR}; today ${TODAY}`);
  api = await startApi();
  web = startWeb();

  // ------------------------------------------------------------ seed
  log("— seed");
  const admin = await apiLogin("admin@sutra.dev");
  const setup = await seedInstitution(admin);
  Object.assign(ids, setup);
  ids.aarav = seedStudent(setup, "Aarav", "Desai", "S2026-0001");
  psql(`UPDATE student.student SET subject_ref = '${STUDENT_SUBJECT}' WHERE id = '${ids.aarav}'`);
  ids.isha = seedStudent(setup, "Isha", "Kulkarni", "S2026-0002");
  Object.assign(ids, seedFinance(setup, ids.aarav, "S2026-0001"));
  log(`  ✓ institution ${setup.id}; Aarav ${ids.aarav}; CHG-000001 ₹1,25,000 published (due 15 Jul 2026)`);
  // 1C: an account placed explicitly in a statement group (the rest derive theirs).
  await attempt("ledger account 1500 in statement group fixed_asset", () =>
    admin.call("POST", `${F}/institutions/${setup.id}/ledger-accounts`, { code: "1500", name: "Laboratory equipment", accountType: "asset", statementGroup: "fixed_asset" }),
  );
  // 3A / 3D: Meera's staff record (linked to the dev faculty login) with an active engagement.
  ids.meera = await attempt("Meera Shah staff record (active, linked to the faculty login)", () =>
    admin.call("POST", `${P}/institutions/${setup.id}/staff`, { givenName: "Meera", familyName: "Shah", staffCategory: "teaching", workEmail: "meera.shah@sit.edu.in", initialStatus: "active", subjectRef: FACULTY_SUBJECT }),
  );
  if (ids.meera)
    await attempt("Meera's engagement (Assistant Professor) active", async () => {
      let e = await admin.call("POST", `${P}/staff/${ids.meera.id}/engagements`, { employmentType: "permanent", designation: "Assistant Professor", staffCategory: "teaching", startDate: "2025-07-01" });
      e = await admin.call("POST", `${P}/engagements/${e.id}/submit`, {}, e.version);
      if (e.pendingChange?.status === "pending_approval") e = await admin.call("POST", `${P}/engagements/${e.id}/decision`, { decision: "approve" }, e.version);
      if (e.status !== "active") e = await admin.call("POST", `${P}/engagements/${e.id}/activate`, {}, e.version);
      if (e.status !== "active") throw new Error(`engagement ${e.status}`);
      return e;
    });
  // 3B: an open requisition with one candidate.
  ids.requisition = await attempt("open requisition + candidate Kavya Nair", async () => {
    let r = await admin.call("POST", `${P}/institutions/${setup.id}/requisitions`, { title: "Assistant Professor (Computer Engineering)", designation: "Assistant Professor", staffCategory: "teaching", employmentType: "probation", openings: 1 });
    r = await admin.call("POST", `${P}/requisitions/${r.id}/submit`, {}, r.version);
    if (r.status !== "open" && r.version !== undefined) r = await admin.call("POST", `${P}/requisitions/${r.id}/decision`, { decision: "approve" }, r.version).catch(() => r);
    await admin.call("POST", `${P}/requisitions/${r.id}/candidates`, { givenName: "Kavya", familyName: "Nair", email: "kavya.nair@example.org" });
    return r;
  });

  await waitFor(`${BASE}/sign-in`, web);
  browser = await chromium.launch({ headless: !process.env.E2E_HEADED, executablePath: process.env.CHROMIUM_PATH || undefined, args: ["--no-sandbox", ...(process.env.E2E_CHROMIUM_ARGS ?? "").split(" ").filter(Boolean)] });
  const context = await browser.newContext({ viewport: { width: 1440, height: 960 }, locale: "en-IN", timezoneId: "Asia/Kolkata", acceptDownloads: true });
  // 3E: no printer is configured, so slips open the browser print dialog through a hidden iframe
  // (features/finance/printing/printer.ts `printPreview`). Stub `print` on the page and on every
  // iframe as it is attached, recording the slip text instead of blocking on a dialog.
  await context.addInitScript(() => {
    if (window.top !== window) return;
    window.__prints = [];
    const record = (w, src) => {
      let text = "";
      try {
        const b = w.document?.body;
        text = (b?.innerText || b?.textContent) ?? "";
      } catch {
        /* cross-origin: keep the call */
      }
      window.__prints.push({ src, text: text.slice(0, 4000) });
      window.__framesUsed = (window.__printFrames ?? []).length;
    };
    window.print = () => record(window, "window");
    window.__printFrames = [];
    const stubFrame = (node) => {
      if (!node || node.tagName !== "IFRAME") return;
      if (node.hasAttribute?.("data-print-preview")) window.__printFrames.push(node);
      try {
        const w = node.contentWindow;
        if (w) w.print = () => record(w, "iframe");
      } catch {
        /* not reachable */
      }
    };
    for (const name of ["appendChild", "insertBefore"]) {
      const orig = Node.prototype[name];
      Node.prototype[name] = function (child, ...rest) {
        const r = orig.call(this, child, ...rest);
        stubFrame(child);
        return r;
      };
    }
  });
  const page = await context.newPage();
  page.setDefaultTimeout(15000);
  watchExtensions(page);
  page.setDefaultNavigationTimeout(180000);
  for (const p of ["/sign-in", "/sign-in"]) await go(page, p);

  await signIn(page, "administrator");
  // Warm the routes (first compile in dev mode); errors before the steps are not attributed.
  for (const p of ["/finance/collections", "/finance/treasury", "/finance/ledger", "/finance/charges", "/finance/receivables", "/students/admissions", "/campus/facilities", "/campus/inventory", "/campus/transport", "/campus/procurement", "/people/attendance", "/people/recruitment"]) await go(page, p);
  page.setDefaultNavigationTimeout(30000);
  newProblems();

  // ============================================================ 3E fee-counter printing
  await step(page, "3E: Collections — cash counter payment → receipt RCT-000001", async () => {
    await go(page, "/finance/collections");
    await page.locator("[data-record-payment]").click();
    await apiAction(page, "POST", /\/finance\/student-accounts\/[^/]+\/payments$/, () =>
      fillDialogForm(page, "record-payment", { accountId: "Aarav", channel: "cash", amount: "25000.00", externalRef: "CTR-EXT-001" }),
    );
    await page.locator('[data-payment="PAY-000001"]').waitFor();
    check(((await row(page, "PAY-000001").textContent()) ?? "").includes("RCT-000001"), "receipt RCT-000001 issued");
    check((await row(page, "PAY-000001").locator("[data-print-receipt]").count()) === 1, "the payment row has a slip Print button");
  });

  await step(page, "3E: Counter printing — day close card, printer setup (Test print), receipt layout saved", async () => {
    await go(page, "/finance/collections");
    const close = await openTile(page, "data-counter-close", "Counter day close");
    await close.locator("[data-counter-count]").waitFor();
    const count = Number(((await close.locator("[data-counter-count]").textContent()) ?? "0").trim());
    check(count >= 1, `day close counts today's receipt (${count})`);
    check(((await close.locator("[data-counter-total]").textContent()) ?? "").includes("₹"), "day close total in rupees");
    let n = (await prints(page)).length;
    await close.locator("[data-counter-print]").click();
    await expectPrint(page, n, "day close print");

    const setupTile = await openTile(page, "data-printer-setup", "Receipt printer on this device");
    check(((await setupTile.textContent()) ?? "").includes("None — the print dialog is used"), "no printer chosen on this device");
    await setupTile.locator("select[data-printer-columns]").selectOption("32");
    n = (await prints(page)).length;
    await apiAction(page, "GET", /\/finance\/institutions\/[^/]+\/receipt-test-slip$/, () => setupTile.locator("[data-printer-test]").click(), { query: "columns=32" });
    await expectPrint(page, n, "test print");

    const settings = await openTile(page, "data-print-settings", "Printed receipt layout");
    await settings.locator("input[data-print-title]").waitFor();
    await settings.locator("input[data-print-title]").fill("SIT Fee Counter");
    await settings.locator("input[data-print-footer]").fill("Fees once paid are not refundable");
    await settings.locator('input[data-print-reg-label="0"]').fill("GSTIN");
    await settings.locator('input[data-print-reg-number="0"]').fill("27AAACS1234F1Z5");
    await apiAction(page, "PUT", /\/finance\/institutions\/[^/]+\/receipt-print-settings$/, () => settings.locator("[data-print-settings-save]").click());
    await go(page, "/finance/collections");
    const again = page.locator("[data-print-settings]");
    await again.locator("input[data-print-title]").waitFor();
    check((await again.locator("input[data-print-title]").inputValue()) === "SIT Fee Counter", "receipt layout title saved and read back");
    check((await again.locator('input[data-print-reg-number="0"]').inputValue()) === "27AAACS1234F1Z5", "GSTIN registration saved");
    await page.locator("[data-counter-close]").scrollIntoViewIfNeeded();
    await shot(page, "finance", "collections-counter-printing");
  });

  await step(page, "3E: receipt dialog Print + Duplicate copy, row Print (print dialog, no printer)", async () => {
    await go(page, "/finance/collections");
    await rowAction(page, "PAY-000001", "payment-receipt");
    const dlg = page.locator('[data-receipt="RCT-000001"]').first();
    await dlg.waitFor();
    let n = (await prints(page)).length;
    await apiAction(page, "GET", /\/finance\/receipts\/[^/]+\/slip$/, () => dlg.locator("[data-print-receipt]").click());
    await expectPrint(page, n, "receipt print", ["RCT-000001", "SIT Fee Counter"]);
    check((await page.locator("iframe[data-print-preview]").count()) >= 1, "slip text handed to the print-dialog iframe");
    await shot(page, "finance", "receipt-dialog-print");
    n = (await prints(page)).length;
    await apiAction(page, "GET", /\/finance\/receipts\/[^/]+\/slip$/, () => dlg.locator("[data-print-duplicate]").click(), { query: "copy=duplicate" });
    await expectPrint(page, n, "duplicate copy", ["DUPLICATE"]);
    await dlg.getByRole("button", { name: "Done" }).click();
    await dlg.waitFor({ state: "detached", timeout: 10000 }).catch(() => {});
    n = (await prints(page)).length;
    await apiAction(page, "GET", /\/finance\/receipts\/[^/]+\/slip$/, () => row(page, "PAY-000001").locator("[data-print-receipt]").click());
    await expectPrint(page, n, "row print", ["RCT-000001"]);
  });

  await step(page, "3E: Student 360 → Fees — staff reprint of the counter slip", async () => {
    await go(page, `/students/record/${ids.aarav}/fees`);
    const btn = page.locator("[data-print-slip] [data-print-receipt]").first();
    await btn.waitFor({ timeout: 20000 });
    const n = (await prints(page)).length;
    await apiAction(page, "GET", /\/finance\/receipts\/[^/]+\/slip$/, () => btn.click());
    await expectPrint(page, n, "Student 360 reprint", ["RCT-000001"]);
    await shot(page, "students", "360-fees-reprint");
  });

  // ============================================================ 1A / 1B bank statement import + reconciliation
  await step(page, "1A: Bank & treasury — bank account, bank CSV previewed and imported", async () => {
    await go(page, "/finance/treasury");
    await page.locator("[data-new-bank-account]").click();
    await apiAction(page, "POST", /\/finance\/institutions\/[^/]+\/bank-accounts$/, () =>
      fillDialogForm(page, "bank-account", { name: "Main collections", bankName: "HDFC Bank", accountNumberLast4: "1234", ifsc: "HDFC0001234" }),
    );
    await row(page, "Main collections").waitFor();
    const upload = page.locator("[data-import-statement-file]");
    await upload.waitFor();
    await page.waitForFunction(() => !document.querySelector("[data-import-statement-file]")?.hasAttribute("disabled"), null, { timeout: 15000 });
    await upload.click();
    const modal = page.locator("[data-statement-file]");
    await modal.waitFor();
    const d1 = daysFromNow(-6);
    const d2 = daysFromNow(-3);
    const csv = [
      "Date,Narration,Chq./Ref.No.,Value Dt,Withdrawal Amt.,Deposit Amt.,Closing Balance",
      `${dmy(d1)},NEFT-FEE COLLECTION AARAV DESAI,UTR26EXT0001,${dmy(d1)},,25000.00,25000.00`,
      `${dmy(d2)},BANK CHARGES,BC0001,${dmy(d2)},250.00,,24750.00`,
    ].join("\r\n");
    await apiAction(page, "POST", /\/finance\/bank-accounts\/[^/]+\/statements\/preview$/, () =>
      modal.locator("input[data-statement-file-input]").setInputFiles({ name: "hdfc-statement.csv", mimeType: "text/csv", buffer: Buffer.from(csv, "utf8") }),
    );
    const preview = modal.locator("[data-statement-preview]");
    await preview.waitFor();
    check((await preview.getAttribute("data-statement-preview")) === "csv", "detected as CSV");
    const fresh = ((await preview.locator("[data-preview-new]").textContent()) ?? "").trim();
    check(fresh === "2", `two new lines in the preview (${fresh})`);
    await shot(page, "finance", "treasury-statement-file-preview");
    await apiAction(page, "POST", /\/finance\/bank-accounts\/[^/]+\/statements$/, () => modal.locator("[data-statement-file-import]").click());
    await modal.waitFor({ state: "detached", timeout: 15000 });
    await page.locator('[data-statement-format="csv"]').first().waitFor({ timeout: 15000 });
    check(true, "imported statement listed with source CSV");
  });

  await step(page, "1B: Reconcile with bank — reconcile, BRS and history views", async () => {
    await go(page, "/finance/treasury");
    const t = await openTile(page, "data-bank-reconcile", "Reconcile with bank");
    await t.locator("[data-recon-date]").waitFor();
    check((await t.locator("[data-recon-finish]").count()) === 1, "Finish reconciliation button present");
    await shot(page, "finance", "treasury-reconcile");
    const view = t.locator("select[data-recon-view]");
    await apiAction(page, "GET", /\/finance\/bank-accounts\/[^/]+\/brs$/, () => view.selectOption("brs"));
    await t.locator("[data-brs]").waitFor();
    check(((await t.locator("[data-brs]").textContent()) ?? "").includes("Balance as per cash book"), "BRS lists the cash-book balance");
    await shot(page, "finance", "treasury-brs");
    await apiAction(page, "GET", /\/finance\/bank-accounts\/[^/]+\/reconciliations$/, () => view.selectOption("history"));
    await t.getByText("Reconciliations").first().waitFor().catch(() => {});
    await view.selectOption("reconcile");
    await settle(page);
  });

  // ============================================================ 1C financial statements
  await step(page, "1C: Ledger → Financial statements — I&E, balance sheet (statement groups), cash flow", async () => {
    await go(page, "/finance/ledger");
    const t = await openTile(page, "data-financial-statements", "Financial statements");
    await t.locator("[data-statement-row]").first().waitFor({ timeout: 15000 }).catch(() => {});
    check((await t.locator("[data-statement-row]").count()) > 0 || (await t.locator(".sb-empty").count()) > 0, "income & expenditure renders rows or its empty state");
    await shot(page, "finance", "ledger-income-expenditure");
    const kind = t.locator("select[data-statement-kind]");
    await apiAction(page, "GET", /\/financial-statements\/balance-sheet$/, () => kind.selectOption("balance-sheet"));
    await page.locator('[data-financial-statements="balance-sheet"]').waitFor();
    const bs = page.locator('[data-financial-statements="balance-sheet"]');
    await bs.locator("[data-statement-row]").first().waitFor({ timeout: 15000 });
    const rows = await bs.locator("[data-statement-row]").count();
    check(rows > 0, `balance sheet lists ${rows} row(s) grouped by statement group`);
    await shot(page, "finance", "ledger-balance-sheet");
    await apiAction(page, "GET", /\/financial-statements\/cash-flow$/, () => kind.selectOption("cash-flow"));
    await page.locator('[data-financial-statements="cash-flow"]').waitFor();
  });

  // ============================================================ 1E billing runs, 1D late fees + reminders
  await step(page, "1E: Fees & charges → Billing runs — plan created, Aarav enrolled, run previewed", async () => {
    await go(page, "/finance/charges");
    const t = await openTile(page, "data-billing-runs", "Billing runs");
    await fill(t, "Code", "HOSTEL-MONTHLY");
    await fill(t, "Name", "Hostel rent");
    await choose(field(t, "Fee head"), "TUITION");
    await fill(t, "Amount (₹)", "4500.00");
    await choose(field(t, "Every"), "monthly");
    await apiAction(page, "POST", /\/finance\/institutions\/[^/]+\/billing-plans$/, () => t.locator('[data-action="new-billing-plan"]').click());
    const plan = t.locator('[data-billing-plan="HOSTEL-MONTHLY"]');
    await plan.waitFor();
    await plan.click();
    const enrol = t.locator('[data-billing-enrolments="HOSTEL-MONTHLY"]');
    await enrol.waitFor();
    await choose(enrol.locator('[data-person-picker="student"] select').first(), "Aarav");
    await apiAction(page, "PUT", /\/finance\/billing-plans\/[^/]+\/enrolments$/, () => enrol.locator('[data-action="enrol"]').click());
    await row(page, "Aarav", enrol).waitFor({ timeout: 10000 });
    await shot(page, "finance", "charges-billing-plans");
    await tab(page, t, "data-billing-view", "run");
    await apiAction(page, "POST", /\/finance\/institutions\/[^/]+\/billing-runs\/preview$/, () => t.locator('[data-action="preview-billing"]').click());
    await t.locator('[data-billing-result="preview"]').waitFor();
    await shot(page, "finance", "charges-billing-run-preview");
    await tab(page, t, "data-billing-view", "schedules");
    await t.getByText("Automatic runs").first().waitFor().catch(() => {});
  });

  await step(page, "1D: Financial rules — late-fee rule and reminder schedule drafted → effective", async () => {
    await go(page, "/finance/charges");
    const rules = page.locator('[data-policy="rule"]');
    await rules.waitFor();
    await rules.locator('[data-action="new-rule"]').click();
    await apiAction(page, "POST", /\/finance\/institutions\/[^/]+\/finance-rules$/, () => fillDialog(page, "late-fee", { mode: "per_day", amount: "50.00", graceDays: "7" }));
    await rules.locator('[data-action="new-reminder-schedule"]').click();
    await apiAction(page, "POST", /\/finance\/institutions\/[^/]+\/finance-rules$/, () => fillDialog(page, "reminder-schedule", { beforeDays: "7, 1", afterDays: "1, 15, 30" }));
    // Draft → submit (dev auto-approval) → activate; activation is offered only once approved.
    for (const key of ["late_fee v1", "reminder_schedule v1"]) {
      await row(page, key, rules).waitFor();
      await apiAction(page, "POST", /\/finance\/finance-rules\/[^/]+\/submit$/, () => rowAction(page, key, "submit-rule", rules));
      await rowAction(page, key, "activate-rule", rules, { optional: true });
      await expectRowText(page, key, "Effective", rules);
    }
    await rules.scrollIntoViewIfNeeded();
    await shot(page, "finance", "charges-late-fee-rules");
  });

  await step(page, "1D: Receivables — pre-due reminders sent, late fees previewed (charged when due)", async () => {
    await go(page, "/finance/receivables");
    await apiAction(page, "POST", /\/finance\/institutions\/[^/]+\/fee-reminders$/, () => page.locator("[data-fee-reminders]").click());
    await apiAction(page, "POST", /\/finance\/institutions\/[^/]+\/late-fees\/preview$/, () => page.locator("[data-late-fee-preview]").click());
    const lf = page.locator('[data-late-fees="preview"]');
    await lf.waitFor();
    await shot(page, "finance", "receivables-late-fee-preview");
    const charge = lf.locator("[data-late-fee-run]");
    if (await charge.isVisible().catch(() => false)) {
      await apiAction(page, "POST", /\/finance\/institutions\/[^/]+\/late-fees$/, () => charge.click());
      await page.locator('[data-late-fees="run"]').waitFor();
      check((await page.locator('[data-late-fee-charge="CHG-000001"]').count()) === 1, "late fee charged on the overdue CHG-000001");
      await shot(page, "finance", "receivables-late-fees-charged");
    } else log("  (nothing late to charge)");
  });

  // ============================================================ 2A admissions funnel
  await step(page, "2A: Students → Admissions — Admissions funnel: enquiry recorded, funnel view", async () => {
    await go(page, "/students/admissions");
    const t = await openTile(page, "data-admission-funnel", "Admissions funnel");
    await fill(t, "First name", "Rohan");
    await fill(t, "Last name", "Patil");
    await fill(t, "Mobile", "+91 98200 12345");
    await fill(t, "E-mail", "rohan.patil@example.org");
    await choose(field(t, "Programme"), "Computer Science").catch(() => log("  (no programme option)"));
    await fill(t, "Intake (YYYY)", String(new Date().getFullYear() + 1));
    await fill(t, "Expected fee (₹)", "150000");
    await apiAction(page, "POST", /\/student\/institutions\/[^/]+\/admission-enquiries$/, () => t.locator('[data-action="new-enquiry"]').click());
    await t.locator("[data-enquiry]", { hasText: "Rohan Patil" }).first().waitFor();
    await shot(page, "admissions", "funnel-enquiries");
    await apiAction(page, "GET", /\/student\/institutions\/[^/]+\/admission-funnel$/, () => t.locator('[data-funnel-view="funnel"]').click());
    await t.locator("[data-funnel-bars], .sb-empty").first().waitFor().catch(() => {});
    await shot(page, "admissions", "funnel-stages");
  });

  // ============================================================ 2B service desk rules
  await step(page, "2B: Campus → Service desk — automation: SLA board, rule saved", async () => {
    await go(page, "/campus/facilities");
    const t = await openTile(page, "data-service-desk-rules", "Service desk automation");
    await shot(page, "campus", "service-desk-sla-board");
    await tab(page, t, "data-desk-tab", "rules");
    await fill(t, "Rule name", "Leaks are urgent");
    await fill(t, "Value", "leak");
    await apiAction(page, "POST", /\/campus\/institutions\/[^/]+\/service-desk\/rules$/, () => t.locator('[data-action="save-rule"]').click());
    await t.locator('[data-rule="Leaks are urgent"]').waitFor();
    await shot(page, "campus", "service-desk-rules");
  });

  // ============================================================ 2C inventory demand + lots / expiry
  await step(page, "2C: Campus → Inventory — Stock demand & expiry: expiry view, settings saved", async () => {
    await go(page, "/campus/inventory");
    const t = await openTile(page, "data-stock-demand", "Stock demand & expiry");
    await shot(page, "campus", "inventory-stock-demand");
    await tab(page, t, "data-demand-tab", "expiry");
    await tab(page, t, "data-demand-tab", "settings");
    await fill(t, "Safety days", "10");
    await apiAction(page, "PUT", /\/resources\/institutions\/[^/]+\/stock-demand\/settings$/, () => t.locator('[data-action="save-demand-settings"]').click());
    await shot(page, "campus", "inventory-demand-settings");
  });

  // ============================================================ 2D fleet maintenance
  await step(page, "2D: Campus → Transport — Fleet maintenance: due board, settings saved", async () => {
    await go(page, "/campus/transport");
    const t = await openTile(page, "data-fleet-maintenance", "Fleet maintenance");
    await shot(page, "campus", "transport-fleet-due");
    await tab(page, t, "data-fleet-tab", "settings");
    await fill(t, "Due soon (days ahead)", "21");
    await apiAction(page, "PUT", /\/campus\/institutions\/[^/]+\/fleet-settings$/, () => t.locator('[data-action="save-fleet-settings"]').click());
    await shot(page, "campus", "transport-fleet-settings");
  });

  // ============================================================ 3A duty roster
  await step(page, "3A: People → Attendance — Duty roster: shift planned", async () => {
    await go(page, "/people/attendance");
    const t = await openTile(page, "data-duty-roster", "Duty roster");
    await fill(t, "Duty", "Hostel night warden");
    await fill(t, "Where", "Boys hostel");
    await apiAction(page, "POST", /\/people\/institutions\/[^/]+\/roster\/shifts$/, () => t.locator('[data-action="add-shift"]').click());
    await t.getByText("Hostel night warden").first().waitFor();
    await shot(page, "people", "attendance-duty-roster");
  });

  // ============================================================ 3B hiring scorecards
  await step(page, "3B: People → Recruitment — Interview scorecards: compare, faculty rubric saved", async () => {
    await go(page, "/people/recruitment");
    const t = await openTile(page, "data-hiring-scorecards", "Interview scorecards");
    if (ids.requisition) await t.getByText("Kavya").first().waitFor({ timeout: 15000 });
    await shot(page, "people", "recruitment-scorecards-compare");
    await tab(page, t, "data-hiring-tab", "due");
    await tab(page, t, "data-hiring-tab", "rubric");
    check(!!ids.requisition, "a requisition is open (seeded)");
    await t.locator('[data-action="faculty-rubric"]').click();
    await apiAction(page, "PUT", /\/people\/requisitions\/[^/]+\/hiring-form$/, () => t.locator('[data-action="save-rubric"]').click());
    await shot(page, "people", "recruitment-scorecards-rubric");
  });

  // ============================================================ 3C contracts & MoUs, 3D expense claims (admin)
  await step(page, "3C: Campus → Procurement — Contracts and MoUs: contract added", async () => {
    await go(page, "/campus/procurement");
    const t = await openTile(page, "data-contracts", "Contracts and MoUs");
    await shot(page, "procurement", "contracts-overview");
    await tab(page, t, "data-contracts-tab", "contracts");
    await fill(t, "Title", "RO plant AMC");
    await fill(t, "Counterparty", "AquaPure Services");
    await fill(t, "Value (₹)", "48000");
    await fill(t, "Starts", TODAY);
    await fill(t, "First term ends", iso(daysFromNow(365)));
    await apiAction(page, "POST", /\/resources\/institutions\/[^/]+\/contracts$/, () => t.locator('[data-action="add-contract"]').click());
    await t.locator("[data-contract-detail]").first().waitFor();
    check(((await t.textContent()) ?? "").includes("RO plant AMC"), "contract listed and opened");
    await shot(page, "procurement", "contracts-added");
  });

  await step(page, "3D: Campus → Procurement — Expense claims (admin): overview, settings saved", async () => {
    await go(page, "/campus/procurement");
    const t = await openTile(page, "data-expenses", "Expense claims");
    await shot(page, "procurement", "expenses-overview");
    await tab(page, t, "data-expenses-tab", "settings");
    await fill(t, "Receipt needed above (₹)", "500");
    await apiAction(page, "PUT", /\/resources\/institutions\/[^/]+\/expense-settings$/, () => t.locator('[data-action="save-expense-settings"]').click());
    await shot(page, "procurement", "expenses-settings");
  });

  // ============================================================ 3D staff self-service (faculty)
  await signOut(page);
  await signIn(page, "faculty");
  await step(page, "3D: faculty My HR → Payslips — My expenses: expense saved, claimed, submitted", async () => {
    await go(page, "/leave/payslips");
    const t = await openTile(page, "data-my-expenses", "My expenses");
    check(!!ids.meera, "the faculty login has a staff record (seeded)");
    check((await t.getByText("Expenses aren’t available").count()) === 0, "My expenses is available for the linked staff record");
    await t.locator('[data-action="type-expense"]').click();
    const form = t.locator("[data-expense-form]");
    await form.waitFor();
    await fill(form, "Merchant", "MSRTC bus");
    await fill(form, "Amount (₹)", "120.00");
    // Local conveyance: Travel (TA) needs a receipt at any amount, so it could not be submitted.
    await choose(field(form, "Category"), "Local conveyance");
    await apiAction(page, "POST", /\/resources\/me\/expenses$/, () => form.locator('[data-action="save-expense"]').click());
    await t.getByText("MSRTC bus").first().waitFor();
    await shot(page, "my-hr", "payslips-my-expenses");
    // Claim it and submit (the submit button stays disabled while a policy blocker applies).
    const pick = t.locator("[data-pick]").first();
    if (await pick.isVisible().catch(() => false)) {
      await pick.check();
      await fill(t, "Purpose", "Paper evaluation at SPPU");
      await apiAction(page, "POST", /\/resources\/me\/expense-claims$/, () => t.locator('[data-action="create-claim"]').click());
      const submit = t.locator('[data-action="submit-claim"]');
      await submit.waitFor({ timeout: 10000 }).catch(() => {});
      if (await submit.isEnabled().catch(() => false)) {
        await apiAction(page, "POST", /\/resources\/me\/expense-claims\/[^/]+\/submit$/, () => submit.click());
        ids.claimSubmitted = true;
      } else log(`  (claim not submittable: ${((await t.locator("[data-submit-blockers]").textContent().catch(() => "")) ?? "").trim()})`);
      await shot(page, "my-hr", "payslips-my-expense-claim");
    }
  });

  await signOut(page);
  await signIn(page, "administrator");
  await step(page, "3D: Procurement → Expense claims → Approvals lists the submitted claim", async () => {
    await go(page, "/campus/procurement");
    const t = await openTile(page, "data-expenses", "Expense claims");
    await apiAction(page, "GET", /\/resources\/institutions\/[^/]+\/expense-claims$/, () => tab(page, t, "data-expenses-tab", "approvals"), { query: "status=submitted" });
    if (ids.claimSubmitted) {
      await t.getByText("Meera").first().waitFor({ timeout: 10000 });
      check(true, "Meera's submitted claim awaits approval");
    }
    await shot(page, "procurement", "expenses-approvals");
  });

  // ============================================================ 5C time tracking (screenshots 60–69)
  shotNo = Math.max(shotNo, 59);
  await step(page, "5C: People → Workload — Time tracking: a SERB grant with Meera as JRF", async () => {
    await go(page, "/people/workload");
    const t = await openTile(page, "data-time-tracking", "Time tracking");
    await fill(t, "Code", "SERB-CRG/2026/014");
    await fill(t, "Name", "Monsoon rainfall nowcasting");
    await fill(t, "Sanction order no.", "CRG/2026/000014");
    await fill(t, "Sanctioned amount (₹)", "3200000");
    await fill(t, "Manpower head (₹)", "1200000");
    await fill(t, "Starts", "2026-04-01");
    await fill(t, "Ends", "2029-03-31");
    await choose(field(t, "Add a member"), "Meera");
    await t.locator('[data-action="add-member"]').click();
    await apiAction(page, "POST", /\/people\/institutions\/[^/]+\/time-projects$/, () => t.locator('[data-action="create-time-project"]').click());
    await t.locator('[data-project-code="SERB-CRG/2026/014"]').first().waitFor();
    await shot(page, "people", "workload-time-projects");
  });

  await signOut(page);
  await signIn(page, "faculty");
  await step(page, "5C: faculty My HR → Workload — My time: 1h30 logged, week submitted", async () => {
    await go(page, "/leave/workload");
    const t = await openTile(page, "data-my-time", "My time");
    await t.locator('[data-week-project="SERB-CRG/2026/014"]').first().waitFor({ timeout: 15000 });
    await choose(field(t, "Project"), "SERB-CRG/2026/014");
    await fill(t, "Duration", "1h30");
    await fill(t, "Task", "Radar data cleaning");
    await apiAction(page, "POST", /\/people\/me\/time\/entries$/, () => t.locator('[data-action="add-entry"]').click());
    await t.getByText("Radar data cleaning").first().waitFor();
    await shot(page, "my-hr", "workload-my-time");
    await apiAction(page, "POST", /\/people\/me\/time\/timesheets$/, () => t.locator('[data-action="submit-week"]').first().click());
    await t.getByText("Awaiting approval").first().waitFor();
    ids.timesheetSubmitted = true;
    await shot(page, "my-hr", "workload-my-time-submitted");
  });

  await signOut(page);
  await signIn(page, "administrator");
  await step(page, "5C: People → Workload — Time tracking: week approved, effort report", async () => {
    await go(page, "/people/workload");
    const t = await openTile(page, "data-time-tracking", "Time tracking");
    await apiAction(page, "GET", /\/people\/institutions\/[^/]+\/timesheets$/, () => tab(page, t, "data-time-tab", "approvals"), { query: "status=submitted" });
    if (ids.timesheetSubmitted) {
      await t.getByText("Meera").first().waitFor({ timeout: 10000 });
      await shot(page, "people", "workload-time-approvals");
      await apiAction(page, "POST", /\/people\/timesheets\/[^/]+\/decision$/, () => t.locator('[data-action="approve-timesheet"]').first().click());
    }
    await apiAction(page, "GET", /\/people\/institutions\/[^/]+\/effort-report$/, () => tab(page, t, "data-time-tab", "effort"));
    await shot(page, "people", "workload-effort-report");
    await apiAction(page, "GET", /\/people\/institutions\/[^/]+\/time-rates$/, () => tab(page, t, "data-time-tab", "rates"));
    await shot(page, "people", "workload-time-rates");
  });

  const late = newProblems();
  if (late.length) failed.push(`errors outside the steps:\n    ${late.join("\n    ")}`);
  else log("  ✓ no page errors, console errors or 5xx answers outside the steps");
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
if (clientErrors.length) log(`${clientErrors.length} /api/ 4xx answer(s) recorded:\n  - ${clientErrors.join("\n  - ")}`);
if (failed.length) {
  console.error(`[e2e] ${failed.length} step(s) failed:\n  - ${failed.join("\n  - ")}`);
  process.exit(1);
}
log(`PASS — ${shotNo} screenshots in ${SHOTS_DIR}`);
process.exit(0);
