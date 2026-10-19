/**
 * Slice 3C API layer (one interface, HTTP + in-memory mock — same pattern as the Batch 2
 * institution feature). Unsafe calls carry `X-CSRF-Token`; creates an `Idempotency-Key`;
 * commands an `If-Match` with the resource's `version`; errors become `ApiProblem`.
 */
import { ApiProblem, isProblem, isUnsafeMethod, newIdempotencyKey } from "@sutra/api-client";
import { getCsrfToken } from "../../../lib/api";
import type {
  AlumniTransition,
  ConferredQualification,
  EmergencyContact,
  Guardian,
  GuardianRelationship,
  Items,
  NewExit,
  PickupAuthorisation,
  QualificationCompletion,
  Recalculation,
  Scholarship,
  ScholarshipEligibility,
  ScholarshipPolicy,
  StudentExit,
} from "./types";

export type Body = Record<string, unknown>;
export type CompletionCommand =
  | "eligibility-evaluation"
  | "degree-audit"
  | "award"
  | "finalise"
  | "complete-exit"
  | "defer"
  | "resume"
  | "cancel"
  | "decision";
export type ExitCommand = "complete" | "cancel" | "decision";
export type RelationshipCommand = "submit-verification" | "verification-decision" | "activate" | "suspend" | "reinstate" | "terminate" | "custody";
export type ScholarshipCommand = "publish" | "activate" | "suspend" | "expire" | "archive";
export type PolicyCommand = "submit" | "decision" | "retire";
export type EligibilityCommand =
  | "recommend"
  | "recommendation-decision"
  | "withdraw-recommendation"
  | "hand-off"
  | "resolve-condition"
  | "reassess"
  | "renew"
  | "expire"
  | "archive";
export type AlumniCommand = "validate-eligibility" | "validate-policy" | "decision" | "execute" | "complete" | "archive";

export interface C3Api {
  // completion + exits
  listCompletions(studentId: string): Promise<QualificationCompletion[]>;
  createCompletion(studentId: string, body: Body): Promise<QualificationCompletion>;
  completionCommand(id: string, version: number, cmd: CompletionCommand, body?: Body): Promise<QualificationCompletion>;
  resolveCompletionClearance(id: string, version: number, clearanceId: string, body: Body): Promise<QualificationCompletion>;
  listConferred(studentId: string): Promise<ConferredQualification[]>;
  listExits(institutionId: string): Promise<StudentExit[]>;
  listStudentExits(studentId: string): Promise<StudentExit[]>;
  createExit(studentId: string, body: NewExit): Promise<StudentExit>;
  exitCommand(id: string, version: number, cmd: ExitCommand, body?: Body): Promise<StudentExit>;
  resolveExitClearance(id: string, version: number, clearanceId: string, body: Body): Promise<StudentExit>;
  // guardians
  listGuardians(institutionId: string): Promise<Guardian[]>;
  createGuardian(institutionId: string, body: Body): Promise<Guardian>;
  listRelationships(studentId: string): Promise<GuardianRelationship[]>;
  proposeRelationship(studentId: string, body: Body): Promise<GuardianRelationship>;
  relationshipCommand(id: string, version: number, cmd: RelationshipCommand, body?: Body): Promise<GuardianRelationship>;
  listEmergencyContacts(studentId: string): Promise<EmergencyContact[]>;
  addEmergencyContact(studentId: string, body: Body): Promise<EmergencyContact>;
  removeEmergencyContact(id: string, version: number): Promise<EmergencyContact>;
  listPickups(studentId: string): Promise<PickupAuthorisation[]>;
  grantPickup(studentId: string, body: Body): Promise<PickupAuthorisation>;
  revokePickup(id: string, version: number): Promise<PickupAuthorisation>;
  // scholarships
  listScholarships(institutionId: string): Promise<Scholarship[]>;
  createScholarship(institutionId: string, body: Body): Promise<Scholarship>;
  scholarshipCommand(id: string, version: number, cmd: ScholarshipCommand, reason?: string): Promise<Scholarship>;
  listPolicies(scholarshipId: string): Promise<ScholarshipPolicy[]>;
  createPolicy(scholarshipId: string, body: Body): Promise<ScholarshipPolicy>;
  policyCommand(id: string, version: number, cmd: PolicyCommand, body?: Body): Promise<ScholarshipPolicy>;
  listEligibilities(studentId: string): Promise<ScholarshipEligibility[]>;
  determine(studentId: string, body: Body): Promise<ScholarshipEligibility>;
  eligibilityCommand(id: string, version: number, cmd: EligibilityCommand, body?: Body): Promise<ScholarshipEligibility>;
  recalculate(institutionId: string, body: Body): Promise<Recalculation>;
  // alumni
  listAlumni(studentId: string): Promise<AlumniTransition[]>;
  initiateAlumni(studentId: string, body?: Body): Promise<AlumniTransition>;
  alumniCommand(id: string, version: number, cmd: AlumniCommand, body?: Body): Promise<AlumniTransition>;
}

type Fetch = (input: string, init: RequestInit) => Promise<Response>;
const S = "/api/v1/student";

