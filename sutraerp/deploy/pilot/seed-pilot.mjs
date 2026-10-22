#!/usr/bin/env node
/* global process, console, fetch, setTimeout */
/**
 * Pilot seed (slice 9E, contract batch9 §12.4): the pilot institution `PILOT` in the fixed pilot
 * tenant `0192f5a0-7c3e-7000-8000-000000009110`, through the public API wherever the API offers
 * a write path (the e2e / demo-seed harness, `apps/web/e2e/lib/harness.mjs`), and through the
 * Batch 3 foundation tables for the students (exactly like the demo seed — admissions are not part
 * of a pilot bootstrap).
 *
 *   SUTRA_DEV_TENANT_ID=0192f5a0-7c3e-7000-8000-000000009110 sutra-api serve   # API in dev auth
 *   E2E_API_URL=http://127.0.0.1:8080 DATABASE_URL=postgres://sutra:sutra@localhost:5432/sutra \
 *     node deploy/pilot/seed-pilot.mjs
 *
 * Contents: institution PILOT “Sutra Pilot College” taken to `active` (profile → org blueprint +
 * root unit → main campus → structure → programme BTECH-CSE + curriculum CSE-2026 → calendar
 * 2026-27 → submit); 50 active students PILOT-0001…0050 (PILOT-0001 linked to the dev student
 * login); 3 active staff with active engagements (principal, faculty linked to the dev faculty
 * login, librarian); fee heads TUITION + HOSTEL and the published fee structure PILOT-Y1; the
 * campus hierarchy site → academic block → floor → central library, hostel block → floor → room
 * with two beds; an active library policy and three catalogued titles with one copy each; an
 * active hostel policy.
 *
 * Idempotent: every step looks for its object first and only creates what is missing. The run
 * ends with `changes=<n>`; with `PILOT_EXPECT_NO_CHANGES=1` any change fails the run (CI's second
 * pass). Every finished check prints one `ok <check>` line (contract §10.4).
 *
 * Environment: E2E_API_URL (default http://127.0.0.1:8080), DATABASE_URL (schema owner, `psql`
 * on PATH), PILOT_TENANT_ID (default the fixed pilot tenant), PILOT_CODE (default PILOT),
 * PILOT_STRICT_TENANT (default `true`: fail when the API's dev tenant is not the pilot tenant;
 * the kind stage sets `false` and seeds the API's tenant), PILOT_EXPECT_NO_CHANGES.
 */
process.env.DATABASE_URL ??= "postgres://sutra:sutra@localhost:5432/sutra";
const { API, apiLogin, I, log, psql, seedInstitution } = await import("../../apps/web/e2e/lib/harness.mjs");

const PILOT_TENANT = process.env.PILOT_TENANT_ID || "0192f5a0-7c3e-7000-8000-000000009110";
const CODE = (process.env.PILOT_CODE || "PILOT").toUpperCase();
const NAME = "Sutra Pilot College";
const STRICT = (process.env.PILOT_STRICT_TENANT ?? "true") !== "false";
const EXPECT_NO_CHANGES = process.env.PILOT_EXPECT_NO_CHANGES === "1";
const STUDENT_SUBJECT = "dev|aarav.desai@sutra.dev";
const FACULTY_SUBJECT = "dev|meera.shah@sutra.dev";
const C = "/api/v1/campus";
const P = "/api/v1/people";
const F = "/api/v1/finance";
const q = (s) => `'${String(s).replace(/'/g, "''")}'`;
let changes = 0;
const created = (what) => {
  changes += 1;
  log(`  + ${what}`);
};
const ok = (check) => console.log(`ok ${check}`);
const fail = (msg) => {
  throw new Error(msg);
};
const itemsOf = (x) => (Array.isArray(x) ? x : (x?.items ?? []));

