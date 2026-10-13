/**
 * In-memory slice-7A API for mock mode and tests: two staff records (one active with an active
 * engagement, one draft), one active position, an open requisition with a candidate and the
 * signed-in faculty member's own profile. Commands move statuses the way the API does (enough for
 * screens and tests; the API stays authoritative).
 */
import { ApiProblem } from "@sutra/api-client";
import type { WorkforceApi } from "./api";
import type { Assignment, Candidate, CertificateRequest, Engagement, MyProfile, OnboardingCase, Position, Requisition, Staff } from "./types";

const conflict = (title: string) =>
  new ApiProblem({ type: "about:blank", title, status: 409, errorCode: "conflict" } as ConstructorParameters<typeof ApiProblem>[0]);
const notFound = () => new ApiProblem({ type: "about:blank", title: "Not found", status: 404, errorCode: "not-found" } as ConstructorParameters<typeof ApiProblem>[0]);

let seq = 100;
const nextId = (p: string) => `${p}-${++seq}`;
const pad = (n: number) => String(n).padStart(6, "0");

export function createMockWorkforceApi(): WorkforceApi {
  const staff: Staff[] = [
    {
      id: "stf-1",
      institutionId: "inst-1",
      staffNumber: "EMP-000001",
      displayName: "Meera Shah",
      givenName: "Meera",
      familyName: "Shah",
      workEmail: "meera.shah@sutra.dev",
      staffCategory: "teaching",
      status: "active",
      subjectRef: "dev|meera.shah@sutra.dev",
      engagement: { id: "eng-1", engagementNumber: "ENG-000001", designation: "Assistant Professor", grade: "AP-1", employmentType: "permanent", staffCategory: "teaching", startDate: "2024-06-01", status: "active" },
      version: 3,
    },
    { id: "stf-2", institutionId: "inst-1", staffNumber: "EMP-000002", displayName: "Ravi Patil", givenName: "Ravi", familyName: "Patil", workEmail: "ravi.patil@sutra.dev", staffCategory: "non_teaching", status: "draft", engagement: null, version: 1 },
  ];
  const engagements: Engagement[] = [
    { id: "eng-1", staffId: "stf-1", staffNumber: "EMP-000001", displayName: "Meera Shah", engagementNumber: "ENG-000001", staffCategory: "teaching", employmentType: "permanent", designation: "Assistant Professor", grade: "AP-1", startDate: "2024-06-01", status: "active", pendingChange: null, version: 2 },
  ];
  const positions: Position[] = [{ id: "pos-1", code: "PROF-CS-01", title: "Professor of Computer Science", staffCategory: "teaching", allowsMultiple: false, status: "active", occupants: 1, version: 2 }];
  const assignments: Assignment[] = [
    { id: "asg-1", positionId: "pos-1", positionCode: "PROF-CS-01", engagementId: "eng-1", engagementNumber: "ENG-000001", staffNumber: "EMP-000001", displayName: "Meera Shah", isPrimary: true, startDate: "2024-06-01", status: "active", version: 2 },
  ];
  const requisitions: Requisition[] = [
    { id: "req-1", number: "REQ-000001", title: "Assistant Professor (CS)", staffCategory: "teaching", employmentType: "probation", designation: "Assistant Professor", openings: 1, filled: 0, status: "open", version: 3 },
  ];
  const candidates: Candidate[] = [{ id: "cand-1", requisitionId: "req-1", givenName: "Kavya", familyName: "Nair", email: "kavya.nair@example.org", status: "applied", version: 1 }];
  const cases: OnboardingCase[] = [];
  const certificates: CertificateRequest[] = [];

  const find = <T extends { id: string }>(list: T[], id: string): T => {
    const x = list.find((r) => r.id === id);
    if (!x) throw notFound();
    return x;
  };
  const move = <T extends { status: string; version: number }>(x: T, from: string[], to: string): T => {
    if (!from.includes(x.status)) throw conflict(`Cannot move from ${x.status} to ${to}`);
    x.status = to;
    x.version += 1;
    return { ...x };
  };
  const syncStaff = (e: Engagement) => {
    const s = staff.find((r) => r.id === e.staffId);
    if (s) s.engagement = { id: e.id, engagementNumber: e.engagementNumber, designation: e.designation, grade: e.grade, employmentType: e.employmentType, staffCategory: e.staffCategory, startDate: e.startDate, status: e.status };
  };

  return {
    staff: async (_i, status) => staff.filter((s) => !status || s.status === status).map((s) => ({ ...s })),
    staffRecord: async (i) => ({ ...find(staff, i) }),
    createStaff: async (i, input) => {
      if (input.workEmail && staff.some((s) => s.workEmail === input.workEmail)) throw conflict("Possible duplicate staff record");
      const s: Staff = {
        id: nextId("stf"),
        institutionId: i,
        staffNumber: `EMP-${pad(staff.length + 1)}`,
        displayName: [input.givenName, input.familyName].filter(Boolean).join(" "),
        givenName: input.givenName,
        familyName: input.familyName ?? null,
        workEmail: input.workEmail ?? null,
        subjectRef: input.subjectRef ?? null,
        staffCategory: input.staffCategory,
        status: "draft",
        engagement: null,
        version: 1,
      };
      staff.push(s);
      return { ...s };
    },
    staffCommand: async (i, _v, cmd) => {
      const s = find(staff, i);
      switch (cmd) {
        case "submit-verification":
          return move(s, ["draft"], "pending_verification");
        case "verify":
          return move(s, ["pending_verification"], "verified");
        case "activate":
          return move(s, ["verified"], "active");
        case "suspend":
          return move(s, ["active"], "suspended");
        case "restore":
          return move(s, ["suspended", "archived"], "active");
        case "archive":
          if (s.engagement && !["closed", "cancelled"].includes(s.engagement.status)) throw conflict("Close the engagement first");
          return move(s, ["active", "suspended"], "archived");
      }
    },
    engagements: async (s) => engagements.filter((e) => e.staffId === s).map((e) => ({ ...e })),
    createEngagement: async (sid, input) => {
      const s = find(staff, sid);
      if (engagements.some((e) => e.staffId === sid && !["closed", "cancelled"].includes(e.status))) throw conflict("The staff member already has a live engagement");
      const e: Engagement = {
        id: nextId("eng"),
        staffId: sid,
        staffNumber: s.staffNumber,
        displayName: s.displayName,
        engagementNumber: `ENG-${pad(engagements.length + 1)}`,
        staffCategory: s.staffCategory,
        employmentType: input.employmentType,
        designation: input.designation,
        grade: input.grade ?? null,
        startDate: input.startDate,
        probationEndDate: input.probationEndDate ?? null,
        status: "pending",
        pendingChange: null,
        version: 1,
      };
      engagements.push(e);
      syncStaff(e);
      return { ...e };
    },
    engagementCommand: async (i, _v, cmd) => {
      const e = find(engagements, i);
      let out: Engagement;
      if (cmd === "submit") {
        if (find(staff, e.staffId).status !== "active") throw conflict("Verify and activate the profile first");
        out = move(e, ["pending"], "active");
      } else if (cmd === "activate") out = move(e, ["pending"], "active");
      else if (cmd === "suspend") out = move(e, ["active"], "suspended");
      else if (cmd === "restore") out = move(e, ["suspended"], "active");
      else out = move(e, ["pending"], "cancelled");
      syncStaff(e);
      return out;
    },
    decideEngagement: async (i, _v, decision) => {
      const e = find(engagements, i);
      if (!e.pendingChange) throw conflict("Nothing awaits a decision");
      e.pendingChange = null;
      const out = decision === "approve" ? move(e, ["pending", "active"], "active") : { ...e };
      syncStaff(e);
      return out;
    },
    closeEngagement: async (i, _v, lastWorkingDate) => {
      const e = find(engagements, i);
      e.lastWorkingDate = lastWorkingDate;
      const out = move(e, ["active", "suspended"], "closed");
      syncStaff(e);
      return out;
    },
    setReporting: async (i, _v, reportsToStaffId) => {
      const e = find(engagements, i);
      e.reportsToStaffId = reportsToStaffId;
      e.version += 1;
      return { ...e };
    },
    positions: async () => positions.map((p) => ({ ...p })),
    createPosition: async (_i, input) => {
      const code = input.code.toUpperCase();
      if (positions.some((p) => p.code === code)) throw conflict(`Position code ${code} is already used`);
      const p: Position = { id: nextId("pos"), code, title: input.title, staffCategory: input.staffCategory ?? null, allowsMultiple: input.allowsMultiple ?? false, status: "draft", occupants: 0, version: 1 };
      positions.push(p);
      return { ...p };
    },
    positionCommand: async (i, _v, cmd) => {
      const p = find(positions, i);
      if (cmd === "activate") return move(p, ["draft", "frozen"], "active");
      if (cmd === "freeze") return move(p, ["active"], "frozen");
      if (p.occupants > 0) throw conflict("End the assignments first");
      return move(p, ["active", "frozen"], "retired");
    },
    assignments: async (p) => assignments.filter((a) => a.positionId === p).map((a) => ({ ...a })),
    assign: async (pid, input) => {
      const p = find(positions, pid);
      const e = find(engagements, input.engagementId);
      if (p.status !== "active") throw conflict(`Position ${p.code} is ${p.status}`);
      if (!p.allowsMultiple && p.occupants > 0) throw conflict(`Position ${p.code} is already occupied`);
      const a: Assignment = { id: nextId("asg"), positionId: pid, positionCode: p.code, engagementId: e.id, engagementNumber: e.engagementNumber, staffNumber: e.staffNumber, displayName: e.displayName, isPrimary: true, startDate: input.startDate, status: "active", version: 2 };
      assignments.push(a);
      p.occupants += 1;
      return { ...a };
    },
    endAssignment: async (i, _v, input) => {
      const a = find(assignments, i);
      a.endDate = input.endDate;
      a.endOutcome = input.outcome;
      const out = move(a, ["active"], "closed");
      const p = positions.find((x) => x.id === a.positionId);
      if (p) p.occupants = Math.max(0, p.occupants - 1);
      return out;
    },
    decideAssignment: async (i, _v, decision) => move(find(assignments, i), ["pending_approval"], decision === "approve" ? "active" : "archived"),
    requisitions: async () => requisitions.map((r) => ({ ...r })),
    createRequisition: async (_i, input) => {
      const r: Requisition = { id: nextId("req"), number: `REQ-${pad(requisitions.length + 1)}`, ...input, filled: 0, status: "draft", version: 1 };
      requisitions.push(r);
      return { ...r };
    },
    submitRequisition: async (i) => move(find(requisitions, i), ["draft"], "open"),
    decideRequisition: async (i, _v, decision) => move(find(requisitions, i), ["pending_approval"], decision === "approve" ? "open" : "draft"),
    closeRequisition: async (i) => {
      const r = find(requisitions, i);
      return move(r, ["draft", "pending_approval", "approved", "open"], r.filled > 0 ? "filled" : "cancelled");
    },
    candidates: async (r) => candidates.filter((c) => c.requisitionId === r).map((c) => ({ ...c })),
    addCandidate: async (rid, input) => {
      if (find(requisitions, rid).status !== "open") throw conflict("Candidates join open requisitions");
      const c: Candidate = { id: nextId("cand"), requisitionId: rid, givenName: input.givenName, familyName: input.familyName ?? null, email: input.email.toLowerCase(), status: "applied", version: 1 };
      candidates.push(c);
      return { ...c };
    },
    candidateCommand: async (i, _v, cmd) => {
      const c = find(candidates, i);
      const to = { shortlist: ["applied", "shortlisted"], interview: ["shortlisted", "interviewed"], accept: ["offered", "offer_accepted"], decline: ["offered", "offer_declined"], reject: ["", "rejected"] }[cmd];
      if (cmd === "reject") return move(c, ["applied", "shortlisted", "interviewed", "offered"], "rejected");
      return move(c, [to[0] ?? ""], to[1] ?? "");
    },
    offer: async (i, _v, proposedJoiningDate) => {
      const c = find(candidates, i);
      c.proposedJoiningDate = proposedJoiningDate;
      c.offeredDesignation = find(requisitions, c.requisitionId).designation;
      return move(c, ["shortlisted", "interviewed"], "offered");
    },
    onboardingCases: async () => cases.map((o) => ({ ...o })),
    startOnboarding: async (cid) => {
      const c = find(candidates, cid);
      if (c.status !== "offer_accepted") throw conflict("Onboarding follows an accepted offer");
      if (c.onboardingCaseId) throw conflict("The candidate already has an onboarding case");
      const o: OnboardingCase = { id: nextId("onb"), number: `ONB-${pad(cases.length + 1)}`, candidateId: cid, candidateName: [c.givenName, c.familyName].filter(Boolean).join(" "), joiningDate: c.proposedJoiningDate ?? "2026-11-02", documents: [], documentsVerified: false, status: "initiated", version: 1 };
      c.onboardingCaseId = o.id;
      cases.push(o);
      return { ...o };
    },
    onboardingDocuments: async (i, _v, documentType, reference) => {
      const o = find(cases, i);
      o.documents = [...o.documents, { documentType, reference }];
      o.documentsVerified = false;
      return move(o, ["initiated", "documentation_pending"], "verification_pending");
    },
    verifyOnboarding: async (i, _v, approved) => {
      const o = find(cases, i);
      if (o.status !== "verification_pending") throw conflict("Nothing to verify");
      if (approved) {
        o.documentsVerified = true;
        o.version += 1;
        return { ...o };
      }
      return move(o, ["verification_pending"], "documentation_pending");
    },
    onboardingReady: async (i) => {
      const o = find(cases, i);
      if (!o.documentsVerified) throw conflict("Documents are not verified yet");
      return move(o, ["verification_pending"], "ready_for_joining");
    },
    join: async (i) => {
      const o = find(cases, i);
      const out = move(o, ["ready_for_joining"], "joined");
      const s: Staff = { id: nextId("stf"), institutionId: "inst-1", staffNumber: `EMP-${pad(staff.length + 1)}`, displayName: o.candidateName, givenName: o.candidateName, staffCategory: "teaching", status: "active", engagement: null, version: 1 };
      staff.push(s);
      o.staffId = s.id;
      return { ...out, staffId: s.id };
    },
    myProfile: async () => {
      const s = staff[0];
      if (!s) return null;
      const profile: MyProfile = {
        staff: { ...s },
        engagement: s.engagement ?? null,
        faculty: { id: "fac-1", status: "active", eligibilityStatus: "approved", eligibilityValidUntil: "2029-06-30", version: 4 },
        qualifications: [{ id: "q-1", qualificationType: "doctorate", title: "PhD Computer Science", awardingBody: "IIT Bombay", yearOfCompletion: 2019, status: "verified", version: 2 }],
        documents: [{ id: "d-1", documentType: "identity", title: "Passport", status: "verified", validUntil: "2031-01-01", version: 2 }],
        certificateRequests: certificates.map((c) => ({ ...c })),
      };
      return profile;
    },
    requestCertificate: async (input) => {
      const c: CertificateRequest = { id: nextId("cert"), kind: input.kind, purpose: input.purpose, requestedVia: "self", status: "approved", version: 2 };
      certificates.unshift(c);
      return { ...c };
    },
  };
}
