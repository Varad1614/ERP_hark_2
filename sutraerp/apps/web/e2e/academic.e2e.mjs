#!/usr/bin/env node
/**
 * Batch 4 (Academic delivery) click-through in a real Chromium — skeleton by the Batch 4
 * foundation; slices 4A/4B/4C add their steps **only inside their own region** below.
 *
 *   pnpm e2e:academic          (from sutraerp/; not part of `pnpm -r test`)
 *
 * Reuses the Batch 3 harness (`e2e/lib/harness.mjs`): starts the API binary (dev auth, PostgreSQL
 * stores) and the web app in API mode, seeds a fresh, migrated database and walks every Batch 4
 * screen:
 *
 *   seed   an institution taken to `active` through the Batch 2 API; two students in the Batch 3
 *          foundation tables (Aarav linked to the dev student login); one `active` course
 *          offering (term-1 component CS101) with section A, taught by Dr. Meera Shah (active
 *          faculty assignment), both students seated — inserted into the Batch 4 foundation tables
 *          exactly like `apps/api/tests/support/b4.rs`;
 *   admin  Academic delivery → every tab;  student  Courses, Timetable, Assignments, Library;
 *   faculty  My classes, My students, Timetable, Attendance, Resources.
 *
 * Screenshots: $E2E_SHOTS (default /workspace/work/out/b4-shots); slices prefix theirs `4a-`, `4b-`,
 * `4c-`. Every console error, page error and failed /api response fails the run.
 * Requires `psql` on PATH. Run with `/workspace/work/tools/run-e2e.sh <log> <shots> academic <worktree>`.
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
  go,
  log,
  newProblems,
  psql,
  seedInstitution,
  seedStudent,
  shot,
  shotCount,
  SHOTS,
  signIn,
  signOut,
  startApi,
  startWeb,
  step,
  stop,
  TENANT,
  STUDENT_SUBJECT,
  waitFor,
  watch,
} from "./lib/harness.mjs";

const FACULTY_SUBJECT = "dev|meera.shah@sutra.dev";

/** Offering + section + faculty + seats in the Batch 4 foundation tables (like tests/support/b4.rs). */
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
  return { offeringId, sectionId, componentId };
}

/** Opens `route` and checks its frame and the owning slice's placeholder or real screen. */
async function expectFrame(page, route, screenKey, slice) {
  await go(page, route);
  await page.locator(`[data-screen="${screenKey}"]`).first().waitFor();
  const marker = await page.locator(`[data-placeholder="${slice}"], [data-slice="${slice}"]`).count();
  check(marker > 0, `${route} renders slice ${slice}`);
}

/**
 * Re-skin v2 (R3): a row's actions are ONE pill (`data-action`) + a kebab (`data-row-menu` →
 * `data-menu-item`). Clicks action `id` in the table row containing `text`, then waits.
 */
