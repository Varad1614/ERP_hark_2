/**
 * Slice-6B API layer (marks, gradebook, moderation, malpractice). Same conventions as
 * `features/finance/payments/api.ts`: one interface, an HTTP implementation (CSRF,
 * `Idempotency-Key` on creates / bulk commands, `If-Match` on state commands, problem+json →
 * `ApiProblem`) and an in-memory mock (`./mock`). Every path template is checked against the
 * generated OpenAPI `paths`.
 */
import { ApiProblem, isProblem, isUnsafeMethod, newIdempotencyKey, type paths } from "@sutra/api-client";
import { getCsrfToken } from "../../../lib/api";
import { fill } from "../../institution/api";
import type { Assessment, BulkSummary, CaseCommand, EntryRow, Gradebook, Items, MalpracticeCase, MarkRowList, MarkSheet, Moderation, SheetCommand } from "./types";

export interface MarksApi {
  myAssessments(): Promise<Assessment[]>;
  institutionPapers(institutionId: string): Promise<Assessment[]>;
  marks(paperId: string): Promise<MarkRowList>;
  enter(paperId: string, rows: EntryRow[]): Promise<MarkRowList>;
  openSheet(paperId: string): Promise<MarkSheet>;
  importInternal(sheetId: string): Promise<BulkSummary>;
  sheetCommand(sheetId: string, version: number, cmd: SheetCommand): Promise<MarkSheet>;
  decideSheet(sheetId: string, version: number, approve: boolean, reason?: string): Promise<MarkSheet>;
  withhold(markId: string, version: number, reason: string): Promise<unknown>;
  release(markId: string, version: number): Promise<unknown>;
  gradebook(paperId: string): Promise<Gradebook>;
  moderations(paperId: string): Promise<Moderation[]>;
  requestModeration(paperId: string, kind: string, value: string, reason: string): Promise<Moderation>;
  moderationCommand(id: string, version: number, cmd: "validate" | "apply" | "withdraw"): Promise<Moderation>;
  // malpractice
  cases(sessionId: string): Promise<MalpracticeCase[]>;
  reportCase(sessionId: string, input: { registrationId: string; paperId?: string; allegation: string; evidence?: string }): Promise<MalpracticeCase>;
  caseCommand(id: string, version: number, cmd: CaseCommand, reason?: string): Promise<MalpracticeCase>;
  decideCase(id: string, version: number, penalty: string, reason: string, marksDeduction?: string): Promise<MalpracticeCase>;
  reviewCase(id: string, version: number, outcome: string, reason: string, penalty?: string): Promise<MalpracticeCase>;
  approveCase(id: string, version: number, approve: boolean, reason?: string): Promise<MalpracticeCase>;
}

const B = "/api/v1/exams";
export const PATHS = {
  me: `${B}/me/assessments`,
  institution: `${B}/institutions/{id}/mark-sheets`,
  marks: `${B}/exam-papers/{id}/marks`,
  sheet: `${B}/exam-papers/{id}/mark-sheet`,
  importInternal: `${B}/mark-sheets/{id}/import-internal`,
  submit: `${B}/mark-sheets/{id}/submit`,
  verify: `${B}/mark-sheets/{id}/verify`,
  requestApproval: `${B}/mark-sheets/{id}/request-approval`,
  finalise: `${B}/mark-sheets/{id}/finalise`,
  decision: `${B}/mark-sheets/{id}/decision`,
  withhold: `${B}/paper-marks/{id}/withhold`,
  release: `${B}/paper-marks/{id}/release`,
  gradebook: `${B}/exam-papers/{id}/gradebook`,
  moderation: `${B}/exam-papers/{id}/moderation`,
  mValidate: `${B}/moderations/{id}/validate`,
  mApply: `${B}/moderations/{id}/apply`,
  mWithdraw: `${B}/moderations/{id}/withdraw`,
  cases: `${B}/exam-sessions/{id}/malpractice-cases`,
  investigate: `${B}/malpractice-cases/{id}/investigate`,
  hearing: `${B}/malpractice-cases/{id}/hearing`,
  appeal: `${B}/malpractice-cases/{id}/appeal`,
  reopen: `${B}/malpractice-cases/{id}/reopen`,
  close: `${B}/malpractice-cases/{id}/close`,
  archive: `${B}/malpractice-cases/{id}/archive`,
  decide: `${B}/malpractice-cases/{id}/decision`,
  review: `${B}/malpractice-cases/{id}/review`,
  approval: `${B}/malpractice-cases/{id}/approval`,
} as const satisfies Record<string, keyof paths>;

