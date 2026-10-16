/**
 * Approval steps of the Part 1 journey (`journey.e2e.mjs`) — REAL human decisions.
 *
 * The stack runs with SUTRA_APPROVAL_MODE=human (compose default, DEF-X-001 fixed): the command
 * that requests approval (submit / submit-for-approval / validate / authorise / request-approval
 * …) leaves the record PENDING. `approve(kind, id)` then
 *
 *   1. reads the record and asserts it is pending (an already-approved record means approvals
 *      are automatic again → the step fails, unless the record was decided earlier in the flow,
 *      `decidedEarlier`, e.g. the institution activation the harness decides itself);
 *   2. optionally (`selfCheck`) lets the MAKER try the decision and asserts 403 with nothing
 *      changed (maker-checker / separation of duties, DEF-X-002);
 *   3. POSTs the domain decision endpoint `{ decision: "approve", reason }` with the record's
 *      version AS THE CHECKER PERSONA for that kind (`registrar`, `finance_approver`,
 *      `exam_controller`; `administrator` where no dedicated checker role holds the decide
 *      permission — docs/readiness/APPROVAL_CONTROL_MATRIX.md);
 *   4. re-reads the record and asserts the approved state.
 *
 * Every decision is recorded in `DECISIONS` (kind, id, decider login, before/after state, the
 * maker's self-decision status) for the journey report.
 */

/** kind → read path, pending/approved states, the decision endpoint and the checker persona. */
export const KINDS = {
  institution_activation: {
    read: (id) => `/api/v1/institution/institutions/${id}`,
    approved: ["active"],
    decide: (id) => `/api/v1/institution/institutions/${id}/decision`,
    checker: "administrator", // institution.activation.decide: administrators only
  },
  offering: {
    read: (id) => `/api/v1/academic/offerings/${id}`,
    approved: ["approved", "published", "enrollment_open", "enrollment_closed", "active"],
    decide: (id) => `/api/v1/academic/offerings/${id}/decision`,
    checker: "administrator", // academic.offering.decide: administrators only (no checker role yet)
  },
  faculty_assignment: {
    read: (id) => `/api/v1/academic/faculty-assignments/${id}`,
    approved: ["approved", "active"],
    decide: (id) => `/api/v1/academic/faculty-assignments/${id}/decision`,
    // faculty assignment decide: administrators only; SoD (S3-appr) → a second administrator
    checker: "academic_admin",
  },
  admission: {
    read: (id) => `/api/v1/student/admissions/${id}`,
    approved: ["approved", "financially_cleared", "confirmed", "student_created", "completed"],
    decide: (id) => `/api/v1/student/admissions/${id}/decision`,
    checker: "registrar", // admission.decide
  },
  registration: {
    read: (id) => `/api/v1/student/registrations/${id}`,
    approved: ["registered", "finalised"],
    decide: (id) => `/api/v1/student/registrations/${id}/decision`,
    // student.registration.manage; SoD (S3-appr): never the requester → a second administrator
    // decides office registrations the administrator submitted (students' own: any admin)
    checker: "academic_admin",
  },
  enrolment_override: {
    read: (id) => `/api/v1/academic/enrolment-overrides/${id}`,
    approved: ["approved"],
    decide: (id) => `/api/v1/academic/enrolment-overrides/${id}/decision`,
    checker: "academic_admin", // academic.offering.decide, never the requester (DEF-P1-DEMO-006)
  },
  fee_structure: {
    read: (id) => `/api/v1/finance/fee-structures/${id}`,
    approved: ["approved", "published"],
    decide: (id) => `/api/v1/finance/fee-structures/${id}/decision`,
    checker: "finance_approver", // finance.approve
  },
  adjustment: {
    read: (id) => `/api/v1/finance/adjustments/${id}`,
    approved: ["posted"],
    decide: (id) => `/api/v1/finance/adjustments/${id}/decision`,
    checker: "finance_approver", // finance.approve
  },
  exam_policy: {
    read: (id) => `/api/v1/exams/exam-policies/${id}`,
    approved: ["approved", "active"],
    decide: (id) => `/api/v1/exams/exam-policies/${id}/decision`,
    checker: "exam_controller", // exam.approve
  },
  exam_session: {
    read: (id) => `/api/v1/exams/exam-sessions/${id}`,
    approved: ["approved", "scheduled", "active"],
    decide: (id) => `/api/v1/exams/exam-sessions/${id}/decision`,
    checker: "exam_controller",
  },
  exam_schedule: {
    read: (id) => `/api/v1/exams/exam-schedules/${id}`,
    approved: ["approved", "published"],
    decide: (id) => `/api/v1/exams/exam-schedules/${id}/decision`,
    checker: "exam_controller",
  },
  mark_sheet: {
    read: (id) => `/api/v1/exams/mark-sheets/${id}`,
    approved: ["approved", "finalised"],
    decide: (id) => `/api/v1/exams/mark-sheets/${id}/decision`,
    checker: "exam_controller",
  },
  result_batch: {
    read: (id) => `/api/v1/exams/result-batches/${id}`,
    approved: ["approved", "finalised"],
    decide: (id) => `/api/v1/exams/result-batches/${id}/decision`,
    checker: "exam_controller", // exam.result.publish
  },
  publication: {
    read: (id) => `/api/v1/exams/publications/${id}`,
    approved: ["authorised", "published"],
    decide: (id) => `/api/v1/exams/publications/${id}/decision`,
    checker: "exam_controller", // exam.result.publish
  },
};

