/** ERP_EXTENSIONS 2B screen helpers: words for conditions, actions, SLA metrics and durations. */
import type { Action, Condition, SlaTarget } from "./types";

export const PRIORITIES = ["low", "medium", "high", "critical"] as const;
export const CATEGORIES = ["facility", "maintenance", "hostel", "transport", "library", "laboratory", "it", "other"] as const;
export const STATUSES = ["open", "reopened", "triaged", "assigned", "in_progress", "on_hold", "resolved", "closed", "cancelled", "merged"] as const;
export const CHANNELS = ["self_service", "admin", "hostel", "transport", "library", "laboratory", "preventive"] as const;

/** Condition fields with their operators (mirrors the server's validation). */
export const FIELDS: { value: string; label: string; ops: string[] }[] = [
  { value: "subject", label: "Title", ops: ["contains", "not_contains"] },
  { value: "priority", label: "Priority", ops: ["is", "is_not", "less_than", "greater_than"] },
  { value: "type", label: "Category", ops: ["is", "is_not"] },
  { value: "status", label: "Status", ops: ["is", "is_not", "less_than", "greater_than", "changed_to"] },
  { value: "channel", label: "Raised from", ops: ["is", "is_not"] },
  { value: "assignee", label: "Assignee", ops: ["is", "is_not", "changed"] },
  { value: "tags", label: "Tags", ops: ["includes", "excludes"] },
  { value: "field:complaint", label: "Complaint", ops: ["is", "is_not"] },
  { value: "field:requester_kind", label: "Requester type", ops: ["is", "is_not", "present", "absent"] },
  { value: "created", label: "Hours since opened", ops: ["less_than", "greater_than"] },
  { value: "updated", label: "Hours since updated", ops: ["less_than", "greater_than"] },
  { value: "actor", label: "Changed by", ops: ["is"] },
];

export const ACTIONS: { value: string; label: string }[] = [
  { value: "priority", label: "Set priority" },
  { value: "type", label: "Set category" },
  { value: "assignee", label: "Assign to" },
  { value: "add_tags", label: "Add tags" },
  { value: "remove_tags", label: "Remove tags" },
  { value: "set_tags", label: "Replace tags" },
];

export const OP_LABEL: Record<string, string> = {
  is: "is",
  is_not: "is not",
  less_than: "below",
  greater_than: "above",
  changed_to: "changed to",
  changed: "changed",
  includes: "has any of",
  excludes: "has none of",
  contains: "contains",
  not_contains: "does not contain",
  present: "is set",
  absent: "is not set",
};

export const words = (s: string) => s.replace(/_/g, " ").replace(/^./, (c) => c.toUpperCase());

/** Choices for a condition's or action's value, or null for free text. */
export function valueChoices(field: string): readonly string[] | null {
  switch (field) {
    case "priority":
      return PRIORITIES;
    case "type":
      return CATEGORIES;
    case "status":
      return STATUSES;
    case "channel":
      return CHANNELS;
    case "field:complaint":
      return ["true", "false"];
    case "field:requester_kind":
      return ["student", "staff"];
    case "actor":
      return ["agent", "customer", "system"];
    default:
      return null;
  }
}

/** The JSON value the server expects for a typed-in condition value. */
export function conditionValue(field: string, raw: string): unknown {
  if (field === "field:complaint") return raw === "true";
  if (field === "created" || field === "updated" || field === "solved") return Number(raw);
  return raw;
}

const show = (v: unknown) => (typeof v === "boolean" ? (v ? "yes" : "no") : words(String(v ?? "")));

export function describeCondition(c: Condition): string {
  const f = FIELDS.find((x) => x.value === c.field)?.label ?? c.field;
  if (c.op === "changed" || c.op === "present" || c.op === "absent") return `${f} ${OP_LABEL[c.op]}`;
  return `${f} ${OP_LABEL[c.op] ?? c.op} ${show(c.value)}`;
}

export function describeAction(a: Action): string {
  const label = ACTIONS.find((x) => x.value === a.field)?.label ?? a.field;
  return `${label} ${show(a.value)}`;
}

/** Compact duration like the server's (45s, 12m, 3h, 2d, 5w); negative keeps its sign. */
export function shortDuration(ms: number): string {
  const sign = ms < 0 ? "-" : "";
  const s = Math.abs(ms) / 1000;
  if (s < 60) return `${sign}${Math.max(1, Math.round(s))}s`;
  const m = s / 60;
  if (m < 60) return `${sign}${Math.round(m)}m`;
  const h = m / 60;
  if (h < 48) return `${sign}${Math.round(h)}h`;
  const d = h / 24;
  if (d < 14) return `${sign}${Math.round(d)}d`;
  return `${sign}${Math.round(d / 7)}w`;
}

export const METRIC_LABEL: Record<string, string> = { first: "First response", next: "Next reply", resolve: "Resolution" };

/** Minutes ↔ hours for the SLA form (hours with up to two decimals). */
export const toHours = (m: number) => (m ? String(Math.round((m / 60) * 100) / 100) : "");
export const toMinutes = (h: string) => {
  const n = Number(h);
  return Number.isFinite(n) && n > 0 ? Math.round(n * 60) : 0;
};

/** Problems with a target row, as the server would report them. */
export function targetProblem(t: SlaTarget): string | null {
  if (!t.resolve) return "needs a resolution target";
  if (t.first && t.first > t.resolve) return "first response must come before resolution";
  return null;
}
