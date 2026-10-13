/**
 * In-memory slice-6C API for mock mode and tests: one active session with one computed result,
 * and a published result for the signed-in student. Commands move statuses the way the API does
 * (enough for screens and tests; the API stays authoritative).
 */
import { ApiProblem } from "@sutra/api-client";
import type { ResultsApi } from "./api";
import type { Closure, Completion, Eligibility, ExamSessionRef, GradeCard, Publication, ResultBatch, Revaluation, TermResult } from "./types";

const conflict = (title: string) =>
  new ApiProblem({ type: "about:blank", title, status: 409, errorCode: "conflict" } as ConstructorParameters<typeof ApiProblem>[0]);

function sampleResult(id: string, status: string): TermResult {
  return {
    id,
    sessionId: "ses-1",
    registrationId: "reg-1",
    studentId: "stu-1",
    studentNumber: "S2026-0001",
    displayName: "Aarav Desai",
    creditsAttempted: "7.00",
    creditsEarned: "7.00",
    gradePoints: "55.00",
    sgpa: "7.86",
    cgpa: "7.86",
    outcome: "pass",
    status,
    revision: 1,
    publishedAt: status === "published" ? "2026-12-20T10:00:00Z" : null,
    lines: [
      { lineNo: 1, paperId: "pap-1", paperCode: "CS101", title: "Programming", credits: "4.00", attemptKind: "regular", internalMarks: "30.00", externalMarks: "60.00", graceMarks: "0.00", totalMarks: "90.00", maxMarks: "140.00", percent: "66.00", grade: "B+", gradePoint: "7.00", passed: true },
      { lineNo: 2, paperId: "pap-2", paperCode: "MD101", title: "Mathematics", credits: "3.00", attemptKind: "regular", internalMarks: "36.00", externalMarks: "85.00", graceMarks: "0.00", totalMarks: "121.00", maxMarks: "140.00", percent: "87.00", grade: "A+", gradePoint: "9.00", passed: true },
    ],
    version: 1,
  };
}

