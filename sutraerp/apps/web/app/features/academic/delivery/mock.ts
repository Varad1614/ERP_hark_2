/**
 * In-memory stand-in of the slice-4B API (mock mode, unit tests). Small but coherent: one term,
 * one CS101 section taught by the signed-in faculty member, an operational timetable, a
 * session ready for attendance, an attendance policy, a monitoring case, a published blueprint
 * with an open assignment. Rules mirror the API loosely (the API stays authoritative).
 */
import { ApiProblem } from "@sutra/api-client";
import type { DeliveryApi } from "./api";
import type {
  AssessmentBlueprint,
  AttendanceCorrection,
  AttendanceCorrectionQueueItem,
  AssessmentWindow,
  AttendancePolicy,
  InternalAssessmentResult,
  MonitoringCase,
  MyAssignment,
  SchedulingRequirement,
  TeachingSection,
  TeachingSession,
  Timetable,
  TimetableSlot,
  TimetableStatus,
} from "./types";

const INST = "00000000-0000-7000-8000-00000000b400";
const TERM = "00000000-0000-7000-8000-00000000b401";
const SECTION = "00000000-0000-7000-8000-00000000b402";
const OFFERING = "00000000-0000-7000-8000-00000000b403";
const S1 = "00000000-0000-7000-8000-00000000b4a1";
const S2 = "00000000-0000-7000-8000-00000000b4a2";
let seq = 1;
const uid = () => `00000000-0000-7000-8000-${String(4000 + seq++).padStart(12, "0")}`;

function conflict(detail: string): never {
  throw new ApiProblem({ type: "about:blank", title: "Conflict", status: 409, detail, errorCode: "conflict" });
}

function find<T extends { id: string }>(rows: T[], id: string): T {
  const r = rows.find((x) => x.id === id);
  if (!r) throw new ApiProblem({ type: "about:blank", title: "Not found", status: 404, errorCode: "not-found" });
  return r;
}

const slot = (timetableId: string, day: number, from: string, to: string): TimetableSlot => ({
  id: uid(),
  timetableId,
  sectionId: SECTION,
  offeringId: OFFERING,
  offeringCode: "CS101-T1",
  title: "Programming I",
  sectionCode: "A",
  dayOfWeek: day,
  startsAt: from,
  endsAt: to,
  roomLabel: "R101",
  facultyNames: ["Dr. Meera Shah"],
});

const NEXT: Partial<Record<string, TimetableStatus>> = {
  generate: "draft",
  validate: "validated",
  submit: "approved",
  publish: "published",
  activate: "active",
  freeze: "frozen",
  retire: "retired",
  archive: "archived",
  withdraw: "archived",
};

