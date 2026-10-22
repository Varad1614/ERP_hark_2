/**
 * Rich demo data for `e2e/demo-seed.mjs` (institution DEMO): everything after the core
 * (institution, the four original students, CS101-T1 / A with Dr. Meera Shah).
 *
 * Built from the patterns the CI suites already run: `lib/reskin2-seed.mjs` (people, exams,
 * campus, shell, academics), `lib/journey-seed.mjs` (fee structure), `tests/db_e3a_roster.rs`,
 * `db_e5c_time_tracking.rs`, `db_e3d_expenses.rs`, `db_e5d_gst.rs`. Every write goes through the
 * API, except the rows the existing seeds already write by SQL (students, offerings / sections /
 * faculty assignments / seats, term registrations, role notifications, the leave queue held at
 * pending approval).
 *
 * Idempotent: each slice checks a marker row first and is skipped when it is already there, so a
 * second run only adds what is missing. A slice that throws is reported; the run then exits 1
 * unless DEMO_SEED_LENIENT=1. Sub-parts that are nice-to-have (e.g. result publication) are
 * recorded as notes and never fail the run.
 *
 * Login subjects: Aarav and Meera may be linked to Keycloak subjects (OIDC demo). The few
 * self-service calls made "as Meera" (time entries, expense claims) sign in with the dev persona
 * and point her staff record at `dev|meera.shah@sutra.dev` only for the duration of those calls,
 * then restore the configured subject.
 */
import { apiLogin, I, log, psql } from "./harness.mjs";

const q = (s) => (s === null || s === undefined ? "NULL" : `'${String(s).replace(/'/g, "''")}'`);
const IST_MS = 330 * 60000;
/** ISO date (India time) `n` days from today. */
export const day = (n = 0) => new Date(Date.now() + IST_MS + n * 864e5).toISOString().slice(0, 10);
const dow = (iso) => new Date(`${iso}T00:00:00Z`).getUTCDay(); // 0 Sunday … 6 Saturday
/** The last `count` past weekdays (before today) whose day of week is in `days`, oldest first. */
function pastDays(count, days, from = -1) {
  const out = [];
  for (let n = from; out.length < count && n > -120; n--) if (days.includes(dow(day(n)))) out.push(day(n));
  return out.reverse();
}
const mondayOffset = () => -((dow(day(0)) + 6) % 7);

export const results = [];
export const notes = [];
const note = (m) => {
  notes.push(m);
  log(`  ! ${m}`);
};
const errText = (e) => (e instanceof Error ? e.message : String(e)).slice(0, 400);
async function part(name, fn) {
  try {
    return await fn();
  } catch (e) {
    note(`${name}: ${errText(e)}`);
    return null;
  }
}
/** A slice: skipped when `seeded()` says its marker row exists; a throw is recorded as FAIL. */
async function slice(name, seeded, fn) {
  if (seeded()) {
    results.push([name, "skip", "already seeded"]);
    log(`  = ${name} (already seeded)`);
    return null;
  }
  try {
    const out = await fn();
    results.push([name, "ok", JSON.stringify(out ?? {})]);
    log(`  ✓ ${name} ${JSON.stringify(out ?? {})}`);
    return out ?? {};
  } catch (e) {
    results.push([name, "FAIL", errText(e)]);
    log(`  ✗ ${name}: ${errText(e)}`);
    return null;
  }
}
const exists = (sql) => () => psql(sql) !== "";

// ------------------------------------------------------------------------------- data

/** Added students: [number suffix, given, family, term]. Kiran (0003) and Rohan (0004) stay untouched for the completion / exit walkthroughs. */
export const MORE_STUDENTS = [
  ["0005", "Ananya", "Iyer", 1],
  ["0006", "Vihaan", "Reddy", 1],
  ["0007", "Sneha", "Patwardhan", 1],
  ["0008", "Arjun", "Menon", 1],
  ["0009", "Diya", "Chatterjee", 1],
  ["0010", "Yash", "Gokhale", 1],
  ["0011", "Meghna", "Rao", 1],
  ["0012", "Siddharth", "Bhosale", 1],
  ["0013", "Riya", "Kapoor", 1],
  ["0014", "Aditya", "Jadhav", 1],
  ["0015", "Pooja", "Bhat", 1],
  ["0016", "Aditi", "Sharma", 4],
  ["0017", "Nikhil", "Kale", 4],
  ["0018", "Tanvi", "Deshmukh", 4],
  ["0019", "Omkar", "Pawar", 4],
  ["0020", "Sanya", "Qureshi", 4],
  ["0021", "Harsh", "Agarwal", 4],
  ["0022", "Ishita", "Banerjee", 4],
  ["0023", "Varun", "Saxena", 4],
];

/** Staff: [given, family, category, email local part, designation, department]. Meera first. */
const STAFF = [
  ["Meera", "Shah", "teaching", "meera.shah", "Associate Professor", "CSE"],
  ["Anil", "Kulkarni", "teaching", "anil.kulkarni", "Professor and HoD", "CSE"],
  ["Sunita", "Menon", "teaching", "sunita.menon", "Professor and HoD", "ECE"],
  ["Kiran", "Nair", "teaching", "kiran.nair", "Associate Professor", "ECE"],
  ["Vikram", "Rao", "teaching", "vikram.rao", "Assistant Professor", "MECH"],
  ["Swati", "Gokhale", "teaching", "swati.gokhale", "Assistant Professor (Mathematics)", "ASH"],
  ["Ritu", "Iyer", "teaching", "ritu.iyer", "Professor and Dean", "MGMT"],
  ["Neha", "Joshi", "administrative", "neha.joshi", "Registrar", "ADMIN"],
  ["Rahul", "Bhatt", "administrative", "rahul.bhatt", "Accounts Officer", "ADMIN"],
  ["Sanjay", "Pawar", "non_teaching", "sanjay.pawar", "Lab Superintendent", "MECH"],
  ["Ganesh", "Shinde", "non_teaching", "ganesh.shinde", "Security Supervisor", "ADMIN"],
];
const DEPARTMENTS = [
  ["CSE", "Computer Science and Engineering"],
  ["ECE", "Electronics and Communication"],
  ["MECH", "Mechanical Engineering"],
  ["ASH", "Applied Sciences and Humanities"],
  ["MGMT", "Management Studies"],
  ["ADMIN", "Administration"],
];

// ------------------------------------------------------------------------------- academic (SQL, like the core seed)

function ensureOffering(ctx, { slot, code, title }) {
  const { tenant, setup } = ctx;
  let id = psql(
    `SELECT id FROM academic.course_offering WHERE institution_id = ${q(setup.id)} AND academic_term_id = ${q(setup.termIds[0])} ` +
      `AND lower(offering_code) = lower(${q(code)}) AND status NOT IN ('cancelled', 'archived', 'merged', 'split') LIMIT 1`,
  );
  if (id) return id;
  const [componentId, , credits] = psql(
    `SELECT id || '|' || title || '|' || credits FROM institution.curriculum_component WHERE curriculum_version_id = ${q(setup.curriculumVersionId)} AND slot_code = ${q(slot)}`,
  ).split("|");
  id = psql(
    `INSERT INTO academic.course_offering (tenant_id, institution_id, academic_year_id, academic_term_id, curriculum_version_id, curriculum_component_id, ` +
      `offering_code, title, course_code, category, credits, capacity, status) VALUES (${q(tenant)}, ${q(setup.id)}, ${q(setup.yearId)}, ${q(setup.termIds[0])}, ` +
      `${q(setup.curriculumVersionId)}, ${q(componentId)}, ${q(code)}, ${q(title)}, ${q(slot)}, 'major_core', ${credits}, 60, 'active') RETURNING id`,
  );
  return id;
}

function ensureSection(ctx, offeringId, sectionCode, [facultyName, facultySubject], studentIds) {
  const { tenant, setup } = ctx;
  let sectionId = psql(
    `SELECT id FROM academic.offering_section WHERE offering_id = ${q(offeringId)} AND lower(section_code) = lower(${q(sectionCode)}) AND status NOT IN ('cancelled', 'merged') LIMIT 1`,
  );
  if (!sectionId)
    sectionId = psql(
      `INSERT INTO academic.offering_section (tenant_id, institution_id, offering_id, section_code, capacity, status) ` +
        `VALUES (${q(tenant)}, ${q(setup.id)}, ${q(offeringId)}, ${q(sectionCode)}, 60, 'active') RETURNING id`,
    );
  psql(
    `INSERT INTO academic.faculty_assignment (tenant_id, institution_id, offering_id, section_id, faculty_subject_ref, faculty_name, role, weekly_hours, effective_from, status) ` +
      `SELECT ${q(tenant)}, ${q(setup.id)}, ${q(offeringId)}, ${q(sectionId)}, ${q(facultySubject)}, ${q(facultyName)}, 'primary_instructor', 4, DATE '2026-06-15', 'active' ` +
      `WHERE NOT EXISTS (SELECT 1 FROM academic.faculty_assignment WHERE section_id = ${q(sectionId)} AND role = 'primary_instructor' AND status = 'active')`,
  );
  for (const sid of studentIds)
    psql(
      `INSERT INTO academic.section_enrolment (tenant_id, institution_id, offering_id, section_id, student_id, student_context_id, student_number, display_name, subject_ref, origin, status) ` +
        `SELECT s.tenant_id, ${q(setup.id)}, ${q(offeringId)}, ${q(sectionId)}, s.id, c.id, s.student_number, s.given_name || ' ' || coalesce(s.family_name, ''), s.subject_ref, 'override', 'enrolled' ` +
        `FROM student.student s JOIN student.student_context c ON c.student_id = s.id AND c.status = 'active' WHERE s.id = ${q(sid)} ` +
        `AND NOT EXISTS (SELECT 1 FROM academic.section_enrolment e WHERE e.offering_id = ${q(offeringId)} AND e.student_id = s.id AND e.status = 'enrolled')`,
    );
  return sectionId;
}

