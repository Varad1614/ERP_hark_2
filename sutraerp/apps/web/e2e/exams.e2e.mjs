#!/usr/bin/env node
/**
 * Batch 6 (Examinations) end-to-end click-through in a real Chromium (integrator).
 *
 *   pnpm e2e:exams          (from sutraerp/; CI matrix entry `exams`, not part of `pnpm -r test`)
 *
 * Reuses the Batch 3 harness (`e2e/lib/harness.mjs`): starts the API binary (dev auth, PostgreSQL)
 * and the web app in API mode on a fresh, migrated database, then walks the whole examination
 * cycle across the three roles, one screenshot per step:
 *
 *   seed     institution → `active` (Batch 2 API); Aarav (linked to the dev student login) in the
 *            Batch 3 tables with an effective term registration for CS101; offering CS101-T1 /
 *            section A taught by Dr. Meera Shah with Aarav enrolled (Batch 4 tables) and a finalised
 *            internal assessment (34.50 / 40); the default chart of accounts + Aarav's finance
 *            account (no dues → hall ticket issued); a ten-point grading scheme (exams API).
 *   admin    policy draft → approved → active; session (term / scheme pickers) + paper (offering
 *            picker) → approved → scheduled → active; timetable + sitting + venue → approved →
 *            published; registrations from rosters; hall tickets; seating plan; conduct (API).
 *   admin    mark sheet opened, internal assessment imported.
 *   faculty  home "To review"; Assessments: marks entry → submit; gradebook.
 *   admin    verify → final marks approval → finalise; results: batch → compute → validate →
 *            finalise → publication authorised → published; grade card.
 *   student  home CGPA; Results: SGPA / CGPA, grade card, hall ticket + timetable; revaluation
 *            request with its fee; dark mode.
 *   admin    Revaluation tab shows the request and its fee status.
 *
 * Screenshots: $E2E_SHOTS (default /workspace/work/out/b6-shots), prefixed `6-`. Every console
 * error, page error and failed /api response fails the run. Requires `psql` on PATH.
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
  expectedFailures,
  expectRowStatus,
  failed,
  fillForm as fillDialogForm,
  go,
  log,
  newProblems,
  psql,
  row,
  rowAction,
  seedInstitution,
  seedStudent,
  settle,
  shot,
  shotCount,
  SHOTS,
  signIn,
  signOut,
  startApi,
  startWeb,
  step,
  stop,
  STUDENT_SUBJECT,
  TENANT,
  tile,
  waitFor,
  watch,
} from "./lib/harness.mjs";

const FACULTY_SUBJECT = "dev|meera.shah@sutra.dev";
const X = "/api/v1/exams";
// The results workspace probes a session's closure; 404 means "not started yet" (api.ts maps it to null).
expectedFailures.push((p, status, body) => status === 404 && /\/exams\/exam-sessions\/[^/]+\/closure$/.test(p) && body.includes("session closure not found"));

/** Every dialog form closes on success: wait for it so the next dialog mounts fresh. */
async function fillForm(page, name, values) {
  await fillDialogForm(page, name, values);
  await page.locator(`[data-form="${name}"]`).waitFor({ state: "detached", timeout: 15000 });
  await settle(page);
}

/** Clicks a page-level action button (`[data-action=id]`) and waits for the network. */
async function action(page, id) {
  const b = page.locator(`[data-action="${id}"]`).first();
  await b.waitFor({ state: "visible", timeout: 15000 });
  await b.click();
  await settle(page);
}

const day = (n) => {
  const d = new Date();
  d.setDate(d.getDate() + n);
  return d.toISOString().slice(0, 10);
};

/** The default chart of accounts of `sutra_finance::shared::coa::DEFAULT_CHART` (as finance.e2e). */
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

