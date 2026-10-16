#!/usr/bin/env node
/**
 * Backup / restore drill data (Part 2 · Stage 5) — runs against the RUNNING production-mode compose
 * stack (Keycloak OIDC + TOTP, human approvals, documents in MinIO) after the Part 1 journey
 * (`apps/web/e2e/journey.e2e.mjs`, which configured the users' TOTP and saved the secrets in
 * E2E_STATE). Everything goes through the public API as real users — no SQL writes:
 *
 *   institution DRL-… taken to active (harness seedInstitution, administrator decisions)
 *   fee head + fee structure DRL-Y1 (₹45,000): maker = administrator, checker = finance approver
 *   DRILL_STUDENTS (default 2 000) applicants → offer (identity proof + qualifying marksheet) →
 *     accept → both documents handed in WITH A PDF FILE each (PUT …/file, stored in MinIO, sha256
 *     recorded in platform.document_object) → verify → submit-for-approval → registrar decision →
 *     financial clearance → confirm → create student → enrol
 *   charges generated for every student (bulk endpoint), then a counter payment per student
 *     (even: paid in full; odd: ₹20,000 part payment) with receipt + journal posting
 * Marks / attendance / results come from the journey institution (same database).
 *
 * Every request passes a client-side limiter (DRILL_RPS, default 80 req/s: the web proxy allows
 * 100 r/s per address) and is retried on 429. Writes DRILL_OUT (JSON: institution, counts,
 * timings). Env: E2E_BASE (http://localhost:3000), E2E_AUTH=oidc, E2E_STATE, DATABASE_URL
 * (superuser; read-only lookups), DRILL_STUDENTS, DRILL_CONCURRENCY (12), DRILL_DOC_KB (24).
 */
/* global process, console */
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

process.env.DATABASE_URL ??= "postgres://sutra:sutra@localhost:5432/sutra";
const lib = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../../../apps/web/e2e/lib");

// ---- global request limiter (token bucket) + 429 retry, installed before the libraries load
const RPS = Number(process.env.DRILL_RPS ?? 80);
const realFetch = globalThis.fetch;
let tokens = RPS;
let last = Date.now();
let requests = 0;
let throttled = 0;
async function takeToken() {
  for (;;) {
    const now = Date.now();
    tokens = Math.min(RPS, tokens + ((now - last) / 1000) * RPS);
    last = now;
    if (tokens >= 1) {
      tokens -= 1;
      return;
    }
    await new Promise((r) => setTimeout(r, Math.ceil(((1 - tokens) / RPS) * 1000)));
  }
}
globalThis.fetch = async (url, init) => {
  for (let attempt = 0; ; attempt++) {
    await takeToken();
    requests++;
    const res = await realFetch(url, init);
    if (res.status !== 429 || attempt >= 8) return res;
    throttled++;
    await res.arrayBuffer().catch(() => {});
    await new Promise((r) => setTimeout(r, 250 * 2 ** Math.min(attempt, 4)));
  }
};

const { login, totpSecrets } = await import(`${lib}/auth.mjs`);
const { approve, CHECKERS } = await import(`${lib}/approvals.mjs`);
const { seedInstitution, psql } = await import(`${lib}/harness.mjs`);

const TOTAL = Number(process.env.DRILL_STUDENTS ?? 2000);
const CONCURRENCY = Number(process.env.DRILL_CONCURRENCY ?? 12);
const DOC_KB = Number(process.env.DRILL_DOC_KB ?? 24);
const STATE = process.env.E2E_STATE ?? "/tmp/journey-state.json";
const OUT = process.env.DRILL_OUT ?? "drill-seed.json";
const S = "/api/v1/student";
const F = "/api/v1/finance";
const q = (s) => `'${String(s).replace(/'/g, "''")}'`;
const log = (...a) => console.log("[drill-seed]", ...a);
const day = (n = 0) => new Intl.DateTimeFormat("en-CA", { timeZone: "Asia/Kolkata" }).format(new Date(Date.now() + n * 86400000));

