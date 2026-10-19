/**
 * In-memory stand-in of the slice-4A API (mock mode, tests). Small but coherent: an active
 * `CS101` offering taught by Dr. Meera Shah with two seated students, a draft `MD101` offering,
 * one effective workload policy, the student's seat and the faculty member's class. Rules mirror
 * the API loosely (the API stays authoritative).
 */
import { ApiProblem } from "@sutra/api-client";
import type { OfferingApi } from "./api";
import type {
  AvailableCourse,
  CourseOfferingStatus,
  FacultyAssignment,
  FacultyAssignmentPolicy,
  MyStudent,
  Offering,
  OfferingCommand,
  OfferingSection,
  OfferingWaitlistEntry,
  SectionEnrolment,
} from "./types";

const INST = "00000000-0000-7000-8000-00000000b400";
const TERM = "00000000-0000-7000-8000-00000000b401";
const NOW = "2026-10-12T09:00:00Z";
const MEERA = "dev|meera.shah@sutra.dev";
let seq = 1;
const uid = () => `00000000-0000-7000-8000-${String(4000 + seq++).padStart(12, "0")}`;

function conflict(detail: string): never {
  throw new ApiProblem({ type: "about:blank", title: "Conflict", status: 409, detail, errorCode: "conflict" });
}

const NEXT: Record<OfferingCommand, [CourseOfferingStatus[], CourseOfferingStatus]> = {
  submit: [["draft"], "approved"],
  publish: [["approved"], "published"],
  "open-enrolment": [["published", "enrollment_closed"], "enrollment_open"],
  "close-enrolment": [["enrollment_open"], "enrollment_closed"],
  activate: [["enrollment_closed"], "active"],
  suspend: [["active"], "suspended"],
  reactivate: [["suspended"], "active"],
  freeze: [["active"], "frozen"],
  complete: [["active"], "completed"],
  archive: [["completed", "cancelled"], "archived"],
};

function offering(code: string, title: string, credits: number, status: CourseOfferingStatus, sections: OfferingSection[]): Offering {
  return {
    id: uid(), institutionId: INST, campusId: null, orgUnitId: null, academicYearId: null, academicTermId: TERM,
    curriculumVersionId: uid(), curriculumComponentId: uid(), offeringCode: code, title, courseCode: code.split("-")[0] ?? code,
    category: "major_core", credits, deliveryMode: "in_person", capacity: 60, seatsTaken: 0, waitlisted: 0, status,
    statusReason: null, statusChangedAt: NOW, processInstanceId: null, mergedIntoId: null, splitFromId: null, version: 1,
    createdAt: NOW, updatedAt: NOW, sections,
  };
}

function section(code: string, status: OfferingSection["status"], capacity: number): OfferingSection {
  return { id: uid(), offeringId: "", sectionCode: code, campusId: null, capacity, seatsTaken: 0, status, statusReason: null, mergedIntoId: null, version: 1, faculty: [] };
}

function seat(offeringId: string, sectionId: string, studentId: string, number: string, name: string, origin: string): SectionEnrolment {
  return {
    id: uid(), offeringId, sectionId, studentId, studentContextId: uid(), studentNumber: number, displayName: name, registrationId: null,
    registrationLineId: null, origin, status: "enrolled", statusReason: null, transferredFromId: null, enrolledAt: NOW, endedAt: null, version: 1,
  };
}

