/**
 * Conduct-slice API layer (DEF-I-007): exam conduct with attendance, invigilation duties, seating
 * plan publication and exception cases, on the existing `exam-scheduling` endpoints. Same
 * conventions as the 6A layer (`../setup/api`): CSRF on unsafe methods, `Idempotency-Key` on
 * creates, `If-Match` on state commands, problem+json → `ApiProblem`. Every path template is
 * checked against the generated OpenAPI (`satisfies Record<string, keyof paths>`).
 */
import { ApiProblem, isProblem, isUnsafeMethod, newIdempotencyKey, type paths } from "@sutra/api-client";
import { getCsrfToken } from "../../../lib/api";
import { fill } from "../../institution/api";
import type { AttendanceEntry, ConductRecord, ExamException, ExceptionInput, InvigilationDuty, InvigilationStaffing, Items, SeatingPlan } from "./types";

export type ConductCommand = "start" | "suspend" | "resume";
export type DutyCommand = "confirm" | "release";
export type ExceptionCommand = "review" | "close";

export interface ExamConductApi {
  /** The sitting's live (else latest) conduct record; `null` before one is opened. */
  conduct(sittingId: string): Promise<ConductRecord | null>;
  openConduct(sittingId: string): Promise<ConductRecord>;
  conductCommand(id: string, version: number, cmd: ConductCommand, reason?: string): Promise<ConductRecord>;
  /** Records attendance; unlisted candidates get `defaultOutcome` (server default `absent`). The paper becomes `conducted`. */
  completeConduct(id: string, version: number, attendance: AttendanceEntry[], defaultOutcome?: string): Promise<ConductRecord>;
  postpone(id: string, version: number, input: { newDate: string; newStartsAt: string; reason: string }): Promise<ConductRecord>;
  conductDecision(id: string, version: number, decision: "approve" | "reject", reason?: string): Promise<ConductRecord>;
  invigilation(sittingId: string): Promise<InvigilationStaffing>;
  assignDuty(sittingId: string, input: { subjectRef: string; name?: string; role?: string; venueId?: string }): Promise<InvigilationDuty>;
  dutyCommand(id: string, version: number, cmd: DutyCommand, reason?: string): Promise<InvigilationDuty>;
  replaceDuty(id: string, version: number, input: { subjectRef: string; name?: string; reason: string }): Promise<InvigilationDuty>;
  /** The sitting's current (draft / published) seating plan, else its latest; `null` before one is generated. */
  seating(sittingId: string): Promise<SeatingPlan | null>;
  generateSeating(sittingId: string): Promise<SeatingPlan>;
  publishSeating(id: string, version: number): Promise<SeatingPlan>;
  reviseSeating(id: string, version: number, reason: string): Promise<SeatingPlan>;
  exceptions(sessionId: string): Promise<ExamException[]>;
  raiseException(sessionId: string, input: ExceptionInput): Promise<ExamException>;
  classifyException(id: string, version: number, input: { classification: string; severity?: string; reason?: string }): Promise<ExamException>;
  resolveException(id: string, version: number, resolution: string): Promise<ExamException>;
  exceptionCommand(id: string, version: number, cmd: ExceptionCommand, reason?: string): Promise<ExamException>;
}

const B = "/api/v1/exams";
export const PATHS = {
  sittingConduct: `${B}/exam-sittings/{id}/conduct`,
  cStart: `${B}/conduct-records/{id}/start`,
  cSuspend: `${B}/conduct-records/{id}/suspend`,
  cResume: `${B}/conduct-records/{id}/resume`,
  cComplete: `${B}/conduct-records/{id}/complete`,
  cPostpone: `${B}/conduct-records/{id}/postpone`,
  cDecision: `${B}/conduct-records/{id}/decision`,
  invigilation: `${B}/exam-sittings/{id}/invigilation`,
  dConfirm: `${B}/invigilation-duties/{id}/confirm`,
  dRelease: `${B}/invigilation-duties/{id}/release`,
  dReplace: `${B}/invigilation-duties/{id}/replace`,
  seating: `${B}/exam-sittings/{id}/seating`,
  spPublish: `${B}/seating-plans/{id}/publish`,
  spRevise: `${B}/seating-plans/{id}/revise`,
  exceptions: `${B}/exam-sessions/{id}/exceptions`,
  xClassify: `${B}/exam-exceptions/{id}/classify`,
  xReview: `${B}/exam-exceptions/{id}/review`,
  xResolve: `${B}/exam-exceptions/{id}/resolve`,
  xClose: `${B}/exam-exceptions/{id}/close`,
} as const satisfies Record<string, keyof paths>;

