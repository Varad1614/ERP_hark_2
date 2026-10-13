/** Batch 4 foundation: Academic delivery frames, role gating, slice placeholders and the faculty nav fix. */
import { screen } from "@testing-library/react";
import type { ComponentType } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { ROLE_CONFIG, SCREEN_BATCH, type Role } from "../../lib/roles";
import { academicDeliveryPath, ACADEMIC_SCREENS, ACADEMIC_TABS, activeTab } from "./index";

beforeEach(() => {
  vi.resetModules();
  window.sessionStorage.clear();
});

async function harness() {
  return (await import("../../test/harness")).renderScreen;
}

/** Placeholder of `slice`, or the slice's real screen once it lands (`data-slice`). */
const sliceMarker = (slice: string) => document.querySelector(`[data-placeholder='${slice}'], [data-slice='${slice}']`);

describe("Batch 4 tabs and screens", () => {
  it("defaults to offerings, resolves known keys and keeps keys unique", () => {
    expect(activeTab(ACADEMIC_TABS, undefined).key).toBe("offerings");
    expect(activeTab(ACADEMIC_TABS, "recognition").key).toBe("recognition");
    expect(activeTab(ACADEMIC_TABS, "bogus").key).toBe("offerings");
    expect(new Set(ACADEMIC_TABS.map((t) => t.key)).size).toBe(ACADEMIC_TABS.length);
    expect(academicDeliveryPath("timetable")).toBe("/academic-delivery/timetable");
    for (const key of Object.keys(ACADEMIC_SCREENS)) expect(SCREEN_BATCH[key]?.batch, key).toBe(4);
  });

  it("points the faculty Students entry at the faculty roster, not the admin-only directory", () => {
    const faculty = ROLE_CONFIG.faculty;
    expect(faculty.nav.map((n) => n.id)).not.toContain("students");
    expect(faculty.nav.map((n) => n.id)).toContain("my-students");
    expect(faculty.kpis.every((k) => k.to !== "students")).toBe(true);
    expect(ROLE_CONFIG.administrator.nav.map((n) => n.id)).toContain("academic-delivery");
  });
});

describe("Batch 4 route frames (foundation placeholders)", () => {
  it("renders Admin → Academic delivery with a tab per slice", async () => {
    const renderScreen = await harness();
    const { default: AcademicDelivery } = await import("../../routes/admin/academic-delivery");
    await renderScreen(<AcademicDelivery />, { path: "/academic-delivery/:tab?", entry: "/academic-delivery/attendance" });
    expect(await screen.findByRole("link", { name: "Credit recognition" })).toBeTruthy();
    expect(screen.getByRole("link", { name: "Faculty assignment" }).getAttribute("href")).toBe("/academic-delivery/faculty");
  });

  it("hides Academic delivery from faculty", async () => {
    const renderScreen = await harness();
    const { default: AcademicDelivery } = await import("../../routes/admin/academic-delivery");
    await renderScreen(<AcademicDelivery />, { path: "/academic-delivery/:tab?", entry: "/academic-delivery", role: "faculty" });
    expect(await screen.findByText("This screen isn’t available")).toBeTruthy();
  });

  type Mod = () => Promise<{ default: ComponentType }>;
  const cases: [string, Mod, Role, string][] = [
    ["courses", () => import("../../routes/student/courses"), "student", "4A"],
    ["assignments", () => import("../../routes/student/assignments"), "student", "4B"],
    ["library", () => import("../../routes/student/library"), "student", "4C"],
    ["timetable", () => import("../../routes/shared/timetable"), "student", "4B"],
    ["timetable", () => import("../../routes/shared/timetable"), "faculty", "4B"],
    ["classes", () => import("../../routes/faculty/classes"), "faculty", "4A"],
    ["my-students", () => import("../../routes/faculty/my-students"), "faculty", "4A"],
    ["attendance", () => import("../../routes/faculty/attendance"), "faculty", "4B"],
    ["resources", () => import("../../routes/faculty/resources"), "faculty", "4C"],
  ];

  it.each(cases)("renders /%s for its role", async (path, load, role, slice) => {
    const renderScreen = await harness();
    const { default: Screen } = await load();
    await renderScreen(<Screen />, { path: `/${path}`, role });
    expect(document.querySelector(`[data-screen='${path}']`)).toBeTruthy();
    expect(sliceMarker(slice)).toBeTruthy();
  });

  it("tells other roles that a role screen is not theirs", async () => {
    const renderScreen = await harness();
    const { default: Classes } = await import("../../routes/faculty/classes");
    await renderScreen(<Classes />, { path: "/classes", role: "student" });
    expect(await screen.findByText("This screen isn’t available")).toBeTruthy();
    const { default: Timetable } = await import("../../routes/shared/timetable");
    await renderScreen(<Timetable />, { path: "/timetable", role: "administrator" });
    expect((await screen.findAllByText("This screen isn’t available")).length).toBeGreaterThan(0);
  });
});
