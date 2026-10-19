/**
 * In-memory `RecordApi` for `mock` mode and the 3A screen tests. It mirrors the server rules the
 * screens depend on (admission state machine → 409, `If-Match` → 412, one live context, the
 * mock student `dev|aarav.desai@sutra.dev` for self-service), not every detail.
 */
import { ApiProblem } from "@sutra/api-client";
import { checkDocumentFile, contentTypeOf, type AttachedFile, type DownloadedFile } from "./files";
import type {
  Admission,
  AdmissionApplication,
  AdmissionDocument,
  AdmissionHistoryEntry,
  AdmissionStatus,
  DocumentRequest,
  IdentityCorrection,
  RecordApi,
  RecordHistoryEntry,
  StatusHistoryEntry,
  Student,
  StudentContext,
  StudentDocument,
  StudentTransfer,
  Suspension,
} from "./api";

let seq = 0;
const uid = () => `01930000-0000-7000-8000-${String(++seq).padStart(12, "0")}`;
const now = () => new Date().toISOString();
const today = () => new Date().toISOString().slice(0, 10);
const plusDays = (n: number) => new Date(Date.now() + n * 86_400_000).toISOString().slice(0, 10);

function problem(status: number, detail: string): never {
  const errorCode = status === 409 ? "conflict" : status === 412 ? "precondition-failed" : status === 404 ? "not-found" : "validation-failed";
  throw new ApiProblem({ type: "about:blank", title: detail, status, detail, errorCode });
}

/** The upload answer of the mock (same shape as the API's `AttachedFile`). */
function mockAttached(documentId: string, version: number, f: { id: string; filename: string; contentType: string; sizeBytes: number }): AttachedFile {
  const at = now();
  return {
    documentId,
    documentVersion: version,
    file: { id: f.id, ownerType: "admission_document", ownerId: documentId, filename: f.filename, contentType: f.contentType, sizeBytes: f.sizeBytes, sha256: "", uploadedAt: at, retentionUntil: at, legalHold: false, version: 1 },
  };
}

function check<T extends { version: number }>(row: T | undefined, version: number, what: string): T {
  if (!row) problem(404, `${what} not found`);
  if (row.version !== version) problem(412, `${what} was changed by someone else — reload and try again`);
  return row;
}

const MOVES: Record<string, Partial<Record<AdmissionStatus, AdmissionStatus>>> = {
  "submit-documents": { initiated: "documents_submitted" },
  "financial-clearance": { approved: "financially_cleared" },
  confirm: { financially_cleared: "confirmed" },
  "create-student": { confirmed: "student_created" },
  cancel: { initiated: "cancelled", documents_submitted: "cancelled", verification_completed: "cancelled", approval_pending: "cancelled", approved: "cancelled", financially_cleared: "cancelled" },
  withdraw: { confirmed: "withdrawn" },
  expire: { initiated: "expired" },
};

/** The self-service persona of mock mode. */
export const MOCK_STUDENT_SUBJECT = "dev|aarav.desai@sutra.dev";

