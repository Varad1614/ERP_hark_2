import { describe, expect, it } from "vitest";
import { createMockRosterApi } from "./mock";
import { addDaysIso, flags, hours, mondayOf, timeRange } from "./roster";

describe("roster helpers", () => {
  it("words hours, ranges and weeks", () => {
    expect(hours(450)).toBe("7.5h");
    expect(hours(480)).toBe("8h");
    expect(timeRange({ start: "22:00", end: "06:00", overnight: true })).toBe("22:00–06:00 +1");
    expect(mondayOf("2026-10-18")).toBe("2026-10-12");
    expect(mondayOf("2026-10-12")).toBe("2026-10-12");
    expect(addDaysIso("2026-10-31", 1)).toBe("2026-11-01");
  });
});

describe("mock roster api", () => {
  it("refuses overlaps across midnight, fills and publishes", async () => {
    const api = createMockRosterApi();
    const night = await api.createShift("i", { date: "2027-03-01", start: "22:00", end: "06:00", role: "Security", staffId: "st-1" });
    expect(night.shift.netMinutes).toBe(480);
    await expect(api.createShift("i", { date: "2027-03-02", start: "05:00", end: "09:00", role: "Security", staffId: "st-1" })).rejects.toThrow(/overlapping/);
    await api.createShift("i", { date: "2027-03-02", start: "05:00", end: "09:00", role: "Security" });
    const plan = await api.fill("i", "2027-03-01", "2027-03-07", true);
    expect(plan.applied).toBe(1);
    expect((await api.publish("i", "2027-03-01", "2027-03-07")).published).toBe(2);
    const board = await api.board("i", "2027-03-01", "2027-03-07");
    expect(board.openShifts).toBe(0);
    expect(flags(board.shifts[0]!)).toEqual([]);
  });
});
