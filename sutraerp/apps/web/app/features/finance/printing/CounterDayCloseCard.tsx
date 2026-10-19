/**
 * ERP_EXTENSIONS 3E: the fee counter's day close — receipts of a day (or only the clerk's own),
 * cancelled count, total and the split by channel, printable on the counter printer.
 */
import { Printer } from "lucide-react";
import { BentoTile, Button, Checkbox, DataTable, DatePicker, EmptyState, FormGrid, KeyValue, useToast } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { problemText, statusLabel } from "../../institution/hooks";
import { money } from "../ui";
import { defaultPrintingApi, type PrintingApi } from "./api";
import { outcomeText, printBuilt } from "./print";
import { printerColumns } from "./printer";

/** Today on the device's calendar (YYYY-MM-DD), not UTC. */
export function localToday(now = new Date()): string {
  const p = (n: number) => String(n).padStart(2, "0");
  return `${now.getFullYear()}-${p(now.getMonth() + 1)}-${p(now.getDate())}`;
}

export function CounterDayCloseCard({ institutionId, api = defaultPrintingApi(), span = 12 }: { institutionId: string; api?: PrintingApi; span?: 6 | 12 }) {
  const toast = useToast();
  const [date, setDate] = useState(localToday());
  const [mine, setMine] = useState(false);
  const [busy, setBusy] = useState(false);
  const columns = printerColumns();
  const q = useQuery({ queryKey: ["institution", "3e", institutionId, "counter-report", date, mine, columns], queryFn: () => api.counterReport(institutionId, { date, columns, mine }), enabled: !!date });
  const r = q.data;
  const print = async () => {
    if (!r) return;
    setBusy(true);
    try {
      toast(outcomeText(await printBuilt({ ...r, columns }, `Counter close ${r.date}`), "Day close"));
    } catch (e) {
      toast(problemText(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <BentoTile
      span={span}
      title="Counter day close"
      data-slice="3E"
      data-counter-close
      action={
        <Button size="sm" icon={<Printer strokeWidth={1.75} />} disabled={busy || !r} data-counter-print onClick={() => void print()}>
          Print
        </Button>
      }
    >
      <FormGrid columns={2}>
        <DatePicker label="Day" value={date} max={localToday()} onChange={(e) => setDate(e.target.value)} data-counter-date />
        <div style={{ alignSelf: "end" }}>
          <Checkbox label="Only my receipts" checked={mine} onChange={(e) => setMine(e.target.checked)} data-counter-mine />
        </div>
      </FormGrid>
      {q.isError && <p style={{ color: "var(--sb-danger, #b42318)", fontSize: 13 }}>{problemText(q.error)}</p>}
      {r && (
        <>
          <FormGrid columns={3}>
            <KeyValue label="Receipts" value={<span data-counter-count>{r.receiptCount}</span>} />
            <KeyValue label="Cancelled" value={<span data-counter-cancelled>{r.cancelledCount}</span>} />
            <KeyValue label="Total" value={<b data-counter-total>{r.totalDisplay}</b>} />
          </FormGrid>
          <DataTable
            caption="By channel"
            rows={r.byChannel}
            rowKey={(c) => c.channel}
            empty={<EmptyState icon={Printer} title="No receipts that day" compact>Receipts issued at the counter appear here.</EmptyState>}
            columns={[
              { key: "c", header: "Channel", render: (c) => statusLabel(c.channel) },
              { key: "n", header: "Receipts", render: (c) => String(c.count) },
              { key: "a", header: "Amount", render: (c) => money(c.amount) },
            ]}
          />
          <details style={{ marginTop: 12 }}>
            <summary style={{ cursor: "pointer", fontSize: 13 }}>Slip preview</summary>
            <pre data-counter-preview style={{ fontFamily: "ui-monospace, 'Courier New', monospace", fontSize: 12, lineHeight: 1.3, whiteSpace: "pre-wrap", maxWidth: `${columns + 2}ch`, background: "var(--sb-bg, #f6f6f6)", padding: 8, borderRadius: 8 }}>{r.preview}</pre>
          </details>
        </>
      )}
    </BentoTile>
  );
}
