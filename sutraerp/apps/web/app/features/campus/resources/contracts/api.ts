/**
 * ERP_EXTENSIONS 3C API layer: one interface, an HTTP implementation (CSRF on writes,
 * Idempotency-Key on creates) and the mock in `./mock`. Paths are checked against the OpenAPI.
 */
import { ApiProblem, isProblem, isUnsafeMethod, newIdempotencyKey, type paths } from "@sutra/api-client";
import { getCsrfToken } from "../../../../lib/api";
import type {
  Activity, ActivityInput, Contract, ContractDashboard, ContractDetail, ContractInput, ContractSettings, DecisionKind, MouEvidence,
  Obligation, ObligationInput, ReconcileResult,
} from "./types";

const B = "/api/v1/resources";
export const CONTRACT_PATHS = {
  settings: `${B}/institutions/{id}/contract-settings`,
  list: `${B}/institutions/{id}/contracts`,
  one: `${B}/contracts/{id}`,
  decision: `${B}/contracts/{id}/decision`,
  obligations: `${B}/contracts/{id}/obligations`,
  obligation: `${B}/contract-obligations/{id}`,
  complete: `${B}/contract-obligations/{id}/complete`,
  undo: `${B}/contract-obligations/{id}/undo`,
  activities: `${B}/contracts/{id}/activities`,
  activity: `${B}/contract-activities/{id}`,
  dashboard: `${B}/institutions/{id}/contract-dashboard`,
  reconcile: `${B}/institutions/{id}/contracts/reconcile-terms`,
  calendar: `${B}/institutions/{id}/contracts/calendar.ics`,
  calendarOne: `${B}/contracts/{id}/calendar.ics`,
  evidence: `${B}/institutions/{id}/mou-evidence`,
} as const satisfies Record<string, keyof paths>;

export interface ContractsApi {
  settings(institutionId: string): Promise<ContractSettings>;
  saveSettings(institutionId: string, s: ContractSettings): Promise<ContractSettings>;
  list(institutionId: string, q?: { kind?: string; attention?: boolean; q?: string }): Promise<Contract[]>;
  create(institutionId: string, input: ContractInput): Promise<ContractDetail>;
  get(id: string): Promise<ContractDetail>;
  update(id: string, input: ContractInput): Promise<ContractDetail>;
  decide(id: string, d: { kind: DecisionKind | null; note?: string; until?: string | null }): Promise<ContractDetail>;
  addObligation(contractId: string, input: ObligationInput): Promise<Obligation>;
  complete(obligationId: string): Promise<Obligation>;
  undo(obligationId: string): Promise<Obligation>;
  removeObligation(obligationId: string): Promise<void>;
  addActivity(contractId: string, input: ActivityInput): Promise<Activity>;
  removeActivity(activityId: string): Promise<void>;
  dashboard(institutionId: string): Promise<ContractDashboard>;
  reconcile(institutionId: string): Promise<ReconcileResult>;
  evidence(institutionId: string, year?: string): Promise<MouEvidence>;
  /** URL of the .ics download (book, or one contract). */
  calendarUrl(institutionId: string, contractId?: string): string;
}

const fill = (t: string, id: string) => t.replace("{id}", encodeURIComponent(id));
type Fetch = (input: string, init: RequestInit) => Promise<Response>;

export function createHttpContractsApi(fetchImpl: Fetch = (i, init) => fetch(i, init), baseUrl: string = import.meta.env.VITE_API_BASE ?? ""): ContractsApi {
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
  const P = CONTRACT_PATHS;
  return {
    settings: (id) => request("GET", fill(P.settings, id)),
    saveSettings: (id, s) => request("PUT", fill(P.settings, id), { body: s }),
    list: (id, q = {}) => {
      const p = new URLSearchParams();
      if (q.kind) p.set("kind", q.kind);
      if (q.attention) p.set("attention", "true");
      if (q.q) p.set("q", q.q);
      const qs = p.toString();
      return request("GET", `${fill(P.list, id)}${qs ? `?${qs}` : ""}`);
    },
    create: (id, input) => request("POST", fill(P.list, id), { body: input, idempotent: true }),
    get: (id) => request("GET", fill(P.one, id)),
    update: (id, input) => request("PUT", fill(P.one, id), { body: input }),
    decide: (id, d) => request("PUT", fill(P.decision, id), { body: d }),
    addObligation: (id, input) => request("POST", fill(P.obligations, id), { body: input, idempotent: true }),
    complete: (id) => request("POST", fill(P.complete, id)),
    undo: (id) => request("POST", fill(P.undo, id)),
    removeObligation: async (id) => {
      await request<void>("DELETE", fill(P.obligation, id));
    },
    addActivity: (id, input) => request("POST", fill(P.activities, id), { body: input, idempotent: true }),
    removeActivity: async (id) => {
      await request<void>("DELETE", fill(P.activity, id));
    },
    dashboard: (id) => request("GET", fill(P.dashboard, id)),
    reconcile: (id) => request("POST", fill(P.reconcile, id)),
    evidence: (id, year) => request("GET", `${fill(P.evidence, id)}${year ? `?year=${encodeURIComponent(year)}` : ""}`),
    calendarUrl: (id, contractId) => `${baseUrl}${contractId ? fill(P.calendarOne, contractId) : fill(P.calendar, id)}`,
  };
}
