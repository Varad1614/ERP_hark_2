/**
 * Admin → Finance → Overview (`/finance`), the prototype's one-page Finance (adm-finance.png):
 * four money KPIs (Collected / Billed / Pending / Collection rate), Monthly collections from real
 * payments, Fee adjustments waiting for a decision as approval cards, and the Invoices table with
 * All / Paid / Pending / Overdue chips. Every figure comes from the 5A / 5B APIs — nothing is
 * invented (the prototype's yearly "Target" has no backend, so the second tile is "Billed").
 */
import { BadgeIndianRupee, CircleDollarSign, Hourglass, Percent, ReceiptText, Wallet } from "lucide-react";
import { ApprovalCard, ApprovalList, Badge, BarChart, BentoTile, DataTable, EmptyState, FilterChips, KpiTile, formatDateShort, type BarDatum } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { REASON, todayIso } from "../institution/forms";
import { statusLabel, useCommand } from "../institution/hooks";
import type { Institution } from "../institution/types";
import { fkeys, useChargesApi } from "./charges/hooks";
import type { Adjustment, ChargeListItem } from "./charges/types";
import { pkeys, usePaymentsApi } from "./payments/hooks";
import { BILLED, INVOICE_BADGE, compact, invoiceState, money, paiseToAmount, rate, sumPaise, useFinanceAsk, type InvoiceState } from "./ui";

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const MONTH_NAMES = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

/** `YYYY-MM` keys of the last `n` months ending with `end` (inclusive). */
function monthKeys(end: string, n: number): string[] {
  let [y, m] = end.split("-").map(Number) as [number, number];
  const out: string[] = [];
  for (let i = 0; i < n; i++) {
    out.unshift(`${y}-${String(m).padStart(2, "0")}`);
    m -= 1;
    if (m === 0) {
      m = 12;
      y -= 1;
    }
  }
  return out;
}

const KIND_LABEL: Record<string, string> = { credit_note: "Credit note", debit_note: "Debit note", concession: "Concession", waiver: "Waiver", write_off: "Write-off" };

