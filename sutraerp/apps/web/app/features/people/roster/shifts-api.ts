/**
 * ERP_EXTENSIONS 5B API layer: shift swaps, open-shift claims, clock punches, timesheets, the
 * payroll export, roster reports, timesheet settings and the staff calendar. Same shape as the
 * 3A `./api`: one interface, an HTTP implementation (CSRF on unsafe methods, Idempotency-Key on
 * create, If-Match on change), then React Query hooks. Query keys start with `institution` so
 * `useCommand` refreshes them after every command. Paths follow the utoipa annotations of
 * `modules/people/src/roster/shifts_http.rs` (not yet in the generated `paths` type).
 */
import { ApiProblem, isProblem, isUnsafeMethod, newIdempotencyKey } from "@sutra/api-client";
import { useQuery } from "@tanstack/react-query";
import { useMemo } from "react";
import { getCsrfToken } from "../../../lib/api";
import { useCommand } from "../../institution/hooks";
import { dispositionName, saveFile } from "../../imports/api";
import type {
  ClaimInput,
  ClockInput,
  DateRange,
  DecisionInput,
  ExportRange,
  PunchInput,
  ReportQuery,
  RosterMyTimesheet,
  RosterOpenShifts,
  RosterPayrollExport,
  RosterPunch,
  RosterReport,
  RosterSwap,
  RosterSwapList,
  RosterTimesheet,
  RosterTimesheetList,
  RosterTimesheetSettings,
  SwapRequestInput,
  SwapRespondInput,
  SwapStatus,
  TimesheetQuery,
  TimesheetSubmitInput,
} from "./shifts-types";

const B = "/api/v1/people";
export const SHIFTS_PATHS = {
  // swaps and claims
  swapRequest: `${B}/me/roster/swaps`,
  mySwaps: `${B}/me/roster/swaps`,
  openShifts: `${B}/me/roster/open-shifts`,
  claim: `${B}/me/roster/shifts/{id}/claim`,
  swapRespond: `${B}/roster/swaps/{id}/respond`,
  swapCancel: `${B}/roster/swaps/{id}/cancel`,
  swaps: `${B}/institutions/{id}/roster/swaps`,
  swapDecide: `${B}/roster/swaps/{id}/decision`,
  // punches
  clock: `${B}/me/roster/shifts/{id}/clock`,
  attendance: `${B}/roster/shifts/{id}/attendance`,
  // timesheets
  myTimesheet: `${B}/me/roster/timesheet`,
  submitTimesheet: `${B}/me/roster/timesheets`,
  timesheets: `${B}/institutions/{id}/roster/timesheets`,
  timesheet: `${B}/roster/timesheets/{id}`,
  timesheetDecide: `${B}/roster/timesheets/{id}/decision`,
  timesheetLock: `${B}/roster/timesheets/{id}/lock`,
  timesheetCsv: `${B}/institutions/{id}/roster/timesheet.csv`,
  timesheetSettings: `${B}/institutions/{id}/roster/timesheet-settings`,
  // payroll, reports, calendar
  payrollExport: `${B}/institutions/{id}/roster/payroll-export`,
  payrollExportCsv: `${B}/institutions/{id}/roster/payroll-export.csv`,
  reports: `${B}/institutions/{id}/roster/reports`,
  reportsCsv: `${B}/institutions/{id}/roster/reports.csv`,
  myIcs: `${B}/me/roster.ics`,
} as const;

