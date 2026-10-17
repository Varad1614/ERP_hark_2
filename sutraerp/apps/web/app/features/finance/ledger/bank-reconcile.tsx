/**
 * ERP_EXTENSIONS 1B: reconcile a bank account against the statement's ending balance (ported
 * from Everything's accounting reconcile screen), the bank reconciliation statement (BRS) and
 * changes made after reconciliation. Entries the bank has already shown on an imported statement
 * come pre-ticked; the figures update as boxes are ticked and the server re-checks them on finish
 * (difference must be zero). Money stays in paise (`bigint`), never floats.
 */
import { Scale } from "lucide-react";
import { Badge, BentoTile, Button, DataTable, EmptyState, FormGrid, FormSelect, KeyValue, TextField, formatDate } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useEffect, useMemo, useState } from "react";
import { todayIso } from "../../institution/forms";
import { useCommand } from "../../institution/hooks";
import { ledger } from "../ui";
import { lkeys, useLedgerApi } from "./hooks";
import { fromPaise, toPaise } from "./mock";
import type { BankReconItem, Brs } from "./types";

export interface ReconFigures {
  beginning: string;
  increases: string;
  decreases: string;
  increaseCount: number;
  decreaseCount: number;
  cleared: string;
  ending: string;
  difference: string;
  balanced: boolean;
}

/** Everything's `summarize` for a bank (debit-natural): the reconcile screen's figures. */
export function reconFigures(beginning: string, items: BankReconItem[], selected: ReadonlySet<string>, ending: string): ReconFigures {
  let inc = 0n;
  let dec = 0n;
  let nIn = 0;
  let nOut = 0;
  for (const it of items) {
    if (!selected.has(it.journalEntryId)) continue;
    const a = toPaise(it.amount);
    if (a >= 0n) {
      inc += a;
      nIn++;
    } else {
      dec += -a;
      nOut++;
    }
  }
  const cleared = toPaise(beginning) + inc - dec;
  const end = /^-?\d+(\.\d{1,2})?$/.test(ending.trim()) ? toPaise(ending.trim()) : 0n;
  const diff = end - cleared;
  return { beginning: fromPaise(toPaise(beginning)), increases: fromPaise(inc), decreases: fromPaise(dec), increaseCount: nIn, decreaseCount: nOut, cleared: fromPaise(cleared), ending: fromPaise(end), difference: fromPaise(diff), balanced: diff === 0n };
}

/** The BRS as label/amount lines in the Indian textbook order. */
export function brsLines(b: Brs): { label: string; amount: string; strong?: boolean }[] {
  return [
    { label: "Balance as per cash book", amount: b.bookBalance, strong: true },
    { label: "Add: cheques issued but not presented", amount: b.issuedNotPresented },
    { label: "Less: cheques deposited but not credited", amount: b.depositedNotCredited },
    { label: "Add: bank credits not yet in the books", amount: b.bankCreditsNotBooked },
    { label: "Less: bank debits not yet in the books", amount: b.bankDebitsNotBooked },
    { label: "Balance as per pass book", amount: b.bankBalance, strong: true },
  ];
}