/** Term-1 offerings: sections, faculty, seats, and an effective course registration per seated student (reskin2 `seedExams` pattern). */
function seedDelivery(ctx) {
  const { tenant, setup, FACULTY_SUBJECT, term1, term4, aarav, isha, cs101 } = ctx;
  const fac = (email, name) => [name, `dev|${email}@sutra.dev`];
  const meeraA = [aarav, isha, ...term1.slice(0, 6)];
  const restB = term1.slice(6);
  const all1 = [aarav, isha, ...term1];
  const el1 = [aarav, ...term1.filter((_, i) => i % 2 === 0)];
  const offerings = {};
  // CS101: Meera keeps section A (Aarav, Isha + six classmates); Prof. Anil Kulkarni takes B.
  offerings.CS101 = { id: cs101.offeringId, sections: { A: ensureSection(ctx, cs101.offeringId, "A", ["Dr. Meera Shah", FACULTY_SUBJECT], meeraA) } };
  offerings.CS101.sections.B = ensureSection(ctx, cs101.offeringId, "B", fac("anil.kulkarni", "Prof. Anil Kulkarni"), restB);
  const add = (slot, code, title, faculty, students) => {
    const id = ensureOffering(ctx, { slot, code, title });
    offerings[slot] = { id, sections: { A: ensureSection(ctx, id, "A", faculty, students) } };
  };
  add("MD101", "MD101-T1", "Design Thinking and Innovation", fac("sunita.menon", "Prof. Sunita Menon"), all1);
  add("EL101", "EL101-T1", "Basic Electronics Engineering", fac("kiran.nair", "Dr. Kiran Nair"), el1);
  add("CS401", "CS401-T1", "Database Management Systems", fac("anil.kulkarni", "Prof. Anil Kulkarni"), term4);
  add("MD401", "MD401-T1", "Entrepreneurship and Product Management", fac("ritu.iyer", "Prof. Ritu Iyer"), term4);
  add("EL401", "EL401-T1", "Engineering Mathematics III", fac("swati.gokhale", "Dr. Swati Gokhale"), term4);

  // Effective term-1 registrations covering the student's seated offerings.
  const term = setup.termIds[0];
  let win = psql(`SELECT id FROM student.registration_window WHERE institution_id = ${q(setup.id)} AND academic_term_id = ${q(term)} ORDER BY created_at LIMIT 1`);
  if (!win)
    win = psql(
      `INSERT INTO student.registration_window (tenant_id, institution_id, academic_term_id, name, opens_on, closes_on, status) ` +
        `VALUES (${q(tenant)}, ${q(setup.id)}, ${q(term)}, 'Term 1 registration', DATE '2026-06-01', DATE '2026-07-15', 'open') RETURNING id`,
    );
  let lines = 0;
  for (const sid of [...all1, ...term4]) {
    const [ctxId, termNo] = psql(`SELECT id || '|' || current_term_no FROM student.student_context WHERE student_id = ${q(sid)} AND status = 'active'`).split("|");
    let reg = psql(`SELECT id FROM student.registration WHERE student_context_id = ${q(ctxId)} AND academic_term_id = ${q(term)} AND status NOT IN ('cancelled', 'archived') LIMIT 1`);
    if (!reg)
      reg = psql(
        `INSERT INTO student.registration (tenant_id, institution_id, student_id, student_context_id, academic_term_id, registration_window_id, term_no, curriculum_version_id, status) ` +
          `VALUES (${q(tenant)}, ${q(setup.id)}, ${q(sid)}, ${q(ctxId)}, ${q(term)}, ${q(win)}, ${termNo}, ${q(setup.curriculumVersionId)}, 'registered') RETURNING id`,
      );
    const out = psql(
      `INSERT INTO student.registration_line (tenant_id, registration_id, curriculum_component_id, kind, status, credits, component_term_no, slot_code, title) ` +
        `SELECT ${q(tenant)}, ${q(reg)}, cc.id, 'regular', 'registered', cc.credits, cc.term_no, cc.slot_code, cc.title ` +
        `FROM academic.section_enrolment e JOIN academic.course_offering o ON o.id = e.offering_id JOIN institution.curriculum_component cc ON cc.id = o.curriculum_component_id ` +
        `WHERE e.student_id = ${q(sid)} AND e.status = 'enrolled' AND o.academic_term_id = ${q(term)} ` +
        `AND NOT EXISTS (SELECT 1 FROM student.registration_line l WHERE l.registration_id = ${q(reg)} AND l.curriculum_component_id = cc.id AND l.status IN ('selected', 'registered')) RETURNING id`,
    );
    lines += out ? out.split("\n").length : 0;
  }
  const seats = psql(`SELECT count(*) FROM academic.section_enrolment WHERE institution_id = ${q(setup.id)} AND status = 'enrolled'`);
  return { offerings, summary: { offerings: Object.keys(offerings).length, sections: Object.values(offerings).reduce((n, o) => n + Object.keys(o.sections).length, 0), seats: Number(seats), registrationLines: lines } };
}

// ------------------------------------------------------------------------------- slices

const A = "/api/v1/academic";
const F = "/api/v1/finance";
const P = "/api/v1/people";
const R = "/api/v1/resources";
const S = "/api/v1/student";
const X = "/api/v1/exams";
const C = "/api/v1/campus";

