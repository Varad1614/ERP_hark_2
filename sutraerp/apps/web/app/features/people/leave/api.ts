/**
 * Slice-7B API layer (leave, attendance & workload). Same conventions as the Batch 6 slices: one
 * interface, an HTTP implementation (CSRF, `Idempotency-Key` on creates / bulk commands,
 * `If-Match` on state commands, problem+json → `ApiProblem`) and an in-memory mock (`./mock`).
 * Every path template is checked against the generated OpenAPI `paths`.
 */
import { ApiProblem, isProblem, isUnsafeMethod, newIdempotencyKey, type paths } from "@sutra/api-client";
import { getCsrfToken } from "../../../lib/api";
import { fill } from "../../institution/api";
import type {
  AccrualRun,
  AttendanceDay,
  AttendanceGrid,
  BulkSummary,
  Items,
  LeaveAdjustment,
  LeaveBalance,
  LeaveEncashment,
  LeaveException,
  LeavePolicy,
  LeaveRequest,
  LeaveRequestInput,
  LeaveRequestList,
  LeaveType,
  LeaveTypeInput,
  LongLeave,
  PolicyCommand,
  StaffRow,
  StaffWorkload,
  WorkloadPlan,
  YearEndRun,
} from "./types";

export interface LeaveApi {
  // policy
  policies(institutionId: string): Promise<LeavePolicy[]>;
  createPolicy(institutionId: string, input: { name: string; effectiveFrom: string; holidays?: string[] }): Promise<LeavePolicy>;
  addType(policyId: string, input: LeaveTypeInput): Promise<LeavePolicy>;
  policyCommand(policyId: string, version: number, cmd: PolicyCommand, reason?: string): Promise<LeavePolicy>;
  decidePolicy(policyId: string, version: number, approve: boolean, reason?: string): Promise<LeavePolicy>;
  // requests
  requests(institutionId: string, status?: string): Promise<LeaveRequestList>;
  requestLeave(institutionId: string, input: LeaveRequestInput): Promise<LeaveRequest>;
  decideRequest(id: string, version: number, approve: boolean, reason?: string): Promise<LeaveRequest>;
  cancelRequest(id: string, version: number, reason?: string): Promise<LeaveRequest>;
  consumeRequest(id: string, version: number): Promise<LeaveRequest>;
  sweep(institutionId: string): Promise<BulkSummary>;
  // runs
  accruals(institutionId: string): Promise<AccrualRun[]>;
  accrue(institutionId: string, leaveYear: number, month?: number): Promise<AccrualRun>;
  yearEnds(institutionId: string): Promise<YearEndRun[]>;
  yearEnd(institutionId: string, leaveYear: number): Promise<YearEndRun>;
  // staff
  staff(institutionId: string): Promise<StaffRow[]>;
  balances(staffId: string, leaveYear?: number): Promise<LeaveBalance[]>;
  adjust(staffId: string, input: { leaveTypeId: string; leaveYear: number; days: string; reason: string }): Promise<LeaveAdjustment>;
  encash(staffId: string, input: { leaveTypeId: string; leaveYear: number; days: string; reason?: string }): Promise<LeaveEncashment>;
  // exceptions
  exceptions(institutionId: string): Promise<LeaveException[]>;
  raiseException(institutionId: string, input: { staffId?: string; category: string; description: string }): Promise<LeaveException>;
  exceptionCommand(id: string, version: number, cmd: "review" | "resolve", resolution?: string): Promise<LeaveException>;
  // attendance
  grid(institutionId: string, date: string): Promise<AttendanceGrid>;
  bulk(institutionId: string, workDate: string, entries: { staffId: string; status: string }[]): Promise<BulkSummary>;
  correct(dayId: string, version: number, status: string, remark: string): Promise<AttendanceDay>;
  // long leave
  longLeaves(engagementId: string): Promise<LongLeave[]>;
  planLongLeave(engagementId: string, input: { plannedStart: string; plannedEnd?: string; reason: string }): Promise<LongLeave>;
  longLeaveCommand(id: string, version: number, cmd: "start" | "end"): Promise<LongLeave>;
  // workload
  workload(staffId: string): Promise<StaffWorkload>;
  workloadPlans(staffId: string): Promise<WorkloadPlan[]>;
  createPlan(staffId: string, input: { termLabel: string; availability: string; plannedWeeklyHours: string; maxWeeklyHours?: string }): Promise<WorkloadPlan>;
  submitPlan(id: string, version: number, exceptionReason?: string): Promise<WorkloadPlan>;
  decidePlan(id: string, version: number, approve: boolean, reason?: string): Promise<WorkloadPlan>;
  // self-service
  myBalances(): Promise<LeaveBalance[]>;
  myTypes(): Promise<LeaveType[]>;
  myRequests(): Promise<LeaveRequest[]>;
  apply(input: LeaveRequestInput): Promise<LeaveRequest>;
  cancelMine(id: string, version: number, reason?: string): Promise<LeaveRequest>;
  approvals(): Promise<LeaveRequest[]>;
  decideApproval(id: string, version: number, approve: boolean, reason?: string): Promise<LeaveRequest>;
  /** My attendance days in `[from, to]` (My HR → Attendance). */
  myAttendance(from: string, to: string): Promise<AttendanceDay[]>;
  /** My teaching load and active workload plan (My HR → Workload). */
  myWorkload(): Promise<StaffWorkload>;
}