export interface ShiftsApi {
  requestSwap(input: SwapRequestInput): Promise<RosterSwap>;
  mySwaps(): Promise<RosterSwap[]>;
  openShifts(range?: DateRange): Promise<RosterOpenShifts>;
  claim(shiftId: string, input?: ClaimInput): Promise<RosterSwap>;
  respondSwap(id: string, version: number, input: SwapRespondInput): Promise<RosterSwap>;
  cancelSwap(id: string, version: number): Promise<RosterSwap>;
  swaps(institutionId: string, status?: SwapStatus): Promise<RosterSwap[]>;
  decideSwap(id: string, version: number, input: DecisionInput): Promise<RosterSwap>;
  clock(shiftId: string, action: ClockInput["action"]): Promise<RosterPunch>;
  recordAttendance(shiftId: string, input: PunchInput): Promise<RosterPunch>;
  myTimesheet(range?: DateRange): Promise<RosterMyTimesheet>;
  submitTimesheet(input: TimesheetSubmitInput): Promise<RosterTimesheet>;
  timesheets(institutionId: string, q?: TimesheetQuery): Promise<RosterTimesheet[]>;
  timesheet(id: string): Promise<RosterTimesheet>;
  decideTimesheet(id: string, version: number, input: DecisionInput): Promise<RosterTimesheet>;
  lockTimesheet(id: string, version: number): Promise<RosterTimesheet>;
  payrollExport(institutionId: string, range: ExportRange): Promise<RosterPayrollExport>;
  reports(institutionId: string, q?: ReportQuery): Promise<RosterReport>;
  timesheetSettings(institutionId: string): Promise<RosterTimesheetSettings>;
  saveTimesheetSettings(institutionId: string, s: RosterTimesheetSettings): Promise<RosterTimesheetSettings>;
}

const fill = (t: string, id: string) => t.replace("{id}", encodeURIComponent(id));

/** `?a=1&b=2` of the defined, non-empty values ("" when none). */
export function queryString(q: Record<string, string | number | undefined | null> = {}): string {
  const p = new URLSearchParams();
  for (const [k, v] of Object.entries(q)) if (v !== undefined && v !== null && v !== "") p.set(k, String(v));
  const s = p.toString();
  return s ? `?${s}` : "";
}

const apiBase = (): string => import.meta.env.VITE_API_BASE ?? "";

// ---- download URLs (authenticated GETs; fetch them with `downloadFile`) -------------------

export function payrollExportCsvUrl(institutionId: string, range: ExportRange, baseUrl = apiBase()): string {
  return `${baseUrl}${fill(SHIFTS_PATHS.payrollExportCsv, institutionId)}${queryString({ from: range.from, to: range.to })}`;
}

export function timesheetCsvUrl(institutionId: string, range: DateRange = {}, baseUrl = apiBase()): string {
  return `${baseUrl}${fill(SHIFTS_PATHS.timesheetCsv, institutionId)}${queryString({ from: range.from, to: range.to })}`;
}

export function reportsCsvUrl(institutionId: string, q: ReportQuery = {}, baseUrl = apiBase()): string {
  return `${baseUrl}${fill(SHIFTS_PATHS.reportsCsv, institutionId)}${queryString({ from: q.from, to: q.to, section: q.section })}`;
}

export function myRosterIcsUrl(range: DateRange = {}, baseUrl = apiBase()): string {
  return `${baseUrl}${SHIFTS_PATHS.myIcs}${queryString({ from: range.from, to: range.to })}`;
}

type Fetch = (input: string, init: RequestInit) => Promise<Response>;

/**
 * Downloads an authenticated file (session cookie) and saves it through the shared imports
 * `saveFile` helper; the server's Content-Disposition name wins over `filename`.
 * problem+json errors are thrown as `ApiProblem`.
 */
export async function downloadFile(url: string, filename: string, fetchImpl: Fetch = (i, init) => fetch(i, init)): Promise<void> {
  const res = await fetchImpl(url, { method: "GET", credentials: "include", headers: { Accept: "text/csv, text/calendar, application/problem+json" } });
  if (!res.ok) {
    let data: unknown;
    try {
      data = JSON.parse(await res.text());
    } catch {
      data = undefined;
    }
    if (isProblem(data)) throw new ApiProblem(data);
    throw new ApiProblem({ type: "about:blank", title: `Download failed (${res.status})`, status: res.status, errorCode: `http-${res.status}` });
  }
  saveFile({ blob: await res.blob(), fileName: dispositionName(res, filename) });
}

