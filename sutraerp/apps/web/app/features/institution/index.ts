/**
 * Batch 2 (Domain B) web feature — owned by slice 2C (see docs/batch2-contracts.md §8).
 *
 * Put the API hooks, tab components and tests for Admin → Academics and Admin → Campus setup in
 * this folder (one file per tab, e.g. `CampusesTab.tsx`, `CalendarTab.tsx`). Route modules in
 * `routes/admin/` stay thin and only pick the tab.
 */

/** Tabs of `/academics/:tab?` (first = default). */
export const ACADEMICS_TABS = [
  { key: "structure", label: "Academic structure" },
  { key: "programmes", label: "Programmes" },
  { key: "curriculum", label: "Curriculum" },
  { key: "school-model", label: "School model" },
  { key: "calendar", label: "Academic calendar" },
] as const;

/** Tabs of `/campus/:tab?` (first = default). */
export const CAMPUS_SETUP_TABS = [
  { key: "institution", label: "Institution" },
  { key: "readiness", label: "Activation readiness" },
  { key: "campuses", label: "Campuses" },
  { key: "organisation", label: "Organisation" },
  { key: "positions", label: "Positions" },
  { key: "policies", label: "Policies" },
] as const;

export type TabDef = { readonly key: string; readonly label: string };

/** The active tab for a route param (unknown or missing → first tab). */
export function activeTab(tabs: readonly TabDef[], param: string | undefined): TabDef {
  const found = tabs.find((t) => t.key === param) ?? tabs[0];
  if (!found) throw new Error("tab list is empty");
  return found;
}
