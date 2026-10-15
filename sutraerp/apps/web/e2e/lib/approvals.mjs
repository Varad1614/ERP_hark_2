/**
 * Approval steps of the Part 1 journey (`journey.e2e.mjs`).
 *
 * TODAY (DEF-X-001, being fixed on part1/s2-appr): every governed decision is auto-approved by
 * the WorkflowPort, so the command that *requests* approval (submit / submit-for-approval /
 * validate / authorise …) already returns the record in its approved state. `approve(kind, id)`
 * therefore only READS the record and ASSERTS that state — it never decides anything itself.
 *
 * INTEGRATOR: once approvals stay pending, switch `approve` to a real human decision: sign in the
 * approving role (a different person from the requester — maker-checker, DEF-X-002), POST the
 * `decision` endpoint listed per kind below with `{ decision: "approve", reason }` and the
 * record's version, then assert the approved state the same way. Every call is recorded in
 * `AUTO_APPROVED` so the journey report lists exactly where the run depended on auto-approval.
 */

/** kind → how to read the record, which states count as "approved", and the real decision endpoint. */
export const KINDS = {
  institution_activation: {
    read: (id) => `/api/v1/institution/institutions/${id}`,
    approved: ["active"],
    decision: "POST /api/v1/institution/activation-requests/{id}/decision",
  },
  offering: {
    read: (id) => `/api/v1/academic/offerings/${id}`,
    approved: ["approved", "published", "enrollment_open", "enrollment_closed", "active"],
    decision: "POST /api/v1/academic/offerings/{id}/decision",
  },
  faculty_assignment: {
    read: (id) => `/api/v1/academic/faculty-assignments/${id}`,
    approved: ["approved", "active"],
    decision: "POST /api/v1/academic/faculty-assignments/{id}/decision",
  },
  admission: {
    read: (id) => `/api/v1/student/admissions/${id}`,
    approved: ["approved", "financially_cleared", "confirmed", "student_created", "completed"],
    decision: "POST /api/v1/student/admissions/{id}/decision",
  },
  registration: {
    read: (id) => `/api/v1/student/me/registrations/${id}`,
    approved: ["registered", "finalised"],
    decision: "POST /api/v1/student/registrations/{id}/decision",
  },
  fee_structure: {
    read: (id) => `/api/v1/finance/fee-structures/${id}`,
    approved: ["approved", "published"],
    decision: "POST /api/v1/finance/fee-structures/{id}/decision",
  },
  exam_policy: {
    read: (id) => `/api/v1/exams/exam-policies/${id}`,
    approved: ["approved", "active"],
    decision: "POST /api/v1/exams/exam-policies/{id}/decision",
  },
  exam_session: {
    read: (id) => `/api/v1/exams/exam-sessions/${id}`,
    approved: ["approved", "scheduled", "active"],
    decision: "POST /api/v1/exams/exam-sessions/{id}/decision",
  },
  exam_schedule: {
    read: (id) => `/api/v1/exams/exam-schedules/${id}`,
    approved: ["approved", "published"],
    decision: "POST /api/v1/exams/exam-schedules/{id}/decision",
  },
  mark_sheet: {
    read: (id) => `/api/v1/exams/mark-sheets/${id}`,
    approved: ["approved", "finalised"],
    decision: "POST /api/v1/exams/mark-sheets/{id}/decision",
  },
  result_batch: {
    read: (id) => `/api/v1/exams/result-batches/${id}`,
    approved: ["approved", "finalised"],
    decision: "POST /api/v1/exams/result-batches/{id}/decision",
  },
  publication: {
    read: (id) => `/api/v1/exams/publications/${id}`,
    approved: ["authorised", "published"],
    decision: "POST /api/v1/exams/publications/{id}/decision",
  },
};

/** Every approval the journey went through, in order: { kind, id, status, step }. */
export const AUTO_APPROVED = [];

/**
 * Asserts that the governed record `kind`/`id` is in an approved state (auto-approval today) and
 * returns the record. `client` = a signed-in API client that may read it (lib/auth.mjs); `step` =
 * the journey step, for the report. Throws when the record is not approved.
 */
export async function approve(kind, id, { client, step = "" } = {}) {
  const k = KINDS[kind];
  if (!k) throw new Error(`approve: unknown kind ${kind}`);
  if (!client) throw new Error("approve: a signed-in client is required");
  const rec = await client.get(k.read(id));
  const status = rec?.status;
  AUTO_APPROVED.push({ kind, id, status, step, decision: k.decision });
  if (!k.approved.includes(status))
    throw new Error(`approve(${kind}, ${id}): expected an approved state (${k.approved.join(" | ")}), found “${status}” — approvals are no longer automatic? Switch approve() to a real decision (${k.decision}).`);
  return rec;
}
