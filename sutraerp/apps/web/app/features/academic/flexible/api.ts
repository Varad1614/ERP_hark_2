/**
 * Slice-4C API layer (flexible / external learning, credit recognition, course resources).
 * Same conventions as `features/institution/api.ts`: one interface, an HTTP implementation
 * (CSRF, Idempotency-Key on creates, If-Match on commands, problem+json → `ApiProblem`) and an
 * in-memory mock (`./mock`). Every path template is checked against the generated OpenAPI paths.
 */
import { ApiProblem, isProblem, isUnsafeMethod, newIdempotencyKey, type paths } from "@sutra/api-client";
import { getCsrfToken } from "../../../lib/api";
import { fill } from "../../institution/api";
import type {
  AllocationRun,
  CertificateVerification,
  CourseResource,
  CreateOpportunity,
  CreateProvider,
  CreditRecognition,
  ExceptionCase,
  FlexibleEnrolment,
  FlexiblePolicy,
  Items,
  LearningOpportunity,
  LearningProvider,
  LibraryItem,
  MappingRule,
  MyFlexibleLearning,
  MyResources,
  PolicyRules,
  ProviderRecord,
  ReconciliationRun,
  ResourceKind,
} from "./types";

export type ProviderCommand = "submit" | "suspend" | "reactivate" | "offboard";
export type OpportunityCommand = "submit" | "suspend" | "reactivate" | "archive";
export type SeatCommand = "allocate" | "promote" | "resolve";

export interface FlexibleApi {
  // providers + opportunities + policies
  listProviders(institutionId: string): Promise<LearningProvider[]>;
  createProvider(institutionId: string, input: CreateProvider): Promise<LearningProvider>;
  providerCommand(id: string, version: number, cmd: ProviderCommand, reason?: string): Promise<LearningProvider>;
  listOpportunities(institutionId: string): Promise<LearningOpportunity[]>;
  createOpportunity(institutionId: string, input: CreateOpportunity): Promise<LearningOpportunity>;
  opportunityCommand(id: string, version: number, cmd: OpportunityCommand, reason?: string): Promise<LearningOpportunity>;
  setCapacity(id: string, version: number, capacity: number, reservedSeats: number): Promise<LearningOpportunity>;
  seatCommand(id: string, version: number, cmd: SeatCommand): Promise<AllocationRun>;
  listPolicies(institutionId: string): Promise<FlexiblePolicy[]>;
  createPolicy(institutionId: string, input: { name: string; rules: PolicyRules; effectiveFrom: string }): Promise<FlexiblePolicy>;
  submitPolicy(id: string, version: number): Promise<FlexiblePolicy>;
  // enrolments
  listEnrolments(opportunityId: string): Promise<FlexibleEnrolment[]>;
  enrolmentDecision(id: string, version: number, approve: boolean, reason?: string): Promise<FlexibleEnrolment>;
  register(id: string, version: number): Promise<FlexibleEnrolment>;
  registrationOutcome(id: string, version: number, outcome: string, providerReference?: string, reason?: string): Promise<FlexibleEnrolment>;
  progress(id: string, version: number, hoursCompleted: number): Promise<FlexibleEnrolment>;
  complete(id: string, version: number, input: { hoursCompleted: number; assessmentStatus: string; providerEvidenceRef?: string; grade?: string }): Promise<FlexibleEnrolment>;
  withdraw(id: string, version: number, reason: string): Promise<FlexibleEnrolment>;
  // exceptions
  listExceptions(institutionId: string): Promise<ExceptionCase[]>;
  raiseException(institutionId: string, input: { category: string; title: string; description?: string }): Promise<ExceptionCase>;
  planException(id: string, version: number, correctivePlan: string): Promise<ExceptionCase>;
  resolveException(id: string, version: number, correctiveAction: string, resolutionEvidence: string): Promise<ExceptionCase>;
  closeException(id: string, version: number): Promise<ExceptionCase>;
  // certificates + recognition
  certificateQueue(institutionId: string): Promise<CertificateVerification[]>;
  submitCertificate(enrolmentId: string, input: { certificateNumber: string; certificateUrl?: string; issuedOn?: string }): Promise<CertificateVerification>;
  verifyCertificate(id: string, version: number, input: { authenticityConfirmed: boolean; identityMatched: boolean; fraudIndicators: string[] }): Promise<CertificateVerification>;
  listMappings(institutionId: string): Promise<MappingRule[]>;
  createMapping(institutionId: string, input: { opportunityId: string; equivalency: string; credits: number; effectiveFrom: string }): Promise<MappingRule>;
  submitMapping(id: string, version: number): Promise<MappingRule>;
  listRecognitions(institutionId: string): Promise<CreditRecognition[]>;
  requestRecognition(enrolmentId: string): Promise<CreditRecognition>;
  decideRecognition(id: string, version: number, approve: boolean, credits?: number, reason?: string): Promise<CreditRecognition>;
  postRecognition(id: string, version: number): Promise<CreditRecognition>;
  listReconciliations(institutionId: string): Promise<ReconciliationRun[]>;
  runReconciliation(institutionId: string, opportunityId: string, records: ProviderRecord[]): Promise<ReconciliationRun>;
  resolveReconciliation(id: string, version: number, resolution: string, notes?: string): Promise<ReconciliationRun>;
  // self-service + resources
  myFlexible(): Promise<MyFlexibleLearning>;
  requestOpportunity(opportunityId: string): Promise<FlexibleEnrolment>;
  myLibrary(): Promise<LibraryItem[]>;
  myResources(): Promise<MyResources>;
  createResource(sectionId: string, input: { title: string; kind: ResourceKind; url: string; description?: string; publish?: boolean }): Promise<CourseResource>;
  resourceCommand(id: string, version: number, cmd: "publish" | "withdraw"): Promise<CourseResource>;
}