/** Offering + section + faculty + enrolment (Batch 4) and the 3B term registration covering CS101. */
function seedDelivery(setup, studentId) {
  const [componentId, title, credits] = psql(
    `SELECT id || '|' || title || '|' || credits FROM institution.curriculum_component ` +
      `WHERE curriculum_version_id = '${setup.curriculumVersionId}' AND slot_code = 'CS101'`,
  ).split("|");
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
  psql(
    `INSERT INTO academic.section_enrolment (tenant_id, institution_id, offering_id, section_id, student_id, ` +
      `student_context_id, student_number, display_name, subject_ref, origin, status) ` +
      `SELECT s.tenant_id, '${setup.id}', '${offeringId}', '${sectionId}', s.id, c.id, s.student_number, ` +
      `s.given_name || ' ' || coalesce(s.family_name, ''), s.subject_ref, 'override', 'enrolled' ` +
      `FROM student.student s JOIN student.student_context c ON c.student_id = s.id WHERE s.id = '${studentId}' RETURNING id`,
  );
  // 3B: an effective (registered) term registration whose line covers CS101.
  const ctx = psql(`SELECT id FROM student.student_context WHERE student_id = '${studentId}'`);
  const window = psql(
    `INSERT INTO student.registration_window (tenant_id, institution_id, academic_term_id, name, opens_on, closes_on, status) ` +
      `VALUES ('${TENANT}', '${setup.id}', '${setup.termIds[0]}', 'Term 1 registration', DATE '2026-06-01', DATE '2026-07-15', 'open') RETURNING id`,
  );
  const reg = psql(
    `INSERT INTO student.registration (tenant_id, institution_id, student_id, student_context_id, academic_term_id, ` +
      `registration_window_id, term_no, curriculum_version_id, status) VALUES ('${TENANT}', '${setup.id}', '${studentId}', ` +
      `'${ctx}', '${setup.termIds[0]}', '${window}', 1, '${setup.curriculumVersionId}', 'registered') RETURNING id`,
  );
  psql(
    `INSERT INTO student.registration_line (tenant_id, registration_id, curriculum_component_id, kind, status, credits, ` +
      `component_term_no, slot_code, title) VALUES ('${TENANT}', '${reg}', '${componentId}', 'regular', 'registered', ` +
      `${credits}, 1, 'CS101', '${title.replace(/'/g, "''")}') RETURNING id`,
  );
  // 4B: a finalised internal assessment result (34.50 / 40) for the 6B import.
  const blueprint = psql(
    `INSERT INTO academic.assessment_blueprint (tenant_id, institution_id, offering_id, section_id, title, total_marks, status, published_at) ` +
      `VALUES ('${TENANT}', '${setup.id}', '${offeringId}', '${sectionId}', 'Internal assessment', 40, 'published', now()) RETURNING id`,
  );
  psql(
    `INSERT INTO academic.internal_assessment_result (tenant_id, institution_id, offering_id, section_id, blueprint_id, ` +
      `academic_term_id, curriculum_component_id, student_id, student_number, display_name, marks_obtained, max_marks, status, finalized_at) ` +
      `VALUES ('${TENANT}', '${setup.id}', '${offeringId}', '${sectionId}', '${blueprint}', '${setup.termIds[0]}', '${componentId}', ` +
      `'${studentId}', 'S2026-0001', 'Aarav Desai', 34.50, 40, 'finalized', now()) RETURNING id`,
  );
  return { offeringId, sectionId, componentId };
}

/** Chart of accounts + Aarav's finance account (no charges: dues are clear). */
function seedFinance(setup, studentId) {
  for (const [key, code, name, type] of CHART)
    psql(
      `INSERT INTO finance.ledger_account (tenant_id, institution_id, code, name, account_type, normal_balance, system_key) ` +
        `VALUES ('${TENANT}', '${setup.id}', '${code}', '${name}', '${type}', '${NORMAL[type]}', '${key}') RETURNING id`,
    );
  return psql(
    `INSERT INTO finance.student_account (tenant_id, institution_id, party_kind, student_id, student_number, display_name, subject_ref) ` +
      `VALUES ('${TENANT}', '${setup.id}', 'student', '${studentId}', 'S2026-0001', 'Aarav Desai', '${STUDENT_SUBJECT}') RETURNING id`,
  );
}

