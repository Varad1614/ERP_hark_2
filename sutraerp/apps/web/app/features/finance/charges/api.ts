/**
 * Slice-5A API layer. Same conventions as `features/institution/api.ts`: one interface, an HTTP
 * implementation (CSRF, Idempotency-Key on creates and money-moving commands, If-Match on
 * commands, problem+json → `ApiProblem`) and an in-memory mock (`./mock`). Every path template
 * is checked against the generated OpenAPI `paths`.
 */
import { ApiProblem, isProblem, isUnsafeMethod, newIdempotencyKey, type paths } from "@sutra/api-client";
import { getCsrfToken } from "../../../lib/api";
import { fill } from "../../institution/api";
import type {
  Adjustment,
  BillingEnrolment,
  BillingFrequency,
  BillingPlan,
  BillingRun,
  BillingRunRequest,
  BillingRunSummary,
  BillingSchedule,
  AdjustmentKind,
  AidAllocation,
  AidDecision,
  AidReconciliation,
  ChargeCommand,
  ChargeDetail,
  ChargeList,
  CreateCharge,
  Dispute,
  FeeCategory,
  FeeHead,
  FeeStructure,
  GenerationResult,
  Items,
  LineInput,
  PolicyKind,
  PolicyVersion,
} from "./types";

export interface ChargesApi {
  feeHeads(institutionId: string): Promise<FeeHead[]>;
  createFeeHead(institutionId: string, input: { code: string; name: string; category: FeeCategory; refundable?: boolean }): Promise<FeeHead>;
  structures(institutionId: string): Promise<FeeStructure[]>;
  createStructure(institutionId: string, input: { code: string; name: string; dueDate?: string; lines: LineInput[] }): Promise<FeeStructure>;
  structureCommand(id: string, version: number, cmd: "submit" | "publish" | "supersede"): Promise<FeeStructure>;
  generate(id: string, studentIds: string[]): Promise<GenerationResult>;
  charges(institutionId: string, q?: { status?: string; q?: string }): Promise<ChargeList>;
  charge(id: string): Promise<ChargeDetail>;
  createCharge(institutionId: string, input: CreateCharge): Promise<ChargeDetail>;
  chargeCommand(id: string, version: number, cmd: ChargeCommand, reason?: string): Promise<ChargeDetail>;
  revise(id: string, version: number, input: { reason: string; newAmount?: string; dueDate?: string; description?: string }): Promise<ChargeDetail>;
  setInstalments(id: string, input: { count: number; firstDueDate: string; intervalMonths?: number }): Promise<ChargeDetail>;
  disputes(chargeId: string): Promise<Dispute[]>;
  raiseDispute(chargeId: string, reason: string): Promise<Dispute>;
  decideDispute(id: string, version: number, input: { decision: "approve" | "reject"; reason?: string; resolution?: "none" | "credit" | "waive"; creditAmount?: string }): Promise<Dispute>;
  myCharges(): Promise<ChargeDetail[]>;
  // adjustments
  adjustments(institutionId: string): Promise<Adjustment[]>;
  createAdjustment(chargeId: string, input: { kind: AdjustmentKind; amount: string; reason: string }): Promise<Adjustment>;
  submitAdjustment(id: string, version: number): Promise<Adjustment>;
  decideAdjustment(id: string, version: number, decision: "approve" | "reject", reason?: string): Promise<Adjustment>;
  reverseAdjustment(id: string, version: number, reason: string): Promise<Adjustment>;
  correctAdjustment(id: string, version: number, input: { amount: string; reason: string; kind?: AdjustmentKind }): Promise<Adjustment>;
  // aid
  aidDecisions(institutionId: string): Promise<AidDecision[]>;
  aidAllocations(institutionId: string): Promise<AidAllocation[]>;
  allocate(institutionId: string, eligibilityId: string): Promise<AidAllocation>;
  approveAid(id: string, version: number, decision: "approve" | "reject", reason?: string): Promise<AidAllocation>;
  disburse(id: string, version: number, input: { mode: "charge_adjustment" | "cash"; chargeId?: string; amount?: string }): Promise<AidAllocation>;
  renewAid(id: string, version: number): Promise<AidAllocation>;
  revokeAid(id: string, version: number, input: { reason: string; recover: boolean }): Promise<AidAllocation>;
  reconciliations(institutionId: string): Promise<AidReconciliation[]>;
  reconcile(institutionId: string): Promise<AidReconciliation>;
  myAid(): Promise<AidAllocation[]>;
  // configuration + rules
  policies(institutionId: string, kind: PolicyKind): Promise<PolicyVersion[]>;
  createPolicy(institutionId: string, kind: PolicyKind, input: { ruleKey?: string; values: Record<string, unknown>; changeNote?: string }): Promise<PolicyVersion>;
  policyCommand(kind: PolicyKind, id: string, version: number, cmd: "submit" | "activate"): Promise<PolicyVersion>;
  rollback(kind: PolicyKind, id: string, reason: string): Promise<PolicyVersion>;
  // ERP_EXTENSIONS 1E — periodic billing runs
  billingPlans(institutionId: string): Promise<BillingPlan[]>;
  createBillingPlan(institutionId: string, input: { code: string; name: string; feeHeadId: string; amount: string; frequency: BillingFrequency; proration?: "none" | "daily"; description?: string }): Promise<BillingPlan>;
  updateBillingPlan(id: string, input: { name?: string; amount?: string; proration?: "none" | "daily"; status?: "active" | "retired"; description?: string }): Promise<BillingPlan>;
  billingEnrolments(planId: string): Promise<BillingEnrolment[]>;
  enrol(planId: string, input: { studentId: string; amount?: string | null; startDate?: string | null; endDate?: string | null; onHold?: boolean }): Promise<BillingEnrolment>;
  unenrol(planId: string, studentId: string): Promise<void>;
  billingRun(institutionId: string, input: BillingRunRequest, preview: boolean): Promise<BillingRun>;
  billingRuns(institutionId: string): Promise<BillingRunSummary[]>;
  billingSchedules(institutionId: string): Promise<BillingSchedule[]>;
  setBillingSchedule(institutionId: string, frequency: BillingFrequency, input: { enabled: boolean; runDay: number; dueDays: number }): Promise<BillingSchedule>;
}