/** Seeds every slice; returns the per-slice results (also in `results` / `notes`). */
export async function seedRich(ctx) {
  const { admin, setup } = ctx;
  const inst = setup.id;
  const d = seedDelivery(ctx);
  results.push(["academic delivery", "ok", JSON.stringify(d.summary)]);
  log(`  ✓ academic delivery ${JSON.stringify(d.summary)}`);
  ctx.offerings = d.offerings;
  const secA = (slot) => d.offerings[slot].sections.A;

  // --- timetable (requirements → generate → … → active), API like reskin2 seedAcademics
  await slice("timetable", exists(`SELECT id FROM academic.timetable WHERE institution_id = ${q(inst)} AND academic_term_id = ${q(setup.termIds[0])} LIMIT 1`), async () => {
    const reqs = [
      [secA("CS101"), 3, 60, "B-204", [1, 3, 5]],
      [d.offerings.CS101.sections.B, 3, 60, "B-205", [1, 3, 5]],
      [secA("MD101"), 2, 90, "B-102", [2, 4]],
      [secA("EL101"), 2, 60, "Lab L2", [2, 5]],
      [secA("CS401"), 3, 60, "A-103", [1, 2, 4]],
      [secA("MD401"), 2, 60, "A-104", [3, 5]],
      [secA("EL401"), 3, 60, "A-105", [1, 3, 4]],
    ];
    for (const [sectionId, n, minutes, room, days] of reqs)
      await admin.call("POST", `${A}/institutions/${inst}/scheduling-requirements`, { sectionId, sessionsPerWeek: n, sessionMinutes: minutes, roomLabel: room, preferredDays: days });
    let tt = await admin.call("POST", `${A}/institutions/${inst}/timetables`, { academicTermId: setup.termIds[0], name: "Term 1 timetable 2026-27" });
    for (const cmd of ["generate", "validate", "submit", "publish", "activate"]) tt = await admin.call("POST", `${A}/timetables/${tt.id}/${cmd}`, {}, tt.version);
    return { status: tt.status, slots: tt.slots?.length ?? 0 };
  });

  // --- attendance: finalised past sessions (admin holds attendance.manage)
  await slice("attendance", exists(`SELECT id FROM academic.teaching_session WHERE section_id = ${q(secA("CS101"))} LIMIT 1`), async () => {
    const seated = (sectionId) => psql(`SELECT string_agg(student_id::text, ',' ORDER BY student_number) FROM academic.section_enrolment WHERE section_id = ${q(sectionId)} AND status = 'enrolled'`).split(",").filter(Boolean);
    const plan = [
      [secA("CS101"), pastDays(6, [1, 3, 5]), "10:30", "11:30", ["Arrays and complexity", "Searching and sorting", "Linked lists", "Stacks", "Queues", "Recursion"]],
      [secA("MD101"), pastDays(4, [2, 4]), "14:00", "15:30", ["Empathy mapping", "Problem framing", "Ideation techniques", "Rapid prototyping"]],
      [secA("EL101"), pastDays(3, [2, 5]), "11:45", "12:45", ["Diodes and rectifiers", "Transistor basics", "Op-amps"]],
    ];
    let n = 0;
    for (const [sectionId, dates, startsAt, endsAt, topics] of plan) {
      const ids = seated(sectionId);
      for (const [k, sessionDate] of dates.entries()) {
        let s = await admin.call("POST", `${A}/sections/${sectionId}/sessions`, { sessionDate, startsAt, endsAt, topic: topics[k % topics.length] });
        s = await admin.call("POST", `${A}/sessions/${s.id}/start`, {}, s.version);
        const mark = (i) => ((i * 7 + k * 3) % 11 === 0 ? "absent" : (i + k) % 9 === 4 ? "late" : "present");
        s = await admin.call("POST", `${A}/sessions/${s.id}/attendance`, { records: ids.map((studentId, i) => ({ studentId, mark: mark(i) })) }, s.version);
        s = await admin.call("POST", `${A}/sessions/${s.id}/submit-attendance`, {}, s.version);
        s = await admin.call("POST", `${A}/sessions/${s.id}/complete`, {}, s.version);
        await admin.call("POST", `${A}/sessions/${s.id}/finalise-attendance`, {}, s.version);
        n++;
      }
    }
    return { sessions: n };
  });

  // --- coursework: CS101-A blueprint + three windows (admin, reskin2 pattern)
  await slice("coursework", exists(`SELECT id FROM academic.assessment_blueprint WHERE section_id = ${q(secA("CS101"))} LIMIT 1`), async () => {
    let bp = await admin.call("POST", `${A}/sections/${secA("CS101")}/assessment-blueprints`, {
      title: "CS101 internal assessment",
      totalMarks: 50,
      components: [
        { key: "a1", title: "Assignments", kind: "assignment", weight: 40 },
        { key: "t1", title: "Tests", kind: "test", weight: 60 },
      ],
    });
    bp = await admin.call("POST", `${A}/assessment-blueprints/${bp.id}/publish`, {}, bp.version);
    const at = (n) => new Date(Date.now() + n * 864e5).toISOString();
    let windows = 0;
    for (const [title, due] of [
      ["Linked lists: practice assignment", 2],
      ["Stack applications problem set", 6],
      ["Recursion: written assignment", 10],
    ]) {
      let w = await admin.call("POST", `${A}/sections/${secA("CS101")}/assessment-windows`, { blueprintId: bp.id, componentKey: "a1", title, opensAt: at(-3), dueAt: at(due), maxMarks: 10 });
      w = await admin.call("POST", `${A}/assessment-windows/${w.id}/open`, {}, w.version);
      windows++;
    }
    await part("course resources", async () => {
      for (const [title, kind, url] of [
        ["Unit 2: Linked lists (slides)", "document", "https://demo.sutra.dev/cs101/unit2-slides.pdf"],
        ["Lab 4 handout: linked lists", "document", "https://demo.sutra.dev/cs101/lab4.pdf"],
        ["VisuAlgo: linked list animations", "link", "https://visualgo.net/en/list"],
      ])
        await admin.call("POST", `${A}/sections/${secA("CS101")}/resources`, { title, kind, url, publish: true });
    });
    await part("syllabus coverage", () =>
      admin.call("POST", `${A}/sections/${secA("CS101")}/syllabus-coverage`, {
        summary: "Term 1 coverage",
        topics: [
          ["Unit 1: Arrays and complexity", "completed"],
          ["Unit 2: Linked lists", "completed"],
          ["Unit 3: Stacks and queues", "started"],
          ["Unit 4: Trees and heaps", "planned"],
          ["Unit 5: Graphs and hashing", "planned"],
        ].map(([title, status]) => ({ title, status })),
      }),
    );
    return { windows };
  });

  // --- people
  const staff = await slice("people: staff, departments, engagements", exists(`SELECT id FROM people.staff_member WHERE institution_id = ${q(inst)} AND work_email = 'ganesh.shinde@demo.sutra.dev' LIMIT 1`), () => seedStaff(ctx));
  const staffIds = Object.fromEntries(
    psql(`SELECT string_agg(split_part(work_email, '@', 1) || '=' || id, ',') FROM people.staff_member WHERE institution_id = ${q(inst)} AND status NOT IN ('merged', 'archived')`)
      .split(",")
      .filter(Boolean)
      .map((x) => x.split("=")),
  );
  ctx.staffIds = staffIds;
  if (staff === null && !staffIds["meera.shah"]) note("people slices after staff skipped: no staff records");
  if (staffIds["meera.shah"]) {
    await slice("people: leave", exists(`SELECT id FROM people.leave_policy WHERE institution_id = ${q(inst)} LIMIT 1`), () => seedLeave(ctx));
    await slice("people: duty roster", exists(`SELECT id FROM people.duty_shift WHERE institution_id = ${q(inst)} LIMIT 1`), () => seedRoster(ctx));
    await slice("people: time tracking", exists(`SELECT id FROM people.time_project WHERE institution_id = ${q(inst)} LIMIT 1`), () => seedTime(ctx));
    await slice("people: expense claims", exists(`SELECT id FROM resources.expense_claim WHERE institution_id = ${q(inst)} LIMIT 1`), () => seedExpenses(ctx));
  }

  // --- finance
  await slice("finance: fee heads, GST, fee structure", exists(`SELECT id FROM finance.fee_head WHERE institution_id = ${q(inst)} AND code = 'HOSTEL' LIMIT 1`), () => seedFeeSetup(ctx));
  await slice("finance: charges, payments, concession", exists(`SELECT id FROM finance.payment WHERE institution_id = ${q(inst)} AND external_ref LIKE 'DEMO-%' LIMIT 1`), () => seedFees(ctx));

  // --- students: guardians, scholarship, admissions pipeline
  await slice("students: guardians, scholarship, admissions", exists(`SELECT id FROM student.scholarship WHERE institution_id = ${q(inst)} AND code = 'MERIT-2026' LIMIT 1`), () => seedStudentArea(ctx));

  // --- exams
  await slice("exams: unit test 1 (results published)", exists(`SELECT id FROM exams.exam_session WHERE institution_id = ${q(inst)} AND code = 'UT1-2026' LIMIT 1`), () => seedUnitTest(ctx));
  await slice("exams: mid-semester timetable", exists(`SELECT id FROM exams.exam_session WHERE institution_id = ${q(inst)} AND code = 'MSE-2026' LIMIT 1`), () => seedMidSem(ctx));

  // --- campus + home
  await slice("campus: rooms, hostel, transport, library", exists(`SELECT id FROM campus.infrastructure_node WHERE institution_id = ${q(inst)} AND code = 'MAIN-SITE' LIMIT 1`), () => seedCampus(ctx));
  await slice("home: notices and notifications", exists(`SELECT id FROM platform.notice WHERE tenant_id = ${q(ctx.tenant)} AND title = 'Hostel fee for January–June due 15 December' LIMIT 1`), () => seedHome(ctx));
  return results;
}

const fmtDay = (n) => new Date(`${day(n)}T00:00:00Z`).toLocaleDateString("en-IN", { day: "numeric", month: "long", timeZone: "UTC" });

// ------------------------------------------------------------------------------- people

async function seedStaff(ctx) {
  const { admin, setup, FACULTY_SUBJECT } = ctx;
  const inst = setup.id;
  const units = {};
  await part("departments", async () => {
    const list = await admin.get(`${I}/institutions/${inst}/org-units`);
    const items = list.items ?? list;
    const root = items.find((u) => u.unitType === "university");
    for (const [code, name] of DEPARTMENTS) {
      const have = items.find((u) => u.code === code);
      if (have) {
        units[code] = have.id;
        continue;
      }
      const u = await admin.call("POST", `${I}/institutions/${inst}/org-units`, { unitType: "department", code, name, parentId: root?.id ?? null });
      const a = await admin.call("POST", `${I}/org-units/${u.id}/activate`, {}, u.version);
      units[code] = a.id ?? u.id;
    }
  });
  let created = 0;
  let engaged = 0;
  for (const [given, family, category, email, designation, dept] of STAFF) {
    const workEmail = `${email}@demo.sutra.dev`;
    let id = psql(`SELECT id FROM people.staff_member WHERE institution_id = ${q(inst)} AND work_email = ${q(workEmail)} AND status NOT IN ('merged', 'archived') LIMIT 1`);
    if (!id) {
      const subjectRef = email === "meera.shah" && !psql(`SELECT id FROM people.staff_member WHERE subject_ref = ${q(FACULTY_SUBJECT)} AND status NOT IN ('merged', 'archived') LIMIT 1`) ? FACULTY_SUBJECT : null;
      const s = await admin.call("POST", `${P}/institutions/${inst}/staff`, { givenName: given, familyName: family, staffCategory: category, workEmail, initialStatus: "active", subjectRef });
      id = s.id;
      created++;
    }
    if (psql(`SELECT id FROM people.staff_engagement WHERE staff_id = ${q(id)} LIMIT 1`)) continue;
    let e = await admin.call("POST", `${P}/staff/${id}/engagements`, { employmentType: "permanent", designation, staffCategory: category, startDate: email === "swati.gokhale" ? "2026-06-01" : "2022-07-01", orgUnitId: units[dept] ?? null });
    e = await admin.call("POST", `${P}/engagements/${e.id}/submit`, {}, e.version);
    if (e.pendingChange?.status === "pending_approval") e = await admin.call("POST", `${P}/engagements/${e.id}/decision`, { decision: "approve" }, e.version);
    if (e.status !== "active") e = await admin.call("POST", `${P}/engagements/${e.id}/activate`, {}, e.version);
    if (e.status !== "active") throw new Error(`${email}: engagement ${e.status}`);
    engaged++;
  }
  return { staff: created, departments: Object.keys(units).length, engagements: engaged };
}

