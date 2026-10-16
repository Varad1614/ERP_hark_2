#!/usr/bin/env node
/**
 * Part 2 · S5-capacity: seeds a realistic 2,000-student college into the PRODUCTION-mode capacity
 * stack (deploy/capacity/compose.yml), through the public API behind the TLS edge — the way the
 * college office would onboard: Keycloak OIDC sign-in (+ TOTP for privileged roles), human
 * approvals by checker personas (Flowable), the Stage 4 data import for bulk data.
 *
 *   import (Stage 4)  departments, programmes, 150 staff, 2,000 students + guardians,
 *                     previous-year dues (opening balances → second-person approval)
 *   identity (S4-idp) the API provisions every imported student into Keycloak; the college then
 *                     sets first passwords (here: Keycloak admin API, as a bulk activation)
 *   API               offerings CS101 / MD101 / EL101 with 34 divisions of 60, 102 faculty
 *                     assignments, term registrations decided by a second administrator, seats,
 *                     a generated + approved timetable, 6 weeks of attendance per division
 *                     (marked by the division's own faculty), fee structure → 2,000 charges →
 *                     counter payments (40 % paid, 40 % part-paid), end-semester exam (CS101):
 *                     registrations, conduct, marks, approval, results computed → approved →
 *                     published; an internal assessment (MD101) left open for live marks entry;
 *                     one verified document (ID card PDF in MinIO) per student.
 *
 * Writes $CAP_DIR/cap-data.json (ids the k6 scripts need) and $CAP_DIR/seed-report.json (counts
 * and durations of every phase). Env: E2E_AUTH=oidc, E2E_BASE (https://erp.<domain>),
 * E2E_PASSWORD (= SEED_PASSWORD), NODE_EXTRA_CA_CERTS (the private CA), DATABASE_URL (schema
 * owner, read-only lookups via psql), CAP_DIR, CAP_DOMAIN, KC_ADMIN_PASSWORD, CAP_STUDENTS
 * (default 2000), CAP_ATTENDANCE_WEEKS (default 6), CAP_CONCURRENCY (default 12).
 */
/* global process, console */
import { readFileSync, writeFileSync } from "node:fs";
import path from "node:path";

const CAP_DIR = process.env.CAP_DIR ?? `${process.cwd()}/capacity-run`;
const DOMAIN = process.env.CAP_DOMAIN ?? "sutra.test";
const N = Number(process.env.CAP_STUDENTS ?? 2000);
const WEEKS = Number(process.env.CAP_ATTENDANCE_WEEKS ?? 6);
const CONC = Number(process.env.CAP_CONCURRENCY ?? 12);
const DIV = 60;
const DIVS = Math.ceil(N / DIV);
const SSO = `https://sso.${DOMAIN}`;

const { login } = await import("../../apps/web/e2e/lib/auth.mjs");
const { approve, CHECKERS } = await import("../../apps/web/e2e/lib/approvals.mjs");
const { seedInstitution, I, psql } = await import("../../apps/web/e2e/lib/harness.mjs");
const STAFF = JSON.parse(readFileSync(path.join(CAP_DIR, "staff.json"), "utf8"));

const A = "/api/v1/academic";
const S = "/api/v1/student";
const F = "/api/v1/finance";
const X = "/api/v1/exams";
const IMP = "/api/v1/platform/imports";
const q = (s) => `'${String(s).replace(/'/g, "''")}'`;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const day = (n = 0) => new Intl.DateTimeFormat("en-CA", { timeZone: "Asia/Kolkata" }).format(new Date(Date.now() + n * 86400000));

const report = { startedAt: new Date().toISOString(), students: N, divisions: DIVS, phases: [] };
const data = { domain: DOMAIN, students: [], sections: {}, office: STAFF.office.map((o) => ({ login: o.login, otp: o.otp })), faculty: [] };
const log = (...a) => console.log(`[seed ${new Date().toISOString().slice(11, 19)}]`, ...a);
const save = () => {
  report.finishedAt = new Date().toISOString();
  writeFileSync(path.join(CAP_DIR, "seed-report.json"), JSON.stringify(report, null, 2));
  writeFileSync(path.join(CAP_DIR, "cap-data.json"), JSON.stringify(data));
};

/** Runs one phase; `critical` phases stop the seed on failure, the others are reported. */
async function phase(name, fn, { critical = true } = {}) {
  const t0 = Date.now();
  log(`▶ ${name}`);
  const entry = { name, status: "running", ms: 0, detail: {} };
  report.phases.push(entry);
  try {
    entry.detail = (await fn()) ?? {};
    entry.status = "ok";
  } catch (e) {
    entry.status = "failed";
    entry.error = String(e?.stack ?? e).slice(0, 2000);
    log(`✗ ${name}: ${entry.error}`);
    if (critical) {
      entry.ms = Date.now() - t0;
      save();
      process.exit(1);
    }
  }
  entry.ms = Date.now() - t0;
  log(`${entry.status === "ok" ? "✓" : "✗"} ${name} (${(entry.ms / 1000).toFixed(1)} s) ${JSON.stringify(entry.detail).slice(0, 400)}`);
  save();
}

