/**
 * Slice-3B API layer (registration + progression). Same conventions as
 * `features/institution/api.ts`: one interface, an HTTP implementation (CSRF, Idempotency-Key on
 * creates, If-Match on commands, problem+json → `ApiProblem`) and an in-memory mock (`./mock`).
 * Every path template is checked against the generated OpenAPI `paths`.
 */
import { ApiProblem, isProblem, isUnsafeMethod, newIdempotencyKey, type paths } from "@sutra/api-client";
import { getCsrfToken } from "../../../lib/api";
import { fill } from "../../institution/api";
import type {
  AbcSync,
  AcademicBreak,
  ConditionCommand,
  CreateBreak,
  CreateDecision,
  CreateProgressionPolicy,
  CreateRegistration,
  CreateWindow,
  CreditLedger,
  CreditTransfer,
  DecisionCommand,
  HoldList,
  Items,
  LineInput,
  PlaceHold,
  PolicyCommand,
  ProbationCase,
  ProbationCommand,
  ProgrammeMigration,
  ProgressionCondition,
  ProgressionDecision,
  ProgressionPolicy,
  ProgressionSummary,
  QualificationEligibility,
  Registration,
  RegistrationCommand,
  RegistrationHold,
  RegistrationOptions,
  RegistrationWindow,
  RejoinBody,
  WindowCommand,
  WindowFinalisation,
} from "./types";

export interface RegistrationApi {
  // registration windows (institution)
  listWindows(institutionId: string): Promise<RegistrationWindow[]>;
  createWindow(institutionId: string, input: CreateWindow): Promise<RegistrationWindow>;
  windowCommand(id: string, version: number, cmd: WindowCommand, reason?: string): Promise<RegistrationWindow>;
  finaliseWindow(id: string, version: number): Promise<WindowFinalisation>;
  // registrations (staff)
  options(studentId: string): Promise<RegistrationOptions>;
  listRegistrations(studentId: string): Promise<Registration[]>;
  createRegistration(studentId: string, input: CreateRegistration): Promise<Registration>;
  addLine(id: string, version: number, line: LineInput): Promise<Registration>;
  dropLine(id: string, version: number, lineId: string): Promise<Registration>;
  registrationCommand(id: string, version: number, cmd: RegistrationCommand, reason?: string): Promise<Registration>;
  // holds
  listHolds(studentId: string): Promise<HoldList>;
  placeHold(studentId: string, input: PlaceHold): Promise<RegistrationHold>;
  releaseHold(id: string, version: number, reason: string): Promise<RegistrationHold>;
  // self-service
  myOptions(): Promise<RegistrationOptions>;
  myRegistrations(): Promise<Registration[]>;
  createMyRegistration(input: CreateRegistration): Promise<Registration>;
  addMyLine(id: string, version: number, line: LineInput): Promise<Registration>;
  dropMyLine(id: string, version: number, lineId: string): Promise<Registration>;
  submitMine(id: string, version: number): Promise<Registration>;
  cancelMine(id: string, version: number, reason: string): Promise<Registration>;
  // progression policies (institution)
  listPolicies(institutionId: string): Promise<ProgressionPolicy[]>;
  createPolicy(institutionId: string, input: CreateProgressionPolicy): Promise<ProgressionPolicy>;
  policyCommand(id: string, version: number, cmd: PolicyCommand, reason?: string): Promise<ProgressionPolicy>;
  revisePolicy(id: string): Promise<ProgressionPolicy>;
  // progression (student)
  summary(studentId: string): Promise<ProgressionSummary>;
  listDecisions(studentId: string): Promise<ProgressionDecision[]>;
  createDecision(studentId: string, input: CreateDecision): Promise<ProgressionDecision>;
  decisionCommand(id: string, version: number, cmd: DecisionCommand): Promise<ProgressionDecision>;
  conditionCommand(id: string, version: number, cmd: ConditionCommand, reason?: string): Promise<ProgressionCondition>;
  probationCommand(id: string, version: number, cmd: ProbationCommand, reason: string): Promise<ProbationCase>;
  ledger(studentId: string): Promise<CreditLedger>;
  listTransfers(studentId: string): Promise<CreditTransfer[]>;
  listMigrations(studentId: string): Promise<ProgrammeMigration[]>;
  listBreaks(studentId: string): Promise<AcademicBreak[]>;
  requestBreak(studentId: string, input: CreateBreak): Promise<AcademicBreak>;
  rejoin(id: string, version: number, input: RejoinBody): Promise<AcademicBreak>;
  evaluateEligibility(studentId: string): Promise<QualificationEligibility>;
  listAbcSyncs(studentId: string): Promise<AbcSync[]>;
  syncAbc(studentId: string): Promise<AbcSync>;
}

