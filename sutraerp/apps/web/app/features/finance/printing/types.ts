/**
 * ERP_EXTENSIONS 3E wire types (docs/extensions-3e-printing.md). Not in the generated OpenAPI
 * client yet, so written by hand against the contract.
 */
export interface Registration {
  label: string;
  number: string;
}

/** Institution receipt header/footer. `title` empty → the institution's own name. */
export interface ReceiptPrintSettings {
  institutionId: string;
  title: string;
  address: string;
  /** At most 2, e.g. GSTIN, PAN. */
  registrations: Registration[];
  footer: string;
  showAmountInWords: boolean;
  updatedAt: string | null;
}
export type ReceiptPrintSettingsInput = Omit<ReceiptPrintSettings, "institutionId" | "updatedAt">;

export interface ReceiptSlipRow {
  label: string;
  value: string;
  strong?: boolean;
}
export interface ReceiptSlipItem {
  qty: string;
  name: string;
  amount: string;
  subs: string[];
}
export interface ReceiptSlipDoc {
  storeName: string;
  address: string;
  registrations: string[];
  voided: boolean;
  kind: string | null;
  meta: string[];
  items: ReceiptSlipItem[];
  totals: ReceiptSlipRow[];
  payments: ReceiptSlipRow[];
  note: string;
  reward: string;
  loyalty: string;
  footer: string;
}

export interface ReceiptSlip {
  receiptId: string | null;
  receiptNumber: string | null;
  columns: 32 | 48;
  doc: ReceiptSlipDoc;
  /** Readable text of the slip (for the browser print fallback). */
  preview: string;
  escposBase64: string;
  /** True only when kick=true was asked and the receipt's channel is cash. */
  kickDrawer: boolean;
}

export interface CounterReportRow {
  label: string;
  value: string;
  section?: string | null;
  strong?: boolean;
}

export interface CounterReport {
  institutionId: string;
  date: string;
  mine: boolean;
  receiptCount: number;
  cancelledCount: number;
  total: string;
  totalDisplay: string;
  byChannel: { channel: string; count: number; amount: string }[];
  rows: CounterReportRow[];
  preview: string;
  escposBase64: string;
}
