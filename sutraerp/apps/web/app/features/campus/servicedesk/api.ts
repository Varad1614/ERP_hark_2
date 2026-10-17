/**
 * ERP_EXTENSIONS 2B API layer: one interface, an HTTP implementation (CSRF, Idempotency-Key on
 * create, If-Match on changes) and the mock in `./mock`. Paths are checked against the OpenAPI.
 */
import { ApiProblem, isProblem, isUnsafeMethod, newIdempotencyKey, type paths } from "@sutra/api-client";
import { getCsrfToken } from "../../../lib/api";
import type { Queue, QueueRow, Rule, RuleInput, RulePreview, RuleRun, SlaPolicy, SlaPolicyInput, View, ViewInput } from "./types";

const B = "/api/v1/campus";
export const DESK_PATHS = {
  rules: `${B}/institutions/{id}/service-desk/rules`,
  rule: `${B}/service-desk/rules/{id}`,
  preview: `${B}/institutions/{id}/service-desk/rules/preview`,
  views: `${B}/institutions/{id}/service-desk/views`,
  view: `${B}/service-desk/views/{id}`,
  viewTickets: `${B}/institutions/{id}/service-desk/views/{viewId}/tickets`,
  board: `${B}/institutions/{id}/service-desk/sla-board`,
  policy: `${B}/institutions/{id}/service-desk/sla-policy`,
  tags: `${B}/tickets/{id}/tags`,
  automation: `${B}/tickets/{id}/automation`,
} as const satisfies Record<string, keyof paths>;

export interface DeskApi {
  rules(institutionId: string): Promise<Rule[]>;
  createRule(institutionId: string, input: RuleInput): Promise<Rule>;
  updateRule(id: string, version: number, input: RuleInput): Promise<Rule>;
  deleteRule(id: string): Promise<void>;
  preview(institutionId: string, input: Pick<RuleInput, "conditions" | "actions">): Promise<RulePreview>;
  views(institutionId: string): Promise<View[]>;
  createView(institutionId: string, input: ViewInput): Promise<View>;
  deleteView(id: string): Promise<void>;
  viewTickets(institutionId: string, viewId: string): Promise<Queue>;
  board(institutionId: string): Promise<Queue>;
  policy(institutionId: string): Promise<SlaPolicy>;
  savePolicy(institutionId: string, input: SlaPolicyInput): Promise<SlaPolicy>;
  tags(ticketId: string, version: number, add: string[], remove: string[]): Promise<QueueRow>;
  automation(ticketId: string): Promise<RuleRun[]>;
}

const fill = (t: string, id: string, viewId?: string) => t.replace("{id}", encodeURIComponent(id)).replace("{viewId}", encodeURIComponent(viewId ?? ""));

type Fetch = (input: string, init: RequestInit) => Promise<Response>;

export function createHttpDeskApi(fetchImpl: Fetch = (i, init) => fetch(i, init), baseUrl: string = import.meta.env.VITE_API_BASE ?? ""): DeskApi {
  async function request<T>(method: string, path: string, opts: { body?: unknown; ifMatch?: number; idempotent?: boolean } = {}): Promise<T> {
    const headers: Record<string, string> = { Accept: "application/json" };
    const unsafe = isUnsafeMethod(method);
    if (unsafe) {
      headers["Content-Type"] = "application/json";
      const csrf = getCsrfToken();
      if (csrf) headers["X-CSRF-Token"] = csrf;
    }
    if (opts.ifMatch !== undefined) headers["If-Match"] = `"${opts.ifMatch}"`;
    if (opts.idempotent) headers["Idempotency-Key"] = newIdempotencyKey();
    const send = unsafe && method !== "DELETE";
    const res = await fetchImpl(`${baseUrl}${path}`, { method, headers, credentials: "include", body: send ? JSON.stringify(opts.body ?? {}) : undefined });
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
  const P = DESK_PATHS;
  return {
    rules: async (id) => (await request<{ items: Rule[] }>("GET", fill(P.rules, id))).items,
    createRule: (id, input) => request("POST", fill(P.rules, id), { body: input, idempotent: true }),
    updateRule: (id, version, input) => request("PUT", fill(P.rule, id), { body: input, ifMatch: version }),
    deleteRule: async (id) => {
      await request("DELETE", fill(P.rule, id));
    },
    preview: (id, input) => request("POST", fill(P.preview, id), { body: input }),
    views: async (id) => (await request<{ items: View[] }>("GET", fill(P.views, id))).items,
    createView: (id, input) => request("POST", fill(P.views, id), { body: input, idempotent: true }),
    deleteView: async (id) => {
      await request("DELETE", fill(P.view, id));
    },
    viewTickets: (id, viewId) => request("GET", fill(P.viewTickets, id, viewId)),
    board: (id) => request("GET", fill(P.board, id)),
    policy: (id) => request("GET", fill(P.policy, id)),
    savePolicy: (id, input) => request("PUT", fill(P.policy, id), { body: input }),
    tags: (id, version, add, remove) => request("POST", fill(P.tags, id), { body: { add, remove }, ifMatch: version }),
    automation: async (id) => (await request<{ items: RuleRun[] }>("GET", fill(P.automation, id))).items,
  };
}
