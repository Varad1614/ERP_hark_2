/**
 * Slice 3A web API layer (admissions + student record, contract §5.2): one interface, two
 * implementations — `createHttpRecordApi()` (sutra-api: CSRF on unsafe calls, `Idempotency-Key`
 * on creates, `If-Match` on updates and state commands, problem+json → `ApiProblem`) and
 * `createMockRecordApi()` (`./mock`, mock mode + screen tests).
 *
 * Every path template is checked against the generated OpenAPI `paths` (`satisfies`), and all
 * DTOs are the generated schemas, so a renamed member fails `tsc`.
 */
import { ApiProblem, isProblem, isUnsafeMethod, newIdempotencyKey, type components, type paths } from "@sutra/api-client";
import { getCsrfToken } from "../../../lib/api";
import { contentTypeOf, filenameOf, type AttachedFile, type DownloadedFile } from "./files";

type S = components["schemas"];

export type Student = S["Student"];
export type StudentContext = S["StudentContext"];
export type StudentStatus = S["StudentStatus"];
export type StudentDirectory = S["StudentDirectory"];
export type AdmissionApplication = S["AdmissionApplication"];
export type Admission = S["Admission"];
export type AdmissionStatus = S["AdmissionStatus"];
export type AdmissionDocument = S["AdmissionDocument"];
export type AdmissionHistoryEntry = S["AdmissionHistoryEntry"];
export type NewApplication = S["CreateApplication"];
export type StudentDocument = S["StudentDocument"];
export type DocumentRequest = S["DocumentRequest"];
export type NewDocumentRequest = S["NewDocumentRequest"];
export type StatusHistoryEntry = S["StudentStatusHistoryEntry"];
export type RecordHistoryEntry = S["StudentRecordHistoryEntry"];
export type CommunicationPreferences = S["CommunicationPreferences"];
export type StudentTransfer = S["StudentTransfer"];
export type NewTransfer = S["NewStudentTransfer"];
export type IdentityCorrection = S["IdentityCorrection"];
export type IdentityChanges = S["IdentityChanges"];
export type Suspension = S["StudentSuspensionEpisode"];
export type EnrolmentUpdate = S["StudentEnrolmentUpdate"];
export type AssignmentUpdate = S["StudentAssignmentUpdate"];
export type ProfileUpdate = S["StudentProfileUpdate"];
export type EnrolInput = S["StudentEnrolment"];

/** Admission Record state commands with an optional `{ reason }` body. */
export type AdmissionCommand =
  | "accept"
  | "submit-documents"
  | "submit-for-approval"
  | "financial-clearance"
  | "confirm"
  | "create-student"
  | "cancel"
  | "withdraw"
  | "expire";
export type ApplicationCommand = "submit" | "review" | "reject" | "withdraw";
export type DocumentRequestCommand = "start" | "reject" | "cancel";
export type Verification = "verified" | "rejected" | "waived";

const B = "/api/v1/student";

