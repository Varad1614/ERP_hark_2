/**
 * In-memory stand-in of the slice-3B API (mock mode, tests). Small but coherent: one open
 * window, three term-1 components, the student's registrations, holds, a policy and a
 * progression record. Rules mirror the API loosely (the API stays authoritative).
 */
import { ApiProblem } from "@sutra/api-client";
import type { RegistrationApi } from "./api";
import type {
  AbcSync,
  AcademicBreak,
  CreditLedger,
  HoldList,
  ProgressionDecision,
  ProgressionPolicy,
  ProgressionSummary,
  QualificationEligibility,
  Registration,
  RegistrationHold,
  RegistrationLine,
  RegistrationLineKind,
  RegistrationOption,
  RegistrationOptions,
  RegistrationWindow,
} from "./types";

const INST = "00000000-0000-7000-8000-00000000b300";
const TERM = "00000000-0000-7000-8000-00000000b301";
const NOW = "2026-10-12T09:00:00Z";
let seq = 1;
const uid = () => `00000000-0000-7000-8000-${String(1000 + seq++).padStart(12, "0")}`;

function conflict(detail: string): never {
  throw new ApiProblem({ type: "about:blank", title: "Conflict", status: 409, detail, errorCode: "conflict" });
}

export const MOCK_COMPONENTS: RegistrationOption[] = [
  { curriculumComponentId: "c-cs101", termNo: 1, slotCode: "CS101", title: "Programming I", category: "major_core", credits: 12, earned: false, suggestedKind: "regular" },
  { curriculumComponentId: "c-md101", termNo: 1, slotCode: "MD101", title: "Design thinking", category: "multidisciplinary", credits: 8, earned: false, suggestedKind: "regular" },
  { curriculumComponentId: "c-ae101", termNo: 1, slotCode: "AE101", title: "Communication", category: "ability_enhancement", credits: 4, earned: false, suggestedKind: "regular" },
];

type LineIn = { curriculumComponentId: string; kind?: RegistrationLineKind | null };

