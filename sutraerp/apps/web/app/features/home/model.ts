/**
 * Customizable Home — layout model (re-skin v2, ported from the prototype's dash.js).
 *
 * A layout is an ordered list of tiles on a 12-column grid; each tile has a widget type, a width
 * (columns), a height (rows), an optional colour and, for metric tiles, the metric index.
 * Layouts live in localStorage per user + role (no preferences API: an API change needs the
 * user's approval first). Pure functions only: easy to test, safe to call during render.
 */
import type { Role } from "../../lib/roles";

export type WidgetType = "hero" | "now" | "kpi" | "tasks" | "shortcuts" | "notice" | "notifs" | "pinned" | "brief" | "note" | "flags" | "week" | "bunk";

export interface HomeTile {
  id: string;
  type: WidgetType;
  w: number;
  h: number;
  /** Metric index (kpi tiles). */
  i?: number;
  color?: string;
  text?: string;
  title?: string;
}

export interface HomeLayout {
  items: HomeTile[];
  preset?: string;
}

export interface WidgetMeta {
  name: string;
  desc: string;
  w: number;
  h: number;
  /** Narrowest width that still reads well. */
  min: number;
  /** More than one allowed (sticky notes). */
  many?: boolean;
  color?: string;
  /** Only these roles can add it. */
  roles?: Role[];
}

export const WIDGETS: Record<WidgetType, WidgetMeta> = {
  hero: { name: "Greeting", desc: "Welcome banner with your main action", w: 7, h: 1, min: 4 },
  now: { name: "Right now", desc: "What is on now, and what is next", w: 5, h: 1, min: 3 },
  kpi: { name: "Metric", desc: "A key number at a glance", w: 3, h: 1, min: 2 },
  tasks: { name: "To-dos", desc: "Your work queue for today", w: 7, h: 2, min: 4 },
  shortcuts: { name: "Shortcuts", desc: "One-tap jumps to common screens", w: 5, h: 1, min: 3 },
  notice: { name: "Announcement", desc: "The latest campus notice", w: 5, h: 1, min: 3 },
  notifs: { name: "Recent activity", desc: "Notifications as a live feed", w: 4, h: 2, min: 3 },
  pinned: { name: "Pinned workspaces", desc: "Workspaces you open most", w: 4, h: 1, min: 3 },
  brief: { name: "Morning brief", desc: "What changed since you last signed in", w: 5, h: 1, min: 4 },
  note: { name: "Sticky note", desc: "A private note, saved as you type", w: 4, h: 1, min: 2, many: true, color: "amber" },
  flags: { name: "Early flags", desc: "Numbers drifting from normal, before they become problems", w: 5, h: 1, min: 4, roles: ["administrator"] },
  week: { name: "Your week", desc: "What you closed this week", w: 7, h: 1, min: 4 },
  bunk: { name: "Attendance buffer", desc: "How many classes you can still miss per course", w: 5, h: 2, min: 4, roles: ["student"] },
};

/** Names of the four metric tiles per role (customize labels, widget library). */
export const KPI_NAMES: Record<Role, string[]> = {
  administrator: ["Students enrolled", "Faculty on roll", "Fee collection", "Open work orders"],
  faculty: ["Classes today", "My students", "To review", "Avg attendance"],
  student: ["Attendance", "CGPA", "Due this week", "Fees"],
};

/** Label of a tile in customize mode. */
export function tileLabel(t: HomeTile, role: Role): string {
  return t.title ?? (t.type === "kpi" ? (KPI_NAMES[role][t.i ?? 0] ?? WIDGETS.kpi.name) : WIDGETS[t.type].name);
}

export const WIDGET_TYPES = Object.keys(WIDGETS) as WidgetType[];

/** Named tile colours: [key, label, light background]. */
export const SWATCHES: ReadonlyArray<readonly [string, string, string | null]> = [
  ["default", "Default", null],
  ["accent", "Accent", "var(--acs)"],
  ["ink", "Ink", "#1C1B22"],
  ["white", "White", "#FFFFFF"],
  ["orange", "Peach", "#FFE9DC"],
  ["amber", "Butter", "#FFF1D1"],
  ["green", "Mint", "#DFF3E8"],
  ["teal", "Aqua", "#DAF2F2"],
  ["blue", "Sky", "#DCEBFF"],
  ["violet", "Lilac", "#E7E1FF"],
  ["rose", "Blush", "#FFE4DE"],
];
export const WIDTHS: ReadonlyArray<readonly [number, string]> = [[3, "S"], [4, "M"], [6, "L"], [8, "XL"], [12, "Full"]];
export const MAX_TILES = 40;

export const isHex = (v: unknown): v is string => typeof v === "string" && /^#[0-9a-f]{6}$/i.test(v);
const clamp = (v: number, a: number, b: number) => Math.max(a, Math.min(b, v));

