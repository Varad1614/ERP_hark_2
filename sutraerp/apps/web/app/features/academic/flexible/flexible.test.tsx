/** Slice 4C screens against the in-memory API (mock mode) + provider-record parsing. */
import { fireEvent, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { parseProviderRecords } from "./api";

beforeEach(() => {
  vi.resetModules();
  window.sessionStorage.clear();
});

async function harness() {
  return (await import("../../../test/harness")).renderScreen;
}

describe("4C provider records", () => {
  it("parses student:reference:state:hours lines", () => {
    expect(parseProviderRecords("S1:REF-1:done:30; S2::open\nS3")).toEqual([
      { studentNumber: "S1", providerReference: "REF-1", completed: true, hoursCompleted: 30 },
      { studentNumber: "S2", providerReference: null, completed: false, hoursCompleted: null },
      { studentNumber: "S3", providerReference: null, completed: false, hoursCompleted: null },
    ]);
  });
});

describe("4C screens (mock API)", () => {
  it("shows the student Library and lets the student request an opportunity", async () => {
    const renderScreen = await harness();
    const { StudentLibraryScreen } = await import("./index");
    await renderScreen(<StudentLibraryScreen />, { role: "student", path: "/library" });
    expect(await screen.findByText(/Course reading list/)).toBeTruthy();
    expect(document.querySelector("[data-slice='4C']")).toBeTruthy();
    const btn = await waitFor(() => {
      const b = document.querySelector<HTMLButtonElement>("[data-action='request-opportunity']");
      if (!b) throw new Error("no request button");
      return b;
    });
    fireEvent.click(btn);
    await waitFor(() => expect(document.querySelector("[data-my-enrolment]")).toBeTruthy());
  });

  it("shows the faculty Resources of the taught section", async () => {
    const renderScreen = await harness();
    const { FacultyResourcesScreen } = await import("./index");
    await renderScreen(<FacultyResourcesScreen />, { role: "faculty", path: "/resources" });
    expect(await screen.findByText("Course reading list")).toBeTruthy();
    expect(document.querySelector("[data-new-resource]")).toBeTruthy();
  });

  it("renders the admin tabs for an institution", async () => {
    const renderScreen = await harness();
    const { FlexibleLearningAdminTab, CreditRecognitionAdminTab } = await import("./index");
    const institution = { id: "00000000-0000-7000-8000-00000000c400", name: "Mock University" } as never;
    await renderScreen(
      <>
        <FlexibleLearningAdminTab institution={institution} />
        <CreditRecognitionAdminTab institution={institution} />
      </>,
    );
    expect(await screen.findByText(/Machine Learning \(NPTEL\)/)).toBeTruthy();
    expect(await screen.findByText(/Version 1 in force/)).toBeTruthy();
    expect(screen.getByText("Credit mapping rules")).toBeTruthy();
  });

  it("asks for an institution first", async () => {
    const renderScreen = await harness();
    const { FlexibleLearningAdminTab } = await import("./index");
    await renderScreen(<FlexibleLearningAdminTab institution={null} />);
    expect(await screen.findByText("No institution selected")).toBeTruthy();
  });
});
