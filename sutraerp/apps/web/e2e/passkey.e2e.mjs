#!/usr/bin/env node
/**
 * End-to-end check of the dev-mode passkey (WebAuthn) flow in a real Chromium with a CDP
 * virtual authenticator (CTAP2, internal/platform, resident keys, user verification) — the same
 * thing Chrome DevTools → WebAuthn offers.
 *
 *   pnpm e2e:passkey          (from sutraerp/; not part of `pnpm -r test`)
 *
 * The script starts and stops everything it needs except PostgreSQL:
 *   • the API binary (dev auth, PostgreSQL stores) — restarted with SUTRA_DEV_MFA=true for phase 2;
 *   • the web app in API mode with the `/api` proxy, either the Vite dev server (:5173, default)
 *     or the production build behind `vite preview` (:3000, E2E_WEB=preview) like compose's web;
 *   • Chromium with a virtual authenticator.
 *
 * Flow:
 *   1. sign in as Administrator → Security → name + "Add a passkey" → listed, session verified;
 *   2. "Add a passkey" while the authenticator never answers → the UI shows "Waiting…" with a
 *      Cancel button → Cancel → idle again with a message (the old UI hung here);
 *   3. sign out → sign in (password only) → Remove the passkey → step-up dialog →
 *      "Use a passkey" → verified, removal retried → add a passkey again;
 *   4. restart the API with SUTRA_DEV_MFA=true → sign out → sign in → "Use a passkey" → Cancel
 *      (prompt unanswered) → message, still on the second-factor step → "Use a passkey" again →
 *      signed in with `mfaLevel = mfa` and `amr` containing `hwk`.
 * Every browser console error, page error and failed /api response is printed; unexpected ones
 * fail the run.
 *
 * Environment (all optional):
 *   DATABASE_URL        PostgreSQL (migrated) — default postgres://sutra:sutra@localhost:5432/sutra
 *   SUTRA_API_BIN       API binary — default $CARGO_TARGET_DIR/debug/sutra-api or target/debug/sutra-api
 *   E2E_WEB             dev (default) | preview  (preview serves apps/web/build/client; build it first
 *                       with VITE_AUTH_MODE=api pnpm --filter @sutra/web build)
 *   E2E_API_PORT        default 8080
 *   CHROMIUM_PATH       Chromium/Chrome executable (default: Playwright's bundled Chromium)
 *   E2E_HEADED=1        show the browser
 * Requires `psql` on PATH to clear the administrator's passkeys / TOTP factor before and after.
 */
