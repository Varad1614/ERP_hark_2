/**
 * Thin, typed API layer of the Batch 2 admin screens (Admin → Campus setup, Admin → Academics).
 *
 * One interface, two implementations (same pattern as `lib/inbox.ts`):
 *
 * - `createHttpInstitutionApi()` — sutra-api. Unsafe calls carry `X-CSRF-Token` (session token),
 *   creates an `Idempotency-Key`, updates and state commands an `If-Match` with the resource's
 *   `version`; error bodies become `ApiProblem` (problem+json).
 * - `createMockInstitutionApi()` (`./mock`) — in-memory stand-in for `mock` mode and tests.
 *
 * Paths: every template is checked against the generated OpenAPI `paths` at compile time
 * (`satisfies Record<string, keyof paths>`), and all request/response types are generated.
 */
import { ApiProblem, isProblem, isUnsafeMethod, newIdempotencyKey, type paths } from "@sutra/api-client";
import { getCsrfToken } from "../../lib/api";
import type {
  AcademicStructure,
  AcademicYear,
  Calendar,
  CalendarAssignment,
  CalendarException,
  CalendarSimulation,
  CalendarVersion,
  Campus,
  CloseAcademicYear,
  ClosureEvidence,
  Curriculum,
  CurriculumVersion,
  DecommissionCampus,
  Institution,
  Items,
  NewAcademicYear,
  NewAssignment,
  NewBlueprint,
  NewCalendar,
  NewCalendarVersion,
  NewCampus,
  NewCurriculum,
  NewException,
  NewInstitution,
  NewKeyDate,
  NewOrgUnit,
  NewPolicy,
  NewPosition,
  NewProgramme,
  NewSchoolModel,
  NewStructure,
  NewTerm,
  OrgTypeBlueprint,
  OrgUnit,
  Policy,
  Position,
  Programme,
  ReadinessReport,
  ReviseCurriculum,
  ReviseProfile,
  SchoolModel,
  SimulationProposal,
} from "./types";

export type InstitutionCommand = "configure" | "submit" | "suspend" | "reactivate" | "archive";
export type ActivationDecision = "approve" | "reject" | "return";
export type OrgUnitCommand = "activate" | "suspend" | "restore" | "decommission" | "archive";
export type PositionCommand = "activate" | "retire" | "archive";
export type CampusCommand = "activate" | "suspend" | "restore" | "decommission" | "archive";
export type PublishCommand = "publish" | "retire";
export type CurriculumVersionCommand = "publish" | "retire";
export type VersionCommand = "publish" | "revise" | "retire";
export type ExceptionCommand = "apply" | "complete" | "withdraw";

/** Commands that need a fresh second factor when step-up is enforced (contract §4.4). */
export const STEP_UP_COMMANDS: ReadonlySet<string> = new Set([
  "institution:suspend",
  "institution:reactivate",
  "institution:archive",
  "institution:decision",
  "campus:decommission",
  "campus:archive",
]);

