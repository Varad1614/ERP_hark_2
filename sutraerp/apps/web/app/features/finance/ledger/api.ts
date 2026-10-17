/**
 * Slice-5C API layer (ledger, periods, treasury). Same conventions as the other finance slices:
 * one interface, an HTTP implementation (CSRF, `Idempotency-Key` on creates and money-moving
 * commands, `If-Match` on state commands, problem+json → `ApiProblem`) and an in-memory mock
 * (`./mock`). Every path template is checked against the generated OpenAPI `paths`.
 */
import { ApiProblem, isProblem, isUnsafeMethod, newIdempotencyKey, type paths } from "@sutra/api-client";
import { getCsrfToken } from "../../../lib/api";
import { fill } from "../../institution/api";
import type {
  BankAccount,
  BankTransaction,
  CashPosition,
  FinancialStatement,
  FinancialYear,
  Items,
  JournalEntry,
  JournalEntryPage,
  LedgerAccount,
  ManualJournal,
  ManualJournalInput,
  Period,
  Reconciliation,
  Statement,
  StatementFileOptions,
  StatementKind,
  StatementParams,
  StatementPreview,
  TrialBalance,
} from "./types";

export type PeriodCommand = "open" | "lock" | "close" | "reopen" | "archive";
export type YearCommand = "open" | "close" | "archive";

export interface LedgerApi {
  // ledger
  accounts(institutionId: string): Promise<LedgerAccount[]>;
  createAccount(institutionId: string, input: { code: string; name: string; accountType: string }): Promise<LedgerAccount>;
  entries(institutionId: string): Promise<JournalEntryPage>;
  reverseEntry(id: string, reason: string): Promise<JournalEntry>;
  journals(institutionId: string): Promise<ManualJournal[]>;
  createJournal(institutionId: string, input: ManualJournalInput): Promise<ManualJournal>;
  submitJournal(id: string, version: number): Promise<ManualJournal>;
  decideJournal(id: string, version: number, approve: boolean, reason?: string): Promise<ManualJournal>;
  trialBalance(institutionId: string, asOf?: string): Promise<TrialBalance>;
  /** ERP_EXTENSIONS 1C: a financial statement computed on the server. */
  financialStatement(institutionId: string, kind: StatementKind, params: StatementParams): Promise<FinancialStatement>;
  reconciliations(institutionId: string): Promise<Reconciliation[]>;
  reconcile(institutionId: string): Promise<Reconciliation>;
  // periods
  years(institutionId: string): Promise<FinancialYear[]>;
  createYear(institutionId: string, input: { code: string; name: string; startDate: string; endDate: string }): Promise<FinancialYear>;
  yearCommand(id: string, version: number, cmd: YearCommand, reason?: string): Promise<FinancialYear>;
  periodCommand(id: string, version: number, cmd: PeriodCommand, reason?: string): Promise<Period>;
  // treasury
  bankAccounts(institutionId: string): Promise<BankAccount[]>;
  createBankAccount(institutionId: string, input: { name: string; bankName: string; accountNumberLast4: string; ifsc: string; purpose?: string }): Promise<BankAccount>;
  transactions(institutionId: string): Promise<BankTransaction[]>;
  statements(bankAccountId: string): Promise<Statement[]>;
  statement(id: string): Promise<Statement>;
  importStatement(bankAccountId: string, input: { statementRef?: string; periodFrom?: string; periodTo?: string; openingBalance?: string; closingBalance?: string; content: string } & StatementFileOptions): Promise<Statement>;
  /** Reads a bank file (CSV, OFX, MT940) without importing it: lines, duplicates, skipped rows. */
  previewStatement(bankAccountId: string, input: { content: string; periodFrom?: string; periodTo?: string } & StatementFileOptions): Promise<StatementPreview>;
  reconcileStatement(id: string, version: number): Promise<Statement>;
  cashPosition(institutionId: string): Promise<CashPosition>;
}