async function act(page, text, id, scope) {
  const { row, settle } = await import("./lib/harness.mjs");
  const r = row(page, text, scope);
  await r.waitFor({ state: "visible", timeout: 10000 });
  await r.locator(`[data-action="${id}"], [data-row-menu]`).first().waitFor({ state: "visible", timeout: 10000 });
  const pill = r.locator(`[data-action="${id}"]`).first();
  if (await pill.count()) await pill.click();
  else {
    await r.locator("[data-row-menu]").first().click();
    await r.locator(`[data-menu-item="${id}"]`).first().click({ timeout: 10000 });
  }
  await settle(page);
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
  const existing = psql("SELECT count(*) FROM academic.course_offering");
  if (existing !== "0") throw new Error(`DATABASE_URL must be a fresh migrated database (found ${existing} offerings)`);

  log(`API (dev auth), web on ${BASE}, shots → ${SHOTS}`);
  api = await startApi();
  web = startWeb();

  log("— seed: institution to activation through the Batch 2 API");
  const admin = await apiLogin("admin@sutra.dev");
  const setup = await seedInstitution(admin);
  Object.assign(ids, setup);
  ids.aarav = seedStudent(setup, "Aarav", "Desai", "S2026-0001");
  psql(`UPDATE student.student SET subject_ref = '${STUDENT_SUBJECT}' WHERE id = '${ids.aarav}'`);
  ids.isha = seedStudent(setup, "Isha", "Kulkarni", "S2026-0002");
  Object.assign(ids, seedDelivery(setup, [ids.aarav, ids.isha]));
  log(`  ✓ institution ${setup.id}, offering ${ids.offeringId} section ${ids.sectionId} (Meera), 2 students seated`);

  await waitFor(`${BASE}/sign-in`, web);
  browser = await chromium.launch({ headless: !process.env.E2E_HEADED, executablePath: process.env.CHROMIUM_PATH || undefined, args: ["--no-sandbox", ...(process.env.E2E_CHROMIUM_ARGS ?? "").split(" ").filter(Boolean)] });
  const context = await browser.newContext({ viewport: { width: 1440, height: 960 }, locale: "en-IN", timezoneId: "Asia/Kolkata" });
  const page = await context.newPage();
  page.setDefaultTimeout(15000);
  watch(page);
  page.setDefaultNavigationTimeout(180000);
  for (const p of ["/sign-in", "/sign-in"]) await go(page, p);

  // ------------------------------------------------------------ admin
  await signIn(page, "administrator");
  for (const p of ["/academic-delivery", "/academic-delivery/timetable", "/academic-delivery/flexible"]) await go(page, p);
  page.setDefaultNavigationTimeout(30000);
  newProblems();

  await step(page, "foundation: admin Academic delivery frame and tabs", async () => {
    for (const [tab, slice] of [
      ["offerings", "4A"],
      ["faculty", "4A"],
      ["timetable", "4B"],
      ["attendance", "4B"],
      ["monitoring", "4B"],
      ["assessment", "4B"],
      ["flexible", "4C"],
      ["recognition", "4C"],
    ])
      await expectFrame(page, `/academic-delivery/${tab}`, "academic-delivery", slice);
    await go(page, "/academic-delivery");
    await shot(page, "foundation-admin-academic-delivery");
  });

  // ==== 4A begin (admin: offerings, sections, enrolment, faculty assignment)
  const h4a = await import("./lib/harness.mjs");
  await step(page, "4A: admin offerings — seeded active offering, sections and roster", async () => {
    await go(page, "/academic-delivery/offerings");
    await page.locator('[data-offering="CS101-T1"]').waitFor();
    check((await h4a.row(page, "CS101-T1").locator('[data-offering-status="active"]').count()) === 1, "CS101-T1 is active");
    await act(page, "CS101-T1", "offering-manage");
    const detail = page.locator('[data-offering-detail="CS101-T1"]');
    await detail.waitFor();
    await act(page, "Dr. Meera Shah", "section-roster", detail);
    await page.locator("[data-roster-student]").first().waitFor();
    check((await page.locator("[data-roster-student]").count()) === 2, "two students seated in CS101-T1 / A");
    await shot(page, "4a-admin-offering-sections-roster");
  });
  await step(page, "4A: admin creates an MD101 offering and takes it through approval and publication", async () => {
    await go(page, "/academic-delivery/offerings");
    await page.click("[data-new-offering]");
    await page.waitForFunction(() => document.querySelectorAll('[data-form="offering-create"] select[name="academicTermId"] option').length > 1);
    await page.waitForFunction(() => document.querySelectorAll('[data-form="offering-create"] select[name="component"] option').length > 1);
    await shot(page, "4a-admin-new-offering-form");
    await h4a.fillForm(page, "offering-create", { academicTermId: "(T1)", component: "MD101", offeringCode: "MD101-T1", sectionCode: "A", sectionCapacity: "40" });
    await page.locator('[data-offering="MD101-T1"]').waitFor();
    check((await h4a.row(page, "MD101-T1").locator('[data-offering-status="draft"]').count()) === 1, "MD101-T1 created as draft");
    await act(page, "MD101-T1", "offering-submit");
    await h4a.row(page, "MD101-T1").locator('[data-offering-status="approved"]').waitFor();
    await act(page, "MD101-T1", "offering-publish");
    await h4a.row(page, "MD101-T1").locator('[data-offering-status="published"]').waitFor();
    await shot(page, "4a-admin-offering-published");
  });
  await step(page, "4A: admin assigns Dr. Meera Shah to MD101-T1 / A (draft → approved → active) and opens enrolment", async () => {
    await go(page, "/academic-delivery/faculty");
    await page.locator('[data-staff-section="MD101-T1/A"]').waitFor();
    await act(page, "MD101-T1 · A", "assignment-create");
    await h4a.fillForm(page, "assignment-create", { facultySubjectRef: FACULTY_SUBJECT, facultyName: "Dr. Meera Shah", role: "primary_instructor", weeklyHours: "3" });
    const assignmentRow = () => page.locator("tr", { has: page.locator("[data-assignment-status]") }).filter({ hasText: "MD101-T1 · A" }).first();
    await assignmentRow().locator('[data-assignment-status="draft"]').waitFor();
    await assignmentRow().locator('[data-action="assignment-submit"]').click();
    await assignmentRow().locator('[data-assignment-status="approved"]').waitFor();
    await assignmentRow().locator('[data-action="assignment-activate"]').click();
    await assignmentRow().locator('[data-assignment-status="active"]').waitFor();
    await shot(page, "4a-admin-faculty-assignment");
    await go(page, "/academic-delivery/offerings");
    await act(page, "MD101-T1", "offering-open-enrolment");
    await h4a.row(page, "MD101-T1").locator('[data-offering-status="enrollment_open"]').waitFor();
    await shot(page, "4a-admin-offering-enrolment-open");
  });
  await step(page, "4A: admin suspension needs a reason; governance version through review", async () => {
    await go(page, "/academic-delivery/offerings");
    await act(page, "CS101-T1", "offering-suspend");
    const form = page.locator('[data-form="offering-suspend"]');
    await form.locator('button[type="submit"]').click();
    await form.getByText("Reason is required").waitFor();
    await shot(page, "4a-admin-suspend-reason-required");
    await page.keyboard.press("Escape");
    await go(page, "/academic-delivery/faculty");
    await page.click('[data-action="policy-create"]');
    await h4a.fillForm(page, "policy-create", { name: "Workload norms", maxWeeklyHours: "16", rationale: "UGC workload guidance" });
    await act(page, "Workload norms", "policy-submit");
    await act(page, "Workload norms", "policy-compatible");
    await h4a.row(page, "Workload norms").getByText("Effective").waitFor();
    await shot(page, "4a-admin-assignment-governance");
  });
  // ==== 4A end
  // ==== 4B begin (admin: timetable, attendance policy, monitoring, internal assessment)
  // helpers not imported by the foundation header (namespaced to stay merge-safe)
  const h4b = await import("./lib/harness.mjs");
  const term4b = ids.termIds[0];
  const pickTerm = async () => {
    await page.locator("select[data-term-picker]").first().selectOption(term4b);
    await h4b.settle(page);
  };
  await step(page, "4B: admin timetable — requirement → generate → validate → approve → publish → activate", async () => {
    await go(page, "/academic-delivery/timetable");
    await pickTerm();
    await act(page, "CS101-T1", "add-requirement");
    await h4b.fillForm(page, "requirement", { sessionsPerWeek: "3", sessionMinutes: "60", roomLabel: "R101" });
    await h4b.expectRowStatus(page, "CS101-T1", "3 × 60 min");
    await page.locator("[data-new-timetable]").click();
    await h4b.fillForm(page, "new-timetable", { name: "Term 1 timetable" });
    for (const a of ["tt-generate", "tt-validate", "tt-submit", "tt-publish", "tt-activate"]) await act(page, "Term 1 timetable v1", a);
    await h4b.expectRowStatus(page, "Term 1 timetable v1", "Active");
    await page.locator("[data-timetable-detail] [data-slot]").first().waitFor();
    check((await page.locator("[data-timetable-detail] [data-slot]").count()) === 3, "generated timetable has 3 weekly slots");
    await shot(page, "4b-admin-timetable");
  });
  await step(page, "4B: admin attendance policy → effective", async () => {
    await go(page, "/academic-delivery/attendance");
    await page.locator("[data-new-policy]").click();
    await h4b.fillForm(page, "attendance-policy", { thresholdPercent: "75", minSessions: "1" });
    await act(page, "Minimum attendance", "policy-submit");
    await h4b.expectRowStatus(page, "Minimum attendance", "Effective");
    await pickTerm();
    await page.locator("select[data-section-picker]").first().selectOption(ids.sectionId);
    await h4b.settle(page);
    await h4b.expectNotEmpty(page, "Section attendance");
    await shot(page, "4b-admin-attendance");
  });
  await step(page, "4B: admin monitoring — open → risk → outcome → governance → certify → close", async () => {
    await go(page, "/academic-delivery/monitoring");
    await pickTerm();
    await page.locator("[data-new-case]").click();
    await h4b.fillForm(page, "monitoring-case", { sectionId: ids.sectionId, title: "CS101 attendance watch" });
    await act(page, "CS101 attendance watch", "case-assess");
    await act(page, "CS101 attendance watch", "case-outcome");
    await h4b.fillForm(page, "case-outcome", { summary: "Attendance is being watched weekly" });
    await act(page, "CS101 attendance watch", "case-review");
    await h4b.fillForm(page, "case-review", {});
    await act(page, "CS101 attendance watch", "case-certify");
    await h4b.expectRowStatus(page, "CS101 attendance watch", "Certified");
    await shot(page, "4b-admin-monitoring-certified");
    await act(page, "CS101 attendance watch", "case-close");
    await h4b.expectRowStatus(page, "CS101 attendance watch", "Closed");
  });
  await step(page, "4B: admin internal assessment — blueprint → publish → assignment window → open", async () => {
    await go(page, "/academic-delivery/assessment");
    await pickTerm();
    await page.locator("select[data-section-picker]").first().selectOption(ids.sectionId);
    await h4b.settle(page);
    await page.locator("[data-new-blueprint]").click();
    await h4b.fillForm(page, "blueprint", { title: "CS101 internal", totalMarks: "50", assignmentWeight: "40" });
    await act(page, "CS101 internal", "bp-publish");
    await h4b.expectRowStatus(page, "CS101 internal", "Published");
    await page.locator("[data-new-window]").click();
    const due = new Date(Date.now() + 7 * 86400000).toISOString().slice(0, 10);
    await h4b.fillForm(page, "assessment-window", { componentKey: "a1", title: "Essay on recursion", dueOn: due, maxMarks: "10" });
    await act(page, "Essay on recursion", "window-open");
    await h4b.expectRowStatus(page, "Essay on recursion", "Open");
    await shot(page, "4b-admin-assessment");
  });
  // ==== 4B end
  // ==== 4C begin (admin: providers, opportunities, certificates, credit recognition)
  const { fillForm: c4fill, expectRowStatus: c4status, tile: c4tile } = await import("./lib/harness.mjs");
  const c4day = (n) => new Date(Date.now() + 5.5 * 3600e3 + n * 86400e3).toISOString().slice(0, 10);
  await step(page, "4C admin: policy, provider onboarding, opportunity activation", async () => {
    await go(page, "/academic-delivery/flexible");
    await page.locator("[data-new-policy]").click();
    await c4fill(page, "policy", {});
    await act(page, "v1", "publish-policy");
    await c4status(page, "v1", "Effective");
    await page.locator("[data-new-provider]").click();
    await c4fill(page, "provider", { code: "NPTEL", name: "NPTEL", providerKind: "government_platform", mode: "nptel", accreditationRef: "MoE/2026/17", accreditationValidUntil: c4day(900) });
    await act(page, "NPTEL · NPTEL", "submit-provider");
    await c4status(page, "NPTEL · NPTEL", "Active");
    await page.locator("[data-new-opportunity]").click();
    await c4fill(page, "opportunity", { code: "NPTEL-ML", title: "Machine Learning (NPTEL)", mode: "nptel", credits: 4, minHours: 20, capacity: 2, enrolmentOpensOn: c4day(0), enrolmentClosesOn: c4day(30) });
    await act(page, "NPTEL-ML", "submit-opportunity");
    await c4status(page, "NPTEL-ML", "Active");
    await shot(page, "4c-admin-flexible-setup");
  });
  await step(page, "4C admin: allocation, provider registration, completion, certificate", async () => {
    const opp = (await admin.get(`/api/v1/academic/institutions/${ids.id}/learning-opportunities`)).items.find((o) => o.code === "NPTEL-ML");
    ids.opportunityNptel = opp.id;
    await admin.call("POST", `/api/v1/academic/learning-opportunities/${opp.id}/enrolments`, { studentId: ids.isha, priority: 1 });
    await go(page, "/academic-delivery/flexible");
    await act(page, "NPTEL-ML", "allocate");
    await act(page, "NPTEL-ML", "view");
    const demand = page.locator("[data-demand]");
    await c4status(page, "S2026-0002", "Allocated", demand);
    await act(page, "S2026-0002", "register", demand);
    await act(page, "S2026-0002", "registration-outcome", demand);
    await c4fill(page, "registration-outcome", { outcome: "accepted", providerReference: "NPTEL-0001" });
    await c4status(page, "S2026-0002", "Registered", demand);
    await act(page, "S2026-0002", "complete", demand);
    await c4fill(page, "completion", { hoursCompleted: 25, assessmentStatus: "passed", providerEvidenceRef: "NPTEL/CERT/0001" });
    await c4status(page, "S2026-0002", "Completed", demand);
    await act(page, "S2026-0002", "certificate", demand);
    await c4fill(page, "certificate", { certificateNumber: "NPTEL/CERT/0001" });
    await shot(page, "4c-admin-flexible-demand");
  });
  await step(page, "4C admin: certificate verification, mapping, credit recognition posted, reconciliation", async () => {
    await go(page, "/academic-delivery/recognition");
    await act(page, "NPTEL/CERT/0001", "verify-certificate");
    await c4fill(page, "verify", {});
    await c4status(page, "NPTEL/CERT/0001", "Accepted");
    await page.locator("[data-new-mapping]").click();
    await c4fill(page, "mapping", { opportunityId: "NPTEL-ML", credits: 4 });
    await act(page, "NPTEL-ML · v1", "publish-mapping");
    await c4status(page, "NPTEL-ML · v1", "Effective");
    await go(page, "/academic-delivery/flexible");
    await act(page, "NPTEL-ML", "view");
    await act(page, "S2026-0002", "request-recognition", page.locator("[data-demand]"));
    await go(page, "/academic-delivery/recognition");
    const recog = c4tile(page, "Credit recognition");
    await act(page, "NPTEL-ML", "post-recognition", recog);
    await c4status(page, "NPTEL-ML", "Posted", recog);
    await page.locator("[data-new-reconciliation]").click();
    await c4fill(page, "reconciliation", { opportunityId: "NPTEL-ML", records: "S2026-0002:NPTEL-0001:done:25" });
    await c4status(page, "NPTEL-ML", "Reconciled", c4tile(page, "Record reconciliation"));
    await shot(page, "4c-admin-credit-recognition");
    const ledger = psql(`SELECT count(*) FROM student.credit_ledger_entry WHERE student_id = '${ids.isha}' AND source_type = 'academic_flexible_learning'`);
    check(ledger === "1", `recognised credits posted to the Domain C ledger (${ledger})`);
    await admin.call("POST", `/api/v1/academic/offerings/${ids.offeringId}/resources`, { title: "CS101 course handbook", kind: "document", url: "https://library.example.edu/cs101/handbook.pdf", publish: true });
  });
  // ==== 4C end

  // ------------------------------------------------------------ student
  await signOut(page);
  await signIn(page, "student");
  await step(page, "foundation: student screens", async () => {
    await expectFrame(page, "/courses", "courses", "4A");
    await expectFrame(page, "/timetable", "timetable", "4B");
    await expectFrame(page, "/assignments", "assignments", "4B");
    await expectFrame(page, "/library", "library", "4C");
    await go(page, "/courses");
    await shot(page, "foundation-student-courses");
  });
  // ==== 4A student begin (Courses)
  await step(page, "4A: student Courses — seat in CS101-T1 with faculty, MD101 not on the registration", async () => {
    await go(page, "/courses");
    await page.locator('[data-my-course="CS101-T1"]').waitFor();
    check((await page.getByText("Dr. Meera Shah").count()) > 0, "faculty shown on the course");
    await page.locator('[data-open-course="MD101-T1"]').waitFor();
    check((await page.locator('[data-open-course="MD101-T1"]').getByText("Not on your registration").count()) === 1, "enrolment gated on the 3B registration");
    await shot(page, "4a-student-courses");
    await page.locator('[data-my-course="CS101-T1"]').click();
    await page.locator('[data-course-detail="CS101-T1"]').waitFor();
    await shot(page, "4a-student-course-detail");
  });
  // ==== 4A student end
  // ==== 4B student begin (Timetable, Assignments + submission)
  await step(page, "4B: student timetable and assignment submission", async () => {
    await go(page, "/timetable");
    await page.locator("[data-student-timetable] [data-slot]").first().waitFor();
    check((await page.locator("[data-student-timetable] [data-slot]").count()) === 3, "student sees the 3 weekly classes");
    await shot(page, "4b-student-timetable");
    await go(page, "/assignments");
    await page.locator('[data-assignment-title="Essay on recursion"] [data-action="assignment-submit"]').click();
    await h4b.fillForm(page, "assignment-submit", { content: "Recursion solves a problem through smaller instances of itself." });
    await page.locator('[data-tab="submitted"]').click();
    const done4b = page.locator('[data-assignment-title="Essay on recursion"]');
    await done4b.filter({ hasText: "Submitted" }).waitFor({ timeout: 10000 });
    check((await done4b.count()) === 1, "the essay moved to Submitted");
    await shot(page, "4b-student-assignments");
  });
  // ==== 4B student end
  // ==== 4C student begin (Library links, flexible-learning request)
  await step(page, "4C student: Library links and a flexible-learning request", async () => {
    await go(page, "/library");
    await page.locator("[data-library-item]").first().waitFor();
    check((await page.locator("[data-library-item]").count()) >= 1, "Library lists the published course resources");
    await act(page, "Machine Learning (NPTEL)", "request-opportunity");
    await page.locator("[data-my-enrolment]").first().waitFor();
    await c4status(page, "Machine Learning (NPTEL)", "Eligible", page.locator("[data-flexible-self] table").last());
    await shot(page, "4c-student-library");
  });
  // ==== 4C student end

  // ------------------------------------------------------------ faculty
  await signOut(page);
  await signIn(page, "faculty");
  await step(page, "foundation: faculty screens and nav", async () => {
    await expectFrame(page, "/classes", "classes", "4A");
    await expectFrame(page, "/my-students", "my-students", "4A");
    await expectFrame(page, "/timetable", "timetable", "4B");
    await expectFrame(page, "/attendance", "attendance", "4B");
    await expectFrame(page, "/resources", "resources", "4C");
    // Batch 3 bug fix: the faculty "Students" entry no longer points at the admin-only directory.
    await go(page, "/");
    check((await page.locator('a[href="/students"]').count()) === 0, "faculty navigation has no admin-only /students link");
    check((await page.locator('a[href="/my-students"]').count()) > 0, "faculty navigation links My students");
    await shot(page, "foundation-faculty-home");
  });
  // ==== 4A faculty begin (My classes, My students)
  await step(page, "4A: faculty My classes and My students", async () => {
    await go(page, "/classes");
    await page.locator('[data-my-class="CS101-T1/A"]').waitFor();
    await page.locator('[data-my-class="MD101-T1/A"]').waitFor();
    await shot(page, "4a-faculty-my-classes");
    await page.locator('[data-my-class="CS101-T1/A"]').click();
    await page.locator('[data-class-detail="CS101-T1/A"]').waitFor();
    await page.locator("[data-roster-student]").first().waitFor();
    await shot(page, "4a-faculty-class-detail-roster");
    for (const t of ["materials", "sessions"]) {
      await page.locator(`[data-tab="${t}"]`).click();
      await h4a.settle(page);
      await shot(page, `4a-faculty-class-detail-${t}`);
    }
    await go(page, "/my-students");
    await page.locator("[data-my-student]").first().waitFor();
    check((await page.locator("[data-my-student]").count()) === 2, "two students in the taught sections");
    await shot(page, "4a-faculty-my-students");
  });
  // ==== 4A faculty end
  // ==== 4B faculty begin (Timetable, sessions + attendance capture, coursework marking)
  await step(page, "4B: faculty timetable", async () => {
    await go(page, "/timetable");
    await page.locator("[data-faculty-timetable] [data-slot]").first().waitFor();
    check((await page.locator("[data-faculty-timetable] [data-slot]").count()) === 3, "faculty sees the 3 weekly teaching slots");
    await shot(page, "4b-faculty-timetable");
  });
  await step(page, "4B: faculty session → quick-grid attendance → submit (capture + complete + finalise)", async () => {
    await go(page, "/attendance");
    await page.locator("[data-new-session]").first().click();
    await h4b.fillForm(page, "new-session", { topic: "Recursion" });
    await page.locator("[data-session-start]").click();
    await h4b.settle(page);
    const tiles = page.locator("[data-attendance-grid] [data-student]");
    await tiles.first().waitFor();
    check((await tiles.count()) === 2, "quick grid lists the 2 seated students");
    await shot(page, "4b-faculty-attendance-grid");
    await tiles.nth(1).click();
    check((await tiles.nth(1).getAttribute("data-mark")) === "absent", "one tap marks a student absent");
    await page.locator("[data-roster-view=list]").click();
    await page.locator("[data-roster-student]").first().waitFor();
    check((await page.locator("[data-roster-student]").count()) === 2, "list view lists the 2 seated students");
    await page.locator("[data-roster-view=grid]").click();
    await page.locator("[data-submit-attendance]").first().click();
    await page.locator('[data-form="attendance-submit"]').waitFor();
    await shot(page, "4b-faculty-attendance-confirm");
    await h4b.fillForm(page, "attendance-submit", {});
    const row4b = page.locator("[data-session-row]", { hasText: "Recursion" }).first();
    await row4b.filter({ hasText: "Finalised" }).waitFor({ timeout: 15000 });
    check((await row4b.textContent()).includes("Finalised"), "the Recursion session is finalised");
    await shot(page, "4b-faculty-attendance-finalised");
  });
  // ==== 4B faculty end
  // ==== 4C faculty begin (Resources)
  await step(page, "4C faculty: add and publish a course resource", async () => {
    await go(page, "/resources");
    await page.locator("[data-new-resource]").click();
    await c4fill(page, "resource", { title: "Week 1 reading list", kind: "reading", url: "https://library.example.edu/cs101/week1" });
    const res4c = page.locator('[data-resource-row="Week 1 reading list"]');
    await res4c.filter({ hasText: "Visible to students" }).waitFor({ timeout: 10000 });
    check((await res4c.count()) === 1, "the new resource is visible to students");
    await shot(page, "4c-faculty-resources");
  });
  // ==== 4C faculty end

  // ------------------------------------------------------------ after the faculty's day: what the others now see
  await step(page, "closing: faculty My classes reads Enrolment open", async () => {
    await go(page, "/classes");
    await page.getByText("Enrolment open").first().waitFor();
    await shot(page, "4a-faculty-my-classes-enrolment-label");
  });
  await signOut(page);
  await signIn(page, "student");
  await step(page, "closing: student sees finalised attendance and the new resource", async () => {
    await go(page, "/timetable");
    await page.getByText("1 / 1").first().waitFor();
    await shot(page, "4b-student-attendance-after-finalise");
    await go(page, "/library");
    await page.getByText("Week 1 reading list").first().waitFor();
    await shot(page, "4c-student-library-new-resource");
  });
  await signOut(page);
  await signIn(page, "administrator");
  await step(page, "closing: admin attendance summary after finalisation", async () => {
    await go(page, "/academic-delivery/attendance");
    await pickTerm();
    await page.locator("select[data-section-picker]").first().selectOption(ids.sectionId);
    await h4b.settle(page);
    await page.getByText("0 / 1").first().waitFor();
    await shot(page, "4b-admin-attendance-after-finalise");
    await go(page, "/academic-delivery/monitoring");
    await pickTerm();
    await page.getByText("Moderate").first().waitFor();
    await shot(page, "4b-admin-monitoring-overview");
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
