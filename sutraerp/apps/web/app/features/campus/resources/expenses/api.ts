/**
 * ERP_EXTENSIONS 3D API layer: one interface, an HTTP implementation (CSRF on writes,
 * Idempotency-Key on creates) and the mock in `./mock`. The 3D paths are not in the generated
 * OpenAPI client yet, so they are plain strings checked against `modules/resources/src/expenses/http.rs`.
 */
import { ApiProblem, isProblem, isUnsafeMethod, newIdempotencyKey } from "@sutra/api-client";
import { getCsrfToken } from "../../../../lib/api";
import type {
  ClaimCreate, ClaimDecision, ClaimLines, ClaimReimburse, ClaimStatus, ExpenseClaimSummary, ExpenseClaimView, ExpenseDashboard, ExpenseInput,
  ExpenseReceiptView, ExpenseSettings, ExpenseView, MyExpenses, ReceiptUpload,
} from "./types";

const B = "/api/v1/resources";
export const EXPENSE_PATHS = {
  settings: `${B}/institutions/{id}/expense-settings`,
  claims: `${B}/institutions/{id}/expense-claims`,
  dashboard: `${B}/institutions/{id}/expense-dashboard`,
  claim: `${B}/expense-claims/{id}`,
  decision: `${B}/expense-claims/{id}/decision`,
  reimburse: `${B}/expense-claims/{id}/reimburse`,
  receiptFile: `${B}/expense-receipts/{id}/file`,
  me: `${B}/me/expenses`,
  meExpense: `${B}/me/expenses/{id}`,
  meReceipts: `${B}/me/expense-receipts`,
  meClaims: `${B}/me/expense-claims`,
  meClaim: `${B}/me/expense-claims/{id}`,
  submit: `${B}/me/expense-claims/{id}/submit`,
  withdraw: `${B}/me/expense-claims/{id}/withdraw`,
} as const;

export interface ExpensesApi {
  // finance
  settings(institutionId: string): Promise<ExpenseSettings>;
  saveSettings(institutionId: string, s: ExpenseSettings): Promise<ExpenseSettings>;
  claims(institutionId: string, status?: ClaimStatus): Promise<ExpenseClaimSummary[]>;
  dashboard(institutionId: string): Promise<ExpenseDashboard>;
  claim(id: string): Promise<ExpenseClaimView>;
  decide(id: string, d: ClaimDecision): Promise<ExpenseClaimView>;
  reimburse(id: string, r: ClaimReimburse): Promise<ExpenseClaimView>;
  /** URL of a receipt's file (thumbnail / open in a new tab). */
  receiptUrl(receiptId: string): string;
  // self-service
  mine(): Promise<MyExpenses>;
  uploadReceipt(u: ReceiptUpload): Promise<ExpenseReceiptView>;
  createExpense(input: ExpenseInput): Promise<ExpenseView>;
  updateExpense(id: string, input: ExpenseInput & { version: number }): Promise<ExpenseView>;
  deleteExpense(id: string): Promise<void>;
  createClaim(input: ClaimCreate): Promise<ExpenseClaimView>;
  updateClaim(id: string, input: ClaimLines): Promise<ExpenseClaimView>;
  deleteClaim(id: string, version: number): Promise<void>;
  submit(id: string, version: number): Promise<ExpenseClaimView>;
  withdraw(id: string, version: number): Promise<ExpenseClaimView>;
}

const fill = (t: string, id: string) => t.replace("{id}", encodeURIComponent(id));
type Fetch = (input: string, init: RequestInit) => Promise<Response>;

export function createHttpExpensesApi(fetchImpl: Fetch = (i, init) => fetch(i, init), baseUrl: string = import.meta.env.VITE_API_BASE ?? ""): ExpensesApi {
  async function request<T>(method: string, path: string, opts: { body?: unknown; idempotent?: boolean } = {}): Promise<T> {
    const headers: Record<string, string> = { Accept: "application/json" };
    const unsafe = isUnsafeMethod(method);
    if (unsafe) {
      headers["Content-Type"] = "application/json";
      const csrf = getCsrfToken();
      if (csrf) headers["X-CSRF-Token"] = csrf;
    }
    if (opts.idempotent) headers["Idempotency-Key"] = newIdempotencyKey();
    const res = await fetchImpl(`${baseUrl}${path}`, { method, headers, credentials: "include", body: unsafe ? JSON.stringify(opts.body ?? {}) : undefined });
    const text = await res.text();
    let data: unknown;
    try {
      data = text ? JSON.parse(text) : undefined;
    } catch {
      data = undefined;
    }
    if (!res.ok) {
      if (isProblem(data)) throw new ApiProblem(data);
      throw new ApiProblem({ type: "about:blank", title: `Request failed (${res.status})`, status: res.status, errorCode: `http-${res.status}` });
    }
    return data as T;
  }
  const P = EXPENSE_PATHS;
  return {
    settings: (id) => request("GET", fill(P.settings, id)),
    saveSettings: (id, s) => request("PUT", fill(P.settings, id), { body: s }),
    claims: (id, status) => request("GET", `${fill(P.claims, id)}${status ? `?status=${encodeURIComponent(status)}` : ""}`),
    dashboard: (id) => request("GET", fill(P.dashboard, id)),
    claim: (id) => request("GET", fill(P.claim, id)),
    decide: (id, d) => request("POST", fill(P.decision, id), { body: d }),
    reimburse: (id, r) => request("POST", fill(P.reimburse, id), { body: r }),
    receiptUrl: (id) => `${baseUrl}${fill(P.receiptFile, id)}`,
    mine: () => request("GET", P.me),
    uploadReceipt: (u) => request("POST", P.meReceipts, { body: u }),
    createExpense: (input) => request("POST", P.me, { body: input, idempotent: true }),
    updateExpense: (id, input) => request("PUT", fill(P.meExpense, id), { body: input }),
    deleteExpense: async (id) => {
      await request<void>("DELETE", fill(P.meExpense, id));
    },
    createClaim: (input) => request("POST", P.meClaims, { body: input, idempotent: true }),
    updateClaim: (id, input) => request("PUT", fill(P.meClaim, id), { body: input }),
    deleteClaim: async (id, version) => {
      await request<void>("DELETE", `${fill(P.meClaim, id)}?version=${version}`);
    },
    submit: (id, version) => request("POST", fill(P.submit, id), { body: { version } }),
    withdraw: (id, version) => request("POST", fill(P.withdraw, id), { body: { version } }),
  };
}
