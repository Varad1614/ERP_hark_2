/** Batch 3 foundation: Student 360 frame and student-facing placeholders render and route. */
import { screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

beforeEach(() => {
  vi.resetModules();
  window.sessionStorage.clear();
});

async function harness() {
  return (await import("../../test/harness")).renderScreen;
}

describe("Batch 3 screens (foundation placeholders)", () => {
  it("renders Student 360 tabs for staff with the slice placeholder", async () => {
    const renderScreen = await harness();
    const { default: Student360 } = await import("../../routes/admin/student-360");
    await renderScreen(<Student360 />, { path: "/students/record/:studentId/:tab?", entry: "/students/record/s1/registration" });
    expect(await screen.findByText("Student 360")).toBeTruthy(); // header chip; the heading is the student's name
    expect(document.querySelector("[data-student-name]")).toBeTruthy();
    expect(document.querySelector("[data-tab='registration']")).toBeTruthy();
    expect(document.querySelector("[data-placeholder='3B'], [data-slice='3B']")).toBeTruthy(); // 3B: slice screens replace the placeholder
    const link = screen.getByRole("link", { name: "Guardians" });
    expect(link.getAttribute("href")).toBe("/students/record/s1/guardians");
  });

  it("hides Student 360 from students", async () => {
    const renderScreen = await harness();
    const { default: Student360 } = await import("../../routes/admin/student-360");
    await renderScreen(<Student360 />, { path: "/students/record/:studentId/:tab?", entry: "/students/record/s1", role: "student" });
    expect(await screen.findByText("This screen isn’t available")).toBeTruthy();
  });

  it("tells staff that the self-service screens are for students", async () => {
    const renderScreen = await harness();
    const { default: Registration } = await import("../../routes/student/registration");
    await renderScreen(<Registration />, { path: "/registration", role: "administrator" });
    expect(await screen.findByText("This screen isn’t available")).toBeTruthy();
  });

  it("renders the student-facing documents and registration screens", async () => {
    const renderScreen = await harness();
    const { default: Documents } = await import("../../routes/student/documents");
    await renderScreen(<Documents />, { path: "/documents", role: "student" });
    // 3A: the placeholder was replaced by the real screen (no linked student in mock mode).
    expect(await screen.findByText("No student record linked")).toBeTruthy();
    const { default: Registration } = await import("../../routes/student/registration");
    await renderScreen(<Registration />, { path: "/registration", role: "student" });
    expect(document.querySelector("[data-placeholder='3B'], [data-slice='3B']")).toBeTruthy(); // 3B: slice screens replace the placeholder
  });
});
