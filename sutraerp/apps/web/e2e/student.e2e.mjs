#!/usr/bin/env node
/**
 * Batch 3 (Student lifecycle) click-through in a real Chromium, the way staff and students use it.
 *
 *   pnpm e2e:student          (from sutraerp/; not part of `pnpm -r test`)
 *
 * Starts the API binary (dev auth, PostgreSQL stores) and the web app in API mode (Vite :5173 with
 * the `/api` proxy, or `vite preview` :3000 with E2E_WEB=preview), seeds a fresh, migrated
 * database through the API and walks every Batch 3 screen:
 *
 *   seed   an institution taken to `active` through the Batch 2 endpoints (profile → configure →
 *          org blueprint + root unit → campus → structure/programme/curriculum → calendar →
 *          submit), plus two students inserted into the foundation tables (completion / exit);
 *   admin  Admissions (application → review → offer → acceptance → documents → verification →
 *          approval → financial clearance → confirmation → student creation; a second application
 *          rejected) → Student 360 enrolment → directory → overview / documents (certificate
 *          request issued) → registration window + progression policy → guardians (register, link,
 *          verify, activate, emergency contact, pickup) → scholarships (definition, policy,
 *          determination, recommendation) → completion (eligibility → audit → clearance →
 *          approval → award → finalise → exit) → alumni → withdrawal exit with clearance;
 *   student  /registration (self-registration, add + drop, submit) and /documents (certificate
 *          request + cancel); faculty  Student 360 read-only.
 *
 * A full-page PNG (1280×800 viewport) is written at each step to $E2E_SHOTS (default
 * /workspace/work/out/b3-shots, else ./e2e-shots). Every console error, page error and failed /api
 * response fails the run, as do empty states where data should be and buttons that change nothing.
 *
 * Environment (all optional):
 *   DATABASE_URL     PostgreSQL, migrated and FRESH (default postgres://sutra:sutra@localhost:5432/sutra_e2e)
 *   SUTRA_API_BIN    API binary — default $CARGO_TARGET_DIR/debug/sutra-api or target/debug/sutra-api
 *   E2E_WEB          dev (default) | preview
 *   E2E_API_PORT     default 8080
 *   E2E_SHOTS        screenshot directory
 *   CHROMIUM_PATH    Chromium/Chrome executable (default: Playwright's bundled Chromium)
 *   E2E_HEADED=1     show the browser
 * Requires `psql` on PATH (student seed rows).
 */