export function createMockOfferingApi(): OfferingApi {
  const cs = offering("CS101-T1", "Programming I", 12, "active", [section("A", "active", 60)]);
  const md = offering("MD101-T1", "Design thinking", 8, "draft", [section("A", "planned", 40)]);
  const offerings: Offering[] = [cs, md];
  for (const o of offerings) for (const s of o.sections) s.offeringId = o.id;
  const csA = cs.sections[0]!;
  const assignments: FacultyAssignment[] = [
    {
      id: uid(), offeringId: cs.id, sectionId: csA.id, facultySubjectRef: MEERA, facultyName: "Dr. Meera Shah", staffId: null,
      role: "primary_instructor", weeklyHours: 4, effectiveFrom: "2026-06-15", effectiveTo: null, status: "active", statusReason: null,
      processInstanceId: null, version: 3, createdAt: NOW,
    },
  ];
  const enrolments: SectionEnrolment[] = [
    seat(cs.id, csA.id, "s1", "S2026001", "Aarav Desai", "registration"),
    seat(cs.id, csA.id, "s2", "S2026002", "Bina Rao", "registration"),
  ];
  const waitlist: OfferingWaitlistEntry[] = [];
  const policies: FacultyAssignmentPolicy[] = [
    {
      id: uid(), institutionId: INST, name: "Workload norms", versionNo: 1, maxWeeklyHours: 16, allowedRoles: [], requirePrimaryInstructor: true,
      rationale: "UGC workload", reviewOutcome: "compatible", reviewNotes: null, status: "effective", statusReason: null, processInstanceId: null,
      supersededBy: null, effectiveAt: NOW, version: 3, createdAt: NOW,
    },
  ];

  const refresh = (o: Offering) => {
    for (const s of o.sections) {
      s.seatsTaken = enrolments.filter((e) => e.sectionId === s.id && e.status === "enrolled").length;
      s.faculty = assignments
        .filter((a) => a.sectionId === s.id && !["archived", "withdrawn", "rejected"].includes(a.status))
        .map((a) => ({ assignmentId: a.id, facultySubjectRef: a.facultySubjectRef, facultyName: a.facultyName, role: a.role, status: a.status }));
    }
    o.seatsTaken = enrolments.filter((e) => e.offeringId === o.id && e.status === "enrolled").length;
    o.waitlisted = waitlist.filter((w) => w.offeringId === o.id && (w.status === "waiting" || w.status === "offered")).length;
    return structuredClone(o);
  };
  const find = (id: string) => offerings.find((o) => o.id === id) ?? conflict("No such offering");
  const bump = (o: Offering, status?: CourseOfferingStatus, reason?: string) => {
    if (status) {
      o.status = status;
      o.statusReason = reason ?? null;
    }
    o.version += 1;
    return refresh(o);
  };
  const check = (v: number, actual: number) => {
    if (v !== actual) throw new ApiProblem({ type: "about:blank", title: "Precondition failed", status: 412, errorCode: "precondition-failed" });
  };
  const sectionOf = (id: string) => offerings.flatMap((o) => o.sections).find((s) => s.id === id) ?? conflict("No such section");
  const assignment = (id: string) => assignments.find((a) => a.id === id) ?? conflict("No such assignment");
  const policy = (id: string) => policies.find((x) => x.id === id) ?? conflict("No such policy");

  return {
    listOfferings: async () => offerings.map(refresh),
    getOffering: async (id) => refresh(find(id)),
    createOffering: async (_i, input) => {
      if (offerings.some((o) => o.offeringCode.toLowerCase() === input.offeringCode.toLowerCase())) conflict("An offering with this code is already live in the term");
      const o = offering(input.offeringCode, input.title ?? input.offeringCode, 4, "draft", (input.sections ?? []).map((s) => section(s.sectionCode, "planned", s.capacity ?? 0)));
      o.capacity = input.capacity ?? null;
      for (const s of o.sections) s.offeringId = o.id;
      offerings.push(o);
      return refresh(o);
    },
    offeringCommand: async (id, v, cmd, reason) => {
      const o = find(id);
      check(v, o.version);
      const [from, to] = NEXT[cmd];
      if (!from.includes(o.status)) conflict(`The course offering is ${o.status.replace(/_/g, " ")}`);
      if (cmd === "submit" && o.sections.length === 0) conflict("Add at least one section before submitting");
      if (cmd === "open-enrolment") for (const s of o.sections) if (s.status === "planned") s.status = "active";
      if (cmd === "activate" && o.sections.some((s) => s.status === "active" && !assignments.some((a) => a.sectionId === s.id && a.status === "active")))
        conflict("The course offering is not ready: a section has no active primary instructor");
      return bump(o, to, reason);
    },
    decideOffering: async (id, v, decision, reason) => {
      const o = find(id);
      check(v, o.version);
      if (o.status !== "pending_approval") conflict("Nothing awaits a decision");
      return bump(o, decision === "approve" ? "approved" : "draft", reason);
    },
    governed: async (id, v, cmd, reason) => {
      const o = find(id);
      check(v, o.version);
      if (cmd === "cancel" && !["draft", "approved", "published", "suspended"].includes(o.status)) conflict("Administrative cancellation is not possible");
      if (cmd === "unfreeze" && o.status !== "frozen") conflict("Only frozen offerings are unfrozen");
      const next = bump(o, cmd === "cancel" ? "cancelled" : "active", reason);
      return {
        offering: next,
        request: {
          id: uid(), offeringId: o.id, kind: cmd === "cancel" ? "cancellation" : "unfreeze", proposed: {}, baseline: null, snapshotAfter: null,
          targetRevisionId: null, restoreStatus: null, status: "applied", reason, decisionReason: "auto-approved", processInstanceId: "noop-mock",
          requestedBy: "dev|admin@sutra.dev", decidedBy: "system:auto-approval", appliedAt: NOW, version: 3, createdAt: NOW,
        },
      };
    },
    addSection: async (offeringId, input) => {
      const o = find(offeringId);
      if (o.sections.some((s) => s.sectionCode.toLowerCase() === input.sectionCode.toLowerCase())) conflict("A live section with this code exists");
      const s = section(input.sectionCode, "planned", input.capacity ?? 0);
      s.offeringId = o.id;
      o.sections.push(s);
      bump(o);
      return structuredClone(s);
    },
    changeCapacity: async (offeringId, v, input) => {
      const o = find(offeringId);
      check(v, o.version);
      if (input.sectionId) {
        const s = sectionOf(input.sectionId);
        if (input.capacity !== null && input.capacity < s.seatsTaken) conflict(`${s.seatsTaken} student(s) are seated`);
        s.capacity = input.capacity;
      } else o.capacity = input.capacity;
      return bump(o);
    },
    roster: async (sectionId) => enrolments.filter((e) => e.sectionId === sectionId).map((e) => structuredClone(e)),
    seat: async (sectionId, input) => {
      const s = sectionOf(sectionId);
      if (!input.overrideEligibility) conflict("The student's registration for the term does not cover this course component");
      const e = seat(s.offeringId, sectionId, input.studentId, "S2026999", "Seated student", "override");
      e.statusReason = input.reason ?? null;
      enrolments.push(e);
      return structuredClone(e);
    },
    withdrawSeat: async (id, v, reason) => {
      const e = enrolments.find((x) => x.id === id) ?? conflict("No such enrolment");
      check(v, e.version);
      e.status = "withdrawn";
      e.statusReason = reason;
      e.version += 1;
      return structuredClone(e);
    },
    waitlist: async (offeringId) => waitlist.filter((w) => w.offeringId === offeringId).map((w) => structuredClone(w)),
    promote: async () => conflict("Nobody is waiting"),
    listAssignments: async (sectionId) => assignments.filter((a) => a.sectionId === sectionId).map((a) => structuredClone(a)),
    createAssignment: async (sectionId, input) => {
      const s = sectionOf(sectionId);
      const a: FacultyAssignment = {
        id: uid(), offeringId: s.offeringId, sectionId, facultySubjectRef: input.facultySubjectRef, facultyName: input.facultyName ?? null, staffId: null,
        role: input.role ?? "primary_instructor", weeklyHours: input.weeklyHours ?? null, effectiveFrom: input.effectiveFrom ?? null,
        effectiveTo: input.effectiveTo ?? null, status: "draft", statusReason: null, processInstanceId: null, version: 1, createdAt: NOW,
      };
      assignments.push(a);
      return structuredClone(a);
    },
    assignmentCommand: async (id, v, cmd, reason) => {
      const a = assignment(id);
      check(v, a.version);
      const next = { submit: ["draft", "approved"], activate: ["approved", "active"], revoke: ["active", "revoked"], complete: ["active", "completed"], withdraw: ["draft", "withdrawn"] } as const;
      const [from, to] = next[cmd];
      if (a.status !== from) conflict(`The assignment is ${a.status.replace(/_/g, " ")}`);
      a.status = to;
      a.statusReason = reason ?? null;
      a.version += 1;
      return structuredClone(a);
    },
    decideAssignment: async (id, v, decision, reason) => {
      const a = assignment(id);
      check(v, a.version);
      a.status = decision === "approve" ? "approved" : "rejected";
      a.statusReason = reason ?? null;
      a.version += 1;
      return structuredClone(a);
    },
    listPolicies: async () => policies.map((p) => structuredClone(p)),
    createPolicy: async (_i, input) => {
      const p: FacultyAssignmentPolicy = {
        id: uid(), institutionId: INST, name: input.name ?? "Faculty assignment policy", versionNo: policies.length + 1, maxWeeklyHours: input.maxWeeklyHours ?? null,
        allowedRoles: input.allowedRoles ?? [], requirePrimaryInstructor: input.requirePrimaryInstructor ?? true, rationale: input.rationale ?? "", reviewOutcome: null,
        reviewNotes: null, status: "draft", statusReason: null, processInstanceId: null, supersededBy: null, effectiveAt: null, version: 1, createdAt: NOW,
      };
      policies.unshift(p);
      return structuredClone(p);
    },
    submitPolicy: async (id, v) => {
      const p = policy(id);
      check(v, p.version);
      if (p.status !== "draft") conflict("Only drafts are submitted");
      p.status = "in_review";
      p.version += 1;
      return structuredClone(p);
    },
    reviewPolicy: async (id, v, outcome, notes) => {
      const p = policy(id);
      check(v, p.version);
      if (p.status !== "in_review") conflict("Only versions in review are reviewed");
      p.reviewOutcome = outcome;
      p.reviewNotes = notes ?? null;
      if (outcome === "compatible") {
        for (const q of policies) if (q.status === "effective") q.status = "superseded";
        p.status = "effective";
      } else p.status = "draft";
      p.version += 1;
      return structuredClone(p);
    },
    myCourses: async () =>
      enrolments
        .filter((e) => e.studentId === "s1" && e.status === "enrolled")
        .map((e) => {
          const o = refresh(find(e.offeringId));
          const s = o.sections.find((x) => x.id === e.sectionId)!;
          return {
            offeringId: o.id, sectionId: s.id, enrolmentId: e.id, offeringCode: o.offeringCode, title: o.title, courseCode: o.courseCode, credits: o.credits,
            sectionCode: s.sectionCode, academicTermId: o.academicTermId, faculty: s.faculty.filter((f) => f.status === "active"), status: o.status, enrolmentVersion: e.version,
          };
        }),
    availableCourses: async (): Promise<AvailableCourse[]> =>
      offerings
        .filter((o) => o.status === "enrollment_open")
        .map((o) => {
          const r = refresh(o);
          return {
            offeringId: o.id, offeringCode: o.offeringCode, title: o.title, credits: o.credits, academicTermId: o.academicTermId, status: o.status, covered: true,
            seated: enrolments.some((e) => e.offeringId === o.id && e.studentId === "s1" && e.status === "enrolled"),
            waitlisted: waitlist.some((w) => w.offeringId === o.id && w.studentId === "s1" && (w.status === "waiting" || w.status === "offered")),
            sections: r.sections
              .filter((s) => s.status === "active")
              .map((s) => ({ sectionId: s.id, sectionCode: s.sectionCode, capacity: s.capacity, seatsTaken: s.seatsTaken, full: s.capacity != null && s.seatsTaken >= s.capacity })),
          };
        }),
    enrolMe: async (sectionId) => {
      const s = sectionOf(sectionId);
      if (enrolments.some((e) => e.offeringId === s.offeringId && e.studentId === "s1" && e.status === "enrolled")) conflict("You already hold a seat in this offering");
      const e = seat(s.offeringId, sectionId, "s1", "S2026001", "Aarav Desai", "registration");
      enrolments.push(e);
      return structuredClone(e);
    },
    dropMine: async (id, v) => {
      const e = enrolments.find((x) => x.id === id && x.studentId === "s1") ?? conflict("No such enrolment");
      check(v, e.version);
      if (find(e.offeringId).status !== "enrollment_open") conflict("Seats are dropped only while enrolment is open");
      e.status = "dropped";
      e.version += 1;
      return structuredClone(e);
    },
    joinWaitlist: async (offeringId) => {
      const w: OfferingWaitlistEntry = {
        id: uid(), offeringId, sectionId: null, studentId: "s1", studentNumber: "S2026001", displayName: "Aarav Desai",
        position: waitlist.filter((x) => x.offeringId === offeringId).length + 1, status: "waiting", statusReason: null, offeredSectionId: null,
        offeredAt: null, offerExpiresOn: null, enrolmentId: null, version: 1, createdAt: NOW,
      };
      waitlist.push(w);
      return structuredClone(w);
    },
    myClasses: async () =>
      assignments
        .filter((a) => a.facultySubjectRef === MEERA && a.status === "active")
        .map((a) => {
          const o = find(a.offeringId);
          const s = o.sections.find((x) => x.id === a.sectionId)!;
          return {
            sectionId: s.id, offeringId: o.id, offeringCode: o.offeringCode, title: o.title, sectionCode: s.sectionCode, academicTermId: o.academicTermId,
            role: a.role, seatedCount: enrolments.filter((e) => e.sectionId === s.id && e.status === "enrolled").length, offeringStatus: o.status,
          };
        }),
    myStudents: async (): Promise<MyStudent[]> =>
      assignments
        .filter((a) => a.facultySubjectRef === MEERA && a.status === "active")
        .flatMap((a) => {
          const o = find(a.offeringId);
          const s = o.sections.find((x) => x.id === a.sectionId)!;
          return enrolments
            .filter((e) => e.sectionId === s.id && e.status === "enrolled")
            .map((e) => ({ studentId: e.studentId, studentNumber: e.studentNumber, displayName: e.displayName, sectionId: s.id, offeringCode: o.offeringCode, sectionCode: s.sectionCode }));
        }),
  };
}
