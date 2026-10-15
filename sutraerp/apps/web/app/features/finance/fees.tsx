/**
 * Student Fees (`/fees`) in the prototype composition (stu-fees.png): Status / Next due / Paid
 * KPI tiles, an Invoices table with a **Pay now** pill per open invoice and the Pay dialog
 * (stu-fees-dlg-fe-pay.png: amount, UPI / Card / Net banking choice → the existing mock-gateway
 * checkout → receipt number). Everything the old 5A / 5B panels did stays reachable: instalments
 * and disputes in the invoice side panel (row kebab → Details / Dispute), receipts, scholarships,
 * unapplied credit and refund requests below the table.
 */
import { Building2, CalendarClock, CircleCheck, CreditCard, GraduationCap, Hourglass, Lock, ReceiptText, RotateCcw, Smartphone, TriangleAlert, Wallet } from "lucide-react";
import type { LucideIcon } from "lucide-react";
import { Badge, BentoTile, DataTable, EmptyState, KeyValue, KpiTile, ListRow, RowActions, SidePanel, StepDialog, TextField, formatDate } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState, type ReactNode } from "react";
import { REASON, todayIso } from "../institution/forms";
import { statusLabel, useCommand } from "../institution/hooks";
import { fkeys, useChargesApi } from "./charges/hooks";
import type { ChargeDetail } from "./charges/types";
import { pkeys, usePaymentsApi } from "./payments/hooks";
import { ReceiptModal } from "./payments";
import { fromPaise, toPaise } from "./payments/mock";
import type { Payment, Receipt } from "./payments/types";
import { INVOICE_BADGE, invoiceState, money, sumPaise, useFinanceAsk } from "./ui";

const METHODS: { id: string; label: string; icon: LucideIcon }[] = [
  { id: "upi", label: "UPI", icon: Smartphone },
  { id: "card", label: "Card", icon: CreditCard },
  { id: "netbanking", label: "Net banking", icon: Building2 },
];

const isOpen = (c: ChargeDetail) => ["published", "active", "disputed"].includes(c.status) && toPaise(c.outstandingAmount || "0") > 0n;