import { spawn, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, readdirSync, unlinkSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright";

const here = path.dirname(fileURLToPath(import.meta.url));
const webDir = path.resolve(here, "..");
const root = path.resolve(webDir, "../..");
const DATABASE_URL = process.env.DATABASE_URL ?? "postgres://sutra:sutra@localhost:5432/sutra_e2e";
const WEB_MODE = process.env.E2E_WEB === "preview" ? "preview" : "dev";
const WEB_PORT = WEB_MODE === "preview" ? 3000 : 5173;
const API_PORT = Number(process.env.E2E_API_PORT ?? 8080);
const BASE = `http://localhost:${WEB_PORT}`;
const API = `http://127.0.0.1:${API_PORT}`;
const SHOTS = process.env.E2E_SHOTS ?? (existsSync("/workspace/work") ? "/workspace/work/out/b3-shots" : path.join(webDir, "e2e-shots"));
const TENANT = process.env.SUTRA_DEV_TENANT_ID ?? "0192f5a0-7c3e-7000-8000-000000000001";
const STUDENT_SUBJECT = "dev|aarav.desai@sutra.dev";
const apiBin =
  process.env.SUTRA_API_BIN ?? path.join(process.env.CARGO_TARGET_DIR ?? path.join(root, "target"), "debug", "sutra-api");

const children = new Set();
const failed = [];
const log = (...a) => console.log("[e2e]", ...a);

function check(cond, what) {
  if (cond) log(`  ✓ ${what}`);
  else throw new Error(`check failed: ${what}`);
}

// ---------------------------------------------------------------- processes

function start(cmd, args, opts, name) {
  const child = spawn(cmd, args, { ...opts, stdio: ["ignore", "pipe", "pipe"], detached: true });
  children.add(child);
  const buf = [];
  const keep = (d) => {
    buf.push(String(d));
    if (buf.length > 300) buf.shift();
  };
  child.stdout.on("data", keep);
  child.stderr.on("data", keep);
  child.on("exit", () => children.delete(child));
  child.tail = (n = 40) => buf.join("").split("\n").slice(-n).join("\n");
  child.label = name;
  return child;
}

async function stop(child) {
  if (!child || child.exitCode !== null) return;
  const exited = new Promise((r) => child.once("exit", r));
  try {
    process.kill(-child.pid, "SIGTERM");
  } catch {
    child.kill("SIGTERM");
  }
  await Promise.race([exited, new Promise((r) => setTimeout(r, 5000))]);
  if (child.exitCode === null) {
    try {
      process.kill(-child.pid, "SIGKILL");
    } catch {
      /* gone */
    }
  }
}

async function waitFor(url, child, ms = 90000) {
  const until = Date.now() + ms;
  while (Date.now() < until) {
    if (child.exitCode !== null) throw new Error(`${child.label} exited:\n${child.tail()}`);
    try {
      const r = await fetch(url);
      if (r.status < 500) return;
    } catch {
      /* not yet */
    }
    await new Promise((r) => setTimeout(r, 300));
  }
  throw new Error(`${url} did not come up:\n${child.tail()}`);
}

async function startApi() {
  const env = {
    ...process.env,
    SUTRA_ENV: "development",
    SUTRA_HTTP_ADDR: `127.0.0.1:${API_PORT}`,
    SUTRA_LOG_FORMAT: "pretty",
    RUST_LOG: process.env.RUST_LOG ?? "warn",
    DATABASE_URL,
    SUTRA_AUTH_MODE: "dev",
    SUTRA_DEV_MFA: "false",
    SUTRA_DEV_TENANT_ID: TENANT,
    SUTRA_SESSION_COOKIE_SECURE: "false",
    SUTRA_OUTBOX_RELAY: "off",
    SUTRA_CACHE: "memory",
    SUTRA_INBOX_CONSUMER: "false",
    SUTRA_WEBAUTHN_RP_ID: "localhost",
    SUTRA_WEBAUTHN_ORIGINS: "http://localhost:3000,http://localhost:5173",
  };
  const api = start(apiBin, ["serve"], { env }, "api");
  await waitFor(`${API}/health/live`, api);
  return api;
}

function startWeb() {
  const env = { ...process.env, VITE_AUTH_MODE: "api", SUTRA_API_URL: `http://localhost:${API_PORT}` };
  const args =
    WEB_MODE === "preview"
      ? ["exec", "vite", "preview", "--outDir", "build/client", "--port", String(WEB_PORT), "--strictPort"]
      : ["exec", "react-router", "dev", "--port", String(WEB_PORT), "--strictPort"];
  return start("pnpm", args, { cwd: webDir, env }, "web");
}

// ---------------------------------------------------------------- API seeding

/** A signed-in API client (dev auth) that keeps the session cookie and CSRF token. */
async function apiLogin(loginId) {
  const cookieOf = (r) => (r.headers.getSetCookie?.() ?? []).map((c) => c.split(";")[0]).join("; ");
  let r = await fetch(`${API}/api/v1/auth/csrf`);
  let cookie = cookieOf(r);
  let csrf = (await r.json()).csrfToken;
  r = await fetch(`${API}/api/v1/auth/login`, {
    method: "POST",
    headers: { "content-type": "application/json", cookie, "x-csrf-token": csrf },
    body: JSON.stringify({ loginId, password: "sutra" }),
  });
  const body = await r.json();
  if (r.status !== 200) throw new Error(`login ${loginId}: ${r.status} ${JSON.stringify(body)}`);
  cookie = cookieOf(r) || cookie;
  csrf = body.csrfToken;
  const call = async (method, p, payload, version) => {
    const headers = { cookie };
    if (method !== "GET") {
      headers["content-type"] = "application/json";
      headers["x-csrf-token"] = csrf;
      headers["idempotency-key"] = crypto.randomUUID();
      if (version !== undefined && version !== null) headers["if-match"] = `"${version}"`;
    }
    const res = await fetch(`${API}${p}`, { method, headers, body: method === "GET" ? undefined : JSON.stringify(payload ?? {}) });
    const text = await res.text();
    let json;
    try {
      json = text ? JSON.parse(text) : null;
    } catch {
      json = text;
    }
    if (res.status >= 400) throw new Error(`${method} ${p} → ${res.status} ${text.slice(0, 400)}`);
    return json;
  };
  return { call, get: (p) => call("GET", p) };
}

const I = "/api/v1/institution";

/** Institution taken to `active` through the Batch 2 endpoints. Returns ids the walk needs. */
async function seedInstitution(admin) {
  const suffix = Date.now().toString(36).toUpperCase().slice(-5);
  let inst = await admin.call("POST", `${I}/institutions`, { code: `SIT-${suffix}`, name: "Sahyadri Institute of Technology" });
  const id = inst.id;
  inst = await admin.call(
    "PUT",
    `${I}/institutions/${id}/profile`,
    {
      shortName: "SIT",
      institutionType: "university",
      registrationNumber: "REG-SIT-2026",
      address: "1 Campus Road, Pune",
      contactPhone: "+91 20 1234 5678",
      officialEmail: "office@sit.edu.in",
      academicPattern: "semester",
      workingDays: ["mon", "tue", "wed", "thu", "fri"],
      defaultLanguage: "en-IN",
      administratorEmail: "admin@sit.edu.in",
    },
    inst.version,
  );
  const configured = await admin.call("POST", `${I}/institutions/${id}/configure`, {}, inst.version);
  check(configured.status === "configured", "institution profile complete and configured");
  const today = new Date().toISOString().slice(0, 10);
  const bp = await admin.call("POST", `${I}/institutions/${id}/org-type-blueprints`, {
    effectiveFrom: today,
    unitTypes: [
      { key: "university", label: "University", allowRoot: true },
      { key: "department", label: "Department", allowedParentTypes: ["university"] },
    ],
  });
  await admin.call("POST", `${I}/org-type-blueprints/${bp.id}/publish`, {}, bp.version);
  const rootUnit = await admin.call("POST", `${I}/institutions/${id}/org-units`, { unitType: "university", code: "ROOT", name: "Sahyadri Institute of Technology" });
  await admin.call("POST", `${I}/org-units/${rootUnit.id}/activate`, {}, rootUnit.version);
  // 2B: campus, structure, programme, curriculum.
  const campus = await admin.call("POST", `${I}/institutions/${id}/campuses`, {
    code: "MAIN",
    name: "Main Campus",
    campusType: "main",
    isMain: true,
    timezone: "Asia/Kolkata",
    establishedOn: "1999-06-01",
    declaredCapacity: 2000,
    contactEmail: "campus@sit.edu.in",
    address: { line1: "1 Campus Road", city: "Pune", state: "Maharashtra", postalCode: "411007", country: "IN" },
  });
  const activeCampus = await admin.call("POST", `${I}/campuses/${campus.id}/activate`, {}, campus.version);
  check(activeCampus.status === "active", "main campus active");
  let s = await admin.call("POST", `${I}/institutions/${id}/academic-structures`, {
    name: "University structure",
    model: "higher_education",
    nepAligned: true,
    effectiveFrom: "2026-06-01",
    nodes: [
      { kind: "level", key: "ug", name: "Undergraduate", nheqfLevel: 5 },
      { kind: "degree_type", key: "btech", name: "B.Tech", parentKey: "ug", nheqfLevel: 6 },
    ],
  });
  await admin.call("POST", `${I}/academic-structures/${s.id}/publish`, {}, s.version);
  let p = await admin.call("POST", `${I}/institutions/${id}/programmes`, { code: "BTECH-CSE", name: "B.Tech Computer Science", level: "ug", degreeType: "btech", durationTerms: 4 });
  p = await admin.call("POST", `${I}/programmes/${p.id}/publish`, {}, p.version);
  const components = [];
  for (let t = 1; t <= 4; t++) {
    components.push({ termNo: t, slotCode: `CS${t}01`, title: `Programming ${t}`, category: "major_core", credits: 12.0, lectureHours: 3, tutorialHours: 1 });
    components.push({ termNo: t, slotCode: `MD${t}01`, title: `Design thinking ${t}`, category: "multidisciplinary", credits: 8.0, lectureHours: 3, tutorialHours: 1 });
    components.push({ termNo: t, slotCode: `EL${t}01`, title: `Elective ${t}`, category: "minor", credits: 4.0, lectureHours: 2, tutorialHours: 0 });
  }
  const c = await admin.call("POST", `${I}/programmes/${p.id}/curricula`, {
    code: "CSE-2026",
    title: "B.Tech CSE 2026",
    initialVersion: {
      effectiveFrom: "2026-07-01",
      appliesFromCohort: "2026-27",
      termStructure: "semester",
      termCount: 4,
      creditFramework: "nep2020",
      minTotalCredits: 80,
      minCreditsPerTerm: 16,
      maxCreditsPerTerm: 28,
      nepAligned: true,
      nheqfLevel: 6,
      multipleEntryExit: false,
      abcEnabled: true,
      exitAwards: [],
      components,
    },
  });
  let cv = await admin.get(`${I}/curriculum-versions/${c.openVersionId}`);
  cv = await admin.call("POST", `${I}/curriculum-versions/${cv.id}/publish`, {}, cv.version);
  // 2C: calendar (two terms), open year, effective assignment.
  const cal = await admin.call("POST", `${I}/institutions/${id}/calendars`, { code: `CAL-${suffix}`, name: "Main calendar" });
  let year = await admin.call("POST", `${I}/institutions/${id}/academic-years`, { calendarId: cal.id, label: "2026-27", startsOn: "2026-06-01", endsOn: "2027-05-31" });
  let v = await admin.call("POST", `${I}/calendars/${cal.id}/versions`, { academicYearId: year.id });
  for (const [code, seq, startsOn, endsOn] of [
    ["T1", 1, "2026-06-15", "2026-11-30"],
    ["T2", 2, "2026-12-01", "2027-04-30"],
  ])
    v = await admin.call("POST", `${I}/calendar-versions/${v.id}/terms`, { code, name: `Term ${seq}`, sequence: seq, startsOn, endsOn }, v.version);
  v = await admin.call("POST", `${I}/calendar-versions/${v.id}/publish`, {}, v.version);
  year = await admin.call("POST", `${I}/academic-years/${year.id}/open`, {}, year.version);
  const a = await admin.call("POST", `${I}/calendars/${cal.id}/assignments`, { calendarVersionId: v.id, scopeKind: "institution", effectiveFrom: "2026-06-01" });
  const effective = await admin.call("POST", `${I}/calendar-assignments/${a.id}/activate`, {}, a.version);
  check(effective.status === "effective", "calendar assignment effective");
  const readiness = await admin.get(`${I}/institutions/${id}/activation-readiness`);
  check(readiness.ready === true, `institution ready for activation (blocking: ${readiness.items.filter((x) => x.status === "missing" || x.status === "blocking").map((x) => x.key).join(", ") || "none"})`);
  inst = await admin.get(`${I}/institutions/${id}`);
  inst = await admin.call("POST", `${I}/institutions/${id}/submit`, {}, inst.version);
  check(inst.status === "active", `institution active (${inst.status})`);
  const terms = [...v.terms].sort((x, y) => x.sequence - y.sequence);
  return { id, programmeId: p.id, curriculumVersionId: cv.id, yearId: year.id, termIds: terms.map((t) => t.id) };
}

function psql(sql) {
  const r = spawnSync("psql", [DATABASE_URL, "-qtAX", "-v", "ON_ERROR_STOP=1", "-c", sql], { encoding: "utf8" });
  if (r.error || r.status !== 0) throw new Error(`psql: ${r.error?.message ?? r.stderr}`);
  return r.stdout.trim();
}

/** An active student + active context inserted into the foundation tables (like tests/support/b3.rs). */
function seedStudent(setup, given, family, number) {
  const sid = psql(
    `INSERT INTO student.student (tenant_id, student_number, given_name, family_name, primary_email, status) ` +
      `VALUES ('${TENANT}', '${number}', '${given}', '${family}', '${given.toLowerCase()}@students.sit.edu.in', 'active') RETURNING id`,
  );
  psql(
    `INSERT INTO student.student_context (tenant_id, student_id, institution_id, programme_id, curriculum_version_id, ` +
      `admission_term_id, admission_year_id, current_term_no, enrolment_number, batch_code, section_code, enrolment_type, enrolled_on, status) ` +
      `VALUES ('${TENANT}', '${sid}', '${setup.id}', '${setup.programmeId}', '${setup.curriculumVersionId}', '${setup.termIds[0]}', ` +
      `'${setup.yearId}', 4, 'EN-${number}', '2023', 'A', 'regular', DATE '2023-07-01', 'active') RETURNING id`,
  );
  return sid;
}

// ---------------------------------------------------------------- browser helpers

const problems = [];
const expectedFailures = [];
function watch(page) {
  page.on("console", (m) => {
    if (m.type() !== "error") return;
    const text = m.text();
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
    const expected =
      (u.pathname === "/api/v1/auth/session" && r.status() === 401) ||
      body.includes("mfa-step-up-required") ||
      expectedFailures.some((f) => f(u.pathname, r.status(), body));
    log(`  ${expected ? "expected" : "UNEXPECTED"} ${r.request().method()} ${u.pathname} → ${r.status()} ${body.slice(0, 300)}`);
    if (!expected) problems.push(`${r.request().method()} ${u.pathname} → ${r.status()} ${body.slice(0, 300)}`);
  });
}

let shotNo = 0;
async function shot(page, name) {
  shotNo++;
  const file = path.join(SHOTS, `${String(shotNo).padStart(2, "0")}-${name}.png`);
  await settle(page);
  // Let the success toasts fade so they don't cover the page.
  await page.waitForFunction(() => document.querySelectorAll(".sb-toast").length === 0, null, { timeout: 6000 }).catch(() => {});
  // Visibly broken: leaked placeholders or a page wider than the viewport.
  const broken = await page.evaluate(() => {
    const main = document.querySelector("main") ?? document.body;
    const text = main.innerText;
    const leaked = text.match(/\bundefined\b|\bNaN\b|\[object Object\]|\bnull\b/);
    const wide = document.documentElement.scrollWidth > window.innerWidth + 1;
    return [leaked ? `text “${leaked[0]}” on the page` : null, wide ? `horizontal overflow (${document.documentElement.scrollWidth}px)` : null].filter(Boolean);
  });
  if (broken.length) problems.push(`${name}: ${broken.join(", ")}`);
  // The fixed environment tag would be painted over table rows in a full-page capture.
  const tag = await page.addStyleTag({ content: ".sb-env-tag { visibility: hidden !important; }" });
  await page.screenshot({ path: file, fullPage: true });
  await tag.evaluate((n) => n.remove());
  log(`  📸 ${path.basename(file)}`);
}

async function settle(page) {
  await page.waitForLoadState("networkidle").catch(() => {});
  await page.waitForTimeout(250);
}

/** Problems seen since `mark` (used to attribute errors to a step). */
let problemMark = 0;
function newProblems() {
  const out = problems.slice(problemMark);
  problemMark = problems.length;
  return out;
}

async function step(page, name, fn) {
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
    const file = path.join(SHOTS, `fail-${String(failed.length).padStart(2, "0")}-${name.replace(/[^a-z0-9]+/gi, "-").toLowerCase().slice(0, 50)}.png`);
    await page.screenshot({ path: file, fullPage: true }).catch(() => {});
    // Close a dialog left open so later steps can go on.
    await page.keyboard.press("Escape").catch(() => {});
  }
}