export interface InstitutionApi {
  // 2A — institution lifecycle + profile
  listInstitutions(): Promise<Institution[]>;
  getInstitution(id: string): Promise<Institution>;
  registerInstitution(input: NewInstitution): Promise<Institution>;
  renameInstitution(id: string, version: number, name: string): Promise<Institution>;
  reviseProfile(id: string, version: number, input: ReviseProfile): Promise<Institution>;
  institutionCommand(id: string, version: number, cmd: InstitutionCommand, reason?: string): Promise<Institution>;
  decideActivation(id: string, version: number, decision: ActivationDecision, reason?: string): Promise<Institution>;
  // 2A — organisation (type blueprints + unit tree), positions, policies
  listBlueprints(institutionId: string): Promise<OrgTypeBlueprint[]>;
  createBlueprint(institutionId: string, input: NewBlueprint): Promise<OrgTypeBlueprint>;
  publishBlueprint(id: string, version: number): Promise<OrgTypeBlueprint>;
  listOrgUnits(institutionId: string): Promise<OrgUnit[]>;
  createOrgUnit(institutionId: string, input: NewOrgUnit): Promise<OrgUnit>;
  orgUnitCommand(id: string, version: number, cmd: OrgUnitCommand, reason?: string): Promise<OrgUnit>;
  listPositions(institutionId: string): Promise<Position[]>;
  createPosition(institutionId: string, input: NewPosition): Promise<Position>;
  positionCommand(id: string, version: number, cmd: PositionCommand, reason?: string): Promise<Position>;
  listPolicies(institutionId: string): Promise<Policy[]>;
  createPolicy(institutionId: string, input: NewPolicy): Promise<Policy>;
  publishPolicy(id: string, version: number): Promise<Policy>;
  // 2B — campuses, structure, programmes, curricula, school model
  listCampuses(institutionId: string): Promise<Campus[]>;
  registerCampus(institutionId: string, input: NewCampus): Promise<Campus>;
  /** `decommission` needs `{ reason, effectiveOn }`; `suspend` needs a reason. */
  campusCommand(id: string, version: number, cmd: CampusCommand, body?: Partial<DecommissionCampus>): Promise<Campus>;
  listStructures(institutionId: string): Promise<AcademicStructure[]>;
  createStructure(institutionId: string, input: NewStructure): Promise<AcademicStructure>;
  structureCommand(id: string, version: number, cmd: PublishCommand, reason?: string): Promise<AcademicStructure>;
  listProgrammes(institutionId: string): Promise<Programme[]>;
  createProgramme(institutionId: string, input: NewProgramme): Promise<Programme>;
  programmeCommand(id: string, version: number, cmd: PublishCommand, reason?: string): Promise<Programme>;
  listCurricula(programmeId: string): Promise<Curriculum[]>;
  /** Creates the curriculum with its first draft version (`initialVersion`). */
  createCurriculum(programmeId: string, input: NewCurriculum): Promise<Curriculum>;
  listCurriculumVersions(curriculumId: string): Promise<CurriculumVersion[]>;
  /** `POST /curricula/{id}/revisions`: successor draft of the published version. */
  reviseCurriculum(curriculumId: string, input: ReviseCurriculum): Promise<CurriculumVersion>;
  curriculumVersionCommand(id: string, version: number, cmd: CurriculumVersionCommand, reason?: string): Promise<CurriculumVersion>;
  listSchoolModels(institutionId: string): Promise<SchoolModel[]>;
  createSchoolModel(institutionId: string, input: NewSchoolModel): Promise<SchoolModel>;
  schoolModelCommand(id: string, version: number, cmd: "publish"): Promise<SchoolModel>;
  // 2C — readiness + calendar
  readiness(institutionId: string): Promise<ReadinessReport>;
  listCalendars(institutionId: string): Promise<Calendar[]>;
  createCalendar(institutionId: string, input: NewCalendar): Promise<Calendar>;
  retireCalendar(id: string, version: number, reason?: string): Promise<Calendar>;
  listYears(institutionId: string): Promise<AcademicYear[]>;
  createYear(institutionId: string, input: NewAcademicYear): Promise<AcademicYear>;
  openYear(id: string, version: number): Promise<AcademicYear>;
  closeYear(id: string, version: number, input: Partial<CloseAcademicYear>): Promise<{ year: AcademicYear; evidence: ClosureEvidence[] }>;
  listVersions(calendarId: string): Promise<CalendarVersion[]>;
  createVersion(calendarId: string, input: NewCalendarVersion): Promise<CalendarVersion>;
  addTerm(versionId: string, version: number, input: NewTerm): Promise<CalendarVersion>;
  removeTerm(versionId: string, version: number, termId: string): Promise<CalendarVersion>;
  addKeyDate(versionId: string, version: number, input: NewKeyDate): Promise<CalendarVersion>;
  removeKeyDate(versionId: string, version: number, keyDateId: string): Promise<CalendarVersion>;
  versionCommand(versionId: string, version: number, cmd: VersionCommand, reason?: string): Promise<CalendarVersion>;
  listAssignments(calendarId: string): Promise<CalendarAssignment[]>;
  createAssignment(calendarId: string, input: NewAssignment): Promise<CalendarAssignment>;
  assignmentCommand(id: string, version: number, cmd: "activate" | "retire"): Promise<CalendarAssignment>;
  listExceptions(calendarId: string): Promise<CalendarException[]>;
  createException(calendarId: string, input: NewException): Promise<CalendarException>;
  exceptionCommand(id: string, version: number, cmd: ExceptionCommand): Promise<CalendarException>;
  simulate(calendarId: string, baselineVersionId: string, proposal: SimulationProposal): Promise<CalendarSimulation>;
  decideSimulation(id: string, version: number, decision: "accept" | "discard", reason?: string): Promise<CalendarSimulation>;
}

// ---------------------------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------------------------

