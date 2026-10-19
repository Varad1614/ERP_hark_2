#!/usr/bin/env node
/**
 * Re-skin v2 visual proof: captures every app screen at the prototype viewport (1440×960,
 * full page) so the integrator can build side-by-side sheets with the prototype PNGs.
 *
 *   pnpm e2e:reskin2      (from sutraerp/; CI matrix suite `reskin2` → artifact e2e-shots-reskin2)
 *
 * Fresh migrated DB; seeds `e2e/lib/reskin2-seed.mjs` (institution, 16 students, CS101 delivery,
 * tuition charges, 12 staff). Then, per role: Home, every nav pill, every header tab / overflow
 * ("More") entry found on the page (`nav.sb-tabs a`, `[data-tab-link]`), Student 360 and its tabs,
 * the common screens (workspaces, to-dos, notices, profile, security, notifications) and the
 * overlays (account menu, bell, palette). Finally the component catalogue (/dev/catalogue, needs
 * a VITE_UI_CATALOGUE=1 build) per role, dark, dialog and side panel.
 *
 * Files: $E2E_SHOTS/<role>-<route-slug>.png (stable names, no counters) + manifest.json
 * ([{ file, role, path, title }]) + seed-report.txt + problems.txt. This suite only fails when it
 * cannot capture (seed core, browser, catalogue missing, or fewer than 40 shots); console / API
 * errors seen on existing screens are listed in problems.txt for the slices, not failed on.
 */
import { existsSync, mkdirSync, readdirSync, unlinkSync, writeFileSync } from "node:fs";
import path from "node:path";
import { chromium } from "playwright";
import { apiBin, apiLogin, BASE, children, go, log, problems, settle, SHOTS, signIn, signOut, startApi, startWeb, stop, waitFor, watch } from "./lib/harness.mjs";
import { report, seedReskin2 } from "./lib/reskin2-seed.mjs";

const ROLES = ["administrator", "faculty", "student"];
const SHORT = { administrator: "adm", faculty: "fac", student: "stu" };
const COMMON = ["/todos", "/notices", "/profile", "/account/security", "/notifications"];
const manifest = [];
const failures = [];
const seen = new Set();