const B = "/api/v1/academic";
export const PATHS = {
  providers: `${B}/institutions/{id}/learning-providers`,
  pSubmit: `${B}/learning-providers/{id}/submit`,
  pSuspend: `${B}/learning-providers/{id}/suspend`,
  pReactivate: `${B}/learning-providers/{id}/reactivate`,
  pOffboard: `${B}/learning-providers/{id}/offboard`,
  opportunities: `${B}/institutions/{id}/learning-opportunities`,
  oSubmit: `${B}/learning-opportunities/{id}/submit`,
  oSuspend: `${B}/learning-opportunities/{id}/suspend`,
  oReactivate: `${B}/learning-opportunities/{id}/reactivate`,
  oArchive: `${B}/learning-opportunities/{id}/archive`,
  oCapacity: `${B}/learning-opportunities/{id}/capacity`,
  oAllocate: `${B}/learning-opportunities/{id}/allocate`,
  oPromote: `${B}/learning-opportunities/{id}/promote-waitlist`,
  oResolve: `${B}/learning-opportunities/{id}/resolve-conflicts`,
  oEnrolments: `${B}/learning-opportunities/{id}/enrolments`,
  policies: `${B}/institutions/{id}/flexible-learning-policies`,
  policySubmit: `${B}/flexible-learning-policies/{id}/submit`,
  eDecision: `${B}/flexible-enrolments/{id}/decision`,
  eRegister: `${B}/flexible-enrolments/{id}/register`,
  eOutcome: `${B}/flexible-enrolments/{id}/registration-outcome`,
  eProgress: `${B}/flexible-enrolments/{id}/progress`,
  eComplete: `${B}/flexible-enrolments/{id}/completion`,
  eWithdraw: `${B}/flexible-enrolments/{id}/withdraw`,
  eCertificates: `${B}/flexible-enrolments/{id}/certificates`,
  eRecognitions: `${B}/flexible-enrolments/{id}/credit-recognitions`,
  exceptions: `${B}/institutions/{id}/flexible-exceptions`,
  xPlan: `${B}/flexible-exceptions/{id}/plan`,
  xResolve: `${B}/flexible-exceptions/{id}/resolve`,
  xClose: `${B}/flexible-exceptions/{id}/close`,
  certQueue: `${B}/institutions/{id}/certificate-verifications`,
  certVerify: `${B}/certificate-verifications/{id}/verify`,
  mappings: `${B}/institutions/{id}/credit-mapping-rules`,
  mappingSubmit: `${B}/credit-mapping-rules/{id}/submit`,
  recognitions: `${B}/institutions/{id}/credit-recognitions`,
  rDecision: `${B}/credit-recognitions/{id}/decision`,
  rPost: `${B}/credit-recognitions/{id}/post`,
  reconciliations: `${B}/institutions/{id}/reconciliation-runs`,
  rcResolve: `${B}/reconciliation-runs/{id}/resolve`,
  meFlexible: `${B}/me/flexible-learning`,
  meRequest: `${B}/me/flexible-learning/requests`,
  meLibrary: `${B}/me/library`,
  myResources: `${B}/my/resources`,
  sectionResources: `${B}/sections/{id}/resources`,
  resPublish: `${B}/resources/{id}/publish`,
  resWithdraw: `${B}/resources/{id}/withdraw`,
} as const satisfies Record<string, keyof paths>;