/** Path templates — a typo or a renamed endpoint fails `tsc` against the generated `paths`. */
const B = "/api/v1/institution";
export const PATHS = {
  // 2A
  institutions: `${B}/institutions`,
  institution: `${B}/institutions/{id}`,
  profile: `${B}/institutions/{id}/profile`,
  configure: `${B}/institutions/{id}/configure`,
  submit: `${B}/institutions/{id}/submit`,
  suspend: `${B}/institutions/{id}/suspend`,
  reactivate: `${B}/institutions/{id}/reactivate`,
  archive: `${B}/institutions/{id}/archive`,
  decision: `${B}/institutions/{id}/decision`,
  blueprints: `${B}/institutions/{id}/org-type-blueprints`,
  blueprintPublish: `${B}/org-type-blueprints/{id}/publish`,
  orgUnits: `${B}/institutions/{id}/org-units`,
  orgUnitActivate: `${B}/org-units/{id}/activate`,
  orgUnitSuspend: `${B}/org-units/{id}/suspend`,
  orgUnitRestore: `${B}/org-units/{id}/restore`,
  orgUnitDecommission: `${B}/org-units/{id}/decommission`,
  orgUnitArchive: `${B}/org-units/{id}/archive`,
  positions: `${B}/institutions/{id}/positions`,
  positionActivate: `${B}/positions/{id}/activate`,
  positionRetire: `${B}/positions/{id}/retire`,
  positionArchive: `${B}/positions/{id}/archive`,
  policies: `${B}/institutions/{id}/policies`,
  policyPublish: `${B}/policies/{id}/publish`,
  // 2B
  campuses: `${B}/institutions/{id}/campuses`,
  campusActivate: `${B}/campuses/{campusId}/activate`,
  campusSuspend: `${B}/campuses/{campusId}/suspend`,
  campusRestore: `${B}/campuses/{campusId}/restore`,
  campusDecommission: `${B}/campuses/{campusId}/decommission`,
  campusArchive: `${B}/campuses/{campusId}/archive`,
  structures: `${B}/institutions/{id}/academic-structures`,
  structurePublish: `${B}/academic-structures/{structureId}/publish`,
  structureRetire: `${B}/academic-structures/{structureId}/retire`,
  programmes: `${B}/institutions/{id}/programmes`,
  programmePublish: `${B}/programmes/{programmeId}/publish`,
  programmeRetire: `${B}/programmes/{programmeId}/retire`,
  curricula: `${B}/programmes/{programmeId}/curricula`,
  curriculumVersions: `${B}/curricula/{curriculumId}/versions`,
  curriculumRevisions: `${B}/curricula/{curriculumId}/revisions`,
  curriculumVersionPublish: `${B}/curriculum-versions/{versionId}/publish`,
  curriculumVersionRetire: `${B}/curriculum-versions/{versionId}/retire`,
  schoolModels: `${B}/institutions/{id}/school-models`,
  schoolModelPublish: `${B}/school-models/{schoolModelId}/publish`,
  // 2C
  readiness: `${B}/institutions/{id}/activation-readiness`,
  calendars: `${B}/institutions/{id}/calendars`,
  calendarRetire: `${B}/calendars/{calendarId}/retire`,
  years: `${B}/institutions/{id}/academic-years`,
  yearOpen: `${B}/academic-years/{yearId}/open`,
  yearClose: `${B}/academic-years/{yearId}/close`,
  versions: `${B}/calendars/{calendarId}/versions`,
  terms: `${B}/calendar-versions/{versionId}/terms`,
  term: `${B}/calendar-versions/{versionId}/terms/{termId}`,
  keyDates: `${B}/calendar-versions/{versionId}/key-dates`,
  keyDate: `${B}/calendar-versions/{versionId}/key-dates/{keyDateId}`,
  versionPublish: `${B}/calendar-versions/{versionId}/publish`,
  versionRevise: `${B}/calendar-versions/{versionId}/revise`,
  versionRetire: `${B}/calendar-versions/{versionId}/retire`,
  assignments: `${B}/calendars/{calendarId}/assignments`,
  assignmentActivate: `${B}/calendar-assignments/{assignmentId}/activate`,
  assignmentRetire: `${B}/calendar-assignments/{assignmentId}/retire`,
  exceptions: `${B}/calendars/{calendarId}/exceptions`,
  exceptionApply: `${B}/calendar-exceptions/{exceptionId}/apply`,
  exceptionComplete: `${B}/calendar-exceptions/{exceptionId}/complete`,
  exceptionWithdraw: `${B}/calendar-exceptions/{exceptionId}/withdraw`,
  simulations: `${B}/calendars/{calendarId}/simulations`,
  simulationDecision: `${B}/calendar-simulations/{simulationId}/decision`,
} as const satisfies Record<string, keyof paths>;

