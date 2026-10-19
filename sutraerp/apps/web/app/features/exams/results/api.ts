/**
 * Slice-6C API layer: one interface, an HTTP implementation (CSRF, Idempotency-Key on creates and
 * bulk commands, If-Match on commands, problem+json → `ApiProblem`) and an in-memory mock
 * (`./mock`). Every 6C path template is checked against the generated OpenAPI `paths`.
 */
import { ApiProblem, isProblem, isUnsafeMethod, newIdempotencyKey, type paths } from "@sutra/api-client";
import { getCsrfToken } from "../../../lib/api";
import { fill } from "../../institution/api";
import type {
  BatchCommand,
  Closure,
  Completion,
  Decision,
  Eligibility,
  ExamSessionRef,
  GradeCard,
  Items,
  Publication,
  PublicationCommand,
  ResultBatch,
  ResultCommand,
  Revaluation,
  ReviewStep,
  TermResult,
} from "./types";

export interface ResultsApi {
  sessions(institutionId: string): Promise<ExamSessionRef[]>;
  batches(sessionId: string): Promise<ResultBatch[]>;
  createBatch(sessionId: string): Promise<ResultBatch>;
  batchCommand(id: string, version: number, cmd: BatchCommand): Promise<ResultBatch>;
  decideBatch(id: string, version: number, decision: Decision, reason?: string): Promise<ResultBatch>;
  results(sessionId: string): Promise<TermResult[]>;
  resultCommand(id: string, version: number, cmd: ResultCommand, reason?: string): Promise<unknown>;
  correct(id: string, version: number, input: { paperId: string; externalMarks?: string; internalMarks?: string; reason: string }): Promise<unknown>;
  issueCard(resultId: string): Promise<GradeCard>;
  publications(sessionId: string): Promise<Publication[]>;
  createPublication(sessionId: string): Promise<Publication>;
  publicationCommand(id: string, version: number, cmd: PublicationCommand): Promise<unknown>;
  delayPublication(id: string, version: number, reason: string): Promise<Publication>;
  cancelPublication(id: string, version: number, reason: string): Promise<Publication>;
  decidePublication(id: string, version: number, decision: Decision): Promise<Publication>;
  reviews(sessionId: string): Promise<Revaluation[]>;
  reviewStep(id: string, version: number, step: ReviewStep): Promise<Revaluation>;
  decideReview(id: string, version: number, decision: Decision, reason?: string): Promise<Revaluation>;
  completeReview(id: string, version: number, externalMarks: string, notes?: string): Promise<Revaluation>;
  withdrawReview(id: string, version: number, reason?: string): Promise<Revaluation>;
  eligibility(sessionId: string): Promise<Eligibility[]>;
  determineEligibility(sessionId: string): Promise<{ eligible: number; exhausted: number; skipped: number }>;
  closure(sessionId: string): Promise<Closure | null>;
  closureCommand(sessionId: string, cmd: "initiate" | "close" | "archive", version?: number): Promise<Closure>;
  completions(institutionId: string): Promise<Completion[]>;
  evaluateCompletion(institutionId: string, studentId: string): Promise<Completion>;
  myResults(): Promise<TermResult[]>;
  myGradeCards(): Promise<GradeCard[]>;
  myReviews(): Promise<Revaluation[]>;
  requestReview(input: { paperId: string; kind: "revaluation" | "rechecking"; reason?: string }): Promise<Revaluation>;
  withdrawMyReview(id: string): Promise<Revaluation>;
}

