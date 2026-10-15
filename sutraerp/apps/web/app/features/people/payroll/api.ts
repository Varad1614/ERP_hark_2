/**
 * Slice-7C API layer: one interface, an HTTP implementation (CSRF, Idempotency-Key on creates and
 * bulk commands, If-Match on commands, problem+json → `ApiProblem`) and an in-memory mock
 * (`./mock`). Every 7C path template is checked against the generated OpenAPI `paths`.
 */
import { ApiProblem, isProblem, isUnsafeMethod, newIdempotencyKey, type paths } from "@sutra/api-client";
import { getCsrfToken } from "../../../lib/api";
import { fill } from "../../institution/api";
import type { BankAccount, BankAccountInput, BulkSummary, Decision, DisbursementFile, DisbursementKind, FinalSettlement, Items, PayComponentInput, PayrollConfig, PayrollDisbursement, PayrollPeriod, PayrollResult, PayrollRun, Payslip, RunCommand, SalaryAssignment, SalaryStructure } from "./types";

export interface PayrollApi {
  configs(institutionId: string): Promise<PayrollConfig[]>;
  createConfig(institutionId: string, input: { name: string; effectiveFrom: string; copyFrom?: string }): Promise<PayrollConfig>;
  configCommand(id: string, version: number, cmd: "submit" | "activate"): Promise<PayrollConfig>;
  periods(institutionId: string): Promise<PayrollPeriod[]>;
  createPeriod(institutionId: string, year: number, month: number): Promise<PayrollPeriod>;
  runs(periodId: string): Promise<PayrollRun[]>;
  createRun(periodId: string, kind: "regular" | "retro"): Promise<PayrollRun>;
  runCommand(id: string, version: number, cmd: RunCommand, reason?: string): Promise<PayrollRun>;
  decideRun(id: string, version: number, decision: Decision, reason?: string): Promise<PayrollRun>;
  results(runId: string): Promise<PayrollResult[]>;
  issuePayslips(runId: string): Promise<BulkSummary>;
  settlements(institutionId: string): Promise<FinalSettlement[]>;
  myPayslips(): Promise<Payslip[]>;
  // integration: configuration lines, salary setup and final settlements from the screen
  addComponent(configId: string, version: number, input: PayComponentInput): Promise<PayrollConfig>;
  decideConfig(id: string, version: number, decision: Decision, reason?: string): Promise<PayrollConfig>;
  structures(institutionId: string): Promise<SalaryStructure[]>;
  createStructure(institutionId: string, input: { code: string; name: string; grade?: string; lines: Array<{ componentCode: string; amount?: string; rate?: string }> }): Promise<SalaryStructure>;
  assignments(engagementId: string): Promise<SalaryAssignment[]>;
  assignSalary(engagementId: string, input: { structureId?: string; effectiveFrom: string; reason?: string }): Promise<SalaryAssignment>;
  createSettlement(engagementId: string, input: { recoveries?: Array<{ code: string; name: string; amount: string }>; notes?: string }): Promise<FinalSettlement>;
  settlementCommand(id: string, version: number, cmd: "calculate" | "complete"): Promise<FinalSettlement>;
  decideSettlement(id: string, version: number, decision: Decision, reason?: string): Promise<FinalSettlement>;
  // DEF-E-007: payroll disbursement (bank transfer file, mark paid → Finance payout posting)
  disbursements(institutionId: string): Promise<PayrollDisbursement[]>;
  createDisbursement(runId: string, kind: DisbursementKind): Promise<PayrollDisbursement>;
  decideDisbursement(id: string, version: number, decision: Decision, reason?: string): Promise<PayrollDisbursement>;
  disbursementFile(id: string): Promise<DisbursementFile>;
  markDisbursementPaid(id: string, version: number, reference: string, paidOn?: string): Promise<PayrollDisbursement>;
  putBankAccount(staffId: string, input: BankAccountInput): Promise<BankAccount>;
}

const B = "/api/v1/people";
export const PATHS = {
  configs: `${B}/institutions/{id}/payroll-configs`,
  configSubmit: `${B}/payroll-configs/{id}/submit`,
  configActivate: `${B}/payroll-configs/{id}/activate`,
  periods: `${B}/institutions/{id}/payroll-periods`,
  runs: `${B}/payroll-periods/{id}/runs`,
  calculate: `${B}/payroll-runs/{id}/calculate`,
  validate: `${B}/payroll-runs/{id}/validate`,
  submit: `${B}/payroll-runs/{id}/submit`,
  decision: `${B}/payroll-runs/{id}/decision`,
  finalise: `${B}/payroll-runs/{id}/finalise`,
  post: `${B}/payroll-runs/{id}/post`,
  close: `${B}/payroll-runs/{id}/close`,
  reprocess: `${B}/payroll-runs/{id}/reprocess`,
  results: `${B}/payroll-runs/{id}/results`,
  issue: `${B}/payroll-runs/{id}/payslips/issue`,
  settlements: `${B}/institutions/{id}/final-settlements`,
  mePayslips: `${B}/me/payslips`,
  components: `${B}/payroll-configs/{id}/components`,
  configDecision: `${B}/payroll-configs/{id}/decision`,
  structures: `${B}/institutions/{id}/salary-structures`,
  assignments: `${B}/engagements/{id}/salary-assignments`,
  settlementCreate: `${B}/engagements/{id}/final-settlement`,
  settlementCalculate: `${B}/final-settlements/{id}/calculate`,
  settlementComplete: `${B}/final-settlements/{id}/complete`,
  settlementDecision: `${B}/final-settlements/{id}/decision`,
  disbursements: `${B}/institutions/{id}/payroll-disbursements`,
  runDisbursements: `${B}/payroll-runs/{id}/disbursements`,
  disbursementDecision: `${B}/payroll-disbursements/{id}/decision`,
  disbursementFile: `${B}/payroll-disbursements/{id}/file`,
  disbursementPaid: `${B}/payroll-disbursements/{id}/mark-paid`,
  bankAccount: `${B}/staff/{id}/bank-account`,
} as const satisfies Record<string, keyof paths>;