export function createMockDeliveryApi(): DeliveryApi {
  const sections: TeachingSection[] = [{ sectionId: SECTION, offeringId: OFFERING, academicTermId: TERM, offeringCode: "CS101-T1", title: "Programming I", sectionCode: "A", status: "active", offeringStatus: "active" }];
  const requirements: SchedulingRequirement[] = [
    { id: uid(), institutionId: INST, academicTermId: TERM, offeringId: OFFERING, sectionId: SECTION, sessionsPerWeek: 3, sessionMinutes: 60, preferredDays: [], roomLabel: "R101", status: "active", version: 1 },
  ];
  const ttId = uid();
  const timetables: Timetable[] = [
    {
      id: ttId,
      institutionId: INST,
      academicTermId: TERM,
      name: "Term 1 timetable",
      versionNo: 1,
      status: "active",
      pendingDecision: false,
      version: 7,
      validationReport: { valid: true, conflicts: [], validatedAt: "2026-07-01T09:00:00Z" },
      slots: [slot(ttId, 1, "09:00", "10:00"), slot(ttId, 3, "09:00", "10:00"), slot(ttId, 5, "11:00", "12:00")],
    },
  ];
  const roster = () => [
    { studentId: S1, studentNumber: "S-0001", displayName: "Aarav Desai", seated: true, mark: null },
    { studentId: S2, studentNumber: "S-0002", displayName: "Bina Rao", seated: true, mark: null },
  ];
  const sessions: TeachingSession[] = [
    { id: uid(), sectionId: SECTION, offeringId: OFFERING, sessionDate: "2026-07-06", startsAt: "09:00", endsAt: "10:00", topic: "Variables", status: "scheduled", attendanceStatus: "not_started", version: 1, records: roster() },
  ];
  const corrections: AttendanceCorrection[] = [];
  const policies: AttendancePolicy[] = [
    { id: uid(), institutionId: INST, name: "Default attendance rule", thresholdPercent: 75, minSessions: 3, lateCountsAsPresent: true, excludeExcused: true, status: "effective", version: 3 },
  ];
  const cases: MonitoringCase[] = [
    { id: uid(), institutionId: INST, scopeKind: "section", sectionId: SECTION, title: "CS101 attendance watch", ownerSubject: "dev|admin@sutra.dev", reviewerSubject: "dev|meera.shah@sutra.dev", status: "open", cycleNo: 1, version: 1, riskAssessments: [], interventions: [] },
  ];
  const bpId = uid();
  const blueprints: AssessmentBlueprint[] = [
    {
      id: bpId,
      sectionId: SECTION,
      title: "CS101 internal",
      totalMarks: 50,
      status: "published",
      version: 2,
      components: [
        { key: "a1", title: "Assignment 1", kind: "assignment", weight: 40 },
        { key: "t1", title: "Test 1", kind: "test", weight: 60 },
      ],
    },
  ];
  const windows: AssessmentWindow[] = [
    { id: uid(), sectionId: SECTION, blueprintId: bpId, componentKey: "a1", title: "Essay on recursion", kind: "assignment", opensAt: "2026-07-01T09:00:00Z", dueAt: "2026-07-20T18:00:00Z", maxMarks: 10, status: "open", version: 2 },
  ];
  const results: InternalAssessmentResult[] = [];
  const mine: Record<string, MyAssignment["submission"]> = {};
  const assignment = (w: AssessmentWindow): MyAssignment => ({
    id: w.id,
    sectionId: w.sectionId,
    offeringCode: "CS101-T1",
    courseTitle: "Programming I",
    title: w.title,
    kind: w.kind,
    opensAt: w.opensAt,
    dueAt: w.dueAt,
    extendedDueAt: w.extendedDueAt,
    maxMarks: w.maxMarks,
    status: w.status,
    submission: mine[w.id] ?? null,
  });
  const bump = <T extends { version: number }>(r: T, patch: Partial<T>): T => Object.assign(r, patch, { version: r.version + 1 });

  return {
    sections: async () => sections,
    listRequirements: async () => requirements,
    createRequirement: async (i, input) => {
      const r: SchedulingRequirement = { id: uid(), institutionId: i, academicTermId: TERM, offeringId: OFFERING, preferredDays: [], roomLabel: null, status: "active", version: 1, ...input };
      requirements.push(r);
      return r;
    },
    listTimetables: async () => timetables,
    getTimetable: async (i) => find(timetables, i),
    createTimetable: async (i, input) => {
      const t: Timetable = { id: uid(), institutionId: i, academicTermId: input.academicTermId, name: input.name, versionNo: 1, status: "planning", pendingDecision: false, version: 1, slots: [] };
      timetables.unshift(t);
      return t;
    },
    timetableCommand: async (i, _v, cmd) => {
      const t = find(timetables, i);
      if (cmd === "generate") t.slots = [slot(t.id, 2, "09:00", "10:00"), slot(t.id, 4, "09:00", "10:00")];
      if (cmd === "activate" && timetables.some((x) => x.id !== t.id && x.status === "active")) conflict("another timetable is operational for this term");
      return bump(t, { status: NEXT[cmd] ?? t.status });
    },
    requestRevision: async (i) => {
      const base = find(timetables, i);
      const t: Timetable = { ...base, id: uid(), versionNo: base.versionNo + 1, revisionOfId: base.id, status: "draft", version: 1, slots: base.slots.map((s) => ({ ...s, id: uid() })) };
      timetables.unshift(t);
      return t;
    },
    myTimetable: async () => ({ items: timetables.find((t) => t.status === "active")?.slots ?? [], weekOf: "2026-07-06" }),
    teachingTimetable: async () => ({ items: timetables.find((t) => t.status === "active")?.slots ?? [], weekOf: "2026-07-06" }),
    facultyAttendance: async () => [{ sectionId: SECTION, offeringId: OFFERING, offeringCode: "CS101-T1", title: "Programming I", sectionCode: "A", seatedCount: 2, sessions }],
    createSession: async (s, input) => {
      const x: TeachingSession = {
        id: uid(),
        sectionId: s,
        offeringId: OFFERING,
        sessionDate: input.sessionDate,
        startsAt: input.startsAt ?? "09:00",
        endsAt: input.endsAt ?? "10:00",
        topic: input.topic ?? null,
        status: "scheduled",
        attendanceStatus: "not_started",
        version: 1,
        records: roster(),
      };
      sessions.unshift(x);
      return x;
    },
    getSession: async (i) => find(sessions, i),
    sessionCommand: async (i, _v, cmd) => {
      const s = find(sessions, i);
      const to = { start: "started", pause: "paused", resume: "in_progress", complete: "completed", cancel: "cancelled", lock: "locked" } as const;
      return bump(s, { status: to[cmd] });
    },
    capture: async (i, _v, records) => {
      const s = find(sessions, i);
      if (s.status === "scheduled") conflict("attendance cannot be recorded for a scheduled session");
      if (s.attendanceStatus === "finalised") conflict("attendance is finalised; use a governed correction");
      for (const r of records) {
        const line = s.records.find((x) => x.studentId === r.studentId);
        if (line) {
          line.mark = r.mark;
          line.id ??= uid();
        }
      }
      return bump(s, { attendanceStatus: "capturing", status: s.status === "started" ? "in_progress" : s.status });
    },
    submitAttendance: async (i) => {
      const s = find(sessions, i);
      if (s.attendanceStatus !== "capturing") conflict(`attendance register cannot move from ${s.attendanceStatus} to submitted`);
      if (s.records.some((r) => !r.mark)) conflict("some seated students have no attendance mark yet");
      return bump(s, { attendanceStatus: "submitted" });
    },
    finalise: async (i) => {
      const s = find(sessions, i);
      if (s.attendanceStatus !== "submitted") conflict(`attendance in capture state ${s.attendanceStatus} cannot be finalised`);
      return bump(s, { attendanceStatus: "finalised" });
    },
    listCorrections: async (sid) => corrections.filter((c) => c.sessionId === sid),
    requestCorrection: async (sid, input) => {
      const s = find(sessions, sid);
      if (s.attendanceStatus !== "finalised") conflict("only finalised attendance is corrected; edit the open register instead");
      const rec = s.records.find((r) => r.id === input.recordId || r.studentId === input.recordId);
      if (!rec?.mark) conflict("the record has no mark");
      if (corrections.some((c) => c.recordId === input.recordId && c.status === "requested")) conflict("a correction of this record is already pending");
      const c: AttendanceCorrection = { id: uid(), sessionId: sid, recordId: input.recordId, studentId: rec.studentId, fromMark: rec.mark, toMark: input.toMark, reason: input.reason, evidence: input.evidence ?? null, status: "requested", requestedBy: "dev|meera.shah@sutra.dev", version: 1, createdAt: new Date().toISOString() };
      corrections.unshift(c);
      return c;
    },
    withdrawCorrection: async (i) => bump(find(corrections, i), { status: "withdrawn" }),
    correctionQueue: async (_i, status) =>
      corrections
        .filter((c) => !status || c.status === status)
        .map((c): AttendanceCorrectionQueueItem => {
          const s = find(sessions, c.sessionId);
          const rec = s.records.find((r) => r.studentId === c.studentId);
          return { ...c, studentNumber: rec?.studentNumber ?? "", displayName: rec?.displayName ?? "", sectionId: s.sectionId, offeringCode: "CS101-T1", sectionCode: "A", sessionDate: s.sessionDate, startsAt: s.startsAt };
        }),
    decideCorrection: async (i, _v, decision, reason) => {
      const c = find(corrections, i);
      if (c.status !== "requested") conflict(`the correction is ${c.status}; it has no pending decision`);
      if (decision === "reject" && !reason) conflict("a rejection needs a reason");
      if (decision === "approve") {
        const rec = find(sessions, c.sessionId).records.find((r) => r.studentId === c.studentId);
        if (rec) rec.mark = c.toMark;
      }
      return bump(c, { status: decision === "approve" ? "applied" : "rejected", statusReason: reason ?? null, decidedBy: "dev|admin@sutra.dev", decidedAt: new Date().toISOString() });
    },
    listPolicies: async () => policies,
    createPolicy: async (i, input) => {
      const p: AttendancePolicy = { id: uid(), institutionId: i, name: input.name, thresholdPercent: input.thresholdPercent, minSessions: input.minSessions ?? 1, lateCountsAsPresent: true, excludeExcused: true, status: "draft", version: 1 };
      policies.unshift(p);
      return p;
    },
    submitPolicy: async (i) => {
      for (const p of policies) if (p.status === "effective") p.status = "superseded";
      return bump(find(policies, i), { status: "effective" });
    },
    summary: async (s) => ({
      sectionId: s,
      thresholdPercent: 75,
      policyId: policies[0]?.id,
      finalisedSessions: 4,
      items: [
        { studentId: S1, studentNumber: "S-0001", displayName: "Aarav Desai", attended: 4, total: 4, percent: 100, belowThreshold: false },
        { studentId: S2, studentNumber: "S-0002", displayName: "Bina Rao", attended: 2, total: 4, percent: 50, belowThreshold: true },
      ],
    }),
    myAttendance: async () => ({ items: [{ sectionId: SECTION, offeringCode: "CS101-T1", title: "Programming I", sectionCode: "A", attended: 4, total: 5, percent: 80, belowThreshold: false, thresholdPercent: 75 }], overall: { attended: 4, total: 5, percent: 80 } }),
    listCases: async () => cases,
    openCase: async (i, input) => {
      const c: MonitoringCase = { id: uid(), institutionId: i, scopeKind: input.scopeKind, sectionId: input.sectionId, title: input.title, ownerSubject: "dev|admin@sutra.dev", reviewerSubject: input.reviewerSubject ?? null, status: "open", cycleNo: 1, version: 1, riskAssessments: [], interventions: [] };
      cases.unshift(c);
      return c;
    },
    assessRisk: async (i, category) => {
      const c = find(cases, i);
      c.riskAssessments.unshift({ id: uid(), category, level: "moderate", basis: "attendance_rule", createdAt: "2026-07-10T09:00:00Z" });
      return bump(c, { status: "under_assessment" });
    },
    publishOutcome: async (i, _v, summary) => bump(find(cases, i), { outcomeSummary: summary, outcomePublishedAt: "2026-07-10T10:00:00Z" }),
    submitReview: async (i, _v, input) => {
      const c = find(cases, i);
      if (input.requiresIntervention) c.interventions.push({ id: uid(), title: input.interventionTitle ?? "Corrective intervention", status: "planned", version: 1, ownerSubject: null });
      return bump(c, { status: input.requiresIntervention ? "intervention" : "under_assessment" });
    },
    certify: async (i) => bump(find(cases, i), { status: "certified" }),
    closeCase: async (i) => bump(find(cases, i), { status: "closed" }),
    interventionCommand: async (c, iv, _v, cmd, body) => {
      const k = find(cases, c);
      const x = find(k.interventions, iv);
      const to = { assign: x.status, trigger: "triggered", start: "in_progress", complete: "completed", cancel: "cancelled" }[cmd];
      bump(x, { status: to, ownerSubject: body?.ownerSubject ?? x.ownerSubject });
      return bump(k, {});
    },
    listBlueprints: async () => blueprints,
    createBlueprint: async (s, input) => {
      const b: AssessmentBlueprint = { id: uid(), sectionId: s, status: "draft", version: 1, ...input };
      blueprints.unshift(b);
      return b;
    },
    publishBlueprint: async (i) => {
      for (const b of blueprints) if (b.status === "published") b.status = "superseded";
      return bump(find(blueprints, i), { status: "published" });
    },
    listWindows: async () => windows,
    createWindow: async (s, input) => {
      const comp = blueprints.find((b) => b.id === input.blueprintId)?.components.find((c) => c.key === input.componentKey);
      const w: AssessmentWindow = { id: uid(), sectionId: s, kind: comp?.kind ?? "assignment", status: "scheduled", version: 1, ...input };
      windows.push(w);
      return w;
    },
    windowCommand: async (i, _v, cmd) => bump(find(windows, i), { status: cmd === "open" ? "open" : "closed" }),
    listResults: async () => results,
    finalizeResults: async () => {
      if (windows.some((w) => w.status !== "closed")) conflict("finalisation is blocked: some windows are not closed");
      results.push({ id: uid(), studentId: S1, studentNumber: "S-0001", displayName: "Aarav Desai", marksObtained: 42, maxMarks: 50, status: "finalized", resultVersion: 1, version: 2 });
      return results;
    },
    myAssignments: async () => windows.filter((w) => w.status !== "scheduled").map(assignment),
    submitAssignment: async (i) => {
      const w = find(windows, i);
      if (mine[i]) conflict("you have already submitted this assignment");
      if (w.status !== "open" && w.status !== "extended") conflict("submissions are not accepted");
      mine[i] = { status: "submitted", submittedAt: "2026-07-12T10:00:00Z", marks: null };
      return assignment(w);
    },
  };
}
