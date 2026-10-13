/** Batch 7 foundation: People frames, role gating and slice placeholders. */
import { screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { ROLE_CONFIG, SCREEN_BATCH, type Role } from "../../lib/roles";
import { workspaceScreen } from "../../lib/workspace-screens";
import { activeTab, MY_HR_TABS, myHrPath, PEOPLE_SCREENS, PEOPLE_TABS, peoplePath } from "./index";

beforeEach(() => {
  vi.resetModules();
  window.sessionStorage.clear();
});

async function harness() {
  return (await import("../../test/harness")).renderScreen;
}

/** Placeholder of `slice`, or the slice's real screen once it lands (`data-slice`). */
const sliceMarker = (slice: string) => document.querySelector(`[data-placeholder='${slice}'], [data-slice='${slice}']`);

describe("Batch 7 tabs and screens", () => {
  it("defaults to staff, resolves known keys and keeps keys unique", () => {
    expect(activeTab(PEOPLE_TABS, undefined).key).toBe("staff");
    expect(activeTab(PEOPLE_TABS, "payroll").key).toBe("payroll");
    expect(activeTab(PEOPLE_TABS, "bogus").key).toBe("staff");
    expect(new Set(PEOPLE_TABS.map((t) => t.key)).size).toBe(PEOPLE_TABS.length);
    expect(peoplePath("leave")).toBe("/people/leave");
    expect(peoplePath()).toBe("/people");
    expect(activeTab(MY_HR_TABS, undefined).key).toBe("leave");
    expect(myHrPath("leave")).toBe("/leave");
    expect(myHrPath("payslips")).toBe("/leave/payslips");
    for (const key of Object.keys(PEOPLE_SCREENS)) expect(SCREEN_BATCH[key]?.batch, key).toBe(7);
  });

  it("links People for administrators and Leave for faculty, not for students", () => {
    expect(ROLE_CONFIG.administrator.nav.map((n) => n.id)).toContain("people");
    expect(ROLE_CONFIG.faculty.nav.map((n) => n.id)).toContain("leave");
    expect(ROLE_CONFIG.student.nav.map((n) => n.id)).not.toContain("leave");
    expect(workspaceScreen("W-E05", "administrator")).toBe("/people/leave");
    expect(workspaceScreen("W-E05", "faculty")).toBe("/leave");
    expect(workspaceScreen("W-E06", "student")).toBeNull();
  });
});

describe("Batch 7 route frames (foundation placeholders)", () => {
  it.each([
    ["staff", "7A"],
    ["recruitment", "7A"],
    ["positions", "7A"],
    ["leave", "7B"],
    ["attendance", "7B"],
    ["workload", "7B"],
    ["payroll", "7C"],
  ])("renders the %s tab of slice %s", async (tab, slice) => {
    const renderScreen = await harness();
    const { default: People } = await import("../../routes/admin/people");
    await renderScreen(<People />, { path: "/people/:tab?", entry: `/people/${tab}` });
    await screen.findByRole("link", { name: "Payroll" });
    expect(sliceMarker(slice)).toBeTruthy();
  });

  it("hides Admin → People from students and faculty", async () => {
    const renderScreen = await harness();
    const { default: People } = await import("../../routes/admin/people");
    for (const role of ["student", "faculty"] as Role[]) {
      await renderScreen(<People />, { path: "/people/:tab?", entry: "/people", role });
      expect((await screen.findAllByText("This screen isn’t available")).length).toBeGreaterThan(0);
    }
  });

  it.each([
    ["", "7B"],
    ["/profile", "7A"],
    ["/attendance", "7B"],
    ["/workload", "7B"],
    ["/payslips", "7C"],
  ])("renders Faculty Leave (My HR) tab '%s' with slice %s", async (sub, slice) => {
    const renderScreen = await harness();
    const { default: Leave } = await import("../../routes/faculty/leave");
    await renderScreen(<Leave />, { path: "/leave/:tab?", entry: `/leave${sub}`, role: "faculty" });
    expect(document.querySelector("[data-screen='leave']")).toBeTruthy();
    expect(document.querySelector("[data-page-head='leave']")).toBeTruthy();
    await screen.findByRole("link", { name: "Payslips" });
    expect(sliceMarker(slice), slice).toBeTruthy();
  });

  it("tells students that Leave is the staff view", async () => {
    const renderScreen = await harness();
    const { default: Leave } = await import("../../routes/faculty/leave");
    await renderScreen(<Leave />, { path: "/leave/:tab?", entry: "/leave", role: "student" });
    expect(await screen.findByText("This screen isn’t available")).toBeTruthy();
  });
});