async function signIn(page, role) {
  await page.goto(`${BASE}/sign-in`);
  await settle(page);
  await page.click(`[data-role-card="${role}"]`);
  await page.waitForURL(`${BASE}/`, { timeout: 20000 });
  await settle(page);
}

async function signOut(page) {
  await go(page, "/account/security");
  await page.getByRole("button", { name: "Sign out of this device" }).click();
  await page.waitForURL(`${BASE}/sign-in`);
  await settle(page);
}

async function go(page, p) {
  await page.goto(`${BASE}${p}`);
  await settle(page);
}

/** The bento tile whose heading starts with `title`. */
function tile(page, title) {
  return page.locator(".sb-tile").filter({ has: page.locator("h4", { hasText: title }) }).first();
}

/** The table row (`tr`) containing `text`. */
function row(page, text, scope) {
  return (scope ?? page).locator("tr", { hasText: text }).first();
}

/** Clicks `[data-action=id]` in the row containing `text`, then waits for the network. */
async function rowAction(page, text, id, scope) {
  const btn = row(page, text, scope).locator(`[data-action="${id}"]`).first();
  await btn.waitFor({ state: "visible", timeout: 10000 });
  await btn.click();
  await settle(page);
}

/** Fills `[data-form=name]` (inputs, selects, checkboxes by `name`) and submits it. */
async function fillForm(page, name, values = {}, { submit = true } = {}) {
  const form = page.locator(`[data-form="${name}"]`).last();
  await form.waitFor({ state: "visible", timeout: 10000 });
  for (const [k, v] of Object.entries(values)) {
    const el = form.locator(`[name="${k}"]`).first();
    const tag = await el.evaluate((n) => `${n.tagName}:${n.getAttribute("type") ?? ""}`);
    if (tag.startsWith("SELECT")) {
      // By value, else by the first option whose label contains `v`.
      const opts = await el.locator("option").evaluateAll((os) => os.map((o) => ({ value: o.value, label: o.textContent ?? "" })));
      const hit = opts.find((o) => o.value === v) ?? opts.find((o) => o.label.includes(v));
      if (!hit) throw new Error(`form ${name}: no option “${v}” in ${k} (${opts.map((o) => o.label).join(" | ")})`);
      await el.selectOption(hit.value);
    } else if (tag.endsWith(":checkbox")) {
      if (v) await el.check();
      else await el.uncheck();
    } else await el.fill(String(v));
  }
  if (submit) {
    await form.locator('button[type="submit"]').click();
    await settle(page);
    // A dialog form closes on success; a page form stays. Report inline errors either way.
    const err = form.locator('[role="alert"], .sb-field-error');
    if (await form.isVisible().catch(() => false)) {
      const errs = (await err.allTextContents().catch(() => [])).filter(Boolean);
      if (errs.length) throw new Error(`form ${name}: ${errs.join(" | ")}`);
    }
  }
}

