#!/usr/bin/env node
/**
 * Performance-gate data generator (Batch 9 · slice 9D, docs/batch9-contracts.md §9).
 *
 * Runs against an ALREADY RUNNING API in dev auth (the `Performance gate (k6)` job starts the
 * release binary) after `apps/web/e2e/demo-seed.mjs` has taken institution `$DEMO_CODE` to
 * active (programme BTECH-CSE, curriculum CSE-2026, calendar 2026-27, offering CS101-T1).
 *
 * Adds `PERF_STUDENTS` (default 2 000) students to that institution THROUGH THE API — every
 * student walks the real Batch 3 admission pipeline as the administrator:
 *   application (draft) → submit → review → offer → admission accept → documents (each required
 *   type) → submit-documents → verify each → submit-for-approval → financial-clearance →
 *   confirm → create-student → enrol (batch 2026, section A)
 * with `PERF_CONCURRENCY` (default 16) students in flight. Then writes the ids the k6 scripts need
 * to `PERF_DATA` (default ./perf-data.json): institution id, a sample of student ids, term ids.
 *
 * Environment: E2E_API_URL (default http://127.0.0.1:8080), DATABASE_URL (schema owner, only to
 * look up the seeded institution), DEMO_CODE (default DEMO), PERF_STUDENTS, PERF_CONCURRENCY,
 * PERF_DATA. Needs `psql` on PATH. Exits non-zero on the first failed student.
 */
/* global process, console */
import { writeFileSync } from "node:fs";

process.env.DATABASE_URL ??= "postgres://sutra:sutra@localhost:5432/sutra";
const { API, apiLogin, log, psql } = await import("../../apps/web/e2e/lib/harness.mjs");

const CODE = (process.env.DEMO_CODE || "DEMO").toUpperCase();
const TOTAL = Number(process.env.PERF_STUDENTS ?? 2000);
const CONCURRENCY = Number(process.env.PERF_CONCURRENCY ?? 16);
const OUT = process.env.PERF_DATA ?? "perf-data.json";
const S = "/api/v1/student";
const q = (s) => `'${String(s).replace(/'/g, "''")}'`;

const GIVEN = ["Aarav", "Isha", "Kiran", "Rohan", "Meera", "Vihaan", "Ananya", "Arjun", "Diya", "Kabir", "Saanvi", "Reyansh", "Myra", "Advait", "Tara", "Ishaan"];
const FAMILY = ["Desai", "Kulkarni", "Joshi", "Patil", "Shah", "Iyer", "Nair", "Rao", "Gupta", "Menon", "Bhat", "Kapoor", "Reddy", "Sen", "Pillai", "Verma"];

const one = (sql, what) => {
  const v = psql(sql);
  if (!v) throw new Error(`institution ${CODE}: no ${what} (run apps/web/e2e/demo-seed.mjs first)`);
  return v.split("\n")[0];
};

const instId = one(`SELECT id FROM institution.institution WHERE upper(code) = ${q(CODE)} AND status = 'active'`, "active institution");
const programmeId = one(`SELECT id FROM institution.programme WHERE institution_id = ${q(instId)} AND code = 'BTECH-CSE'`, "programme BTECH-CSE");
const termIds = one(
  `SELECT string_agg(t.id::text, ',' ORDER BY t.sequence) FROM institution.academic_term t ` +
    `JOIN institution.calendar_version v ON v.id = t.calendar_version_id WHERE t.institution_id = ${q(instId)} AND v.status = 'published'`,
  "published calendar terms",
).split(",");

const admin = await apiLogin("admin@sutra.dev");
const post = (p, body, version) => admin.call("POST", `${S}${p}`, body ?? {}, version);

