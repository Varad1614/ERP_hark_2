/**
 * Batch 8 (Domains G · H · J, Campus · Services · Resources) web feature — tab and screen
 * definitions (foundation-owned, docs/batch8-contracts.md §10).
 *
 * Each slice owns one folder and fills the placeholder components already wired into the route
 * frames: `facilities/` (8A), `residence/` (8B), `library/` (8C, plus `features/reports/analytics/`),
 * `resources/` (8D). Route modules stay thin. Frames and placeholders use only @sutra/ui
 * components, with no custom styling.
 */
import { CAMPUS_SETUP_TABS } from "../institution";

export { activeTab, type TabDef } from "../institution";

/** Batch 8 tabs of Admin → Campus (`/campus/:tab?`), after the Batch 2 setup tabs, with their owning slice. */
export const CAMPUS_OPS_TABS = [
  { key: "infrastructure", label: "Infrastructure & rooms", slice: "8A" },
  { key: "facilities", label: "Service desk & maintenance", slice: "8A" },
  { key: "labs", label: "Laboratories", slice: "8A" },
  { key: "hostel", label: "Hostel", slice: "8B" },
  { key: "transport", label: "Transport", slice: "8B" },
  { key: "library", label: "Library", slice: "8C" },
  { key: "procurement", label: "Procurement", slice: "8D" },
  { key: "inventory", label: "Inventory", slice: "8D" },
  { key: "assets", label: "Assets", slice: "8D" },
] as const;

/** Every tab of Admin → Campus (first = default: the Batch 2 institution tab). */
export const CAMPUS_TABS = [...CAMPUS_SETUP_TABS, ...CAMPUS_OPS_TABS] as const;

/**
 * Re-skin v2 (R7): Admin → Campus shows five header tabs; each groups its sections, which stay
 * addressable as `/campus/<section>` (old links keep working) and appear as sub-section chips.
 * `/campus` (no section) is the Overview — the prototype Campus page.
 */
export const CAMPUS_GROUPS = [
  { key: "overview", label: "Overview", sections: [] as string[] },
  { key: "spaces", label: "Spaces", sections: ["infrastructure", "labs", "hostel"] },
  { key: "services", label: "Services", sections: ["facilities", "transport", "library"] },
  { key: "resources", label: "Resources", sections: ["procurement", "inventory", "assets"] },
  { key: "setup", label: "Setup", sections: ["institution", "readiness", "campuses", "organisation", "positions", "policies"] },
] as const;

/** Short chip labels of the Campus sections. */
export const SECTION_LABELS: Record<string, string> = {
  infrastructure: "Rooms & buildings",
  labs: "Laboratories",
  hostel: "Hostel",
  facilities: "Service desk",
  transport: "Transport",
  library: "Library",
  procurement: "Procurement",
  inventory: "Inventory",
  assets: "Assets",
  institution: "Institution",
  readiness: "Readiness",
  campuses: "Campuses",
  organisation: "Organisation",
  positions: "Positions",
  policies: "Policies",
};

/** Header group of a Campus section (`undefined` / unknown → overview). */
export function campusGroupOf(section: string | undefined): (typeof CAMPUS_GROUPS)[number] {
  return CAMPUS_GROUPS.find((g) => (g.sections as readonly string[]).includes(section ?? "")) ?? CAMPUS_GROUPS[0];
}

/**
 * Tabs of Admin → Reports (`/reports/:tab?`, first = default) with their owning slice. Re-skin v2
 * (R7): Overview = the six report cards; Governance moved here from Campus (`/campus/governance`
 * redirects).
 */
export const REPORTS_TABS = [
  { key: "overview", label: "Overview", slice: "8C" },
  { key: "dashboards", label: "Dashboards", slice: "8C" },
  { key: "regulatory", label: "Regulatory", slice: "8C" },
  { key: "accreditation", label: "Accreditation", slice: "9E" }, // 9E: H09 accreditation
  { key: "governance", label: "Governance", slice: "9E" }, // 9E: G00 campus governance
] as const;

/** Role-facing Batch 8 screens: route path → the slices that render panels on it and the roles that may open it. */
export const CAMPUS_SCREENS = {
  campus: { slices: ["8A", "8B", "8C", "8D"], roles: ["administrator"] },
  reports: { slices: ["8C"], roles: ["administrator"] },
  library: { slices: ["8C"], roles: ["student"] },
} as const;

/** Path of an Admin → Campus tab. */
export function campusPath(tab?: string): string {
  return `/campus${tab ? `/${tab}` : ""}`;
}

/** Path of an Admin → Reports tab. */
export function reportsPath(tab?: string): string {
  return `/reports${tab ? `/${tab}` : ""}`;
}
