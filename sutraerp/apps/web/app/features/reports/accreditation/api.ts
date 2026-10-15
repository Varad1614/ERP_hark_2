/**
 * Slice-9E accreditation (H09) API layer: one interface, an HTTP implementation (CSRF,
 * `Idempotency-Key` on creates, `If-Match` on state commands, problem+json → `ApiProblem`) and an
 * in-memory mock (`./mock`). Every path template is checked against the generated OpenAPI `paths`.
 */
import { ApiProblem, isProblem, isUnsafeMethod, newIdempotencyKey, type paths } from "@sutra/api-client";
import { getCsrfToken } from "../../../lib/api";
import { fill } from "../../institution/api";
import type {
  AccreditationCycle,
  AccreditationFramework,
  AccreditationItemProgress,
  AccreditationRecord,
  Decision,
  Items,
  NewAccreditationCycle,
  NewAccreditationFramework,
  NewAccreditationItem,
  NewAccreditationRecord,
  PlanEntry,
  RecordCommand,
} from "./types";

export interface AccreditationApi {
  // frameworks (H-061)
  frameworks(institutionId: string): Promise<AccreditationFramework[]>;
  createFramework(institutionId: string, input: NewAccreditationFramework): Promise<AccreditationFramework>;
  submitFramework(id: string, version: number): Promise<AccreditationFramework>;
  decideFramework(id: string, version: number, decision: Decision, reason?: string): Promise<AccreditationFramework>;
  // cycles + assessment plans (H-062, H-071)
  cycles(institutionId: string): Promise<AccreditationCycle[]>;
  createCycle(institutionId: string, input: NewAccreditationCycle): Promise<AccreditationCycle>;
  planCycle(id: string, version: number, entries: PlanEntry[]): Promise<AccreditationCycle>;
  submitCycle(id: string, version: number): Promise<AccreditationCycle>;
  decideCycle(id: string, version: number, decision: Decision, reason?: string): Promise<AccreditationCycle>;
  closeCycle(id: string, version: number): Promise<AccreditationCycle>;
  // records (H-063…071)
  records(institutionId: string): Promise<AccreditationRecord[]>;
  createRecord(institutionId: string, input: NewAccreditationRecord): Promise<AccreditationRecord>;
  recordCommand(id: string, version: number, cmd: RecordCommand, reason?: string): Promise<AccreditationRecord>;
  submitRecord(id: string, version: number, outcome: string, rationale?: string, grade?: string): Promise<AccreditationRecord>;
  decideRecord(id: string, version: number, decision: Decision, reason?: string): Promise<AccreditationRecord>;
  handoff(id: string, version: number, reference: string): Promise<AccreditationRecord>;
  addItem(recordId: string, input: NewAccreditationItem): Promise<AccreditationRecord>;
  progressItem(itemId: string, version: number, input: AccreditationItemProgress): Promise<AccreditationRecord>;
  verifyItem(itemId: string, version: number, evidenceRef?: string): Promise<AccreditationRecord>;
}

const B = "/api/v1/services";
export const PATHS = {
  frameworks: `${B}/institutions/{id}/accreditation-frameworks`,
  frameworkSubmit: `${B}/accreditation-frameworks/{id}/submit`,
  frameworkDecision: `${B}/accreditation-frameworks/{id}/decision`,
  cycles: `${B}/institutions/{id}/accreditation-cycles`,
  cyclePlan: `${B}/accreditation-cycles/{id}/plan`,
  cycleSubmit: `${B}/accreditation-cycles/{id}/submit`,
  cycleDecision: `${B}/accreditation-cycles/{id}/decision`,
  cycleClose: `${B}/accreditation-cycles/{id}/close`,
  records: `${B}/institutions/{id}/accreditation-records`,
  recordStart: `${B}/accreditation-records/{id}/start`,
  recordCancel: `${B}/accreditation-records/{id}/cancel`,
  recordSubmit: `${B}/accreditation-records/{id}/submit`,
  recordDecision: `${B}/accreditation-records/{id}/decision`,
  recordHandoff: `${B}/accreditation-records/{id}/handoff`,
  recordItems: `${B}/accreditation-records/{id}/items`,
  itemProgress: `${B}/accreditation-items/{id}/progress`,
  itemVerify: `${B}/accreditation-items/{id}/verify`,
} as const satisfies Record<string, keyof paths>;

const RECORD_CMD: Record<RecordCommand, string> = { start: PATHS.recordStart, cancel: PATHS.recordCancel };

type Fetch = (input: string, init: RequestInit) => Promise<Response>;

interface RequestOptions {
  body?: unknown;
  ifMatch?: number;
  idempotent?: boolean;
}

const decisionBody = (decision: Decision, reason?: string) => ({ decision, reason: reason || undefined });

export function createHttpAccreditationApi(fetchImpl: Fetch = (i, init) => fetch(i, init), baseUrl: string = import.meta.env.VITE_API_BASE ?? ""): AccreditationApi {
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
    frameworks: (inst) => items(`${id(PATHS.frameworks, inst)}?limit=200`),
    createFramework: (inst, input) => request("POST", id(PATHS.frameworks, inst), { body: input, idempotent: true }),
    submitFramework: (fid, v) => request("POST", id(PATHS.frameworkSubmit, fid), { body: {}, ifMatch: v }),
    decideFramework: (fid, v, decision, reason) => request("POST", id(PATHS.frameworkDecision, fid), { body: decisionBody(decision, reason), ifMatch: v }),
    cycles: (inst) => items(`${id(PATHS.cycles, inst)}?limit=200`),
    createCycle: (inst, input) => request("POST", id(PATHS.cycles, inst), { body: input, idempotent: true }),
    planCycle: (cid, v, entries) => request("POST", id(PATHS.cyclePlan, cid), { body: { entries }, ifMatch: v }),
    submitCycle: (cid, v) => request("POST", id(PATHS.cycleSubmit, cid), { body: {}, ifMatch: v }),
    decideCycle: (cid, v, decision, reason) => request("POST", id(PATHS.cycleDecision, cid), { body: decisionBody(decision, reason), ifMatch: v }),
    closeCycle: (cid, v) => request("POST", id(PATHS.cycleClose, cid), { body: {}, ifMatch: v }),
    records: (inst) => items(`${id(PATHS.records, inst)}?limit=200`),
    createRecord: (inst, input) => request("POST", id(PATHS.records, inst), { body: input, idempotent: true }),
    recordCommand: (rid, v, cmd, reason) => request("POST", id(RECORD_CMD[cmd], rid), { body: { reason: reason || undefined }, ifMatch: v }),
    submitRecord: (rid, v, outcome, rationale, grade) => request("POST", id(PATHS.recordSubmit, rid), { body: { outcome, rationale: rationale || undefined, grade: grade || undefined }, ifMatch: v }),
    decideRecord: (rid, v, decision, reason) => request("POST", id(PATHS.recordDecision, rid), { body: decisionBody(decision, reason), ifMatch: v }),
    handoff: (rid, v, reference) => request("POST", id(PATHS.recordHandoff, rid), { body: { reference }, ifMatch: v }),
    addItem: (rid, input) => request("POST", id(PATHS.recordItems, rid), { body: input, idempotent: true }),
    progressItem: (iid, v, input) => request("POST", id(PATHS.itemProgress, iid), { body: input, ifMatch: v }),
    verifyItem: (iid, v, evidenceRef) => request("POST", id(PATHS.itemVerify, iid), { body: { evidenceRef: evidenceRef || undefined }, ifMatch: v }),
  };
}