const tile = (type: WidgetType, w: number, h: number, i?: number): HomeTile => ({ id: type + (i ?? ""), type, w, h, ...(i === undefined ? {} : { i }) });

const BASE: ReadonlyArray<readonly [WidgetType, number, number, number?]> = [
  ["hero", 7, 1],
  ["now", 5, 1],
  ["kpi", 3, 1, 0],
  ["kpi", 3, 1, 1],
  ["kpi", 3, 1, 2],
  ["kpi", 3, 1, 3],
  ["tasks", 7, 2],
  ["shortcuts", 5, 1],
  ["notice", 5, 1],
];

/** Role tiles the prototype appends to the standard Home. */
const ROLE_EXTRA: Record<Role, ReadonlyArray<readonly [WidgetType, number, number]>> = {
  administrator: [["flags", 5, 1], ["week", 7, 1]],
  faculty: [["week", 5, 1]],
  student: [["bunk", 5, 2]],
};

/** The built-in layout of a role. */
export function builtInLayout(role: Role = "administrator"): HomeLayout {
  return { items: [...BASE, ...ROLE_EXTRA[role]].map(([t, w, h, i]) => tile(t, w, h, i)), preset: "everyday" };
}

/** Season presets per role: [key, name, description]. */
export const PRESETS: Record<Role, ReadonlyArray<readonly [string, string, string]>> = {
  administrator: [
    ["everyday", "Everyday", "The standard Home"],
    ["exam", "Exam week", "Calendar, attendance and your queue up front"],
    ["admissions", "Admissions", "Enrolment, activity and fees"],
    ["results", "Results day", "What changed, and verification work"],
  ],
  faculty: [
    ["everyday", "Everyday", "The standard Home"],
    ["exam", "Exam week", "Today’s classes and your queue up front"],
    ["grading", "Grading", "Submissions to review, front and centre"],
    ["start", "Semester start", "Timetable, classes and pinned workspaces"],
  ],
  student: [
    ["everyday", "Everyday", "The standard Home"],
    ["exam", "Exam week", "Deadlines and attendance"],
    ["results", "Results day", "Grades and what changed"],
  ],
};

type Spec = readonly [WidgetType, number, number, number?];
const LAYOUTS: Record<string, readonly Spec[]> = {
  exam: [["brief", 7, 1], ["now", 5, 1], ["tasks", 7, 2], ["notifs", 5, 2], ["kpi", 4, 1, 3], ["shortcuts", 8, 1]],
  admissions: [["brief", 7, 1], ["kpi", 5, 1, 0], ["tasks", 7, 2], ["notifs", 5, 2], ["kpi", 4, 1, 2], ["shortcuts", 8, 1]],
  results: [["brief", 12, 1], ["kpi", 4, 1, 1], ["kpi", 4, 1, 0], ["kpi", 4, 1, 3], ["tasks", 7, 2], ["notifs", 5, 2]],
  grading: [["brief", 6, 1], ["kpi", 6, 1, 2], ["tasks", 7, 2], ["notifs", 5, 2], ["shortcuts", 12, 1]],
  start: [["hero", 7, 1], ["now", 5, 1], ["pinned", 4, 1], ["shortcuts", 4, 1], ["kpi", 4, 1, 0], ["kpi", 4, 1, 1], ["tasks", 8, 2]],
};

/** The layout of a preset; personal sticky notes come along. */
export function presetLayout(key: string, current: HomeLayout, role: Role): HomeLayout {
  const notes = current.items.filter((x) => x.type === "note");
  const specs = LAYOUTS[key];
  const items = key === "everyday" || !specs ? builtInLayout(role).items : specs.map(([t, w, h, i]) => tile(t, w, h, i));
  return { items: [...items, ...notes], preset: key };
}

/** Drops anything unusable (unknown types, bad sizes, duplicates); never throws. */
export function sanitizeLayout(raw: unknown): HomeLayout | undefined {
  if (!raw || typeof raw !== "object" || !Array.isArray((raw as HomeLayout).items)) return undefined;
  const seen = new Set<string>();
  const items: HomeTile[] = [];
  for (const t of (raw as HomeLayout).items.slice(0, MAX_TILES)) {
    if (!t || typeof t !== "object" || !WIDGET_TYPES.includes(t.type)) continue;
    const id = String(t.id ?? "").toLowerCase().replace(/[^a-z0-9-]/g, "").slice(0, 40);
    if (!id || seen.has(id)) continue;
    if (t.type === "kpi" && !(Number.isInteger(t.i) && (t.i as number) >= 0 && (t.i as number) <= 3)) continue;
    seen.add(id);
    const out: HomeTile = { id, type: t.type, w: clamp(Math.round(Number(t.w) || WIDGETS[t.type].w), 1, 12), h: clamp(Math.round(Number(t.h) || 1), 1, 4) };
    if (t.type === "kpi") out.i = t.i;
    if (typeof t.color === "string" && (isHex(t.color) || SWATCHES.some((s) => s[0] === t.color))) out.color = t.color;
    if (typeof t.text === "string") out.text = t.text.slice(0, 2000);
    if (typeof t.title === "string" && t.title.trim()) out.title = t.title.replace(/[\r\n\t]/g, " ").slice(0, 60);
    items.push(out);
  }
  const preset = typeof (raw as HomeLayout).preset === "string" && /^[a-z-]{1,32}$/.test((raw as HomeLayout).preset as string) ? (raw as HomeLayout).preset : undefined;
  return preset ? { items, preset } : { items };
}

