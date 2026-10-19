/**
 * ERP_EXTENSIONS 1C: financial statements read from the journal — income & expenditure, balance
 * sheet, cash flow and general ledger, with period columns and comparisons. Everything is
 * computed on the server (`/financial-statements/*`); this screen only picks the period and
 * lays the rows out. CSV export keeps the figures exactly as served.
 */
import { FileSpreadsheet } from "lucide-react";
import { Badge, BentoTile, Button, DataTable, EmptyState, FormGrid, FormSelect, TextField } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { csvRow } from "../../../lib/csv";
import { todayIso } from "../../institution/forms";
import { ledger } from "../ui";
import { lkeys, useLedgerApi } from "./hooks";
import type { FinancialStatement, StatementKind, StatementParams, StatementRow } from "./types";

const KINDS: { value: StatementKind; label: string }[] = [
  { value: "income-expenditure", label: "Income & expenditure" },
  { value: "balance-sheet", label: "Balance sheet" },
  { value: "cash-flow", label: "Cash flow" },
  { value: "general-ledger", label: "General ledger" },
];

/** First day of the Indian financial year (April) containing an ISO date. */
export function fyStart(iso: string): string {
  const y = Number(iso.slice(0, 4));
  const m = Number(iso.slice(5, 7));
  return `${m >= 4 ? y : y - 1}-04-01`;
}

/** A cell as shown: money with paise, a percent column from basis points, blank for null. */
export function cellText(v: string | null | undefined, percent: boolean): string {
  if (v === null || v === undefined) return "";
  if (percent) return `${(Number(v) / 100).toFixed(1)}%`;
  return ledger(v);
}

const STRONG = new Set(["total", "grand", "subtotal"]);

/** CSV of a statement exactly as served (one row per line, label indented by depth). */
export function statementCsv(s: FinancialStatement): string {
  // Audit 2026-10 A2: account / group labels are user-entered, so cells go through lib/csv
  // (quoting + spreadsheet formula neutralisation; plain numbers such as -2227.00 stay numbers).
  const head = ["", ...s.columns.map((c) => c.label)];
  const lines = s.rows.map((r) => [`${"  ".repeat(r.depth)}${r.label}`, ...r.values.map((v) => v ?? "")]);
  return [head, ...lines].map(csvRow).join("\n");
}

export function FinancialStatementsTile({ institutionId }: { institutionId: string }) {
  const api = useLedgerApi();
  const today = todayIso();
  const [kind, setKind] = useState<StatementKind>("income-expenditure");
  const [from, setFrom] = useState(fyStart(today));
  const [to, setTo] = useState(today);
  const [columns, setColumns] = useState("total");
  const [compare, setCompare] = useState("");
  const params: StatementParams = kind === "balance-sheet"
    ? { asOf: to, columns: compare ? undefined : columns === "year" ? "total" : columns, comparePreviousYear: compare === "previous_year" }
    : kind === "income-expenditure"
      ? { from, to, columns: compare ? undefined : columns, compare: compare || undefined }
      : { from, to };
  const q = useQuery({
    queryKey: lkeys.scoped(institutionId, `statement-${kind}-${JSON.stringify(params)}`),
    queryFn: () => api.financialStatement(institutionId, kind, params),
  });
  const s = q.data;
  const pctIndex = s ? s.columns.findIndex((c) => c.key === "pct") : -1;
  const download = () => {
    if (!s) return;
    const url = URL.createObjectURL(new Blob([statementCsv(s)], { type: "text/csv" }));
    const a = document.createElement("a");
    a.href = url;
    a.download = `${s.id}-${to}.csv`;
    a.click();
    URL.revokeObjectURL(url);
  };
  return (
    <BentoTile
      span={12}
      title="Financial statements"
      data-financial-statements={kind}
      action={
        <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
          {s?.balanced != null && <Badge kind={s.balanced ? "done" : "attention"}>{s.balanced ? "Balanced" : "Out of balance"}</Badge>}
          {s?.reconciled != null && <Badge kind={s.reconciled ? "done" : "attention"}>{s.reconciled ? "Agrees with cash and bank" : "Cash does not agree"}</Badge>}
          <Button size="sm" variant="ghost" icon={<FileSpreadsheet strokeWidth={1.75} />} disabled={!s} onClick={download} data-statement-csv>
            CSV
          </Button>
        </div>
      }
    >
      <FormGrid columns={3}>
        <FormSelect label="Statement" value={kind} onChange={(e) => setKind(e.target.value as StatementKind)} options={KINDS} data-statement-kind />
        {kind !== "balance-sheet" && <TextField label="From" type="date" value={from} onChange={(e) => setFrom(e.target.value)} />}
        <TextField label={kind === "balance-sheet" ? "As of" : "To"} type="date" value={to} onChange={(e) => setTo(e.target.value)} />
        {(kind === "income-expenditure" || kind === "balance-sheet") && (
          <FormSelect
            label="Columns"
            value={columns}
            onChange={(e) => setColumns(e.target.value)}
            options={[
              { value: "total", label: "Total only" },
              { value: "month", label: kind === "balance-sheet" ? "Last six month ends" : "By month" },
              { value: "quarter", label: kind === "balance-sheet" ? "Last six quarter ends" : "By quarter" },
              ...(kind === "income-expenditure" ? [{ value: "year", label: "By financial year" }] : []),
            ]}
          />
        )}
        {(kind === "income-expenditure" || kind === "balance-sheet") && (
          <FormSelect
            label="Compare with"
            value={compare}
            onChange={(e) => setCompare(e.target.value)}
            options={[
              { value: "", label: "Nothing" },
              ...(kind === "income-expenditure" ? [{ value: "previous_period", label: "Previous period" }] : []),
              { value: "previous_year", label: "Same period last year" },
            ]}
          />
        )}
      </FormGrid>
      {s && s.net != null && kind === "income-expenditure" && (
        <p data-statement-net style={{ margin: "12px 0" }}>
          {s.subtitle} · {Number(s.net) >= 0 ? "Surplus" : "Deficit"} <b>{ledger(s.net.replace("-", ""))}</b>
        </p>
      )}
      <DataTable<StatementRow & { i: number }>
        caption={s?.title ?? "Statement"}
        rows={(s?.rows ?? []).map((r, i) => ({ ...r, i }))}
        rowKey={(r) => String(r.i)}
        empty={<EmptyState icon={FileSpreadsheet} title="Nothing posted in this period">Charges, payments and journals appear here once posted.</EmptyState>}
        columns={[
          {
            key: "l",
            header: "",
            render: (r) => (
              <span data-statement-row={r.key ?? r.accountId ?? ""} style={{ paddingLeft: r.depth * 16, fontWeight: STRONG.has(r.kind) || r.emphasis ? 600 : r.kind === "section" ? 500 : 400 }}>
                {r.detail ? `${r.detail.date} · ${r.detail.entryNumber} · ${r.label}` : r.label}
              </span>
            ),
          },
          ...(s?.columns ?? []).map((c, ci) => ({
            key: `c${ci}`,
            header: c.label,
            render: (r: StatementRow) => <span style={{ fontWeight: STRONG.has(r.kind) || r.emphasis ? 600 : 400 }}>{cellText(r.values[ci], ci === pctIndex)}</span>,
          })),
        ]}
      />
    </BentoTile>
  );
}