const B = "/api/v1/exams";
export const PATHS = {
  batches: `${B}/exam-sessions/{id}/result-batches`,
  compute: `${B}/result-batches/{id}/compute`,
  validate: `${B}/result-batches/{id}/validate`,
  finalise: `${B}/result-batches/{id}/finalise`,
  batchDecision: `${B}/result-batches/{id}/decision`,
  results: `${B}/exam-sessions/{id}/results`,
  withhold: `${B}/term-results/{id}/withhold`,
  release: `${B}/term-results/{id}/release`,
  recompute: `${B}/term-results/{id}/recompute`,
  correct: `${B}/term-results/{id}/correct`,
  withdraw: `${B}/term-results/{id}/withdraw`,
  restore: `${B}/term-results/{id}/restore`,
  gradeCard: `${B}/term-results/{id}/grade-card`,
  publications: `${B}/exam-sessions/{id}/publications`,
  authorise: `${B}/publications/{id}/authorise`,
  publish: `${B}/publications/{id}/publish`,
  resume: `${B}/publications/{id}/resume`,
  retry: `${B}/publications/{id}/retry`,
  delay: `${B}/publications/{id}/delay`,
  cancel: `${B}/publications/{id}/cancel`,
  pubDecision: `${B}/publications/{id}/decision`,
  reviews: `${B}/exam-sessions/{id}/revaluation-requests`,
  verify: `${B}/revaluation-requests/{id}/verify`,
  startReview: `${B}/revaluation-requests/{id}/start-review`,
  verifyOutcome: `${B}/revaluation-requests/{id}/verify-outcome`,
  approveMarks: `${B}/revaluation-requests/{id}/approve-marks`,
  completeReview: `${B}/revaluation-requests/{id}/complete`,
  withdrawReview: `${B}/revaluation-requests/{id}/withdraw`,
  reviewDecision: `${B}/revaluation-requests/{id}/decision`,
  eligibility: `${B}/exam-sessions/{id}/supplementary-eligibility`,
  closure: `${B}/exam-sessions/{id}/closure`,
  closureInitiate: `${B}/exam-sessions/{id}/closure/initiate`,
  closureClose: `${B}/exam-sessions/{id}/closure/close`,
  closureArchive: `${B}/exam-sessions/{id}/closure/archive`,
  completions: `${B}/institutions/{id}/completion-evaluations`,
  meResults: `${B}/me/results`,
  meCards: `${B}/me/grade-cards`,
  meReviews: `${B}/me/revaluation-requests`,
  meWithdraw: `${B}/me/revaluation-requests/{id}/withdraw`,
} as const satisfies Record<string, keyof paths>;

/** Slice 6A's session list (typed locally until 6A merges into the base, contract §10). */
export const SESSIONS_PATH = `${B}/institutions/{id}/exam-sessions`;

const RESULT_CMD: Record<ResultCommand, string> = {
  withhold: PATHS.withhold,
  release: PATHS.release,
  recompute: PATHS.recompute,
  withdraw: PATHS.withdraw,
  restore: PATHS.restore,
};
const BATCH_CMD: Record<BatchCommand, string> = { compute: PATHS.compute, validate: PATHS.validate, finalise: PATHS.finalise };
const PUB_CMD: Record<PublicationCommand, string> = { authorise: PATHS.authorise, publish: PATHS.publish, resume: PATHS.resume, retry: PATHS.retry };
const STEP: Record<ReviewStep, string> = {
  verify: PATHS.verify,
  "start-review": PATHS.startReview,
  "verify-outcome": PATHS.verifyOutcome,
  "approve-marks": PATHS.approveMarks,
};
const CLOSURE: Record<"initiate" | "close" | "archive", string> = {
  initiate: PATHS.closureInitiate,
  close: PATHS.closureClose,
  archive: PATHS.closureArchive,
};

type Fetch = (input: string, init: RequestInit) => Promise<Response>;

interface RequestOptions {
  body?: unknown;
  ifMatch?: number;
  idempotent?: boolean;
}

