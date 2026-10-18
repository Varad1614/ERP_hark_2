import { describe, expect, it } from "vitest";
import { createHttpTimeApi } from "./api";
import { createMockTimeApi } from "./mock";
import { addDaysIso, budgetBar, clock, human, monthLabel, parseDuration, shiftMonth, weekStartOf } from "./timetrack";

describe("time tracking helpers", () => {
  it("parses durations the way the server does", () => {
    expect(parseDuration("1h30")).toBe(5400);
    expect(parseDuration("1.5")).toBe(5400);
    expect(parseDuration("90m")).toBe(5400);
    expect(parseDuration("1:30")).toBe(5400);
    expect(parseDuration(":45")).toBe(2700);
    expect(parseDuration("45")).toBe(2700); // bare integers above 24 are minutes
    expect(parseDuration("8")).toBe(28800);
    expect(parseDuration(",5")).toBe(1800);
    expect(parseDuration("1:99")).toBeNull();
    expect(parseDuration("abc")).toBeNull();
  });

  it("words durations, weeks and months", () => {
    expect(clock(5400)).toBe("1:30");
    expect(human(3599)).toBe("1h");
    expect(human(2700)).toBe("45m");
    expect(weekStartOf("2026-10-18", 1)).toBe("2026-10-12");
    expect(weekStartOf("2026-10-18", 0)).toBe("2026-10-18");
    expect(addDaysIso("2026-12-31", 1)).toBe("2027-01-01");
    expect(shiftMonth("2026-01", -1)).toBe("2025-12");
    expect(monthLabel("2026-09")).toMatch(/Sep/);
    expect(budgetBar(0.85)).toEqual({ pct: 85, kind: "review" });
    expect(budgetBar(1.2).kind).toBe("attention");
  });
});

describe("mock time api", () => {
  it("logs, refuses overlaps, locks a submitted week and approves it", async () => {
    const api = createMockTimeApi();
    const week = await api.myWeek("2026-09-09");
    expect(week.weekStart).toBe("2026-09-07");
    const serb = week.projects.find((p) => p.code.startsWith("SERB"))!;
    const iqac = week.projects.find((p) => p.code === "IQAC")!;
    const e = await api.addEntry({ projectId: serb.projectId, date: "2026-09-07", start: "09:00", end: "10:30" });
    expect(e.seconds).toBe(5400);
    await expect(api.addEntry({ projectId: iqac.projectId, date: "2026-09-07", start: "10:00", end: "11:00" })).rejects.toThrow(/overlaps/);
    const c = await api.addEntry({ projectId: iqac.projectId, date: "2026-09-07", duration: "1h" });
    expect(c.billable).toBe(false);
    const sheet = await api.submitWeek(serb.projectId, "2026-09-10");
    expect(sheet.status).toBe("submitted");
    await expect(api.addEntry({ projectId: serb.projectId, date: "2026-09-08", duration: "1" })).rejects.toThrow(/submitted/);
    const after = await api.myWeek("2026-09-07");
    expect(after.entries.find((x) => x.id === e.id)!.locked).toBe(true);
    expect((await api.decide(sheet.id, sheet.version, true)).status).toBe("approved");
    const report = await api.effort("i", "2026-09", "2026-09");
    const row = report.rows.find((r) => r.projectId === serb.projectId)!;
    expect(row.percent).toBe(60); // 1.5 of 2.5 hours
  });

  it("runs one timer at a time", async () => {
    const api = createMockTimeApi();
    const { projects } = await api.myWeek();
    await api.startTimer(projects[0]!.projectId);
    await expect(api.startTimer(projects[0]!.projectId)).rejects.toThrow(/already running/);
    const e = await api.stopTimer();
    expect(e.seconds).toBe(60);
  });
});

describe("http time api", () => {
  it("sends If-Match and the decision body", async () => {
    const calls: { url: string; init: RequestInit }[] = [];
    const api = createHttpTimeApi(async (url, init) => {
      calls.push({ url, init });
      return new Response(JSON.stringify({ id: "ts-1", status: "approved" }), { status: 200 });
    }, "");
    await api.decide("ts-1", 3, false, "Fix Tuesday");
    expect(calls[0]!.url).toBe("/api/v1/people/timesheets/ts-1/decision");
    expect((calls[0]!.init.headers as Record<string, string>)["If-Match"]).toBe('"3"');
    expect(JSON.parse(String(calls[0]!.init.body))).toEqual({ decision: "reject", reason: "Fix Tuesday" });
    expect(api.effortCsvUrl("i-1", "2026-04", "2027-03")).toBe("/api/v1/people/institutions/i-1/effort-report/csv?from=2026-04&to=2027-03");
  });
});
