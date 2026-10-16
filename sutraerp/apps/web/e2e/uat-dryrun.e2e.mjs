#!/usr/bin/env node
/**
 * Part 2 · Stage 6 (S6-pilot) — UAT DRY RUN with SYNTHETIC data only. NOT user acceptance testing:
 * no real pilot user, institution or personal data is involved, and nothing here is an acceptance
 * result. It proves that one key UAT case per pilot role (docs/readiness/UAT_TEST_CASES.md) can be
 * executed end to end on the full demo stack, so a real UAT can start the day an institution
 * authorises a pilot (BLOCKED_EXTERNAL until then).
 *
 * Runs against the RUNNING compose stack of `.github/workflows/sutraerp-part1-demo.yml`, AFTER
 * `journey.e2e.mjs` (journey + persistence phases): it reuses the journey's synthetic institution
 * (ids + the Keycloak TOTP secrets from E2E_STATE) and signs every role in through the real flow
 * of the stack (Keycloak OIDC + TOTP, or the dev authenticator). Approval mode is the compose
 * default (human): every governed decision is made by an independent checker persona.
 *
 *   UAT-ADM-01  administrator        /students/record/:id        student record (Student 360) read
 *   UAT-FAC-01  faculty              /attendance                 session → mark → submit → finalised
 *   UAT-ACC-01  accounts staff       /finance/adjustments        maker drafts + submits a concession;
 *               (finance approver)                               the maker's own decision is refused
 *                                                                (403); the finance approver approves
 *                                                                it in the browser → posted
 *   UAT-EXM-01  examination staff    /examinations/malpractice   exam cell proposes a committee
 *               (controller of exams)                            decision; the proposer's own approval
 *                                                                is refused (403); the controller of
 *                                                                examinations approves it in the browser
 *   UAT-STU-01  student              /results, /fees             own published result + no dues
 *
 * Environment: E2E_BASE, E2E_AUTH (oidc | dev), DATABASE_URL (schema owner, psql on PATH),
 * E2E_STATE (journey state), E2E_SHOTS, E2E_UAT_RESULTS (results JSON; default
 * $E2E_SHOTS/uat-dryrun-results.json), CHROMIUM_PATH, E2E_HEADED=1.
 * Exit status: 0 when every case passed, 1 otherwise. `node e2e/uat-dryrun.e2e.mjs --report <json>`
 * prints the results JSON as a Markdown table (job summary).
 */
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";

if (process.argv[2] === "--report") {
  const R = JSON.parse(readFileSync(process.argv[3], "utf8"));
  const out = [
    "### UAT dry run (synthetic data — NOT user acceptance testing)",
    "",
    `Stack: ${R.base} · auth ${R.auth} · sha ${R.sha ?? "local"} · run ${R.runId ?? "local"}`,
    "",
    "| Case | Role (persona) | Screen / API | Result | ms |",
    "|---|---|---|---|---:|",
    ...R.cases.map((c) => `| ${c.id} | ${c.role} | ${c.via} | ${c.status === "pass" ? "✅ pass" : "❌ fail"} | ${c.ms} |`),
    "",
    ...R.cases.filter((c) => c.status !== "pass").map((c) => `- **${c.id}**: ${(c.evidence ?? []).filter((x) => x.startsWith("ERROR")).join(" ").slice(0, 600)}`),
    ...(R.observations?.length ? ["", "Observations (not failures):", ...R.observations.map((o) => `- ${o}`)] : []),
  ];
  console.log(out.join("\n"));
  process.exit(0);
}

const { chromium } = await import("playwright");
process.env.DATABASE_URL ??= "postgres://sutra:sutra@localhost:5432/sutra";
const { psql } = await import("./lib/harness.mjs");
const { AUTH, BASE, LOGINS, PASSWORD, login, browserSignIn, totpSecrets } = await import("./lib/auth.mjs");

