/**
 * ERP_EXTENSIONS 2D API layer: one interface, an HTTP implementation (CSRF, Idempotency-Key on
 * create, If-Match on changes) and the mock in `./mock`. Paths are checked against the OpenAPI.
 */
import { ApiProblem, isProblem, isUnsafeMethod, newIdempotencyKey, type paths } from "@sutra/api-client";
import { getCsrfToken } from "../../../lib/api";
import type { Board, FleetCosts, FleetSettings, FuelInput, FuelLog, Renewal, RenewalInput, ReminderInput, ServiceInput, ServiceReminder, VehicleFile, VehicleLite } from "./types";

const B = "/api/v1/campus";
export const FLEET_PATHS = {
  vehicles: `${B}/institutions/{id}/vehicles`,
  board: `${B}/institutions/{id}/fleet-maintenance`,
  settings: `${B}/institutions/{id}/fleet-settings`,
  costs: `${B}/institutions/{id}/fleet-costs`,
  file: `${B}/vehicles/{id}/maintenance`,
  meter: `${B}/vehicles/{id}/meter-readings`,
  reminders: `${B}/vehicles/{id}/service-reminders`,
  reminder: `${B}/service-reminders/{id}`,
  reminderSnooze: `${B}/service-reminders/{id}/snooze`,
  renewals: `${B}/vehicles/{id}/renewals`,
  renewalComplete: `${B}/vehicle-renewals/{id}/complete`,
  renewalSnooze: `${B}/vehicle-renewals/{id}/snooze`,
  services: `${B}/vehicles/{id}/service-entries`,
  fuel: `${B}/vehicles/{id}/fuel-entries`,
  expenses: `${B}/vehicles/{id}/expenses`,
} as const satisfies Record<string, keyof paths>;

export interface FleetApi {
  vehicles(institutionId: string): Promise<VehicleLite[]>;
  board(institutionId: string, status?: string): Promise<Board>;
  settings(institutionId: string): Promise<FleetSettings>;
  saveSettings(institutionId: string, s: FleetSettings): Promise<FleetSettings>;
  costs(institutionId: string, from?: string, to?: string): Promise<FleetCosts>;
  file(vehicleId: string): Promise<VehicleFile>;
  meter(vehicleId: string, value: string, setup?: { meterUnit: string; fuelCapacity: string | null }): Promise<VehicleFile>;
  addReminder(vehicleId: string, input: ReminderInput): Promise<ServiceReminder>;
  updateReminder(id: string, version: number, input: ReminderInput): Promise<ServiceReminder>;
  snoozeReminder(id: string, until: string | null): Promise<ServiceReminder>;
  addRenewal(vehicleId: string, input: RenewalInput): Promise<Renewal>;
  completeRenewal(id: string, version: number, body: { date?: string | null; nextDue?: string | null; cost?: string | null }): Promise<Renewal>;
  snoozeRenewal(id: string, until: string | null): Promise<Renewal>;
  addService(vehicleId: string, input: ServiceInput): Promise<VehicleFile>;
  fuel(vehicleId: string): Promise<FuelLog>;
  addFuel(vehicleId: string, input: FuelInput): Promise<FuelLog>;
  addExpense(vehicleId: string, input: { expenseType: string; amount: string; spentOn?: string | null; note?: string | null }): Promise<{ id: string }>;
}

const fill = (t: string, id: string) => t.replace("{id}", encodeURIComponent(id));

type Fetch = (input: string, init: RequestInit) => Promise<Response>;

export function createHttpFleetApi(fetchImpl: Fetch = (i, init) => fetch(i, init), baseUrl: string = import.meta.env.VITE_API_BASE ?? ""): FleetApi {
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
  const P = FLEET_PATHS;
  const q = (o: Record<string, string | undefined>) => {
    const p = Object.entries(o).filter(([, v]) => v).map(([k, v]) => `${k}=${encodeURIComponent(v!)}`);
    return p.length ? `?${p.join("&")}` : "";
  };
  return {
    vehicles: async (id) => (await request<{ items: VehicleLite[] }>("GET", fill(P.vehicles, id))).items,
    board: (id, status) => request("GET", `${fill(P.board, id)}${q({ status })}`),
    settings: (id) => request("GET", fill(P.settings, id)),
    saveSettings: (id, s) => request("PUT", fill(P.settings, id), { body: s }),
    costs: (id, from, to) => request("GET", `${fill(P.costs, id)}${q({ from, to })}`),
    file: (id) => request("GET", fill(P.file, id)),
    meter: (id, value, setup) => request("POST", fill(P.meter, id), { body: { value, setup }, idempotent: true }),
    addReminder: (id, input) => request("POST", fill(P.reminders, id), { body: input, idempotent: true }),
    updateReminder: (id, version, input) => request("PUT", fill(P.reminder, id), { body: input, ifMatch: version }),
    snoozeReminder: (id, until) => request("POST", fill(P.reminderSnooze, id), { body: { until } }),
    addRenewal: (id, input) => request("POST", fill(P.renewals, id), { body: input, idempotent: true }),
    completeRenewal: (id, version, body) => request("POST", fill(P.renewalComplete, id), { body, ifMatch: version }),
    snoozeRenewal: (id, until) => request("POST", fill(P.renewalSnooze, id), { body: { until } }),
    addService: (id, input) => request("POST", fill(P.services, id), { body: input, idempotent: true }),
    fuel: (id) => request("GET", fill(P.fuel, id)),
    addFuel: (id, input) => request("POST", fill(P.fuel, id), { body: input, idempotent: true }),
    addExpense: (id, input) => request("POST", fill(P.expenses, id), { body: input, idempotent: true }),
  };
}
