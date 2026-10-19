import { describe, expect, it } from "vitest";
import { agendaFrom, isoDay } from "./live";
import { WORKSPACE_SCREENS, workspaceBatch, workspaceScreen } from "../../lib/workspace-screens";
import { CATALOG } from "../../lib/api";
import type { TimetableSlot } from "../academic/delivery/types";

const slot = (id: string, day: number, from: string, to: string): TimetableSlot => ({
  id, timetableId: "t", sectionId: "s", offeringId: "o", offeringCode: "CS101-T1", title: id, sectionCode: "A", dayOfWeek: day, startsAt: from, endsAt: to, roomLabel: "R1", facultyNames: [],
});

describe("home agenda", () => {
  // Tuesday 13 Oct 2026, 10:15
  const now = new Date(2026, 9, 13, 10, 15);
  it("uses ISO weekdays", () => {
    expect(isoDay(now)).toBe(2);
    expect(isoDay(new Date(2026, 9, 18))).toBe(7);
  });
  it("picks the class in progress, then the next ones of the week", () => {
    const a = agendaFrom([slot("late", 2, "14:00", "15:00"), slot("now", 2, "10:00", "11:00"), slot("past", 2, "08:00", "09:00"), slot("mon", 1, "09:00", "10:00"), slot("wed", 3, "09:00", "10:00")], now);
    expect(a.current?.id).toBe("now");
    expect(a.live).toBe(true);
    expect(a.later.map((l) => l.slot.id)).toEqual(["late", "wed"]);
  });
  it("shows the next class when nothing is running and wraps the week", () => {
    const a = agendaFrom([slot("mon", 1, "09:00", "10:00")], now);
    expect(a.current?.id).toBe("mon");
    expect(a.live).toBe(false);
    expect(agendaFrom([], now).current).toBeNull();
  });
});

describe("workspace directory", () => {
  it("maps only catalogue keys, and batches by domain", () => {
    const keys = new Set(CATALOG.map((w) => w.key));
    for (const k of Object.keys(WORKSPACE_SCREENS)) expect(keys.has(k), k).toBe(true);
    expect(workspaceBatch("W-I05")).toBe(6);
    expect(workspaceBatch("W-E05")).toBe(7);
    expect(workspaceBatch("W-H08")).toBe(8);
    expect(workspaceBatch("W-H09")).toBe(9);
    expect(workspaceBatch("W-G05")).toBe(8);
    expect(workspaceScreen("W-F01", "student")).toBe("/fees");
    expect(workspaceScreen("W-F05", "student")).toBeNull();
    expect(workspaceScreen("W-I05", "administrator")).toBe("/examinations/registrations");
    expect(workspaceScreen("W-I08", "faculty")).toBe("/assessments");
    expect(workspaceScreen("W-I06", "administrator")).toBe("/examinations/conduct");
    expect(workspaceScreen("W-I07", "faculty")).toBeNull();
  });
});
