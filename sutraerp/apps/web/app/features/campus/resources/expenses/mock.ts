/** In-memory `ExpensesApi` for mock mode and screen tests (a simplified policy, not the rules). */
import type { ExpensesApi } from "./api";
import { isBlocking } from "./expenses";
import type { ClaimStatus, ExpenseClaimSummary, ExpenseClaimView, ExpenseInput, ExpenseReceiptView, ExpenseSettings, ExpenseView, OcrFields } from "./types";

const DAY = 86_400_000;
const iso = (ms: number) => new Date(ms).toISOString().slice(0, 10);
const days = (a: string, b: string) => Math.round((Date.parse(b) - Date.parse(a)) / DAY);
const two = (n: number) => n.toFixed(2);

export const MOCK_SETTINGS: ExpenseSettings = {
  receiptRequiredAbove: "250.00",
  maxAgeDays: 90,
  categories: [
    { id: "travel", name: "Travel (TA)", limit: null, receiptAbove: "0.00" },
    { id: "local_conveyance", name: "Local conveyance", limit: "1500.00", receiptAbove: null },
    { id: "meals", name: "Meals and refreshments", limit: "800.00", receiptAbove: null },
    { id: "stationery", name: "Stationery and printing", limit: null, receiptAbove: null },
  ],
};

/** What the mock "reads" off any uploaded receipt. */
export const MOCK_READING: OcrFields = {
  merchant: "Hotel Sai Palace, Ahilyanagar", date: null, total: 378, tax: 18, tip: null, currency: "INR", categoryId: "meals",
  paymentMethod: "Mobile payment", cardLast4: null, items: [{ name: "Veg thali x2", amount: 360 }], notes: null, confidence: 0.82,
};

export function sampleExpense(n: number, over: Partial<ExpenseView> = {}): ExpenseView {
  return {
    id: `exp-${n}`, claimId: null, spentOn: "2026-10-10", merchant: `Merchant ${n}`, amount: "120.00", currency: "INR", categoryId: "local_conveyance",
    categoryName: "Local conveyance", paymentMethod: "Cash", tax: null, gstin: null, notes: null, items: [], receiptId: null, ocr: null, flags: [], version: 1, ...over,
  };
}

interface ClaimRec {
  id: string; number: string; title: string; purpose: string | null; status: ClaimStatus; staffId: string; staffLabel: string;
  submittedAt: string | null; approvedAt: string | null; reimbursedAt: string | null; decidedBy: string | null; decisionNote: string | null;
  returnedReason: string | null; paymentReference: string | null; total: string | null; version: number; updatedAt: string;
}