const B = "/api/v1/finance";
export const PATHS = {
  accounts: `${B}/institutions/{id}/ledger-accounts`,
  entries: `${B}/institutions/{id}/journal-entries`,
  reverse: `${B}/journal-entries/{id}/reverse`,
  journals: `${B}/institutions/{id}/manual-journals`,
  submit: `${B}/manual-journals/{id}/submit`,
  decision: `${B}/manual-journals/{id}/decision`,
  trialBalance: `${B}/institutions/{id}/trial-balance`,
  incomeExpenditure: `${B}/institutions/{id}/financial-statements/income-expenditure`,
  balanceSheet: `${B}/institutions/{id}/financial-statements/balance-sheet`,
  cashFlow: `${B}/institutions/{id}/financial-statements/cash-flow`,
  generalLedger: `${B}/institutions/{id}/financial-statements/general-ledger`,
  reconciliations: `${B}/institutions/{id}/reconciliations`,
  years: `${B}/institutions/{id}/financial-years`,
  yOpen: `${B}/financial-years/{id}/open`,
  yClose: `${B}/financial-years/{id}/close`,
  yArchive: `${B}/financial-years/{id}/archive`,
  pOpen: `${B}/financial-periods/{id}/open`,
  pLock: `${B}/financial-periods/{id}/lock`,
  pClose: `${B}/financial-periods/{id}/close`,
  pReopen: `${B}/financial-periods/{id}/reopen`,
  pArchive: `${B}/financial-periods/{id}/archive`,
  banks: `${B}/institutions/{id}/bank-accounts`,
  transactions: `${B}/institutions/{id}/bank-transactions`,
  statements: `${B}/bank-accounts/{id}/statements`,
  statementPreview: `${B}/bank-accounts/{id}/statements/preview`,
  statement: `${B}/bank-statements/{id}`,
  reconcile: `${B}/bank-statements/{id}/reconcile`,
  cash: `${B}/institutions/{id}/cash-position`,
} as const satisfies Record<string, keyof paths>;

const YEAR_CMD: Record<YearCommand, string> = { open: PATHS.yOpen, close: PATHS.yClose, archive: PATHS.yArchive };
const PERIOD_CMD: Record<PeriodCommand, string> = { open: PATHS.pOpen, lock: PATHS.pLock, close: PATHS.pClose, reopen: PATHS.pReopen, archive: PATHS.pArchive };

/** Commands that need a fresh second factor when step-up is enforced (`finance.approve`, `period.close`). */
export const STEP_UP_COMMANDS: ReadonlySet<string> = new Set(["journal:reverse", "journal:decision", "period:close", "period:reopen", "period:archive", "year:close", "year:archive"]);

type Fetch = (input: string, init: RequestInit) => Promise<Response>;

interface RequestOptions {
  body?: unknown;
  ifMatch?: number;
  idempotent?: boolean;
}

