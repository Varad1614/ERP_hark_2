/** Slice-5C wire types (ledger, periods, treasury). Amounts are decimal strings, never floats. */
export interface Items<T> {
  items: T[];
}

export interface HistoryEntry {
  fromStatus?: string | null;
  toStatus: string;
  reason?: string | null;
  actor?: string | null;
  at: string;
}

export interface LedgerAccount {
  id: string;
  code: string;
  name: string;
  accountType: string;
  normalBalance: string;
  systemKey?: string | null;
  isPostable: boolean;
  status: string;
  debitTotal: string;
  creditTotal: string;
  balance: string;
  postingCount: number;
  version: number;
}

export interface JournalLine {
  lineNo: number;
  ledgerAccountId: string;
  accountCode: string;
  accountName: string;
  debit: string;
  credit: string;
  memo?: string | null;
}

export interface JournalEntry {
  id: string;
  entryNumber: string;
  postingDate: string;
  kind: string;
  sourceType: string;
  sourceId: string;
  description: string;
  totalDebit: string;
  totalCredit: string;
  financialPeriodId?: string | null;
  reversesEntryId?: string | null;
  reversedByEntryId?: string | null;
  lines: JournalLine[];
}

export interface JournalEntryPage {
  items: JournalEntry[];
  nextCursor?: string | null;
}

export interface ManualJournal {
  id: string;
  journalNumber: string;
  postingDate: string;
  entryKind: string;
  description: string;
  totalAmount: string;
  status: string;
  requestedBy?: string | null;
  decidedBy?: string | null;
  journalEntryId?: string | null;
  reversalEntryId?: string | null;
  lines: JournalLine[];
  history: HistoryEntry[];
  version: number;
}

export interface ManualJournalInput {
  postingDate: string;
  entryKind?: string;
  description: string;
  lines: { ledgerAccountId: string; debit?: string; credit?: string; memo?: string }[];
}

export interface TrialBalanceRow {
  ledgerAccountId: string;
  code: string;
  name: string;
  accountType: string;
  debit: string;
  credit: string;
  balance: string;
}

export interface TrialBalance {
  asOf: string;
  periodId?: string | null;
  rows: TrialBalanceRow[];
  totalDebit: string;
  totalCredit: string;
  balanced: boolean;
}

export interface Reconciliation {
  id: string;
  scope: string;
  asOf: string;
  status: string;
  sourceTotal: string;
  ledgerTotal: string;
  difference: string;
  exceptions: { chargeId: string; chargeNumber: string; difference: string }[];
  createdAt: string;
}

export interface Period {
  id: string;
  financialYearId: string;
  periodNo: number;
  name: string;
  startDate: string;
  endDate: string;
  status: string;
  acceptsPostings: boolean;
  reopenCount: number;
  entryCount: number;
  history: HistoryEntry[];
  version: number;
}

export interface FinancialYear {
  id: string;
  code: string;
  name: string;
  startDate: string;
  endDate: string;
  status: string;
  periods: Period[];
  version: number;
}

export interface BankAccount {
  id: string;
  name: string;
  bankName: string;
  accountNumberLast4: string;
  ifsc: string;
  purpose: string;
  ledgerAccountId: string;
  ledgerAccountCode: string;
  status: string;
  ledgerBalance: string;
  unreconciledLines: number;
  version: number;
}

export interface BankTransaction {
  id: string;
  bankAccountId: string;
  kind: string;
  amount: string;
  feeAmount: string;
  valueDate: string;
  reference: string;
  status: string;
  items: { paymentId: string; gross: string; fee: string; net: string }[];
  version: number;
}

export interface StatementLine {
  id: string;
  lineNo: number;
  valueDate: string;
  direction: string;
  amount: string;
  reference?: string | null;
  description?: string | null;
  matchStatus: string;
  matchedEntryNumber?: string | null;
  matchedTransactionId?: string | null;
  note?: string | null;
}

export interface Statement {
  id: string;
  bankAccountId: string;
  statementRef: string;
  periodFrom: string;
  periodTo: string;
  closingBalance?: string | null;
  totalCredits: string;
  totalDebits: string;
  lineCount: number;
  /** `jsonl`, `csv`, `ofx` or `mt940`. */
  sourceFormat?: string;
  /** Lines of the file an earlier statement already held (skipped on import). */
  duplicateLines?: number;
  status: string;
  lines: StatementLine[];
  version: number;
}

export interface CashPosition {
  asOf: string;
  rows: { kind: string; bankAccountId?: string | null; name: string; ledgerAccountCode: string; balance: string; unreconciledLines: number }[];
  available: string;
  inTransit: string;
  total: string;
}

/** CSV column positions (0-based); a missing key is a column the file does not have. */
export interface ColumnMapping {
  date?: number | null;
  description?: number | null;
  amount?: number | null;
  debit?: number | null;
  credit?: number | null;
  indicator?: number | null;
  reference?: number | null;
}

export type StatementFormat = "auto" | "csv" | "ofx" | "mt940" | "jsonl";

export interface StatementFileOptions {
  format?: StatementFormat;
  /** `dmy` (Indian banks, default) or `mdy`. */
  dateStyle?: "dmy" | "mdy";
  invert?: boolean;
  mapping?: ColumnMapping | null;
}

export interface StatementPreviewLine {
  valueDate: string;
  direction: string;
  amount: string;
  reference?: string | null;
  description?: string | null;
  duplicate: boolean;
}

export interface StatementPreview {
  format: string;
  headers: string[];
  mapping?: ColumnMapping | null;
  statementRef?: string | null;
  periodFrom: string;
  periodTo: string;
  openingBalance?: string | null;
  closingBalance?: string | null;
  totalCredits: string;
  totalDebits: string;
  newLines: number;
  duplicateLines: number;
  lines: StatementPreviewLine[];
  skipped: { row: number; reason: string }[];
}
