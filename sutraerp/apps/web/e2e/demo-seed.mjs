#!/usr/bin/env node
/**
 * Demo seed for a local click-through of Batch 3 (student lifecycle) and Batch 4 (academic
 * delivery) against an ALREADY RUNNING stack (Docker compose or a local API) — not a test suite.
 *
 *   Docker (Windows / PowerShell, no local Node/psql needed):
 *     docker compose --profile demo run --rm --build demo-seed
 *   Local:
 *     E2E_API_URL=http://127.0.0.1:8080 DATABASE_URL=postgres://sutra:sutra@localhost:5432/sutra node apps/web/e2e/demo-seed.mjs
 *
 * Creates, next to whatever is already in the database (it never reads or changes any other
 * institution):
 *   - institution DEMO "Sutra Demo College" taken to `active` through the Batch 2 API — the same
 *     calls as the e2e harness (`seedInstitution`): profile → configure → org blueprint + root unit
 *     → main campus → structure → programme BTECH-CSE + curriculum CSE-2026 → calendar 2026-27
 *     (T1, T2) → submit;
 *   - four active students in the Batch 3 foundation tables (like the harness `seedStudent`):
 *     DEMO-0001 Aarav Desai (term 1, linked to the dev student login aarav.desai@sutra.dev),
 *     DEMO-0002 Isha Kulkarni (term 1), DEMO-0003 Kiran Joshi (term 4, for completion + alumni),
 *     DEMO-0004 Rohan Patil (term 4, for a withdrawal exit);
 *   - the Batch 4 delivery fixture of `academic.e2e.mjs` (`seedDelivery`): active offering CS101-T1
 *     (term T1, component CS101) with section A, Dr. Meera Shah (dev|meera.shah@sutra.dev) as
 *     active primary instructor, Aarav and Isha seated.
 *
 * Idempotent: a second run finds DEMO active and only adds what is missing. If DEMO exists but is
 * not active (a run stopped half-way), it refuses — re-run with another code (DEMO_CODE=DEMO2).
 *
 * Environment: E2E_API_URL (default http://127.0.0.1:8080), DATABASE_URL (schema owner, default
 * postgres://sutra:sutra@localhost:5432/sutra), DEMO_CODE (default DEMO), DEMO_NAME
 * (default "Sutra Demo College"). Needs `psql` on PATH (the Docker image has it).
 */
process.env.DATABASE_URL ??= "postgres://sutra:sutra@localhost:5432/sutra";
const { API, apiLogin, check, I, log, psql, seedInstitution } = await import("./lib/harness.mjs");

const CODE = (process.env.DEMO_CODE || "DEMO").toUpperCase();
const NAME = process.env.DEMO_NAME || "Sutra Demo College";
const STUDENT_SUBJECT = "dev|aarav.desai@sutra.dev";
const FACULTY_SUBJECT = "dev|meera.shah@sutra.dev";
const q = (s) => `'${String(s).replace(/'/g, "''")}'`;

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
  throw new Error(`API ${API} is not reachable (${last}). Start the stack first: docker compose --profile app up --build -d`);
}

