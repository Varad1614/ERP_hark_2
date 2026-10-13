/**
 * Slice-7A API layer: one interface, an HTTP implementation (CSRF, Idempotency-Key on creates,
 * If-Match on commands, problem+json → `ApiProblem`) and an in-memory mock (`./mock`). Every
 * path template is checked against the generated OpenAPI `paths`.
 */
import { ApiProblem, isProblem, isUnsafeMethod, newIdempotencyKey, type paths } from "@sutra/api-client";
import { getCsrfToken } from "../../../lib/api";
import { fill } from "../../institution/api";
import type {
  Assignment,
  Candidate,
  CandidateCommand,
  CertificateRequest,
  Decision,
  Engagement,
  EngagementCommand,
  EngagementInput,
  Items,
  MyProfile,
  OnboardingCase,
  Position,
  PositionCommand,
  Requisition,
  Staff,
  StaffCommand,
  StaffInput,
} from "./types";

export interface WorkforceApi {
  staff(institutionId: string, status?: string): Promise<Staff[]>;
  staffRecord(id: string): Promise<Staff>;
  createStaff(institutionId: string, input: StaffInput): Promise<Staff>;
  staffCommand(id: string, version: number, cmd: StaffCommand, reason?: string): Promise<Staff>;
  engagements(staffId: string): Promise<Engagement[]>;
  createEngagement(staffId: string, input: EngagementInput): Promise<Engagement>;
  engagementCommand(id: string, version: number, cmd: EngagementCommand, reason?: string): Promise<Engagement>;
  decideEngagement(id: string, version: number, decision: Decision, reason?: string): Promise<Engagement>;
  closeEngagement(id: string, version: number, lastWorkingDate: string, reason: string): Promise<Engagement>;
  /** Reporting manager of an engagement (`null` removes it, L3-E-018). */
  setReporting(id: string, version: number, reportsToStaffId: string | null, reason?: string): Promise<Engagement>;
  positions(institutionId: string): Promise<Position[]>;
  createPosition(institutionId: string, input: { code: string; title: string; staffCategory?: string; allowsMultiple?: boolean }): Promise<Position>;
  positionCommand(id: string, version: number, cmd: PositionCommand, reason?: string): Promise<Position>;
  assignments(positionId: string): Promise<Assignment[]>;
  assign(positionId: string, input: { engagementId: string; startDate: string }): Promise<Assignment>;
  endAssignment(id: string, version: number, input: { endDate: string; outcome: "completed" | "terminated"; reason: string }): Promise<Assignment>;
  decideAssignment(id: string, version: number, decision: Decision, reason?: string): Promise<Assignment>;
  requisitions(institutionId: string): Promise<Requisition[]>;
  createRequisition(institutionId: string, input: { title: string; staffCategory: string; employmentType: string; designation: string; openings: number }): Promise<Requisition>;
  submitRequisition(id: string, version: number): Promise<Requisition>;
  decideRequisition(id: string, version: number, decision: Decision, reason?: string): Promise<Requisition>;
  closeRequisition(id: string, version: number, reason: string): Promise<Requisition>;
  candidates(requisitionId: string): Promise<Candidate[]>;
  addCandidate(requisitionId: string, input: { givenName: string; familyName?: string; email: string }): Promise<Candidate>;
  candidateCommand(id: string, version: number, cmd: CandidateCommand, reason?: string): Promise<Candidate>;
  offer(id: string, version: number, proposedJoiningDate: string): Promise<Candidate>;
  onboardingCases(institutionId: string): Promise<OnboardingCase[]>;
  startOnboarding(candidateId: string, input: { probationEndDate?: string }): Promise<OnboardingCase>;
  onboardingDocuments(id: string, version: number, documentType: string, reference: string): Promise<OnboardingCase>;
  verifyOnboarding(id: string, version: number, approved: boolean, reason?: string): Promise<OnboardingCase>;
  onboardingReady(id: string, version: number): Promise<OnboardingCase>;
  join(id: string, version: number): Promise<OnboardingCase>;
  myProfile(): Promise<MyProfile | null>;
  requestCertificate(input: { kind: "service" | "experience"; purpose: string }): Promise<CertificateRequest>;
}