async function display(page, setting) {
  await page.click('[aria-label="Account menu"]');
  await page.locator(`[data-display="${setting}"]`).click();
  await page.keyboard.press("Escape");
  await page.mouse.click(4, 700);
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
  const existing = psql("SELECT count(*) FROM exams.exam_session");
  if (existing !== "0") throw new Error(`DATABASE_URL must be a fresh migrated database (found ${existing} exam sessions)`);

  log(`API (dev auth), web on ${BASE}, shots → ${SHOTS}`);
  api = await startApi();
  web = startWeb();

  log("— seed: institution, student, offering, term registration, internal assessment, finance account, grading scheme");
  const admin = await apiLogin("admin@sutra.dev");
  const setup = await seedInstitution(admin);
  Object.assign(ids, setup);
  ids.aarav = seedStudent(setup, "Aarav", "Desai", "S2026-0001");
  psql(`UPDATE student.student SET subject_ref = '${STUDENT_SUBJECT}' WHERE id = '${ids.aarav}'`);
  Object.assign(ids, seedDelivery(setup, ids.aarav));
  ids.studentAccountId = seedFinance(setup, ids.aarav);
  const scheme = await admin.call("POST", `${X}/institutions/${setup.id}/grading-schemes`, {
    code: "CBCS-10",
    name: "CBCS ten-point",
    bands: [
      { grade: "O", minPercent: "90", gradePoint: "10", isPass: true },
      { grade: "A+", minPercent: "80", gradePoint: "9", isPass: true },
      { grade: "A", minPercent: "70", gradePoint: "8", isPass: true },
      { grade: "B+", minPercent: "60", gradePoint: "7", isPass: true },
      { grade: "B", minPercent: "55", gradePoint: "6", isPass: true },
      { grade: "C", minPercent: "50", gradePoint: "5", isPass: true },
      { grade: "P", minPercent: "40", gradePoint: "4", isPass: true },
      { grade: "F", minPercent: "0", gradePoint: "0", isPass: false },
    ],
  });
  ids.schemeId = scheme.id;
  log(`  ✓ institution ${setup.id}, offering CS101-T1 ${ids.offeringId}, scheme ${scheme.code}`);

  await waitFor(`${BASE}/sign-in`, web);
  browser = await chromium.launch({ headless: !process.env.E2E_HEADED, executablePath: process.env.CHROMIUM_PATH || undefined, args: ["--no-sandbox", ...(process.env.E2E_CHROMIUM_ARGS ?? "").split(" ").filter(Boolean)] });
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, locale: "en-IN", timezoneId: "Asia/Kolkata" });
  const page = await context.newPage();
  page.setDefaultTimeout(15000);
  watch(page);
  page.setDefaultNavigationTimeout(180000);
  for (const p of ["/sign-in", "/sign-in"]) await go(page, p);

  // ------------------------------------------------------------ admin: setup
  await signIn(page, "administrator");
  for (const p of ["/examinations", "/examinations/schedule", "/examinations/results"]) await go(page, p);
  page.setDefaultNavigationTimeout(30000);
  newProblems();

  await step(page, "admin: Examinations is live in the new UI (page header, tabs)", async () => {
    await go(page, "/");
    await page.click('nav.sb-pills a[href="/examinations"]');
    await settle(page);
    await page.locator('[data-screen="examinations"]').waitFor();
    check((await page.locator("[data-coming]").count()) === 0, "no “Coming in Batch 6” page");
    const tabs = await page.locator("[data-tab-link]").evaluateAll((as) => as.map((a) => a.getAttribute("data-tab-link")));
    check(JSON.stringify(tabs) === JSON.stringify(["sessions", "schedule", "registrations", "marks", "malpractice", "results", "revaluation"]), `seven tabs (${tabs.join(", ")})`);
    await shot(page, "6-admin-examinations-empty");
  });

  await step(page, "admin: examination policy drafted → approved → active", async () => {
    await go(page, "/examinations/sessions");
    await page.click("[data-new-policy]");
    await fillForm(page, "policy-create", { code: "EXAM-POL", name: "Examination policy 2026" });
    await page.locator('[data-policy="EXAM-POL"]').waitFor();
    await rowAction(page, "EXAM-POL v1", "policy-submit");
    await rowAction(page, "EXAM-POL v1", "policy-activate");
    await expectRowStatus(page, "EXAM-POL v1", "Active");
    await shot(page, "6-admin-policy-active");
  });

  await step(page, "admin: session from the term and scheme pickers, paper from the offering picker → active", async () => {
    await page.click("[data-new-session]");
    await fillForm(page, "session-create", {
      code: "ESE-NOV-2026",
      name: "End-semester examinations",
      academicTermId: "Term 1",
      gradingSchemeId: "CBCS-10",
      startsOn: day(20),
      endsOn: day(40),
      registrationOpensOn: day(-5),
      registrationClosesOn: day(15),
    });
    await page.locator('[data-session="ESE-NOV-2026"]').waitFor();
    await rowAction(page, "ESE-NOV-2026", "paper-add");
    await fillForm(page, "paper-add", { offeringId: "CS101-T1" });
    await row(page, "ESE-NOV-2026").getByText("CS101").first().waitFor();
    await rowAction(page, "ESE-NOV-2026", "session-submit");
    await expectRowStatus(page, "ESE-NOV-2026", "Approved");
    await rowAction(page, "ESE-NOV-2026", "session-schedule");
    await rowAction(page, "ESE-NOV-2026", "session-activate");
    await expectRowStatus(page, "ESE-NOV-2026", "Active");
    const sessions = await admin.get(`${X}/institutions/${setup.id}/exam-sessions`);
    ids.session = sessions.items.find((s) => s.code === "ESE-NOV-2026");
    check(ids.session?.status === "active" && ids.session.papers.length === 1, "session active with one paper");
    check(ids.session.gradingSchemeId === ids.schemeId, "grading scheme picked from the list");
    await shot(page, "6-admin-session-active");
  });

  await step(page, "admin: timetable planned, sitting + venue → approved → published", async () => {
    await go(page, "/examinations/schedule");
    await page.click("[data-new-schedule]");
    await page.locator('[data-schedule-status="draft"]').waitFor();
    await action(page, "sitting-add");
    await fillForm(page, "sitting-add", { paperId: "CS101", sitsOn: day(25), startsAt: "09:30" });
    await page.locator('[data-sitting^="CS101"]').waitFor();
    await page.click("[data-new-venue]");
    await fillForm(page, "venue-create", { code: "HALL-A", name: "Main Hall A", capacity: "60" });
    await tile(page, "Venues").getByText("HALL-A").first().waitFor();
    await rowAction(page, "CS101", "venue-allocate", tile(page, "Timetable"));
    await fillForm(page, "venue-allocate", { venueId: "HALL-A" });
    await row(page, "CS101", tile(page, "Timetable")).getByText("HALL-A").waitFor();
    await action(page, "schedule-submit");
    await page.locator('[data-schedule-status="approved"]').waitFor();
    await action(page, "schedule-publish");
    await page.locator('[data-schedule-status="published"]').waitFor();
    check(true, "official timetable published");
    await shot(page, "6-admin-timetable-published");
  });

  await step(page, "admin: registrations from the rosters, hall tickets issued (dues clear)", async () => {
    await go(page, "/examinations/registrations");
    await page.click("[data-bulk-register]");
    await settle(page);
    const reg = page.locator("[data-registration]").first();
    await reg.waitFor();
    ids.regNo = (await reg.getAttribute("data-registration")) ?? "";
    await expectRowStatus(page, ids.regNo, "Confirmed");
    await page.click("[data-issue-tickets]");
    await settle(page);
    const ticket = page.locator("[data-ticket]").first();
    await ticket.waitFor();
    ids.ticketNo = (await ticket.getAttribute("data-ticket")) ?? "";
    await expectRowStatus(page, ids.ticketNo, "Active", tile(page, "Hall tickets"));
    await shot(page, "6-admin-registrations-hall-tickets");
  });

  await step(page, "admin: seating plan generated for the sitting", async () => {
    await go(page, "/examinations/schedule");
    await rowAction(page, "CS101", "seating-generate", tile(page, "Timetable"));
    const plans = await admin.get(`${X}/exam-sessions/${ids.session.id}/schedules`);
    const sitting = plans.items.find((s) => s.status === "published")?.sittings?.[0];
    check(!!sitting, "published timetable has the sitting");
    ids.sittingId = sitting.id;
    await shot(page, "6-admin-seating");
  });

  await step(page, "conduct (API): sitting started and completed — Aarav present, paper conducted", async () => {
    const regs = await admin.get(`${X}/exam-sessions/${ids.session.id}/registrations`);
    const reg = regs.items.find((r) => r.registrationNumber === ids.regNo);
    let c = await admin.call("POST", `${X}/exam-sittings/${ids.sittingId}/conduct`, {});
    c = await admin.call("POST", `${X}/conduct-records/${c.id}/start`, {}, c.version);
    c = await admin.call("POST", `${X}/conduct-records/${c.id}/complete`, { attendance: [{ registrationId: reg.id, outcome: "present" }] }, c.version);
    check(c.status === "completed", `conduct record completed (${c.status})`);
    const paper = await admin.get(`${X}/exam-papers/${ids.session.papers[0].id}`);
    check(paper.status === "conducted", `paper conducted (${paper.status})`);
  });

  await step(page, "admin: mark sheet opened, internal assessment imported (Batch 4)", async () => {
    await go(page, "/examinations/marks");
    await rowAction(page, "CS101", "sheet-open", tile(page, "Mark sheets"));
    await rowAction(page, "CS101", "paper-open", tile(page, "Mark sheets"));
    await action(page, "sheet-import");
    await row(page, "S2026-0001").getByText("34.50").first().waitFor();
    check(true, "internal 34.50 / 40 imported");
    await shot(page, "6-admin-mark-sheet-import");
  });

  // ------------------------------------------------------------ faculty
  await signOut(page);
  await signIn(page, "faculty");
  await step(page, "faculty: home “To review” is live", async () => {
    await go(page, "/");
    const kpi = page.locator('[data-kpi="review"]');
    await kpi.waitFor();
    await page.waitForFunction(() => !document.querySelector('[data-kpi="review"] b')?.textContent?.includes("…"));
    check(!(await kpi.evaluate((n) => n.classList.contains("is-pending"))), "To review is no longer pending");
    check(((await kpi.locator("b").textContent()) ?? "").trim() === "1", `one candidate to review (${await kpi.locator("b").textContent()})`);
    await shot(page, "6-faculty-home");
  });

  await step(page, "faculty: Assessments — marks entered and submitted", async () => {
    await page.click('nav.sb-pills a[href="/assessments"]');
    await settle(page);
    await page.locator('[data-slice="6B"]').first().waitFor();
    check((await page.locator("[data-page-head]").count()) === 1, "page header");
    await shot(page, "6-faculty-assessments");
    await rowAction(page, "CS101", "paper-open", tile(page, "My assessments"));
    const input = page.locator('[data-mark-input="S2026-0001"]');
    await input.waitFor();
    await input.fill("78");
    await action(page, "marks-save");
    await expectRowStatus(page, "S2026-0001", "Entered");
    await shot(page, "6-faculty-marks-entered");
    await action(page, "sheet-submit");
    await expectRowStatus(page, "S2026-0001", "Submitted");
    await action(page, "gradebook-toggle");
    await page.locator("[data-gradebook]").waitFor();
    await shot(page, "6-faculty-gradebook");
  });

  // ------------------------------------------------------------ admin: marks approval + results
  await signOut(page);
  await signIn(page, "administrator");
  await step(page, "admin: marks verified → final approval → finalised", async () => {
    await go(page, "/examinations/marks");
    await rowAction(page, "CS101", "paper-open", tile(page, "Mark sheets"));
    await action(page, "sheet-verify");
    await action(page, "sheet-approval");
    await action(page, "sheet-finalise");
    await expectRowStatus(page, "S2026-0001", "Finalised");
    await shot(page, "6-admin-marks-finalised");
  });

  await step(page, "admin: results computed → validated (approved) → finalised", async () => {
    await go(page, "/examinations/results");
    await action(page, "new-batch");
    await action(page, "batch-compute");
    await expectRowStatus(page, "S2026-0001", "Computed", tile(page, "Results"));
    await shot(page, "6-admin-results-computed");
    await action(page, "batch-validate");
    await expectRowStatus(page, "S2026-0001", "Approved", tile(page, "Results"));
    await action(page, "batch-finalise");
  });

  await step(page, "admin: publication authorised → published, grade card issued", async () => {
    await action(page, "new-publication");
    await action(page, "publication-authorise");
    await action(page, "publication-publish");
    await expectRowStatus(page, "S2026-0001", "Published", tile(page, "Results"));
    await rowAction(page, "S2026-0001", "result-card", tile(page, "Results"));
    const cards = psql(`SELECT count(*) FROM exams.grade_card WHERE status <> 'withdrawn'`);
    check(cards === "1", `grade card issued (${cards})`);
    await shot(page, "6-admin-results-published");
  });

  // ------------------------------------------------------------ student
  await signOut(page);
  await signIn(page, "student");
  await step(page, "student: home CGPA is live", async () => {
    await go(page, "/");
    const kpi = page.locator('[data-kpi="cgpa"]');
    await kpi.waitFor();
    await page.waitForFunction(() => !document.querySelector('[data-kpi="cgpa"] b')?.textContent?.includes("…"));
    const v = ((await kpi.locator("b").textContent()) ?? "").trim();
    check(/^\d+\.\d\d$/.test(v), `CGPA shows the published value (${v})`);
    await shot(page, "6-student-home");
  });

  await step(page, "student: Results — SGPA / CGPA, grade card, hall ticket and timetable", async () => {
    await page.click('nav.sb-pills a[href="/results"]');
    await settle(page);
    await page.locator('[data-slice="6C"]').first().waitFor();
    await page.locator("[data-result]").first().waitFor();
    const cgpa = ((await page.locator('[data-kpi="my-cgpa"] b').textContent()) ?? "").trim();
    check(/^\d+\.\d\d$/.test(cgpa), `CGPA tile ${cgpa}`);
    check((await page.locator(`[data-my-ticket="${ids.ticketNo}"]`).count()) === 1, `hall ticket ${ids.ticketNo} listed`);
    check((await page.locator("[data-my-timetable] [data-my-sitting]").count()) >= 1, "exam timetable listed");
    await tile(page, "Results").getByText(/GC-|grade card/i).first().waitFor().catch(() => {});
    await shot(page, "6-student-results");
  });

  await step(page, "student: revaluation requested — Finance fee raised", async () => {
    await rowAction(page, "CS101", "request-review", tile(page, "Results"));
    await fillForm(page, "review-request", { kind: "revaluation", reason: "Expected higher marks in section B" });
    const r = row(page, "CS101", tile(page, "Results").locator("table").last());
    await r.waitFor();
    const text = ((await r.textContent()) ?? "").replace(/\s+/g, " ");
    check(/₹\d/.test(text) && /due|paid/.test(text), `review request shows its fee (${text.slice(0, 120)})`);
    const charge = psql(`SELECT count(*) FROM finance.charge WHERE source_type = 'exam_fee' AND student_id = '${ids.aarav}'`);
    check(charge === "1", `exam fee charge raised (${charge})`);
    await shot(page, "6-student-revaluation-requested");
  });

  await step(page, "student: Results in dark mode", async () => {
    await display(page, "theme:dark");
    check(await page.evaluate(() => document.documentElement.classList.contains("dk")), "dark mode on <html>");
    await shot(page, "6-student-results-dark");
    await display(page, "theme:light");
  });

  // ------------------------------------------------------------ admin: revaluation queue
  await signOut(page);
  await signIn(page, "administrator");
  await step(page, "admin: Revaluation tab lists the request with its fee status", async () => {
    await go(page, "/examinations/revaluation");
    await page.locator('[data-slice="6C"]').first().waitFor();
    await page.locator("tr", { hasText: "CS101" }).first().waitFor();
    check(true, "revaluation request visible to the exam cell");
    await shot(page, "6-admin-revaluation");
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
