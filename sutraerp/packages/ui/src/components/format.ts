/**
 * Display formatters of the re-skin (docs/reskin2-contracts.md §4). Every screen formats money,
 * counts, days, dates and wire codes through these — never `toFixed`, raw decimals or raw enums.
 */
import type { BadgeKind } from "./Badge";
import { TONES, type Tone } from "./tones";

const MINUS = "\u2212";

/** Splits a decimal (string from the API, or number) into sign, integer digits and fraction digits without float maths. */
function splitDecimal(value: number | string): { negative: boolean; int: string; frac: string } | null {
  let s = typeof value === "number" ? (Number.isFinite(value) ? value.toFixed(6) : "") : value.trim();
  if (!s || !/^[-+]?\d*(\.\d*)?$/.test(s) || s === "-" || s === "+" || s === ".") return null;
  const negative = s.startsWith("-");
  s = s.replace(/^[-+]/, "");
  const [intRaw = "0", fracRaw = ""] = s.split(".");
  const int = (intRaw || "0").replace(/^0+(?=\d)/, "");
  return { negative, int, frac: fracRaw };
}

/** Indian digit grouping of an integer digit string: `140000` → `1,40,000`. */
export function groupIndian(int: string): string {
  const last3 = int.slice(-3);
  const rest = int.slice(0, -3).replace(/\B(?=(\d{2})+(?!\d))/g, ",");
  return rest ? `${rest},${last3}` : last3;
}

export interface InrOptions {
  /** `auto` (default) drops a needless `.00` but keeps `.50`; `always` keeps two digits; `never` rounds to rupees. */
  paise?: "auto" | "always" | "never";
  /** Lakh / crore short form for KPI tiles and chart labels: `₹3.4 Cr`, `₹92 L`, `₹46,250` below one lakh. */
  compact?: boolean;
  /** Text for a missing value (default an em dash). */
  empty?: string;
}

/** Rupees with Indian grouping: `140000` → `₹1,40,000`, `"46250.50"` → `₹46,250.50`, `-2500` → `−₹2,500`. */
export function formatINR(value: number | string | null | undefined, opts: InrOptions = {}): string {
  const { paise = "auto", compact = false, empty = "—" } = opts;
  if (value == null || value === "") return empty;
  const parts = splitDecimal(value);
  if (!parts) return empty;
  const sign = parts.negative && /[1-9]/.test(parts.int + parts.frac) ? MINUS : "";
  if (compact) {
    const n = Math.abs(Number(`${parts.int}.${parts.frac || "0"}`));
    if (n >= 1e7) return `${sign}₹${trimNumber(n / 1e7, 1)} Cr`;
    if (n >= 1e5) return `${sign}₹${trimNumber(n / 1e5, 1)} L`;
    return `${sign}₹${groupIndian(String(Math.round(n)))}`;
  }
  if (paise === "never") {
    const rounded = Math.round(Number(`${parts.int}.${parts.frac || "0"}`));
    return `${sign}₹${groupIndian(String(rounded))}`;
  }
  const two = (parts.frac + "00").slice(0, 2);
  const frac = paise === "always" ? two : two === "00" ? "" : two;
  return `${sign}₹${groupIndian(parts.int)}${frac ? `.${frac}` : ""}`;
}

function trimNumber(n: number, digits: number): string {
  const fixed = n.toFixed(digits);
  return fixed.includes(".") ? fixed.replace(/\.?0+$/, "") : fixed;
}

/** Plain count / quantity with Indian grouping and no needless decimals: `"12.00"` → `12`, `4812` → `4,812`, `2.5` → `2.5`. */
export function formatNumber(value: number | string | null | undefined, maxFraction = 2, empty = "—"): string {
  if (value == null || value === "") return empty;
  let parts = splitDecimal(value);
  if (!parts) return typeof value === "string" ? value : empty;
  if (parts.frac.length > maxFraction) {
    // Display rounding only (half away from zero); the API value itself is never re-used.
    const abs = Math.abs(Number(`${parts.int}.${parts.frac}`));
    const fixed = (Math.round(abs * 10 ** maxFraction) / 10 ** maxFraction).toFixed(maxFraction);
    const [i = "0", f = ""] = fixed.split(".");
    parts = { negative: parts.negative, int: i, frac: f };
  }
  const frac = parts.frac.replace(/0+$/, "");
  const sign = parts.negative && /[1-9]/.test(parts.int + frac) ? MINUS : "";
  return `${sign}${groupIndian(parts.int)}${frac ? `.${frac}` : ""}`;
}

