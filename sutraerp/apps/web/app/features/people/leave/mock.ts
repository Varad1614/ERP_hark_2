/**
 * In-memory stand-in of the slice-7B API (mock mode and tests): one active leave policy (CL / EL /
 * LOP), three staff members, a reporting line (Meera manages Ravi), leave requests that reserve on
 * approval and consume per month, attendance days, long leave and workload plans. Days stay
 * decimal strings; arithmetic runs in integer hundredths (never floats).
 */
import { ApiProblem } from "@sutra/api-client";
import type { LeaveApi } from "./api";
import type {
  AccrualRun,
  AttendanceDay,
  LeaveBalance,
  LeaveException,
  LeavePolicy,
  LeaveRequest,
  LeaveRequestInput,
  LeaveType,
  LongLeave,
  StaffRow,
  WorkloadPlan,
  YearEndRun,
} from "./types";

const conflict = (detail: string) => new ApiProblem({ type: "about:blank", title: "Conflict", status: 409, errorCode: "conflict", detail });

/** "2.50" → 250. */
export function toHundredths(s: string): number {
  const neg = s.trim().startsWith("-");
  const [i = "0", f = ""] = s.trim().replace("-", "").split(".");
  const v = Number(i || "0") * 100 + Number((f + "00").slice(0, 2));
  return neg ? -v : v;
}

/** 250 → "2.50". */
export function fromHundredths(n: number): string {
  const neg = n < 0;
  const a = Math.abs(n);
  return `${neg ? "-" : ""}${Math.trunc(a / 100)}.${String(a % 100).padStart(2, "0")}`;
}

/** Working days (Sundays off) between two ISO dates, in hundredths, with half days. */
export function leaveDays(start: string, end: string, halfStart = false, halfEnd = false): number {
  const s = new Date(`${start}T00:00:00Z`);
  const e = new Date(`${end}T00:00:00Z`);
  let total = 0;
  for (let d = new Date(s); d <= e; d.setUTCDate(d.getUTCDate() + 1)) {
    if (d.getUTCDay() === 0) continue;
    const iso = d.toISOString().slice(0, 10);
    total += (halfStart && iso === start) || (halfEnd && iso === end) ? 50 : 100;
  }
  return total;
}

const INST = "00000000-0000-7000-8000-0000000b7b01";
const POLICY = "00000000-0000-7000-8000-0000000b7b02";
const MEERA = "00000000-0000-7000-8000-0000000b7b10";
const RAVI = "00000000-0000-7000-8000-0000000b7b11";
const SANA = "00000000-0000-7000-8000-0000000b7b12";

