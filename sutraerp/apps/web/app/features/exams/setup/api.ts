/**
 * Slice-6A API layer (examination policy, sessions + papers, timetables, venues, seating,
 * invigilation, conduct, registrations, hall tickets, student self-service). Same conventions as
 * the Batch 5 layers: one interface, an HTTP implementation (CSRF, `Idempotency-Key` on creates
 * and bulk commands, `If-Match` on state commands, problem+json → `ApiProblem`) and an
 * in-memory mock (`./mock`). Every path template is checked against the generated OpenAPI.
 */
import { ApiProblem, isProblem, isUnsafeMethod, newIdempotencyKey, type paths } from "@sutra/api-client";
import { getCsrfToken } from "../../../lib/api";
import { fill } from "../../institution/api";
import type {
  BulkSummary,
  ExamPolicy,
  ExamRegistration,
  ExamSchedule,
  ExamSession,
  ExamSitting,
  ExamVenue,
  HallTicket,
  Items,
  MyHallTicket,
  SelfOption,
  TimetableEntry,
} from "./types";

export type PolicyCommand = "submit" | "activate" | "retire";
export type SessionCommand = "submit" | "schedule" | "activate" | "suspend" | "reopen";
export type ScheduleCommand = "submit" | "publish" | "revise" | "withdraw";
export type RegistrationCommand = "confirm" | "withdraw" | "reinstate";
export type TicketCommand = "withhold" | "release" | "withdraw" | "reissue";

export interface ExamSetupApi {
  policies(institutionId: string): Promise<ExamPolicy[]>;
  createPolicy(institutionId: string, input: Record<string, unknown>): Promise<ExamPolicy>;
  policyCommand(id: string, version: number, cmd: PolicyCommand, reason?: string): Promise<ExamPolicy>;
  sessions(institutionId: string): Promise<ExamSession[]>;
  createSession(institutionId: string, input: Record<string, unknown>): Promise<ExamSession>;
  sessionCommand(id: string, version: number, cmd: SessionCommand, reason?: string): Promise<ExamSession>;
  addPaper(sessionId: string, offeringId: string): Promise<unknown>;
  /** Grading schemes of the institution (foundation API) for the session picker. */
  gradingSchemes(institutionId: string): Promise<{ id: string; code: string; name: string; status: string }[]>;
  generatePapers(sessionId: string): Promise<BulkSummary>;
  schedules(sessionId: string): Promise<ExamSchedule[]>;
  createSchedule(sessionId: string, name: string): Promise<ExamSchedule>;
  scheduleCommand(id: string, version: number, cmd: ScheduleCommand, reason?: string): Promise<ExamSchedule>;
  addSitting(scheduleId: string, input: { paperId: string; sitsOn: string; startsAt: string }): Promise<ExamSitting>;
  venues(institutionId: string): Promise<ExamVenue[]>;
  createVenue(institutionId: string, input: { code: string; name: string; capacity: number }): Promise<ExamVenue>;
  allocate(sittingId: string, venueId: string): Promise<unknown>;
  generateSeating(sittingId: string): Promise<{ id: string; version: number; candidates: number }>;
  registrations(sessionId: string): Promise<ExamRegistration[]>;
  bulkRegister(sessionId: string): Promise<BulkSummary>;
  register(sessionId: string, studentId: string): Promise<ExamRegistration>;
  registrationCommand(id: string, version: number, cmd: RegistrationCommand, reason?: string): Promise<ExamRegistration>;
  hallTickets(sessionId: string): Promise<HallTicket[]>;
  issueHallTickets(sessionId: string): Promise<BulkSummary>;
  ticketCommand(id: string, version: number, cmd: TicketCommand, reason?: string): Promise<HallTicket>;
  finalisePopulation(sessionId: string): Promise<BulkSummary>;
  // student
  myOptions(): Promise<SelfOption[]>;
  myRegistrations(): Promise<ExamRegistration[]>;
  registerMe(sessionId: string): Promise<ExamRegistration>;
  myHallTickets(): Promise<MyHallTicket[]>;
  myTimetable(): Promise<TimetableEntry[]>;
}