type PathKey = keyof typeof PATHS;
const INSTITUTION_COMMAND: Record<InstitutionCommand, PathKey> = { configure: "configure", submit: "submit", suspend: "suspend", reactivate: "reactivate", archive: "archive" };
const ORG_UNIT_COMMAND: Record<OrgUnitCommand, PathKey> = { activate: "orgUnitActivate", suspend: "orgUnitSuspend", restore: "orgUnitRestore", decommission: "orgUnitDecommission", archive: "orgUnitArchive" };
const POSITION_COMMAND: Record<PositionCommand, PathKey> = { activate: "positionActivate", retire: "positionRetire", archive: "positionArchive" };
const CAMPUS_COMMAND: Record<CampusCommand, PathKey> = { activate: "campusActivate", suspend: "campusSuspend", restore: "campusRestore", decommission: "campusDecommission", archive: "campusArchive" };

/** Fills `{name}` placeholders (URL-encoded). */
export function fill(template: string, params: Record<string, string>): string {
  return template.replace(/\{(\w+)\}/g, (_, k: string) => {
    const v = params[k];
    if (v === undefined) throw new Error(`missing path parameter ${k}`);
    return encodeURIComponent(v);
  });
}

// ---------------------------------------------------------------------------------------------
// HTTP implementation
// ---------------------------------------------------------------------------------------------

type Fetch = (input: string, init: RequestInit) => Promise<Response>;

export interface RequestOptions {
  body?: unknown;
  /** Resource version → `If-Match: "<version>"`. */
  ifMatch?: number;
  /** Adds a fresh `Idempotency-Key` (creates). */
  idempotent?: boolean;
}

