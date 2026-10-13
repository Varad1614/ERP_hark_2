/** Batch 5 foundation: Finance frames, role gating, slice placeholders and the INR formatter. */
import { screen } from "@testing-library/react";
import type { ComponentType } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { ROLE_CONFIG, SCREEN_BATCH, type Role } from "../../lib/roles";
import { activeTab, FINANCE_SCREENS, FINANCE_TABS, financePath, formatInr } from "./index";

beforeEach(() => {
  vi.resetModules();
  window.sessionStorage.clear();
});

async function harness() {
  return (await import("../../test/harness")).renderScreen;
}

/** Placeholder of `slice`, or the slice's real screen once it lands (`data-slice`). */
const sliceMarker = (slice: string) => document.querySelector(`[data-placeholder='${slice}'], [data-slice='${slice}']`);

describe("Batch 5 tabs, screens and money formatting", () => {
  it("defaults to charges, resolves known keys and keeps keys unique", () => {
    expect(activeTab(FINANCE_TABS, undefined).key).toBe("charges");
    expect(activeTab(FINANCE_TABS, "treasury").key).toBe("treasury");
    expect(activeTab(FINANCE_TABS, "bogus").key).toBe("charges");
    expect(new Set(FINANCE_TABS.map((t) => t.key)).size).toBe(FINANCE_TABS.length);
    expect(financePath("refunds")).toBe("/finance/refunds");
    expect(financePath()).toBe("/finance");
    for (const key of Object.keys(FINANCE_SCREENS)) expect(SCREEN_BATCH[key]?.batch, key).toBe(5);
  });

  it("links Finance for administrators and Fees for students", () => {
    expect(ROLE_CONFIG.administrator.nav.map((n) => n.id)).toContain("finance");
    expect(ROLE_CONFIG.student.nav.map((n) => n.id)).toContain("fees");
    expect(ROLE_CONFIG.faculty.nav.map((n) => n.id)).not.toContain("finance");
  });

  it("formats decimal strings in Indian grouping without floats", () => {
    expect(formatInr("125000.00")).toBe("₹1,25,000.00");
    expect(formatInr("12345678.9")).toBe("₹1,23,45,678.90");
    expect(formatInr("999")).toBe("₹999.00");
    expect(formatInr("-50.5")).toBe("-₹50.50");
    expect(formatInr("0.10")).toBe("₹0.10");
    expect(formatInr(null)).toBe("—");
  });
});

describe("Batch 5 route frames (foundation placeholders)", () => {
  it("renders Admin → Finance with a tab per slice", async () => {
    const renderScreen = await harness();
    const { default: Finance } = await import("../../routes/admin/finance");
    await renderScreen(<Finance />, { path: "/finance/:tab?", entry: "/finance/ledger" });
    expect(await screen.findByRole("link", { name: "Bank & treasury" })).toBeTruthy();
    expect(screen.getByRole("link", { name: "Refunds" }).getAttribute("href")).toBe("/finance/refunds");
    expect(sliceMarker("5C")).toBeTruthy();
  });

  it.each([
    ["charges", "5A"],
    ["aid", "5A"],
    ["collections", "5B"],
    ["receivables", "5B"],
    ["periods", "5C"],
  ])("renders the %s tab of slice %s", async (tab, slice) => {
    const renderScreen = await harness();
    const { default: Finance } = await import("../../routes/admin/finance");
    await renderScreen(<Finance />, { path: "/finance/:tab?", entry: `/finance/${tab}` });
    await screen.findByRole("link", { name: "Ledger" });
    expect(sliceMarker(slice)).toBeTruthy();
  });

  it("hides Admin → Finance from students and faculty", async () => {
    const renderScreen = await harness();
    const { default: Finance } = await import("../../routes/admin/finance");
    for (const role of ["student", "faculty"] as Role[]) {
      await renderScreen(<Finance />, { path: "/finance/:tab?", entry: "/finance", role });
      expect((await screen.findAllByText("This screen isn’t available")).length).toBeGreaterThan(0);
    }
  });

  type Mod = () => Promise<{ default: ComponentType }>;
  it("renders Student Fees with the 5A and 5B panels", async () => {
    const renderScreen = await harness();
    const load: Mod = () => import("../../routes/student/fees");
    const { default: Fees } = await load();
    await renderScreen(<Fees />, { path: "/fees", role: "student" });
    expect(document.querySelector("[data-screen='fees']")).toBeTruthy();
    expect(sliceMarker("5A")).toBeTruthy();
    expect(sliceMarker("5B")).toBeTruthy();
  });

  it("tells staff that Fees is the students’ view", async () => {
    const renderScreen = await harness();
    const { default: Fees } = await import("../../routes/student/fees");
    await renderScreen(<Fees />, { path: "/fees", role: "administrator" });
    expect(await screen.findByText("This screen isn’t available")).toBeTruthy();
  });
});