const B = "/api/v1/people";
export const PATHS = {
  staff: `${B}/institutions/{id}/staff`,
  staffRecord: `${B}/staff/{id}`,
  submitVerification: `${B}/staff/{id}/submit-verification`,
  verify: `${B}/staff/{id}/verify`,
  activateStaff: `${B}/staff/{id}/activate`,
  suspendStaff: `${B}/staff/{id}/suspend`,
  restoreStaff: `${B}/staff/{id}/restore`,
  archiveStaff: `${B}/staff/{id}/archive`,
  engagements: `${B}/staff/{id}/engagements`,
  reporting: `${B}/engagements/{id}/reporting`,
  submit: `${B}/engagements/{id}/submit`,
  activate: `${B}/engagements/{id}/activate`,
  suspend: `${B}/engagements/{id}/suspend`,
  restore: `${B}/engagements/{id}/restore`,
  cancel: `${B}/engagements/{id}/cancel`,
  close: `${B}/engagements/{id}/close`,
  decision: `${B}/engagements/{id}/decision`,
  positions: `${B}/institutions/{id}/positions`,
  positionActivate: `${B}/positions/{id}/activate`,
  positionFreeze: `${B}/positions/{id}/freeze`,
  positionRetire: `${B}/positions/{id}/retire`,
  assignments: `${B}/positions/{id}/assignments`,
  assignmentEnd: `${B}/position-assignments/{id}/end`,
  assignmentDecision: `${B}/position-assignments/{id}/decision`,
  requisitions: `${B}/institutions/{id}/requisitions`,
  requisitionSubmit: `${B}/requisitions/{id}/submit`,
  requisitionDecision: `${B}/requisitions/{id}/decision`,
  requisitionClose: `${B}/requisitions/{id}/close`,
  candidates: `${B}/requisitions/{id}/candidates`,
  shortlist: `${B}/candidates/{id}/shortlist`,
  interview: `${B}/candidates/{id}/interview`,
  offer: `${B}/candidates/{id}/offer`,
  accept: `${B}/candidates/{id}/accept`,
  decline: `${B}/candidates/{id}/decline`,
  reject: `${B}/candidates/{id}/reject`,
  onboarding: `${B}/candidates/{id}/onboarding`,
  onboardingCases: `${B}/institutions/{id}/onboarding-cases`,
  onboardingDocuments: `${B}/onboarding-cases/{id}/documents`,
  onboardingVerify: `${B}/onboarding-cases/{id}/verify`,
  onboardingReady: `${B}/onboarding-cases/{id}/ready`,
  onboardingJoin: `${B}/onboarding-cases/{id}/join`,
  meProfile: `${B}/me/profile`,
  meCertificates: `${B}/me/certificate-requests`,
} as const satisfies Record<string, keyof paths>;

const STAFF_CMD: Record<StaffCommand, string> = {
  "submit-verification": PATHS.submitVerification,
  verify: PATHS.verify,
  activate: PATHS.activateStaff,
  suspend: PATHS.suspendStaff,
  restore: PATHS.restoreStaff,
  archive: PATHS.archiveStaff,
};
const ENG_CMD: Record<EngagementCommand, string> = {
  submit: PATHS.submit,
  activate: PATHS.activate,
  suspend: PATHS.suspend,
  restore: PATHS.restore,
  cancel: PATHS.cancel,
};
const POS_CMD: Record<PositionCommand, string> = { activate: PATHS.positionActivate, freeze: PATHS.positionFreeze, retire: PATHS.positionRetire };
const CAND_CMD: Record<CandidateCommand, string> = {
  shortlist: PATHS.shortlist,
  interview: PATHS.interview,
  accept: PATHS.accept,
  decline: PATHS.decline,
  reject: PATHS.reject,
};

type Fetch = (input: string, init: RequestInit) => Promise<Response>;

interface RequestOptions {
  body?: unknown;
  ifMatch?: number;
  idempotent?: boolean;
}