export function createHttpInstitutionApi(fetchImpl: Fetch = (i, init) => fetch(i, init), baseUrl: string = import.meta.env.VITE_API_BASE ?? ""): InstitutionApi {
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
    const res = await fetchImpl(`${baseUrl}${path}`, {
      method,
      headers,
      credentials: "include",
      body: unsafe && method !== "DELETE" ? JSON.stringify(opts.body ?? {}) : undefined,
    });
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
  const get = <T>(path: string) => request<T>("GET", path);
  const items = async <T>(path: string) => (await get<Items<T>>(path)).items;
  const create = <T>(path: string, body: unknown) => request<T>("POST", path, { body, idempotent: true });
  const command = <T>(path: string, version: number, body: unknown = {}) => request<T>("POST", path, { body, ifMatch: version });
  const P = PATHS;
  const reasonBody = (reason?: string) => (reason ? { reason } : {});

  return {
    listInstitutions: () => items(P.institutions),
    getInstitution: (id) => get(fill(P.institution, { id })),
    registerInstitution: (input) => create(P.institutions, input),
    renameInstitution: (id, version, name) => request("PATCH", fill(P.institution, { id }), { body: { name }, ifMatch: version }),
    reviseProfile: (id, version, input) => request("PUT", fill(P.profile, { id }), { body: input, ifMatch: version }),
    institutionCommand: (id, version, cmd, reason) => command(fill(P[INSTITUTION_COMMAND[cmd]], { id }), version, reasonBody(reason)),
    decideActivation: (id, version, decision, reason) => command(fill(P.decision, { id }), version, { decision, reason }),
    listBlueprints: (id) => items(fill(P.blueprints, { id })),
    createBlueprint: (id, input) => create(fill(P.blueprints, { id }), input),
    publishBlueprint: (id, version) => command(fill(P.blueprintPublish, { id }), version),
    listOrgUnits: (id) => items(fill(P.orgUnits, { id })),
    createOrgUnit: (id, input) => create(fill(P.orgUnits, { id }), input),
    orgUnitCommand: (id, version, cmd, reason) => command(fill(P[ORG_UNIT_COMMAND[cmd]], { id }), version, reasonBody(reason)),
    listPositions: (id) => items(fill(P.positions, { id })),
    createPosition: (id, input) => create(fill(P.positions, { id }), input),
    positionCommand: (id, version, cmd, reason) => command(fill(P[POSITION_COMMAND[cmd]], { id }), version, reasonBody(reason)),
    listPolicies: (id) => items(fill(P.policies, { id })),
    createPolicy: (id, input) => create(fill(P.policies, { id }), input),
    publishPolicy: (id, version) => command(fill(P.policyPublish, { id }), version),
    listCampuses: (id) => items(fill(P.campuses, { id })),
    registerCampus: (id, input) => create(fill(P.campuses, { id }), input),
    campusCommand: (campusId, version, cmd, body) => command(fill(P[CAMPUS_COMMAND[cmd]], { campusId }), version, body ?? {}),
    listStructures: (id) => items(fill(P.structures, { id })),
    createStructure: (id, input) => create(fill(P.structures, { id }), input),
    structureCommand: (structureId, version, cmd, reason) => command(fill(cmd === "publish" ? P.structurePublish : P.structureRetire, { structureId }), version, reasonBody(reason)),
    listProgrammes: (id) => items(fill(P.programmes, { id })),
    createProgramme: (id, input) => create(fill(P.programmes, { id }), input),
    programmeCommand: (programmeId, version, cmd, reason) => command(fill(cmd === "publish" ? P.programmePublish : P.programmeRetire, { programmeId }), version, reasonBody(reason)),
    listCurricula: (programmeId) => items(fill(P.curricula, { programmeId })),
    createCurriculum: (programmeId, input) => create(fill(P.curricula, { programmeId }), input),
    listCurriculumVersions: (curriculumId) => items(fill(P.curriculumVersions, { curriculumId })),
    reviseCurriculum: (curriculumId, input) => create(fill(P.curriculumRevisions, { curriculumId }), input),
    curriculumVersionCommand: (versionId, version, cmd, reason) => command(fill(cmd === "publish" ? P.curriculumVersionPublish : P.curriculumVersionRetire, { versionId }), version, reasonBody(reason)),
    listSchoolModels: (id) => items(fill(P.schoolModels, { id })),
    createSchoolModel: (id, input) => create(fill(P.schoolModels, { id }), input),
    schoolModelCommand: (schoolModelId, version) => command(fill(P.schoolModelPublish, { schoolModelId }), version),

    readiness: (id) => get(fill(P.readiness, { id })),
    listCalendars: (id) => items(fill(P.calendars, { id })),
    createCalendar: (id, input) => create(fill(P.calendars, { id }), input),
    retireCalendar: (calendarId, version, reason) => command(fill(P.calendarRetire, { calendarId }), version, { reason }),
    listYears: (id) => items(fill(P.years, { id })),
    createYear: (id, input) => create(fill(P.years, { id }), input),
    openYear: (yearId, version) => command(fill(P.yearOpen, { yearId }), version),
    closeYear: (yearId, version, input) => command(fill(P.yearClose, { yearId }), version, input),
    listVersions: (calendarId) => items(fill(P.versions, { calendarId })),
    createVersion: (calendarId, input) => create(fill(P.versions, { calendarId }), input),
    addTerm: (versionId, version, input) => request("POST", fill(P.terms, { versionId }), { body: input, ifMatch: version, idempotent: true }),
    removeTerm: (versionId, version, termId) => request("DELETE", fill(P.term, { versionId, termId }), { ifMatch: version }),
    addKeyDate: (versionId, version, input) => request("POST", fill(P.keyDates, { versionId }), { body: input, ifMatch: version, idempotent: true }),
    removeKeyDate: (versionId, version, keyDateId) => request("DELETE", fill(P.keyDate, { versionId, keyDateId }), { ifMatch: version }),
    versionCommand(versionId, version, cmd, reason) {
      const t = cmd === "publish" ? P.versionPublish : cmd === "revise" ? P.versionRevise : P.versionRetire;
      return command(fill(t, { versionId }), version, reason ? { reason } : {});
    },
    listAssignments: (calendarId) => items(fill(P.assignments, { calendarId })),
    createAssignment: (calendarId, input) => create(fill(P.assignments, { calendarId }), input),
    assignmentCommand: (assignmentId, version, cmd) => command(fill(cmd === "activate" ? P.assignmentActivate : P.assignmentRetire, { assignmentId }), version),
    listExceptions: (calendarId) => items(fill(P.exceptions, { calendarId })),
    createException: (calendarId, input) => create(fill(P.exceptions, { calendarId }), input),
    exceptionCommand(exceptionId, version, cmd) {
      const t = cmd === "apply" ? P.exceptionApply : cmd === "complete" ? P.exceptionComplete : P.exceptionWithdraw;
      return command(fill(t, { exceptionId }), version);
    },
    simulate: (calendarId, baselineVersionId, proposal) => create(fill(P.simulations, { calendarId }), { baselineVersionId, proposal }),
    decideSimulation: (simulationId, version, decision, reason) => command(fill(P.simulationDecision, { simulationId }), version, { decision, reason }),
  };
}