export function createHttpResultsApi(fetchImpl: Fetch = (i, init) => fetch(i, init), baseUrl: string = import.meta.env.VITE_API_BASE ?? ""): ResultsApi {
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
  const items = async <T>(path: string) => (await request<Items<T>>("GET", path)).items;
  const id = (i: string) => ({ id: i });
  const reasonBody = (reason?: string) => (reason ? { reason } : {});
  return {
    sessions: async (i) => {
      try {
        return await items<ExamSessionRef>(fill(SESSIONS_PATH, id(i)));
      } catch {
        return [];
      }
    },
    batches: (s) => items(fill(PATHS.batches, id(s))),
    createBatch: (s) => request("POST", fill(PATHS.batches, id(s)), { idempotent: true }),
    batchCommand: (i, v, cmd) => request("POST", fill(BATCH_CMD[cmd], id(i)), { ifMatch: v, idempotent: cmd === "compute" }),
    decideBatch: (i, v, decision, reason) => request("POST", fill(PATHS.batchDecision, id(i)), { ifMatch: v, body: { decision, reason } }),
    results: (s) => items(fill(PATHS.results, id(s))),
    resultCommand: (i, v, cmd, reason) => request("POST", fill(RESULT_CMD[cmd], id(i)), { ifMatch: v, body: reasonBody(reason) }),
    correct: (i, v, input) => request("POST", fill(PATHS.correct, id(i)), { ifMatch: v, body: input }),
    issueCard: (r) => request("POST", fill(PATHS.gradeCard, id(r)), { idempotent: true }),
    publications: (s) => items(fill(PATHS.publications, id(s))),
    createPublication: (s) => request("POST", fill(PATHS.publications, id(s)), { idempotent: true }),
    publicationCommand: (i, v, cmd) => request("POST", fill(PUB_CMD[cmd], id(i)), { ifMatch: v, idempotent: cmd === "publish" || cmd === "retry" }),
    delayPublication: (i, v, reason) => request("POST", fill(PATHS.delay, id(i)), { ifMatch: v, body: { reason } }),
    cancelPublication: (i, v, reason) => request("POST", fill(PATHS.cancel, id(i)), { ifMatch: v, body: { reason } }),
    decidePublication: (i, v, decision) => request("POST", fill(PATHS.pubDecision, id(i)), { ifMatch: v, body: { decision } }),
    reviews: (s) => items(fill(PATHS.reviews, id(s))),
    reviewStep: (i, v, step) => request("POST", fill(STEP[step], id(i)), { ifMatch: v }),
    decideReview: (i, v, decision, reason) => request("POST", fill(PATHS.reviewDecision, id(i)), { ifMatch: v, body: { decision, reason } }),
    completeReview: (i, v, externalMarks, notes) => request("POST", fill(PATHS.completeReview, id(i)), { ifMatch: v, body: { externalMarks, notes } }),
    withdrawReview: (i, v, reason) => request("POST", fill(PATHS.withdrawReview, id(i)), { ifMatch: v, body: reasonBody(reason) }),
    eligibility: (s) => items(fill(PATHS.eligibility, id(s))),
    determineEligibility: (s) => request("POST", fill(PATHS.eligibility, id(s)), { idempotent: true }),
    closure: async (s) => {
      try {
        return await request<Closure>("GET", fill(PATHS.closure, id(s)));
      } catch (e) {
        if (e instanceof ApiProblem && e.problem.status === 404) return null;
        throw e;
      }
    },
    closureCommand: (s, cmd, v) => request("POST", fill(CLOSURE[cmd], id(s)), cmd === "initiate" ? { idempotent: true } : { ifMatch: v }),
    completions: (i) => items(fill(PATHS.completions, id(i))),
    evaluateCompletion: (i, studentId) => request("POST", fill(PATHS.completions, id(i)), { body: { studentId }, idempotent: true }),
    myResults: () => items(PATHS.meResults),
    myGradeCards: () => items(PATHS.meCards),
    myReviews: () => items(PATHS.meReviews),
    requestReview: (input) => request("POST", PATHS.meReviews, { body: input, idempotent: true }),
    withdrawMyReview: (i) => request("POST", fill(PATHS.meWithdraw, id(i)), {}),
  };
}
