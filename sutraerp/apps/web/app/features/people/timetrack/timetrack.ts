/**
 * ERP_EXTENSIONS 5C display helpers. The duration parser mirrors the server's port of
 * Everything's `parseDuration` for an instant preview; the server stays authoritative.
 */
import type { MemberRole, ProjectKind, TimesheetStatus } from "./types";

export const FUNDING_AGENCIES = ["DST", "SERB (ANRF)", "UGC", "ICSSR", "AICTE", "DBT", "CSIR", "ICMR", "MeitY", "Industry"];

export const KIND_LABEL: Record<ProjectKind, string> = {
  sponsored_research: "Sponsored research",
  consultancy: "Consultancy",
  internal_project: "Internal project",
  committee: "Committee",
  other: "Other",
};

export const ROLE_LABEL: Record<MemberRole, string> = {
  pi: "PI",
  co_pi: "Co-PI",
  member: "Member",
  jrf: "JRF",
  srf: "SRF",
  research_associate: "Research associate",
  project_assistant: "Project assistant",
  technical_staff: "Technical staff",
};

export const STATUS_LABEL: Record<TimesheetStatus, string> = {
  draft: "Not submitted",
  submitted: "Awaiting approval",
  approved: "Approved",
  rejected: "Returned",
};

export function statusKind(s: TimesheetStatus | undefined | null): "done" | "attention" | "info" | "review" {
  if (s === "approved") return "done";
  if (s === "rejected") return "attention";
  if (s === "submitted") return "review";
  return "info";
}

/** `h:mm` of seconds (`fmtClock`). */
export function clock(seconds: number): string {
  const t = Math.max(0, Math.round(seconds));
  return `${Math.floor(t / 3600)}:${String(Math.floor((t % 3600) / 60)).padStart(2, "0")}`;
}

/** `1h 30m` / `45m` (`fmtDuration`). */
export function human(seconds: number): string {
  const m = Math.round(Math.max(0, seconds) / 60);
  const h = Math.floor(m / 60);
  const r = m % 60;
  if (h && r) return `${h}h ${r}m`;
  if (h) return `${h}h`;
  return `${r}m`;
}

/** Typed duration to seconds, or null (`parseDuration`: `1:30`, `1h30`, `1.5`, `90m`, `:45`). */
export function parseDuration(input: string): number | null {
  let s = input.trim().toLowerCase().replace(",", ".");
  if (!s) return null;
  if (s.startsWith(".")) s = `0${s}`;
  let m = s.match(/^(\d+):([0-5]?\d)(?::([0-5]?\d))?$/);
  if (m) return +m[1]! * 3600 + +m[2]! * 60 + +(m[3] ?? 0);
  m = s.match(/^:([0-5]?\d)$/);
  if (m) return +m[1]! * 60;
  if (/^\d+(?:\.\d+)?$/.test(s)) {
    const n = parseFloat(s);
    return Number.isInteger(n) && n > 24 ? n * 60 : Math.round(n * 3600);
  }
  m = s.match(/^(\d+(?:\.\d+)?)\s*h(?:ours?|rs?)?(?:\s*(\d+(?:\.\d+)?)\s*m(?:in(?:ute)?s?)?|\s+(\d+(?:\.\d+)?)|(\d+(?:\.\d+)?))?$/);
  if (m) return Math.round(parseFloat(m[1]!) * 3600 + parseFloat(m[2] ?? m[3] ?? m[4] ?? "0") * 60);
  m = s.match(/^(\d+(?:\.\d+)?)\s*m(?:in(?:ute)?s?)?$/);
  if (m) return Math.round(parseFloat(m[1]!) * 60);
  return null;
}

/** ISO date plus `n` days (UTC calendar). */
export function addDaysIso(iso: string, n: number): string {
  const d = new Date(`${iso}T00:00:00Z`);
  d.setUTCDate(d.getUTCDate() + n);
  return d.toISOString().slice(0, 10);
}

/** First day of the week of `iso` (`weekStartOf`): 1 = Monday, 0 = Sunday. */
export function weekStartOf(iso: string, weekStart = 1): string {
  const d = new Date(`${iso}T00:00:00Z`);
  const diff = (d.getUTCDay() - weekStart + 7) % 7;
  return addDaysIso(iso, -diff);
}

/** `YYYY-MM` shifted by `delta` months (`shiftMonth`). */
export function shiftMonth(month: string, delta: number): string {
  const [y, m] = month.split("-").map(Number);
  const t = y! * 12 + (m! - 1) + delta;
  return `${Math.floor(t / 12)}-${String((t % 12) + 1).padStart(2, "0")}`;
}

/** "Sep 2026" for `2026-09`. */
export function monthLabel(month: string): string {
  const [y, m] = month.split("-").map(Number);
  return new Date(Date.UTC(y!, m! - 1, 1)).toLocaleDateString("en-IN", { month: "short", year: "numeric", timeZone: "UTC" });
}

/** Budget bar: % used and its tone. */
export function budgetBar(ratio: number): { pct: number; kind: "done" | "attention" | "review" } {
  const pct = Math.round(ratio * 100);
  return { pct, kind: ratio >= 1 ? "attention" : ratio >= 0.8 ? "review" : "done" };
}