const GIVEN = ["Aarav", "Isha", "Kiran", "Rohan", "Ananya", "Vihaan", "Diya", "Arjun", "Saanvi", "Kabir", "Meera", "Reyansh", "Aditi", "Vivaan", "Tara", "Ishaan", "Riya", "Advait", "Kavya", "Dhruv", "Myra", "Aryan", "Nisha", "Sai", "Pooja"];
const FAMILY = ["Desai", "Kulkarni", "Joshi", "Patil", "Shah", "Rao", "Iyer", "Nair", "Mehta", "Kapoor"];
const STAFF = [
  { key: "principal", givenName: "Sunita", familyName: "Deshmukh", workEmail: "principal@pilot.sutra.dev", staffCategory: "administrative", designation: "Principal", grade: "PR-1" },
  { key: "faculty", givenName: "Meera", familyName: "Shah", workEmail: "meera.shah@pilot.sutra.dev", staffCategory: "teaching", designation: "Assistant Professor", grade: "AP-1", subjectRef: FACULTY_SUBJECT },
  { key: "librarian", givenName: "Prakash", familyName: "Kale", workEmail: "librarian@pilot.sutra.dev", staffCategory: "non_teaching", designation: "Librarian", grade: "LB-1" },
];
const TITLES = [
  { title: "Introduction to Algorithms", authors: ["Thomas H. Cormen", "Charles E. Leiserson"], isbn: "9780262046305", shelfMark: "CS-ALG-001" },
  { title: "Engineering Mathematics", authors: ["B. S. Grewal"], isbn: "9788174091956", shelfMark: "MA-ENG-001" },
  { title: "Fluid Mechanics", authors: ["Frank M. White"], isbn: "9789353166625", shelfMark: "ME-FLU-001" },
];

async function waitForApi(ms = 180000) {
  const until = Date.now() + ms;
  let last = "";
  while (Date.now() < until) {
    try {
      const r = await fetch(`${API}/health/live`);
      if (r.status < 500) return;
      last = `HTTP ${r.status}`;
    } catch (e) {
      last = e instanceof Error ? e.message : String(e);
    }
    await new Promise((r) => setTimeout(r, 2000));
  }
  fail(`API ${API} is not reachable (${last})`);
}

/** Ids of the existing, active pilot institution (same shape as `seedInstitution` returns). */
function lookupSetup(id) {
  const one = (sql, what) => psql(sql) || fail(`${CODE} is active but has no ${what} — it was not created by this seed`);
  const programmeId = one(`SELECT id FROM institution.programme WHERE institution_id = ${q(id)} AND code = 'BTECH-CSE'`, "programme BTECH-CSE");
  const curriculumVersionId = one(
    `SELECT v.id FROM institution.curriculum_version v JOIN institution.curriculum c ON c.id = v.curriculum_id ` + `WHERE c.institution_id = ${q(id)} AND c.code = 'CSE-2026' AND v.status = 'published'`,
    "published curriculum CSE-2026",
  );
  const yearId = one(`SELECT id FROM institution.academic_year WHERE institution_id = ${q(id)} AND label = '2026-27' AND status = 'open'`, "open year 2026-27");
  const terms = one(
    `SELECT string_agg(t.id::text, ',' ORDER BY t.sequence) FROM institution.academic_term t ` +
      `JOIN institution.calendar_version v ON v.id = t.calendar_version_id WHERE t.institution_id = ${q(id)} AND v.status = 'published'`,
    "published calendar terms",
  );
  return { id, programmeId, curriculumVersionId, yearId, termIds: terms.split(",") };
}

