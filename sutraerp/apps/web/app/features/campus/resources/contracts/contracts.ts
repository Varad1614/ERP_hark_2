/** ERP_EXTENSIONS 3C screen helpers: labels and the words a contract's clock is shown in. */
import { formatDate } from "@sutra/ui";
import type { Contract, ContractKind, DecisionKind, Obligation, Phase } from "./types";

export const KINDS: { value: ContractKind; label: string }[] = [
  { value: "vendor", label: "Vendor / supplier" },
  { value: "service", label: "Service agreement" },
  { value: "amc", label: "AMC" },
  { value: "lease", label: "Lease" },
  { value: "license", label: "Licence / SaaS" },
  { value: "nda", label: "NDA" },
  { value: "mou_academic", label: "MoU: academic" },
  { value: "mou_industry", label: "MoU: industry" },
  { value: "mou_research", label: "MoU: research" },
  { value: "other", label: "Other" },
];

export const ACTIVITY_KINDS: { value: string; label: string }[] = [
  { value: "internship", label: "Internship" },
  { value: "on_job_training", label: "On-the-job training" },
  { value: "project_work", label: "Project work" },
  { value: "student_exchange", label: "Student exchange" },
  { value: "faculty_exchange", label: "Faculty exchange" },
  { value: "collaborative_research", label: "Collaborative research" },
  { value: "workshop", label: "Workshop / seminar" },
  { value: "training", label: "Training" },
  { value: "consultancy", label: "Consultancy" },
  { value: "placement", label: "Placement" },
  { value: "publication", label: "Joint publication" },
  { value: "other", label: "Other" },
];

export const DECISIONS: { value: DecisionKind; label: string }[] = [
  { value: "renewing", label: "Renewing" },
  { value: "notice-given", label: "Notice given" },
  { value: "not-renewing", label: "Ending at term" },
  { value: "snoozed", label: "Snooze" },
];

export const kindLabel = (k: string) => KINDS.find((x) => x.value === k)?.label ?? k;
export const activityLabel = (k: string) => ACTIVITY_KINDS.find((x) => x.value === k)?.label ?? k;
export const decisionLabel = (k: string) => DECISIONS.find((x) => x.value === k)?.label ?? k;
export const isMou = (k: string) => k.startsWith("mou_");

/** "in 3 days" / "today" / "5 days ago". */
export function relativeDays(days: number): string {
  if (days === 0) return "today";
  if (days === 1) return "tomorrow";
  if (days === -1) return "yesterday";
  return days > 1 ? `in ${days} days` : `${-days} days ago`;
}

/** Badge kind for a phase. */
export function phaseKind(c: Pick<Contract, "attention" | "renewal">): "attention" | "info" | "done" {
  if (c.attention) return "attention";
  return c.renewal.phase === "soon" || c.renewal.phase === "notice" ? "info" : "done";
}

/** The one line a contract's clock reads as. */
export function clockText(c: Pick<Contract, "autoRenew" | "renewal" | "status">): string {
  const r = c.renewal;
  const word: Record<Phase, string> = { none: c.status === "draft" ? "Draft" : "Ended", evergreen: "No end date", expired: "", notice: "", soon: "", ok: "" };
  if (r.phase === "none" || r.phase === "evergreen") return word[r.phase];
  const verb = c.autoRenew && r.decision?.kind !== "notice-given" && r.decision?.kind !== "not-renewing" ? "Renews" : "Ends";
  if (r.phase === "expired") return `Ended ${relativeDays(r.daysLeft ?? 0)}`;
  let s = `${verb} ${relativeDays(r.daysLeft ?? 0)}`;
  if (r.noticeDate && r.noticeDaysLeft !== null && r.noticeDaysLeft >= 0 && !r.handled) s += ` · notice by ${formatDate(r.noticeDate)}`;
  if (r.handled && r.decision) s += ` · ${decisionLabel(r.decision.kind).toLowerCase()}`;
  return s;
}

/** Who owes it, in words. */
export const ownerLabel = (o: Obligation["owner"]) => (o === "them" ? "Counterparty" : o === "mutual" ? "Both" : "Us");

export function dueText(o: Pick<Obligation, "done" | "daysLeft" | "repeat" | "completed">): string {
  if (o.done) return o.repeat ? `Finished (${o.completed} done)` : "Done";
  if (o.daysLeft === null) return "No date";
  const when = o.daysLeft < 0 ? `Overdue ${-o.daysLeft} day${o.daysLeft === -1 ? "" : "s"}` : `Due ${relativeDays(o.daysLeft)}`;
  return o.repeat ? `${when} · repeats ${o.repeat.every}` : when;
}

/** AQAR year (1 July – 30 June) of an ISO day, e.g. "2026-27". */
export function aqarYear(iso: string): string {
  const [y, m] = iso.split("-").map(Number) as [number, number];
  const start = m >= 7 ? y : y - 1;
  return `${start}-${String((start + 1) % 100).padStart(2, "0")}`;
}

/** The last few AQAR years, newest first. */
export function aqarYears(iso: string, n = 5): string[] {
  const first = Number(aqarYear(iso).slice(0, 4));
  return Array.from({ length: n }, (_, i) => `${first - i}-${String((first - i + 1) % 100).padStart(2, "0")}`);
}

/** Per-currency totals as one line; never summed across currencies. */
export function totalsText(byCurrency: Record<string, number>, inr: (v: number) => string): string {
  const parts = Object.entries(byCurrency).filter(([, v]) => v > 0).sort((a, b) => b[1] - a[1]);
  if (!parts.length) return "—";
  return parts.map(([c, v]) => (c === "INR" ? inr(v) : `${c} ${v.toLocaleString("en-IN", { maximumFractionDigits: 2 })}`)).join(" · ");
}