/** Day counts: `"12.00"` → `12 days`, `1` → `1 day`, `0.5` → `0.5 day`, `1.5` → `1.5 days`. */
export function formatDays(value: number | string | null | undefined, empty = "—"): string {
  if (value == null || value === "") return empty;
  const n = formatNumber(value, 1, "");
  if (!n) return empty;
  const abs = Math.abs(Number(String(value)));
  return `${n} ${abs > 0 && abs <= 1 ? "day" : "days"}`;
}

/** `n` with a singular / plural noun: `plural(1, "student")` → `1 student`, `plural(64, "student")` → `64 students`. */
export function plural(n: number, singular: string, pluralForm = `${singular}s`): string {
  return `${formatNumber(n, 0)} ${n === 1 ? singular : pluralForm}`;
}

/** Percent: `0.82` with `{ ratio: true }` or `82` → `82%`; `"72.50"` → `72.5%`. */
export function formatPercent(value: number | string | null | undefined, opts: { ratio?: boolean; digits?: number; empty?: string } = {}): string {
  const { ratio = false, digits = 1, empty = "—" } = opts;
  if (value == null || value === "") return empty;
  const n = Number(value);
  if (!Number.isFinite(n)) return empty;
  return `${trimNumber(ratio ? n * 100 : n, digits)}%`;
}

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"] as const;
const WEEKDAYS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"] as const;
const TZ = "Asia/Kolkata";

/** Calendar parts of an ISO date (`2026-07-15`) or timestamp (shown in IST). */
function dateParts(value: string | Date): { y: number; m: number; d: number; wd: number; hh: number; mm: number; hasTime: boolean } | null {
  if (typeof value === "string") {
    const plain = /^(\d{4})-(\d{2})-(\d{2})$/.exec(value.trim());
    if (plain) {
      const y = Number(plain[1]);
      const m = Number(plain[2]);
      const d = Number(plain[3]);
      const wd = new Date(Date.UTC(y, m - 1, d)).getUTCDay();
      return { y, m, d, wd, hh: 0, mm: 0, hasTime: false };
    }
  }
  const date = typeof value === "string" ? new Date(value) : value;
  if (Number.isNaN(date.getTime())) return null;
  const f = new Intl.DateTimeFormat("en-GB", { timeZone: TZ, year: "numeric", month: "numeric", day: "numeric", hour: "2-digit", minute: "2-digit", hourCycle: "h23", weekday: "short" });
  const get = (t: string) => f.formatToParts(date).find((p) => p.type === t)?.value ?? "";
  const wd = WEEKDAYS.indexOf(get("weekday") as (typeof WEEKDAYS)[number]);
  return { y: Number(get("year")), m: Number(get("month")), d: Number(get("day")), wd: wd < 0 ? 0 : wd, hh: Number(get("hour")), mm: Number(get("minute")), hasTime: true };
}

const pad = (n: number) => String(n).padStart(2, "0");

/** `dd MMM yyyy`: `2026-07-15` → `15 Jul 2026`. */
export function formatDate(value: string | Date | null | undefined, empty = "—"): string {
  if (value == null || value === "") return empty;
  const p = dateParts(value);
  return p ? `${p.d} ${MONTHS[p.m - 1] ?? ""} ${p.y}` : empty;
}

/** `dd MMM` (same year context): `15 Jul`. */
export function formatDateShort(value: string | Date | null | undefined, empty = "—"): string {
  if (value == null || value === "") return empty;
  const p = dateParts(value);
  return p ? `${p.d} ${MONTHS[p.m - 1] ?? ""}` : empty;
}

/** `Tue 16 Sep` (weekday chip lists, timetables). */
export function formatWeekdayDate(value: string | Date | null | undefined, empty = "—"): string {
  if (value == null || value === "") return empty;
  const p = dateParts(value);
  return p ? `${WEEKDAYS[p.wd] ?? ""} ${p.d} ${MONTHS[p.m - 1] ?? ""}` : empty;
}

/** `15 Jul 2026, 10:30` (IST) for timestamps; plain dates fall back to `formatDate`. */
export function formatDateTime(value: string | Date | null | undefined, empty = "—"): string {
  if (value == null || value === "") return empty;
  const p = dateParts(value);
  if (!p) return empty;
  const d = `${p.d} ${MONTHS[p.m - 1] ?? ""} ${p.y}`;
  return p.hasTime ? `${d}, ${pad(p.hh)}:${pad(p.mm)}` : d;
}

/** `10:30` from `10:30:00` or a timestamp (IST). */
export function formatTime(value: string | Date | null | undefined, empty = "—"): string {
  if (value == null || value === "") return empty;
  if (typeof value === "string") {
    const t = /^(\d{1,2}):(\d{2})/.exec(value.trim());
    if (t) return `${pad(Number(t[1]))}:${t[2]}`;
  }
  const p = dateParts(value);
  return p && p.hasTime ? `${pad(p.hh)}:${pad(p.mm)}` : empty;
}

