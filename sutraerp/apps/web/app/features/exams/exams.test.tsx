/** Batch 6 foundation: Examinations frames, role gating and slice placeholders. */
import { screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { ROLE_CONFIG, SCREEN_BATCH, type Role } from "../../lib/roles";
import { activeTab, EXAM_SCREENS, EXAM_TABS, examinationsPath } from "./index";

beforeEach(() => {
  vi.resetModules();
  window.sessionStorage.clear();
});

async function harness() {
  return (await import("../../test/harness")).renderScreen;
}

/** Placeholder of `slice`, or the slice's real screen once it lands (`data-slice`). */
const sliceMarker = (slice: string) => document.querySelector(`[data-placeholder='${slice}'], [data-slice='${slice}']`);

describe("Batch 6 tabs and screens", () => {
  it("defaults to the overview, resolves known keys and keeps keys unique", () => {
    expect(activeTab(EXAM_TABS, undefined).key).toBe("overview");
    expect(activeTab(EXAM_TABS, "revaluation").key).toBe("revaluation");
    expect(activeTab(EXAM_TABS, "bogus").key).toBe("overview");
    expect(new Set(EXAM_TABS.map((t) => t.key)).size).toBe(EXAM_TABS.length);
    // Re-skin v2: the former seven tabs keep their URLs; four show in the header, the rest under More.
    for (const key of ["sessions", "schedule", "registrations", "conduct", "marks", "malpractice", "results", "revaluation"]) expect(EXAM_TABS.some((t) => t.key === key), key).toBe(true);
    expect(examinationsPath("marks")).toBe("/examinations/marks");
    expect(examinationsPath()).toBe("/examinations");
    expect(examinationsPath("overview")).toBe("/examinations");
    for (const key of Object.keys(EXAM_SCREENS)) expect(SCREEN_BATCH[key]?.batch, key).toBe(6);
  });

  it("links Examinations for administrators, Assessments for faculty and Results for students", () => {
    expect(ROLE_CONFIG.administrator.nav.map((n) => n.id)).toContain("examinations");
    expect(ROLE_CONFIG.faculty.nav.map((n) => n.id)).toContain("assessments");
    expect(ROLE_CONFIG.student.nav.map((n) => n.id)).toContain("results");
  });
});

describe("Batch 6 route frames (foundation placeholders)", () => {
  it.each([
    ["overview", "R4"],
    ["sessions", "6A"],
    ["registrations", "6A"],
    ["conduct", "6A"],
    ["marks", "6B"],
    ["malpractice", "6B"],
    ["results", "6C"],
    ["revaluation", "6C"],
  ])("renders the %s tab of slice %s", async (tab, slice) => {
    const renderScreen = await harness();
    const { default: Examinations } = await import("../../routes/admin/examinations");
    await renderScreen(<Examinations />, { path: "/examinations/:tab?", entry: `/examinations/${tab}` });
    await screen.findByRole("link", { name: "Schedule" });
    expect(document.querySelectorAll(".sb-area-head nav.sb-tabs a").length).toBeLessThanOrEqual(5);
    expect(document.querySelector("[data-tab-more]")).toBeTruthy();
    expect(sliceMarker(slice)).toBeTruthy();
  });

  it("hides Admin → Examinations from students and faculty", async () => {
    const renderScreen = await harness();
    const { default: Examinations } = await import("../../routes/admin/examinations");
    for (const role of ["student", "faculty"] as Role[]) {
      await renderScreen(<Examinations />, { path: "/examinations/:tab?", entry: "/examinations", role });
      expect((await screen.findAllByText("This screen isn’t available")).length).toBeGreaterThan(0);
    }
  });

  it("renders Faculty Assessments with the 6B panel", async () => {
    const renderScreen = await harness();
    const { default: Assessments } = await import("../../routes/faculty/assessments");
    await renderScreen(<Assessments />, { path: "/assessments", role: "faculty" });
    expect(document.querySelector("[data-screen='assessments']")).toBeTruthy();
    expect(sliceMarker("6B")).toBeTruthy();
  });

  it("renders Student Results with the 6C and 6A panels", async () => {
    const renderScreen = await harness();
    const { default: Results } = await import("../../routes/student/results");
    await renderScreen(<Results />, { path: "/results", role: "student" });
    expect(document.querySelector("[data-screen='results']")).toBeTruthy();
    expect(sliceMarker("6C")).toBeTruthy();
    expect(sliceMarker("6A")).toBeTruthy();
  });

  it("tells staff that Results is the students’ view", async () => {
    const renderScreen = await harness();
    const { default: Results } = await import("../../routes/student/results");
    await renderScreen(<Results />, { path: "/results", role: "administrator" });
    expect(await screen.findByText("This screen isn’t available")).toBeTruthy();
  });
});
