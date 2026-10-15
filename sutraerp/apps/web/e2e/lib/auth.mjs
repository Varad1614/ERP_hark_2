/**
 * Sign-in for the Part 1 full-stack journey (`journey.e2e.mjs`), in either auth mode of the stack:
 *
 *   E2E_AUTH=oidc  sutra-api in Keycloak mode (SUTRA_AUTH_MODE=keycloak) and the web image built
 *                  with VITE_AUTH_MODE=oidc. API clients run the real Authorization Code + PKCE
 *                  flow: GET /api/v1/auth/oidc/start → Keycloak login form (username/password, the
 *                  administrator's CONFIGURE_TOTP required action on first sign-in, then the OTP
 *                  form) → /api/v1/auth/oidc/callback → session cookie. Browser contexts click
 *                  “Sign in with SSO” and fill the same Keycloak forms.
 *   E2E_AUTH=dev   the dev authenticator (POST /api/v1/auth/login, role cards in the browser).
 *
 * Every client talks to the browser-facing origin (E2E_BASE, default http://localhost:3000: the
 * nginx web container proxying /api to the API), exactly like the SPA. No mocks: the TOTP codes
 * are computed from the secret Keycloak itself issues on the configuration page (RFC 6238).
 */
import { createHmac, randomUUID } from "node:crypto";

export const AUTH = process.env.E2E_AUTH === "oidc" ? "oidc" : "dev";
export const BASE = (process.env.E2E_BASE ?? "http://localhost:3000").replace(/\/$/, "");
export const PASSWORD = process.env.E2E_PASSWORD ?? "sutra";
export const LOGINS = {
  administrator: process.env.E2E_ADMIN_LOGIN ?? "admin@sutra.dev",
  faculty: process.env.E2E_FACULTY_LOGIN ?? "meera.shah@sutra.dev",
  student: process.env.E2E_STUDENT_LOGIN ?? "aarav.desai@sutra.dev",
};

const log = (...a) => console.log("[auth]", ...a);

// ---------------------------------------------------------------- cookies

/** Minimal cookie jar (name → value). Keycloak and SutraERP cookies have distinct names. */
export class Jar {
  constructor() {
    this.c = new Map();
  }
  take(res) {
    for (const raw of res.headers.getSetCookie?.() ?? []) {
      const [pair, ...attrs] = raw.split(";");
      const i = pair.indexOf("=");
      const name = pair.slice(0, i).trim();
      const value = pair.slice(i + 1).trim();
      const expired = attrs.some((a) => /^\s*max-age=0\s*$/i.test(a)) || attrs.some((a) => /^\s*expires=Thu, 01 Jan 1970/i.test(a));
      if (expired || value === "") this.c.delete(name);
      else this.c.set(name, value);
    }
  }
  header() {
    return [...this.c].map(([k, v]) => `${k}=${v}`).join("; ");
  }
}

async function send(jar, url, init = {}) {
  const headers = { ...(init.headers ?? {}) };
  const cookie = jar.header();
  if (cookie) headers.cookie = cookie;
  const res = await fetch(url, { ...init, headers, redirect: "manual" });
  jar.take(res);
  return res;
}

const unescapeHtml = (s) => s.replace(/&amp;/g, "&").replace(/&quot;/g, '"').replace(/&#39;/g, "'").replace(/&lt;/g, "<").replace(/&gt;/g, ">");

/** The Keycloak form that carries the credential inputs (else the first form): action + named inputs. */
function parseForm(html, pageUrl) {
  const forms = [...html.matchAll(/<form\b[^>]*\baction="([^"]+)"[^>]*>([\s\S]*?)<\/form>/gi)];
  if (!forms.length) return null;
  const m = forms.find((f) => /name="(?:password|otp|totp|totpSecret|username)"/.test(f[2])) ?? forms[0];
  const fields = {};
  for (const input of m[2].matchAll(/<input\b[^>]*>/gi)) {
    const tag = input[0];
    const name = tag.match(/\bname="([^"]+)"/i)?.[1];
    if (!name) continue;
    const type = (tag.match(/\btype="([^"]+)"/i)?.[1] ?? "text").toLowerCase();
    if ((type === "checkbox" || type === "radio") && !/\bchecked\b/i.test(tag)) continue;
    fields[name] = unescapeHtml(tag.match(/\bvalue="([^"]*)"/i)?.[1] ?? "");
  }
  return { action: new URL(unescapeHtml(m[1]), pageUrl).toString(), fields };
}

function kcError(html) {
  const m =
    html.match(/<span[^>]*id="input-error[^"]*"[^>]*>([\s\S]*?)<\/span>/i) ??
    html.match(/class="[^"]*(?:kc-feedback-text|pf-v5-c-alert__title|alert-error)[^"]*"[^>]*>([\s\S]*?)</i);
  return m ? m[1].replace(/<[^>]+>/g, "").replace(/\s+/g, " ").trim() : "";
}