export function createHttpShiftsApi(fetchImpl: Fetch = (i, init) => fetch(i, init), baseUrl: string = apiBase()): ShiftsApi {
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
  const P = SHIFTS_PATHS;
  const range = (r: DateRange = {}) => queryString({ from: r.from, to: r.to });
  return {
    requestSwap: (input) => request("POST", P.swapRequest, { body: input, idempotent: true }),
    mySwaps: async () => (await request<RosterSwapList>("GET", P.mySwaps)).items,
    openShifts: (r) => request("GET", `${P.openShifts}${range(r)}`),
    claim: (sid, input) => request("POST", fill(P.claim, sid), { body: input ?? {}, idempotent: true }),
    respondSwap: (id, version, input) => request("POST", fill(P.swapRespond, id), { body: input, ifMatch: version }),
    cancelSwap: (id, version) => request("POST", fill(P.swapCancel, id), { ifMatch: version }),
    swaps: async (id, status) => (await request<RosterSwapList>("GET", `${fill(P.swaps, id)}${queryString({ status })}`)).items,
    decideSwap: (id, version, input) => request("POST", fill(P.swapDecide, id), { body: input, ifMatch: version }),
    clock: (sid, action) => request("POST", fill(P.clock, sid), { body: { action } satisfies ClockInput }),
    recordAttendance: (sid, input) => request("PUT", fill(P.attendance, sid), { body: input }),
    myTimesheet: (r) => request("GET", `${P.myTimesheet}${range(r)}`),
    submitTimesheet: (input) => request("POST", P.submitTimesheet, { body: input, idempotent: true }),
    timesheets: async (id, q = {}) => (await request<RosterTimesheetList>("GET", `${fill(P.timesheets, id)}${queryString({ from: q.from, to: q.to, status: q.status })}`)).items,
    timesheet: (id) => request("GET", fill(P.timesheet, id)),
    decideTimesheet: (id, version, input) => request("POST", fill(P.timesheetDecide, id), { body: input, ifMatch: version }),
    lockTimesheet: (id, version) => request("POST", fill(P.timesheetLock, id), { ifMatch: version }),
    payrollExport: (id, r) => request("POST", fill(P.payrollExport, id), { body: r }),
    reports: (id, q = {}) => request("GET", `${fill(P.reports, id)}${queryString({ from: q.from, to: q.to })}`),
    timesheetSettings: (id) => request("GET", fill(P.timesheetSettings, id)),
    saveTimesheetSettings: (id, s) => request("PUT", fill(P.timesheetSettings, id), { body: s }),
  };
}

let fallback: ShiftsApi | null = null;
/** HTTP only: 5B has no mock yet (screens should pass `api` in tests). */
export function defaultShiftsApi(): ShiftsApi {
  fallback ??= createHttpShiftsApi();
  return fallback;
}

// ---- query keys + hooks -------------------------------------------------------------------

/** Prefix `institution`: `useCommand` refreshes every `institution` query after a command. */
export const shiftKeys = {
  mine: (...p: string[]) => ["institution", "roster", "me", ...p] as const,
  inst: (institutionId: string, ...p: string[]) => ["institution", "roster", institutionId, ...p] as const,
};

export function useMySwaps(api: ShiftsApi = defaultShiftsApi(), enabled = true) {
  return useQuery({ queryKey: shiftKeys.mine("swaps"), queryFn: () => api.mySwaps(), enabled });
}

export function useOpenShifts(range: DateRange = {}, api: ShiftsApi = defaultShiftsApi(), enabled = true) {
  return useQuery({ queryKey: shiftKeys.mine("open-shifts", range.from ?? "", range.to ?? ""), queryFn: () => api.openShifts(range), enabled });
}

export function useSwaps(institutionId: string, status?: SwapStatus, api: ShiftsApi = defaultShiftsApi(), enabled = true) {
  return useQuery({ queryKey: shiftKeys.inst(institutionId, "swaps", status ?? ""), queryFn: () => api.swaps(institutionId, status), enabled });
}

export function useMyTimesheet(range: DateRange = {}, api: ShiftsApi = defaultShiftsApi(), enabled = true) {
  return useQuery({ queryKey: shiftKeys.mine("timesheet", range.from ?? "", range.to ?? ""), queryFn: () => api.myTimesheet(range), enabled });
}