/** Month labels for charts: `formatMonth(7)` → `Jul`. */
export function formatMonth(month1to12: number): string {
  return MONTHS[month1to12 - 1] ?? "";
}

/** Human label of a wire code: `not_submitted` → `Not submitted`, `approvalPending` → `Approval pending`, `UNDER-REVIEW` → `Under review`. */
export function humanize(code: string | null | undefined, empty = "—"): string {
  if (code == null || code === "") return empty;
  const words = code
    .replace(/([a-z0-9])([A-Z])/g, "$1 $2")
    .replace(/[_\-.]+/g, " ")
    .trim()
    .toLowerCase()
    .split(/\s+/);
  const fixed = words.map((w) => ACRONYMS[w] ?? w);
  const first = fixed[0] ?? "";
  return [first === first.toLowerCase() ? first.charAt(0).toUpperCase() + first.slice(1) : first, ...fixed.slice(1)].join(" ");
}

const ACRONYMS: Record<string, string> = {
  ug: "UG", pg: "PG", phd: "PhD", nep: "NEP", abc: "ABC", nptel: "NPTEL", swayam: "SWAYAM", upi: "UPI", gst: "GST", tds: "TDS", pf: "PF", esi: "ESI",
  hod: "HoD", id: "ID", sla: "SLA", naac: "NAAC", nba: "NBA", nirf: "NIRF", aishe: "AISHE", ugc: "UGC", aicte: "AICTE", cgpa: "CGPA", sgpa: "SGPA", btech: "B.Tech", mba: "MBA",
};

const STATUS_KIND: Record<string, BadgeKind> = {};
for (const s of ["approved", "active", "paid", "done", "completed", "complete", "published", "verified", "resolved", "present", "settled", "issued", "returned", "posted", "reconciled", "effective", "enrolled", "granted", "closed_ok", "available", "on_time", "finalised", "finalized", "passed", "pass", "cleared", "open_ok", "configured", "ready"])
  STATUS_KIND[s] = "done";
for (const s of ["pending", "submitted", "under_review", "in_review", "review", "approval_pending", "awaiting_approval", "partial", "partially_paid", "due", "due_soon", "late", "on_leave", "waitlisted", "hold", "on_hold", "provisional", "requested", "applied", "shortlisted", "moderation", "draft_review", "reserved"])
  STATUS_KIND[s] = "review";
for (const s of ["overdue", "rejected", "failed", "fail", "suspended", "blocked", "absent", "cancelled", "canceled", "withdrawn", "expired", "breached", "lost", "malpractice", "defaulted", "terminated", "detained", "missing", "error", "delayed"])
  STATUS_KIND[s] = "attention";
for (const s of ["scheduled", "open", "in_progress", "running", "upcoming", "new", "offered", "processing", "booked", "checked_out", "borrowed"]) STATUS_KIND[s] = "info";

/** Badge kind of a status code (`approval_pending` → `review`); unknown codes are `neutral`. */
export function statusKind(status: string | null | undefined): BadgeKind {
  if (!status) return "neutral";
  const key = status
    .replace(/([a-z0-9])([A-Z])/g, "$1_$2")
    .replace(/[\s\-.]+/g, "_")
    .toLowerCase();
  return STATUS_KIND[key] ?? "neutral";
}

const HONORIFICS = new Set(["dr", "prof", "mr", "mrs", "ms", "shri", "smt", "sri", "kum"]);

/** Initials for avatars: `Dr. Meera Shah` → `MS`, `Aditi` → `AD`. */
export function initialsOf(name: string | null | undefined): string {
  if (!name) return "?";
  const words = name
    .split(/\s+/)
    .map((w) => w.replace(/[^\p{L}\p{N}]/gu, ""))
    .filter((w) => w && !HONORIFICS.has(w.toLowerCase()));
  if (words.length === 0) return "?";
  if (words.length === 1) return (words[0] ?? "?").slice(0, 2).toUpperCase();
  return `${(words[0] ?? "").charAt(0)}${(words[words.length - 1] ?? "").charAt(0)}`.toUpperCase();
}

/** A stable pastel tone for a key (person avatars, class chips). Grey is never picked. */
export function toneFor(key: string | null | undefined): Tone {
  const palette = TONES.filter((t) => t !== "grey");
  let h = 0;
  for (const ch of key ?? "") h = (h * 31 + ch.charCodeAt(0)) >>> 0;
  return palette[h % palette.length] ?? "orange";
}
