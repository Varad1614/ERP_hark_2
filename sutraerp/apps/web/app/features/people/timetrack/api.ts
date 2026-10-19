/**
 * ERP_EXTENSIONS 5C API layer: one interface, an HTTP implementation (CSRF, Idempotency-Key on
 * create, If-Match on change / delete / decide) and the mock in `./mock`. The endpoints are new,
 * so their paths and types are local (`./types`) until the regenerated OpenAPI lands.
 */
import { ApiProblem, isProblem, isUnsafeMethod, newIdempotencyKey } from "@sutra/api-client";
import { getCsrfToken } from "../../../lib/api";
import type {
  EffortReport,
  ImportResult,
  MyTimeWeek,
  StaffRate,
  TimeEntry,
  TimeEntryInput,
  Timesheet,
  TimeProject,
  TimeProjectInput,
  TimerView,
  TimeSettings,
} from "./types";

const B = "/api/v1/people";
export const TIME_PATHS = {
  settings: `${B}/institutions/{id}/time-settings`,
  projects: `${B}/institutions/{id}/time-projects`,
  project: `${B}/time-projects/{id}`,
  timesheets: `${B}/institutions/{id}/timesheets`,
  timesheet: `${B}/timesheets/{id}`,
  decision: `${B}/timesheets/{id}/decision`,
  rates: `${B}/institutions/{id}/time-rates`,
  rate: `${B}/staff/{id}/time-rate`,
  effort: `${B}/institutions/{id}/effort-report`,
  effortCsv: `${B}/institutions/{id}/effort-report/csv`,
  entriesCsv: `${B}/institutions/{id}/time-entries/csv`,
  me: `${B}/me/time`,
  entries: `${B}/me/time/entries`,
  entry: `${B}/me/time/entries/{id}`,
  timer: `${B}/me/time/timer`,
  timerStop: `${B}/me/time/timer/stop`,
  submit: `${B}/me/time/timesheets`,
  withdraw: `${B}/me/time/timesheets/{id}/withdraw`,
  approvals: `${B}/me/time-approvals`,
  myCsv: `${B}/me/time/csv`,
  import: `${B}/me/time/import`,
} as const;

export interface TimeApi {
  settings(institutionId: string): Promise<TimeSettings>;
  saveSettings(institutionId: string, s: TimeSettings): Promise<TimeSettings>;
  projects(institutionId: string, status?: string): Promise<TimeProject[]>;
  createProject(institutionId: string, input: TimeProjectInput): Promise<TimeProject>;
  updateProject(id: string, version: number, input: TimeProjectInput): Promise<TimeProject>;
  timesheets(institutionId: string, status?: string): Promise<Timesheet[]>;
  decide(id: string, version: number, approve: boolean, reason?: string): Promise<Timesheet>;
  rates(institutionId: string): Promise<StaffRate[]>;
  setRate(staffId: string, rate: number | null, from: string | null): Promise<StaffRate>;
  effort(institutionId: string, from: string, to: string, approvedOnly?: boolean): Promise<EffortReport>;
  effortCsvUrl(institutionId: string, from: string, to: string): string;
  entriesCsvUrl(institutionId: string, from: string, to: string): string;
  myWeek(week?: string): Promise<MyTimeWeek>;
  addEntry(input: TimeEntryInput): Promise<TimeEntry>;
  updateEntry(id: string, version: number, input: TimeEntryInput): Promise<TimeEntry>;
  deleteEntry(id: string, version: number): Promise<void>;
  startTimer(projectId: string, task?: string): Promise<TimerView>;
  stopTimer(): Promise<TimeEntry>;
  discardTimer(): Promise<void>;
  submitWeek(projectId: string, week: string): Promise<Timesheet>;
  withdraw(id: string, version: number): Promise<Timesheet>;
  myApprovals(): Promise<Timesheet[]>;
  myCsvUrl(from: string, to: string): string;
  importCsv(csv: string, dryRun: boolean): Promise<ImportResult>;
}

const fill = (t: string, id: string) => t.replace("{id}", encodeURIComponent(id));
type Fetch = (input: string, init: RequestInit) => Promise<Response>;

export function createHttpTimeApi(fetchImpl: Fetch = (i, init) => fetch(i, init), baseUrl: string = import.meta.env.VITE_API_BASE ?? ""): TimeApi {
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
  const P = TIME_PATHS;
  const q = (params: Record<string, string | boolean | undefined>) => {
    const s = Object.entries(params)
      .filter(([, v]) => v !== undefined && v !== "")
      .map(([k, v]) => `${k}=${encodeURIComponent(String(v))}`)
      .join("&");
    return s ? `?${s}` : "";
  };
  return {
    settings: (id) => request("GET", fill(P.settings, id)),
    saveSettings: (id, s) => request("PUT", fill(P.settings, id), { body: s }),
    projects: async (id, status) => (await request<{ items: TimeProject[] }>("GET", `${fill(P.projects, id)}${q({ status })}`)).items,
    createProject: (id, input) => request("POST", fill(P.projects, id), { body: input, idempotent: true }),
    updateProject: (id, version, input) => request("PUT", fill(P.project, id), { body: input, ifMatch: version }),
    timesheets: async (id, status) => (await request<{ items: Timesheet[] }>("GET", `${fill(P.timesheets, id)}${q({ status })}`)).items,
    decide: (id, version, approve, reason) => request("POST", fill(P.decision, id), { body: { decision: approve ? "approve" : "reject", reason }, ifMatch: version }),
    rates: async (id) => (await request<{ items: StaffRate[] }>("GET", fill(P.rates, id))).items,
    setRate: (staffId, rate, from) => request("PUT", fill(P.rate, staffId), { body: { rate, from } }),
    effort: (id, from, to, approvedOnly) => request("GET", `${fill(P.effort, id)}${q({ from, to, approvedOnly: approvedOnly || undefined })}`),
    effortCsvUrl: (id, from, to) => `${baseUrl}${fill(P.effortCsv, id)}${q({ from, to })}`,
    entriesCsvUrl: (id, from, to) => `${baseUrl}${fill(P.entriesCsv, id)}${q({ from, to })}`,
    myWeek: (week) => request("GET", `${P.me}${q({ week })}`),
    addEntry: async (input) => (await request<{ entry: TimeEntry }>("POST", P.entries, { body: input, idempotent: true })).entry,
    updateEntry: async (id, version, input) => (await request<{ entry: TimeEntry }>("PUT", fill(P.entry, id), { body: input, ifMatch: version })).entry,
    deleteEntry: async (id, version) => {
      await request<void>("DELETE", fill(P.entry, id), { ifMatch: version });
    },
    startTimer: (projectId, task) => request("POST", P.timer, { body: { projectId, task } }),
    stopTimer: async () => (await request<{ entry: TimeEntry }>("POST", P.timerStop, { body: {} })).entry,
    discardTimer: async () => {
      await request<void>("DELETE", P.timer);
    },
    submitWeek: (projectId, week) => request("POST", P.submit, { body: { projectId, week } }),
    withdraw: (id, version) => request("POST", fill(P.withdraw, id), { ifMatch: version }),
    myApprovals: async () => (await request<{ items: Timesheet[] }>("GET", P.approvals)).items,
    myCsvUrl: (from, to) => `${baseUrl}${P.myCsv}${q({ from, to })}`,
    importCsv: (csv, dryRun) => request("POST", P.import, { body: { csv, dryRun } }),
  };
}
