/**
 * ERP_EXTENSIONS 3A API layer: one interface, an HTTP implementation (CSRF, Idempotency-Key on
 * create, If-Match on change / delete) and the mock in `./mock`. Paths are checked against the
 * OpenAPI.
 */
import { ApiProblem, isProblem, isUnsafeMethod, newIdempotencyKey, type paths } from "@sutra/api-client";
import { getCsrfToken } from "../../../lib/api";
import type { Candidate, DutyShiftInput, DutyShiftResult, FillPlan, RosterBoard, RosterSettings } from "./types";

const B = "/api/v1/people";
export const ROSTER_PATHS = {
  board: `${B}/institutions/{id}/roster`,
  shifts: `${B}/institutions/{id}/roster/shifts`,
  shift: `${B}/roster/shifts/{id}`,
  suggestions: `${B}/roster/shifts/{id}/suggestions`,
  fill: `${B}/institutions/{id}/roster/fill`,
  publish: `${B}/institutions/{id}/roster/publish`,
  settings: `${B}/institutions/{id}/roster/settings`,
} as const satisfies Record<string, keyof paths>;

export interface RosterApi {
  board(institutionId: string, from?: string, to?: string): Promise<RosterBoard>;
  createShift(institutionId: string, input: DutyShiftInput): Promise<DutyShiftResult>;
  updateShift(id: string, version: number, input: DutyShiftInput): Promise<DutyShiftResult>;
  deleteShift(id: string, version: number): Promise<void>;
  suggestions(id: string): Promise<Candidate[]>;
  fill(institutionId: string, from: string, to: string, apply: boolean): Promise<FillPlan>;
  publish(institutionId: string, from: string, to: string): Promise<{ published: number }>;
  settings(institutionId: string): Promise<RosterSettings>;
  saveSettings(institutionId: string, s: RosterSettings): Promise<RosterSettings>;
}

const fill = (t: string, id: string) => t.replace("{id}", encodeURIComponent(id));
type Fetch = (input: string, init: RequestInit) => Promise<Response>;

export function createHttpRosterApi(fetchImpl: Fetch = (i, init) => fetch(i, init), baseUrl: string = import.meta.env.VITE_API_BASE ?? ""): RosterApi {
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
  const P = ROSTER_PATHS;
  return {
    board: (id, from, to) => request("GET", `${fill(P.board, id)}${from ? `?from=${from}${to ? `&to=${to}` : ""}` : ""}`),
    createShift: (id, input) => request("POST", fill(P.shifts, id), { body: input, idempotent: true }),
    updateShift: (id, version, input) => request("PUT", fill(P.shift, id), { body: input, ifMatch: version }),
    deleteShift: async (id, version) => {
      await request<void>("DELETE", fill(P.shift, id), { ifMatch: version });
    },
    suggestions: async (id) => (await request<{ items: Candidate[] }>("GET", fill(P.suggestions, id))).items,
    fill: (id, from, to, apply) => request("POST", fill(P.fill, id), { body: { from, to, apply } }),
    publish: (id, from, to) => request("POST", fill(P.publish, id), { body: { from, to } }),
    settings: (id) => request("GET", fill(P.settings, id)),
    saveSettings: (id, s) => request("PUT", fill(P.settings, id), { body: s }),
  };
}