type Fetch = (input: string, init: RequestInit) => Promise<Response>;

interface RequestOptions {
  body?: unknown;
  ifMatch?: number;
  idempotent?: boolean;
}

const PROVIDER_CMD: Record<ProviderCommand, string> = { submit: PATHS.pSubmit, suspend: PATHS.pSuspend, reactivate: PATHS.pReactivate, offboard: PATHS.pOffboard };
const OPPORTUNITY_CMD: Record<OpportunityCommand, string> = { submit: PATHS.oSubmit, suspend: PATHS.oSuspend, reactivate: PATHS.oReactivate, archive: PATHS.oArchive };
const SEAT_CMD: Record<SeatCommand, string> = { allocate: PATHS.oAllocate, promote: PATHS.oPromote, resolve: PATHS.oResolve };

export function createHttpFlexibleApi(fetchImpl: Fetch = (i, init) => fetch(i, init), baseUrl: string = import.meta.env.VITE_API_BASE ?? ""): FlexibleApi {
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
  const decision = (approve: boolean, reason?: string) => ({ decision: approve ? "approve" : "reject", reason });
  return {
    listProviders: (i) => items(fill(PATHS.providers, id(i))),
    createProvider: (i, input) => request("POST", fill(PATHS.providers, id(i)), { body: input, idempotent: true }),
    providerCommand: (i, v, cmd, reason) => request("POST", fill(PROVIDER_CMD[cmd], id(i)), { body: reasonBody(reason), ifMatch: v }),
    listOpportunities: (i) => items(fill(PATHS.opportunities, id(i))),
    createOpportunity: (i, input) => request("POST", fill(PATHS.opportunities, id(i)), { body: input, idempotent: true }),
    opportunityCommand: (i, v, cmd, reason) => request("POST", fill(OPPORTUNITY_CMD[cmd], id(i)), { body: reasonBody(reason), ifMatch: v }),
    setCapacity: (i, v, capacity, reservedSeats) => request("POST", fill(PATHS.oCapacity, id(i)), { body: { capacity, reservedSeats }, ifMatch: v }),
    seatCommand: (i, v, cmd) => request("POST", fill(SEAT_CMD[cmd], id(i)), { ifMatch: v }),
    listPolicies: (i) => items(fill(PATHS.policies, id(i))),
    createPolicy: (i, input) => request("POST", fill(PATHS.policies, id(i)), { body: input, idempotent: true }),
    submitPolicy: (i, v) => request("POST", fill(PATHS.policySubmit, id(i)), { ifMatch: v }),
    listEnrolments: (o) => items(fill(PATHS.oEnrolments, id(o))),
    enrolmentDecision: (i, v, approve, reason) => request("POST", fill(PATHS.eDecision, id(i)), { body: decision(approve, reason), ifMatch: v }),
    register: (i, v) => request("POST", fill(PATHS.eRegister, id(i)), { ifMatch: v }),
    registrationOutcome: (i, v, outcome, providerReference, reason) =>
      request("POST", fill(PATHS.eOutcome, id(i)), { body: { outcome, providerReference, reason, evidenceKey: providerReference ? `ack-${providerReference}` : undefined }, ifMatch: v }),
    progress: (i, v, hoursCompleted) => request("POST", fill(PATHS.eProgress, id(i)), { body: { hoursCompleted }, ifMatch: v }),
    complete: (i, v, input) => request("POST", fill(PATHS.eComplete, id(i)), { body: input, ifMatch: v }),
    withdraw: (i, v, reason) => request("POST", fill(PATHS.eWithdraw, id(i)), { body: { reason }, ifMatch: v }),
    listExceptions: (i) => items(fill(PATHS.exceptions, id(i))),
    raiseException: (i, input) => request("POST", fill(PATHS.exceptions, id(i)), { body: input, idempotent: true }),
    planException: (i, v, correctivePlan) => request("POST", fill(PATHS.xPlan, id(i)), { body: { correctivePlan }, ifMatch: v }),
    resolveException: (i, v, correctiveAction, resolutionEvidence) => request("POST", fill(PATHS.xResolve, id(i)), { body: { correctiveAction, resolutionEvidence }, ifMatch: v }),
    closeException: (i, v) => request("POST", fill(PATHS.xClose, id(i)), { ifMatch: v }),
    certificateQueue: (i) => items(fill(PATHS.certQueue, id(i))),
    submitCertificate: (e, input) => request("POST", fill(PATHS.eCertificates, id(e)), { body: input, idempotent: true }),
    verifyCertificate: (i, v, input) => request("POST", fill(PATHS.certVerify, id(i)), { body: input, ifMatch: v }),
    listMappings: (i) => items(fill(PATHS.mappings, id(i))),
    createMapping: (i, input) => request("POST", fill(PATHS.mappings, id(i)), { body: input, idempotent: true }),
    submitMapping: (i, v) => request("POST", fill(PATHS.mappingSubmit, id(i)), { ifMatch: v }),
    listRecognitions: (i) => items(fill(PATHS.recognitions, id(i))),
    requestRecognition: (e) => request("POST", fill(PATHS.eRecognitions, id(e)), { body: {}, idempotent: true }),
    decideRecognition: (i, v, approve, credits, reason) => request("POST", fill(PATHS.rDecision, id(i)), { body: { ...decision(approve, reason), credits }, ifMatch: v }),
    postRecognition: (i, v) => request("POST", fill(PATHS.rPost, id(i)), { ifMatch: v }),
    listReconciliations: (i) => items(fill(PATHS.reconciliations, id(i))),
    runReconciliation: (i, opportunityId, providerRecords) => request("POST", fill(PATHS.reconciliations, id(i)), { body: { opportunityId, providerRecords }, idempotent: true }),
    resolveReconciliation: (i, v, resolution, notes) => request("POST", fill(PATHS.rcResolve, id(i)), { body: { resolution, notes }, ifMatch: v }),
    myFlexible: () => request("GET", PATHS.meFlexible),
    requestOpportunity: (opportunityId) => request("POST", PATHS.meRequest, { body: { opportunityId }, idempotent: true }),
    myLibrary: () => items(PATHS.meLibrary),
    myResources: () => request("GET", PATHS.myResources),
    createResource: (s, input) => request("POST", fill(PATHS.sectionResources, id(s)), { body: input, idempotent: true }),
    resourceCommand: (i, v, cmd) => request("POST", fill(cmd === "publish" ? PATHS.resPublish : PATHS.resWithdraw, id(i)), { ifMatch: v }),
  };
}

/** Parses "S1:REF-1:done:30; S2::open" provider records (studentNumber:reference:done|open:hours). */
export function parseProviderRecords(text: string): ProviderRecord[] {
  return text
    .split(/[;\n]/)
    .map((l) => l.trim())
    .filter(Boolean)
    .map((l) => {
      const [studentNumber = "", ref = "", state = "", hours = ""] = l.split(":").map((x) => x.trim());
      return { studentNumber, providerReference: ref || null, completed: state === "done" || state === "completed", hoursCompleted: hours ? Number(hours) : null };
    });
}
