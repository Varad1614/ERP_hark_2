#!/usr/bin/env node
/**
 * Part 2 · S4-ui-import — Admin → Data import end-to-end in a real Chromium.
 *
 *   pnpm e2e:imports          (from sutraerp/; CI job `E2E imports (preview)`, not part of `pnpm -r test`)
 *
 * Shared harness (`e2e/lib/harness.mjs`): the API binary (dev auth, runtime DB role under RLS) and
 * the web app in API mode on a fresh, migrated database.
 *
 *   seed     institution → `active` with the BTECH-CSE programme (Batch 2 API).
 *   admin    Data import from the avatar menu; CSV template download; a small students CSV with
 *            one bad row → mapping (suggested) → dry run shows the row errors → error report
 *            download; a corrected CSV → dry run clean → commit → reconciliation (3 created),
 *            the students exist in the database.
 *   admin    Opening fee balances for those students → dry run → "Request approval": the
 *            requester sees no Approve / Reject (maker ≠ checker), on the job and in Approvals.
 *   checker  the finance approver decides through the API (the dev sign-in page has cards for the
 *            three personas only) → the admin sees the job committed and reconciled (file = ledger).
 *   admin    History lists the jobs; Export downloads the students CSV.
 *
 * Screenshots: $E2E_SHOTS (CI: the job's artifact directory). Every console error, page error and
 * failed /api response fails the run. Requires `psql` on PATH.
 */
import { existsSync, mkdirSync, readdirSync, readFileSync, unlinkSync } from "node:fs";
import path from "node:path";
import { chromium } from "playwright";
import {
  apiBin,
  apiLogin,
  BASE,
  check,
  children,
  failed,
  go,
  log,
  newProblems,
  psql,
  seedInstitution,
  settle,
  shot,
  shotCount,
  SHOTS,
  signIn,
  startApi,
  startWeb,
  step,
  stop,
  tile,
  waitFor,
  watch,
} from "./lib/harness.mjs";

const IMP = "/api/v1/platform/imports";
const SUFFIX = Date.now().toString(36).toUpperCase().slice(-5);
const num = (i) => `MIG${SUFFIX}/${i}`;
const HEADER = "student_number,given_name,family_name,date_of_birth,email,programme_code";

/** Clicks `selector` and returns the download it starts. */
async function download(page, selector) {
  const [dl] = await Promise.all([page.waitForEvent("download", { timeout: 15000 }), page.click(selector)]);
  const file = path.join(SHOTS, `dl-${dl.suggestedFilename()}`);
  await dl.saveAs(file);
  // Server CSVs carry a UTF-8 BOM (spreadsheets detect the encoding) and CRLF line ends.
  return { name: dl.suggestedFilename(), text: readFileSync(file, "utf8").replace(/^\uFEFF/, "") };
}

/** Uploads `csv` as `name` for the selected entity and waits for the mapping step. */
async function upload(page, name, csv) {
  await page.locator('input[name="file"]').setInputFiles({ name, mimeType: "text/csv", buffer: Buffer.from(csv) });
  await page.click("[data-import-upload]");
  await page.locator("[data-import-mapping]").waitFor({ timeout: 20000 });
  await settle(page);
}

async function status(page) {
  return page.locator("[data-import-job]").getAttribute("data-import-status");
}

