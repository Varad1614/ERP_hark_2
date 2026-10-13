/**
 * In-memory slice-7C API for mock mode and tests: an active configuration, one open period with
 * a calculated regular run, and an issued payslip for the signed-in staff member. Commands move
 * statuses the way the API does (enough for screens and tests; the API stays authoritative).
 */
import { ApiProblem } from "@sutra/api-client";
import type { PayrollApi } from "./api";
import type { FinalSettlement, PayrollConfig, PayrollPeriod, PayrollResult, PayrollRun, Payslip, RunCommand, SalaryAssignment, SalaryStructure } from "./types";

const conflict = (title: string) =>
  new ApiProblem({ type: "about:blank", title, status: 409, errorCode: "conflict" } as ConstructorParameters<typeof ApiProblem>[0]);

const NEXT: Record<RunCommand, Record<string, string>> = {
  calculate: { created: "calculated", calculated: "calculated", reprocessing: "calculated" },
  validate: { calculated: "validating" },
  submit: { validating: "approved" },
  finalise: { approved: "finalized" },
  post: { finalized: "posted" },
  close: { posted: "closed" },
  reprocess: { failed: "reprocessing" },
};

export function createMockPayrollApi(): PayrollApi {
  let configs: PayrollConfig[] = [
    {
      id: "cfg-1",
      institutionId: "inst-1",
      versionNo: 1,
      name: "Payroll 2026",
      effectiveFrom: "2026-04-01",
      rounding: "round2",
      encashmentDivisor: 30,
      status: "active",
      components: [],
      statutoryRules: [],
      version: 4,
    },
  ];
  let periods: PayrollPeriod[] = [{ id: "per-1", institutionId: "inst-1", year: 2026, month: 11, periodStart: "2026-11-01", periodEnd: "2026-11-30", status: "open", version: 1 }];
  let runs: PayrollRun[] = [
    {
      id: "run-1",
      number: "PR-000001",
      periodId: "per-1",
      kind: "regular",
      status: "calculated",
      headcount: 1,
      grossEarnings: "43800.00",
      employeeDeductions: "2000.00",
      employerContributions: "1800.00",
      netPay: "41800.00",
      exceptions: [],
      version: 3,
    },
  ];
  const results: PayrollResult[] = [
    {
      id: "res-1",
      runId: "run-1",
      staffNumber: "EMP-000001",
      displayName: "Meera Shah",
      status: "calculated",
      revision: 1,
      lopDays: "3.00",
      payableDays: "27.00",
      grossEarnings: "43800.00",
      totalDeductions: "2000.00",
      netPay: "41800.00",
      lines: [],
      version: 1,
    },
  ];
  const mine: Payslip[] = [
    {
      id: "ps-1",
      number: "PS-000001",
      payrollRunId: "run-0",
      staffNumber: "EMP-000001",
      displayName: "Meera Shah",
      periodStart: "2026-10-01",
      periodEnd: "2026-10-31",
      earnings: [
        { code: "BASIC", name: "Basic pay", amount: "30000.00" },
        { code: "HRA", name: "House rent allowance", amount: "12000.00" },
      ],
      deductions: [{ code: "PF", name: "Provident fund", amount: "1800.00" }],
      grossEarnings: "42000.00",
      totalDeductions: "1800.00",
      netPay: "40200.00",
      lopDays: "0.00",
      status: "issued",
      version: 1,
    },
  ];
  const settlements: FinalSettlement[] = [];
  const structures: SalaryStructure[] = [{ id: "sst-1", institutionId: "inst-1", code: "AP-1", name: "Assistant Professor 1", grade: "AP-1", status: "active", lines: [{ componentCode: "BASIC", amount: "30000.00" }], version: 1 }];
  const assignments: SalaryAssignment[] = [];
  let n = 2;
  const bump = <T extends { version: number }>(x: T, patch: Partial<T>): T => ({ ...x, ...patch, version: x.version + 1 });
  const find = <T extends { id: string }>(xs: T[], id: string): T => {
    const x = xs.find((y) => y.id === id);
    if (!x) throw conflict("not found");
    return x;
  };
  const check = (x: { version: number }, v: number) => {
    if (x.version !== v) throw new ApiProblem({ type: "about:blank", title: "Version mismatch", status: 412, errorCode: "precondition-failed" } as ConstructorParameters<typeof ApiProblem>[0]);
  };
  return {
    configs: async () => configs,
    createConfig: async (institutionId, input) => {
      const c: PayrollConfig = { ...(configs[0] as PayrollConfig), id: `cfg-${n++}`, institutionId, name: input.name, effectiveFrom: input.effectiveFrom, versionNo: configs.length + 1, status: "draft", version: 1 };
      configs = [c, ...configs];
      return c;
    },
    configCommand: async (id, v, cmd) => {
      const c = find(configs, id);
      check(c, v);
      let next: PayrollConfig;
      if (cmd === "submit") {
        if (c.status !== "draft") throw conflict("only a draft can be submitted");
        next = bump(c, { status: "approved" });
      } else {
        if (c.status !== "approved") throw conflict("only an approved configuration can be activated");
        configs = configs.map((x) => (x.status === "active" ? bump(x, { status: "superseded" }) : x));
        next = bump(c, { status: "active" });
      }
      configs = configs.map((x) => (x.id === id ? next : x));
      return next;
    },
    periods: async () => periods,
    createPeriod: async (institutionId, year, month) => {
      if (periods.some((p) => p.year === year && p.month === month)) throw conflict("the payroll period exists");
      const mm = String(month).padStart(2, "0");
      const p: PayrollPeriod = { id: `per-${n++}`, institutionId, year, month, periodStart: `${year}-${mm}-01`, periodEnd: `${year}-${mm}-28`, status: "open", version: 1 };
      periods = [p, ...periods];
      return p;
    },
    runs: async (p) => runs.filter((r) => r.periodId === p),
    createRun: async (periodId, kind) => {
      if (kind === "regular" && runs.some((r) => r.periodId === periodId && r.kind === "regular")) throw conflict("the period already has a regular payroll run");
      const r: PayrollRun = { ...(runs[0] as PayrollRun), id: `run-${n}`, number: `PR-${String(n++).padStart(6, "0")}`, periodId, kind, status: "created", headcount: 0, version: 1 };
      runs = [...runs, r];
      return r;
    },
    runCommand: async (id, v, cmd) => {
      const r = find(runs, id);
      check(r, v);
      const to = NEXT[cmd][r.status];
      if (!to) throw conflict(`payroll run ${r.number} is ${r.status}`);
      const next = bump(r, { status: to });
      runs = runs.map((x) => (x.id === id ? next : x));
      return next;
    },
    decideRun: async (id, v, decision) => {
      const r = find(runs, id);
      check(r, v);
      if (r.status !== "pending_approval") throw conflict("only a pending run can be decided");
      const next = bump(r, { status: decision === "approve" ? "approved" : "calculated" });
      runs = runs.map((x) => (x.id === id ? next : x));
      return next;
    },
    results: async (runId) => results.filter((r) => r.runId === runId),
    issuePayslips: async (runId) => {
      const r = find(runs, runId);
      if (!["finalized", "posted", "closed"].includes(r.status)) throw conflict("payslips are issued from finalised results");
      return { processed: r.headcount, skipped: 0, failed: [] };
    },
    settlements: async () => settlements,
    myPayslips: async () => mine,
    addComponent: async (id, v, input) => {
      const c = find(configs, id);
      check(c, v);
      if (c.status !== "draft") throw conflict("only a draft configuration is edited");
      if (c.components.some((x) => x.code === input.code)) throw conflict(`component ${input.code} exists`);
      const next = bump(c, { components: [...c.components, { ...input, amount: input.amount ?? null, rate: input.rate ?? null, sequence: (c.components.length + 1) * 10 }] });
      configs = configs.map((x) => (x.id === id ? next : x));
      return next;
    },
    decideConfig: async (id, v, decision) => {
      const c = find(configs, id);
      check(c, v);
      const next = bump(c, { status: decision === "approve" ? "approved" : "draft" });
      configs = configs.map((x) => (x.id === id ? next : x));
      return next;
    },
    structures: async () => structures,
    createStructure: async (institutionId, input) => {
      if (structures.some((x) => x.code === input.code)) throw conflict(`structure ${input.code} exists`);
      const st: SalaryStructure = { id: `sst-${n++}`, institutionId, code: input.code, name: input.name, grade: input.grade ?? null, status: "active", lines: input.lines, version: 1 };
      structures.push(st);
      return st;
    },
    assignments: async (engagementId) => assignments.filter((a) => a.engagementId === engagementId),
    assignSalary: async (engagementId, input) => {
      const st = structures.find((x) => x.id === input.structureId) ?? structures[0];
      if (!st) throw conflict("no salary structure");
      const a: SalaryAssignment = { id: `sa-${n++}`, institutionId: st.institutionId, staffId: "stf-1", engagementId, structureId: st.id, structureCode: st.code, effectiveFrom: input.effectiveFrom, status: "active", version: 1 };
      assignments.push(a);
      return a;
    },
    createSettlement: async (engagementId) => {
      const f: FinalSettlement = { id: `fnf-${n++}`, engagementId, status: "initiated", lastWorkingDate: "2026-11-15", salaryAmount: "0.00", encashmentAmount: "0.00", recoveryAmount: "0.00", netPay: "0.00", version: 1 };
      settlements.push(f);
      return f;
    },
    settlementCommand: async (id, v, cmd) => {
      const f = find(settlements, id);
      check(f, v);
      Object.assign(f, cmd === "calculate" ? { status: "approved", salaryAmount: "22500.00", netPay: "22500.00" } : { status: "completed", number: "FNF-000001" }, { version: f.version + 1 });
      return { ...f };
    },
    decideSettlement: async (id, v, decision) => {
      const f = find(settlements, id);
      check(f, v);
      Object.assign(f, { status: decision === "approve" ? "approved" : "rejected", version: f.version + 1 });
      return { ...f };
    },
  };
}
