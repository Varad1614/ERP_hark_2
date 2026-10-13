/**
 * Slice-5B API layer (payments, receipts, the mock gateway checkout, refunds, receivables).
 * Same conventions as `features/institution/api.ts`: one interface, an HTTP implementation
 * (CSRF, `Idempotency-Key` on creates and money-moving commands, `If-Match` on state commands,
 * problem+json → `ApiProblem`) and an in-memory mock (`./mock`). Every path template is checked
 * against the generated OpenAPI `paths`.
 */
import { ApiProblem, isProblem, isUnsafeMethod, newIdempotencyKey, type paths } from "@sutra/api-client";
import { getCsrfToken } from "../../../lib/api";
import { fill } from "../../institution/api";
import type {
  AccountSummary,
  Collection,
  Evaluation,
  Items,
  MyAccount,
  Payment,
  Receipt,
  Receivable,
  ReceivableList,
  RecordPayment,
  Refund,
  UnidentifiedReceipt,
} from "./types";

export type RefundCommand = "process" | "retry" | "complete" | "cancel" | "reverse" | "close";

export interface PaymentsApi {
  // staff: collections
  accounts(institutionId: string): Promise<AccountSummary[]>;
  payments(institutionId: string): Promise<Payment[]>;
  recordPayment(accountId: string, input: RecordPayment): Promise<Payment>;
  allocate(id: string, version: number): Promise<Payment>;
  reversePayment(id: string, version: number, reason: string): Promise<Payment>;
  receipt(paymentId: string): Promise<Receipt>;
  unidentified(institutionId: string): Promise<UnidentifiedReceipt[]>;
  recordUnidentified(institutionId: string, input: { channel: string; amount: string; externalRef?: string; payerName?: string }): Promise<UnidentifiedReceipt>;
  identify(id: string, version: number, accountId: string): Promise<UnidentifiedReceipt>;
  allocateUnidentified(id: string, version: number): Promise<UnidentifiedReceipt>;
  collections(institutionId: string): Promise<Collection[]>;
  // staff: refunds
  refunds(institutionId: string): Promise<Refund[]>;
  requestRefund(accountId: string, input: { amount: string; reason: string }): Promise<Refund>;
  decideRefund(id: string, version: number, approve: boolean, reason?: string): Promise<Refund>;
  refundCommand(id: string, version: number, cmd: RefundCommand, reason?: string): Promise<Refund>;
  // staff: receivables
  receivables(institutionId: string): Promise<ReceivableList>;
  evaluate(institutionId: string): Promise<Evaluation>;
  dunning(id: string, version: number): Promise<Receivable>;
  writeOff(id: string, version: number, reason: string): Promise<Receivable>;
  reopen(id: string, version: number, reason: string): Promise<Receivable>;
  // student self-service
  myAccounts(): Promise<MyAccount[]>;
  myPayments(): Promise<Payment[]>;
  initiate(amount: string, chargeIds?: string[]): Promise<Payment>;
  confirm(id: string, version: number): Promise<Payment>;
  cancelMine(id: string, version: number): Promise<Payment>;
  myReceipts(): Promise<Receipt[]>;
  myRefunds(): Promise<Refund[]>;
  requestMyRefund(input: { amount: string; reason: string }): Promise<Refund>;
}

const B = "/api/v1/finance";
export const PATHS = {
  accounts: `${B}/institutions/{id}/student-accounts`,
  payments: `${B}/institutions/{id}/payments`,
  accountPayments: `${B}/student-accounts/{id}/payments`,
  allocate: `${B}/payments/{id}/allocate`,
  reverse: `${B}/payments/{id}/reverse`,
  receipt: `${B}/payments/{id}/receipt`,
  unidentified: `${B}/institutions/{id}/unidentified-receipts`,
  identify: `${B}/unidentified-receipts/{id}/identify`,
  uAllocate: `${B}/unidentified-receipts/{id}/allocate`,
  collections: `${B}/institutions/{id}/collections`,
  refunds: `${B}/institutions/{id}/refunds`,
  accountRefunds: `${B}/student-accounts/{id}/refunds`,
  decision: `${B}/refunds/{id}/decision`,
  rProcess: `${B}/refunds/{id}/process`,
  rRetry: `${B}/refunds/{id}/retry`,
  rComplete: `${B}/refunds/{id}/complete`,
  rCancel: `${B}/refunds/{id}/cancel`,
  rReverse: `${B}/refunds/{id}/reverse`,
  rClose: `${B}/refunds/{id}/close`,
  receivables: `${B}/institutions/{id}/receivables`,
  dunning: `${B}/receivables/{id}/dunning`,
  writeOff: `${B}/receivables/{id}/write-off`,
  reopen: `${B}/receivables/{id}/reopen`,
  meAccounts: `${B}/me/accounts`,
  mePayments: `${B}/me/payments`,
  meConfirm: `${B}/me/payments/{id}/confirm`,
  meCancel: `${B}/me/payments/{id}/cancel`,
  meReceipts: `${B}/me/receipts`,
  meRefunds: `${B}/me/refunds`,
} as const satisfies Record<string, keyof paths>;