async function seedLeave(ctx) {
  const { admin, setup, staffIds } = ctx;
  const inst = setup.id;
  let policy = await admin.call("POST", `${P}/institutions/${inst}/leave-policies`, { name: "Leave policy 2026", effectiveFrom: "2026-01-01", weeklyOffDays: [6, 7] });
  for (const [code, name, annual] of [
    ["CL", "Casual leave", "8.00"],
    ["ML", "Medical leave", "10.00"],
    ["EL", "Earned leave", "15.00"],
    ["DL", "Duty leave", null],
  ])
    await admin.call(
      "POST",
      `${P}/leave-policies/${policy.id}/leave-types`,
      annual ? { code, name, annualEntitlement: annual, accrualFrequency: "yearly", isPaid: true, requiresBalance: true } : { code, name, accrualFrequency: "none", isPaid: true, requiresBalance: false },
    );
  policy = (await admin.get(`${P}/institutions/${inst}/leave-policies`)).items.find((p) => p.id === policy.id);
  const types = Object.fromEntries((policy.leaveTypes ?? []).map((t) => [t.code, t.id]));
  policy = await admin.call("POST", `${P}/leave-policies/${policy.id}/submit`, {}, policy.version);
  if (policy.status === "under_review") policy = await admin.call("POST", `${P}/leave-policies/${policy.id}/decision`, { decision: "approve" }, policy.version);
  if (policy.status !== "active") policy = await admin.call("POST", `${P}/leave-policies/${policy.id}/activate`, {}, policy.version);
  if (policy.status !== "active") throw new Error(`leave policy ${policy.status}`);
  await admin.call("POST", `${P}/institutions/${inst}/leave-accruals`, { leaveYear: 2026 });
  const request = (email, code, startDate, endDate, reason) => admin.call("POST", `${P}/institutions/${inst}/leave-requests`, { staffId: staffIds[email], leaveTypeId: types[code], startDate, endDate, reason });
  let taken = 0;
  for (const row of [
    ["meera.shah", "CL", "2026-08-20", "2026-08-21", "Family function in Nashik"],
    ["meera.shah", "DL", "2026-09-15", "2026-09-15", "External examiner, SPPU practical exams"],
    ["rahul.bhatt", "CL", "2026-09-04", "2026-09-04", "Personal work"],
  ]) {
    const r = await request(...row);
    if (r.status === "reserved") await admin.call("POST", `${P}/leave-requests/${r.id}/consume`, {}, r.version);
    taken++;
  }
  const pending = [];
  for (const row of [
    ["meera.shah", "CL", "2026-11-19", "2026-11-20", "Family wedding in Pune"],
    ["vikram.rao", "ML", "2026-11-16", "2026-11-18", "Minor surgery, certificate attached"],
    ["kiran.nair", "DL", "2026-11-24", "2026-11-26", "Presenting at IEEE INDICON, Bengaluru"],
    ["sanjay.pawar", "CL", "2026-11-27", "2026-11-27", "Child’s school annual day"],
    ["neha.joshi", "EL", "2026-12-14", "2026-12-18", "Year-end family travel"],
  ])
    pending.push((await request(...row)).id);
  // Requests auto-approve in the demo approval mode: hold the queue at pending approval (reskin2 pattern).
  psql(`UPDATE people.leave_request SET status = 'pending_approval' WHERE id IN (${pending.map(q).join(", ")}) AND status IN ('approved', 'reserved')`);
  return { leaveTypes: Object.keys(types).length, taken, pending: pending.length };
}

async function seedRoster(ctx) {
  const { admin, setup, staffIds } = ctx;
  const inst = setup.id;
  await admin.call("PUT", `${P}/institutions/${inst}/roster/settings`, {
    rules: { minRestHours: 11, maxDailyHours: 10 },
    requirements: [
      { role: "Security", days: [0, 1, 2, 3, 4, 5, 6], count: 1, start: "22:00", end: "06:00" },
      { role: "Lab duty", days: [1, 2, 3, 4, 5], count: 1, start: "08:00", end: "16:00" },
    ],
  });
  await part("roster profiles", async () => {
    await admin.call("PUT", `${P}/staff/${staffIds["ganesh.shinde"]}/roster-profile`, { role: "Security", targetHours: 40, maxHours: 48 });
    await admin.call("PUT", `${P}/staff/${staffIds["sanjay.pawar"]}/roster-profile`, { role: "Lab duty", targetHours: 40, maxHours: 48 });
  });
  const mon = mondayOffset();
  const from = day(mon);
  const to = day(mon + 13);
  const shifts = [];
  for (let w = 0; w < 2; w++)
    for (let k = 0; k < 5; k++) {
      const date = day(mon + w * 7 + k);
      shifts.push({ date, start: "22:00", end: "06:00", role: "Security", location: "Main gate", staffId: staffIds["ganesh.shinde"] });
      shifts.push({ date, start: "08:00", end: "16:00", role: "Lab duty", location: "Mechanical workshop", staffId: staffIds["sanjay.pawar"] });
    }
  // Dr. Meera Shah: exam invigilation and the evening project lab.
  shifts.push({ date: day(mon + 1), start: "15:00", end: "18:00", role: "Project lab supervision", location: "Lab L1", staffId: staffIds["meera.shah"] });
  shifts.push({ date: day(mon + 8), start: "09:30", end: "12:30", role: "Exam invigilation", location: "Main Hall A", staffId: staffIds["meera.shah"] });
  shifts.push({ date: day(mon + 10), start: "09:30", end: "12:30", role: "Exam invigilation", location: "Main Hall B", staffId: staffIds["meera.shah"] });
  shifts.push({ date: day(mon + 5), start: "09:00", end: "13:00", role: "Fee counter", location: "Accounts office", staffId: staffIds["rahul.bhatt"] });
  shifts.push({ date: day(mon + 12), start: "09:00", end: "13:00", role: "Fee counter", location: "Accounts office", staffId: null });
  for (const s of shifts) await admin.call("POST", `${P}/institutions/${inst}/roster/shifts`, s);
  const pub = await admin.call("POST", `${P}/institutions/${inst}/roster/publish`, { from, to });
  return { shifts: shifts.length, published: pub.published, from, to };
}

/** Self-service calls as Meera: her staff record points at the dev persona only while `fn` runs. */
async function asMeera(ctx, fn) {
  const staffId = ctx.staffIds["meera.shah"];
  const dev = "dev|meera.shah@sutra.dev";
  const cur = psql(`SELECT coalesce(subject_ref, '') FROM people.staff_member WHERE id = ${q(staffId)}`);
  const swap = cur !== dev;
  if (swap) psql(`UPDATE people.staff_member SET subject_ref = ${q(dev)} WHERE id = ${q(staffId)}`);
  try {
    return await fn(await apiLogin("meera.shah@sutra.dev"));
  } finally {
    if (swap) psql(`UPDATE people.staff_member SET subject_ref = ${cur ? q(cur) : "NULL"} WHERE id = ${q(staffId)}`);
  }
}

async function seedTime(ctx) {
  const { admin, setup, staffIds } = ctx;
  const inst = setup.id;
  await admin.call("PUT", `${P}/institutions/${inst}/time-settings`, { weekStart: 1, roundingMinutes: 0, standardMonthlyHours: 176, defaultHourlyRate: null, timerCapHours: 12 });
  const aicte = await admin.call("POST", `${P}/institutions/${inst}/time-projects`, {
    code: "AICTE-RPS/2026/031",
    name: "Low-cost IoT air-quality sensing for Pune schools",
    kind: "sponsored_research",
    fundingAgency: "AICTE",
    scheme: "Research Promotion Scheme",
    sanctionNumber: "RPS/2026/000031",
    sanctionedAmount: "1850000.00",
    manpowerBudget: "600000.00",
    startDate: "2026-04-01",
    endDate: "2028-03-31",
    piStaffId: staffIds["anil.kulkarni"],
    costCentre: "RES-AICTE-31",
    members: [{ staffId: staffIds["meera.shah"], role: "co_pi", commitmentPercent: 20 }],
    budgetKind: "hours",
    budgetPeriod: "total",
    budgetValue: "1500",
  });
  const iqac = await admin.call("POST", `${P}/institutions/${inst}/time-projects`, { code: "IQAC", name: "IQAC committee — NAAC SSR", kind: "committee", openToAll: true, billableDefault: false, piStaffId: staffIds["ritu.iyer"] });
  const mon = mondayOffset();
  const lastWeek = [0, 1, 2, 3, 4].map((k) => day(mon - 7 + k));
  const thisWeek = [0, 1, 2, 3, 4].map((k) => day(mon + k)).filter((x) => x < day(0));
  return asMeera(ctx, async (meera) => {
    let entries = 0;
    const add = async (projectId, date, start, end, task) => {
      await meera.call("POST", `${P}/me/time/entries`, { projectId, date, start, end, task });
      entries++;
    };
    const tasks = ["Sensor calibration", "Field deployment at partner school", "Data pipeline", "Literature review", "Weekly project review"];
    for (const [k, date] of lastWeek.entries()) await add(aicte.id, date, "16:00", k % 2 ? "18:00" : "17:30", tasks[k]);
    for (const date of [lastWeek[1], lastWeek[3]]) await add(iqac.id, date, "12:00", "13:00", "SSR criterion 2 evidence");
    for (const [k, date] of thisWeek.entries()) await add(aicte.id, date, "16:00", "17:30", tasks[(k + 2) % tasks.length]);
    const sheet = await meera.call("POST", `${P}/me/time/timesheets`, { projectId: aicte.id, week: lastWeek[2] });
    return { projects: 2, entries, timesheet: sheet.status };
  });
}