export function createHttpLedgerApi(fetchImpl: Fetch = (i, init) => fetch(i, init), baseUrl: string = import.meta.env.VITE_API_BASE ?? ""): LedgerApi {
  async function request<T>(method: string, path: string, opts: RequestOptions = {}): Promise<T> {
    const headers: Record<string, string> = { Accept: "application/json" };
    const unsafe = isUnsafeMethod(method);
    if (unsafe) {
      headers["Content-Type"] = "application/json";
      const csrf = getCsrfToken();
      if (csrf) headers["X-CSRF-Token"] = csrf;
    }
    if (opts.ifMatch !== undefined) headers["If-Match"] = `"${opts.ifMatch}"`;
    if (opts.idempotent) headers["Idempotency-Key"] = newIdempotencyKey();
    const res = await fetchImpl(`${baseUrl}${path}`, { method, headers, credentials: "include", body: unsafe ? JSON.stringify(opts.body ?? {}) : undefined });
    const text = await res.text();
    let data: unknown = undefined;
    if (text) {
      try {
        data = JSON.parse(text);
      } catch {
        data = undefined;
      }
    }
    if (!res.ok) {
      if (isProblem(data)) throw new ApiProblem(data);
      throw new ApiProblem({ type: "about:blank", title: `Request failed (${res.status})`, status: res.status, errorCode: `http-${res.status}` });
    }
    return data as T;
  }
  const items = async <T>(path: string) => (await request<Items<T>>("GET", path)).items;
  return {
    accounts: (inst) => items(fill(PATHS.accounts, { id: inst })),
    createAccount: (inst, input) => request("POST", fill(PATHS.accounts, { id: inst }), { body: input, idempotent: true }),
    entries: (inst) => request("GET", fill(PATHS.entries, { id: inst }) + "?limit=50"),
    reverseEntry: (id, reason) => request("POST", fill(PATHS.reverse, { id }), { body: { reason }, idempotent: true }),
    journals: (inst) => items(fill(PATHS.journals, { id: inst })),
    createJournal: (inst, input) => request("POST", fill(PATHS.journals, { id: inst }), { body: input, idempotent: true }),
    submitJournal: (id, v) => request("POST", fill(PATHS.submit, { id }), { body: {}, ifMatch: v, idempotent: true }),
    decideJournal: (id, v, approve, reason) => request("POST", fill(PATHS.decision, { id }), { body: { decision: approve ? "approve" : "reject", reason }, ifMatch: v, idempotent: true }),
    trialBalance: (inst, asOf) => request("GET", fill(PATHS.trialBalance, { id: inst }) + (asOf ? `?asOf=${encodeURIComponent(asOf)}` : "")),
    financialStatement: (inst, kind, params) => {
      const path = { "income-expenditure": PATHS.incomeExpenditure, "balance-sheet": PATHS.balanceSheet, "cash-flow": PATHS.cashFlow, "general-ledger": PATHS.generalLedger }[kind];
      const qs = new URLSearchParams(Object.entries(params).filter(([, v]) => v !== undefined && v !== "").map(([k, v]) => [k, String(v)])).toString();
      return request("GET", fill(path, { id: inst }) + (qs ? `?${qs}` : ""));
    },
    reconciliations: (inst) => items(fill(PATHS.reconciliations, { id: inst })),
    reconcile: (inst) => request("POST", fill(PATHS.reconciliations, { id: inst }), { body: {}, idempotent: true }),
    years: (inst) => items(fill(PATHS.years, { id: inst })),
    createYear: (inst, input) => request("POST", fill(PATHS.years, { id: inst }), { body: input, idempotent: true }),
    yearCommand: (id, v, cmd, reason) => request("POST", fill(YEAR_CMD[cmd], { id }), { body: reason ? { reason } : {}, ifMatch: v }),
    periodCommand: (id, v, cmd, reason) => request("POST", fill(PERIOD_CMD[cmd], { id }), { body: reason ? { reason } : {}, ifMatch: v }),
    bankAccounts: (inst) => items(fill(PATHS.banks, { id: inst })),
    createBankAccount: (inst, input) => request("POST", fill(PATHS.banks, { id: inst }), { body: input, idempotent: true }),
    transactions: (inst) => items(fill(PATHS.transactions, { id: inst })),
    statements: (bank) => items(fill(PATHS.statements, { id: bank })),
    statement: (id) => request("GET", fill(PATHS.statement, { id })),
    importStatement: (bank, input) => request("POST", fill(PATHS.statements, { id: bank }), { body: input, idempotent: true }),
    previewStatement: (bank, input) => request("POST", fill(PATHS.statementPreview, { id: bank }), { body: input }),
    reconcileStatement: (id, v) => request("POST", fill(PATHS.reconcile, { id }), { body: {}, ifMatch: v }),
    cashPosition: (inst) => request("GET", fill(PATHS.cash, { id: inst })),
  };
}
