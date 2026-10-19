/**
 * ERP_EXTENSIONS 3E: what every printed fee receipt of the institution says at the top and the
 * bottom (title, address, up to two registrations such as GSTIN / PAN, footer, amount in words).
 * The time on the slip is the institution's own timezone (server-side), so nothing to set here.
 */
import { BentoTile, Button, Checkbox, FormGrid, TextArea, TextField, useToast } from "@sutra/ui";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { problemText } from "../../institution/hooks";
import { defaultPrintingApi, type PrintingApi } from "./api";
import type { Registration, ReceiptPrintSettingsInput } from "./types";

export const MAX_REGISTRATIONS = 2;

/** Trim, drop blank registration rows, cap at two. */
export function cleanSettings(s: ReceiptPrintSettingsInput): ReceiptPrintSettingsInput {
  return {
    title: s.title.trim(),
    address: s.address.trim(),
    registrations: s.registrations.map((r) => ({ label: r.label.trim(), number: r.number.trim() })).filter((r) => r.label || r.number).slice(0, MAX_REGISTRATIONS),
    footer: s.footer.trim(),
    showAmountInWords: s.showAmountInWords,
  };
}

const blankRegs = (regs: Registration[]): Registration[] => [...regs, { label: "", number: "" }, { label: "", number: "" }].slice(0, MAX_REGISTRATIONS);

export function ReceiptPrintSettingsForm({ institutionId, institutionName, api = defaultPrintingApi(), span = 6 }: { institutionId: string; institutionName?: string; api?: PrintingApi; span?: 6 | 12 }) {
  const toast = useToast();
  const client = useQueryClient();
  const key = ["institution", "3e", institutionId, "print-settings"];
  const q = useQuery({ queryKey: key, queryFn: () => api.settings(institutionId) });
  const [draft, setDraft] = useState<ReceiptPrintSettingsInput | null>(null);
  const [busy, setBusy] = useState(false);
  const base: ReceiptPrintSettingsInput | null = q.data
    ? { title: q.data.title, address: q.data.address, registrations: q.data.registrations, footer: q.data.footer, showAmountInWords: q.data.showAmountInWords }
    : null;
  const shown = draft ?? base;
  const set = (p: Partial<ReceiptPrintSettingsInput>) => shown && setDraft({ ...shown, ...p });
  const regs = blankRegs(shown?.registrations ?? []);
  const setReg = (i: number, p: Partial<Registration>) => set({ registrations: regs.map((r, j) => (j === i ? { ...r, ...p } : r)) });

  const save = async () => {
    if (!shown) return;
    setBusy(true);
    try {
      const saved = await api.saveSettings(institutionId, cleanSettings(shown));
      client.setQueryData(key, saved);
      setDraft(null);
      toast("Receipt layout saved");
    } catch (e) {
      toast(problemText(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <BentoTile
      span={span}
      title="Printed receipt layout"
      data-slice="3E"
      data-print-settings
      action={
        <Button size="sm" disabled={busy || !draft} data-print-settings-save onClick={() => void save()}>
          Save
        </Button>
      }
    >
      {q.isError && <p style={{ color: "var(--sb-danger, #b42318)", fontSize: 13 }}>{problemText(q.error)}</p>}
      {shown && (
        <>
          <FormGrid columns={2}>
            <TextField label="Title" hint="Leave blank to print the institution’s name." placeholder={institutionName || "Institution name"} value={shown.title} maxLength={80} onChange={(e) => set({ title: e.target.value })} data-print-title />
            <TextField label="Footer" placeholder="Thank you · Fees once paid are not refundable" value={shown.footer} maxLength={160} onChange={(e) => set({ footer: e.target.value })} data-print-footer />
          </FormGrid>
          <TextArea label="Address" rows={2} value={shown.address} maxLength={240} onChange={(e) => set({ address: e.target.value })} data-print-address />
          <FormGrid columns={2}>
            {regs.map((r, i) => (
              <FormGrid key={i} columns={2}>
                <TextField label={`Registration ${i + 1}`} placeholder={i === 0 ? "GSTIN" : "PAN"} value={r.label} maxLength={20} onChange={(e) => setReg(i, { label: e.target.value })} data-print-reg-label={i} />
                <TextField label="Number" value={r.number} maxLength={30} onChange={(e) => setReg(i, { number: e.target.value })} data-print-reg-number={i} />
              </FormGrid>
            ))}
          </FormGrid>
          <Checkbox label="Print the amount in words" description="e.g. Rupees One Lakh Twenty-Five Thousand Only" checked={shown.showAmountInWords} onChange={(e) => set({ showAmountInWords: e.target.checked })} data-print-words />
        </>
      )}
    </BentoTile>
  );
}