const B = "/api/v1/people";
export const PATHS = {
  policies: `${B}/institutions/{id}/leave-policies`,
  types: `${B}/leave-policies/{id}/leave-types`,
  submit: `${B}/leave-policies/{id}/submit`,
  activate: `${B}/leave-policies/{id}/activate`,
  supersede: `${B}/leave-policies/{id}/supersede`,
  policyDecision: `${B}/leave-policies/{id}/decision`,
  requests: `${B}/institutions/{id}/leave-requests`,
  requestDecision: `${B}/leave-requests/{id}/decision`,
  cancel: `${B}/leave-requests/{id}/cancel`,
  consume: `${B}/leave-requests/{id}/consume`,
  sweep: `${B}/institutions/{id}/leave-consumption`,
  accruals: `${B}/institutions/{id}/leave-accruals`,
  yearEnd: `${B}/institutions/{id}/leave-year-end`,
  staff: `${B}/institutions/{id}/staff`,
  balances: `${B}/staff/{id}/leave-balances`,
  adjustments: `${B}/staff/{id}/leave-adjustments`,
  encashments: `${B}/staff/{id}/leave-encashments`,
  exceptions: `${B}/institutions/{id}/leave-exceptions`,
  review: `${B}/leave-exceptions/{id}/review`,
  resolve: `${B}/leave-exceptions/{id}/resolve`,
  attendance: `${B}/institutions/{id}/attendance`,
  bulk: `${B}/institutions/{id}/attendance/bulk`,
  correct: `${B}/attendance-days/{id}/correct`,
  longLeave: `${B}/engagements/{id}/long-leave`,
  llStart: `${B}/long-leaves/{id}/start`,
  llEnd: `${B}/long-leaves/{id}/end`,
  workload: `${B}/staff/{id}/workload`,
  plans: `${B}/staff/{id}/workload-plans`,
  planSubmit: `${B}/workload-plans/{id}/submit`,
  planDecision: `${B}/workload-plans/{id}/decision`,
  myBalances: `${B}/me/leave-balances`,
  myTypes: `${B}/me/leave-types`,
  myRequests: `${B}/me/leave-requests`,
  myCancel: `${B}/me/leave-requests/{id}/cancel`,
  approvals: `${B}/me/leave-approvals`,
  approvalDecision: `${B}/me/leave-approvals/{id}/decision`,
  myAttendance: `${B}/me/attendance`,
  myWorkload: `${B}/me/workload`,
} as const satisfies Record<string, keyof paths>;

const POLICY_CMD: Record<PolicyCommand, string> = { submit: PATHS.submit, activate: PATHS.activate, supersede: PATHS.supersede };

type Fetch = (input: string, init: RequestInit) => Promise<Response>;

interface RequestOptions {
  body?: unknown;
  ifMatch?: number;
  idempotent?: boolean;
}

const decision = (approve: boolean, reason?: string) => ({ decision: approve ? "approve" : "reject", reason: reason || undefined });

