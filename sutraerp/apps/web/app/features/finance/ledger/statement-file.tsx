/**
 * ERP_EXTENSIONS 1A: import the bank's own statement file (CSV / Excel-saved CSV, OFX/QFX,
 * MT940). The server reads the file (`…/statements/preview`), shows what importing it would do
 * (lines, the ones an earlier statement already holds, rows that are not transactions), and
 * imports it with the same format and column mapping. Parsing lives on the server only.
 */
import { FileUp } from "lucide-react";
import { Badge, Button, DataTable, Field, FormSelect, KeyValue, Modal, formatDate } from "@sutra/ui";
import { useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useCommand } from "../../institution/hooks";
import { ledger } from "../ui";
import { lkeys, useLedgerApi } from "./hooks";
import type { ColumnMapping, StatementFormat, StatementPreview } from "./types";

const FORMATS: { value: StatementFormat; label: string }[] = [
  { value: "auto", label: "Detect from the file" },
  { value: "csv", label: "CSV (bank download)" },
  { value: "mt940", label: "MT940 (SWIFT)" },
  { value: "ofx", label: "OFX / QFX" },
];
const ROLES: { key: keyof ColumnMapping; label: string }[] = [
  { key: "date", label: "Date" },
  { key: "description", label: "Narration" },
  { key: "reference", label: "Cheque / UTR" },
  { key: "debit", label: "Withdrawal" },
  { key: "credit", label: "Deposit" },
  { key: "amount", label: "Amount" },
  { key: "indicator", label: "Dr / Cr" },
];

