/**
 * Slice **5B** web components (payments, receipts, refunds, receivables). The export names are
 * imported by the route frames (docs/batch5-contracts.md §10); real screens carry
 * `data-slice="5B"`. Money is always a decimal string formatted with `formatInr`.
 *
 * - `CollectionsAdminTab` — Admin → Finance → Collections (counter / bank payments, allocation,
 *   receipts, reversal, unidentified receipts, bulk collections).
 * - `RefundsAdminTab` — Admin → Finance → Refunds (decision, payout, failure / retry, reversal, closure).
 * - `ReceivablesAdminTab` — Admin → Finance → Receivables (ageing, dunning, write-off, reopen).
 * - `StudentPaymentsPanel` — student `/fees`: pay with the mock gateway, receipts, refund request.
 */
import { CreditCard, HandCoins, Inbox, ReceiptText, RotateCcw, Scale } from "lucide-react";
import { Badge, BentoTile, Button, DataTable, EmptyState, Input, Modal } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { Actions, fmtDate, REASON, useAsk, type RowAction } from "../../institution/forms";
import { StatusBadge, statusLabel, useCommand } from "../../institution/hooks";
import type { Institution } from "../../institution/types";
import { formatInr } from "../index";
import { pkeys, usePaymentsApi } from "./hooks";
import { fromPaise, toPaise } from "./mock";
import type { Payment, Receipt, Refund } from "./types";

export { PaymentsApiProvider } from "./hooks";

const CHANNELS = [
  { value: "cash", label: "Cash" },
  { value: "cheque", label: "Cheque" },
  { value: "bank_transfer", label: "Bank transfer" },
  { value: "demand_draft", label: "Demand draft" },
  { value: "sponsor", label: "Sponsor" },
];

const isZero = (a: string | null | undefined) => !a || toPaise(a) === 0n;

function NoInstitutionTile({ title }: { title: string }) {
  return (
    <BentoTile span={12} title={title} data-slice="5B">
      <EmptyState icon={Inbox} title="No institution selected">
        Register an institution in Campus setup → Institution first.
      </EmptyState>
    </BentoTile>
  );
}

function ReceiptModal({ receipt, onClose }: { receipt: Receipt; onClose: () => void }) {
  return (
    <Modal open onClose={onClose} label={`Receipt ${receipt.receiptNumber}`}>
      <div data-receipt={receipt.receiptNumber}>
        <h2>Receipt {receipt.receiptNumber}</h2>
        <p>
          {receipt.accountName ?? "—"}
          {receipt.studentNumber ? ` · ${receipt.studentNumber}` : ""} · {statusLabel(receipt.channel)} · {fmtDate(receipt.issuedAt.slice(0, 10))}
        </p>
        <p>
          <b data-receipt-amount>{formatInr(receipt.amount)}</b> received against {receipt.paymentNumber}
          {receipt.status === "cancelled" ? " — cancelled" : ""}
        </p>
        <ul>
          {receipt.allocations.map((a) => (
            <li key={a.id}>
              {a.chargeNumber}: {formatInr(a.amount)} {a.status === "reversed" ? "(reversed)" : ""}
            </li>
          ))}
          {!isZero(receipt.unappliedAmount) && <li>Unapplied credit: {formatInr(receipt.unappliedAmount)}</li>}
        </ul>
        <Button size="sm" variant="ghost" onClick={onClose}>
          Close
        </Button>
      </div>
    </Modal>
  );
}

// ------------------------------------------------------------------ Collections