/** One applicant through the whole admission pipeline; returns the new student id. */
async function admit(n) {
  // (given, family, date of birth) never repeats within 6 912 applicants: no duplicate candidates.
  const given = GIVEN[n % GIVEN.length];
  const family = FAMILY[Math.floor(n / GIVEN.length) % FAMILY.length];
  let a = await post(`/institutions/${instId}/admission-applications`, {
    programmeId,
    givenName: given,
    familyName: family,
    dateOfBirth: `2007-${String((n % 12) + 1).padStart(2, "0")}-${String((n % 27) + 1).padStart(2, "0")}`,
    email: `perf.${n}@applicants.perf.sutra.dev`,
    phone: `+91 98${String(n).padStart(8, "0")}`,
    admissionTermId: termIds[0],
  });
  a = await post(`/admission-applications/${a.id}/submit`, {}, a.version);
  a = await post(`/admission-applications/${a.id}/review`, {}, a.version);
  a = await post(`/admission-applications/${a.id}/offer`, {}, a.version);
  if (a.status !== "offered" || !a.admissionId) throw new Error(`applicant ${n}: offer → ${a.status}`);
  const aid = a.admissionId;
  let adm = await admin.get(`${S}/admissions/${aid}`);
  adm = await post(`/admissions/${aid}/accept`, {}, adm.version);
  const docs = [];
  for (const t of adm.requiredDocuments ?? [])
    docs.push(await post(`/admissions/${aid}/documents`, { documentType: t, title: `${t} scan`, fileRef: `dms://perf/${n}/${t}.pdf` }));
  await post(`/admissions/${aid}/submit-documents`, {}, adm.version);
  for (const d of docs) await post(`/admissions/${aid}/documents/${d.id}/verify`, { outcome: "verified" }, d.version);
  adm = await admin.get(`${S}/admissions/${aid}`);
  adm = await post(`/admissions/${aid}/submit-for-approval`, {}, adm.version);
  if (adm.status !== "approved") throw new Error(`applicant ${n}: submit-for-approval → ${adm.status}`);
  adm = await post(`/admissions/${aid}/financial-clearance`, {}, adm.version);
  adm = await post(`/admissions/${aid}/confirm`, {}, adm.version);
  adm = await post(`/admissions/${aid}/create-student`, {}, adm.version);
  if (adm.status !== "student_created" || !adm.studentId) throw new Error(`applicant ${n}: create-student → ${adm.status}`);
  const sid = adm.studentId;
  const st = await admin.get(`${S}/students/${sid}`);
  const pending = (st.contexts ?? []).find((c) => c.status === "pending_activation");
  if (!pending) throw new Error(`applicant ${n}: no pending context`);
  const enrolled = await post(`/students/${sid}/enrol`, { batchCode: "2026", sectionCode: "A" }, pending.version);
  if (enrolled.currentContext?.status !== "active") throw new Error(`applicant ${n}: enrol → ${enrolled.currentContext?.status}`);
  return sid;
}

const started = Date.now();
log(`perf seed: ${TOTAL} students into ${CODE} (${instId}) via ${API}, ${CONCURRENCY} in flight`);
const ids = [];
let next = 0;
let done = 0;
async function worker() {
  while (next < TOTAL) {
    const n = next++;
    ids.push(await admit(n));
    done++;
    if (done % 250 === 0) log(`  ${done}/${TOTAL} students (${Math.round((Date.now() - started) / 1000)} s)`);
  }
}
try {
  await Promise.all(Array.from({ length: Math.max(1, CONCURRENCY) }, worker));
} catch (e) {
  console.error(`\n[perf-seed] FAILED after ${done} students: ${e instanceof Error ? e.message : String(e)}\n`);
  process.exit(1);
}
const active = Number(psql(`SELECT count(*) FROM student.student_context WHERE institution_id = ${q(instId)} AND status = 'active'`));
if (active < TOTAL) {
  console.error(`[perf-seed] only ${active} active student contexts in ${CODE}, expected ≥ ${TOTAL}`);
  process.exit(1);
}
const data = {
  institutionId: instId,
  programmeId,
  termIds,
  studentIds: ids.slice(0, 1000),
  students: active,
  seededAt: new Date().toISOString(),
  seedSeconds: Math.round((Date.now() - started) / 1000),
};
writeFileSync(OUT, JSON.stringify(data, null, 2));
log(`  ✓ ${active} active students in ${CODE} after ${data.seedSeconds} s → ${OUT}`);
process.exit(0);
