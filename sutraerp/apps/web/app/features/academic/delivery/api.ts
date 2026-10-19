/**
 * Slice-4B API layer (timetable, sessions + attendance, monitoring, internal assessment and
 * coursework). Same conventions as `features/institution/api.ts`: one interface, an HTTP
 * implementation (CSRF, Idempotency-Key on creates, If-Match on commands, problem+json →
 * `ApiProblem`) and an in-memory mock (`./mock`). Every path template is checked against the
 * generated OpenAPI `paths`.
 */
import { ApiProblem, isProblem, isUnsafeMethod, newIdempotencyKey, type paths } from "@sutra/api-client";
import { getCsrfToken } from "../../../lib/api";
import { fill } from "../../institution/api";
import type {
  AssessmentBlueprint,
  AssessmentWindow,
  AttendanceCorrection,
  AttendanceCorrectionQueueItem,
  AttendanceMark,
  AttendancePolicy,
  AttendanceSummary,
  BlueprintComponent,
  CreatePolicy,
  CreateRequirement,
  CreateSession,
  CreateWindow,
  CorrectionStatus,
  InterventionCommand,
  InternalAssessmentResult,
  Items,
  MonitoringCase,
  MyAssignment,
  MyAttendanceLine,
  OpenCase,
  RequestCorrection,
  SchedulingRequirement,
  SessionCommand,
  TaughtSection,
  TeachingSection,
  TeachingSession,
  Timetable,
  TimetableCommand,
  WeeklyTimetable,
} from "./types";

export interface DeliveryApi {
  // timetable
  sections(institutionId: string, termId: string): Promise<TeachingSection[]>;
  listRequirements(institutionId: string, termId: string): Promise<SchedulingRequirement[]>;
  createRequirement(institutionId: string, input: CreateRequirement): Promise<SchedulingRequirement>;
  listTimetables(institutionId: string, termId: string): Promise<Timetable[]>;
  getTimetable(id: string): Promise<Timetable>;
  createTimetable(institutionId: string, input: { academicTermId: string; name: string }): Promise<Timetable>;
  timetableCommand(id: string, version: number, cmd: TimetableCommand, reason?: string): Promise<Timetable>;
  requestRevision(id: string, reason: string): Promise<Timetable>;
  myTimetable(): Promise<WeeklyTimetable>;
  teachingTimetable(): Promise<WeeklyTimetable>;
  // sessions + attendance
  facultyAttendance(): Promise<TaughtSection[]>;
  createSession(sectionId: string, input: CreateSession): Promise<TeachingSession>;
  getSession(id: string): Promise<TeachingSession>;
  sessionCommand(id: string, version: number, cmd: SessionCommand, reason?: string): Promise<TeachingSession>;
  capture(id: string, version: number, records: { studentId: string; mark: AttendanceMark }[]): Promise<TeachingSession>;
  submitAttendance(id: string, version: number): Promise<TeachingSession>;
  finalise(id: string, version: number): Promise<TeachingSession>;
  // governed corrections of finalised registers (faculty request; HOD / administrator decide)
  listCorrections(sessionId: string): Promise<AttendanceCorrection[]>;
  requestCorrection(sessionId: string, input: RequestCorrection): Promise<AttendanceCorrection>;
  withdrawCorrection(id: string, version: number, reason?: string): Promise<AttendanceCorrection>;
  correctionQueue(institutionId: string, status?: CorrectionStatus): Promise<AttendanceCorrectionQueueItem[]>;
  decideCorrection(id: string, version: number, decision: "approve" | "reject", reason?: string): Promise<AttendanceCorrection>;
  listPolicies(institutionId: string): Promise<AttendancePolicy[]>;
  createPolicy(institutionId: string, input: CreatePolicy): Promise<AttendancePolicy>;
  submitPolicy(id: string, version: number): Promise<AttendancePolicy>;
  summary(sectionId: string): Promise<AttendanceSummary>;
  myAttendance(): Promise<MyAttendanceLine[]>;
  // monitoring
  listCases(institutionId: string): Promise<MonitoringCase[]>;
  openCase(institutionId: string, input: OpenCase): Promise<MonitoringCase>;
  assessRisk(id: string, category: string, level?: string, rationale?: string): Promise<MonitoringCase>;
  publishOutcome(id: string, version: number, summary: string): Promise<MonitoringCase>;
  submitReview(id: string, version: number, input: { requiresIntervention: boolean; interventionTitle?: string; reviewerSubject?: string }): Promise<MonitoringCase>;
  certify(id: string, version: number): Promise<MonitoringCase>;
  closeCase(id: string, version: number, reason?: string): Promise<MonitoringCase>;
  interventionCommand(caseId: string, interventionId: string, version: number, cmd: InterventionCommand, body?: Record<string, string>): Promise<MonitoringCase>;
  // internal assessment
  listBlueprints(sectionId: string): Promise<AssessmentBlueprint[]>;
  createBlueprint(sectionId: string, input: { title: string; totalMarks: number; components: BlueprintComponent[] }): Promise<AssessmentBlueprint>;
  publishBlueprint(id: string, version: number): Promise<AssessmentBlueprint>;
  listWindows(sectionId: string): Promise<AssessmentWindow[]>;
  createWindow(sectionId: string, input: CreateWindow): Promise<AssessmentWindow>;
  windowCommand(id: string, version: number, cmd: "open" | "close"): Promise<AssessmentWindow>;
  listResults(sectionId: string): Promise<InternalAssessmentResult[]>;
  finalizeResults(sectionId: string): Promise<InternalAssessmentResult[]>;
  myAssignments(): Promise<MyAssignment[]>;
  submitAssignment(id: string, content: string): Promise<MyAssignment>;
}

