/**
 * In-memory `C3Api` for `mock` mode and the screen tests. It mirrors the transitions the 3C
 * screens depend on (auto-approving WorkflowPort, 409 on illegal transitions, 412 on stale
 * versions), not every server rule.
 */
import { ApiProblem } from "@sutra/api-client";
import type { Body, C3Api } from "./api";
import type {
  AlumniTransition,
  Clearance,
  ConferredQualification,
  EmergencyContact,
  Guardian,
  GuardianRelationship,
  PickupAuthorisation,
  QualificationCompletion,
  Scholarship,
  ScholarshipEligibility,
  ScholarshipPolicy,
  StudentExit,
} from "./types";

let seq = 0;
const uid = () => `01930000-0000-7000-8000-${String(++seq).padStart(12, "0")}`;
const today = () => new Date().toISOString().slice(0, 10);

function fail(status: number, detail: string): never {
  throw new ApiProblem({ type: "about:blank", title: detail, status, detail, errorCode: status === 409 ? "conflict" : status === 412 ? "precondition-failed" : "validation-failed" });
}

function check<T extends { version: number }>(row: T | undefined, version: number): T {
  if (!row) fail(404, "Not found");
  if (row.version !== version) fail(412, "The record changed; reload and try again");
  return row;
}

function bump<T extends { version: number }>(row: T, patch: Partial<T>): T {
  Object.assign(row, patch, { version: row.version + 1 });
  return { ...row };
}

const clearances = (items: string[]): Clearance[] =>
  items.map((item) => ({ id: uid(), item: item as Clearance["item"], status: item === "finance" ? "cleared" : "pending", note: null, version: 1 }));

