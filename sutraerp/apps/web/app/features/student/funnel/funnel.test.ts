import { describe, expect, it } from "vitest";
import { barWidths, nextStage, pct, rupees, sourceLabel, stageKind } from "./funnel";
import { createMockFunnelApi } from "./mock";

describe("funnel helpers", () => {
  it("words rates, rupees and sources", () => {
    expect(pct(0.375)).toBe("38%");
    expect(pct(null)).toBe("—");
    expect(rupees("1250000.00")).toBe("₹12,50,000");
    expect(sourceLabel("education_fair")).toBe("Education fair");
    expect(stageKind("counselling", true)).toBe("attention");
  });
  it("scales bars to the widest stage and finds the next stage", () => {
    const row = (entered: number) => ({ stage: "s", label: "S", gone: false, entered, advanced: 0, lostHere: 0, open: 0 });
    expect(barWidths([row(40), row(10), row(0)])).toEqual([100, 25, 0]);
    const stages = [
      { id: "enquiry", label: "Enquiry", probability: 0.1 },
      { id: "applied", label: "Applied", probability: 0.6 },
      { id: "admitted", label: "Admitted", probability: 1 },
    ];
    expect(nextStage(stages, "enquiry")?.id).toBe("applied");
    expect(nextStage(stages, "applied")).toBeNull();
  });
});

describe("mock funnel api", () => {
  it("records enquiries, flags duplicates and needs a reason to lose one", async () => {
    const api = createMockFunnelApi();
    const a = await api.createEnquiry("inst", { givenName: "Aarav", familyName: "Kulkarni", email: "aarav@example.in" });
    expect(a.enquiry.stage).toBe("enquiry");
    const b = await api.createEnquiry("inst", { givenName: "Aarav", familyName: "Kulkarni", email: "AARAV@example.in " });
    expect(b.possibleDuplicates[0]!.reasons).toEqual(["email", "name"]);
    await expect(api.updateEnquiry(b.enquiry.id, b.enquiry.version, { stage: "lost" })).rejects.toThrow(/reason/);
    await api.updateEnquiry(b.enquiry.id, b.enquiry.version, { stage: "lost", closeReason: "Duplicate enquiry" });
    const moved = await api.updateEnquiry(a.enquiry.id, a.enquiry.version, { stage: "counselling" });
    expect(moved.stageHistory).toHaveLength(2);
    const f = await api.funnel("inst");
    expect(f.report.totals).toMatchObject({ enquiries: 2, lost: 1 });
    expect(f.report.rows[0]).toMatchObject({ stage: "enquiry", entered: 2, advanced: 1, lostHere: 1 });
  });
});