async function seedExpenses(ctx) {
  const ago = (n) => day(-n);
  return asMeera(ctx, async (meera) => {
    const exp = (spentOn, merchant, amount, categoryId, paymentMethod = "Cash") => meera.call("POST", `${R}/me/expenses`, { spentOn, merchant, amount, categoryId, paymentMethod });
    const claim = async (title, purpose, lines) => {
      const ids = [];
      for (const l of lines) ids.push((await exp(...l)).id);
      return meera.call("POST", `${R}/me/expense-claims`, { title, purpose, expenseIds: ids });
    };
    const out = {};
    // 1. approved
    let c1 = await claim("NAAC peer-team visit preparation", "Evidence collection visits for the SSR", [
      [ago(24), "Auto rickshaw (Shivajinagar)", "180.00", "local_conveyance"],
      [ago(24), "Vaishali Restaurant", "240.00", "meals", "Card"],
      [ago(23), "Pragati Book Depot", "220.00", "books", "Card"],
    ]);
    c1 = await meera.call("POST", `${R}/me/expense-claims/${c1.id}/submit`, { version: c1.version });
    out.approved = await part("expense approval", async () => {
      const approver = await apiLogin("procurement.approver@sutra.dev");
      const a = await approver.call("POST", `${R}/expense-claims/${c1.id}/decision`, { decision: "approve", reason: "Within DA limits", version: c1.version });
      return a.status;
    });
    // 2. submitted, awaiting a decision
    let c2 = await claim("Guest lecture at COEP", "Data structures guest lecture for COEP second-years", [
      [ago(9), "Uber Auto", "210.00", "local_conveyance", "Card"],
      [ago(9), "Cafe Goodluck", "150.00", "meals"],
    ]);
    c2 = await meera.call("POST", `${R}/me/expense-claims/${c2.id}/submit`, { version: c2.version });
    out.submitted = c2.status;
    // 3. draft (receipts still to attach)
    const c3 = await claim("IEEE INDICON 2026, Bengaluru", "Paper presentation (travel approved by the HoD)", [
      [ago(3), "IEEE INDICON 2026 registration", "2500.00", "conference", "Card"],
      [ago(2), "Hotel Ramanashree, Bengaluru", "3450.00", "lodging", "Card"],
    ]);
    out.draft = c3.status;
    return out;
  });
}

// ------------------------------------------------------------------------------- finance

async function seedFeeSetup(ctx) {
  const { admin, setup } = ctx;
  const inst = setup.id;
  const heads = {};
  for (const [code, name, category, refundable] of [
    ["TUITION", "Tuition fee", "tuition", false],
    ["EXAM", "Examination fee", "examination", false],
    ["LIBRARY", "Library fee", "library", false],
    ["HOSTEL", "Hostel fee", "hostel", false],
  ]) {
    const have = psql(`SELECT id FROM finance.fee_head WHERE institution_id = ${q(inst)} AND code = ${q(code)} LIMIT 1`);
    heads[code] = have || (await admin.call("POST", `${F}/institutions/${inst}/fee-heads`, { code, name, category, refundable })).id;
  }
  const gst = await part("GST profile + fee-head tax", async () => {
    await admin.call("PUT", `${F}/institutions/${inst}/gst-profile`, {
      registrationType: "regular",
      gstin: "27AAPFU0939F1ZV",
      legalName: "Sutra Demo College Trust",
      tradeName: "Sutra Demo College",
      address: "1 Campus Road, Pune 411007",
    });
    for (const code of ["TUITION", "EXAM", "LIBRARY"]) await admin.call("PUT", `${F}/institutions/${inst}/gst-fee-heads/${heads[code]}`, { sac: "9992", gstRate: "0", exempt: true, taxNote: "preset:educationExempt" });
    await admin.call("PUT", `${F}/institutions/${inst}/gst-fee-heads/${heads.HOSTEL}`, { sac: "996311", gstRate: "12" });
    return "hostel 12% (CGST 6 + SGST 6), education exempt";
  });
  const structure = async (code, name, lines) => {
    let fs = await admin.call("POST", `${F}/institutions/${inst}/fee-structures`, { code, name, academicYearId: setup.yearId, programmeId: setup.programmeId, dueDate: "2026-08-14", lines });
    fs = await admin.call("POST", `${F}/fee-structures/${fs.id}/submit`, {}, fs.version);
    if (fs.status !== "approved" && fs.status !== "published") fs = await admin.call("POST", `${F}/fee-structures/${fs.id}/decision`, { decision: "approve" }, fs.version);
    if (fs.status !== "published") fs = await admin.call("POST", `${F}/fee-structures/${fs.id}/publish`, {}, fs.version);
    if (fs.status !== "published") throw new Error(`fee structure ${code}: ${fs.status}`);
    return fs;
  };
  const y1 = await structure("BTECH-CSE-Y1-2026", "B.Tech CSE — first year 2026-27", [
    { feeHeadId: heads.TUITION, description: "Tuition fee — 2026-27", amount: "145000.00" },
    { feeHeadId: heads.EXAM, description: "Examination fee — 2026-27", amount: "3500.00" },
    { feeHeadId: heads.LIBRARY, description: "Library fee — 2026-27", amount: "2000.00" },
  ]);
  return { feeHeads: Object.keys(heads).length, gst: gst ?? "skipped", structure: `${y1.code} ₹${y1.totalAmount}` };
}

async function seedFees(ctx) {
  const { admin, setup, aarav, isha, term1, term4 } = ctx;
  const inst = setup.id;
  const head = (code) => psql(`SELECT id FROM finance.fee_head WHERE institution_id = ${q(inst)} AND code = ${q(code)} LIMIT 1`);
  const fsId = psql(`SELECT id FROM finance.fee_structure WHERE institution_id = ${q(inst)} AND code = 'BTECH-CSE-Y1-2026' AND status = 'published' LIMIT 1`);
  if (!fsId) throw new Error("fee structure BTECH-CSE-Y1-2026 is not published (previous slice failed)");
  const first = [aarav, isha, ...term1];
  // First years: the published structure (₹1,50,500 each).
  const g = await admin.call("POST", `${F}/fee-structures/${fsId}/generate-charges`, { studentIds: first, publish: true });
  let charges = g.created?.length ?? 0;
  const manual = async (studentId, description, dueDate, lines) => {
    const c = await admin.call("POST", `${F}/institutions/${inst}/charges`, { studentId, description, dueDate, academicYearId: setup.yearId, academicTermId: setup.termIds[0], lines, publish: true });
    charges++;
    return c;
  };
  // Fourth-term students: second-year fees, raised by hand.
  for (const sid of term4)
    await manual(sid, "B.Tech CSE — second year 2026-27", "2026-08-14", [
      { feeHeadId: head("TUITION"), description: "Tuition fee — 2026-27", amount: "152000.00" },
      { feeHeadId: head("EXAM"), description: "Examination fee — 2026-27", amount: "3500.00" },
    ]);
  // Hostel residents (GST 12% on the hostel line when the GST profile is set).
  const hostel = [aarav, term1[0], term1[2], term1[4], term4[0], term4[2], term4[5]];
  for (const sid of hostel)
    await part("hostel charge", () => manual(sid, "Hostel fee — July to December 2026", "2026-07-31", [{ feeHeadId: head("HOSTEL"), description: "Hostel rent — July to December 2026", amount: "42000.00" }]));
  const account = (sid) => psql(`SELECT id FROM finance.student_account WHERE institution_id = ${q(inst)} AND student_id = ${q(sid)} LIMIT 1`);
  const outstanding = (sid) => Number(psql(`SELECT coalesce(sum(outstanding_amount), 0) FROM finance.charge WHERE institution_id = ${q(inst)} AND student_id = ${q(sid)} AND status = 'published'`));
  // Payments: [student, amount or 'all', received on, channel]
  const plan = [
    [aarav, "150500.00", "2026-07-10", "bank_transfer"],
    [aarav, "25000.00", "2026-08-12", "cash"],
    [isha, "all", "2026-07-08", "bank_transfer"],
    [term1[0], "all", "2026-07-14", "bank_transfer"],
    [term1[1], "75000.00", "2026-07-21", "bank_transfer"],
    [term1[2], "all", "2026-07-30", "bank_transfer"],
    [term1[3], "all", "2026-08-05", "cash"],
    [term1[5], "50000.00", "2026-08-18", "bank_transfer"],
    [term1[6], "all", "2026-09-02", "bank_transfer"],
    [term1[8], "all", "2026-09-11", "bank_transfer"],
    [term4[0], "all", "2026-07-12", "bank_transfer"],
    [term4[1], "100000.00", "2026-07-25", "bank_transfer"],
    [term4[2], "all", "2026-08-08", "bank_transfer"],
    [term4[3], "all", "2026-09-20", "cash"],
    [term4[5], "60000.00", "2026-10-05", "bank_transfer"],
  ];
  let paid = 0;
  let total = 0;
  for (const [k, [sid, amt, receivedOn, channel]] of plan.entries()) {
    const acc = account(sid);
    if (!acc) continue;
    const amount = amt === "all" ? outstanding(sid).toFixed(2) : amt;
    if (Number(amount) <= 0) continue;
    await admin.call("POST", `${F}/student-accounts/${acc}/payments`, { channel, amount, receivedOn, externalRef: `DEMO-${receivedOn.replace(/-/g, "")}-${String(k + 1).padStart(2, "0")}` });
    paid++;
    total += Number(amount);
  }
  // Merit concession (25% of tuition) on one first-year charge.
  const concession = await part("merit concession", async () => {
    const charge = psql(`SELECT id FROM finance.charge WHERE institution_id = ${q(inst)} AND student_id = ${q(term1[7])} AND status = 'published' ORDER BY created_at LIMIT 1`);
    let adj = await admin.call("POST", `${F}/charges/${charge}/adjustments`, { kind: "concession", amount: "36250.00", reason: "Merit scholarship 2026 — 25% of tuition (HSC 96.4%)" });
    adj = await admin.call("POST", `${F}/adjustments/${adj.id}/submit`, {}, adj.version);
    if (adj.status === "pending_approval") {
      const fin = await apiLogin("finance.approver@sutra.dev");
      adj = await fin.call("POST", `${F}/adjustments/${adj.id}/decision`, { decision: "approve", reason: "Merit list 2026" }, adj.version);
    }
    return adj.status;
  });
  await part("receivables", () => admin.call("POST", `${F}/institutions/${inst}/receivables`, {}));
  return { charges, payments: paid, collected: `₹${total.toLocaleString("en-IN")}`, concession };
}