const B = "/api/v1/exams";
export const PATHS = {
  policies: `${B}/institutions/{id}/exam-policies`,
  pSubmit: `${B}/exam-policies/{id}/submit`,
  pActivate: `${B}/exam-policies/{id}/activate`,
  pRetire: `${B}/exam-policies/{id}/retire`,
  sessions: `${B}/institutions/{id}/exam-sessions`,
  sSubmit: `${B}/exam-sessions/{id}/submit`,
  sSchedule: `${B}/exam-sessions/{id}/schedule`,
  sActivate: `${B}/exam-sessions/{id}/activate`,
  sSuspend: `${B}/exam-sessions/{id}/suspend`,
  sReopen: `${B}/exam-sessions/{id}/reopen`,
  papers: `${B}/exam-sessions/{id}/papers`,
  schemes: `${B}/institutions/{id}/grading-schemes`,
  generate: `${B}/exam-sessions/{id}/papers/generate`,
  schedules: `${B}/exam-sessions/{id}/schedules`,
  scSubmit: `${B}/exam-schedules/{id}/submit`,
  scPublish: `${B}/exam-schedules/{id}/publish`,
  scRevise: `${B}/exam-schedules/{id}/revise`,
  scWithdraw: `${B}/exam-schedules/{id}/withdraw`,
  sittings: `${B}/exam-schedules/{id}/sittings`,
  venues: `${B}/institutions/{id}/exam-venues`,
  allocate: `${B}/exam-sittings/{id}/venues`,
  seating: `${B}/exam-sittings/{id}/seating`,
  registrations: `${B}/exam-sessions/{id}/registrations`,
  bulk: `${B}/exam-sessions/{id}/registrations/bulk`,
  rConfirm: `${B}/exam-registrations/{id}/confirm`,
  rWithdraw: `${B}/exam-registrations/{id}/withdraw`,
  rReinstate: `${B}/exam-registrations/{id}/reinstate`,
  finalise: `${B}/exam-sessions/{id}/registration-population/finalise`,
  tickets: `${B}/exam-sessions/{id}/hall-tickets`,
  issue: `${B}/exam-sessions/{id}/hall-tickets/issue`,
  tWithhold: `${B}/hall-tickets/{id}/withhold`,
  tRelease: `${B}/hall-tickets/{id}/release`,
  tWithdraw: `${B}/hall-tickets/{id}/withdraw`,
  tReissue: `${B}/hall-tickets/{id}/reissue`,
  meOptions: `${B}/me/exam-registrations/options`,
  meRegistrations: `${B}/me/exam-registrations`,
  meTickets: `${B}/me/hall-tickets`,
  meTimetable: `${B}/me/exam-timetable`,
} as const satisfies Record<string, keyof paths>;

const POLICY_CMD: Record<PolicyCommand, string> = { submit: PATHS.pSubmit, activate: PATHS.pActivate, retire: PATHS.pRetire };
const SESSION_CMD: Record<SessionCommand, string> = { submit: PATHS.sSubmit, schedule: PATHS.sSchedule, activate: PATHS.sActivate, suspend: PATHS.sSuspend, reopen: PATHS.sReopen };
const SCHEDULE_CMD: Record<ScheduleCommand, string> = { submit: PATHS.scSubmit, publish: PATHS.scPublish, revise: PATHS.scRevise, withdraw: PATHS.scWithdraw };
const REG_CMD: Record<RegistrationCommand, string> = { confirm: PATHS.rConfirm, withdraw: PATHS.rWithdraw, reinstate: PATHS.rReinstate };
const TICKET_CMD: Record<TicketCommand, string> = { withhold: PATHS.tWithhold, release: PATHS.tRelease, withdraw: PATHS.tWithdraw, reissue: PATHS.tReissue };

