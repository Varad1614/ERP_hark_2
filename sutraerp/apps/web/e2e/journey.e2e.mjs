#!/usr/bin/env node
/**
 * Part 1 Stage 2 — the full-stack student journey against a RUNNING compose stack
 * (`.github/workflows/sutraerp-part1-demo.yml`; guide: docs/readiness/DEMO_RUN_GUIDE.md).
 *
 *   E2E_PHASE=journey      seed + Admission → Enrolment → Attendance → Fees → Examination → Result
 *   E2E_PHASE=persistence  after `docker compose restart postgres api`: attendance, fees and
 *                          results re-queried (API, DB, browser) and compared with the snapshot
 *
 * Each step: the action in Chromium where a screen exists (else the API — listed as a gap), the
 * API response asserted (captured from the browser's own request, or the API call), the DB state
 * verified with psql (rows + platform.audit_log), and what admin / faculty / student can see.
 * Negatives: invalid transition, duplicate admission, wrong role 403, duplicate payment
 * idempotency key, invalid marks, maker self-approval (403). Approvals are REAL human decisions
 * (SUTRA_APPROVAL_MODE=human): lib/approvals.mjs `approve(kind, id)` posts the domain decision
 * endpoint as the checker persona (registrar, finance_approver, exam_controller, or the
 * administrator where no checker role holds the permission) after asserting the record is pending.
 *
 * Environment: E2E_BASE (default http://localhost:3000, the web container), E2E_AUTH (oidc | dev),
 * DATABASE_URL (schema owner, default postgres://sutra:sutra@localhost:5432/sutra), E2E_SHOTS,
 * E2E_RESULTS (step results JSON, rendered by lib/journey-report.mjs), E2E_STATE (private state
 * between phases: ids + the TOTP secret of the dev realm admin; default /tmp/journey-state.json),
 * E2E_KNOWN_DEFECTS (comma list of open defect ids whose failing check is reported but does not
 * fail the run), CHROMIUM_PATH, E2E_HEADED=1.
 */
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { createHash, randomUUID } from "node:crypto";
import { chromium } from "playwright";

process.env.DATABASE_URL ??= "postgres://sutra:sutra@localhost:5432/sutra";
const { psql, HARNESS_DECISIONS } = await import("./lib/harness.mjs");
const { AUTH, BASE, login, browserSignIn, totpSecrets } = await import("./lib/auth.mjs");
const { approve, DECISIONS, CHECKERS } = await import("./lib/approvals.mjs");
const { seedJourney, day } = await import("./lib/journey-seed.mjs");

const PHASE = process.env.E2E_PHASE ?? "journey";
const SHOTS = process.env.E2E_SHOTS ?? path.resolve("e2e-journey-shots");
const RESULTS = process.env.E2E_RESULTS ?? path.join(SHOTS, "journey-results.json");
const STATE = process.env.E2E_STATE ?? "/tmp/journey-state.json";
const KNOWN = new Set((process.env.E2E_KNOWN_DEFECTS ?? "").split(",").map((s) => s.trim()).filter(Boolean));
const S = "/api/v1/student";
const A = "/api/v1/academic";
const F = "/api/v1/finance";
const X = "/api/v1/exams";
mkdirSync(SHOTS, { recursive: true });

const log = (...a) => console.log("[journey]", ...a);
const q = (v) => `'${String(v).replace(/'/g, "''")}'`;
const R =
  PHASE === "persistence" && existsSync(RESULTS)
    ? JSON.parse(readFileSync(RESULTS, "utf8"))
    : { auth: AUTH, base: BASE, sha: process.env.GITHUB_SHA ?? null, runId: process.env.GITHUB_RUN_ID ?? null, startedAt: new Date().toISOString(), steps: [], gaps: [], autoApproved: [], defects: [], snapshot: {} };
const state = PHASE === "persistence" && existsSync(STATE) ? JSON.parse(readFileSync(STATE, "utf8")) : { ids: {}, totp: {} };
for (const [k, v] of Object.entries(state.totp ?? {})) totpSecrets.set(k, v);
const ids = state.ids;
const save = () => {
  R.decisions = [...(R.decisions ?? []).filter((a) => a.phase !== PHASE), ...HARNESS_DECISIONS.map((a) => ({ ...a, step: "seed: institution set-up (harness)", phase: PHASE })), ...DECISIONS.map((a) => ({ ...a, phase: PHASE }))];
  delete R.autoApproved;
  R.finishedAt = new Date().toISOString();
  writeFileSync(RESULTS, JSON.stringify(R, null, 2));
  state.totp = Object.fromEntries(totpSecrets);
  writeFileSync(STATE, JSON.stringify(state, null, 2));
};
const gap = (step, why) => R.gaps.push({ step, why });

/**
 * Part 2 · S4-docs: a plain PDF (no scripts / embedded files) of about `size` bytes; the padding is
 * PDF comment lines, so the content check (magic bytes, active-content refusal) accepts it.
 */
function pdf(label, size = 0) {
  const head = `%PDF-1.7\n% SutraERP journey document: ${label}\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n2 0 obj << /Type /Pages /Kids [] /Count 0 >> endobj\n`;
  const tail = "trailer << /Root 1 0 R >>\n%%EOF\n";
  const line = `%${"0".repeat(1022)}\n`;
  const n = Math.max(0, Math.ceil((size - head.length - tail.length) / line.length));
  return Buffer.from(head + line.repeat(n) + tail, "latin1");
}
const sha256hex = (b) => createHash("sha256").update(b).digest("hex");

// ---------------------------------------------------------------- step runner + checks

const pages = {};
let shotNo = PHASE === "persistence" ? 100 : 0;
async function snap(page, name) {
  if (!page) return;
  shotNo++;
  const file = path.join(SHOTS, `${String(shotNo).padStart(2, "0")}-${name}.png`);
  await page.waitForLoadState("networkidle").catch(() => {});
  await page.screenshot({ path: file, fullPage: true }).catch(() => {});
  return path.basename(file);
}

async function step(phase, name, via, fn, { knownDefect } = {}) {
  const evidence = [];
  const e = (s) => {
    evidence.push(s);
    log(`   · ${s}`);
  };
  log(`— [${phase}] ${name}`);
  const t0 = Date.now();
  let status = "pass";
  try {
    await fn(e);
  } catch (err) {
    const msg = err instanceof Error ? err.message : String(err);
    evidence.push(`ERROR: ${msg.slice(0, 1200)}`);
    console.error(`[journey]   ✗ ${name}: ${msg}`);
    status = knownDefect && KNOWN.has(knownDefect) ? "known" : "fail";
    if (knownDefect) evidence.push(`expected behaviour not met — open defect ${knownDefect}${status === "known" ? " (listed in E2E_KNOWN_DEFECTS: reported, does not fail the run)" : ""}`);
    for (const [role, p] of via.startsWith("browser") ? Object.entries(pages) : []) {
      const f = await snap(p, `fail-${role}-${name.replace(/[^a-z0-9]+/gi, "-").toLowerCase().slice(0, 40)}`);
      if (f) evidence.push(`screenshot ${f}`);
      await p?.keyboard.press("Escape").catch(() => {});
    }
  }
  R.steps.push({ phase, name, via, status, ms: Date.now() - t0, evidence, knownDefect: knownDefect ?? null });
  save();
  return status === "pass";
}

function check(cond, what, e) {
  if (!cond) throw new Error(`check failed: ${what}`);
  e?.(`✓ ${what}`);
}
function need(v, what) {
  if (v === undefined || v === null || v === "") throw new Error(`prerequisite missing: ${what} (an earlier step failed)`);
  return v;
}

/** psql value, logged as evidence; `pred` must hold. */
function db(e, label, sql, pred = (v) => v !== "") {
  const v = psql(sql);
  e(`DB ${label}: ${v.replace(/\n/g, " ; ").slice(0, 400)}`);
  if (!pred(v)) throw new Error(`DB check failed — ${label}: got “${v.slice(0, 300)}”`);
  return v;
}

/** platform.audit_log rows for a target; `actions` must all be present. Returns the actors. */
function audit(e, targetId, actions = [], label = "") {
  const v = psql(
    `SELECT count(*) || '|' || coalesce(string_agg(DISTINCT action, ',' ORDER BY action), '') || '|' || coalesce(string_agg(DISTINCT actor_id, ','), '') ` +
      `FROM platform.audit_log WHERE target_id = ${q(targetId)}`,
  );
  const [n, acts, actors] = v.split("|");
  e(`DB audit ${label || targetId}: ${n} row(s) — actions [${acts}] actors [${actors}]`);
  if (Number(n) < 1) throw new Error(`no platform.audit_log row for ${label || targetId}`);
  for (const a of actions) if (!acts.split(",").includes(a)) throw new Error(`audit action ${a} missing for ${label || targetId} (have ${acts})`);
  return { count: Number(n), actions: acts.split(","), actors: actors.split(",") };
}