/** Ids of an existing, active institution (same shape as `seedInstitution` returns). */
function lookupSetup(id) {
  const one = (sql, what) => {
    const v = psql(sql);
    if (!v) throw new Error(`${CODE} is active but has no ${what} — it was not created by this seed; use DEMO_CODE=<other code>`);
    return v;
  };
  const programmeId = one(`SELECT id FROM institution.programme WHERE institution_id = ${q(id)} AND code = 'BTECH-CSE'`, "programme BTECH-CSE");
  const curriculumVersionId = one(
    `SELECT v.id FROM institution.curriculum_version v JOIN institution.curriculum c ON c.id = v.curriculum_id ` +
      `WHERE c.institution_id = ${q(id)} AND c.code = 'CSE-2026' AND v.status = 'published'`,
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

/** Active student + active context (harness `seedStudent`, parameterised); reuses an existing one. */
function ensureStudent(tenant, setup, s) {
  const found = psql(
    `SELECT s.id || '|' || coalesce(c.institution_id::text, '') FROM student.student s ` +
      `LEFT JOIN student.student_context c ON c.student_id = s.id AND c.status = 'active' ` +
      `WHERE s.tenant_id = ${q(tenant)} AND s.student_number = ${q(s.number)}`,
  );
  if (found) {
    const [sid, inst] = found.split("\n")[0].split("|");
    if (inst !== setup.id) throw new Error(`student number ${s.number} already exists outside ${CODE}; use DEMO_CODE=<other code>`);
    log(`  = ${s.number} ${s.given} ${s.family} (already seeded)`);
    return sid;
  }
  const sid = psql(
    `INSERT INTO student.student (tenant_id, student_number, given_name, family_name, primary_email, status) ` +
      `VALUES (${q(tenant)}, ${q(s.number)}, ${q(s.given)}, ${q(s.family)}, ${q(`${s.given.toLowerCase()}.${s.family.toLowerCase()}@students.demo.sutra.dev`)}, 'active') RETURNING id`,
  );
  psql(
    `INSERT INTO student.student_context (tenant_id, student_id, institution_id, programme_id, curriculum_version_id, ` +
      `admission_term_id, admission_year_id, current_term_no, enrolment_number, batch_code, section_code, enrolment_type, enrolled_on, status) ` +
      `VALUES (${q(tenant)}, ${q(sid)}, ${q(setup.id)}, ${q(setup.programmeId)}, ${q(setup.curriculumVersionId)}, ${q(setup.termIds[0])}, ` +
      `${q(setup.yearId)}, ${s.termNo}, ${q(`EN-${s.number}`)}, ${q(s.batch)}, 'A', 'regular', DATE ${q(s.enrolledOn)}, 'active') RETURNING id`,
  );
  log(`  + ${s.number} ${s.given} ${s.family} (term ${s.termNo})`);
  return sid;
}

/** Offering + section + faculty + seats (academic.e2e.mjs `seedDelivery`), each only if missing. */
function ensureDelivery(tenant, setup, studentIds) {
  const [componentId, title, credits] = psql(
    `SELECT id || '|' || title || '|' || credits FROM institution.curriculum_component ` +
      `WHERE curriculum_version_id = ${q(setup.curriculumVersionId)} AND slot_code = 'CS101'`,
  ).split("|");
  let offeringId = psql(
    `SELECT id FROM academic.course_offering WHERE institution_id = ${q(setup.id)} AND academic_term_id = ${q(setup.termIds[0])} ` +
      `AND lower(offering_code) = 'cs101-t1' AND status NOT IN ('cancelled', 'archived', 'merged', 'split') LIMIT 1`,
  );
  if (!offeringId)
    offeringId = psql(
      `INSERT INTO academic.course_offering (tenant_id, institution_id, academic_year_id, academic_term_id, ` +
        `curriculum_version_id, curriculum_component_id, offering_code, title, course_code, category, credits, capacity, status) ` +
        `VALUES (${q(tenant)}, ${q(setup.id)}, ${q(setup.yearId)}, ${q(setup.termIds[0])}, ${q(setup.curriculumVersionId)}, ` +
        `${q(componentId)}, 'CS101-T1', ${q(title)}, 'CS101', 'major_core', ${credits}, 60, 'active') RETURNING id`,
    );
  let sectionId = psql(`SELECT id FROM academic.offering_section WHERE offering_id = ${q(offeringId)} AND lower(section_code) = 'a' AND status NOT IN ('cancelled', 'merged') LIMIT 1`);
  if (!sectionId)
    sectionId = psql(
      `INSERT INTO academic.offering_section (tenant_id, institution_id, offering_id, section_code, capacity, status) ` +
        `VALUES (${q(tenant)}, ${q(setup.id)}, ${q(offeringId)}, 'A', 60, 'active') RETURNING id`,
    );
  psql(
    `INSERT INTO academic.faculty_assignment (tenant_id, institution_id, offering_id, section_id, faculty_subject_ref, ` +
      `faculty_name, role, weekly_hours, effective_from, status) SELECT ${q(tenant)}, ${q(setup.id)}, ${q(offeringId)}, ` +
      `${q(sectionId)}, ${q(FACULTY_SUBJECT)}, 'Dr. Meera Shah', 'primary_instructor', 4, DATE '2026-06-15', 'active' ` +
      `WHERE NOT EXISTS (SELECT 1 FROM academic.faculty_assignment WHERE section_id = ${q(sectionId)} ` +
      `AND faculty_subject_ref = ${q(FACULTY_SUBJECT)} AND role = 'primary_instructor' AND status = 'active')`,
  );
  for (const sid of studentIds)
    psql(
      `INSERT INTO academic.section_enrolment (tenant_id, institution_id, offering_id, section_id, student_id, ` +
        `student_context_id, student_number, display_name, subject_ref, origin, status) ` +
        `SELECT s.tenant_id, ${q(setup.id)}, ${q(offeringId)}, ${q(sectionId)}, s.id, c.id, s.student_number, ` +
        `s.given_name || ' ' || coalesce(s.family_name, ''), s.subject_ref, 'override', 'enrolled' ` +
        `FROM student.student s JOIN student.student_context c ON c.student_id = s.id AND c.status = 'active' WHERE s.id = ${q(sid)} ` +
        `AND NOT EXISTS (SELECT 1 FROM academic.section_enrolment e WHERE e.offering_id = ${q(offeringId)} AND e.student_id = s.id AND e.status = 'enrolled')`,
    );
  return { offeringId, sectionId };
}

try {
  if (!/^[A-Z0-9][A-Z0-9-]{1,15}$/.test(CODE)) throw new Error(`DEMO_CODE “${CODE}” must be 2-16 letters, digits or dashes`);
  log(`demo seed: institution ${CODE} “${NAME}” via ${API}`);
  psql("SELECT 1 FROM academic.course_offering LIMIT 1"); // DB reachable and migrated through Batch 4
  await waitForApi();
  const admin = await apiLogin("admin@sutra.dev");

  const existing = psql(`SELECT id || '|' || status || '|' || tenant_id FROM institution.institution WHERE upper(code) = ${q(CODE)}`);
  if (existing.includes("\n")) throw new Error(`more than one institution with code ${CODE} (several tenants?) — use DEMO_CODE=<other code>`);
  let setup;
  if (existing) {
    const [id, status] = existing.split("|");
    if (status !== "active")
      throw new Error(
        `institution ${CODE} already exists with status “${status}” (an earlier seed stopped half-way, or the code belongs to another institution). ` +
          `Nothing was changed. Re-run with a new code, e.g. PowerShell: $env:DEMO_CODE="DEMO2"; docker compose --profile demo run --rm demo-seed`,
      );
    log(`— ${CODE} already active (${id}): checking students and the course offering`);
    setup = lookupSetup(id);
  } else {
    log("— institution to activation through the Batch 2 API");
    setup = await seedInstitution(admin, { code: CODE, name: NAME, shortName: CODE, domain: `${CODE.toLowerCase()}.sutra.dev`, city: "Pune" });
  }
  const tenant = psql(`SELECT tenant_id FROM institution.institution WHERE id = ${q(setup.id)}`);

  log("— students (Batch 3 foundation tables)");
  const st = (n, given, family, termNo, batch, enrolledOn) => ({ number: `${CODE}-${n}`, given, family, termNo, batch, enrolledOn });
  const aarav = ensureStudent(tenant, setup, st("0001", "Aarav", "Desai", 1, "2026", "2026-07-01"));
  const isha = ensureStudent(tenant, setup, st("0002", "Isha", "Kulkarni", 1, "2026", "2026-07-01"));
  ensureStudent(tenant, setup, st("0003", "Kiran", "Joshi", 4, "2023", "2023-07-01"));
  ensureStudent(tenant, setup, st("0004", "Rohan", "Patil", 4, "2023", "2023-07-01"));
  // Aarav ↔ the dev student login (one live student per login subject in the tenant).
  const holder = psql(
    `SELECT s.student_number FROM student.student s WHERE s.tenant_id = ${q(tenant)} AND s.subject_ref = ${q(STUDENT_SUBJECT)} AND s.status <> 'archived'`,
  );
  if (!holder) {
    psql(`UPDATE student.student SET subject_ref = ${q(STUDENT_SUBJECT)}, updated_at = now() WHERE id = ${q(aarav)}`);
    log(`  ✓ ${CODE}-0001 Aarav Desai linked to the student login (aarav.desai@sutra.dev)`);
  } else if (holder === `${CODE}-0001`) log("  = Aarav already linked to the student login");
  else log(`  ! the student login is already linked to student ${holder} — Aarav left unlinked (the Student role signs in as ${holder})`);

  log("— course offering CS101-T1 / section A, Dr. Meera Shah, seats (Batch 4 foundation tables)");
  const d = ensureDelivery(tenant, setup, [aarav, isha]);
  const seats = psql(`SELECT count(*) FROM academic.section_enrolment WHERE section_id = ${q(d.sectionId)} AND status = 'enrolled'`);
  check(Number(seats) >= 2, `CS101-T1 / A has ${seats} seated students, faculty dev|meera.shah@sutra.dev`);

  const inst = await admin.get(`${I}/institutions/${setup.id}`);
  check(inst.status === "active", `${inst.code} “${inst.name}” is ${inst.status}`);
  console.log(
    [
      "",
      `Demo data ready — institution ${inst.code} “${inst.name}” (${setup.id})`,
      "  Open http://localhost:3000 and sign in with the development role cards:",
      "    Administrator (admin@sutra.dev)   → pick “" + inst.name + "” in the Institution picker",
      `    Student       (aarav.desai@sutra.dev) → Aarav Desai, ${CODE}-0001, seated in CS101-T1`,
      "    Faculty       (meera.shah@sutra.dev)  → Dr. Meera Shah, teaches CS101-T1 / A",
      `  Other students: ${CODE}-0002 Isha Kulkarni (seated), ${CODE}-0003 Kiran Joshi (completion/alumni), ${CODE}-0004 Rohan Patil (exit)`,
      "",
    ].join("\n"),
  );
  process.exit(0);
} catch (e) {
  console.error(`\n[demo-seed] FAILED: ${e instanceof Error ? e.message : String(e)}\n`);
  process.exit(1);
}
