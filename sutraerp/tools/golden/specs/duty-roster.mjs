// duty-roster  <-  shifts/src/core/scheduling.js, rules.js, fill.js, lib/time.js
//   validateShift / findConflicts / availabilityStatus / weeklyMinutes / hoursStatus /
//   evaluateMove / suggestAssignees / coverageGaps, ruleViolations / scheduleRuleViolations,
//   planFill, formatDate / formatHours / startOfWeek over random rosters. Run in UTC.
import { rng } from '../rng.mjs';

process.env.TZ = 'UTC';

const CORE = 'src/shifts/src/core/';
const pick = (r, a) => a[r.int(0, a.length - 1)];
const DAY = 86400000;
const iso = (ms) => new Date(ms).toISOString().slice(0, 10);
const NAMES = ['Asha', 'Bilal', 'Chitra', 'Deepak', 'Esha', 'Farhan', 'Gauri', 'Harish'];
const ROLES = ['Warden', 'Security', 'Lab', 'warden ', ''];
const pad = (n) => String(n).padStart(2, '0');
const time = (r) => (r.next() < 0.03 ? pick(r, ['24:00', '9:00', '', '12:60']) : `${pad(r.int(0, 23))}:${pick(r, ['00', '30', '15', '45'])}`);
const maybe = (r, p, f) => (r.next() < p ? f() : null);