export function useTimesheets(institutionId: string, q: TimesheetQuery = {}, api: ShiftsApi = defaultShiftsApi(), enabled = true) {
  return useQuery({
    queryKey: shiftKeys.inst(institutionId, "timesheets", q.from ?? "", q.to ?? "", q.status ?? ""),
    queryFn: () => api.timesheets(institutionId, q),
    enabled,
  });
}

export function useTimesheet(id: string | null, api: ShiftsApi = defaultShiftsApi()) {
  return useQuery({ queryKey: ["institution", "roster", "timesheet", id ?? ""], queryFn: () => api.timesheet(id!), enabled: !!id });
}

export function useRosterReport(institutionId: string, q: ReportQuery = {}, api: ShiftsApi = defaultShiftsApi(), enabled = true) {
  return useQuery({ queryKey: shiftKeys.inst(institutionId, "reports", q.from ?? "", q.to ?? ""), queryFn: () => api.reports(institutionId, q), enabled });
}

export function useTimesheetSettings(institutionId: string, api: ShiftsApi = defaultShiftsApi(), enabled = true) {
  return useQuery({ queryKey: shiftKeys.inst(institutionId, "timesheet-settings"), queryFn: () => api.timesheetSettings(institutionId), enabled });
}

/**
 * Every 5B command through `useCommand` (toasts, problem+json text, step-up retry, refresh of
 * the `institution` queries). Returns `undefined` when a command failed (already toasted).
 */
export function useShiftCommands(api: ShiftsApi = defaultShiftsApi()) {
  const { run, submit, busy, dialog } = useCommand();
  const commands = useMemo(
    () => ({
      requestSwap: (input: SwapRequestInput) => run(() => api.requestSwap(input), "Request sent"),
      claim: (shiftId: string, reason?: string | null) => run(() => api.claim(shiftId, { reason: reason ?? null }), "Claim sent for approval"),
      respondSwap: (s: Pick<RosterSwap, "id" | "version">, accept: boolean, reason?: string | null) =>
        run(() => api.respondSwap(s.id, s.version, { accept, reason: reason ?? null }), accept ? "Accepted — sent to a manager" : "Declined"),
      cancelSwap: (s: Pick<RosterSwap, "id" | "version">) => run(() => api.cancelSwap(s.id, s.version), "Request withdrawn"),
      decideSwap: (s: Pick<RosterSwap, "id" | "version">, input: DecisionInput) =>
        run(() => api.decideSwap(s.id, s.version, input), input.decision === "approve" ? "Approved — roster updated" : "Declined"),
      clock: (shiftId: string, action: ClockInput["action"]) => run(() => api.clock(shiftId, action), action === "in" ? "Clocked in" : "Clocked out"),
      recordAttendance: (shiftId: string, input: PunchInput) => submit(() => api.recordAttendance(shiftId, input), "Attendance saved"),
      submitTimesheet: (input: TimesheetSubmitInput) => run(() => api.submitTimesheet(input), "Timesheet submitted"),
      decideTimesheet: (t: Pick<RosterTimesheet, "id" | "version">, input: DecisionInput) =>
        run(() => api.decideTimesheet(t.id, t.version, input), input.decision === "approve" ? "Timesheet approved" : "Timesheet returned"),
      lockTimesheet: (t: Pick<RosterTimesheet, "id" | "version">) => run(() => api.lockTimesheet(t.id, t.version), "Timesheet locked"),
      payrollExport: (institutionId: string, range: ExportRange) =>
        run(() => api.payrollExport(institutionId, range), (r) => `Exported ${r.exported} timesheet(s) to payroll${r.pending ? `; ${r.pending} still awaiting approval` : ""}`),
      saveTimesheetSettings: (institutionId: string, s: RosterTimesheetSettings) => submit(() => api.saveTimesheetSettings(institutionId, s), "Timesheet settings saved"),
      download: (url: string, filename: string) => run(() => downloadFile(url, filename)),
    }),
    [api, run, submit],
  );
  return { ...commands, run, busy, dialog };
}
