#!/usr/bin/env node
/**
 * On-prem smoke journey (Part 2 · S5-deploy) — run from a verification checkout AGAINST an
 * installed server, through its public https names exactly like a browser would (real Keycloak
 * OIDC sign-in with TOTP, the TLS edge, the web container's /api proxy). Never touches the server's
 * files; the database is read by the shell harness (sutra-onprem counts), not here.
 *
 *   node onprem-smoke.mjs seed       institution → active, a student admitted end to end (admission
 *                                    approved by the registrar), fee structure (approved by the
 *                                    finance approver) → charge → counter payment with receipt
 *   node onprem-smoke.mjs document   student record document + a 3 MiB file uploaded over the 11 MiB
 *                                    route, downloaded back byte for byte (MinIO over TLS)
 *   node onprem-smoke.mjs verify     signs in again and re-reads everything the state file names
 *                                    (student, payment + receipt, document bytes when present)
 *
 * Env: E2E_AUTH=oidc, E2E_BASE=https://<erp host>, E2E_PASSWORD, E2E_ADMIN_LOGIN,
 * E2E_REGISTRAR_LOGIN, E2E_FINANCE_APPROVER_LOGIN (users created with sutra-onprem create-user),
 * NODE_EXTRA_CA_CERTS=<CA of the server certificate>, ONPREM_STATE=<json file kept between phases>.
 * Prints one "ok …" line per check and exits non-zero on the first failure.
 */
import { createHash } from "node:crypto";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { login, totpSecrets } from "../../../apps/web/e2e/lib/auth.mjs";
import { CHECKERS, approve } from "../../../apps/web/e2e/lib/approvals.mjs";
import { seedInstitution, I } from "../../../apps/web/e2e/lib/harness.mjs";
import { admitViaApi, day } from "../../../apps/web/e2e/lib/journey-seed.mjs";

const S = "/api/v1/student";
const F = "/api/v1/finance";
const PHASE = process.argv[2] ?? "verify";
const STATE = process.env.ONPREM_STATE ?? "onprem-state.json";
const state = existsSync(STATE) ? JSON.parse(readFileSync(STATE, "utf8")) : {};
const t0 = Date.now();

const ok = (m) => console.log(`ok ${m}`);
function need(cond, m) {
  if (!cond) throw new Error(`FAILED: ${m}`);
  ok(m);
}
const sha256hex = (b) => createHash("sha256").update(b).digest("hex");
/** Minimal valid PDF padded with comment lines (the document content check accepts it). */
function pdf(label, size = 0) {
  const head = `%PDF-1.7\n% SutraERP on-prem smoke document: ${label}\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n2 0 obj << /Type /Pages /Kids [] /Count 0 >> endobj\n`;
  const tail = "trailer << /Root 1 0 R >>\n%%EOF\n";
  const line = `%${"0".repeat(1022)}\n`;
  const n = Math.max(0, Math.ceil((size - head.length - tail.length) / line.length));
  return Buffer.from(head + line.repeat(n) + tail, "latin1");
}
function save() {
  state.totp = Object.fromEntries(totpSecrets);
  state.lastSignInAt = Date.now();
  writeFileSync(STATE, JSON.stringify(state, null, 2));
}

// TOTP secrets survive between phases (Keycloak issued them at the first sign-in); a code may be
// used once, so a new process waits for the next 30 s window after the previous phase's sign-in.
for (const [k, v] of Object.entries(state.totp ?? {})) totpSecrets.set(k, v);
if (state.lastSignInAt && Date.now() - state.lastSignInAt < 31000) {
  await new Promise((r) => setTimeout(r, 31000 - (Date.now() - state.lastSignInAt)));
}

