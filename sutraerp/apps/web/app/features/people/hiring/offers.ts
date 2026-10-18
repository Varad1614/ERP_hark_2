/**
 * ERP_EXTENSIONS 5A: pure offer helpers for the Offers & onboarding tile — live CTC totals of a
 * breakup while the form is typed (the server recomputes and stores the real ones), the
 * statutory employer PF suggestion, status labels and the actions each status allows. Money is
 * done in whole paise so decimal strings never pick up float error.
 */
import { formatDate } from "@sutra/ui";
import { formatInr } from "../../finance";
import type { OfferCtc, OfferCtcInput, OfferStatus, OfferView } from "./offersTypes";

/** The EPF wage ceiling (₹ per month) on which the 12 % employer share is mandatory. */
export const EPF_WAGE_CEILING = 15_000;

type Amount = string | number | null | undefined;

/** `"1,800.50"` / `1800.5` / `""` → paise (integer); unparseable or blank → 0. */
export function toPaise(v: Amount): number {
  if (v == null) return 0;
  const s = String(v).trim().replace(/,/g, "");
  const m = /^([+-])?(\d*)(?:\.(\d*))?$/.exec(s);
  if (!m || (m[2] === "" && (m[3] ?? "") === "")) return 0;
  const frac = ((m[3] ?? "") + "00").slice(0, 3);
  let p = Number(m[2] || "0") * 100 + Number(frac.slice(0, 2));
  if (Number(frac[2]) >= 5) p += 1;
  return m[1] === "-" ? -p : p;
}

/** Paise → decimal string with two places (`180050` → `"1800.50"`). */
export function fromPaise(p: number): string {
  const neg = p < 0;
  const a = Math.abs(Math.round(p));
  return `${neg ? "-" : ""}${Math.floor(a / 100)}.${String(a % 100).padStart(2, "0")}`;
}

type Breakup = OfferCtc | OfferCtcInput;

/** Monthly gross: basic + HRA + DA + allowances (everything but the employer PF), ₹ string. */
export function grossMonthly(c: Breakup): string {
  return fromPaise(toPaise(c.basic) + toPaise(c.hra) + toPaise(c.da) + toPaise(c.allowances));
}

/**
 * Employer EPF = 12 % of min(basic + DA, ₹15,000), rounded to whole rupees half-to-even (as
 * the server's `suggested_employer_pf`). ₹ string.
 */
export function suggestedEmployerPf(basic: Amount, da: Amount): string {
  const wages = Math.max(0, Math.min(toPaise(basic) + toPaise(da), EPF_WAGE_CEILING * 100));
  const num = wages * 12; // rupees × 10 000
  let q = Math.floor(num / 10_000);
  const r = num % 10_000;
  if (r > 5_000 || (r === 5_000 && q % 2 === 1)) q += 1;
  return fromPaise(q * 100);
}

/** The PF the server will store: the typed one, or the suggestion when left blank. */
export function employerPfOf(c: Breakup): string {
  const typed = c.employerPf;
  return typed == null || String(typed).trim() === "" ? suggestedEmployerPf(c.basic, c.da) : fromPaise(toPaise(typed));
}

/** Monthly CTC: gross + employer PF (blank PF → the statutory suggestion). ₹ string. */
export function monthlyCtc(c: Breakup): string {
  return fromPaise(toPaise(grossMonthly(c)) + toPaise(employerPfOf(c)));
}

/** Annual CTC: 12 × monthly. ₹ string. */
export function annualCtc(c: Breakup): string {
  return fromPaise(toPaise(monthlyCtc(c)) * 12);
}

/** Display totals for the form: `{ gross, pf, monthly, annual }` as `₹1,23,456.00`. */
export function ctcTotals(c: Breakup): { gross: string; pf: string; monthly: string; annual: string } {
  return { gross: formatInr(grossMonthly(c)), pf: formatInr(employerPfOf(c)), monthly: formatInr(monthlyCtc(c)), annual: formatInr(annualCtc(c)) };
}

// ------------------------------------------------------------------ status

export type OfferAction = "submit" | "approve" | "reject" | "send" | "accept" | "decline" | "withdraw";

export const OFFER_STATUS_LABELS: Record<OfferStatus, string> = {
  draft: "Draft",
  pending_approval: "Awaiting approval",
  approved: "Approved",
  sent: "Sent to candidate",
  accepted: "Accepted",
  declined: "Declined",
  expired: "Expired",
  withdrawn: "Withdrawn",
};

const ACTIONS: Record<OfferStatus, readonly OfferAction[]> = {
  draft: ["submit", "withdraw"],
  pending_approval: ["approve", "reject"],
  approved: ["send", "withdraw"],
  sent: ["accept", "decline", "withdraw"],
  accepted: [],
  declined: [],
  expired: [],
  withdrawn: [],
};

export const ACTION_LABELS: Record<OfferAction, string> = {
  submit: "Submit for approval",
  approve: "Approve",
  reject: "Return to draft",
  send: "Send letter",
  accept: "Record acceptance",
  decline: "Record decline",
  withdraw: "Withdraw",
};

/** Actions that need a reason in the request body. */
export const REASON_ACTIONS: readonly OfferAction[] = ["reject", "decline", "withdraw"];

/** Human label; an unknown status is shown as given. */
export const offerStatusLabel = (s: OfferStatus | string): string => OFFER_STATUS_LABELS[s as OfferStatus] ?? s;

/** What can be done next; terminal and unknown statuses allow nothing. */
export const allowedActions = (s: OfferStatus | string): readonly OfferAction[] => ACTIONS[s as OfferStatus] ?? [];

export const isTerminal = (s: OfferStatus | string): boolean => allowedActions(s).length === 0;

/** Only a draft's terms can be edited. */
export const canEdit = (s: OfferStatus | string): boolean => s === "draft";

/** Badge kind for the status. */
export function offerStatusKind(s: OfferStatus | string): "done" | "attention" | "info" | "neutral" {
  if (s === "accepted") return "done";
  if (s === "declined" || s === "expired") return "attention";
  if (s === "withdrawn") return "neutral";
  return "info";
}

// ------------------------------------------------------------------ formatting

/** `"OFR-000012 · Assistant Professor · ₹6,00,000.00 a year"`. */
export function offerSummary(o: Pick<OfferView, "number" | "designation" | "ctcAnnual">): string {
  return `${o.number} · ${o.designation} · ${formatInr(o.ctcAnnual)} a year`;
}

/** `"Joins 1 Jun 2026 · expires 20 May 2026"`, dropping the expiry when there is none. */
export function offerDates(o: Pick<OfferView, "joiningDate" | "expiresOn">): string {
  const join = `Joins ${formatDate(o.joiningDate)}`;
  return o.expiresOn ? `${join} · expires ${formatDate(o.expiresOn)}` : join;
}

export { formatDate, formatInr };