export function createHttpWorkforceApi(fetchImpl: Fetch = (i, init) => fetch(i, init), baseUrl: string = import.meta.env.VITE_API_BASE ?? ""): WorkforceApi {
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
    staff: (i, status) => items(fill(PATHS.staff, id(i)) + (status ? `?status=${encodeURIComponent(status)}` : "")),
    staffRecord: (i) => request("GET", fill(PATHS.staffRecord, id(i))),
    createStaff: (i, input) => request("POST", fill(PATHS.staff, id(i)), { body: input, idempotent: true }),
    staffCommand: (i, v, cmd, reason) => request("POST", fill(STAFF_CMD[cmd], id(i)), { ifMatch: v, body: reasonBody(reason) }),
    engagements: (s) => items(fill(PATHS.engagements, id(s))),
    createEngagement: (s, input) => request("POST", fill(PATHS.engagements, id(s)), { body: input, idempotent: true }),
    engagementCommand: (i, v, cmd, reason) => request("POST", fill(ENG_CMD[cmd], id(i)), { ifMatch: v, body: reasonBody(reason) }),
    decideEngagement: (i, v, decision, reason) => request("POST", fill(PATHS.decision, id(i)), { ifMatch: v, body: { decision, reason } }),
    closeEngagement: (i, v, lastWorkingDate, reason) => request("POST", fill(PATHS.close, id(i)), { ifMatch: v, body: { lastWorkingDate, reason } }),
    setReporting: (i, v, reportsToStaffId, reason) => request("PUT", fill(PATHS.reporting, id(i)), { ifMatch: v, body: { reportsToStaffId, reason: reason || undefined } }),
    positions: (i) => items(fill(PATHS.positions, id(i))),
    createPosition: (i, input) => request("POST", fill(PATHS.positions, id(i)), { body: input, idempotent: true }),
    positionCommand: (i, v, cmd, reason) => request("POST", fill(POS_CMD[cmd], id(i)), { ifMatch: v, body: reasonBody(reason) }),
    assignments: (p) => items(fill(PATHS.assignments, id(p))),
    assign: (p, input) => request("POST", fill(PATHS.assignments, id(p)), { body: input, idempotent: true }),
    endAssignment: (i, v, input) => request("POST", fill(PATHS.assignmentEnd, id(i)), { ifMatch: v, body: input }),
    decideAssignment: (i, v, decision, reason) => request("POST", fill(PATHS.assignmentDecision, id(i)), { ifMatch: v, body: { decision, reason } }),
    requisitions: (i) => items(fill(PATHS.requisitions, id(i))),
    createRequisition: (i, input) => request("POST", fill(PATHS.requisitions, id(i)), { body: input, idempotent: true }),
    submitRequisition: (i, v) => request("POST", fill(PATHS.requisitionSubmit, id(i)), { ifMatch: v }),
    decideRequisition: (i, v, decision, reason) => request("POST", fill(PATHS.requisitionDecision, id(i)), { ifMatch: v, body: { decision, reason } }),
    closeRequisition: (i, v, reason) => request("POST", fill(PATHS.requisitionClose, id(i)), { ifMatch: v, body: { reason } }),
    candidates: (r) => items(fill(PATHS.candidates, id(r))),
    addCandidate: (r, input) => request("POST", fill(PATHS.candidates, id(r)), { body: input, idempotent: true }),
    candidateCommand: (i, v, cmd, reason) => request("POST", fill(CAND_CMD[cmd], id(i)), { ifMatch: v, body: reasonBody(reason) }),
    offer: (i, v, proposedJoiningDate) => request("POST", fill(PATHS.offer, id(i)), { ifMatch: v, body: { proposedJoiningDate } }),
    onboardingCases: (i) => items(fill(PATHS.onboardingCases, id(i))),
    startOnboarding: (c, input) => request("POST", fill(PATHS.onboarding, id(c)), { body: input, idempotent: true }),
    onboardingDocuments: (i, v, documentType, reference) =>
      request("POST", fill(PATHS.onboardingDocuments, id(i)), { ifMatch: v, body: { documents: [{ documentType, reference }], submitForVerification: true } }),
    verifyOnboarding: (i, v, approved, reason) => request("POST", fill(PATHS.onboardingVerify, id(i)), { ifMatch: v, body: { approved, reason } }),
    onboardingReady: (i, v) => request("POST", fill(PATHS.onboardingReady, id(i)), { ifMatch: v }),
    join: (i, v) => request("POST", fill(PATHS.onboardingJoin, id(i)), { ifMatch: v }),
    myProfile: async () => {
      try {
        return await request<MyProfile>("GET", PATHS.meProfile);
      } catch (e) {
        if (e instanceof ApiProblem && e.problem.status === 404) return null;
        throw e;
      }
    },
    requestCertificate: (input) => request("POST", PATHS.meCertificates, { body: input, idempotent: true }),
  };
}