export function CollectionsAdminTab({ institution }: { institution: Institution | null }) {
  const api = usePaymentsApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const [receipt, setReceipt] = useState<Receipt | null>(null);
  const inst = institution?.id ?? "-";
  const accounts = useQuery({ queryKey: pkeys.scoped(inst, "accounts"), queryFn: () => api.accounts(inst), enabled: !!institution });
  const payments = useQuery({ queryKey: pkeys.scoped(inst, "payments"), queryFn: () => api.payments(inst), enabled: !!institution });
  const unidentified = useQuery({ queryKey: pkeys.scoped(inst, "unidentified"), queryFn: () => api.unidentified(inst), enabled: !!institution });
  const collections = useQuery({ queryKey: pkeys.scoped(inst, "collections"), queryFn: () => api.collections(inst), enabled: !!institution });
  if (!institution) return <NoInstitutionTile title="Collections" />;
  const accountOptions = [{ value: "", label: "Choose a student" }, ...(accounts.data ?? []).map((a) => ({ value: a.id, label: `${a.displayName}${a.studentNumber ? ` (${a.studentNumber})` : ""} · due ${formatInr(a.balance.outstanding)}` }))];
  const actions = (p: Payment): RowAction[] => {
    const a: RowAction[] = [];
    if (p.receipt) a.push({ id: "payment-receipt", label: "Receipt", onClick: () => void run(async () => setReceipt(await api.receipt(p.id))) });
    if (p.status === "completed" && !isZero(p.unappliedAmount)) a.push({ id: "payment-allocate", label: "Allocate credit", onClick: () => void run(() => api.allocate(p.id, p.version), "Unapplied credit allocated") });
    if (p.status === "completed")
      a.push({
        id: "payment-reverse",
        label: "Reverse",
        onClick: () => ask({ title: `Reverse ${p.paymentNumber}`, name: "payment-reverse", fields: [REASON], submitLabel: "Reverse payment", onSubmit: (v) => submit(() => api.reversePayment(p.id, p.version, v.reason!), "Payment reversed") }),
      });
    return a;
  };
  return (
    <>
      <BentoTile
        span={12}
        title="Payments & receipts"
        data-slice="5B"
        action={
          <Button
            size="sm"
            icon={<HandCoins strokeWidth={1.75} />}
            data-record-payment
            onClick={() =>
              ask({
                title: "Record a payment",
                name: "record-payment",
                submitLabel: "Record & issue receipt",
                fields: [
                  { name: "accountId", label: "Student", type: "select", required: true, options: accountOptions, wide: true },
                  { name: "channel", label: "Channel", type: "select", required: true, options: CHANNELS, initial: "cash" },
                  { name: "amount", label: "Amount (₹)", required: true, placeholder: "25000.00" },
                  { name: "externalRef", label: "Cheque / UTR / DD number" },
                  { name: "payerName", label: "Payer (sponsor)" },
                ],
                onSubmit: (v) =>
                  submit(
                    () => api.recordPayment(v.accountId!, { channel: v.channel as "cash", amount: v.amount!, externalRef: v.externalRef || undefined, payerName: v.payerName || undefined }),
                    (p) => `${p.paymentNumber} recorded · receipt ${p.receipt?.receiptNumber ?? "—"}`,
                  ),
              })
            }
          >
            Record payment
          </Button>
        }
      >
        <DataTable
          caption="Payments"
          rows={payments.data ?? []}
          rowKey={(p) => p.id}
          empty={
            <EmptyState icon={CreditCard} title="No payments yet">
              Record a counter or bank payment, or wait for students to pay online.
            </EmptyState>
          }
          columns={[
            { key: "n", header: "Payment", render: (p) => <span data-payment={p.paymentNumber}>{p.paymentNumber}</span> },
            { key: "w", header: "Student", render: (p) => p.accountName ?? "—" },
            { key: "c", header: "Channel", render: (p) => statusLabel(p.channel) },
            { key: "a", header: "Amount", render: (p) => formatInr(p.amount) },
            { key: "u", header: "Unapplied", render: (p) => (isZero(p.unappliedAmount) ? "—" : formatInr(p.unappliedAmount)) },
            { key: "r", header: "Receipt", render: (p) => p.receipt?.receiptNumber ?? "—" },
            { key: "s", header: "Status", render: (p) => <StatusBadge status={p.status} /> },
            { key: "x", header: "", render: (p) => <Actions actions={actions(p)} disabled={busy} /> },
          ]}
        />
      </BentoTile>
      <BentoTile
        span={6}
        title="Unidentified receipts"
        data-unidentified
        action={
          <Button
            size="sm"
            variant="ghost"
            data-record-unidentified
            onClick={() =>
              ask({
                title: "Record an unidentified receipt",
                name: "record-unidentified",
                submitLabel: "Record",
                fields: [
                  { name: "channel", label: "Channel", type: "select", required: true, options: CHANNELS, initial: "bank_transfer" },
                  { name: "amount", label: "Amount (₹)", required: true },
                  { name: "externalRef", label: "UTR / reference" },
                  { name: "payerName", label: "Remitter as shown" },
                ],
                onSubmit: (v) => submit(() => api.recordUnidentified(inst, { channel: v.channel!, amount: v.amount!, externalRef: v.externalRef || undefined, payerName: v.payerName || undefined }), "Held as unapplied receipt"),
              })
            }
          >
            Record
          </Button>
        }
      >
        <DataTable
          caption="Unidentified receipts"
          rows={unidentified.data ?? []}
          rowKey={(u) => u.id}
          empty={<EmptyState icon={Inbox} title="Nothing in suspense">Money received without a known payer appears here.</EmptyState>}
          columns={[
            { key: "a", header: "Amount", render: (u) => formatInr(u.amount) },
            { key: "r", header: "Reference", render: (u) => u.externalRef ?? u.payerName ?? "—" },
            { key: "s", header: "Status", render: (u) => <StatusBadge status={u.status} /> },
            {
              key: "x",
              header: "",
              render: (u) => {
                const a: RowAction[] = [];
                if (u.status === "unidentified" || u.status === "under_investigation")
                  a.push({
                    id: "unidentified-identify",
                    label: "Identify",
                    onClick: () => ask({ title: "Identify the payer", name: "identify", fields: [{ name: "accountId", label: "Student", type: "select", required: true, options: accountOptions, wide: true }], submitLabel: "Identify", onSubmit: (v) => submit(() => api.identify(u.id, u.version, v.accountId!), "Payer identified · receipt issued") }),
                  });
                if (u.status === "identified") a.push({ id: "unidentified-allocate", label: "Allocate", onClick: () => void run(() => api.allocateUnidentified(u.id, u.version), "Allocated to open dues") });
                return <Actions actions={a} disabled={busy} />;
              },
            },
          ]}
        />
      </BentoTile>
      <BentoTile span={6} title="Bulk collections">
        <DataTable
          caption="Bulk collections"
          rows={collections.data ?? []}
          rowKey={(c) => c.id}
          empty={<EmptyState icon={ReceiptText} title="No bulk collections">Camp-counter and sponsor batches appear here.</EmptyState>}
          columns={[
            { key: "n", header: "Batch", render: (c) => c.name },
            { key: "t", header: "Total", render: (c) => formatInr(c.totalAmount) },
            { key: "i", header: "Items", render: (c) => String(c.items.length) },
            { key: "s", header: "Status", render: (c) => <StatusBadge status={c.status} /> },
          ]}
        />
      </BentoTile>
      {receipt && <ReceiptModal receipt={receipt} onClose={() => setReceipt(null)} />}
      {prompt}
      {dialog}
    </>
  );
}