export const PATHS = {
  applications: `${B}/institutions/{id}/admission-applications`,
  application: `${B}/admission-applications/{id}`,
  appSubmit: `${B}/admission-applications/{id}/submit`,
  appReview: `${B}/admission-applications/{id}/review`,
  appOffer: `${B}/admission-applications/{id}/offer`,
  appReject: `${B}/admission-applications/{id}/reject`,
  appWithdraw: `${B}/admission-applications/{id}/withdraw`,
  admissions: `${B}/institutions/{id}/admissions`,
  expireOffers: `${B}/institutions/{id}/admissions/expire-offers`,
  admission: `${B}/admissions/{id}`,
  admissionHistory: `${B}/admissions/{id}/history`,
  admissionDocuments: `${B}/admissions/{id}/documents`,
  admissionDocumentVerify: `${B}/admissions/{id}/documents/{documentId}/verify`,
  accept: `${B}/admissions/{id}/accept`,
  submitDocuments: `${B}/admissions/{id}/submit-documents`,
  submitForApproval: `${B}/admissions/{id}/submit-for-approval`,
  decision: `${B}/admissions/{id}/decision`,
  financialClearance: `${B}/admissions/{id}/financial-clearance`,
  confirm: `${B}/admissions/{id}/confirm`,
  createStudent: `${B}/admissions/{id}/create-student`,
  cancel: `${B}/admissions/{id}/cancel`,
  withdraw: `${B}/admissions/{id}/withdraw`,
  revoke: `${B}/admissions/{id}/revoke`,
  expire: `${B}/admissions/{id}/expire`,
  readmit: `${B}/admissions/{id}/readmit`,
  directory: `${B}/institutions/{id}/students`,
  student: `${B}/students/{id}`,
  me: `${B}/me/student`,
  statusHistory: `${B}/students/{id}/status-history`,
  recordHistory: `${B}/students/{id}/record-history`,
  enrol: `${B}/students/{id}/enrol`,
  enrolment: `${B}/students/{id}/enrolment`,
  assignment: `${B}/students/{id}/assignment`,
  suspend: `${B}/students/{id}/suspend`,
  restore: `${B}/students/{id}/restore`,
  suspensions: `${B}/students/{id}/suspensions`,
  archive: `${B}/students/{id}/archive`,
  profile: `${B}/students/{id}/profile`,
  preferences: `${B}/students/{id}/communication-preferences`,
  myPreferences: `${B}/me/communication-preferences`,
  documents: `${B}/students/{id}/documents`,
  documentVerify: `${B}/students/{id}/documents/{documentId}/verify`,
  myDocuments: `${B}/me/documents`,
  // Part 2 · S4-docs stored files
  admissionDocumentFile: `${B}/admissions/{id}/documents/{documentId}/file`,
  documentFile: `${B}/students/{id}/documents/{documentId}/file`,
  myDocumentFile: `${B}/me/documents/{documentId}/file`,
  studentRequests: `${B}/students/{id}/document-requests`,
  institutionRequests: `${B}/institutions/{id}/document-requests`,
  myRequests: `${B}/me/document-requests`,
  myRequestCancel: `${B}/me/document-requests/{id}/cancel`,
  request: `${B}/document-requests/{id}`,
  requestStart: `${B}/document-requests/{id}/start`,
  requestIssue: `${B}/document-requests/{id}/issue`,
  requestReject: `${B}/document-requests/{id}/reject`,
  requestCancel: `${B}/document-requests/{id}/cancel`,
  corrections: `${B}/students/{id}/identity-corrections`,
  correctionSubmit: `${B}/identity-corrections/{id}/submit`,
  transfers: `${B}/students/{id}/transfers`,
  transferSubmit: `${B}/transfers/{id}/submit`,
} as const satisfies Record<string, keyof paths>;

export function fill(template: string, params: Record<string, string>): string {
  return template.replace(/\{(\w+)\}/g, (_, k: string) => {
    const v = params[k];
    if (v === undefined) throw new Error(`missing path parameter ${k}`);
    return encodeURIComponent(v);
  });
}

const ADMISSION_COMMAND_PATH: Record<AdmissionCommand, keyof typeof PATHS> = {
  accept: "accept",
  "submit-documents": "submitDocuments",
  "submit-for-approval": "submitForApproval",
  "financial-clearance": "financialClearance",
  confirm: "confirm",
  "create-student": "createStudent",
  cancel: "cancel",
  withdraw: "withdraw",
  expire: "expire",
};
const APPLICATION_COMMAND_PATH: Record<ApplicationCommand, keyof typeof PATHS> = {
  submit: "appSubmit",
  review: "appReview",
  reject: "appReject",
  withdraw: "appWithdraw",
};
const REQUEST_COMMAND_PATH: Record<DocumentRequestCommand, keyof typeof PATHS> = {
  start: "requestStart",
  reject: "requestReject",
  cancel: "requestCancel",
};

