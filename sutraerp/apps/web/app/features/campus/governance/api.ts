/**
 * Slice-9E campus governance (G00) API layer: one interface, an HTTP implementation (CSRF,
 * `Idempotency-Key` on creates, `If-Match` on state commands, problem+json → `ApiProblem`) and an
 * in-memory mock (`./mock`). Every path template is checked against the generated OpenAPI `paths`.
 */
import { ApiProblem, isProblem, isUnsafeMethod, newIdempotencyKey, type paths } from "@sutra/api-client";
import { getCsrfToken } from "../../../lib/api";
import { fill } from "../../institution/api";
import type {
  CaseCommand,
  Decision,
  GovernanceCase,
  GovernanceConfigKey,
  GovernanceConfigVersion,
  GovernanceEvidence,
  GovernanceItemProgress,
  GovernancePolicyView,
  Items,
  NewGovernanceCase,
  NewGovernanceItem,
} from "./types";

export interface GovernanceApi {
  // cases (G-001, 003…013)
  cases(institutionId: string): Promise<GovernanceCase[]>;
  createCase(institutionId: string, input: NewGovernanceCase): Promise<GovernanceCase>;
  caseCommand(id: string, version: number, cmd: CaseCommand, reason?: string): Promise<GovernanceCase>;
  submitCase(id: string, version: number, outcome: string, rationale?: string, residualScore?: number): Promise<GovernanceCase>;
  decideCase(id: string, version: number, decision: Decision, reason?: string): Promise<GovernanceCase>;
  escalate(id: string, title: string, reason: string): Promise<GovernanceCase>;
  addItem(caseId: string, input: NewGovernanceItem): Promise<GovernanceCase>;
  progressItem(itemId: string, version: number, input: GovernanceItemProgress): Promise<GovernanceCase>;
  verifyItem(itemId: string, version: number, evidenceRef?: string): Promise<GovernanceCase>;
  // configuration (G-014)
  configs(institutionId: string): Promise<GovernanceConfigVersion[]>;
  createConfig(institutionId: string, key: GovernanceConfigKey, settings: Record<string, unknown>): Promise<GovernanceConfigVersion>;
  submitConfig(id: string, version: number): Promise<GovernanceConfigVersion>;
  decideConfig(id: string, version: number, decision: Decision, reason?: string): Promise<GovernanceConfigVersion>;
  policy(institutionId: string): Promise<GovernancePolicyView>;
  // evidence (G-002, G-015)
  evidence(institutionId: string): Promise<GovernanceEvidence[]>;
  createEvidence(institutionId: string, caseId: string, title?: string, summary?: string): Promise<GovernanceEvidence>;
  publishEvidence(id: string, version: number): Promise<GovernanceEvidence>;
}

const B = "/api/v1/campus";
export const PATHS = {
  cases: `${B}/institutions/{id}/governance-cases`,
  case: `${B}/governance-cases/{id}`,
  caseStart: `${B}/governance-cases/{id}/start`,
  caseRefresh: `${B}/governance-cases/{id}/refresh`,
  caseCancel: `${B}/governance-cases/{id}/cancel`,
  caseSubmit: `${B}/governance-cases/{id}/submit`,
  caseDecision: `${B}/governance-cases/{id}/decision`,
  caseEscalate: `${B}/governance-cases/{id}/escalate`,
  caseItems: `${B}/governance-cases/{id}/items`,
  itemProgress: `${B}/governance-items/{id}/progress`,
  itemVerify: `${B}/governance-items/{id}/verify`,
  configs: `${B}/institutions/{id}/governance-configs`,
  configSubmit: `${B}/governance-configs/{id}/submit`,
  configDecision: `${B}/governance-configs/{id}/decision`,
  policy: `${B}/institutions/{id}/governance-policy`,
  evidence: `${B}/institutions/{id}/governance-evidence`,
  evidencePublish: `${B}/governance-evidence/{id}/publish`,
} as const satisfies Record<string, keyof paths>;

const CASE_CMD: Record<CaseCommand, string> = { start: PATHS.caseStart, refresh: PATHS.caseRefresh, cancel: PATHS.caseCancel };

type Fetch = (input: string, init: RequestInit) => Promise<Response>;

interface RequestOptions {
  body?: unknown;
  ifMatch?: number;
  idempotent?: boolean;
}

const reasonBody = (reason?: string) => ({ reason: reason || undefined });

export function createHttpGovernanceApi(fetchImpl: Fetch = (i, init) => fetch(i, init), baseUrl: string = import.meta.env.VITE_API_BASE ?? ""): GovernanceApi {
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
  const id = (template: string, value: string) => fill(template, { id: value });
  return {
    cases: (inst) => items(`${id(PATHS.cases, inst)}?limit=200`),
    createCase: (inst, input) => request("POST", id(PATHS.cases, inst), { body: input, idempotent: true }),
    caseCommand: (cid, v, cmd, reason) => request("POST", id(CASE_CMD[cmd], cid), { body: reasonBody(reason), ifMatch: v }),
    submitCase: (cid, v, outcome, rationale, residualScore) =>
      request("POST", id(PATHS.caseSubmit, cid), { body: { outcome, rationale: rationale || undefined, residualScore }, ifMatch: v }),
    decideCase: (cid, v, decision, reason) => request("POST", id(PATHS.caseDecision, cid), { body: { decision, reason: reason || undefined }, ifMatch: v }),
    escalate: (cid, title, reason) => request("POST", id(PATHS.caseEscalate, cid), { body: { title, reason }, idempotent: true }),
    addItem: (cid, input) => request("POST", id(PATHS.caseItems, cid), { body: input, idempotent: true }),
    progressItem: (iid, v, input) => request("POST", id(PATHS.itemProgress, iid), { body: input, ifMatch: v }),
    verifyItem: (iid, v, evidenceRef) => request("POST", id(PATHS.itemVerify, iid), { body: { evidenceRef: evidenceRef || undefined }, ifMatch: v }),
    configs: (inst) => items(`${id(PATHS.configs, inst)}?limit=200`),
    createConfig: (inst, key, settings) => request("POST", id(PATHS.configs, inst), { body: { key, settings }, idempotent: true }),
    submitConfig: (cid, v) => request("POST", id(PATHS.configSubmit, cid), { body: {}, ifMatch: v }),
    decideConfig: (cid, v, decision, reason) => request("POST", id(PATHS.configDecision, cid), { body: { decision, reason: reason || undefined }, ifMatch: v }),
    policy: (inst) => request("GET", id(PATHS.policy, inst)),
    evidence: (inst) => items(`${id(PATHS.evidence, inst)}?limit=200`),
    createEvidence: (inst, caseId, title, summary) => request("POST", id(PATHS.evidence, inst), { body: { caseId, title: title || undefined, summary: summary || undefined }, idempotent: true }),
    publishEvidence: (eid, v) => request("POST", id(PATHS.evidencePublish, eid), { body: {}, ifMatch: v }),
  };
}
