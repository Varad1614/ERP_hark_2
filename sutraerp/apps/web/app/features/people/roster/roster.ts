/**
 * ERP_EXTENSIONS 3A screen helpers. The rules run on the server (golden-tested against
 * Everything's shifts scheduling.js / rules.js / fill.js); these only word them.
 */
import type { DutyShift } from "./types";

/** "7.5h", "8h". */
export function hours(minutes: number): string {
  const r = Math.round((minutes / 60) * 10) / 10;
  return `${r % 1 === 0 ? r.toFixed(0) : r}h`;
}

/** "22:00–06:00 +1". */
export function timeRange(s: Pick<DutyShift, "start" | "end" | "overnight">): string {
  return `${s.start}–${s.end}${s.overnight ? " +1" : ""}`;
}

/** Monday of the week containing an ISO date. */
export function mondayOf(iso: string): string {
  const d = new Date(`${iso}T00:00:00Z`);
  const dow = d.getUTCDay();
  d.setUTCDate(d.getUTCDate() + (dow === 0 ? -6 : 1 - dow));
  return d.toISOString().slice(0, 10);
}

export function addDaysIso(iso: string, n: number): string {
  const d = new Date(`${iso}T00:00:00Z`);
  d.setUTCDate(d.getUTCDate() + n);
  return d.toISOString().slice(0, 10);
}

/** The short flags a shift row shows. */
export function flags(s: DutyShift): string[] {
  const out: string[] = [];
  if (s.conflicts.length) out.push("Overlap");
  if (s.staffId && s.availability === "unavailable") out.push("Unavailable");
  if (s.staffId && s.availability === "partial") out.push("Partly available");
  for (const w of s.ruleWarnings) out.push(w.short);
  return out;
}

export const WEEKDAYS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
