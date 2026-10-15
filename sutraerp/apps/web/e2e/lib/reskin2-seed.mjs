/**
 * Re-skin v2 demo seed for the visual-proof suite (`e2e/reskin2-shots.e2e.mjs`), on a fresh
 * migrated database. Fills every area with prototype-like volumes so screens are not empty:
 *
 *   core      institution SIT (active) via `seedInstitution`, 16 students (names from the
 *             prototype), Aarav linked to the dev student login
 *   academic  offering CS101-T1 section A taught by Dr. Meera Shah, every student seated
 *   finance   default chart of accounts, tuition fee head, a published tuition charge per student
 *             (₹92,500–₹2,40,000, due dates spread over the term) with its journal entry, bank
 *             payments received Jul–Oct, three fee adjustments awaiting approval
 *   people    12 active staff records (Meera linked to the dev faculty login)
 *
 * Each domain is its own function so slices R2–R7 add their area's rows in their own function
 * (merge-friendly). A failing domain is reported (seed-report) and the run goes on; only the core
 * seed is fatal. Every insert goes through the API where a create endpoint exists, else through
 * SQL shaped like the Rust test fixtures (tests/support/b*.rs).
 */
import { apiLogin, check, I, log, psql, seedInstitution, seedStudent, STUDENT_SUBJECT, TENANT } from "./harness.mjs";

export const FACULTY_SUBJECT = "dev|meera.shah@sutra.dev";
const q = (s) => `'${String(s).replace(/'/g, "''")}'`;

/** Prototype student names (adm-students.png). */
export const DEMO_STUDENTS = [
  ["Aarav", "Desai"],
  ["Aditi", "Sharma"],
  ["Rohan", "Patil"],
  ["Ishita", "Iyer"],
  ["Kabir", "Kulkarni"],
  ["Ananya", "Reddy"],
  ["Vihaan", "Joshi"],
  ["Sneha", "Nair"],
  ["Arjun", "Gupta"],
  ["Diya", "Menon"],
  ["Yash", "Deshpande"],
  ["Kavya", "Rao"],
  ["Siddharth", "Chatterjee"],
  ["Riya", "Kapoor"],
  ["Aditya", "Mehta"],
  ["Pooja", "Bhat"],
];

/** Prototype staff (adm-people.png): [given, family, category, email local part]. */
export const DEMO_STAFF = [
  ["Meera", "Shah", "teaching", "meera.shah"],
  ["Anil", "Kulkarni", "teaching", "anil.kulkarni"],
  ["Sunita", "Menon", "teaching", "sunita.menon"],
  ["Vikram", "Rao", "teaching", "vikram.rao"],
  ["Kiran", "Nair", "teaching", "kiran.nair"],
  ["Ritu", "Iyer", "teaching", "ritu.iyer"],
  ["Lata", "Deshmukh", "teaching", "lata.deshmukh"],
  ["Farhan", "Qureshi", "teaching", "farhan.qureshi"],
  ["Neha", "Joshi", "administrative", "neha.joshi"],
  ["Rahul", "Bhatt", "administrative", "rahul.bhatt"],
  ["Sanjay", "Pawar", "non_teaching", "sanjay.pawar"],
  ["Priya", "Iyer", "administrative", "priya.iyer"],
];

export const report = [];
async function domain(name, fn) {
  try {
    const out = await fn();
    report.push(`ok    ${name}`);
    log(`  ✓ seed ${name}`);
    return out;
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    report.push(`FAIL  ${name}: ${msg.slice(0, 400)}`);
    log(`  ✗ seed ${name}: ${msg.slice(0, 400)}`);
    return null;
  }
}

/** Core (fatal on failure): active institution + students. */
export async function seedCore(admin) {
  const setup = await seedInstitution(admin);
  const students = DEMO_STUDENTS.map(([g, f], i) => ({ id: seedStudent(setup, g, f, `SIT25-${String(i + 1).padStart(4, "0")}`), name: `${g} ${f}`, number: `SIT25-${String(i + 1).padStart(4, "0")}` }));
  psql(`UPDATE student.student SET subject_ref = '${STUDENT_SUBJECT}' WHERE id = '${students[0].id}'`);
  check(students.length === DEMO_STUDENTS.length, `${students.length} demo students`);
  return { setup, students };
}

