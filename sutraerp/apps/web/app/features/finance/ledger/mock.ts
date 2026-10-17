/**
 * In-memory 5C API for mock mode and unit tests. Money is kept in paise (`bigint`) and shown as
 * decimal strings — never floats. Manual journals auto-approve and post (Batch 5 default);
 * statements reconcile every line whose amount equals a ledger movement of the bank account.
 */
import { ApiProblem } from "@sutra/api-client";
import type { LedgerApi } from "./api";
import type { BankAccount, FinancialYear, JournalEntry, LedgerAccount, ManualJournal, Period, Reconciliation, Statement } from "./types";

export const toPaise = (s: string): bigint => {
  const neg = s.startsWith("-");
  const [i = "0", f = ""] = s.replace(/^[-+]/, "").split(".");
  const v = BigInt(i || "0") * 100n + BigInt((f + "00").slice(0, 2));
  return neg ? -v : v;
};
export const fromPaise = (p: bigint): string => {
  const neg = p < 0n;
  const a = neg ? -p : p;
  return `${neg ? "-" : ""}${a / 100n}.${String(a % 100n).padStart(2, "0")}`;
};

const conflict = (detail: string) => new ApiProblem({ type: "about:blank", title: "Conflict", status: 409, errorCode: "conflict", detail });

const CHART: [string, string, string, string][] = [
  ["1100", "Cash on hand", "asset", "cash_on_hand"],
  ["1200", "Bank — main collection account", "asset", "bank_main"],
  ["1250", "Payment gateway clearing", "asset", "gateway_clearing"],
  ["1300", "Student fee receivables", "asset", "student_receivables"],
  ["2100", "Unapplied receipts and student advances", "liability", "unapplied_receipts"],
  ["4100", "Tuition and academic fee income", "income", "fee_income"],
  ["5900", "Bank and gateway charges", "expense", "bank_charges"],
];

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

