#!/usr/bin/env node
/**
 * Batch 5 (Finance) click-through in a real Chromium — skeleton by the Batch 5 foundation;
 * slices 5A/5B/5C add their steps **only inside their own region** below.
 *
 *   pnpm e2e:finance          (from sutraerp/; not part of `pnpm -r test`)
 *
 * Reuses the Batch 3 harness (`e2e/lib/harness.mjs`): starts the API binary (dev auth, PostgreSQL
 * stores) and the web app in API mode, seeds a fresh, migrated database and walks every Batch 5
 * screen:
 *
 *   seed   an institution taken to `active` through the Batch 2 API; two students in the Batch 3
 *          foundation tables (Aarav linked to the dev student login); the default chart of
 *          accounts, a TUITION fee head, Aarav's finance account and one published ₹1,25,000
 *          tuition charge with its publication journal entry — inserted into the Batch 5
 *          foundation tables exactly like `apps/api/tests/support/b5.rs` (`seedFinance` below);
 *   admin  Finance → every tab;  student  Fees;  faculty  no Finance entry, Fees not available.
 *
 * Screenshots: $E2E_SHOTS (default /workspace/work/out/b5-shots); slices prefix theirs `5a-`,
 * `5b-`, `5c-`. Every console error, page error and failed /api response fails the run.
 * Requires `psql` on PATH. Run with
 * `/workspace/work/tools/run-e2e.sh <log> /workspace/work/out/b5-shots finance <worktree>`.
 */
import { existsSync, mkdirSync, readdirSync, unlinkSync } from "node:fs";
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
  seedStudent,
  shot,
  shotCount,
  SHOTS,
  signIn,
  signOut,
  startApi,
  startWeb,
  step,
  stop,
  TENANT,
  STUDENT_SUBJECT,
  waitFor,
  watch,
} from "./lib/harness.mjs";

/** The default chart of accounts of `sutra_finance::shared::coa::DEFAULT_CHART` (keep in sync). */
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

/** "₹1,25,250.00" / "-₹250.00" → paise (integer), so totals compare exactly. */
function inr(text) {
  const t = String(text ?? "").trim();
  const neg = /^[-−]/.test(t);
  const paise = Math.round(Number(t.replace(/[^0-9.]/g, "")) * 100);
  return neg ? -paise : paise;
}

