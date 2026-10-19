/**
 * ERP_EXTENSIONS 3B API layer: one interface, an HTTP implementation (CSRF, Idempotency-Key on
 * create) and the mock in `./mock`. Paths are checked against the OpenAPI.
 */
import { ApiProblem, isProblem, isUnsafeMethod, newIdempotencyKey, type paths } from "@sutra/api-client";
import { getCsrfToken } from "../../../lib/api";
import type {
  CandidateFeedback, CandidateInterview, CandidateInterviewInput, FeedbackDueRow, HiringForm, HiringSettings, RatingField,
  Scorecard, ScorecardCompare, ScorecardInput, ScreeningAnswer, ScreeningQuestion,
} from "./types";

const B = "/api/v1/people";
export const HIRING_PATHS = {
  settings: `${B}/institutions/{id}/hiring-settings`,
  form: `${B}/requisitions/{id}/hiring-form`,
  answers: `${B}/candidates/{id}/screening-answers`,
  interviews: `${B}/candidates/{id}/interviews`,
  cancel: `${B}/candidate-interviews/{id}/cancel`,
  scorecards: `${B}/candidates/{id}/scorecards`,
  feedback: `${B}/candidates/{id}/feedback`,
  compare: `${B}/requisitions/{id}/scorecards`,
  due: `${B}/institutions/{id}/feedback-due`,
} as const satisfies Record<string, keyof paths>;

export interface HiringApi {
  settings(institutionId: string): Promise<HiringSettings>;
  saveSettings(institutionId: string, s: HiringSettings): Promise<HiringSettings>;
  saveForm(requisitionId: string, f: { ratingFields?: RatingField[] | null; screeningQuestions?: ScreeningQuestion[] | null }): Promise<HiringForm>;
  saveAnswers(candidateId: string, answers: ScreeningAnswer[]): Promise<void>;
  scheduleInterview(candidateId: string, input: CandidateInterviewInput): Promise<CandidateInterview>;
  cancelInterview(id: string): Promise<void>;
  createScorecard(candidateId: string, input: ScorecardInput): Promise<Scorecard>;
  feedback(candidateId: string): Promise<CandidateFeedback>;
  compare(requisitionId: string): Promise<ScorecardCompare>;
  feedbackDue(institutionId: string): Promise<FeedbackDueRow[]>;
}

const fill = (t: string, id: string) => t.replace("{id}", encodeURIComponent(id));
type Fetch = (input: string, init: RequestInit) => Promise<Response>;

export function createHttpHiringApi(fetchImpl: Fetch = (i, init) => fetch(i, init), baseUrl: string = import.meta.env.VITE_API_BASE ?? ""): HiringApi {
  async function request<T>(method: string, path: string, opts: { body?: unknown; idempotent?: boolean } = {}): Promise<T> {
    const headers: Record<string, string> = { Accept: "application/json" };
    const unsafe = isUnsafeMethod(method);
    if (unsafe) {
      headers["Content-Type"] = "application/json";
      const csrf = getCsrfToken();
      if (csrf) headers["X-CSRF-Token"] = csrf;
    }
    if (opts.idempotent) headers["Idempotency-Key"] = newIdempotencyKey();
    const res = await fetchImpl(`${baseUrl}${path}`, { method, headers, credentials: "include", body: unsafe ? JSON.stringify(opts.body ?? {}) : undefined });
    const text = await res.text();
    let data: unknown;
    try {
      data = text ? JSON.parse(text) : undefined;
    } catch {
      data = undefined;
    }
    if (!res.ok) {
      if (isProblem(data)) throw new ApiProblem(data);
      throw new ApiProblem({ type: "about:blank", title: `Request failed (${res.status})`, status: res.status, errorCode: `http-${res.status}` });
    }
    return data as T;
  }
  const P = HIRING_PATHS;
  return {
    settings: (id) => request("GET", fill(P.settings, id)),
    saveSettings: (id, s) => request("PUT", fill(P.settings, id), { body: s }),
    saveForm: (id, f) => request("PUT", fill(P.form, id), { body: f }),
    saveAnswers: async (id, answers) => {
      await request<void>("PUT", fill(P.answers, id), { body: { answers } });
    },
    scheduleInterview: (id, input) => request("POST", fill(P.interviews, id), { body: input, idempotent: true }),
    cancelInterview: async (id) => {
      await request<void>("POST", fill(P.cancel, id));
    },
    createScorecard: (id, input) => request("POST", fill(P.scorecards, id), { body: input, idempotent: true }),
    feedback: (id) => request("GET", fill(P.feedback, id)),
    compare: (id) => request("GET", fill(P.compare, id)),
    feedbackDue: async (id) => (await request<{ items: FeedbackDueRow[] }>("GET", fill(P.due, id))).items,
  };
}
