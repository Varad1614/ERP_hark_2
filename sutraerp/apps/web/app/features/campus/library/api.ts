/**
 * Slice-8C library API layer: one interface, an HTTP implementation (CSRF, `Idempotency-Key` on
 * creates and bulk runs, `If-Match` on state commands, problem+json → `ApiProblem`) and an
 * in-memory mock (`./mock`). Every path template is checked against the generated OpenAPI `paths`.
 */
import { ApiProblem, isProblem, isUnsafeMethod, newIdempotencyKey, type paths } from "@sutra/api-client";
import { getCsrfToken } from "../../../lib/api";
import { fill } from "../../institution/api";
import type {
  BulkResult,
  CatalogueRecord,
  Clearance,
  Copy,
  CopyCommand,
  CopyCondition,
  Decision,
  Fine,
  FineCommand,
  Items,
  LibraryComplaint,
  LibraryPolicy,
  LibraryReservation,
  Loan,
  LoanCommand,
  Membership,
  MembershipCommand,
  MyLibrary,
  NewCatalogueRecord,
  NewCopy,
  NewLibraryPolicy,
  NewLoan,
  NewMembership,
  PolicyCommand,
  ReservationCommand,
} from "./types";

export interface LibraryApi {
  // office
  policies(institutionId: string): Promise<LibraryPolicy[]>;
  createPolicy(institutionId: string, input: NewLibraryPolicy): Promise<LibraryPolicy>;
  policyCommand(id: string, version: number, cmd: PolicyCommand): Promise<LibraryPolicy>;
  decidePolicy(id: string, version: number, decision: Decision, reason?: string): Promise<LibraryPolicy>;
  memberships(institutionId: string): Promise<Membership[]>;
  createMembership(institutionId: string, input: NewMembership): Promise<Membership>;
  membershipCommand(id: string, version: number, cmd: MembershipCommand, reason?: string): Promise<Membership>;
  restrict(id: string, version: number, restricted: boolean, reason?: string): Promise<Membership>;
  catalogue(institutionId: string, q?: string): Promise<CatalogueRecord[]>;
  createRecord(institutionId: string, input: NewCatalogueRecord): Promise<CatalogueRecord>;
  publishRecord(id: string, version: number): Promise<CatalogueRecord>;
  withdrawRecord(id: string, version: number, reason?: string): Promise<CatalogueRecord>;
  copies(recordId: string): Promise<Copy[]>;
  addCopy(recordId: string, input: NewCopy): Promise<Copy>;
  copyCommand(id: string, version: number, cmd: CopyCommand, reason?: string): Promise<Copy>;
  copyCondition(id: string, version: number, condition: CopyCondition, reason?: string): Promise<Copy>;
  loans(institutionId: string, status?: string): Promise<Loan[]>;
  issue(institutionId: string, input: NewLoan): Promise<Loan>;
  loanCommand(id: string, version: number, cmd: LoanCommand): Promise<Loan>;
  overdueSweep(institutionId: string): Promise<BulkResult>;
  fines(institutionId: string, status?: string): Promise<Fine[]>;
  fineCommand(id: string, version: number, cmd: FineCommand, reason?: string): Promise<Fine>;
  decideFine(id: string, version: number, decision: Decision, reason?: string): Promise<Fine>;
  reservations(institutionId: string, status?: string): Promise<LibraryReservation[]>;
  reservationCommand(id: string, version: number, cmd: ReservationCommand, reason?: string): Promise<LibraryReservation>;
  clearances(institutionId: string, status?: string): Promise<Clearance[]>;
  requestClearance(institutionId: string, membershipId: string, purpose: string): Promise<Clearance>;
  decideClearance(id: string, version: number, decision: Decision, reason?: string): Promise<Clearance>;
  // self-service
  mine(): Promise<MyLibrary>;
  myCatalogue(q?: string): Promise<CatalogueRecord[]>;
  reserve(recordId: string): Promise<LibraryReservation>;
  renewMine(loanId: string, version: number): Promise<Loan>;
  cancelMine(reservationId: string, version: number): Promise<LibraryReservation>;
  myClearance(purpose: string): Promise<Clearance>;
  complain(title: string, description?: string, copyId?: string): Promise<LibraryComplaint>;
}