export function createMockC3Api(): C3Api {
  const completions: QualificationCompletion[] = [];
  const conferred: (ConferredQualification & { studentId: string })[] = [];
  const exits: (StudentExit & { institutionId: string })[] = [];
  const guardians: (Guardian & { institutionId: string })[] = [];
  const relationships: GuardianRelationship[] = [];
  const contacts: (EmergencyContact & { studentId: string })[] = [];
  const pickups: (PickupAuthorisation & { studentId: string })[] = [];
  const scholarships: (Scholarship & { institutionId: string })[] = [];
  const policies: (ScholarshipPolicy & { scholarshipId: string })[] = [];
  const eligibilities: ScholarshipEligibility[] = [];
  const alumni: (AlumniTransition & { studentId: string })[] = [];

  const settleExit = (e: StudentExit) => {
    if (e.status === "clearance_pending" && e.clearances.every((c) => c.status === "cleared" || c.status === "waived")) e.status = "approved";
  };
  const settleCompletion = (c: QualificationCompletion) => {
    if (c.status === "institutional_clearance" && c.clearances.every((x) => x.status === "cleared" || x.status === "waived")) c.status = "approved";
  };

  return {
    listCompletions: async (sid) => completions.filter((c) => c.studentId === sid).map((c) => ({ ...c })),
    createCompletion: async (sid, body) => {
      if (completions.some((c) => c.studentId === sid && !["cancelled", "exit_completed"].includes(c.status))) fail(409, "An open completion exists");
      const c: QualificationCompletion = {
        id: uid(),
        studentId: sid,
        outcome: String(body.outcome ?? "degree"),
        pathway: body.pathway === "meme_exit" ? "meme_exit" : "programme_completion",
        status: "eligibility_evaluation",
        clearances: [],
        version: 1,
        createdAt: new Date().toISOString(),
      };
      completions.push(c);
      return { ...c };
    },
    completionCommand: async (id, v, cmd, body: Body = {}) => {
      const c = check(completions.find((x) => x.id === id), v);
      const next: Partial<QualificationCompletion> = {};
      switch (cmd) {
        case "eligibility-evaluation":
          if (c.status !== "eligibility_evaluation") fail(409, "Not in eligibility evaluation");
          next.status = "degree_audit";
          next.eligibilityResult = { eligible: true, unmet: [] };
          break;
        case "degree-audit":
          if (c.status !== "degree_audit") fail(409, "Not in degree audit");
          next.status = "institutional_clearance";
          next.clearances = clearances(["finance", "library", "department"]);
          break;
        case "award": {
          if (c.status !== "approved") fail(409, "The completion is not approved");
          const q = { id: uid(), studentId: c.studentId, title: `${c.outcome} conferred`, outcome: c.outcome, conferredOn: today(), status: "conferred" as const, version: 1 };
          conferred.push(q);
          next.status = "awarded";
          next.conferredQualificationId = q.id;
          next.conferredOn = q.conferredOn;
          break;
        }
        case "finalise":
          if (c.status !== "awarded") fail(409, "Not awarded");
          next.status = "record_finalized";
          break;
        case "complete-exit":
          if (c.status !== "record_finalized") fail(409, "The record is not finalised");
          next.status = "exit_completed";
          break;
        case "defer":
          if (!String(body.reason ?? "").trim()) fail(422, "Reason is required");
          next.status = "deferred";
          break;
        case "resume":
          if (c.status !== "deferred") fail(409, "Not deferred");
          next.status = "eligibility_evaluation";
          break;
        case "cancel":
          if (["awarded", "record_finalized", "exit_completed", "cancelled"].includes(c.status)) fail(409, "Cannot cancel");
          next.status = "cancelled";
          break;
        case "decision":
          if (c.status !== "approval_pending") fail(409, "No decision pending");
          next.status = body.decision === "approve" ? "approved" : "degree_audit";
          break;
      }
      return bump(c, next);
    },
    resolveCompletionClearance: async (id, v, cid, body) => {
      const c = check(completions.find((x) => x.id === id), v);
      const item = c.clearances.find((x) => x.id === cid);
      if (!item) fail(404, "Not found");
      if (item.item === "finance") fail(409, "Finance clearance comes from Finance");
      item.status = body.status as Clearance["status"];
      const r = bump(c, {});
      settleCompletion(c);
      return { ...c, version: r.version };
    },
    listConferred: async (sid) => conferred.filter((q) => q.studentId === sid).map((q) => ({ ...q })),
    listExits: async (inst) => exits.filter((e) => e.institutionId === inst).map((e) => ({ ...e })),
    listStudentExits: async (sid) => exits.filter((e) => e.studentId === sid).map((e) => ({ ...e })),
    createExit: async (sid, body) => {
      if (!body.reason?.trim()) fail(422, "Reason is required");
      if (exits.some((e) => e.studentId === sid && !["completed", "cancelled", "rejected"].includes(e.status))) fail(409, "An open exit exists");
      const e = {
        id: uid(),
        institutionId: "",
        studentId: sid,
        kind: body.kind,
        status: "clearance_pending" as StudentExit["status"],
        reason: body.reason,
        clearances: clearances(["finance", "library"]),
        version: 1,
        createdAt: new Date().toISOString(),
      };
      exits.push(e);
      return { ...e };
    },
    exitCommand: async (id, v, cmd, body: Body = {}) => {
      const e = check(exits.find((x) => x.id === id), v);
      if (cmd === "complete") {
        if (e.status !== "approved") fail(409, "The exit is not approved");
        return bump(e, { status: "completed", effectiveOn: today() });
      }
      if (cmd === "decision") {
        if (e.status !== "approval_pending") fail(409, "No decision pending");
        return bump(e, { status: body.decision === "approve" ? "approved" : "rejected" });
      }
      if (["completed", "cancelled", "rejected"].includes(e.status)) fail(409, "The exit is closed");
      return bump(e, { status: "cancelled" });
    },
    resolveExitClearance: async (id, v, cid, body) => {
      const e = check(exits.find((x) => x.id === id), v);
      const item = e.clearances.find((x) => x.id === cid);
      if (!item) fail(404, "Not found");
      if (item.item === "finance") fail(409, "Finance clearance comes from Finance");
      if (body.status === "waived" && !String(body.note ?? "").trim()) fail(422, "A waiver needs a note");
      item.status = body.status as Clearance["status"];
      e.version += 1;
      settleExit(e);
      return { ...e };
    },
    listGuardians: async (inst) => guardians.filter((g) => g.institutionId === inst).map((g) => ({ ...g })),
    createGuardian: async (inst, body) => {
      if (!body.phone && !body.email) fail(422, "A guardian needs a phone number or an e-mail address");
      const g = { id: uid(), institutionId: inst, givenName: String(body.givenName), familyName: (body.familyName as string) ?? null, email: (body.email as string) ?? null, phone: (body.phone as string) ?? null, version: 1 };
      guardians.push(g);
      return { ...g };
    },
    listRelationships: async (sid) => relationships.filter((r) => r.studentId === sid).map((r) => ({ ...r, guardian: guardians.find((g) => g.id === r.guardianId) ?? null })),
    proposeRelationship: async (sid, body) => {
      const r: GuardianRelationship = { id: uid(), guardianId: String(body.guardianId), studentId: sid, relationshipType: String(body.relationshipType), isPrimary: !!body.isPrimary, hasLegalCustody: false, status: "proposed", version: 1 };
      relationships.push(r);
      return { ...r };
    },
    relationshipCommand: async (id, v, cmd, body: Body = {}) => {
      const r = check(relationships.find((x) => x.id === id), v);
      const to: Record<string, [GuardianRelationship["status"][], GuardianRelationship["status"]]> = {
        "submit-verification": [["proposed"], "verified"],
        "verification-decision": [["pending_verification"], body.decision === "approve" ? "verified" : "proposed"],
        activate: [["verified"], "active"],
        suspend: [["active"], "suspended"],
        reinstate: [["suspended"], "active"],
        terminate: [["proposed", "pending_verification", "verified", "active", "suspended"], "terminated"],
      };
      if (cmd === "custody") return bump(r, { hasLegalCustody: !!body.hasLegalCustody });
      const [from, next] = to[cmd]!;
      if (!from.includes(r.status)) fail(409, `The relationship is ${r.status}`);
      return bump(r, { status: next });
    },
    listEmergencyContacts: async (sid) => contacts.filter((c) => c.studentId === sid).map((c) => ({ ...c })),
    addEmergencyContact: async (sid, body) => {
      const c = { id: uid(), studentId: sid, name: String(body.name ?? "Guardian"), relation: String(body.relation ?? "guardian"), phone: String(body.phone ?? ""), priority: Number(body.priority ?? 1), status: "active" as const, version: 1 };
      if (contacts.some((x) => x.studentId === sid && x.status === "active" && x.priority === c.priority)) fail(409, "Another contact has this priority");
      contacts.push(c);
      return { ...c };
    },
    removeEmergencyContact: async (id, v) => bump(check(contacts.find((x) => x.id === id), v), { status: "revoked" }),
    listPickups: async (sid) => pickups.filter((p) => p.studentId === sid).map((p) => ({ ...p })),
    grantPickup: async (sid, body) => {
      const p = { id: uid(), studentId: sid, personName: String(body.personName ?? "Guardian"), validUntil: (body.validUntil as string) ?? null, status: "active" as const, version: 1 };
      pickups.push(p);
      return { ...p };
    },
    revokePickup: async (id, v) => bump(check(pickups.find((x) => x.id === id), v), { status: "revoked" }),
    listScholarships: async (inst) =>
      scholarships.filter((s) => s.institutionId === inst).map((s) => {
        const p = policies.find((x) => x.scholarshipId === s.id && x.status === "effective");
        return { ...s, effectivePolicy: p ? { id: p.id, versionNo: p.versionNo } : null };
      }),
    createScholarship: async (inst, body) => {
      const code = String(body.code ?? "").toUpperCase();
      if (scholarships.some((s) => s.institutionId === inst && s.code === code)) fail(422, "A scholarship with this code exists");
      const s = { id: uid(), institutionId: inst, code, name: String(body.name), sponsorType: String(body.sponsorType), benefitType: String(body.benefitType), status: "draft" as Scholarship["status"], renewable: !!body.renewable, version: 1 };
      scholarships.push(s);
      return { ...s };
    },
    scholarshipCommand: async (id, v, cmd) => {
      const s = check(scholarships.find((x) => x.id === id), v);
      const next = ({ publish: "published", activate: "active", suspend: "suspended", expire: "expired", archive: "archived" } as const)[cmd];
      if (cmd === "activate" && !policies.some((p) => p.scholarshipId === id && p.status === "effective")) fail(409, "The scholarship needs an effective eligibility policy");
      if (cmd === "publish" && s.status !== "draft") fail(409, "Only drafts are published");
      return bump(s, { status: next });
    },
    listPolicies: async (sid) => policies.filter((p) => p.scholarshipId === sid).map((p) => ({ ...p })),
    createPolicy: async (sid) => {
      const p = { id: uid(), scholarshipId: sid, versionNo: policies.filter((x) => x.scholarshipId === sid).length + 1, status: "draft" as ScholarshipPolicy["status"], version: 1 };
      policies.push(p);
      return { ...p };
    },
    policyCommand: async (id, v, cmd) => {
      const p = check(policies.find((x) => x.id === id), v);
      if (cmd === "submit") {
        policies.filter((x) => x.scholarshipId === p.scholarshipId && x.status === "effective").forEach((x) => (x.status = "superseded"));
        return bump(p, { status: "effective" });
      }
      return bump(p, { status: cmd === "retire" ? "retired" : "effective" });
    },
    listEligibilities: async (sid) => eligibilities.filter((e) => e.studentId === sid).map((e) => ({ ...e })),
    determine: async (sid, body) => {
      const e: ScholarshipEligibility = { id: uid(), scholarshipId: String(body.scholarshipId), studentId: sid, status: "eligible", conditions: [], recommendationPending: false, version: 1 };
      eligibilities.push(e);
      return { ...e };
    },
    eligibilityCommand: async (id, v, cmd) => {
      const e = check(eligibilities.find((x) => x.id === id), v);
      const map: Partial<Record<string, [ScholarshipEligibility["status"][], ScholarshipEligibility["status"]]>> = {
        recommend: [["eligible"], "recommendation_generated"],
        "hand-off": [["recommendation_generated"], "active"],
        renew: [["active"], "active"],
        reassess: [["active"], "active"],
        expire: [["active", "eligible", "conditionally_eligible", "recommendation_generated"], "expired"],
        archive: [["expired", "not_eligible"], "archived"],
        "resolve-condition": [["conditionally_eligible"], "eligible"],
        "withdraw-recommendation": [["recommendation_generated"], "eligible"],
      };
      const step = map[cmd];
      if (!step || !step[0].includes(e.status)) fail(409, `The eligibility is ${e.status}`);
      return bump(e, { status: step[1] });
    },
    recalculate: async () => ({ id: uid(), evaluated: eligibilities.length, changed: 0 }),
    listAlumni: async (sid) => alumni.filter((a) => a.studentId === sid).map((a) => ({ ...a })),
    initiateAlumni: async (sid) => {
      if (!exits.some((e) => e.studentId === sid && e.status === "completed") && !completions.some((c) => c.studentId === sid && c.status === "exit_completed"))
        fail(409, "The student relationship has not concluded");
      const a = { id: uid(), studentId: sid, status: "pending_verification" as AlumniTransition["status"], version: 1 };
      alumni.push(a);
      return { ...a };
    },
    alumniCommand: async (id, v, cmd) => {
      const a = check(alumni.find((x) => x.id === id), v);
      const next = ({ "validate-eligibility": "verified", "validate-policy": "approved", decision: "approved", execute: "activated", complete: "completed", archive: "archived" } as const)[cmd];
      return bump(a, { status: next });
    },
  };
}