/* ------------------------------------------------------------------ edits (pure) */

export function moveTile(layout: HomeLayout, id: string, to: number): HomeLayout {
  const from = layout.items.findIndex((x) => x.id === id);
  if (from < 0) return layout;
  const items = [...layout.items];
  const [it] = items.splice(from, 1);
  items.splice(clamp(to, 0, items.length), 0, it!);
  return { ...layout, items };
}

export function resizeTile(layout: HomeLayout, id: string, size: { w?: number; h?: number }): HomeLayout {
  return {
    ...layout,
    items: layout.items.map((x) =>
      x.id === id ? { ...x, w: size.w === undefined ? x.w : clamp(size.w, WIDGETS[x.type].min, 12), h: size.h === undefined ? x.h : clamp(size.h, 1, 4) } : x,
    ),
  };
}

export function updateTile(layout: HomeLayout, id: string, patch: Partial<Pick<HomeTile, "color" | "text" | "title">>): HomeLayout {
  return {
    ...layout,
    items: layout.items.map((x) => {
      if (x.id !== id) return x;
      const next = { ...x, ...patch };
      (Object.keys(patch) as (keyof typeof patch)[]).forEach((k) => patch[k] === undefined && delete next[k]);
      return next;
    }),
  };
}

export function removeTile(layout: HomeLayout, id: string): HomeLayout {
  return { ...layout, items: layout.items.filter((x) => x.id !== id) };
}

/** Whether a (non-repeatable) widget / metric is already on the layout. */
export function hasTile(layout: HomeLayout, type: WidgetType, i?: number): boolean {
  return layout.items.some((x) => x.type === type && (i === undefined || x.i === i));
}

export function addTile(layout: HomeLayout, type: WidgetType, i?: number, now: number = Date.now()): { layout: HomeLayout; tile: HomeTile } {
  const m = WIDGETS[type];
  const base = type + (i ?? "");
  const id = m.many || layout.items.some((x) => x.id === base) ? `${base}-${now.toString(36)}` : base;
  const t: HomeTile = { id, type, w: m.w, h: m.h, ...(i === undefined ? {} : { i }), ...(m.color ? { color: m.color } : {}) };
  return { layout: { ...layout, items: [...layout.items, t].slice(0, MAX_TILES) }, tile: t };
}

/** Keyboard editing (prototype): arrows move, Shift+arrows resize. Returns null when unhandled. */
export function keyEdit(layout: HomeLayout, id: string, key: string, shift: boolean): HomeLayout | null {
  const idx = layout.items.findIndex((x) => x.id === id);
  const it = layout.items[idx];
  if (!it) return null;
  if (shift) {
    if (key === "ArrowRight") return resizeTile(layout, id, { w: it.w + 1 });
    if (key === "ArrowLeft") return resizeTile(layout, id, { w: it.w - 1 });
    if (key === "ArrowDown") return resizeTile(layout, id, { h: it.h + 1 });
    if (key === "ArrowUp") return resizeTile(layout, id, { h: it.h - 1 });
    return null;
  }
  if (key === "ArrowLeft" || key === "ArrowUp") return idx > 0 ? moveTile(layout, id, idx - 1) : layout;
  if (key === "ArrowRight" || key === "ArrowDown") return moveTile(layout, id, idx + 1);
  return null;
}

/** Focus mode keeps only what is on now and the work queue, full width (prototype `.dfg`). */
export function focusLayout(): HomeLayout {
  return { items: [tile("now", 12, 1), tile("tasks", 12, 2)] };
}

/** Background and text tone for a tile colour (contrast against the swatch). */
export function tileTone(color: string | undefined): { background?: string; dark?: boolean } {
  if (!color || color === "default") return {};
  if (color === "accent") return { background: "var(--acs)" };
  const sw = SWATCHES.find((s) => s[0] === color);
  const hex = sw ? sw[2] : isHex(color) ? color : null;
  if (!hex) return {};
  if (!hex.startsWith("#")) return { background: hex };
  const [r, g, b] = [1, 3, 5].map((n) => parseInt(hex.slice(n, n + 2), 16) / 255).map((v) => (v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4));
  const lum = 0.2126 * r! + 0.7152 * g! + 0.0722 * b!;
  return { background: hex, dark: lum < 0.32 };
}