export function createMockLeaveApi(): LeaveApi {
  let n = 100;
  const nid = () => `00000000-0000-7000-8000-${String(++n).padStart(12, "0")}`;
  let lv = 0;
  const type = (code: string, name: string, annual: string, accrual: LeaveType["accrualFrequency"], extra: Partial<LeaveType> = {}): LeaveType => ({
    id: nid(),
    policyId: POLICY,
    code,
    name,
    annualEntitlement: annual,
    accrualFrequency: accrual,
    carryForwardCap: "0.00",
    encashmentCap: "0.00",
    isPaid: true,
    requiresBalance: true,
    halfDayAllowed: true,
    applicableCategories: ["teaching", "non_teaching", "administrative"],
    ...extra,
  });
  const policies: LeavePolicy[] = [
    {
      id: POLICY,
      institutionId: INST,
      name: "Leave policy 2026",
      versionNo: 1,
      effectiveFrom: "2026-01-01",
      weeklyOffDays: [7],
      holidays: [],
      status: "active",
      validationFindings: [],
      leaveTypes: [
        type("CL", "Casual leave", "12.00", "yearly", { carryForwardCap: "5.00" }),
        type("EL", "Earned leave", "15.00", "monthly", { carryForwardCap: "30.00", encashmentCap: "10.00" }),
        type("LOP", "Loss of pay", "0.00", "none", { isPaid: false, requiresBalance: false }),
      ],
      version: 4,
    },
  ];
  const staff: StaffRow[] = [
    { id: MEERA, staffNumber: "EMP-000001", displayName: "Meera Shah", staffCategory: "teaching", status: "active", engagement: { id: nid(), status: "active", designation: "Associate Professor" } },
    { id: RAVI, staffNumber: "EMP-000002", displayName: "Ravi Kumar", staffCategory: "teaching", status: "active", engagement: { id: nid(), status: "active", designation: "Assistant Professor" } },
    { id: SANA, staffNumber: "EMP-000003", displayName: "Sana Iyer", staffCategory: "non_teaching", status: "active", engagement: { id: nid(), status: "active", designation: "Office superintendent" } },
  ];
  const managerOf: Record<string, string> = { [RAVI]: MEERA };
  // ledger: staff → code → hundredths available / reserved / consumed / entitled
  const ledger = new Map<string, { entitled: number; reserved: number; consumed: number }>();
  const key = (s: string, code: string) => `${s}:${code}`;
  const bal = (s: string, code: string) => {
    const k = key(s, code);
    if (!ledger.has(k)) ledger.set(k, { entitled: 0, reserved: 0, consumed: 0 });
    return ledger.get(k)!;
  };
  for (const s of staff) {
    bal(s.id, "CL").entitled = 1200;
    bal(s.id, "EL").entitled = 1250;
  }
  const requests: LeaveRequest[] = [];
  const exceptions: LeaveException[] = [];
  const days: AttendanceDay[] = [];
  const longLeaves: LongLeave[] = [];
  const plans: WorkloadPlan[] = [];
  const accruals: AccrualRun[] = [];
  const yearEnds: YearEndRun[] = [];
  const types = () => policies.find((p) => p.status === "active")?.leaveTypes ?? [];
  const toBalance = (s: string, t: LeaveType): LeaveBalance => {
    const b = bal(s, t.code);
    return {
      leaveTypeId: t.id,
      leaveTypeCode: t.code,
      leaveYear: 2026,
      entitled: fromHundredths(b.entitled),
      reserved: fromHundredths(b.reserved),
      consumed: fromHundredths(b.consumed),
      available: fromHundredths(b.entitled - b.reserved - b.consumed),
    };
  };
  const findReq = (id: string) => {
    const r = requests.find((x) => x.id === id);
    if (!r) throw conflict("leave request not found");
    return r;
  };
  const check = (r: { version: number }, v: number) => {
    if (r.version !== v) throw new ApiProblem({ type: "about:blank", title: "Precondition failed", status: 412, errorCode: "precondition-failed" });
  };
  const create = (staffId: string, input: LeaveRequestInput): LeaveRequest => {
    const t = types().find((x) => x.id === input.leaveTypeId);
    if (!t) throw conflict("unknown leave type");
    const d = leaveDays(input.startDate, input.endDate, input.halfDayStart, input.halfDayEnd);
    const b = bal(staffId, t.code);
    if (t.requiresBalance && b.entitled - b.reserved - b.consumed < d) throw conflict(`insufficient ${t.code} balance`);
    const s = staff.find((x) => x.id === staffId)!;
    const managed = managerOf[staffId];
    const r: LeaveRequest = {
      id: nid(),
      institutionId: INST,
      number: `LV-${String(++lv).padStart(6, "0")}`,
      staffId,
      staffNumber: s.staffNumber,
      displayName: s.displayName,
      leaveTypeId: t.id,
      leaveTypeCode: t.code,
      isPaid: t.isPaid,
      startDate: input.startDate,
      endDate: input.endDate,
      halfDayStart: !!input.halfDayStart,
      halfDayEnd: !!input.halfDayEnd,
      days: fromHundredths(d),
      leaveYear: Number(input.startDate.slice(0, 4)),
      reason: input.reason ?? null,
      status: managed ? "pending_approval" : "reserved",
      approverStaffId: managed ?? null,
      segments: [],
      version: 1,
    };
    if (!managed) b.reserved += d;
    requests.unshift(r);
    return r;
  };
  const decide = (r: LeaveRequest, approve: boolean, reason?: string) => {
    if (r.status !== "pending_approval") throw conflict(`leave request ${r.number} is ${r.status}`);
    if (approve) bal(r.staffId, r.leaveTypeCode).reserved += toHundredths(r.days);
    r.status = approve ? "reserved" : "rejected";
    r.statusReason = reason ?? null;
    r.version += 1;
    return r;
  };
  const cancel = (r: LeaveRequest, reason?: string) => {
    const b = bal(r.staffId, r.leaveTypeCode);
    if (r.status === "reserved") b.reserved -= toHundredths(r.days);
    else if (r.status === "consumed") b.consumed -= toHundredths(r.days);
    else if (r.status !== "pending_approval") throw conflict(`leave request ${r.number} is ${r.status}`);
    r.status = "cancelled";
    r.statusReason = reason ?? null;
    r.version += 1;
    return r;
  };
  const me = MEERA;

  return {
    policies: async () => policies,
    createPolicy: async (_inst, input) => {
      const p: LeavePolicy = { id: nid(), institutionId: INST, name: input.name, versionNo: policies.length + 1, effectiveFrom: input.effectiveFrom, weeklyOffDays: [7], holidays: input.holidays ?? [], status: "draft", validationFindings: [], leaveTypes: [], version: 1 };
      policies.unshift(p);
      return p;
    },
    addType: async (policyId, input) => {
      const p = policies.find((x) => x.id === policyId)!;
      if (p.status !== "draft") throw conflict("leave types change only while the policy is a draft");
      if (p.leaveTypes.some((t) => t.code === input.code)) throw conflict(`leave type ${input.code} already exists`);
      p.leaveTypes.push({ ...type(input.code, input.name, input.annualEntitlement ?? "0.00", (input.accrualFrequency as LeaveType["accrualFrequency"]) ?? "yearly"), policyId });
      p.version += 1;
      return p;
    },
    policyCommand: async (policyId, v, cmd) => {
      const p = policies.find((x) => x.id === policyId)!;
      check(p, v);
      if (cmd === "submit") {
        if (p.leaveTypes.length === 0) throw new ApiProblem({ type: "about:blank", title: "Validation failed", status: 422, errorCode: "validation-failed", detail: "the policy defines no leave type" });
        p.status = "approved";
      } else if (cmd === "activate") {
        if (p.status !== "approved") throw conflict(`leave policy is ${p.status}`);
        for (const o of policies) if (o.status === "active") o.status = "superseded";
        p.status = "active";
      } else {
        p.status = "superseded";
      }
      p.version += 1;
      return p;
    },
    decidePolicy: async (policyId, v, approve) => {
      const p = policies.find((x) => x.id === policyId)!;
      check(p, v);
      p.status = approve ? "approved" : "draft";
      p.version += 1;
      return p;
    },
    requests: async (_inst, status) => ({ items: requests.filter((r) => !status || r.status === status), nextCursor: null }),
    requestLeave: async (_inst, input) => create(input.staffId ?? me, input),
    decideRequest: async (id, v, approve, reason) => {
      const r = findReq(id);
      check(r, v);
      return decide(r, approve, reason);
    },
    cancelRequest: async (id, v, reason) => {
      const r = findReq(id);
      check(r, v);
      return cancel(r, reason);
    },
    consumeRequest: async (id, v) => {
      const r = findReq(id);
      check(r, v);
      if (r.status !== "reserved") throw conflict(`leave request ${r.number} is ${r.status}`);
      const b = bal(r.staffId, r.leaveTypeCode);
      b.reserved -= toHundredths(r.days);
      b.consumed += toHundredths(r.days);
      r.status = "consumed";
      r.version += 1;
      return r;
    },
    sweep: async () => ({ processed: 0, skipped: requests.filter((r) => r.status === "reserved").length, failed: [] }),
    accruals: async () => accruals,
    accrue: async (_inst, leaveYear, month) => {
      const period = month ? `${leaveYear}-${String(month).padStart(2, "0")}` : String(leaveYear);
      const existing = accruals.find((a) => a.period === period);
      if (existing) {
        existing.runs += 1;
        existing.processed = 0;
        existing.skipped = staff.length;
        return existing;
      }
      const run: AccrualRun = { id: nid(), period, leaveYear, status: "completed", staffCount: staff.length, processed: staff.length, skipped: 0, runs: 1 };
      accruals.unshift(run);
      return run;
    },
    yearEnds: async () => yearEnds,
    yearEnd: async (_inst, leaveYear) => {
      const run: YearEndRun = { id: nid(), leaveYear, status: "completed", staffCount: staff.length, carriedDays: "15.00", expiredDays: "21.00", runs: 1 };
      yearEnds.unshift(run);
      return run;
    },
    staff: async () => staff,
    balances: async (staffId) => types().filter((t) => t.requiresBalance).map((t) => toBalance(staffId, t)),
    adjust: async (staffId, input) => {
      const t = types().find((x) => x.id === input.leaveTypeId)!;
      bal(staffId, t.code).entitled += toHundredths(input.days);
      return { id: nid(), staffId, leaveTypeCode: t.code, leaveYear: input.leaveYear, days: input.days, reason: input.reason, status: "applied", version: 3 };
    },
    encash: async (staffId, input) => {
      const t = types().find((x) => x.id === input.leaveTypeId)!;
      const b = bal(staffId, t.code);
      const avail = b.entitled - b.reserved - b.consumed;
      const ok = toHundredths(t.encashmentCap) > 0 && toHundredths(input.days) <= avail;
      if (ok) b.consumed += toHundredths(input.days);
      return { id: nid(), staffId, leaveTypeCode: t.code, leaveYear: input.leaveYear, days: input.days, availableDays: fromHundredths(avail), status: ok ? "approved" : "not_eligible", statusReason: ok ? null : "not_encashable", version: 2 };
    },
    exceptions: async () => exceptions,
    raiseException: async (_inst, input) => {
      const e: LeaveException = { id: nid(), number: `LEX-${String(exceptions.length + 1).padStart(6, "0")}`, staffId: input.staffId ?? null, category: input.category, description: input.description, status: "raised", version: 1 };
      exceptions.unshift(e);
      return e;
    },
    exceptionCommand: async (id, v, cmd, resolution) => {
      const e = exceptions.find((x) => x.id === id)!;
      check(e, v);
      if (cmd === "review") {
        if (e.status !== "raised") throw conflict(`exception is ${e.status}`);
        e.status = "under_review";
      } else {
        if (e.status !== "under_review") throw conflict(`exception is ${e.status}`);
        e.status = "resolved";
        e.resolution = resolution ?? null;
      }
      e.version += 1;
      return e;
    },
    grid: async (_inst, date) => ({
      workDate: date,
      items: staff.map((s) => ({
        staffId: s.id,
        staffNumber: s.staffNumber,
        displayName: s.displayName,
        engagementStatus: s.engagement?.status ?? "active",
        available: (s.engagement?.status ?? "active") === "active",
        day: days.find((d) => d.staffId === s.id && d.workDate === date) ?? null,
      })),
    }),
    bulk: async (_inst, workDate, entries) => {
      let processed = 0;
      let skipped = 0;
      for (const e of entries) {
        const d = days.find((x) => x.staffId === e.staffId && x.workDate === workDate);
        if (d && d.status === e.status) skipped += 1;
        else if (d) {
          d.status = e.status;
          d.version += 1;
          processed += 1;
        } else {
          days.push({ id: nid(), staffId: e.staffId, workDate, status: e.status, source: "bulk", version: 1 });
          processed += 1;
        }
      }
      return { processed, skipped, failed: [] };
    },
    correct: async (dayId, v, status, remark) => {
      const d = days.find((x) => x.id === dayId)!;
      check(d, v);
      d.status = status;
      d.source = "correction";
      d.remark = remark;
      d.version += 1;
      return d;
    },
    longLeaves: async (eng) => longLeaves.filter((l) => l.engagementId === eng),
    planLongLeave: async (eng, input) => {
      if (longLeaves.some((l) => l.engagementId === eng && (l.status === "planned" || l.status === "active"))) throw conflict("the engagement already has an open long leave");
      const s = staff.find((x) => x.engagement?.id === eng)!;
      const l: LongLeave = { id: nid(), staffId: s.id, engagementId: eng, reason: input.reason, plannedStart: input.plannedStart, plannedEnd: input.plannedEnd ?? null, status: "planned", engagementStatus: "active", available: true, version: 1 };
      longLeaves.unshift(l);
      return l;
    },
    longLeaveCommand: async (id, v, cmd) => {
      const l = longLeaves.find((x) => x.id === id)!;
      check(l, v);
      const s = staff.find((x) => x.id === l.staffId)!;
      if (cmd === "start") {
        if (l.status !== "planned") throw conflict(`long leave is ${l.status}`);
        l.status = "active";
        l.engagementStatus = "long_leave";
      } else {
        if (l.status !== "active") throw conflict(`long leave is ${l.status}`);
        l.status = "ended";
        l.engagementStatus = "active";
      }
      if (s.engagement) s.engagement.status = l.engagementStatus;
      l.available = l.engagementStatus === "active";
      l.version += 1;
      return l;
    },
    workload: async (staffId) => {
      const active = plans.find((p) => p.staffId === staffId && p.status === "active") ?? null;
      const max = active?.maxWeeklyHours ?? "16.00";
      const total = "4.00";
      return {
        staffId,
        engagementStatus: "active",
        available: true,
        totalWeeklyHours: total,
        policyMaxWeeklyHours: "16.00",
        effectiveMaxHours: max,
        overAllocated: !!active && Math.max(toHundredths(total), toHundredths(active.plannedWeeklyHours)) > toHundredths(max),
        items: [{ assignmentId: nid(), offeringCode: "CS101", title: "Programming I", role: "primary_instructor", status: "active", weeklyHours: total }],
        activePlan: active,
      };
    },
    workloadPlans: async (staffId) => plans.filter((p) => p.staffId === staffId),
    createPlan: async (staffId, input) => {
      if (plans.some((p) => p.staffId === staffId && (p.status === "draft" || p.status === "submitted"))) throw conflict("an open workload plan already exists");
      const p: WorkloadPlan = { id: nid(), staffId, termLabel: input.termLabel, availability: input.availability, plannedWeeklyHours: input.plannedWeeklyHours, maxWeeklyHours: input.maxWeeklyHours ?? null, overAllocated: false, status: "draft", version: 1 };
      plans.unshift(p);
      return p;
    },
    submitPlan: async (id, v, exceptionReason) => {
      const p = plans.find((x) => x.id === id)!;
      check(p, v);
      const max = toHundredths(p.maxWeeklyHours ?? "16.00");
      const over = Math.max(400, toHundredths(p.plannedWeeklyHours)) > max;
      if (over && !exceptionReason) throw conflict("the load exceeds the maximum; give an exception reason");
      for (const o of plans) if (o.staffId === p.staffId && o.status === "active") o.status = "superseded";
      Object.assign(p, { status: "active", teachingWeeklyHours: "4.00", effectiveMaxHours: fromHundredths(max), overAllocated: over, exceptionReason: exceptionReason ?? null, version: p.version + 1 });
      return p;
    },
    decidePlan: async (id, v, approve) => {
      const p = plans.find((x) => x.id === id)!;
      check(p, v);
      p.status = approve ? "active" : "draft";
      p.version += 1;
      return p;
    },
    myBalances: async () => types().filter((t) => t.requiresBalance).map((t) => toBalance(me, t)),
    myTypes: async () => types(),
    myRequests: async () => requests.filter((r) => r.staffId === me),
    apply: async (input) => create(me, input),
    cancelMine: async (id, v, reason) => {
      const r = findReq(id);
      check(r, v);
      if (r.staffId !== me) throw conflict("not your request");
      return cancel(r, reason);
    },
    approvals: async () => requests.filter((r) => r.status === "pending_approval" && managerOf[r.staffId] === me),
    decideApproval: async (id, v, approve, reason) => {
      const r = findReq(id);
      check(r, v);
      if (managerOf[r.staffId] !== me) throw new ApiProblem({ type: "about:blank", title: "Not found", status: 404, errorCode: "not-found" });
      return decide(r, approve, reason);
    },
    myAttendance: async (from, to) => days.filter((d) => d.staffId === me && d.workDate >= from && d.workDate <= to),
    async myWorkload() {
      return this.workload(me);
    },
  };
}