const SHOTS = process.env.E2E_SHOTS ?? path.resolve("e2e-uat-shots");
const STATE = process.env.E2E_STATE ?? "/tmp/journey-state.json";
const RESULTS = process.env.E2E_UAT_RESULTS ?? path.join(SHOTS, "uat-dryrun-results.json");
const F = "/api/v1/finance";
const X = "/api/v1/exams";
mkdirSync(SHOTS, { recursive: true });

const log = (...a) => console.log("[uat-dryrun]", ...a);
const q = (v) => `'${String(v).replace(/'/g, "''")}'`;
if (!existsSync(STATE)) {
  console.error(`[uat-dryrun] no journey state at ${STATE}: run apps/web/e2e/journey.e2e.mjs first (it seeds the synthetic institution)`);
  process.exit(1);
}
const state = JSON.parse(readFileSync(STATE, "utf8"));
for (const [k, v] of Object.entries(state.totp ?? {})) totpSecrets.set(k, v);
const ids = state.ids ?? {};
const R = { kind: "synthetic dry run — not UAT", auth: AUTH, base: BASE, sha: process.env.GITHUB_SHA ?? null, runId: process.env.GITHUB_RUN_ID ?? null, startedAt: new Date().toISOString(), cases: [], observations: [] };
const save = () => {
  R.finishedAt = new Date().toISOString();
  writeFileSync(RESULTS, JSON.stringify(R, null, 2));
};

// ---------------------------------------------------------------- checks and evidence

let shotNo = 200;
async function snap(page, name) {
  shotNo++;
  const file = path.join(SHOTS, `${shotNo}-uat-${name}.png`);
  await page.waitForLoadState("networkidle").catch(() => {});
  await page.screenshot({ path: file, fullPage: true }).catch(() => {});
  return path.basename(file);
}
function check(cond, what, e) {
  if (!cond) throw new Error(`check failed: ${what}`);
  e?.(`✓ ${what}`);
}
function need(v, what) {
  if (v === undefined || v === null || v === "") throw new Error(`prerequisite missing: ${what} (the journey did not produce it)`);
  return v;
}
function db(e, label, sql, pred = (v) => v !== "") {
  const v = psql(sql);
  e(`DB ${label}: ${v.replace(/\n/g, " ; ").slice(0, 300)}`);
  if (!pred(v)) throw new Error(`DB check failed — ${label}: got “${v.slice(0, 300)}”`);
  return v;
}

const pages = {};
const serverErrors = [];
const forbiddenSeen = new Map();
function watch(page, role) {
  page.on("response", async (r) => {
    const u = new URL(r.url());
    if (!u.pathname.startsWith("/api/")) return;
    if (r.status() >= 500) serverErrors.push(`${role}: ${r.request().method()} ${u.pathname} → ${r.status()} ${(await r.text().catch(() => "")).slice(0, 200)}`);
    else if (r.status() === 403) forbiddenSeen.set(`${role} ${r.request().method()} ${u.pathname.replace(/[0-9a-f]{8}-[0-9a-f-]{27}/g, "{id}")}`, true);
  });
  page.on("pageerror", (err) => serverErrors.push(`${role}: page error ${err.message}`));
}
function noServerErrors(e) {
  const errs = serverErrors.splice(0);
  if (errs.length) throw new Error(`server/page errors during the case:\n  ${errs.join("\n  ")}`);
  e("✓ no 5xx / page errors in the browser");
}

async function uatCase(id, role, title, via, fn) {
  const evidence = [];
  const e = (s) => {
    evidence.push(s);
    log(`   · ${s}`);
  };
  log(`— ${id} [${role}] ${title}`);
  const t0 = Date.now();
  let status = "pass";
  try {
    await fn(e);
  } catch (err) {
    status = "fail";
    const msg = err instanceof Error ? err.message : String(err);
    evidence.push(`ERROR: ${msg.slice(0, 1200)}`);
    console.error(`[uat-dryrun]   ✗ ${id}: ${msg}`);
    for (const [r, p] of Object.entries(pages)) {
      const f = await snap(p, `fail-${id}-${r}`);
      evidence.push(`screenshot ${f}`);
      await p.keyboard.press("Escape").catch(() => {});
    }
  }
  R.cases.push({ id, role, title, via, status, ms: Date.now() - t0, evidence });
  save();
}

