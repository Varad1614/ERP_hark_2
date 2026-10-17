import { describe, expect, it } from "vitest";
import { dueIn, dueLine, meterIn, wholeOrNull } from "./fleet";
import { createMockFleetApi } from "./mock";

describe("fleet helpers", () => {
  it("words due dates and meters", () => {
    expect(dueIn(0)).toBe("today");
    expect(dueIn(1)).toBe("in 1 day");
    expect(dueIn(-3)).toBe("3 days late");
    expect(meterIn(1500)).toBe("1,500 km to go");
    expect(meterIn(-120, "hr")).toBe("120 hr past");
    expect(dueLine({ nextDate: "2027-01-01", daysLeft: 5, meterLeft: null })).toBe("2027-01-01 (in 5 days)");
    expect(wholeOrNull("6")).toBe(6);
    expect(wholeOrNull("0")).toBeNull();
    expect(wholeOrNull("")).toBeNull();
  });
});

describe("mock fleet api", () => {
  it("keeps a board, completes reminders and renewals", async () => {
    const api = createMockFleetApi();
    const board = await api.board("i");
    expect(board.overdue).toBe(1);
    expect(board.dueSoon).toBe(1);
    await expect(api.addReminder("veh-1", { task: "Nothing" })).rejects.toThrow(/repeat/);
    const file = await api.addService("veh-1", { description: "Oil", amount: "3450", meter: "20500", reminderIds: ["rem-1"] });
    expect(file.reminders[0]!.history[0]!.onTime).toBe(false);
    expect(file.meter).toBe(20500);
    const ren = await api.completeRenewal("ren-1", 1, { cost: "12450" });
    expect(ren.history).toHaveLength(1);
    expect((await api.addFuel("veh-1", { volume: "60", total: "6000" })).warnings).toHaveLength(1);
  });
});
