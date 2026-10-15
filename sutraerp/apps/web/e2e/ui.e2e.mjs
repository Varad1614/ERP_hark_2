#!/usr/bin/env node
/**
 * UI re-skin click-through (SutraERP prototype design system) in a real Chromium.
 *
 *   pnpm e2e:ui          (from sutraerp/; not part of `pnpm -r test`)
 *
 * Reuses the shared harness (`e2e/lib/harness.mjs`): API binary (dev auth) + web app in API mode
 * on a fresh migrated database; seeds an active institution, Aarav (linked to the dev student
 * login) seated in CS101-T1 / A taught by Dr. Meera Shah, then for every role captures the
 * prototype screens at the prototype viewport (1440×960): Home, every pill of the top navigation,
 * Workspaces, the account menu, dark mode and high contrast. Checks the shell contract: the role's
 * prototype navigation, the role accent, live KPI tiles (pending tiles for figures with no backend
 * yet), "coming in Batch N" pages for areas not built, and live/coming workspace cards.
 *
 * Screenshots: $E2E_SHOTS (default /workspace/work/out/ui-shots), named `<role>-<screen>`.
 */
import { existsSync, mkdirSync, readdirSync, unlinkSync } from "node:fs";
import path from "node:path";
import { chromium } from "playwright";
import { apiBin, apiLogin, BASE, check, children, expectedFailures, failed, go, log, newProblems, psql, seedInstitution, seedStudent, settle, shot, shotCount, SHOTS, signIn, signOut, startApi, startWeb, step, stop, STUDENT_SUBJECT, TENANT, waitFor, watch } from "./lib/harness.mjs";

const FACULTY_SUBJECT = "dev|meera.shah@sutra.dev";
// 7B: the dev faculty login has no staff record in this walk-through, so the My HR panels'
// `/api/v1/people/me/*` reads answer 404 "staff record not found" (the panel shows its empty state).
expectedFailures.push((p, status, body) => status === 404 && p.startsWith("/api/v1/people/me/") && body.includes("staff record not found"));

/** Prototype navigation per role (SutraERP-Prototype-Lite.html ROLES[*].nav). */
const NAV = {
  administrator: ["Home", "Students", "Academics", "Examinations", "Finance", "People", "Campus", "Reports", "Workspaces"],
  faculty: ["Home", "My classes", "Attendance", "Assessments", "Students", "Timetable", "Leave", "Resources", "Workspaces"],
  student: ["Home", "Timetable", "Courses", "Assignments", "Results", "Fees", "Documents", "Library", "Workspaces"],
};
const ACCENT = { administrator: "#e8743b", faculty: "#3b7be8", student: "#7254e6" };
/** Screens not built yet → "Coming in Batch N" page. */
const COMING = { administrator: [], faculty: [], student: [] };
/** Batch 6 / 7 / 8 screens now live (real exam, people and campus / reports screens, no “Coming in Batch N” page). */
const LIVE_B6 = { administrator: ["/examinations", "/people", "/campus", "/reports"], faculty: ["/assessments", "/leave"], student: ["/results", "/library"] };
/** Home KPI tiles still pending per role (figures whose backend lands in a later batch). */
// 9E: the admin KPIs read the services metrics catalogue (no pending tile left).
const PENDING_KPIS = { administrator: 0, faculty: 0, student: 0 };