const state = existsSync(STATE) ? JSON.parse(readFileSync(STATE, "utf8")) : { ids: {}, totp: {} };
for (const [k, v] of Object.entries(state.totp ?? {})) totpSecrets.set(k, v);
const saveTotp = () => {
  const now = existsSync(STATE) ? JSON.parse(readFileSync(STATE, "utf8")) : { ids: {}, totp: {} };
  now.totp = { ...(now.totp ?? {}), ...Object.fromEntries(totpSecrets) };
  writeFileSync(STATE, JSON.stringify(now, null, 2));
};

/** A plain PDF of about `kb` KiB whose padding is unique hex (distinct sha256 per document). */
function pdf(label, kb) {
  const head = `%PDF-1.7\n% SutraERP backup drill document: ${label}\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n2 0 obj << /Type /Pages /Kids [] /Count 0 >> endobj\n`;
  const tail = "trailer << /Root 1 0 R >>\n%%EOF\n";
  const lines = [];
  let size = head.length + tail.length;
  for (let i = 0; size < kb * 1024; i++) {
    const l = `%${crypto.randomUUID().replace(/-/g, "").repeat(15)}${String(i).padStart(6, "0")}\n`;
    lines.push(l);
    size += l.length;
  }
  return Buffer.from(head + lines.join("") + tail, "latin1");
}

const t0 = Date.now();
const timing = {};
const mark = (k) => (timing[k] = Math.round((Date.now() - t0) / 1000));

// ---- sign-ins (Keycloak OIDC + TOTP; MFA sessions for the privileged roles)
const admin = await login("administrator");
for (const role of ["registrar", "finance_approver"]) CHECKERS[role] = await login(role);
CHECKERS.administrator = admin;
saveTotp();
log(`signed in: administrator ${admin.subject}, registrar, finance approver (mfa ${admin.session.mfaLevel})`);

// ---- institution + fee structure
const suffix = Date.now().toString(36).toUpperCase().slice(-4);
const code = `DRL-${suffix}`;
const setup = await seedInstitution(admin, { code, name: `Backup Drill College ${suffix}`, shortName: "DRL", domain: "drill.edu.in", city: "Pune" });
log(`institution ${code} ${setup.id} active`);
const campuses = await admin.get(`/api/v1/institution/institutions/${setup.id}/campuses`);
const campusId = (campuses.items ?? campuses).find((c) => c.code === "MAIN")?.id ?? null;
const head = await admin.call("POST", `${F}/institutions/${setup.id}/fee-heads`, { code: "TUITION", name: "Tuition fee", category: "tuition", refundable: false });
let fs = await admin.call("POST", `${F}/institutions/${setup.id}/fee-structures`, {
  code: "DRL-Y1",
  name: "Drill year 1",
  academicYearId: setup.yearId,
  programmeId: setup.programmeId,
  dueDate: day(45),
  lines: [{ feeHeadId: head.id, description: "Tuition fee — year 1", amount: "45000.00" }],
});
fs = await admin.call("POST", `${F}/fee-structures/${fs.id}/submit`, {}, fs.version);
fs = await approve("fee_structure", fs.id, { client: admin, step: "drill: fee structure" });
fs = await admin.call("POST", `${F}/fee-structures/${fs.id}/publish`, {}, fs.version);
if (fs.status !== "published") throw new Error(`fee structure: ${fs.status}`);
mark("setupSeconds");
log(`fee structure DRL-Y1 ${fs.id} published (${timing.setupSeconds} s)`);

