/**
 * Shared Playwright harness of the SutraERP click-through suites (Batch 4 foundation; the helpers
 * are the Batch 3 `student.e2e.mjs` ones, extracted unchanged so `academic.e2e.mjs` reuses them).
 *
 * Processes (API binary in dev auth + web app in API mode), API seeding (`apiLogin`,
 * `seedInstitution` = an institution taken to `active` through the Batch 2 endpoints, `psql`,
 * `seedStudent`), and browser helpers (`watch`, `shot`, `step`, `signIn`, `signOut`, `go`, `tile`,
 * `row`, `rowAction`, `fillForm`, `expectRowStatus`, `lastToast`, `expectNotEmpty`). Environment:
 * see `student.e2e.mjs` (DATABASE_URL, SUTRA_API_BIN, E2E_WEB, E2E_API_PORT, E2E_SHOTS,
 * CHROMIUM_PATH, E2E_HEADED). `student.e2e.mjs` keeps its own inline copy (Batch 3, unchanged).
 */
import { spawn, spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
export const webDir = path.resolve(here, "../..");
export const root = path.resolve(webDir, "../..");
export const DATABASE_URL = process.env.DATABASE_URL ?? "postgres://sutra:sutra@localhost:5432/sutra_e2e";
export const WEB_MODE = process.env.E2E_WEB === "preview" ? "preview" : "dev";
export const WEB_PORT = WEB_MODE === "preview" ? 3000 : 5173;
export const API_PORT = Number(process.env.E2E_API_PORT ?? 8080);
export const BASE = `http://localhost:${WEB_PORT}`;
// E2E_API_URL points the API seeding at an already running API (the Docker demo seed uses http://api:8080).
export const API = process.env.E2E_API_URL ?? `http://127.0.0.1:${API_PORT}`;
export const SHOTS = process.env.E2E_SHOTS ?? (existsSync("/workspace/work") ? `/workspace/work/out/${process.env.E2E_SUITE_SHOTS ?? "b4-shots"}` : path.join(webDir, "e2e-shots"));
export const TENANT = process.env.SUTRA_DEV_TENANT_ID ?? "0192f5a0-7c3e-7000-8000-000000000001";
export const STUDENT_SUBJECT = "dev|aarav.desai@sutra.dev";
export const apiBin =
  process.env.SUTRA_API_BIN ?? path.join(process.env.CARGO_TARGET_DIR ?? path.join(root, "target"), "debug", "sutra-api");

export const children = new Set();
export const failed = [];
export const log = (...a) => console.log("[e2e]", ...a);

export function check(cond, what) {
  if (cond) log(`  ✓ ${what}`);
  else throw new Error(`check failed: ${what}`);
}

// ---------------------------------------------------------------- processes

export function start(cmd, args, opts, name) {
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

export async function stop(child) {
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

export async function waitFor(url, child, ms = 90000) {
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

export async function startApi() {
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
    // Part 1 (DEF-X-001): the API defaults to human approval; this click-through suite exercises
    // domain flows, not the approval control (covered by db_p1_appr.rs), so it opts in to dev auto-approval.
    SUTRA_APPROVAL_MODE: "auto",
    SUTRA_WEBAUTHN_RP_ID: "localhost",
    SUTRA_WEBAUTHN_ORIGINS: "http://localhost:3000,http://localhost:5173",
  };
  const api = start(apiBin, ["serve"], { env }, "api");
  await waitFor(`${API}/health/live`, api);
  return api;
}

export function startWeb() {
  const env = { ...process.env, VITE_AUTH_MODE: "api", SUTRA_API_URL: `http://localhost:${API_PORT}` };
  const args =
    WEB_MODE === "preview"
      ? ["exec", "vite", "preview", "--outDir", "build/client", "--port", String(WEB_PORT), "--strictPort"]
      : ["exec", "react-router", "dev", "--port", String(WEB_PORT), "--strictPort"];
  return start("pnpm", args, { cwd: webDir, env }, "web");
}

// ---------------------------------------------------------------- API seeding

/** A signed-in API client (dev auth) that keeps the session cookie and CSRF token. */
export async function apiLogin(loginId) {
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

export const I = "/api/v1/institution";

/**
 * Institution taken to `active` through the Batch 2 endpoints. Returns ids the walk needs.
 * `opts` (all optional; defaults = the e2e suites' SIT institution): `code`, `name`, `shortName`,
 * `domain` (e-mail domain), `city` — used by the Docker demo seed (`e2e/demo-seed.mjs`).
 */
/**
 * Governed set-up steps of the institution (Batch 2: org blueprint, campus activation, academic
 * structure, curriculum version, calendar version). With SUTRA_APPROVAL_MODE=auto the command
 * already returns the final state; in human mode (default) the record is pending and the
 * decision endpoint completes it. These decide permissions are held by administrators only (no
 * checker role, SoD not enforced for them — APPROVAL_CONTROL_MATRIX.md), so the decider is the
 * administrator unless `opts.decider` names another signed-in client. Every decision made here is
 * recorded in HARNESS_DECISIONS.
 */
export const HARNESS_DECISIONS = [];
async function decideIfPending(decider, rec, path, done, label) {
  if (done.includes(rec?.status)) return rec;
  const d = await decider.call("POST", path, { decision: "approve", reason: `${label}: approved (harness, human approval mode)` }, rec.version);
  HARNESS_DECISIONS.push({ kind: label, id: rec.id, before: rec.status, after: d?.status, decider: decider.login ?? "administrator" });
  if (!done.includes(d?.status)) throw new Error(`${label}: decision left “${d?.status}” (expected ${done.join(" | ")})`);
  return { ...rec, ...d };
}

export async function seedInstitution(admin, opts = {}) {
  const decider = opts.decider ?? admin;
  const suffix = Date.now().toString(36).toUpperCase().slice(-5);
  const o = { code: `SIT-${suffix}`, name: "Sahyadri Institute of Technology", shortName: "SIT", domain: "sit.edu.in", city: "Pune", ...opts };
  let inst = await admin.call("POST", `${I}/institutions`, { code: o.code, name: o.name });
  const id = inst.id;
  inst = await admin.call(
    "PUT",
    `${I}/institutions/${id}/profile`,
    {
      shortName: o.shortName,
      institutionType: "university",
      registrationNumber: `REG-${o.shortName}-2026`,
      address: `1 Campus Road, ${o.city}`,
      contactPhone: "+91 20 1234 5678",
      officialEmail: `office@${o.domain}`,
      academicPattern: "semester",
      workingDays: ["mon", "tue", "wed", "thu", "fri"],
      defaultLanguage: "en-IN",
      administratorEmail: `admin@${o.domain}`,
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
  const bpPub = await admin.call("POST", `${I}/org-type-blueprints/${bp.id}/publish`, {}, bp.version);
  await decideIfPending(decider, bpPub, `${I}/org-type-blueprints/${bp.id}/decision`, ["published", "active", "effective"], "org type blueprint");
  const rootUnit = await admin.call("POST", `${I}/institutions/${id}/org-units`, { unitType: "university", code: "ROOT", name: o.name });
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
    contactEmail: `campus@${o.domain}`,
    address: { line1: "1 Campus Road", city: o.city, state: "Maharashtra", postalCode: "411007", country: "IN" },
  });
  let activeCampus = await admin.call("POST", `${I}/campuses/${campus.id}/activate`, {}, campus.version);
  activeCampus = await decideIfPending(decider, activeCampus, `${I}/campuses/${campus.id}/decision`, ["active"], "campus activation");
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
  const sPub = await admin.call("POST", `${I}/academic-structures/${s.id}/publish`, {}, s.version);
  await decideIfPending(decider, sPub, `${I}/academic-structures/${s.id}/decision`, ["published"], "academic structure");
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
  cv = await decideIfPending(decider, cv, `${I}/curriculum-versions/${cv.id}/decision`, ["published"], "curriculum version");
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
  { const terms = v.terms; v = await decideIfPending(decider, v, `${I}/calendar-versions/${v.id}/decision`, ["published", "effective", "active"], "calendar version"); v.terms ??= terms; }
  year = await admin.call("POST", `${I}/academic-years/${year.id}/open`, {}, year.version);
  const a = await admin.call("POST", `${I}/calendars/${cal.id}/assignments`, { calendarVersionId: v.id, scopeKind: "institution", effectiveFrom: "2026-06-01" });
  const effective = await admin.call("POST", `${I}/calendar-assignments/${a.id}/activate`, {}, a.version);
  check(effective.status === "effective", "calendar assignment effective");
  const readiness = await admin.get(`${I}/institutions/${id}/activation-readiness`);
  check(readiness.ready === true, `institution ready for activation (blocking: ${readiness.items.filter((x) => x.status === "missing" || x.status === "blocking").map((x) => x.key).join(", ") || "none"})`);
  inst = await admin.get(`${I}/institutions/${id}`);
  inst = await admin.call("POST", `${I}/institutions/${id}/submit`, {}, inst.version);
  // DEF-B-002: submit only requests activation; the Activation Authority decides.
  check(inst.status === "pending_approval", `activation requested (${inst.status})`);
  inst = await admin.call("POST", `${I}/institutions/${id}/decision`, { decision: "approve" }, inst.version);
  check(inst.status === "active", `institution active (${inst.status})`);
  const terms = [...v.terms].sort((x, y) => x.sequence - y.sequence);
  return { id, programmeId: p.id, curriculumVersionId: cv.id, yearId: year.id, termIds: terms.map((t) => t.id) };
}

export function psql(sql) {
  const r = spawnSync("psql", [DATABASE_URL, "-qtAX", "-v", "ON_ERROR_STOP=1", "-c", sql], { encoding: "utf8" });
  if (r.error || r.status !== 0) throw new Error(`psql: ${r.error?.message ?? r.stderr}`);
  return r.stdout.trim();
}

/** An active student + active context inserted into the foundation tables (like tests/support/b3.rs). */
export function seedStudent(setup, given, family, number) {
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

export const problems = [];
export const expectedFailures = [];
export function watch(page) {
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
      // Batch 7: My HR reads (`/people/me/*`, also the faculty home leave shortcut) answer 404 for a
      // login without a staff record; the screens show their empty state.
      (r.status() === 404 && u.pathname.startsWith("/api/v1/people/me/") && body.includes("staff record not found")) ||
      expectedFailures.some((f) => f(u.pathname, r.status(), body));
    log(`  ${expected ? "expected" : "UNEXPECTED"} ${r.request().method()} ${u.pathname} → ${r.status()} ${body.slice(0, 300)}`);
    if (!expected) problems.push(`${r.request().method()} ${u.pathname} → ${r.status()} ${body.slice(0, 300)}`);
  });
}

let shotNo = 0;
/** Screenshots written so far. */
export const shotCount = () => shotNo;
export async function shot(page, name) {
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

export async function settle(page) {
  await page.waitForLoadState("networkidle").catch(() => {});
  await page.waitForTimeout(250);
}

/** Problems seen since `mark` (used to attribute errors to a step). */
let problemMark = 0;
export function newProblems() {
  const out = problems.slice(problemMark);
  problemMark = problems.length;
  return out;
}

export async function step(page, name, fn) {
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

export async function signIn(page, role) {
  await page.goto(`${BASE}/sign-in`);
  await settle(page);
  await page.click(`[data-role-card="${role}"]`);
  await page.waitForURL(`${BASE}/`, { timeout: 20000 });
  await settle(page);
}

export async function signOut(page) {
  await go(page, "/account/security");
  await page.getByRole("button", { name: "Sign out of this device" }).click();
  await page.waitForURL(`${BASE}/sign-in`);
  await settle(page);
}

export async function go(page, p) {
  await page.goto(`${BASE}${p}`);
  await settle(page);
}

/** The bento tile whose heading starts with `title`. */
export function tile(page, title) {
  return page.locator(".sb-tile").filter({ has: page.locator("h4", { hasText: title }) }).first();
}

/** The table row (`tr`) containing `text`. */
export function row(page, text, scope) {
  return (scope ?? page).locator("tr", { hasText: text }).first();
}

/** Clicks `[data-action=id]` in the row containing `text`, then waits for the network. */
export async function rowAction(page, text, id, scope) {
  const btn = row(page, text, scope).locator(`[data-action="${id}"]`).first();
  await btn.waitFor({ state: "visible", timeout: 10000 });
  await btn.click();
  await settle(page);
}

/** Fills `[data-form=name]` (inputs, selects, checkboxes by `name`) and submits it. */
export async function fillForm(page, name, values = {}, { submit = true } = {}) {
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

export async function expectRowStatus(page, text, status, scope) {
  const r = row(page, text, scope);
  await r.filter({ hasText: status }).waitFor({ timeout: 10000 }).catch(() => {});
  const content = ((await r.textContent().catch(() => "")) ?? "").replace(/\s+/g, " ").trim();
  check(content.includes(status), `row “${text}” shows “${status}” (row: ${content.slice(0, 160)})`);
}

export async function lastToast(page) {
  const t = page.locator(".sb-toast").last();
  await t.waitFor({ state: "visible", timeout: 5000 }).catch(() => {});
  return (await t.textContent().catch(() => ""))?.trim() ?? "";
}

/** No unexpected empty states inside the tile titled `title`. */
export async function expectNotEmpty(page, title) {
  const rows = await tile(page, title).locator("tbody tr").filter({ hasNot: page.locator(".sb-empty") }).count();
  check(rows > 0, `“${title}” lists data (${rows} row(s))`);
}