// ------------------------------------------------------------------ Refunds

export function RefundsAdminTab({ institution }: { institution: Institution | null }) {
  const api = usePaymentsApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const inst = institution?.id ?? "-";
  const refunds = useQuery({ queryKey: pkeys.scoped(inst, "refunds"), queryFn: () => api.refunds(inst), enabled: !!institution });
  const accounts = useQuery({ queryKey: pkeys.scoped(inst, "accounts"), queryFn: () => api.accounts(inst), enabled: !!institution });
  if (!institution) return <NoInstitutionTile title="Refunds" />;
  const withReason = (r: Refund, cmd: "cancel" | "reverse", label: string, done: string): RowAction => ({
    id: `refund-${cmd}`,
    label,
    onClick: () => ask({ title: `${label} ${r.refundNumber}`, name: `refund-${cmd}`, fields: [REASON], submitLabel: label, onSubmit: (v) => submit(() => api.refundCommand(r.id, r.version, cmd, v.reason), done) }),
  });
  const actions = (r: Refund): RowAction[] => {
    const a: RowAction[] = [];
    const cmd = (c: "process" | "retry" | "complete" | "close", label: string, done: string): RowAction => ({ id: `refund-${c}`, label, onClick: () => void run(() => api.refundCommand(r.id, r.version, c), done) });
    switch (r.status) {
      case "under_review":
        a.push({ id: "refund-approve", label: "Approve", variant: "primary", onClick: () => void run(() => api.decideRefund(r.id, r.version, true), "Refund approved") });
        a.push({
          id: "refund-reject",
          label: "Reject",
          onClick: () => ask({ title: `Reject ${r.refundNumber}`, name: "refund-reject", fields: [REASON], submitLabel: "Reject", onSubmit: (v) => submit(() => api.decideRefund(r.id, r.version, false, v.reason), "Refund rejected") }),
        });
        break;
      case "approved":
        a.push(cmd("process", "Pay out", "Payout started"), withReason(r, "cancel", "Cancel", "Refund cancelled"));
        break;
      case "processing":
        a.push(cmd("complete", "Confirm paid", "Refund completed"));
        break;
      case "failed":
        a.push(cmd("retry", "Retry", "Payout retried"), cmd("complete", "Paid manually", "Refund completed"), withReason(r, "cancel", "Cancel", "Refund cancelled"));
        break;
      case "completed":
        a.push(withReason(r, "reverse", "Reverse", "Refund reversed"), cmd("close", "Close", "Refund closed"));
        break;
      case "rejected":
      case "cancelled":
      case "reversed":
        a.push(cmd("close", "Close", "Refund closed"));
        break;
      default:
        break;
    }
    return a;
  };
  return (
    <>
      <BentoTile
        span={12}
        title="Refunds"
        data-slice="5B"
        action={
          <Button
            size="sm"
            icon={<RotateCcw strokeWidth={1.75} />}
            data-request-refund
            onClick={() =>
              ask({
                title: "Request a refund",
                name: "request-refund",
                submitLabel: "Request",
                fields: [
                  { name: "accountId", label: "Student", type: "select", required: true, options: [{ value: "", label: "Choose a student" }, ...(accounts.data ?? []).map((x) => ({ value: x.id, label: x.displayName }))], wide: true },
                  { name: "amount", label: "Amount (₹)", required: true },
                  { ...REASON, label: "Why" },
                ],
                onSubmit: (v) => submit(() => api.requestRefund(v.accountId!, { amount: v.amount!, reason: v.reason! }), (r) => `${r.refundNumber} ${statusLabel(r.status).toLowerCase()}`),
              })
            }
          >
            Request refund
          </Button>
        }
      >
        <DataTable
          caption="Refunds"
          rows={refunds.data ?? []}
          rowKey={(r) => r.id}
          empty={<EmptyState icon={RotateCcw} title="No refunds">Refund requests from students and the finance office appear here.</EmptyState>}
          columns={[
            { key: "n", header: "Refund", render: (r) => <span data-refund={r.refundNumber}>{r.refundNumber}</span> },
            { key: "w", header: "Student", render: (r) => r.accountName ?? "—" },
            { key: "a", header: "Amount", render: (r) => formatInr(r.amount) },
            { key: "m", header: "Method", render: (r) => (r.method === "original" ? "Original instrument" : "Bank transfer") },
            { key: "q", header: "Reason", render: (r) => r.reason },
            { key: "s", header: "Status", render: (r) => <StatusBadge status={r.status} /> },
            { key: "x", header: "", render: (r) => <Actions actions={actions(r)} disabled={busy} /> },
          ]}
        />
      </BentoTile>
      {prompt}
      {dialog}
    </>
  );
}

