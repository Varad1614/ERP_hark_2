/**
 * In-memory slice-5A API for mock mode and component tests (no network). Money stays in paise
 * integers internally and is rendered back to decimal strings — never floats.
 */
import type { ChargesApi } from "./api";
import { periodFor } from "./billing";
import type { Adjustment, AidAllocation, BillingEnrolment, BillingPlan, BillingRun, BillingRunSummary, BillingSchedule, ChargeDetail, ChargeStatus, FeeHead, FeeStructure, PolicyVersion } from "./types";

const paise = (s: string) => {
  const neg = s.startsWith("-");
  const [i = "0", f = ""] = s.replace(/^[-+]/, "").split(".");
  const v = Number(i) * 100 + Number((f + "00").slice(0, 2));
  return neg ? -v : v;
};
const money = (p: number) => `${p < 0 ? "-" : ""}${Math.floor(Math.abs(p) / 100)}.${String(Math.abs(p) % 100).padStart(2, "0")}`;
const uid = () => `00000000-0000-7000-8000-${Math.random().toString(16).slice(2, 14).padEnd(12, "0")}`;

export function createMockChargesApi(): ChargesApi {
  const heads: FeeHead[] = [
    { id: uid(), institutionId: "inst", code: "TUITION", name: "Tuition fee", category: "tuition", incomeAccountId: uid(), incomeAccountCode: "4100", refundable: false, status: "active", version: 1 },
  ];
  const structures: FeeStructure[] = [];
  const charge = (n: number, total: string, status: ChargeStatus): ChargeDetail => ({
    id: uid(),
    institutionId: "inst",
    chargeNumber: `CHG-${String(n).padStart(6, "0")}`,
    studentAccountId: "acc",
    displayName: "Aarav Desai",
    studentNumber: "S2026-0001",
    description: "Tuition fee — term 1",
    currency: "INR",
    totalAmount: total,
    adjustmentAmount: "0.00",
    settledAmount: "0.00",
    outstandingAmount: total,
    dueDate: "2026-07-15",
    sourceType: "manual",
    status,
    version: 1,
    lines: [{ lineNo: 1, feeHeadId: heads[0]!.id, feeHeadCode: "TUITION", description: "Tuition", amount: total }],
    instalments: [],
  });
  const charges: ChargeDetail[] = [charge(1, "125000.00", "published")];
  const adjustments: Adjustment[] = [];
  const allocations: AidAllocation[] = [];
  const policies: Record<"config" | "rule", PolicyVersion[]> = { config: [], rule: [] };
  const billingPlans: BillingPlan[] = [];
  const enrolments: BillingEnrolment[] = [];
  const runs: BillingRunSummary[] = [];
  const billedKeys = new Set<string>();
  const schedules: BillingSchedule[] = (["weekly", "monthly", "quarterly", "half_yearly", "annual"] as const).map((f) => ({
    frequency: f, enabled: false, runDay: 1, dueDays: 7, lastPeriodKey: null, lastRunId: null, currentPeriodLabel: periodFor(f, new Date().toISOString().slice(0, 10)).label,
  }));
  const find = <T extends { id: string }>(list: T[], id: string) => {
    const x = list.find((y) => y.id === id);
    if (!x) throw new Error("not found");
    return x;
  };
  const recompute = (c: ChargeDetail) => {
    c.outstandingAmount = money(paise(c.totalAmount) + paise(c.adjustmentAmount) - paise(c.settledAmount));
    c.version += 1;
  };
  return {
    feeHeads: async () => heads,
    createFeeHead: async (_i, input) => {
      const h: FeeHead = { id: uid(), institutionId: "inst", code: input.code.toUpperCase(), name: input.name, category: input.category, incomeAccountId: uid(), incomeAccountCode: "4900", refundable: !!input.refundable, status: "active", version: 1 };
      heads.push(h);
      return h;
    },
    structures: async () => structures,
    createStructure: async (_i, input) => {
      const total = input.lines.reduce((s, l) => s + paise(l.amount), 0);
      const s: FeeStructure = { id: uid(), institutionId: "inst", code: input.code, versionNo: structures.filter((x) => x.code === input.code).length + 1, name: input.name, dueDate: input.dueDate ?? null, currency: "INR", totalAmount: money(total), status: "draft", chargesGenerated: 0, lines: input.lines.map((l, i) => ({ lineNo: i + 1, feeHeadId: l.feeHeadId, feeHeadCode: heads.find((h) => h.id === l.feeHeadId)?.code ?? "FEE", description: l.description ?? "Fee", amount: l.amount })), version: 1 };
      structures.push(s);
      return s;
    },
    structureCommand: async (id, _v, cmd) => {
      const s = find(structures, id);
      s.status = cmd === "submit" ? "approved" : cmd === "publish" ? "published" : "superseded";
      s.version += 1;
      return s;
    },
    generate: async (id, studentIds) => {
      const s = find(structures, id);
      const created = studentIds.map(() => {
        const c = charge(charges.length + 1, s.totalAmount, "published");
        c.sourceType = "fee_structure";
        charges.push(c);
        s.chargesGenerated += 1;
        return { studentId: "student", chargeId: c.id, chargeNumber: c.chargeNumber, status: c.status };
      });
      return { feeStructureId: id, created, skipped: [] };
    },
    charges: async (_i, q = {}) => ({ items: charges.filter((c) => !q.status || c.status === q.status), nextCursor: null }),
    charge: async (id) => find(charges, id),
    createCharge: async (_i, input) => {
      const total = money(input.lines.reduce((s, l) => s + paise(l.amount), 0));
      const c = charge(charges.length + 1, total, input.publish ? "published" : "draft");
      c.description = input.description;
      charges.push(c);
      return c;
    },
    chargeCommand: async (id, _v, cmd) => {
      const c = find(charges, id);
      const to: Record<string, ChargeStatus> = { validate: "validated", publish: "published", suspend: "suspended", resume: "active", cancel: "cancelled", close: "closed", archive: "archived" };
      c.status = to[cmd] ?? c.status;
      c.version += 1;
      return c;
    },
    revise: async (id, _v, input) => {
      const c = find(charges, id);
      if (input.newAmount) c.adjustmentAmount = money(paise(input.newAmount) - paise(c.totalAmount));
      if (input.dueDate) c.dueDate = input.dueDate;
      recompute(c);
      return c;
    },
    setInstalments: async (id, input) => {
      const c = find(charges, id);
      const net = paise(c.totalAmount) + paise(c.adjustmentAmount);
      const each = Math.round(net / input.count);
      c.instalments = Array.from({ length: input.count }, (_, i) => ({ id: uid(), sequence: i + 1, dueDate: input.firstDueDate, amount: money(i === input.count - 1 ? net - each * (input.count - 1) : each), status: "scheduled" as const, version: 1 }));
      return c;
    },
    disputes: async () => [],
    raiseDispute: async (chargeId, reason) => {
      const c = find(charges, chargeId);
      c.status = "disputed";
      return { id: uid(), chargeId, chargeNumber: c.chargeNumber, reason, raisedByStudent: true, status: "raised", version: 1, createdAt: new Date().toISOString() };
    },
    decideDispute: async (id, v, input) => ({ id, chargeId: "", chargeNumber: "", reason: "", raisedByStudent: false, status: input.decision === "approve" ? "upheld" : "rejected", version: v + 1, createdAt: new Date().toISOString() }),
    myCharges: async () => charges.filter((c) => !["draft", "pending_validation", "validated"].includes(c.status)),
    adjustments: async () => adjustments,
    createAdjustment: async (chargeId, input) => {
      const c = find(charges, chargeId);
      const a: Adjustment = { id: uid(), adjustmentNumber: `${input.kind === "debit_note" ? "DN" : "CN"}-${String(adjustments.length + 1).padStart(6, "0")}`, chargeId, chargeNumber: c.chargeNumber, studentAccountId: c.studentAccountId, kind: input.kind, amount: input.amount, effect: input.kind === "debit_note" ? input.amount : `-${input.amount}`, currency: "INR", reason: input.reason, origin: "manual", status: "draft", version: 1, createdAt: new Date().toISOString() };
      adjustments.push(a);
      return a;
    },
    submitAdjustment: async (id) => {
      const a = find(adjustments, id);
      const c = find(charges, a.chargeId);
      c.adjustmentAmount = money(paise(c.adjustmentAmount) + paise(a.effect));
      recompute(c);
      a.status = "posted";
      a.version += 1;
      return a;
    },
    decideAdjustment: async (id, _v, decision) => {
      const a = find(adjustments, id);
      a.status = decision === "approve" ? "posted" : "rejected";
      a.version += 1;
      return a;
    },
    reverseAdjustment: async (id) => {
      const a = find(adjustments, id);
      a.status = "reversed";
      a.version += 1;
      return a;
    },
    correctAdjustment: async (id, _v, input) => {
      const a = find(adjustments, id);
      a.status = "superseded";
      const s: Adjustment = { ...a, id: uid(), amount: input.amount, status: "draft", supersedesId: a.id, version: 1 };
      adjustments.push(s);
      return s;
    },
    aidDecisions: async () => [{ eligibilityId: "elig-1", scholarshipId: "sch-1", scholarshipCode: "MERIT", scholarshipName: "Merit scholarship", sponsorType: "institutional", benefitType: "fixed_amount", benefitValue: "30000.00", studentId: "student", status: "active", live: true, validUntil: "2027-05-31", renewalCount: 0, allocationId: allocations[0]?.id ?? null, allocationStatus: allocations[0]?.status ?? null }],
    aidAllocations: async () => allocations,
    allocate: async () => {
      const a: AidAllocation = { id: uid(), allocationNumber: "AID-000001", scholarshipEligibilityId: "elig-1", scholarshipCode: "MERIT", scholarshipName: "Merit scholarship", studentId: "student", studentAccountId: "acc", displayName: "Aarav Desai", benefitType: "fixed_amount", benefitValue: "30000.00", allocatedAmount: "30000.00", appliedAmount: "0.00", disbursedAmount: "0.00", recoveredAmount: "0.00", remainingAmount: "30000.00", renewalCount: 0, status: "allocated", disbursements: [], version: 1 };
      allocations.push(a);
      return a;
    },
    approveAid: async (id) => find(allocations, id),
    disburse: async (id, _v, input) => {
      const a = find(allocations, id);
      const amt = input.amount ?? a.remainingAmount ?? "0.00";
      a.appliedAmount = money(paise(a.appliedAmount) + paise(amt));
      a.remainingAmount = money(paise(a.allocatedAmount ?? "0") - paise(a.appliedAmount));
      a.status = "disbursed";
      a.version += 1;
      return a;
    },
    renewAid: async (id) => find(allocations, id),
    revokeAid: async (id) => {
      const a = find(allocations, id);
      a.status = "revoked";
      a.version += 1;
      return a;
    },
    reconciliations: async () => [],
    reconcile: async () => ({ id: uid(), allocationCount: allocations.length, allocatedTotal: "30000.00", utilisedTotal: "0.00", recoveredTotal: "0.00", ledgerTotal: "0.00", variance: "0.00", status: "matched", exceptions: [], createdAt: new Date().toISOString() }),
    myAid: async () => allocations,
    policies: async (_i, kind) => policies[kind],
    createPolicy: async (_i, kind, input) => {
      const p: PolicyVersion = { id: uid(), institutionId: "inst", ruleKey: input.ruleKey ?? null, versionNo: policies[kind].length + 1, values: input.values, changeNote: input.changeNote ?? null, status: "draft", version: 1 };
      policies[kind].unshift(p);
      return p;
    },
    policyCommand: async (kind, id, _v, cmd) => {
      const p = find(policies[kind], id);
      if (cmd === "activate") for (const o of policies[kind]) if (o.status === "effective" && o.ruleKey === p.ruleKey) o.status = "superseded";
      p.status = cmd === "submit" ? "in_review" : "effective";
      p.approvedAt = p.approvedAt ?? new Date().toISOString();
      p.version += 1;
      return p;
    },
    rollback: async (kind, id, reason) => {
      const b = find(policies[kind], id);
      const p: PolicyVersion = { ...b, id: uid(), versionNo: policies[kind].length + 1, status: "draft", basedOnId: b.id, rollbackReason: reason, approvedAt: null, version: 1 };
      policies[kind].unshift(p);
      return p;
    },
    billingPlans: async () => billingPlans.map((p) => ({ ...p, enrolled: enrolments.filter((e) => e.planId === p.id).length })),
    createBillingPlan: async (_i, input) => {
      const head = find(heads, input.feeHeadId);
      const p: BillingPlan = { id: uid(), institutionId: "inst", code: input.code, name: input.name, description: input.description ?? null, feeHeadId: head.id, feeHeadCode: head.code, amount: money(paise(input.amount)), frequency: input.frequency, proration: input.proration ?? "none", status: "active", enrolled: 0, version: 1 };
      billingPlans.push(p);
      return p;
    },
    updateBillingPlan: async (id, input) => {
      const p = find(billingPlans, id);
      Object.assign(p, { ...input, amount: input.amount ? money(paise(input.amount)) : p.amount, version: p.version + 1 });
      return p;
    },
    billingEnrolments: async (planId) => enrolments.filter((e) => e.planId === planId),
    enrol: async (planId, input) => {
      let e = enrolments.find((x) => x.planId === planId && x.studentId === input.studentId);
      if (!e) {
        e = { id: uid(), planId, studentId: input.studentId, studentName: "Aarav Desai", studentNumber: "S2026-0001", onHold: false };
        enrolments.push(e);
      }
      Object.assign(e, { amount: input.amount ?? null, startDate: input.startDate ?? null, endDate: input.endDate ?? null, onHold: !!input.onHold });
      return e;
    },
    unenrol: async (planId, studentId) => {
      const i = enrolments.findIndex((x) => x.planId === planId && x.studentId === studentId);
      if (i >= 0) enrolments.splice(i, 1);
    },
    billingRun: async (_i, input, preview) => {
      const day = input.day ?? new Date().toISOString().slice(0, 10);
      const period = periodFor(input.frequency, day, input.weekStart ?? 1);
      const due = new Date(Date.parse(period.start > day ? period.start : day) + (input.dueDays ?? 7) * 86400000).toISOString().slice(0, 10);
      const plans = billingPlans.filter((p) => p.frequency === input.frequency && p.status === "active");
      const result: BillingRun = { id: preview ? null : uid(), preview, frequency: input.frequency, periodKey: period.key, periodLabel: period.label, periodStart: period.start, periodEnd: period.end, runDate: day, dueDays: input.dueDays ?? 7, trigger: "manual", billed: 0, skipped: 0, failed: 0, total: "0.00", items: [], skips: [] };
      const students = [...new Set(enrolments.filter((e) => plans.some((p) => p.id === e.planId)).map((e) => e.studentId))];
      let total = 0;
      for (const sid of students) {
        const mine = enrolments.filter((e) => e.studentId === sid && plans.some((p) => p.id === e.planId));
        const name = mine[0]!.studentName;
        if (billedKeys.has(`${sid}|${period.key}`)) {
          result.skips.push({ studentId: sid, studentName: name, reason: "already_billed" });
          continue;
        }
        const lines = mine.filter((e) => !e.onHold).map((e) => {
          const p = plans.find((x) => x.id === e.planId)!;
          return { planId: p.id, description: `${p.name}, ${period.label}`, amount: e.amount ?? p.amount };
        });
        const sum = lines.reduce((a, l) => a + paise(l.amount), 0);
        if (!lines.length || sum <= 0) {
          result.skips.push({ studentId: sid, studentName: name, reason: lines.length ? "nothing_to_bill" : "on_hold" });
          continue;
        }
        if (!preview) billedKeys.add(`${sid}|${period.key}`);
        total += sum;
        result.items.push({ studentId: sid, studentName: name, studentNumber: mine[0]!.studentNumber, dueDate: due, total: money(sum), lines, chargeId: preview ? null : uid(), chargeNumber: preview ? null : `CHG-${String(charges.length + result.items.length + 1).padStart(6, "0")}` });
      }
      result.billed = result.items.length;
      result.skipped = result.skips.length;
      result.total = money(total);
      if (!preview) runs.unshift({ id: result.id!, frequency: result.frequency, periodKey: result.periodKey, periodLabel: result.periodLabel, runDate: day, trigger: "manual", billed: result.billed, skipped: result.skipped, failed: 0, total: result.total, createdBy: "you", createdAt: new Date().toISOString() });
      return result;
    },
    billingRuns: async () => runs,
    billingSchedules: async () => schedules,
    setBillingSchedule: async (_i, frequency, input) => {
      const s = schedules.find((x) => x.frequency === frequency)!;
      Object.assign(s, input);
      return s;
    },
  };
}
