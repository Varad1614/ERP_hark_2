/**
 * In-memory stand-in of the slice-9E accreditation API (mock mode and tests): an active NAAC
 * framework (v1, seven criteria) plus a draft NBA one, an active cycle with its approved
 * assessment plan, an open compliance obligation, an approved evidence record and an approved
 * self-study report awaiting hand-off. Legal transitions only (409 otherwise); auto-approval, like
 * the noop workflow adapter.
 */
import { ApiProblem } from "@sutra/api-client";
import type { AccreditationApi } from "./api";
import { needsCycle, recordItemKinds, RECORD_OUTCOMES } from "./rules";
import type { AccreditationCycle, AccreditationFramework, AccreditationItem, AccreditationRecord, Criterion, RecordKind } from "./types";

const conflict = (detail: string) => new ApiProblem({ type: "about:blank", title: "Conflict", status: 409, errorCode: "conflict", detail });
const notFound = () => new ApiProblem({ type: "about:blank", title: "Not found", status: 404, errorCode: "not-found" });
const invalid = (detail: string) => new ApiProblem({ type: "about:blank", title: "Unprocessable", status: 422, errorCode: "validation-failed", detail });
const stale = () => new ApiProblem({ type: "about:blank", title: "Precondition failed", status: 412, errorCode: "precondition-failed" });

const INST = "00000000-0000-7000-8000-0000000b9e02";
let seq = 950;
const uid = () => `00000000-0000-7000-8000-${String(++seq).padStart(12, "0")}`;
const pad = (n: number) => String(n).padStart(6, "0");
const asObject = (o: Record<string, unknown>) => o as unknown as AccreditationRecord["refs"];

const NAAC: Criterion[] = [
  { code: "C1", title: "Curricular aspects", weight: 15, metricKeys: [] },
  { code: "C2", title: "Teaching-learning and evaluation", weight: 20, metricKeys: ["student.active_students"] },
  { code: "C3", title: "Research, innovations and extension", weight: 15, metricKeys: [] },
  { code: "C4", title: "Infrastructure and learning resources", weight: 15, metricKeys: ["campus.library_titles"] },
  { code: "C5", title: "Student support and progression", weight: 10, metricKeys: [] },
  { code: "C6", title: "Governance, leadership and management", weight: 15, metricKeys: [] },
  { code: "C7", title: "Institutional values and best practices", weight: 10, metricKeys: [] },
];

