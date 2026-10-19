/**
 * In-memory stand-in of the conduct-slice API (mock mode and tests). It follows the server's main
 * rules: one live conduct record per sitting (scheduled → in progress ⇄ suspended → completed,
 * or postponed under a contingency decision); completion records one attendance row per listed
 * candidate; a seating plan is published from draft and revised with a reason; invigilation duties
 * are assigned → confirmed / released / replaced; exceptions are raised → classified / under
 * review → resolved → closed.
 */
import { ApiProblem } from "@sutra/api-client";
import type { ExamConductApi } from "./api";
import type { ConductRecord, ExamException, InvigilationDuty, SeatingPlan } from "./types";

const conflict = (detail: string) => new ApiProblem({ type: "about:blank", title: "Conflict", status: 409, errorCode: "conflict", detail });
const invalid = (detail: string) => new ApiProblem({ type: "about:blank", title: "Validation failed", status: 422, errorCode: "validation", detail });

const CONDUCT_NEXT: Record<string, Record<string, string>> = { start: { scheduled: "in_progress" }, suspend: { in_progress: "suspended" }, resume: { suspended: "in_progress" } };
const DUTY_NEXT: Record<string, Record<string, string>> = { confirm: { assigned: "confirmed" }, release: { assigned: "released", confirmed: "released" } };
const EXCEPTION_NEXT: Record<string, Record<string, string>> = { review: { raised: "under_review", classified: "under_review", resolved: "under_review" }, close: { resolved: "closed" } };
const LIVE_DUTY = ["assigned", "confirmed"];
const clone = <T>(x: T): T => JSON.parse(JSON.stringify(x)) as T;