export default {
  fn: 'roster scheduling / rules / fill',
  module: `${CORE}scheduling.js`,
  modules: { sched: `${CORE}scheduling.js`, rules: `${CORE}rules.js`, fill: `${CORE}fill.js`, time: 'src/shifts/src/lib/time.js' },
  paths: [CORE, 'src/shifts/src/lib/'],
  out: 'modules/people/tests/golden/duty_roster.json',
  cases() {
    const r = rng(3101);
    const out = [];
    for (let b = 1; b <= 200; b++) {
      const base = Date.UTC(2026, r.int(0, 11), r.int(1, 28));
      const day = (n) => iso(base + n * DAY);
      const n = r.int(2, 6);
      const employees = NAMES.slice(0, n).map((name, i) => ({
        id: `e${i}`,
        name,
        role: pick(r, ['Warden', 'Security', 'Lab']),
        active: r.next() < 0.9,
        maxHours: maybe(r, 0.4, () => pick(r, [8, 16, 24, 40])),
        targetHours: maybe(r, 0.4, () => pick(r, [4, 12, 20, 30])),
      }));
      const availability = {};
      for (const e of employees) {
        if (r.next() < 0.3) continue;
        availability[e.id] = {
          days: r.next() < 0.85 ? Array.from({ length: 7 }, () => (r.next() < 0.1 ? null : { off: r.next() < 0.2, start: time(r), end: pick(r, [time(r), '23:59']) })) : undefined,
          exceptions: r.next() < 0.5 ? [{ from: day(r.int(-2, 4)), to: day(r.int(4, 8)), note: maybe(r, 0.5, () => 'Leave') }] : [],
        };
      }
      const shifts = Array.from({ length: r.int(3, 14) }, (_, i) => ({
        id: `s${i}`,
        date: day(r.int(0, 8)),
        start: time(r),
        end: time(r),
        breakMinutes: r.next() < 0.6 ? null : pick(r, [0, 30, 45, 60, -5, 900]),
        role: pick(r, ROLES),
        assigneeId: r.next() < 0.35 ? null : pick(r, employees).id,
      }));
      const rules = r.next() < 0.2 ? null : {
        minRestHours: maybe(r, 0.6, () => pick(r, [8, 11, 12.5, 30, 0])),
        maxDailyHours: maybe(r, 0.5, () => pick(r, [6, 8, 10, 25])),
        maxConsecutiveDays: maybe(r, 0.5, () => pick(r, [1, 2, 3, 6])),
      };
      const requirements = Array.from({ length: r.int(0, 3) }, (_, i) => ({
        id: `q${i}`,
        role: pick(r, ['Warden', 'Security', 'Lab']),
        days: [0, 1, 2, 3, 4, 5, 6].filter(() => r.next() < 0.6),
        count: r.int(1, 3),
        start: maybe(r, 0.6, () => time(r)),
        end: maybe(r, 0.6, () => pick(r, [time(r), '23:59'])),
      }));
      const dates = Array.from({ length: 9 }, (_, i) => day(i));
      const moves = Array.from({ length: 3 }, () => ({
        shift: r.int(0, shifts.length - 1),
        date: r.next() < 0.05 ? 'nope' : day(r.int(0, 8)),
        assigneeId: r.next() < 0.2 ? null : r.next() < 0.05 ? 'ghost' : pick(r, employees).id,
        copy: r.next() < 0.3,
      }));
      const attendance = Object.fromEntries(shifts.filter(() => r.next() < 0.1).map((s) => [s.id, true]));
      const choices = {};
      for (const s of shifts) if (!s.assigneeId && r.next() < 0.2) choices[s.id] = r.next() < 0.4 ? null : pick(r, employees).id;
      out.push({ employees, availability, shifts, rules, requirements, dates, moves, attendance, choices, limit: r.int(1, 4) });
    }
    return out;
  },
  run(m, c) {
    const { sched, rules: R, fill, time } = m;
    const settings = { rules: c.rules };
    const ids = (list) => list.map((s) => s.id);
    const cand = (x) => ({ employeeId: x.employee.id, name: x.employee.name, status: x.status, deficitMinutes: x.deficitMinutes, roleMatch: x.roleMatch, projectedMinutes: x.projectedMinutes, ruleWarnings: x.ruleWarnings, reasons: x.reasons });
    const empName = (id) => c.employees.find((e) => e.id === id)?.name ?? 'This employee';
    return {
      perShift: c.shifts.map((s) => ({
        errors: sched.validateShift(s),
        conflicts: ids(sched.findConflicts(s, c.shifts)),
        availability: sched.availabilityStatus(s, c.availability[s.assigneeId]),
        net: sched.shiftNetMinutes(s),
        rules: c.rules && s.assigneeId ? R.ruleViolations(s, s.assigneeId, c.shifts, c.rules, { name: empName(s.assigneeId) }) : [],
        suggestions: sched.suggestAssignees(s, { employees: c.employees, shifts: c.shifts, availability: c.availability, rules: c.rules }, c.limit).map(cand),
        formatDate: time.isValidDate(s.date) ? time.formatDate(s.date) : null,
        week: time.isValidDate(s.date) ? time.startOfWeek(s.date) : null,
      })),
      perEmployee: c.employees.map((e) => {
        const mins = sched.weeklyMinutes(c.shifts, e.id, time.startOfWeek(c.dates[0]));
        return { minutes: mins, status: sched.hoursStatus(mins, e), hours: time.formatHours(mins), rules: R.scheduleRuleViolations(e.id, c.shifts, c.dates, c.rules, { name: e.name }) };
      }),
      moves: c.moves.map((mv) => sched.evaluateMove(c.shifts[mv.shift], { date: mv.date, assigneeId: mv.assigneeId }, { shifts: c.shifts, employees: c.employees, availability: c.availability, attendance: c.attendance }, { copy: mv.copy })),
      gaps: sched.coverageGaps(c.shifts, c.requirements, c.dates).map((g) => ({ date: g.date, requirementId: g.requirement.id, role: g.requirement.role, scheduled: g.scheduled, missing: g.missing })),
      plan: fill.planFill({ shifts: c.shifts, employees: c.employees, availability: c.availability, settings }, c.dates, { limit: c.limit, choices: c.choices }).map((p) => ({ shiftId: p.shift.id, candidates: p.candidates.map(cand), proposal: p.proposal?.employee.id ?? null, skipped: p.skipped, stale: p.stale, pinned: p.pinned })),
    };
  },
};
