/**
 * In-memory stand-in of the slice-5B API (mock mode and tests). Money is handled as integer
 * paise (BigInt) parsed from the decimal strings — never floats. The mock gateway follows the
 * server's deterministic rule: paise `.13` fails, `.99` stays pending, anything else captures.
 */
import { ApiProblem } from "@sutra/api-client";
import type { PaymentsApi, RefundCommand } from "./api";
import type { AccountSummary, Collection, MyAccount, Payment, Receipt, Receivable, Refund, RefundStatus, UnidentifiedReceipt } from "./types";

export function toPaise(s: string): bigint {
  const neg = s.startsWith("-");
  const [i = "0", f = ""] = s.replace(/^[-+]/, "").split(".");
  const v = BigInt(i || "0") * 100n + BigInt((f + "00").slice(0, 2));
  return neg ? -v : v;
}

export function fromPaise(p: bigint): string {
  const neg = p < 0n;
  const a = neg ? -p : p;
  return `${neg ? "-" : ""}${a / 100n}.${String(a % 100n).padStart(2, "0")}`;
}

const conflict = (detail: string) => new ApiProblem({ type: "about:blank", title: "Conflict", status: 409, errorCode: "conflict", detail });

export function createMockPaymentsApi(): PaymentsApi {
  let n = 0;
  const id = () => `00000000-0000-7000-8000-${String(++n).padStart(12, "0")}`;
  const now = () => new Date().toISOString();
  const today = () => now().slice(0, 10);
  const account: AccountSummary = { id: "00000000-0000-7000-8000-00000000a001", displayName: "Aarav Desai", studentNumber: "S-0001", balance: { outstanding: "125000.00", overdue: "0.00", totalSettled: "0.00", currency: "INR" } };
  const charge = { id: "00000000-0000-7000-8000-00000000c001", chargeNumber: "CHG-000001", description: "Tuition fee — term 1", outstanding: toPaise("125000.00"), dueDate: "2026-12-31", status: "published" };
  const payments: Payment[] = [];
  const receipts: Receipt[] = [];
  const refunds: Refund[] = [];
  const unidentified: UnidentifiedReceipt[] = [];
  const collections: Collection[] = [];
  const receivables: Receivable[] = [];
  const credit = () => payments.filter((p) => p.status === "completed").reduce((s, p) => s + toPaise(p.unappliedAmount), 0n);

  function syncAccount() {
    account.balance.outstanding = fromPaise(charge.outstanding);
    account.balance.totalSettled = fromPaise(toPaise("125000.00") - charge.outstanding);
  }

  function complete(p: Payment) {
    const take = toPaise(p.amount) < charge.outstanding ? toPaise(p.amount) : charge.outstanding;
    if (take > 0n) {
      charge.outstanding -= take;
      charge.status = "active";
      p.allocations.push({ id: id(), chargeId: charge.id, chargeNumber: charge.chargeNumber, amount: fromPaise(take), source: "capture", status: "allocated" });
    }
    p.unappliedAmount = fromPaise(toPaise(p.amount) - take);
    p.status = "completed";
    p.completedAt = now();
    p.receipt = { id: id(), receiptNumber: `RCT-${String(receipts.length + 1).padStart(6, "0")}`, status: "issued", issuedAt: now() };
    receipts.push({ id: p.receipt.id, receiptNumber: p.receipt.receiptNumber, paymentId: p.id, paymentNumber: p.paymentNumber, studentAccountId: account.id, accountName: account.displayName, studentNumber: account.studentNumber, channel: p.channel, amount: p.amount, currency: "INR", status: "issued", issuedAt: now(), allocations: p.allocations, unappliedAmount: p.unappliedAmount, amountDisplay: p.amount });
    p.version += 1;
    syncAccount();
  }

  function newPayment(channel: Payment["channel"], amount: string, status: Payment["status"]): Payment {
    const p: Payment = { id: id(), paymentNumber: `PAY-${String(payments.length + 1).padStart(6, "0")}`, institutionId: "inst", studentAccountId: account.id, accountName: account.displayName, channel, payerKind: "student", amount, currency: "INR", status, receivedOn: today(), allocations: [], unappliedAmount: "0.00", createdAt: now(), version: 1 };
    payments.unshift(p);
    return p;
  }

  function refundTo(r: Refund, to: RefundStatus, reason?: string) {
    r.history.push({ fromStatus: r.status, toStatus: to, reason: reason ?? null, at: now() });
    r.status = to;
    r.version += 1;
  }

  function newRefund(input: { amount: string; reason: string }, via: "self" | "staff"): Refund {
    if (toPaise(input.amount) > credit()) throw conflict(`only ${fromPaise(credit())} of unapplied credit is available for a refund`);
    const r: Refund = { id: id(), refundNumber: `RFD-${String(refunds.length + 1).padStart(6, "0")}`, studentAccountId: account.id, accountName: account.displayName, amount: input.amount, currency: "INR", reason: input.reason, method: "bank_transfer", status: "requested", requestedVia: via, attempts: 0, history: [{ toStatus: "requested", at: now() }], createdAt: now(), version: 1, processInstanceId: "noop-1", decidedBy: "system:auto-approval" };
    refundTo(r, "under_review");
    refundTo(r, "approved", "auto-approved");
    let left = toPaise(input.amount);
    for (const p of payments.filter((x) => x.status === "completed")) {
      const avail = toPaise(p.unappliedAmount);
      const take = avail < left ? avail : left;
      p.unappliedAmount = fromPaise(avail - take);
      left -= take;
    }
    refunds.unshift(r);
    return r;
  }

  const find = <T extends { id: string }>(xs: T[], i: string): T => {
    const x = xs.find((y) => y.id === i);
    if (!x) throw new ApiProblem({ type: "about:blank", title: "Not found", status: 404, errorCode: "not-found" });
    return x;
  };

  return {
    accounts: async () => [account],
    payments: async () => payments,
    recordPayment: async (_acc, input) => {
      const p = newPayment(input.channel, input.amount, "initiated");
      p.externalRef = input.externalRef ?? null;
      complete(p);
      return p;
    },
    allocate: async (pid) => find(payments, pid),
    reversePayment: async (pid, _v, reason) => {
      const p = find(payments, pid);
      if (p.status !== "completed") throw conflict(`payment cannot move from ${p.status} to reversed`);
      for (const a of p.allocations) {
        charge.outstanding += toPaise(a.amount);
        a.status = "reversed";
      }
      p.status = "reversed";
      p.statusReason = reason;
      p.unappliedAmount = "0.00";
      if (p.receipt) p.receipt.status = "cancelled";
      p.version += 1;
      syncAccount();
      return p;
    },
    receipt: async (pid) => {
      const r = receipts.find((x) => x.paymentId === pid);
      if (!r) throw new ApiProblem({ type: "about:blank", title: "Not found", status: 404, errorCode: "not-found" });
      return r;
    },
    unidentified: async () => unidentified,
    recordUnidentified: async (_inst, input) => {
      const u: UnidentifiedReceipt = { id: id(), channel: input.channel as UnidentifiedReceipt["channel"], amount: input.amount, receivedOn: today(), payerName: input.payerName ?? null, externalRef: input.externalRef ?? null, status: "unidentified", version: 1 };
      unidentified.unshift(u);
      return u;
    },
    identify: async (uid, _v, acc) => {
      const u = find(unidentified, uid);
      const p = newPayment(u.channel, u.amount, "completed");
      p.unappliedAmount = u.amount;
      Object.assign(u, { status: "identified", studentAccountId: acc, paymentId: p.id, version: u.version + 1 });
      return u;
    },
    allocateUnidentified: async (uid) => {
      const u = find(unidentified, uid);
      const p = find(payments, u.paymentId ?? "");
      const take = toPaise(p.unappliedAmount) < charge.outstanding ? toPaise(p.unappliedAmount) : charge.outstanding;
      charge.outstanding -= take;
      p.unappliedAmount = fromPaise(toPaise(p.unappliedAmount) - take);
      if (p.unappliedAmount === "0.00") u.status = "allocated";
      u.version += 1;
      syncAccount();
      return u;
    },
    collections: async () => collections,
    refunds: async () => refunds,
    requestRefund: async (_acc, input) => newRefund(input, "staff"),
    decideRefund: async (rid, _v, approve, reason) => {
      const r = find(refunds, rid);
      if (r.status !== "under_review") throw conflict(`refund cannot move from ${r.status}`);
      refundTo(r, approve ? "approved" : "rejected", reason);
      return r;
    },
    refundCommand: async (rid, _v, cmd: RefundCommand, reason) => {
      const r = find(refunds, rid);
      const next: Record<RefundCommand, RefundStatus> = { process: "processing", retry: "processing", complete: "completed", cancel: "cancelled", reverse: "reversed", close: "closed" };
      if (cmd === "process" || cmd === "retry") r.attempts += 1;
      refundTo(r, next[cmd], reason);
      return r;
    },
    receivables: async () => ({ items: receivables, totalOutstanding: fromPaise(charge.outstanding), totalOverdue: "0.00" }),
    evaluate: async () => {
      if (!receivables.length)
        receivables.push({ id: id(), studentAccountId: account.id, accountName: account.displayName, studentNumber: account.studentNumber, chargeId: charge.id, chargeNumber: charge.chargeNumber, description: charge.description, recognisedAmount: "125000.00", outstandingAmount: fromPaise(charge.outstanding), writtenOffAmount: "0.00", dueDate: charge.dueDate, daysOverdue: 0, ageingBucket: "current", status: charge.outstanding === 0n ? "settled" : "open", writeOffs: [], version: 1 });
      return { recognised: 1, overdue: 0, settled: 0, reopened: 0, evaluated: receivables.length, asOf: today() };
    },
    dunning: async (rid) => {
      const r = find(receivables, rid);
      if (r.status !== "overdue") throw conflict(`only an overdue receivable is dunned (receivable is ${r.status})`);
      return r;
    },
    writeOff: async (rid, _v, reason) => {
      const r = find(receivables, rid);
      r.writeOffs.push({ id: id(), amount: r.outstandingAmount, reason, status: "posted" });
      Object.assign(r, { status: "written_off", writtenOffAmount: r.outstandingAmount, outstandingAmount: "0.00", version: r.version + 1 });
      return r;
    },
    reopen: async (rid) => {
      const r = find(receivables, rid);
      Object.assign(r, { status: "reopened", outstandingAmount: r.writtenOffAmount, writtenOffAmount: "0.00", version: r.version + 1 });
      return r;
    },
    myAccounts: async (): Promise<MyAccount[]> => [{ account, charges: [{ id: charge.id, chargeNumber: charge.chargeNumber, description: charge.description, outstandingAmount: fromPaise(charge.outstanding), dueDate: charge.dueDate, status: charge.status }] }],
    myPayments: async () => payments,
    gatewayInfo: async () => ({ gateway: "mock", onlinePayments: true, checkoutKeyId: null }),
    initiate: async (amount) => {
      if (toPaise(amount) > charge.outstanding) throw new ApiProblem({ type: "about:blank", title: "Validation failed", status: 422, errorCode: "validation-failed", validationErrors: [{ field: "amount", code: "exceeds_outstanding", message: "cannot exceed what is outstanding on the selected charges" }] });
      const p = newPayment("gateway", amount, "pending");
      p.gateway = "mock";
      p.gatewayOrderRef = `order_mock_${p.id.slice(-12)}`;
      p.checkoutUrl = `/fees?mockOrder=${p.gatewayOrderRef}`;
      return p;
    },
    confirm: async (pid) => {
      const p = find(payments, pid);
      if (p.status !== "pending") return p;
      const paise = toPaise(p.amount) % 100n;
      if (paise === 13n) Object.assign(p, { status: "failed", failureCode: "BAD_REQUEST_ERROR", version: p.version + 1 });
      else if (paise !== 99n) complete(p);
      return p;
    },
    cancelMine: async (pid) => {
      const p = find(payments, pid);
      if (!["pending", "initiated"].includes(p.status)) throw conflict(`payment cannot move from ${p.status} to cancelled`);
      Object.assign(p, { status: "cancelled", version: p.version + 1 });
      return p;
    },
    myReceipts: async () => receipts,
    myRefunds: async () => refunds,
    requestMyRefund: async (input) => newRefund(input, "self"),
  };
}