const B = "/api/v1/campus";
export const PATHS = {
  policies: `${B}/institutions/{id}/library-policies`,
  policySubmit: `${B}/library-policies/{id}/submit`,
  policyActivate: `${B}/library-policies/{id}/activate`,
  policyDecision: `${B}/library-policies/{id}/decision`,
  memberships: `${B}/institutions/{id}/library-memberships`,
  membershipSuspend: `${B}/library-memberships/{id}/suspend`,
  membershipRestore: `${B}/library-memberships/{id}/restore`,
  membershipClose: `${B}/library-memberships/{id}/close`,
  restriction: `${B}/library-memberships/{id}/restriction`,
  catalogue: `${B}/institutions/{id}/catalogue`,
  publish: `${B}/catalogue-records/{id}/publish`,
  withdraw: `${B}/catalogue-records/{id}/withdraw`,
  copies: `${B}/catalogue-records/{id}/copies`,
  copyLost: `${B}/library-copies/{id}/lost`,
  copyWithdraw: `${B}/library-copies/{id}/withdraw`,
  copyCondition: `${B}/library-copies/{id}/condition`,
  loans: `${B}/institutions/{id}/library-loans`,
  loanReturn: `${B}/library-loans/{id}/return`,
  loanRenew: `${B}/library-loans/{id}/renew`,
  sweep: `${B}/institutions/{id}/library-loans/overdue-sweep`,
  fines: `${B}/institutions/{id}/library-fines`,
  fineBill: `${B}/library-fines/{id}/bill`,
  fineWaiver: `${B}/library-fines/{id}/waiver`,
  fineDecision: `${B}/library-fines/{id}/decision`,
  reservations: `${B}/institutions/{id}/library-reservations`,
  reservationCollect: `${B}/library-reservations/{id}/collect`,
  reservationCancel: `${B}/library-reservations/{id}/cancel`,
  reservationExpire: `${B}/library-reservations/{id}/expire`,
  clearances: `${B}/institutions/{id}/library-clearances`,
  clearanceDecision: `${B}/library-clearances/{id}/decision`,
  mine: `${B}/me/library`,
  myCatalogue: `${B}/me/library/catalogue`,
  myReservations: `${B}/me/library/reservations`,
  myRenew: `${B}/me/library/loans/{id}/renew`,
  myCancel: `${B}/me/library/reservations/{id}/cancel`,
  myClearance: `${B}/me/library/clearance`,
  myComplaints: `${B}/me/library/complaints`,
} as const satisfies Record<string, keyof paths>;

const POLICY_CMD: Record<PolicyCommand, string> = { submit: PATHS.policySubmit, activate: PATHS.policyActivate };
const MEMBER_CMD: Record<MembershipCommand, string> = { suspend: PATHS.membershipSuspend, restore: PATHS.membershipRestore, close: PATHS.membershipClose };
const LOAN_CMD: Record<LoanCommand, string> = { return: PATHS.loanReturn, renew: PATHS.loanRenew };
const FINE_CMD: Record<FineCommand, string> = { bill: PATHS.fineBill, waiver: PATHS.fineWaiver };
const COPY_CMD: Record<CopyCommand, string> = { lost: PATHS.copyLost, withdraw: PATHS.copyWithdraw };
const RES_CMD: Record<ReservationCommand, string> = { collect: PATHS.reservationCollect, cancel: PATHS.reservationCancel, expire: PATHS.reservationExpire };
const reasonBody = (reason?: string) => ({ reason: reason || undefined });
const decisionBody = (decision: Decision, reason?: string) => ({ decision, reason: reason || undefined });

type Fetch = (input: string, init: RequestInit) => Promise<Response>;

interface RequestOptions {
  body?: unknown;
  ifMatch?: number;
  idempotent?: boolean;
}

const query = (params: Record<string, string | undefined>) => {
  const q = Object.entries(params)
    .filter(([, v]) => v !== undefined && v !== "")
    .map(([k, v]) => `${k}=${encodeURIComponent(String(v))}`)
    .join("&");
  return q ? `?${q}` : "";
};