export function createMockLedgerApi(): LedgerApi {
  let seq = 0;
  const nid = (p: string) => `${p}-${(++seq).toString().padStart(4, "0")}-mock`;
  const accounts = new Map<string, LedgerAccount[]>();
  const entries: (JournalEntry & { inst: string })[] = [];
  const journals: (ManualJournal & { inst: string })[] = [];
  const years: (FinancialYear & { inst: string })[] = [];
  const banks: (BankAccount & { inst: string })[] = [];
  const statements: Statement[] = [];
  const recs: (Reconciliation & { inst: string })[] = [];
  const chart = (inst: string) => {
    let c = accounts.get(inst);
    if (!c) {
      c = CHART.map(([code, name, accountType, systemKey]) => ({
        id: nid("acc"), code, name, accountType, normalBalance: accountType === "asset" || accountType === "expense" ? "debit" : "credit",
        systemKey, isPostable: true, status: "active", debitTotal: "0.00", creditTotal: "0.00", balance: "0.00", postingCount: 0, version: 1,
      }));
      accounts.set(inst, c);
    }
    return c;
  };
  const post = (inst: string, j: ManualJournal) => {
    const c = chart(inst);
    for (const l of j.lines) {
      const a = c.find((x) => x.id === l.ledgerAccountId);
      if (!a) continue;
      const dr = toPaise(a.debitTotal) + toPaise(l.debit);
      const cr = toPaise(a.creditTotal) + toPaise(l.credit);
      Object.assign(a, { debitTotal: fromPaise(dr), creditTotal: fromPaise(cr), balance: fromPaise(a.normalBalance === "debit" ? dr - cr : cr - dr), postingCount: a.postingCount + 1 });
    }
    const e: JournalEntry & { inst: string } = {
      inst, id: nid("je"), entryNumber: `JE-${String(entries.length + 1).padStart(6, "0")}`, postingDate: j.postingDate, kind: j.entryKind, sourceType: "manual_journal",
      sourceId: j.id, description: `Manual journal ${j.journalNumber}: ${j.description}`, totalDebit: j.totalAmount, totalCredit: j.totalAmount, lines: j.lines,
    };
    entries.unshift(e);
    return e;
  };
  const periodOf = (id: string): Period => {
    for (const y of years) for (const p of y.periods) if (p.id === id) return p;
    throw conflict("unknown period");
  };
  const tb = (inst: string, asOf: string) => {
    const rows = chart(inst).filter((a) => a.postingCount > 0).map((a) => ({ ledgerAccountId: a.id, code: a.code, name: a.name, accountType: a.accountType, debit: a.debitTotal, credit: a.creditTotal, balance: a.balance }));
    const d = rows.reduce((s, r) => s + toPaise(r.debit), 0n);
    const c = rows.reduce((s, r) => s + toPaise(r.credit), 0n);
    return { asOf, rows, totalDebit: fromPaise(d), totalCredit: fromPaise(c), balanced: d === c };
  };
  return {
    accounts: async (inst) => chart(inst),
    createAccount: async (inst, input) => {
      const c = chart(inst);
      if (c.some((a) => a.code === input.code)) throw conflict(`ledger account code ${input.code} already exists`);
      const a: LedgerAccount = { id: nid("acc"), ...input, normalBalance: input.accountType === "asset" || input.accountType === "expense" ? "debit" : "credit", systemKey: null, isPostable: true, status: "active", debitTotal: "0.00", creditTotal: "0.00", balance: "0.00", postingCount: 0, version: 1 };
      c.push(a);
      c.sort((x, y) => x.code.localeCompare(y.code));
      return a;
    },
    entries: async (inst) => ({ items: entries.filter((e) => e.inst === inst), nextCursor: null }),
    reverseEntry: async (id) => {
      const e = entries.find((x) => x.id === id);
      if (!e || e.reversedByEntryId) throw conflict("entry cannot be reversed");
      const j = journals.find((x) => x.id === e.sourceId)!;
      const mirror = post(e.inst, { ...j, lines: j.lines.map((l) => ({ ...l, debit: l.credit, credit: l.debit })) });
      Object.assign(mirror, { kind: "reversal", reversesEntryId: e.id });
      e.reversedByEntryId = mirror.id;
      Object.assign(j, { status: "reversed", reversalEntryId: mirror.id, version: j.version + 1 });
      return mirror;
    },
    journals: async (inst) => journals.filter((j) => j.inst === inst),
    createJournal: async (inst, input) => {
      const c = chart(inst);
      const lines = input.lines.map((l, i) => {
        const a = c.find((x) => x.id === l.ledgerAccountId)!;
        return { lineNo: i + 1, ledgerAccountId: a.id, accountCode: a.code, accountName: a.name, debit: fromPaise(toPaise(l.debit ?? "0")), credit: fromPaise(toPaise(l.credit ?? "0")), memo: l.memo ?? null };
      });
      const d = lines.reduce((s, l) => s + toPaise(l.debit), 0n);
      const cr = lines.reduce((s, l) => s + toPaise(l.credit), 0n);
      if (d !== cr || d === 0n) throw conflict("the journal is unbalanced");
      const j: ManualJournal & { inst: string } = {
        inst, id: nid("mj"), journalNumber: `MJ-${String(journals.length + 1).padStart(6, "0")}`, postingDate: input.postingDate, entryKind: input.entryKind ?? "manual",
        description: input.description, totalAmount: fromPaise(d), status: "draft", lines, history: [{ toStatus: "draft", at: new Date().toISOString() }], version: 1,
      };
      journals.unshift(j);
      return j;
    },
    submitJournal: async (id) => {
      const j = journals.find((x) => x.id === id)!;
      if (j.status !== "draft") throw conflict(`manual journal is ${j.status}`);
      const e = post(j.inst, j);
      Object.assign(j, { status: "posted", journalEntryId: e.id, decidedBy: "system:auto-approval", version: j.version + 4 });
      return j;
    },
    decideJournal: async (id, _v, approve, reason) => {
      const j = journals.find((x) => x.id === id)!;
      if (approve) Object.assign(j, { status: "posted", journalEntryId: post(j.inst, j).id });
      else Object.assign(j, { status: "rejected", history: [...j.history, { toStatus: "rejected", reason, at: new Date().toISOString() }] });
      j.version += 1;
      return j;
    },
    trialBalance: async (inst, asOf) => tb(inst, asOf ?? new Date().toISOString().slice(0, 10)),
    financialStatement: async (_inst, kind) => ({ id: kind, title: kind, subtitle: "", columns: [{ key: "total", label: "Total" }], rows: [], net: "0.00", balanced: kind === "balance-sheet" ? true : null, reconciled: null }),
    reconciliations: async (inst) => recs.filter((r) => r.inst === inst),
    reconcile: async (inst) => {
      const r = { inst, id: nid("rec"), scope: "receivables", asOf: new Date().toISOString().slice(0, 10), status: "matched", sourceTotal: "0.00", ledgerTotal: "0.00", difference: "0.00", exceptions: [], createdAt: new Date().toISOString() };
      recs.unshift(r);
      return r;
    },
    years: async (inst) => years.filter((y) => y.inst === inst),
    createYear: async (inst, input) => {
      if (years.some((y) => y.inst === inst && y.code === input.code.toUpperCase())) throw conflict(`financial year ${input.code} already exists`);
      const id = nid("fy");
      const periods: Period[] = [];
      const [sy, sm] = input.startDate.split("-").map(Number) as [number, number];
      for (let i = 0; i < 12; i++) {
        const m = ((sm - 1 + i) % 12) + 1;
        const y = sy + Math.floor((sm - 1 + i) / 12);
        const last = new Date(Date.UTC(y, m, 0)).getUTCDate();
        const mm = String(m).padStart(2, "0");
        periods.push({ id: nid("fp"), financialYearId: id, periodNo: i + 1, name: `${MONTHS[m - 1]} ${y}`, startDate: `${y}-${mm}-01`, endDate: `${y}-${mm}-${last}`, status: "planned", acceptsPostings: false, reopenCount: 0, entryCount: 0, history: [], version: 1 });
      }
      const fy = { inst, id, code: input.code.toUpperCase(), name: input.name, startDate: input.startDate, endDate: input.endDate, status: "draft", periods, version: 1 };
      years.unshift(fy);
      return fy;
    },
    yearCommand: async (id, _v, cmd) => {
      const y = years.find((x) => x.id === id)!;
      if (cmd === "close" && y.periods.some((p) => p.status !== "closed" && p.status !== "archived")) throw conflict("close every period first");
      Object.assign(y, { status: cmd === "open" ? "open" : cmd === "close" ? "closed" : "archived", version: y.version + 1 });
      return y;
    },
    periodCommand: async (id, _v, cmd) => {
      const p = periodOf(id);
      const to = { open: "open", lock: "locked", close: "closed", reopen: "reopened", archive: "archived" }[cmd];
      Object.assign(p, { status: to, acceptsPostings: to === "open" || to === "reopened", reopenCount: p.reopenCount + (cmd === "reopen" ? 1 : 0), version: p.version + 1 });
      return p;
    },
    bankAccounts: async (inst) => banks.filter((b) => b.inst === inst),
    createBankAccount: async (inst, input) => {
      const main = chart(inst).find((a) => a.systemKey === "bank_main")!;
      const b = { inst, id: nid("bank"), purpose: "collection", ...input, ledgerAccountId: main.id, ledgerAccountCode: banks.some((x) => x.inst === inst) ? `120${banks.length}` : "1200", status: "active", ledgerBalance: main.balance, unreconciledLines: 0, version: 3 };
      banks.push(b);
      return b;
    },
    transactions: async () => [],
    statements: async (bank) => statements.filter((s) => s.bankAccountId === bank),
    statement: async (id) => statements.find((s) => s.id === id)!,
    previewStatement: async (_bank, input) => {
      const lines = mockFileLines(input.content);
      const sum = (d: string) => fromPaise(lines.filter((l) => l.direction === d).reduce((s, l) => s + toPaise(l.amount), 0n));
      const dates = lines.map((l) => l.valueDate).sort();
      return {
        format: "csv",
        headers: ["Date", "Narration", "Amount"],
        mapping: { date: 0, description: 1, amount: 2 },
        periodFrom: dates[0] ?? "",
        periodTo: dates[dates.length - 1] ?? "",
        totalCredits: sum("credit"),
        totalDebits: sum("debit"),
        newLines: lines.length,
        duplicateLines: 0,
        lines: lines.map((l) => ({ ...l, duplicate: false })),
        skipped: [],
      };
    },
    importStatement: async (bank, input) => {
      const parsed = !input.format || input.format === "jsonl"
        ? input.content.split("\n").filter((l) => l.trim()).map((raw) => JSON.parse(raw) as { valueDate: string; direction: string; amount: string; reference?: string; description?: string })
        : mockFileLines(input.content);
      const lines = parsed.map((o, i) => ({ id: nid("sl"), lineNo: i + 1, valueDate: o.valueDate, direction: o.direction, amount: o.amount, reference: o.reference ?? null, description: o.description ?? null, matchStatus: "unmatched" }));
      const dates = lines.map((l) => l.valueDate).sort();
      input = { ...input, statementRef: input.statementRef || `${dates[0]} to ${dates[dates.length - 1]}`, periodFrom: input.periodFrom || dates[0], periodTo: input.periodTo || dates[dates.length - 1] };
      const sum = (d: string) => fromPaise(lines.filter((l) => l.direction === d).reduce((s, l) => s + toPaise(l.amount), 0n));
      const s: Statement = { id: nid("st"), bankAccountId: bank, statementRef: input.statementRef!, periodFrom: input.periodFrom!, periodTo: input.periodTo!, sourceFormat: input.format && input.format !== "auto" ? input.format : "jsonl", duplicateLines: 0, closingBalance: input.closingBalance ?? null, totalCredits: sum("credit"), totalDebits: sum("debit"), lineCount: lines.length, status: "imported", lines, version: 1 };
      statements.unshift(s);
      return s;
    },
    reconcileStatement: async (id) => {
      const s = statements.find((x) => x.id === id)!;
      for (const l of s.lines) if (entries.some((e) => e.totalDebit === l.amount)) l.matchStatus = "matched";
      s.status = s.lines.every((l) => l.matchStatus !== "unmatched") ? "reconciled" : "reconciling";
      s.version += 1;
      return s;
    },
    cashPosition: async (inst) => {
      const rows = banks.filter((b) => b.inst === inst).map((b) => ({ kind: "bank", bankAccountId: b.id, name: b.name, ledgerAccountCode: b.ledgerAccountCode, balance: chart(inst).find((a) => a.id === b.ledgerAccountId)?.balance ?? "0.00", unreconciledLines: 0 }));
      const total = fromPaise(rows.reduce((s, r) => s + toPaise(r.balance), 0n));
      return { asOf: new Date().toISOString().slice(0, 10), rows, available: total, inTransit: "0.00", total };
    },
  };
}

/** Mock-only reader for `Date,Narration,Amount` CSV with ISO dates (the server does the real parsing). */
function mockFileLines(content: string): { valueDate: string; direction: string; amount: string; reference?: string; description?: string }[] {
  return content
    .split(/\r?\n/)
    .map((r) => r.split(","))
    .filter((c) => /^\d{4}-\d{2}-\d{2}$/.test(c[0]?.trim() ?? "") && c.length >= 3)
    .map((c) => {
      const raw = (c[2] ?? "0").replace(/[^\d.-]/g, "");
      const out = raw.startsWith("-");
      return { valueDate: c[0]!.trim(), direction: out ? "debit" : "credit", amount: fromPaise(toPaise(raw.replace("-", "") || "0")), description: c[1]?.trim() };
    });
}