// ------------------------------------------------------------------ Receivables

export function ReceivablesAdminTab({ institution }: { institution: Institution | null }) {
  const api = usePaymentsApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const inst = institution?.id ?? "-";
  const list = useQuery({ queryKey: pkeys.scoped(inst, "receivables"), queryFn: () => api.receivables(inst), enabled: !!institution });
  if (!institution) return <NoInstitutionTile title="Receivables" />;
  return (
    <>
      <BentoTile
        span={12}
        title="Receivables & dunning"
        data-slice="5B"
        action={
          <Button size="sm" icon={<Scale strokeWidth={1.75} />} data-evaluate onClick={() => void run(() => api.evaluate(inst), (e) => `${e.recognised} recognised · ${e.overdue} newly overdue · ${e.settled} settled`)}>
            Evaluate dues
          </Button>
        }
      >
        <p data-receivable-totals>
          Outstanding <b>{formatInr(list.data?.totalOutstanding ?? "0.00")}</b> · overdue <b>{formatInr(list.data?.totalOverdue ?? "0.00")}</b>
        </p>
        <DataTable
          caption="Receivables"
          rows={list.data?.items ?? []}
          rowKey={(r) => r.id}
          empty={<EmptyState icon={Scale} title="No receivables yet">Evaluate dues to recognise every published charge.</EmptyState>}
          columns={[
            { key: "c", header: "Charge", render: (r) => <span data-receivable={r.chargeNumber}>{r.chargeNumber}</span> },
            { key: "w", header: "Student", render: (r) => r.accountName },
            { key: "d", header: "Due", render: (r) => fmtDate(r.dueDate) },
            { key: "g", header: "Ageing", render: (r) => (r.daysOverdue > 0 ? <Badge kind="attention">{`${r.ageingBucket} days`}</Badge> : "Current") },
            { key: "o", header: "Outstanding", render: (r) => formatInr(r.outstandingAmount) },
            { key: "n", header: "Reminders", render: (r) => (r.dunning ? `${r.dunning.level} · ${statusLabel(r.dunning.status)}` : "—") },
            { key: "s", header: "Status", render: (r) => <StatusBadge status={r.status} /> },
            {
              key: "x",
              header: "",
              render: (r) => {
                const a: RowAction[] = [];
                if (r.status === "overdue" && (r.dunning?.level ?? 0) < 3) a.push({ id: "receivable-dunning", label: "Send reminder", onClick: () => void run(() => api.dunning(r.id, r.version), "Reminder sent") });
                if (["open", "partially_settled", "overdue", "reopened"].includes(r.status) && !isZero(r.outstandingAmount))
                  a.push({ id: "receivable-write-off", label: "Write off", onClick: () => ask({ title: `Write off ${r.chargeNumber}`, name: "write-off", fields: [REASON], submitLabel: "Request write-off", onSubmit: (v) => submit(() => api.writeOff(r.id, r.version, v.reason!), "Write-off decided") }) });
                if (r.status === "written_off") a.push({ id: "receivable-reopen", label: "Reopen", onClick: () => ask({ title: `Reopen ${r.chargeNumber}`, name: "reopen", fields: [REASON], submitLabel: "Reopen", onSubmit: (v) => submit(() => api.reopen(r.id, r.version, v.reason!), "Receivable reopened") }) });
                return <Actions actions={a} disabled={busy} />;
              },
            },
          ]}
        />
      </BentoTile>
      {prompt}
      {dialog}
    </>
  );
}