export interface RecordApi {
  // admissions
  listApplications(institutionId: string): Promise<AdmissionApplication[]>;
  createApplication(institutionId: string, input: NewApplication): Promise<AdmissionApplication>;
  applicationCommand(id: string, version: number, cmd: ApplicationCommand, reason?: string): Promise<AdmissionApplication>;
  offer(id: string, version: number, input: { offerExpiresOn?: string; requiredDocuments?: string[] }): Promise<AdmissionApplication>;
  listAdmissions(institutionId: string): Promise<Admission[]>;
  admissionHistory(id: string): Promise<AdmissionHistoryEntry[]>;
  admissionDocuments(id: string): Promise<AdmissionDocument[]>;
  addAdmissionDocument(id: string, input: { documentType: string; title: string; fileRef?: string }): Promise<AdmissionDocument>;
  verifyAdmissionDocument(id: string, documentId: string, version: number, outcome: Verification, note?: string): Promise<AdmissionDocument>;
  admissionCommand(id: string, version: number, cmd: AdmissionCommand, reason?: string): Promise<Admission>;
  decideAdmission(id: string, version: number, decision: "approve" | "reject", reason?: string): Promise<Admission>;
  revokeAdmission(id: string, version: number, reason: string, evidence: string): Promise<Admission>;
  readmit(id: string, reason: string): Promise<Admission>;
  expireOffers(institutionId: string): Promise<{ asOf: string; expired: string[] }>;
  // student record
  directory(institutionId: string, q?: string): Promise<Student[]>;
  getStudent(id: string): Promise<Student>;
  me(): Promise<Student>;
  statusHistory(id: string): Promise<StatusHistoryEntry[]>;
  recordHistory(id: string): Promise<RecordHistoryEntry[]>;
  suspensions(id: string): Promise<Suspension[]>;
  enrol(id: string, contextVersion: number, input: EnrolInput): Promise<Student>;
  updateEnrolment(id: string, contextVersion: number, input: EnrolmentUpdate): Promise<Student>;
  changeAssignment(id: string, contextVersion: number, input: AssignmentUpdate): Promise<Student>;
  suspend(id: string, contextVersion: number, reason: string, category: string): Promise<Student>;
  restore(id: string, contextVersion: number, reason: string): Promise<Student>;
  archive(id: string, version: number, reason: string): Promise<Student>;
  updateProfile(id: string, version: number, input: ProfileUpdate): Promise<Student>;
  requestTransfer(id: string, input: NewTransfer): Promise<StudentTransfer>;
  submitTransfer(id: string, version: number): Promise<StudentTransfer>;
  listTransfers(id: string): Promise<StudentTransfer[]>;
  requestCorrection(id: string, changes: IdentityChanges, evidence: string): Promise<IdentityCorrection>;
  submitCorrection(id: string, version: number): Promise<IdentityCorrection>;
  // documents
  documents(id: string): Promise<StudentDocument[]>;
  addDocument(id: string, input: { documentType: string; title: string; fileRef?: string }): Promise<StudentDocument>;
  verifyDocument(id: string, documentId: string, version: number, outcome: Verification, note?: string): Promise<StudentDocument>;
  studentRequests(id: string): Promise<DocumentRequest[]>;
  raiseRequest(id: string, input: NewDocumentRequest): Promise<DocumentRequest>;
  requestCommand(id: string, version: number, cmd: DocumentRequestCommand, reason?: string): Promise<DocumentRequest>;
  issueRequest(id: string, version: number, issuedReference: string): Promise<DocumentRequest>;
  myDocuments(): Promise<StudentDocument[]>;
  myRequests(): Promise<DocumentRequest[]>;
  raiseMyRequest(input: NewDocumentRequest): Promise<DocumentRequest>;
  cancelMyRequest(id: string, version: number): Promise<DocumentRequest>;
  // Part 2 · S4-docs: stored files (raw body with the file's own Content-Type, `?filename=`, no Idempotency-Key)
  uploadAdmissionFile(id: string, documentId: string, file: File): Promise<AttachedFile>;
  downloadAdmissionFile(id: string, documentId: string): Promise<DownloadedFile>;
  uploadDocumentFile(studentId: string, documentId: string, file: File): Promise<AttachedFile>;
  downloadDocumentFile(studentId: string, documentId: string): Promise<DownloadedFile>;
  downloadMyDocumentFile(documentId: string): Promise<DownloadedFile>;
}

type Fetch = (input: string, init: RequestInit) => Promise<Response>;
interface RequestOptions {
  body?: unknown;
  ifMatch?: number;
  idempotent?: boolean;
}