const RUN_CMD: Record<RunCommand, string> = {
  calculate: PATHS.calculate,
  validate: PATHS.validate,
  submit: PATHS.submit,
  finalise: PATHS.finalise,
  post: PATHS.post,
  close: PATHS.close,
  reprocess: PATHS.reprocess,
};

type Fetch = (input: string, init: RequestInit) => Promise<Response>;

interface RequestOptions {
  body?: unknown;
  ifMatch?: number;
  idempotent?: boolean;
}

export function createHttpPayrollApi(fetchImpl: Fetch = (i, init) => fetch(i, init), baseUrl: string = import.meta.env.VITE_API_BASE ?? ""): PayrollApi {
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
  /** GET of a text file (the bank transfer CSV); problem+json errors still become `ApiProblem`. */
  async function file(path: string): Promise<DisbursementFile> {
    const res = await fetchImpl(`${baseUrl}${path}`, { method: "GET", headers: { Accept: "text/csv, application/problem+json" }, credentials: "include" });
    const text = await res.text();
    if (!res.ok) {
      let data: unknown = undefined;
      try {
        data = JSON.parse(text);
      } catch {
        data = undefined;
      }
      if (isProblem(data)) throw new ApiProblem(data);
      throw new ApiProblem({ type: "about:blank", title: `Request failed (${res.status})`, status: res.status, errorCode: `http-${res.status}` });
    }
    const disposition = res.headers.get("Content-Disposition") ?? "";
    const fileName = /filename="([^"]+)"/.exec(disposition)?.[1] ?? "payroll-disbursement.csv";
    return { fileName, content: text };
  }
  const items = async <T>(path: string) => (await request<Items<T>>("GET", path)).items;
  const id = (i: string) => ({ id: i });
  return {
    configs: (i) => items(fill(PATHS.configs, id(i))),
    createConfig: (i, input) => request("POST", fill(PATHS.configs, id(i)), { body: input, idempotent: true }),
    configCommand: (i, v, cmd) => request("POST", fill(cmd === "submit" ? PATHS.configSubmit : PATHS.configActivate, id(i)), { ifMatch: v }),
    periods: (i) => items(fill(PATHS.periods, id(i))),
    createPeriod: (i, year, month) => request("POST", fill(PATHS.periods, id(i)), { body: { year, month }, idempotent: true }),
    runs: (p) => items(fill(PATHS.runs, id(p))),
    createRun: (p, kind) => request("POST", fill(PATHS.runs, id(p)), { body: { kind }, idempotent: true }),
    runCommand: (i, v, cmd, reason) =>
      request("POST", fill(RUN_CMD[cmd], id(i)), { ifMatch: v, idempotent: cmd === "calculate", body: reason ? { reason } : {} }),
    decideRun: (i, v, decision, reason) => request("POST", fill(PATHS.decision, id(i)), { ifMatch: v, body: { decision, reason } }),
    results: (r) => items(fill(PATHS.results, id(r))),
    issuePayslips: (r) => request("POST", fill(PATHS.issue, id(r)), { idempotent: true }),
    settlements: (i) => items(fill(PATHS.settlements, id(i))),
    myPayslips: () => items(PATHS.mePayslips),
    addComponent: (c, v, input) => request("POST", fill(PATHS.components, id(c)), { ifMatch: v, body: input }),
    decideConfig: (c, v, decision, reason) => request("POST", fill(PATHS.configDecision, id(c)), { ifMatch: v, body: { decision, reason } }),
    structures: (i) => items(fill(PATHS.structures, id(i))),
    createStructure: (i, input) => request("POST", fill(PATHS.structures, id(i)), { body: input, idempotent: true }),
    assignments: (e) => items(fill(PATHS.assignments, id(e))),
    assignSalary: (e, input) => request("POST", fill(PATHS.assignments, id(e)), { body: input, idempotent: true }),
    createSettlement: (e, input) => request("POST", fill(PATHS.settlementCreate, id(e)), { body: input, idempotent: true }),
    settlementCommand: (f, v, cmd) => request("POST", fill(cmd === "calculate" ? PATHS.settlementCalculate : PATHS.settlementComplete, id(f)), { ifMatch: v }),
    decideSettlement: (f, v, decision, reason) => request("POST", fill(PATHS.settlementDecision, id(f)), { ifMatch: v, body: { decision, reason } }),
    disbursements: (i) => items(fill(PATHS.disbursements, id(i))),
    createDisbursement: (r, kind) => request("POST", fill(PATHS.runDisbursements, id(r)), { body: { kind }, idempotent: true }),
    decideDisbursement: (d, v, decision, reason) => request("POST", fill(PATHS.disbursementDecision, id(d)), { ifMatch: v, body: { decision, reason } }),
    disbursementFile: (d) => file(fill(PATHS.disbursementFile, id(d))),
    markDisbursementPaid: (d, v, reference, paidOn) => request("POST", fill(PATHS.disbursementPaid, id(d)), { ifMatch: v, body: { reference, paidOn: paidOn || undefined } }),
    putBankAccount: (s, input) => request("PUT", fill(PATHS.bankAccount, id(s)), { body: input }),
  };
}