/** Chart + fee head + Aarav's finance account + one published tuition charge (like tests/support/b5.rs). */
function seedFinance(setup, studentId, studentNumber) {
  for (const [key, code, name, type] of CHART)
    psql(
      `INSERT INTO finance.ledger_account (tenant_id, institution_id, code, name, account_type, normal_balance, system_key) ` +
        `VALUES ('${TENANT}', '${setup.id}', '${code}', '${name}', '${type}', '${NORMAL[type]}', '${key}') RETURNING id`,
    );
  const account = (key) => psql(`SELECT id FROM finance.ledger_account WHERE institution_id = '${setup.id}' AND system_key = '${key}'`);
  const feeHeadId = psql(
    `INSERT INTO finance.fee_head (tenant_id, institution_id, code, name, category, income_account_id) ` +
      `VALUES ('${TENANT}', '${setup.id}', 'TUITION', 'Tuition fee', 'tuition', '${account("fee_income")}') RETURNING id`,
  );
  const studentAccountId = psql(
    `INSERT INTO finance.student_account (tenant_id, institution_id, party_kind, student_id, student_number, display_name, subject_ref) ` +
      `VALUES ('${TENANT}', '${setup.id}', 'student', '${studentId}', '${studentNumber}', 'Aarav Desai', '${STUDENT_SUBJECT}') RETURNING id`,
  );
  const chargeId = psql(
    `INSERT INTO finance.charge (tenant_id, institution_id, student_account_id, student_id, charge_number, academic_year_id, ` +
      `academic_term_id, description, total_amount, due_date, source_type, status, published_at) VALUES ('${TENANT}', '${setup.id}', ` +
      `'${studentAccountId}', '${studentId}', 'CHG-000001', '${setup.yearId}', '${setup.termIds[0]}', 'Tuition fee — term 1', ` +
      `125000.00, DATE '2026-07-15', 'manual', 'published', now()) RETURNING id`,
  );
  psql(
    `INSERT INTO finance.charge_line (tenant_id, charge_id, line_no, fee_head_id, description, amount, income_account_id) ` +
      `VALUES ('${TENANT}', '${chargeId}', 1, '${feeHeadId}', 'Tuition', 125000.00, '${account("fee_income")}') RETURNING id`,
  );
  psql(`INSERT INTO finance.number_sequence (tenant_id, institution_id, kind, next_value) VALUES ('${TENANT}', '${setup.id}', 'charge', 2), ('${TENANT}', '${setup.id}', 'journal', 2) RETURNING kind`);
  // Publication entry (Dr receivables / Cr fee income) in one transaction: the balance trigger is deferred to commit.
  psql(
    `WITH e AS (INSERT INTO finance.journal_entry (tenant_id, institution_id, entry_number, posting_date, kind, source_type, source_id, ` +
      `posting_key, description, total_debit, total_credit) VALUES ('${TENANT}', '${setup.id}', 'JE-000001', DATE '2026-07-01', 'standard', ` +
      `'charge', '${chargeId}', 'publish', 'Charge CHG-000001 published', 125000.00, 125000.00) RETURNING id) ` +
      `INSERT INTO finance.journal_line (tenant_id, journal_entry_id, line_no, ledger_account_id, debit, credit, student_account_id, charge_id) ` +
      `SELECT '${TENANT}'::uuid, e.id, 1, '${account("student_receivables")}'::uuid, 125000.00, 0, '${studentAccountId}'::uuid, '${chargeId}'::uuid FROM e ` +
      `UNION ALL SELECT '${TENANT}'::uuid, e.id, 2, '${account("fee_income")}'::uuid, 0, 125000.00, NULL::uuid, NULL::uuid FROM e RETURNING journal_entry_id`,
  );
  return { feeHeadId, studentAccountId, chargeId };
}

/** Opens `route` and checks its frame and the owning slice's placeholder or real screen. */
async function expectFrame(page, route, screenKey, slice) {
  await go(page, route);
  await page.locator(`[data-screen="${screenKey}"]`).first().waitFor();
  const marker = await page.locator(`[data-placeholder="${slice}"], [data-slice="${slice}"]`).count();
  check(marker > 0, `${route} renders slice ${slice}`);
}