export function createMockRecordApi(): RecordApi {
  const applications = new Map<string, AdmissionApplication>();
  const admissions = new Map<string, Admission & { _history: AdmissionHistoryEntry[] }>();
  const admissionDocs = new Map<string, AdmissionDocument>();
  const students = new Map<string, Student & { _subject?: string | null }>();
  const docs = new Map<string, StudentDocument>();
  const requests = new Map<string, DocumentRequest>();
  const statusHistory: Array<StatusHistoryEntry & { studentId: string }> = [];
  const recordHistory: Array<RecordHistoryEntry & { studentId: string }> = [];
  const suspensions: Array<Suspension & { studentId: string }> = [];
  const transfers = new Map<string, StudentTransfer>();
  const corrections = new Map<string, IdentityCorrection>();
  // Part 2 · S4-docs: stored files by object id (mirrors the server rules of files.rs / validate.rs).
  const files = new Map<string, { blob: Blob; filename: string; contentType: string }>();
  const store = async (file: File) => {
    const hint = await checkDocumentFile(file);
    if (hint) throw new ApiProblem({ type: "about:blank", title: "Validation failed", status: 422, errorCode: "validation-failed", validationErrors: [{ field: "file", code: "type_not_allowed", message: hint }] });
    const id = uid();
    const contentType = contentTypeOf(file.name)!;
    files.set(id, { blob: file, filename: file.name.split(/[\\/]/).pop() ?? file.name, contentType });
    return { id, filename: file.name, contentType, sizeBytes: file.size };
  };
  const fetchFile = (objectId: string | null | undefined): DownloadedFile => {
    const f = objectId ? files.get(objectId) : undefined;
    if (!f) problem(404, "document file not found");
    return { blob: f.blob, filename: f.filename, contentType: f.contentType, sha256: null };
  };

  const copy = <T>(x: T): T => JSON.parse(JSON.stringify(x)) as T;
  const view = (s: Student): Student => {
    const live = s.contexts.find((c) => ["pending_activation", "active", "suspended"].includes(c.status)) ?? s.contexts[0] ?? null;
    const { _subject, ...rest } = s as Student & { _subject?: string | null };
    void _subject;
    return copy({ ...rest, currentContext: live });
  };
  const studentOf = (id: string) => students.get(id) ?? problem(404, "student not found");
  const live = (s: Student) => s.contexts.find((c) => ["pending_activation", "active", "suspended"].includes(c.status)) ?? problem(409, "the student has no live context");
  const persona = (s: Student, to: Student["status"], cause: string) => {
    statusHistory.push({ studentId: s.id, institutionId: s.contexts[0]?.institutionId ?? "", scope: "student", fromStatus: s.status, toStatus: to, causeEventType: cause, occurredAt: now() });
    s.status = to;
    s.version += 1;
    s.statusChangedAt = now();
  };
  const ctxStatus = (s: Student, c: StudentContext, to: StudentContext["status"], cause: string, reason?: string) => {
    statusHistory.push({ studentId: s.id, studentContextId: c.id, institutionId: c.institutionId, scope: "context", fromStatus: c.status, toStatus: to, reason, causeEventType: cause, occurredAt: now() });
    c.status = to;
    c.statusReason = reason ?? null;
    c.version += 1;
    c.statusChangedAt = now();
  };
  const admMove = (a: Admission & { _history: AdmissionHistoryEntry[] }, to: AdmissionStatus, reason?: string) => {
    a._history.push({ fromStatus: a.status, toStatus: to, reason: reason ?? null, occurredAt: now() });
    a.status = to;
    if (reason) a.statusReason = reason;
    a.version += 1;
    a.updatedAt = now();
  };
  const strip = (a: Admission & { _history?: AdmissionHistoryEntry[] }): Admission => {
    const { _history, ...rest } = a;
    void _history;
    return copy(rest);
  };
  const verifyComplete = (a: Admission) => {
    const live = [...admissionDocs.values()].filter((d) => d.admissionId === a.id && d.status !== "superseded");
    const okTypes = a.requiredDocuments.every((t) => live.some((d) => d.documentType === t && (d.status === "verified" || d.status === "waived")));
    return okTypes && !live.some((d) => d.status === "submitted");
  };
  const me = () => [...students.values()].find((s) => s._subject === MOCK_STUDENT_SUBJECT && s.status !== "archived") ?? problem(404, "student not found");
  const raise = (s: Student, documentType: string, channel: string, purpose?: string | null): DocumentRequest => {
    if ([...requests.values()].some((r) => r.studentId === s.id && r.documentType === documentType && (r.status === "requested" || r.status === "in_progress"))) problem(409, "an active request of this document type already exists");
    const c = s.contexts[0] ?? problem(409, "the student has no institution context");
    const r: DocumentRequest = { id: uid(), studentId: s.id, studentContextId: c.id, institutionId: c.institutionId, documentType, purpose: purpose ?? null, copies: 1, channel, status: "requested", version: 1, createdAt: now(), updatedAt: now() };
    requests.set(r.id, r);
    return copy(r);
  };

  const api: RecordApi = {
    async listApplications(inst) {
      return [...applications.values()].filter((a) => a.institutionId === inst).map(copy);
    },
    async createApplication(inst, input) {
      if (!input.givenName?.trim()) problem(422, "givenName is required");
      const a: AdmissionApplication = {
        id: uid(),
        institutionId: inst,
        programmeId: input.programmeId,
        givenName: input.givenName.trim(),
        familyName: input.familyName ?? null,
        displayName: [input.givenName, input.familyName].filter(Boolean).join(" "),
        email: input.email ?? null,
        subjectRef: input.subjectRef ?? null,
        status: "draft",
        version: 1,
        createdAt: now(),
        updatedAt: now(),
      };
      applications.set(a.id, a);
      return copy(a);
    },
    async applicationCommand(id, version, cmd, reason) {
      const a = check(applications.get(id), version, "application");
      const to = ({ submit: { draft: "submitted" }, review: { submitted: "under_review" }, reject: { submitted: "rejected", under_review: "rejected" }, withdraw: { draft: "withdrawn", submitted: "withdrawn", under_review: "withdrawn" } } as Record<string, Record<string, AdmissionApplication["status"]>>)[cmd]?.[a.status];
      if (!to) problem(409, `application is ${a.status}; it cannot ${cmd}`);
      if (cmd === "reject" && !reason) problem(422, "a reason is required");
      Object.assign(a, { status: to, statusReason: reason ?? null, version: a.version + 1, updatedAt: now() });
      return copy(a);
    },
    async offer(id, version, input) {
      const app = check(applications.get(id), version, "application");
      if (app.status !== "submitted" && app.status !== "under_review") problem(409, `application is ${app.status}; it cannot be offered`);
      const adm: Admission & { _history: AdmissionHistoryEntry[] } = {
        id: uid(),
        institutionId: app.institutionId,
        applicationId: app.id,
        programmeId: app.programmeId,
        curriculumVersionId: uid(),
        givenName: app.givenName,
        familyName: app.familyName,
        displayName: app.displayName,
        email: app.email,
        subjectRef: app.subjectRef,
        offerExpiresOn: input.offerExpiresOn ?? plusDays(14),
        requiredDocuments: input.requiredDocuments ?? ["identity_proof", "qualifying_marksheet"],
        enrolmentType: "regular",
        status: "initiated",
        version: 1,
        createdAt: now(),
        updatedAt: now(),
        _history: [{ toStatus: "initiated", occurredAt: now() }],
      };
      admissions.set(adm.id, adm);
      Object.assign(app, { status: "offered", admissionId: adm.id, version: app.version + 1, updatedAt: now() });
      return copy(app);
    },
    async listAdmissions(inst) {
      return [...admissions.values()].filter((a) => a.institutionId === inst).map(strip);
    },
    async admissionHistory(id) {
      return copy((admissions.get(id) ?? problem(404, "admission not found"))._history);
    },
    async admissionDocuments(id) {
      return [...admissionDocs.values()].filter((d) => d.admissionId === id).map(copy);
    },
    async addAdmissionDocument(id, input) {
      const a = admissions.get(id) ?? problem(404, "admission not found");
      if (a.status !== "initiated" && a.status !== "documents_submitted") problem(409, `admission is ${a.status}; documents can no longer be added`);
      for (const d of admissionDocs.values()) if (d.admissionId === id && d.documentType === input.documentType && d.status === "rejected") Object.assign(d, { status: "superseded", version: d.version + 1 });
      const d: AdmissionDocument = { id: uid(), admissionId: id, documentType: input.documentType, title: input.title, fileRef: input.fileRef ?? null, status: "submitted", version: 1, createdAt: now() };
      admissionDocs.set(d.id, d);
      return copy(d);
    },
    async verifyAdmissionDocument(id, documentId, version, outcome, note) {
      const a = admissions.get(id) ?? problem(404, "admission not found");
      if (a.status !== "documents_submitted") problem(409, `admission is ${a.status}; documents are verified after submission`);
      const d = check(admissionDocs.get(documentId), version, "document");
      if (outcome !== "verified" && !note) problem(422, "a note is required to reject or waive");
      Object.assign(d, { status: outcome, note: note ?? d.note, version: d.version + 1, verifiedAt: now() });
      if (verifyComplete(a)) admMove(a, "verification_completed");
      return copy(d);
    },
    async admissionCommand(id, version, cmd, reason) {
      const a = check(admissions.get(id), version, "admission");
      if (cmd === "accept") {
        if (a.status !== "initiated" || a.offerAcceptedAt) problem(409, "the offer cannot be accepted");
        Object.assign(a, { offerAcceptedAt: now(), version: a.version + 1 });
        return strip(a);
      }
      if (cmd === "submit-for-approval") {
        if (a.status !== "verification_completed") problem(409, `admission is ${a.status}; approval needs completed verification`);
        admMove(a, "approval_pending");
        a.processInstanceId = `noop:${a.id}`;
        admMove(a, "approved");
        Object.assign(a, { approvalDecision: "approve", approvedBy: "system:auto-approval" });
        return strip(a);
      }
      if (cmd === "submit-documents" && !a.offerAcceptedAt) problem(409, "the offer has not been accepted yet");
      if ((cmd === "cancel" || cmd === "withdraw") && !reason) problem(422, "a reason is required");
      const to = MOVES[cmd]?.[a.status];
      if (!to) problem(409, `admission is ${a.status}; ${cmd} is not possible`);
      if (cmd === "create-student") {
        const ctx: StudentContext = {
          id: uid(),
          institutionId: a.institutionId,
          programmeId: a.programmeId,
          curriculumVersionId: a.curriculumVersionId,
          currentTermNo: 1,
          enrolmentType: a.enrolmentType,
          admissionId: a.id,
          status: "pending_activation",
          statusChangedAt: now(),
          version: 1,
        };
        const existing = a.studentId ? students.get(a.studentId) : undefined;
        const s =
          existing ??
          ({
            id: uid(),
            studentNumber: `S${new Date().getFullYear()}${String(students.size + 1).padStart(5, "0")}`,
            displayName: a.displayName,
            givenName: a.givenName,
            familyName: a.familyName,
            primaryEmail: a.email,
            subjectLinked: !!a.subjectRef,
            status: "created",
            statusChangedAt: now(),
            originAdmissionId: a.id,
            version: 1,
            createdAt: now(),
            contexts: [],
            _subject: a.subjectRef,
          } as Student & { _subject?: string | null });
        s.contexts.unshift(ctx);
        students.set(s.id, s);
        if (s.status === "created" || s.status === "archived") persona(s, "activated", "sutraerp.student.student.activated");
        a.studentId = s.id;
        a.studentContextId = ctx.id;
        for (const d of admissionDocs.values())
          if (d.admissionId === a.id && (d.status === "verified" || d.status === "waived"))
            docs.set(uid(), { id: uid(), studentId: s.id, institutionId: a.institutionId, documentType: d.documentType, title: d.title, status: d.status, source: "admission", version: 1, createdAt: now(), documentObjectId: d.documentObjectId ?? null });
      }
      admMove(a, to, reason);
      return strip(a);
    },
    async decideAdmission(id, version, decision, reason) {
      const a = check(admissions.get(id), version, "admission");
      if (a.status !== "approval_pending") problem(409, `admission is ${a.status}; no approval is pending`);
      if (decision === "reject" && !reason) problem(422, "a reason is required");
      admMove(a, decision === "approve" ? "approved" : "cancelled", reason);
      a.approvalDecision = decision;
      return strip(a);
    },
    async revokeAdmission(id, version, reason, evidence) {
      const a = check(admissions.get(id), version, "admission");
      if (!["confirmed", "student_created", "completed"].includes(a.status)) problem(409, `admission is ${a.status}; it cannot be revoked`);
      if (!reason || !evidence) problem(422, "reason and evidence are required");
      admMove(a, "revoked", reason);
      const s = a.studentId ? students.get(a.studentId) : undefined;
      const c = s?.contexts.find((x) => x.id === a.studentContextId);
      if (s && c && ["pending_activation", "active", "suspended"].includes(c.status)) ctxStatus(s, c, "terminated", "sutraerp.student.admission.revoked", reason);
      return strip(a);
    },
    async readmit(id, reason) {
      const src = admissions.get(id) ?? problem(404, "admission not found");
      if (!["cancelled", "withdrawn", "revoked", "expired", "completed"].includes(src.status)) problem(409, `admission is ${src.status}; readmission starts from a closed admission`);
      if (!reason) problem(422, "a reason is required");
      const a = { ...copy(strip(src)), id: uid(), applicationId: null, previousAdmissionId: src.id, status: "initiated" as const, offerAcceptedAt: null, enrolmentType: src.studentId ? "readmission" : "regular", version: 1, _history: [{ toStatus: "initiated", reason, occurredAt: now() }] };
      admissions.set(a.id, a);
      return strip(a);
    },
    async expireOffers(inst) {
      const expired: string[] = [];
      for (const a of admissions.values())
        if (a.institutionId === inst && a.status === "initiated" && !a.offerAcceptedAt && a.offerExpiresOn && a.offerExpiresOn < today()) {
          admMove(a, "expired", "offer validity date passed without acceptance");
          expired.push(a.id);
        }
      return { asOf: today(), expired };
    },
    async directory(inst, q) {
      const needle = (q ?? "").trim().toLowerCase();
      return [...students.values()]
        .filter((s) => s.contexts.some((c) => c.institutionId === inst))
        .filter((s) => !needle || s.displayName.toLowerCase().includes(needle) || s.studentNumber.toLowerCase().includes(needle))
        .map((s) => ({ ...view(s), contexts: [] }));
    },
    async getStudent(id) {
      return view(studentOf(id));
    },
    async me() {
      return view(me());
    },
    async statusHistory(id) {
      return statusHistory.filter((h) => h.studentId === id).map(copy);
    },
    async recordHistory(id) {
      return recordHistory.filter((h) => h.studentId === id).map(copy);
    },
    async suspensions(id) {
      return suspensions.filter((x) => x.studentId === id).map(copy);
    },
    async enrol(id, v, input) {
      const s = studentOf(id);
      const c = check(s.contexts.find((x) => x.status === "pending_activation"), v, "student context");
      Object.assign(c, { enrolmentNumber: input.enrolmentNumber ?? `EN${new Date().getFullYear()}${String(seq).padStart(5, "0")}`, batchCode: input.batchCode ?? c.batchCode, sectionCode: input.sectionCode ?? c.sectionCode, rollNumber: input.rollNumber ?? c.rollNumber, enrolledOn: input.enrolledOn ?? today() });
      recordHistory.push({ studentId: id, studentContextId: c.id, kind: "enrolment", before: {}, after: { enrolmentNumber: c.enrolmentNumber }, occurredAt: now() });
      ctxStatus(s, c, "active", "sutraerp.student.student.enrolled");
      if (s.status === "activated") for (const to of ["enrolled", "academically_assigned", "active"] as const) persona(s, to, "sutraerp.student.student.enrolled");
      const a = c.admissionId ? admissions.get(c.admissionId) : undefined;
      if (a?.status === "student_created") admMove(a, "completed");
      return view(s);
    },
    async updateEnrolment(id, v, input) {
      const s = studentOf(id);
      const c = check(live(s), v, "student context");
      if (c.status !== "active") problem(409, `student context is ${c.status}; the student must be active`);
      const before = { rollNumber: c.rollNumber, enrolmentNumber: c.enrolmentNumber };
      Object.assign(c, { rollNumber: input.rollNumber ?? c.rollNumber, enrolmentNumber: input.enrolmentNumber ?? c.enrolmentNumber, enrolledOn: input.enrolledOn ?? c.enrolledOn, version: c.version + 1 });
      recordHistory.push({ studentId: id, studentContextId: c.id, kind: "enrolment", before, after: { rollNumber: c.rollNumber, enrolmentNumber: c.enrolmentNumber }, reason: input.reason ?? null, occurredAt: now() });
      return view(s);
    },
    async changeAssignment(id, v, input) {
      const s = studentOf(id);
      const c = check(live(s), v, "student context");
      if (c.status !== "active") problem(409, `student context is ${c.status}; the student must be active`);
      if (input.programmeId && input.programmeId !== c.programmeId) problem(422, "a programme change needs an approved programme migration");
      const before = { batchCode: c.batchCode, sectionCode: c.sectionCode };
      Object.assign(c, { batchCode: input.batchCode ?? c.batchCode, sectionCode: input.sectionCode ?? c.sectionCode, orgUnitId: input.orgUnitId ?? c.orgUnitId, version: c.version + 1 });
      recordHistory.push({ studentId: id, studentContextId: c.id, kind: "assignment", before, after: { batchCode: c.batchCode, sectionCode: c.sectionCode }, reason: input.reason ?? null, occurredAt: now() });
      return view(s);
    },
    async suspend(id, v, reason, category) {
      const s = studentOf(id);
      const c = check(live(s), v, "student context");
      if (c.status !== "active") problem(409, `student context is ${c.status}; the student must be active`);
      if (!reason.trim()) problem(422, "reason is required");
      ctxStatus(s, c, "suspended", "sutraerp.student.student.suspended", reason);
      if (s.status === "active") persona(s, "suspended", "sutraerp.student.student.suspended");
      suspensions.push({ studentId: id, id: uid(), studentContextId: c.id, category, reason, suspendedOn: today(), status: "active" });
      return view(s);
    },
    async restore(id, v, reason) {
      const s = studentOf(id);
      const c = check(live(s), v, "student context");
      if (c.status !== "suspended") problem(409, `student context is ${c.status}; only a suspended student is restored`);
      ctxStatus(s, c, "active", "sutraerp.student.student.restored", reason);
      if (s.status === "suspended") persona(s, "active", "sutraerp.student.student.restored");
      for (const x of suspensions) if (x.studentContextId === c.id && x.status === "active") Object.assign(x, { status: "restored", restoredOn: today(), restorationReason: reason });
      return view(s);
    },
    async archive(id, v, reason) {
      const s = check(students.get(id), v, "student");
      if (!reason.trim()) problem(422, "a reason is required");
      if (s.status === "archived") problem(409, "the student record is archived and read-only");
      if (s.contexts.some((c) => ["pending_activation", "active", "suspended"].includes(c.status))) problem(409, "the student still has a live context; complete the exit before archiving");
      persona(s, "archived", "sutraerp.student.student.archived");
      return view(s);
    },
    async updateProfile(id, v, input) {
      const s = check(students.get(id), v, "student");
      if (s.status === "archived") problem(409, "the student record is archived and read-only");
      if (input.primaryEmail && !input.primaryEmail.includes("@")) problem(422, "primaryEmail must be an e-mail address");
      Object.assign(s, { preferredName: input.preferredName ?? s.preferredName, primaryEmail: input.primaryEmail ?? s.primaryEmail, primaryPhone: input.primaryPhone ?? s.primaryPhone, version: s.version + 1 });
      recordHistory.push({ studentId: id, kind: "profile", before: {}, after: { preferredName: s.preferredName }, occurredAt: now() });
      return view(s);
    },
    async requestTransfer(id, input) {
      const s = studentOf(id);
      const c = live(s);
      const t: StudentTransfer = { id: uid(), studentId: id, kind: input.kind, fromContextId: c.id, fromInstitutionId: c.institutionId, toInstitutionId: input.toInstitutionId ?? c.institutionId, toSectionCode: input.toSectionCode ?? null, toBatchCode: input.toBatchCode ?? null, toProgrammeId: input.toProgrammeId ?? null, effectiveOn: input.effectiveOn ?? today(), reason: input.reason, status: "requested", version: 1, createdAt: now(), updatedAt: now() };
      transfers.set(t.id, t);
      return copy(t);
    },
    async submitTransfer(id, v) {
      const t = check(transfers.get(id), v, "transfer");
      if (t.status !== "requested") problem(409, `transfer is ${t.status}`);
      const s = studentOf(t.studentId);
      const c = live(s);
      if (t.kind === "intra_institute") {
        Object.assign(c, { sectionCode: t.toSectionCode ?? c.sectionCode, batchCode: t.toBatchCode ?? c.batchCode, version: c.version + 1 });
        t.toContextId = c.id;
      } else {
        ctxStatus(s, c, "transferred", "sutraerp.student.student.transferred", t.reason);
        const n: StudentContext = { id: uid(), institutionId: t.toInstitutionId, programmeId: t.toProgrammeId, enrolmentType: "transfer_in", predecessorContextId: c.id, currentTermNo: c.currentTermNo, status: "pending_activation", statusChangedAt: now(), version: 1 };
        s.contexts.unshift(n);
        t.toContextId = n.id;
      }
      Object.assign(t, { status: "completed", version: t.version + 2 });
      return copy(t);
    },
    async listTransfers(id) {
      return [...transfers.values()].filter((t) => t.studentId === id).map(copy);
    },
    async requestCorrection(id, changes, evidence) {
      const s = studentOf(id);
      if (!evidence.trim()) problem(422, "evidence is required");
      const ic: IdentityCorrection = { id: uid(), studentId: id, institutionId: s.contexts[0]?.institutionId ?? "", changes, evidence, status: "requested", version: 1, createdAt: now(), updatedAt: now() };
      corrections.set(ic.id, ic);
      return copy(ic);
    },
    async submitCorrection(id, v) {
      const ic = check(corrections.get(id), v, "identity correction");
      if (ic.status !== "requested") problem(409, `identity correction is ${ic.status}`);
      const s = studentOf(ic.studentId);
      ic.previous = { givenName: s.givenName, familyName: s.familyName ?? undefined };
      Object.assign(s, { givenName: ic.changes.givenName ?? s.givenName, familyName: ic.changes.familyName ?? s.familyName, dateOfBirth: ic.changes.dateOfBirth ?? s.dateOfBirth, version: s.version + 1 });
      s.displayName = [s.givenName, s.middleName, s.familyName].filter(Boolean).join(" ");
      Object.assign(ic, { status: "completed", version: ic.version + 3 });
      return copy(ic);
    },
    async documents(id) {
      return [...docs.values()].filter((d) => d.studentId === id).map(copy);
    },
    async addDocument(id, input) {
      studentOf(id);
      const d: StudentDocument = { id: uid(), studentId: id, documentType: input.documentType, title: input.title, fileRef: input.fileRef ?? null, status: "submitted", source: "staff", version: 1, createdAt: now() };
      docs.set(d.id, d);
      return copy(d);
    },
    async verifyDocument(_id, documentId, v, outcome, note) {
      const d = check(docs.get(documentId), v, "document");
      if (d.status !== "submitted") problem(409, `document is ${d.status}; it was already decided`);
      if (outcome !== "verified" && !note) problem(422, "a note is required unless verified");
      Object.assign(d, { status: outcome, note: note ?? d.note, version: d.version + 1 });
      return copy(d);
    },
    async studentRequests(id) {
      return [...requests.values()].filter((r) => r.studentId === id).map(copy);
    },
    async raiseRequest(id, input) {
      return raise(studentOf(id), input.documentType, "staff", input.purpose);
    },
    async requestCommand(id, v, cmd, reason) {
      const r = check(requests.get(id), v, "document request");
      const ok = cmd === "start" ? r.status === "requested" : r.status === "requested" || r.status === "in_progress";
      if (!ok) problem(409, `document request is ${r.status}`);
      if (cmd === "reject" && !reason) problem(422, "a reason is required");
      Object.assign(r, { status: cmd === "start" ? "in_progress" : cmd === "reject" ? "rejected" : "cancelled", statusReason: reason ?? r.statusReason, version: r.version + 1, updatedAt: now() });
      return copy(r);
    },
    async issueRequest(id, v, issuedReference) {
      const r = check(requests.get(id), v, "document request");
      if (r.status !== "requested" && r.status !== "in_progress") problem(409, `document request is ${r.status}`);
      Object.assign(r, { status: "issued", issuedReference, issuedOn: today(), version: r.version + 1, updatedAt: now() });
      const d: StudentDocument = { id: uid(), studentId: r.studentId, documentType: r.documentType, title: r.documentType.replace(/_/g, " "), fileRef: issuedReference, status: "verified", source: "request", version: 1, createdAt: now() };
      docs.set(d.id, d);
      return copy(r);
    },
    async myDocuments() {
      const s = me();
      return [...docs.values()].filter((d) => d.studentId === s.id).map(copy);
    },
    async myRequests() {
      const s = me();
      return [...requests.values()].filter((r) => r.studentId === s.id).map(copy);
    },
    async raiseMyRequest(input) {
      return raise(me(), input.documentType, "self_service", input.purpose);
    },
    async uploadAdmissionFile(id, documentId, file) {
      const a = admissions.get(id) ?? problem(404, "admission not found");
      const d = admissionDocs.get(documentId);
      if (!d || d.admissionId !== id) problem(404, "admission document not found");
      if (a.status !== "initiated" && a.status !== "documents_submitted") problem(409, `admission is ${a.status}; files can no longer be attached`);
      if (d.status !== "submitted") problem(409, `document is ${d.status}; a file is attached while the document awaits verification`);
      if (d.documentObjectId) problem(409, "a file is already attached; hand in a new document to replace it");
      const f = await store(file);
      Object.assign(d, { documentObjectId: f.id, version: d.version + 1 });
      return mockAttached(d.id, d.version, f);
    },
    async downloadAdmissionFile(id, documentId) {
      const d = admissionDocs.get(documentId);
      if (!d || d.admissionId !== id) problem(404, "admission document not found");
      return fetchFile(d.documentObjectId);
    },
    async uploadDocumentFile(studentId, documentId, file) {
      const d = [...docs.values()].find((x) => x.id === documentId && x.studentId === studentId) ?? problem(404, "student document not found");
      if (d.status !== "submitted") problem(409, `document is ${d.status}; a file is attached while the document awaits verification`);
      if (d.documentObjectId) problem(409, "a file is already attached; file a new document to replace it");
      const f = await store(file);
      Object.assign(d, { documentObjectId: f.id, version: d.version + 1 });
      return mockAttached(d.id, d.version, f);
    },
    async downloadDocumentFile(studentId, documentId) {
      const d = [...docs.values()].find((x) => x.id === documentId && x.studentId === studentId) ?? problem(404, "student document not found");
      return fetchFile(d.documentObjectId);
    },
    async downloadMyDocumentFile(documentId) {
      const s = me();
      const d = [...docs.values()].find((x) => x.id === documentId && x.studentId === s.id) ?? problem(404, "student document not found");
      return fetchFile(d.documentObjectId);
    },
    async cancelMyRequest(id, v) {
      const s = me();
      const r = check(requests.get(id), v, "document request");
      if (r.studentId !== s.id) problem(404, "document request not found");
      return api.requestCommand(id, v, "cancel");
    },
  };
  return api;
}