/** Runs a browser `action` and returns the matching API response the page sent ({ status, body }). */
async function ui(page, method, pathRe, action, e) {
  const [resp] = await Promise.all([
    page.waitForResponse((r) => r.request().method() === method && pathRe.test(new URL(r.url()).pathname), { timeout: 30000 }),
    action(),
  ]);
  const body = await resp.json().catch(() => null);
  e(`browser → ${method} ${new URL(resp.url()).pathname} → ${resp.status()}${body?.status ? ` (status ${body.status})` : ""}`);
  return { status: resp.status(), body };
}

// ---------------------------------------------------------------- browser helpers (selectors of the CI e2e suites)

async function settle(page) {
  await page.waitForLoadState("networkidle").catch(() => {});
  await page.waitForTimeout(250);
}
async function go(page, p) {
  await page.goto(`${BASE}${p}`);
  await settle(page);
}
const row = (scope, text) => scope.locator("tr, .sb-lrow", { hasText: text }).first();

/** Row primary pill `[data-action=id]` or kebab `[data-row-menu]` → `[data-menu-item=id]`. */
async function act(page, container, id, timeout = 15000) {
  const deadline = Date.now() + timeout;
  const direct = container.locator(`[data-action="${id}"]`).first();
  while (Date.now() < deadline) {
    if (await direct.isVisible().catch(() => false)) {
      await direct.click();
      await settle(page);
      return;
    }
    const menus = container.locator("[data-row-menu]");
    const n = await menus.count().catch(() => 0);
    for (let i = 0; i < n; i++) {
      const m = menus.nth(i);
      if (!(await m.isVisible().catch(() => false))) continue;
      if ((await m.getAttribute("aria-expanded")) !== "true") await m.click();
      const item = container.locator(`[data-menu-item="${id}"]`).first();
      if (await item.isVisible({ timeout: 1000 }).catch(() => false)) {
        await item.click();
        await settle(page);
        return;
      }
      await page.keyboard.press("Escape").catch(() => {});
    }
    await page.waitForTimeout(250);
  }
  throw new Error(`action ${id} not found (pill or kebab)`);
}

/** True when `container` offers the row action `id` (pill or kebab item). */
async function offers(page, container, id) {
  if (await container.locator(`[data-action="${id}"]`).first().isVisible().catch(() => false)) return true;
  const menus = container.locator("[data-row-menu]");
  const n = await menus.count().catch(() => 0);
  for (let i = 0; i < n; i++) {
    const m = menus.nth(i);
    if (!(await m.isVisible().catch(() => false))) continue;
    if ((await m.getAttribute("aria-expanded")) !== "true") await m.click();
    const found = await container.locator(`[data-menu-item="${id}"]`).first().isVisible({ timeout: 1000 }).catch(() => false);
    await page.keyboard.press("Escape").catch(() => {});
    if (found) return true;
  }
  return false;
}

/** Fills `[data-form=name]` (inputs, selects by value or label) and submits. */
async function fillForm(page, name, values = {}) {
  const form = page.locator(`[data-form="${name}"]`).last();
  await form.waitFor({ state: "visible", timeout: 15000 });
  for (const [k, v] of Object.entries(values)) {
    const el = form.locator(`[name="${k}"]`).first();
    const tag = await el.evaluate((n) => n.tagName);
    if (tag === "SELECT") {
      const opts = await el.locator("option").evaluateAll((os) => os.map((o) => ({ value: o.value, label: o.textContent ?? "" })));
      const hit = opts.find((o) => o.value === v) ?? opts.find((o) => o.label.includes(v));
      if (!hit) throw new Error(`form ${name}: no option “${v}” in ${k} (${opts.map((o) => o.label).join(" | ")})`);
      await el.selectOption(hit.value);
    } else await el.fill(String(v));
  }
  await form.locator('button[type="submit"]').click();
  await settle(page);
  if (await form.isVisible().catch(() => false)) {
    const errs = (await form.locator('[role="alert"], .sb-field-error').allTextContents().catch(() => [])).filter(Boolean);
    if (errs.length) throw new Error(`form ${name}: ${errs.join(" | ")}`);
  }
}