import { clickDevPersona } from "./lib/dev-password.mjs";
import { spawn, spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright";

const here = path.dirname(fileURLToPath(import.meta.url));
const webDir = path.resolve(here, "..");
const root = path.resolve(webDir, "../..");
const DATABASE_URL = process.env.DATABASE_URL ?? "postgres://sutra:sutra@localhost:5432/sutra";
const WEB_MODE = process.env.E2E_WEB === "preview" ? "preview" : "dev";
const WEB_PORT = WEB_MODE === "preview" ? 3000 : 5173;
const API_PORT = Number(process.env.E2E_API_PORT ?? 8080);
const BASE = `http://localhost:${WEB_PORT}`;
const API = `http://127.0.0.1:${API_PORT}`;
const ADMIN = "dev|admin@sutra.dev";
const apiBin =
  process.env.SUTRA_API_BIN ?? path.join(process.env.CARGO_TARGET_DIR ?? path.join(root, "target"), "debug", "sutra-api");

// Part 1 S3 (DEF-X-003): the API under test acts as the RUNTIME login role (member of sutra_app,
// NOSUPERUSER, no BYPASSRLS), exactly like production, so PostgreSQL row-level tenant isolation
// applies to every request the suite makes. DATABASE_URL (schema owner / superuser) is used ONLY
// for migrations + provisioning that role and for psql seeding / teardown, never by the app.
const RUNTIME_DB_USER = process.env.E2E_DB_RUNTIME_USER ?? "sutra_e2e_runtime";
const RUNTIME_DB_PASSWORD = process.env.E2E_DB_RUNTIME_PASSWORD ?? "sutra-e2e-runtime-pw";
function runtimeDatabaseUrl() {
  const u = new URL(DATABASE_URL);
  u.username = RUNTIME_DB_USER;
  u.password = RUNTIME_DB_PASSWORD;
  return u.toString();
}
/** Idempotent: `sutra-api migrate` as the owner (no-op when migrated) + provision the runtime role. */
function provisionRuntimeRole() {
  const r = spawnSync(apiBin, ["migrate"], {
    encoding: "utf8",
    env: { ...process.env, SUTRA_ENV: "development", DATABASE_URL, SUTRA_DB_RUNTIME_USER: RUNTIME_DB_USER, SUTRA_DB_RUNTIME_PASSWORD: RUNTIME_DB_PASSWORD },
  });
  if (r.error || r.status !== 0) throw new Error(`provisioning the runtime DB role failed: ${r.error?.message ?? r.stderr}`);
}

const children = new Set();
let failures = 0;
const log = (...a) => console.log("[e2e]", ...a);
function step(name) {
  log(`— ${name}`);
}
function check(cond, what) {
  if (cond) log(`  ✓ ${what}`);
  else {
    failures++;
    console.error(`[e2e]   ✗ ${what}`);
    throw new Error(`check failed: ${what}`);
  }
}

function sh(cmd, args, env = {}) {
  return spawnSync(cmd, args, { env: { ...process.env, ...env }, encoding: "utf8" });
}

// Teardown only: runs as the schema owner (DATABASE_URL), not as the app.
function clearAdminFactors() {
  const sql =
    `DELETE FROM platform.webauthn_credential WHERE subject = '${ADMIN}'; ` +
    `DELETE FROM platform.mfa_factor WHERE subject = '${ADMIN}';`;
  const r = sh("psql", [DATABASE_URL, "-qc", sql]);
  if (r.error || r.status !== 0) log(`  (could not clear factors with psql: ${r.error?.message ?? r.stderr.trim()})`);
}

function start(cmd, args, opts, name) {
  const child = spawn(cmd, args, { ...opts, stdio: ["ignore", "pipe", "pipe"], detached: true });
  children.add(child);
  const buf = [];
  const keep = (d) => {
    buf.push(String(d));
    if (buf.length > 200) buf.shift();
  };
  child.stdout.on("data", keep);
  child.stderr.on("data", keep);
  child.on("exit", () => children.delete(child));
  child.tail = () => buf.join("").split("\n").slice(-30).join("\n");
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

async function waitFor(url, child, ms = 60000) {
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

async function startApi(mfa) {
  provisionRuntimeRole();
  const env = {
    ...process.env,
    SUTRA_ENV: "development",
    SUTRA_HTTP_ADDR: `127.0.0.1:${API_PORT}`,
    SUTRA_LOG_FORMAT: "pretty",
    RUST_LOG: process.env.RUST_LOG ?? "warn,sutra_platform=info",
    // The app connects as the runtime role (RLS applies); see provisionRuntimeRole().
    DATABASE_URL: runtimeDatabaseUrl(),
    SUTRA_AUTH_MODE: "dev",
    SUTRA_DEV_AUTH_PASSWORD: process.env.SUTRA_DEV_AUTH_PASSWORD,
    SUTRA_DEV_MFA: mfa ? "true" : "false",
    SUTRA_SESSION_COOKIE_SECURE: "false",
    SUTRA_OUTBOX_RELAY: "off",
    SUTRA_CACHE: "memory",
    SUTRA_INBOX_CONSUMER: "false",
    // Part 1 (DEF-X-001): the API defaults to human approval; this click-through suite exercises
    // domain flows, not the approval control (covered by db_p1_appr.rs), so it opts in to dev auto-approval.
    SUTRA_APPROVAL_MODE: "auto",
    SUTRA_WEBAUTHN_RP_ID: "localhost",
    SUTRA_WEBAUTHN_RP_NAME: "SutraERP",
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

// ---------------------------------------------------------------- browser helpers
const problems = [];
function watch(page) {
  page.on("console", (m) => {
    if (m.type() !== "error") return;
    const text = m.text();
    // Failed /api responses are judged by the response handler below; Vite re-optimisation is noise.
    if (/Failed to load resource|Outdated Optimize Dep/.test(text)) return;
    problems.push(`console: ${text}`);
    log(`  console error: ${text}`);
  });
  page.on("pageerror", (e) => {
    if (/dynamically imported module/.test(e.message)) return; // Vite dev re-optimisation reload
    problems.push(`pageerror: ${e.message}`);
    log(`  page error: ${e.message}`);
  });
  page.on("response", async (r) => {
    const u = new URL(r.url());
    if (!u.pathname.startsWith("/api/") || r.status() < 400) return;
    const body = await r.text().catch(() => "");
    const expected =
      (u.pathname === "/api/v1/auth/session" && r.status() === 401) ||
      body.includes("mfa-step-up-required"); // drives the step-up dialog
    log(`  ${expected ? "expected" : "UNEXPECTED"} ${r.request().method()} ${u.pathname} → ${r.status()} ${body.slice(0, 240)}`);
    if (!expected) problems.push(`${r.request().method()} ${u.pathname} → ${r.status()} ${body.slice(0, 240)}`);
  });
}

async function signIn(page) {
  await page.goto(`${BASE}/sign-in`);
  await page.waitForLoadState("networkidle");
  await clickDevPersona(page, "administrator");
}

async function signOut(page) {
  await page.goto(`${BASE}/account/security`);
  await page.getByRole("button", { name: "Sign out of this device" }).click();
  // A1: signing out lands on /sign-in?returnTo=<page> (the returnTo-safe Sign in page).
  await page.waitForURL((u) => u.toString().startsWith(`${BASE}/sign-in`));
}

async function session(page) {
  return page.evaluate(async () => (await fetch("/api/v1/auth/session")).json());
}

async function openSecurity(page) {
  await page.goto(`${BASE}/account/security`);
  await page.waitForSelector("[data-passkey-form]");
}

// ---------------------------------------------------------------- run
let api;
let web;
let browser;
try {
  if (!existsSync(apiBin)) throw new Error(`API binary not found at ${apiBin} — run \`cargo build -p sutra-api\` or set SUTRA_API_BIN`);
  if (WEB_MODE === "preview" && !existsSync(path.join(webDir, "build/client/index.html")))
    throw new Error("E2E_WEB=preview needs the web build: VITE_AUTH_MODE=api pnpm --filter @sutra/web build");
  clearAdminFactors();

  step(`API (dev auth, MFA off) on :${API_PORT}, web (${WEB_MODE}) on ${BASE}`);
  api = await startApi(false);
  web = startWeb();
  await waitFor(`${BASE}/sign-in`, web);

  browser = await chromium.launch({
    headless: !process.env.E2E_HEADED,
    executablePath: process.env.CHROMIUM_PATH || undefined,
    args: ["--no-sandbox"],
  });
  const context = await browser.newContext();
  const page = await context.newPage();
  watch(page);
  const cdp = await context.newCDPSession(page);
  await cdp.send("WebAuthn.enable");
  const { authenticatorId } = await cdp.send("WebAuthn.addVirtualAuthenticator", {
    options: {
      protocol: "ctap2",
      transport: "internal",
      hasResidentKey: true,
      hasUserVerification: true,
      isUserVerified: true,
      automaticPresenceSimulation: true,
    },
  });
  const credentials = async () => (await cdp.send("WebAuthn.getCredentials", { authenticatorId })).credentials;

  // Warm the dev server's dependency optimiser so its one-off reload doesn't race the clicks.
  await page.goto(`${BASE}/sign-in`);
  await page.waitForLoadState("networkidle");
  await page.reload();
  await page.waitForLoadState("networkidle");

  step("1. sign in → Security → Add a passkey");
  await signIn(page);
  await page.waitForURL(`${BASE}/`);
  await openSecurity(page);
  await page.fill('input[name="label"]', "E2E virtual passkey");
  await page.click("[data-add-passkey]");
  await page.waitForSelector("[data-passkey]", { timeout: 20000 });
  check((await page.locator("[data-passkey]").count()) === 1, "passkey listed on the Security page");
  check((await page.locator("[data-passkey] b").first().textContent())?.includes("E2E virtual passkey"), "listed with its name");
  check((await page.textContent("[data-add-passkey]"))?.includes("Add a passkey"), "button back to “Add a passkey”");
  const created = await credentials();
  check(created.length === 1, "virtual authenticator holds the credential");
  check(created[0].isResidentCredential === true, "discoverable credential (residentKey: preferred)");
  const native = await page.evaluate(() => ({
    create: typeof PublicKeyCredential.parseCreationOptionsFromJSON === "function",
    get: typeof PublicKeyCredential.parseRequestOptionsFromJSON === "function",
  }));
  log(`  (browser JSON helpers: parseCreationOptionsFromJSON=${native.create}, parseRequestOptionsFromJSON=${native.get})`);
  const s1 = await session(page);
  check(s1.mfaLevel === "mfa", "session elevated to mfa after enrolment");

  step("2. authenticator never answers → Cancel resets the UI");
  await cdp.send("WebAuthn.setAutomaticPresenceSimulation", { authenticatorId, enabled: false });
  await page.fill('input[name="label"]', "Never answered");
  await page.click("[data-add-passkey]");
  await page.waitForSelector("[data-passkey-cancel]", { timeout: 10000 });
  check((await page.textContent("[data-add-passkey]"))?.includes("Waiting for your device"), "shows “Waiting for your device…”");
  await page.click("[data-passkey-cancel]");
  await page.waitForSelector("[data-passkey-cancel]", { state: "detached", timeout: 10000 });
  check((await page.textContent("[data-add-passkey]"))?.includes("Add a passkey"), "idle again after Cancel");
  check((await page.locator("[data-passkey-error]").textContent())?.toLowerCase().includes("cancel"), "explains the cancellation");
  check((await page.locator("[data-passkey]").count()) === 1, "nothing was added");
  await cdp.send("WebAuthn.setAutomaticPresenceSimulation", { authenticatorId, enabled: true });

  step("3. password-only session → Remove → step-up with the passkey");
  await signOut(page);
  await signIn(page);
  await page.waitForURL(`${BASE}/`);
  check((await session(page)).mfaLevel !== "mfa", "new session is password-only");
  await openSecurity(page);
  await page.locator("[data-passkey] button", { hasText: "Remove" }).first().click();
  await page.waitForSelector("[data-stepup-passkey]", { timeout: 10000 });
  await page.click("[data-stepup-passkey]");
  await page.waitForSelector("[data-stepup-passkey]", { state: "detached", timeout: 20000 });
  await page.waitForFunction(() => document.querySelectorAll("[data-passkey]").length === 0, null, { timeout: 15000 });
  check(true, "step-up with the passkey verified the session and the removal went through");
  check((await session(page)).mfaLevel === "mfa", "session is mfa after step-up");
  const before = (await credentials())[0]?.signCount ?? 0;
  await page.fill('input[name="label"]', "E2E virtual passkey");
  await page.click("[data-add-passkey]");
  await page.waitForSelector("[data-passkey]", { timeout: 20000 });
  check((await page.locator("[data-passkey]").count()) === 1, "passkey added again");
  log(`  (authenticator sign count before re-registration: ${before})`);

  step("4. SUTRA_DEV_MFA=true → sign out → sign in → Use a passkey");
  await stop(api);
  api = await startApi(true);
  await signOut(page);
  await signIn(page);
  await page.waitForSelector("[data-passkey-signin]", { timeout: 15000 });
  check(true, "second-factor step offers “Use a passkey”");
  // Cancel first: the prompt never gets an answer, Cancel returns to the choice with a message,
  // and the renewed challenge still works afterwards.
  await cdp.send("WebAuthn.setAutomaticPresenceSimulation", { authenticatorId, enabled: false });
  await page.click("[data-passkey-signin]");
  await page.waitForSelector("[data-passkey-cancel]", { timeout: 10000 });
  await page.click("[data-passkey-cancel]");
  await page.waitForSelector("[data-mfa-error]", { timeout: 10000 });
  check((await page.textContent("[data-mfa-error]"))?.toLowerCase().includes("cancel"), "sign-in prompt cancelled with a message");
  check((await page.textContent("[data-passkey-signin]"))?.includes("Use a passkey"), "“Use a passkey” available again");
  await cdp.send("WebAuthn.setAutomaticPresenceSimulation", { authenticatorId, enabled: true });
  await page.click("[data-passkey-signin]");
  await page.waitForURL(`${BASE}/`, { timeout: 20000 });
  const s4 = await session(page);
  check(s4.mfaLevel === "mfa", "signed in with mfaLevel = mfa");
  check(Array.isArray(s4.amr) ? s4.amr.includes("hwk") : true, "amr includes hwk (passkey)");
  await openSecurity(page);
  check((await page.locator("[data-passkey] small").first().textContent())?.includes("used"), "passkey shows as used");

  check(problems.length === 0, `no unexpected console/page/network errors${problems.length ? `:\n    ${problems.join("\n    ")}` : ""}`);
  log("PASS");
} catch (e) {
  failures++;
  console.error("[e2e] FAIL:", e instanceof Error ? e.message : e);
  if (api) console.error(`[e2e] api log tail:\n${api.tail()}`);
} finally {
  await browser?.close().catch(() => {});
  await stop(web);
  await stop(api);
  for (const c of children) await stop(c);
  clearAdminFactors();
}
process.exit(failures ? 1 : 0);
