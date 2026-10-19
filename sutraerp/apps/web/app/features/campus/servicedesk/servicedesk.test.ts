import { describe, expect, it } from "vitest";
import { describeAction, describeCondition, shortDuration, targetProblem, toHours, toMinutes } from "./desk";
import { createMockDeskApi, sampleRow } from "./mock";

describe("service desk helpers", () => {
  it("words conditions, actions and durations", () => {
    expect(describeCondition({ field: "subject", op: "contains", value: "leak" })).toBe("Title contains Leak");
    expect(describeCondition({ field: "assignee", op: "changed", value: null })).toBe("Assignee changed");
    expect(describeAction({ field: "priority", value: "critical" })).toBe("Set priority Critical");
    expect(shortDuration(-90 * 60_000)).toBe("-2h");
    expect(shortDuration(500)).toBe("1s");
    expect(shortDuration(20 * 86_400_000)).toBe("3w");
  });
  it("converts SLA hours and checks a target row", () => {
    expect(toMinutes("1.5")).toBe(90);
    expect(toHours(4320)).toBe("72");
    expect(targetProblem({ first: 0, next: 0, resolve: 0 })).toMatch(/resolution/);
    expect(targetProblem({ first: 300, next: 0, resolve: 240 })).toMatch(/before/);
    expect(targetProblem({ first: 60, next: 0, resolve: 240 })).toBeNull();
  });
});

describe("mock desk api", () => {
  it("keeps rules, queues, tags and the SLA policy", async () => {
    const api = createMockDeskApi([sampleRow(1, { title: "Water leak" }, -1), sampleRow(2, { title: "Fan" }, 5)]);
    await expect(api.createRule("i", { title: "x", actions: [{ field: "status", value: "resolved" }] })).rejects.toThrow(/status/);
    const r = await api.createRule("i", { title: "Leaks", conditions: { all: [{ field: "subject", op: "contains", value: "leak" }], any: [] }, actions: [{ field: "priority", value: "critical" }] });
    expect(r.event).toBe("any");
    expect((await api.preview("i", { conditions: r.conditions, actions: r.actions })).matched).toBe(1);
    const off = await api.updateRule(r.id, r.version, { title: r.title, active: false, actions: r.actions });
    expect(off.active).toBe(false);
    const board = await api.board("i");
    expect(board.items.map((x) => x.ticket.number)).toEqual(["SR-000001", "SR-000002"]);
    expect(board.breached).toBe(1);
    await api.tags("t-2", 1, ["VIP"], []);
    const v = await api.createView("i", { title: "VIP", conditions: { all: [{ field: "tags", op: "includes", value: "vip" }], any: [] } });
    expect((await api.viewTickets("i", v.id)).items).toHaveLength(1);
    const p = await api.policy("i");
    expect(p.targets.critical).toEqual({ first: 60, next: 0, resolve: 240 });
    const saved = await api.savePolicy("i", { enabled: true, targets: { critical: { first: 30, next: 0, resolve: 120 } }, version: 0 });
    expect([saved.customised, saved.targets.critical!.resolve, saved.targets.low!.resolve]).toEqual([true, 120, 10080]);
  });
});