/**
 * A mock pre-filled with one active student (linked to the mock student login) in `institutionId`
 * — handy for previews and tests.
 */
export async function seededMockRecordApi(institutionId: string, programmeId = "01930000-0000-7000-8000-00000000beef"): Promise<{ api: RecordApi; studentId: string }> {
  const api = createMockRecordApi();
  let app = await api.createApplication(institutionId, { programmeId, givenName: "Aarav", familyName: "Desai", subjectRef: MOCK_STUDENT_SUBJECT });
  app = await api.applicationCommand(app.id, app.version, "submit");
  await api.offer(app.id, app.version, { requiredDocuments: ["identity_proof"] });
  let adm = (await api.listAdmissions(institutionId))[0]!;
  adm = await api.admissionCommand(adm.id, adm.version, "accept");
  const d = await api.addAdmissionDocument(adm.id, { documentType: "identity_proof", title: "Aadhaar" });
  adm = await api.admissionCommand(adm.id, adm.version, "submit-documents");
  await api.verifyAdmissionDocument(adm.id, d.id, d.version, "verified");
  adm = (await api.listAdmissions(institutionId))[0]!;
  adm = await api.admissionCommand(adm.id, adm.version, "submit-for-approval");
  adm = await api.admissionCommand(adm.id, adm.version, "financial-clearance");
  adm = await api.admissionCommand(adm.id, adm.version, "confirm");
  adm = await api.admissionCommand(adm.id, adm.version, "create-student");
  const s = await api.getStudent(adm.studentId!);
  await api.enrol(s.id, s.currentContext!.version, { batchCode: "2026", sectionCode: "A" });
  return { api, studentId: s.id };
}