// ------------------------------------------------------------------------------- students

async function seedStudentArea(ctx) {
  const { admin, setup, aarav, isha, term1, term4 } = ctx;
  const inst = setup.id;
  const out = { guardians: 0, applications: 0 };
  const guardians = [
    [aarav, "Sunita", "Desai", "mother", "+91 98220 44556", "Teacher"],
    [aarav, "Prakash", "Desai", "father", "+91 98220 44557", "Civil engineer"],
    [isha, "Madhavi", "Kulkarni", "mother", "+91 98230 11872", "Bank officer"],
    [term1[0], "Ramesh", "Iyer", "father", "+91 98450 23019", "Chartered accountant"],
    [term1[1], "Lakshmi", "Reddy", "mother", "+91 98490 77120", "Doctor"],
    [term1[3], "Suresh", "Menon", "father", "+91 98950 31244", "Businessman"],
    [term4[0], "Anita", "Sharma", "mother", "+91 98110 55210", "Homemaker"],
    [term4[1], "Deepak", "Kale", "father", "+91 98600 41288", "Farmer"],
  ];
  for (const [sid, givenName, familyName, relationshipType, phone, occupation] of guardians) {
    const email = `${givenName}.${familyName}@example.in`.toLowerCase();
    let g = psql(`SELECT id FROM student.guardian WHERE institution_id = ${q(inst)} AND email = ${q(email)} LIMIT 1`);
    if (!g) g = (await admin.call("POST", `${S}/institutions/${inst}/guardians`, { givenName, familyName, phone, email, occupation })).id;
    await part(`guardian link ${email}`, () => admin.call("POST", `${S}/students/${sid}/guardian-relationships`, { guardianId: g, relationshipType, isPrimary: relationshipType === "mother" || sid !== aarav }));
    out.guardians++;
  }
  const sch = await admin.call("POST", `${S}/institutions/${inst}/scholarships`, { code: "MERIT-2026", name: "Merit scholarship 2026", sponsorType: "institutional", benefitType: "fee_percentage", benefitValue: 25, validFrom: "2026-06-01", renewable: true });
  await admin.call("POST", `${S}/scholarships/${sch.id}/publish`, {}, sch.version);
  await part("admissions pipeline", async () => {
    for (const [given, family, dob, steps] of [
      ["Ishaan", "Verma", "2008-04-11", []],
      ["Meera", "Pillai", "2008-01-23", ["submit"]],
      ["Rehan", "Shaikh", "2007-11-05", ["submit", "review"]],
      ["Gauri", "Phadke", "2008-06-30", ["submit", "review", "offer"]],
    ]) {
      let a = await admin.call("POST", `${S}/institutions/${inst}/admission-applications`, {
        programmeId: setup.programmeId,
        givenName: given,
        familyName: family,
        dateOfBirth: dob,
        email: `${given}.${family}@example.in`.toLowerCase(),
        phone: `+91 98200 4${String(out.applications).padStart(4, "0")}`,
      });
      for (const st of steps)
        a = await admin.call("POST", `${S}/admission-applications/${a.id}/${st}`, st === "offer" ? { offerExpiresOn: "2026-12-31", requiredDocuments: ["identity_proof", "qualifying_marksheet"] } : {}, a.version);
      out.applications++;
    }
  });
  out.scholarship = "MERIT-2026 published";
  return out;
}

// ------------------------------------------------------------------------------- exams

const BANDS = [
  ["O", "90", "10"], ["A+", "80", "9"], ["A", "70", "8"], ["B+", "60", "7"], ["B", "55", "6"], ["C", "50", "5"], ["P", "40", "4"], ["F", "0", "0"],
].map(([grade, minPercent, gradePoint]) => ({ grade, minPercent, gradePoint, isPass: grade !== "F" }));

async function examBasics(ctx) {
  const { admin, setup } = ctx;
  const inst = setup.id;
  let scheme = psql(`SELECT id FROM exams.grading_scheme WHERE institution_id = ${q(inst)} AND code = 'CBCS-10' LIMIT 1`);
  if (!scheme) scheme = (await admin.call("POST", `${X}/institutions/${inst}/grading-schemes`, { code: "CBCS-10", name: "CBCS ten-point", bands: BANDS })).id;
  if (!psql(`SELECT id FROM exams.exam_policy WHERE institution_id = ${q(inst)} AND status = 'active' LIMIT 1`)) {
    let pol = await admin.call("POST", `${X}/institutions/${inst}/exam-policies`, { code: "EXAM-POL", name: "Examination policy 2026", requireDuesCleared: false });
    pol = await admin.call("POST", `${X}/exam-policies/${pol.id}/submit`, {}, pol.version);
    if (pol.status !== "active") await admin.call("POST", `${X}/exam-policies/${pol.id}/activate`, {}, pol.version);
  }
  const venues = [];
  for (const [code, name, capacity] of [["MH-A", "Main Hall A", 120], ["MH-B", "Main Hall B", 120], ["B-204", "B-204", 64]]) {
    const have = psql(`SELECT id FROM exams.exam_venue WHERE institution_id = ${q(inst)} AND code = ${q(code)} LIMIT 1`);
    venues.push(have ? { id: have } : await admin.call("POST", `${X}/institutions/${inst}/exam-venues`, { code, name, capacity }));
  }
  return { scheme, venues };
}

async function newSession(ctx, { code, name, scheme, start, end, regOpen, regClose, slots }) {
  const { admin, setup } = ctx;
  let ses = await admin.call("POST", `${X}/institutions/${setup.id}/exam-sessions`, {
    code,
    name,
    academicTermId: setup.termIds[0],
    gradingSchemeId: scheme,
    startsOn: day(start),
    endsOn: day(end),
    registrationOpensOn: day(regOpen),
    registrationClosesOn: day(regClose),
  });
  for (const slot of slots) await admin.call("POST", `${X}/exam-sessions/${ses.id}/papers`, { offeringId: ctx.offerings[slot].id });
  ses = await admin.get(`${X}/exam-sessions/${ses.id}`);
  ses = await admin.call("POST", `${X}/exam-sessions/${ses.id}/submit`, {}, ses.version);
  if (ses.status === "approved") ses = await admin.call("POST", `${X}/exam-sessions/${ses.id}/schedule`, {}, ses.version);
  if (ses.status !== "active") ses = await admin.call("POST", `${X}/exam-sessions/${ses.id}/activate`, {}, ses.version);
  if (ses.status !== "active") throw new Error(`${code}: session ${ses.status}`);
  return ses;
}