// ------------------------------------------------------------------ Student panel

export function StudentPaymentsPanel() {
  const api = usePaymentsApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const [amount, setAmount] = useState("");
  const [receipt, setReceipt] = useState<Receipt | null>(null);
  const accounts = useQuery({ queryKey: pkeys.scoped("me", "accounts"), queryFn: () => api.myAccounts() });
  const payments = useQuery({ queryKey: pkeys.scoped("me", "payments"), queryFn: () => api.myPayments() });
  const receipts = useQuery({ queryKey: pkeys.scoped("me", "receipts"), queryFn: () => api.myReceipts() });
  const refunds = useQuery({ queryKey: pkeys.scoped("me", "refunds"), queryFn: () => api.myRefunds() });
  const outstanding = (accounts.data ?? []).reduce((s, a) => s + toPaise(a.account.balance.outstanding), 0n);
  const credit = (payments.data ?? []).filter((p) => p.status === "completed").reduce((s, p) => s + toPaise(p.unappliedAmount), 0n);
  const open = (payments.data ?? []).find((p) => p.channel === "gateway" && p.status === "pending");
  const due = fromPaise(outstanding);
  return (
    <BentoTile span={4} title="Pay & receipts" data-slice="5B" data-student-payments>
      <p>
        Outstanding <b data-outstanding>{formatInr(due)}</b>
      </p>
      {open ? (
        <div className="sb-checkout" data-checkout={open.gatewayOrderRef ?? ""}>
          <p>
            <CreditCard strokeWidth={1.75} size={16} /> Secure checkout (test gateway) · {formatInr(open.amount)}
          </p>
          <small>Order {open.gatewayOrderRef}</small>
          <div className="sb-row-actions">
            <Button size="sm" data-action="checkout-pay" disabled={busy} onClick={() => void run(() => api.confirm(open.id, open.version), (p) => (p.status === "completed" ? `Paid · receipt ${p.receipt?.receiptNumber ?? ""}` : p.status === "failed" ? "Payment failed — no money was taken" : "Still waiting for the bank"))}>
              Complete payment
            </Button>
            <Button size="sm" variant="ghost" data-action="checkout-cancel" disabled={busy} onClick={() => void run(() => api.cancelMine(open.id, open.version), "Payment cancelled")}>
              Cancel
            </Button>
          </div>
        </div>
      ) : outstanding > 0n ? (
        <form
          data-form="pay"
          onSubmit={(e) => {
            e.preventDefault();
            void run(() => api.initiate(amount || due), "Checkout opened");
            setAmount("");
          }}
        >
          <Input label="Amount (₹)" name="amount" value={amount} placeholder={due} onChange={(e) => setAmount(e.target.value)} inputMode="decimal" />
          <Button size="sm" type="submit" icon={<CreditCard strokeWidth={1.75} />} disabled={busy} data-action="pay-now">
            Pay now
          </Button>
        </form>
      ) : (
        <p>You have no dues right now.</p>
      )}
      <h3>Payments</h3>
      {(payments.data ?? []).length === 0 ? (
        <p>No payments yet.</p>
      ) : (
        <ul className="sb-list" data-my-payments>
          {(payments.data ?? []).slice(0, 6).map((p) => (
            <li key={p.id}>
              {p.paymentNumber} · {formatInr(p.amount)} <StatusBadge status={p.status} />
            </li>
          ))}
        </ul>
      )}
      <h3>Receipts</h3>
      {(receipts.data ?? []).length === 0 ? (
        <p>Receipts appear here after a payment.</p>
      ) : (
        <ul className="sb-list" data-my-receipts>
          {(receipts.data ?? []).map((r) => (
            <li key={r.id}>
              <Button size="sm" variant="ghost" data-action="open-receipt" onClick={() => setReceipt(r)}>
                {r.receiptNumber} · {formatInr(r.amount)}
              </Button>
            </li>
          ))}
        </ul>
      )}
      {credit > 0n && (
        <p data-credit>
          Credit on your account: <b>{formatInr(fromPaise(credit))}</b>{" "}
          <Button
            size="sm"
            variant="ghost"
            data-action="request-refund"
            onClick={() =>
              ask({
                title: "Request a refund",
                name: "my-refund",
                submitLabel: "Request refund",
                fields: [
                  { name: "amount", label: "Amount (₹)", required: true, initial: fromPaise(credit) },
                  { ...REASON, label: "Why" },
                ],
                onSubmit: (v) => submit(() => api.requestMyRefund({ amount: v.amount!, reason: v.reason! }), (r) => `Refund ${r.refundNumber}: ${statusLabel(r.status).toLowerCase()}`),
              })
            }
          >
            Request refund
          </Button>
        </p>
      )}
      {(refunds.data ?? []).length > 0 && (
        <ul className="sb-list" data-my-refunds>
          {(refunds.data ?? []).map((r) => (
            <li key={r.id}>
              {r.refundNumber} · {formatInr(r.amount)} <StatusBadge status={r.status} />
            </li>
          ))}
        </ul>
      )}
      {receipt && <ReceiptModal receipt={receipt} onClose={() => setReceipt(null)} />}
      {prompt}
      {dialog}
    </BentoTile>
  );
}
