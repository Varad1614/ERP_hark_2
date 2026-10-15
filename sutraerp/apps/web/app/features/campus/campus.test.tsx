/** Batch 8 foundation: Campus / Reports / Student Library frames, role gating and slice placeholders. */
import { screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { ROLE_CONFIG, SCREEN_BATCH, type Role } from "../../lib/roles";
import { workspaceBatch, workspaceScreen } from "../../lib/workspace-screens";
import { CAMPUS_SETUP_TABS } from "../institution";
import { activeTab, CAMPUS_GROUPS, CAMPUS_OPS_TABS, CAMPUS_TABS, campusGroupOf, campusPath, REPORTS_TABS, reportsPath } from "./index";

beforeEach(() => {
  vi.resetModules();
  window.sessionStorage.clear();
});

async function harness() {
  return (await import("../../test/harness")).renderScreen;
}

/** Placeholder of `slice`, or the slice's real screen once it lands (`data-slice`). */
const sliceMarker = (slice: string) => document.querySelector(`[data-placeholder='${slice}'], [data-slice='${slice}']`);

describe("Batch 8 tabs and screens", () => {
  it("keeps the Batch 2 setup tabs first, adds the Batch 8 tabs and keeps keys unique", () => {
    expect(activeTab(CAMPUS_TABS, undefined).key).toBe(CAMPUS_SETUP_TABS[0].key);
    expect(CAMPUS_TABS.slice(0, CAMPUS_SETUP_TABS.length).map((t) => t.key)).toEqual(CAMPUS_SETUP_TABS.map((t) => t.key));
    expect(activeTab(CAMPUS_TABS, "library").key).toBe("library");
    expect(new Set(CAMPUS_TABS.map((t) => t.key)).size).toBe(CAMPUS_TABS.length);
    expect(new Set(CAMPUS_OPS_TABS.map((t) => t.slice))).toEqual(new Set(["8A", "8B", "8C", "8D"]));
    // Re-skin v2: ≤5 header tabs; every section belongs to exactly one group; governance moved to Reports.
    expect(CAMPUS_GROUPS.length).toBeLessThanOrEqual(5);
    expect(CAMPUS_GROUPS.flatMap((g) => [...g.sections]).sort()).toEqual(CAMPUS_TABS.map((t) => t.key).sort());
    expect(campusGroupOf(undefined).key).toBe("overview");
    expect(campusGroupOf("hostel").key).toBe("spaces");
    expect(campusGroupOf("positions").key).toBe("setup");
    expect(REPORTS_TABS.length).toBeLessThanOrEqual(5);
    expect(REPORTS_TABS.map((t) => t.key)).toContain("governance");
    expect(activeTab(REPORTS_TABS, undefined).key).toBe("overview");
    expect(campusPath("hostel")).toBe("/campus/hostel");
    expect(reportsPath()).toBe("/reports");
    expect(SCREEN_BATCH.reports?.batch).toBe(8);
  });

  it("links Campus and Reports for administrators and Library for students", () => {
    const admin = ROLE_CONFIG.administrator.nav.map((n) => n.id);
    expect(admin).toContain("campus");
    expect(admin).toContain("reports");
    expect(ROLE_CONFIG.student.nav.map((n) => n.id)).toContain("library");
    expect(workspaceScreen("W-G03", "administrator")).toBe("/campus/hostel");
    expect(workspaceScreen("W-G05", "student")).toBe("/library");
    expect(workspaceScreen("W-H08", "administrator")).toBe("/reports/regulatory");
    expect(workspaceScreen("W-J08", "administrator")).toBe("/campus/assets");
    expect(workspaceScreen("W-G06", "administrator")).toBeNull();
    // Batch 9 carry-overs (integration).
    expect(workspaceScreen("W-G00", "administrator")).toBe("/campus/governance");
    expect(workspaceScreen("W-H09", "administrator")).toBe("/reports/accreditation");
    expect(workspaceScreen("W-G00", "student")).toBeNull();
    expect(workspaceBatch("W-J05")).toBe(8);
  });
});

describe("Batch 8 route frames (foundation placeholders)", () => {
  it.each(CAMPUS_OPS_TABS.map((t): [string, string] => [t.key, t.slice]))("renders the Campus %s tab of slice %s", async (tab, slice) => {
    const renderScreen = await harness();
    const { default: Campus } = await import("../../routes/admin/campus-setup");
    await renderScreen(<Campus />, { path: "/campus/:tab?", entry: `/campus/${tab}` });
    await screen.findByRole("link", { name: "Resources" });
    expect(document.querySelector(`[data-tab-link="${tab}"][aria-current="page"]`)).toBeTruthy();
    expect(sliceMarker(slice)).toBeTruthy();
  });

  it("opens the Campus overview (rooms today, transport, hostel, maintenance) with five header tabs", async () => {
    const renderScreen = await harness();
    const { default: Campus } = await import("../../routes/admin/campus-setup");
    const { createMockInstitutionApi } = await import("../institution/mock");
    const { InstitutionApiProvider, selectInstitution } = await import("../institution/hooks");
    const api = createMockInstitutionApi();
    const inst = await api.registerInstitution({ code: "OVW", name: "Overview College" });
    selectInstitution(inst.id);
    await renderScreen(
      <InstitutionApiProvider api={api}>
        <Campus />
      </InstitutionApiProvider>,
      { path: "/campus/:tab?", entry: "/campus" },
    );
    expect(await screen.findByText("Rooms today")).toBeTruthy();
    expect(screen.getByText("Maintenance tickets")).toBeTruthy();
    expect(document.querySelectorAll("[data-area-head] nav.sb-tabs a").length).toBe(5);
    expect(document.querySelector("[data-screen='campus']")?.getAttribute("data-group")).toBe("overview");
  });

  it.each(REPORTS_TABS.map((t): [string, string] => [t.key, t.slice]))("renders the Reports %s tab of slice %s", async (tab, slice) => {
    const renderScreen = await harness();
    const { default: Reports } = await import("../../routes/admin/reports");
    await renderScreen(<Reports />, { path: "/reports/:tab?", entry: `/reports/${tab}` });
    await screen.findByRole("link", { name: "Regulatory" });
    expect(document.querySelector("[data-screen='reports']")).toBeTruthy();
    expect(sliceMarker(slice)).toBeTruthy();
  });

  it("hides Admin → Reports from students and faculty", async () => {
    const renderScreen = await harness();
    const { default: Reports } = await import("../../routes/admin/reports");
    for (const role of ["student", "faculty"] as Role[]) {
      await renderScreen(<Reports />, { path: "/reports/:tab?", entry: "/reports", role });
      expect((await screen.findAllByText("This screen isn’t available")).length).toBeGreaterThan(0);
    }
  });

  it("renders the Student Library with the 8C panel and the 4C course resources", async () => {
    const renderScreen = await harness();
    const { default: Library } = await import("../../routes/student/library");
    await renderScreen(<Library />, { path: "/library", role: "student" });
    expect(document.querySelector("[data-screen='library']")).toBeTruthy();
    expect(sliceMarker("8C")).toBeTruthy();
    expect(sliceMarker("4C")).toBeTruthy();
  });
});