export function createHttpRecordApi(fetchImpl: Fetch = (i, init) => fetch(i, init), baseUrl: string = import.meta.env.VITE_API_BASE ?? ""): RecordApi {
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
  async function fail(res: Response): Promise<never> {
    const text = await res.text().catch(() => "");
    let data: unknown;
    try {
      data = text ? JSON.parse(text) : undefined;
    } catch {
      data = undefined;
    }
    if (isProblem(data)) throw new ApiProblem(data);
    throw new ApiProblem({ type: "about:blank", title: `Request failed (${res.status})`, status: res.status, errorCode: `http-${res.status}` });
  }
  /**
   * PUT of a document file: the raw file as the body with its own media type and `?filename=`.
   * No `Idempotency-Key` (S4-docs: one file per document, a retry is a 409; bodies above 1 MiB
   * must not carry one).
   */
  async function upload(path: string, file: File): Promise<AttachedFile> {
    const headers: Record<string, string> = { Accept: "application/json", "Content-Type": contentTypeOf(file.name) ?? (file.type || "application/octet-stream") };
    const csrf = getCsrfToken();
    if (csrf) headers["X-CSRF-Token"] = csrf;
    const res = await fetchImpl(`${baseUrl}${path}?filename=${encodeURIComponent(file.name)}`, { method: "PUT", headers, credentials: "include", body: file });
    if (!res.ok) return fail(res);
    return (await res.json()) as AttachedFile;
  }
  async function download(path: string): Promise<DownloadedFile> {
    const res = await fetchImpl(`${baseUrl}${path}`, { method: "GET", headers: { Accept: "application/pdf, image/png, image/jpeg, application/problem+json" }, credentials: "include" });
    if (!res.ok) return fail(res);
    const contentType = res.headers.get("content-type") ?? "application/octet-stream";
    return { blob: await res.blob(), filename: filenameOf(res.headers.get("content-disposition")), contentType, sha256: res.headers.get("x-document-sha256") };
  }
  const P = PATHS;
  const get = <T>(path: string) => request<T>("GET", path);
  const items = async <T>(path: string) => (await get<{ items: T[] }>(path)).items;
  const create = <T>(path: string, body: unknown) => request<T>("POST", path, { body, idempotent: true });
  const command = <T>(path: string, version: number, body: unknown = {}) => request<T>("POST", path, { body, ifMatch: version });
  const reasonBody = (reason?: string) => (reason ? { reason } : {});

  return {
    listApplications: (id) => items(fill(P.applications, { id })),
    createApplication: (id, input) => create(fill(P.applications, { id }), input),
    applicationCommand: (id, version, cmd, reason) => command(fill(P[APPLICATION_COMMAND_PATH[cmd]], { id }), version, reasonBody(reason)),
    offer: (id, version, input) => command(fill(P.appOffer, { id }), version, input),
    listAdmissions: (id) => items(fill(P.admissions, { id })),
    admissionHistory: (id) => items(fill(P.admissionHistory, { id })),
    admissionDocuments: (id) => items(fill(P.admissionDocuments, { id })),
    addAdmissionDocument: (id, input) => create(fill(P.admissionDocuments, { id }), input),
    verifyAdmissionDocument: (id, documentId, version, outcome, note) => command(fill(P.admissionDocumentVerify, { id, documentId }), version, { outcome, note }),
    admissionCommand: (id, version, cmd, reason) => command(fill(P[ADMISSION_COMMAND_PATH[cmd]], { id }), version, reasonBody(reason)),
    decideAdmission: (id, version, decision, reason) => command(fill(P.decision, { id }), version, { decision, reason }),
    revokeAdmission: (id, version, reason, evidence) => command(fill(P.revoke, { id }), version, { reason, evidence }),
    readmit: (id, reason) => create(fill(P.readmit, { id }), { reason }),
    expireOffers: (id) => request("POST", fill(P.expireOffers, { id })),
    directory: async (id, q) => (await get<StudentDirectory>(`${fill(P.directory, { id })}${q ? `?q=${encodeURIComponent(q)}` : ""}`)).items,
    getStudent: (id) => get(fill(P.student, { id })),
    me: () => get(P.me),
    statusHistory: (id) => items(fill(P.statusHistory, { id })),
    recordHistory: (id) => items(fill(P.recordHistory, { id })),
    suspensions: (id) => items(fill(P.suspensions, { id })),
    enrol: (id, v, input) => command(fill(P.enrol, { id }), v, input),
    updateEnrolment: (id, v, input) => request("PATCH", fill(P.enrolment, { id }), { body: input, ifMatch: v }),
    changeAssignment: (id, v, input) => request("PATCH", fill(P.assignment, { id }), { body: input, ifMatch: v }),
    suspend: (id, v, reason, category) => command(fill(P.suspend, { id }), v, { reason, category }),
    restore: (id, v, reason) => command(fill(P.restore, { id }), v, { reason }),
    archive: (id, v, reason) => command(fill(P.archive, { id }), v, { reason }),
    updateProfile: (id, v, input) => request("PATCH", fill(P.profile, { id }), { body: input, ifMatch: v }),
    requestTransfer: (id, input) => create(fill(P.transfers, { id }), input),
    submitTransfer: (id, v) => command(fill(P.transferSubmit, { id }), v),
    listTransfers: (id) => items(fill(P.transfers, { id })),
    requestCorrection: (id, changes, evidence) => create(fill(P.corrections, { id }), { changes, evidence }),
    submitCorrection: (id, v) => command(fill(P.correctionSubmit, { id }), v),
    documents: (id) => items(fill(P.documents, { id })),
    addDocument: (id, input) => create(fill(P.documents, { id }), input),
    verifyDocument: (id, documentId, v, outcome, note) => command(fill(P.documentVerify, { id, documentId }), v, { outcome, note }),
    studentRequests: (id) => items(fill(P.studentRequests, { id })),
    raiseRequest: (id, input) => create(fill(P.studentRequests, { id }), input),
    requestCommand: (id, v, cmd, reason) => command(fill(P[REQUEST_COMMAND_PATH[cmd]], { id }), v, reasonBody(reason)),
    issueRequest: (id, v, issuedReference) => command(fill(P.requestIssue, { id }), v, { issuedReference }),
    myDocuments: () => items(P.myDocuments),
    myRequests: () => items(P.myRequests),
    raiseMyRequest: (input) => create(P.myRequests, input),
    cancelMyRequest: (id, v) => command(fill(P.myRequestCancel, { id }), v),
    uploadAdmissionFile: (id, documentId, file) => upload(fill(P.admissionDocumentFile, { id, documentId }), file),
    downloadAdmissionFile: (id, documentId) => download(fill(P.admissionDocumentFile, { id, documentId })),
    uploadDocumentFile: (id, documentId, file) => upload(fill(P.documentFile, { id, documentId }), file),
    downloadDocumentFile: (id, documentId) => download(fill(P.documentFile, { id, documentId })),
    downloadMyDocumentFile: (documentId) => download(fill(P.myDocumentFile, { documentId })),
  };
}

/** Next Admission Record actions an administrator can take, by status (contract §4.2). */
export function admissionActions(a: Pick<Admission, "status" | "offerAcceptedAt">): Array<AdmissionCommand | "decide" | "revoke" | "readmit" | "documents"> {
  switch (a.status) {
    case "initiated":
      return a.offerAcceptedAt ? ["documents", "submit-documents", "cancel"] : ["accept", "documents", "cancel", "expire"];
    case "documents_submitted":
      return ["documents", "cancel"];
    case "verification_completed":
      return ["submit-for-approval", "cancel"];
    case "approval_pending":
      return ["decide"];
    case "approved":
      return ["financial-clearance", "cancel"];
    case "financially_cleared":
      return ["confirm", "cancel"];
    case "confirmed":
      return ["create-student", "withdraw", "revoke"];
    case "student_created":
    case "completed":
      return ["revoke"];
    case "cancelled":
    case "withdrawn":
    case "revoked":
    case "expired":
      return ["readmit"];
    default:
      return [];
  }
}

/** Short id for tables (`019a…3f2c`). */
export function shortId(id: string | null | undefined): string {
  if (!id) return "—";
  return `${id.slice(0, 4)}…${id.slice(-4)}`;
}