// ---------------------------------------------------------------- run
let api;
let web;
let browser;
try {
  if (!existsSync(apiBin)) throw new Error(`API binary not found at ${apiBin} — run \`cargo build -p sutra-api\` or set SUTRA_API_BIN`);
  mkdirSync(SHOTS, { recursive: true });
  for (const f of readdirSync(SHOTS)) if (/^(\d\d|fail|dl)-.*\.(png|csv|xlsx)$/.test(f)) unlinkSync(path.join(SHOTS, f));
  log(`API (dev auth), web on ${BASE}, shots → ${SHOTS}`);
  api = await startApi();
  web = startWeb();

  log("— seed: institution");
  const admin = await apiLogin("admin@sutra.dev");
  const setup = await seedInstitution(admin);
  const inst = setup.id;
  const checker = await apiLogin("finance.approver@sutra.dev");

  await waitFor(`${BASE}/sign-in`, web);
  browser = await chromium.launch({ headless: !process.env.E2E_HEADED, executablePath: process.env.CHROMIUM_PATH || undefined, args: ["--no-sandbox", ...(process.env.E2E_CHROMIUM_ARGS ?? "").split(" ").filter(Boolean)] });
  const context = await browser.newContext({ viewport: { width: 1440, height: 960 }, locale: "en-IN", timezoneId: "Asia/Kolkata", acceptDownloads: true });
  const page = await context.newPage();
  page.setDefaultTimeout(15000);
  watch(page);
  page.setDefaultNavigationTimeout(180000);
  for (const p of ["/sign-in", "/sign-in"]) await go(page, p);
  await signIn(page, "administrator");
  await go(page, "/data-import");
  page.setDefaultNavigationTimeout(30000);
  newProblems();

  await step(page, "admin: Data import from the avatar menu; entity picker + CSV template", async () => {
    await go(page, "/");
    await page.click('[aria-label="Account menu"]');
    await page.click("[data-import-menu]");
    await page.waitForURL(`${BASE}/data-import`);
    await page.locator('[data-screen="data-import"]').waitFor();
    await page.selectOption('select[name="entity"]', "students");
    await page.locator('[data-import-columns="students"]').waitFor();
    check((await page.getByText(/up to 8 MiB/).count()) > 0, "upload limit shown (8 MiB)");
    const t = await download(page, '[data-import-template="csv"]');
    check(t.name === "students-template.csv", `template file name (${t.name})`);
    check(t.text.split(/\r?\n/)[0].includes("student_number"), "template header has student_number");
    await shot(page, "imp-entity-template");
  });

  await step(page, "admin: students CSV with a bad row → mapping → dry run shows the row errors + error report", async () => {
    const bad = [HEADER, `${num(1)},Asha,Kulkarni,2005-01-12,asha@example.org,BTECH-CSE`, `${num(2)},,Patil,2005-04-12,not-an-email,BTECH-CSE`, `${num(3)},Ravi,Joshi,2005-03-02,ravi@example.org,BTECH-CSE`].join("\n");
    await upload(page, "students-bad.csv", `${bad}\n`);
    check((await page.locator('select[name="map:given_name"]').inputValue()) === "given_name", "mapping suggested from the headers");
    await shot(page, "imp-mapping");
    await page.click("[data-import-dry-run]");
    await page.locator('[data-import-dryrun="invalid"]').waitFor({ timeout: 30000 });
    const err = page.locator('[data-import-row-error="3"]');
    await err.waitFor();
    const text = (await err.textContent()) ?? "";
    check(/given_name/i.test(text) && /mail/i.test(text), `line 3 errors listed (${text.slice(0, 200)})`);
    check(await page.locator("[data-import-commit]").isDisabled(), "commit disabled while a row has errors");
    const rep = await download(page, "[data-import-error-csv]");
    check(rep.text.startsWith("line,column,code,message") && /\n3,/.test(rep.text), `error report CSV lists line 3 (${rep.text.slice(0, 120)})`);
    await shot(page, "imp-dryrun-errors");
  });

  await step(page, "admin: corrected CSV → dry run clean → commit → reconciliation", async () => {
    await page.click("[data-import-new]");
    await page.selectOption('select[name="entity"]', "students");
    const good = [HEADER, `${num(1)},Asha,Kulkarni,2005-01-12,asha@example.org,BTECH-CSE`, `${num(2)},Meera,Patil,2005-04-12,meera@example.org,BTECH-CSE`, `${num(3)},Ravi,Joshi,2005-03-02,ravi@example.org,BTECH-CSE`].join("\n");
    await upload(page, "students.csv", `${good}\n`);
    await page.click("[data-import-dry-run]");
    await page.locator('[data-import-dryrun="validated"]').waitFor({ timeout: 30000 });
    await shot(page, "imp-dryrun-clean");
    await page.click("[data-import-commit]");
    await page.locator('[data-import-reconciliation="committed"]').waitFor({ timeout: 30000 });
    const created = await tile(page, "Reconciliation").textContent();
    check(/Created\s*3/.test(created ?? ""), "reconciliation: 3 created");
    const n = psql(`SELECT count(*) FROM student.student WHERE student_number LIKE 'MIG${SUFFIX}/%'`);
    check(n === "3", `3 students in the database (${n})`);
    await shot(page, "imp-committed");
  });

  let balanceJob = "";
  await step(page, "admin: opening balances → request approval; the requester is not offered the decision", async () => {
    await page.click("[data-import-new]");
    await page.selectOption('select[name="entity"]', "opening_balances");
    const csv = ["student_number,amount,due_date,description", `${num(1)},12500.00,2026-12-31,Fees carried over`, `${num(2)},4999.50,,`].join("\n");
    await upload(page, "balances.csv", `${csv}\n`);
    await page.click("[data-import-dry-run]");
    await page.locator('[data-import-dryrun="validated"]').waitFor({ timeout: 30000 });
    check(((await page.locator("[data-import-commit]").textContent()) ?? "").includes("Request approval"), "commit asks for approval");
    await page.click("[data-import-commit]");
    await page.locator("[data-import-pending]").waitFor();
    balanceJob = (await page.locator("[data-import-job]").getAttribute("data-import-job")) ?? "";
    check((await status(page)) === "pending_approval", "job waits for approval");
    check((await page.locator("[data-import-pending] [data-approve]").count()) === 0, "no Approve for the requester");
    check((await page.locator("[data-import-awaiting]").count()) === 1, "waiting note shown");
    const charges = psql(`SELECT count(*) FROM finance.charge WHERE institution_id = '${inst}' AND source_type = 'opening_balance'`);
    check(charges === "0", "nothing posted before approval");
    await shot(page, "imp-pending-requester");
    await go(page, "/data-import/approvals");
    const card = page.locator(`[data-import-approval="${balanceJob}"]`);
    await card.waitFor();
    check((await card.locator("[data-approve]").count()) === 0, "Approvals tab: no Approve on the requester's own job");
    await shot(page, "imp-approvals-requester");
  });

  await step(page, "checker approves (API) → admin sees the import committed and reconciled", async () => {
    const pending = await checker.get(`${IMP}?institutionId=${inst}`);
    check(pending.items.some((j) => j.id === balanceJob), "the finance checker lists the job it decides");
    const decided = await checker.call("POST", `${IMP}/${balanceJob}/decision`, { approve: true, reason: "Matches the legacy ledger" });
    check(decided.status === "committed", `decision → ${decided.status}`);
    await go(page, `/data-import?job=${balanceJob}`);
    await page.locator('[data-import-reconciliation="committed"]').waitFor();
    check((await page.locator('[data-import-reconciled="true"]').count()) === 1, "file total = ledger total (reconciled)");
    const charges = psql(`SELECT count(*) FROM finance.charge WHERE institution_id = '${inst}' AND source_type = 'opening_balance'`);
    check(charges === "2", `two opening balances posted (${charges})`);
    await shot(page, "imp-balances-reconciled");
  });

  await step(page, "admin: History and Export", async () => {
    await go(page, "/data-import/history");
    for (const f of ["students-bad.csv", "students.csv", "balances.csv"]) await page.locator(`[data-import-history-row="${f}"]`).waitFor();
    await shot(page, "imp-history");
    await go(page, "/data-import/export");
    const ex = await download(page, '[data-import-export-csv="students"]');
    check(ex.text.includes(num(2)), "students export contains the imported student");
    await shot(page, "imp-export");
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