const REFUND_CMD: Record<RefundCommand, string> = {
  process: PATHS.rProcess,
  retry: PATHS.rRetry,
  complete: PATHS.rComplete,
  cancel: PATHS.rCancel,
  reverse: PATHS.rReverse,
  close: PATHS.rClose,
};
/** Money-moving refund commands carry an Idempotency-Key. */
const REFUND_MONEY: ReadonlySet<RefundCommand> = new Set(["process", "retry", "complete"]);

/** Commands that need a fresh second factor when step-up is enforced (`finance.approve`). */
export const STEP_UP_COMMANDS: ReadonlySet<string> = new Set(["payment:reverse", "refund:decision", "refund:reverse"]);

type Fetch = (input: string, init: RequestInit) => Promise<Response>;

interface RequestOptions {
  body?: unknown;
  ifMatch?: number;
  idempotent?: boolean;
}

export function createHttpPaymentsApi(fetchImpl: Fetch = (i, init) => fetch(i, init), baseUrl: string = import.meta.env.VITE_API_BASE ?? ""): PaymentsApi {
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
    accounts: async (inst) => (await items<{ id: string; displayName: string; studentNumber?: string | null; balance: AccountSummary["balance"] }>(fill(PATHS.accounts, { id: inst }))).map((a) => ({ id: a.id, displayName: a.displayName, studentNumber: a.studentNumber, balance: a.balance })),
    payments: (inst) => items(fill(PATHS.payments, { id: inst }) + "?limit=100"),
    recordPayment: (acc, input) => request("POST", fill(PATHS.accountPayments, { id: acc }), { body: input, idempotent: true }),
    allocate: (id, v) => request("POST", fill(PATHS.allocate, { id }), { body: {}, ifMatch: v, idempotent: true }),
    reversePayment: (id, v, reason) => request("POST", fill(PATHS.reverse, { id }), { body: { reason }, ifMatch: v }),
    receipt: (id) => request("GET", fill(PATHS.receipt, { id })),
    unidentified: (inst) => items(fill(PATHS.unidentified, { id: inst })),
    recordUnidentified: (inst, input) => request("POST", fill(PATHS.unidentified, { id: inst }), { body: input, idempotent: true }),
    identify: (id, v, acc) => request("POST", fill(PATHS.identify, { id }), { body: { studentAccountId: acc }, ifMatch: v }),
    allocateUnidentified: (id, v) => request("POST", fill(PATHS.uAllocate, { id }), { body: {}, ifMatch: v, idempotent: true }),
    collections: (inst) => items(fill(PATHS.collections, { id: inst })),
    refunds: (inst) => items(fill(PATHS.refunds, { id: inst })),
    requestRefund: (acc, input) => request("POST", fill(PATHS.accountRefunds, { id: acc }), { body: input, idempotent: true }),
    decideRefund: (id, v, approve, reason) => request("POST", fill(PATHS.decision, { id }), { body: { decision: approve ? "approve" : "reject", reason }, ifMatch: v, idempotent: true }),
    refundCommand: (id, v, cmd, reason) => request("POST", fill(REFUND_CMD[cmd], { id }), { body: reason ? { reason } : {}, ifMatch: v, idempotent: REFUND_MONEY.has(cmd) }),
    receivables: (inst) => request("GET", fill(PATHS.receivables, { id: inst })),
    evaluate: (inst) => request("POST", fill(PATHS.receivables, { id: inst }), { body: {} }),
    dunning: (id, v) => request("POST", fill(PATHS.dunning, { id }), { body: {}, ifMatch: v }),
    writeOff: (id, v, reason) => request("POST", fill(PATHS.writeOff, { id }), { body: { reason }, ifMatch: v, idempotent: true }),
    reopen: (id, v, reason) => request("POST", fill(PATHS.reopen, { id }), { body: { reason }, ifMatch: v }),
    myAccounts: () => items(PATHS.meAccounts),
    myPayments: () => items(PATHS.mePayments),
    initiate: (amount, chargeIds) => request("POST", PATHS.mePayments, { body: { amount, chargeIds }, idempotent: true }),
    confirm: (id, v) => request("POST", fill(PATHS.meConfirm, { id }), { body: {}, ifMatch: v }),
    cancelMine: (id, v) => request("POST", fill(PATHS.meCancel, { id }), { body: {}, ifMatch: v }),
    myReceipts: () => items(PATHS.meReceipts),
    myRefunds: () => items(PATHS.meRefunds),
    requestMyRefund: (input) => request("POST", PATHS.meRefunds, { body: input, idempotent: true }),
  };
}