const B = "/api/v1/academic";
export const PATHS = {
  sections: `${B}/institutions/{id}/teaching-sections`,
  requirements: `${B}/institutions/{id}/scheduling-requirements`,
  timetables: `${B}/institutions/{id}/timetables`,
  timetable: `${B}/timetables/{id}`,
  ttGenerate: `${B}/timetables/{id}/generate`,
  ttValidate: `${B}/timetables/{id}/validate`,
  ttSubmit: `${B}/timetables/{id}/submit`,
  ttPublish: `${B}/timetables/{id}/publish`,
  ttActivate: `${B}/timetables/{id}/activate`,
  ttFreeze: `${B}/timetables/{id}/freeze`,
  ttWithdraw: `${B}/timetables/{id}/withdraw`,
  ttRetire: `${B}/timetables/{id}/retire`,
  ttArchive: `${B}/timetables/{id}/archive`,
  ttRevisions: `${B}/timetables/{id}/revisions`,
  meTimetable: `${B}/me/timetable`,
  myTimetable: `${B}/my/timetable`,
  myAttendance: `${B}/my/attendance`,
  meAttendance: `${B}/me/attendance`,
  sessions: `${B}/sections/{id}/sessions`,
  session: `${B}/sessions/{id}`,
  sStart: `${B}/sessions/{id}/start`,
  sPause: `${B}/sessions/{id}/pause`,
  sResume: `${B}/sessions/{id}/resume`,
  sComplete: `${B}/sessions/{id}/complete`,
  sCancel: `${B}/sessions/{id}/cancel`,
  sLock: `${B}/sessions/{id}/lock`,
  attendance: `${B}/sessions/{id}/attendance`,
  submitAttendance: `${B}/sessions/{id}/submit-attendance`,
  finalise: `${B}/sessions/{id}/finalise-attendance`,
  corrections: `${B}/sessions/{id}/attendance-corrections`,
  correctionQueue: `${B}/institutions/{id}/attendance-corrections`,
  correctionDecision: `${B}/attendance-corrections/{id}/decision`,
  correctionWithdraw: `${B}/attendance-corrections/{id}/withdraw`,
  policies: `${B}/institutions/{id}/attendance-policies`,
  policySubmit: `${B}/attendance-policies/{id}/submit`,
  summary: `${B}/sections/{id}/attendance-summary`,
  cases: `${B}/institutions/{id}/monitoring-cases`,
  risk: `${B}/monitoring-cases/{id}/risk-assessments`,
  outcome: `${B}/monitoring-cases/{id}/publish-outcome`,
  review: `${B}/monitoring-cases/{id}/submit-review`,
  certify: `${B}/monitoring-cases/{id}/certify`,
  close: `${B}/monitoring-cases/{id}/close`,
  ivAssign: `${B}/monitoring-cases/{id}/interventions/{interventionId}/assign`,
  ivTrigger: `${B}/monitoring-cases/{id}/interventions/{interventionId}/trigger`,
  ivStart: `${B}/monitoring-cases/{id}/interventions/{interventionId}/start`,
  ivComplete: `${B}/monitoring-cases/{id}/interventions/{interventionId}/complete`,
  ivCancel: `${B}/monitoring-cases/{id}/interventions/{interventionId}/cancel`,
  blueprints: `${B}/sections/{id}/assessment-blueprints`,
  bpPublish: `${B}/assessment-blueprints/{id}/publish`,
  windows: `${B}/sections/{id}/assessment-windows`,
  wOpen: `${B}/assessment-windows/{id}/open`,
  wClose: `${B}/assessment-windows/{id}/close`,
  results: `${B}/sections/{id}/internal-assessment`,
  finalize: `${B}/sections/{id}/internal-assessment/finalize`,
  meAssignments: `${B}/me/assignments`,
  meSubmit: `${B}/me/assignments/{id}/submission`,
} as const satisfies Record<string, keyof paths>;