async function expectRowStatus(page, text, status, scope) {
  const r = row(page, text, scope);
  await r.filter({ hasText: status }).waitFor({ timeout: 10000 }).catch(() => {});
  const content = ((await r.textContent().catch(() => "")) ?? "").replace(/\s+/g, " ").trim();
  check(content.includes(status), `row “${text}” shows “${status}” (row: ${content.slice(0, 160)})`);
}

async function lastToast(page) {
  const t = page.locator(".sb-toast").last();
  await t.waitFor({ state: "visible", timeout: 5000 }).catch(() => {});
  return (await t.textContent().catch(() => ""))?.trim() ?? "";
}

/** No unexpected empty states inside the tile titled `title`. */
async function expectNotEmpty(page, title) {
  const rows = await tile(page, title).locator("tbody tr").filter({ hasNot: page.locator(".sb-empty") }).count();
  check(rows > 0, `“${title}” lists data (${rows} row(s))`);
}

// ---------------------------------------------------------------- run
let api;
let web;
let browser;
const ids = {};
try {
  if (!existsSync(apiBin)) throw new Error(`API binary not found at ${apiBin} — run \`cargo build -p sutra-api\` or set SUTRA_API_BIN`);
  mkdirSync(SHOTS, { recursive: true });
  for (const f of readdirSync(SHOTS)) if (/^(\d\d|fail)-.*\.png$/.test(f)) unlinkSync(path.join(SHOTS, f));
  const existing = psql("SELECT count(*) FROM student.student");
  if (existing !== "0") throw new Error(`DATABASE_URL must be a fresh migrated database (found ${existing} students)`);

  log(`API (dev auth) on :${API_PORT}, web (${WEB_MODE}) on ${BASE}, shots → ${SHOTS}`);
  api = await startApi();
  web = startWeb();

  log("— seed: institution to activation through the Batch 2 API");
  const admin = await apiLogin("admin@sutra.dev");
  const setup = await seedInstitution(admin);
  Object.assign(ids, setup);
  ids.meera = seedStudent(setup, "Meera", "Kulkarni", "S2023-0101");
  ids.rohan = seedStudent(setup, "Rohan", "Patil", "S2023-0102");
  log(`  ✓ institution ${setup.id}, students Meera (completion) + Rohan (exit)`);

  await waitFor(`${BASE}/sign-in`, web);
  browser = await chromium.launch({
    headless: !process.env.E2E_HEADED,
    executablePath: process.env.CHROMIUM_PATH || undefined,
    args: ["--no-sandbox", ...(process.env.E2E_CHROMIUM_ARGS ?? "").split(" ").filter(Boolean)],
  });
  const context = await browser.newContext({ viewport: { width: 1280, height: 800 }, locale: "en-IN", timezoneId: "Asia/Kolkata" });
  const page = await context.newPage();
  page.setDefaultTimeout(15000);
  watch(page);
  // Warm the dev server's dependency optimiser (cold start compiles + one-off reloads).
  page.setDefaultNavigationTimeout(180000);
  for (const p of ["/sign-in", "/sign-in"]) await go(page, p);

  await signIn(page, "administrator");
  // Warm every lazily optimised route once so the reload doesn't race the clicks.
  for (const p of ["/students", "/students/admissions", `/students/record/${ids.meera}`, "/registration", "/documents"]) await go(page, p);
  page.setDefaultNavigationTimeout(30000);
  newProblems();

  // ------------------------------------------------------------ admissions
  await step(page, "admissions: applications list", async () => {
    await go(page, "/students/admissions");
    await page.locator("[data-new-application]").waitFor();
    check(await page.locator("[data-new-application]").isEnabled(), "“New application” enabled (programme published)");
    await page.click("[data-new-application]");
    await fillForm(page, "new-application", {
      givenName: "Aarav",
      familyName: "Desai",
      dateOfBirth: "2007-03-14",
      email: "aarav.desai@students.sit.edu.in",
      phone: "+91 98220 11223",
      subjectRef: STUDENT_SUBJECT,
    });
    await page.click("[data-new-application]");
    await fillForm(page, "new-application", { givenName: "Kavya", familyName: "Joshi", dateOfBirth: "2007-08-02", email: "kavya@example.in" });
    await page.locator('[data-application="Aarav Desai"]').waitFor();
    await expectRowStatus(page, "Aarav Desai", "Draft");
    await shot(page, "admissions-applications");
  });

  await step(page, "admissions: submit, review, offer, reject", async () => {
    await rowAction(page, "Aarav Desai", "application-submit");
    await expectRowStatus(page, "Aarav Desai", "Submitted");
    await rowAction(page, "Aarav Desai", "application-review");
    await expectRowStatus(page, "Aarav Desai", "Under review");
    await rowAction(page, "Kavya Joshi", "application-submit");
    await rowAction(page, "Kavya Joshi", "application-reject");
    await fillForm(page, "reject-application", { reason: "Eligibility criteria not met (qualifying marks)" });
    await expectRowStatus(page, "Kavya Joshi", "Rejected");
    await rowAction(page, "Aarav Desai", "application-offer");
    await shot(page, "admissions-offer-dialog");
    await fillForm(page, "offer", { offerExpiresOn: "2026-12-31", requiredDocuments: "identity_proof, qualifying_marksheet" });
    await page.locator('[data-admission="Aarav Desai"]').waitFor();
    await shot(page, "admissions-offer-made");
  });

  await step(page, "admissions: acceptance and documents", async () => {
    const recs = page.locator("tr", { has: page.locator('[data-admission="Aarav Desai"]') });
    await recs.locator('[data-action="admission-accept"]').click();
    await settle(page);
    await recs.locator('[data-action="admission-documents"]').click();
    const drawer = page.getByRole("dialog");
    await drawer.waitFor();
    for (const [type, title] of [
      ["identity_proof", "Aadhaar card"],
      ["qualifying_marksheet", "HSC marksheet 2025"],
    ]) {
      await drawer.locator("[data-add-admission-document]").click();
      await fillForm(page, "admission-document", { documentType: type, title, fileRef: `dms://admissions/${type}.pdf` });
    }
    await page.waitForFunction(() => document.querySelectorAll('[role="dialog"] tbody tr').length >= 2);
    await shot(page, "admissions-documents-drawer");
    await page.keyboard.press("Escape");
    await settle(page);
    await recs.locator('[data-action="admission-submit-documents"]').click();
    await settle(page);
    await recs.locator('[data-action="admission-documents"]').click();
    await drawer.waitFor();
    for (let i = 0; i < 2; i++) {
      await drawer.locator('[data-action="doc-verify"]').first().click();
      await settle(page);
    }
    await page.waitForFunction(() => [...document.querySelectorAll('[role="dialog"] tbody tr')].every((r) => r.textContent.includes("Verified")));
    await shot(page, "admissions-documents-verified");
    await page.keyboard.press("Escape");
    await settle(page);
    await expectRowStatus(page, "Aarav Desai", "Verification completed", tile(page, "Admission records"));
  });

  await step(page, "admissions: approval, clearance, confirmation, student", async () => {
    const recs = () => tile(page, "Admission records").locator("tr", { hasText: "Aarav Desai" });
    await recs().locator('[data-action="admission-submit-for-approval"]').click();
    await settle(page);
    await expectRowStatus(page, "Aarav Desai", "Approved", tile(page, "Admission records"));
    await recs().locator('[data-action="admission-financial-clearance"]').click();
    await settle(page);
    await recs().locator('[data-action="admission-confirm"]').click();
    await settle(page);
    await expectRowStatus(page, "Aarav Desai", "Confirmed", tile(page, "Admission records"));
    await shot(page, "admissions-confirmed");
    await recs().locator('[data-action="admission-create-student"]').click();
    await settle(page);
    await recs().getByRole("link", { name: "Aarav Desai" }).waitFor();
    await shot(page, "admissions-student-created");
  });

  await step(page, "student 360: complete enrolment", async () => {
    await go(page, "/students");
    await page.getByRole("link", { name: "Aarav Desai" }).click();
    await page.waitForURL(/\/students\/record\//);
    ids.aarav = page.url().split("/students/record/")[1].split("/")[0];
    await go(page, `/students/record/${ids.aarav}/enrolment`);
    await page.locator('[data-action="student-enrol"]').click();
    await fillForm(page, "student-enrol", { rollNumber: "CSE-26-001", batchCode: "2026", sectionCode: "A" });
    await page.locator('[data-context-status="active"]').waitFor();
    await shot(page, "student360-enrolment");
  });

  await step(page, "students: directory", async () => {
    await go(page, "/students");
    await page.locator('[data-student]').first().waitFor();
    check((await page.locator("tbody tr").count()) >= 3, "directory lists the three students");
    await expectRowStatus(page, "Aarav Desai", "Active");
    await shot(page, "students-directory");
  });

  await step(page, "student 360: overview", async () => {
    await go(page, `/students/record/${ids.aarav}`);
    await page.locator('[data-current-context="active"]').waitFor();
    await page.locator('[data-action="student-profile"]').click();
    await fillForm(page, "student-profile", { preferredName: "Aarav" });
    await shot(page, "student360-overview");
  });

  await step(page, "student 360: documents and certificate request", async () => {
    await go(page, `/students/record/${ids.aarav}/documents`);
    await page.locator('[data-document="identity_proof"]').waitFor();
    await page.click("[data-raise-request]");
    await fillForm(page, "student-request", { documentType: "bonafide_certificate", purpose: "Bank account opening" });
    const requests = tile(page, "Document & certificate requests");
    await rowAction(page, "Bonafide certificate", "request-start", requests);
    await rowAction(page, "Bonafide certificate", "request-issue", requests);
    await fillForm(page, "request-issue", { issuedReference: "BON/2026/0001" });
    await expectRowStatus(page, "Bonafide certificate", "Issued", requests);
    await expectRowStatus(page, "Bonafide certificate", "Verified", tile(page, "Documents on record"));
    await shot(page, "student360-documents");
  });

  // ------------------------------------------------------------ registration + progression (admin)
  await step(page, "registration: schedule and open a window", async () => {
    await go(page, "/students/registration");
    await page.click("[data-new-window]");
    const form = page.locator('[data-form="new-window"]');
    await form.waitFor();
    await page.waitForFunction(() => document.querySelectorAll('[data-form="new-window"] select[name="academicTermId"] option').length > 1);
    const term = await form.locator('select[name="academicTermId"] option').nth(1).getAttribute("value");
    await fillForm(page, "new-window", { academicTermId: term, name: "Term 1 registration", closesOn: "2026-11-15", minCredits: "16", maxCredits: "28" });
    await rowAction(page, "Term 1 registration", "window-open");
    await expectRowStatus(page, "Term 1 registration", "Open");
    await shot(page, "registration-windows");
  });

  await step(page, "progression: policy", async () => {
    await go(page, "/students/progression");
    await page.click("[data-new-progression-policy]");
    await fillForm(page, "new-progression-policy", { code: "UG", name: "UG progression rules" });
    await rowAction(page, "UG progression rules", "policy-submit");
    await expectRowStatus(page, "UG progression rules", "Effective");
    await shot(page, "progression-policies");
  });

  // ------------------------------------------------------------ student self-service
  await signOut(page);
  await signIn(page, "student");
  await step(page, "student: self-registration with add and drop", async () => {
    await go(page, "/registration");
    await page.locator('[data-form="new-registration"]').waitFor();
    await shot(page, "student-registration-start");
    for (const slot of ["CS101", "MD101"]) await page.check(`[data-component="${slot}"]`);
    await page.click('[data-action="registration-create"]');
    await settle(page);
    await page.locator("[data-registration]").waitFor();
    // Add the elective after starting, then drop it again.
    const add = page.locator('[data-action="line-add"]');
    check((await add.count()) > 0, "an “Add component” action is offered on the draft registration");
    await add.first().click();
    await fillForm(page, "line-add", { curriculumComponentId: "EL101" });
    await page.locator("[data-registration] tr", { hasText: "EL101" }).waitFor();
    await rowAction(page, "EL101", "line-drop");
    await page.waitForFunction(() => [...document.querySelectorAll("[data-registration] tr")].some((r) => r.textContent.includes("EL101") && /Dropped/i.test(r.textContent)) || ![...document.querySelectorAll("[data-registration] tr")].some((r) => r.textContent.includes("EL101")));
    await page.click('[data-action="registration-submit"]');
    await settle(page);
    const text = await page.locator("[data-registration]").locator("..").textContent();
    check(/Registered|Approved|Submitted/.test(text ?? ""), `registration submitted (${(text ?? "").slice(0, 80)})`);
    await shot(page, "student-registration-submitted");
  });

  await step(page, "student: documents and certificate request", async () => {
    await go(page, "/documents");
    await page.getByText("Identity proof", { exact: false }).first().waitFor();
    const req = page.locator("[data-request-certificate], [data-action='request-certificate']").first();
    await req.click();
    await fillForm(page, "my-request", { documentType: "transcript_copy", purpose: "Internship application" });
    await expectRowStatus(page, "Transcript copy", "Requested");
    await rowAction(page, "Transcript copy", "my-request-cancel");
    await expectRowStatus(page, "Transcript copy", "Cancelled");
    await shot(page, "student-documents");
  });

  // ------------------------------------------------------------ back to admin
  await signOut(page);
  await signIn(page, "administrator");

  await step(page, "student 360: registration (admin view, finalise)", async () => {
    await go(page, `/students/record/${ids.aarav}/registration`);
    await page.locator("[data-registration]").waitFor();
    await rowAction(page, "Term 1 registration", "window-open").catch(() => {});
    await go(page, "/students/registration");
    await rowAction(page, "Term 1 registration", "window-close");
    await expectRowStatus(page, "Term 1 registration", "Closed");
    await rowAction(page, "Term 1 registration", "window-finalise");
    check(/finalised/.test(await lastToast(page)), "window finalised its registrations");
    await go(page, `/students/record/${ids.aarav}/registration`);
    await page.locator("[data-registration]").waitFor();
    check(((await page.locator("[data-registration]").locator("..").locator("..").textContent()) ?? "").includes("Finalised"), "registration shows Finalised");
    await shot(page, "student360-registration");
  });

  await step(page, "student 360: progression decision and credits", async () => {
    await go(page, `/students/record/${ids.aarav}/progression`);
    await page.click('[data-action="decision-new"]');
    const form = page.locator('[data-form="decision-new"]');
    await form.waitFor();
    await page.waitForFunction(() => document.querySelectorAll('[data-form="decision-new"] select[name="academicTermId"] option').length > 1);
    const term = await form.locator('select[name="academicTermId"] option').nth(1).getAttribute("value");
    await fillForm(page, "decision-new", { academicTermId: term, creditsAttempted: "20", creditsEarned: "20", evidenceNote: "Term 1 results (manual entry)" });
    await rowAction(page, "Term 1", "decision-submit");
    await expectRowStatus(page, "Term 1", "Effective");
    await page.click('[data-action="eligibility-evaluate"]');
    await settle(page);
    await page.locator("[data-eligibility]").waitFor();
    await expectNotEmpty(page, "Credit ledger");
    await shot(page, "student360-progression");
  });

  // ------------------------------------------------------------ guardians
  await step(page, "guardians: register", async () => {
    await go(page, "/students/guardians");
    await page.click("[data-new-guardian]");
    await fillForm(page, "new-guardian", { givenName: "Sunita", familyName: "Desai", phone: "+91 98220 44556", email: "sunita.desai@example.in" });
    await page.locator('[data-guardian="Sunita"]').waitFor();
    await shot(page, "guardians-list");
  });

  await step(page, "student 360: guardian relationship lifecycle", async () => {
    await go(page, `/students/record/${ids.aarav}/guardians`);
    await page.click("[data-new-relationship]");
    await fillForm(page, "guardian-link", { guardianId: "Sunita Desai", relationshipType: "mother", isPrimary: true });
    await page.locator('[data-relationship="mother"]').waitFor();
    check(((await row(page, "Mother").textContent()) ?? "").includes("Sunita Desai"), "relationship shows the guardian's name");
    await rowAction(page, "Mother", "relationship-submit-verification");
    await fillForm(page, "guardian-verify", { method: "document", evidenceRef: "Birth certificate BC-2007-1123" });
    // Verification is auto-decided through the WorkflowPort; approve by hand when it waits.
    if ((await row(page, "Mother").locator('[data-action="relationship-verification-decision"]').count()) > 0) await rowAction(page, "Mother", "relationship-verification-decision");
    await expectRowStatus(page, "Mother", "Verified");
    await rowAction(page, "Mother", "relationship-activate");
    await expectRowStatus(page, "Mother", "Active");
    await page.click("[data-new-contact]");
    await fillForm(page, "emergency-contact", { name: "Rajesh Desai", relation: "Father", phone: "+91 98220 77889", priority: "1" });
    await page.click("[data-new-pickup]");
    await fillForm(page, "pickup", { personName: "Sunita Desai", idDocumentRef: "Aadhaar XXXX-1123", validUntil: "2027-05-31" });
    await expectRowStatus(page, "Rajesh Desai", "+91");
    await shot(page, "student360-guardians");
  });

  // ------------------------------------------------------------ scholarships
  await step(page, "scholarships: definition and policy", async () => {
    await go(page, "/students/scholarships");
    await page.click("[data-new-scholarship]");
    await fillForm(page, "new-scholarship", { code: "MERIT-2026", name: "Merit scholarship 2026", benefitType: "fee_percentage", benefitValue: "50", validFrom: "2026-06-01", renewable: true });
    await page.locator('[data-scholarship="MERIT-2026"]').waitFor();
    await rowAction(page, "MERIT-2026", "scholarship-policy");
    await fillForm(page, "scholarship-policy", { minAcademicScore: "75", maxFamilyIncome: "800000" });
    await rowAction(page, "MERIT-2026", "scholarship-publish");
    await rowAction(page, "MERIT-2026", "scholarship-activate");
    await expectRowStatus(page, "MERIT-2026", "Active");
    check(!((await row(page, "MERIT-2026").textContent()) ?? "").includes("—"), "policy version shown");
    await shot(page, "scholarships-list");
  });

  await step(page, "student 360: scholarship determination", async () => {
    await go(page, `/students/record/${ids.aarav}/scholarships`);
    await page.click("[data-new-eligibility]");
    await fillForm(page, "determine-eligibility", { scholarshipId: "Merit scholarship 2026", academicScore: "88", familyIncome: "600000" });
    await page.locator("[data-eligibility]").first().waitFor();
    check(((await row(page, "Merit").textContent()) ?? "").includes("Merit scholarship 2026"), "eligibility names the scholarship");
    await rowAction(page, "Merit", "eligibility-recommend");
    if ((await row(page, "Merit").locator('[data-action="eligibility-recommendation-decision"]').count()) > 0) await rowAction(page, "Merit", "eligibility-recommendation-decision");
    await expectRowStatus(page, "Merit", "Recommendation generated");
    await rowAction(page, "Merit", "eligibility-hand-off");
    await expectRowStatus(page, "Merit", "Active");
    await shot(page, "student360-scholarships");
  });

  // ------------------------------------------------------------ completion + alumni (Meera)
  await step(page, "student 360: qualification completion", async () => {
    await go(page, `/students/record/${ids.meera}/completion`);
    await page.click('[data-action="completion-start"]');
    await settle(page);
    await rowAction(page, "Degree", "completion-eligibility-evaluation");
    await fillForm(page, "completion-eligibility", { creditsEarned: "96" });
    await expectRowStatus(page, "Degree", "Degree audit");
    await rowAction(page, "Degree", "completion-degree-audit");
    await settle(page);
    await shot(page, "student360-completion-clearance");
    for (let i = 0; i < 6; i++) {
      const r = row(page, "Degree").locator('[data-action^="resolve-"]');
      if ((await r.count()) === 0) break;
      await r.first().click();
      await settle(page);
    }
    // Auto-approved through the WorkflowPort once the clearances are done; decide when it waits.
    if ((await row(page, "Degree").locator('[data-action="completion-decision"]').count()) > 0) await rowAction(page, "Degree", "completion-decision");
    await expectRowStatus(page, "Degree", "Approved");
    for (const [cmd, status] of [
      ["completion-award", "Awarded"],
      ["completion-finalise", "Record finalized"],
      ["completion-complete-exit", "Exit completed"],
    ]) {
      await rowAction(page, "Degree", cmd);
      await expectRowStatus(page, "Degree", status);
    }
    await expectNotEmpty(page, "Conferred qualifications");
    await shot(page, "student360-completion-awarded");
  });

  await step(page, "student 360: alumni transition", async () => {
    await go(page, `/students/record/${ids.meera}/completion`);
    await page.click('[data-action="alumni-initiate"]');
    await settle(page);
    for (let i = 0; i < 6; i++) {
      const tile = page.locator("tr", { has: page.locator('[data-action^="alumni-"]') });
      if ((await tile.count()) === 0) break;
      const btn = tile.locator('[data-action^="alumni-"]').first();
      const id = await btn.getAttribute("data-action");
      await btn.click();
      await settle(page);
      if (id === "alumni-validate-policy") await fillForm(page, "alumni-policy", { graduationYear: "2026" });
      if (id === "alumni-archive") break;
    }
    await expectRowStatus(page, "Qualification", "Archived", tile(page, "Alumni transition"));
    // Without a reload: the header follows the persona (archived once no live context remains).
    await page.locator(".sb-page-head").filter({ hasText: "Archived" }).waitFor({ timeout: 10000 }).catch(() => {});
    check(((await page.locator(".sb-page-head").first().textContent()) ?? "").includes("Archived"), "Student 360 header shows the archived persona without a reload");
    await shot(page, "student360-alumni");
  });

  // ------------------------------------------------------------ exits (Rohan)
  await step(page, "exits: withdrawal with clearance", async () => {
    await go(page, `/students/record/${ids.rohan}/completion`);
    await page.click("[data-new-exit]");
    await fillForm(page, "new-exit", { kind: "withdrawal", reason: "Relocating abroad with family" });
    await go(page, "/students/exits");
    await page.locator('[data-exit="withdrawal"]').waitFor();
    await shot(page, "exits-clearance-pending");
    for (let i = 0; i < 6; i++) {
      const r = row(page, "Withdrawal").locator('[data-action^="resolve-"]');
      if ((await r.count()) === 0) break;
      await r.first().click();
      await fillForm(page, "exit-clearance", { status: "cleared", note: "No dues" });
    }
    if ((await row(page, "Withdrawal").locator('[data-action="exit-approve"]').count()) > 0) await rowAction(page, "Withdrawal", "exit-approve");
    await expectRowStatus(page, "Withdrawal", "Approved");
    await rowAction(page, "Withdrawal", "exit-complete");
    await expectRowStatus(page, "Withdrawal", "Completed");
    await shot(page, "exits-completed");
    await go(page, `/students/record/${ids.rohan}`);
    await page.locator('[data-current-context]').waitFor();
    await shot(page, "student360-after-withdrawal");
  });

  // ------------------------------------------------------------ faculty read-only
  await signOut(page);
  await signIn(page, "faculty");
  await step(page, "faculty: Student 360 read-only", async () => {
    await go(page, `/students/record/${ids.aarav}`);
    await page.locator('[data-current-context="active"]').waitFor();
    check((await page.locator('[data-action="student-suspend"]').count()) === 0, "no admin actions for faculty");
    await shot(page, "faculty-student360");
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
log(`PASS — ${shotNo} screenshots in ${SHOTS}`);
process.exit(0);