// ---------------------------------------------------------------- run
let api;
let web;
let browser;
const ids = {};
try {
  if (!existsSync(apiBin)) throw new Error(`API binary not found at ${apiBin} — run \`cargo build -p sutra-api\` or set SUTRA_API_BIN`);
  mkdirSync(SHOTS, { recursive: true });
  for (const f of readdirSync(SHOTS)) if (/^(\d\d|fail)-.*\.png$/.test(f)) unlinkSync(path.join(SHOTS, f));
  const existing = psql("SELECT count(*) FROM finance.charge");
  if (existing !== "0") throw new Error(`DATABASE_URL must be a fresh migrated database (found ${existing} charges)`);

  log(`API (dev auth), web on ${BASE}, shots → ${SHOTS}`);
  api = await startApi();
  web = startWeb();

  log("— seed: institution to activation through the Batch 2 API");
  const admin = await apiLogin("admin@sutra.dev");
  const setup = await seedInstitution(admin);
  Object.assign(ids, setup);
  ids.aarav = seedStudent(setup, "Aarav", "Desai", "S2026-0001");
  psql(`UPDATE student.student SET subject_ref = '${STUDENT_SUBJECT}' WHERE id = '${ids.aarav}'`);
  ids.isha = seedStudent(setup, "Isha", "Kulkarni", "S2026-0002");
  Object.assign(ids, seedFinance(setup, ids.aarav, "S2026-0001"));
  log(`  ✓ institution ${setup.id}, Aarav's finance account ${ids.studentAccountId}, charge CHG-000001 ₹1,25,000 published`);

  await waitFor(`${BASE}/sign-in`, web);
  browser = await chromium.launch({ headless: !process.env.E2E_HEADED, executablePath: process.env.CHROMIUM_PATH || undefined, args: ["--no-sandbox", ...(process.env.E2E_CHROMIUM_ARGS ?? "").split(" ").filter(Boolean)] });
  const context = await browser.newContext({ viewport: { width: 1280, height: 800 }, locale: "en-IN", timezoneId: "Asia/Kolkata" });
  const page = await context.newPage();
  page.setDefaultTimeout(15000);
  watch(page);
  page.setDefaultNavigationTimeout(180000);
  for (const p of ["/sign-in", "/sign-in"]) await go(page, p);

  // ------------------------------------------------------------ admin
  await signIn(page, "administrator");
  for (const p of ["/finance", "/finance/collections", "/finance/ledger"]) await go(page, p);
  page.setDefaultNavigationTimeout(30000);
  newProblems();

  await step(page, "foundation: admin Finance frame and tabs", async () => {
    for (const [tab, slice] of [
      ["charges", "5A"],
      ["adjustments", "5A"],
      ["aid", "5A"],
      ["collections", "5B"],
      ["refunds", "5B"],
      ["receivables", "5B"],
      ["ledger", "5C"],
      ["periods", "5C"],
      ["treasury", "5C"],
    ])
      await expectFrame(page, `/finance/${tab}`, "finance", slice);
    await go(page, "/");
    check((await page.locator('a[href="/finance"]').count()) > 0, "admin navigation links Finance");
    await go(page, "/finance");
    await shot(page, "foundation-admin-finance");
  });

  // ==== 5A begin (admin: fee heads, fee structures, charges, instalments, disputes, adjustments, aid, configuration)
  {
    const { fillForm: fillDialogForm, row, rowAction, tile } = await import("./lib/harness.mjs");
    // Every 5A form is a `useAsk` dialog: wait until it has closed before the next action, so the next
    // dialog mounts fresh instead of racing the previous one's close (flaky on fast runners).
    const fillForm = async (p, name, values) => {
      await fillDialogForm(p, name, values);
      await p.locator(`[data-form="${name}"]`).waitFor({ state: "detached", timeout: 15000 });
    };
    await step(page, "5A: fee head and fee structure → approval → publication → charges", async () => {
      await go(page, "/finance/charges");
      await page.locator('[data-slice="5A"]').first().waitFor();
      await page.click('[data-action="new-fee-head"]');
      await fillForm(page, "fee-head", { code: "HOSTEL", name: "Hostel fee", category: "hostel" });
      await page.locator('[data-fee-head="HOSTEL"]').waitFor();
      await page.click('[data-action="new-fee-structure"]');
      await fillForm(page, "fee-structure", { code: "BTECH-Y1", name: "B.Tech year 1", dueDate: "2026-08-15", feeHeadId: "TUITION", amount: "90000.00" });
      await page.locator('[data-fee-structure="BTECH-Y1-v1"]').waitFor();
      await rowAction(page, "BTECH-Y1 v1", "submit-structure", tile(page, "Fee structures"));
      await rowAction(page, "BTECH-Y1 v1", "publish-structure", tile(page, "Fee structures"));
      await rowAction(page, "BTECH-Y1 v1", "generate-charges", tile(page, "Fee structures"));
      await fillForm(page, "generate", { studentIds: ids.aarav });
      await page.locator('[data-charge="CHG-000002"]').waitFor();
      check((await page.locator('[data-outstanding="CHG-000002"]').innerText()).includes("₹90,000.00"), "CHG-000002 published for ₹90,000.00");
      await page.click('[data-charge="CHG-000002"]');
      await page.locator('[data-charge-detail="CHG-000002"]').waitFor();
      await page.click('[data-action="set-instalments"]');
      await fillForm(page, "instalments", { count: "3", firstDueDate: "2026-08-15" });
      await page.locator("[data-instalments] li").nth(2).waitFor();
      check((await page.locator("[data-instalments]").innerText()).includes("₹30,000.00"), "three instalments of ₹30,000.00");
      await shot(page, "5a-admin-fees-and-charges");
    });
    await step(page, "5A: finance configuration version → approval → effective", async () => {
      await page.click('[data-action="new-config"]');
      await fillForm(page, "config", { defaultDueDays: "30", maxInstalments: "6" });
      await rowAction(page, "v1", "submit-config", tile(page, "Finance configuration"));
      await rowAction(page, "v1", "activate-config", tile(page, "Finance configuration"));
      await tile(page, "Finance configuration").getByText("Effective").first().waitFor();
      check(true, "configuration v1 effective");
      await page.locator('[data-policy="config"]').scrollIntoViewIfNeeded();
      await shot(page, "5a-admin-finance-configuration");
    });
    await step(page, "5A: credit note on a charge → submitted → posted", async () => {
      await go(page, "/finance/adjustments");
      await page.locator("[data-adjustments]").waitFor();
      await page.click('[data-action="new-adjustment"]');
      await fillForm(page, "adjustment", { chargeId: "CHG-000002", kind: "credit_note", amount: "5000.00", reason: "Merit concession" });
      await rowAction(page, "Merit concession", "submit-adjustment", tile(page, "Adjustments"));
      await row(page, "Merit concession").getByText("Posted").waitFor();
      check(true, "credit note posted");
      await shot(page, "5a-admin-adjustments");
    });
    await step(page, "5A: handed-off scholarship decision → aid allocation → applied to the charge", async () => {
      const ctx = psql(`SELECT id FROM student.student_context WHERE student_id = '${ids.aarav}'`);
      const sch = psql(
        `INSERT INTO student.scholarship (tenant_id, institution_id, code, name, sponsor_type, benefit_type, benefit_value, valid_from, status) ` +
          `VALUES ('${TENANT}', '${ids.id}', 'MERIT', 'Merit scholarship', 'institutional', 'fixed_amount', 20000.00, DATE '2026-06-01', 'active') RETURNING id`,
      );
      psql(
        `INSERT INTO student.scholarship_eligibility (tenant_id, institution_id, scholarship_id, student_id, student_context_id, status, valid_from, valid_until, handed_off_at) ` +
          `VALUES ('${TENANT}', '${ids.id}', '${sch}', '${ids.aarav}', '${ctx}', 'active', DATE '2026-06-01', DATE '2027-05-31', now()) RETURNING id`,
      );
      await go(page, "/finance/aid");
      await page.locator('[data-aid-decision="MERIT"]').waitFor();
      await rowAction(page, "MERIT", "allocate-aid", tile(page, "Handed-off scholarship decisions"));
      await page.locator('[data-aid-allocation="AID-000001"]').waitFor();
      await rowAction(page, "AID-000001", "apply-aid", tile(page, "Aid allocations"));
      await fillForm(page, "apply-aid", { chargeId: "CHG-000002" });
      await page.locator('[data-aid-status="AID-000001"]').getByText("Disbursed").waitFor();
      await page.click('[data-action="reconcile-aid"]');
      await page.locator('[data-reconciliation="matched"]').first().waitFor();
      check(true, "aid applied and the programme reconciliation matches the ledger");
      await shot(page, "5a-admin-financial-aid");
    });
  }
  // ==== 5A end
  // ==== 5B begin (admin: collections, receipts, unidentified receipts, refunds, receivables / dunning)
  {
    const h5b = await import("./lib/harness.mjs");
    await step(page, "5b: counter payment → receipt (allocation + posting)", async () => {
      await go(page, "/finance/collections");
      await page.locator("[data-record-payment]").click();
      await h5b.fillForm(page, "record-payment", { accountId: "Aarav", channel: "cash", amount: "30000.00", externalRef: "CTR-5B-001" });
      await page.locator('[data-payment="PAY-000001"]').waitFor();
      check((await h5b.row(page, "PAY-000001").textContent()).includes("RCT-000001"), "receipt RCT-000001 issued");
      await shot(page, "5b-collections-payment");
      await h5b.rowAction(page, "PAY-000001", "payment-receipt");
      await page.locator('[data-receipt="RCT-000001"]').waitFor();
      check((await page.locator("[data-receipt-amount]").textContent()) === "₹30,000.00", "receipt shows ₹30,000.00");
      await shot(page, "5b-receipt");
      await page.locator('[data-receipt="RCT-000001"]').getByRole("button", { name: "Close" }).click();
    });
    await step(page, "5b: unidentified receipt identified (credit kept unapplied)", async () => {
      await page.locator("[data-record-unidentified]").click();
      await h5b.fillForm(page, "record-unidentified", { channel: "bank_transfer", amount: "5000.00", externalRef: "UTR-5B-777" });
      await h5b.rowAction(page, "UTR-5B-777", "unidentified-identify");
      await h5b.fillForm(page, "identify", { accountId: "Aarav" });
      await h5b.row(page, "UTR-5B-777").getByText("Identified").waitFor();
      await shot(page, "5b-unidentified-identified");
    });
    await step(page, "5b: receivables evaluated, overdue reminder sent", async () => {
      await go(page, "/finance/receivables");
      await page.locator("[data-evaluate]").click();
      await h5b.settle(page);
      await page.locator('[data-receivable="CHG-000001"]').waitFor();
      check((await h5b.row(page, "CHG-000001").textContent()).includes("Overdue"), "CHG-000001 is overdue");
      await h5b.rowAction(page, "CHG-000001", "receivable-dunning");
      await h5b.row(page, "CHG-000001").getByText("1 · Notice sent").waitFor();
      await shot(page, "5b-receivables-dunning");
    });
    await step(page, "5b: refund approved (reference approval), paid out and confirmed", async () => {
      await go(page, "/finance/refunds");
      await page.locator("[data-request-refund]").click();
      await h5b.fillForm(page, "request-refund", { accountId: "Aarav", amount: "2000.00", reason: "Overpaid by bank transfer" });
      await page.locator('[data-refund="RFD-000001"]').waitFor();
      check((await h5b.row(page, "RFD-000001").textContent()).includes("Approved"), "RFD-000001 auto-approved");
      await h5b.rowAction(page, "RFD-000001", "refund-process");
      await h5b.rowAction(page, "RFD-000001", "refund-complete");
      await h5b.row(page, "RFD-000001").getByText("Completed").waitFor();
      await shot(page, "5b-refunds");
    });
  }
  // ==== 5B end
  // ==== 5C begin (admin: chart of accounts, journals, manual journal, trial balance, periods, bank & treasury)
  {
    const h5c = await import("./lib/harness.mjs");
    await step(page, "5c: chart of accounts, manual journal auto-approved and posted", async () => {
      await go(page, "/finance/ledger");
      await page.locator('[data-account="1300"]').waitFor();
      await page.locator("[data-new-journal]").click();
      await h5c.fillForm(page, "manual-journal", {
        postingDate: "2026-07-20",
        description: "Bank charges for July",
        debit: "5900",
        credit: "1200",
        amount: "250.00",
        memo: "BANK-CHG-JUL",
      });
      await page.locator('[data-journal="MJ-000001"]').waitFor();
      await h5c.rowAction(page, "MJ-000001", "journal-submit");
      await h5c.expectRowStatus(page, "MJ-000001", "Posted");
      await shot(page, "5c-ledger-manual-journal");
    });
    await step(page, "5c: trial balance balances, receivables reconcile", async () => {
      await go(page, "/finance/ledger");
      await page.locator("[data-trial-balance]").getByText("Balanced").first().waitFor();
      // The merged flow carries 5A/5B postings into the ledger, so the totals are not fixed — they must agree.
      const tbDebit = await page.locator("[data-tb-debit]").textContent();
      const tbCredit = await page.locator("[data-tb-credit]").textContent();
      check(inr(tbDebit) > 0 && inr(tbDebit) === inr(tbCredit), `trial balance debits ${tbDebit} = credits ${tbCredit}`);
      await page.locator("[data-run-reconciliation]").click();
      await h5c.settle(page);
      await page.locator('[data-reconciliation-status="matched"]').waitFor();
      await shot(page, "5c-ledger-trial-balance");
    });
    await step(page, "5c: bank account, statement import and reconciliation", async () => {
      await go(page, "/finance/treasury");
      await page.locator("[data-new-bank-account]").click();
      await h5c.fillForm(page, "bank-account", { name: "Main collections", bankName: "HDFC Bank", accountNumberLast4: "1234", ifsc: "HDFC0001234" });
      await h5c.expectRowStatus(page, "Main collections", "Active");
      await page.locator("[data-import-statement]").click();
      await h5c.fillForm(page, "statement-import", {
        statementRef: "JUL-2026",
        periodFrom: "2026-07-01",
        periodTo: "2026-07-31",
        closingBalance: "-250.00",
        content: '{"valueDate":"2026-07-20","direction":"debit","amount":"250.00","reference":"BANK-CHG-JUL"}',
      });
      await page.locator('[data-statement="JUL-2026"]').waitFor();
      await h5c.rowAction(page, "JUL-2026", "statement-reconcile");
      await h5c.expectRowStatus(page, "JUL-2026", "Reconciled");
      await h5c.rowAction(page, "JUL-2026", "statement-view");
      await page.locator('[data-line-match="matched"]').first().waitFor();
      // Merged flow: bank 1200 holds the identified ₹5,000 − the ₹2,000 refund paid out − the ₹250 bank charge;
      // 1100 cash on hand holds 5B's ₹30,000 counter payment. Available is bank + cash.
      const cashRows = await page.locator("[data-cash-position] li").allInnerTexts();
      const bankRow = cashRows.find((r) => r.startsWith("1200 ")) ?? "";
      check(bankRow.endsWith("₹2,750.00"), `cash position shows the bank charge on 1200 (${bankRow})`);
      const avail = await page.locator("[data-cash-available]").textContent();
      const banksAndCash = cashRows.filter((r) => /^(1200|1100) /.test(r)).reduce((t, r) => t + inr(r.slice(r.lastIndexOf(":") + 1)), 0);
      check(inr(avail) === banksAndCash, `available ${avail} = bank + cash on hand`);
      await shot(page, "5c-treasury-reconciled");
    });
  }
  // ==== 5C end

  // ------------------------------------------------------------ student
  await signOut(page);
  await signIn(page, "student");
  await step(page, "foundation: student Fees", async () => {
    await expectFrame(page, "/fees", "fees", "5A");
    check((await page.locator('[data-placeholder="5B"], [data-slice="5B"]').count()) > 0, "/fees renders slice 5B");
    await go(page, "/");
    check((await page.locator('a[href="/fees"]').count()) > 0, "student navigation links Fees");
    await go(page, "/fees");
    await shot(page, "foundation-student-fees");
  });
  // ==== 5A student begin (charges, instalments, concessions / aid)
  await step(page, "5A: student sees charges, instalments, concessions and aid", async () => {
    await go(page, "/fees");
    await page.locator('[data-my-charge="CHG-000002"]').waitFor();
    const due = await page.locator('[data-my-outstanding="CHG-000002"]').innerText();
    check(due.includes("₹65,000.00"), `CHG-000002 due ₹65,000.00 after the credit note and aid (${due})`);
    check((await page.locator('[data-my-charge="CHG-000002"] [data-my-instalments] li').count()) === 3, "three instalments listed");
    check((await page.locator('[data-my-aid="list"]').innerText()).includes("Merit scholarship"), "aid listed");
    await shot(page, "5a-student-fees");
  });
  // ==== 5A student end
  // ==== 5B student begin (pay with the mock gateway, receipts, refund request)
  {
    const h5b = await import("./lib/harness.mjs");
    await step(page, "5b student: pay with the mock gateway, receipt, refund request", async () => {
      await go(page, "/fees");
      await page.locator("[data-student-payments] [data-outstanding]").waitFor();
      // Integrated flow: CHG-000001 ₹1,25,000 − counter ₹30,000 = ₹95,000, plus 5A's CHG-000002 ₹90,000 − credit
      // note − aid = ₹65,000 (the identified ₹5,000 stays as unapplied credit, ₹2,000 of it refunded).
      check((await page.locator("[data-outstanding]").textContent()) === "₹1,60,000.00", "outstanding ₹1,60,000.00 after the counter payment");
      await page.locator('[data-form="pay"] input[name="amount"]').fill("10000.00");
      await page.locator('[data-action="pay-now"]').click();
      await h5b.settle(page);
      await page.locator("[data-checkout]").waitFor();
      await shot(page, "5b-student-checkout");
      await page.locator('[data-action="checkout-pay"]').click();
      await h5b.settle(page);
      await page.locator("[data-my-receipts]").getByText("RCT-000003").waitFor();
      check((await page.locator("[data-outstanding]").textContent()) === "₹1,50,000.00", "outstanding ₹1,50,000.00 after paying online");
      await page.locator("[data-my-receipts]").getByText("RCT-000003").click();
      await page.locator('[data-receipt="RCT-000003"]').waitFor();
      await shot(page, "5b-student-receipt");
      await page.locator('[data-receipt="RCT-000003"]').getByRole("button", { name: "Close" }).click();
      await page.locator('[data-action="request-refund"]').click();
      await h5b.fillForm(page, "my-refund", { amount: "1000.00", reason: "Paid twice at the bank" });
      await page.locator("[data-my-refunds]").getByText("RFD-000002").waitFor();
      await shot(page, "5b-student-fees");
    });
  }
  // ==== 5B student end

  // ------------------------------------------------------------ faculty
  await signOut(page);
  await signIn(page, "faculty");
  await step(page, "foundation: faculty has no Finance screens", async () => {
    await go(page, "/");
    check((await page.locator('a[href="/finance"]').count()) === 0, "faculty navigation has no Finance link");
    await go(page, "/fees");
    await page.getByText("This screen isn’t available").first().waitFor();
    await shot(page, "foundation-faculty-fees-unavailable");
  });

  // ==== 5C closing begin (admin: the ledger after the day — trial balance balances, period close)
  {
    const h5c = await import("./lib/harness.mjs");
    await signOut(page);
    await signIn(page, "administrator");
    // Established only after the student's online payment: once the year is open, a period that is merely
    // planned (today's month) refuses postings, so the year/period lifecycle runs last and closes only July.
    await step(page, "5c: financial year established, July opened", async () => {
      await go(page, "/finance/periods");
      await page.locator("[data-new-year]").click();
      await h5c.fillForm(page, "financial-year", { code: "FY2026-27", name: "Financial year 2026-27", startDate: "2026-04-01", endDate: "2027-03-31" });
      await page.locator('[data-year="FY2026-27"]').waitFor();
      await h5c.rowAction(page, "FY2026-27", "year-open");
      await h5c.expectRowStatus(page, "FY2026-27", "Open");
      await h5c.rowAction(page, "Jul 2026", "period-open");
      await h5c.expectRowStatus(page, "Jul 2026", "Accepted");
      await shot(page, "5c-periods-open");
    });
    await step(page, "5c closing: July closed — postings refused, trial balance still balances", async () => {
      await go(page, "/finance/periods");
      await h5c.rowAction(page, "Jul 2026", "period-close");
      await h5c.expectRowStatus(page, "Jul 2026", "Closed");
      await h5c.expectRowStatus(page, "Jul 2026", "Refused");
      await shot(page, "5c-periods-july-closed");
      await go(page, "/finance/ledger");
      await page.locator("[data-trial-balance]").getByText("Balanced").first().waitFor();
      await shot(page, "5c-closing-trial-balance");
    });
  }
  // ==== 5C closing end

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
