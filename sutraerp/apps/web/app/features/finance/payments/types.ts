/**
 * Slice-5B wire types (payments, receipts, refunds, receivables). Amounts are decimal strings
 * (contract §3.1) — format them with `formatINR` (`../ui` money / ledger), never parse them to floats.
 */
export type PaymentStatus = "initiated" | "pending" | "completed" | "failed" | "expired" | "cancelled" | "reversed" | "charged_back";
export type PaymentChannel = "gateway" | "cash" | "cheque" | "bank_transfer" | "demand_draft" | "sponsor";
export type RefundStatus = "requested" | "under_review" | "approved" | "rejected" | "processing" | "completed" | "failed" | "cancelled" | "reversed" | "closed";
export type ReceivableStatus = "open" | "partially_settled" | "overdue" | "settled" | "written_off" | "reopened";
export type DunningStatus = "open" | "notice_sent" | "escalated" | "resolved" | "cancelled";

export interface Items<T> {
  items: T[];
}

export interface Allocation {
  id: string;
  chargeId: string;
  chargeNumber: string;
  amount: string;
  source: "capture" | "unapplied";
  status: "allocated" | "reversed";
  journalEntryId?: string | null;
}

export interface Payment {
  id: string;
  paymentNumber: string;
  institutionId: string;
  studentAccountId: string;
  accountName?: string | null;
  channel: PaymentChannel;
  payerKind: string;
  payerName?: string | null;
  externalRef?: string | null;
  amount: string;
  currency: string;
  status: PaymentStatus;
  statusReason?: string | null;
  gatewayOrderRef?: string | null;
  gatewayPaymentRef?: string | null;
  checkoutUrl?: string | null;
  failureCode?: string | null;
  receivedOn: string;
  completedAt?: string | null;
  allocations: Allocation[];
  unappliedAmount: string;
  receipt?: { id: string; receiptNumber: string; status: "issued" | "cancelled"; issuedAt: string } | null;
  journalEntryId?: string | null;
  reversalEntryId?: string | null;
  processInstanceId?: string | null;
  createdAt: string;
  version: number;
}

export interface Receipt {
  id: string;
  receiptNumber: string;
  paymentId: string;
  paymentNumber: string;
  studentAccountId: string;
  accountName?: string | null;
  studentNumber?: string | null;
  channel: PaymentChannel;
  amount: string;
  currency: string;
  status: "issued" | "cancelled";
  issuedAt: string;
  allocations: Allocation[];
  unappliedAmount: string;
  amountDisplay: string;
}

export interface UnidentifiedReceipt {
  id: string;
  channel: PaymentChannel;
  amount: string;
  receivedOn: string;
  payerName?: string | null;
  externalRef?: string | null;
  status: "unidentified" | "under_investigation" | "identified" | "allocated" | "refunded";
  studentAccountId?: string | null;
  paymentId?: string | null;
  version: number;
}

export interface Collection {
  id: string;
  name: string;
  channel: PaymentChannel;
  receivedOn: string;
  totalAmount: string;
  status: "draft" | "validated" | "executing" | "completed" | "partially_completed" | "failed";
  items: { lineNo: number; studentAccountId: string; amount: string; status: string; error?: string | null; paymentId?: string | null }[];
  version: number;
}

export interface RefundHistory {
  fromStatus?: string | null;
  toStatus: string;
  reason?: string | null;
  actor?: string | null;
  at: string;
}

export interface Refund {
  id: string;
  refundNumber: string;
  studentAccountId: string;
  accountName?: string | null;
  paymentId?: string | null;
  amount: string;
  currency: string;
  reason: string;
  method: "original" | "bank_transfer";
  status: RefundStatus;
  requestedVia: "self" | "staff";
  processInstanceId?: string | null;
  decidedBy?: string | null;
  journalEntryId?: string | null;
  payEntryId?: string | null;
  attempts: number;
  failureReason?: string | null;
  history: RefundHistory[];
  createdAt: string;
  version: number;
}

export interface DunningCase {
  id: string;
  receivableId: string;
  status: DunningStatus;
  level: number;
  notices: { level: number; amountDue: string; noticeDate: string; message: string }[];
  version: number;
}

export interface Receivable {
  id: string;
  studentAccountId: string;
  accountName: string;
  studentNumber?: string | null;
  chargeId: string;
  chargeNumber: string;
  description: string;
  recognisedAmount: string;
  outstandingAmount: string;
  writtenOffAmount: string;
  dueDate?: string | null;
  daysOverdue: number;
  ageingBucket: string;
  status: ReceivableStatus;
  dunning?: DunningCase | null;
  writeOffs: { id: string; amount: string; reason: string; status: string }[];
  version: number;
}

export interface ReceivableList extends Items<Receivable> {
  totalOutstanding: string;
  totalOverdue: string;
}

export interface Evaluation {
  recognised: number;
  overdue: number;
  settled: number;
  reopened: number;
  evaluated: number;
  asOf: string;
}

/** Foundation account view (`/me/accounts`, `/institutions/{id}/student-accounts`). */
export interface AccountSummary {
  id: string;
  displayName: string;
  studentNumber?: string | null;
  balance: { outstanding: string; overdue: string; totalSettled: string; currency: string };
}

export interface MyAccount {
  account: AccountSummary;
  charges: { id: string; chargeNumber: string; description: string; outstandingAmount: string; dueDate?: string | null; status: string }[];
}

export interface RecordPayment {
  channel: Exclude<PaymentChannel, "gateway">;
  amount: string;
  externalRef?: string;
  payerName?: string;
}
