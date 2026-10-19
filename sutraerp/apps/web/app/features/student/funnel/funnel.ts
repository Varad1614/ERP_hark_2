/**
 * ERP_EXTENSIONS 2A: small display helpers for the admissions funnel (ported from Everything's
 * CRM, MIT). The server computes every figure; these only word and shape them.
 */
import type { FunnelRow, FunnelStage } from "./types";

export const SOURCES = [
  { value: "walk_in", label: "Walk-in" },
  { value: "website", label: "Website" },
  { value: "phone", label: "Phone call" },
  { value: "referral", label: "Referral" },
  { value: "campaign", label: "Campaign" },
  { value: "education_fair", label: "Education fair" },
  { value: "social", label: "Social media" },
  { value: "agent", label: "Agent / consultant" },
  { value: "school_visit", label: "School visit" },
  { value: "other", label: "Other" },
] as const;

export const LOST_REASONS = ["Fee too high", "Chose another college", "Did not qualify", "No response", "Relocated", "Duplicate enquiry", "Other"] as const;

export const sourceLabel = (s: string) => SOURCES.find((x) => x.value === s)?.label ?? s;

/** `0.375` → `"38%"`; `null` → `"—"`. */
export function pct(rate: number | null | undefined): string {
  return rate == null ? "—" : `${Math.round(rate * 100)}%`;
}

/** Rupees with Indian grouping: `"1250000.00"` → `"₹12,50,000"`. */
export function rupees(v: string | number): string {
  const n = typeof v === "number" ? v : Number(v);
  return `₹${(Number.isFinite(n) ? n : 0).toLocaleString("en-IN", { maximumFractionDigits: 0 })}`;
}

/** Bar width (0–100) of each funnel row, relative to the widest stage. */
export function barWidths(rows: FunnelRow[]): number[] {
  const max = Math.max(0, ...rows.map((r) => r.entered));
  return rows.map((r) => (max ? Math.round((r.entered / max) * 100) : 0));
}

/** The open stage after `current`, or `null` at the end of the funnel. */
export function nextStage(stages: FunnelStage[], current: string): FunnelStage | null {
  const open = stages.filter((s) => s.id !== "admitted" && s.id !== "lost");
  const i = open.findIndex((s) => s.id === current);
  return i >= 0 && i < open.length - 1 ? open[i + 1]! : null;
}

/** Kind of the badge for a stage. */
export function stageKind(stage: string, stalled: boolean): "done" | "attention" | "review" | "neutral" {
  if (stage === "admitted") return "done";
  if (stage === "lost") return "neutral";
  return stalled ? "attention" : "review";
}
