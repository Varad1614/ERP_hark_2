/**
 * In-memory stand-in of the slice-4C API (mock mode, unit tests). Small but coherent: one
 * active provider, one active MOOC opportunity with an effective policy, a published resource
 * for the signed-in student's section, one section taught by the signed-in faculty member.
 * Rules mirror the API loosely (the API stays authoritative).
 */
import { ApiProblem } from "@sutra/api-client";
import type { FlexibleApi } from "./api";
import type {
  CertificateVerification,
  CourseResource,
  CreditRecognition,
  ExceptionCase,
  FlexibleEnrolment,
  FlexiblePolicy,
  LearningOpportunity,
  LearningProvider,
  MappingRule,
  ReconciliationRun,
} from "./types";

const INST = "00000000-0000-7000-8000-00000000c400";
const TERM = "00000000-0000-7000-8000-00000000c401";
const SECTION = "00000000-0000-7000-8000-00000000c402";
const OFFERING = "00000000-0000-7000-8000-00000000c403";
let seq = 1;
const uid = () => `00000000-0000-7000-8000-${String(5000 + seq++).padStart(12, "0")}`;

function fail(status: number, detail: string): never {
  throw new ApiProblem({ type: "about:blank", title: status === 404 ? "Not found" : "Conflict", status, detail, errorCode: status === 404 ? "not-found" : "conflict" });
}

function find<T extends { id: string }>(rows: T[], id: string): T {
  return rows.find((x) => x.id === id) ?? fail(404, "not found");
}

function bump<T extends { version: number }>(row: T, version: number, patch: Partial<T>): T {
  if (row.version !== version) fail(412, "stale version");
  Object.assign(row, patch, { version: row.version + 1 });
  return { ...row };
}