const B = "/api/v1/finance";
export const PATHS = {
  feeHeads: `${B}/institutions/{id}/fee-heads`,
  structures: `${B}/institutions/{id}/fee-structures`,
  fsSubmit: `${B}/fee-structures/{id}/submit`,
  fsPublish: `${B}/fee-structures/{id}/publish`,
  fsSupersede: `${B}/fee-structures/{id}/supersede`,
  fsGenerate: `${B}/fee-structures/{id}/generate-charges`,
  charges: `${B}/institutions/{id}/charges`,
  charge: `${B}/charges/{id}`,
  cValidate: `${B}/charges/{id}/validate`,
  cPublish: `${B}/charges/{id}/publish`,
  cSuspend: `${B}/charges/{id}/suspend`,
  cResume: `${B}/charges/{id}/resume`,
  cCancel: `${B}/charges/{id}/cancel`,
  cClose: `${B}/charges/{id}/close`,
  cArchive: `${B}/charges/{id}/archive`,
  cRevise: `${B}/charges/{id}/revise`,
  instalments: `${B}/charges/{id}/instalments`,
  disputes: `${B}/charges/{id}/disputes`,
  dDecide: `${B}/disputes/{id}/decision`,
  meCharges: `${B}/me/charges`,
  chargeAdjustments: `${B}/charges/{id}/adjustments`,
  adjustments: `${B}/institutions/{id}/adjustments`,
  aSubmit: `${B}/adjustments/{id}/submit`,
  aDecide: `${B}/adjustments/{id}/decision`,
  aReverse: `${B}/adjustments/{id}/reverse`,
  aCorrect: `${B}/adjustments/{id}/correct`,
  aidDecisions: `${B}/institutions/{id}/aid-decisions`,
  aidAllocations: `${B}/institutions/{id}/aid-allocations`,
  aidApprove: `${B}/aid-allocations/{id}/approve`,
  aidDisburse: `${B}/aid-allocations/{id}/disburse`,
  aidRenew: `${B}/aid-allocations/{id}/renew`,
  aidRevoke: `${B}/aid-allocations/{id}/revoke`,
  aidRecs: `${B}/institutions/{id}/aid-reconciliations`,
  meAid: `${B}/me/aid`,
  configs: `${B}/institutions/{id}/finance-config`,
  cfgSubmit: `${B}/finance-config/{id}/submit`,
  cfgActivate: `${B}/finance-config/{id}/activate`,
  cfgRollback: `${B}/finance-config/{id}/rollback`,
  rules: `${B}/institutions/{id}/finance-rules`,
  ruleSubmit: `${B}/finance-rules/{id}/submit`,
  ruleActivate: `${B}/finance-rules/{id}/activate`,
  ruleRollback: `${B}/finance-rules/{id}/rollback`,
  billingPlans: `${B}/institutions/{id}/billing-plans`,
  billingPlan: `${B}/billing-plans/{id}`,
  billingEnrolments: `${B}/billing-plans/{id}/enrolments`,
  billingEnrolment: `${B}/billing-plans/{id}/enrolments/{studentId}`,
  billingRuns: `${B}/institutions/{id}/billing-runs`,
  billingRunPreview: `${B}/institutions/{id}/billing-runs/preview`,
  billingSchedules: `${B}/institutions/{id}/billing-schedules`,
  billingSchedule: `${B}/institutions/{id}/billing-schedules/{frequency}`,
} as const satisfies Record<string, keyof paths>;