const SHEET_CMD: Record<SheetCommand, string> = {
  submit: PATHS.submit,
  verify: PATHS.verify,
  "request-approval": PATHS.requestApproval,
  finalise: PATHS.finalise,
};
const MOD_CMD: Record<"validate" | "apply" | "withdraw", string> = { validate: PATHS.mValidate, apply: PATHS.mApply, withdraw: PATHS.mWithdraw };
const CASE_CMD: Record<CaseCommand, string> = {
  investigate: PATHS.investigate,
  hearing: PATHS.hearing,
  appeal: PATHS.appeal,
  reopen: PATHS.reopen,
  close: PATHS.close,
  archive: PATHS.archive,
};

type Fetch = (input: string, init: RequestInit) => Promise<Response>;

interface RequestOptions {
  body?: unknown;
  ifMatch?: number;
  idempotent?: boolean;
}

export function createHttpMarksApi(fetchImpl: Fetch = (i, init) => fetch(i, init), baseUrl: string = import.meta.env.VITE_API_BASE ?? ""): MarksApi {
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
  return {
    myAssessments: () => items(PATHS.me),
    institutionPapers: (inst) => items(fill(PATHS.institution, { id: inst })),
    marks: (paper) => request("GET", fill(PATHS.marks, { id: paper })),
    enter: (paper, rows) => request("PUT", fill(PATHS.marks, { id: paper }), { body: { rows } }),
    openSheet: (paper) => request("POST", fill(PATHS.sheet, { id: paper }), { body: {}, idempotent: true }),
    importInternal: (sheet) => request("POST", fill(PATHS.importInternal, { id: sheet }), { body: {}, idempotent: true }),
    sheetCommand: (sheet, v, cmd) => request("POST", fill(SHEET_CMD[cmd], { id: sheet }), { body: {}, ifMatch: v }),
    decideSheet: (sheet, v, approve, reason) => request("POST", fill(PATHS.decision, { id: sheet }), { body: { decision: approve ? "approve" : "reject", reason }, ifMatch: v }),
    withhold: (mark, v, reason) => request("POST", fill(PATHS.withhold, { id: mark }), { body: { reason }, ifMatch: v }),
    release: (mark, v) => request("POST", fill(PATHS.release, { id: mark }), { body: {}, ifMatch: v }),
    gradebook: (paper) => request("GET", fill(PATHS.gradebook, { id: paper })),
    moderations: (paper) => items(fill(PATHS.moderation, { id: paper })),
    requestModeration: (paper, kind, value, reason) => request("POST", fill(PATHS.moderation, { id: paper }), { body: { kind, value, reason }, idempotent: true }),
    moderationCommand: (id, v, cmd) => request("POST", fill(MOD_CMD[cmd], { id }), { body: {}, ifMatch: v }),
    cases: (session) => items(fill(PATHS.cases, { id: session })),
    reportCase: (session, input) => request("POST", fill(PATHS.cases, { id: session }), { body: input, idempotent: true }),
    caseCommand: (id, v, cmd, reason) => request("POST", fill(CASE_CMD[cmd], { id }), { body: reason ? { reason } : {}, ifMatch: v }),
    decideCase: (id, v, penalty, reason, marksDeduction) => request("POST", fill(PATHS.decide, { id }), { body: { penalty, reason, marksDeduction }, ifMatch: v }),
    reviewCase: (id, v, outcome, reason, penalty) => request("POST", fill(PATHS.review, { id }), { body: { outcome, reason, penalty }, ifMatch: v }),
    approveCase: (id, v, approve, reason) => request("POST", fill(PATHS.approval, { id }), { body: { decision: approve ? "approve" : "reject", reason }, ifMatch: v }),
  };
}