const CONDUCT_CMD: Record<ConductCommand, string> = { start: PATHS.cStart, suspend: PATHS.cSuspend, resume: PATHS.cResume };
const DUTY_CMD: Record<DutyCommand, string> = { confirm: PATHS.dConfirm, release: PATHS.dRelease };
const EXCEPTION_CMD: Record<ExceptionCommand, string> = { review: PATHS.xReview, close: PATHS.xClose };

type Fetch = (input: string, init: RequestInit) => Promise<Response>;

interface RequestOptions {
  body?: unknown;
  ifMatch?: number;
  idempotent?: boolean;
}

export function createHttpExamConductApi(fetchImpl: Fetch = (i, init) => fetch(i, init), baseUrl: string = import.meta.env.VITE_API_BASE ?? ""): ExamConductApi {
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
  /** GET that maps "not created yet" (404) to `null`. */
  async function maybe<T>(path: string): Promise<T | null> {
    try {
      return await request<T>("GET", path);
    } catch (e) {
      if (e instanceof ApiProblem && e.problem.status === 404) return null;
      throw e;
    }
  }
  const body = (reason?: string) => (reason ? { reason } : {});
  return {
    conduct: (sitting) => maybe(fill(PATHS.sittingConduct, { id: sitting })),
    openConduct: (sitting) => request("POST", fill(PATHS.sittingConduct, { id: sitting }), { body: {}, idempotent: true }),
    conductCommand: (id, v, cmd, reason) => request("POST", fill(CONDUCT_CMD[cmd], { id }), { body: body(reason), ifMatch: v }),
    completeConduct: (id, v, attendance, defaultOutcome) => request("POST", fill(PATHS.cComplete, { id }), { body: defaultOutcome ? { attendance, defaultOutcome } : { attendance }, ifMatch: v }),
    postpone: (id, v, input) => request("POST", fill(PATHS.cPostpone, { id }), { body: input, ifMatch: v }),
    conductDecision: (id, v, decision, reason) => request("POST", fill(PATHS.cDecision, { id }), { body: { decision, ...body(reason) }, ifMatch: v }),
    invigilation: (sitting) => request("GET", fill(PATHS.invigilation, { id: sitting })),
    assignDuty: (sitting, input) => request("POST", fill(PATHS.invigilation, { id: sitting }), { body: input, idempotent: true }),
    dutyCommand: (id, v, cmd, reason) => request("POST", fill(DUTY_CMD[cmd], { id }), { body: body(reason), ifMatch: v }),
    replaceDuty: (id, v, input) => request("POST", fill(PATHS.dReplace, { id }), { body: input, ifMatch: v }),
    seating: (sitting) => maybe(fill(PATHS.seating, { id: sitting })),
    generateSeating: (sitting) => request("POST", fill(PATHS.seating, { id: sitting }), { body: {}, idempotent: true }),
    publishSeating: (id, v) => request("POST", fill(PATHS.spPublish, { id }), { body: {}, ifMatch: v }),
    reviseSeating: (id, v, reason) => request("POST", fill(PATHS.spRevise, { id }), { body: { reason }, ifMatch: v }),
    exceptions: async (sid) => (await request<Items<ExamException>>("GET", fill(PATHS.exceptions, { id: sid }))).items,
    raiseException: (sid, input) => request("POST", fill(PATHS.exceptions, { id: sid }), { body: input, idempotent: true }),
    classifyException: (id, v, input) => request("POST", fill(PATHS.xClassify, { id }), { body: input, ifMatch: v }),
    resolveException: (id, v, resolution) => request("POST", fill(PATHS.xResolve, { id }), { body: { resolution }, ifMatch: v }),
    exceptionCommand: (id, v, cmd, reason) => request("POST", fill(EXCEPTION_CMD[cmd], { id }), { body: body(reason), ifMatch: v }),
  };
}