type Fetch = (input: string, init: RequestInit) => Promise<Response>;

interface RequestOptions {
  body?: unknown;
  ifMatch?: number;
  idempotent?: boolean;
}

export function createHttpExamSetupApi(fetchImpl: Fetch = (i, init) => fetch(i, init), baseUrl: string = import.meta.env.VITE_API_BASE ?? ""): ExamSetupApi {
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
  const body = (reason?: string) => (reason ? { reason } : {});
  return {
    policies: (inst) => items(fill(PATHS.policies, { id: inst })),
    createPolicy: (inst, input) => request("POST", fill(PATHS.policies, { id: inst }), { body: input, idempotent: true }),
    policyCommand: (id, v, cmd, reason) => request("POST", fill(POLICY_CMD[cmd], { id }), { body: body(reason), ifMatch: v }),
    sessions: (inst) => items(fill(PATHS.sessions, { id: inst })),
    gradingSchemes: (inst) => items(fill(PATHS.schemes, { id: inst })),
    createSession: (inst, input) => request("POST", fill(PATHS.sessions, { id: inst }), { body: input, idempotent: true }),
    sessionCommand: (id, v, cmd, reason) => request("POST", fill(SESSION_CMD[cmd], { id }), { body: body(reason), ifMatch: v }),
    addPaper: (sid, offeringId) => request("POST", fill(PATHS.papers, { id: sid }), { body: { offeringId }, idempotent: true }),
    generatePapers: (sid) => request("POST", fill(PATHS.generate, { id: sid }), { body: {}, idempotent: true }),
    schedules: (sid) => items(fill(PATHS.schedules, { id: sid })),
    createSchedule: (sid, name) => request("POST", fill(PATHS.schedules, { id: sid }), { body: { name }, idempotent: true }),
    scheduleCommand: (id, v, cmd, reason) => request("POST", fill(SCHEDULE_CMD[cmd], { id }), { body: body(reason), ifMatch: v }),
    addSitting: (sch, input) => request("POST", fill(PATHS.sittings, { id: sch }), { body: input, idempotent: true }),
    venues: (inst) => items(fill(PATHS.venues, { id: inst })),
    createVenue: (inst, input) => request("POST", fill(PATHS.venues, { id: inst }), { body: input, idempotent: true }),
    allocate: (sitting, venueId) => request("POST", fill(PATHS.allocate, { id: sitting }), { body: { venueId }, idempotent: true }),
    generateSeating: (sitting) => request("POST", fill(PATHS.seating, { id: sitting }), { body: {}, idempotent: true }),
    registrations: (sid) => items(fill(PATHS.registrations, { id: sid }) + "?limit=200"),
    bulkRegister: (sid) => request("POST", fill(PATHS.bulk, { id: sid }), { body: {}, idempotent: true }),
    register: (sid, studentId) => request("POST", fill(PATHS.registrations, { id: sid }), { body: { studentId }, idempotent: true }),
    registrationCommand: (id, v, cmd, reason) => request("POST", fill(REG_CMD[cmd], { id }), { body: body(reason), ifMatch: v }),
    hallTickets: (sid) => items(fill(PATHS.tickets, { id: sid })),
    issueHallTickets: (sid) => request("POST", fill(PATHS.issue, { id: sid }), { body: {}, idempotent: true }),
    ticketCommand: (id, v, cmd, reason) => request("POST", fill(TICKET_CMD[cmd], { id }), { body: body(reason), ifMatch: v }),
    finalisePopulation: (sid) => request("POST", fill(PATHS.finalise, { id: sid }), { body: {}, idempotent: true }),
    myOptions: () => items(PATHS.meOptions),
    myRegistrations: () => items(PATHS.meRegistrations),
    registerMe: (sessionId) => request("POST", PATHS.meRegistrations, { body: { sessionId }, idempotent: true }),
    myHallTickets: () => items(PATHS.meTickets),
    myTimetable: () => items(PATHS.meTimetable),
  };
}
