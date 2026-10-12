/** Slice 3B screens in mock mode: registration (staff + self-service) and progression. */
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

describe("slice 3B screens", () => {
  it("lets a student register for the open window and submit", async () => {
    const renderScreen = await harness();
    const { StudentRegistrationScreen } = await import("./index");
    await renderScreen(<StudentRegistrationScreen />, { path: "/registration", role: "student" });
    expect(await screen.findByRole("heading", { name: "My registration" })).toBeTruthy();
    const cs = await waitFor(() => {
      const el = document.querySelector<HTMLInputElement>("[data-component='CS101']");
      expect(el).toBeTruthy();
      return el!;
    });
    fireEvent.click(cs);
    fireEvent.click(document.querySelector<HTMLInputElement>("[data-component='MD101']")!);
    expect(screen.getByText("20 cr selected")).toBeTruthy();
    fireEvent.click(document.querySelector("[data-action='registration-create']")!);
    await waitFor(() => expect(document.querySelector("[data-registration]")).toBeTruthy());
    fireEvent.click(document.querySelector("[data-action='registration-submit']")!);
    await waitFor(() => expect(document.querySelector("[data-action='registration-submit']")).toBeNull());
    expect(screen.getAllByText("Registered").length).toBeGreaterThan(0);
  });

  it("adds and drops a component on a draft registration (add/drop)", async () => {
    const renderScreen = await harness();
    const { StudentRegistrationScreen } = await import("./index");
    await renderScreen(<StudentRegistrationScreen />, { path: "/registration", role: "student" });
    const cs = await waitFor(() => {
      const el = document.querySelector<HTMLInputElement>("[data-component='CS101']");
      expect(el).toBeTruthy();
      return el!;
    });
    fireEvent.click(cs);
    fireEvent.click(document.querySelector("[data-action='registration-create']")!);
    await waitFor(() => expect(document.querySelector("[data-action='line-add']")).toBeTruthy());
    fireEvent.click(document.querySelector("[data-action='line-add']")!);
    const form = await waitFor(() => {
      const f = document.querySelector<HTMLFormElement>("[data-form='line-add']");
      expect(f).toBeTruthy();
      return f!;
    });
    const select = form.querySelector<HTMLSelectElement>("select[name='curriculumComponentId']")!;
    const md = [...select.options].find((o) => o.textContent?.startsWith("MD101"));
    expect(md).toBeTruthy();
    fireEvent.change(select, { target: { value: md!.value } });
    fireEvent.click(form.querySelector("button[type=submit]")!);
    await waitFor(() => expect(screen.getByText(/MD101 ·/)).toBeTruthy());
    expect(document.querySelectorAll("[data-registration] [data-action='line-drop']").length).toBe(2);
  });

  it("shows holds and the progression record in Student 360", async () => {
    const renderScreen = await harness();
    const { Student360Registration, Student360Progression } = await import("./index");
    await renderScreen(
      <>
        <Student360Registration studentId="s1" />
        <Student360Progression studentId="s1" />
      </>,
      { path: "/students/record/s1/registration" },
    );
    expect(await screen.findByText("No holds")).toBeTruthy();
    await waitFor(() => expect(document.querySelector("[data-finance='notApplicable']")).toBeTruthy());
    expect(screen.getByText("Finance: No obligation configured")).toBeTruthy();
    expect(screen.getByText("Progression decisions")).toBeTruthy();
    expect(screen.getByText("Not evaluated yet")).toBeTruthy();
    fireEvent.click(document.querySelector("[data-action='eligibility-evaluate']")!);
    await waitFor(() => expect(document.querySelector("[data-eligibility='pending']")).toBeTruthy());
  });

  it("lists registration windows and progression policies for administrators", async () => {
    const renderScreen = await harness();
    const { RegistrationAdminTab, ProgressionAdminTab } = await import("./index");
    const inst = { id: "i1", code: "SU", name: "Sutra University", status: "draft", timezone: "Asia/Kolkata" } as never;
    await renderScreen(
      <>
        <RegistrationAdminTab institution={inst} />
        <ProgressionAdminTab institution={inst} />
      </>,
      { path: "/students/registration" },
    );
    expect(await screen.findByText("Term 1 registration")).toBeTruthy();
    expect(await screen.findByText(/UG progression · UG v1/)).toBeTruthy();
    fireEvent.click(document.querySelector("[data-action='window-close']")!);
    await waitFor(() => expect(document.querySelector("[data-action='window-finalise']")).toBeTruthy());
  });

  it("asks for an institution first", async () => {
    const renderScreen = await harness();
    const { RegistrationAdminTab } = await import("./index");
    await renderScreen(<RegistrationAdminTab institution={null} />, { path: "/students/registration" });
    expect(screen.getByText("No institution selected")).toBeTruthy();
  });
});