async function timetable(ctx, ses, venues, name, at) {
  const { admin } = ctx;
  const created = await admin.call("POST", `${X}/exam-sessions/${ses.id}/schedules`, { name });
  const papers = [...ses.papers].sort((a, b) => a.paperCode.localeCompare(b.paperCode, "en", { numeric: true }));
  const sittings = [];
  for (const [i, p] of papers.entries()) {
    const [sitsOn, startsAt] = at(i);
    sittings.push(await admin.call("POST", `${X}/exam-schedules/${created.id}/sittings`, { paperId: p.id, sitsOn, startsAt }));
  }
  for (const [i, x] of sittings.entries()) await admin.call("POST", `${X}/exam-sittings/${x.id}/venues`, { venueId: venues[i % venues.length].id });
  let sch = (await admin.get(`${X}/exam-sessions/${ses.id}/schedules`)).items.find((x) => x.id === created.id);
  sch = await admin.call("POST", `${X}/exam-schedules/${sch.id}/submit`, {}, sch.version);
  if (sch.status === "approved") sch = await admin.call("POST", `${X}/exam-schedules/${sch.id}/publish`, {}, sch.version);
  if (sch.status !== "published") throw new Error(`${ses.code}: timetable ${sch.status}`);
  return sittings;
}

/** Unit test 1 on Aarav's three courses: conducted, marked, finalised, results published, grade card, closed. */
async function seedUnitTest(ctx) {
  const { admin, aarav } = ctx;
  const { scheme, venues } = await examBasics(ctx);
  const ses = await newSession(ctx, { code: "UT1-2026", name: "Unit test 1", scheme, start: 0, end: 2, regOpen: -7, regClose: 0, slots: ["CS101", "MD101", "EL101"] });
  const sittings = await timetable(ctx, ses, venues, "Unit test 1 timetable", (i) => [day(i), "10:00"]);
  const bulk = await admin.call("POST", `${X}/exam-sessions/${ses.id}/registrations/bulk`, {});
  await admin.call("POST", `${X}/exam-sessions/${ses.id}/hall-tickets/issue`, {});
  let marked = 0;
  for (const [n, x] of sittings.entries()) {
    const c0 = await admin.call("POST", `${X}/exam-sittings/${x.id}/conduct`, {});
    const c1 = await admin.call("POST", `${X}/conduct-records/${c0.id}/start`, {}, c0.version);
    await admin.call("POST", `${X}/conduct-records/${c1.id}/complete`, { attendance: [], defaultOutcome: "present" }, c1.version);
    await admin.call("POST", `${X}/exam-papers/${x.paperId}/mark-sheet`, {});
    const m = await admin.get(`${X}/exam-papers/${x.paperId}/marks`);
    const rows = m.items.filter((r) => r.effective).map((r, i) => ({ id: r.id, version: r.version, externalMarks: (Number(r.externalMax || "100") * (0.52 + (((i + n) * 11) % 45) / 100)).toFixed(2) }));
    if (rows.length) await admin.call("PUT", `${X}/exam-papers/${x.paperId}/marks`, { rows });
    let sheet = (await admin.get(`${X}/exam-papers/${x.paperId}/marks`)).sheet;
    for (const cmd of ["submit", "verify", "request-approval"]) sheet = await admin.call("POST", `${X}/mark-sheets/${sheet.id}/${cmd}`, {}, sheet.version);
    if (sheet.awaitingDecision) sheet = await admin.call("POST", `${X}/mark-sheets/${sheet.id}/decision`, { decision: "approve" }, sheet.version);
    await admin.call("POST", `${X}/mark-sheets/${sheet.id}/finalise`, {}, sheet.version);
    marked++;
  }
  const published = await part("result publication", async () => {
    let batch = await admin.call("POST", `${X}/exam-sessions/${ses.id}/result-batches`, {});
    const computed = await admin.call("POST", `${X}/result-batches/${batch.id}/compute`, {}, batch.version);
    batch = computed.batch ?? computed;
    batch = await admin.call("POST", `${X}/result-batches/${batch.id}/validate`, {}, batch.version);
    if (batch.status === "validated" && batch.processInstanceId) batch = await admin.call("POST", `${X}/result-batches/${batch.id}/decision`, { decision: "approve" }, batch.version);
    await admin.call("POST", `${X}/result-batches/${batch.id}/finalise`, {}, batch.version);
    let pub = await admin.call("POST", `${X}/exam-sessions/${ses.id}/publications`, {});
    pub = await admin.call("POST", `${X}/publications/${pub.id}/authorise`, {}, pub.version);
    if (pub.processInstanceId && pub.status !== "authorised") pub = await admin.call("POST", `${X}/publications/${pub.id}/decision`, { decision: "approve" }, pub.version);
    await admin.call("POST", `${X}/publications/${pub.id}/publish`, {}, pub.version);
    const res = (await admin.get(`${X}/exam-sessions/${ses.id}/results`)).items;
    const mine = res.find((r) => r.studentId === aarav && r.status === "published");
    if (mine) await part("grade card", () => admin.call("POST", `${X}/term-results/${mine.id}/grade-card`, {}));
    await part("session closure", async () => {
      const closing = await admin.call("POST", `${X}/exam-sessions/${ses.id}/closure/initiate`, {});
      await admin.call("POST", `${X}/exam-sessions/${ses.id}/closure/close`, {}, closing.version);
    });
    return res.filter((r) => r.status === "published").length;
  });
  return { papers: sittings.length, registered: bulk.processed, marked, resultsPublished: published ?? 0 };
}

/** Mid-semester examinations for all six term-1 offerings: published timetable, registrations, hall tickets. */
async function seedMidSem(ctx) {
  const { admin } = ctx;
  if (psql(`SELECT id FROM exams.exam_session WHERE institution_id = ${q(ctx.setup.id)} AND code = 'UT1-2026' AND status = 'active' LIMIT 1`))
    throw new Error("UT1-2026 is still active (its closure failed), so a second regular session of term 1 is refused");
  const { scheme, venues } = await examBasics(ctx);
  const ses = await newSession(ctx, { code: "MSE-2026", name: "Mid-semester examinations", scheme, start: 14, end: 17, regOpen: -3, regClose: 10, slots: ["CS101", "MD101", "EL101", "CS401", "MD401", "EL401"] });
  const sittings = await timetable(ctx, ses, venues, "Mid-semester timetable", (i) => [day(14 + Math.floor(i / 2)), i % 2 ? "14:00" : "10:00"]);
  const bulk = await admin.call("POST", `${X}/exam-sessions/${ses.id}/registrations/bulk`, {});
  await part("hall tickets", () => admin.call("POST", `${X}/exam-sessions/${ses.id}/hall-tickets/issue`, {}));
  return { papers: sittings.length, registered: bulk.processed, from: day(14) };
}

// ------------------------------------------------------------------------------- campus + home

