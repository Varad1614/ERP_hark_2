/**
 * ERP_EXTENSIONS 5D: the GST part of a charge's side panel — the tax invoice / bill of supply the
 * charge was issued as (number, place of supply, tax breakup, amount in words, zero-tax note) and
 * the payer's GST details (state for the place of supply, GSTIN for a registered payer).
 */
import { Badge, Button, KeyValue } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useCommand } from "../../institution/hooks";
import { money, useFinanceAsk } from "../ui";
import { defaultGstApi, type GstApi } from "./api";
import { gkeys, normalGstin } from "./gst";

export function ChargeGstSection({ chargeId, studentAccountId, institutionId, api = defaultGstApi() }: { chargeId: string; studentAccountId: string; institutionId: string; api?: GstApi }) {
  const { submit } = useCommand();
  const { ask, prompt } = useFinanceAsk();
  const doc = useQuery({ queryKey: gkeys.scoped(chargeId, "gst-charge-document"), queryFn: () => api.chargeDocument(chargeId) });
  const party = useQuery({ queryKey: gkeys.scoped(studentAccountId, "gst-party"), queryFn: () => api.party(studentAccountId) });
  const profile = useQuery({ queryKey: gkeys.scoped(institutionId, "gst-profile"), queryFn: () => api.profile(institutionId) });
  const d = doc.data;
  const pty = party.data;
  const states = profile.data?.states ?? [];

  const editParty = () =>
    ask({
      title: "Payer GST details",
      eyebrow: "Fees & charges",
      name: "gst-party",
      submitLabel: "Save",
      intro: <p style={{ color: "var(--sb-mute)", fontSize: 14, marginBottom: 12 }}>Used for charges raised from now on: the state decides CGST + SGST or IGST, a GSTIN makes it a B2B invoice.</p>,
      fields: [
        { name: "stateCode", label: "State (place of supply)", type: "select", options: [{ value: "", label: "Institution’s state" }, ...states.map((s) => ({ value: s.code, label: `${s.code} · ${s.name}` }))], initial: pty?.stateCode ?? "" },
        { name: "gstin", label: "GSTIN (registered payer)", initial: pty?.gstin ?? "" },
        { name: "legalName", label: "Legal name on invoice", wide: true, initial: pty?.legalName ?? "" },
      ],
      onSubmit: (v) => submit(() => api.saveParty(studentAccountId, { stateCode: v.stateCode || null, gstin: normalGstin(v.gstin), legalName: (v.legalName ?? "").trim() }), "Payer GST details saved"),
    });

  return (
    <section data-charge-gst={d ? d.kind : "none"}>
      <h3 style={{ fontSize: 15, marginBottom: 8 }}>GST</h3>
      {d ? (
        <>
          <KeyValue label={d.title} value={<span data-gst-doc-number>{d.number ?? "Numbered when published"}</span>} />
          {d.dateDisplay && <KeyValue label="Date" value={d.dateDisplay} />}
          <KeyValue label="Place of supply" value={<>{d.placeOfSupply} {d.interState ? <Badge kind="info">Inter-state</Badge> : null}</>} />
          <KeyValue label="Taxable value" value={money(d.taxableValue)} />
          {d.taxes.map((t) => (
            <KeyValue key={t.label} label={t.label} value={<span data-gst-tax={t.component}>{money(t.amount)}</span>} />
          ))}
          <KeyValue label="Total" value={<b>{money(d.total)}</b>} />
          <p style={{ color: "var(--sb-mute)", fontSize: 13, marginTop: 6 }} data-gst-words>{d.amountInWords}</p>
          {d.taxNote && <p style={{ color: "var(--sb-mute)", fontSize: 13 }}>{d.taxNote}</p>}
        </>
      ) : (
        <p style={{ color: "var(--sb-mute)", fontSize: 14 }}>{doc.isLoading ? "Loading…" : "No GST document: the charge's fee heads have no GST set up, or GST is off."}</p>
      )}
      <KeyValue label="Payer" value={pty?.configured ? `${pty.stateName ?? pty.stateCode ?? "—"}${pty.gstin ? ` · ${pty.gstin}` : ""}` : "Institution’s state (default)"} />
      <div style={{ marginTop: 8 }}>
        <Button size="sm" variant="ghost" data-action="gst-party" onClick={editParty}>
          Payer GST details
        </Button>
      </div>
      {prompt}
    </section>
  );
}