function seedDelivery(setup, studentIds) {
  const component = psql(
    `SELECT id || '|' || title || '|' || credits FROM institution.curriculum_component ` +
      `WHERE curriculum_version_id = '${setup.curriculumVersionId}' AND slot_code = 'CS101'`,
  );
  const [componentId, title, credits] = component.split("|");
  const offeringId = psql(
    `INSERT INTO academic.course_offering (tenant_id, institution_id, academic_year_id, academic_term_id, ` +
      `curriculum_version_id, curriculum_component_id, offering_code, title, course_code, category, credits, capacity, status) ` +
      `VALUES ('${TENANT}', '${setup.id}', '${setup.yearId}', '${setup.termIds[0]}', '${setup.curriculumVersionId}', ` +
      `'${componentId}', 'CS101-T1', '${title.replace(/'/g, "''")}', 'CS101', 'major_core', ${credits}, 60, 'active') RETURNING id`,
  );
  const sectionId = psql(
    `INSERT INTO academic.offering_section (tenant_id, institution_id, offering_id, section_code, capacity, status) ` +
      `VALUES ('${TENANT}', '${setup.id}', '${offeringId}', 'A', 60, 'active') RETURNING id`,
  );
  psql(
    `INSERT INTO academic.faculty_assignment (tenant_id, institution_id, offering_id, section_id, faculty_subject_ref, ` +
      `faculty_name, role, weekly_hours, effective_from, status) VALUES ('${TENANT}', '${setup.id}', '${offeringId}', ` +
      `'${sectionId}', '${FACULTY_SUBJECT}', 'Dr. Meera Shah', 'primary_instructor', 4, DATE '2026-06-15', 'active') RETURNING id`,
  );
  for (const sid of studentIds) {
    psql(
      `INSERT INTO academic.section_enrolment (tenant_id, institution_id, offering_id, section_id, student_id, ` +
        `student_context_id, student_number, display_name, subject_ref, origin, status) ` +
        `SELECT s.tenant_id, '${setup.id}', '${offeringId}', '${sectionId}', s.id, c.id, s.student_number, ` +
        `s.given_name || ' ' || coalesce(s.family_name, ''), s.subject_ref, 'override', 'enrolled' ` +
        `FROM student.student s JOIN student.student_context c ON c.student_id = s.id WHERE s.id = '${sid}' RETURNING id`,
    );
  }
  return { offeringId, sectionId };
}

const rgb = (hex) => `rgb(${[1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16)).join(", ")})`;

async function display(page, setting) {
  await page.click('[aria-label="Account menu"]');
  await page.locator(`[data-display="${setting}"]`).click();
  await page.keyboard.press("Escape");
  await page.mouse.click(4, 700);
  await settle(page);
}

async function roleTour(page, role) {
  const r = role.slice(0, 3);
  await step(page, `${role}: shell — prototype navigation, accent, search/bell/avatar`, async () => {
    await go(page, "/");
    const labels = (await page.locator("nav.sb-pills a").allTextContents()).map((t) => t.trim());
    check(JSON.stringify(labels) === JSON.stringify(NAV[role]), `${role} pills = ${NAV[role].join(" / ")} (got ${labels.join(" / ")})`);
    const logo = await page.locator(".sb-logo > i").evaluate((el) => getComputedStyle(el).backgroundColor);
    check(logo === rgb(ACCENT[role]), `${role} accent ${ACCENT[role]} on the logo (got ${logo})`);
    for (const l of ["Search (Ctrl K)", "Account menu"]) check((await page.getByRole("button", { name: l }).count()) === 1, `header has “${l}”`);
    check((await page.getByRole("button", { name: /^Notifications/ }).count()) === 1, "header has the bell");
  });
  await step(page, `${role}: home — hero, right now, four KPI tiles, to-dos, shortcuts, announcement`, async () => {
    await go(page, "/");
    await page.locator(`[data-home="${role}"]`).waitFor();
    check((await page.locator(".sb-hero h1").textContent())?.startsWith("Hi "), "greeting hero");
    await page.locator('[data-right-now]:not([data-right-now="loading"])').waitFor();
    check((await page.locator("[data-kpi]").count()) === 4, "four KPI tiles");
    const pending = await page.locator(".sb-stat.is-pending").count();
    check(pending === PENDING_KPIS[role], `${pending} KPI tile(s) marked pending (no backend yet), none invented`);
    await page.locator('[data-home-todos]:not([data-home-todos="loading"])').waitFor();
    check((await page.locator(".sb-shortcut").count()) === 4, "four shortcuts");
    await page.locator('[data-announcement]:not([data-announcement="loading"])').waitFor();
    await shot(page, `${r}-home`);
  });
  await step(page, `${role}: every navigation pill`, async () => {
    const hrefs = await page.locator("nav.sb-pills a").evaluateAll((as) => as.map((a) => a.getAttribute("href")));
    for (const h of hrefs.slice(1)) {
      await page.click(`nav.sb-pills a[href="${h}"]`);
      await settle(page);
      await page.waitForTimeout(400);
      check((await page.locator(`nav.sb-pills a[href="${h}"].on`).count()) === 1, `${h} pill active`);
      if (COMING[role].includes(h)) check((await page.locator("[data-coming]").count()) === 1, `${h} shows “Coming in Batch N”`);
      if (LIVE_B6[role].includes(h)) check((await page.locator("[data-coming]").count()) === 0 && (await page.locator("[data-page-head], .sb-page-head, h1").count()) >= 1, `${h} is a live Batch 6/7 screen`);
      await shot(page, `${r}-${h.slice(1).replace(/\//g, "_")}`);
    }
  });
  await step(page, `${role}: workspaces — live and coming cards`, async () => {
    await go(page, "/workspaces");
    await page.locator("[data-ws]").first().waitFor();
    const live = await page.locator('[data-ws-live="true"]').count();
    const coming = await page.locator('[data-ws-live="false"]').count();
    check(live > 0 && coming > 0, `${live} live and ${coming} coming workspaces`);
    await page.click('[data-ws-status="live"]');
    check((await page.locator('[data-ws-live="false"]').count()) === 0, "Live filter hides coming workspaces");
    await page.click('[data-ws-status="all"]');
    await shot(page, `${r}-workspaces`);
    await page.locator('[data-ws-live="true"]').first().click();
    await settle(page);
    check(!page.url().includes("/workspaces/"), `a live card opens its screen (${new URL(page.url()).pathname})`);
  });
  await step(page, `${role}: account menu, dark mode, high contrast`, async () => {
    await go(page, "/");
    await page.click('[aria-label="Account menu"]');
    await page.locator('[data-display="theme:dark"]').waitFor();
    await page.waitForTimeout(300);
    await page.screenshot({ path: path.join(SHOTS, `${r}-menu.png`) });
    await page.keyboard.press("Escape");
    await page.mouse.click(4, 700);
    await display(page, "theme:dark");
    check(await page.evaluate(() => document.documentElement.classList.contains("dk")), "dark mode on <html>");
    await shot(page, `${r}-dark-home`);
    await go(page, "/workspaces");
    await shot(page, `${r}-dark-workspaces`);
    const second = NAV[role][1];
    await page.locator("nav.sb-pills a", { hasText: second }).first().click();
    await settle(page);
    await shot(page, `${r}-dark-${second.toLowerCase().replace(/\s+/g, "-")}`);
    await go(page, "/");
    await display(page, "theme:light");
    await display(page, "contrast:high");
    check(await page.evaluate(() => document.documentElement.classList.contains("hc")), "high contrast on <html>");
    await shot(page, `${r}-hc-home`);
    await display(page, "contrast:standard");
  });
}

