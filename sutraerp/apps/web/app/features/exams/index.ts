/**
 * Batch 6 (Domain I, Examinations) web feature — tab and screen definitions (foundation-owned,
 * docs/batch6-contracts.md §10).
 *
 * Each slice owns one folder and fills the placeholder components already wired into the route
 * frames: `setup/` (6A), `marks/` (6B), `results/` (6C). Route modules stay thin. The UI is
 * being re-skinned (branch ui/reskin): frames and placeholders use only @sutra/ui components,
 * with no custom styling, so the new design applies without edits here.
 */
export { activeTab, type TabDef } from "../institution";

/** Tabs of Admin → Examinations (`/examinations/:tab?`, first = default) with their owning slice. */
export const EXAM_TABS = [
  { key: "sessions", label: "Sessions & policy", slice: "6A" },
  { key: "schedule", label: "Timetable & seating", slice: "6A" },
  { key: "registrations", label: "Registrations & hall tickets", slice: "6A" },
  { key: "marks", label: "Marks & moderation", slice: "6B" },
  { key: "malpractice", label: "Malpractice", slice: "6B" },
  { key: "results", label: "Results & publication", slice: "6C" },
  { key: "revaluation", label: "Revaluation", slice: "6C" },
] as const;

/** Role-facing Batch 6 screens: route path → the slices that render panels on it and the roles that may open it. */
export const EXAM_SCREENS = {
  examinations: { slices: ["6A", "6B", "6C"], roles: ["administrator"] },
  assessments: { slices: ["6B"], roles: ["faculty"] },
  results: { slices: ["6A", "6C"], roles: ["student"] },
} as const;

/** Path of an Admin → Examinations tab. */
export function examinationsPath(tab?: string): string {
  return `/examinations${tab ? `/${tab}` : ""}`;
}