const B = "/api/v1/student";
export const PATHS = {
  windows: `${B}/institutions/{id}/registration-windows`,
  windowOpen: `${B}/registration-windows/{id}/open`,
  windowClose: `${B}/registration-windows/{id}/close`,
  windowCancel: `${B}/registration-windows/{id}/cancel`,
  windowFinalise: `${B}/registration-windows/{id}/finalise`,
  options: `${B}/students/{id}/registrations/options`,
  registrations: `${B}/students/{id}/registrations`,
  lines: `${B}/registrations/{id}/lines`,
  line: `${B}/registrations/{id}/lines/{lineId}`,
  submit: `${B}/registrations/{id}/submit`,
  finalise: `${B}/registrations/{id}/finalise`,
  cancel: `${B}/registrations/{id}/cancel`,
  holds: `${B}/students/{id}/holds`,
  holdRelease: `${B}/registration-holds/{id}/release`,
  myOptions: `${B}/me/registration-windows`,
  myRegistrations: `${B}/me/registrations`,
  myLines: `${B}/me/registrations/{id}/lines`,
  myLine: `${B}/me/registrations/{id}/lines/{lineId}`,
  mySubmit: `${B}/me/registrations/{id}/submit`,
  myCancel: `${B}/me/registrations/{id}/cancel`,
  policies: `${B}/institutions/{id}/progression-policies`,
  policySubmit: `${B}/progression-policies/{id}/submit`,
  policyRetire: `${B}/progression-policies/{id}/retire`,
  policyRevise: `${B}/progression-policies/{id}/revise`,
  summary: `${B}/students/{id}/progression`,
  decisions: `${B}/students/{id}/progression-decisions`,
  decisionSubmit: `${B}/progression-decisions/{id}/submit`,
  decisionDiscard: `${B}/progression-decisions/{id}/discard`,
  conditionFulfil: `${B}/progression-conditions/{id}/fulfil`,
  conditionFail: `${B}/progression-conditions/{id}/fail`,
  conditionWaive: `${B}/progression-conditions/{id}/waive`,
  probationClear: `${B}/probation-cases/{id}/clear`,
  probationEscalate: `${B}/probation-cases/{id}/escalate`,
  ledger: `${B}/students/{id}/credit-ledger`,
  transfers: `${B}/students/{id}/credit-transfers`,
  migrations: `${B}/students/{id}/programme-migrations`,
  breaks: `${B}/students/{id}/academic-breaks`,
  breakRejoin: `${B}/academic-breaks/{id}/rejoin`,
  eligibility: `${B}/students/{id}/qualification-eligibility`,
  abcSyncs: `${B}/students/{id}/abc-syncs`,
} as const satisfies Record<string, keyof paths>;

type Fetch = (input: string, init: RequestInit) => Promise<Response>;

interface RequestOptions {
  body?: unknown;
  ifMatch?: number;
  idempotent?: boolean;
}

const WINDOW_CMD = { open: PATHS.windowOpen, close: PATHS.windowClose, cancel: PATHS.windowCancel } as const;
const REG_CMD = { submit: PATHS.submit, finalise: PATHS.finalise, cancel: PATHS.cancel } as const;
const POLICY_CMD = { submit: PATHS.policySubmit, retire: PATHS.policyRetire } as const;
const DECISION_CMD = { submit: PATHS.decisionSubmit, discard: PATHS.decisionDiscard } as const;
const CONDITION_CMD = { fulfil: PATHS.conditionFulfil, fail: PATHS.conditionFail, waive: PATHS.conditionWaive } as const;
const PROBATION_CMD = { clear: PATHS.probationClear, escalate: PATHS.probationEscalate } as const;

