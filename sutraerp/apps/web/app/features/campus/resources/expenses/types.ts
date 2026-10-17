/**
 * ERP_EXTENSIONS 3D wire types (staff expense claims). Mirrors `modules/resources/src/expenses/http.rs`
 * by hand: the OpenAPI client is not regenerated for 3D yet. Money is a decimal string ("378.00").
 */
export type ClaimStatus = "draft" | "submitted" | "approved" | "reimbursed";
export type PaymentMethod = "Card" | "Cash" | "Bank transfer" | "Mobile payment" | "Other";
export type ExpenseFlag = "over_limit" | "receipt_missing" | "too_old" | "future_date" | "uncategorised" | "foreign_currency";
export type ReceiptContentType = "image/jpeg" | "image/png" | "image/webp" | "application/pdf";

export interface ExpenseCategory {
  id: string;
  name: string;
  /** Most one expense of this category may claim (₹). */
  limit?: string | null;
  /** Receipt required above this amount (₹; the institution threshold when absent). */
  receiptAbove?: string | null;
}

export interface ExpenseSettings {
  receiptRequiredAbove: string;
  /** 0 = no limit. */
  maxAgeDays: number;
  categories: ExpenseCategory[];
}

export interface OcrItem {
  name: string;
  amount: number | null;
}

/** What reading a receipt pre-filled (every field is a suggestion). */
export interface OcrFields {
  merchant: string | null;
  date: string | null;
  total: number | null;
  tax: number | null;
  tip: number | null;
  currency: string;
  categoryId: string | null;
  paymentMethod: string | null;
  cardLast4: string | null;
  items: OcrItem[];
  notes: string | null;
  /** 0…1. */
  confidence: number;
}

export interface ReceiptReading {
  status: "read" | "failed" | "disabled";
  engine: string;
  error?: string | null;
  fields?: OcrFields | null;
  gstin?: string | null;
  lineCount: number;
}

export interface ExpenseReceiptView {
  id: string;
  fileName: string;
  contentType: string;
  byteSize: number;
  duplicate: boolean;
  reading: ReceiptReading;
  createdAt: string;
}

export interface ReceiptUpload {
  fileName: string;
  contentType: ReceiptContentType;
  dataBase64: string;
}

export interface ExpenseInput {
  spentOn: string;
  merchant: string;
  amount: string;
  currency?: string;
  categoryId?: string | null;
  paymentMethod?: PaymentMethod | null;
  tax?: string | null;
  gstin?: string | null;
  notes?: string | null;
  items: OcrItem[];
  receiptId?: string | null;
  /** Required on update. */
  version?: number;
}

export interface ExpenseView {
  id: string;
  claimId: string | null;
  spentOn: string;
  merchant: string;
  amount: string;
  currency: string;
  categoryId: string | null;
  categoryName: string | null;
  paymentMethod: string | null;
  tax: string | null;
  gstin: string | null;
  notes: string | null;
  items: OcrItem[];
  receiptId: string | null;
  /** What OCR read when the expense was created from a receipt (`total` is a number). */
  ocr: (Partial<OcrFields> & Record<string, unknown>) | null;
  flags: string[];
  version: number;
}

export interface ExpenseClaimSummary {
  id: string;
  number: string;
  title: string;
  status: ClaimStatus;
  staffId: string;
  staffLabel: string;
  total: string;
  lineCount: number;
  submittedAt: string | null;
  returnedReason: string | null;
  version: number;
  updatedAt: string;
}

export interface ExpenseClaimView {
  id: string;
  institutionId: string;
  number: string;
  title: string;
  purpose: string | null;
  status: ClaimStatus;
  nextStep: string | null;
  staffId: string;
  staffLabel: string;
  currency: string;
  total: string;
  taxTotal: string;
  lineCount: number;
  withReceipt: number;
  submittedAt: string | null;
  approvedAt: string | null;
  reimbursedAt: string | null;
  decidedBy: string | null;
  decisionNote: string | null;
  returnedReason: string | null;
  approvalEntry: string | null;
  paymentEntry: string | null;
  paymentReference: string | null;
  flags: string[];
  canSubmit: boolean;
  lines: ExpenseView[];
  version: number;
  updatedAt: string;
}

export interface MyExpenses {
  staffLabel: string;
  institutionId: string;
  settings: ExpenseSettings;
  ocrEnabled: boolean;
  ocrEngine: string;
  unclaimed: ExpenseView[];
  unclaimedTotal: string;
  claims: ExpenseClaimSummary[];
}

export interface ClaimCreate {
  title?: string | null;
  purpose?: string | null;
  expenseIds: string[];
}

export interface ClaimLines extends ClaimCreate {
  version: number;
}

export interface ClaimDecision {
  decision: "approve" | "return";
  reason?: string | null;
  version: number;
}

export interface ClaimReimburse {
  paidOn?: string | null;
  reference?: string | null;
  version: number;
}

export interface ExpenseDashboard {
  awaitingApproval: number;
  awaitingApprovalAmount: string;
  awaitingReimbursement: number;
  awaitingReimbursementAmount: string;
  byCategory: { categoryId: string | null; name: string; amount: string }[];
  trend: { month: string; amount: string }[];
  ocrMismatches: number;
}