/**
 * Browser sign-in of any persona. OIDC: the Keycloak login page (+ TOTP) for every persona. Dev
 * authenticator: the role card for administrator / faculty / student; the checker personas have
 * no card on the dev sign-in page, so the page itself makes the same POST /api/v1/auth/login the
 * SPA makes, then reloads (the SPA restores the session from GET /api/v1/auth/session).
 */
async function signInAs(page, role) {
  if (AUTH === "oidc" || ["administrator", "faculty", "student"].includes(role)) return browserSignIn(page, role);
  await page.goto(`${BASE}/sign-in`);
  await settle(page);
  const status = await page.evaluate(
    async ({ loginId, password }) => {
      const c = await fetch("/api/v1/auth/csrf", { credentials: "include" });
      const { csrfToken } = await c.json();
      const r = await fetch("/api/v1/auth/login", { method: "POST", credentials: "include", headers: { "content-type": "application/json", "x-csrf-token": csrfToken }, body: JSON.stringify({ loginId, password }) });
      return r.status;
    },
    { loginId: LOGINS[role], password: PASSWORD },
  );
  if (status !== 200) throw new Error(`dev sign-in of ${role} from the page → ${status}`);
  await go(page, "/");
  if (page.url().includes("/sign-in")) throw new Error(`${role}: still on the sign-in page after login`);
}

async function newPage(browser, role) {
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 960 }, locale: "en-IN", timezoneId: "Asia/Kolkata" });
  const page = await ctx.newPage();
  page.setDefaultTimeout(20000);
  page.setDefaultNavigationTimeout(60000);
  watch(page, role);
  pages[role] = page;
  await signInAs(page, role);
  return page;
}

// ---------------------------------------------------------------- run