/** Active student + active context in the foundation tables (demo-seed `ensureStudent`). */
function ensureStudent(tenant, setup, s) {
  const found = psql(
    `SELECT s.id || '|' || coalesce(c.institution_id::text, '') FROM student.student s ` +
      `LEFT JOIN student.student_context c ON c.student_id = s.id AND c.status = 'active' ` +
      `WHERE s.tenant_id = ${q(tenant)} AND s.student_number = ${q(s.number)}`,
  );
  if (found) {
    const [sid, inst] = found.split("\n")[0].split("|");
    if (inst !== setup.id) fail(`student number ${s.number} already exists outside ${CODE}`);
    return sid;
  }
  const sid = psql(
    `INSERT INTO student.student (tenant_id, student_number, given_name, family_name, primary_email, status) ` +
      `VALUES (${q(tenant)}, ${q(s.number)}, ${q(s.given)}, ${q(s.family)}, ${q(`${s.number.toLowerCase()}@students.pilot.sutra.dev`)}, 'active') RETURNING id`,
  );
  psql(
    `INSERT INTO student.student_context (tenant_id, student_id, institution_id, programme_id, curriculum_version_id, ` +
      `admission_term_id, admission_year_id, current_term_no, enrolment_number, batch_code, section_code, enrolment_type, enrolled_on, status) ` +
      `VALUES (${q(tenant)}, ${q(sid)}, ${q(setup.id)}, ${q(setup.programmeId)}, ${q(setup.curriculumVersionId)}, ${q(setup.termIds[0])}, ` +
      `${q(setup.yearId)}, 1, ${q(`EN-${s.number}`)}, '2026', ${q(s.section)}, 'regular', DATE '2026-07-01', 'active') RETURNING id`,
  );
  created(`student ${s.number} ${s.given} ${s.family}`);
  return sid;
}

async function ensureStaff(admin, inst, campusId, s) {
  const list = itemsOf(await admin.get(`${P}/institutions/${inst}/staff?limit=200`));
  let staff = list.find((x) => (x.workEmail ?? "").toLowerCase() === s.workEmail && x.status !== "merged" && x.status !== "archived");
  if (!staff) {
    staff = await admin.call("POST", `${P}/institutions/${inst}/staff`, {
      givenName: s.givenName,
      familyName: s.familyName,
      workEmail: s.workEmail,
      staffCategory: s.staffCategory,
      // One live staff record per login subject in the tenant: when another seed (the demo seed's
      // Dr. Meera Shah) already holds this login, the pilot record is created unlinked.
      subjectRef: s.subjectRef && !psql(`SELECT id FROM people.staff_member WHERE subject_ref = '${s.subjectRef.replace(/'/g, "''")}' AND status NOT IN ('merged', 'archived') LIMIT 1`) ? s.subjectRef : null,
      initialStatus: "active",
    });
    created(`staff ${s.givenName} ${s.familyName} (${s.designation})`);
  }
  if (staff.status !== "active") fail(`staff ${s.workEmail} is ${staff.status}, not active`);
  const engagements = itemsOf(await admin.get(`${P}/staff/${staff.id}/engagements`));
  let eng = engagements.find((e) => e.status === "active");
  if (!eng) {
    eng = engagements.find((e) => !["closed", "cancelled", "separated"].includes(e.status));
    if (!eng) {
      eng = await admin.call("POST", `${P}/staff/${staff.id}/engagements`, { employmentType: "permanent", designation: s.designation, grade: s.grade, startDate: "2026-06-01", campusId });
      created(`engagement ${s.designation}`);
    }
    // Submit (auto-approval activates at once: the start date has come). Part 1 (DEF-X-001): with
    // the default human approval mode the activation request waits for a decision, which the
    // seeding administrator (people.approve) gives; then activate if still pending.
    if (eng.status === "pending" && !eng.pendingChange) eng = await admin.call("POST", `${P}/engagements/${eng.id}/submit`, {}, eng.version);
    if (eng.status === "pending" && eng.pendingChange?.status === "pending_approval")
      eng = await admin.call("POST", `${P}/engagements/${eng.id}/decision`, { decision: "approve" }, eng.version);
    if (eng.status === "pending") eng = await admin.call("POST", `${P}/engagements/${eng.id}/activate`, {}, eng.version);
    if (eng.status !== "active") fail(`engagement of ${s.workEmail} is ${eng.status}`);
    changes += 1;
  }
  return staff;
}