/** Pay dialog: amount + method → checkout (test gateway) → receipt. */
function PayDialog({ charge, onClose, onReceipt }: { charge: ChargeDetail; onClose: () => void; onReceipt: (r: Receipt) => void }) {
  const api = usePaymentsApi();
  const { run, busy, dialog } = useCommand();
  const [method, setMethod] = useState("upi");
  const [amount, setAmount] = useState(fromPaise(toPaise(charge.outstandingAmount)));
  const [payment, setPayment] = useState<Payment | null>(null);
  const [partial, setPartial] = useState(false);
  const receipts = useQuery({ queryKey: pkeys.scoped("me", "receipts"), queryFn: () => api.myReceipts() });
  const valid = /^\d+(\.\d{1,2})?$/.test(amount.trim()) && toPaise(amount.trim()) > 0n;
  const eyebrow = `${charge.chargeNumber}${charge.dueDate ? ` · Due ${formatDate(charge.dueDate)}` : ""}`;
  const title = `Pay ${charge.description.charAt(0).toLowerCase()}${charge.description.slice(1)}`;
  const done = payment?.status === "completed";
  const receipt = done ? (receipts.data ?? []).find((r) => r.paymentId === payment?.id) : undefined;

  let body: ReactNode;
  let footer: ReactNode;
  if (!payment) {
    body = (
      <form
        id="fees-pay"
        data-form="pay"
        noValidate
        onSubmit={(e) => {
          e.preventDefault();
          if (!valid) return;
          void run(() => api.initiate(amount.trim(), [charge.id])).then((p) => p && setPayment(p));
        }}
      >
        <div style={{ fontSize: 32, fontWeight: 800, letterSpacing: "-0.03em", margin: "4px 0 18px" }} data-pay-amount>
          {money(amount.trim() || "0")}
        </div>
        <span style={{ display: "block", fontSize: 13, fontWeight: 600, color: "var(--sb-mute)", marginBottom: 8 }}>Payment method</span>
        <div role="radiogroup" aria-label="Payment method" style={{ display: "grid", gridTemplateColumns: "repeat(3, 1fr)", gap: 10, marginBottom: 16 }}>
          {METHODS.map((m) => {
            const on = m.id === method;
            const Icon = m.icon;
            return (
              <button
                key={m.id}
                type="button"
                role="radio"
                aria-checked={on}
                data-method={m.id}
                onClick={() => setMethod(m.id)}
                style={{
                  display: "flex",
                  flexDirection: "column",
                  alignItems: "center",
                  gap: 8,
                  padding: "16px 10px",
                  minHeight: 44,
                  borderRadius: 18,
                  border: `2px solid ${on ? "var(--ac)" : "var(--sb-line)"}`,
                  background: on ? "var(--acs)" : "var(--sb-surface, #fff)",
                  fontSize: 13.5,
                  fontWeight: 600,
                  color: "var(--sb-ink)",
                  cursor: "pointer",
                }}
              >
                <Icon strokeWidth={1.75} size={18} aria-hidden="true" />
                {m.label}
              </button>
            );
          })}
        </div>
        {partial ? (
          <TextField label="Amount (₹)" name="amount" inputMode="decimal" value={amount} onChange={(e) => setAmount(e.target.value)} hint={`Up to ${money(charge.outstandingAmount)} due on this invoice`} />
        ) : (
          <button type="button" className="sb-link" style={{ fontSize: 13, fontWeight: 600 }} data-action="pay-partial" onClick={() => setPartial(true)}>
            Pay part of this invoice
          </button>
        )}
      </form>
    );
    footer = (
      <div className="sb-dialog-foot">
        <button type="submit" form="fees-pay" className="sb-btn" disabled={busy || !valid} data-action="pay-now">
          <Lock strokeWidth={1.75} aria-hidden="true" />
          {busy ? "Working…" : `Pay ${money(amount.trim() || "0")}`}
        </button>
        <button type="button" className="sb-btn sb-btn--ghost" onClick={onClose}>
          Cancel
        </button>
      </div>
    );
  } else if (!done) {
    const failed = payment.status === "failed" || payment.status === "cancelled";
    body = (
      <div data-checkout={payment.gatewayOrderRef ?? ""}>
        <div style={{ fontSize: 32, fontWeight: 800, letterSpacing: "-0.03em", margin: "4px 0 12px" }}>{money(payment.amount)}</div>
        <ListRow icon={failed ? TriangleAlert : Lock} tone={failed ? "rose" : "green"} title={failed ? "Payment not completed — no money was taken" : "Secure checkout · test gateway"} meta={`${METHODS.find((m) => m.id === method)?.label ?? ""} · order ${payment.gatewayOrderRef ?? "—"}`} badge={{ label: statusLabel(payment.status), kind: failed ? "attention" : "review" }} />
      </div>
    );
    footer = (
      <div className="sb-dialog-foot">
        {!failed && (
          <button
            type="button"
            className="sb-btn"
            disabled={busy}
            data-action="checkout-pay"
            onClick={() => void run(() => api.confirm(payment.id, payment.version), (p) => (p.status === "completed" ? `Paid · receipt ${p.receipt?.receiptNumber ?? ""}` : p.status === "failed" ? "Payment failed — no money was taken" : "Still waiting for the bank")).then((p) => p && setPayment(p))}
          >
            <Lock strokeWidth={1.75} aria-hidden="true" />
            Complete payment
          </button>
        )}
        {!failed && (
          <button type="button" className="sb-btn sb-btn--ghost" disabled={busy} data-action="checkout-cancel" onClick={() => void run(() => api.cancelMine(payment.id, payment.version), "Payment cancelled").then(() => onClose())}>
            Cancel payment
          </button>
        )}
        {failed && (
          <button type="button" className="sb-btn sb-btn--ghost" onClick={onClose}>
            Close
          </button>
        )}
      </div>
    );
  } else {
    const number = payment.receipt?.receiptNumber ?? receipt?.receiptNumber ?? "";
    body = (
      <div style={{ textAlign: "center", display: "flex", flexDirection: "column", alignItems: "center", gap: 10, padding: "8px 0" }} data-paid>
        <span style={{ width: 56, height: 56, borderRadius: "50%", display: "grid", placeItems: "center", background: "var(--tint-green-bg, #E1F4EA)", color: "var(--tint-green-fg, #1D6A40)" }}>
          <CircleCheck strokeWidth={1.75} size={28} aria-hidden="true" />
        </span>
        <b style={{ fontSize: 18 }}>{money(payment.amount)} paid</b>
        <span style={{ color: "var(--sb-mute)", fontSize: 14 }}>Receipt number</span>
        <span data-receipt-number style={{ fontSize: 20, fontWeight: 800, letterSpacing: "0.02em", background: "var(--sb-bg)", borderRadius: 14, padding: "10px 16px" }}>
          {number}
        </span>
      </div>
    );
    footer = (
      <div className="sb-dialog-foot">
        <button type="button" className="sb-btn" data-action="pay-done" onClick={onClose}>
          Done
        </button>
        {receipt && (
          <button type="button" className="sb-btn sb-btn--ghost" data-action="pay-receipt" onClick={() => onReceipt(receipt)}>
            View receipt
          </button>
        )}
      </div>
    );
  }
  return (
    <StepDialog open onClose={onClose} eyebrow={eyebrow} title={done ? "Payment received" : title} footer={footer} data-pay-dialog={charge.chargeNumber}>
      {body}
      {dialog}
    </StepDialog>
  );
}

