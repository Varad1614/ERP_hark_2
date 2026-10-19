/**
 * ERP_EXTENSIONS 2C API layer: one interface, an HTTP implementation (CSRF on writes) and the
 * mock in `./mock`. Paths are checked against the OpenAPI.
 */
import { ApiProblem, isProblem, isUnsafeMethod, type paths } from "@sutra/api-client";
import { getCsrfToken } from "../../../../lib/api";
import type { DemandReport, DemandSettings, ExpiryReport, ItemPlanning, LotList } from "./types";

const B = "/api/v1/resources";
export const DEMAND_PATHS = {
  demand: `${B}/institutions/{id}/stock-demand`,
  apply: `${B}/institutions/{id}/stock-demand/apply-thresholds`,
  settings: `${B}/institutions/{id}/stock-demand/settings`,
  planning: `${B}/items/{id}/planning`,
  lots: `${B}/items/{id}/lots`,
  expiry: `${B}/institutions/{id}/stock-expiry`,
} as const satisfies Record<string, keyof paths>;

export interface DemandApi {
  demand(institutionId: string, opts?: { attention?: boolean; days?: number }): Promise<DemandReport>;
  applyThresholds(institutionId: string, itemIds: string[]): Promise<{ updatedIds: string[]; skipped: number }>;
  settings(institutionId: string): Promise<DemandSettings>;
  saveSettings(institutionId: string, s: DemandSettings): Promise<DemandSettings>;
  savePlanning(itemId: string, p: ItemPlanning): Promise<ItemPlanning>;
  lots(itemId: string): Promise<LotList>;
  expiry(institutionId: string, days?: number): Promise<ExpiryReport>;
}

const fill = (t: string, id: string) => t.replace("{id}", encodeURIComponent(id));

type Fetch = (input: string, init: RequestInit) => Promise<Response>;

export function createHttpDemandApi(fetchImpl: Fetch = (i, init) => fetch(i, init), baseUrl: string = import.meta.env.VITE_API_BASE ?? ""): DemandApi {
  async function request<T>(method: string, path: string, body?: unknown): Promise<T> {
    const headers: Record<string, string> = { Accept: "application/json" };
    const unsafe = isUnsafeMethod(method);
    if (unsafe) {
      headers["Content-Type"] = "application/json";
      const csrf = getCsrfToken();
      if (csrf) headers["X-CSRF-Token"] = csrf;
    }
    const res = await fetchImpl(`${baseUrl}${path}`, { method, headers, credentials: "include", body: unsafe ? JSON.stringify(body ?? {}) : undefined });
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
  const P = DEMAND_PATHS;
  const qs = (o: Record<string, string | number | boolean | undefined>) => {
    const p = Object.entries(o).filter(([, v]) => v !== undefined).map(([k, v]) => `${k}=${encodeURIComponent(String(v))}`);
    return p.length ? `?${p.join("&")}` : "";
  };
  return {
    demand: (id, o = {}) => request("GET", `${fill(P.demand, id)}${qs({ attention: o.attention, days: o.days })}`),
    applyThresholds: (id, itemIds) => request("POST", fill(P.apply, id), { itemIds }),
    settings: (id) => request("GET", fill(P.settings, id)),
    saveSettings: (id, s) => request("PUT", fill(P.settings, id), s),
    savePlanning: (id, p) => request("PUT", fill(P.planning, id), p),
    lots: (id) => request("GET", fill(P.lots, id)),
    expiry: (id, days) => request("GET", `${fill(P.expiry, id)}${qs({ days })}`),
  };
}
