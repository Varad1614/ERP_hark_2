/** Slice 4A screens in mock mode: offerings admin, faculty assignment, student courses, faculty classes / students. */
import { fireEvent, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

beforeEach(() => {
  vi.resetModules();
  window.sessionStorage.clear();
  window.localStorage.clear();
});

async function harness() {
  return (await import("../../../test/harness")).renderScreen;
}

const inst = { id: "i1", code: "SU", name: "Sutra University", status: "active", timezone: "Asia/Kolkata" } as never;

describe("slice 4A screens", () => {
  it("runs a draft offering through approval and publication", async () => {
    const renderScreen = await harness();
    const { OfferingsAdminTab } = await import("./index");
    await renderScreen(<OfferingsAdminTab institution={inst} />, { path: "/academic-delivery/offerings" });
    expect(document.querySelector("[data-slice='4A']")).toBeTruthy();
    expect(await screen.findByText("MD101-T1")).toBeTruthy();
    const row = () => document.querySelector("[data-offering='MD101-T1']")!.closest("tr")!;
    fireEvent.click(row().querySelector("[data-action='offering-submit']")!);
    await waitFor(() => expect(row().querySelector("[data-offering-status='approved']")).toBeTruthy());
    fireEvent.click(row().querySelector("[data-action='offering-publish']")!);
    await waitFor(() => expect(row().querySelector("[data-offering-status='published']")).toBeTruthy());
    fireEvent.click(row().querySelector("[data-action='offering-manage']")!);
    expect(await screen.findByText("MD101-T1 · sections")).toBeTruthy();
  });

  it("asks for a reason before suspending an active offering", async () => {
    const renderScreen = await harness();
    const { OfferingsAdminTab } = await import("./index");
    await renderScreen(<OfferingsAdminTab institution={inst} />, { path: "/academic-delivery/offerings" });
    await screen.findByText("CS101-T1");
    const row = () => document.querySelector("[data-offering='CS101-T1']")!.closest("tr")!;
    fireEvent.click(row().querySelector("[data-action='offering-suspend']")!);
    const form = await waitFor(() => {
      const f = document.querySelector<HTMLFormElement>("[data-form='offering-suspend']");
      expect(f).toBeTruthy();
      return f!;
    });
    fireEvent.click(form.querySelector("button[type=submit]")!);
    expect(await screen.findByText("Reason is required")).toBeTruthy();
    fireEvent.change(form.querySelector("input[name='reason']")!, { target: { value: "Instructor on leave" } });
    fireEvent.click(form.querySelector("button[type=submit]")!);
    await waitFor(() => expect(row().querySelector("[data-offering-status='suspended']")).toBeTruthy());
  });

  it("lists faculty assignments and governance versions", async () => {
    const renderScreen = await harness();
    const { FacultyAssignmentsAdminTab } = await import("./index");
    await renderScreen(<FacultyAssignmentsAdminTab institution={inst} />, { path: "/academic-delivery/faculty" });
    await waitFor(() => expect(document.querySelector("[data-assignment-status='active']")).toBeTruthy());
    expect(screen.getAllByText("Dr. Meera Shah").length).toBeGreaterThan(0);
    expect(await screen.findByText(/v1 · Workload norms/)).toBeTruthy();
    expect(screen.getByText("16 h/week")).toBeTruthy();
  });

  it("shows the student's courses with their faculty", async () => {
    const renderScreen = await harness();
    const { StudentCoursesScreen } = await import("./index");
    await renderScreen(<StudentCoursesScreen />, { path: "/courses", role: "student" });
    expect(await screen.findByRole("heading", { name: "My courses" })).toBeTruthy();
    await waitFor(() => expect(document.querySelector("[data-my-course='CS101-T1']")).toBeTruthy());
    expect(screen.getByText("Dr. Meera Shah")).toBeTruthy();
    expect(screen.getByText("Nothing open right now")).toBeTruthy();
  });

  it("shows the faculty member's classes and students", async () => {
    const renderScreen = await harness();
    const { FacultyClassesScreen, FacultyStudentsScreen } = await import("./index");
    await renderScreen(
      <>
        <FacultyClassesScreen />
        <FacultyStudentsScreen />
      </>,
      { path: "/classes", role: "faculty" },
    );
    await waitFor(() => expect(document.querySelector("[data-my-class='CS101-T1/A']")).toBeTruthy());
    await waitFor(() => expect(document.querySelector("[data-my-student='S2026001']")).toBeTruthy());
    expect(document.querySelector("[data-my-student='S2026001']")?.getAttribute("href")).toMatch(/^\/students\/record\//);
    expect(screen.getByText("Primary instructor")).toBeTruthy();
  });

  it("asks for an institution first", async () => {
    const renderScreen = await harness();
    const { OfferingsAdminTab } = await import("./index");
    await renderScreen(<OfferingsAdminTab institution={null} />, { path: "/academic-delivery/offerings" });
    expect(screen.getByText("No institution selected")).toBeTruthy();
  });
});