async function seedCampus(ctx) {
  const { admin, setup, aarav, isha, term1, term4 } = ctx;
  const inst = setup.id;
  const year = Number(day(0).slice(0, 4));
  const campusId = psql(`SELECT id FROM institution.campus WHERE institution_id = ${q(inst)} ORDER BY created_at LIMIT 1`);
  const node = async (body) => (await admin.call("POST", `${C}/institutions/${inst}/infrastructure-nodes`, { status: "active", ...body })).node;
  const rooms = {};
  let library = null;
  const hostelRooms = [];
  const site = await node({ nodeType: "site", campusId, code: "MAIN-SITE", name: "Main campus" });
  for (const [code, name, spaceType, spaces, capacity] of [
    ["MAIN-A", "A block", "classroom", ["A-103", "A-104", "A-105"], 60],
    ["MAIN-B", "B block", "classroom", ["B-102", "B-204", "B-205"], 60],
    ["MAIN-L", "Lab block", "lab", ["Lab L1", "Lab L2"], 30],
  ]) {
    const b = await node({ nodeType: "building", parentId: site.id, code, name });
    const f = await node({ nodeType: "floor", parentId: b.id, code: `${code}-F1`, name: `${name}, first floor` });
    for (const sp of spaces) rooms[sp] = await node({ nodeType: "space", parentId: f.id, code: `${code}-${sp.replace(/[^A-Za-z0-9]/g, "").toUpperCase()}`, name: spaceType === "lab" ? sp : `Room ${sp}`, spaceType, capacity });
    if (code === "MAIN-A") {
      rooms["Seminar hall"] = await node({ nodeType: "space", parentId: f.id, code: "MAIN-A-SEM", name: "Seminar hall", spaceType: "seminar_room", capacity: 120 });
      library = await node({ nodeType: "space", parentId: f.id, code: "MAIN-A-LIB", name: "Central library", spaceType: "library", capacity: 150 });
    }
  }
  for (const [code, name] of [["MAIN-H1", "Boys’ hostel"], ["MAIN-H2", "Girls’ hostel"]]) {
    const b = await node({ nodeType: "building", parentId: site.id, code, name });
    const f = await node({ nodeType: "floor", parentId: b.id, code: `${code}-G`, name: `${name}, ground floor` });
    for (const r of ["G01", "G02", "G03"]) hostelRooms.push(await node({ nodeType: "space", parentId: f.id, code: `${code}-${r}`, name: `${name} room ${r}`, spaceType: "hostel_room", capacity: 2 }));
  }
  const today = day(0);
  await part("room bookings", async () => {
    for (const [room, startTime, endTime, attendees, purpose] of [
      ["A-103", "18:00", "19:00", 40, "Data structures — remedial class"],
      ["B-204", "18:30", "20:00", 52, "Placement aptitude session"],
      ["Seminar hall", "16:00", "17:30", 110, "Guest lecture: careers in cloud computing"],
    ])
      await admin.call("POST", `${C}/institutions/${inst}/reservations`, { nodeId: rooms[room].id, date: today, startTime, endTime, attendees, purpose });
  });
  await part("maintenance tickets", async () => {
    for (const [title, category, priority, room, assignee] of [
      ["Projector not working", "it", "high", "B-204", "Ravi Kumar (AV)"],
      ["AC not cooling", "facility", "medium", "Seminar hall", null],
      ["Lab PC-17 does not boot", "laboratory", "medium", "Lab L2", "IT desk"],
    ]) {
      let t = await admin.call("POST", `${C}/institutions/${inst}/tickets`, { category, priority, title, nodeId: rooms[room]?.id ?? null });
      if (assignee) t = await admin.call("POST", `${C}/tickets/${t.id}/assign`, { assignee }, t.version);
    }
  });
  await part("hostel", async () => {
    for (const r of hostelRooms) for (const bed of ["A", "B"]) await admin.call("POST", `${C}/hostel-rooms/${r.id}/beds`, { bedCode: bed });
    let pol = await admin.call("POST", `${C}/institutions/${inst}/hostel-policies`, { name: `Hostel policy ${year}-${String(year + 1).slice(2)}`, feePerTerm: "42000.00", deposit: "5000.00", curfewTime: "21:30", effectiveFrom: `${year}-01-01` });
    pol = await admin.call("POST", `${C}/hostel-policies/${pol.id}/submit`, {}, pol.version);
    if (pol.status !== "approved" && pol.status !== "active") pol = await admin.call("POST", `${C}/hostel-policies/${pol.id}/decision`, { decision: "approve" }, pol.version);
    if (pol.status !== "active") await admin.call("POST", `${C}/hostel-policies/${pol.id}/activate`, {}, pol.version);
    for (const sid of [aarav, term1[0], term1[2], term1[4], term4[0], term4[2]]) {
      let app = await admin.call("POST", `${C}/institutions/${inst}/hostel-applications`, { personKind: "student", personId: sid, preference: "Near the library" });
      if (app.status === "under_review") app = await admin.call("POST", `${C}/hostel-applications/${app.id}/decision`, { decision: "approve" }, app.version);
      if (app.status === "approved") await admin.call("POST", `${C}/hostel-applications/${app.id}/allocate`, {}, app.version);
    }
  });
  await part("transport", async () => {
    const stops = [];
    for (const [code, name, landmark] of [
      ["ST-HIN", "Hinjewadi Phase 1", "IT park gate"],
      ["ST-WAK", "Wakad chowk", "Bus stand"],
      ["ST-KOT", "Kothrud depot", "PMPML depot"],
      ["ST-KAR", "Karve Nagar", "Market"],
      ["ST-GATE", "Main gate", "Campus entrance"],
    ])
      stops.push(await admin.call("POST", `${C}/institutions/${inst}/transport-stops`, { code, name, landmark }));
    const live = [];
    for (const [code, name, seq] of [
      ["R-01", "Hinjewadi – Wakad – Campus", [0, 1, 4]],
      ["R-02", "Kothrud – Karve Nagar – Campus", [2, 3, 4]],
    ]) {
      const route = await admin.call("POST", `${C}/institutions/${inst}/transport-routes`, { code, name, capacity: 52, fare: "18000.00", effectiveFrom: `${year}-01-01` });
      const v1 = (await admin.get(`${C}/transport-routes/${route.id}/versions`)).items[0];
      await admin.call("POST", `${C}/route-versions/${v1.id}/stops`, { stops: seq.map((i, n) => ({ stopId: stops[i].id, sequence: n + 1, pickupTime: `07:${String(15 + n * 15).padStart(2, "0")}` })) }, v1.version);
      await admin.call("POST", `${C}/transport-routes/${route.id}/activate`, {}, route.version);
      live.push({ route, stops: seq.slice(0, -1).map((i) => stops[i]) });
    }
    for (const [n, sid] of [isha, ...term1.slice(5, 9), ...term4.slice(3, 6)].entries()) {
      const { route, stops: on } = live[n % 2];
      await admin.call("POST", `${C}/institutions/${inst}/transport-allocations`, { personKind: "student", personId: sid, routeId: route.id, stopId: on[n % on.length].id });
    }
  });
  await part("library", async () => {
    let pol = await admin.call("POST", `${C}/institutions/${inst}/library-policies`, { name: `Library rules ${year}`, effectiveFrom: `${year}-01-01`, loanDaysStudent: 14, loanDaysStaff: 30, maxLoansStudent: 3, maxLoansStaff: 6, maxRenewals: 1, finePerDay: "2.00", reservationHoldDays: 3, lostChargeFactor: "1.5" });
    pol = await admin.call("POST", `${C}/library-policies/${pol.id}/submit`, {}, pol.version);
    if (pol.approvalState === "pending") pol = await admin.call("POST", `${C}/library-policies/${pol.id}/decision`, { decision: "approve" }, pol.version);
    if (pol.status !== "active") await admin.call("POST", `${C}/library-policies/${pol.id}/activate`, {}, pol.version);
    const mA = await admin.call("POST", `${C}/institutions/${inst}/library-memberships`, { personKind: "student", personId: aarav });
    const mI = await admin.call("POST", `${C}/institutions/${inst}/library-memberships`, { personKind: "student", personId: isha });
    const copies = {};
    for (const [title, authors, shelfMark] of [
      ["Introduction to Algorithms", ["Cormen", "Leiserson", "Rivest", "Stein"], "CSE-005"],
      ["Data Structures Using C", ["Reema Thareja"], "CSE-021"],
      ["Higher Engineering Mathematics", ["B. S. Grewal"], "MAT-510"],
      ["Electronic Devices and Circuit Theory", ["Boylestad", "Nashelsky"], "ECE-104"],
      ["The C Programming Language", ["Kernighan", "Ritchie"], "CSE-001"],
      ["Wings of Fire", ["A. P. J. Abdul Kalam"], "GEN-112"],
    ]) {
      const rec = await admin.call("POST", `${C}/institutions/${inst}/catalogue`, { title, authors, replacementCost: "650.00" });
      await admin.call("POST", `${C}/catalogue-records/${rec.id}/publish`, {}, rec.version);
      copies[title] = await admin.call("POST", `${C}/catalogue-records/${rec.id}/copies`, { nodeId: library?.id, shelfMark });
    }
    for (const t of ["Data Structures Using C", "Higher Engineering Mathematics"]) await admin.call("POST", `${C}/institutions/${inst}/library-loans`, { membershipId: mA.id, copyId: copies[t].id });
    await admin.call("POST", `${C}/institutions/${inst}/library-loans`, { membershipId: mI.id, copyId: copies["Introduction to Algorithms"].id });
  });
  return { rooms: Object.keys(rooms).length, hostelRooms: hostelRooms.length };
}

async function seedHome(ctx) {
  const { admin, tenant } = ctx;
  const ALL = ["administrator", "faculty", "student"];
  const notices = [
    ["Campus Wi-Fi maintenance on Saturday", "Wi-Fi and the ERP may be briefly unavailable between 7:00 and 9:00 AM. Wired lab machines are unaffected.", ALL, false],
    ["Library open until 8:00 PM on weekdays", "Extended hours run through the examination season. Saturday hours stay 9:00 AM to 2:00 PM.", ALL, false],
    ["Hostel fee for January–June due 15 December", "Pay at the accounts office counter or by bank transfer; quote your student number.", ["administrator", "student"], false],
    [`Mid-semester examinations from ${fmtDay(14)}`, "The timetable is published. Hall tickets are on the student portal; carry your college ID to every paper.", ALL, true],
  ];
  for (const [title, body, audienceRoles, pinned] of notices) await admin.call("POST", "/api/v1/notices", { title, body, audienceRoles, pinned });
  const NOTES = {
    administrator: [["system", "Fee receipts for October reconciled", "Accounts office", "/finance"], ["todo", "Staff leave requests pending", "HR operations", "/people"], ["todo", "Expense claim awaiting approval", "Procurement", "/campus"]],
    faculty: [["notice", "Mid-semester timetable published", "Exam cell", "/notices"], ["institution", "Room change for CS101 on Friday: B-204", "Academic office", "/timetable"]],
    student: [["notice", "Hall tickets available", "Exam cell", "/notices"], ["system", "Fee receipt ready", "Accounts office", "/fees"], ["todo", "New assignment: Linked lists practice", "Programming 1", "/assignments"]],
  };
  let n = 0;
  for (const [role, rows] of Object.entries(NOTES))
    for (const [i, [category, title, body, link]] of rows.entries()) {
      psql(
        `INSERT INTO platform.notification (tenant_id, recipient_role, category, title, body, link, created_at, visible_at) ` +
          `SELECT ${q(tenant)}, ${q(role)}, ${q(category)}, ${q(title)}, ${q(body)}, ${q(link)}, now() - interval '${(i + 1) * 53} minutes', now() - interval '${(i + 1) * 53} minutes' ` +
          `WHERE NOT EXISTS (SELECT 1 FROM platform.notification WHERE tenant_id = ${q(tenant)} AND recipient_role = ${q(role)} AND title = ${q(title)})`,
      );
      n++;
    }
  return { notices: notices.length, notifications: n };
}
