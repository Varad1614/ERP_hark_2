/**
 * ERP_EXTENSIONS 5D: the "GST" tile of Admin → Finance → Fees & charges (docs/extensions-5d-gst.md).
 * Profile (GSTIN, state, registration, document series, default zero-tax note), the tax set-up of
 * every fee head (SAC, slab, exempt with its note, tax-inclusive pricing) and the GSTR-1 friendly
 * summary of a period with its CSV. Documents are issued by the server when charges publish.
 */
import { FileSpreadsheet, Receipt } from "lucide-react";
import { Badge, BentoTile, Button, DataTable, DatePicker, EmptyState, FormGrid, KeyValue } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { problemText, useCommand } from "../../institution/hooks";
import { money, rowActions, useFinanceAsk } from "../ui";
import { defaultGstApi, saveCsv, type GstApi } from "./api";
import { defaultPeriod, gkeys, kindLabel, noteLabel, profileInput, profileValues, taxLabel } from "./gst";
import type { GstFeeHead, GstProfile } from "./types";

type View = "profile" | "heads" | "summary";
const REG = [
  { value: "regular", label: "Regular" },
  { value: "composition", label: "Composition" },
  { value: "unregistered", label: "Not registered" },
];

export function GstTile({ institutionId, api = defaultGstApi() }: { institutionId: string; api?: GstApi }) {
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useFinanceAsk();
  const [view, setView] = useState<View>("profile");
  const [period, setPeriod] = useState(defaultPeriod);
  const profile = useQuery({ queryKey: gkeys.scoped(institutionId, "gst-profile"), queryFn: () => api.profile(institutionId) });
  const heads = useQuery({ queryKey: gkeys.scoped(institutionId, "gst-fee-heads"), queryFn: () => api.feeHeads(institutionId), enabled: view === "heads" });
  const summary = useQuery({ queryKey: gkeys.scoped(`${institutionId}:${period.from}:${period.to}`, "gst-summary"), queryFn: () => api.summary(institutionId, period), enabled: view === "summary" && !!period.from && !!period.to });
  const p = profile.data;
  const presets = p?.presets ?? [];
  const noteOptions = [{ value: "", label: "No note" }, ...presets.map((x) => ({ value: x.id, label: x.label }))];

  const editProfile = (pr: GstProfile) => {
    const init = profileValues(pr);
    ask({
      title: "GST profile",
      eyebrow: "Fees & charges",
      name: "gst-profile",
      submitLabel: "Save",
      fields: [
        { name: "registrationType", label: "Registration", type: "select", options: REG, initial: init.registrationType, required: true },
        { name: "gstin", label: "GSTIN", placeholder: "27AAPFU0939F1ZV", initial: init.gstin },
        { name: "legalName", label: "Legal name", required: true, initial: init.legalName },
        { name: "tradeName", label: "Trade name", initial: init.tradeName },
        { name: "stateCode", label: "State", type: "select", options: pr.states.map((s) => ({ value: s.code, label: `${s.code} · ${s.name}` })), initial: init.stateCode },
        { name: "defaultTaxNote", label: "Note on bills of supply", type: "select", options: noteOptions, initial: init.defaultTaxNote },
        { name: "invoicePrefix", label: "Tax invoice series", initial: init.invoicePrefix },
        { name: "billPrefix", label: "Bill of supply series", initial: init.billPrefix },
        { name: "creditNotePrefix", label: "Credit note series", initial: init.creditNotePrefix },
        { name: "address", label: "Address", wide: true, initial: init.address },
      ],
      onSubmit: (v) => submit(() => api.saveProfile(institutionId, profileInput(v)), "GST profile saved"),
    });
  };

  const editHead = (h: GstFeeHead) => {
    const t = h.tax;
    ask({
      title: `GST · ${h.code}`,
      eyebrow: h.name,
      name: "gst-fee-head",
      submitLabel: "Save",
      fields: [
        { name: "sac", label: "SAC / HSN", required: true, placeholder: "9992", initial: t?.sac ?? (h.category === "tuition" || h.category === "examination" || h.category === "admission" ? "9992" : "") },
        { name: "gstRate", label: "GST rate", type: "select", options: (p?.rates ?? [0, 5, 12, 18, 28]).map((r) => ({ value: String(r), label: `${r}%` })), initial: t?.gstRate ?? "0" },
        { name: "taxNote", label: "Note when no tax", type: "select", options: noteOptions, initial: t?.taxNote ?? "" },
        { name: "exempt", label: "Exempt supply", type: "checkbox", initial: String(t?.exempt ?? false) },
        { name: "taxInclusive", label: "Amounts include GST", type: "checkbox", initial: String(t?.taxInclusive ?? false) },
      ],
      onSubmit: (v) =>
        submit(
          () => api.saveFeeHead(institutionId, h.feeHeadId, { sac: (v.sac ?? "").trim(), gstRate: v.exempt === "true" ? "0" : v.gstRate || "0", exempt: v.exempt === "true", taxNote: v.taxNote ?? "", taxInclusive: v.taxInclusive === "true" }),
          `${h.code}: GST saved`,
        ),
    });
  };

  const download = () =>
    void run(async () => {
      const csv = await api.summaryCsv(institutionId, period);
      saveCsv(csv);
      return csv;
    }, (c) => `${c.fileName} downloaded`);

  const tabs = (
    <div className="flex gap-1" role="tablist">
      {(["profile", "heads", "summary"] as const).map((v) => (
        <Button key={v} size="sm" variant={view === v ? "primary" : "ghost"} data-gst-view={v} onClick={() => setView(v)}>
          {v === "profile" ? "Profile" : v === "heads" ? "Fee heads" : "Summary"}
        </Button>
      ))}
    </div>
  );

  return (
    <BentoTile span={12} title="GST" data-slice="5D" data-gst action={tabs}>
      {profile.isError && <p style={{ color: "var(--sb-danger, #b42318)", fontSize: 13 }}>{problemText(profile.error)}</p>}
      {view === "profile" && p && (
        <div data-gst-profile={p.configured ? "saved" : "default"}>
          {!p.configured && <p style={{ color: "var(--sb-mute)", fontSize: 14, marginBottom: 8 }}>No GST profile yet: charges are raised without GST documents until you save one.</p>}
          <FormGrid columns={3}>
            <KeyValue label="GSTIN" value={<span data-gst-gstin>{p.gstin ?? "—"}</span>} />
            <KeyValue label="Registration" value={REG.find((r) => r.value === p.registrationType)?.label ?? p.registrationType} />
            <KeyValue label="State" value={p.stateName ? `${p.stateCode} · ${p.stateName}` : p.stateCode} />
            <KeyValue label="Legal name" value={p.legalName || "—"} />
            <KeyValue label="Next numbers" value={p.sampleNumbers.join(" · ")} />
            <KeyValue label="Note on bills of supply" value={noteLabel(presets, p.defaultTaxNote)} />
          </FormGrid>
          <div style={{ marginTop: 12 }}>
            <Button size="sm" data-action="edit-gst-profile" disabled={busy} onClick={() => editProfile(p)}>
              {p.configured ? "Edit profile" : "Set up GST"}
            </Button>
          </div>
        </div>
      )}
      {view === "heads" && (
        <DataTable caption="Fee head GST" rows={heads.data ?? []} rowKey={(h) => h.feeHeadId}
          empty={<EmptyState icon={Receipt} title="No fee heads">Create fee heads first, then set their GST here.</EmptyState>}
          columns={[
            { key: "c", header: "Fee head", render: (h) => <span data-gst-head={h.code}>{`${h.code} · ${h.name}`}</span> },
            { key: "s", header: "SAC", render: (h) => h.tax?.sac ?? "—" },
            { key: "r", header: "GST", render: (h) => <span data-gst-head-rate={h.code}>{taxLabel(h.tax)}</span> },
            { key: "n", header: "Note", render: (h) => (h.tax ? noteLabel(presets, h.tax.taxNote) : "—") },
            { key: "i", header: "Prices", render: (h) => (h.tax ? (h.tax.taxInclusive ? "Include GST" : "Plus GST") : "—") },
            {
              key: "a",
              header: "",
              render: (h) =>
                rowActions([
                  { id: "gst-head-edit", label: h.tax ? "Edit" : "Set up", variant: h.tax ? "ghost" : "primary", onClick: () => editHead(h) },
                  ...(h.tax ? [{ id: "gst-head-clear", label: "Remove GST", onClick: () => void run(() => api.clearFeeHead(institutionId, h.feeHeadId), `${h.code}: no GST`) }] : []),
                ], busy),
            },
          ]} />
      )}
      {view === "summary" && (
        <div data-gst-summary>
          <div style={{ display: "flex", gap: 12, alignItems: "flex-end", flexWrap: "wrap", marginBottom: 12 }}>
            <div style={{ minWidth: 160 }}>
              <DatePicker label="From" name="gstFrom" value={period.from} onChange={(e) => setPeriod((x) => ({ ...x, from: e.target.value }))} />
            </div>
            <div style={{ minWidth: 160 }}>
              <DatePicker label="To" name="gstTo" value={period.to} onChange={(e) => setPeriod((x) => ({ ...x, to: e.target.value }))} />
            </div>
            <Button size="sm" variant="ghost" data-action="gst-csv" disabled={busy} icon={<FileSpreadsheet size={14} />} iconPosition="start" onClick={download}>
              CSV (GSTR-1)
            </Button>
          </div>
          {summary.isError && <p style={{ color: "var(--sb-danger, #b42318)", fontSize: 13 }}>{problemText(summary.error)}</p>}
          {summary.data && (
            <>
              <FormGrid columns={3}>
                <KeyValue label="Taxable value" value={<b data-gst-net-taxable>{money(summary.data.net.taxableValue)}</b>} />
                <KeyValue label="CGST + SGST" value={`${money(summary.data.net.cgst)} + ${money(summary.data.net.sgst)}`} />
                <KeyValue label="IGST" value={money(summary.data.net.igst)} />
                <KeyValue label="Exempt (intra-state)" value={money(summary.data.exempt.intraExempt)} />
                <KeyValue label="Nil-rated (intra-state)" value={money(summary.data.exempt.intraNil)} />
                <KeyValue label="Credit notes" value={summary.data.creditNotes.length} />
              </FormGrid>
              <h3 style={{ fontSize: 15, margin: "16px 0 8px" }}>SAC summary</h3>
              <DataTable caption="SAC summary" rows={summary.data.hsn} rowKey={(r) => `${r.sac}:${r.rate}`}
                empty={<EmptyState icon={Receipt} title="Nothing issued in this period">GST documents appear here once charges on GST fee heads are published.</EmptyState>}
                columns={[
                  { key: "s", header: "SAC", render: (r) => r.sac },
                  { key: "r", header: "Rate", render: (r) => `${r.rate}%` },
                  { key: "n", header: "Lines", render: (r) => r.count },
                  { key: "t", header: "Taxable value", render: (r) => money(r.amounts.taxableValue) },
                  { key: "x", header: "Tax", render: (r) => money(r.amounts.totalTax) },
                ]} />
              <h3 style={{ fontSize: 15, margin: "16px 0 8px" }}>Documents issued</h3>
              {summary.data.documents.length === 0 ? (
                <p style={{ color: "var(--sb-mute)", fontSize: 14 }}>None.</p>
              ) : (
                summary.data.documents.map((d) => (
                  <KeyValue key={d.kind} label={kindLabel(d.kind)} value={<span data-gst-doc-range={d.kind}>{`${d.from} to ${d.to} · ${d.total}${d.cancelled ? ` (${d.cancelled} cancelled)` : ""}`}</span>} />
                ))
              )}
              {summary.data.creditNotes.length > 0 && (
                <>
                  <h3 style={{ fontSize: 15, margin: "16px 0 8px" }}>Credit notes</h3>
                  {summary.data.creditNotes.map((c) => (
                    <KeyValue key={c.number} label={`${c.number} · ${c.recipientName}`} value={<>{money(c.noteValue)} <Badge kind="neutral">{c.originalNumber ? `against ${c.originalNumber}` : "credit note"}</Badge></>} />
                  ))}
                </>
              )}
            </>
          )}
        </div>
      )}
      {prompt}
      {dialog}
    </BentoTile>
  );
}