export function createHttpLibraryApi(fetchImpl: Fetch = (i, init) => fetch(i, init), baseUrl: string = import.meta.env.VITE_API_BASE ?? ""): LibraryApi {
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
    policies: (inst) => items(id(PATHS.policies, inst)),
    createPolicy: (inst, input) => request("POST", id(PATHS.policies, inst), { body: input, idempotent: true }),
    policyCommand: (pid, v, cmd) => request("POST", id(POLICY_CMD[cmd], pid), { body: {}, ifMatch: v }),
    decidePolicy: (pid, v, decision, reason) => request("POST", id(PATHS.policyDecision, pid), { body: decisionBody(decision, reason), ifMatch: v }),
    memberships: (inst) => items(`${id(PATHS.memberships, inst)}${query({ limit: "200" })}`),
    createMembership: (inst, input) => request("POST", id(PATHS.memberships, inst), { body: input, idempotent: true }),
    membershipCommand: (mid, v, cmd, reason) => request("POST", id(MEMBER_CMD[cmd], mid), { body: reasonBody(reason), ifMatch: v }),
    restrict: (mid, v, restricted, reason) => request("POST", id(PATHS.restriction, mid), { body: { restricted, reason: reason || undefined }, ifMatch: v }),
    catalogue: (inst, q) => items(`${id(PATHS.catalogue, inst)}${query({ q, limit: "200" })}`),
    createRecord: (inst, input) => request("POST", id(PATHS.catalogue, inst), { body: input, idempotent: true }),
    publishRecord: (rid, v) => request("POST", id(PATHS.publish, rid), { body: {}, ifMatch: v }),
    withdrawRecord: (rid, v, reason) => request("POST", id(PATHS.withdraw, rid), { body: reasonBody(reason), ifMatch: v }),
    copies: (rid) => items(id(PATHS.copies, rid)),
    addCopy: (rid, input) => request("POST", id(PATHS.copies, rid), { body: input, idempotent: true }),
    copyCommand: (cid, v, cmd, reason) => request("POST", id(COPY_CMD[cmd], cid), { body: reasonBody(reason), ifMatch: v }),
    copyCondition: (cid, v, condition, reason) => request("POST", id(PATHS.copyCondition, cid), { body: { condition, reason: reason || undefined }, ifMatch: v }),
    loans: (inst, status) => items(`${id(PATHS.loans, inst)}${query({ status, limit: "200" })}`),
    issue: (inst, input) => request("POST", id(PATHS.loans, inst), { body: input, idempotent: true }),
    loanCommand: (lid, v, cmd) => request("POST", id(LOAN_CMD[cmd], lid), { body: {}, ifMatch: v }),
    overdueSweep: (inst) => request("POST", id(PATHS.sweep, inst), { body: {}, idempotent: true }),
    fines: (inst, status) => items(`${id(PATHS.fines, inst)}${query({ status, limit: "200" })}`),
    fineCommand: (fid, v, cmd, reason) => request("POST", id(FINE_CMD[cmd], fid), { body: reasonBody(reason), ifMatch: v }),
    decideFine: (fid, v, decision, reason) => request("POST", id(PATHS.fineDecision, fid), { body: decisionBody(decision, reason), ifMatch: v }),
    reservations: (inst, status) => items(`${id(PATHS.reservations, inst)}${query({ status })}`),
    reservationCommand: (rid, v, cmd, reason) => request("POST", id(RES_CMD[cmd], rid), { body: reasonBody(reason), ifMatch: v }),
    clearances: (inst, status) => items(`${id(PATHS.clearances, inst)}${query({ status })}`),
    requestClearance: (inst, membershipId, purpose) => request("POST", id(PATHS.clearances, inst), { body: { membershipId, purpose }, idempotent: true }),
    decideClearance: (cid, v, decision, reason) => request("POST", id(PATHS.clearanceDecision, cid), { body: decisionBody(decision, reason), ifMatch: v }),
    mine: () => request("GET", PATHS.mine),
    myCatalogue: (q) => items(`${PATHS.myCatalogue}${query({ q })}`),
    reserve: (recordId) => request("POST", PATHS.myReservations, { body: { recordId }, idempotent: true }),
    renewMine: (lid, v) => request("POST", id(PATHS.myRenew, lid), { body: {}, ifMatch: v }),
    cancelMine: (rid, v) => request("POST", id(PATHS.myCancel, rid), { body: {}, ifMatch: v }),
    myClearance: (purpose) => request("POST", PATHS.myClearance, { body: { purpose }, idempotent: true }),
    complain: (title, description, copyId) => request("POST", PATHS.myComplaints, { body: { title, description: description || undefined, copyId: copyId || undefined }, idempotent: true }),
  };
}