/** Every decision the journey made, in order. */
export const DECISIONS = [];

/** Signed-in checker clients by persona (`administrator`, `registrar`, …); set by the journey. */
export const CHECKERS = {};

async function read(clients, path) {
  let last;
  for (const c of clients.filter(Boolean)) {
    const r = await c.raw("GET", path);
    if (r.status === 200) return r.json;
    last = `${c.login}: ${r.status} ${r.text.slice(0, 200)}`;
  }
  throw new Error(`approve: cannot read ${path} (${last})`);
}

/**
 * Makes the real approval decision on `kind`/`id` and returns the approved record.
 *
 * `client` = the maker (or any signed-in reader); `selfCheck: true` asserts that the maker's own
 * decision is refused with 403 and changes nothing; `decidedEarlier: true` accepts a record that
 * an earlier step already decided (only asserted, recorded as such); `step` names the journey
 * step for the report; `ev` receives evidence lines.
 */
export async function approve(kind, id, { client, step = "", selfCheck = false, decidedEarlier = false, reason, ev = () => {} } = {}) {
  const k = KINDS[kind];
  if (!k) throw new Error(`approve: unknown kind ${kind}`);
  if (!client) throw new Error("approve: a signed-in client is required");
  const checkerName = k.checker;
  const checker = CHECKERS[checkerName];
  const rec0 = await read([checker, client], k.read(id));
  const entry = { kind, id, step, before: rec0?.status, decider: null, selfDecision: null, after: null, earlier: false };
  DECISIONS.push(entry);
  if (k.approved.includes(rec0?.status)) {
    if (!decidedEarlier)
      throw new Error(`approve(${kind}, ${id}): already “${rec0.status}” before any decision — approvals are automatic again (expected a pending record in human mode, DEF-X-001)`);
    entry.earlier = true;
    entry.after = rec0.status;
    ev(`approve(${kind}): already ${rec0.status} — decided earlier in the flow`);
    return rec0;
  }
  if (!checker) throw new Error(`approve(${kind}): checker persona “${checkerName}” is not signed in`);
  ev(`approve(${kind}): ${rec0.status} (v${rec0.version}) — pending a human decision`);

  if (selfCheck) {
    const r = await client.raw("POST", k.decide(id), { decision: "approve", reason: "maker approving own request (must be refused)" }, rec0.version);
    entry.selfDecision = r.status;
    ev(`maker ${client.login} decides own ${kind} → ${r.status} ${r.json?.detail ?? r.json?.title ?? ""}`);
    if (r.status !== 403) throw new Error(`approve(${kind}): the maker's self-decision answered ${r.status}, expected 403 (maker-checker, DEF-X-002) ${r.text.slice(0, 300)}`);
    const again = await read([checker, client], k.read(id));
    if (again.status !== rec0.status || again.version !== rec0.version)
      throw new Error(`approve(${kind}): refused self-decision changed the record (${rec0.status} v${rec0.version} → ${again.status} v${again.version})`);
    ev(`✓ self-decision refused (403), ${kind} unchanged (${again.status} v${again.version})`);
  }

  if (checker.login === client.login && selfCheck) throw new Error(`approve(${kind}): checker and maker are the same login ${client.login}`);
  const r = await checker.raw("POST", k.decide(id), { decision: "approve", reason: reason ?? `Approved by ${checkerName} in the Part 1 journey` }, rec0.version);
  entry.decider = checker.login;
  ev(`${checkerName} ${checker.login} POST ${k.decide(id).replace(id, "{id}")} approve → ${r.status} ${r.json?.status ?? r.json?.detail ?? ""}`);
  if (r.status >= 400) throw new Error(`approve(${kind}): decision by ${checker.login} → ${r.status} ${r.text.slice(0, 400)}`);
  const rec = await read([client, checker], k.read(id));
  entry.after = rec?.status;
  if (!k.approved.includes(rec?.status))
    throw new Error(`approve(${kind}, ${id}): after the decision expected ${k.approved.join(" | ")}, found “${rec?.status}”`);
  ev(`✓ ${kind} → ${rec.status} (decided by ${checker.login})`);
  return rec;
}
