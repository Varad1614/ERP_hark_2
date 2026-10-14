/**
 * In-memory stand-in of the slice-8C library API (mock mode and tests): an active policy, two
 * members, two published titles with copies, one loan out, one overdue loan with its fine and a
 * reservation. Legal transitions only (409 otherwise): publish a draft, return / renew a loan once
 * per rule, bill only a student's assessed fine, waive only before billing.
 */
import { ApiProblem } from "@sutra/api-client";
import type { LibraryApi } from "./api";
import type { CatalogueRecord, Clearance, Copy, Fine, LibraryPolicy, LibraryReservation, Loan, Membership, MyLibrary } from "./types";

const conflict = (detail: string) => new ApiProblem({ type: "about:blank", title: "Conflict", status: 409, errorCode: "conflict", detail });
const notFound = () => new ApiProblem({ type: "about:blank", title: "Not found", status: 404, errorCode: "not-found" });

const INST = "00000000-0000-7000-8000-0000000b8c01";
let seq = 300;
const uid = () => `00000000-0000-7000-8000-${String(++seq).padStart(12, "0")}`;
const day = (offset: number) => new Date(Date.now() + offset * 86_400_000).toISOString().slice(0, 10);

export function createMockLibraryApi(): LibraryApi {
  const policies: LibraryPolicy[] = [
    {
      id: uid(), institutionId: INST, name: "Library rules 2026", effectiveFrom: "2026-06-01", loanDaysStudent: 14, loanDaysStaff: 30, maxLoansStudent: 3,
      maxLoansStaff: 6, maxRenewals: 1, finePerDay: "2.00", maxFine: "100.00", reservationHoldDays: 3, lostChargeFactor: "1.5000", status: "active",
      approvalState: "approved", version: 3,
    },
  ];
  const members: Membership[] = [
    { id: uid(), institutionId: INST, number: "LM-000001", personKind: "student", personId: uid(), personNumber: "S2026-0001", displayName: "Aarav Desai", status: "active", activeLoans: 2, version: 1 },
    { id: uid(), institutionId: INST, number: "LM-000002", personKind: "staff", personId: uid(), personNumber: "ST-0001", displayName: "Meera Shah", status: "active", activeLoans: 0, version: 1 },
  ];
  const record = (title: string, authors: string[], copies: number, available: number): CatalogueRecord => ({
    id: uid(), institutionId: INST, title, authors, subjects: ["Engineering"], resourceType: "book", replacementCost: "450.00", status: "published",
    copies, availableCopies: available, queuedReservations: 0, version: 2,
  });
  const records: CatalogueRecord[] = [record("Fluid Mechanics", ["F. M. White"], 2, 1), record("Engineering Thermodynamics", ["P. K. Nag"], 1, 0)];
  const copies: Copy[] = [
    { id: uid(), institutionId: INST, recordId: records[0]!.id, title: records[0]!.title, accessionNumber: "ACC-000001", condition: "good", status: "issued", nodeCode: "MAIN-LIB", version: 2 },
    { id: uid(), institutionId: INST, recordId: records[0]!.id, title: records[0]!.title, accessionNumber: "ACC-000002", condition: "good", status: "available", nodeCode: "MAIN-LIB", version: 1 },
    { id: uid(), institutionId: INST, recordId: records[1]!.id, title: records[1]!.title, accessionNumber: "ACC-000003", condition: "fair", status: "issued", nodeCode: "MAIN-LIB", version: 2 },
  ];
  const aarav = members[0]!;
  const loan = (c: Copy, n: number, issued: number, due: number, status: Loan["status"], fine?: string): Loan => ({
    id: uid(), institutionId: INST, number: `LN-00000${n}`, copyId: c.id, accessionNumber: c.accessionNumber, recordId: c.recordId, title: c.title,
    memberId: aarav.id, memberNumber: aarav.number, displayName: aarav.displayName, personKind: "student", personId: aarav.personId,
    issuedOn: day(issued), dueOn: day(due), renewals: 0, status, fineAmount: fine ?? null, version: 1,
  });
  const loans: Loan[] = [loan(copies[0]!, 1, -3, 11, "active"), loan(copies[2]!, 2, -20, -6, "overdue", "12.00")];
  const fines: Fine[] = [
    {
      id: uid(), institutionId: INST, membershipId: aarav.id, memberNumber: aarav.number, displayName: aarav.displayName, personKind: "student", personId: aarav.personId,
      kind: "overdue", days: 6, amount: "12.00", status: "assessed", assessedOn: day(0), loanId: loans[1]!.id, loanNumber: loans[1]!.number, version: 1,
    },
  ];
  const meera = members[1]!;
  const reservations: LibraryReservation[] = [
    {
      id: uid(), institutionId: INST, recordId: records[1]!.id, title: records[1]!.title, membershipId: meera.id, memberNumber: meera.number, displayName: meera.displayName,
      personKind: "staff", queuePosition: 1, currentPosition: 1, status: "queued", version: 1,
    },
  ];
  records[1]!.queuedReservations = 1;
  const clearances: Clearance[] = [];
  const unsettled = (membershipId: string) => fines.filter((f) => f.membershipId === membershipId && ["assessed", "billed", "waiver_requested"].includes(f.status));
  const check = (m: Membership): Clearance["verification"] => {
    const activeLoans = loans.filter((l) => l.memberId === m.id && (l.status === "active" || l.status === "overdue")).length;
    const owed = unsettled(m.id).reduce((sum, f) => sum + Math.round(Number(f.amount) * 100), 0);
    const live = reservations.filter((r) => r.membershipId === m.id && (r.status === "queued" || r.status === "ready")).length;
    const blockers = [...(activeLoans ? [`${activeLoans} loan(s) still out`] : []), ...(owed ? ["unsettled library fines"] : [])];
    return { activeLoans, liveReservations: live, unsettledFines: (owed / 100).toFixed(2), blockers };
  };
  const clearance = (m: Membership, purpose: string): Clearance => {
    const verification = check(m);
    const c: Clearance = {
      id: uid(), institutionId: INST, membershipId: m.id, memberNumber: m.number, displayName: m.displayName, personKind: m.personKind, purpose,
      status: verification.blockers.length ? "rejected" : "approved", statusReason: verification.blockers.length ? verification.blockers.join("; ") : null,
      verification, createdAt: new Date().toISOString(), version: 1,
    };
    clearances.unshift(c);
    return c;
  };

  const find = <T extends { id: string; version: number }>(list: T[], id: string, version?: number): T => {
    const x = list.find((i) => i.id === id);
    if (!x) throw notFound();
    if (version !== undefined && x.version !== version) throw new ApiProblem({ type: "about:blank", title: "Precondition failed", status: 412, errorCode: "precondition-failed" });
    return x;
  };
  const bump = <T extends { version: number }>(x: T, patch: Partial<T>): T => Object.assign(x, patch, { version: x.version + 1 });

  const mine = (): MyLibrary => ({
    personKind: "student",
    personNumber: aarav.personNumber,
    displayName: aarav.displayName,
    membership: aarav,
    loans: loans.filter((l) => l.memberId === aarav.id && (l.status === "active" || l.status === "overdue")),
    reservations: reservations.filter((r) => r.membershipId === aarav.id),
    fines: fines.filter((f) => f.membershipId === aarav.id),
    eligibility: { membershipId: aarav.id, eligible: aarav.status === "active", activeLoans: aarav.activeLoans, maxLoans: 3, blockers: [], unsettledFines: check(aarav).unsettledFines },
    clearance: clearances.find((c) => c.membershipId === aarav.id) ?? null,
  });

  return {
    policies: async () => [...policies],
    createPolicy: async (_inst, input) => {
      const p: LibraryPolicy = { ...input, id: uid(), institutionId: INST, status: "draft", approvalState: "not_submitted", version: 1 };
      policies.unshift(p);
      return p;
    },
    policyCommand: async (id, v, cmd) => {
      const p = find(policies, id, v);
      if (cmd === "submit") {
        if (p.status !== "draft") throw conflict("only a draft policy is submitted");
        return bump(p, { approvalState: "pending" });
      }
      if (p.status !== "draft" || p.approvalState !== "approved") throw conflict("only an approved draft is activated");
      for (const o of policies) if (o.status === "active") bump(o, { status: "superseded" });
      return bump(p, { status: "active" });
    },
    decidePolicy: async (id, v, decision, reason) => {
      const p = find(policies, id, v);
      if (p.approvalState !== "pending") throw conflict("the policy is not awaiting a decision");
      return bump(p, { approvalState: decision === "approve" ? "approved" : "rejected", statusReason: reason ?? null });
    },
    memberships: async () => [...members],
    createMembership: async (_inst, input) => {
      if (members.some((m) => m.personId === input.personId && m.status !== "closed")) throw conflict("one live membership per person");
      const m: Membership = { id: uid(), institutionId: INST, number: `LM-${String(members.length + 1).padStart(6, "0")}`, personKind: input.personKind, personId: input.personId, personNumber: "—", displayName: "New member", status: "active", activeLoans: 0, version: 1 };
      members.push(m);
      return m;
    },
    membershipCommand: async (id, v, cmd, reason) => {
      const m = find(members, id, v);
      const next = cmd === "suspend" ? "suspended" : cmd === "restore" ? "active" : "closed";
      return bump(m, { status: next, statusReason: reason ?? null });
    },
    restrict: async (id, v, restricted, reason) => {
      const m = find(members, id, v);
      if (m.status !== "active") throw conflict("only an active membership is restricted");
      return bump(m, { restrictionReason: restricted ? (reason ?? "Restricted") : null });
    },
    catalogue: async (_inst, q) => records.filter((r) => !q || r.title.toLowerCase().includes(q.toLowerCase())),
    createRecord: async (_inst, input) => {
      const r: CatalogueRecord = {
        id: uid(), institutionId: INST, title: input.title, authors: input.authors ?? [], subjects: input.subjects ?? [], resourceType: "book",
        replacementCost: input.replacementCost ?? "0.00", status: "draft", copies: 0, availableCopies: 0, queuedReservations: 0, version: 1,
      };
      records.unshift(r);
      return r;
    },
    publishRecord: async (id, v) => {
      const r = find(records, id, v);
      if (r.status !== "draft") throw conflict("only a draft record is published");
      return bump(r, { status: "published" });
    },
    withdrawRecord: async (id, v, reason) => {
      const r = find(records, id, v);
      if (r.status === "withdrawn") throw conflict("already withdrawn");
      return bump(r, { status: "withdrawn", statusReason: reason ?? null });
    },
    copies: async (recordId) => copies.filter((c) => c.recordId === recordId),
    addCopy: async (recordId, input) => {
      const r = find(records, recordId);
      const c: Copy = { id: uid(), institutionId: INST, recordId, title: r.title, accessionNumber: `ACC-${String(copies.length + 1).padStart(6, "0")}`, condition: "good", status: "available", shelfMark: input.shelfMark ?? null, version: 1 };
      copies.push(c);
      bump(r, { copies: r.copies + 1, availableCopies: r.availableCopies + 1 });
      return c;
    },
    copyCommand: async (id, v, cmd, reason) => {
      const c = find(copies, id, v);
      const r = find(records, c.recordId);
      if (cmd === "withdraw") {
        if (c.status !== "available" && c.status !== "damaged") throw conflict(`a ${c.status} copy is not withdrawn`);
        if (c.status === "available") bump(r, { availableCopies: r.availableCopies - 1 });
        return bump(c, { status: "withdrawn", statusReason: reason ?? null });
      }
      if (c.status !== "issued" && c.status !== "available") throw conflict(`a ${c.status} copy cannot be lost`);
      const l = loans.find((x) => x.copyId === c.id && (x.status === "active" || x.status === "overdue"));
      if (l) {
        bump(l, { status: "lost" });
        fines.push({
          id: uid(), institutionId: INST, membershipId: l.memberId, memberNumber: l.memberNumber, displayName: l.displayName, personKind: l.personKind, personId: l.personId,
          kind: "replacement", days: 0, amount: (Number(r.replacementCost) * 1.5).toFixed(2), status: "assessed", assessedOn: day(0), loanId: l.id, loanNumber: l.number, version: 1,
        });
      } else bump(r, { availableCopies: r.availableCopies - 1 });
      return bump(c, { status: "lost", statusReason: reason ?? null });
    },
    copyCondition: async (id, v, condition, reason) => {
      const c = find(copies, id, v);
      return bump(c, { condition, status: condition === "damaged" && c.status === "available" ? "damaged" : c.status, statusReason: reason ?? null });
    },
    loans: async (_inst, status) => loans.filter((l) => !status || l.status === status),
    issue: async (_inst, input) => {
      const m = find(members, input.membershipId);
      const c = copies.find((x) => x.id === input.copyId || x.accessionNumber === input.accessionNumber);
      if (!c) throw notFound();
      if (c.status !== "available") throw conflict(`copy ${c.accessionNumber} is ${c.status}`);
      bump(c, { status: "issued" });
      const l: Loan = {
        id: uid(), institutionId: INST, number: `LN-${String(loans.length + 1).padStart(6, "0")}`, copyId: c.id, accessionNumber: c.accessionNumber, recordId: c.recordId,
        title: c.title, memberId: m.id, memberNumber: m.number, displayName: m.displayName, personKind: m.personKind, personId: m.personId, issuedOn: day(0),
        dueOn: day(m.personKind === "staff" ? 30 : 14), renewals: 0, status: "active", version: 1,
      };
      loans.unshift(l);
      return l;
    },
    loanCommand: async (id, v, cmd) => {
      const l = find(loans, id, v);
      if (l.status !== "active" && l.status !== "overdue") throw conflict("the loan is closed");
      if (cmd === "renew") {
        if (l.status === "overdue") throw conflict("an overdue loan is not renewed");
        if (l.renewals >= 1) throw conflict("no renewals left");
        return bump(l, { renewals: l.renewals + 1, dueOn: day(14) });
      }
      const c = copies.find((x) => x.id === l.copyId);
      if (c) bump(c, { status: "available" });
      return bump(l, { status: "returned", returnedOn: day(0) });
    },
    overdueSweep: async () => ({ processed: loans.filter((l) => l.status === "overdue").length, skipped: 0, failed: [] }),
    fines: async (_inst, status) => fines.filter((f) => !status || f.status === status),
    fineCommand: async (id, v, cmd, reason) => {
      const f = find(fines, id, v);
      if (f.status !== "assessed") throw conflict(`the fine is ${f.status}`);
      if (cmd === "bill") {
        if (f.personKind !== "student") throw conflict("only students are billed through Finance; staff fines stay on the library record");
        return bump(f, { status: "billed", chargeNumber: "CHG-000101", statusReason: null });
      }
      return bump(f, { status: "waived", statusReason: reason ?? null });
    },
    decideFine: async (id, v, decision, reason) => {
      const f = find(fines, id, v);
      if (f.status !== "waiver_requested") throw conflict("no waiver awaits a decision");
      return bump(f, { status: decision === "approve" ? "waived" : "assessed", statusReason: reason ?? null });
    },
    reservations: async (_inst, status) => reservations.filter((r) => !status || r.status === status),
    reservationCommand: async (id, v, cmd, reason) => {
      const x = find(reservations, id, v);
      const allowed = cmd === "cancel" ? ["queued", "ready"] : ["ready"];
      if (!allowed.includes(x.status)) throw conflict(`a ${x.status} reservation cannot be ${cmd === "collect" ? "collected" : cmd === "cancel" ? "cancelled" : "expired"}`);
      const r = find(records, x.recordId);
      if (x.status === "queued") bump(r, { queuedReservations: Math.max(0, r.queuedReservations - 1) });
      return bump(x, { status: cmd === "collect" ? "collected" : cmd === "cancel" ? "cancelled" : "expired", statusReason: reason ?? null });
    },
    clearances: async (_inst, status) => clearances.filter((c) => !status || c.status === status),
    requestClearance: async (_inst, membershipId, purpose) => clearance(find(members, membershipId), purpose),
    decideClearance: async (id, v, decision, reason) => {
      const c = find(clearances, id, v);
      if (c.status !== "requested") throw conflict("the clearance is decided");
      return bump(c, { status: decision === "approve" ? "approved" : "rejected", statusReason: reason ?? null, decidedAt: new Date().toISOString() });
    },
    mine: async () => mine(),
    myCatalogue: async (q) => records.filter((r) => r.status === "published" && (!q || r.title.toLowerCase().includes(q.toLowerCase()))),
    reserve: async (recordId) => {
      const r = find(records, recordId);
      if (reservations.some((x) => x.recordId === recordId && x.membershipId === aarav.id && (x.status === "queued" || x.status === "ready"))) throw conflict("already reserved");
      const x: LibraryReservation = {
        id: uid(), institutionId: INST, recordId, title: r.title, membershipId: aarav.id, memberNumber: aarav.number, displayName: aarav.displayName, personKind: "student",
        queuePosition: r.queuedReservations + 1, status: "queued", version: 1,
      };
      reservations.push(x);
      bump(r, { queuedReservations: r.queuedReservations + 1 });
      return x;
    },
    renewMine: async (id, v) => {
      const l = find(loans, id, v);
      if (l.status !== "active") throw conflict("an overdue loan is not renewed");
      if (l.renewals >= 1) throw conflict("no renewals left");
      return bump(l, { renewals: l.renewals + 1, dueOn: day(14) });
    },
    cancelMine: async (id, v) => {
      const x = find(reservations, id, v);
      if (x.status !== "queued" && x.status !== "ready") throw conflict(`a ${x.status} reservation cannot be cancelled`);
      return bump(x, { status: "cancelled" });
    },
    myClearance: async (purpose) => clearance(aarav, purpose),
    complain: async (title) => ({ ticketId: uid(), number: "SR-000101", title, status: "open", slaDueAt: new Date(Date.now() + 48 * 3_600_000).toISOString() }),
  };
}