export function createHttpLeaveApi(fetchImpl: Fetch = (i, init) => fetch(i, init), baseUrl: string = import.meta.env.VITE_API_BASE ?? ""): LeaveApi {
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
  const id = (template: string, value: string) => fill(template, { id: value });
  return {
    policies: (inst) => items(id(PATHS.policies, inst)),
    createPolicy: (inst, input) => request("POST", id(PATHS.policies, inst), { body: input, idempotent: true }),
    addType: (policy, input) => request("POST", id(PATHS.types, policy), { body: input, idempotent: true }),
    policyCommand: (policy, v, cmd, reason) => request("POST", id(POLICY_CMD[cmd], policy), { body: { reason: reason || undefined }, ifMatch: v }),
    decidePolicy: (policy, v, approve, reason) => request("POST", id(PATHS.policyDecision, policy), { body: decision(approve, reason), ifMatch: v }),
    requests: (inst, status) => request("GET", `${id(PATHS.requests, inst)}${status ? `?status=${encodeURIComponent(status)}` : ""}`),
    requestLeave: (inst, input) => request("POST", id(PATHS.requests, inst), { body: input, idempotent: true }),
    decideRequest: (rid, v, approve, reason) => request("POST", id(PATHS.requestDecision, rid), { body: decision(approve, reason), ifMatch: v }),
    cancelRequest: (rid, v, reason) => request("POST", id(PATHS.cancel, rid), { body: { reason: reason || undefined }, ifMatch: v }),
    consumeRequest: (rid, v) => request("POST", id(PATHS.consume, rid), { body: {}, ifMatch: v }),
    sweep: (inst) => request("POST", id(PATHS.sweep, inst), { body: {}, idempotent: true }),
    accruals: (inst) => items(id(PATHS.accruals, inst)),
    accrue: (inst, leaveYear, month) => request("POST", id(PATHS.accruals, inst), { body: { leaveYear, month }, idempotent: true }),
    yearEnds: (inst) => items(id(PATHS.yearEnd, inst)),
    yearEnd: (inst, leaveYear) => request("POST", id(PATHS.yearEnd, inst), { body: { leaveYear }, idempotent: true }),
    staff: (inst) => items(`${id(PATHS.staff, inst)}?limit=200`),
    balances: (staff, year) => items(`${id(PATHS.balances, staff)}${year ? `?leaveYear=${year}` : ""}`),
    adjust: (staff, input) => request("POST", id(PATHS.adjustments, staff), { body: input, idempotent: true }),
    encash: (staff, input) => request("POST", id(PATHS.encashments, staff), { body: input, idempotent: true }),
    exceptions: (inst) => items(id(PATHS.exceptions, inst)),
    raiseException: (inst, input) => request("POST", id(PATHS.exceptions, inst), { body: input, idempotent: true }),
    exceptionCommand: (eid, v, cmd, resolution) =>
      request("POST", id(cmd === "review" ? PATHS.review : PATHS.resolve, eid), { body: cmd === "review" ? {} : { resolution }, ifMatch: v }),
    grid: (inst, date) => request("GET", `${id(PATHS.attendance, inst)}?date=${encodeURIComponent(date)}`),
    bulk: (inst, workDate, entries) => request("POST", id(PATHS.bulk, inst), { body: { workDate, entries }, idempotent: true }),
    correct: (day, v, status, remark) => request("POST", id(PATHS.correct, day), { body: { status, remark }, ifMatch: v }),
    longLeaves: (eng) => items(id(PATHS.longLeave, eng)),
    planLongLeave: (eng, input) => request("POST", id(PATHS.longLeave, eng), { body: input, idempotent: true }),
    longLeaveCommand: (ll, v, cmd) => request("POST", id(cmd === "start" ? PATHS.llStart : PATHS.llEnd, ll), { body: {}, ifMatch: v }),
    workload: (staff) => request("GET", id(PATHS.workload, staff)),
    workloadPlans: (staff) => items(id(PATHS.plans, staff)),
    createPlan: (staff, input) => request("POST", id(PATHS.plans, staff), { body: input, idempotent: true }),
    submitPlan: (plan, v, exceptionReason) => request("POST", id(PATHS.planSubmit, plan), { body: { exceptionReason: exceptionReason || undefined }, ifMatch: v }),
    decidePlan: (plan, v, approve, reason) => request("POST", id(PATHS.planDecision, plan), { body: decision(approve, reason), ifMatch: v }),
    myBalances: () => items(PATHS.myBalances),
    myTypes: () => items(PATHS.myTypes),
    myRequests: () => items(PATHS.myRequests),
    apply: (input) => request("POST", PATHS.myRequests, { body: input, idempotent: true }),
    cancelMine: (rid, v, reason) => request("POST", id(PATHS.myCancel, rid), { body: { reason: reason || undefined }, ifMatch: v }),
    approvals: () => items(PATHS.approvals),
    decideApproval: (rid, v, approve, reason) => request("POST", id(PATHS.approvalDecision, rid), { body: decision(approve, reason), ifMatch: v }),
    myAttendance: (from, to) => items(`${PATHS.myAttendance}?from=${encodeURIComponent(from)}&to=${encodeURIComponent(to)}`),
    myWorkload: () => request("GET", PATHS.myWorkload),
  };
}