const slug = (p) => (p === "/" ? "home" : p.replace(/^\//, "").replace(/[/?=&]+/g, "_").replace(/_+$/, "")) || "home";

async function capture(page, role, name, p) {
  const file = `${SHORT[role] ?? role}-${name}.png`;
  if (seen.has(file)) return;
  seen.add(file);
  await settle(page);
  await page.waitForFunction(() => document.querySelectorAll(".sb-toast").length === 0, null, { timeout: 5000 }).catch(() => {});
  await page.waitForTimeout(300);
  const tag = await page.addStyleTag({ content: ".sb-env-tag { visibility: hidden !important; }" }).catch(() => null);
  await page.screenshot({ path: path.join(SHOTS, file), fullPage: true });
  await tag?.evaluate((n) => n.remove()).catch(() => {});
  const title = ((await page.locator("main h1").first().textContent({ timeout: 1000 }).catch(() => "")) ?? "").trim();
  manifest.push({ file, role, path: p, title });
  log(`  📸 ${file}${title ? ` (${title})` : ""}`);
}

async function visit(page, role, p, name = slug(p)) {
  try {
    await go(page, p);
    await page.waitForTimeout(500);
    await capture(page, role, name, p);
  } catch (e) {
    problems.push(`${role} ${p}: ${e instanceof Error ? e.message : e}`);
  }
}

/** Header tab hrefs on the current page (today's Tabs, tomorrow's AreaHeader incl. the More menu). */
async function tabHrefs(page) {
  const more = page.locator("[data-tab-more]");
  if (await more.count()) await more.first().click().catch(() => {});
  const hrefs = await page.locator("nav.sb-tabs a[href], [data-tab-link][href], .sb-menu a[href]").evaluateAll((as) => as.map((a) => a.getAttribute("href")));
  await page.keyboard.press("Escape").catch(() => {});
  return [...new Set(hrefs.filter((h) => h && h.startsWith("/")))];
}

async function overlays(page, role) {
  await go(page, "/");
  try {
    await page.click('[aria-label="Account menu"]');
    await page.waitForTimeout(400);
    await page.screenshot({ path: path.join(SHOTS, `${SHORT[role]}-menu.png`) });
    manifest.push({ file: `${SHORT[role]}-menu.png`, role, path: "(account menu)", title: "Account menu" });
    await page.keyboard.press("Escape");
    await page.mouse.click(4, 700);
  } catch (e) {
    problems.push(`${role} account menu: ${e instanceof Error ? e.message : e}`);
  }
  try {
    await page.getByRole("button", { name: /^Notifications/ }).first().click();
    await page.waitForTimeout(400);
    await page.screenshot({ path: path.join(SHOTS, `${SHORT[role]}-bell.png`) });
    manifest.push({ file: `${SHORT[role]}-bell.png`, role, path: "(bell)", title: "Notifications" });
    await page.keyboard.press("Escape");
    await page.mouse.click(4, 700);
  } catch (e) {
    problems.push(`${role} bell: ${e instanceof Error ? e.message : e}`);
  }
  try {
    await page.getByRole("button", { name: "Search (Ctrl K)" }).click();
    await page.waitForTimeout(400);
    await page.screenshot({ path: path.join(SHOTS, `${SHORT[role]}-palette.png`) });
    manifest.push({ file: `${SHORT[role]}-palette.png`, role, path: "(palette)", title: "Command palette" });
    await page.keyboard.press("Escape");
  } catch (e) {
    problems.push(`${role} palette: ${e instanceof Error ? e.message : e}`);
  }
}

async function roleTour(page, role) {
  log(`— ${role}`);
  await visit(page, role, "/");
  const pills = await page.locator("nav.sb-pills a").evaluateAll((as) => as.map((a) => a.getAttribute("href")));
  // Breadth-first over each pill's header tabs; a tab that opens another area (e.g. /academic-delivery) has its tabs expanded too.
  const queue = pills.filter((h) => h && h !== "/" && h !== "/workspaces");
  const done = new Set();
  const area = (u) => u.split("/")[1];
  while (queue.length && done.size < 80) {
    const p = queue.shift();
    if (done.has(p)) continue;
    done.add(p);
    await visit(page, role, p);
    for (const t of await tabHrefs(page)) {
      if (done.has(t) || t.startsWith("/workspaces/") || t.startsWith("/students/record/")) continue;
      if (area(t) === area(p) && t.split("/").length > 2) {
        done.add(t);
        await visit(page, role, t);
      } else if (!queue.includes(t)) queue.push(t);
    }
  }
  if (role === "student") await visit(page, role, "/registration");
  if (role === "administrator") {
    await go(page, "/students");
    const rec = await page.locator('a[href^="/students/record/"]').first().getAttribute("href", { timeout: 5000 }).catch(() => null);
    if (rec) {
      await visit(page, role, rec, "student-360");
      for (const t of await tabHrefs(page)) if (t.startsWith("/students/record/")) await visit(page, role, t, `student-360_${t.split("/").pop()}`);
    } else problems.push("administrator: no student record link on /students (Student 360 not captured)");
  }
  for (const p of COMMON) await visit(page, role, p);
  await visit(page, role, "/workspaces");
  try {
    await page.locator('[data-ws-live="false"]').first().click({ timeout: 5000 });
    await settle(page);
    const wp = new URL(page.url()).pathname;
    if (wp.startsWith("/workspaces/")) {
      await capture(page, role, "workspace-detail", wp);
      for (const t of await tabHrefs(page)) if (t.startsWith("/workspaces/")) await visit(page, role, t, `workspace-detail_${t.split("/").pop()}`);
    }
  } catch (e) {
    problems.push(`${role} workspace detail: ${e instanceof Error ? e.message : e}`);
  }
  await overlays(page, role);
}

async function catalogue(page) {
  log("— component catalogue");
  for (const role of ROLES) await visit(page, role, `/dev/catalogue?role=${role}`, "catalogue");
  await visit(page, "administrator", "/dev/catalogue?role=administrator&theme=dark", "catalogue-dark");
  for (const [open, name] of [
    ["dialog", "catalogue-dialog"],
    ["panel", "catalogue-panel"],
  ]) {
    await go(page, `/dev/catalogue?role=administrator&open=${open}`);
    await page.waitForTimeout(500);
    await page.screenshot({ path: path.join(SHOTS, `adm-${name}.png`) });
    manifest.push({ file: `adm-${name}.png`, role: "administrator", path: `/dev/catalogue?open=${open}`, title: name });
  }
  await go(page, "/dev/catalogue?role=administrator");
  const ok = await page.locator("[data-catalogue]").count();
  if (!ok) failures.push("catalogue: /dev/catalogue did not render (build without VITE_UI_CATALOGUE=1?)");
  await page.evaluate(() => document.documentElement.classList.remove("dk"));
}

// ---------------------------------------------------------------- run
let api;
let web;
let browser;
try {
  if (!existsSync(apiBin)) throw new Error(`API binary not found at ${apiBin} — run \`cargo build -p sutra-api\` or set SUTRA_API_BIN`);
  mkdirSync(SHOTS, { recursive: true });
  for (const f of readdirSync(SHOTS)) if (/\.(png|json|txt)$/.test(f)) unlinkSync(path.join(SHOTS, f));
  log(`API (dev auth), web on ${BASE}, shots → ${SHOTS}`);
  api = await startApi();
  web = startWeb();
  const admin = await apiLogin("admin@sutra.dev");
  log("— seed (reskin2)");
  await seedReskin2(admin);
  writeFileSync(path.join(SHOTS, "seed-report.txt"), report.join("\n") + "\n");

  await waitFor(`${BASE}/sign-in`, web);
  browser = await chromium.launch({ headless: !process.env.E2E_HEADED, executablePath: process.env.CHROMIUM_PATH || undefined, args: ["--no-sandbox", ...(process.env.E2E_CHROMIUM_ARGS ?? "").split(" ").filter(Boolean)] });
  const context = await browser.newContext({ viewport: { width: 1440, height: 960 }, locale: "en-IN", timezoneId: "Asia/Kolkata" });
  const page = await context.newPage();
  page.setDefaultTimeout(15000);
  watch(page);
  page.setDefaultNavigationTimeout(180000);
  await go(page, "/sign-in");
  page.setDefaultNavigationTimeout(45000);
  await page.locator('[data-role-card="administrator"]').waitFor();
  await page.screenshot({ path: path.join(SHOTS, "signin.png"), fullPage: true });
  manifest.push({ file: "signin.png", role: "all", path: "/sign-in", title: "Sign in" });

  await catalogue(page);
  for (const role of ROLES) {
    await signIn(page, role);
    await roleTour(page, role);
    await signOut(page).catch((e) => problems.push(`${role} sign-out: ${e instanceof Error ? e.message : e}`));
  }
  if (manifest.length < 40) failures.push(`only ${manifest.length} screenshots captured (expected ≥ 40)`);
} catch (e) {
  failures.push(`setup: ${e instanceof Error ? e.message : e}`);
  console.error("[e2e] FAIL:", e instanceof Error ? e.message : e);
  if (api) console.error(`[e2e] api log tail:\n${api.tail()}`);
} finally {
  try {
    writeFileSync(path.join(SHOTS, "manifest.json"), JSON.stringify(manifest, null, 2));
    writeFileSync(path.join(SHOTS, "problems.txt"), problems.join("\n") + "\n");
  } catch {
    /* shots dir missing */
  }
  await browser?.close().catch(() => {});
  await stop(web);
  await stop(api);
  for (const c of children) await stop(c);
}
log(`seed: ${report.join(" | ")}`);
log(`${problems.length} problem(s) noted on existing screens (problems.txt)`);
if (failures.length) {
  console.error(`[e2e] reskin2 shots failed:\n  - ${failures.join("\n  - ")}`);
  process.exit(1);
}
log(`PASS — ${manifest.length} screenshots in ${SHOTS}`);
process.exit(0);
