import { describe, expect, it } from "vitest";
import { myRosterIcsUrl, payrollExportCsvUrl, queryString, reportsCsvUrl, timesheetCsvUrl } from "./shifts-api";
import { duration, swapActions, swapStatusLabel, sumTotals, timesheetActions, timesheetStatusLabel, totalsSummary } from "./shifts";
import type { TimesheetTotals } from "./shifts-types";

const totals = (p: Partial<TimesheetTotals> = {}): TimesheetTotals => ({
  shifts: 0,
  scheduledMinutes: 0,
  workedMinutes: 0,
  overtimeMinutes: 0,
  lateCount: 0,
  lateMinutes: 0,
  earlyLeaveCount: 0,
  earlyLeaveMinutes: 0,
  missedPunches: 0,
  notRecorded: 0,
  absences: 0,
  lopDays: 0,
  nightShifts: 0,
  holidayShifts: 0,
  ...p,
});

describe("duration", () => {
  it("words minutes", () => {
    expect(duration(450)).toBe("7h 30m");
    expect(duration(480)).toBe("8h");
    expect(duration(45)).toBe("45m");
    expect(duration(0)).toBe("0m");
    expect(duration(89.6)).toBe("1h 30m");
    expect(duration(-90)).toBe("-1h 30m");
    expect(duration(null)).toBe("—");
  });
});

describe("status labels", () => {
  it("labels known statuses and passes unknown through", () => {
    expect(swapStatusLabel("awaiting_peer")).toBe("Awaiting colleague");
    expect(swapStatusLabel("cancelled")).toBe("Withdrawn");
    expect(timesheetStatusLabel("returned")).toBe("Returned");
    expect(timesheetStatusLabel("weird")).toBe("weird");
  });
});

describe("swap actions", () => {
  const give = { kind: "give_away" as const, status: "awaiting_peer" as const, fromStaffId: "a", toStaffId: "b" };
  it("lets the colleague respond and the requester withdraw", () => {
    expect(swapActions(give, { staffId: "b" })).toEqual(["respond"]);
    expect(swapActions(give, { staffId: "a" })).toEqual(["cancel"]);
    expect(swapActions(give, { staffId: "c" })).toEqual([]);
  });
  it("lets anyone but the requester take an open cover", () => {
    expect(swapActions({ ...give, kind: "cover", toStaffId: null }, { staffId: "c" })).toEqual(["respond"]);
  });
  it("lets an uninvolved manager decide an agreed request", () => {
    const agreed = { ...give, status: "awaiting_manager" as const };
    expect(swapActions(agreed, { staffId: "m", manager: true })).toEqual(["decide"]);
    expect(swapActions(agreed, { staffId: "b", manager: true })).toEqual([]);
    expect(swapActions(agreed, { staffId: "a", manager: true })).toEqual(["cancel"]);
  });
  it("treats the claimant as requester of a claim", () => {
    const claim = { kind: "claim" as const, status: "awaiting_manager" as const, fromStaffId: null, toStaffId: "c" };
    expect(swapActions(claim, { staffId: "c" })).toEqual(["cancel"]);
    expect(swapActions(claim, { staffId: "m", manager: true })).toEqual(["decide"]);
  });
  it("allows nothing once resolved", () => {
    expect(swapActions({ ...give, status: "approved" }, { staffId: "a", manager: true })).toEqual([]);
  });
});

describe("timesheet actions", () => {
  it("follows the status", () => {
    expect(timesheetActions(null, { own: true })).toEqual(["submit"]);
    expect(timesheetActions("returned", { own: true })).toEqual(["submit"]);
    expect(timesheetActions("submitted", { own: true, manager: true })).toEqual([]);
    expect(timesheetActions("submitted", { manager: true })).toEqual(["decide"]);
    expect(timesheetActions("approved", { manager: true, payroll: true })).toEqual(["lock", "export"]);
    expect(timesheetActions("locked", { payroll: true, exported: true })).toEqual([]);
  });
});

describe("totals", () => {
  it("summarises and sums", () => {
    const a = totals({ shifts: 5, workedMinutes: 2310, scheduledMinutes: 2400, overtimeMinutes: 120, lateCount: 1, missedPunches: 1 });
    expect(totalsSummary(a)).toBe("5 shifts · 38h 30m worked of 40h · 2h overtime · 1 late · 1 missed punch");
    expect(totalsSummary(totals({ shifts: 1, workedMinutes: 480, scheduledMinutes: 480, lopDays: 0.5 }))).toBe("1 shift · 8h worked of 8h · 0.5 LOP days");
    const s = sumTotals([a, totals({ shifts: 1, workedMinutes: 30, nightShifts: 1 })]);
    expect(s.shifts).toBe(6);
    expect(s.workedMinutes).toBe(2340);
    expect(s.nightShifts).toBe(1);
  });
});

describe("download urls", () => {
  it("builds the CSV and calendar urls", () => {
    expect(queryString({ a: "1", b: undefined, c: "" })).toBe("?a=1");
    expect(payrollExportCsvUrl("i 1", { from: "2026-10-01", to: "2026-10-31" }, "")).toBe("/api/v1/people/institutions/i%201/roster/payroll-export.csv?from=2026-10-01&to=2026-10-31");
    expect(timesheetCsvUrl("i", {}, "")).toBe("/api/v1/people/institutions/i/roster/timesheet.csv");
    expect(reportsCsvUrl("i", { from: "2026-10-12", section: "hours" }, "")).toBe("/api/v1/people/institutions/i/roster/reports.csv?from=2026-10-12&section=hours");
    expect(myRosterIcsUrl({}, "https://x")).toBe("https://x/api/v1/people/me/roster.ics");
  });
});