export function createHttpC3Api(fetchImpl: Fetch = (i, init) => fetch(i, init), baseUrl: string = import.meta.env.VITE_API_BASE ?? ""): C3Api {
  async function request<T>(method: string, path: string, opts: { body?: unknown; ifMatch?: number; idempotent?: boolean } = {}): Promise<T> {
    const headers: Record<string, string> = { Accept: "application/json" };
    const unsafe = isUnsafeMethod(method);
    if (unsafe) {
      headers["Content-Type"] = "application/json";
      const csrf = getCsrfToken();
      if (csrf) headers["X-CSRF-Token"] = csrf;
    }
    if (opts.ifMatch !== undefined) headers["If-Match"] = `"${opts.ifMatch}"`;
    if (opts.idempotent) headers["Idempotency-Key"] = newIdempotencyKey();
    const res = await fetchImpl(`${baseUrl}${S}${path}`, { method, headers, credentials: "include", body: unsafe ? JSON.stringify(opts.body ?? {}) : undefined });
    const text = await res.text();
    let data: unknown;
    try {
      data = text ? JSON.parse(text) : undefined;
    } catch {
      data = undefined;
    }
    if (!res.ok) {
      if (isProblem(data)) throw new ApiProblem(data);
      throw new ApiProblem({ type: "about:blank", title: `Request failed (${res.status})`, status: res.status, errorCode: `http-${res.status}` });
    }
    return data as T;
  }
  const items = async <T>(path: string) => (await request<Items<T>>("GET", path)).items;
  const create = <T>(path: string, body: unknown = {}) => request<T>("POST", path, { body, idempotent: true });
  const command = <T>(path: string, version: number, body: unknown = {}) => request<T>("POST", path, { body, ifMatch: version });
  const e = encodeURIComponent;
  return {
    listCompletions: (sid) => items(`/students/${e(sid)}/qualification-completions`),
    createCompletion: (sid, body) => create(`/students/${e(sid)}/qualification-completions`, body),
    completionCommand: (id, v, cmd, body) => command(`/qualification-completions/${e(id)}/${cmd}`, v, body),
    resolveCompletionClearance: (id, v, cid, body) => command(`/qualification-completions/${e(id)}/clearances/${e(cid)}/resolve`, v, body),
    listConferred: (sid) => items(`/students/${e(sid)}/conferred-qualifications`),
    listExits: (inst) => items(`/institutions/${e(inst)}/exits`),
    listStudentExits: (sid) => items(`/students/${e(sid)}/exits`),
    createExit: (sid, body) => create(`/students/${e(sid)}/exits`, body),
    exitCommand: (id, v, cmd, body) => command(`/exits/${e(id)}/${cmd}`, v, body),
    resolveExitClearance: (id, v, cid, body) => command(`/exits/${e(id)}/clearances/${e(cid)}/resolve`, v, body),
    listGuardians: (inst) => items(`/institutions/${e(inst)}/guardians`),
    createGuardian: (inst, body) => create(`/institutions/${e(inst)}/guardians`, body),
    listRelationships: (sid) => items(`/students/${e(sid)}/guardian-relationships`),
    proposeRelationship: (sid, body) => create(`/students/${e(sid)}/guardian-relationships`, body),
    relationshipCommand: (id, v, cmd, body) => command(`/guardian-relationships/${e(id)}/${cmd}`, v, body),
    listEmergencyContacts: (sid) => items(`/students/${e(sid)}/emergency-contacts`),
    addEmergencyContact: (sid, body) => create(`/students/${e(sid)}/emergency-contacts`, body),
    removeEmergencyContact: (id, v) => command(`/emergency-contacts/${e(id)}/remove`, v),
    listPickups: (sid) => items(`/students/${e(sid)}/pickup-authorisations`),
    grantPickup: (sid, body) => create(`/students/${e(sid)}/pickup-authorisations`, body),
    revokePickup: (id, v) => command(`/pickup-authorisations/${e(id)}/revoke`, v),
    listScholarships: (inst) => items(`/institutions/${e(inst)}/scholarships`),
    createScholarship: (inst, body) => create(`/institutions/${e(inst)}/scholarships`, body),
    scholarshipCommand: (id, v, cmd, reason) => command(`/scholarships/${e(id)}/${cmd}`, v, reason ? { reason } : {}),
    listPolicies: (id) => items(`/scholarships/${e(id)}/policies`),
    createPolicy: (id, body) => create(`/scholarships/${e(id)}/policies`, body),
    policyCommand: (id, v, cmd, body) => command(`/scholarship-policies/${e(id)}/${cmd}`, v, body),
    listEligibilities: (sid) => items(`/students/${e(sid)}/scholarship-eligibilities`),
    determine: (sid, body) => create(`/students/${e(sid)}/scholarship-eligibilities`, body),
    eligibilityCommand: (id, v, cmd, body) => command(`/scholarship-eligibilities/${e(id)}/${cmd}`, v, body),
    recalculate: (inst, body) => create(`/institutions/${e(inst)}/scholarship-recalculations`, body),
    listAlumni: (sid) => items(`/students/${e(sid)}/alumni-transitions`),
    initiateAlumni: (sid, body) => create(`/students/${e(sid)}/alumni-transitions`, body ?? {}),
    alumniCommand: (id, v, cmd, body) => command(`/alumni-transitions/${e(id)}/${cmd}`, v, body),
  };
}