async function ensureNode(admin, inst, nodes, body) {
  const hit = nodes.find((n) => n.code === body.code);
  if (hit) return hit;
  const d = await admin.call("POST", `${C}/institutions/${inst}/infrastructure-nodes`, { status: "active", ...body });
  nodes.push(d.node);
  created(`campus node ${body.code} (${body.nodeType})`);
  return d.node;
}

/** Draft → submit (→ approve when the workflow waits) → activate; `kind` = library | hostel. */
async function ensurePolicy(admin, inst, kind, body) {
  const base = kind === "library" ? "library-policies" : "hostel-policies";
  const list = itemsOf(await admin.get(`${C}/institutions/${inst}/${base}`));
  if (list.some((p) => p.status === "active")) return;
  let p = list.find((x) => !["superseded", "retired", "rejected", "withdrawn"].includes(x.status)) ?? null;
  if (!p) {
    p = await admin.call("POST", `${C}/institutions/${inst}/${base}`, body);
    created(`${kind} policy “${body.name}”`);
  }
  if (p.approvalState !== "approved") {
    if (p.approvalState !== "pending") p = await admin.call("POST", `${C}/${base}/${p.id}/submit`, {}, p.version);
    if (p.approvalState === "pending") p = await admin.call("POST", `${C}/${base}/${p.id}/decision`, { decision: "approve" }, p.version);
  }
  if (p.status !== "active") p = await admin.call("POST", `${C}/${base}/${p.id}/activate`, {}, p.version);
  if (p.status !== "active") fail(`${kind} policy is ${p.status}`);
  changes += 1;
}

async function sessionTenant(admin) {
  const s = await admin.get("/api/v1/auth/session");
  return s?.principal?.scope?.tenantId ?? s?.principal?.scope?.tenant_id ?? null;
}