// ---------------------------------------------------------------- TOTP (RFC 6238, HmacSHA1, 6 digits, 30 s)

/** Keycloak's TOTP secret per login (the raw `totpSecret`; HMAC key = its UTF-8 bytes). */
export const totpSecrets = new Map();
const lastCounter = new Map();

function hotp(secret, counter) {
  const msg = Buffer.alloc(8);
  msg.writeBigUInt64BE(BigInt(counter));
  const h = createHmac("sha1", Buffer.from(secret, "utf8")).update(msg).digest();
  const o = h[h.length - 1] & 0xf;
  const code = ((h[o] & 0x7f) << 24) | (h[o + 1] << 16) | (h[o + 2] << 8) | h[o + 3];
  return String(code % 1_000_000).padStart(6, "0");
}

/** A fresh code: Keycloak refuses a code twice (otpPolicyCodeReusable=false), so wait for the next step. */
export async function totpCode(login) {
  const secret = totpSecrets.get(login);
  if (!secret) throw new Error(`no TOTP secret captured for ${login}`);
  for (;;) {
    const counter = Math.floor(Date.now() / 30000);
    // Leave >= 3 s of validity so the code is not stale on arrival.
    const remaining = 30000 - (Date.now() % 30000);
    if (counter !== lastCounter.get(login) && remaining > 3000) {
      lastCounter.set(login, counter);
      return hotp(secret, counter);
    }
    await new Promise((r) => setTimeout(r, Math.min(remaining + 250, 30500)));
  }
}

// ---------------------------------------------------------------- API clients

/**
 * A signed-in API client. `call(method, path, body, version, { key, expect })` throws on >= 400
 * unless `expect` lists the status; `raw(...)` never throws and returns { status, headers, json }.
 */
function client(jar, session, login) {
  const csrf = () => session.csrfToken;
  const raw = async (method, p, payload, version, opts = {}) => {
    const headers = {};
    if (method !== "GET") {
      headers["content-type"] = "application/json";
      headers["x-csrf-token"] = csrf();
      headers["idempotency-key"] = opts.key ?? randomUUID();
      if (version !== undefined && version !== null) headers["if-match"] = `"${version}"`;
    }
    const res = await send(jar, `${BASE}${p}`, { method, headers, body: method === "GET" ? undefined : JSON.stringify(payload ?? {}) });
    const text = await res.text();
    let json;
    try {
      json = text ? JSON.parse(text) : null;
    } catch {
      json = text;
    }
    return { status: res.status, headers: res.headers, json, text };
  };
  const call = async (method, p, payload, version, opts = {}) => {
    const r = await raw(method, p, payload, version, opts);
    if (r.status >= 400 && !(opts.expect ?? []).includes(r.status)) throw new Error(`${method} ${p} → ${r.status} ${r.text.slice(0, 500)}`);
    return r.json;
  };
  return {
    login,
    session,
    subject: session.principal.subject,
    tenant: session.principal.scope.tenantId,
    call,
    raw,
    get: (p) => call("GET", p),
    async refresh() {
      const r = await send(jar, `${BASE}/api/v1/auth/session`);
      if (r.status !== 200) throw new Error(`session ${login}: ${r.status}`);
      Object.assign(session, await r.json());
      return session;
    },
  };
}

async function devLogin(login) {
  const jar = new Jar();
  let r = await send(jar, `${BASE}/api/v1/auth/csrf`);
  const csrf = (await r.json()).csrfToken;
  r = await send(jar, `${BASE}/api/v1/auth/login`, {
    method: "POST",
    headers: { "content-type": "application/json", "x-csrf-token": csrf },
    body: JSON.stringify({ loginId: login, password: PASSWORD }),
  });
  const body = await r.json();
  if (r.status !== 200 || body.outcome !== "authenticated") throw new Error(`dev login ${login}: ${r.status} ${JSON.stringify(body)}`);
  r = await send(jar, `${BASE}/api/v1/auth/session`);
  return client(jar, await r.json(), login);
}