/** GET as `who`; asserts the status (and `pred` on 200). */
async function sees(e, who, client, p, expect, pred, what = "") {
  const r = await client.raw("GET", p);
  const ok = r.status === expect && (r.status !== 200 || !pred || pred(r.json));
  e(`${who}: GET ${p} → ${r.status}${what ? ` (${what})` : ""}${ok ? "" : " ✗"}`);
  if (!ok) throw new Error(`visibility: ${who} GET ${p} → ${r.status} (expected ${expect}${pred ? " with the data" : ""}) ${r.text.slice(0, 200)}`);
  return r.json;
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
const tile = (page, title) => page.locator(".sb-tile").filter({ has: page.locator("h4", { hasText: title }) }).first();
const row = (page, text, scope) => (scope ?? page).locator("tr, .sb-lrow", { hasText: text }).first();

/** Row primary pill `[data-action=id]` or kebab `[data-row-menu]` → `[data-menu-item=id]` (re-skin v2). */
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
const rowAction = async (page, text, id, scope) => {
  const r = row(page, text, scope);
  await r.waitFor({ timeout: 15000 });
  await act(page, r, id);
};

/** Fills `[data-form=name]` (inputs, selects by value or label, checkboxes, TagInput chips) and optionally submits. */
async function fillForm(page, name, values = {}, { submit = true } = {}) {
  const form = page.locator(`[data-form="${name}"]`).last();
  await form.waitFor({ state: "visible", timeout: 15000 });
  for (const [k, v] of Object.entries(values)) {
    const el = form.locator(`[name="${k}"]`).first();
    const tag = await el.evaluate((n) => `${n.tagName}:${n.getAttribute("type") ?? ""}`);
    if (tag === "INPUT:hidden") {
      for (const t of String(v).split(",").map((x) => x.trim()).filter(Boolean)) await form.locator(`[data-tag-option="${t}"]`).click();
    } else if (tag.startsWith("SELECT")) {
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
    if (await form.isVisible().catch(() => false)) {
      const errs = (await form.locator('[role="alert"], .sb-field-error').allTextContents().catch(() => [])).filter(Boolean);
      if (errs.length) throw new Error(`form ${name}: ${errs.join(" | ")}`);
    }
  }
}

/** Collects 5xx answers and unexpected 4xx of a page's /api calls (5xx fail the step that saw them). */
const serverErrors = [];
function watch(page, role) {
  page.on("response", async (r) => {
    const u = new URL(r.url());
    if (!u.pathname.startsWith("/api/") || r.status() < 500) return;
    serverErrors.push(`${role}: ${r.request().method()} ${u.pathname} → ${r.status()} ${(await r.text().catch(() => "")).slice(0, 200)}`);
  });
  page.on("pageerror", (err) => serverErrors.push(`${role}: page error ${err.message}`));
}
function noServerErrors(e) {
  const errs = serverErrors.splice(0);
  if (errs.length) throw new Error(`server/page errors during the step:\n  ${errs.join("\n  ")}`);
  e("✓ no 5xx / page errors in the browser");
}

// ---------------------------------------------------------------- run

let browser;
const clients = {};
try {
  log(`phase ${PHASE} · auth ${AUTH} · ${BASE} · shots ${SHOTS}`);
  psql("SELECT 1");

  await step(PHASE, "Sign-in: administrator, faculty and student (API clients)", AUTH === "oidc" ? "API · Keycloak OIDC (code + PKCE)" : "API · dev authenticator", async (e) => {
    for (const role of ["administrator", "faculty", "student"]) {
      const c = await login(role);
      clients[role] = c;
      e(`${role}: ${c.login} subject ${c.subject} roles ${c.session.principal.roles.join(",")} mfa ${c.session.mfaLevel} amr ${JSON.stringify(c.session.amr)}`);
      check(c.session.principal.roles.includes(role), `${role} session carries the ${role} role`, e);
    }
    check(clients.administrator.session.mfaLevel === "mfa" || AUTH === "dev", "administrator session is MFA (step-up permissions)", e);
    state.subjects = { administrator: clients.administrator.subject, faculty: clients.faculty.subject, student: clients.student.subject };
    CHECKERS.administrator = clients.administrator;
  });
  if (PHASE === "journey")
    await step(PHASE, "Sign-in: checker personas registrar, finance_approver, exam_controller (API clients, MFA)", AUTH === "oidc" ? "API · Keycloak OIDC (code + PKCE + TOTP)" : "API · dev authenticator", async (e) => {
      for (const role of ["registrar", "finance_approver", "exam_controller"]) {
        const c = await login(role);
        CHECKERS[role] = c;
        clients[role] = c;
        e(`${role}: ${c.login} subject ${c.subject} roles [${c.session.principal.roles.join(",")}] mfa ${c.session.mfaLevel}`);
        check(c.session.principal.roles.includes(role), `${role} session carries the ${role} role`, e);
        check(!c.session.principal.roles.includes("administrator"), `${role} is not an administrator (independent checker)`, e);
        check(c.session.mfaLevel === "mfa" || AUTH === "dev", `${role} session is MFA (decisions are step-up permissions)`, e);
        check(c.subject !== clients.administrator?.subject, `${role} is a different person from the administrator (maker)`, e);
      }
      // S3-appr: a second, independent administrator decides administrator-only requests the
      // first administrator made (separation of duties applies to administrators too).
      const dean = await login("academic_admin");
      CHECKERS.academic_admin = dean;
      clients.academic_admin = dean;
      e(`academic_admin: ${dean.login} subject ${dean.subject} roles [${dean.session.principal.roles.join(",")}] mfa ${dean.session.mfaLevel}`);
      check(dean.session.principal.roles.includes("administrator"), "the second administrator holds the administrator role", e);
      check(dean.subject !== clients.administrator?.subject, "the second administrator is a different person from the administrator (maker)", e);
      state.subjects = { ...state.subjects, registrar: CHECKERS.registrar.subject, finance_approver: CHECKERS.finance_approver.subject, exam_controller: CHECKERS.exam_controller.subject, academic_admin: dean.subject };
    });
  const admin = clients.administrator;
  const faculty = clients.faculty;
  const student = clients.student;

  browser = await chromium.launch({ headless: !process.env.E2E_HEADED, executablePath: process.env.CHROMIUM_PATH || undefined, args: ["--no-sandbox"] });
  await step(PHASE, "Sign-in: administrator, faculty and student (browser)", AUTH === "oidc" ? "browser · Keycloak login page" : "browser · role cards", async (e) => {
    for (const role of ["administrator", "faculty", "student"]) {
      const ctx = await browser.newContext({ viewport: { width: 1440, height: 960 }, locale: "en-IN", timezoneId: "Asia/Kolkata" });
      const page = await ctx.newPage();
      page.setDefaultTimeout(20000);
      page.setDefaultNavigationTimeout(60000);
      watch(page, role);
      pages[role] = page;
      await browserSignIn(page, role);
      e(`${role}: signed in, at ${page.url()} (${await snap(page, `${PHASE}-home-${role}`)})`);
    }
    noServerErrors(e);
  });
  const P = pages;

  if (PHASE === "journey") {
    // ============================================================ seed
    await step("seed", "Synthetic institution through the API (institution, campus, departments, programmes, year, courses, sections, faculty, students, fee structure, exam session)", "API", async (e) => {
      need(admin && faculty, "signed-in clients");
      Object.assign(ids, await seedJourney({ admin, faculty }, e));
      db(e, "institution active", `SELECT status FROM institution.institution WHERE id = ${q(ids.id)}`, (v) => v === "active");
      db(e, "departments", `SELECT string_agg(code || ':' || status, ',' ORDER BY code) FROM institution.org_unit WHERE institution_id = ${q(ids.id)} AND unit_type = 'department'`, (v) => v === "CSE:active,MATH:active");
      db(e, "offerings", `SELECT string_agg(offering_code || ':' || status, ',' ORDER BY offering_code) FROM academic.course_offering WHERE institution_id = ${q(ids.id)}`, (v) => v.split(",").length === 3 && !v.includes("draft"));
      db(e, "faculty assignment", `SELECT status || '|' || faculty_subject_ref FROM academic.faculty_assignment WHERE id = ${q(ids.facultyAssignmentId)}`, (v) => v === `active|${faculty.subject}`);
      db(e, "seed students", `SELECT count(*) FROM student.student_context WHERE institution_id = ${q(ids.id)} AND status = 'active'`, (v) => Number(v) >= 2);
      db(e, "fee structure", `SELECT status || '|' || total_amount FROM finance.fee_structure WHERE id = ${q(ids.feeStructureId)}`, (v) => v.startsWith("published|90000"));
      db(e, "exam session", `SELECT status FROM exams.exam_session WHERE id = ${q(ids.sessionId)}`, (v) => v === "active");
      audit(e, ids.id, [], "institution");
      e(`ids: ${JSON.stringify({ institution: ids.id, offering: ids.offerings?.CS101, section: ids.sections?.CS101, feeStructure: ids.feeStructureId, examSession: ids.sessionId })}`);
    });
    save();

    // ============================================================ 1. Admission (browser: admin)
    const AARAV = { fullName: "Aarav Desai", given: "Aarav", family: "Desai", dob: "2007-03-14", email: `aarav.${(ids.suffix ?? "x").toLowerCase()}@students.jit.edu.in`, phone: "+91 98220 11223" };
    await step("1 Admission", "Application → review → offer → acceptance → documents → verification → approval → clearance → confirmation → student", "browser (admin /students/admissions)", async (e) => {
      need(ids.id, "institution");
      const page = P.administrator;
      await go(page, "/students/admissions");
      await page.locator("[data-new-application]").waitFor();
      const created = await ui(page, "POST", new RegExp(`/student/institutions/${ids.id}/admission-applications$`), async () => {
        await page.click("[data-new-application]");
        const form = page.locator('[data-form="new-application"]');
        await form.waitFor({ state: "visible" });
        await fillForm(page, "new-application", { fullName: AARAV.fullName, dateOfBirth: AARAV.dob, phone: AARAV.phone, email: AARAV.email }, { submit: false });
        await form.locator('button[type="submit"]').click();
        await form.locator('[name="programmeId"]').waitFor();
        await fillForm(page, "new-application", { programmeId: ids.programmeId, subjectRef: student.subject }, { submit: false });
        await form.locator('button[type="submit"]').click();
        await form.locator("[data-confirm]").waitFor();
        await form.locator('button[type="submit"]').click();
      }, e);
      check(created.status === 201 && created.body?.status === "draft", "application created (201, draft)", e);
      check(created.body.subjectRef === student.subject, "application linked to the student login subject", e);
      ids.applicationId = created.body.id;
      await page.locator('[data-application="Aarav Desai"]').waitFor();
      let r = await ui(page, "POST", /\/admission-applications\/[^/]+\/submit$/, () => rowAction(page, "Aarav Desai", "application-submit"), e);
      check(r.status === 200 && r.body.status === "submitted", "submitted", e);
      r = await ui(page, "POST", /\/admission-applications\/[^/]+\/review$/, () => rowAction(page, "Aarav Desai", "application-review"), e);
      check(r.status === 200 && r.body.status === "under_review", "under review", e);
      r = await ui(page, "POST", /\/admission-applications\/[^/]+\/offer$/, async () => {
        await rowAction(page, "Aarav Desai", "application-offer");
        await fillForm(page, "offer", { offerExpiresOn: day(30), requiredDocuments: "identity_proof, qualifying_marksheet" });
      }, e);
      check(r.status === 200 && r.body.status === "offered" && r.body.admissionId, "offered — admission record initiated", e);
      ids.admissionId = r.body.admissionId;
      const recs = () => tile(page, "Admission records").locator(".sb-lrow", { hasText: "Aarav Desai" });
      await page.locator('[data-admission="Aarav Desai"]').waitFor();
      r = await ui(page, "POST", /\/admissions\/[^/]+\/accept$/, () => act(page, page.locator('.sb-lrow[data-admission="Aarav Desai"]'), "admission-accept"), e);
      check(r.status === 200 && r.body.offerAcceptedAt, "offer accepted", e);
      await act(page, page.locator('.sb-lrow[data-admission="Aarav Desai"]'), "admission-documents");
      const drawer = page.getByRole("dialog");
      await drawer.waitFor();
      for (const [type, title] of [
        ["identity_proof", "Aadhaar card"],
        ["qualifying_marksheet", "HSC marksheet 2025"],
      ]) {
        r = await ui(page, "POST", /\/admissions\/[^/]+\/documents$/, async () => {
          await drawer.locator("[data-add-admission-document]").click();
          await fillForm(page, "admission-document", { documentType: type, title, fileRef: `dms://admissions/${type}.pdf` });
        }, e);
        check(r.status === 201 && r.body.status === "submitted", `${type} handed in`, e);
        // Part 2 · S4-docs: the scanned file goes to object storage (MinIO in the demo stack). The
        // marksheet is above 1 MiB: sent with its own content type and without an Idempotency-Key.
        const bytes = pdf(title, type === "qualifying_marksheet" ? 1536 * 1024 : 0);
        const filename = `${title}.pdf`;
        const up = await admin.upload(`${S}/admissions/${ids.admissionId}/documents/${r.body.id}/file`, bytes, "application/pdf", filename, { key: randomUUID() });
        e(`PUT …/documents/${type}/file (${bytes.length} bytes, application/pdf${bytes.length > 1024 * 1024 ? ", no Idempotency-Key" : ""}) → ${up.status} ${up.status === 201 ? `object ${up.json.file?.id}` : up.text.slice(0, 300)}`);
        check(up.status === 201 && up.json.file?.sha256 === sha256hex(bytes) && up.json.file?.sizeBytes === bytes.length, `${type} file stored (201, SHA-256 and size match)`, e);
        check(up.json.file?.filename === filename && up.json.file?.contentType === "application/pdf" && !("storageKey" in (up.json.file ?? {})), `${type} file metadata (name, type; no storage key exposed)`, e);
        (ids.files ??= []).push({ type, documentId: r.body.id, objectId: up.json.file.id, sha256: sha256hex(bytes), size: bytes.length, filename });
      }
      gap("1 Admission: document files", "no upload control on the admission documents drawer yet — the files are attached through the API (PUT …/documents/{id}/file)");
      await page.keyboard.press("Escape");
      await settle(page);
      r = await ui(page, "POST", /\/admissions\/[^/]+\/submit-documents$/, () => act(page, page.locator('.sb-lrow[data-admission="Aarav Desai"]'), "admission-submit-documents"), e);
      check(r.status === 200 && r.body.status === "documents_submitted", "documents submitted", e);
      await act(page, page.locator('.sb-lrow[data-admission="Aarav Desai"]'), "admission-documents");
      await drawer.waitFor();
      for (let i = 0; i < 2; i++) {
        r = await ui(page, "POST", /\/admissions\/[^/]+\/documents\/[^/]+\/verify$/, () => drawer.locator('[data-action="doc-verify"]').first().click(), e);
        check(r.status === 200 && r.body.status === "verified", `document ${i + 1} verified`, e);
        await settle(page);
      }
      await page.keyboard.press("Escape");
      await settle(page);
      let adm = await admin.get(`${S}/admissions/${ids.admissionId}`);
      check(adm.status === "verification_completed", `admission verification completed (${adm.status})`, e);
      r = await ui(page, "POST", /\/admissions\/[^/]+\/submit-for-approval$/, () => act(page, recs(), "admission-submit-for-approval"), e);
      check(r.status === 200, "approval requested", e);
      check(r.body?.status === "approval_pending", `admission pending a human decision (${r.body?.status})`, e);
      // Maker-checker: the administrator who requested the approval is refused (403); the registrar decides.
      adm = await approve("admission", ids.admissionId, { client: admin, selfCheck: true, ev: e, step: "1 Admission: registrar decides (maker refused)" });
      e(`approve(admission) → ${adm.status} (approvedBy ${adm.approvedBy ?? "?"})`);
      check(!adm.approvedBy || adm.approvedBy === CHECKERS.registrar.subject, "approved by the registrar", e);
      await go(page, "/students/admissions");
      await page.locator('[data-admission="Aarav Desai"]').waitFor();
      r = await ui(page, "POST", /\/admissions\/[^/]+\/financial-clearance$/, () => act(page, recs(), "admission-financial-clearance"), e);
      check(r.status === 200 && r.body.status === "financially_cleared", "financially cleared", e);
      r = await ui(page, "POST", /\/admissions\/[^/]+\/confirm$/, () => act(page, recs(), "admission-confirm"), e);
      check(r.status === 200 && r.body.status === "confirmed", "confirmed", e);
      r = await ui(page, "POST", /\/admissions\/[^/]+\/create-student$/, () => act(page, recs(), "admission-create-student"), e);
      check(r.status === 200 && r.body.status === "student_created" && r.body.studentId, "student created", e);
      ids.aarav = { id: r.body.studentId, contextId: r.body.studentContextId };
      await recs().getByRole("link", { name: "Aarav Desai" }).waitFor();
      e(`admin sees the admission linked to the new student (${await snap(page, "1-admission-student-created")})`);
      noServerErrors(e);
      // DB
      db(e, "application", `SELECT status FROM student.admission_application WHERE id = ${q(ids.applicationId)}`, (v) => v === "offered");
      db(e, "admission", `SELECT status || '|' || student_id FROM student.admission WHERE id = ${q(ids.admissionId)}`, (v) => v === `student_created|${ids.aarav.id}`);
      db(e, "documents", `SELECT string_agg(document_type || ':' || status, ',' ORDER BY document_type) FROM student.admission_document WHERE admission_id = ${q(ids.admissionId)}`, (v) => v === "identity_proof:verified,qualifying_marksheet:verified");
      const st = db(e, "student", `SELECT status || '|' || student_number || '|' || coalesce(subject_ref, '') FROM student.student WHERE id = ${q(ids.aarav.id)}`, (v) => v.endsWith(`|${student.subject}`));
      ids.aarav.number = st.split("|")[1];
      const au = audit(e, ids.admissionId, ["student.admission.approve", "student.admission.confirm", "student.admission.create_student"], "admission");
      check(!au.actors.some((a) => a.startsWith("system:")), "no system/auto-approval actor in the admission audit trail", e);
      check(au.actors.includes(CHECKERS.registrar.subject), "the registrar is an actor in the admission audit trail", e);
      db(e, "approval decided by the registrar", `SELECT actor FROM student.admission_status_history WHERE admission_id = ${q(ids.admissionId)} AND to_status = 'approved' ORDER BY occurred_at DESC LIMIT 1`, (v) => v === CHECKERS.registrar.subject);
      // visibility
      await sees(e, "faculty", faculty, `${S}/admissions/${ids.admissionId}`, 200, (j) => j.id === ids.admissionId, "student.read: read-only");
      await sees(e, "student", student, `${S}/admissions/${ids.admissionId}`, 403);
    });

    await step("1 Admission", "Document files: downloaded back byte-for-byte, integrity verified at verification, the student retrieves the verified files, access denied to others (MinIO)", "API (no file screen yet)", async (e) => {
      need(ids.files?.length === 2 && ids.aarav?.id, "uploaded files + student");
      for (const f of ids.files) {
        const d = await admin.download(`${S}/admissions/${ids.admissionId}/documents/${f.documentId}/file`);
        e(`admin GET …/documents/${f.type}/file → ${d.status}, ${d.bytes.length} bytes, ${d.headers.get("content-type")}, ${d.headers.get("content-disposition")}`);
        check(d.status === 200 && d.bytes.length === f.size && sha256hex(d.bytes) === f.sha256, `${f.type}: identical bytes downloaded`, e);
        check(d.headers.get("content-type") === "application/pdf" && (d.headers.get("content-disposition") ?? "").startsWith("attachment;") && d.headers.get("x-document-sha256") === f.sha256, `${f.type}: attachment, own content type, SHA-256 header`, e);
        db(e, `${f.type} object`, `SELECT content_type || '|' || size_bytes || '|' || sha256 || '|' || CASE WHEN quarantined_at IS NULL THEN 'clean' ELSE 'quarantined' END FROM platform.document_object WHERE id = ${q(f.objectId)}`, (v) => v === `application/pdf|${f.size}|${f.sha256}|clean`);
        db(e, `${f.type} linked`, `SELECT status || '|' || document_object_id FROM student.admission_document WHERE id = ${q(f.documentId)}`, (v) => v === `verified|${f.objectId}`);
        const au = audit(e, f.objectId, ["platform.document.upload", "platform.document.verify_integrity", "platform.document.download"], `${f.type} file`);
        check(au.actors.includes(admin.subject), "the administrator is the actor of the file audit trail", e);
        const s403 = await student.download(`${S}/admissions/${ids.admissionId}/documents/${f.documentId}/file`);
        e(`student GET admission file → ${s403.status}`);
        check(s403.status === 403, "the student cannot read the admission's files (403)", e);
      }
      const mine = await student.get(`${S}/me/documents`);
      const items = (Array.isArray(mine) ? mine : mine.items ?? []).filter((d) => d.documentObjectId);
      e(`student GET /me/documents → ${items.length} document(s) with a file`);
      check(items.length === ids.files.length, "both verified files moved to the student's record", e);
      for (const d of items) {
        const f = ids.files.find((x) => x.objectId === d.documentObjectId);
        check(!!f, `record document ${d.id} carries an uploaded object`, e);
        const g = await student.download(`${S}/me/documents/${d.id}/file`);
        e(`student GET /me/documents/{id}/file → ${g.status}, ${g.bytes.length} bytes`);
        check(g.status === 200 && sha256hex(g.bytes) === f.sha256, `student retrieves ${f.type} (identical bytes)`, e);
      }
      const other = await faculty.download(`${S}/me/documents/${items[0].id}/file`);
      e(`faculty GET the student's /me document → ${other.status}`);
      check(other.status === 403 || other.status === 404, "another user cannot read the student's own file", e);
    });

    await step("1 Admission", "Negative: wrong role — faculty cannot decide an admission, student cannot create an application (403)", "API", async (e) => {
      need(ids.admissionId, "admission");
      let r = await faculty.raw("POST", `${S}/admissions/${ids.admissionId}/confirm`, {}, 1);
      e(`faculty POST /admissions/{id}/confirm → ${r.status}`);
      check(r.status === 403, "faculty → 403", e);
      r = await student.raw("POST", `${S}/institutions/${ids.id}/admission-applications`, { givenName: "Mallory", programmeId: ids.programmeId });
      e(`student POST /institutions/{id}/admission-applications → ${r.status}`);
      check(r.status === 403, "student → 403", e);
      db(e, "no application by the student", `SELECT count(*) FROM student.admission_application WHERE institution_id = ${q(ids.id)} AND given_name = 'Mallory'`, (v) => v === "0");
    });

    await step("1 Admission", "Negative: invalid transition — confirming an admission that already created its student (409)", "API", async (e) => {
      const adm = await admin.get(`${S}/admissions/${need(ids.admissionId, "admission")}`);
      const r = await admin.raw("POST", `${S}/admissions/${adm.id}/confirm`, {}, adm.version);
      e(`admin POST /admissions/{id}/confirm on ${adm.status} → ${r.status} ${r.json?.detail ?? r.json?.title ?? ""}`);
      check(r.status === 409, "invalid transition rejected with 409", e);
      db(e, "admission unchanged", `SELECT status FROM student.admission WHERE id = ${q(adm.id)}`, (v) => v === adm.status);
    });

    await step(
      "1 Admission",
      "Negative: duplicate admission — a second application for the same applicant (same login, name, date of birth, e-mail) is refused at intake with 409 (DEF-C-003)",
      "API",
      async (e) => {
        need(ids.aarav?.id, "student");
        const before = psql(`SELECT count(*) FROM student.admission_application WHERE institution_id = ${q(ids.id)}`);
        const body = { givenName: AARAV.given, familyName: AARAV.family, dateOfBirth: AARAV.dob, email: AARAV.email, phone: AARAV.phone, programmeId: ids.programmeId, admissionTermId: ids.termIds[0], subjectRef: student.subject };
        const r = await admin.raw("POST", `${S}/institutions/${ids.id}/admission-applications`, body);
        e(`create duplicate application → ${r.status} ${r.json?.detail ?? r.json?.title ?? ""}`);
        if (r.status === 201) {
          // Keep the rest of the journey clean even when the check fails.
          const w = await admin.raw("POST", `${S}/admission-applications/${r.json.id}/withdraw`, { reason: "journey negative test cleanup" }, r.json.version);
          e(`cleanup: duplicate application withdraw → ${w.status}`);
        }
        check(r.status === 409, "duplicate applicant refused at intake (409)", e);
        db(e, "no application created", `SELECT count(*) FROM student.admission_application WHERE institution_id = ${q(ids.id)}`, (v) => v === before);
      },
    );

    // ============================================================ 2. Enrolment
    await step("2 Enrolment", "Student 360: complete enrolment (pending_activation → active)", "browser (admin /students/record/:id/enrolment)", async (e) => {
      const page = P.administrator;
      await go(page, `/students/record/${need(ids.aarav?.id, "student")}/enrolment`);
      const r = await ui(page, "POST", new RegExp(`/student/students/${ids.aarav.id}/enrol$`), async () => {
        await page.locator('[data-action="student-enrol"]').click();
        await fillForm(page, "student-enrol", { rollNumber: "CSE-26-001", batchCode: "2026", sectionCode: "A" });
      }, e);
      check(r.status === 200, "enrol → 200", e);
      const ctx = (r.body.contexts ?? []).find((c) => c.status === "active");
      check(ctx, "an active student context", e);
      await page.locator('[data-context-status="active"]').waitFor();
      e(`admin sees the active context (${await snap(page, "2-student360-enrolled")})`);
      noServerErrors(e);
      db(e, "context", `SELECT status || '|' || coalesce(roll_number, '') FROM student.student_context WHERE student_id = ${q(ids.aarav.id)} AND institution_id = ${q(ids.id)}`, (v) => v === "active|CSE-26-001");
      db(e, "admission completed", `SELECT status FROM student.admission WHERE id = ${q(ids.admissionId)}`, (v) => v === "completed");
      db(e, "student active", `SELECT status FROM student.student WHERE id = ${q(ids.aarav.id)}`, (v) => v === "active");
      audit(e, ids.aarav.id, [], "student");
      await sees(e, "faculty", faculty, `${S}/students/${ids.aarav.id}`, 200, (j) => j.currentContext?.status === "active", "Student 360 read-only");
      await sees(e, "student", student, `${S}/students/${ids.aarav.id}`, 403);
    });

    await step("2 Enrolment", "Registration window for term 1 scheduled and opened", "API (screen exists: /students/registration — not driven)", async (e) => {
      gap("2 Enrolment: registration window", "screen exists (/students/registration); scheduled + opened through the API to keep the journey on one browser path");
      let w = await admin.call("POST", `${S}/institutions/${need(ids.id, "institution")}/registration-windows`, { academicTermId: ids.termIds[0], name: "Term 1 registration", opensOn: day(0), closesOn: day(20), minCredits: 16, maxCredits: 28 });
      e(`window ${w.id} → ${w.status}`);
      w = await admin.call("POST", `${S}/registration-windows/${w.id}/open`, {}, w.version);
      check(w.status === "open", "window open", e);
      ids.windowId = w.id;
      db(e, "window", `SELECT status FROM student.registration_window WHERE id = ${q(w.id)}`, (v) => v === "open");
      audit(e, w.id, [], "registration window");
    });

    await step("2 Enrolment", "Seed student Isha registered by the academic office (CS101, MD101, EL101 → decided) and seated in CS101-T1 / A", "API (office registration on the student's behalf)", async (e) => {
      // Without a registration the office could only seat her with an eligibility override, which
      // in human approval mode waits for a decision that has no endpoint yet (DEF-P1-DEMO-006).
      need(ids.windowId, "open window");
      let reg = await admin.call("POST", `${S}/students/${need(ids.isha?.id, "seed student Isha")}/registrations`, { academicTermId: ids.termIds[0], lines: ["CS101", "MD101", "EL101"].map((slot) => ({ curriculumComponentId: ids.components[slot] })) });
      check(reg.lines?.length === 3, `registration ${reg.id} drafted with 3 lines`, e);
      reg = await admin.call("POST", `${S}/registrations/${reg.id}/submit`, {}, reg.version);
      check(reg.status === "submitted", `submitted — pending a decision (${reg.status})`, e);
      reg = await approve("registration", reg.id, { client: admin, selfCheck: true, ev: e, step: "2 Enrolment: office registration of Isha decided by a second administrator (maker refused)" });
      const seat = await admin.call("POST", `${A}/sections/${ids.sections.CS101}/enrolments`, { studentId: ids.isha.id });
      check(seat.status === "enrolled", `Isha seated in CS101-T1/A (enrolment ${seat.id}, origin ${seat.origin})`, e);
      db(e, "Isha's registration", `SELECT status FROM student.registration WHERE id = ${q(reg.id)}`, (v) => v === "registered");
    });

    await step("2 Enrolment", "Student self-registration (CS101, MD101, EL101) → submitted → registered", "browser (student /registration)", async (e) => {
      need(ids.windowId, "open window");
      const page = P.student;
      await go(page, "/registration");
      await page.locator('[data-form="new-registration"]').waitFor();
      for (const slot of ["CS101", "MD101", "EL101"]) await page.check(`[data-component="${slot}"]`);
      let r = await ui(page, "POST", /\/student\/me\/registrations$/, () => page.click('[data-action="registration-create"]'), e);
      check(r.status === 201 && r.body.lines?.length === 3, "registration created with 3 lines", e);
      ids.registrationId = r.body.id;
      await page.locator("[data-registration]").waitFor();
      r = await ui(page, "POST", /\/student\/me\/registrations\/[^/]+\/submit$/, () => page.click('[data-action="registration-submit"]'), e);
      check(r.status === 200, "registration submitted", e);
      check(r.body?.status === "submitted", `registration pending a decision (${r.body?.status})`, e);
      const reg = await approve("registration", ids.registrationId, { client: student, ev: e, step: "2 Enrolment: academic office decides the registration" });
      e(`approve(registration) → ${reg.status}`);
      await go(page, "/registration");
      e(`student sees the registration (${await snap(page, "2-student-registration")})`);
      noServerErrors(e);
      db(e, "registration", `SELECT status FROM student.registration WHERE id = ${q(ids.registrationId)}`, (v) => v === "registered");
      db(e, "lines", `SELECT string_agg(slot_code || ':' || status, ',' ORDER BY slot_code) FROM student.registration_line WHERE registration_id = ${q(ids.registrationId)}`, (v) => v === "CS101:registered,EL101:registered,MD101:registered");
      audit(e, ids.registrationId, [], "registration");
      await sees(e, "admin", admin, `${S}/students/${ids.aarav.id}`, 200);
      await sees(e, "student", student, `${S}/me/registrations/${ids.registrationId}`, 200, (j) => j.status === "registered");
      await sees(e, "faculty", faculty, `${S}/me/registrations/${ids.registrationId}`, 403);
    });

    await step("2 Enrolment", "Course enrolment: the student enrols in CS101-T1 / A from Courses", "browser (student /courses)", async (e) => {
      const page = P.student;
      await go(page, "/courses");
      const open = page.locator('[data-open-course="CS101-T1"]');
      await open.waitFor();
      const r = await ui(page, "POST", /\/academic\/me\/enrolments$/, () => open.locator('[data-action="course-enrol"]').click(), e);
      check(r.status === 201 && r.body.status === "enrolled" && r.body.sectionId === ids.sections.CS101, "seated in CS101-T1 / A", e);
      ids.enrolmentId = r.body.id;
      await page.locator('[data-my-course="CS101-T1"]').waitFor();
      e(`student sees CS101-T1 in My courses (${await snap(page, "2-student-courses")})`);
      noServerErrors(e);
      db(e, "section enrolment", `SELECT status || '|' || student_id FROM academic.section_enrolment WHERE id = ${q(ids.enrolmentId)}`, (v) => v === `enrolled|${ids.aarav.id}`);
      db(e, "CS101/A roster", `SELECT count(*) FROM academic.section_enrolment WHERE section_id = ${q(ids.sections.CS101)} AND status = 'enrolled'`, (v) => v === "2");
      audit(e, ids.enrolmentId, [], "section enrolment");
      await sees(e, "admin", admin, `${A}/sections/${ids.sections.CS101}/enrolments`, 200, (j) => (j.items ?? j).some((x) => x.studentId === ids.aarav.id), "roster");
      await sees(e, "faculty", faculty, `${A}/sections/${ids.sections.CS101}/enrolments`, 200, (j) => (j.items ?? j).some((x) => x.studentId === ids.aarav.id), "own section roster");
      await sees(e, "student", student, `${A}/me/courses`, 200, (j) => JSON.stringify(j).includes("CS101-T1"));
      const fpage = P.faculty;
      await go(fpage, "/classes");
      await fpage.getByText("CS101").first().waitFor();
      e(`faculty sees CS101 in My classes (${await snap(fpage, "2-faculty-classes")})`);
    });

    // ============================================================ 3. Attendance (browser: faculty)
    await step("3 Attendance", "Faculty: session → quick-grid attendance (Isha absent) → submit → finalised", "browser (faculty /attendance)", async (e) => {
      need(ids.enrolmentId, "seated student");
      const page = P.faculty;
      await go(page, "/attendance");
      let r = await ui(page, "POST", /\/academic\/sections\/[^/]+\/sessions$/, async () => {
        await page.locator("[data-new-session]").first().click();
        await fillForm(page, "new-session", { topic: "Recursion" });
      }, e);
      check(r.status === 201 && r.body.sectionId === ids.sections.CS101, "session scheduled on CS101-T1 / A", e);
      ids.teachingSessionId = r.body.id;
      r = await ui(page, "POST", /\/academic\/sessions\/[^/]+\/start$/, () => page.locator("[data-session-start]").click(), e);
      check(r.status === 200 && ["started", "in_progress"].includes(r.body.status), "session started", e);
      const tiles = page.locator("[data-attendance-grid] [data-student]");
      await tiles.first().waitFor();
      check((await tiles.count()) === 2, "quick grid lists the 2 seated students", e);
      const names = await tiles.allTextContents();
      const ishaIdx = names.findIndex((t) => t.includes("Isha"));
      check(ishaIdx >= 0, "Isha in the grid", e);
      await tiles.nth(ishaIdx).click();
      check((await tiles.nth(ishaIdx).getAttribute("data-mark")) === "absent", "one tap marks Isha absent", e);
      await snap(page, "3-faculty-attendance-grid");
      r = await ui(page, "POST", /\/academic\/sessions\/[^/]+\/finalise-attendance$/, async () => {
        await page.locator("[data-submit-attendance]").first().click();
        await fillForm(page, "attendance-submit", {});
      }, e);
      check(r.status === 200 && r.body.attendanceStatus === "finalised", "attendance finalised", e);
      const marks = Object.fromEntries((r.body.records ?? []).map((x) => [x.studentId, x.mark]));
      check(marks[ids.aarav.id] === "present" && marks[ids.isha.id] === "absent", "Aarav present, Isha absent", e);
      await page.locator("[data-session-row]", { hasText: "Recursion" }).filter({ hasText: "Finalised" }).first().waitFor();
      e(`faculty sees the session Finalised (${await snap(page, "3-faculty-attendance-finalised")})`);
      noServerErrors(e);
      db(e, "teaching session", `SELECT status || '|' || attendance_status FROM academic.teaching_session WHERE id = ${q(ids.teachingSessionId)}`, (v) => v.endsWith("|finalised"));
      db(e, "attendance records", `SELECT string_agg(student_number || ':' || mark, ',' ORDER BY student_number) FROM academic.attendance_record WHERE session_id = ${q(ids.teachingSessionId)}`, (v) => v.includes(":present") && v.includes(":absent") && v.split(",").length === 2);
      // academic.attendance_audit is the append-only history of changes to FINALISED marks
      // (corrections, migration 20261025000011); a fresh finalisation has none.
      db(e, "attendance_audit (finalised-mark changes)", `SELECT count(*) FROM academic.attendance_audit a JOIN academic.attendance_record r ON r.id = a.record_id WHERE r.session_id = ${q(ids.teachingSessionId)}`, (v) => v === "0");
      audit(e, ids.teachingSessionId, [], "teaching session");
      R.snapshot.attendance = psql(`SELECT string_agg(student_id || ':' || mark, ',' ORDER BY student_id) FROM academic.attendance_record WHERE session_id = ${q(ids.teachingSessionId)}`);
      // visibility
      await sees(e, "admin", admin, `${A}/sections/${ids.sections.CS101}/attendance-summary`, 200, null, "section summary");
      await sees(e, "student", student, `${A}/me/attendance`, 200, (j) => JSON.stringify(j).includes(ids.teachingSessionId) || JSON.stringify(j).includes("CS101"), "own attendance");
      await sees(e, "student", student, `${A}/sessions/${ids.teachingSessionId}`, 403);
      const sp = P.student;
      await go(sp, "/timetable");
      await sp.getByText("1 / 1").first().waitFor();
      e(`student sees 1 / 1 attended (${await snap(sp, "3-student-attendance")})`);
    });

    await step("3 Attendance", "Negatives: re-starting a finalised session (409); a student cannot record attendance (403)", "API", async (e) => {
      const s = await faculty.get(`${A}/sessions/${need(ids.teachingSessionId, "session")}`);
      let r = await faculty.raw("POST", `${A}/sessions/${s.id}/start`, {}, s.version);
      e(`faculty POST /sessions/{id}/start on ${s.status}/${s.attendanceStatus} → ${r.status} ${r.json?.detail ?? ""}`);
      check(r.status === 409, "invalid transition → 409", e);
      r = await faculty.raw("POST", `${A}/sessions/${s.id}/attendance`, { records: [{ studentId: ids.isha.id, mark: "present" }] }, s.version);
      e(`faculty re-marks after finalisation → ${r.status} ${r.json?.detail ?? ""}`);
      check(r.status === 409, "finalised attendance is immutable → 409", e);
      r = await student.raw("POST", `${A}/sessions/${s.id}/attendance`, { records: [{ studentId: ids.aarav.id, mark: "present" }] }, s.version);
      e(`student POST /sessions/{id}/attendance → ${r.status}`);
      check(r.status === 403, "student → 403", e);
      db(e, "Isha still absent", `SELECT mark FROM academic.attendance_record WHERE session_id = ${q(s.id)} AND student_id = ${q(ids.isha.id)}`, (v) => v === "absent");
    });

    // ============================================================ 4. Fees
    await step("4 Fees", "Charges generated from the published fee structure BTECH-Y1 for the student", "browser (admin /finance/charges)", async (e) => {
      const page = P.administrator;
      await go(page, "/finance/charges");
      const structures = tile(page, "Fee structures");
      await row(page, "BTECH-Y1", structures).waitFor();
      const r = await ui(page, "POST", new RegExp(`/finance/fee-structures/${need(ids.feeStructureId, "fee structure")}/generate-charges$`), async () => {
        await rowAction(page, "BTECH-Y1", "generate-charges", structures);
        await fillForm(page, "generate", { [`studentIds:${need(ids.aarav?.id, "student")}`]: true });
      }, e);
      check(r.status === 200 && r.body.created?.length === 1, "one charge generated", e);
      const c = r.body.created[0];
      check(c.studentId === ids.aarav.id && c.status === "published", `charge ${c.chargeNumber} published for the student`, e);
      ids.chargeId = c.chargeId;
      ids.chargeNumber = c.chargeNumber;
      await page.locator(`[data-charge="${c.chargeNumber}"]`).waitFor();
      e(`admin sees ${c.chargeNumber} (${await snap(page, "4-admin-charge")})`);
      noServerErrors(e);
      const ch = db(e, "charge", `SELECT status || '|' || total_amount || '|' || outstanding_amount || '|' || student_account_id FROM finance.charge WHERE id = ${q(c.chargeId)}`, (v) => v.startsWith("published|90000.00|90000.00|"));
      ids.studentAccountId = ch.split("|")[3];
      db(e, "publication journal balanced", `SELECT sum(total_debit) || '=' || sum(total_credit) FROM finance.journal_entry WHERE source_id = ${q(c.chargeId)}`, (v) => /^(\d+\.\d+)=\1$/.test(v));
      audit(e, c.chargeId, [], "charge");
      await sees(e, "student", student, `${F}/me/accounts`, 200, (j) => JSON.stringify(j).includes(c.chargeNumber), "own charge");
      await sees(e, "faculty", faculty, `${F}/student-accounts/${ids.studentAccountId}`, 403);
    });

    await step("4 Fees", "Counter payment ₹30,000 recorded with receipt (allocation + posting)", "browser (admin /finance/collections)", async (e) => {
      const page = P.administrator;
      await go(page, "/finance/collections");
      const r = await ui(page, "POST", /\/finance\/student-accounts\/[^/]+\/payments$/, async () => {
        await page.locator("[data-record-payment]").click();
        await fillForm(page, "record-payment", { accountId: "Aarav", channel: "cash", amount: "30000.00", externalRef: `CTR-${ids.suffix}-001` });
      }, e);
      check(r.status === 201 && r.body.status === "completed" && r.body.receipt?.receiptNumber, `payment ${r.body?.paymentNumber} completed, receipt ${r.body?.receipt?.receiptNumber}`, e);
      check(r.body.studentAccountId === need(ids.studentAccountId, "student account"), "on the student's account", e);
      ids.counterPaymentId = r.body.id;
      await page.locator(`[data-payment="${r.body.paymentNumber}"]`).waitFor();
      e(`admin sees ${r.body.paymentNumber} (${await snap(page, "4-admin-counter-payment")})`);
      noServerErrors(e);
      db(e, "payment", `SELECT status || '|' || amount || '|' || channel FROM finance.payment WHERE id = ${q(ids.counterPaymentId)}`, (v) => v === "completed|30000.00|cash");
      db(e, "receipt", `SELECT count(*) FROM finance.receipt WHERE payment_id = ${q(ids.counterPaymentId)}`, (v) => v === "1");
      db(e, "charge outstanding", `SELECT outstanding_amount FROM finance.charge WHERE id = ${q(ids.chargeId)}`, (v) => v === "60000.00");
      audit(e, ids.counterPaymentId, [], "payment");
    });

    await step("4 Fees", "Negative: duplicate payment Idempotency-Key — replay returns the same payment, a different body with the key is rejected (422), nothing is double-charged", "API (admin, explicit key)", async (e) => {
      const key = `journey-pay-${randomUUID()}`;
      const body = { amount: "10000.00", channel: "bank_transfer", externalRef: `UTR-${ids.suffix}-777`, allocations: [{ chargeId: need(ids.chargeId, "charge"), amount: "10000.00" }] };
      const p = `${F}/student-accounts/${need(ids.studentAccountId, "account")}/payments`;
      const first = await admin.raw("POST", p, body, null, { key });
      e(`first POST (key ${key.slice(0, 20)}…) → ${first.status} ${first.json?.paymentNumber ?? ""} ${first.json?.status ?? ""}`);
      check(first.status === 201 && first.json.status === "completed", "first payment completed", e);
      ids.bankPaymentId = first.json.id;
      const replay = await admin.raw("POST", p, body, null, { key });
      e(`replay same key + same body → ${replay.status}, Idempotency-Replayed: ${replay.headers.get("idempotency-replayed")}, payment ${replay.json?.id}`);
      check(replay.status === 201 && replay.json.id === first.json.id && replay.headers.get("idempotency-replayed") === "true", "replay returns the stored response, no new payment", e);
      const other = await admin.raw("POST", p, { ...body, amount: "1.00", allocations: [{ chargeId: ids.chargeId, amount: "1.00" }] }, null, { key });
      e(`same key + different body → ${other.status} ${other.json?.title ?? ""} ${other.json?.detail ?? ""}`);
      check(other.status === 422, "duplicate key with a different payload rejected (422)", e);
      db(e, "payments on the account", `SELECT count(*) || '|' || sum(amount) FROM finance.payment WHERE student_account_id = ${q(ids.studentAccountId)} AND status = 'completed'`, (v) => v === "2|40000.00");
      db(e, "charge outstanding", `SELECT outstanding_amount FROM finance.charge WHERE id = ${q(ids.chargeId)}`, (v) => v === "50000.00");
    });

    await step("4 Fees", "Negative: wrong role — student and faculty cannot record counter payments (403)", "API", async (e) => {
      const p = `${F}/student-accounts/${need(ids.studentAccountId, "account")}/payments`;
      const body = { amount: "1.00", channel: "cash" };
      for (const [who, c] of [
        ["student", student],
        ["faculty", faculty],
      ]) {
        const r = await c.raw("POST", p, body);
        e(`${who} POST /student-accounts/{id}/payments → ${r.status}`);
        check(r.status === 403, `${who} → 403`, e);
      }
      db(e, "no extra payment", `SELECT count(*) FROM finance.payment WHERE student_account_id = ${q(ids.studentAccountId)} AND amount = 1.00`, (v) => v === "0");
    });

    await step("4 Fees", "Student pays the remaining ₹50,000 online (mock gateway) → receipt → nothing outstanding", "browser (student /fees)", async (e) => {
      const page = P.student;
      await go(page, "/fees");
      await page.locator(`[data-pay="${need(ids.chargeNumber, "charge")}"]`).waitFor();
      const before = (await page.locator("[data-outstanding]").textContent())?.trim();
      e(`student sees outstanding ${before}`);
      check(before === "₹50,000", "₹50,000 outstanding before paying", e);
      await page.locator(`[data-pay="${ids.chargeNumber}"]`).click();
      await page.locator('[data-form="pay"]').waitFor();
      await page.locator('[data-action="pay-partial"]').click();
      await page.locator('[data-form="pay"] input[name="amount"]').fill("50000.00");
      let r = await ui(page, "POST", /\/finance\/me\/payments$/, () => page.locator('[data-action="pay-now"]').click(), e);
      check(r.status === 201 && r.body.channel === "gateway", "gateway order created (pending payment)", e);
      ids.onlinePaymentId = r.body.id;
      await page.locator("[data-checkout]").waitFor();
      await snap(page, "4-student-checkout");
      r = await ui(page, "POST", /\/finance\/me\/payments\/[^/]+\/confirm$/, () => page.locator('[data-action="checkout-pay"]').click(), e);
      check(r.status === 200 && r.body.status === "completed" && r.body.receipt?.receiptNumber, `online payment completed, receipt ${r.body?.receipt?.receiptNumber}`, e);
      await page.locator("[data-receipt-number]").first().waitFor();
      await page.locator('[data-action="pay-done"]').click();
      await settle(page);
      // With nothing due the Invoices tile swaps the amount for [data-outstanding-none].
      const after = (await page.locator("[data-outstanding-none]").textContent({ timeout: 15000 }))?.trim();
      e(`student sees “${after}” (${await snap(page, "4-student-fees-paid")})`);
      check(after === "You have no dues right now" && (await page.locator("[data-outstanding]").count()) === 0, "nothing outstanding on the student's Fees screen", e);
      noServerErrors(e);
      db(e, "charge settled", `SELECT status || '|' || outstanding_amount || '|' || settled_amount FROM finance.charge WHERE id = ${q(ids.chargeId)}`, (v) => v.split("|")[1] === "0.00" && v.split("|")[2] === "90000.00");
      db(e, "payments", `SELECT string_agg(channel || ':' || amount || ':' || status, ',' ORDER BY created_at) FROM finance.payment WHERE student_account_id = ${q(ids.studentAccountId)}`, (v) => v === "cash:30000.00:completed,bank_transfer:10000.00:completed,gateway:50000.00:completed");
      db(e, "receipts", `SELECT count(*) FROM finance.receipt r JOIN finance.payment p ON p.id = r.payment_id WHERE p.student_account_id = ${q(ids.studentAccountId)}`, (v) => v === "3");
      db(e, "ledger balanced (institution)", `SELECT sum(total_debit) || '=' || sum(total_credit) FROM finance.journal_entry WHERE institution_id = ${q(ids.id)}`, (v) => /^(\d+\.\d+)=\1$/.test(v));
      audit(e, ids.onlinePaymentId, [], "online payment");
      R.snapshot.fees = psql(`SELECT outstanding_amount || '|' || settled_amount || '|' || (SELECT count(*) FROM finance.payment WHERE student_account_id = c.student_account_id AND status = 'completed') FROM finance.charge c WHERE id = ${q(ids.chargeId)}`);
      await sees(e, "admin", admin, `${F}/student-accounts/${ids.studentAccountId}`, 200, (j) => Number(j.balance?.outstanding ?? j.account?.balance?.outstanding ?? NaN) === 0, "balance 0");
      const fp = P.faculty;
      await go(fp, "/fees");
      await fp.getByText("This screen isn’t available").first().waitFor();
      e(`faculty has no Fees screen (${await snap(fp, "4-faculty-fees-unavailable")})`);
    });

    await step("4 Fees", "Fee adjustment (concession ₹5,000) on a second student's charge: the maker's own decision is refused (403), the finance approver posts it", "API (no adjustment decision screen driven)", async (e) => {
      gap("4 Fees: fee adjustment + decision", "driven through the API (finance approver persona has no browser context in the journey)");
      const g = await admin.call("POST", `${F}/fee-structures/${need(ids.feeStructureId, "fee structure")}/generate-charges`, { studentIds: [need(ids.kiran?.id, "seed student Kiran")] });
      check(g.created?.length === 1 && g.created[0].status === "published", `charge ${g.created?.[0]?.chargeNumber} published for Kiran`, e);
      const kc = g.created[0];
      ids.kiranChargeId = kc.chargeId;
      let adj = await admin.call("POST", `${F}/charges/${kc.chargeId}/adjustments`, { kind: "concession", amount: "5000.00", reason: "Merit concession (journey maker-checker test)" });
      check(adj.status === "draft", `adjustment ${adj.adjustmentNumber ?? adj.id} drafted`, e);
      adj = await admin.call("POST", `${F}/adjustments/${adj.id}/submit`, {}, adj.version);
      check(adj.status === "pending_approval", `submitted — pending approval (${adj.status})`, e);
      db(e, "charge untouched while pending", `SELECT outstanding_amount FROM finance.charge WHERE id = ${q(kc.chargeId)}`, (v) => v === "90000.00");
      adj = await approve("adjustment", adj.id, { client: admin, selfCheck: true, ev: e, step: "4 Fees: finance approver posts the concession (maker refused)" });
      ids.adjustmentId = adj.id;
      db(e, "adjustment decided by the finance approver", `SELECT status || '|' || coalesce(decided_by, '') || '|' || coalesce(created_by, '') FROM finance.adjustment WHERE id = ${q(adj.id)}`, (v) => v === `posted|${CHECKERS.finance_approver.subject}|${admin.subject}`);
      db(e, "charge reduced by the concession", `SELECT outstanding_amount FROM finance.charge WHERE id = ${q(kc.chargeId)}`, (v) => v === "85000.00");
      db(e, "adjustment journal balanced", `SELECT count(*) || '|' || coalesce(sum(total_debit), 0) || '=' || coalesce(sum(total_credit), 0) FROM finance.journal_entry WHERE source_id = ${q(adj.id)}`, (v) => /^[1-9]\d*\|(\d+\.\d+)=\1$/.test(v));
      audit(e, adj.id, [], "adjustment");
    });

    // ============================================================ 5. Examination
    await step("5 Examination", "Registrations from the rosters and hall tickets (dues clear after the payments)", "browser (admin /examinations/registrations)", async (e) => {
      need(ids.sessionId, "exam session");
      const page = P.administrator;
      await go(page, "/examinations/registrations");
      let r = await ui(page, "POST", /\/exams\/exam-sessions\/[^/]+\/registrations\/bulk$/, () => page.click("[data-bulk-register]"), e);
      check(r.status === 200 && r.body.processed >= 1, `bulk registration processed ${r.body?.processed}, failed ${JSON.stringify(r.body?.failed ?? [])}`, e);
      const regs = await admin.get(`${X}/exam-sessions/${ids.sessionId}/registrations`);
      const reg = (regs.items ?? []).find((x) => x.studentId === ids.aarav.id);
      check(reg, "the student is registered for the session", e);
      ids.examRegistrationId = reg.id;
      r = await ui(page, "POST", /\/exams\/exam-sessions\/[^/]+\/hall-tickets\/issue$/, () => page.click("[data-issue-tickets]"), e);
      check(r.status === 200, `hall tickets issued (processed ${r.body?.processed})`, e);
      await page.locator("[data-ticket]").first().waitFor();
      e(`admin sees registrations + hall tickets (${await snap(page, "5-admin-registrations")})`);
      noServerErrors(e);
      db(e, "exam registration", `SELECT status FROM exams.exam_registration WHERE id = ${q(reg.id)}`, (v) => v !== "" && !["withdrawn", "cancelled", "rejected"].includes(v));
      db(e, "hall ticket", `SELECT string_agg(status, ',') FROM exams.hall_ticket WHERE registration_id = ${q(reg.id)}`, (v) => v !== "" && !v.split(",").includes("withheld"));
      audit(e, reg.id, [], "exam registration");
      await sees(e, "student", student, `${X}/me/hall-tickets`, 200, (j) => (j.items ?? j).length >= 1, "own hall ticket");
      await sees(e, "faculty", faculty, `${X}/exam-sessions/${ids.sessionId}/registrations`, 200, null, "exam.read");
    });

    await step("5 Examination", "Conduct: sitting started and completed — the student present, paper conducted", "API (no conduct screen — gap)", async (e) => {
      gap("5 Examination: exam conduct (start / attendance / complete)", "no screen: conduct records are API-only (exams e2e does the same)");
      let c = await admin.call("POST", `${X}/exam-sittings/${need(ids.sittingId, "sitting")}/conduct`, {});
      c = await admin.call("POST", `${X}/conduct-records/${c.id}/start`, {}, c.version);
      c = await admin.call("POST", `${X}/conduct-records/${c.id}/complete`, { attendance: [{ registrationId: need(ids.examRegistrationId, "registration"), outcome: "present" }] }, c.version);
      check(c.status === "completed" && c.presentCount >= 1, `conduct completed (${c.presentCount} present)`, e);
      const paper = await admin.get(`${X}/exam-papers/${ids.paperId}`);
      check(paper.status === "conducted", `paper conducted (${paper.status})`, e);
      db(e, "conduct attendance", `SELECT string_agg(outcome, ',') FROM exams.conduct_attendance WHERE registration_id = ${q(ids.examRegistrationId)}`, (v) => v.includes("present"));
      audit(e, c.id, [], "conduct record");
    });

    await step("5 Examination", "Mark sheet opened by the exam cell", "API (screen exists: /examinations/marks — not driven)", async (e) => {
      gap("5 Examination: open mark sheet", "screen exists (/examinations/marks “Open sheet”); opened through the API so marks entry stays the faculty's browser step");
      const sheet = await admin.call("POST", `${X}/exam-papers/${need(ids.paperId, "paper")}/mark-sheet`, {});
      check(["planned", "assigned", "in_evaluation", "pending"].includes(sheet.status), `mark sheet ${sheet.id} ${sheet.status}`, e);
      ids.markSheetId = sheet.id;
      db(e, "paper in evaluation", `SELECT status FROM exams.exam_paper WHERE id = ${q(ids.paperId)}`, (v) => v === "evaluation");
      audit(e, sheet.id, [], "mark sheet");
    });

    await step("5 Examination", "Negative: invalid marks (above the maximum, negative) are rejected (422) and nothing is stored", "API (faculty)", async (e) => {
      const marks = await faculty.get(`${X}/exam-papers/${need(ids.paperId, "paper")}/marks`);
      const m = (marks.items ?? []).find((x) => x.studentNumber === need(ids.aarav?.number, "student number"));
      check(m, "the student's mark row is visible to the teaching faculty", e);
      for (const bad of ["150", "-5"]) {
        const r = await faculty.raw("PUT", `${X}/exam-papers/${ids.paperId}/marks`, { rows: [{ id: m.id, version: m.version, externalMarks: bad }] });
        e(`faculty PUT marks externalMarks=${bad} → ${r.status} ${r.json?.detail ?? r.json?.title ?? ""}`);
        check(r.status === 422, `externalMarks ${bad} → 422`, e);
      }
      const r = await student.raw("PUT", `${X}/exam-papers/${ids.paperId}/marks`, { rows: [{ id: m.id, version: m.version, externalMarks: "99" }] });
      e(`student PUT marks → ${r.status}`);
      check(r.status === 403, "student → 403", e);
      db(e, "no marks stored", `SELECT coalesce(external_marks::text, 'null') FROM exams.paper_mark WHERE id = ${q(m.id)}`, (v) => v === "null");
    });

    await step("5 Examination", "Faculty enters 78 / 100 and submits the mark sheet", "browser (faculty /assessments)", async (e) => {
      const page = P.faculty;
      await go(page, "/assessments");
      const paperRow = page.locator('[data-paper^="CS101"]').first();
      await paperRow.waitFor();
      await paperRow.locator("[data-row-open]").click();
      await settle(page);
      const input = page.locator(`[data-mark-input="${need(ids.aarav?.number, "student number")}"]`);
      await input.waitFor();
      await input.fill("78");
      let r = await ui(page, "PUT", /\/exams\/exam-papers\/[^/]+\/marks$/, () => act(page, page.locator("main"), "marks-save"), e);
      const mine = (r.body?.items ?? []).find((x) => x.studentNumber === ids.aarav.number);
      check(r.status === 200 && Number(mine?.externalMarks) === 78, "78 saved (entered)", e);
      r = await ui(page, "POST", /\/exams\/mark-sheets\/[^/]+\/submit$/, () => act(page, page.locator("main"), "sheet-submit"), e);
      check(r.status === 200 && r.body.status === "evaluated", "mark sheet submitted (evaluated)", e);
      e(`faculty sees the submitted sheet (${await snap(page, "5-faculty-marks-submitted")})`);
      noServerErrors(e);
      db(e, "paper mark", `SELECT external_marks || '|' || status FROM exams.paper_mark WHERE paper_id = ${q(ids.paperId)} AND student_id = ${q(ids.aarav.id)}`, (v) => v === "78.00|submitted");
      db(e, "mark history", `SELECT count(*) FROM exams.paper_mark_history h JOIN exams.paper_mark m ON m.id = h.paper_mark_id WHERE m.paper_id = ${q(ids.paperId)}`, (v) => Number(v) >= 1);
      audit(e, ids.markSheetId, [], "mark sheet");
    });

    await step("5 Examination", "Exam cell verifies → final marks approval → finalised", "API (screen exists: /examinations/marks — not driven)", async (e) => {
      gap("5 Examination: verify / approve / finalise marks", "screen exists (/examinations overview card + Marks tab); driven through the API — the faculty's entry is the browser step");
      let sh = await admin.get(`${X}/mark-sheets/${need(ids.markSheetId, "sheet")}`);
      sh = await admin.call("POST", `${X}/mark-sheets/${sh.id}/verify`, {}, sh.version);
      check(sh.status === "validated", `verified (${sh.status})`, e);
      sh = await admin.call("POST", `${X}/mark-sheets/${sh.id}/request-approval`, {}, sh.version);
      sh = await approve("mark_sheet", sh.id, { client: admin, ev: e, step: "5 Examination: controller of examinations approves final marks" });
      e(`approve(mark_sheet) → ${sh.status}`);
      sh = await admin.call("POST", `${X}/mark-sheets/${sh.id}/finalise`, {}, sh.version);
      check(sh.status === "finalised", "marks finalised", e);
      db(e, "paper", `SELECT status FROM exams.exam_paper WHERE id = ${q(ids.paperId)}`, (v) => v === "marks_finalised");
      db(e, "paper mark final", `SELECT status FROM exams.paper_mark WHERE paper_id = ${q(ids.paperId)} AND student_id = ${q(ids.aarav.id)}`, (v) => v === "finalised");
      audit(e, sh.id, [], "mark sheet");
      await sees(e, "faculty", faculty, `${X}/exam-papers/${ids.paperId}/gradebook`, 200, null, "gradebook");
      await sees(e, "student", student, `${X}/exam-papers/${ids.paperId}/marks`, 403);
    });

    // ============================================================ 6. Result
    await step("6 Result", "Results computed → validated (approved) → finalised → publication authorised → published", "browser (admin /examinations/results)", async (e) => {
      need(ids.markSheetId, "finalised marks");
      const page = P.administrator;
      const main = page.locator("main");
      await go(page, "/examinations/results");
      let r = await ui(page, "POST", /\/exams\/exam-sessions\/[^/]+\/result-batches$/, () => act(page, main, "new-batch"), e);
      check(r.status === 201, "result batch opened", e);
      ids.resultBatchId = r.body.id;
      r = await ui(page, "POST", /\/exams\/result-batches\/[^/]+\/compute$/, () => act(page, main, "batch-compute"), e);
      check(r.status === 200 && r.body.computed >= 1, `computed ${r.body?.computed}`, e);
      r = await ui(page, "POST", /\/exams\/result-batches\/[^/]+\/validate$/, () => act(page, main, "batch-validate"), e);
      check(r.status === 200, "validated — approval requested", e);
      const b = await approve("result_batch", ids.resultBatchId, { client: admin, selfCheck: true, ev: e, step: "6 Result: controller approves the results (maker refused)" });
      e(`approve(result_batch) → ${b.status}`);
      await go(page, "/examinations/results");
      r = await ui(page, "POST", /\/exams\/result-batches\/[^/]+\/finalise$/, () => act(page, main, "batch-finalise"), e);
      check(r.status === 200, `batch finalised (${r.body?.status})`, e);
      r = await ui(page, "POST", /\/exams\/exam-sessions\/[^/]+\/publications$/, () => act(page, main, "new-publication"), e);
      check(r.status === 201, "publication planned", e);
      ids.publicationId = r.body.id;
      r = await ui(page, "POST", /\/exams\/publications\/[^/]+\/authorise$/, () => act(page, main, "publication-authorise"), e);
      check(r.status === 200, "authorisation requested", e);
      const pub = await approve("publication", ids.publicationId, { client: admin, selfCheck: true, ev: e, step: "6 Result: controller authorises publication (maker refused)" });
      e(`approve(publication) → ${pub.status}`);
      await go(page, "/examinations/results");
      r = await ui(page, "POST", /\/exams\/publications\/[^/]+\/publish$/, () => act(page, main, "publication-publish"), e);
      check(r.status === 200 && r.body.published >= 1, `published ${r.body?.published}`, e);
      e(`admin sees published results (${await snap(page, "6-admin-results-published")})`);
      noServerErrors(e);
      const tr = db(e, "term result", `SELECT id || '|' || status || '|' || coalesce(sgpa::text, '') FROM exams.term_result WHERE session_id = ${q(ids.sessionId)} AND student_id = ${q(ids.aarav.id)} AND status = 'published'`, (v) => v.split("|")[1] === "published");
      ids.termResultId = tr.split("|")[0];
      db(e, "result line", `SELECT paper_code || ':' || coalesce(grade, '') || ':' || coalesce(total_marks::text, '') FROM exams.term_result_line WHERE result_id = ${q(ids.termResultId)}`, (v) => v.startsWith("CS101") && v.includes(":A:"));
      // Results are audited per command on their batch and publication (exams results repo:
      // result_batch.compute / validate / finalise, publication.authorise / publish with counts).
      audit(e, ids.resultBatchId, ["exams.result_batch.compute", "exams.result_batch.create", "exams.result_batch.finalise", "exams.result_batch.validate"], "result batch");
      const pa = audit(e, ids.publicationId, ["exams.publication.authorise", "exams.publication.create", "exams.publication.publish"], "publication");
      check(pa.actors.includes(CHECKERS.exam_controller.subject) && !pa.actors.some((a) => a.startsWith("system:")), "publication decided by the controller of examinations (no auto-approval actor)", e);
      const ba = audit(e, ids.resultBatchId, [], "result batch actors");
      check(ba.actors.includes(CHECKERS.exam_controller.subject) && !ba.actors.some((a) => a.startsWith("system:")), "result batch approved by the controller of examinations (no auto-approval actor)", e);
      R.snapshot.results = psql(`SELECT r.status || '|' || coalesce(r.sgpa::text, '') || '|' || string_agg(l.grade || ':' || coalesce(l.total_marks::text, ''), ',') FROM exams.term_result r JOIN exams.term_result_line l ON l.result_id = r.id WHERE r.id = ${q(ids.termResultId)} GROUP BY r.status, r.sgpa`);
    });

    await step("6 Result", "Visibility: student sees the published result, faculty the gradebook, admin the session results", "browser (student /results) + API", async (e) => {
      need(ids.termResultId, "published result");
      const mine = await sees(e, "student", student, `${X}/me/results`, 200, (j) => (j.items ?? []).some((x) => x.id === ids.termResultId), "own published result");
      const line = mine.items.find((x) => x.id === ids.termResultId).lines[0];
      e(`student result: ${line.paperCode} ${line.totalMarks ?? line.externalMarks} → grade ${line.grade} (${line.gradePoint})`);
      check(line.grade === "A", "78 % → grade A on the ten-point scheme", e);
      await sees(e, "admin", admin, `${X}/exam-sessions/${ids.sessionId}/results`, 200, (j) => JSON.stringify(j).includes(ids.termResultId));
      await sees(e, "faculty", faculty, `${X}/exam-papers/${ids.paperId}/gradebook`, 200);
      await sees(e, "faculty", faculty, `${X}/me/results`, 403);
      const sp = P.student;
      await go(sp, "/results");
      await sp.locator("[data-result]").first().waitFor();
      e(`student Results screen (${await snap(sp, "6-student-results")})`);
      noServerErrors(e);
    });

    // ============================================================ 8. Data import (Part 2 · S4-ui-import)
    await step("8 Data import", "Admin Data import: a 2.2 MiB students CSV (above the 2 MiB default body limit, through nginx) → suggested mapping → dry run → commit → reconciliation", "browser (admin /data-import)", async (e) => {
      need(ids.id, "institution");
      const page = P.administrator;
      // The screen works on the selected institution (picker state in localStorage).
      await page.evaluate((id) => window.localStorage.setItem("sutra.selectedInstitution", id), ids.id);
      await go(page, "/data-import");
      await page.locator('[data-screen="data-import"]').waitFor();
      await page.selectOption('select[name="entity"]', "students");
      const tag = `IMP${Date.now().toString(36).toUpperCase().slice(-5)}`;
      const notes = "n".repeat(560 * 1024);
      const lines = ["student_number,given_name,family_name,date_of_birth,programme_code,notes"];
      for (let i = 1; i <= 4; i++) lines.push(`${tag}-${i},Migrated${i},Student,2006-05-0${i},BTECH-CSE,${notes}`);
      const file = Buffer.from(`${lines.join("\n")}\n`);
      check(file.length > 2 * 1024 * 1024, `file is ${file.length} bytes (> 2 MiB)`, e);
      await page.locator('input[name="file"]').setInputFiles({ name: "legacy-students.csv", mimeType: "text/csv", buffer: file });
      let r = await ui(page, "POST", /^\/api\/v1\/platform\/imports$/, () => page.click("[data-import-upload]"), e);
      check(r.status === 201, `upload accepted through nginx (${r.status})`, e);
      ids.importJobId = r.body.id;
      check(r.body.fileBytes === file.length && r.body.summary.rowsInFile === 4, "job holds the whole file (4 rows)", e);
      await page.locator("[data-import-mapping]").waitFor();
      r = await ui(page, "POST", /\/api\/v1\/platform\/imports\/[^/]+\/dry-run$/, () => page.click("[data-import-dry-run]"), e);
      check(r.status === 200 && r.body.status === "validated" && r.body.actions.create === 4, `dry run: ${r.body?.status}, ${r.body?.actions?.create} to create`, e);
      await page.locator('[data-import-dryrun="validated"]').waitFor();
      r = await ui(page, "POST", /\/api\/v1\/platform\/imports\/[^/]+\/commit$/, () => page.click("[data-import-commit]"), e);
      check(r.status === 200 && r.body.status === "committed" && r.body.summary.created === 4, `committed: ${r.body?.summary?.created} created`, e);
      await page.locator('[data-import-reconciliation="committed"]').waitFor();
      e(`admin sees the reconciliation (${await snap(page, "8-admin-data-import")})`);
      noServerErrors(e);
      db(e, "imported students", `SELECT count(*) FROM student.student WHERE student_number LIKE ${q(`${tag}-%`)}`, (v) => v === "4");
      db(e, "import job", `SELECT status || '|' || file_bytes FROM platform.import_job WHERE id = ${q(ids.importJobId)}`, (v) => v === `committed|${file.length}`);
      audit(e, ids.importJobId, ["platform.import.upload", "platform.import.dry_run", "platform.import.commit"], "import job");
      await sees(e, "faculty", faculty, `/api/v1/platform/imports/${ids.importJobId}`, 403);
    });

    R.snapshot.pgStartedAt = psql("SELECT pg_postmaster_start_time()");
    state.snapshot = R.snapshot;
    save();
  }

  // ============================================================ persistence (after docker compose restart postgres api)
  if (PHASE === "persistence") {
    const snapBefore = R.snapshot ?? {};
    await step("7 Persistence", "The stack really restarted (postgres start time moved, API healthy)", "DB + API", async (e) => {
      const now = psql("SELECT pg_postmaster_start_time()");
      e(`postgres started before: ${snapBefore.pgStartedAt} · now: ${now}`);
      check(snapBefore.pgStartedAt && now !== snapBefore.pgStartedAt, "postgres was restarted", e);
      const r = await fetch(`${BASE}/api/v1/auth/csrf`);
      check(r.status === 200, "API reachable through the web proxy after the restart", e);
    });
    await step("7 Persistence", "Attendance persists (API + DB + student screen)", "API + DB + browser", async (e) => {
      const s = await faculty.get(`${A}/sessions/${need(ids.teachingSessionId, "session id from the journey phase")}`);
      check(s.attendanceStatus === "finalised", `session still finalised (${s.attendanceStatus})`, e);
      const now = db(e, "attendance records", `SELECT string_agg(student_id || ':' || mark, ',' ORDER BY student_id) FROM academic.attendance_record WHERE session_id = ${q(ids.teachingSessionId)}`);
      check(now === snapBefore.attendance, "identical to the pre-restart snapshot", e);
      await sees(e, "student", student, `${A}/me/attendance`, 200);
      await go(P.student, "/timetable");
      await P.student.getByText("1 / 1").first().waitFor();
      e(`student still sees 1 / 1 (${await snap(P.student, "7-student-attendance-after-restart")})`);
    });
    await step("7 Persistence", "Fees persist (charge settled, 3 completed payments, receipts)", "API + DB + browser", async (e) => {
      const now = db(e, "charge + payments", `SELECT outstanding_amount || '|' || settled_amount || '|' || (SELECT count(*) FROM finance.payment WHERE student_account_id = c.student_account_id AND status = 'completed') FROM finance.charge c WHERE id = ${q(need(ids.chargeId, "charge id"))}`);
      check(now === snapBefore.fees, `identical to the pre-restart snapshot (${snapBefore.fees})`, e);
      await sees(e, "student", student, `${F}/me/accounts`, 200, (j) => JSON.stringify(j).includes(ids.chargeNumber));
      await go(P.student, "/fees");
      const out = (await P.student.locator("[data-outstanding-none]").textContent({ timeout: 15000 }))?.trim();
      check(out === "You have no dues right now" && (await P.student.locator("[data-outstanding]").count()) === 0, `student still sees no dues (${out})`, e);
      e(`(${await snap(P.student, "7-student-fees-after-restart")})`);
    });
    await step("7 Persistence", "Results persist (published term result and grade)", "API + DB + browser", async (e) => {
      const now = db(e, "term result", `SELECT r.status || '|' || coalesce(r.sgpa::text, '') || '|' || string_agg(l.grade || ':' || coalesce(l.total_marks::text, ''), ',') FROM exams.term_result r JOIN exams.term_result_line l ON l.result_id = r.id WHERE r.id = ${q(need(ids.termResultId, "term result id"))} GROUP BY r.status, r.sgpa`);
      check(now === snapBefore.results, `identical to the pre-restart snapshot (${snapBefore.results})`, e);
      await sees(e, "student", student, `${X}/me/results`, 200, (j) => (j.items ?? []).some((x) => x.id === ids.termResultId));
      await go(P.student, "/results");
      await P.student.locator("[data-result]").first().waitFor();
      e(`(${await snap(P.student, "7-student-results-after-restart")})`);
    });
  }
} catch (err) {
  R.steps.push({ phase: PHASE, name: "setup", via: "", status: "fail", evidence: [String(err instanceof Error ? err.stack : err).slice(0, 1500)] });
  console.error("[journey] FATAL", err);
} finally {
  save();
  await browser?.close().catch(() => {});
}

const mine = PHASE === "persistence" ? R.steps.filter((s) => s.phase === "7 Persistence" || s.phase === "persistence") : R.steps;
const failed = mine.filter((s) => s.status === "fail");
const known = mine.filter((s) => s.status === "known");
log(`${mine.length} step(s): ${mine.filter((s) => s.status === "pass").length} pass, ${failed.length} fail, ${known.length} known-defect`);
for (const s of [...failed, ...known]) log(`  ${s.status.toUpperCase()} [${s.phase}] ${s.name}: ${(s.evidence ?? []).filter((x) => x.startsWith("ERROR")).join(" ")}`);
process.exit(failed.length ? 1 : 0);
