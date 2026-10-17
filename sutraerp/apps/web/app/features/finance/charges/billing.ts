/**
 * ERP_EXTENSIONS 1E: billing-period labels for the billing-runs screen and the mock API (a mirror
 * of the server's `charges::billing::period_for`; the server decides every real period).
 */
import type { BillingFrequency } from "./types";

export const FREQUENCIES: { value: BillingFrequency; label: string }[] = [
  { value: "weekly", label: "Weekly" },
  { value: "monthly", label: "Monthly" },
  { value: "quarterly", label: "Quarterly" },
  { value: "half_yearly", label: "Half-yearly" },
  { value: "annual", label: "Annual (financial year)" },
];

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const LONG = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

const parse = (iso: string) => {
  const [y, m, d] = iso.split("-").map(Number);
  return new Date(Date.UTC(y!, m! - 1, d!));
};
const iso = (d: Date) => d.toISOString().slice(0, 10);
const addDays = (d: Date, n: number) => new Date(d.getTime() + n * 86400000);
const addMonths = (d: Date, n: number) => new Date(Date.UTC(d.getUTCFullYear(), d.getUTCMonth() + n, 1));

/** The billing period containing `day` (mirror of the server's `period_for`, for labels only). */
export function periodFor(frequency: BillingFrequency, day: string, weekStart = 1, fyStartMonth = 4): { key: string; start: string; end: string; label: string } {
  const d = parse(day);
  if (frequency === "weekly") {
    const back = (d.getUTCDay() - weekStart + 7) % 7;
    const s = addDays(d, -back);
    return { key: `weekly:${iso(s)}`, start: iso(s), end: iso(addDays(s, 6)), label: `Week of ${s.getUTCDate()} ${MONTHS[s.getUTCMonth()]} ${s.getUTCFullYear()}` };
  }
  if (frequency === "monthly") {
    const s = addMonths(d, 0);
    return { key: `monthly:${iso(s).slice(0, 7)}`, start: iso(s), end: iso(addDays(addMonths(s, 1), -1)), label: `${LONG[s.getUTCMonth()]} ${s.getUTCFullYear()}` };
  }
  const span = frequency === "quarterly" ? 3 : frequency === "half_yearly" ? 6 : 12;
  const month = d.getUTCMonth() + 1;
  const since = (month + 12 - fyStartMonth) % 12;
  const fyYear = month >= fyStartMonth ? d.getUTCFullYear() : d.getUTCFullYear() - 1;
  const index = Math.floor(since / span);
  const s = new Date(Date.UTC(fyYear, fyStartMonth - 1 + index * span, 1));
  const e = addDays(addMonths(s, span), -1);
  const fy = `${fyYear}-${String((fyYear + 1) % 100).padStart(2, "0")}`;
  const range = `${MONTHS[s.getUTCMonth()]}–${MONTHS[e.getUTCMonth()]}`;
  const label = frequency === "quarterly" ? `Q${index + 1} ${fy} (${range})` : frequency === "half_yearly" ? `H${index + 1} ${fy} (${range})` : `FY ${fy}`;
  return { key: `${frequency}:${iso(s).slice(0, 7)}`, start: iso(s), end: iso(e), label };
}