export function BankReconcileTile({ institutionId, bankAccountId }: { institutionId: string; bankAccountId: string }) {
  const api = useLedgerApi();
  const { run, busy, dialog } = useCommand();
  const [view, setView] = useState<"reconcile" | "brs" | "history">("reconcile");
  const [date, setDate] = useState("");
  const [ending, setEnding] = useState("");
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [asOf, setAsOf] = useState(todayIso());
  const ws = useQuery({
    queryKey: lkeys.scoped(bankAccountId, `recon-${date}`),
    queryFn: () => api.reconWorkspace(bankAccountId, date || undefined),
  });
  const history = useQuery({ queryKey: lkeys.scoped(bankAccountId, "recon-history"), queryFn: () => api.reconHistory(bankAccountId), enabled: view === "history" });
  const brs = useQuery({ queryKey: lkeys.scoped(bankAccountId, `brs-${asOf}`), queryFn: () => api.brs(bankAccountId, asOf), enabled: view === "brs" });
  const disc = useQuery({ queryKey: lkeys.scoped(institutionId, "recon-discrepancies"), queryFn: () => api.reconDiscrepancies(institutionId), enabled: view === "history" });
  const w = ws.data;
  // A fresh workspace pre-ticks what the bank has shown and proposes the statement's closing.
  useEffect(() => {
    if (!w) return;
    if (!date) setDate(w.statementDate);
    setSelected(new Set(w.items.filter((i) => i.cleared).map((i) => i.journalEntryId)));
    if (w.statementClosingBalance) setEnding(w.statementClosingBalance);
  }, [w, date]);
  const fig = useMemo(() => (w ? reconFigures(w.beginningBalance, w.items, selected, ending) : null), [w, selected, ending]);
  const toggle = (id: string) =>
    setSelected((s) => {
      const n = new Set(s);
      if (n.has(id)) n.delete(id);
      else n.add(id);
      return n;
    });
  const last = w?.lastReconciliation;
  return (
    <BentoTile
      span={12}
      title="Reconcile with bank"
      data-bank-reconcile
      action={
        <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
          {view === "reconcile" && fig && <Badge kind={fig.balanced ? "done" : "attention"}>{fig.balanced ? "Difference ₹0.00" : `Difference ${ledger(fig.difference)}`}</Badge>}
          <div style={{ minWidth: 180 }}>
            <FormSelect
              label="View"
              hideLabel
              value={view}
              onChange={(e) => setView(e.target.value as typeof view)}
              options={[
                { value: "reconcile", label: "Reconcile" },
                { value: "brs", label: "Bank reconciliation statement" },
                { value: "history", label: "History & changes" },
              ]}
              data-recon-view
            />
          </div>
        </div>
      }
    >
      {view === "reconcile" && (
        <>
          <FormGrid columns={3}>
            <TextField label="Statement date" type="date" value={date} onChange={(e) => setDate(e.target.value)} data-recon-date />
            <TextField label="Statement ending balance (₹)" value={ending} onChange={(e) => setEnding(e.target.value)} inputMode="decimal" data-recon-ending />
            <div style={{ alignSelf: "end" }}>
              <Button
                data-recon-finish
                disabled={!fig?.balanced || selected.size === 0 || busy}
                onClick={() =>
                  void run(
                    () => api.reconFinish(bankAccountId, { statementDate: date, endingBalance: ending.trim(), journalEntryIds: [...selected] }),
                    (r) => `Reconciled to ${formatDate(r.statementDate)} · ${r.clearedCount} entries`,
                  )
                }
              >
                Finish reconciliation
              </Button>
            </div>
          </FormGrid>
          {fig && (
            <p data-recon-figures style={{ margin: "12px 0" }}>
              Opening {ledger(fig.beginning)} · deposits {ledger(fig.increases)} ({fig.increaseCount}) · payments {ledger(fig.decreases)} ({fig.decreaseCount}) · cleared <b>{ledger(fig.cleared)}</b>
              {last ? ` · last reconciled to ${formatDate(last.statementDate)}` : ""}
            </p>
          )}
          <DataTable<BankReconItem>
            caption="Unreconciled entries"
            rows={w?.items ?? []}
            rowKey={(i) => i.journalEntryId}
            empty={<EmptyState icon={Scale} title="Nothing to reconcile">Every entry up to this date is reconciled.</EmptyState>}
            columns={[
              {
                key: "t",
                header: "",
                render: (i) => <input type="checkbox" aria-label={`Tick ${i.entryNumber}`} checked={selected.has(i.journalEntryId)} onChange={() => toggle(i.journalEntryId)} data-recon-tick={i.entryNumber} />,
              },
              { key: "d", header: "Date", render: (i) => formatDate(i.postingDate) },
              { key: "n", header: "Entry", render: (i) => i.entryNumber },
              { key: "x", header: "Description", render: (i) => i.description },
              { key: "a", header: "Amount", render: (i) => ledger(i.amount) },
              { key: "c", header: "Bank", render: (i) => (i.cleared ? <Badge kind="done">On statement</Badge> : "") },
            ]}
          />
        </>
      )}
      {view === "brs" && (
        <>
          <FormGrid columns={3}>
            <TextField label="As of" type="date" value={asOf} onChange={(e) => setAsOf(e.target.value)} data-brs-as-of />
          </FormGrid>
          {brs.data && (
            <div data-brs style={{ marginTop: 12 }}>
              {brsLines(brs.data).map((l) => (
                <KeyValue key={l.label} label={l.strong ? <b>{l.label}</b> : l.label} value={l.strong ? <b>{ledger(l.amount)}</b> : ledger(l.amount)} />
              ))}
              {brs.data.statementBalance != null && (
                <KeyValue
                  label="Statement closing balance"
                  value={
                    <>
                      {ledger(brs.data.statementBalance)}{" "}
                      <Badge kind={brs.data.difference === "0.00" ? "done" : "attention"}>{brs.data.difference === "0.00" ? "Agrees" : `Off by ${ledger(brs.data.difference ?? "0.00")}`}</Badge>
                    </>
                  }
                />
              )}
              <DataTable
                caption="Items behind the adjustments"
                rows={[
                  ...brs.data.issued.map((x) => ({ ...x, kind: "Issued, not presented" })),
                  ...brs.data.deposited.map((x) => ({ ...x, kind: "Deposited, not credited" })),
                  ...brs.data.bankCredits.map((x) => ({ ...x, kind: "Bank credit, not booked" })),
                  ...brs.data.bankDebits.map((x) => ({ ...x, kind: "Bank debit, not booked" })),
                ]}
                rowKey={(x) => `${x.kind}-${x.journalEntryId ?? x.statementLineId}`}
                empty={<EmptyState icon={Scale} title="Nothing outstanding">The books and the bank agree item for item.</EmptyState>}
                columns={[
                  { key: "k", header: "Type", render: (x) => x.kind },
                  { key: "d", header: "Date", render: (x) => formatDate(x.date) },
                  { key: "r", header: "Reference", render: (x) => x.reference ?? x.description ?? "—" },
                  { key: "a", header: "Amount", render: (x) => ledger(x.amount) },
                ]}
              />
            </div>
          )}
        </>
      )}
      {view === "history" && (
        <>
          <DataTable
            caption="Reconciliations"
            rows={history.data ?? []}
            rowKey={(r) => r.id}
            empty={<EmptyState icon={Scale} title="No reconciliations yet">Finish one to carry its balance forward.</EmptyState>}
            columns={[
              { key: "d", header: "Statement date", render: (r) => formatDate(r.statementDate) },
              { key: "b", header: "Opening", render: (r) => ledger(r.beginningBalance) },
              { key: "e", header: "Ending", render: (r) => ledger(r.endingBalance) },
              { key: "c", header: "Entries", render: (r) => String(r.clearedCount) },
              { key: "s", header: "Status", render: (r) => <Badge kind={r.status === "done" ? "done" : "neutral"}>{r.status === "done" ? "Done" : "Undone"}</Badge> },
              {
                key: "x",
                header: "",
                render: (r) =>
                  last && r.id === last.id ? (
                    <Button size="sm" variant="ghost" disabled={busy} data-recon-undo onClick={() => void run(() => api.reconUndo(bankAccountId, r.id), () => "Last reconciliation undone")}>
                      Undo
                    </Button>
                  ) : null,
              },
            ]}
          />
          <DataTable
            caption="Changed after reconciliation"
            rows={disc.data ?? []}
            rowKey={(d) => `${d.reconciliationId}-${d.journalEntryId}`}
            empty={<EmptyState icon={Scale} title="No changes found">Reconciled entries still carry the amounts they were reconciled at.</EmptyState>}
            columns={[
              { key: "a", header: "Account", render: (d) => d.bankAccountName },
              { key: "s", header: "Statement date", render: (d) => formatDate(d.statementDate) },
              { key: "n", header: "Entry", render: (d) => d.entryNumber ?? "Deleted" },
              { key: "w", header: "Reconciled", render: (d) => ledger(d.reconciledAmount) },
              { key: "c", header: "Now", render: (d) => ledger(d.currentAmount) },
              { key: "x", header: "Change", render: (d) => ledger(d.change) },
            ]}
          />
        </>
      )}
      {dialog}
    </BentoTile>
  );
}
