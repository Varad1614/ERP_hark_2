import { describe, expect, it } from "vitest";
import { attention, differs, settingsProblem, urgency } from "./demand";
import { createMockDemandApi, sampleDemandRow } from "./mock";

describe("stock demand helpers", () => {
  it("orders and words rows", () => {
    expect(urgency({ status: "out", coverDays: 0 })).toBe(-1);
    expect(urgency({ status: "ok", coverDays: null })).toBe(Number.POSITIVE_INFINITY);
    expect(attention(sampleDemandRow(1, { runningOut: true }))).toMatch(/5 days/);
    expect(attention(sampleDemandRow(1))).toBe("");
    expect(differs({ reorderLevel: "6.000", suggestedReorderLevel: "6" })).toBe(false);
    expect(differs({ reorderLevel: null, suggestedReorderLevel: "6" })).toBe(true);
    expect(settingsProblem({ demandDays: 3, safetyDays: 3, coverageDays: 0, expiryDays: 30 })).toMatch(/7 to 365/);
    expect(settingsProblem({ demandDays: 30, safetyDays: 3, coverageDays: 0, expiryDays: 30 })).toBeNull();
  });
});

describe("mock demand api", () => {
  it("lists by urgency, applies suggestions and keeps planning", async () => {
    const api = createMockDemandApi();
    const all = await api.demand("i");
    expect(all.items[0]!.status).toBe("out");
    expect((await api.demand("i", { attention: true })).items).toHaveLength(1);
    const r = await api.applyThresholds("i", ["item-1"]);
    expect(r.updatedIds).toEqual(["item-1"]);
    await api.savePlanning("item-1", { leadTimeDays: 4, packSize: "6", safetyDays: null, trackExpiry: true });
    expect((await api.lots("item-1")).trackExpiry).toBe(true);
    await expect(api.saveSettings("i", { demandDays: 2, safetyDays: 3, coverageDays: 0, expiryDays: 30 })).rejects.toThrow(/7 to 365/);
  });
});
