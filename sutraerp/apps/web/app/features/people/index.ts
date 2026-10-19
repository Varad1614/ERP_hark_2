/**
 * Batch 7 (Domain E, People & workforce) web feature — tab and screen definitions
 * (foundation-owned, docs/batch7-contracts.md §10).
 *
 * Each slice owns one folder and fills the placeholder components already wired into the route
 * frames: `workforce/` (7A), `leave/` (7B), `payroll/` (7C). Route modules stay thin. Frames and
 * placeholders use only @sutra/ui components, with no custom styling.
 */
export { activeTab, type TabDef } from "../institution";

/**
 * Tabs of Admin → People (`/people/:tab?`, first = default) with their owning slice. Re-skin v2:
 * the header shows Directory · n / Leave requests · n / Payroll / Recruitment and folds the rest
 * into "More" (≤5 header tabs, docs/reskin2-contracts.md §2.1).
 */
export const PEOPLE_TABS = [
  { key: "staff", label: "Directory", slice: "7A" },
  { key: "leave", label: "Leave requests", slice: "7B" },
  { key: "payroll", label: "Payroll", slice: "7C" },
  { key: "recruitment", label: "Recruitment & onboarding", slice: "7A" },
  { key: "positions", label: "Positions & reporting", slice: "7A" },
  { key: "attendance", label: "Attendance", slice: "7B" },
  { key: "workload", label: "Workload", slice: "7B" },
] as const;

/** Tabs of Faculty Leave = My HR (`/leave/:tab?`, first = default) with their owning slice. */
export const MY_HR_TABS = [
  { key: "leave", label: "Leave", slice: "7B" },
  { key: "profile", label: "My record", slice: "7A" },
  { key: "attendance", label: "Attendance", slice: "7B" },
  { key: "workload", label: "Workload", slice: "7B" },
  { key: "payslips", label: "Payslips", slice: "7C" },
] as const;

/** Path of a My HR tab (the default tab is `/leave`). */
export function myHrPath(tab?: string): string {
  return !tab || tab === "leave" ? "/leave" : `/leave/${tab}`;
}

/** Role-facing Batch 7 screens: route path → the slices that render panels on it and the roles that may open it. */
export const PEOPLE_SCREENS = {
  people: { slices: ["7A", "7B", "7C"], roles: ["administrator"] },
  leave: { slices: ["7A", "7B", "7C"], roles: ["faculty"] },
} as const;

/** Path of an Admin → People tab. */
export function peoplePath(tab?: string): string {
  return `/people${tab && tab !== "staff" ? `/${tab}` : ""}`;
}