/** Academic delivery: CS101-T1 section A, Dr. Meera Shah primary instructor, every student seated. */
export function seedAcademic(setup, students) {
  const component = psql(
    `SELECT id || '|' || title || '|' || credits FROM institution.curriculum_component ` +
      `WHERE curriculum_version_id = '${setup.curriculumVersionId}' AND slot_code = 'CS101'`,
  );
  const [componentId, title, credits] = component.split("|");
  const offeringId = psql(
    `INSERT INTO academic.course_offering (tenant_id, institution_id, academic_year_id, academic_term_id, ` +
      `curriculum_version_id, curriculum_component_id, offering_code, title, course_code, category, credits, capacity, status) ` +
      `VALUES ('${TENANT}', '${setup.id}', '${setup.yearId}', '${setup.termIds[0]}', '${setup.curriculumVersionId}', ` +
      `'${componentId}', 'CS101-T1', ${q(title)}, 'CS101', 'major_core', ${credits}, 60, 'active') RETURNING id`,
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
  for (const s of students)
    psql(
      `INSERT INTO academic.section_enrolment (tenant_id, institution_id, offering_id, section_id, student_id, ` +
        `student_context_id, student_number, display_name, subject_ref, origin, status) ` +
        `SELECT s.tenant_id, '${setup.id}', '${offeringId}', '${sectionId}', s.id, c.id, s.student_number, ` +
        `s.given_name || ' ' || coalesce(s.family_name, ''), s.subject_ref, 'override', 'enrolled' ` +
        `FROM student.student s JOIN student.student_context c ON c.student_id = s.id WHERE s.id = '${s.id}' RETURNING id`,
    );
  return { offeringId, sectionId };
}

/** The default chart of `sutra_finance::shared::coa::DEFAULT_CHART` (same codes, so the lazy provisioning on first read finds every role). */
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
const AMOUNTS = ["185000.00", "92500.00", "185000.00", "92500.00", "185000.00", "120000.00", "87500.00", "240000.00"];
const DUES = ["2026-07-15", "2026-11-30", "2026-07-15", "2026-08-31", "2026-07-15", "2026-11-30", "2026-08-31", "2026-07-15"];
/** Aarav's (the dev student's) other invoices, like stu-fees.png: [description, amount, due]; the last one stays upcoming. */
const AARAV_EXTRA = [
  ["Hostel fee — Q2", "18000.00", "2026-06-30"],
  ["Examination fee — term 1", "2500.00", "2026-08-28"],
  ["Tuition fee — term 2", "185000.00", "2027-01-15"],
];

/** Bank payments (student index, amount, date received): receipts spread over Jul–Oct for the monthly collections chart. */
const PAYMENTS = [
  [0, "18000.00", "2026-06-29"],
  [0, "185000.00", "2026-07-10"],
  [0, "2500.00", "2026-08-27"],
  [2, "185000.00", "2026-07-12"],
  [4, "185000.00", "2026-07-14"],
  [7, "240000.00", "2026-07-02"],
  [5, "60000.00", "2026-08-20"],
  [1, "92500.00", "2026-09-05"],
  [8, "185000.00", "2026-10-03"],
];
/** Fee adjustments left awaiting a decision (student index, kind, amount, reason). */
const ADJUSTMENTS = [
  [3, "concession", "23125.00", "Merit scholarship 25%"],
  [6, "waiver", "2500.00", "Late fee waiver"],
  [9, "credit_note", "5000.00", "Sibling concession"],
];

/**
 * Finance: chart + fee head + one published tuition charge (and its posting) per student, then
 * bank payments through the API (back-dated so the monthly chart spans Jul–Oct) and three fee
 * adjustments drafted through the API and left `pending_approval` (as under ApprovalMode::AwaitDecision).
 */
export async function seedFinance(admin, setup, students) {
  for (const [key, code, name, type] of CHART)
    psql(
      `INSERT INTO finance.ledger_account (tenant_id, institution_id, code, name, account_type, normal_balance, system_key) ` +
        `VALUES ('${TENANT}', '${setup.id}', '${code}', ${q(name)}, '${type}', '${NORMAL[type]}', '${key}') RETURNING id`,
    );
  const account = (key) => psql(`SELECT id FROM finance.ledger_account WHERE institution_id = '${setup.id}' AND system_key = '${key}'`);
  const income = account("fee_income");
  const receivables = account("student_receivables");
  const feeHeadId = psql(
    `INSERT INTO finance.fee_head (tenant_id, institution_id, code, name, category, income_account_id) ` +
      `VALUES ('${TENANT}', '${setup.id}', 'TUITION', 'Tuition fee', 'tuition', '${income}') RETURNING id`,
  );
  let n = 0;
  const accounts = [];
  const charges = [];
  const raise = (s, accountId, description, amount, due) => {
    n++;
    const num = String(n).padStart(6, "0");
    const chargeId = psql(
      `INSERT INTO finance.charge (tenant_id, institution_id, student_account_id, student_id, charge_number, academic_year_id, ` +
        `academic_term_id, description, total_amount, due_date, source_type, status, published_at) VALUES ('${TENANT}', '${setup.id}', ` +
        `'${accountId}', '${s.id}', 'CHG-${num}', '${setup.yearId}', '${setup.termIds[0]}', ${q(description)}, ` +
        `${amount}, DATE '${due}', 'manual', 'published', now()) RETURNING id`,
    );
    psql(
      `INSERT INTO finance.charge_line (tenant_id, charge_id, line_no, fee_head_id, description, amount, income_account_id) ` +
        `VALUES ('${TENANT}', '${chargeId}', 1, '${feeHeadId}', ${q(description)}, ${amount}, '${income}') RETURNING id`,
    );
    psql(
      `WITH e AS (INSERT INTO finance.journal_entry (tenant_id, institution_id, entry_number, posting_date, kind, source_type, source_id, ` +
        `posting_key, description, total_debit, total_credit) VALUES ('${TENANT}', '${setup.id}', 'JE-${num}', DATE '2026-07-01', 'standard', ` +
        `'charge', '${chargeId}', 'publish', 'Charge CHG-${num} published', ${amount}, ${amount}) RETURNING id) ` +
        `INSERT INTO finance.journal_line (tenant_id, journal_entry_id, line_no, ledger_account_id, debit, credit, student_account_id, charge_id) ` +
        `SELECT '${TENANT}'::uuid, e.id, 1, '${receivables}'::uuid, ${amount}, 0, '${accountId}'::uuid, '${chargeId}'::uuid FROM e ` +
        `UNION ALL SELECT '${TENANT}'::uuid, e.id, 2, '${income}'::uuid, 0, ${amount}, NULL::uuid, NULL::uuid FROM e RETURNING journal_entry_id`,
    );
    return chargeId;
  };
  students.forEach((s, i) => {
    const subject = i === 0 ? q(STUDENT_SUBJECT) : "NULL";
    const accountId = psql(
      `INSERT INTO finance.student_account (tenant_id, institution_id, party_kind, student_id, student_number, display_name, subject_ref) ` +
        `VALUES ('${TENANT}', '${setup.id}', 'student', '${s.id}', '${s.number}', ${q(s.name)}, ${subject}) RETURNING id`,
    );
    accounts.push(accountId);
    charges.push(raise(s, accountId, "Tuition fee — term 1", AMOUNTS[i % AMOUNTS.length], DUES[i % DUES.length]));
  });
  if (students[0]) for (const [description, amount, due] of AARAV_EXTRA) raise(students[0], accounts[0], description, amount, due);
  psql(
    `INSERT INTO finance.number_sequence (tenant_id, institution_id, kind, next_value) VALUES ` +
      `('${TENANT}', '${setup.id}', 'charge', ${n + 1}), ('${TENANT}', '${setup.id}', 'journal', ${n + 1}) RETURNING kind`,
  );
  // The chart must read back without provisioning conflicts (the R0 shots logged a 409 here).
  const chart = await admin.get(`/api/v1/finance/institutions/${setup.id}/ledger-accounts`);
  check(chart.items.filter((a) => a.systemKey).length === CHART.length, `chart of accounts reads back (${chart.items.length} accounts)`);
  let paid = 0;
  for (const [i, amount, on] of PAYMENTS) {
    if (!accounts[i]) continue;
    const p = await admin.call("POST", `/api/v1/finance/student-accounts/${accounts[i]}/payments`, { channel: "bank_transfer", amount, externalRef: `UTR-${on.replace(/-/g, "")}-${i + 1}` });
    psql(`UPDATE finance.payment SET received_on = DATE '${on}', completed_at = TIMESTAMPTZ '${on} 11:30:00+05:30' WHERE id = '${p.id}' RETURNING id`);
    psql(`UPDATE finance.receipt SET issued_at = TIMESTAMPTZ '${on} 11:30:00+05:30' WHERE payment_id = '${p.id}' RETURNING id`);
    paid++;
  }
  // Receivables: recognise every published charge (ageing / dunning on the Receivables tab).
  await admin.call("POST", `/api/v1/finance/institutions/${setup.id}/receivables`, {});
  let pending = 0;
  for (const [i, kind, amount, reason] of ADJUSTMENTS) {
    if (!charges[i]) continue;
    const a = await admin.call("POST", `/api/v1/finance/charges/${charges[i]}/adjustments`, { kind, amount, reason });
    psql(`UPDATE finance.adjustment SET status = 'pending_approval', process_instance_id = 'reskin2-seed', version = version + 1 WHERE id = '${a.id}' RETURNING id`);
    pending++;
  }
  return { feeHeadId, charges: n, payments: paid, adjustments: pending };
}

/** Prototype departments (adm-people.png chips) → org-unit code. */
const DEPARTMENTS = [
  ["CSE", "Computer Engg"],
  ["ECE", "Electronics"],
  ["MECH", "Mechanical"],
  ["CIVIL", "Civil"],
  ["MGMT", "Management"],
  ["ADMIN", "Administration"],
];

/** Prototype roles (adm-people.png), by DEMO_STAFF email: [designation, department code]. */
const STAFF_ROLES = {
  "meera.shah": ["Associate Professor", "CSE"],
  "anil.kulkarni": ["Professor and HoD", "CSE"],
  "sunita.menon": ["Professor and HoD", "ECE"],
  "vikram.rao": ["Assistant Professor", "MECH"],
  "kiran.nair": ["Associate Professor", "ECE"],
  "ritu.iyer": ["Professor and Dean", "MGMT"],
  "lata.deshmukh": ["Assistant Professor", "CIVIL"],
  "farhan.qureshi": ["Professor and HoD", "CIVIL"],
  "neha.joshi": ["Registrar", "ADMIN"],
  "rahul.bhatt": ["Accounts Officer", "ADMIN"],
  "sanjay.pawar": ["Lab Superintendent", "MECH"],
  "priya.iyer": ["HR Manager", "ADMIN"],
};

/** Leave types of the demo policy: [code, name, days a year (null = no balance)]. */
const LEAVE_TYPES = [
  ["CL", "Casual leave", "8.00"],
  ["ML", "Medical leave", "10.00"],
  ["EL", "Earned leave", "15.00"],
  ["CFL", "Conference leave", null],
  ["DL", "Duty leave", null],
];

/** Pending leave requests (adm-people-tab-leave-requests.png), weekdays of Nov–Dec 2026. */
const PENDING_LEAVE = [
  ["meera.shah", "CL", "2026-11-19", "2026-11-20", "Family function in Pune"],
  ["vikram.rao", "ML", "2026-11-16", "2026-11-20", "Minor surgery, certificate attached"],
  ["kiran.nair", "CFL", "2026-11-24", "2026-11-26", "Presenting at IEEE INDICON, Bengaluru"],
  ["rahul.bhatt", "CL", "2026-11-23", "2026-11-23", "Personal work at the bank"],
  ["farhan.qureshi", "EL", "2026-12-01", "2026-12-10", "Annual family travel"],
  ["lata.deshmukh", "DL", "2026-11-20", "2026-11-20", "External examiner at COEP"],
  ["sanjay.pawar", "CL", "2026-11-19", "2026-11-19", "Child’s school event"],
  ["anil.kulkarni", "CFL", "2026-12-03", "2026-12-04", "ACM India council meeting"],
  ["priya.iyer", "EL", "2026-12-14", "2026-12-18", "Year-end family travel"],
  ["sunita.menon", "ML", "2026-11-17", "2026-11-19", "Viral fever"],
  ["ritu.iyer", "DL", "2026-11-25", "2026-11-25", "AICTE review panel, Mumbai"],
  ["neha.joshi", "CL", "2026-11-26", "2026-11-26", "Family wedding"],
];

/** Meera's leave already taken (fac-leave.png history). */
const TAKEN_LEAVE = [
  ["meera.shah", "CL", "2026-08-20", "2026-08-21", "Family function"],
  ["meera.shah", "EL", "2026-07-07", "2026-07-07", "Conference travel"],
];

const P = "/api/v1/people";

/**
 * People: 12 active staff records through the API (Meera linked to the dev faculty login), then
 * (R6) prototype departments as org units, one active engagement per person (designation +
 * department — Meera's engagement is what `GET /people/me/leave-types` needs), an active leave
 * policy with opening entitlements, Meera's taken leave and 12 pending leave requests.
 */
export async function seedPeople(admin, setup) {
  const ids = [];
  const byEmail = {};
  for (const [given, family, category, email] of DEMO_STAFF) {
    const s = await admin.call("POST", `${P}/institutions/${setup.id}/staff`, {
      givenName: given,
      familyName: family,
      staffCategory: category,
      workEmail: `${email}@sit.edu.in`,
      initialStatus: "active",
      subjectRef: email === "meera.shah" ? FACULTY_SUBJECT : null,
    });
    ids.push(s.id);
    byEmail[email] = s;
  }

  // Departments (org units under the root unit). Best effort: engagements go on without one.
  const units = {};
  try {
    const list = await admin.get(`${I}/institutions/${setup.id}/org-units`);
    const root = (list.items ?? list).find((u) => u.unitType === "university");
    for (const [code, name] of DEPARTMENTS) {
      const u = await admin.call("POST", `${I}/institutions/${setup.id}/org-units`, { unitType: "department", code, name, parentId: root?.id ?? null });
      const a = await admin.call("POST", `${I}/org-units/${u.id}/activate`, {}, u.version);
      units[code] = a.id ?? u.id;
    }
  } catch (e) {
    log(`  ! seed people: departments skipped (${e instanceof Error ? e.message.slice(0, 200) : e})`);
  }

  // One active engagement per person (pending → submit → approve when asked → activate).
  const engagements = {};
  for (const [, , category, email] of DEMO_STAFF) {
    const [designation, dept] = STAFF_ROLES[email];
    let e = await admin.call("POST", `${P}/staff/${byEmail[email].id}/engagements`, {
      employmentType: "permanent",
      designation,
      staffCategory: category,
      startDate: "2025-07-01",
      orgUnitId: units[dept] ?? null,
    });
    e = await admin.call("POST", `${P}/engagements/${e.id}/submit`, {}, e.version);
    if (e.pendingChange?.status === "pending_approval") e = await admin.call("POST", `${P}/engagements/${e.id}/decision`, { decision: "approve" }, e.version);
    if (e.status !== "active") e = await admin.call("POST", `${P}/engagements/${e.id}/activate`, {}, e.version);
    check(e.status === "active", `${email} engagement active (${e.status})`);
    engagements[email] = e;
  }

  // Leave policy (Mon–Fri working week) → active, opening entitlements for the year.
  let policy = await admin.call("POST", `${P}/institutions/${setup.id}/leave-policies`, { name: "Leave policy 2026", effectiveFrom: "2026-01-01", weeklyOffDays: [6, 7] });
  for (const [code, name, annual] of LEAVE_TYPES) {
    await admin.call(
      "POST",
      `${P}/leave-policies/${policy.id}/leave-types`,
      annual
        ? { code, name, annualEntitlement: annual, accrualFrequency: "yearly", isPaid: true, requiresBalance: true }
        : { code, name, accrualFrequency: "none", isPaid: true, requiresBalance: false },
    );
  }
  // POST …/leave-types answers with the whole policy: take the type ids from its leaveTypes.
  policy = (await admin.get(`${P}/institutions/${setup.id}/leave-policies`)).items.find((p) => p.id === policy.id);
  const types = Object.fromEntries((policy.leaveTypes ?? []).map((t) => [t.code, t.id]));
  check(LEAVE_TYPES.every(([code]) => types[code]), `leave types created (${Object.keys(types).join(", ")})`);
  policy = await admin.call("POST", `${P}/leave-policies/${policy.id}/submit`, {}, policy.version);
  if (policy.status === "under_review") policy = await admin.call("POST", `${P}/leave-policies/${policy.id}/decision`, { decision: "approve" }, policy.version);
  if (policy.status !== "active") policy = await admin.call("POST", `${P}/leave-policies/${policy.id}/activate`, {}, policy.version);
  check(policy.status === "active", `leave policy active (${policy.status})`);
  await admin.call("POST", `${P}/institutions/${setup.id}/leave-accruals`, { leaveYear: 2026 });

  // Taken leave (consumed), then the pending queue (requested on behalf, held at pending approval).
  const request = (email, code, startDate, endDate, reason) =>
    admin.call("POST", `${P}/institutions/${setup.id}/leave-requests`, { staffId: byEmail[email].id, leaveTypeId: types[code], startDate, endDate, reason });
  for (const row of TAKEN_LEAVE) {
    const r = await request(...row);
    if (r.status === "reserved") await admin.call("POST", `${P}/leave-requests/${r.id}/consume`, {}, r.version);
  }
  const pending = [];
  for (const row of PENDING_LEAVE) pending.push((await request(...row)).id);
  // Requests are auto-approved until Batch 9 workflows: hold the demo queue at pending approval.
  psql(`UPDATE people.leave_request SET status = 'pending_approval' WHERE id IN (${pending.map((id) => `'${id}'`).join(", ")}) AND status IN ('approved', 'reserved') RETURNING id`);
  return { staff: ids.length, departments: Object.keys(units).length, engagements: Object.keys(engagements).length, pendingLeave: pending.length };
}

const X = "/api/v1/exams";
const dayIso = (n) => {
  const d = new Date();
  d.setDate(d.getDate() + n);
  return d.toISOString().slice(0, 10);
};

/**
 * R4 Exams: a live mid-semester session like `adm-examinations.png` — 12 papers (every curriculum
 * component offered in term 1, every student seated and registered), a published timetable of
 * two sittings a day over six days in four halls, registrations + hall tickets, and four papers
 * conducted with marks entered and submitted (evaluated → "Result verification" cards). Meera
 * teaches CS101 / MD101 / EL101 (faculty Assessments). Idempotent towards other slices' rows
 * (reuses an existing term-1 offering / window / registration instead of inserting a duplicate).
 */
export async function seedExams(admin, setup, students) {
  const term = setup.termIds[0];
  const components = psql(
    `SELECT string_agg(id || '|' || slot_code || '|' || replace(title, '|', '/') || '|' || credits || '|' || term_no, ';' ORDER BY term_no, slot_code) ` +
      `FROM institution.curriculum_component WHERE curriculum_version_id = '${setup.curriculumVersionId}'`,
  )
    .split(";")
    .filter(Boolean)
    .map((r) => {
      const [id, slot, title, credits, termNo] = r.split("|");
      return { id, slot, title, credits, termNo };
    });
  check(components.length >= 4, `${components.length} curriculum components`);
  const ids = students.map((s) => `'${s.id}'`).join(", ");
  // Offerings + section A + seats (reuse an existing term-1 offering of the component).
  const offerings = [];
  for (const c of components) {
    let off = psql(`SELECT id FROM academic.course_offering WHERE institution_id = '${setup.id}' AND academic_term_id = '${term}' AND curriculum_component_id = '${c.id}' ORDER BY created_at LIMIT 1`);
    if (!off)
      off = psql(
        `INSERT INTO academic.course_offering (tenant_id, institution_id, academic_year_id, academic_term_id, curriculum_version_id, ` +
          `curriculum_component_id, offering_code, title, course_code, category, credits, capacity, status) VALUES ('${TENANT}', '${setup.id}', ` +
          `'${setup.yearId}', '${term}', '${setup.curriculumVersionId}', '${c.id}', '${c.slot}-T1', ${q(c.title)}, '${c.slot}', 'major_core', ${c.credits}, 60, 'active') RETURNING id`,
      );
    let sec = psql(`SELECT id FROM academic.offering_section WHERE offering_id = '${off}' ORDER BY created_at LIMIT 1`);
    if (!sec) sec = psql(`INSERT INTO academic.offering_section (tenant_id, institution_id, offering_id, section_code, capacity, status) VALUES ('${TENANT}', '${setup.id}', '${off}', 'A', 60, 'active') RETURNING id`);
    if (["CS101", "MD101", "EL101"].includes(c.slot) && psql(`SELECT count(*) FROM academic.faculty_assignment WHERE section_id = '${sec}'`) === "0")
      psql(
        `INSERT INTO academic.faculty_assignment (tenant_id, institution_id, offering_id, section_id, faculty_subject_ref, faculty_name, role, weekly_hours, effective_from, status) ` +
          `VALUES ('${TENANT}', '${setup.id}', '${off}', '${sec}', '${FACULTY_SUBJECT}', 'Dr. Meera Shah', 'primary_instructor', 4, DATE '2026-06-15', 'active') RETURNING id`,
      );
    psql(
      `INSERT INTO academic.section_enrolment (tenant_id, institution_id, offering_id, section_id, student_id, student_context_id, student_number, display_name, subject_ref, origin, status) ` +
        `SELECT s.tenant_id, '${setup.id}', '${off}', '${sec}', s.id, c.id, s.student_number, s.given_name || ' ' || coalesce(s.family_name, ''), s.subject_ref, 'override', 'enrolled' ` +
        `FROM student.student s JOIN student.student_context c ON c.student_id = s.id WHERE s.id IN (${ids}) ` +
        `AND NOT EXISTS (SELECT 1 FROM academic.section_enrolment e WHERE e.offering_id = '${off}' AND e.student_id = s.id) RETURNING id`,
    );
    offerings.push({ ...c, offeringId: off });
  }
  // Effective term-1 registration of every student covering every component (3B).
  let win = psql(`SELECT id FROM student.registration_window WHERE institution_id = '${setup.id}' AND academic_term_id = '${term}' ORDER BY created_at LIMIT 1`);
  if (!win)
    win = psql(
      `INSERT INTO student.registration_window (tenant_id, institution_id, academic_term_id, name, opens_on, closes_on, status) ` +
        `VALUES ('${TENANT}', '${setup.id}', '${term}', 'Term 1 registration', DATE '2026-06-01', DATE '2026-07-15', 'open') RETURNING id`,
    );
  for (const s of students) {
    const ctx = psql(`SELECT id FROM student.student_context WHERE student_id = '${s.id}'`);
    let reg = psql(`SELECT id FROM student.registration WHERE student_context_id = '${ctx}' AND academic_term_id = '${term}' AND status NOT IN ('cancelled', 'archived') LIMIT 1`);
    if (!reg)
      reg = psql(
        `INSERT INTO student.registration (tenant_id, institution_id, student_id, student_context_id, academic_term_id, registration_window_id, term_no, curriculum_version_id, status) ` +
          `VALUES ('${TENANT}', '${setup.id}', '${s.id}', '${ctx}', '${term}', '${win}', 1, '${setup.curriculumVersionId}', 'registered') RETURNING id`,
      );
    else psql(`UPDATE student.registration SET status = 'registered' WHERE id = '${reg}' RETURNING id`);
    for (const c of offerings)
      psql(
        `INSERT INTO student.registration_line (tenant_id, registration_id, curriculum_component_id, kind, status, credits, component_term_no, slot_code, title) ` +
          `SELECT '${TENANT}', '${reg}', '${c.id}', 'regular', 'registered', ${c.credits}, ${c.termNo}, '${c.slot}', ${q(c.title)} ` +
          `WHERE NOT EXISTS (SELECT 1 FROM student.registration_line l WHERE l.registration_id = '${reg}' AND l.curriculum_component_id = '${c.id}' AND l.status IN ('selected', 'registered')) RETURNING id`,
      );
  }
  // Grading scheme, policy (dues not checked: the finance seed leaves tuition outstanding).
  const scheme = await admin.call("POST", `${X}/institutions/${setup.id}/grading-schemes`, {
    code: "CBCS-10",
    name: "CBCS ten-point",
    bands: [
      ["O", "90", "10"], ["A+", "80", "9"], ["A", "70", "8"], ["B+", "60", "7"], ["B", "55", "6"], ["C", "50", "5"], ["P", "40", "4"], ["F", "0", "0"],
    ].map(([grade, minPercent, gradePoint]) => ({ grade, minPercent, gradePoint, isPass: grade !== "F" })),
  });
  let pol = await admin.call("POST", `${X}/institutions/${setup.id}/exam-policies`, { code: "EXAM-POL", name: "Examination policy 2026", requireDuesCleared: false });
  pol = await admin.call("POST", `${X}/exam-policies/${pol.id}/submit`, {}, pol.version);
  if (pol.status !== "active") await admin.call("POST", `${X}/exam-policies/${pol.id}/activate`, {}, pol.version);
  const venues = [];
  for (const [code, name, capacity] of [["MH-A", "Main Hall A", 240], ["MH-B", "Main Hall B", 240], ["B-204", "B-204", 64], ["SEM", "Seminar Hall", 120]])
    venues.push(await admin.call("POST", `${X}/institutions/${setup.id}/exam-venues`, { code, name, capacity }));
  // An earlier session of the term taken through results publication and closure (student Results, faculty "Finalised" rows).
  let prior = null;
  try {
    prior = await examCycle(admin, setup, { term, scheme, venues, code: "UT1-2026", name: "Unit test examinations", start: 2, offerings: offerings.filter((o) => ["CS101", "MD101", "EL101"].includes(o.slot)), aarav: students[0].id });
  } catch (e) {
    report.push(`note  exams: earlier session: ${(e instanceof Error ? e.message : String(e)).slice(0, 300)}`);
  }
  // Session: six exam days from a week ahead, registration open now.
  let ses = await admin.call("POST", `${X}/institutions/${setup.id}/exam-sessions`, {
    code: "MSE-2026",
    name: "Mid-semester examinations",
    academicTermId: term,
    gradingSchemeId: scheme.id,
    startsOn: dayIso(7),
    endsOn: dayIso(12),
    registrationOpensOn: dayIso(-5),
    registrationClosesOn: dayIso(5),
  });
  for (const o of offerings) await admin.call("POST", `${X}/exam-sessions/${ses.id}/papers`, { offeringId: o.offeringId });
  ses = await admin.get(`${X}/exam-sessions/${ses.id}`).catch(async () => (await admin.get(`${X}/institutions/${setup.id}/exam-sessions`)).items.find((x) => x.id === ses.id));
  ses = await admin.call("POST", `${X}/exam-sessions/${ses.id}/submit`, {}, ses.version);
  if (ses.status === "approved") ses = await admin.call("POST", `${X}/exam-sessions/${ses.id}/schedule`, {}, ses.version);
  ses = await admin.call("POST", `${X}/exam-sessions/${ses.id}/activate`, {}, ses.version);
  check(ses.status === "active", `exam session active (${ses.status}, ${ses.papers.length} papers)`);
  // Venues + timetable: two sittings a day (10:00, 14:00), one hall each, rotating.
  let sch = await admin.call("POST", `${X}/exam-sessions/${ses.id}/schedules`, { name: "Mid-semester timetable" });
  const papers = [...ses.papers].sort((a, b) => a.paperCode.localeCompare(b.paperCode, "en", { numeric: true }));
  const order = [...papers].sort((a, b) => (a.paperCode.slice(2) + a.paperCode).localeCompare(b.paperCode.slice(2) + b.paperCode));
  const sittings = [];
  for (const [i, p] of order.entries()) sittings.push(await admin.call("POST", `${X}/exam-schedules/${sch.id}/sittings`, { paperId: p.id, sitsOn: dayIso(7 + Math.floor(i / 2)), startsAt: i % 2 ? "14:00" : "10:00" }));
  for (const [i, x] of sittings.entries()) await admin.call("POST", `${X}/exam-sittings/${x.id}/venues`, { venueId: venues[i % venues.length].id });
  sch = (await admin.get(`${X}/exam-sessions/${ses.id}/schedules`)).items.find((x) => x.id === sch.id);
  sch = await admin.call("POST", `${X}/exam-schedules/${sch.id}/submit`, {}, sch.version);
  if (sch.status === "approved") sch = await admin.call("POST", `${X}/exam-schedules/${sch.id}/publish`, {}, sch.version);
  check(sch.status === "published", `timetable ${sch.status} (${sittings.length} sittings)`);
  // Registrations from the rosters + hall tickets.
  const bulk = await admin.call("POST", `${X}/exam-sessions/${ses.id}/registrations/bulk`, {});
  await admin.call("POST", `${X}/exam-sessions/${ses.id}/hall-tickets/issue`, {});
  // Four papers conducted, marks entered and submitted → waiting for verification.
  const marked = [];
  for (const x of sittings.slice(0, 4)) {
    try {
      let c = await admin.call("POST", `${X}/exam-sittings/${x.id}/conduct`, {});
      c = await admin.call("POST", `${X}/conduct-records/${c.id}/start`, {}, c.version);
      await admin.call("POST", `${X}/conduct-records/${c.id}/complete`, { attendance: [], defaultOutcome: "present" }, c.version);
      await admin.call("POST", `${X}/exam-papers/${x.paperId}/mark-sheet`, {});
      let m = await admin.get(`${X}/exam-papers/${x.paperId}/marks`);
      const rows = m.items.filter((r) => r.effective).map((r, i) => ({ id: r.id, version: r.version, externalMarks: (Number(r.externalMax || "100") * (0.48 + ((i * 7) % 45) / 100)).toFixed(2) }));
      if (rows.length) await admin.call("PUT", `${X}/exam-papers/${x.paperId}/marks`, { rows });
      m = await admin.get(`${X}/exam-papers/${x.paperId}/marks`);
      if (m.sheet && m.sheet.status === "in_evaluation") await admin.call("POST", `${X}/mark-sheets/${m.sheet.id}/submit`, {}, m.sheet.version);
      marked.push(x.paperCode);
    } catch (e) {
      report.push(`note  exams: marks for ${x.paperCode}: ${(e instanceof Error ? e.message : String(e)).slice(0, 300)}`);
    }
  }
  return { prior, session: ses.code, papers: papers.length, sittings: sittings.length, registered: bulk.processed, marked: marked.join(",") };
}

/** A complete examination cycle on `offerings`: session → timetable → registrations → conduct → marks → results published → grade card → closure. */
async function examCycle(admin, setup, { term, scheme, venues, code, name, start, offerings, aarav }) {
  let ses = await admin.call("POST", `${X}/institutions/${setup.id}/exam-sessions`, { code, name, academicTermId: term, gradingSchemeId: scheme.id, startsOn: dayIso(start), endsOn: dayIso(start + offerings.length), registrationOpensOn: dayIso(-5), registrationClosesOn: dayIso(start) });
  for (const o of offerings) await admin.call("POST", `${X}/exam-sessions/${ses.id}/papers`, { offeringId: o.offeringId });
  ses = await admin.get(`${X}/exam-sessions/${ses.id}`);
  ses = await admin.call("POST", `${X}/exam-sessions/${ses.id}/submit`, {}, ses.version);
  if (ses.status === "approved") ses = await admin.call("POST", `${X}/exam-sessions/${ses.id}/schedule`, {}, ses.version);
  ses = await admin.call("POST", `${X}/exam-sessions/${ses.id}/activate`, {}, ses.version);
  const created = await admin.call("POST", `${X}/exam-sessions/${ses.id}/schedules`, { name: `${name} timetable` });
  const sittings = [];
  for (const [i, p] of ses.papers.entries()) sittings.push(await admin.call("POST", `${X}/exam-schedules/${created.id}/sittings`, { paperId: p.id, sitsOn: dayIso(start + i), startsAt: "10:00" }));
  for (const [i, x] of sittings.entries()) await admin.call("POST", `${X}/exam-sittings/${x.id}/venues`, { venueId: venues[i % venues.length].id });
  const draft = (await admin.get(`${X}/exam-sessions/${ses.id}/schedules`)).items.find((x) => x.id === created.id);
  const submitted = await admin.call("POST", `${X}/exam-schedules/${draft.id}/submit`, {}, draft.version);
  if (submitted.status === "approved") await admin.call("POST", `${X}/exam-schedules/${draft.id}/publish`, {}, submitted.version);
  await admin.call("POST", `${X}/exam-sessions/${ses.id}/registrations/bulk`, {});
  await admin.call("POST", `${X}/exam-sessions/${ses.id}/hall-tickets/issue`, {});
  for (const x of sittings) {
    const c0 = await admin.call("POST", `${X}/exam-sittings/${x.id}/conduct`, {});
    const c1 = await admin.call("POST", `${X}/conduct-records/${c0.id}/start`, {}, c0.version);
    await admin.call("POST", `${X}/conduct-records/${c1.id}/complete`, { attendance: [], defaultOutcome: "present" }, c1.version);
    await admin.call("POST", `${X}/exam-papers/${x.paperId}/mark-sheet`, {});
    const m = await admin.get(`${X}/exam-papers/${x.paperId}/marks`);
    const rows = m.items.filter((r) => r.effective).map((r, i) => ({ id: r.id, version: r.version, externalMarks: (Number(r.externalMax || "100") * (0.62 + ((i * 11) % 33) / 100)).toFixed(2) }));
    if (rows.length) await admin.call("PUT", `${X}/exam-papers/${x.paperId}/marks`, { rows });
    let sheet = (await admin.get(`${X}/exam-papers/${x.paperId}/marks`)).sheet;
    for (const cmd of ["submit", "verify", "request-approval"]) sheet = await admin.call("POST", `${X}/mark-sheets/${sheet.id}/${cmd}`, {}, sheet.version);
    if (sheet.awaitingDecision) sheet = await admin.call("POST", `${X}/mark-sheets/${sheet.id}/decision`, { decision: "approve" }, sheet.version);
    await admin.call("POST", `${X}/mark-sheets/${sheet.id}/finalise`, {}, sheet.version);
  }
  let batch = await admin.call("POST", `${X}/exam-sessions/${ses.id}/result-batches`, {});
  batch = await admin.call("POST", `${X}/result-batches/${batch.id}/compute`, {}, batch.version);
  batch = await admin.call("POST", `${X}/result-batches/${batch.id}/validate`, {}, batch.version);
  if (batch.status === "validated" && batch.processInstanceId) batch = await admin.call("POST", `${X}/result-batches/${batch.id}/decision`, { decision: "approve" }, batch.version);
  await admin.call("POST", `${X}/result-batches/${batch.id}/finalise`, {}, batch.version);
  let pub = await admin.call("POST", `${X}/exam-sessions/${ses.id}/publications`, {});
  pub = await admin.call("POST", `${X}/publications/${pub.id}/authorise`, {}, pub.version);
  if (pub.processInstanceId && pub.status !== "authorised") pub = await admin.call("POST", `${X}/publications/${pub.id}/decision`, { decision: "approve" }, pub.version);
  await admin.call("POST", `${X}/publications/${pub.id}/publish`, {}, pub.version);
  const results = (await admin.get(`${X}/exam-sessions/${ses.id}/results`)).items;
  const mine = results.find((r) => r.studentId === aarav && r.status === "published");
  if (mine) await admin.call("POST", `${X}/term-results/${mine.id}/grade-card`, {});
  const closing = await admin.call("POST", `${X}/exam-sessions/${ses.id}/closure/initiate`, {});
  const closure = await admin.call("POST", `${X}/exam-sessions/${ses.id}/closure/close`, {}, closing.version);
  return { code, published: results.filter((r) => r.status === "published").length, closure: closure.status };
}

/**
 * Shell (R1): notices for every role, personal to-dos per persona (one already done, so the
 * Home ring has progress), and role notifications for the bell (SQL: there is no create endpoint).
 */
export async function seedShell(admin) {
  const day = 86_400_000;
  const at = (days, hh, mm = 0) => {
    const d = new Date(Date.now() + days * day);
    // hh:mm India time (UTC+05:30) on that day
    return new Date(Date.UTC(d.getUTCFullYear(), d.getUTCMonth(), d.getUTCDate(), hh - 5, mm - 30)).toISOString();
  };
  const ALL = ["administrator", "faculty", "student"];
  const notices = [
    ["Innovation Week begins 23 September", "Three days of project showcases, hackathons and industry talks. Classes run on a reduced timetable; registrations close 20 September.", ALL, true],
    ["Library open until 8:00 PM on weekdays", "Extended hours run through the examination season. Saturday hours stay 9:00 AM to 2:00 PM.", ALL, false],
    ["Mid-semester examinations from 21 September", "Hall tickets are on the student portal. Carry your college ID to every paper.", ["administrator", "faculty", "student"], false],
    ["Campus Wi-Fi maintenance on Saturday", "Wi-Fi and the ERP may be briefly unavailable during the window. Wired lab machines are unaffected.", ALL, false],
  ];
  // Oldest first, so the newest-first list reads like the prototype (Innovation Week on top).
  for (const [title, body, audienceRoles, pinned] of [...notices].reverse()) await admin.call("POST", "/api/v1/notices", { title, body, audienceRoles, pinned });
  const TODOS = {
    "admin@sutra.dev": [
      ["Admission documents to review", "Students · Registrar’s office", at(0, 16), "/students"],
      ["Fee adjustments for review", "Finance · Accounts office", at(0, 18), "/finance"],
      ["Result verification", "Examinations · Exam cell", at(2, 17), "/examinations"],
      ["Staff leave requests", "People · HR operations", at(3, 17), "/people"],
      ["Approve the timetable draft", "Academics · Dean’s office", at(-1, 12), "/academics"],
    ],
    "meera.shah@sutra.dev": [
      ["Take attendance · Data Structures", "CS101 · Section A", at(0, 10, 30), "/attendance"],
      ["Review lab submissions", "Linked lists lab", at(0, 18), "/assessments"],
      ["Student check-ins", "Academic mentoring", at(1, 15), "/my-students"],
      ["Assessment material review", "Mid-semester paper", at(2, 17), "/assessments"],
      ["Upload week 6 slides", "Data Structures", at(-1, 17), "/resources"],
    ],
    "aarav.desai@sutra.dev": [
      ["Linked lists: practice assignment", "Data Structures · CS101", at(0, 17), "/assignments"],
      ["Read: relational database design", "Database Systems", at(1, 17), "/courses"],
      ["Mathematics II assessment", "Unit assessment", at(5, 10), "/results"],
      ["Collect the library card", "Central library", at(-1, 13), "/library"],
    ],
  };
  const CONTACT = {
    "admin@sutra.dev": ["dhruv.naik@sit.edu.in", "+919820144127"],
    "meera.shah@sutra.dev": ["meera.shah@sit.edu.in", "+919820155210"],
    "aarav.desai@sutra.dev": ["aarav.desai@sit.edu.in", "+919820166330"],
  };
  let todos = 0;
  for (const [login, items] of Object.entries(TODOS)) {
    const who = login === "admin@sutra.dev" ? admin : await apiLogin(login);
    const [contactEmail, contactPhone] = CONTACT[login];
    try {
      const prof = await who.get("/api/v1/profile");
      await who.call("PATCH", "/api/v1/profile", { contactEmail, contactPhone }, prof.version);
    } catch (e) {
      log(`  ! profile contact for ${login}: ${e instanceof Error ? e.message : e}`);
    }
    const created = [];
    for (const [title, detail, dueAt, link] of items) created.push(await who.call("POST", "/api/v1/todos", { title, detail, dueAt, link }));
    // The last one is already done (progress on the Home ring, "Done" column on /todos).
    const last = created[created.length - 1];
    if (last) await who.call("POST", `/api/v1/todos/${last.id}/complete`, {});
    todos += created.length;
  }
  const NOTES = {
    administrator: [
      ["institution", "Admission files uploaded", "Registrar’s office", "/students"],
      ["system", "Fee batch reconciled", "Accounts office", "/finance"],
      ["todo", "Semester results ready to verify", "Exam cell", "/examinations"],
      ["todo", "Staff leave requests pending", "HR operations", "/people"],
    ],
    faculty: [
      ["todo", "Lab submissions to review", "Data Structures", "/assessments"],
      ["notice", "Mid-semester timetable published", "Exam cell", "/notices"],
      ["institution", "Room change for CS101 on Friday", "Academic office", "/timetable"],
    ],
    student: [
      ["todo", "New assignment: Linked lists practice", "Data Structures", "/assignments"],
      ["notice", "Hall tickets available", "Exam cell", "/notices"],
      ["system", "Fee receipt ready", "Accounts office", "/fees"],
    ],
  };
  let n = 0;
  for (const [role, rows] of Object.entries(NOTES))
    rows.forEach(([category, title, body, link], i) => {
      psql(
        `INSERT INTO platform.notification (tenant_id, recipient_role, category, title, body, link, created_at, visible_at) ` +
          `VALUES ('${TENANT}', '${role}', '${category}', ${q(title)}, ${q(body)}, ${q(link)}, now() - interval '${(i + 1) * 47} minutes', now() - interval '${(i + 1) * 47} minutes')`,
      );
      n++;
    });
  return { notices: notices.length, todos, notifications: n };
}

/**
 * Students (R2): admission applications in every early state (draft → offer), a registered
 * guardian linked to Aarav, a published merit scholarship and a certificate request, all via the API.
 */
export async function seedStudents(admin, setup, students) {
  const S = "/api/v1/student";
  const apps = [];
  for (const [given, family, dob, steps] of [
    ["Ishaan", "Verma", "2007-04-11", []],
    ["Meghna", "Pillai", "2007-01-23", ["submit"]],
    ["Omkar", "Agarwal", "2006-11-05", ["submit", "review"]],
    ["Tanvi", "Shetty", "2007-06-30", ["submit", "review", "offer"]],
  ]) {
    let a = await admin.call("POST", `${S}/institutions/${setup.id}/admission-applications`, {
      programmeId: setup.programmeId,
      givenName: given,
      familyName: family,
      dateOfBirth: dob,
      email: `${given.toLowerCase()}.${family.toLowerCase()}@example.in`,
      phone: "+91 98200 4" + String(apps.length).padStart(4, "0"),
    });
    for (const st of steps)
      a = await admin.call("POST", `${S}/admission-applications/${a.id}/${st}`, st === "offer" ? { offerExpiresOn: "2026-12-31", requiredDocuments: ["identity_proof", "qualifying_marksheet"] } : {}, a.version);
    apps.push(a.id);
  }
  const g = await admin.call("POST", `${S}/institutions/${setup.id}/guardians`, { givenName: "Sunita", familyName: "Desai", phone: "+91 98220 44556", email: "sunita.desai@example.in" });
  await admin.call("POST", `${S}/students/${students[0].id}/guardian-relationships`, { guardianId: g.id, relationshipType: "mother", isPrimary: true });
  const sch = await admin.call("POST", `${S}/institutions/${setup.id}/scholarships`, { code: "MERIT-2026", name: "Merit scholarship 2026", sponsorType: "institutional", benefitType: "fee_percentage", benefitValue: 50, validFrom: "2026-06-01", renewable: true });
  await admin.call("POST", `${S}/scholarships/${sch.id}/publish`, {}, sch.version);
  await admin.call("POST", `${S}/students/${students[1].id}/document-requests`, { documentType: "bonafide_certificate", purpose: "Bank account opening" });
  await admin.call("POST", `${S}/students/${students[0].id}/document-requests`, { documentType: "transcript_copy", purpose: "Passport application" });
  // An open registration window (student /registration shows the component checklist); non-fatal.
  try {
    const w = await admin.call("POST", `${S}/institutions/${setup.id}/registration-windows`, { academicTermId: setup.termIds[0], name: "Term 1 registration", closesOn: "2026-12-15", minCredits: 16, maxCredits: 28 });
    await admin.call("POST", `${S}/registration-windows/${w.id}/open`, {}, w.version);
  } catch (e) {
    report.push(`WARN  students: registration window: ${(e instanceof Error ? e.message : String(e)).slice(0, 200)}`);
  }
  // Part payments on the first two fee accounts (prototype "Partial" + a Payments row); non-fatal.
  for (const [i, amount, ref] of [[0, "111000.00", "UTR 4402 1189"], [1, "50000.00", "UTR 4402 1190"]]) {
    try {
      const acc = psql(`SELECT id FROM finance.student_account WHERE student_id = '${students[i].id}'`);
      if (acc) await admin.call("POST", `/api/v1/finance/student-accounts/${acc}/payments`, { amount, channel: "bank_transfer", externalRef: ref, receivedOn: "2026-07-10" });
    } catch (e) {
      report.push(`WARN  students: payment for ${students[i].name}: ${(e instanceof Error ? e.message : String(e)).slice(0, 200)}`);
    }
  }
  return { applications: apps.length, guardian: g.id, scholarship: sch.id };
}

/**
 * R3 (Academics + teaching): two more term-1 offerings (MD101 taught by Dr. Meera Shah, EL101 by
 * Prof. Anil Kulkarni) with students seated, an operational term-1 timetable (requirements →
 * generate → validate → submit → publish → activate), finalised CS101 sessions with marks plus
 * today's started session, course resources (published + a draft) and coursework windows the
 * student sees as pending (one submitted). API where an endpoint exists, SQL like seedAcademic.
 */
export async function seedAcademics(admin, setup, students, academic) {
  const A = "/api/v1/academic";
  const out = { offerings: 1, timetable: null, sessions: 0, resources: 0, windows: 0 };
  const login = async (id) => {
    try {
      const { apiLogin } = await import("./harness.mjs");
      return await apiLogin(id);
    } catch {
      return admin;
    }
  };
  const meera = await login("meera.shah@sutra.dev");
  const aarav = await login("aarav.desai@sutra.dev");
  const offer = (slot, code, faculty, subject, seated) => {
    const [componentId, title, credits] = psql(
      `SELECT id || '|' || title || '|' || credits FROM institution.curriculum_component WHERE curriculum_version_id = '${setup.curriculumVersionId}' AND slot_code = '${slot}'`,
    ).split("|");
    const offeringId = psql(
      `INSERT INTO academic.course_offering (tenant_id, institution_id, academic_year_id, academic_term_id, curriculum_version_id, curriculum_component_id, ` +
        `offering_code, title, course_code, category, credits, capacity, status) VALUES ('${TENANT}', '${setup.id}', '${setup.yearId}', '${setup.termIds[0]}', ` +
        `'${setup.curriculumVersionId}', '${componentId}', '${code}', ${q(title)}, '${slot}', 'major_core', ${credits}, 60, 'active') RETURNING id`,
    );
    const sectionId = psql(`INSERT INTO academic.offering_section (tenant_id, institution_id, offering_id, section_code, capacity, status) VALUES ('${TENANT}', '${setup.id}', '${offeringId}', 'A', 60, 'active') RETURNING id`);
    psql(
      `INSERT INTO academic.faculty_assignment (tenant_id, institution_id, offering_id, section_id, faculty_subject_ref, faculty_name, role, weekly_hours, effective_from, status) ` +
        `VALUES ('${TENANT}', '${setup.id}', '${offeringId}', '${sectionId}', ${q(subject)}, ${q(faculty)}, 'primary_instructor', 3, DATE '2026-06-15', 'active') RETURNING id`,
    );
    for (const st of seated)
      psql(
        `INSERT INTO academic.section_enrolment (tenant_id, institution_id, offering_id, section_id, student_id, student_context_id, student_number, display_name, subject_ref, origin, status) ` +
          `SELECT s.tenant_id, '${setup.id}', '${offeringId}', '${sectionId}', s.id, c.id, s.student_number, s.given_name || ' ' || coalesce(s.family_name, ''), s.subject_ref, 'override', 'enrolled' ` +
          `FROM student.student s JOIN student.student_context c ON c.student_id = s.id WHERE s.id = '${st.id}' RETURNING id`,
      );
    out.offerings++;
    return { offeringId, sectionId };
  };
  const md = offer("MD101", "MD101-T1", "Dr. Meera Shah", FACULTY_SUBJECT, students.slice(0, 10));
  const el = offer("EL101", "EL101-T1", "Prof. Anil Kulkarni", "dev|anil.kulkarni@sutra.dev", students.filter((_, i) => i === 0 || i >= 8));

  // timetable: requirements → operational version
  for (const [sectionId, n, minutes, room, days] of [
    [academic.sectionId, 3, 60, "B-204", [1, 3, 5]],
    [md.sectionId, 2, 90, "B-102", [2, 4]],
    [el.sectionId, 2, 60, "A-103", [2, 5]],
  ])
    await admin.call("POST", `${A}/institutions/${setup.id}/scheduling-requirements`, { sectionId, sessionsPerWeek: n, sessionMinutes: minutes, roomLabel: room, preferredDays: days });
  let tt = await admin.call("POST", `${A}/institutions/${setup.id}/timetables`, { academicTermId: setup.termIds[0], name: "Term 1 timetable" });
  for (const cmd of ["generate", "validate", "submit", "publish", "activate"]) tt = await admin.call("POST", `${A}/timetables/${tt.id}/${cmd}`, {}, tt.version);
  out.timetable = `${tt.status} (${tt.slots?.length ?? 0} slots)`;

  // CS101 sessions: three finalised with marks, today's started
  const day = (n) => new Date(Date.now() + 5.5 * 3600e3 + n * 86400e3).toISOString().slice(0, 10);
  const seatedIds = students.map((s) => s.id);
  const pattern = [
    (i) => (i === 11 ? "absent" : i === 4 ? "late" : "present"),
    (i) => (i === 11 || i === 13 ? "absent" : "present"),
    (i) => (i === 7 ? "late" : i === 11 ? "absent" : "present"),
  ];
  const topics = ["Arrays and complexity", "Linked lists", "Stacks and queues"];
  for (const [k, offset] of [-9, -6, -2].entries()) {
    let s = await meera.call("POST", `${A}/sections/${academic.sectionId}/sessions`, { sessionDate: day(offset), startsAt: "10:30", endsAt: "11:30", topic: topics[k] });
    s = await meera.call("POST", `${A}/sessions/${s.id}/start`, {}, s.version);
    s = await meera.call("POST", `${A}/sessions/${s.id}/attendance`, { records: seatedIds.map((studentId, i) => ({ studentId, mark: pattern[k](i) })) }, s.version);
    s = await meera.call("POST", `${A}/sessions/${s.id}/submit-attendance`, {}, s.version);
    s = await meera.call("POST", `${A}/sessions/${s.id}/complete`, {}, s.version);
    await meera.call("POST", `${A}/sessions/${s.id}/finalise-attendance`, {}, s.version);
    out.sessions++;
  }
  const today = await meera.call("POST", `${A}/sections/${academic.sectionId}/sessions`, { sessionDate: day(0), startsAt: "10:30", endsAt: "11:30", topic: "Trees and heaps" });
  await meera.call("POST", `${A}/sessions/${today.id}/start`, {}, today.version);
  out.sessions++;

  // resources (faculty, per section)
  for (const [sectionId, title, kind, url, publish] of [
    [academic.sectionId, "Unit 2: Linked lists", "document", "https://library.sit.edu.in/cs101/unit2-slides.pptx", true],
    [academic.sectionId, "Lab 4 handout: linked lists", "document", "https://library.sit.edu.in/cs101/lab4.pdf", true],
    [academic.sectionId, "VisuAlgo: linked list animations", "link", "https://visualgo.net/en/list", true],
    [academic.sectionId, "Unit 3: Stacks and queues", "document", "https://library.sit.edu.in/cs101/unit3-slides.pptx", false],
    [md.sectionId, "Design thinking playbook", "reading", "https://library.sit.edu.in/md101/playbook.pdf", true],
    [md.sectionId, "Empathy mapping worksheet", "document", "https://library.sit.edu.in/md101/empathy.pdf", true],
    [md.sectionId, "IDEO design kit", "link", "https://www.designkit.org/methods", true],
  ]) {
    await meera.call("POST", `${A}/sections/${sectionId}/resources`, { title, kind, url, publish });
    out.resources++;
  }

  // syllabus coverage (class-card progress): CS101 2 of 5 units, MD101 1 of 4
  for (const [sectionId, topics] of [
    [academic.sectionId, [["Unit 1: Arrays and complexity", "completed"], ["Unit 2: Linked lists", "completed"], ["Unit 3: Stacks and queues", "started"], ["Unit 4: Trees and heaps", "planned"], ["Unit 5: Graphs and hashing", "planned"]]],
    [md.sectionId, [["Unit 1: Empathise", "completed"], ["Unit 2: Define", "started"], ["Unit 3: Ideate", "planned"], ["Unit 4: Prototype and test", "planned"]]],
  ])
    await meera.call("POST", `${A}/sections/${sectionId}/syllabus-coverage`, { summary: "Term 1 coverage", topics: topics.map(([title, status]) => ({ title, status })) });

  // coursework: CS101 blueprint + three open windows; Aarav submits one
  let bp = await admin.call("POST", `${A}/sections/${academic.sectionId}/assessment-blueprints`, {
    title: "CS101 internal assessment",
    totalMarks: 50,
    components: [
      { key: "a1", title: "Assignments", kind: "assignment", weight: 40 },
      { key: "t1", title: "Tests", kind: "test", weight: 60 },
    ],
  });
  bp = await admin.call("POST", `${A}/assessment-blueprints/${bp.id}/publish`, {}, bp.version);
  const due = (n, h) => new Date(Date.now() + n * 86400e3 + h * 3600e3).toISOString();
  const windows = [];
  for (const [title, d] of [
    ["Linked lists: practice assignment", 0.6],
    ["Stack applications problem set", 4],
    ["Recursion essay", 7],
  ]) {
    let w = await admin.call("POST", `${A}/sections/${academic.sectionId}/assessment-windows`, { blueprintId: bp.id, componentKey: "a1", title, opensAt: due(-3, 0), dueAt: due(d, 0), maxMarks: 10 });
    w = await admin.call("POST", `${A}/assessment-windows/${w.id}/open`, {}, w.version);
    windows.push(w);
    out.windows++;
  }
  if (aarav !== admin) await aarav.call("POST", `${A}/me/assignments/${windows[2].id}/submission`, { content: "https://github.com/aarav-desai/recursion-essay" });

  // more programmes for the Academics overview cards (each on its own: a refused level skips it)
  out.programmes = 1;
  for (const [code, name, durationTerms] of [
    ["BTECH-ECE", "B.Tech Electronics and Communication", 8],
    ["BTECH-ME", "B.Tech Mechanical Engineering", 8],
    ["BTECH-CE", "B.Tech Civil Engineering", 8],
    ["BBA", "Bachelor of Business Administration", 6],
    ["BCA", "Bachelor of Computer Applications", 6],
  ])
    try {
      let p = await admin.call("POST", `${I}/institutions/${setup.id}/programmes`, { code, name, level: "ug", durationTerms });
      p = await admin.call("POST", `${I}/programmes/${p.id}/publish`, {}, p.version);
      out.programmes++;
    } catch (e) {
      out.programmeError = String(e).slice(0, 160);
    }
  report.push(`info  academics (R3): ${JSON.stringify(out)}`);
  return out;
}

/**
 * Campus + Reports (R7): buildings and rooms with bookings today, maintenance tickets (some assigned so
 * "Mark resolved" shows), hostel blocks with beds, two active bus routes with stops, a library with
 * an active policy, titles, copies and loans (Aarav holds one; another student holds the only copy of
 * a second so it shows "Issued" + Reserve), governance cases and an accreditation framework. Each part
 * is independent: a failing part is noted in the seed report and the rest still seeds.
 */
export async function seedCampus(admin, setup, students) {
  const C = "/api/v1/campus";
  const inst = setup.id;
  const notes = [];
  const part = async (name, fn) => {
    try {
      await fn();
    } catch (e) {
      notes.push(`${name}: ${(e instanceof Error ? e.message : String(e)).slice(0, 200)}`);
    }
  };
  const ist = new Date(Date.now() + 330 * 60000);
  const today = ist.toISOString().slice(0, 10);
  const plus = (d) => new Date(ist.getTime() + d * 864e5).toISOString().slice(0, 10);
  const year = ist.getUTCFullYear();
  const campusId = psql(`SELECT id FROM institution.campus WHERE institution_id = '${inst}' ORDER BY created_at LIMIT 1`);
  const node = async (body) => (await admin.call("POST", `${C}/institutions/${inst}/infrastructure-nodes`, { status: "active", ...body })).node;
  const rooms = {};
  let library = null;
  const hostelRooms = [];
  await part("infrastructure", async () => {
    const site = await node({ nodeType: "site", campusId, code: "MAIN", name: "Main campus" });
    const blocks = [
      ["MAIN-A", "A block", "classroom", ["A-101", "A-102", "A-103", "A-104", "A-105", "A-106"], 60],
      ["MAIN-B", "B block", "classroom", ["B-201", "B-202", "B-203", "B-204"], 60],
      ["MAIN-L", "Lab block", "lab", ["Lab L1", "Lab L2", "Lab L3"], 30],
    ];
    for (const [code, name, spaceType, spaces, capacity] of blocks) {
      const b = await node({ nodeType: "building", parentId: site.id, code, name });
      const f = await node({ nodeType: "floor", parentId: b.id, code: `${code}-F1`, name: `${name}, first floor` });
      for (const sp of spaces) {
        const spCode = `${code}-${sp.replace(/[^A-Za-z0-9]/g, "").toUpperCase()}`;
        rooms[sp] = await node({ nodeType: "space", parentId: f.id, code: spCode, name: spaceType === "lab" ? sp : `Room ${sp}`, spaceType, capacity });
      }
      if (code === "MAIN-A") {
        rooms["Seminar hall"] = await node({ nodeType: "space", parentId: f.id, code: "MAIN-A-SEM", name: "Seminar hall", spaceType: "seminar_room", capacity: 120 });
        library = await node({ nodeType: "space", parentId: f.id, code: "MAIN-A-LIB", name: "Central library", spaceType: "library", capacity: 150 });
      }
    }
    for (const [code, name] of [
      ["MAIN-H1", "Boys' hostel"],
      ["MAIN-H2", "Girls' hostel"],
    ]) {
      const b = await node({ nodeType: "building", parentId: site.id, code, name });
      const f = await node({ nodeType: "floor", parentId: b.id, code: `${code}-G`, name: `${name}, ground floor` });
      for (const r of ["G01", "G02", "G03"]) hostelRooms.push(await node({ nodeType: "space", parentId: f.id, code: `${code}-${r}`, name: `${name} room ${r}`, spaceType: "hostel_room", capacity: 2 }));
    }
  });
  await part("bookings", async () => {
    const slots = [
      ["A-101", "18:00", "19:00", 48, "Data structures — remedial class"],
      ["A-103", "18:30", "20:00", 52, "Placement aptitude session"],
      ["B-202", "19:00", "20:30", 40, "Guest lecture: cloud careers"],
      ["Lab L1", "18:00", "21:00", 28, "Project lab — final year"],
      ["Seminar hall", "20:00", "21:30", 110, "Student council meeting"],
    ];
    for (const [room, startTime, endTime, attendees, purpose] of slots) if (rooms[room]) await admin.call("POST", `${C}/institutions/${inst}/reservations`, { nodeId: rooms[room].id, date: today, startTime, endTime, attendees, purpose });
  });
  await part("tickets", async () => {
    const list = [
      ["Projector not working", "it", "high", "B-202", "Ravi Kumar (AV)", true],
      ["Water leak near stairwell", "maintenance", "high", "A-101", "Plumbing team", false],
      ["AC not cooling", "facility", "medium", "Seminar hall", null, false],
      ["Broken bench, row 4", "maintenance", "low", "A-104", null, false],
      ["Lab PC-17 does not boot", "laboratory", "medium", "Lab L2", "IT desk", true],
    ];
    for (const [title, category, priority, room, assignee, start] of list) {
      let t = await admin.call("POST", `${C}/institutions/${inst}/tickets`, { category, priority, title, nodeId: rooms[room]?.id ?? null });
      if (assignee) {
        t = await admin.call("POST", `${C}/tickets/${t.id}/assign`, { assignee }, t.version);
        if (start) await admin.call("POST", `${C}/tickets/${t.id}/start`, {}, t.version);
      }
    }
  });
  await part("hostel", async () => {
    for (const r of hostelRooms) for (const bed of ["A", "B"]) await admin.call("POST", `${C}/hostel-rooms/${r.id}/beds`, { bedCode: bed });
    let pol = await admin.call("POST", `${C}/institutions/${inst}/hostel-policies`, { name: `Hostel policy ${year}-${String(year + 1).slice(2)}`, feePerTerm: "45000.00", deposit: "5000.00", curfewTime: "21:30", effectiveFrom: `${year}-01-01` });
    pol = await admin.call("POST", `${C}/hostel-policies/${pol.id}/submit`, {}, pol.version);
    if (pol.status !== "approved" && pol.status !== "active") pol = await admin.call("POST", `${C}/hostel-policies/${pol.id}/decision`, { decision: "approve" }, pol.version);
    if (pol.status !== "active") await admin.call("POST", `${C}/hostel-policies/${pol.id}/activate`, {}, pol.version);
    // Seven residents (the office applies on their behalf; a bed is allocated automatically).
    for (const st of students.slice(2, 9)) {
      let app = await admin.call("POST", `${C}/institutions/${inst}/hostel-applications`, { personKind: "student", personId: st.id, preference: "Near the library" });
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
    // Riders: ten students allocated to a stop (the office allocates on their behalf).
    for (const [n, st] of students.slice(0, 10).entries()) {
      const { route, stops: on } = live[n % 2];
      await admin.call("POST", `${C}/institutions/${inst}/transport-allocations`, { personKind: "student", personId: st.id, routeId: route.id, stopId: on[n % on.length].id });
    }
  });
  await part("library", async () => {
    let pol = await admin.call("POST", `${C}/institutions/${inst}/library-policies`, { name: `Library rules ${year}`, effectiveFrom: `${year}-01-01`, loanDaysStudent: 14, loanDaysStaff: 30, maxLoansStudent: 3, maxLoansStaff: 6, maxRenewals: 1, finePerDay: "2.00", reservationHoldDays: 3, lostChargeFactor: "1.5" });
    pol = await admin.call("POST", `${C}/library-policies/${pol.id}/submit`, {}, pol.version);
    if (pol.approvalState === "pending") pol = await admin.call("POST", `${C}/library-policies/${pol.id}/decision`, { decision: "approve" }, pol.version);
    if (pol.status !== "active") await admin.call("POST", `${C}/library-policies/${pol.id}/activate`, {}, pol.version);
    const aarav = await admin.call("POST", `${C}/institutions/${inst}/library-memberships`, { personKind: "student", personId: students[0].id });
    const other = await admin.call("POST", `${C}/institutions/${inst}/library-memberships`, { personKind: "student", personId: students[1].id });
    const titles = [
      ["Introduction to Algorithms", ["Cormen", "Leiserson", "Rivest", "Stein"], "CSE-005"],
      ["Database System Concepts", ["Silberschatz", "Korth", "Sudarshan"], "CSE-112"],
      ["Computer Networking: A Top-Down Approach", ["Kurose", "Ross"], "CSE-210"],
      ["Operating System Concepts", ["Silberschatz", "Galvin", "Gagne"], "CSE-118"],
      ["Higher Engineering Mathematics", ["B. S. Grewal"], "MAT-510"],
      ["The C Programming Language", ["Kernighan", "Ritchie"], "CSE-001"],
      ["Clean Code", ["Robert C. Martin"], "CSE-330"],
    ];
    const copies = {};
    for (const [title, authors, shelfMark] of titles) {
      const rec = await admin.call("POST", `${C}/institutions/${inst}/catalogue`, { title, authors, replacementCost: "650.00" });
      await admin.call("POST", `${C}/catalogue-records/${rec.id}/publish`, {}, rec.version);
      copies[title] = await admin.call("POST", `${C}/catalogue-records/${rec.id}/copies`, { nodeId: library?.id, shelfMark });
    }
    for (const t of ["Database System Concepts", "Higher Engineering Mathematics"]) await admin.call("POST", `${C}/institutions/${inst}/library-loans`, { membershipId: aarav.id, copyId: copies[t].id });
    for (const t of ["Operating System Concepts", "Clean Code"]) await admin.call("POST", `${C}/institutions/${inst}/library-loans`, { membershipId: other.id, copyId: copies[t].id });
  });
  await part("governance", async () => {
    for (const body of [
      { kind: "readiness_assessment", title: "Campus readiness for the academic year", dueOn: plus(30), owner: "Registrar" },
      { kind: "risk", title: "Hostel fire-safety audit overdue", severity: "high", likelihood: 3, impact: 4, dueOn: plus(14), owner: "Estate office" },
      { kind: "operational_review", title: "Service desk review — Q3", dueOn: plus(21), owner: "Facilities head" },
    ])
      await admin.call("POST", `${C}/institutions/${inst}/governance-cases`, body);
  });
  await part("accreditation", async () => {
    await admin.call("POST", `/api/v1/services/institutions/${inst}/accreditation-frameworks`, {
      code: "NAAC",
      name: "NAAC Revised Accreditation Framework",
      body: "NAAC",
      criteria: [
        { code: "C1", title: "Curricular aspects", weight: 40, metricKeys: [] },
        { code: "C2", title: "Teaching-learning and evaluation", weight: 35, metricKeys: [] },
        { code: "C3", title: "Infrastructure and learning resources", weight: 25, metricKeys: [] },
      ],
    });
  });
  if (notes.length) report.push(`note  campus parts: ${notes.join(" | ")}`);
  return { rooms: Object.keys(rooms).length, hostelRooms: hostelRooms.length, failedParts: notes.length };
}

/**
 * Seeds everything. Returns `{ setup, students, ... }`; `report` lists each domain's outcome.
 * Slices: add `seed<Area>()` above and one `domain()` line here.
 */
export async function seedReskin2(admin) {
  const { setup, students } = await seedCore(admin);
  report.push("ok    core");
  const academic = await domain("academic", () => seedAcademic(setup, students));
  const finance = await domain("finance", () => seedFinance(admin, setup, students.slice(0, 10)));
  const people = await domain("people", () => seedPeople(admin, setup));
  await domain("shell", () => seedShell(admin));
  const studentsArea = await domain("students", () => seedStudents(admin, setup, students));
  if (academic) await domain("academics (R3)", () => seedAcademics(admin, setup, students, academic));
  const exams = await domain("exams", () => seedExams(admin, setup, students));
  if (exams) report.push(`note  exams: ${JSON.stringify(exams)}`);
  const campus = await domain("campus", () => seedCampus(admin, setup, students));
  return { setup, students, academic, finance, people, studentsArea, exams, campus, I };
}
