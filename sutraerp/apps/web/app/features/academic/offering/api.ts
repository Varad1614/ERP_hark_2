/**
 * Slice-4A API layer (offerings, sections, enrolment, faculty assignment, self-service). Same
 * conventions as `features/institution/api.ts`: one interface, an HTTP implementation (CSRF,
 * Idempotency-Key on creates, If-Match on commands, problem+json → `ApiProblem`) and an
 * in-memory mock (`./mock`). Every path template is checked against the generated OpenAPI `paths`.
 */
import { ApiProblem, isProblem, isUnsafeMethod, newIdempotencyKey, type paths } from "@sutra/api-client";
import { getCsrfToken } from "../../../lib/api";
import { fill } from "../../institution/api";
import type {
  AssignmentCommand,
  AvailableCourse,
  CreateOffering,
  Decision,
  FacultyAssignment,
  FacultyAssignmentPolicy,
  FacultyPolicyDraft,
  GovernedCommand,
  Items,
  MyClass,
  MyCourse,
  MyStudent,
  NewFacultyAssignment,
  NewOfferingSection,
  Offering,
  OfferingCommand,
  OfferingGovernedOutcome,
  OfferingSection,
  OfferingWaitlistEntry,
  SectionEnrolment,
} from "./types";

export interface OfferingApi {
  listOfferings(institutionId: string): Promise<Offering[]>;
  getOffering(id: string): Promise<Offering>;
  createOffering(institutionId: string, input: CreateOffering): Promise<Offering>;
  offeringCommand(id: string, version: number, cmd: OfferingCommand, reason?: string): Promise<Offering>;
  decideOffering(id: string, version: number, decision: Decision, reason?: string): Promise<Offering>;
  governed(id: string, version: number, cmd: GovernedCommand, reason: string): Promise<OfferingGovernedOutcome>;
  addSection(offeringId: string, input: NewOfferingSection): Promise<OfferingSection>;
  changeCapacity(offeringId: string, version: number, input: { sectionId?: string | null; capacity: number | null; reason: string }): Promise<Offering>;
  roster(sectionId: string): Promise<SectionEnrolment[]>;
  seat(sectionId: string, input: { studentId: string; overrideEligibility?: boolean; reason?: string }): Promise<SectionEnrolment>;
  withdrawSeat(enrolmentId: string, version: number, reason: string): Promise<SectionEnrolment>;
  waitlist(offeringId: string): Promise<OfferingWaitlistEntry[]>;
  promote(offeringId: string): Promise<OfferingWaitlistEntry>;
  listAssignments(sectionId: string): Promise<FacultyAssignment[]>;
  createAssignment(sectionId: string, input: NewFacultyAssignment): Promise<FacultyAssignment>;
  assignmentCommand(id: string, version: number, cmd: AssignmentCommand, reason?: string): Promise<FacultyAssignment>;
  decideAssignment(id: string, version: number, decision: Decision, reason?: string): Promise<FacultyAssignment>;
  listPolicies(institutionId: string): Promise<FacultyAssignmentPolicy[]>;
  createPolicy(institutionId: string, input: FacultyPolicyDraft): Promise<FacultyAssignmentPolicy>;
  submitPolicy(id: string, version: number): Promise<FacultyAssignmentPolicy>;
  reviewPolicy(id: string, version: number, outcome: "compatible" | "incompatible", notes?: string): Promise<FacultyAssignmentPolicy>;
  myCourses(): Promise<MyCourse[]>;
  availableCourses(): Promise<AvailableCourse[]>;
  enrolMe(sectionId: string): Promise<SectionEnrolment>;
  dropMine(enrolmentId: string, version: number): Promise<SectionEnrolment>;
  joinWaitlist(offeringId: string): Promise<OfferingWaitlistEntry>;
  myClasses(): Promise<MyClass[]>;
  myStudents(): Promise<MyStudent[]>;
}

const B = "/api/v1/academic";
export const PATHS = {
  offerings: `${B}/institutions/{id}/offerings`,
  offering: `${B}/offerings/{id}`,
  submit: `${B}/offerings/{id}/submit`,
  decision: `${B}/offerings/{id}/decision`,
  publish: `${B}/offerings/{id}/publish`,
  openEnrolment: `${B}/offerings/{id}/open-enrolment`,
  closeEnrolment: `${B}/offerings/{id}/close-enrolment`,
  activate: `${B}/offerings/{id}/activate`,
  suspend: `${B}/offerings/{id}/suspend`,
  reactivate: `${B}/offerings/{id}/reactivate`,
  freeze: `${B}/offerings/{id}/freeze`,
  complete: `${B}/offerings/{id}/complete`,
  archive: `${B}/offerings/{id}/archive`,
  cancel: `${B}/offerings/{id}/cancel`,
  unfreeze: `${B}/offerings/{id}/unfreeze`,
  sections: `${B}/offerings/{id}/sections`,
  capacity: `${B}/offerings/{id}/capacity`,
  enrolments: `${B}/sections/{id}/enrolments`,
  withdraw: `${B}/enrolments/{id}/withdraw`,
  waitlist: `${B}/offerings/{id}/waitlist`,
  promote: `${B}/offerings/{id}/waitlist/promote`,
  assignments: `${B}/sections/{id}/faculty-assignments`,
  assignmentSubmit: `${B}/faculty-assignments/{id}/submit`,
  assignmentDecision: `${B}/faculty-assignments/{id}/decision`,
  assignmentActivate: `${B}/faculty-assignments/{id}/activate`,
  assignmentRevoke: `${B}/faculty-assignments/{id}/revoke`,
  assignmentComplete: `${B}/faculty-assignments/{id}/complete`,
  assignmentWithdraw: `${B}/faculty-assignments/{id}/withdraw`,
  policies: `${B}/institutions/{id}/faculty-assignment-policies`,
  policySubmit: `${B}/faculty-assignment-policies/{id}/submit`,
  policyReview: `${B}/faculty-assignment-policies/{id}/review`,
  myCourses: `${B}/me/courses`,
  available: `${B}/me/courses/available`,
  myEnrolments: `${B}/me/enrolments`,
  myDrop: `${B}/me/enrolments/{id}/drop`,
  myWaitlist: `${B}/me/waitlist`,
  myClasses: `${B}/my/classes`,
  myStudents: `${B}/my/students`,
} as const satisfies Record<string, keyof paths>;