// ---- students through admission, with document files
const GIVEN = ["Aarav", "Isha", "Kiran", "Rohan", "Meera", "Vihaan", "Ananya", "Arjun", "Diya", "Kabir", "Saanvi", "Reyansh", "Myra", "Advait", "Tara", "Ishaan"];
const FAMILY = ["Desai", "Kulkarni", "Joshi", "Patil", "Shah", "Iyer", "Nair", "Rao", "Gupta", "Menon", "Bhat", "Kapoor", "Reddy", "Sen", "Pillai", "Verma"];
const DOCS = [
  ["identity_proof", "Aadhaar card"],
  ["qualifying_marksheet", "HSC marksheet"],
];
let docBytes = 0;
let docCount = 0;
async function admit(n) {
  const given = GIVEN[n % GIVEN.length];
  const family = FAMILY[Math.floor(n / GIVEN.length) % FAMILY.length];
  let a = await admin.call("POST", `${S}/institutions/${setup.id}/admission-applications`, {
    programmeId: setup.programmeId,
    givenName: given,
    familyName: `${family} ${n}`,
    dateOfBirth: `2007-${String((n % 12) + 1).padStart(2, "0")}-${String((n % 27) + 1).padStart(2, "0")}`,
    email: `drill.${suffix.toLowerCase()}.${n}@applicants.drill.edu.in`,
    phone: `+91 97${String(n).padStart(8, "0")}`,
    admissionTermId: setup.termIds[0],
    campusId,
  });
  a = await admin.call("POST", `${S}/admission-applications/${a.id}/submit`, {}, a.version);
  a = await admin.call("POST", `${S}/admission-applications/${a.id}/review`, {}, a.version);
  a = await admin.call("POST", `${S}/admission-applications/${a.id}/offer`, { offerExpiresOn: day(30), requiredDocuments: DOCS.map((d) => d[0]) }, a.version);
  if (!a.admissionId) throw new Error(`applicant ${n}: offer → ${a.status}`);
  const aid = a.admissionId;
  let adm = await admin.get(`${S}/admissions/${aid}`);
  adm = await admin.call("POST", `${S}/admissions/${aid}/accept`, {}, adm.version);
  const docs = [];
  for (const [type, title] of DOCS) {
    const d = await admin.call("POST", `${S}/admissions/${aid}/documents`, { documentType: type, title, fileRef: `dms://drill/${n}/${type}.pdf` });
    const bytes = pdf(`${code} applicant ${n} ${type}`, DOC_KB);
    const up = await admin.upload(`${S}/admissions/${aid}/documents/${d.id}/file`, bytes, "application/pdf", `${type}-${n}.pdf`);
    if (up.status !== 201) throw new Error(`applicant ${n}: upload ${type} → ${up.status} ${up.text.slice(0, 300)}`);
    docBytes += bytes.length;
    docCount++;
    // the upload bumps the document version (ETag) — verify with the new one
    docs.push({ id: d.id, version: up.json?.documentVersion ?? d.version });
  }
  adm = await admin.get(`${S}/admissions/${aid}`);
  adm = await admin.call("POST", `${S}/admissions/${aid}/submit-documents`, {}, adm.version);
  for (const d of docs) await admin.call("POST", `${S}/admissions/${aid}/documents/${d.id}/verify`, { outcome: "verified" }, d.version);
  adm = await admin.get(`${S}/admissions/${aid}`);
  adm = await admin.call("POST", `${S}/admissions/${aid}/submit-for-approval`, {}, adm.version);
  adm = await approve("admission", aid, { client: admin, step: `drill: admission ${n}` });
  adm = await admin.call("POST", `${S}/admissions/${aid}/financial-clearance`, {}, adm.version);
  adm = await admin.call("POST", `${S}/admissions/${aid}/confirm`, {}, adm.version);
  adm = await admin.call("POST", `${S}/admissions/${aid}/create-student`, {}, adm.version);
  if (!adm.studentId) throw new Error(`applicant ${n}: create-student → ${adm.status}`);
  const st = await admin.get(`${S}/students/${adm.studentId}`);
  const pending = (st.contexts ?? []).find((c) => c.status === "pending_activation");
  if (!pending) throw new Error(`applicant ${n}: no pending context`);
  const enrolled = await admin.call("POST", `${S}/students/${adm.studentId}/enrol`, { batchCode: "2026", sectionCode: "A" }, pending.version);
  if (enrolled.currentContext?.status !== "active") throw new Error(`applicant ${n}: enrol → ${enrolled.currentContext?.status}`);
  return adm.studentId;
}