async function seed() {
  const admin = await login("administrator");
  need(admin.session.mfaLevel === "mfa", `administrator signed in through Keycloak OIDC with TOTP (mfa=${admin.session.mfaLevel}, tenant ${admin.tenant})`);
  CHECKERS.administrator = admin;
  CHECKERS.registrar = await login("registrar");
  CHECKERS.finance_approver = await login("finance_approver");
  save();
  ok("checker personas signed in (registrar, finance approver)");

  const setup = await seedInstitution(admin, { code: `ONP-${Date.now().toString(36).toUpperCase().slice(-4)}`, name: "On-prem Staging College", shortName: "OSC", domain: "osc.edu.in", city: "Pune" });
  await approve("institution_activation", setup.id, { client: admin, decidedEarlier: true, step: "on-prem: institution activation" });
  const inst = await admin.get(`${I}/institutions/${setup.id}`);
  need(inst.status === "active", `institution ${inst.code} ${setup.id} active`);
  const campuses = await admin.get(`${I}/institutions/${setup.id}/campuses`);
  setup.campusId = (campuses.items ?? campuses).find((c) => c.code === "MAIN").id;

  const student = await admitViaApi(admin, setup, { given: "Ananya", family: "Patil", dob: "2007-03-14", email: `ananya.${Date.now()}@students.osc.edu.in`, phone: "+91 98220 11223", roll: "OSC-26-001" });
  need(!!student.id, `student created through admission (approved by the registrar): ${student.id} (${student.number ?? "no number"})`);

  const head = await admin.call("POST", `${F}/institutions/${setup.id}/fee-heads`, { code: "TUITION", name: "Tuition fee", category: "tuition", refundable: false });
  let fs = await admin.call("POST", `${F}/institutions/${setup.id}/fee-structures`, {
    code: "BTECH-Y1",
    name: "B.Tech year 1",
    academicYearId: setup.yearId,
    programmeId: setup.programmeId,
    dueDate: day(45),
    lines: [{ feeHeadId: head.id, description: "Tuition fee — year 1", amount: "90000.00" }],
  });
  fs = await admin.call("POST", `${F}/fee-structures/${fs.id}/submit`, {}, fs.version);
  fs = await approve("fee_structure", fs.id, { client: admin, selfCheck: true, step: "on-prem: fee structure" });
  fs = await admin.call("POST", `${F}/fee-structures/${fs.id}/publish`, {}, fs.version);
  need(fs.status === "published", `fee structure BTECH-Y1 approved by the finance approver (maker's self-approval refused) and published (₹${fs.totalAmount})`);
  const gen = await admin.call("POST", `${F}/fee-structures/${fs.id}/generate-charges`, { studentIds: [student.id] });
  const charge = gen.created?.[0];
  need(charge?.status === "published" && charge.studentId === student.id, `charge ${charge?.chargeNumber} published for the student`);
  const ch = await admin.get(`${F}/charges/${charge.chargeId}`);
  const accountId = ch.studentAccountId ?? ch.accountId;
  need(!!accountId, `student account ${accountId}`);
  const pay = await admin.call("POST", `${F}/student-accounts/${accountId}/payments`, { channel: "cash", amount: "30000.00", externalRef: `ONPREM-${Date.now()}` });
  need(pay.status === "completed" && pay.receipt?.receiptNumber, `counter payment ${pay.paymentNumber} ₹30,000 completed, receipt ${pay.receipt?.receiptNumber}`);

  Object.assign(state, {
    institutionId: setup.id,
    studentId: student.id,
    chargeId: charge.chargeId,
    accountId,
    paymentId: pay.id,
    paymentNumber: pay.paymentNumber,
    receiptNumber: pay.receipt.receiptNumber,
    seededAt: new Date().toISOString(),
  });
  save();
}

async function document() {
  need(!!state.studentId, "state names a seeded student");
  const admin = await login("administrator");
  save();
  const doc = await admin.call("POST", `${S}/students/${state.studentId}/documents`, { documentType: "identity_proof", title: "Aadhaar card (on-prem smoke)" });
  const bytes = pdf("identity proof", 3 * 1024 * 1024);
  const up = await admin.upload(`${S}/students/${state.studentId}/documents/${doc.id}/file`, bytes, "application/pdf", "aadhaar-onprem.pdf");
  need(up.status === 201, `PUT document file (${bytes.length} bytes > 2 MiB, through the TLS edge + web 11m route) → ${up.status} ${up.status === 201 ? "" : up.text.slice(0, 300)}`);
  need(up.json.file?.sha256 === sha256hex(bytes) && up.json.file?.sizeBytes === bytes.length, `stored object SHA-256 ${up.json.file?.sha256?.slice(0, 16)}… and size match`);
  const d = await admin.download(`${S}/students/${state.studentId}/documents/${doc.id}/file`);
  need(d.status === 200 && sha256hex(d.bytes) === sha256hex(bytes), `downloaded back byte for byte (${d.bytes.length} bytes, MinIO over TLS with the internal CA, SSE AES256)`);
  state.document = { id: doc.id, sha256: sha256hex(bytes), size: bytes.length };
  save();
}

async function verify() {
  need(!!state.studentId, "state names a seeded student");
  const admin = await login("administrator");
  save();
  const st = await admin.get(`${S}/students/${state.studentId}`);
  need(st.id === state.studentId, `student ${st.studentNumber ?? st.id} still readable`);
  const pays = await admin.get(`${F}/student-accounts/${state.accountId}/payments`);
  const items = pays.items ?? pays;
  const p = items.find((x) => x.id === state.paymentId);
  need(p?.status === "completed" && Number(p.amount) === 30000, `payment ${state.paymentNumber} still completed (₹${p?.amount})`);
  const ch = await admin.get(`${F}/charges/${state.chargeId}`);
  need(Number(ch.outstandingAmount ?? ch.outstanding) === 60000, `charge outstanding ₹${ch.outstandingAmount ?? ch.outstanding} (₹90,000 − ₹30,000 paid)`);
  if (state.document && process.env.ONPREM_SKIP_DOCUMENT !== "1") {
    const d = await admin.download(`${S}/students/${state.studentId}/documents/${state.document.id}/file`);
    need(d.status === 200 && sha256hex(d.bytes) === state.document.sha256, `document ${state.document.id} downloaded, SHA-256 unchanged`);
  }
}

try {
  if (PHASE === "seed") await seed();
  else if (PHASE === "document") await document();
  else if (PHASE === "verify") await verify();
  else throw new Error(`unknown phase ${PHASE}`);
  ok(`phase ${PHASE} passed in ${((Date.now() - t0) / 1000).toFixed(1)} s`);
} catch (e) {
  console.error(e?.stack ?? String(e));
  process.exit(1);
}