export function StudentFees() {
  const charges = useChargesApi();
  const payApi = usePaymentsApi();
  const { submit, run, busy, dialog } = useCommand();
  const { ask, prompt } = useFinanceAsk();
  const [paying, setPaying] = useState<ChargeDetail | null>(null);
  const [detailId, setDetailId] = useState<string | null>(null);
  const [receipt, setReceipt] = useState<Receipt | null>(null);
  const mine = useQuery({ queryKey: fkeys.scoped("me", "charges"), queryFn: () => charges.myCharges() });
  const aid = useQuery({ queryKey: fkeys.scoped("me", "aid"), queryFn: () => charges.myAid() });
  const accounts = useQuery({ queryKey: pkeys.scoped("me", "accounts"), queryFn: () => payApi.myAccounts() });
  const payments = useQuery({ queryKey: pkeys.scoped("me", "payments"), queryFn: () => payApi.myPayments() });
  const receipts = useQuery({ queryKey: pkeys.scoped("me", "receipts"), queryFn: () => payApi.myReceipts() });
  const refunds = useQuery({ queryKey: pkeys.scoped("me", "refunds"), queryFn: () => payApi.myRefunds() });

  const today = todayIso();
  const items: ChargeDetail[] = mine.data ?? [];
  const outstanding = (accounts.data ?? []).reduce((s, a) => s + toPaise(a.account.balance.outstanding), 0n);
  const overdue = (accounts.data ?? []).reduce((s, a) => s + toPaise(a.account.balance.overdue), 0n);
  const completed = (payments.data ?? []).filter((p) => p.status === "completed");
  const paid = sumPaise(completed, (p) => p.amount);
  const credit = sumPaise(completed, (p) => p.unappliedAmount);
  const open = (payments.data ?? []).find((p) => p.channel === "gateway" && p.status === "pending");

  // Next due: the earliest unpaid instalment, else the earliest open invoice.
  const dues: { date: string; label: string; amount: string }[] = [];
  for (const c of items.filter(isOpen)) {
    const inst = c.instalments.filter((i) => ["scheduled", "due", "overdue"].includes(i.status));
    if (inst.length) for (const i of inst) dues.push({ date: i.dueDate, label: `${c.description} · instalment ${i.sequence}`, amount: i.amount });
    else if (c.dueDate) dues.push({ date: c.dueDate, label: c.description, amount: c.outstandingAmount });
  }
  dues.sort((a, b) => a.date.localeCompare(b.date));
  const next = dues[0];

  // When each settled invoice was last paid (latest completed payment allocated to it).
  const paidOn = new Map<string, string>();
  for (const p of completed)
    for (const a of p.allocations)
      if (a.status === "allocated") {
        const on = (p.completedAt ?? p.receivedOn).slice(0, 10);
        if ((paidOn.get(a.chargeId) ?? "") < on) paidOn.set(a.chargeId, on);
      }

  const account = accounts.data?.[0]?.account;
  const detail = items.find((c) => c.id === detailId) ?? null;
  // Invoices whose due date has not come yet are upcoming, not dues (prototype: "Paid · No dues outstanding" with a next due date).
  const dueNow = items.some((c) => isOpen(c) && (!c.dueDate || c.dueDate <= today));
  const statusKpi =
    outstanding <= 0n || (overdue <= 0n && !dueNow)
      ? { value: "Paid", sub: outstanding <= 0n ? "No dues outstanding" : `No dues yet · ${money(fromPaise(outstanding))} upcoming`, icon: CircleCheck, tone: "green" as const }
      : overdue > 0n
        ? { value: "Overdue", sub: `${money(fromPaise(overdue))} past due`, icon: TriangleAlert, tone: "rose" as const }
        : { value: "Due", sub: `${money(fromPaise(outstanding))} outstanding`, icon: Hourglass, tone: "amber" as const };
  const dispute = (c: ChargeDetail) =>
    ask({ title: `Dispute ${c.chargeNumber}`, eyebrow: "Fees", name: "dispute", submitLabel: "Raise dispute", fields: [REASON], onSubmit: (v) => submit(() => charges.raiseDispute(c.id, v.reason!), "Dispute raised — the finance office will review it") });

  return (
    <>
      <KpiTile span={4} icon={statusKpi.icon} tone={statusKpi.tone} label="Status" value={statusKpi.value} sub={statusKpi.sub} loading={accounts.isLoading} data-kpi="fee-status" />
      <KpiTile span={4} icon={CalendarClock} tone="orange" label="Next due" value={next ? formatDate(next.date) : "Nothing due"} sub={next ? `${next.label} · ${money(next.amount)}` : "You have no open invoices"} loading={mine.isLoading} data-kpi="next-due" />
      <KpiTile span={4} icon={Wallet} tone="violet" label="Paid so far" value={money(fromPaise(paid))} sub={`${completed.length} ${completed.length === 1 ? "payment" : "payments"} received`} loading={payments.isLoading} data-kpi="paid" />

      <BentoTile
        span={12}
        title="Invoices"
        data-slice="5A"
        data-my-fees
        action={
          outstanding > 0n ? (
            <span style={{ fontSize: 13, fontWeight: 500, color: "var(--sb-mute)" }}>
              <b data-outstanding style={{ color: "var(--sb-ink)" }}>
                {money(fromPaise(outstanding))}
              </b>{" "}
              outstanding
            </span>
          ) : (
            <span style={{ fontSize: 13, color: "var(--sb-mute)" }} data-outstanding-none>
              You have no dues right now
            </span>
          )
        }
      >
        {open && !paying && (
          <div data-checkout={open.gatewayOrderRef ?? ""} style={{ marginBottom: 12 }}>
            <ListRow
              icon={Lock}
              tone="amber"
              title={`Payment in progress · ${money(open.amount)}`}
              meta={`Secure checkout (test gateway) · order ${open.gatewayOrderRef ?? "—"}`}
              primary={{ label: "Complete payment", disabled: busy, "data-action": "checkout-pay", onClick: () => void run(() => payApi.confirm(open.id, open.version), (p) => (p.status === "completed" ? `Paid · receipt ${p.receipt?.receiptNumber ?? ""}` : p.status === "failed" ? "Payment failed — no money was taken" : "Still waiting for the bank")) }}
              menu={[{ id: "checkout-cancel", label: "Cancel payment", danger: true, disabled: busy, onSelect: () => void run(() => payApi.cancelMine(open.id, open.version), "Payment cancelled") }]}
            />
          </div>
        )}
        <DataTable
          caption="Invoices"
          rows={items}
          rowKey={(c) => c.id}
          empty={<EmptyState icon={ReceiptText} title="No invoices yet">Invoices appear here once the finance office publishes them.</EmptyState>}
          columns={[
            {
              key: "i",
              header: "Item",
              render: (c) => (
                <b data-my-charge={c.chargeNumber} style={{ fontWeight: 700 }}>
                  {c.description}
                </b>
              ),
            },
            { key: "n", header: "Invoice", render: (c) => c.chargeNumber },
            {
              key: "a",
              header: "Amount",
              render: (c) => (
                <span>
                  {money(c.totalAmount)}
                  {toPaise(c.outstandingAmount || "0") > 0n && toPaise(c.outstandingAmount) !== toPaise(c.totalAmount) && (
                    <small style={{ display: "block", color: "var(--sb-mute)", fontSize: 12.5 }}>
                      <span data-my-outstanding={c.chargeNumber}>{`${money(c.outstandingAmount)} due`}</span>
                    </small>
                  )}
                </span>
              ),
            },
            {
              key: "d",
              header: "Date",
              render: (c) => {
                const st = invoiceState(c, today);
                if (st === "paid") return paidOn.get(c.id) ? `Paid ${formatDate(paidOn.get(c.id))}` : "Paid";
                return c.dueDate ? `Due ${formatDate(c.dueDate)}` : "—";
              },
            },
            {
              key: "s",
              header: "Status",
              render: (c) => {
                const st = invoiceState(c, today);
                const badge = st === "other" ? { label: statusLabel(c.status), kind: "neutral" as const } : st === "pending" ? { label: "Upcoming", kind: "neutral" as const } : INVOICE_BADGE[st];
                return (
                  <span style={{ display: "inline-flex", alignItems: "center", gap: 10, flexWrap: "wrap" }}>
                    <Badge kind={badge.kind}>{badge.label}</Badge>
                    {isOpen(c) && !open && (
                      <button type="button" className="sb-btn" data-action="pay-invoice" data-pay={c.chargeNumber} onClick={() => setPaying(c)}>
                        <CreditCard strokeWidth={1.75} aria-hidden="true" />
                        Pay now
                      </button>
                    )}
                    <ChargeMenu charge={c} onDetails={() => setDetailId(c.id)} onDispute={() => dispute(c)} />
                  </span>
                );
              },
            },
          ]}
        />
      </BentoTile>

      <BentoTile span={8} title="Receipts" data-slice="5B" data-student-payments>
        {(receipts.data ?? []).length === 0 && (payments.data ?? []).filter((p) => p.status !== "completed").length === 0 ? (
          <EmptyState icon={ReceiptText} title="No payments yet" compact>
            Receipts appear here after a payment.
          </EmptyState>
        ) : (
          <div data-my-receipts>
            <div data-my-payments>
              {(receipts.data ?? []).map((r) => (
                <ListRow key={r.id} icon={ReceiptText} tone="green" title={`${r.receiptNumber} · ${money(r.amount)}`} meta={`${formatDate(r.issuedAt)} · ${statusLabel(r.channel)} · ${r.paymentNumber}`} badge={r.status === "cancelled" ? { label: "Cancelled", kind: "neutral" } : undefined} onOpen={() => setReceipt(r)} data-action="open-receipt" />
              ))}
              {(payments.data ?? [])
                .filter((p) => p.status !== "completed")
                .slice(0, 6)
                .map((p) => (
                  <ListRow key={p.id} icon={CreditCard} tone="grey" title={`${p.paymentNumber} · ${money(p.amount)}`} meta={`${formatDate(p.receivedOn)} · ${statusLabel(p.channel)}`} badge={{ label: statusLabel(p.status), kind: p.status === "failed" ? "attention" : "neutral" }} />
                ))}
            </div>
          </div>
        )}
      </BentoTile>

      <BentoTile span={4} title="Scholarships & refunds">
        {(aid.data ?? []).length === 0 ? (
          <p data-my-aid="none" style={{ color: "var(--sb-mute)", fontSize: 14 }}>
            No financial aid allocated.
          </p>
        ) : (
          <div data-my-aid="list">
            {(aid.data ?? []).map((a) => (
              <ListRow key={a.id} icon={GraduationCap} tone="violet" title={a.scholarshipName} meta={`Applied ${money(a.appliedAmount)}${toPaise(a.disbursedAmount || "0") > 0n ? ` · paid ${money(a.disbursedAmount)}` : ""}`} badge={{ label: statusLabel(a.status), kind: "done" }} />
            ))}
          </div>
        )}
        {credit > 0n && (
          <div data-credit style={{ marginTop: 12 }}>
            <ListRow
              icon={Wallet}
              tone="green"
              title={`Credit on your account · ${money(fromPaise(credit))}`}
              meta="Money received beyond your invoices"
              primary={{
                label: "Request refund",
                ghost: true,
                "data-action": "request-refund",
                onClick: () =>
                  ask({
                    title: "Request a refund",
                    eyebrow: "Fees",
                    name: "my-refund",
                    submitLabel: "Request refund",
                    fields: [
                      { name: "amount", label: "Amount (₹)", required: true, initial: fromPaise(credit) },
                      { ...REASON, label: "Why" },
                    ],
                    onSubmit: (v) => submit(() => payApi.requestMyRefund({ amount: v.amount!, reason: v.reason! }), (r) => `Refund ${r.refundNumber}: ${statusLabel(r.status).toLowerCase()}`),
                  }),
              }}
            />
          </div>
        )}
        {(refunds.data ?? []).length > 0 && (
          <div data-my-refunds style={{ marginTop: 12 }}>
            {(refunds.data ?? []).map((r) => (
              <ListRow key={r.id} icon={RotateCcw} tone="blue" title={`${r.refundNumber} · ${money(r.amount)}`} meta={r.reason} badge={{ label: statusLabel(r.status), kind: r.status === "completed" ? "done" : "review" }} />
            ))}
          </div>
        )}
      </BentoTile>

      <SidePanel
        open={!!detail}
        onClose={() => setDetailId(null)}
        title={detail?.description ?? "Invoice"}
        eyebrow={detail ? `Fees · ${detail.chargeNumber}` : undefined}
        chip={detail ? (() => {
          const st = invoiceState(detail, today);
          return st === "other" ? { label: statusLabel(detail.status) } : INVOICE_BADGE[st];
        })() : undefined}
        facts={
          detail
            ? [
                { label: "Total", value: money(detail.totalAmount) },
                ...(toPaise(detail.adjustmentAmount || "0") !== 0n ? [{ label: "Concessions / adjustments", value: money(detail.adjustmentAmount) }] : []),
                { label: "Paid", value: money(detail.settledAmount) },
                { label: "Due", value: money(detail.outstandingAmount) },
                { label: "Due date", value: formatDate(detail.dueDate) },
              ]
            : undefined
        }
        actions={
          detail && isOpen(detail) ? (
            <>
              {!open && (
                <button type="button" className="sb-btn" data-action="pay-invoice" onClick={() => { setPaying(detail); setDetailId(null); }}>
                  <CreditCard strokeWidth={1.75} aria-hidden="true" />
                  Pay now
                </button>
              )}
              {(detail.status === "published" || detail.status === "active") && (
                <button type="button" className="sb-btn sb-btn--ghost" data-action="dispute-charge" onClick={() => dispute(detail)}>
                  Dispute
                </button>
              )}
            </>
          ) : undefined
        }
      >
        {detail && (
          <div style={{ marginTop: 18 }}>
            <h3 style={{ fontSize: 15, marginBottom: 8 }}>Instalments</h3>
            {detail.instalments.length === 0 ? (
              <p style={{ color: "var(--sb-mute)", fontSize: 14 }}>One payment, no instalments.</p>
            ) : (
              <ul data-my-instalments style={{ listStyle: "none", margin: 0, padding: 0 }}>
                {detail.instalments.map((i) => (
                  <li key={i.id}>
                    <KeyValue label={`Instalment ${i.sequence} · due ${formatDate(i.dueDate)}`} value={<>{money(i.amount)} <Badge kind={i.status === "paid" ? "done" : i.status === "overdue" ? "attention" : "neutral"}>{statusLabel(i.status)}</Badge></>} />
                  </li>
                ))}
              </ul>
            )}
          </div>
        )}
      </SidePanel>

      {paying && (
        <PayDialog
          charge={paying}
          onClose={() => setPaying(null)}
          onReceipt={(r) => {
            setPaying(null);
            setReceipt(r);
          }}
        />
      )}
      {receipt && <ReceiptModal receipt={receipt} onClose={() => setReceipt(null)} />}
      {prompt}
      {dialog}
      {account && <span hidden data-fee-account={account.studentNumber ?? ""} />}
    </>
  );
}

/** Row kebab of an invoice: Details (side panel with instalments) and Dispute. */
function ChargeMenu({ charge, onDetails, onDispute }: { charge: ChargeDetail; onDetails: () => void; onDispute: () => void }) {
  const items = [{ id: "my-charge-details", label: "Details & instalments", onSelect: onDetails }];
  if (charge.status === "published" || charge.status === "active") items.push({ id: "dispute-charge", label: "Dispute", onSelect: onDispute });
  return <RowMenu items={items} />;
}

function RowMenu({ items }: { items: { id: string; label: string; onSelect: () => void }[] }) {
  return <RowActions menu={items} label="Invoice actions" />;
}

/** Header summary of the student Fees page ("Aarav Desai · Roll no. SIT25-0001"). */
export function useFeesSummary(enabled: boolean): string | undefined {
  const api = usePaymentsApi();
  const accounts = useQuery({ queryKey: pkeys.scoped("me", "accounts"), queryFn: () => api.myAccounts(), enabled });
  const a = accounts.data?.[0]?.account;
  if (!a) return undefined;
  return [a.displayName, a.studentNumber ? `Roll no. ${a.studentNumber}` : null].filter(Boolean).join(" · ");
}
