import { describe, expect, it } from "vitest";
import { aqarYear, aqarYears, clockText, dueText, phaseKind, totalsText } from "./contracts";
import { createMockContractsApi, sampleContract } from "./mock";

describe("contract helpers", () => {
  it("words the clock and obligations", () => {
    const c = sampleContract(1, { autoRenew: true, renewal: { phase: "notice", date: "2026-11-06", daysLeft: 20, noticeDate: "2026-10-07", noticeDaysLeft: -10, decision: null, handled: false }, attention: true });
    expect(clockText(c)).toBe("Renews in 20 days");
    expect(phaseKind(c)).toBe("attention");
    const n = sampleContract(2, { renewal: { phase: "soon", date: "2026-11-06", daysLeft: 20, noticeDate: "2026-11-01", noticeDaysLeft: 15, decision: null, handled: false } });
    expect(clockText(n)).toBe("Ends in 20 days · notice by 1 Nov 2026");
    expect(clockText(sampleContract(3, { status: "draft", renewal: { ...n.renewal, phase: "none" } }))).toBe("Draft");
    expect(dueText({ done: false, daysLeft: -1, repeat: null, completed: 0 })).toBe("Overdue 1 day");
    expect(dueText({ done: true, daysLeft: 0, repeat: { every: "monthly", anchor: "2026-01-31", until: null, count: 3, contractEnd: false }, completed: 3 })).toBe("Finished (3 done)");
  });

  it("knows AQAR years and never sums currencies", () => {
    expect(aqarYear("2026-10-17")).toBe("2026-27");
    expect(aqarYear("2026-06-30")).toBe("2025-26");
    expect(aqarYears("2026-10-17", 2)).toEqual(["2026-27", "2025-26"]);
    expect(totalsText({ INR: 54000, USD: 1200 }, (v) => `₹${v}`)).toBe("₹54000 · USD 1,200");
    expect(totalsText({}, String)).toBe("—");
  });
});

describe("mock contracts api", () => {
  it("flags the notice window, records a decision and moves obligations on", async () => {
    const api = createMockContractsApi("2026-10-17");
    const dash = await api.dashboard("i");
    expect(dash.attention.map((c) => c.id)).toEqual(["con-1"]);
    await api.decide("con-1", { kind: "renewing" });
    expect((await api.list("i", { attention: true }))).toHaveLength(0);
    const ob = await api.complete("ob-1");
    expect(ob.dueOn).toBe("2026-11-14");
    expect((await api.undo("ob-1")).completed).toBe(0);
    await api.addActivity("con-2", { heldOn: "2026-09-01", kind: "internship", title: "Shop-floor internship", students: 12 });
    const ev = await api.evidence("i");
    expect(ev.year).toBe("2026-27");
    expect(ev.totals.functional).toBe(1);
    await expect(api.addActivity("con-2", { heldOn: "2027-01-01", kind: "workshop", title: "Later" })).rejects.toThrow(/future/);
    expect((await api.list("i", { kind: "mou" })).map((c) => c.id)).toEqual(["con-2"]);
  });
});
