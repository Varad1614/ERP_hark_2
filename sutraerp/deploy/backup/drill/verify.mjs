#!/usr/bin/env node
/**
 * Application-level checks after a restore (Part 2 · Stage 5 backup drill). Runs against the
 * RESTORED compose stack, through the public web origin, as real Keycloak users (the TOTP secrets
 * are the users' authenticators, kept by the operator in E2E_STATE — they survive the server):
 *
 *   1. Keycloak OIDC sign-in (code + PKCE + TOTP) of the administrator, the registrar, the
 *      finance approver and the student — users and credentials came back with the Keycloak DB
 *   2. POST /api/v1/platform/audit/chain/verify → ok (the hash chain of every tenant is intact)
 *   3. DRILL_SAMPLE (default 40) random restored document files downloaded through the API: the
 *      vault re-checks the sha256 on every read (a mismatch would answer 409), and the bytes'
 *      sha256 equals platform.document_object.sha256
 *   4. a new write on restored data: ₹100 counter payment on a drill student account → completed
 *      + receipt, institution ledger still balanced
 * One "ok …" line per check; exit 1 on the first failure. Env: E2E_BASE, E2E_AUTH=oidc, E2E_STATE,
 * DATABASE_URL (superuser; read-only lookups), DRILL_INSTITUTION (id), DRILL_SAMPLE.
 */
/* global process, console */
import { createHash, randomUUID } from "node:crypto";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

process.env.DATABASE_URL ??= "postgres://sutra:sutra@localhost:5432/sutra";
const lib = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../../../apps/web/e2e/lib");
const { login, totpSecrets } = await import(`${lib}/auth.mjs`);
const { psql } = await import(`${lib}/harness.mjs`);

const STATE = process.env.E2E_STATE ?? "/tmp/journey-state.json";
const INST = process.env.DRILL_INSTITUTION ?? "";
const SAMPLE = Number(process.env.DRILL_SAMPLE ?? 40);
const q = (s) => `'${String(s).replace(/'/g, "''")}'`;
const ok = (m) => console.log(`ok ${m}`);
const fail = (m) => {
  console.error(`FAIL ${m}`);
  process.exit(1);
};
if (!existsSync(STATE)) fail(`no ${STATE} (TOTP secrets of the users)`);
const state = JSON.parse(readFileSync(STATE, "utf8"));
for (const [k, v] of Object.entries(state.totp ?? {})) totpSecrets.set(k, v);

// 1. sign-ins
const clients = {};
for (const role of ["administrator", "registrar", "finance_approver", "student"]) {
  try {
    clients[role] = await login(role);
  } catch (e) {
    fail(`Keycloak sign-in ${role}: ${e instanceof Error ? e.message : e}`);
  }
  const c = clients[role];
  if (!c.session.principal.roles.includes(role)) fail(`${role}: session without the ${role} role`);
  ok(`Keycloak sign-in ${role} (${c.login}, subject ${c.subject}, mfa ${c.session.mfaLevel})`);
}
const admin = clients.administrator;
// keep the authenticators' last state (codes are never reused within 30 s)
state.totp = { ...(state.totp ?? {}), ...Object.fromEntries(totpSecrets) };
writeFileSync(STATE, JSON.stringify(state, null, 2));

// 2. audit chain
{
  const r = await admin.raw("POST", "/api/v1/platform/audit/chain/verify", {});
  if (r.status !== 200) fail(`audit chain verify HTTP ${r.status} ${r.text.slice(0, 300)}`);
  if (r.json?.ok !== true) fail(`audit chain verify: ${r.text.slice(0, 500)}`);
  ok(`audit chain verify (${r.text.slice(0, 200).replace(/\s+/g, " ")})`);
}

// 3. sample document downloads (record documents of students: /students/{id}/documents/{doc}/file)
{
  const rows = psql(
    `SELECT sd.student_id || '|' || sd.id || '|' || o.sha256 || '|' || o.size_bytes FROM student.student_document sd ` +
      `JOIN platform.document_object o ON o.id = sd.document_object_id ` +
      `WHERE o.deleted_at IS NULL AND o.quarantined_at IS NULL ${INST ? `AND o.institution_id = ${q(INST)}` : ""} ORDER BY random() LIMIT ${SAMPLE}`,
  )
    .split("\n")
    .filter(Boolean);
  if (rows.length === 0) fail("no restored student documents with files to sample");
  let bytes = 0;
  for (const row of rows) {
    const [sid, did, sha, size] = row.split("|");
    const d = await admin.download(`/api/v1/student/students/${sid}/documents/${did}/file`);
    if (d.status !== 200) fail(`download ${sid}/${did} → ${d.status} ${d.bytes.toString("utf8", 0, 200)}`);
    const got = createHash("sha256").update(d.bytes).digest("hex");
    if (got !== sha || d.bytes.length !== Number(size)) fail(`document ${did}: sha256 ${got} (${d.bytes.length} B) ≠ recorded ${sha} (${size} B)`);
    bytes += d.bytes.length;
  }
  ok(`${rows.length} restored documents downloaded through the API, sha256 = platform.document_object (${bytes} bytes)`);
}

// 4. a write on the restored data
if (INST) {
  const acct = psql(`SELECT student_account_id FROM finance.charge WHERE institution_id = ${q(INST)} AND outstanding_amount > 0 ORDER BY charge_number LIMIT 1`);
  if (!acct) fail("no open charge in the drill institution");
  const before = psql(`SELECT count(*) FROM finance.payment WHERE student_account_id = ${q(acct)} AND status = 'completed'`);
  const p = await admin.call("POST", `/api/v1/finance/student-accounts/${acct}/payments`, { channel: "cash", amount: "100.00", externalRef: `POST-RESTORE-${randomUUID().slice(0, 8)}` });
  if (p.status !== "completed" || !p.receipt?.receiptNumber) fail(`post-restore payment: ${JSON.stringify(p).slice(0, 300)}`);
  const after = psql(`SELECT count(*) FROM finance.payment WHERE student_account_id = ${q(acct)} AND status = 'completed'`);
  const balanced = psql(`SELECT sum(total_debit) = sum(total_credit) FROM finance.journal_entry WHERE institution_id = ${q(INST)}`);
  if (Number(after) !== Number(before) + 1 || balanced !== "t") fail(`post-restore payment: payments ${before}→${after}, balanced ${balanced}`);
  ok(`post-restore write: payment ${p.paymentNumber} completed, receipt ${p.receipt.receiptNumber}, ledger balanced`);
}
process.exit(0);
