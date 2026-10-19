/**
 * Batch 4 (Domain D, Academic delivery) web feature — tab definitions (foundation-owned,
 * docs/batch4-contracts.md §10).
 *
 * Each slice owns one folder and fills the placeholder components already wired into the routes:
 * `offering/` (4A), `delivery/` (4B), `flexible/` (4C). Route modules stay thin.
 */
export { activeTab, type TabDef } from "../institution";

/** Tabs of Admin → Academic delivery (`/academic-delivery/:tab?`, first = default) with their owning slice. */
export const ACADEMIC_TABS = [
  { key: "offerings", label: "Offerings", slice: "4A" },
  { key: "faculty", label: "Faculty assignment", slice: "4A" },
  { key: "timetable", label: "Timetable", slice: "4B" },
  { key: "attendance", label: "Attendance", slice: "4B" },
  { key: "monitoring", label: "Monitoring", slice: "4B" },
  { key: "assessment", label: "Internal assessment", slice: "4B" },
  { key: "flexible", label: "Flexible learning", slice: "4C" },
  { key: "recognition", label: "Credit recognition", slice: "4C" },
] as const;

/** Role-facing Batch 4 screens: route path → owning slice and the roles that may open it. */
export const ACADEMIC_SCREENS = {
  courses: { slice: "4A", roles: ["student"] },
  classes: { slice: "4A", roles: ["faculty"] },
  "my-students": { slice: "4A", roles: ["faculty"] },
  timetable: { slice: "4B", roles: ["student", "faculty"] },
  assignments: { slice: "4B", roles: ["student"] },
  attendance: { slice: "4B", roles: ["faculty"] },
  library: { slice: "4C", roles: ["student"] },
  resources: { slice: "4C", roles: ["faculty"] },
} as const;

/** Path of an Academic delivery admin tab. */
export function academicDeliveryPath(tab?: string): string {
  return `/academic-delivery${tab ? `/${tab}` : ""}`;
}