export function FinanceOverview({ institution }: { institution: Institution | null }) {
  const charges = useChargesApi();
  const payApi = usePaymentsApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useFinanceAsk();
  const [chip, setChip] = useState<"all" | InvoiceState>("all");
  const iid = institution?.id;
  const list = useQuery({ queryKey: fkeys.scoped(`${iid}:`, "charges"), queryFn: () => charges.charges(iid!), enabled: !!iid });
  const adjustments = useQuery({ queryKey: fkeys.scoped(iid ?? "-", "adjustments"), queryFn: () => charges.adjustments(iid!), enabled: !!iid });
  const payments = useQuery({ queryKey: pkeys.scoped(iid ?? "-", "payments"), queryFn: () => payApi.payments(iid!), enabled: !!iid });
  if (!institution)
    return (
      <BentoTile span={12} data-slice="R5">
        <EmptyState icon={BadgeIndianRupee} title="No institution selected">
          Register an institution in Campus setup → Institution first.
        </EmptyState>
      </BentoTile>
    );

  const today = todayIso();
  const all = list.data?.items ?? [];
  const billed = all.filter((c) => BILLED.includes(c.status));
  const completed = (payments.data ?? []).filter((p) => p.status === "completed");
  const collected = sumPaise(completed, (p) => p.amount);
  const total = sumPaise(billed, (c) => c.totalAmount);
  const pending = sumPaise(billed, (c) => c.outstandingAmount);
  const settled = sumPaise(billed, (c) => c.settledAmount);
  const openInvoices = billed.filter((c) => invoiceState(c, today) === "pending" || invoiceState(c, today) === "overdue");
  const dueDates = new Set(openInvoices.map((c) => c.dueDate).filter(Boolean));

  // Monthly collections: completed payments by the month they were received (last 6 months).
  const byMonth = new Map<string, bigint>();
  for (const p of completed) {
    const k = p.receivedOn.slice(0, 7);
    byMonth.set(k, (byMonth.get(k) ?? 0n) + sumPaise([p], (x) => x.amount));
  }
  const thisMonth = today.slice(0, 7);
  const first = [...byMonth.keys()].sort()[0];
  const keys = monthKeys(thisMonth, 6).filter((k) => !first || k >= first);
  const bars: BarDatum[] = keys.map((k) => {
    const paise = byMonth.get(k) ?? 0n;
    const amount = paiseToAmount(paise);
    return { label: MONTHS[Number(k.slice(5)) - 1] ?? k, value: Number(paise / 100n), display: compact(amount), soft: k === thisMonth };
  });
  const peak = keys.reduce<string | null>((best, k) => ((byMonth.get(k) ?? 0n) > (best ? (byMonth.get(best) ?? 0n) : -1n) ? k : best), null);
  const firstKey = keys[0];
  const range = !firstKey ? "" : firstKey === thisMonth ? MONTH_NAMES[Number(thisMonth.slice(5)) - 1] ?? "" : `${MONTH_NAMES[Number(firstKey.slice(5)) - 1]} to ${MONTH_NAMES[Number(thisMonth.slice(5)) - 1]}`;
  const lakhs = [...byMonth.values()].some((p) => p >= 10_000_000n);

  // Fee adjustments: pending decisions first, then drafts waiting to be submitted.
  const byCharge = new Map(all.map((c) => [c.id, c]));
  const queue = (adjustments.data ?? []).filter((a) => a.status === "pending_approval" || a.status === "draft").sort((a, b) => (a.status === b.status ? a.createdAt.localeCompare(b.createdAt) : a.status === "pending_approval" ? -1 : 1));
  const approve = (a: Adjustment) =>
    a.status === "pending_approval"
      ? void run(() => charges.decideAdjustment(a.id, a.version, "approve"), `${a.adjustmentNumber} approved and posted`)
      : void run(() => charges.submitAdjustment(a.id, a.version), (r) => `${r.adjustmentNumber}: ${statusLabel(r.status)}`);
  const reject = (a: Adjustment) =>
    ask({ title: `Reject ${a.adjustmentNumber}`, eyebrow: "Fee adjustments", name: "reject-adjustment", submitLabel: "Reject", fields: [REASON], onSubmit: (v) => submit(() => charges.decideAdjustment(a.id, a.version, "reject", v.reason), "Adjustment rejected") });

  // Invoices with status chips.
  const states = new Map(billed.map((c) => [c.id, invoiceState(c, today)]));
  const count = (s: InvoiceState) => billed.filter((c) => states.get(c.id) === s).length;
  const rows = chip === "all" ? billed : billed.filter((c) => states.get(c.id) === chip);

  return (
    <>
      <KpiTile icon={Wallet} tone="green" label="Collected" value={compact(paiseToAmount(collected))} sub={`${completed.length.toLocaleString("en-IN")} ${completed.length === 1 ? "payment" : "payments"} received`} loading={payments.isLoading} data-kpi="collected" />
      <KpiTile icon={CircleDollarSign} tone="blue" label="Billed" value={compact(paiseToAmount(total))} sub={`${billed.length.toLocaleString("en-IN")} ${billed.length === 1 ? "invoice" : "invoices"} raised`} loading={list.isLoading} data-kpi="billed" />
      <KpiTile icon={Hourglass} tone="amber" label="Pending" value={compact(paiseToAmount(pending))} sub={`Across ${openInvoices.length} open ${openInvoices.length === 1 ? "invoice" : "invoices"}${dueDates.size > 1 ? ` · ${dueDates.size} due dates` : ""}`} loading={list.isLoading} data-kpi="pending" />
      <KpiTile icon={Percent} tone="violet" label="Collection rate" value={rate(settled, total)} sub="Of the amount billed" loading={list.isLoading} data-kpi="collection-rate" />

      <BentoTile span={8} title="Monthly collections" data-collections-chart action={range ? <span style={{ fontSize: 13, fontWeight: 500, color: "var(--sb-mute)" }}>{lakhs ? `${range} · ₹ lakh` : range}</span> : undefined}>
        {completed.length === 0 ? (
          <EmptyState icon={ReceiptText} title="No collections yet">
            Recorded and online payments appear here by the month they were received.
          </EmptyState>
        ) : (
          <BarChart
            label="Monthly collections"
            data={bars}
            height={300}
            note={`${peak && peak !== thisMonth ? `${MONTH_NAMES[Number(peak.slice(5)) - 1]} is the highest month so far. ` : ""}${MONTH_NAMES[Number(thisMonth.slice(5)) - 1]} is month to date.`}
          />
        )}
      </BentoTile>

      <BentoTile span={4} title="Fee adjustments" data-fee-adjustments action={<span style={{ fontSize: 13, fontWeight: 500, color: "var(--sb-mute)" }}>{queue.length ? `${queue.length} to review` : "All clear"}</span>}>
        {queue.length === 0 ? (
          <EmptyState icon={CircleDollarSign} title="Nothing to review" compact>
            Concessions, waivers and credit notes waiting for a decision appear here.
          </EmptyState>
        ) : (
          <div style={{ maxHeight: 420, overflowY: "auto" }}>
            <ApprovalList>
              {queue.slice(0, 8).map((a) => {
                const c = byCharge.get(a.chargeId);
                return (
                  <ApprovalCard
                    key={a.id}
                    icon={BadgeIndianRupee}
                    tone="green"
                    data-adjustment-card={a.adjustmentNumber}
                    title={`${c?.displayName ?? a.chargeNumber} · ${a.reason}`}
                    meta={[c?.studentNumber, `${KIND_LABEL[a.kind] ?? statusLabel(a.kind)} ${money(a.effect)}`].filter(Boolean).join(" · ")}
                    approveLabel={a.status === "draft" ? "Submit" : "Approve"}
                    onApprove={() => approve(a)}
                    onReject={a.status === "pending_approval" ? () => reject(a) : undefined}
                    busy={busy}
                  />
                );
              })}
            </ApprovalList>
          </div>
        )}
      </BentoTile>

      <BentoTile span={12} data-invoices>
        <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", gap: 16, flexWrap: "wrap", marginBottom: 8 }}>
          <h4 style={{ margin: 0 }}>Invoices</h4>
          <FilterChips
            value={chip}
            onChange={(id) => setChip(id as typeof chip)}
            options={[
              { id: "all", label: "All", count: billed.length },
              { id: "paid", label: "Paid", count: count("paid") },
              { id: "pending", label: "Pending", count: count("pending") },
              { id: "overdue", label: "Overdue", count: count("overdue") },
            ]}
          />
        </div>
        <DataTable<ChargeListItem>
          caption="Invoices"
          rows={rows}
          rowKey={(c) => c.id}
          empty={<EmptyState icon={ReceiptText} title={chip === "all" ? "No invoices yet" : `No ${chip} invoices`}>Charges published to students appear here.</EmptyState>}
          columns={[
            { key: "n", header: "Invoice", render: (c) => <b data-invoice={c.chargeNumber}>{c.chargeNumber}</b> },
            { key: "s", header: "Student", render: (c) => c.displayName },
            { key: "f", header: "Fee", render: (c) => <span style={{ color: "var(--sb-mute)" }}>{c.description}</span> },
            { key: "d", header: "Due", render: (c) => <span style={{ color: "var(--sb-mute)" }}>{formatDateShort(c.dueDate)}</span> },
            { key: "a", header: "Amount", render: (c) => <b>{money(c.totalAmount)}</b> },
            {
              key: "st",
              header: "Status",
              render: (c) => {
                const st = states.get(c.id) ?? "other";
                const b = st === "other" ? { label: statusLabel(c.status), kind: "neutral" as const } : INVOICE_BADGE[st];
                return <Badge kind={b.kind}>{b.label}</Badge>;
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
