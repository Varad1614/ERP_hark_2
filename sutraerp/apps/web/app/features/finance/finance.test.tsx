/** Batch 5 foundation: Finance frames, role gating, slice placeholders and the INR formatter. */
import { screen } from "@testing-library/react";
import type { ComponentType } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { ROLE_CONFIG, SCREEN_BATCH, type Role } from "../../lib/roles";
import { activeTab, FINANCE_SCREENS, FINANCE_TABS, financePath, formatInr } from "./index";
import { compact, invoiceState, ledger, money, rate } from "./ui";
import type { Institution } from "../institution/types";

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
  it("defaults to the overview, resolves known keys and keeps keys unique", () => {
    expect(activeTab(FINANCE_TABS, undefined).key).toBe("overview");
    expect(activeTab(FINANCE_TABS, "treasury").key).toBe("treasury");
    expect(activeTab(FINANCE_TABS, "bogus").key).toBe("overview");
    expect(FINANCE_TABS.slice(0, 4).map((t) => t.label)).toEqual(["Overview", "Collections", "Receivables", "Ledger"]);
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

  it("formats finance money through the R0 formatINR (no needless .00, paise on ledgers)", () => {
    expect(money("125000.00")).toBe("₹1,25,000");
    expect(money("46250.50")).toBe("₹46,250.50");
    expect(ledger("125000")).toBe("₹1,25,000.00");
    expect(compact("34000000.00")).toBe("₹3.4 Cr");
    expect(rate(82n, 100n)).toBe("82%");
  });

  it("classifies invoices as paid, pending or overdue", () => {
    const c = (outstandingAmount: string, dueDate: string | null, status = "published") => ({ status, outstandingAmount, dueDate });
    expect(invoiceState(c("0.00", "2026-07-15"), "2026-10-15")).toBe("paid");
    expect(invoiceState(c("100.00", "2026-07-15"), "2026-10-15")).toBe("overdue");
    expect(invoiceState(c("100.00", "2026-12-15"), "2026-10-15")).toBe("pending");
    expect(invoiceState(c("100.00", null, "draft"), "2026-10-15")).toBe("other");
  });

  it("keeps the legacy two-decimal formatter for the areas that still import it", () => {
    expect(formatInr("125000.00")).toBe("₹1,25,000.00");
    expect(formatInr("12345678.9")).toBe("₹1,23,45,678.90");
    expect(formatInr("999")).toBe("₹999.00");
    expect(formatInr("-50.5")).toBe("-₹50.50");
    expect(formatInr("0.10")).toBe("₹0.10");
    expect(formatInr(null)).toBe("—");
  });
});

describe("Batch 5 route frames (foundation placeholders)", () => {
  it("renders Admin → Finance with four header tabs and the rest under More", async () => {
    const renderScreen = await harness();
    const { default: Finance } = await import("../../routes/admin/finance");
    await renderScreen(<Finance />, { path: "/finance/:tab?", entry: "/finance/ledger" });
    expect(await screen.findByRole("link", { name: "Ledger" })).toBeTruthy();
    expect(screen.getByRole("link", { name: "Collections" }).getAttribute("href")).toBe("/finance/collections");
    expect(screen.getByRole("link", { name: "Overview" }).getAttribute("href")).toBe("/finance");
    expect(screen.queryByRole("link", { name: "Refunds" })).toBeNull();
    expect(document.querySelector("[data-tab-more]")).toBeTruthy();
    expect(sliceMarker("5C")).toBeTruthy();
  });

  it("opens on the overview: money KPIs, monthly collections, adjustments and invoices", async () => {
    const renderScreen = await harness();
    const { FinanceOverview } = await import("./overview");
    const institution = { id: "inst-1", name: "Sahyadri Institute of Technology" } as unknown as Institution;
    await renderScreen(<FinanceOverview institution={institution} />, { path: "/finance" });
    expect(await screen.findByText("Collection rate")).toBeTruthy();
    expect(document.querySelector("[data-kpi='collected']")).toBeTruthy();
    expect(document.querySelector("[data-collections-chart]")).toBeTruthy();
    expect(document.querySelector("[data-fee-adjustments]")).toBeTruthy();
    expect(document.querySelector("[data-invoices]")).toBeTruthy();
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