export function createMockExamConductApi(): ExamConductApi {
  let n = 0;
  const id = () => `00000000-0000-7000-8000-c0${String(++n).padStart(10, "0")}`;
  const records: ConductRecord[] = [];
  const duties: InvigilationDuty[] = [];
  const plans: SeatingPlan[] = [];
  const exceptions: ExamException[] = [];
  const find = <T extends { id: string; version: number }>(list: T[], key: string, version: number): T => {
    const x = list.find((v) => v.id === key);
    if (!x) throw new ApiProblem({ type: "about:blank", title: "Not found", status: 404, errorCode: "not-found" });
    if (x.version !== version) throw new ApiProblem({ type: "about:blank", title: "Precondition failed", status: 412, errorCode: "precondition-failed" });
    return x;
  };
  const move = <T extends { status: string; version: number }>(x: T, table: Record<string, Record<string, string>>, cmd: string, label: string): T => {
    const to = table[cmd]?.[x.status];
    if (!to) throw conflict(`${label} is ${x.status}; it cannot ${cmd}`);
    x.status = to;
    x.version += 1;
    return clone(x);
  };
  const liveRecord = (sitting: string) => records.filter((r) => r.sittingId === sitting).find((r) => !["postponed", "terminated"].includes(r.status));
  const currentPlan = (sitting: string) => plans.filter((p) => p.sittingId === sitting).find((p) => ["draft", "published"].includes(p.status));
  const newPlan = (sitting: string, supersedesId: string | null): SeatingPlan => {
    const p: SeatingPlan = { id: id(), sittingId: sitting, planNo: plans.filter((x) => x.sittingId === sitting).length + 1, supersedesId, candidates: 0, status: "draft", seats: [], version: 1 };
    plans.push(p);
    return p;
  };
  return {
    conduct: async (sitting) => {
      const list = records.filter((r) => r.sittingId === sitting);
      const r = liveRecord(sitting) ?? list.at(-1);
      return r ? clone(r) : null;
    },
    openConduct: async (sitting) => {
      if (liveRecord(sitting)) throw conflict("the sitting already has a live conduct record");
      const r: ConductRecord = { id: id(), sessionId: "", sittingId: sitting, paperId: "", status: "scheduled", presentCount: 0, absentCount: 0, deniedCount: 0, attendance: [], version: 1 };
      records.push(r);
      return clone(r);
    },
    conductCommand: async (cid, v, cmd, reason) => {
      const r = find(records, cid, v);
      if (cmd === "suspend" && !reason) throw invalid("a reason is required");
      return move(r, CONDUCT_NEXT, cmd, "the conduct record");
    },
    completeConduct: async (cid, v, attendance, defaultOutcome) => {
      const r = find(records, cid, v);
      if (r.status !== "in_progress") throw conflict(`the conduct record is ${r.status}; it cannot complete`);
      for (const a of attendance) if (!["present", "absent", "denied_entry"].includes(a.outcome)) throw invalid("present, absent or denied_entry");
      if (defaultOutcome && !["present", "absent", "denied_entry"].includes(defaultOutcome)) throw invalid("present, absent or denied_entry");
      r.attendance = attendance.map((a) => ({ registrationId: a.registrationId, studentId: "", studentNumber: "", displayName: "", outcome: a.outcome, reason: a.reason ?? null }));
      r.presentCount = attendance.filter((a) => a.outcome === "present").length;
      r.absentCount = attendance.filter((a) => a.outcome === "absent").length;
      r.deniedCount = attendance.filter((a) => a.outcome === "denied_entry").length;
      r.status = "completed";
      r.completedAt = new Date().toISOString();
      r.version += 1;
      for (const d of duties) if (d.sittingId === r.sittingId && LIVE_DUTY.includes(d.status)) d.status = "completed";
      return clone(r);
    },
    postpone: async (cid, v, input) => {
      const r = find(records, cid, v);
      if (!input.reason) throw invalid("a reason is required");
      if (!["scheduled", "in_progress", "suspended"].includes(r.status)) throw conflict(`the conduct record is ${r.status}; it cannot be postponed`);
      if (r.pendingPostpone) throw conflict("a postponement is already awaiting its decision");
      r.pendingPostpone = { ...input };
      r.version += 1;
      return clone(r);
    },
    conductDecision: async (cid, v, decision) => {
      const r = find(records, cid, v);
      if (!r.pendingPostpone) throw conflict("no postponement is awaiting a decision");
      if (decision === "approve") r.status = "postponed";
      r.pendingPostpone = null;
      r.version += 1;
      return clone(r);
    },
    invigilation: async (sitting) => {
      const list = duties.filter((d) => d.sittingId === sitting);
      const live = list.filter((d) => LIVE_DUTY.includes(d.status));
      const examiners = live.filter((d) => d.role.endsWith("_examiner")).length;
      const assigned = live.length - examiners;
      return { sittingId: sitting, candidates: 0, ratio: 30, required: 1, assigned, examiners, shortage: Math.max(0, 1 - assigned), duties: clone(list) };
    },
    assignDuty: async (sitting, input) => {
      if (!input.subjectRef?.trim()) throw invalid("the invigilator's login subject is required");
      if (duties.some((d) => d.sittingId === sitting && d.subjectRef === input.subjectRef && LIVE_DUTY.includes(d.status))) throw conflict(`${input.subjectRef} already has a duty at this sitting`);
      const d: InvigilationDuty = { id: id(), institutionId: "", sittingId: sitting, subjectRef: input.subjectRef.trim(), name: input.name ?? null, role: input.role ?? "invigilator", status: "assigned", version: 1 };
      duties.push(d);
      return clone(d);
    },
    dutyCommand: async (did, v, cmd, reason) => {
      const d = find(duties, did, v);
      if (cmd === "release" && !reason) throw invalid("a reason is required");
      return move(d, DUTY_NEXT, cmd, "the duty");
    },
    replaceDuty: async (did, v, input) => {
      const d = find(duties, did, v);
      if (!input.reason) throw invalid("a reason is required");
      if (!LIVE_DUTY.includes(d.status)) throw conflict(`the duty is ${d.status}; it cannot be replaced`);
      d.status = "replaced";
      d.version += 1;
      const r: InvigilationDuty = { id: id(), institutionId: d.institutionId, sittingId: d.sittingId, subjectRef: input.subjectRef, name: input.name ?? null, role: d.role, replacesId: d.id, status: "assigned", version: 1 };
      duties.push(r);
      return clone(r);
    },
    seating: async (sitting) => {
      const p = currentPlan(sitting) ?? plans.filter((x) => x.sittingId === sitting).at(-1);
      return p ? clone(p) : null;
    },
    generateSeating: async (sitting) => {
      const cur = currentPlan(sitting);
      if (cur?.status === "published") throw conflict("the published seating plan must be revised instead");
      if (cur) cur.status = "cancelled";
      return clone(newPlan(sitting, null));
    },
    publishSeating: async (pid, v) => {
      const p = find(plans, pid, v);
      if (p.status !== "draft") throw conflict(`seating plan ${p.planNo} is ${p.status}; it cannot be published`);
      p.status = "published";
      p.publishedAt = new Date().toISOString();
      p.version += 1;
      return clone(p);
    },
    reviseSeating: async (pid, v, reason) => {
      const p = find(plans, pid, v);
      if (!reason) throw invalid("a reason is required");
      if (p.status !== "published") throw conflict(`seating plan ${p.planNo} is ${p.status}; only a published plan is revised`);
      p.status = "revised";
      p.statusReason = reason;
      p.version += 1;
      return clone(newPlan(p.sittingId, p.id));
    },
    exceptions: async (sid) => clone(exceptions.filter((x) => x.sessionId === sid)),
    raiseException: async (sid, input) => {
      if (!["conduct", "venue", "paper", "candidate", "technical", "other"].includes(input.category)) throw invalid("conduct, venue, paper, candidate, technical or other");
      if (!input.description?.trim()) throw invalid("description must be 1 … 2000 characters");
      const x: ExamException = {
        id: id(),
        institutionId: "",
        sessionId: sid,
        number: `EXC-${String(exceptions.length + 1).padStart(6, "0")}`,
        sittingId: input.sittingId ?? null,
        paperId: input.paperId ?? null,
        registrationId: input.registrationId ?? null,
        category: input.category,
        severity: input.severity ?? null,
        description: input.description.trim(),
        status: "raised",
        version: 1,
      };
      exceptions.push(x);
      return clone(x);
    },
    classifyException: async (xid, v, input) => {
      const x = find(exceptions, xid, v);
      if (!input.classification?.trim()) throw invalid("classification is required");
      if (!["raised", "under_review"].includes(x.status)) throw conflict(`exception ${x.number} is ${x.status}; it cannot be classified`);
      x.classification = input.classification.trim();
      if (input.severity) x.severity = input.severity;
      x.status = "classified";
      x.version += 1;
      return clone(x);
    },
    resolveException: async (xid, v, resolution) => {
      const x = find(exceptions, xid, v);
      if (!resolution?.trim()) throw invalid("resolution is required");
      if (!["classified", "under_review"].includes(x.status)) throw conflict(`exception ${x.number} is ${x.status}; it cannot be resolved`);
      x.resolution = resolution.trim();
      x.status = "resolved";
      x.version += 1;
      return clone(x);
    },
    exceptionCommand: async (xid, v, cmd) => move(find(exceptions, xid, v), EXCEPTION_NEXT, cmd, "the exception"),
  };
}