export function createMockFlexibleApi(): FlexibleApi {
  const providers: LearningProvider[] = [
    { id: uid(), institutionId: INST, code: "NPTEL", name: "NPTEL", providerKind: "government_platform", accreditationRef: "MoE-2026", accreditationValidUntil: "2030-12-31", modes: ["nptel", "mooc"], status: "active", statusReason: null, version: 3 },
  ];
  const opportunities: LearningOpportunity[] = [
    {
      id: uid(), institutionId: INST, code: "NPTEL-ML", title: "Machine Learning (NPTEL)", mode: "nptel", providerId: providers[0]!.id, offeringId: null, academicTermId: TERM,
      credits: 4, minHours: 30, capacity: 2, reservedSeats: 0, waitlistEnabled: true, requiresApproval: false, enrolmentOpensOn: "2026-01-01", enrolmentClosesOn: "2027-12-31",
      status: "active", statusReason: null, seatsAllocated: 0, waitlistCount: 0, overAllocated: false, providerName: "NPTEL", offeringCode: null, version: 3,
    },
  ];
  const policies: FlexiblePolicy[] = [
    { id: uid(), name: "Flexible learning policy", versionNo: 1, rules: { maxConcurrentEnrolments: 2, maxCreditsPerTerm: 8, requireHumanApproval: false, waitlistValidityDays: 14, withdrawalAllowedUntil: "registered" }, effectiveFrom: "2026-01-01", effectiveTo: null, status: "effective", version: 3 },
  ];
  const enrolments: FlexibleEnrolment[] = [];
  const exceptions: ExceptionCase[] = [];
  const certificates: CertificateVerification[] = [];
  const mappings: MappingRule[] = [];
  const recognitions: CreditRecognition[] = [];
  const runs: ReconciliationRun[] = [];
  const resources: CourseResource[] = [
    { id: uid(), offeringId: OFFERING, sectionId: SECTION, title: "Course reading list", kind: "reading", url: "https://library.example.edu/cs101", description: null, status: "published", version: 2 },
  ];
  const enrolment = (opportunityId: string): FlexibleEnrolment => {
    const o = find(opportunities, opportunityId);
    if (o.status !== "active") fail(409, "the opportunity accepts no new enrolment");
    const e: FlexibleEnrolment = {
      id: uid(), institutionId: INST, opportunityId, studentId: "student", studentNumber: "S-0001", displayName: "Aarav Desai", priority: 100, status: "eligible",
      statusReason: null, waitlistPosition: null, waitlistExpiresOn: null, hoursCompleted: null, opportunityCode: o.code, opportunityTitle: o.title, version: 2,
    };
    enrolments.push(e);
    return { ...e };
  };
  return {
    listProviders: async () => providers.map((p) => ({ ...p })),
    createProvider: async (_i, input) => {
      const p: LearningProvider = { id: uid(), institutionId: INST, accreditationRef: null, accreditationValidUntil: null, ...input, status: "proposed", statusReason: null, version: 1 } as LearningProvider;
      providers.push(p);
      return { ...p };
    },
    providerCommand: async (id, v, cmd, reason) => {
      const to = { submit: "active", suspend: "suspended", reactivate: "active", offboard: "offboarded" } as const;
      return bump(find(providers, id), v, { status: to[cmd], statusReason: reason ?? null });
    },
    listOpportunities: async () => opportunities.map((o) => ({ ...o })),
    createOpportunity: async (_i, input) => {
      const o: LearningOpportunity = {
        id: uid(), institutionId: INST, providerId: null, offeringId: null, credits: input.credits ?? 0, minHours: null, reservedSeats: 0, waitlistEnabled: true, requiresApproval: false,
        enrolmentOpensOn: null, enrolmentClosesOn: null, ...input, status: "draft", statusReason: null, seatsAllocated: 0, waitlistCount: 0, overAllocated: false, providerName: null, offeringCode: null, version: 1,
      } as LearningOpportunity;
      opportunities.push(o);
      return { ...o };
    },
    opportunityCommand: async (id, v, cmd, reason) => {
      const to = { submit: "active", suspend: "suspended", reactivate: "active", archive: "archived" } as const;
      return bump(find(opportunities, id), v, { status: to[cmd], statusReason: reason ?? null });
    },
    setCapacity: async (id, v, capacity, reservedSeats) => bump(find(opportunities, id), v, { capacity, reservedSeats }),
    seatCommand: async (id, v, cmd) => {
      const o = find(opportunities, id);
      const run = { opportunity: null as LearningOpportunity | null, allocated: [] as string[], waitlisted: [] as string[], unallocated: [], expired: [], ineligible: [], released: [] };
      if (cmd !== "resolve") {
        for (const e of enrolments.filter((x) => x.opportunityId === id && (x.status === "eligible" || (cmd === "promote" && x.status === "waitlisted")))) {
          if (o.seatsAllocated < o.capacity - o.reservedSeats) {
            e.status = "allocated";
            o.seatsAllocated += 1;
            run.allocated.push(e.id);
          } else {
            e.status = "waitlisted";
            run.waitlisted.push(e.id);
          }
          e.version += 1;
        }
      }
      run.opportunity = bump(o, v, {});
      return run;
    },
    listPolicies: async () => policies.map((p) => ({ ...p })),
    createPolicy: async (_i, input) => {
      const p: FlexiblePolicy = { id: uid(), versionNo: policies.length + 1, effectiveTo: null, status: "draft", version: 1, ...input };
      policies.push(p);
      return { ...p };
    },
    submitPolicy: async (id, v) => {
      for (const p of policies) if (p.status === "effective") p.status = "superseded";
      return bump(find(policies, id), v, { status: "effective" });
    },
    listEnrolments: async (o) => enrolments.filter((e) => e.opportunityId === o).map((e) => ({ ...e })),
    enrolmentDecision: async (id, v, approve, reason) => bump(find(enrolments, id), v, { status: approve ? "eligible" : "rejected", statusReason: reason ?? null }),
    register: async (id, v) => bump(find(enrolments, id), v, { statusReason: "provider registration requested" }),
    registrationOutcome: async (id, v, outcome, providerReference) =>
      bump(find(enrolments, id), v, outcome === "accepted" ? { status: "registered", registrations: [{ id: uid(), status: "accepted", providerReference: providerReference ?? null, outcomeReason: null }] } : { statusReason: `provider registration ${outcome}` }),
    progress: async (id, v, hoursCompleted) => bump(find(enrolments, id), v, { status: "in_progress", hoursCompleted }),
    complete: async (id, v, input) => bump(find(enrolments, id), v, { status: "completed", hoursCompleted: input.hoursCompleted }),
    withdraw: async (id, v, reason) => bump(find(enrolments, id), v, { status: "withdrawn", statusReason: reason }),
    listExceptions: async () => exceptions.map((x) => ({ ...x })),
    raiseException: async (_i, input) => {
      const x: ExceptionCase = { id: uid(), category: input.category, title: input.title, ownerSubject: "dev|admin@sutra.dev", correctivePlan: null, resolutionEvidence: null, onHold: false, status: "open", version: 1 };
      exceptions.push(x);
      return { ...x };
    },
    planException: async (id, v, correctivePlan) => bump(find(exceptions, id), v, { status: "plan_approved", correctivePlan }),
    resolveException: async (id, v, _a, resolutionEvidence) => bump(find(exceptions, id), v, { status: "resolved", resolutionEvidence }),
    closeException: async (id, v) => bump(find(exceptions, id), v, { status: "closed" }),
    certificateQueue: async () => certificates.map((c) => ({ ...c })),
    submitCertificate: async (enrolmentId, input) => {
      const c: CertificateVerification = {
        id: uid(), enrolmentId, studentId: "student", certificateNumber: input.certificateNumber, certificateUrl: input.certificateUrl ?? null, issuedOn: input.issuedOn ?? null,
        status: "submitted", checks: { authenticityConfirmed: null, identityMatched: null, providerAccredited: null, fraudIndicators: [] }, decisionReason: null, version: 1,
      };
      certificates.push(c);
      return { ...c };
    },
    verifyCertificate: async (id, v, input) => {
      const ok = input.authenticityConfirmed && input.identityMatched && input.fraudIndicators.length === 0;
      return bump(find(certificates, id), v, { status: input.fraudIndicators.length ? "flagged" : ok ? "accepted" : "rejected", checks: { ...input, providerAccredited: true } });
    },
    listMappings: async () => mappings.map((m) => ({ ...m })),
    createMapping: async (_i, input) => {
      const m: MappingRule = { id: uid(), curriculumComponentId: null, versionNo: 1, status: "draft", version: 1, ...input };
      mappings.push(m);
      return { ...m };
    },
    submitMapping: async (id, v) => bump(find(mappings, id), v, { status: "effective" }),
    listRecognitions: async () => recognitions.map((r) => ({ ...r })),
    requestRecognition: async (enrolmentId) => {
      const e = find(enrolments, enrolmentId);
      const r: CreditRecognition = { id: uid(), studentId: e.studentId, flexibleEnrolmentId: e.id, opportunityId: e.opportunityId, credits: 4, requestedCredits: 4, curriculumComponentId: null, status: "approved", ledgerEntryId: null, decisionReason: null, version: 3 };
      recognitions.push(r);
      return { ...r };
    },
    decideRecognition: async (id, v, approve, credits, reason) => bump(find(recognitions, id), v, { status: approve ? "approved" : "rejected", credits: credits ?? find(recognitions, id).credits, decisionReason: reason ?? null }),
    postRecognition: async (id, v) => bump(find(recognitions, id), v, { status: "posted", ledgerEntryId: uid() }),
    listReconciliations: async () => runs.map((r) => ({ ...r })),
    runReconciliation: async (_i, opportunityId, records) => {
      const r: ReconciliationRun = {
        id: uid(), opportunityId, status: records.length ? "discrepancies" : "reconciled", resolution: null, exceptionCaseId: null, createdAt: new Date().toISOString(), version: 1,
        discrepancies: records.map((x) => ({ studentNumber: x.studentNumber, kind: "unknown_learner", institutional: null, provider: x.providerReference ?? null })),
      };
      runs.push(r);
      return { ...r };
    },
    resolveReconciliation: async (id, v, resolution) => bump(find(runs, id), v, { status: resolution === "escalated" ? "escalated" : "reconciled", resolution }),
    myFlexible: async () => ({ opportunities: opportunities.filter((o) => o.status === "active").map((o) => ({ ...o })), enrolments: enrolments.map((e) => ({ ...e })) }),
    requestOpportunity: async (opportunityId) => enrolment(opportunityId),
    myLibrary: async () =>
      resources
        .filter((r) => r.status === "published")
        .map((r) => ({ id: r.id, offeringId: r.offeringId, sectionId: r.sectionId, title: r.title, kind: r.kind, url: r.url, description: r.description, offeringCode: "CS101-T1", offeringTitle: "Programming Fundamentals", sectionCode: "A", publishedAt: "2026-10-01T09:00:00Z" })),
    myResources: async () => ({ sections: [{ sectionId: SECTION, offeringId: OFFERING, offeringCode: "CS101-T1", title: "Programming Fundamentals", sectionCode: "A" }], items: resources.map((r) => ({ ...r })) }),
    createResource: async (sectionId, input) => {
      const r: CourseResource = { id: uid(), offeringId: OFFERING, sectionId, title: input.title, kind: input.kind, url: input.url, description: input.description ?? null, status: input.publish ? "published" : "draft", version: 1 };
      resources.push(r);
      return { ...r };
    },
    resourceCommand: async (id, v, cmd) => bump(find(resources, id), v, { status: cmd === "publish" ? "published" : "withdrawn" }),
  };
}
