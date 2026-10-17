/**
 * ERP_EXTENSIONS 2A API layer: one interface, an HTTP implementation (CSRF, Idempotency-Key on
 * create, If-Match on changes) and the mock in `./mock`. Paths are checked against the OpenAPI.
 */
import { ApiProblem, isProblem, isUnsafeMethod, newIdempotencyKey, type paths } from "@sutra/api-client";
import { getCsrfToken } from "../../../lib/api";
import type { CreatedEnquiry, Duplicates, Enquiry, EnquiryChange, EnquiryList, Forecast, Funnel, FunnelSettings, FunnelSettingsInput, NewEnquiry } from "./types";

const B = "/api/v1/student";
export const FUNNEL_PATHS = {
  enquiries: `${B}/institutions/{id}/admission-enquiries`,
  enquiry: `${B}/admission-enquiries/{id}`,
  link: `${B}/admission-enquiries/{id}/link-application`,
  funnel: `${B}/institutions/{id}/admission-funnel`,
  forecast: `${B}/institutions/{id}/admission-forecast`,
  duplicates: `${B}/institutions/{id}/admission-duplicates`,
  settings: `${B}/institutions/{id}/admission-funnel-settings`,
} as const satisfies Record<string, keyof paths>;

export interface FunnelApi {
  enquiries(institutionId: string, filter?: { stage?: string; q?: string; open?: boolean }): Promise<EnquiryList>;
  createEnquiry(institutionId: string, input: NewEnquiry): Promise<CreatedEnquiry>;
  updateEnquiry(id: string, version: number, change: EnquiryChange): Promise<Enquiry>;
  linkApplication(id: string, version: number, applicationId: string): Promise<Enquiry>;
  funnel(institutionId: string, window?: string): Promise<Funnel>;
  forecast(institutionId: string, months?: number): Promise<Forecast>;
  duplicates(institutionId: string): Promise<Duplicates>;
  settings(institutionId: string): Promise<FunnelSettings>;
  saveSettings(institutionId: string, input: FunnelSettingsInput): Promise<FunnelSettings>;
}

const fill = (t: string, id: string) => t.replace("{id}", encodeURIComponent(id));

function query(params: Record<string, string | number | boolean | undefined>): string {
  const q = Object.entries(params).filter(([, v]) => v !== undefined && v !== "");
  return q.length ? `?${q.map(([k, v]) => `${k}=${encodeURIComponent(String(v))}`).join("&")}` : "";
}

type Fetch = (input: string, init: RequestInit) => Promise<Response>;

export function createHttpFunnelApi(fetchImpl: Fetch = (i, init) => fetch(i, init), baseUrl: string = import.meta.env.VITE_API_BASE ?? ""): FunnelApi {
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
  const P = FUNNEL_PATHS;
  return {
    enquiries: (id, f = {}) => request("GET", `${fill(P.enquiries, id)}${query({ stage: f.stage, q: f.q, open: f.open })}`),
    createEnquiry: (id, input) => request("POST", fill(P.enquiries, id), { body: input, idempotent: true }),
    updateEnquiry: (id, version, change) => request("PATCH", fill(P.enquiry, id), { body: change, ifMatch: version }),
    linkApplication: (id, version, applicationId) => request("POST", fill(P.link, id), { body: { applicationId }, ifMatch: version }),
    funnel: (id, window) => request("GET", `${fill(P.funnel, id)}${query({ window })}`),
    forecast: (id, months) => request("GET", `${fill(P.forecast, id)}${query({ months })}`),
    duplicates: (id) => request("GET", fill(P.duplicates, id)),
    settings: (id) => request("GET", fill(P.settings, id)),
    saveSettings: (id, input) => request("PUT", fill(P.settings, id), { body: input }),
  };
}
