import { describe, expect, it } from "vitest";
import { periodFor } from "./billing";
import { createMockChargesApi } from "./mock";

describe("billing periods (mirror of charges::billing::period_for)", () => {
  it("follows the April financial year for quarters, halves and years", () => {
    expect(periodFor("quarterly", "2027-02-10")).toEqual({ key: "quarterly:2027-01", start: "2027-01-01", end: "2027-03-31", label: "Q4 2026-27 (Jan–Mar)" });
    expect(periodFor("half_yearly", "2026-10-17").label).toBe("H2 2026-27 (Oct–Mar)");
    expect(periodFor("annual", "2026-03-31")).toMatchObject({ start: "2025-04-01", end: "2026-03-31", label: "FY 2025-26" });
  });
  it("starts weeks on Monday and months on the 1st", () => {
    expect(periodFor("weekly", "2026-10-17")).toMatchObject({ start: "2026-10-12", end: "2026-10-18", label: "Week of 12 Oct 2026" });
    expect(periodFor("monthly", "2028-02-17")).toMatchObject({ end: "2028-02-29", label: "February 2028" });
  });
});

describe("mock billing run", () => {
  it("bills each enrolled student once per period", async () => {
    const api = createMockChargesApi();
    const [head] = await api.feeHeads("inst");
    const plan = await api.createBillingPlan("inst", { code: "HOSTEL", name: "Hostel", feeHeadId: head!.id, amount: "4500", frequency: "monthly" });
    await api.enrol(plan.id, { studentId: "s1" });
    const preview = await api.billingRun("inst", { frequency: "monthly", day: "2026-11-05" }, true);
    expect(preview).toMatchObject({ preview: true, billed: 1, total: "4500.00" });
    expect(preview.items[0]!.dueDate).toBe("2026-11-12");
    await api.billingRun("inst", { frequency: "monthly", day: "2026-11-05" }, false);
    const again = await api.billingRun("inst", { frequency: "monthly", day: "2026-11-20" }, false);
    expect(again.billed).toBe(0);
    expect(again.skips[0]!.reason).toBe("already_billed");
    expect(await api.billingRuns("inst")).toHaveLength(2);
  });
});