export function createMockAccreditationApi(): AccreditationApi {
  let cycleNo = 0;
  let recordNo = 0;
  const frameworks: AccreditationFramework[] = [
    { id: uid(), institutionId: INST, code: "NAAC", name: "NAAC Revised Accreditation Framework", body: "NAAC", versionNo: 1, criteria: NAAC, status: "active", approvalState: "approved", version: 3 },
    { id: uid(), institutionId: INST, code: "NBA", name: "NBA Tier-II engineering", body: "NBA", versionNo: 1, criteria: [{ code: "P1", title: "Programme outcomes", weight: 100, metricKeys: [] }], status: "draft", approvalState: "not_requested", version: 1 },
  ];
  const naac = frameworks[0]!;
  const newCycle = (f: AccreditationFramework, title: string, periodStart: string, periodEnd: string, extra: Partial<AccreditationCycle> = {}): AccreditationCycle => ({
    id: uid(),
    institutionId: INST,
    number: `AC-${pad(++cycleNo)}`,
    frameworkId: f.id,
    frameworkCode: f.code,
    frameworkVersionNo: f.versionNo,
    criteria: f.criteria,
    title,
    periodStart,
    periodEnd,
    plan: [],
    status: "planning",
    approvalState: "not_requested",
    records: [],
    version: 1,
    ...extra,
  });
  const cycle = newCycle(naac, "NAAC cycle 2026-27", "2026-06-01", "2027-05-31", {
    status: "active",
    approvalState: "approved",
    plan: NAAC.map((c, i) => ({ criterionCode: c.code, owner: i % 2 ? "IQAC coordinator" : "Principal", dueOn: "2027-03-31" })),
    version: 4,
  });
  const cycles: AccreditationCycle[] = [cycle];

  const newRecord = (kind: RecordKind, title: string, extra: Partial<AccreditationRecord> = {}): AccreditationRecord => ({
    id: uid(),
    institutionId: INST,
    number: `ACR-${pad(++recordNo)}`,
    kind,
    title,
    severity: "medium",
    refs: asObject({}),
    facts: asObject({}),
    status: "draft",
    approvalState: "not_requested",
    allowedOutcomes: RECORD_OUTCOMES[kind],
    items: [],
    version: 1,
    ...extra,
  });
  const item = (recordId: string, kind: AccreditationItem["kind"], title: string, extra: Partial<AccreditationItem> = {}): AccreditationItem => ({ id: uid(), recordId, kind, title, status: "open", version: 1, ...extra });

  const obligation = newRecord("obligation", "UGC anti-ragging compliance", { status: "open", owner: "Dean of students", dueOn: "2026-12-31", version: 2 });
  obligation.items = [item(obligation.id, "requirement", "Anti-ragging committee constituted", { status: "verified", evidenceRef: "Office order 14/2026", version: 3 }), item(obligation.id, "requirement", "Affidavits collected from first-year students")];
  const evidenceRec = newRecord("evidence", "Library resources evidence (C4)", { cycleId: cycle.id, criterionCode: "C4", status: "closed", approvalState: "approved", outcome: "approved_evidence", refs: asObject({ sourceUrl: "https://example.edu/library-report.pdf" }), version: 5 });
  const selfStudy = newRecord("self_study", "Self-study report 2026-27", { cycleId: cycle.id, status: "closed", approvalState: "approved", outcome: "approved", version: 4 });
  const records: AccreditationRecord[] = [obligation, evidenceRec, selfStudy];

  const find = <T extends { id: string; version: number }>(list: T[], id: string, version?: number): T => {
    const x = list.find((y) => y.id === id);
    if (!x) throw notFound();
    if (version !== undefined && x.version !== version) throw stale();
    return x;
  };
  const touch = <T extends { version: number }>(x: T, patch: Partial<T>): T => {
    Object.assign(x, patch, { version: x.version + 1 });
    return { ...x };
  };
  const findItem = (id: string, version: number) => {
    for (const r of records) {
      const it = r.items.find((i) => i.id === id);
      if (it) {
        if (it.version !== version) throw stale();
        return { r, it };
      }
    }
    throw notFound();
  };
  const refresh = (c: AccreditationCycle) => {
    const counts = new Map<string, number>();
    for (const r of records.filter((x) => x.cycleId === c.id)) counts.set(`${r.kind}|${r.status}`, (counts.get(`${r.kind}|${r.status}`) ?? 0) + 1);
    c.records = [...counts].map(([k, count]) => {
      const [kind = "", status = ""] = k.split("|");
      return { kind, status, count };
    });
    return { ...c };
  };
  refresh(cycle);

  return {
    frameworks: async () => frameworks.map((f) => ({ ...f })),
    createFramework: async (_inst, input) => {
      if (input.criteria.reduce((s, c) => s + c.weight, 0) !== 100) throw invalid("criterion weights must add up to 100");
      const prev = frameworks.filter((f) => f.code === input.code);
      const f: AccreditationFramework = { id: uid(), institutionId: INST, code: input.code, name: input.name, body: input.body, versionNo: prev.length + 1, criteria: input.criteria, predecessorId: prev.find((p) => p.status === "active")?.id ?? null, status: "draft", approvalState: "not_requested", version: 1 };
      frameworks.unshift(f);
      return { ...f };
    },
    submitFramework: async (id, version) => {
      const f = find(frameworks, id, version);
      if (f.status !== "draft" || f.approvalState === "pending") throw conflict("only a draft framework version is submitted");
      for (const p of frameworks) if (p.code === f.code && p.status === "active") Object.assign(p, { status: "superseded", version: p.version + 1 });
      return touch(f, { status: "active", approvalState: "approved" });
    },
    decideFramework: async (id, version, decision, reason) => {
      const f = find(frameworks, id, version);
      if (f.approvalState !== "pending") throw conflict("nothing to decide");
      return decision === "approve" ? touch(f, { status: "active", approvalState: "approved" }) : touch(f, { approvalState: "rejected", statusReason: reason ?? null });
    },
    cycles: async () => cycles.map((c) => refresh(c)),
    createCycle: async (_inst, input) => {
      const f = find(frameworks, input.frameworkId);
      if (f.status !== "active") throw conflict(`${f.code} v${f.versionNo} is not active`);
      if (input.periodEnd <= input.periodStart) throw invalid("the period ends after it starts");
      const c = newCycle(f, input.title, input.periodStart, input.periodEnd);
      cycles.unshift(c);
      return { ...c };
    },
    planCycle: async (id, version, entries) => {
      const c = find(cycles, id, version);
      if (c.status !== "planning") throw conflict("only a planning cycle is planned");
      const codes = c.criteria.map((x) => x.code).sort().join(",");
      if (entries.map((e) => e.criterionCode).sort().join(",") !== codes) throw invalid("every criterion exactly once");
      if (entries.some((e) => !e.owner.trim() || e.dueOn < c.periodStart || e.dueOn > c.periodEnd)) throw invalid("every entry needs an owner and a due date inside the period");
      return touch(c, { plan: entries, approvalState: "not_requested" });
    },
    submitCycle: async (id, version) => {
      const c = find(cycles, id, version);
      if (c.status !== "planning" || !c.plan.length) throw conflict("plan the cycle first");
      return touch(c, { status: "active", approvalState: "approved" });
    },
    decideCycle: async (id, version, decision, reason) => {
      const c = find(cycles, id, version);
      if (c.approvalState !== "pending") throw conflict("nothing to decide");
      return decision === "approve" ? touch(c, { status: "active", approvalState: "approved" }) : touch(c, { status: "planning", approvalState: "rejected", statusReason: reason ?? null });
    },
    closeCycle: async (id, version) => {
      const c = find(cycles, id, version);
      if (c.status !== "submitted") throw conflict("only a submitted cycle (self-study handed off) is closed");
      return touch(c, { status: "closed" });
    },
    records: async () => records.map((r) => ({ ...r, items: [...r.items] })),
    createRecord: async (_inst, input) => {
      if (needsCycle(input.kind) && !input.cycleId) throw invalid(`a ${input.kind} record belongs to a cycle`);
      const refs: Record<string, unknown> = {};
      if (input.sourceUrl) refs.sourceUrl = input.sourceUrl;
      if (input.governanceEvidenceIds?.length) refs.governanceEvidenceIds = input.governanceEvidenceIds;
      if (input.regulatorySnapshotIds?.length) refs.regulatorySnapshotIds = input.regulatorySnapshotIds;
      const r = newRecord(input.kind, input.title, { cycleId: input.cycleId ?? null, criterionCode: input.criterionCode ?? null, owner: input.owner ?? null, dueOn: input.dueOn ?? null, description: input.description ?? null, severity: input.severity ?? "medium", parentId: input.parentId ?? null, refs: asObject(refs) });
      records.unshift(r);
      return { ...r };
    },
    recordCommand: async (id, version, cmd, reason) => {
      const r = find(records, id, version);
      if (cmd === "start") {
        if (r.status !== "draft") throw conflict("only a draft record is started");
        return touch(r, { status: "open" });
      }
      if (r.status !== "draft" && r.status !== "open") throw conflict("only a draft or open record is cancelled");
      if (!reason?.trim()) throw invalid("reason is required");
      return touch(r, { status: "cancelled", statusReason: reason });
    },
    submitRecord: async (id, version, outcome, rationale, grade) => {
      const r = find(records, id, version);
      if (r.status !== "open") throw conflict("only an open record is submitted");
      if (!r.allowedOutcomes.includes(outcome)) throw invalid(`outcome must be one of ${r.allowedOutcomes.join(", ")}`);
      if (r.kind === "obligation" && outcome === "compliant" && r.items.some((i) => i.status !== "verified" && i.status !== "cancelled")) throw invalid("every requirement must be verified to be compliant");
      if (r.kind === "evidence" && outcome === "approved_evidence" && !Object.keys(r.refs as Record<string, unknown>).length) throw invalid("approved evidence cites at least one source");
      if (r.kind === "external_assessment" && !grade) throw invalid("the awarded grade / score is required");
      return touch(r, { status: "closed", approvalState: "approved", outcome, rationale: rationale ?? null });
    },
    decideRecord: async (id, version, decision, reason) => {
      const r = find(records, id, version);
      if (r.status !== "pending_decision") throw conflict("nothing to decide");
      return decision === "approve" ? touch(r, { status: "closed", approvalState: "approved" }) : touch(r, { status: "open", approvalState: "rejected", outcome: null, statusReason: reason ?? null });
    },
    handoff: async (id, version, reference) => {
      const r = find(records, id, version);
      if (r.kind !== "self_study" || r.status !== "closed" || r.handedOffAt) throw conflict(`${r.number} is not an approved self-study report awaiting hand-off`);
      if (!reference.trim()) throw invalid("reference is required");
      const c = cycles.find((x) => x.id === r.cycleId);
      if (c) touch(c, { status: "submitted" });
      return touch(r, { handedOffAt: new Date().toISOString(), handoffReference: reference });
    },
    addItem: async (recordId, input) => {
      const r = find(records, recordId);
      if (r.status !== "draft" && r.status !== "open") throw conflict("items are added to a draft or open record");
      if (!recordItemKinds(r.kind).includes(input.kind)) throw invalid(`a ${r.kind} record takes no ${input.kind} items`);
      r.items.push(item(r.id, input.kind, input.title, { owner: input.owner ?? null, dueOn: input.dueOn ?? null }));
      return touch(r, {});
    },
    progressItem: async (itemId, version, input) => {
      const { r, it } = findItem(itemId, version);
      const ok: Record<string, string[]> = { open: ["in_progress", "done", "cancelled"], in_progress: ["done", "cancelled"], done: ["verified", "in_progress"] };
      if (!(ok[it.status] ?? []).includes(input.status)) throw conflict(`${it.status} → ${input.status} is not allowed`);
      Object.assign(it, { status: input.status, evidenceRef: input.evidenceRef ?? it.evidenceRef, version: it.version + 1 });
      return touch(r, {});
    },
    verifyItem: async (itemId, version, evidenceRef) => {
      const { r, it } = findItem(itemId, version);
      if (it.status !== "done") throw conflict("only a done item is verified");
      const ref = evidenceRef || it.evidenceRef;
      if (!ref) throw invalid("an evidence reference is required");
      Object.assign(it, { status: "verified", evidenceRef: ref, verifiedBy: "mock-admin", version: it.version + 1 });
      return touch(r, {});
    },
  };
}