try {
  if (!/^[A-Z0-9][A-Z0-9-]{1,15}$/.test(CODE)) fail(`PILOT_CODE “${CODE}” must be 2-16 letters, digits or dashes`);
  log(`pilot seed: institution ${CODE} “${NAME}” in tenant ${PILOT_TENANT} via ${API}`);
  psql("SELECT 1 FROM institution.institution LIMIT 1");
  ok("database-reachable");
  await waitForApi();
  const admin = await apiLogin("admin@sutra.dev");
  const apiTenant = await sessionTenant(admin);
  if (apiTenant !== PILOT_TENANT) {
    if (STRICT) fail(`the API's dev tenant is ${apiTenant}, not the pilot tenant ${PILOT_TENANT} (start it with SUTRA_DEV_TENANT_ID=${PILOT_TENANT})`);
    log(`  ! API tenant ${apiTenant} is not the pilot tenant (PILOT_STRICT_TENANT=false): seeding the API's tenant`);
  }
  const tenant = apiTenant ?? PILOT_TENANT;
  ok(`pilot-tenant ${tenant}`);

  // ---- institution
  const existing = psql(`SELECT id || '|' || status FROM institution.institution WHERE tenant_id = ${q(tenant)} AND upper(code) = ${q(CODE)}`);
  let setup;
  if (existing) {
    const [id, status] = existing.split("|");
    if (status !== "active") fail(`institution ${CODE} exists with status “${status}” (an earlier run stopped half-way); nothing was changed — restore the database or use PILOT_CODE=<other>`);
    setup = lookupSetup(id);
  } else {
    setup = await seedInstitution(admin, { code: CODE, name: NAME, shortName: CODE, domain: "pilot.sutra.dev", city: "Pune" });
    created(`institution ${CODE} active (${setup.id})`);
  }
  const inst = setup.id;
  ok(`institution-active ${CODE} ${inst}`);
  const campusId = psql(`SELECT id FROM institution.campus WHERE institution_id = ${q(inst)} AND code = 'MAIN'`) || fail("main campus missing");
  ok("programme-and-calendar BTECH-CSE 2026-27");

  // ---- students
  const ids = [];
  for (let n = 1; n <= 50; n++) {
    // 25 given × 10 family names: 50 distinct pairs.
    const given = GIVEN[(n - 1) % GIVEN.length];
    const family = FAMILY[(n - 1) % FAMILY.length];
    ids.push(ensureStudent(tenant, setup, { number: `${CODE}-${String(n).padStart(4, "0")}`, given, family, section: n <= 25 ? "A" : "B" }));
  }
  const holder = psql(`SELECT student_number FROM student.student WHERE tenant_id = ${q(tenant)} AND subject_ref = ${q(STUDENT_SUBJECT)} AND status <> 'archived'`);
  if (!holder) {
    psql(`UPDATE student.student SET subject_ref = ${q(STUDENT_SUBJECT)}, updated_at = now() WHERE id = ${q(ids[0])}`);
    created(`${CODE}-0001 linked to the student login`);
  }
  const studentCount = Number(psql(`SELECT count(*) FROM student.student_context WHERE institution_id = ${q(inst)} AND status = 'active'`));
  if (studentCount < 50) fail(`only ${studentCount} active students`);
  ok(`students ${studentCount}`);

  // ---- staff
  for (const s of STAFF) await ensureStaff(admin, inst, campusId, s);
  ok(`staff ${STAFF.length} active with active engagements`);

  // ---- fees
  const heads = itemsOf(await admin.get(`${F}/institutions/${inst}/fee-heads`));
  const head = async (code, name, category) => {
    const h = heads.find((x) => x.code === code);
    if (h) return h;
    const n = await admin.call("POST", `${F}/institutions/${inst}/fee-heads`, { code, name, category });
    created(`fee head ${code}`);
    heads.push(n);
    return n;
  };
  const tuition = await head("TUITION", "Tuition fee", "tuition");
  const hostelHead = await head("HOSTEL", "Hostel fee", "hostel");
  let fs = itemsOf(await admin.get(`${F}/institutions/${inst}/fee-structures`)).find((x) => x.code === `${CODE}-Y1` && x.status !== "superseded");
  if (!fs) {
    fs = await admin.call("POST", `${F}/institutions/${inst}/fee-structures`, {
      code: `${CODE}-Y1`,
      name: "B.Tech CSE — year 1 (2026-27)",
      programmeId: setup.programmeId,
      academicYearId: setup.yearId,
      dueDate: "2026-08-31",
      lines: [
        { feeHeadId: tuition.id, description: "Tuition", amount: "95000.00" },
        { feeHeadId: hostelHead.id, description: "Hostel (optional)", amount: "45000.00" },
      ],
    });
    created(`fee structure ${CODE}-Y1`);
  }
  if (fs.status === "draft") fs = await admin.call("POST", `${F}/fee-structures/${fs.id}/submit`, {}, fs.version);
  if (fs.status === "in_review") fs = await admin.call("POST", `${F}/fee-structures/${fs.id}/decision`, { decision: "approve" }, fs.version);
  if (fs.status === "approved") {
    fs = await admin.call("POST", `${F}/fee-structures/${fs.id}/publish`, {}, fs.version);
    changes += 1;
  }
  if (fs.status !== "published") fail(`fee structure ${CODE}-Y1 is ${fs.status}`);
  ok(`fee-structure ${CODE}-Y1 published (${fs.totalAmount})`);

  // ---- campus hierarchy, library, hostel
  const nodes = itemsOf(await admin.get(`${C}/institutions/${inst}/infrastructure-nodes`));
  const site = await ensureNode(admin, inst, nodes, { nodeType: "site", campusId, code: "MAIN", name: "Main campus site" });
  const block = await ensureNode(admin, inst, nodes, { nodeType: "building", parentId: site.id, code: "MAIN-B1", name: "Academic block A" });
  const floor = await ensureNode(admin, inst, nodes, { nodeType: "floor", parentId: block.id, code: "MAIN-B1-F1", name: "Block A, first floor" });
  const library = await ensureNode(admin, inst, nodes, { nodeType: "space", parentId: floor.id, code: "MAIN-B1-LIB", name: "Central library", spaceType: "library", capacity: 120 });
  const hostelBlock = await ensureNode(admin, inst, nodes, { nodeType: "building", parentId: site.id, code: "MAIN-H1", name: "Hostel — Ganga" });
  const hostelFloor = await ensureNode(admin, inst, nodes, { nodeType: "floor", parentId: hostelBlock.id, code: "MAIN-H1-F1", name: "Ganga, ground floor" });
  const room = await ensureNode(admin, inst, nodes, { nodeType: "space", parentId: hostelFloor.id, code: "MAIN-H1-G01", name: "Room G-01", spaceType: "hostel_room", capacity: 2 });
  ok("campus-hierarchy site → blocks → floors → library + hostel room");

  await ensurePolicy(admin, inst, "library", {
    name: "Library rules 2026-27",
    effectiveFrom: "2026-06-01",
    loanDaysStudent: 14,
    loanDaysStaff: 30,
    maxLoansStudent: 3,
    maxLoansStaff: 6,
    maxRenewals: 2,
    finePerDay: "2.00",
    maxFine: "100.00",
    reservationHoldDays: 3,
    lostChargeFactor: "1.5",
  });
  for (const t of TITLES) {
    let rec = itemsOf(await admin.get(`${C}/institutions/${inst}/catalogue?q=${encodeURIComponent(t.title)}&limit=50`)).find((r) => r.title === t.title);
    if (!rec) {
      rec = await admin.call("POST", `${C}/institutions/${inst}/catalogue`, { title: t.title, authors: t.authors, isbn: t.isbn, replacementCost: "650.00" });
      created(`catalogue record “${t.title}”`);
    }
    if (rec.status === "draft") {
      rec = await admin.call("POST", `${C}/catalogue-records/${rec.id}/publish`, {}, rec.version);
      changes += 1;
    }
    const copies = itemsOf(await admin.get(`${C}/catalogue-records/${rec.id}/copies`));
    if (!copies.length) {
      await admin.call("POST", `${C}/catalogue-records/${rec.id}/copies`, { nodeId: library.id, shelfMark: t.shelfMark });
      created(`copy ${t.shelfMark}`);
    }
  }
  ok(`library policy active, ${TITLES.length} titles catalogued with copies`);

  await ensurePolicy(admin, inst, "hostel", { name: "Hostel policy 2026-27", feePerTerm: "45000.00", deposit: "5000.00", curfewTime: "21:30", effectiveFrom: "2026-06-01" });
  const beds = itemsOf(await admin.get(`${C}/hostel-rooms/${room.id}/beds`));
  for (const bedCode of ["A", "B"])
    if (!beds.some((b) => b.bedCode === bedCode)) {
      await admin.call("POST", `${C}/hostel-rooms/${room.id}/beds`, { bedCode });
      created(`hostel bed G-01/${bedCode}`);
    }
  ok("hostel policy active, room G-01 with 2 beds");

  const final = await admin.get(`${I}/institutions/${inst}`);
  if (final.status !== "active") fail(`${CODE} is ${final.status}`);
  console.log(`changes=${changes}`);
  if (EXPECT_NO_CHANGES && changes > 0) fail(`idempotency: the second run changed ${changes} thing(s)`);
  ok(EXPECT_NO_CHANGES ? "idempotent (no changes on re-run)" : `seeded (${changes} change(s))`);
  console.log(`PILOT_INSTITUTION_ID=${inst}`);
  process.exit(0);
} catch (e) {
  console.error(`\n[seed-pilot] FAILED: ${e instanceof Error ? e.message : String(e)}\n`);
  process.exit(1);
}