type Fetch = (input: string, init: RequestInit) => Promise<Response>;

interface RequestOptions {
  body?: unknown;
  ifMatch?: number;
  idempotent?: boolean;
}

const CHARGE_CMD: Record<ChargeCommand, string> = {
  validate: PATHS.cValidate,
  publish: PATHS.cPublish,
  suspend: PATHS.cSuspend,
  resume: PATHS.cResume,
  cancel: PATHS.cCancel,
  close: PATHS.cClose,
  archive: PATHS.cArchive,
};
const FS_CMD = { submit: PATHS.fsSubmit, publish: PATHS.fsPublish, supersede: PATHS.fsSupersede } as const;
const POLICY = {
  config: { list: PATHS.configs, submit: PATHS.cfgSubmit, activate: PATHS.cfgActivate, rollback: PATHS.cfgRollback },
  rule: { list: PATHS.rules, submit: PATHS.ruleSubmit, activate: PATHS.ruleActivate, rollback: PATHS.ruleRollback },
} as const;

export function createHttpChargesApi(fetchImpl: Fetch = (i, init) => fetch(i, init), baseUrl: string = import.meta.env.VITE_API_BASE ?? ""): ChargesApi {
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
  const id = (i: string) => ({ id: i });
  const reasonBody = (reason?: string) => (reason ? { reason } : {});
  return {
    feeHeads: (i) => items(fill(PATHS.feeHeads, id(i))),
    createFeeHead: (i, input) => request("POST", fill(PATHS.feeHeads, id(i)), { body: input, idempotent: true }),
    structures: (i) => items(fill(PATHS.structures, id(i))),
    createStructure: (i, input) => request("POST", fill(PATHS.structures, id(i)), { body: input, idempotent: true }),
    structureCommand: (i, v, cmd) => request("POST", fill(FS_CMD[cmd], id(i)), { ifMatch: v }),
    generate: (i, studentIds) => request("POST", fill(PATHS.fsGenerate, id(i)), { body: { studentIds }, idempotent: true }),
    charges: (i, q = {}) => {
      const p = new URLSearchParams();
      if (q.status) p.set("status", q.status);
      if (q.q) p.set("q", q.q);
      const qs = p.toString();
      return request("GET", fill(PATHS.charges, id(i)) + (qs ? `?${qs}` : ""));
    },
    charge: (i) => request("GET", fill(PATHS.charge, id(i))),
    createCharge: (i, input) => request("POST", fill(PATHS.charges, id(i)), { body: input, idempotent: true }),
    chargeCommand: (i, v, cmd, reason) => request("POST", fill(CHARGE_CMD[cmd], id(i)), { body: reasonBody(reason), ifMatch: v, idempotent: cmd === "publish" }),
    revise: (i, v, input) => request("POST", fill(PATHS.cRevise, id(i)), { body: input, ifMatch: v, idempotent: true }),
    setInstalments: (i, input) => request("POST", fill(PATHS.instalments, id(i)), { body: input, idempotent: true }),
    disputes: (c) => items(fill(PATHS.disputes, id(c))),
    raiseDispute: (c, reason) => request("POST", fill(PATHS.disputes, id(c)), { body: { reason }, idempotent: true }),
    decideDispute: (i, v, input) => request("POST", fill(PATHS.dDecide, id(i)), { body: input, ifMatch: v, idempotent: true }),
    myCharges: () => items(PATHS.meCharges),
    adjustments: (i) => items(fill(PATHS.adjustments, id(i))),
    createAdjustment: (c, input) => request("POST", fill(PATHS.chargeAdjustments, id(c)), { body: input, idempotent: true }),
    submitAdjustment: (i, v) => request("POST", fill(PATHS.aSubmit, id(i)), { ifMatch: v, idempotent: true }),
    decideAdjustment: (i, v, decision, reason) => request("POST", fill(PATHS.aDecide, id(i)), { body: { decision, reason }, ifMatch: v, idempotent: true }),
    reverseAdjustment: (i, v, reason) => request("POST", fill(PATHS.aReverse, id(i)), { body: { reason }, ifMatch: v, idempotent: true }),
    correctAdjustment: (i, v, input) => request("POST", fill(PATHS.aCorrect, id(i)), { body: input, ifMatch: v, idempotent: true }),
    aidDecisions: (i) => items(fill(PATHS.aidDecisions, id(i))),
    aidAllocations: (i) => items(fill(PATHS.aidAllocations, id(i))),
    allocate: (i, eligibilityId) => request("POST", fill(PATHS.aidAllocations, id(i)), { body: { eligibilityId }, idempotent: true }),
    approveAid: (i, v, decision, reason) => request("POST", fill(PATHS.aidApprove, id(i)), { body: { decision, reason }, ifMatch: v }),
    disburse: (i, v, input) => request("POST", fill(PATHS.aidDisburse, id(i)), { body: input, ifMatch: v, idempotent: true }),
    renewAid: (i, v) => request("POST", fill(PATHS.aidRenew, id(i)), { ifMatch: v }),
    revokeAid: (i, v, input) => request("POST", fill(PATHS.aidRevoke, id(i)), { body: input, ifMatch: v, idempotent: true }),
    reconciliations: (i) => items(fill(PATHS.aidRecs, id(i))),
    reconcile: (i) => request("POST", fill(PATHS.aidRecs, id(i)), { body: {}, idempotent: true }),
    myAid: () => items(PATHS.meAid),
    policies: (i, kind) => items(fill(POLICY[kind].list, id(i))),
    createPolicy: (i, kind, input) => request("POST", fill(POLICY[kind].list, id(i)), { body: input, idempotent: true }),
    policyCommand: (kind, i, v, cmd) => request("POST", fill(POLICY[kind][cmd], id(i)), { ifMatch: v }),
    rollback: (kind, i, reason) => request("POST", fill(POLICY[kind].rollback, id(i)), { body: { reason }, idempotent: true }),
    billingPlans: (i) => items(fill(PATHS.billingPlans, id(i))),
    createBillingPlan: (i, input) => request("POST", fill(PATHS.billingPlans, id(i)), { body: input }),
    updateBillingPlan: (i, input) => request("PATCH", fill(PATHS.billingPlan, id(i)), { body: input }),
    billingEnrolments: (p) => items(fill(PATHS.billingEnrolments, id(p))),
    enrol: (p, input) => request("PUT", fill(PATHS.billingEnrolments, id(p)), { body: input }),
    unenrol: async (p, studentId) => {
      await request("DELETE", fill(PATHS.billingEnrolment, { id: p, studentId }));
    },
    billingRun: (i, input, preview) => request("POST", fill(preview ? PATHS.billingRunPreview : PATHS.billingRuns, id(i)), { body: input }),
    billingRuns: (i) => items(fill(PATHS.billingRuns, id(i))),
    billingSchedules: (i) => items(fill(PATHS.billingSchedules, id(i))),
    setBillingSchedule: (i, frequency, input) => request("PUT", fill(PATHS.billingSchedule, { id: i, frequency }), { body: input }),
  };
}