/** Bounded parallel map; collects failures instead of stopping at the first. */
async function pool(items, n, fn) {
  let i = 0;
  const out = new Array(items.length);
  const errors = [];
  await Promise.all(
    Array.from({ length: Math.max(1, Math.min(n, items.length)) }, async () => {
      while (i < items.length) {
        const k = i++;
        try {
          out[k] = await fn(items[k], k);
        } catch (e) {
          errors.push(`${k}: ${String(e?.message ?? e).slice(0, 300)}`);
        }
      }
    }),
  );
  return { out, errors };
}
const must = ({ out, errors }, what, tolerance = 0) => {
  if (errors.length > tolerance) throw new Error(`${what}: ${errors.length} failed — first: ${errors.slice(0, 3).join(" | ")}`);
  return out;
};

/** Every item of a cursor-paginated list. */
async function all(c, p) {
  const items = [];
  let cursor = null;
  for (let page = 0; page < 200; page++) {
    const sep = p.includes("?") ? "&" : "?";
    const r = await c.get(`${p}${sep}limit=200${cursor ? `&cursor=${encodeURIComponent(cursor)}` : ""}`);
    items.push(...(r.items ?? []));
    cursor = r.nextCursor ?? null;
    if (!cursor) break;
  }
  return items;
}

const csvCell = (v) => (v === null || v === undefined ? "" : /[",\n]/.test(String(v)) ? `"${String(v).replace(/"/g, '""')}"` : String(v));
const csv = (header, rows) => `${header.join(",")}\n${rows.map((r) => header.map((h) => csvCell(r[h])).join(",")).join("\n")}\n`;

/** Stage 4 data import: upload → dry run → commit (→ second-person approval when required). */
async function runImport(c, entity, inst, rows, header, { checker } = {}) {
  const file = csv(header, rows);
  const t0 = Date.now();
  let job = await c.call("POST", IMP, { entity, institutionId: inst, fileName: `${entity}.csv`, contentBase64: Buffer.from(file).toString("base64") });
  const tUp = Date.now();
  job = await c.call("POST", `${IMP}/${job.id}/dry-run`, {});
  const tDry = Date.now();
  if (job.status !== "validated") {
    const errs = await c.get(`${IMP}/${job.id}/rows?errorsOnly=true`).catch(() => ({}));
    throw new Error(`import ${entity}: dry run ${job.status} ${JSON.stringify(job.summary)} ${JSON.stringify((errs.items ?? []).slice(0, 3))}`);
  }
  job = await c.call("POST", `${IMP}/${job.id}/commit`, {});
  if (job.status === "pending_approval") {
    if (!checker) throw new Error(`import ${entity} needs an approver`);
    job = await checker.call("POST", `${IMP}/${job.id}/decision`, { approve: true, reason: "Matches the legacy ledger (capacity seed)" });
  }
  if (job.status !== "committed") throw new Error(`import ${entity}: ${job.status} ${JSON.stringify(job.summary)}`);
  return { entity, rows: rows.length, bytes: file.length, uploadMs: tUp - t0, dryRunMs: tDry - tUp, commitMs: Date.now() - tDry, summary: job.summary };
}

// ------------------------------------------------------------------ Keycloak admin (bulk activation)
async function kcToken() {
  const r = await fetch(`${SSO}/realms/master/protocol/openid-connect/token`, {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({ grant_type: "password", client_id: "admin-cli", username: "admin", password: process.env.KC_ADMIN_PASSWORD }).toString(),
  });
  if (r.status !== 200) throw new Error(`Keycloak admin token → ${r.status} ${(await r.text()).slice(0, 200)}`);
  return (await r.json()).access_token;
}
let kc = { token: null, at: 0 };
async function kcCall(method, p, body) {
  if (!kc.token || Date.now() - kc.at > 40000) kc = { token: await kcToken(), at: Date.now() };
  const r = await fetch(`${SSO}/admin/realms/sutra${p}`, {
    method,
    headers: { authorization: `Bearer ${kc.token}`, "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (r.status >= 300) throw new Error(`Keycloak ${method} ${p} → ${r.status} ${(await r.text()).slice(0, 200)}`);
  return r.status === 204 ? null : r.json().catch(() => null);
}

// ------------------------------------------------------------------ names
const GIVEN = ["Aarav", "Isha", "Kiran", "Rohan", "Meera", "Vihaan", "Ananya", "Arjun", "Diya", "Kabir", "Saanvi", "Reyansh", "Myra", "Advait", "Tara", "Ishaan", "Pooja", "Siddharth", "Neha", "Omkar"];
const FAMILY = ["Desai", "Kulkarni", "Joshi", "Patil", "Shah", "Iyer", "Nair", "Rao", "Gupta", "Menon", "Bhat", "Kapoor", "Reddy", "Sen", "Pillai", "Verma", "Deshmukh", "Jadhav", "Pawar", "Gaikwad"];
const pad = (n, w = 4) => String(n).padStart(w, "0");
const divCode = (d) => `D${pad(d + 1, 2)}`;

// ================================================================== run
const C = {};
await phase("sign-in: administrator, second administrator and checkers (Keycloak OIDC + TOTP)", async () => {
  for (const role of ["administrator", "academic_admin", "registrar", "finance_approver", "exam_controller"]) {
    const t0 = Date.now();
    C[role] = await login(role);
    CHECKERS[role] = C[role];
    report[`login_${role}_ms`] = Date.now() - t0;
  }
  return { roles: Object.keys(C), adminMfa: C.administrator.session.mfaLevel };
});
const admin = C.administrator;
const dean = C.academic_admin;

let setup;
const ids = {};
await phase("institution → active (profile, blueprint, campus, structure, BTECH-CSE + curriculum, calendar, activation)", async () => {
  const suffix = Date.now().toString(36).toUpperCase().slice(-4);
  setup = await seedInstitution(admin, { code: `CAP-${suffix}`, name: `Sahyadri College of Engineering ${suffix}`, shortName: "SCOE", domain: `students.${DOMAIN}`, city: "Pune", decider: admin });
  const campuses = await admin.get(`${I}/institutions/${setup.id}/campuses`);
  ids.campusId = (campuses.items ?? campuses).find((c) => c.code === "MAIN").id;
  data.institutionId = setup.id;
  data.termId = setup.termIds[0];
  return { institution: setup.id, code: `CAP-${suffix}` };
});
const inst = setup.id;

await phase("import: departments + programmes (Stage 4 import)", async () => {
  const units = [
    ["CSE", "Department of Computer Engineering"],
    ["ECE", "Department of Electronics and Telecommunication"],
    ["MECH", "Department of Mechanical Engineering"],
    ["CIVIL", "Department of Civil Engineering"],
    ["MATH", "Department of Engineering Sciences"],
  ].map(([code, name]) => ({ code, name, unit_type: "department", parent_code: "ROOT" }));
  const u = await runImport(admin, "org_units", inst, units, ["code", "name", "unit_type", "parent_code"]);
  const progs = [
    ["BTECH-ECE", "B.Tech Electronics and Telecommunication", "ECE"],
    ["BTECH-ME", "B.Tech Mechanical Engineering", "MECH"],
    ["BTECH-CE", "B.Tech Civil Engineering", "CIVIL"],
  ].map(([code, name, org]) => ({ code, name, level: "ug", degree_type: "btech", duration_terms: 8, org_unit_code: org }));
  const p = await runImport(admin, "programmes", inst, progs, ["code", "name", "level", "degree_type", "duration_terms", "org_unit_code"]);
  const all_ = await admin.get(`${I}/institutions/${inst}/org-units`);
  ids.depts = Object.fromEntries((all_.items ?? all_).map((x) => [x.code, x.id]));
  return { orgUnits: u, programmes: p };
}, { critical: false });

await phase("import: 150 staff (100 teaching, 40 administrative, 10 non-teaching)", async () => {
  const rows = [];
  STAFF.faculty.forEach((f, i) => rows.push({ given_name: `Faculty${pad(i + 1, 3)}`, family_name: "Capacity", work_email: f.login, phone: `+91 97${pad(i + 1, 8)}`, staff_category: "teaching", status: "active" }));
  STAFF.office.forEach((o, i) => rows.push({ given_name: `Office${pad(i + 1, 3)}`, family_name: "Capacity", work_email: o.login, phone: `+91 96${pad(i + 1, 8)}`, staff_category: "administrative", status: "active" }));
  for (let i = 1; i <= 10; i++) rows.push({ given_name: `Support${pad(i, 3)}`, family_name: "Capacity", work_email: `cap.support.${pad(i, 3)}@${DOMAIN}`, phone: `+91 95${pad(i, 8)}`, staff_category: "non_teaching", status: "active" });
  return runImport(admin, "staff", inst, rows, ["given_name", "family_name", "work_email", "phone", "staff_category", "status"]);
}, { critical: false });

await phase(`import: ${N} students + guardians (Stage 4 import, one CSV)`, async () => {
  const rows = [];
  for (let i = 0; i < N; i++) {
    const g = GIVEN[i % GIVEN.length];
    const f = FAMILY[Math.floor(i / GIVEN.length) % FAMILY.length];
    rows.push({
      student_number: `CAP26/${pad(i + 1)}`,
      given_name: g,
      family_name: f,
      date_of_birth: `2007-${pad((i % 12) + 1, 2)}-${pad((i % 27) + 1, 2)}`,
      email: `s${pad(i + 1)}@students.${DOMAIN}`,
      programme_code: "BTECH-CSE",
      enrolment_number: `CAP26-${pad(i + 1)}`,
      roll_number: `${divCode(Math.floor(i / DIV))}-${pad((i % DIV) + 1, 2)}`,
      batch_code: "2026",
      section_code: divCode(Math.floor(i / DIV)),
      guardian_name: `${GIVEN[(i + 7) % GIVEN.length]} ${f}`,
      guardian_relationship: i % 2 ? "father" : "mother",
      guardian_phone: `+91 98${pad(i + 1, 8)}`,
    });
  }
  const r = await runImport(admin, "students", inst, rows, Object.keys(rows[0]));
  const db = psql(`SELECT s.id || '|' || s.student_number FROM student.student s JOIN student.student_context c ON c.student_id = s.id WHERE c.institution_id = ${q(inst)} AND s.student_number LIKE 'CAP26/%' ORDER BY s.student_number`);
  for (const line of db.split("\n").filter(Boolean)) {
    const [id, number] = line.split("|");
    const i = Number(number.split("/")[1]) - 1;
    data.students[i] = { id, number, login: `s${pad(i + 1)}@students.${DOMAIN}`, div: Math.floor(i / DIV) };
  }
  if (data.students.filter(Boolean).length !== N) throw new Error(`expected ${N} imported students, found ${data.students.filter(Boolean).length}`);
  return r;
});

await phase("identities: imported students provisioned into Keycloak by the API (idp-events → idp-sync)", async () => {
  const t0 = Date.now();
  let synced = 0;
  let last = "";
  for (let i = 0; i < 360; i++) {
    const r = psql(`SELECT coalesce(sum((sync_status = 'synced')::int), 0) || '|' || count(*) || '|' || coalesce(sum((sync_status NOT IN ('synced','pending'))::int), 0) FROM platform.identity_link WHERE subject_type = 'student' AND subject_id IN (SELECT id FROM student.student WHERE student_number LIKE 'CAP26/%')`);
    const [s, total, bad] = r.split("|").map(Number);
    synced = s;
    if (r !== last) log(`  identities synced ${s} / links ${total} / failed ${bad} (${Math.round((Date.now() - t0) / 1000)} s)`);
    last = r;
    if (s >= N) break;
    await sleep(5000);
  }
  if (synced < N) throw new Error(`only ${synced}/${N} identities synced after ${Math.round((Date.now() - t0) / 1000)} s`);
  return { synced, seconds: Math.round((Date.now() - t0) / 1000), perSecond: +(synced / ((Date.now() - t0) / 1000)).toFixed(1) };
});

await phase("identities: first passwords set by the college (Keycloak admin API, bulk activation)", async () => {
  const links = psql(`SELECT l.idp_user_id || '|' || s.student_number FROM platform.identity_link l JOIN student.student s ON s.id = l.subject_id WHERE s.student_number LIKE 'CAP26/%'`).split("\n").filter(Boolean);
  const r = await pool(links, 8, async (line) => {
    const [uid] = line.split("|");
    await kcCall("PUT", `/users/${uid}`, { requiredActions: [], emailVerified: true });
    await kcCall("PUT", `/users/${uid}/reset-password`, { type: "password", value: process.env.E2E_PASSWORD, temporary: false });
  });
  must(r, "set passwords");
  return { users: links.length };
});

// ------------------------------------------------------------------ academics
const SLOTS = ["CS101", "MD101", "EL101"];
await phase(`offerings CS101 / MD101 / EL101 × ${DIVS} divisions (submit → approval → publish → open)`, async () => {
  const cv = await admin.get(`${I}/curriculum-versions/${setup.curriculumVersionId}`);
  const comps = Object.fromEntries((cv.components ?? []).filter((c) => c.termNo === 1).map((c) => [c.slotCode, c]));
  ids.components = {};
  ids.offerings = {};
  for (const slot of SLOTS) {
    if (!comps[slot]) throw new Error(`curriculum component ${slot} missing`);
    let o = await admin.call("POST", `${A}/institutions/${inst}/offerings`, {
      academicTermId: setup.termIds[0],
      curriculumVersionId: setup.curriculumVersionId,
      slotCode: slot,
      offeringCode: `${slot}-T1`,
      capacity: DIVS * DIV,
      campusId: ids.campusId,
      orgUnitId: ids.depts?.[slot === "MD101" ? "MATH" : "CSE"] ?? undefined,
      sections: Array.from({ length: DIVS }, (_, d) => ({ sectionCode: divCode(d), capacity: DIV })),
    });
    o = await admin.call("POST", `${A}/offerings/${o.id}/submit`, {}, o.version);
    o = await approve("offering", o.id, { client: admin, step: `capacity: offering ${slot}` });
    o = await admin.call("POST", `${A}/offerings/${o.id}/publish`, {}, o.version);
    o = await admin.call("POST", `${A}/offerings/${o.id}/open-enrolment`, {}, o.version);
    if (o.status !== "enrollment_open") throw new Error(`offering ${slot}: ${o.status}`);
    ids.offerings[slot] = o.id;
    ids.components[slot] = o.curriculumComponentId;
    data.sections[slot] = o.sections.map((s) => ({ id: s.id, code: s.sectionCode }));
  }
  data.offerings = ids.offerings;
  return { sections: SLOTS.map((s) => data.sections[s].length) };
});

await phase("faculty assignments (one teacher per division and course; maker-checker)", async () => {
  const work = [];
  SLOTS.forEach((slot, si) => data.sections[slot].forEach((sec, d) => work.push({ slot, sec, d, f: STAFF.faculty[(si * DIVS + d) % STAFF.faculty.length] })));
  const r = await pool(work, 6, async ({ sec, f }) => {
    let fa = await admin.call("POST", `${A}/sections/${sec.id}/faculty-assignments`, { facultySubjectRef: f.subject, facultyName: f.name, role: "primary_instructor", weeklyHours: 3, effectiveFrom: "2026-06-15" });
    fa = await admin.call("POST", `${A}/faculty-assignments/${fa.id}/submit`, {}, fa.version);
    fa = await approve("faculty_assignment", fa.id, { client: admin, step: "capacity: faculty assignment" });
    fa = await admin.call("POST", `${A}/faculty-assignments/${fa.id}/activate`, {}, fa.version);
    if (fa.status !== "active") throw new Error(`assignment ${fa.status}`);
    sec.faculty = f.login;
  });
  must(r, "faculty assignments");
  const used = new Set(work.map((w) => w.f.login));
  data.faculty = STAFF.faculty.filter((f) => used.has(f.login)).map((f) => ({ login: f.login, subject: f.subject }));
  return { assignments: work.length, faculty: used.size };
});

await phase(`term registrations for ${N} students (office registration → second administrator decides) + seats in 3 divisions`, async () => {
  let w = await admin.call("POST", `${S}/institutions/${inst}/registration-windows`, { academicTermId: setup.termIds[0], name: "Term 1 registration", opensOn: day(0), closesOn: day(20), minCredits: 16, maxCredits: 28 });
  w = await admin.call("POST", `${S}/registration-windows/${w.id}/open`, {}, w.version);
  const r = await pool(data.students, CONC, async (st) => {
    let reg = await admin.call("POST", `${S}/students/${st.id}/registrations`, { academicTermId: setup.termIds[0], lines: SLOTS.map((slot) => ({ curriculumComponentId: ids.components[slot] })) });
    reg = await admin.call("POST", `${S}/registrations/${reg.id}/submit`, {}, reg.version);
    const d = await dean.call("POST", `${S}/registrations/${reg.id}/decision`, { decision: "approve", reason: "Term 1 registration (capacity seed)" }, reg.version);
    if (!["registered", "finalised"].includes(d?.status)) throw new Error(`registration ${d?.status}`);
    for (const slot of SLOTS) {
      const seat = await admin.call("POST", `${A}/sections/${data.sections[slot][st.div].id}/enrolments`, { studentId: st.id });
      if (seat.status !== "enrolled") throw new Error(`seat ${slot}: ${seat.status}`);
    }
  });
  must(r, "registrations + seats", Math.floor(N * 0.01));
  return { students: N, errors: r.errors.length, firstErrors: r.errors.slice(0, 3) };
});

await phase("timetable: scheduling requirements → generate → validate → approve → publish → activate", async () => {
  const reqs = [];
  SLOTS.forEach((slot, si) => data.sections[slot].forEach((sec) => reqs.push({ sec, perWeek: [3, 2, 1][si] })));
  must(await pool(reqs, 6, ({ sec, perWeek }) => admin.call("POST", `${A}/institutions/${inst}/scheduling-requirements`, { sectionId: sec.id, sessionsPerWeek: perWeek, sessionMinutes: 60, roomLabel: `Room-${sec.code}` })), "scheduling requirements");
  let tt = await admin.call("POST", `${A}/institutions/${inst}/timetables`, { academicTermId: setup.termIds[0], name: "Term 1 timetable" });
  tt = await admin.call("POST", `${A}/timetables/${tt.id}/generate`, {}, tt.version);
  tt = await admin.call("POST", `${A}/timetables/${tt.id}/validate`, {}, tt.version);
  if (tt.status !== "validated") throw new Error(`timetable not valid: ${JSON.stringify(tt.validationReport).slice(0, 600)}`);
  tt = await admin.call("POST", `${A}/timetables/${tt.id}/submit`, {}, tt.version);
  if (tt.status !== "approved") {
    const d = await dean.call("POST", `${A}/timetables/${tt.id}/decision`, { decision: "approve", reason: "Term 1 timetable" }, tt.version);
    tt = { ...tt, ...d };
  }
  tt = await admin.get(`${A}/timetables/${tt.id}`);
  tt = await admin.call("POST", `${A}/timetables/${tt.id}/publish`, {}, tt.version);
  tt = await admin.call("POST", `${A}/timetables/${tt.id}/activate`, {}, tt.version);
  data.timetableId = tt.id;
  return { slots: tt.slots?.length, status: tt.status };
}, { critical: false });

// seat map: division → student ids (same division in every course)
const byDiv = Array.from({ length: DIVS }, () => []);
data.students.forEach((s) => byDiv[s.div].push(s.id));

await phase(`attendance: ${WEEKS} weeks of sessions per division, marked and finalised by each division's teacher`, async () => {
  const facultyClients = {};
  const logins = [...new Set(SLOTS.flatMap((slot) => data.sections[slot].map((s) => s.faculty)).filter(Boolean))];
  must(await pool(logins, 4, async (l) => { facultyClients[l] = await login(l); }), "faculty sign-in", 2);
  // past weekdays, newest first
  const dates = [];
  for (let k = 3; dates.length < WEEKS * 5; k++) {
    const d = new Date(Date.now() - k * 86400000);
    if (d.getUTCDay() !== 0 && d.getUTCDay() !== 6) dates.push(day(-k));
  }
  const work = [];
  SLOTS.forEach((slot, si) =>
    data.sections[slot].forEach((sec, d) => {
      // CS101 3 / week, MD101 2, EL101 1 — the timetable's weekly load
      const n = WEEKS * [3, 2, 1][si];
      for (let k = 0; k < n; k++) work.push({ sec, d, date: dates[Math.floor((k * dates.length) / n)], hour: 9 + si });
    }),
  );
  let records = 0;
  const r = await pool(work, CONC, async ({ sec, d, date, hour }) => {
    const fc = facultyClients[sec.faculty];
    if (!fc) throw new Error(`no faculty session for ${sec.faculty}`);
    let s = await fc.call("POST", `${A}/sections/${sec.id}/sessions`, { sessionDate: date, startsAt: `${String(hour).padStart(2, "0")}:00`, endsAt: `${String(hour).padStart(2, "0")}:55`, topic: "Lecture" });
    s = await fc.call("POST", `${A}/sessions/${s.id}/start`, {}, s.version);
    const marks = byDiv[d].map((id) => ({ studentId: id, mark: Math.random() < 0.86 ? "present" : Math.random() < 0.7 ? "absent" : "late" }));
    s = await fc.call("POST", `${A}/sessions/${s.id}/attendance`, { records: marks }, s.version);
    s = await fc.call("POST", `${A}/sessions/${s.id}/submit-attendance`, {}, s.version);
    s = await fc.call("POST", `${A}/sessions/${s.id}/complete`, {}, s.version);
    s = await fc.call("POST", `${A}/sessions/${s.id}/finalise-attendance`, {}, s.version);
    if (s.attendanceStatus !== "finalised") throw new Error(`session ${s.attendanceStatus}`);
    records += marks.length;
  });
  must(r, "attendance sessions", Math.floor(work.length * 0.02));
  return { sessions: work.length - r.errors.length, records, errors: r.errors.length, firstErrors: r.errors.slice(0, 3) };
}, { critical: false });

// ------------------------------------------------------------------ fees
await phase(`fees: structure BTECH-Y1 (approved) → ${N} charges → counter payments (40 % full, 40 % part)`, async () => {
  const head = await admin.call("POST", `${F}/institutions/${inst}/fee-heads`, { code: "TUITION", name: "Tuition fee", category: "tuition", refundable: false });
  let fs = await admin.call("POST", `${F}/institutions/${inst}/fee-structures`, { code: "BTECH-Y1", name: "B.Tech year 1", academicYearId: setup.yearId, programmeId: setup.programmeId, dueDate: day(45), lines: [{ feeHeadId: head.id, description: "Tuition fee — year 1", amount: "90000.00" }] });
  fs = await admin.call("POST", `${F}/fee-structures/${fs.id}/submit`, {}, fs.version);
  fs = await approve("fee_structure", fs.id, { client: admin, step: "capacity: fee structure" });
  fs = await admin.call("POST", `${F}/fee-structures/${fs.id}/publish`, {}, fs.version);
  const chunks = [];
  for (let i = 0; i < N; i += 100) chunks.push(data.students.slice(i, i + 100));
  const t0 = Date.now();
  const g = await pool(chunks, 2, (ch) => admin.call("POST", `${F}/fee-structures/${fs.id}/generate-charges`, { studentIds: ch.map((s) => s.id) }));
  must(g, "generate charges");
  const genMs = Date.now() - t0;
  const charges = new Map(g.out.flatMap((x) => x?.created ?? []).map((c) => [c.studentId, c.chargeId]));
  const acc = psql(`SELECT student_id || '|' || student_account_id || '|' || id FROM finance.charge WHERE institution_id = ${q(inst)} AND status = 'published' AND source_type <> 'opening_balance'`).split("\n").filter(Boolean);
  const accounts = new Map(acc.map((l) => l.split("|")).map(([s, a, c]) => [s, { account: a, charge: c }]));
  data.students.forEach((s) => Object.assign(s, { account: accounts.get(s.id)?.account, charge: accounts.get(s.id)?.charge ?? charges.get(s.id) }));
  const payers = data.students.map((s, i) => ({ s, amount: i % 5 < 2 ? "90000.00" : i % 5 < 4 ? "30000.00" : null })).filter((x) => x.amount && x.s.account);
  const t1 = Date.now();
  const p = await pool(payers, CONC, ({ s, amount }, k) =>
    admin.call("POST", `${F}/student-accounts/${s.account}/payments`, { channel: k % 3 ? "bank_transfer" : "cash", amount, externalRef: `CAP-SEED-${pad(k + 1, 5)}`, allocations: [{ chargeId: s.charge, amount }] }),
  );
  must(p, "counter payments", Math.floor(payers.length * 0.01));
  return { charges: charges.size, generateMs: genMs, payments: payers.length - p.errors.length, paymentsMs: Date.now() - t1, feeStructure: fs.id };
});

await phase("import: previous-year dues for 300 students (opening balances → finance approver decides)", async () => {
  const rows = data.students.slice(0, 300).map((s, i) => ({ student_number: s.number, amount: `${(1500 + (i % 7) * 250).toFixed(2)}`, due_date: day(30), description: "Dues carried over from 2025-26" }));
  return runImport(admin, "opening_balances", inst, rows, ["student_number", "amount", "due_date", "description"], { checker: C.finance_approver });
}, { critical: false });

// ------------------------------------------------------------------ examinations
async function examSession(code, name, slot, scheme, pol) {
  let ses = await admin.call("POST", `${X}/institutions/${inst}/exam-sessions`, { code, name, academicTermId: setup.termIds[0], gradingSchemeId: scheme.id, policyId: pol.id, startsOn: day(-12), endsOn: day(-5), registrationOpensOn: day(-30), registrationClosesOn: day(10) });
  const paper = await admin.call("POST", `${X}/exam-sessions/${ses.id}/papers`, { offeringId: ids.offerings[slot], maxMarks: "100", internalWeight: "0", passPercent: "40", durationMinutes: 180 });
  ses = await admin.get(`${X}/exam-sessions/${ses.id}`);
  ses = await admin.call("POST", `${X}/exam-sessions/${ses.id}/submit`, {}, ses.version);
  ses = await approve("exam_session", ses.id, { client: admin, step: `capacity: exam session ${code}` });
  if (ses.status === "approved") ses = await admin.call("POST", `${X}/exam-sessions/${ses.id}/schedule`, {}, ses.version);
  if (ses.status === "scheduled") ses = await admin.call("POST", `${X}/exam-sessions/${ses.id}/activate`, {}, ses.version);
  if (ses.status !== "active") throw new Error(`exam session ${code}: ${ses.status}`);
  const venue = await admin.call("POST", `${X}/institutions/${inst}/exam-venues`, { code: `HALL-${code}`, name: `Exam hall ${code}`, capacity: N + 100 });
  let sch = await admin.call("POST", `${X}/exam-sessions/${ses.id}/schedules`, { name: `${name} timetable` });
  const sitting = await admin.call("POST", `${X}/exam-schedules/${sch.id}/sittings`, { paperId: paper.id, sitsOn: day(-8), startsAt: "09:30" });
  await admin.call("POST", `${X}/exam-sittings/${sitting.id}/venues`, { venueId: venue.id });
  sch = await admin.get(`${X}/exam-schedules/${sch.id}`);
  sch = await admin.call("POST", `${X}/exam-schedules/${sch.id}/submit`, {}, sch.version);
  sch = await approve("exam_schedule", sch.id, { client: admin, step: `capacity: exam timetable ${code}` });
  if (sch.status !== "published") sch = await admin.call("POST", `${X}/exam-schedules/${sch.id}/publish`, {}, sch.version);
  // registrations from the rosters (in chunks), hall tickets, conduct (everyone present)
  let registered = 0;
  for (let i = 0; i < N; i += 250) {
    const r = await admin.call("POST", `${X}/exam-sessions/${ses.id}/registrations/bulk`, { studentIds: data.students.slice(i, i + 250).map((s) => s.id) });
    registered += r.processed ?? 0;
  }
  await admin.call("POST", `${X}/exam-sessions/${ses.id}/hall-tickets/issue`, {}).catch((e) => log(`  hall tickets: ${e.message.slice(0, 200)}`));
  let c = await admin.call("POST", `${X}/exam-sittings/${sitting.id}/conduct`, {});
  c = await admin.call("POST", `${X}/conduct-records/${c.id}/start`, {}, c.version);
  c = await admin.call("POST", `${X}/conduct-records/${c.id}/complete`, { attendance: [], defaultOutcome: "present" }, c.version);
  const sheet = await admin.call("POST", `${X}/exam-papers/${paper.id}/mark-sheet`, {});
  return { session: ses, paper, sheet, registered, present: c.presentCount };
}

/** Enters marks for every row of a paper (bulk PUTs of 250 rows), as `who`. */
async function enterMarks(who, paperId) {
  const marks = await who.get(`${X}/exam-papers/${paperId}/marks`);
  const rows = (marks.items ?? []).filter((m) => !m.absent);
  for (let i = 0; i < rows.length; i += 250)
    await who.call("PUT", `${X}/exam-papers/${paperId}/marks`, { rows: rows.slice(i, i + 250).map((m) => ({ id: m.id, version: m.version, externalMarks: String(35 + Math.floor(Math.random() * 61)) })) });
  return rows.length;
}

await phase("examinations: end-semester CS101 → marks → approval → results computed, approved, published", async () => {
  const scheme = await admin.call("POST", `${X}/institutions/${inst}/grading-schemes`, {
    code: "CBCS-10",
    name: "CBCS ten-point",
    bands: [
      ["O", "90", "10", true], ["A+", "80", "9", true], ["A", "70", "8", true], ["B+", "60", "7", true],
      ["B", "55", "6", true], ["C", "50", "5", true], ["P", "40", "4", true], ["F", "0", "0", false],
    ].map(([grade, minPercent, gradePoint, isPass]) => ({ grade, minPercent, gradePoint, isPass })),
  });
  let pol = await admin.call("POST", `${X}/institutions/${inst}/exam-policies`, { code: "EXAM-POL", name: "Examination policy 2026" });
  pol = await admin.call("POST", `${X}/exam-policies/${pol.id}/submit`, {}, pol.version);
  pol = await approve("exam_policy", pol.id, { client: admin, step: "capacity: exam policy" });
  if (pol.status !== "active") pol = await admin.call("POST", `${X}/exam-policies/${pol.id}/activate`, {}, pol.version);
  ids.scheme = scheme;
  ids.policy = pol;
  const ese = await examSession(`ESE-${Date.now().toString(36).slice(-4).toUpperCase()}`, "End-semester examinations", "CS101", scheme, pol);
  const t0 = Date.now();
  let entered;
  try {
    entered = await enterMarks(admin, ese.paper.id);
  } catch (e) {
    log(`  marks as administrator refused (${e.message.slice(0, 160)}); entering as each division's teacher`);
    entered = 0;
    for (const sec of data.sections.CS101) entered += await enterMarks(await login(sec.faculty), ese.paper.id).catch(() => 0);
  }
  const marksMs = Date.now() - t0;
  let sh = await admin.get(`${X}/mark-sheets/${ese.sheet.id}`);
  sh = await admin.call("POST", `${X}/mark-sheets/${sh.id}/submit`, {}, sh.version);
  sh = await admin.call("POST", `${X}/mark-sheets/${sh.id}/verify`, {}, sh.version);
  sh = await admin.call("POST", `${X}/mark-sheets/${sh.id}/request-approval`, {}, sh.version);
  sh = await approve("mark_sheet", sh.id, { client: admin, step: "capacity: final marks" });
  sh = await admin.call("POST", `${X}/mark-sheets/${sh.id}/finalise`, {}, sh.version);
  const t1 = Date.now();
  let b = await admin.call("POST", `${X}/exam-sessions/${ese.session.id}/result-batches`, {});
  b = await admin.call("POST", `${X}/result-batches/${b.id}/compute`, {}, b.version);
  const computed = b.computed;
  b = await admin.get(`${X}/result-batches/${b.id}`);
  b = await admin.call("POST", `${X}/result-batches/${b.id}/validate`, {}, b.version);
  b = await approve("result_batch", b.id, { client: admin, step: "capacity: results" });
  b = await admin.call("POST", `${X}/result-batches/${b.id}/finalise`, {}, b.version);
  let pub = await admin.call("POST", `${X}/exam-sessions/${ese.session.id}/publications`, {});
  pub = await admin.call("POST", `${X}/publications/${pub.id}/authorise`, {}, pub.version);
  pub = await approve("publication", pub.id, { client: admin, step: "capacity: publication" });
  pub = await admin.call("POST", `${X}/publications/${pub.id}/publish`, {}, pub.version);
  const published = Number(psql(`SELECT count(*) FROM exams.term_result WHERE session_id = ${q(ese.session.id)} AND status = 'published'`));
  data.ese = { sessionId: ese.session.id, paperId: ese.paper.id };
  return { registered: ese.registered, present: ese.present, marksEntered: entered, marksMs, computed, published, resultsMs: Date.now() - t1 };
}, { critical: false });

await phase("examinations: internal assessment MD101 left open for live marks entry", async () => {
  if (!ids.scheme) throw new Error("no grading scheme (end-semester phase failed)");
  const ia = await examSession(`IA-${Date.now().toString(36).slice(-4).toUpperCase()}`, "Internal assessment 1", "MD101", ids.scheme, ids.policy);
  data.ia = { sessionId: ia.session.id, paperId: ia.paper.id, sheetId: ia.sheet.id };
  return { registered: ia.registered, sheet: ia.sheet.status };
}, { critical: false });

// ------------------------------------------------------------------ documents
const PDF = (() => {
  // A small valid PDF (~40 KB with padding comment) — a scanned ID card stand-in.
  const body = "%PDF-1.4\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\n3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 300 200]>>endobj\n";
  const pad_ = `%${"0".repeat(78)}\n`.repeat(500);
  const trailer = "trailer<</Root 1 0 R>>\n%%EOF\n";
  return Buffer.from(body + pad_ + trailer);
})();
await phase(`documents: one verified ID-card PDF per student (${(PDF.length / 1024).toFixed(0)} KiB, MinIO, SSE)`, async () => {
  const r = await pool(data.students, CONC, async (st) => {
    let d = await admin.call("POST", `${S}/students/${st.id}/documents`, { documentType: "identity_proof", title: "College ID card" });
    const up = await admin.upload(`${S}/students/${st.id}/documents/${d.id}/file`, PDF, "application/pdf", `id-card-${st.number.replace("/", "-")}.pdf`);
    if (up.status >= 300) throw new Error(`upload → ${up.status} ${up.text.slice(0, 200)}`);
    d = up.json ?? d;
    const v = await admin.call("POST", `${S}/students/${st.id}/documents/${d.id}/verify`, { outcome: "verified" }, d.version);
    st.document = v?.id ?? d.id;
  });
  must(r, "documents", Math.floor(N * 0.01));
  return { documents: N - r.errors.length, bytes: PDF.length, errors: r.errors.length, firstErrors: r.errors.slice(0, 3) };
}, { critical: false });

await phase("final counts (database)", async () => {
  const count = (sql) => Number(psql(sql));
  return {
    students: count(`SELECT count(*) FROM student.student_context WHERE institution_id = ${q(inst)} AND status = 'active'`),
    identities: count(`SELECT count(*) FROM platform.identity_link WHERE sync_status = 'synced'`),
    seats: count(`SELECT count(*) FROM academic.section_enrolment WHERE institution_id = ${q(inst)}`),
    attendanceRecords: count(`SELECT count(*) FROM academic.attendance_record r JOIN academic.teaching_session t ON t.id = r.session_id WHERE t.institution_id = ${q(inst)}`),
    sessions: count(`SELECT count(*) FROM academic.teaching_session WHERE institution_id = ${q(inst)}`),
    charges: count(`SELECT count(*) FROM finance.charge WHERE institution_id = ${q(inst)}`),
    payments: count(`SELECT count(*) FROM finance.payment WHERE institution_id = ${q(inst)}`),
    termResults: count(`SELECT count(*) FROM exams.term_result WHERE institution_id = ${q(inst)}`),
    documents: count(`SELECT count(*) FROM platform.document_object`),
    auditRows: count(`SELECT count(*) FROM platform.audit_log`),
    dbBytes: count(`SELECT pg_database_size(current_database())`),
  };
}, { critical: false });

save();
log(`seed complete → ${path.join(CAP_DIR, "cap-data.json")}`);
process.exit(0);