// ---------------------------------------------------------------- run
let api;
let web;
let browser;
try {
  if (!existsSync(apiBin)) throw new Error(`API binary not found at ${apiBin} — run \`cargo build -p sutra-api\` or set SUTRA_API_BIN`);
  mkdirSync(SHOTS, { recursive: true });
  for (const f of readdirSync(SHOTS)) if (/\.png$/.test(f)) unlinkSync(path.join(SHOTS, f));
  log(`API (dev auth), web on ${BASE}, shots → ${SHOTS}`);
  api = await startApi();
  web = startWeb();

  log("— seed: active institution, Aarav seated in CS101-T1 / A (Dr. Meera Shah)");
  const admin = await apiLogin("admin@sutra.dev");
  const setup = await seedInstitution(admin);
  const aarav = seedStudent(setup, "Aarav", "Desai", "S2026-0001");
  psql(`UPDATE student.student SET subject_ref = '${STUDENT_SUBJECT}' WHERE id = '${aarav}'`);
  const isha = seedStudent(setup, "Isha", "Kulkarni", "S2026-0002");
  seedDelivery(setup, [aarav, isha]);

  await waitFor(`${BASE}/sign-in`, web);
  browser = await chromium.launch({ headless: !process.env.E2E_HEADED, executablePath: process.env.CHROMIUM_PATH || undefined, args: ["--no-sandbox", ...(process.env.E2E_CHROMIUM_ARGS ?? "").split(" ").filter(Boolean)] });
  const context = await browser.newContext({ viewport: { width: 1440, height: 960 }, locale: "en-IN", timezoneId: "Asia/Kolkata" });
  const page = await context.newPage();
  page.setDefaultTimeout(15000);
  watch(page);
  page.setDefaultNavigationTimeout(180000);
  await go(page, "/sign-in");
  page.setDefaultNavigationTimeout(30000);
  newProblems();
  await step(page, "sign-in screen", async () => {
    await page.locator('[data-role-card="administrator"]').waitFor();
    await page.screenshot({ path: path.join(SHOTS, "signin.png"), fullPage: true });
  });

  for (const role of ["administrator", "faculty", "student"]) {
    await signIn(page, role);
    await roleTour(page, role);
    await signOut(page);
  }

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
