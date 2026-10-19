/** In-memory `RosterApi` for mock mode and screen tests (the server's rules are authoritative). */
import type { RosterApi } from "./api";
import { addDaysIso, mondayOf } from "./roster";
import type { DutyShift, RosterSettings } from "./types";

const toMin = (t: string) => Number(t.slice(0, 2)) * 60 + Number(t.slice(3));
const span = (s: { date: string; start: string; end: string }) => {
  const base = Date.parse(`${s.date}T00:00:00Z`) / 60000 + toMin(s.start);
  const d = toMin(s.end) > toMin(s.start) ? toMin(s.end) - toMin(s.start) : toMin(s.end) + 1440 - toMin(s.start);
  return [base, base + d] as const;
};

export function createMockRosterApi(): RosterApi {
  const staff = [
    { staffId: "st-1", name: "Asha Patil", role: "Security" },
    { staffId: "st-2", name: "Bilal Khan", role: "Warden" },
  ];
  const shifts: DutyShift[] = [];
  let seq = 0;
  let settings: RosterSettings = { rules: { minRestHours: 11, maxDailyHours: null, maxConsecutiveDays: null }, requirements: [] };
  const view = (s: DutyShift): DutyShift => {
    const [a, b] = span(s);
    const conflicts = s.staffId ? shifts.filter((o) => o.id !== s.id && o.staffId === s.staffId && span(o)[0] < b && a < span(o)[1]).map((o) => o.id) : [];
    return { ...s, conflicts, staffName: staff.find((x) => x.staffId === s.staffId)?.name ?? null };
  };
  const net = (start: string, end: string, brk = 0) => (toMin(end) > toMin(start) ? toMin(end) - toMin(start) : toMin(end) + 1440 - toMin(start)) - brk;
  const save = (s: DutyShift) => {
    const v = view(s);
    if (v.conflicts.length) throw new Error(`${v.staffName} already has an overlapping shift then.`);
    return v;
  };
  return {
    board: async (_id, from, to) => {
      const f = from ?? mondayOf(new Date().toISOString().slice(0, 10));
      const t = to ?? addDaysIso(f, 6);
      const list = shifts.filter((s) => s.date >= f && s.date <= t).map(view);
      return {
        from: f, to: t, weekStart: mondayOf(f), openShifts: list.filter((s) => !s.staffId).length, shifts: list, gaps: [],
        staff: staff.map((x) => ({ ...x, rosterable: true, targetHours: null, maxHours: null, weekMinutes: list.filter((s) => s.staffId === x.staffId).reduce((n, s) => n + s.netMinutes, 0), hoursStatus: "ok" as const, ruleWarnings: [] })),
      };
    },
    createShift: async (_id, input) => {
      if (input.start === input.end) throw new Error("End time must differ from start time.");
      const s: DutyShift = { id: `sh-${++seq}`, date: input.date, start: input.start, end: input.end, breakMinutes: input.breakMinutes ?? 0, netMinutes: net(input.start, input.end, input.breakMinutes ?? 0), overnight: toMin(input.end) < toMin(input.start), role: input.role, location: input.location ?? null, note: input.note ?? null, staffId: input.staffId ?? null, staffName: null, status: "draft", conflicts: [], availability: "available", ruleWarnings: [], version: 1 };
      const v = save(s);
      shifts.push(s);
      return { shift: v, warnings: [] };
    },
    updateShift: async (id, _version, input) => {
      const i = shifts.findIndex((s) => s.id === id);
      const next: DutyShift = { ...shifts[i]!, ...input, staffId: input.staffId ?? null, location: input.location ?? null, note: input.note ?? null, breakMinutes: input.breakMinutes ?? 0, netMinutes: net(input.start, input.end, input.breakMinutes ?? 0), overnight: toMin(input.end) < toMin(input.start), version: shifts[i]!.version + 1 };
      const v = save(next);
      shifts[i] = next;
      return { shift: v, warnings: [] };
    },
    deleteShift: async (id) => {
      shifts.splice(shifts.findIndex((s) => s.id === id), 1);
    },
    suggestions: async (id) => {
      const s = shifts.find((x) => x.id === id)!;
      return staff.filter((x) => !shifts.some((o) => o.staffId === x.staffId && span(o)[0] < span(s)[1] && span(s)[0] < span(o)[1])).map((x) => ({ employeeId: x.staffId, name: x.name, status: "available" as const, deficitMinutes: 0, roleMatch: x.role === s.role, projectedMinutes: s.netMinutes, ruleWarnings: [], reasons: ["Available"] }));
    },
    fill: async (_id, from, to, apply) => {
      const open = shifts.filter((s) => !s.staffId && s.date >= from && s.date <= to);
      const rows = [];
      for (const s of open) {
        const c = staff.find((x) => !shifts.some((o) => o.staffId === x.staffId && span(o)[0] < span(s)[1] && span(s)[0] < span(o)[1]));
        rows.push({ shiftId: s.id, candidates: [], proposal: c?.staffId ?? null, skipped: false, stale: false, pinned: false });
        if (apply && c) s.staffId = c.staffId;
      }
      const filling = rows.filter((r) => r.proposal).length;
      return { rows, total: rows.length, filling, skipped: 0, nobody: rows.length - filling, applied: apply ? filling : 0 };
    },
    publish: async (_id, from, to) => {
      let n = 0;
      for (const s of shifts) if (s.status === "draft" && s.date >= from && s.date <= to) { s.status = "published"; n += 1; }
      return { published: n };
    },
    settings: async () => settings,
    saveSettings: async (_id, s) => {
      settings = s;
      return s;
    },
  };
}