let browser;
let exitCode = 0;
try {
  log(`synthetic dry run (not UAT) · auth ${AUTH} · ${BASE} · state ${STATE}`);
  need(ids.id, "journey institution");
  const admin = await login("administrator");
  const financeApprover = await login("finance_approver");
  const examController = await login("exam_controller");
  check(financeApprover.subject !== admin.subject && examController.subject !== admin.subject, "checker personas are different people from the administrator (maker)");
  browser = await chromium.launch({ headless: !process.env.E2E_HEADED, executablePath: process.env.CHROMIUM_PATH || undefined, args: ["--no-sandbox"] });

  // ============================================================ administrator
  await uatCase("UAT-ADM-01", "administrator", "Student record: open a student's Student 360 and confirm the active enrolment", "browser /students/record/:id/enrolment · GET /api/v1/student/students/{id}", async (e) => {
    const sid = need(ids.kiran?.id, "seed student Kiran");
    const page = pages.administrator ?? (await newPage(browser, "administrator"));
    const r = await ui(page, "GET", new RegExp(`/student/students/${sid}$`), () => go(page, `/students/record/${sid}/enrolment`), e);
    check(r.status === 200 && r.body?.id === sid, "Student 360 read → 200", e);
    await page.locator('[data-context-status="active"]').first().waitFor();
    check((await page.getByText("Kiran").count()) > 0, "the student's name is shown", e);
    e(`admin sees the active context (${await snap(page, "adm-student360")})`);
    noServerErrors(e);
    db(e, "student + context", `SELECT s.status || '|' || c.status FROM student.student s JOIN student.student_context c ON c.student_id = s.id WHERE s.id = ${q(sid)} AND c.institution_id = ${q(ids.id)}`, (v) => v === "active|active");
  });

  // ============================================================ faculty
  await uatCase("UAT-FAC-01", "faculty", "Attendance: schedule a session, mark one student absent, submit → finalised", "browser /attendance · POST /api/v1/academic/sections/{id}/sessions, …/start, …/finalise-attendance", async (e) => {
    need(ids.isha?.id, "seed student Isha");
    const page = pages.faculty ?? (await newPage(browser, "faculty"));
    await go(page, "/attendance");
    let r = await ui(page, "POST", /\/academic\/sections\/[^/]+\/sessions$/, async () => {
      await page.locator("[data-new-session]").first().click();
      await fillForm(page, "new-session", { startsAt: "14:00", endsAt: "15:00", topic: "UAT dry run — arrays" });
    }, e);
    check(r.status === 201 && r.body.sectionId === ids.sections?.CS101, "session scheduled on CS101-T1 / A", e);
    const sessionId = r.body.id;
    r = await ui(page, "POST", /\/academic\/sessions\/[^/]+\/start$/, () => page.locator("[data-session-start]").click(), e);
    check(r.status === 200, `session started (${r.body?.status})`, e);
    const tiles = page.locator("[data-attendance-grid] [data-student]");
    await tiles.first().waitFor();
    const names = await tiles.allTextContents();
    const ishaIdx = names.findIndex((t) => t.includes("Isha"));
    check(ishaIdx >= 0, `Isha in the quick grid (${names.length} students)`, e);
    await tiles.nth(ishaIdx).click();
    check((await tiles.nth(ishaIdx).getAttribute("data-mark")) === "absent", "one tap marks Isha absent", e);
    r = await ui(page, "POST", /\/academic\/sessions\/[^/]+\/finalise-attendance$/, async () => {
      await page.locator("[data-submit-attendance]").first().click();
      await fillForm(page, "attendance-submit", {});
    }, e);
    check(r.status === 200 && r.body.attendanceStatus === "finalised", "attendance finalised", e);
    e(`faculty sees the finalised session (${await snap(page, "fac-attendance")})`);
    noServerErrors(e);
    db(e, "attendance records", `SELECT string_agg(mark, ',' ORDER BY mark) FROM academic.attendance_record WHERE session_id = ${q(sessionId)} AND student_id = ${q(ids.isha.id)}`, (v) => v === "absent");
  });

  // ============================================================ accounts staff (maker) + finance approver (checker)
  await uatCase("UAT-ACC-01", "accounts staff → finance approver", "Fee concession: drafted and submitted by the accounts maker, refused to the maker (403), approved by the finance approver in the browser → posted", "browser /finance/adjustments · POST /api/v1/finance/charges/{id}/adjustments, /adjustments/{id}/submit, /adjustments/{id}/decision", async (e) => {
    const chargeId = need(ids.kiranChargeId, "Kiran's charge (journey step 4)");
    const before = db(e, "charge outstanding before", `SELECT outstanding_amount FROM finance.charge WHERE id = ${q(chargeId)}`);
    const page = pages.administrator ?? (await newPage(browser, "administrator"));
    await go(page, "/finance/adjustments");
    let r = await ui(page, "POST", /\/finance\/charges\/[^/]+\/adjustments$/, async () => {
      await page.locator('[data-action="new-adjustment"]').click();
      await fillForm(page, "adjustment", { chargeId, kind: "concession", amount: "2000.00", reason: "UAT dry run (synthetic): sibling concession" });
    }, e);
    check(r.status === 201 && r.body.status === "draft", `adjustment ${r.body?.adjustmentNumber} drafted`, e);
    const adj = r.body;
    const tileA = page.locator("[data-adjustments]");
    r = await ui(page, "POST", /\/finance\/adjustments\/[^/]+\/submit$/, () => act(page, row(tileA, adj.adjustmentNumber), "submit-adjustment"), e);
    check(r.status === 200 && r.body.status === "pending_approval", "submitted — pending approval", e);
    db(e, "charge untouched while pending", `SELECT outstanding_amount FROM finance.charge WHERE id = ${q(chargeId)}`, (v) => v === before);
    if (await offers(page, row(tileA, adj.adjustmentNumber), "approve-adjustment"))
      R.observations.push("UAT-ACC-01: the Adjustments screen offers Approve to the maker of a pending adjustment (the API refuses it with 403; UI-only issue, see PILOT_DEFECT_REGISTER.md PDEF-SYN-002)");
    const pending = await admin.get(`${F}/adjustments/${adj.id}`);
    const self = await admin.raw("POST", `${F}/adjustments/${adj.id}/decision`, { decision: "approve", reason: "maker approving own request (must be refused)" }, pending.version);
    e(`maker's own decision → ${self.status}`);
    check(self.status === 403, "the maker cannot approve their own concession (403)", e);
    const fp = pages.finance_approver ?? (await newPage(browser, "finance_approver"));
    await go(fp, "/finance/adjustments");
    const tileB = fp.locator("[data-adjustments]");
    await row(tileB, adj.adjustmentNumber).waitFor();
    r = await ui(fp, "POST", /\/finance\/adjustments\/[^/]+\/decision$/, () => act(fp, row(tileB, adj.adjustmentNumber), "approve-adjustment"), e);
    check(r.status === 200 && r.body.status === "posted", "finance approver approves → posted", e);
    e(`finance approver sees the posted adjustment (${await snap(fp, "acc-adjustment-posted")})`);
    noServerErrors(e);
    db(e, "decided by the finance approver", `SELECT status || '|' || coalesce(decided_by, '') || '|' || coalesce(created_by, '') FROM finance.adjustment WHERE id = ${q(adj.id)}`, (v) => v === `posted|${financeApprover.subject}|${admin.subject}`);
    db(e, "charge reduced by ₹2,000", `SELECT outstanding_amount = ${before}::numeric - 2000 FROM finance.charge WHERE id = ${q(chargeId)}`, (v) => v === "t");
    db(e, "adjustment journal balanced", `SELECT count(*) || '|' || (coalesce(sum(total_debit), 0) = coalesce(sum(total_credit), 0)) FROM finance.journal_entry WHERE source_id = ${q(adj.id)}`, (v) => /^[1-9]\d*\|t$/.test(v));
  });

  // ============================================================ examination staff: exam cell (maker) + controller of examinations (checker)
  await uatCase("UAT-EXM-01", "examination staff → controller of examinations", "Malpractice: the exam cell proposes a committee decision (warning); the proposer's own approval is refused (403); the controller of examinations approves it in the browser", "browser /examinations/malpractice · POST /api/v1/exams/exam-sessions/{id}/malpractice-cases, /malpractice-cases/{id}/investigate, /decision, /approval", async (e) => {
    const sessionId = need(ids.sessionId, "exam session");
    const regId = db(
      e,
      "a confirmed registration of the session (Isha, else Aarav)",
      `SELECT id FROM exams.exam_registration WHERE session_id = ${q(sessionId)} AND student_id IN (${q(ids.isha?.id ?? "")}, ${q(ids.aarav?.id ?? "")}) ORDER BY (student_id = ${q(ids.isha?.id ?? "")}) DESC LIMIT 1`,
    );
    let c = await admin.call("POST", `${X}/exam-sessions/${sessionId}/malpractice-cases`, { registrationId: regId, paperId: need(ids.paperId, "paper"), allegation: "UAT dry run (synthetic): unauthorised notes found during the sitting" });
    e(`exam cell reported ${c.caseNumber} (${c.status})`);
    c = await admin.call("POST", `${X}/malpractice-cases/${c.id}/investigate`, {}, c.version);
    c = await admin.call("POST", `${X}/malpractice-cases/${c.id}/decision`, { penalty: "warning", reason: "UAT dry run: first offence, written warning" }, c.version);
    check(c.status === "under_investigation" && c.pending, `committee decision proposed — awaiting approval (${c.status}, pending ${c.pending?.kind ?? "none"})`, e);
    const self = await admin.raw("POST", `${X}/malpractice-cases/${c.id}/approval`, { decision: "approve", reason: "proposer approving own decision (must be refused)" }, c.version);
    e(`proposer's own approval → ${self.status} ${self.json?.detail ?? ""}`);
    check(self.status === 403, "the proposer cannot approve their own committee decision (403, maker ≠ checker)", e);
    db(e, "case still awaiting approval", `SELECT status || '|' || coalesce(proposal_kind, '') FROM exams.malpractice_case WHERE id = ${q(c.id)}`, (v) => v === "under_investigation|decision");
    const xp = pages.exam_controller ?? (await newPage(browser, "exam_controller"));
    await go(xp, "/examinations/malpractice");
    const main = xp.locator("main");
    await row(main, c.caseNumber).waitFor();
    const r = await ui(xp, "POST", /\/exams\/malpractice-cases\/[^/]+\/approval$/, () => act(xp, row(main, c.caseNumber), "case-approve"), e);
    check(r.status === 200 && r.body.status === "decided", `controller of examinations approves → ${r.body?.status}`, e);
    e(`controller sees the decided case (${await snap(xp, "exm-malpractice-decided")})`);
    noServerErrors(e);
    db(e, "case decided, warning", `SELECT status || '|' || coalesce(penalty, '') FROM exams.malpractice_case WHERE id = ${q(c.id)}`, (v) => v === "decided|warning");
    db(e, "approval audited by the controller", `SELECT count(*) FROM platform.audit_log WHERE target_id = ${q(c.id)} AND actor_id = ${q(examController.subject)}`, (v) => Number(v) >= 1);
  });

  // ============================================================ student
  await uatCase("UAT-STU-01", "student", "Self-service: own published result and fee status", "browser /results, /fees · GET /api/v1/exams/me/results, /api/v1/finance/me/accounts", async (e) => {
    const page = pages.student ?? (await newPage(browser, "student"));
    let r = await ui(page, "GET", /\/exams\/me\/results$/, () => go(page, "/results"), e);
    check(r.status === 200 && (r.body?.items ?? []).some((x) => x.status === "published"), "own published result returned", e);
    await page.locator("[data-result]").first().waitFor();
    e(`student Results screen (${await snap(page, "stu-results")})`);
    r = await ui(page, "GET", /\/finance\/me\/accounts$/, () => go(page, "/fees"), e);
    check(r.status === 200, "own fee account read → 200", e);
    const none = await page.locator("[data-outstanding-none]").count();
    const dues = await page.locator("[data-outstanding]").count();
    check(none + dues > 0, `fee status shown (${none ? "no dues" : `${dues} due item(s)`})`, e);
    e(`student Fees screen (${await snap(page, "stu-fees")})`);
    noServerErrors(e);
  });

  for (const k of forbiddenSeen.keys()) R.observations.push(`403 answered to the browser (screen degraded, not a failure): ${k}`);
} catch (err) {
  R.cases.push({ id: "setup", role: "", title: "setup", via: "", status: "fail", ms: 0, evidence: [String(err instanceof Error ? err.stack : err).slice(0, 1500)] });
  console.error("[uat-dryrun] FATAL", err);
} finally {
  save();
  await browser?.close().catch(() => {});
}

const failedCases = R.cases.filter((c) => c.status !== "pass");
log(`${R.cases.length} case(s): ${R.cases.length - failedCases.length} pass, ${failedCases.length} fail · results ${RESULTS}`);
for (const c of failedCases) log(`  FAIL ${c.id}: ${(c.evidence ?? []).filter((x) => x.startsWith("ERROR")).join(" ")}`);
if (failedCases.length || R.cases.length < 5) exitCode = 1;
process.exit(exitCode);
