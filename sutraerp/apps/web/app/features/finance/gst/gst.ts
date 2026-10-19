/**
 * ERP_EXTENSIONS 5D pure helpers for the GST screens. The server decides every amount and checks
 * every GSTIN; these only label and shape what the user sees and types.
 */
import type { GstFeeHeadTax, GstPeriod, GstProfile, GstProfileInput, GstTaxNotePreset, GstDocKind } from "./types";

/** React Query keys: under `institution` so `useCommand` refreshes them after every write. */
export const gkeys = {
  scoped: (owner: string, what: string) => ["institution", "5d", owner, what] as const,
};

/** The shape of a GSTIN (2-digit state, 13 more); the check character is the server's call. */
export const GSTIN_SHAPE = /^[0-9]{2}[0-9A-Z]{13}$/;

export function normalGstin(v: string | null | undefined): string | null {
  const g = (v ?? "").trim().toUpperCase();
  return g ? g : null;
}

/** "18% · CGST 9% + SGST 9%" (intra-state), "Exempt", "Nil-rated (0%)", "Not set up". */
export function taxLabel(tax: GstFeeHeadTax | null, interState = false): string {
  if (!tax) return "Not set up";
  if (tax.exempt || tax.treatment === "exempt") return "Exempt";
  const rate = Number(tax.gstRate);
  if (!rate) return "Nil-rated (0%)";
  if (interState) return `${tax.gstRate}% IGST`;
  const half = rate / 2;
  const h = Number.isInteger(half) ? String(half) : half.toFixed(1);
  return `${tax.gstRate}% · CGST ${h}% + SGST ${h}%`;
}

/** A stored note as words: a preset's label, own words verbatim, blank as "—". */
export function noteLabel(presets: GstTaxNotePreset[], value: string): string {
  const v = value.trim();
  if (!v) return "—";
  return presets.find((p) => p.id === v)?.label ?? v;
}

const KIND: Record<GstDocKind, string> = { tax_invoice: "Tax invoice", bill_of_supply: "Bill of supply", credit_note: "Credit note" };
export const kindLabel = (k: GstDocKind) => KIND[k] ?? k;

const pad = (n: number) => String(n).padStart(2, "0");
export const isoDay = (d: Date) => `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;

/** The 1st of this month to today: the server's own default period (a GSTR-1 month). */
export function defaultPeriod(today: Date = new Date()): GstPeriod {
  return { from: isoDay(new Date(today.getFullYear(), today.getMonth(), 1)), to: isoDay(today) };
}

/** Form values of the profile dialog → the PUT body (blank optional fields left out). */
export function profileInput(v: Record<string, string>): GstProfileInput {
  const out: GstProfileInput = {
    registrationType: (v.registrationType || "regular") as GstProfileInput["registrationType"],
    gstin: normalGstin(v.gstin),
    legalName: (v.legalName ?? "").trim(),
    tradeName: (v.tradeName ?? "").trim(),
    address: (v.address ?? "").trim(),
    stateCode: (v.stateCode ?? "").trim() || null,
    defaultTaxNote: (v.defaultTaxNote ?? "").trim(),
    enabled: v.enabled !== "false",
  };
  for (const k of ["invoicePrefix", "billPrefix", "creditNotePrefix"] as const) {
    const p = (v[k] ?? "").trim();
    if (p) out[k] = p;
  }
  return out;
}

/** The profile as the dialog's starting values. */
export function profileValues(p: GstProfile): Record<string, string> {
  return {
    registrationType: p.registrationType,
    gstin: p.gstin ?? "",
    legalName: p.legalName,
    tradeName: p.tradeName,
    address: p.address,
    stateCode: p.stateCode,
    defaultTaxNote: p.defaultTaxNote,
    invoicePrefix: p.invoicePrefix,
    billPrefix: p.billPrefix,
    creditNotePrefix: p.creditNotePrefix,
  };
}