export function createMockRegistrationApi(): RegistrationApi {
  const windows: RegistrationWindow[] = [
    { id: uid(), institutionId: INST, academicTermId: TERM, name: "Term 1 registration", opensOn: "2026-10-01", closesOn: "2026-10-31", minCredits: 16, maxCredits: 28, status: "open", version: 2, createdAt: NOW, updatedAt: NOW },
  ];
  const regs: Registration[] = [];
  const holds: RegistrationHold[] = [];
  const policies: ProgressionPolicy[] = [
    {
      id: uid(), institutionId: INST, code: "UG", versionNo: 1, name: "UG progression", effectiveFrom: "2026-06-01", status: "effective", version: 3, createdAt: NOW, updatedAt: NOW,
      rules: { progressMinRatio: 1, conditionalMinRatio: 0.75, probationMinRatio: 0.5, maxFailedForConditional: 2, escalateRepeatProbation: true, detentionRepeatUnit: "term", allowRevision: true },
    },
  ];
  const decisions: ProgressionDecision[] = [];
  const breaks: AcademicBreak[] = [];
  const abc: AbcSync[] = [];
  let eligibility: QualificationEligibility | null = null;
  const ledger: CreditLedger = { studentId: "s1", totalCredits: 0, entries: [] };

  const total = (lines: RegistrationLine[]) => lines.filter((l) => (l.status === "selected" || l.status === "registered") && l.kind !== "audit").reduce((a, l) => a + l.credits, 0);
  const touch = <T extends { version: number }>(x: T, patch: Partial<T>): T => Object.assign(x, patch, { version: x.version + 1 });
  const reg = (id: string) => regs.find((r) => r.id === id) ?? conflict("registration not found");
  const line = (c: string, kind: RegistrationLineKind = "regular"): RegistrationLine => {
    const comp = MOCK_COMPONENTS.find((x) => x.curriculumComponentId === c) ?? conflict("component not in curriculum");
    return { id: uid(), curriculumComponentId: c, kind, status: "selected", credits: comp.credits, componentTermNo: comp.termNo, slotCode: comp.slotCode, title: comp.title, addedAfterRegistration: false };
  };
  const options = (studentId: string): RegistrationOptions => ({
    studentId,
    studentContextId: "ctx-1",
    institutionId: INST,
    termNo: 1,
    standing: "good_standing",
    components: MOCK_COMPONENTS,
    windows: windows.filter((w) => w.status === "open" || w.status === "scheduled"),
    activeHolds: holds.filter((h) => h.status === "active"),
  });
  const create = (studentId: string, origin: string, input: { academicTermId: string; lines?: LineIn[] | null }): Registration => {
    if (holds.some((h) => h.status === "active")) conflict("the student has an active registration hold");
    if (regs.some((r) => r.studentId === studentId && r.academicTermId === input.academicTermId && r.status !== "cancelled")) conflict("the student already has a registration for this term");
    const lines = (input.lines ?? []).map((l) => line(l.curriculumComponentId, l.kind ?? "regular"));
    const r: Registration = {
      id: uid(), studentId, studentContextId: "ctx-1", institutionId: INST, academicTermId: input.academicTermId, registrationWindowId: windows[0]!.id, termNo: 1,
      status: "draft", origin, totalCredits: total(lines), lines, version: 1, createdAt: NOW, updatedAt: NOW,
    };
    regs.unshift(r);
    return r;
  };
  const addLine = (id: string, l: LineIn) => {
    const r = reg(id);
    if (r.lines.some((x) => x.curriculumComponentId === l.curriculumComponentId && x.status !== "dropped")) conflict("already selected");
    const n = line(l.curriculumComponentId, l.kind ?? "regular");
    if (r.status === "registered") Object.assign(n, { status: "registered", addedAfterRegistration: true });
    r.lines.push(n);
    return touch(r, { totalCredits: total(r.lines) });
  };
  const dropLine = (id: string, lineId: string) => {
    const r = reg(id);
    const l = r.lines.find((x) => x.id === lineId) ?? conflict("line not found");
    l.status = "dropped";
    return touch(r, { totalCredits: total(r.lines) });
  };
  const submit = (id: string) => {
    const r = reg(id);
    if (total(r.lines) < 16) conflict("16 credits minimum");
    r.lines.forEach((l) => {
      if (l.status === "selected") l.status = "registered";
    });
    return touch(r, { status: "registered", processInstanceId: `noop-${r.id}` });
  };
  const cancel = (id: string, reason?: string) => {
    const r = reg(id);
    if (r.status === "finalised") conflict("registration is finalised");
    r.lines.forEach((l) => (l.status = "withdrawn"));
    return touch(r, { status: "cancelled", statusReason: reason ?? null, totalCredits: 0 });
  };
  const summary = (studentId: string): ProgressionSummary => ({
    studentId,
    studentContextId: "ctx-1",
    currentTermNo: decisions.some((d) => d.status === "effective") ? 2 : 1,
    standing: "good_standing",
    totalCredits: ledger.totalCredits,
    effectivePolicy: policies.find((p) => p.status === "effective") ?? null,
    probationCases: [],
    detentionCases: [],
    pendingConditions: [],
    eligibility,
  });

  return {
    listWindows: async () => windows,
    createWindow: async (_i, input) => {
      const w: RegistrationWindow = { id: uid(), institutionId: INST, academicTermId: input.academicTermId, name: input.name ?? "Registration", opensOn: input.opensOn ?? "2026-11-01", closesOn: input.closesOn ?? "2026-11-15", status: "scheduled", version: 1, createdAt: NOW, updatedAt: NOW };
      windows.push(w);
      return w;
    },
    windowCommand: async (id, _v, cmd) => {
      const w = windows.find((x) => x.id === id) ?? conflict("window not found");
      return touch(w, { status: cmd === "open" ? "open" : cmd === "close" ? "closed" : "cancelled" });
    },
    finaliseWindow: async (id) => {
      const w = windows.find((x) => x.id === id) ?? conflict("window not found");
      const ids = regs.filter((r) => r.status === "registered").map((r) => touch(r, { status: "finalised" }).id);
      return { window: touch(w, {}), finalisedRegistrationIds: ids };
    },
    options: async (s) => options(s),
    listRegistrations: async (s) => regs.filter((r) => r.studentId === s),
    createRegistration: async (s, input) => create(s, "staff", input),
    addLine: async (id, _v, l) => addLine(id, l),
    dropLine: async (id, _v, lineId) => dropLine(id, lineId),
    registrationCommand: async (id, _v, cmd, reason) => (cmd === "submit" ? submit(id) : cmd === "cancel" ? cancel(id, reason) : touch(reg(id), { status: "finalised" })),
    listHolds: async (): Promise<HoldList> => ({ items: holds, financeEvidence: { kind: "notApplicable" } }),
    placeHold: async (s, input) => {
      const h: RegistrationHold = { id: uid(), studentId: s, institutionId: INST, source: input.source, reason: input.reason, status: "active", placedAt: NOW, version: 1 };
      holds.unshift(h);
      return h;
    },
    releaseHold: async (id, _v, reason) => {
      const h = holds.find((x) => x.id === id) ?? conflict("hold not found");
      return touch(h, { status: "released", releaseReason: reason, releasedAt: NOW });
    },
    myOptions: async () => options("me"),
    myRegistrations: async () => regs.filter((r) => r.studentId === "me"),
    createMyRegistration: async (input) => create("me", "self", input),
    addMyLine: async (id, _v, l) => addLine(id, l),
    dropMyLine: async (id, _v, lineId) => dropLine(id, lineId),
    submitMine: async (id) => submit(id),
    cancelMine: async (id, _v, reason) => cancel(id, reason),
    listPolicies: async () => policies,
    createPolicy: async (_i, input) => {
      const p: ProgressionPolicy = { id: uid(), institutionId: INST, code: input.code, versionNo: 1, name: input.name, effectiveFrom: input.effectiveFrom, rules: input.rules, status: "draft", version: 1, createdAt: NOW, updatedAt: NOW };
      policies.push(p);
      return p;
    },
    policyCommand: async (id, _v, cmd) => {
      const p = policies.find((x) => x.id === id) ?? conflict("policy not found");
      if (cmd === "submit") policies.filter((x) => x.code === p.code && x.status === "effective").forEach((x) => touch(x, { status: "superseded" }));
      return touch(p, { status: cmd === "submit" ? "effective" : "retired" });
    },
    revisePolicy: async (id) => {
      const p = policies.find((x) => x.id === id) ?? conflict("policy not found");
      const n: ProgressionPolicy = { ...p, id: uid(), versionNo: p.versionNo + 1, status: "draft", version: 1, supersedesPolicyId: p.id };
      policies.push(n);
      return n;
    },
    summary: async (s) => summary(s),
    listDecisions: async () => decisions,
    createDecision: async (s, input) => {
      const d: ProgressionDecision = {
        id: uid(), studentId: s, studentContextId: "ctx-1", institutionId: INST, academicTermId: input.academicTermId, termNo: 1, outcome: input.outcome ?? "progressed",
        recommendedOutcome: "progressed", evidenceSource: "manual", creditsEarned: input.creditsEarned ?? 0, creditsAttempted: input.creditsAttempted ?? null,
        failedComponentIds: input.failedComponentIds ?? [], status: "draft", conditions: [], version: 1, createdAt: NOW, updatedAt: NOW,
      };
      decisions.unshift(d);
      return d;
    },
    decisionCommand: async (id, _v, cmd) => {
      const d = decisions.find((x) => x.id === id) ?? conflict("decision not found");
      if (cmd === "submit") {
        ledger.entries.push({ id: uid(), kind: "earned", credits: d.creditsEarned, sourceType: "progression_decision", sourceId: d.id, institutionApproved: true, recordedAt: NOW });
        ledger.totalCredits += d.creditsEarned;
      }
      return touch(d, { status: cmd === "submit" ? "effective" : "rejected", resultingTermNo: 2 });
    },
    conditionCommand: async () => conflict("no conditions in mock mode"),
    probationCommand: async () => conflict("no probation in mock mode"),
    ledger: async () => ledger,
    listTransfers: async () => [],
    listMigrations: async () => [],
    listBreaks: async () => breaks,
    requestBreak: async (s, input) => {
      const b: AcademicBreak = { id: uid(), studentId: s, institutionId: INST, reason: input.reason, startsOn: input.startsOn, expectedReturnOn: input.expectedReturnOn, termNoAtStart: 1, status: "in_effect", version: 2, createdAt: NOW, updatedAt: NOW };
      breaks.unshift(b);
      return b;
    },
    rejoin: async (id, _v, input) => {
      const b = breaks.find((x) => x.id === id) ?? conflict("break not found");
      return touch(b, { status: "rejoined", rejoinTermNo: input.termNo ?? 1, rejoinedOn: "2026-10-12" });
    },
    evaluateEligibility: async (s) => {
      const ok = ledger.totalCredits >= 80;
      eligibility = {
        id: uid(), studentId: s, studentContextId: "ctx-1", institutionId: INST, eligible: ok, outcomes: ok ? ["degree"] : [],
        totalCredits: ledger.totalCredits, requiredCredits: 80, outstanding: ok ? [] : [`minimum programme credits 80 not met (${ledger.totalCredits} recorded)`],
        basis: "ledger", evaluatedAt: NOW,
      };
      return eligibility;
    },
    listAbcSyncs: async () => abc,
    syncAbc: async (s) => {
      const a: AbcSync = { id: uid(), studentId: s, institutionId: INST, entryIds: ledger.entries.map((e) => e.id), totalCredits: ledger.totalCredits, status: "failed", failureReason: "not_configured", attemptedAt: NOW, version: 1 };
      abc.unshift(a);
      return a;
    },
  };
}
