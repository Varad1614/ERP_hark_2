/**
 * ERP_EXTENSIONS 2D screen helpers. The rules run on the server (golden-tested against
 * Everything's fleet reminders.js / fuel.js / costs.js); these only word them.
 */
import type { BoardRow, DueStatus } from "./types";

export const STATUS_LABEL: Record<DueStatus, string> = { overdue: "Overdue", due_soon: "Due soon", upcoming: "Upcoming", snoozed: "Snoozed" };

export const RENEWAL_TYPES = [
  { value: "registration", label: "Registration (RC)" },
  { value: "insurance", label: "Insurance" },
  { value: "fitness", label: "Fitness certificate" },
  { value: "pollution", label: "PUC (pollution)" },
  { value: "permit", label: "Permit" },
  { value: "road_tax", label: "Road tax" },
  { value: "other", label: "Other" },
];

export const UNITS = ["days", "weeks", "months", "years"];

/** "in 12 days", "3 days late", "today". */
export function dueIn(days: number | null): string {
  if (days === null) return "";
  if (days === 0) return "today";
  if (days > 0) return `in ${days} day${days === 1 ? "" : "s"}`;
  return `${-days} day${days === -1 ? "" : "s"} late`;
}

/** What is left on the meter, or how far past it is. */
export function meterIn(left: number | null, unit = "km"): string {
  if (left === null) return "";
  const n = Math.round(Math.abs(left)).toLocaleString("en-IN");
  return left > 0 ? `${n} ${unit} to go` : `${n} ${unit} past`;
}

/** The line a board row shows under its title. */
export function dueLine(r: Pick<BoardRow, "nextDate" | "daysLeft" | "meterLeft">, unit = "km"): string {
  return [r.nextDate ? `${r.nextDate} (${dueIn(r.daysLeft)})` : "", meterIn(r.meterLeft, unit)].filter(Boolean).join(" · ");
}

/** A whole positive number, or null. */
export function wholeOrNull(s: string): number | null {
  const n = Number(s.trim());
  return s.trim() !== "" && Number.isInteger(n) && n > 0 ? n : null;
}