export function createMockExpensesApi(today: string = iso(Date.now()), options: { ocr?: "read" | "disabled" | "failed" } = {}): ExpensesApi {
  let settings: ExpenseSettings = structuredClone(MOCK_SETTINGS);
  const at = (n: number) => iso(Date.parse(today) + n * DAY);
  const now = () => new Date(`${today}T10:00:00Z`).toISOString();
  const ocrMode = options.ocr ?? "read";
  const receipts = new Map<string, ExpenseReceiptView & { url: string }>();
  let seq = 100;
  const me = { id: "staff-1", label: "Prof. Meera Kulkarni (EMP-0042)" };

  const expenses: ExpenseView[] = [
    sampleExpense(1, { merchant: "Ola cab to SPPU", amount: "420.00", spentOn: at(-6), receiptId: "rc-1", ocr: { total: 420 } }),
    sampleExpense(2, { merchant: "Xerox Corner", amount: "86.00", spentOn: at(-3), categoryId: "stationery", categoryName: "Stationery and printing" }),
    sampleExpense(3, { merchant: "MSRTC bus, Pune", amount: "310.00", spentOn: at(-120), categoryId: "travel", categoryName: "Travel (TA)" }),
    sampleExpense(4, { claimId: "clm-1", merchant: "Shivneri bus Pune–Mumbai", amount: "525.00", spentOn: at(-12), categoryId: "travel", categoryName: "Travel (TA)", receiptId: "rc-2", ocr: { total: 552 } }),
    sampleExpense(5, { claimId: "clm-1", merchant: "Conference lunch", amount: "640.00", spentOn: at(-11), categoryId: "meals", categoryName: "Meals and refreshments", receiptId: "rc-3", ocr: { total: 640 } }),
    sampleExpense(6, { claimId: "clm-2", merchant: "Lab chemicals courier", amount: "1180.00", spentOn: at(-20), categoryId: "local_conveyance", receiptId: "rc-4" }),
  ];
  const claims: ClaimRec[] = [
    { id: "clm-1", number: "EXP-2026-0007", title: "NAAC workshop, Mumbai", purpose: "Attended NAAC AQAR workshop", status: "submitted", staffId: "staff-2", staffLabel: "Dr. Anil Patil (EMP-0017)", submittedAt: now(), approvedAt: null, reimbursedAt: null, decidedBy: null, decisionNote: null, returnedReason: null, paymentReference: null, total: null, version: 2, updatedAt: now() },
    { id: "clm-2", number: "EXP-2026-0005", title: "September expenses", purpose: null, status: "approved", staffId: "staff-3", staffLabel: "Ms. Sana Shaikh (EMP-0031)", submittedAt: now(), approvedAt: now(), reimbursedAt: null, decidedBy: "Finance Officer", decisionNote: null, returnedReason: null, paymentReference: null, total: null, version: 3, updatedAt: now() },
  ];
  for (const [id, total] of [["rc-1", 420], ["rc-2", 552], ["rc-3", 640], ["rc-4", 1180]] as const) {
    receipts.set(id, { id, fileName: `${id}.jpg`, contentType: "image/jpeg", byteSize: 182_000, duplicate: false, reading: { status: "read", engine: "paddleocr", fields: { ...MOCK_READING, total }, lineCount: 9 }, createdAt: now(), url: "" });
  }

  const flagsOf = (e: Omit<ExpenseView, "flags">): string[] => {
    const f: string[] = [];
    const cat = settings.categories.find((c) => c.id === e.categoryId);
    const amount = Number(e.amount);
    if (!cat) f.push("uncategorised");
    if (cat?.limit && amount > Number(cat.limit)) f.push("over_limit");
    const threshold = Number(cat?.receiptAbove ?? settings.receiptRequiredAbove);
    if (!e.receiptId && amount > threshold) f.push("receipt_missing");
    if (e.spentOn > today) f.push("future_date");
    else if (settings.maxAgeDays > 0 && days(e.spentOn, today) > settings.maxAgeDays) f.push("too_old");
    if (e.currency !== "INR") f.push("foreign_currency");
    return f;
  };
  const view = (e: ExpenseView): ExpenseView => ({ ...e, categoryName: settings.categories.find((c) => c.id === e.categoryId)?.name ?? null, flags: flagsOf(e) });
  const linesOf = (id: string) => expenses.filter((e) => e.claimId === id).map(view);
  const sum = (ls: ExpenseView[], k: "amount" | "tax") => two(ls.reduce((n, l) => n + Number(l[k] ?? 0), 0));
  const full = (c: ClaimRec): ExpenseClaimView => {
    const lines = linesOf(c.id);
    const flags = [...new Set(lines.flatMap((l) => l.flags))];
    return {
      ...c, institutionId: "i", currency: "INR", total: c.total ?? sum(lines, "amount"), taxTotal: sum(lines, "tax"), lineCount: lines.length,
      withReceipt: lines.filter((l) => l.receiptId).length, approvalEntry: c.approvedAt ? `JV-${c.number}` : null, paymentEntry: c.reimbursedAt ? `PV-${c.number}` : null,
      nextStep: { draft: "Submit claim", submitted: "Approve", approved: "Mark reimbursed", reimbursed: null }[c.status], flags,
      canSubmit: c.status === "draft" && lines.length > 0 && !flags.some(isBlocking), lines,
    };
  };
  const summary = (c: ClaimRec): ExpenseClaimSummary => {
    const v = full(c);
    return { id: v.id, number: v.number, title: v.title, status: v.status, staffId: v.staffId, staffLabel: v.staffLabel, total: v.total, lineCount: v.lineCount, submittedAt: v.submittedAt, returnedReason: v.returnedReason, version: v.version, updatedAt: v.updatedAt };
  };
  const claimOf = (id: string) => {
    const c = claims.find((x) => x.id === id);
    if (!c) throw new Error("Claim not found");
    return c;
  };
  const checkVersion = (c: { version: number }, v: number) => {
    if (c.version !== v) throw new Error("Someone changed this claim; reload and try again");
  };
  const touch = (c: ClaimRec) => {
    c.version += 1;
    c.updatedAt = now();
  };
  const mineOnly = (c: ClaimRec) => {
    if (c.staffId !== me.id) throw new Error("Not your claim");
  };
  const setLines = (c: ClaimRec, ids: string[]) => {
    for (const e of expenses) if (e.claimId === c.id) e.claimId = null;
    for (const id of ids) {
      const e = expenses.find((x) => x.id === id);
      if (!e || (e.claimId && e.claimId !== c.id)) throw new Error("That expense is on another claim");
      e.claimId = c.id;
    }
  };
  const fromInput = (input: ExpenseInput) => ({
    spentOn: input.spentOn, merchant: input.merchant, amount: two(Number(input.amount)), currency: input.currency ?? "INR", categoryId: input.categoryId ?? null,
    paymentMethod: input.paymentMethod ?? null, tax: input.tax ? two(Number(input.tax)) : null, gstin: input.gstin ?? null, notes: input.notes ?? null,
    items: input.items ?? [], receiptId: input.receiptId ?? null,
  });
  const mine = expenses.slice(0, 3).map((e) => e.id);
  const myExpense = (id: string) => {
    const e = expenses.find((x) => x.id === id && mine.includes(x.id));
    if (!e) throw new Error("Expense not found");
    return e;
  };

  return {
    settings: async () => structuredClone(settings),
    saveSettings: async (_i, s) => {
      if (s.categories.some((c) => !/^[a-z0-9_]+$/.test(c.id))) throw new Error("Category keys use lowercase letters, digits and _");
      settings = structuredClone(s);
      return structuredClone(settings);
    },
    claims: async (_i, status) => claims.filter((c) => !status || c.status === status).map(summary),
    dashboard: async () => {
      const sub = claims.filter((c) => c.status === "submitted").map(full);
      const appr = claims.filter((c) => c.status === "approved").map(full);
      const done = claims.filter((c) => c.status === "approved" || c.status === "reimbursed").flatMap((c) => linesOf(c.id));
      const byCat = new Map<string, number>();
      for (const l of done) byCat.set(l.categoryName ?? "Uncategorised", (byCat.get(l.categoryName ?? "Uncategorised") ?? 0) + Number(l.amount));
      const months = Array.from({ length: 6 }, (_, i) => {
        const d = new Date(Date.parse(today));
        d.setUTCDate(1);
        d.setUTCMonth(d.getUTCMonth() - (5 - i));
        return d.toISOString().slice(0, 7);
      });
      return {
        awaitingApproval: sub.length, awaitingApprovalAmount: two(sub.reduce((n, c) => n + Number(c.total), 0)),
        awaitingReimbursement: appr.length, awaitingReimbursementAmount: two(appr.reduce((n, c) => n + Number(c.total), 0)),
        byCategory: [...byCat].map(([name, amount]) => ({ categoryId: settings.categories.find((c) => c.name === name)?.id ?? null, name, amount: two(amount) })),
        trend: months.map((month, i) => ({ month, amount: two([0, 4200, 3100, 8650, 2300, done.reduce((n, l) => n + Number(l.amount), 0)][i] ?? 0) })),
        ocrMismatches: sub.flatMap((c) => c.lines).filter((l) => typeof l.ocr?.total === "number" && Number(l.ocr.total) !== Number(l.amount)).length,
      };
    },
    claim: async (id) => full(claimOf(id)),
    decide: async (id, d) => {
      const c = claimOf(id);
      checkVersion(c, d.version);
      if (c.status !== "submitted") throw new Error("Only a submitted claim can be decided");
      if (d.decision === "return") {
        if (!d.reason?.trim()) throw new Error("Say why the claim is sent back");
        c.status = "draft";
        c.returnedReason = d.reason.trim();
        c.submittedAt = null;
        c.total = null;
      } else {
        c.status = "approved";
        c.approvedAt = now();
        c.decidedBy = "Finance Officer";
        c.decisionNote = d.reason ?? null;
      }
      touch(c);
      return full(c);
    },
    reimburse: async (id, r) => {
      const c = claimOf(id);
      checkVersion(c, r.version);
      if (c.status !== "approved") throw new Error("Only an approved claim can be reimbursed");
      if (r.paidOn && r.paidOn > today) throw new Error("The payment date cannot be in the future");
      c.status = "reimbursed";
      c.reimbursedAt = now();
      c.paymentReference = r.reference ?? null;
      touch(c);
      return full(c);
    },
    receiptUrl: (id) => receipts.get(id)?.url || `data:image/svg+xml,${encodeURIComponent(`<svg xmlns="http://www.w3.org/2000/svg" width="120" height="160"><rect width="120" height="160" fill="#f4f1ea"/><text x="60" y="84" font-size="12" text-anchor="middle" fill="#555">${id}</text></svg>`)}`,

    mine: async () => {
      const unclaimed = expenses.filter((e) => mine.includes(e.id) && !e.claimId).map(view);
      return {
        staffLabel: me.label, institutionId: "i", settings: structuredClone(settings), ocrEnabled: ocrMode !== "disabled", ocrEngine: ocrMode === "disabled" ? "none" : "paddleocr",
        unclaimed, unclaimedTotal: sum(unclaimed, "amount"), claims: claims.filter((c) => c.staffId === me.id).map(summary),
      };
    },
    uploadReceipt: async (u) => {
      const id = `rc-${++seq}`;
      const reading = ocrMode === "read"
        ? { status: "read" as const, engine: "paddleocr", fields: { ...MOCK_READING, date: at(-1) }, gstin: "27AAPFU0939F1ZV", lineCount: 12 }
        : ocrMode === "failed"
          ? { status: "failed" as const, engine: "paddleocr", error: "Reader timed out", lineCount: 0 }
          : { status: "disabled" as const, engine: "none", lineCount: 0 };
      const r = { id, fileName: u.fileName, contentType: u.contentType, byteSize: Math.floor((u.dataBase64.length * 3) / 4), duplicate: false, reading, createdAt: now(), url: u.contentType.startsWith("image/") ? `data:${u.contentType};base64,${u.dataBase64}` : "" };
      receipts.set(id, r);
      return { id: r.id, fileName: r.fileName, contentType: r.contentType, byteSize: r.byteSize, duplicate: r.duplicate, reading: r.reading, createdAt: r.createdAt };
    },
    createExpense: async (input) => {
      const ocr = input.receiptId ? receipts.get(input.receiptId)?.reading.fields ?? null : null;
      const e: ExpenseView = { id: `exp-${++seq}`, claimId: null, ...fromInput(input), categoryName: null, ocr: ocr ? { ...ocr } : null, flags: [], version: 1 };
      expenses.push(e);
      mine.push(e.id);
      return view(e);
    },
    updateExpense: async (id, input) => {
      const e = myExpense(id);
      if (e.version !== input.version) throw new Error("This expense changed; reload and try again");
      Object.assign(e, fromInput(input), { version: e.version + 1 });
      return view(e);
    },
    deleteExpense: async (id) => {
      const e = myExpense(id);
      if (e.claimId && claimOf(e.claimId).status !== "draft") throw new Error("This expense is on a submitted claim");
      expenses.splice(expenses.indexOf(e), 1);
    },
    createClaim: async (input) => {
      if (!input.expenseIds.length) throw new Error("Pick at least one expense");
      const n = claims.length + 8;
      const c: ClaimRec = {
        id: `clm-${++seq}`, number: `EXP-2026-${String(n).padStart(4, "0")}`, title: input.title?.trim() || `${new Date(Date.parse(today)).toLocaleString("en-IN", { month: "long", year: "numeric", timeZone: "UTC" })} expenses`,
        purpose: input.purpose ?? null, status: "draft", staffId: me.id, staffLabel: me.label, submittedAt: null, approvedAt: null, reimbursedAt: null, decidedBy: null,
        decisionNote: null, returnedReason: null, paymentReference: null, total: null, version: 1, updatedAt: now(),
      };
      for (const id of input.expenseIds) myExpense(id);
      claims.push(c);
      setLines(c, input.expenseIds);
      return full(c);
    },
    updateClaim: async (id, input) => {
      const c = claimOf(id);
      mineOnly(c);
      checkVersion(c, input.version);
      if (c.status !== "draft") throw new Error("Only a draft claim can be changed");
      setLines(c, input.expenseIds);
      if (input.title) c.title = input.title;
      if (input.purpose !== undefined) c.purpose = input.purpose ?? null;
      touch(c);
      return full(c);
    },
    deleteClaim: async (id, version) => {
      const c = claimOf(id);
      mineOnly(c);
      checkVersion(c, version);
      if (c.status !== "draft") throw new Error("Only a draft claim can be deleted");
      setLines(c, []);
      claims.splice(claims.indexOf(c), 1);
    },
    submit: async (id, version) => {
      const c = claimOf(id);
      mineOnly(c);
      checkVersion(c, version);
      if (!full(c).canSubmit) throw new Error("Fix the flagged lines before submitting");
      c.status = "submitted";
      c.submittedAt = now();
      c.returnedReason = null;
      c.total = full(c).total;
      touch(c);
      return full(c);
    },
    withdraw: async (id, version) => {
      const c = claimOf(id);
      mineOnly(c);
      checkVersion(c, version);
      if (c.status !== "submitted") throw new Error("Only a submitted claim can be withdrawn");
      c.status = "draft";
      c.submittedAt = null;
      c.total = null;
      touch(c);
      return full(c);
    },
  };
}