export function StatementFileImport({ bankAccountId, open, onClose }: { bankAccountId: string; open: boolean; onClose: () => void }) {
  const api = useLedgerApi();
  const client = useQueryClient();
  const { run, busy, dialog } = useCommand();
  const [fileName, setFileName] = useState("");
  const [content, setContent] = useState("");
  const [format, setFormat] = useState<StatementFormat>("auto");
  const [dateStyle, setDateStyle] = useState<"dmy" | "mdy">("dmy");
  const [mapping, setMapping] = useState<ColumnMapping | null>(null);
  const [preview, setPreview] = useState<StatementPreview | null>(null);

  const close = () => {
    setFileName("");
    setContent("");
    setMapping(null);
    setPreview(null);
    onClose();
  };
  const read = async (text: string, m: ColumnMapping | null) => {
    const p = await run(() => api.previewStatement(bankAccountId, { content: text, format, dateStyle, mapping: m }));
    if (p) {
      setPreview(p);
      setMapping(p.mapping ?? null);
    }
  };
  const remap = (key: keyof ColumnMapping, value: string) => {
    const next: ColumnMapping = { ...(mapping ?? {}), [key]: value === "" ? null : Number(value) };
    // Separate withdrawal / deposit columns and a single amount column are alternatives.
    if ((key === "debit" || key === "credit") && value !== "") next.amount = null;
    if (key === "amount" && value !== "") {
      next.debit = null;
      next.credit = null;
    }
    setMapping(next);
    void read(content, next);
  };
  const importIt = async () => {
    const fmt = preview?.format as StatementFormat | undefined;
    const s = await run(
      () => api.importStatement(bankAccountId, { content, format: fmt, dateStyle, mapping: fmt === "csv" ? mapping : undefined }),
      (r) => `${r.statementRef} imported · ${r.lineCount} lines${r.duplicateLines ? ` · ${r.duplicateLines} already imported` : ""}`,
    );
    if (s) {
      await client.invalidateQueries({ queryKey: lkeys.scoped(bankAccountId, "statements") });
      close();
    }
  };

  return (
    <Modal open={open} onClose={close} label="Import a bank file">
      <div data-statement-file>
        <h2 style={{ marginTop: 0 }}>Import the bank's statement file</h2>
        <p style={{ marginTop: 0 }}>CSV from net banking (save Excel downloads as CSV), MT940 or OFX. Lines already imported from an earlier statement are skipped.</p>
        <div className="sb-actions-row" style={{ marginTop: 0 }}>
          <Field label="File" htmlFor="statement-file-input">
            <input
              id="statement-file-input"
              type="file"
              accept=".csv,.txt,.ofx,.qfx,.qbo,.sta,.940,.mt940"
              data-statement-file-input
              onChange={async (e) => {
                const f = e.target.files?.[0];
                if (!f) return;
                const text = await f.text();
                setFileName(f.name);
                setContent(text);
                setMapping(null);
                await read(text, null);
              }}
            />
          </Field>
          <div style={{ minWidth: 200 }}>
            <FormSelect label="Format" value={format} onChange={(e) => setFormat(e.target.value as StatementFormat)} options={FORMATS} />
          </div>
          <div style={{ minWidth: 180 }}>
            <FormSelect
              label="Dates like 03/04/2026"
              value={dateStyle}
              onChange={(e) => setDateStyle(e.target.value as "dmy" | "mdy")}
              options={[
                { value: "dmy", label: "Day first (3 April)" },
                { value: "mdy", label: "Month first (4 March)" },
              ]}
            />
          </div>
          {content && (
            <Button size="sm" variant="ghost" disabled={busy} onClick={() => void read(content, mapping)}>
              Read again
            </Button>
          )}
        </div>

        {preview && (
          <div data-statement-preview={preview.format}>
            <p>
              <b>{fileName}</b> · {preview.format.toUpperCase()} · {formatDate(preview.periodFrom)} – {formatDate(preview.periodTo)}
            </p>
            <KeyValue label="New lines" value={<span data-preview-new>{preview.newLines.toLocaleString("en-IN")}</span>} />
            <KeyValue label="Already imported" value={<span data-preview-duplicates>{preview.duplicateLines.toLocaleString("en-IN")}</span>} />
            <KeyValue label="Money in · out" value={`${ledger(preview.totalCredits)} · ${ledger(preview.totalDebits)}`} />
            {preview.closingBalance && <KeyValue label="Closing balance (file)" value={ledger(preview.closingBalance)} />}
            {preview.skipped.length > 0 && (
              <KeyValue label="Rows skipped" value={preview.skipped.slice(0, 5).map((s) => `row ${s.row}: ${s.reason}`).join(" · ") + (preview.skipped.length > 5 ? " …" : "")} />
            )}

            {preview.format === "csv" && preview.headers.length > 0 && (
              <div className="sb-actions-row" data-statement-mapping>
                {ROLES.map((r) => (
                  <div key={r.key} style={{ minWidth: 150 }}>
                    <FormSelect
                      label={r.label}
                      value={mapping?.[r.key] == null ? "" : String(mapping[r.key])}
                      onChange={(e) => remap(r.key, e.target.value)}
                      options={[{ value: "", label: "—" }, ...preview.headers.map((h, i) => ({ value: String(i), label: h || `Column ${i + 1}` }))]}
                    />
                  </div>
                ))}
              </div>
            )}

            <DataTable
              caption="Lines in the file"
              rows={preview.lines.slice(0, 50).map((l, i) => ({ ...l, i }))}
              rowKey={(l) => String(l.i)}
              columns={[
                { key: "d", header: "Date", render: (l) => formatDate(l.valueDate) },
                { key: "n", header: "Narration", render: (l) => l.description ?? "—" },
                { key: "r", header: "Cheque / UTR", render: (l) => l.reference ?? "—" },
                { key: "a", header: "Amount", render: (l) => ledger(l.direction === "debit" ? `-${l.amount}` : l.amount) },
                { key: "s", header: "", render: (l) => (l.duplicate ? <Badge kind="neutral">Already imported</Badge> : <Badge kind="info">New</Badge>) },
              ]}
            />
            {preview.lines.length > 50 && <p>Showing 50 of {preview.lines.length.toLocaleString("en-IN")} lines.</p>}
          </div>
        )}

        <div className="sb-actions-row">
          <Button variant="ghost" onClick={close}>
            Cancel
          </Button>
          <Button icon={<FileUp strokeWidth={1.75} />} data-statement-file-import disabled={busy || !preview || preview.newLines === 0} onClick={() => void importIt()}>
            {preview ? `Import ${preview.newLines.toLocaleString("en-IN")} lines` : "Import"}
          </Button>
        </div>
      </div>
      {dialog}
    </Modal>
  );
}
