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

/**
 * Tabs of Admin → Examinations (`/examinations/:tab?`, first = default) with their owning slice.
 * Re-skin v2 (R4): Overview + Schedule + Marks + Results in the header, the rest behind
 * "More" (AreaHeader folds tabs 5+ into the menu). Every former tab key keeps its URL.
 */
export const EXAM_TABS = [
  { key: "overview", label: "Overview", slice: "R4" },
  { key: "schedule", label: "Schedule", slice: "6A" },
  { key: "marks", label: "Marks", slice: "6B" },
  { key: "results", label: "Results", slice: "6C" },
  { key: "registrations", label: "Registrations & hall tickets", slice: "6A" },
  { key: "malpractice", label: "Malpractice", slice: "6B" },
  { key: "revaluation", label: "Revaluation", slice: "6C" },
  { key: "sessions", label: "Sessions & policy", slice: "6A" },
] as const;

/** Role-facing Batch 6 screens: route path → the slices that render panels on it and the roles that may open it. */
export const EXAM_SCREENS = {
  examinations: { slices: ["6A", "6B", "6C"], roles: ["administrator"] },
  assessments: { slices: ["6B"], roles: ["faculty"] },
  results: { slices: ["6A", "6C"], roles: ["student"] },
} as const;

/** Path of an Admin → Examinations tab. */
export function examinationsPath(tab?: string): string {
  return `/examinations${tab && tab !== "overview" ? `/${tab}` : ""}`;
}