export function createMockResultsApi(): ResultsApi {
  const sessions: ExamSessionRef[] = [{ id: "ses-1", code: "ESE-2026-ODD", name: "End-semester examinations", status: "active", kind: "regular" }];
  let batches: ResultBatch[] = [{ id: "bat-1", sessionId: "ses-1", status: "computed", runCount: 1, candidates: 1, computedCount: 1, withheldCount: 0, version: 2 }];
  let results: TermResult[] = [sampleResult("res-1", "computed")];
  let publications: Publication[] = [];
  let reviews: Revaluation[] = [];
  const eligibility: Eligibility[] = [];
  let closure: Closure | null = null;
  const completions: Completion[] = [];
  const mine: TermResult[] = [sampleResult("res-me", "published")];
  let myReviews: Revaluation[] = [];
  let n = 1;
  const bump = <T extends { version: number }>(x: T, patch: Partial<T>): T => ({ ...x, ...patch, version: x.version + 1 });
  const find = <T extends { id: string }>(xs: T[], id: string): T => {
    const x = xs.find((y) => y.id === id);
    if (!x) throw conflict("not found");
    return x;
  };
  return {
    sessions: async () => sessions,
    batches: async (s) => batches.filter((b) => b.sessionId === s),
    createBatch: async (s) => {
      if (batches.some((b) => b.sessionId === s && b.status !== "closed")) throw conflict("the session already has a live result batch");
      const b: ResultBatch = { id: `bat-${++n}`, sessionId: s, status: "pending", runCount: 0, candidates: 0, computedCount: 0, withheldCount: 0, version: 1 };
      batches = [...batches, b];
      return b;
    },
    batchCommand: async (id, _v, cmd) => {
      const b = find(batches, id);
      const status = cmd === "compute" ? "computed" : cmd === "validate" ? "approved" : "ready_for_publication";
      if (cmd === "validate") results = results.map((r) => (r.status === "computed" ? bump(r, { status: "approved" }) : r));
      const next = bump(b, { status, runCount: cmd === "compute" ? b.runCount + 1 : b.runCount });
      batches = batches.map((x) => (x.id === id ? next : x));
      return next;
    },
    decideBatch: async (id, _v, decision) => {
      const next = bump(find(batches, id), { status: decision === "approve" ? "approved" : "computed" });
      batches = batches.map((x) => (x.id === id ? next : x));
      return next;
    },
    results: async (s) => results.filter((r) => r.sessionId === s),
    resultCommand: async (id, _v, cmd, reason) => {
      const status = { withhold: "withheld", release: "approved", recompute: "computed", withdraw: "withdrawn", restore: "published" }[cmd];
      const next = bump(find(results, id), { status, statusReason: reason ?? null });
      results = results.map((x) => (x.id === id ? next : x));
      return next;
    },
    correct: async (id) => find(results, id),
    issueCard: async (resultId) => ({ id: `gc-${++n}`, number: `GC-${String(n).padStart(6, "0")}`, status: "issued", resultId, registrationId: "reg-1", issuedAt: new Date().toISOString(), version: 1 }),
    publications: async (s) => publications.filter((p) => p.sessionId === s),
    createPublication: async (s) => {
      const p: Publication = { id: `pub-${++n}`, sessionId: s, status: "planned", publishedCount: 0, skippedCount: 0, version: 1 };
      publications = [...publications, p];
      return p;
    },
    publicationCommand: async (id, _v, cmd) => {
      const p = find(publications, id);
      const publishing = cmd === "publish" || cmd === "retry";
      if (publishing) results = results.map((r) => (r.status === "approved" ? bump(r, { status: "published" }) : r));
      const next = bump(p, { status: publishing ? "published" : "authorised" });
      publications = publications.map((x) => (x.id === id ? next : x));
      return next;
    },
    delayPublication: async (id, _v, reason) => {
      const next = bump(find(publications, id), { status: "delayed", statusReason: reason });
      publications = publications.map((x) => (x.id === id ? next : x));
      return next;
    },
    cancelPublication: async (id, _v, reason) => {
      const next = bump(find(publications, id), { status: "cancelled", statusReason: reason });
      publications = publications.map((x) => (x.id === id ? next : x));
      return next;
    },
    decidePublication: async (id, _v, decision) => {
      const next = bump(find(publications, id), { status: decision === "approve" ? "authorised" : "planned" });
      publications = publications.map((x) => (x.id === id ? next : x));
      return next;
    },
    reviews: async (s) => reviews.filter((r) => r.sessionId === s),
    reviewStep: async (id, _v, step) => {
      const status = { verify: "approved", "start-review": "review_in_progress", "verify-outcome": "outcome_verified", "approve-marks": "closed" }[step];
      const next = bump(find(reviews, id), { status, feeSettled: true });
      reviews = reviews.map((x) => (x.id === id ? next : x));
      return next;
    },
    decideReview: async (id, _v, decision) => {
      const next = bump(find(reviews, id), { status: decision === "approve" ? "approved" : "rejected" });
      reviews = reviews.map((x) => (x.id === id ? next : x));
      return next;
    },
    completeReview: async (id, _v, externalMarks) => {
      const r = find(reviews, id);
      const next = bump(r, { status: "review_completed", outcome: { fromMarks: r.outcome?.fromMarks ?? null, toMarks: externalMarks, changed: true } });
      reviews = reviews.map((x) => (x.id === id ? next : x));
      return next;
    },
    withdrawReview: async (id) => {
      const next = bump(find(reviews, id), { status: "withdrawn" });
      reviews = reviews.map((x) => (x.id === id ? next : x));
      return next;
    },
    eligibility: async () => eligibility,
    determineEligibility: async () => ({ eligible: 0, exhausted: 0, skipped: 0 }),
    closure: async () => closure,
    closureCommand: async (s, cmd) => {
      const status = cmd === "initiate" ? "closing" : cmd === "close" ? "closed" : "archived";
      closure = closure ? bump(closure, { status }) : { id: `clo-${++n}`, sessionId: s, status, checks: { ready: true, blockers: [] }, completedRegistrations: 0, version: 1 };
      return closure;
    },
    completions: async () => completions,
    evaluateCompletion: async (_i, studentId) => {
      const c: Completion = { id: `cmp-${++n}`, studentNumber: studentId, displayName: studentId, creditsRequired: "160.00", creditsEarned: "7.00", componentsRequired: 40, componentsPassed: 2, cgpa: "7.86", status: "not_eligible", version: 1 };
      completions.unshift(c);
      return c;
    },
    myResults: async () => mine,
    myGradeCards: async (): Promise<GradeCard[]> => [],
    myReviews: async () => myReviews,
    requestReview: async (input) => {
      if (myReviews.some((r) => r.paperId === input.paperId && !["withdrawn", "rejected", "closed"].includes(r.status))) throw conflict("a review of this paper is already in progress");
      const r: Revaluation = {
        id: `rev-${++n}`,
        number: `REV-${String(n).padStart(6, "0")}`,
        kind: input.kind,
        sessionId: "ses-1",
        paperId: input.paperId,
        paperCode: mine[0]?.lines.find((l) => l.paperId === input.paperId)?.paperCode ?? "",
        studentNumber: "S2026-0001",
        displayName: "Aarav Desai",
        status: "fee_pending",
        feeAmount: input.kind === "revaluation" ? "500.00" : "200.00",
        feeChargeNumber: `CHG-${String(n).padStart(6, "0")}`,
        feeSettled: false,
        version: 2,
      };
      myReviews = [r, ...myReviews];
      return r;
    },
    withdrawMyReview: async (id) => {
      const next = bump(find(myReviews, id), { status: "withdrawn" });
      myReviews = myReviews.map((x) => (x.id === id ? next : x));
      return next;
    },
  };
}
