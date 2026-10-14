/** Batch 8 foundation: Campus / Reports / Student Library frames, role gating and slice placeholders. */
import { screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { ROLE_CONFIG, SCREEN_BATCH, type Role } from "../../lib/roles";
import { workspaceBatch, workspaceScreen } from "../../lib/workspace-screens";
import { CAMPUS_SETUP_TABS } from "../institution";
import { activeTab, CAMPUS_OPS_TABS, CAMPUS_TABS, campusPath, REPORTS_TABS, reportsPath } from "./index";

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
    expect(activeTab(REPORTS_TABS, undefined).key).toBe("dashboards");
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
    expect(workspaceBatch("W-J05")).toBe(8);
  });
});

describe("Batch 8 route frames (foundation placeholders)", () => {
  it.each(CAMPUS_OPS_TABS.map((t): [string, string] => [t.key, t.slice]))("renders the Campus %s tab of slice %s", async (tab, slice) => {
    const renderScreen = await harness();
    const { default: Campus } = await import("../../routes/admin/campus-setup");
    await renderScreen(<Campus />, { path: "/campus/:tab?", entry: `/campus/${tab}` });
    await screen.findByRole("link", { name: "Assets" });
    expect(sliceMarker(slice)).toBeTruthy();
  });

  it.each(REPORTS_TABS.map((t): [string, string] => [t.key, t.slice]))("renders the Reports %s tab of slice %s", async (tab, slice) => {
    const renderScreen = await harness();
    const { default: Reports } = await import("../../routes/admin/reports");
    await renderScreen(<Reports />, { path: "/reports/:tab?", entry: `/reports/${tab}` });
    await screen.findByRole("link", { name: "Regulatory reporting" });
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