const ids = [];
let next = 0;
let done = 0;
let failure = null;
async function worker() {
  while (next < TOTAL && !failure) {
    const n = next++;
    try {
      ids[n] = await admit(n);
    } catch (e) {
      failure = e;
      return;
    }
    done++;
    if (done % 100 === 0) log(`  ${done}/${TOTAL} students (${Math.round((Date.now() - t0) / 1000)} s, ${requests} requests, ${throttled} throttled)`);
  }
}
await Promise.all(Array.from({ length: Math.max(1, CONCURRENCY) }, worker));
if (failure) {
  console.error(`[drill-seed] FAILED after ${done} students: ${failure instanceof Error ? failure.stack : String(failure)}`);
  process.exit(1);
}
mark("studentsSeconds");
log(`✓ ${done} students admitted + enrolled with ${docCount} document files (${(docBytes / 1048576).toFixed(1)} MiB) after ${timing.studentsSeconds} s`);

// ---- fees: charges for everyone, then a counter payment each
let charges = 0;
for (let i = 0; i < ids.length; i += 100) {
  const r = await admin.call("POST", `${F}/fee-structures/${fs.id}/generate-charges`, { studentIds: ids.slice(i, i + 100) });
  charges += r.created?.length ?? 0;
}
const rows = psql(`SELECT c.student_account_id || '|' || c.outstanding_amount FROM finance.charge c WHERE c.institution_id = ${q(setup.id)} AND c.status = 'published' ORDER BY c.charge_number`)
  .split("\n")
  .filter(Boolean);
let paid = 0;
let paidAmount = 0;
next = 0;
async function payer() {
  while (next < rows.length && !failure) {
    const i = next++;
    const [account, outstanding] = rows[i].split("|");
    const amount = i % 2 === 0 ? outstanding : "20000.00";
    try {
      const p = await admin.call("POST", `${F}/student-accounts/${account}/payments`, { channel: i % 3 === 0 ? "bank_transfer" : "cash", amount, externalRef: `DRL-${suffix}-${String(i).padStart(5, "0")}` });
      if (p.status !== "completed" || !p.receipt?.receiptNumber) throw new Error(`payment ${i}: ${p.status}`);
    } catch (e) {
      failure = e;
      return;
    }
    paid++;
    paidAmount += Number(amount);
  }
}
await Promise.all(Array.from({ length: Math.max(1, CONCURRENCY) }, payer));
if (failure) {
  console.error(`[drill-seed] payments FAILED after ${paid}: ${failure instanceof Error ? failure.stack : String(failure)}`);
  process.exit(1);
}
mark("feesSeconds");
const balanced = psql(`SELECT sum(total_debit) = sum(total_credit) FROM finance.journal_entry WHERE institution_id = ${q(setup.id)}`);
log(`✓ ${charges} charges, ${paid} payments (₹${paidAmount.toFixed(2)}), ledger balanced: ${balanced} (${timing.feesSeconds} s)`);
if (balanced !== "t") process.exit(1);

const out = {
  institutionId: setup.id,
  institutionCode: code,
  tenantId: admin.tenant,
  students: done,
  documents: docCount,
  documentBytes: docBytes,
  charges,
  payments: paid,
  paidAmount: paidAmount.toFixed(2),
  requests,
  throttled,
  timing,
  sampleStudentIds: ids.slice(0, 20),
};
writeFileSync(OUT, JSON.stringify(out, null, 2));
log(`→ ${OUT}`);
process.exit(0);