type Fetch = (input: string, init: RequestInit) => Promise<Response>;

interface RequestOptions {
  body?: unknown;
  ifMatch?: number;
  idempotent?: boolean;
}

const OFFERING_CMD: Record<OfferingCommand, string> = {
  submit: PATHS.submit,
  publish: PATHS.publish,
  "open-enrolment": PATHS.openEnrolment,
  "close-enrolment": PATHS.closeEnrolment,
  activate: PATHS.activate,
  suspend: PATHS.suspend,
  reactivate: PATHS.reactivate,
  freeze: PATHS.freeze,
  complete: PATHS.complete,
  archive: PATHS.archive,
};
const GOVERNED_CMD: Record<GovernedCommand, string> = { cancel: PATHS.cancel, unfreeze: PATHS.unfreeze };
const ASSIGNMENT_CMD: Record<AssignmentCommand, string> = {
  submit: PATHS.assignmentSubmit,
  activate: PATHS.assignmentActivate,
  revoke: PATHS.assignmentRevoke,
  complete: PATHS.assignmentComplete,
  withdraw: PATHS.assignmentWithdraw,
};

export function createHttpOfferingApi(fetchImpl: Fetch = (i, init) => fetch(i, init), baseUrl: string = import.meta.env.VITE_API_BASE ?? ""): OfferingApi {
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
    listOfferings: (i) => items(fill(PATHS.offerings, id(i))),
    getOffering: (i) => request("GET", fill(PATHS.offering, id(i))),
    createOffering: (i, input) => request("POST", fill(PATHS.offerings, id(i)), { body: input, idempotent: true }),
    offeringCommand: (i, v, cmd, reason) => request("POST", fill(OFFERING_CMD[cmd], id(i)), { body: reasonBody(reason), ifMatch: v }),
    decideOffering: (i, v, decision, reason) => request("POST", fill(PATHS.decision, id(i)), { body: { decision, reason }, ifMatch: v }),
    governed: (i, v, cmd, reason) => request("POST", fill(GOVERNED_CMD[cmd], id(i)), { body: { reason }, ifMatch: v }),
    addSection: (i, input) => request("POST", fill(PATHS.sections, id(i)), { body: input, idempotent: true }),
    changeCapacity: (i, v, input) => request("POST", fill(PATHS.capacity, id(i)), { body: input, ifMatch: v }),
    roster: (s) => items(fill(PATHS.enrolments, id(s))),
    seat: (s, input) => request("POST", fill(PATHS.enrolments, id(s)), { body: input, idempotent: true }),
    withdrawSeat: (e, v, reason) => request("POST", fill(PATHS.withdraw, id(e)), { body: { reason }, ifMatch: v }),
    waitlist: (o) => items(fill(PATHS.waitlist, id(o))),
    promote: (o) => request("POST", fill(PATHS.promote, id(o)), { idempotent: true }),
    listAssignments: (s) => items(fill(PATHS.assignments, id(s))),
    createAssignment: (s, input) => request("POST", fill(PATHS.assignments, id(s)), { body: input, idempotent: true }),
    assignmentCommand: (a, v, cmd, reason) => request("POST", fill(ASSIGNMENT_CMD[cmd], id(a)), { body: reasonBody(reason), ifMatch: v }),
    decideAssignment: (a, v, decision, reason) => request("POST", fill(PATHS.assignmentDecision, id(a)), { body: { decision, reason }, ifMatch: v }),
    listPolicies: (i) => items(fill(PATHS.policies, id(i))),
    createPolicy: (i, input) => request("POST", fill(PATHS.policies, id(i)), { body: input, idempotent: true }),
    submitPolicy: (p, v) => request("POST", fill(PATHS.policySubmit, id(p)), { ifMatch: v }),
    reviewPolicy: (p, v, outcome, notes) => request("POST", fill(PATHS.policyReview, id(p)), { body: { outcome, notes }, ifMatch: v }),
    myCourses: () => items(PATHS.myCourses),
    availableCourses: () => items(PATHS.available),
    enrolMe: (sectionId) => request("POST", PATHS.myEnrolments, { body: { sectionId }, idempotent: true }),
    dropMine: (e, v) => request("POST", fill(PATHS.myDrop, id(e)), { ifMatch: v }),
    joinWaitlist: (offeringId) => request("POST", PATHS.myWaitlist, { body: { offeringId }, idempotent: true }),
    myClasses: () => items(PATHS.myClasses),
    myStudents: () => items(PATHS.myStudents),
  };
}