type Fetch = (input: string, init: RequestInit) => Promise<Response>;

interface RequestOptions {
  body?: unknown;
  ifMatch?: number;
  idempotent?: boolean;
}

const TT_CMD: Record<TimetableCommand, string> = {
  generate: PATHS.ttGenerate,
  validate: PATHS.ttValidate,
  submit: PATHS.ttSubmit,
  publish: PATHS.ttPublish,
  activate: PATHS.ttActivate,
  freeze: PATHS.ttFreeze,
  withdraw: PATHS.ttWithdraw,
  retire: PATHS.ttRetire,
  archive: PATHS.ttArchive,
};
const SESSION_CMD: Record<SessionCommand, string> = {
  start: PATHS.sStart,
  pause: PATHS.sPause,
  resume: PATHS.sResume,
  complete: PATHS.sComplete,
  cancel: PATHS.sCancel,
  lock: PATHS.sLock,
};
const IV_CMD: Record<InterventionCommand, string> = {
  assign: PATHS.ivAssign,
  trigger: PATHS.ivTrigger,
  start: PATHS.ivStart,
  complete: PATHS.ivComplete,
  cancel: PATHS.ivCancel,
};

export function createHttpDeliveryApi(fetchImpl: Fetch = (i, init) => fetch(i, init), baseUrl: string = import.meta.env.VITE_API_BASE ?? ""): DeliveryApi {
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
  const term = (t: string) => `?academicTermId=${encodeURIComponent(t)}`;
  return {
    sections: (i, t) => items(fill(PATHS.sections, id(i)) + term(t)),
    listRequirements: (i, t) => items(fill(PATHS.requirements, id(i)) + term(t)),
    createRequirement: (i, input) => request("POST", fill(PATHS.requirements, id(i)), { body: input, idempotent: true }),
    listTimetables: (i, t) => items(fill(PATHS.timetables, id(i)) + term(t)),
    getTimetable: (i) => request("GET", fill(PATHS.timetable, id(i))),
    createTimetable: (i, input) => request("POST", fill(PATHS.timetables, id(i)), { body: input, idempotent: true }),
    timetableCommand: (i, v, cmd, reason) => request("POST", fill(TT_CMD[cmd], id(i)), { body: reasonBody(reason), ifMatch: v }),
    requestRevision: (i, reason) => request("POST", fill(PATHS.ttRevisions, id(i)), { body: { reason }, idempotent: true }),
    myTimetable: () => request("GET", PATHS.meTimetable),
    teachingTimetable: () => request("GET", PATHS.myTimetable),
    facultyAttendance: () => items(PATHS.myAttendance),
    createSession: (s, input) => request("POST", fill(PATHS.sessions, id(s)), { body: input, idempotent: true }),
    getSession: (i) => request("GET", fill(PATHS.session, id(i))),
    sessionCommand: (i, v, cmd, reason) => request("POST", fill(SESSION_CMD[cmd], id(i)), { body: reasonBody(reason), ifMatch: v }),
    capture: (i, v, records) => request("POST", fill(PATHS.attendance, id(i)), { body: { records }, ifMatch: v }),
    submitAttendance: (i, v) => request("POST", fill(PATHS.submitAttendance, id(i)), { ifMatch: v }),
    finalise: (i, v) => request("POST", fill(PATHS.finalise, id(i)), { ifMatch: v }),
    listCorrections: (s) => items(fill(PATHS.corrections, id(s))),
    requestCorrection: (s, input) => request("POST", fill(PATHS.corrections, id(s)), { body: input, idempotent: true }),
    withdrawCorrection: (i, v, reason) => request("POST", fill(PATHS.correctionWithdraw, id(i)), { body: reasonBody(reason), ifMatch: v }),
    correctionQueue: (i, status) => items(fill(PATHS.correctionQueue, id(i)) + (status ? `?status=${encodeURIComponent(status)}` : "")),
    decideCorrection: (i, v, decision, reason) => request("POST", fill(PATHS.correctionDecision, id(i)), { body: reason ? { decision, reason } : { decision }, ifMatch: v }),
    listPolicies: (i) => items(fill(PATHS.policies, id(i))),
    createPolicy: (i, input) => request("POST", fill(PATHS.policies, id(i)), { body: input, idempotent: true }),
    submitPolicy: (i, v) => request("POST", fill(PATHS.policySubmit, id(i)), { ifMatch: v }),
    summary: (s) => request("GET", fill(PATHS.summary, id(s))),
    myAttendance: () => items(PATHS.meAttendance),
    listCases: (i) => items(fill(PATHS.cases, id(i))),
    openCase: (i, input) => request("POST", fill(PATHS.cases, id(i)), { body: input, idempotent: true }),
    assessRisk: (i, category, level, rationale) => request("POST", fill(PATHS.risk, id(i)), { body: { category, level, rationale }, idempotent: true }),
    publishOutcome: (i, v, summary) => request("POST", fill(PATHS.outcome, id(i)), { body: { summary }, ifMatch: v }),
    submitReview: (i, v, input) => request("POST", fill(PATHS.review, id(i)), { body: input, ifMatch: v }),
    certify: (i, v) => request("POST", fill(PATHS.certify, id(i)), { ifMatch: v }),
    closeCase: (i, v, reason) => request("POST", fill(PATHS.close, id(i)), { body: reasonBody(reason), ifMatch: v }),
    interventionCommand: (c, iv, v, cmd, body) => request("POST", fill(IV_CMD[cmd], { id: c, interventionId: iv }), { body: body ?? {}, ifMatch: v }),
    listBlueprints: (s) => items(fill(PATHS.blueprints, id(s))),
    createBlueprint: (s, input) => request("POST", fill(PATHS.blueprints, id(s)), { body: input, idempotent: true }),
    publishBlueprint: (i, v) => request("POST", fill(PATHS.bpPublish, id(i)), { ifMatch: v }),
    listWindows: (s) => items(fill(PATHS.windows, id(s))),
    createWindow: (s, input) => request("POST", fill(PATHS.windows, id(s)), { body: input, idempotent: true }),
    windowCommand: (i, v, cmd) => request("POST", fill(cmd === "open" ? PATHS.wOpen : PATHS.wClose, id(i)), { ifMatch: v }),
    listResults: (s) => items(fill(PATHS.results, id(s))),
    finalizeResults: async (s) => (await request<Items<InternalAssessmentResult>>("POST", fill(PATHS.finalize, id(s)))).items,
    myAssignments: () => items(PATHS.meAssignments),
    submitAssignment: (i, content) => request("POST", fill(PATHS.meSubmit, id(i)), { body: { content }, idempotent: true }),
  };
}