export function createHttpRegistrationApi(fetchImpl: Fetch = (i, init) => fetch(i, init), baseUrl: string = import.meta.env.VITE_API_BASE ?? ""): RegistrationApi {
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
  return {
    listWindows: (i) => items(fill(PATHS.windows, id(i))),
    createWindow: (i, input) => request("POST", fill(PATHS.windows, id(i)), { body: input, idempotent: true }),
    windowCommand: (i, v, cmd, reason) => request("POST", fill(WINDOW_CMD[cmd], id(i)), { body: reasonBody(reason), ifMatch: v }),
    finaliseWindow: (i, v) => request("POST", fill(PATHS.windowFinalise, id(i)), { ifMatch: v }),
    options: (s) => request("GET", fill(PATHS.options, id(s))),
    listRegistrations: (s) => items(fill(PATHS.registrations, id(s))),
    createRegistration: (s, input) => request("POST", fill(PATHS.registrations, id(s)), { body: input, idempotent: true }),
    addLine: (i, v, line) => request("POST", fill(PATHS.lines, id(i)), { body: line, ifMatch: v }),
    dropLine: (i, v, lineId) => request("DELETE", fill(PATHS.line, { id: i, lineId }), { ifMatch: v }),
    registrationCommand: (i, v, cmd, reason) => request("POST", fill(REG_CMD[cmd], id(i)), { body: reasonBody(reason), ifMatch: v }),
    listHolds: (s) => request("GET", fill(PATHS.holds, id(s))),
    placeHold: (s, input) => request("POST", fill(PATHS.holds, id(s)), { body: input, idempotent: true }),
    releaseHold: (i, v, reason) => request("POST", fill(PATHS.holdRelease, id(i)), { body: { reason }, ifMatch: v }),
    myOptions: () => request("GET", PATHS.myOptions),
    myRegistrations: () => items(PATHS.myRegistrations),
    createMyRegistration: (input) => request("POST", PATHS.myRegistrations, { body: input, idempotent: true }),
    addMyLine: (i, v, line) => request("POST", fill(PATHS.myLines, id(i)), { body: line, ifMatch: v }),
    dropMyLine: (i, v, lineId) => request("DELETE", fill(PATHS.myLine, { id: i, lineId }), { ifMatch: v }),
    submitMine: (i, v) => request("POST", fill(PATHS.mySubmit, id(i)), { ifMatch: v }),
    cancelMine: (i, v, reason) => request("POST", fill(PATHS.myCancel, id(i)), { body: { reason }, ifMatch: v }),
    listPolicies: (i) => items(fill(PATHS.policies, id(i))),
    createPolicy: (i, input) => request("POST", fill(PATHS.policies, id(i)), { body: input, idempotent: true }),
    policyCommand: (i, v, cmd, reason) => request("POST", fill(POLICY_CMD[cmd], id(i)), { body: reasonBody(reason), ifMatch: v }),
    revisePolicy: (i) => request("POST", fill(PATHS.policyRevise, id(i)), { idempotent: true }),
    summary: (s) => request("GET", fill(PATHS.summary, id(s))),
    listDecisions: (s) => items(fill(PATHS.decisions, id(s))),
    createDecision: (s, input) => request("POST", fill(PATHS.decisions, id(s)), { body: input, idempotent: true }),
    decisionCommand: (i, v, cmd) => request("POST", fill(DECISION_CMD[cmd], id(i)), { ifMatch: v }),
    conditionCommand: (i, v, cmd, reason) => request("POST", fill(CONDITION_CMD[cmd], id(i)), { body: reasonBody(reason), ifMatch: v }),
    probationCommand: (i, v, cmd, reason) => request("POST", fill(PROBATION_CMD[cmd], id(i)), { body: { reason }, ifMatch: v }),
    ledger: (s) => request("GET", fill(PATHS.ledger, id(s))),
    listTransfers: (s) => items(fill(PATHS.transfers, id(s))),
    listMigrations: (s) => items(fill(PATHS.migrations, id(s))),
    listBreaks: (s) => items(fill(PATHS.breaks, id(s))),
    requestBreak: (s, input) => request("POST", fill(PATHS.breaks, id(s)), { body: input, idempotent: true }),
    rejoin: (i, v, input) => request("POST", fill(PATHS.breakRejoin, id(i)), { body: input, ifMatch: v }),
    evaluateEligibility: (s) => request("POST", fill(PATHS.eligibility, id(s)), { idempotent: true }),
    listAbcSyncs: (s) => items(fill(PATHS.abcSyncs, id(s))),
    syncAbc: (s) => request("POST", fill(PATHS.abcSyncs, id(s)), { idempotent: true }),
  };
}