/** Runs the Keycloak OIDC flow for `login` and returns the SutraERP session (or throws). */
async function oidcLogin(login) {
  const jar = new Jar();
  let res = await send(jar, `${BASE}/api/v1/auth/oidc/start?returnTo=%2F`);
  if (res.status !== 303 && res.status !== 302) throw new Error(`oidc/start → ${res.status} ${(await res.text()).slice(0, 300)}`);
  let url = new URL(res.headers.get("location"), BASE).toString();
  let page = null; // a Keycloak page rendered in the answer to a POST (no redirect)
  const steps = [];
  for (let hop = 0; hop < 24; hop++) {
    if (!page) {
      if (url.startsWith(`${BASE}/api/v1/auth/oidc/callback`)) {
        res = await send(jar, url);
        const to = res.headers.get("location") ?? "";
        if (to.includes("authError")) throw new Error(`OIDC callback refused the sign-in for ${login}: ${to} (steps: ${steps.join(" → ")})`);
        const s = await send(jar, `${BASE}/api/v1/auth/session`);
        if (s.status !== 200) throw new Error(`no session after the OIDC callback for ${login}: ${s.status}`);
        steps.push("callback");
        log(`${login}: ${steps.join(" → ")}`);
        return client(jar, await s.json(), login);
      }
      res = await send(jar, url);
      if (res.status >= 300 && res.status < 400) {
        url = new URL(res.headers.get("location"), url).toString();
        continue;
      }
      const html = await res.text();
      if (res.status !== 200) throw new Error(`Keycloak ${url} → ${res.status} ${kcError(html) || html.slice(0, 300)}`);
      page = { url, html };
    }
    const { html } = page;
    const form = parseForm(html, page.url);
    if (!form) throw new Error(`Keycloak page without a form at ${page.url}: ${kcError(html) || html.slice(0, 300)}`);
    const f = form.fields;
    let body;
    let kind;
    if ("totpSecret" in f) {
      // Required action CONFIGURE_TOTP: enrol the authenticator with the secret Keycloak issued.
      kind = "configure-totp";
      totpSecrets.set(login, f.totpSecret);
      lastCounter.delete(login);
      body = { ...f, totp: await totpCode(login), userLabel: "e2e-journey" };
    } else if ("otp" in f || /name="otp"/.test(html)) {
      kind = "otp";
      body = { ...f, otp: await totpCode(login) };
    } else if ("username" in f || "password" in f || /name="password"/.test(html)) {
      kind = "password";
      body = { ...f, username: login, password: PASSWORD };
      delete body.login;
    } else {
      throw new Error(`unexpected Keycloak page for ${login} at ${page.url}: ${kcError(html) || Object.keys(f).join(",")}`);
    }
    if (steps.at(-1) === kind) throw new Error(`Keycloak rejected step ${kind} for ${login}: ${kcError(html) || "same form again"}`);
    steps.push(kind);
    res = await send(jar, form.action, { method: "POST", headers: { "content-type": "application/x-www-form-urlencoded" }, body: new URLSearchParams(body).toString() });
    if (res.status >= 300 && res.status < 400) {
      url = new URL(res.headers.get("location"), form.action).toString();
      page = null;
      continue;
    }
    const next = await res.text();
    if (res.status !== 200) throw new Error(`Keycloak did not accept step ${kind} for ${login}: ${res.status} ${kcError(next) || next.slice(0, 300)}`);
    // Keycloak renders the next form (OTP, required action) directly in the POST answer.
    page = { url: form.action, html: next };
  }
  throw new Error(`OIDC sign-in for ${login} did not finish in 20 hops`);
}

/**
 * Signs `role` in through the real flow of the configured auth mode. In OIDC mode the
 * administrator's first sign-in only configures TOTP (amr = pwd, no MFA); a second sign-in with
 * the OTP gives the MFA session that step-up permissions need.
 */
export async function login(role) {
  const id = LOGINS[role] ?? role;
  if (AUTH === "dev") return devLogin(id);
  let c = await oidcLogin(id);
  if (role === "administrator" && c.session.mfaLevel !== "mfa") {
    log(`${id}: session mfaLevel=${c.session.mfaLevel} amr=${JSON.stringify(c.session.amr)} — signing in again with the OTP`);
    c = await oidcLogin(id);
  }
  log(`${id}: signed in (${AUTH}) subject=${c.subject} roles=${c.session.principal.roles.join(",")} mfa=${c.session.mfaLevel} amr=${JSON.stringify(c.session.amr)}`);
  return c;
}

// ---------------------------------------------------------------- browser

/** Signs `role` in inside a Playwright `page` (its own browser context). */
export async function browserSignIn(page, role) {
  const id = LOGINS[role] ?? role;
  await page.goto(`${BASE}/sign-in`);
  await page.waitForLoadState("networkidle").catch(() => {});
  if (AUTH === "dev") {
    await page.click(`[data-role-card="${role}"]`);
  } else {
    await page.click("[data-oidc-start]");
    await page.locator("#username").waitFor({ timeout: 30000 });
    await page.fill("#username", id);
    await page.fill("#password", PASSWORD);
    await page.locator("#kc-login, [name=login], button[type=submit]").first().click();
    // OTP form (administrator), else straight back to the SPA.
    const otp = page.locator("#otp, input[name=otp]").first();
    const back = page.waitForURL((u) => u.toString().startsWith(`${BASE}/`) && !u.toString().includes("/sign-in"), { timeout: 30000 }).then(() => "spa");
    const which = await Promise.race([back, otp.waitFor({ timeout: 30000 }).then(() => "otp")]);
    if (which === "otp") {
      await otp.fill(await totpCode(id));
      await page.locator("#kc-login, [name=login], button[type=submit]").first().click();
    }
  }
  await page.waitForURL((u) => u.toString().startsWith(`${BASE}/`) && !u.toString().includes("/sign-in") && !u.toString().includes("/api/"), { timeout: 30000 });
  await page.waitForLoadState("networkidle").catch(() => {});
}
