// shift-swaps-timesheets  <-  shifts/src/core/swaps.js, attendance.js, timesheet.js, payroll.js,
//   reports.js, ics.js (+ lib/time.js): createSwapRequest / evaluateSwap / resolveSwap /
//   applySwap, lateMinutes / workedMinutes / elapsedMinutes / isInProgress / isRunningNow /
//   clockInRecord / clockOutRecord / summarizeAttendance / attendanceForRange, timesheetRows /
//   employeeSummaries / timesheetCSV, laborCost / budgetStatus, buildScheduleICS, datesBetween /
//   previousRange / buildReport / reportCSV over random rosters. Run in UTC.
//
// Names and roles are chosen so that String#localeCompare and a byte comparison agree (the Rust
// port sorts by bytes).
import { rng } from '../rng.mjs';

process.env.TZ = 'UTC';

const CORE = 'src/shifts/src/core/';
const pick = (r, a) => a[r.int(0, a.length - 1)];
const DAY = 86400000;
const iso = (ms) => new Date(ms).toISOString().slice(0, 10);
const NAMES = ['Asha', 'Bilal', 'Chitra', 'Deepak', 'Esha', 'Farhan', 'Gauri', 'Harish', 'Asha', '=HYPERLINK("x")', 'Liam "Lee" Torres, Jr.'];
const ROLES = ['Warden', 'Security', 'Lab', '', '@cmd', 'Night; gate, main'];
const NOTES = ['Bring keys; door code 1234, ask Sam', 'one\rEND:VEVENT\rBEGIN:VEVENT\r\ntwo\nthree', `a${'🎉'.repeat(40)}`, 'R'.repeat(90), '', 'Back\\slash'];
const pad = (n) => String(n).padStart(2, '0');
const time = (r) => (r.next() < 0.03 ? pick(r, ['24:00', '9:00', '', '12:60']) : `${pad(r.int(0, 23))}:${pick(r, ['00', '30', '15', '45', '07'])}`);
const maybe = (r, p, f) => (r.next() < p ? f() : null);

function attendanceRecord(r) {
  const rec = { status: pick(r, ['present', 'late', 'absent', 'present']) };
  if (r.next() < 0.85) rec.clockIn = r.next() < 0.1 ? '' : time(r);
  if (r.next() < 0.7) rec.clockOut = r.next() < 0.1 ? '' : time(r);
  if (r.next() < 0.3) rec.note = pick(r, ['Traffic', '', 'Swapped in']);
  return rec;
}

export default {
  fn: 'shift swaps / attendance / timesheet / payroll / reports / ics',
  module: `${CORE}swaps.js`,
  modules: {
    swaps: `${CORE}swaps.js`,
    att: `${CORE}attendance.js`,
    ts: `${CORE}timesheet.js`,
    pay: `${CORE}payroll.js`,
    rep: `${CORE}reports.js`,
    ics: `${CORE}ics.js`,
    time: 'src/shifts/src/lib/time.js',
  },
  paths: [CORE, 'src/shifts/src/lib/'],
  out: 'modules/people/tests/golden/shift_swaps_timesheets.json',
  cases() {
    const r = rng(5202);
    const out = [];
    for (let b = 1; b <= 200; b++) {
      const base = Date.UTC(2026, r.int(0, 11), r.int(1, 28));
      const day = (n) => iso(base + n * DAY);
      const n = r.int(2, 6);
      const names = [...NAMES];
      const employees = Array.from({ length: n }, (_, i) => ({
        id: `e${i}`,
        name: r.next() < 0.15 ? pick(r, names) : names[i],
        role: pick(r, ['Warden', 'Security', 'Lab']),
        active: r.next() < 0.85,
        maxHours: maybe(r, 0.4, () => pick(r, [8, 16, 24, 40])),
        targetHours: null,
        hourlyRate: maybe(r, 0.7, () => pick(r, [10, 12.5, 20, 0, 17.75, 333.33])),
      }));
      const availability = {};
      for (const e of employees) {
        if (r.next() < 0.5) continue;
        availability[e.id] = {
          days: r.next() < 0.8 ? Array.from({ length: 7 }, () => (r.next() < 0.1 ? null : { off: r.next() < 0.2, start: time(r), end: pick(r, [time(r), '23:59']) })) : undefined,
          exceptions: r.next() < 0.4 ? [{ from: day(r.int(-2, 4)), to: day(r.int(4, 8)), note: maybe(r, 0.5, () => 'Leave') }] : [],
        };
      }
      const shifts = Array.from({ length: r.int(3, 14) }, (_, i) => ({
        id: `s${i}`,
        date: day(r.int(-3, 9)),
        start: time(r),
        end: time(r),
        breakMinutes: r.next() < 0.6 ? null : pick(r, [0, 30, 45, 60, -5, 900, 22.5]),
        role: pick(r, ROLES),
        assigneeId: r.next() < 0.25 ? null : r.next() < 0.08 ? 'gone' : pick(r, employees).id,
        note: maybe(r, 0.25, () => pick(r, NOTES)),
      }));
      const attendance = {};
      for (const s of shifts) if (r.next() < 0.55) attendance[s.id] = attendanceRecord(r);
      const rules = r.next() < 0.4 ? null : {
        minRestHours: maybe(r, 0.6, () => pick(r, [8, 11, 12.5])),
        maxDailyHours: maybe(r, 0.5, () => pick(r, [6, 8, 10])),
        maxConsecutiveDays: maybe(r, 0.5, () => pick(r, [1, 2, 3, 6])),
      };
      const coverage = Array.from({ length: r.int(0, 3) }, (_, i) => ({
        id: `q${i}`,
        role: pick(r, ['Warden', 'Security', 'Lab']),
        days: [0, 1, 2, 3, 4, 5, 6].filter(() => r.next() < 0.6),
        count: r.int(1, 3),
        start: maybe(r, 0.5, () => time(r)),
        end: maybe(r, 0.5, () => pick(r, [time(r), '23:59'])),
      }));
      const swaps = Array.from({ length: 3 }, () => ({
        shift: r.next() < 0.05 ? -1 : r.int(0, shifts.length - 1),
        to: r.next() < 0.08 ? null : r.next() < 0.1 ? 'ghost' : pick(r, employees).id,
        reason: pick(r, ['  Trip  ', '', 'Exam duty', '\tFamily\n']),
        withEmployees: r.next() < 0.7,
        withRules: r.next() < 0.6,
        withAttendance: r.next() < 0.6,
        resolve: [pick(r, ['approved', 'declined', 'cancelled', 'pending', 'bogus']), pick(r, ['approved', 'declined'])],
        apply: pick(r, ['as-is', 'as-is', 'reassigned', 'deleted']),
      }));
      const clocks = Array.from({ length: 4 }, () => ({
        shift: r.int(0, shifts.length - 1),
        record: r.next() < 0.3 ? null : attendanceRecord(r),
        at: time(r),
        now: time(r),
        grace: pick(r, [0, 5, 10, -3, null, 2.5]),
        today: day(r.int(-3, 9)),
      }));
      const ranges = [
        [day(0), day(6)],
        [day(r.int(-3, 3)), day(r.int(3, 9))],
      ];
      const cost = {
        dates: r.next() < 0.5 ? Array.from({ length: 7 }, (_, i) => day(i)) : Array.from({ length: 7 }, (_, i) => day(i)).filter(() => r.next() < 0.6),
        threshold: maybe(r, 0.6, () => pick(r, [4, 8, 10, 40])),
        multiplier: maybe(r, 0.6, () => pick(r, [1, 1.5, 2])),
        budget: pick(r, [null, 0, -5, 100, 1000, 37.5]),
      };
      const ics = {
        employeeId: r.next() < 0.3 ? pick(r, employees).id : null,
        generatedAt: new Date(base + r.int(0, 86399) * 1000).toISOString(),
      };
      out.push({ employees, availability, shifts, attendance, rules, coverage, swaps, clocks, ranges, cost, ics });
    }
    return out;
  },
  run(m, c) {
    const { swaps: S, att: A, ts: T, pay: P, rep: R, ics: I } = m;
    const err = (f) => {
      try {
        return { ok: f() };
      } catch (e) {
        return { error: e.message };
      }
    };
    const swaps = c.swaps.map((sw) => {
      const shift = sw.shift < 0 ? null : c.shifts[sw.shift];
      const created = err(() => S.createSwapRequest({ shift, toEmployeeId: sw.to, reason: sw.reason, id: 'w1', now: 't0' }));
      if (!created.ok) return { created };
      const evaluation = S.evaluateSwap({
        shift,
        toEmployeeId: sw.to,
        shifts: c.shifts,
        availability: c.availability,
        employees: sw.withEmployees ? c.employees : undefined,
        attendance: sw.withAttendance ? c.attendance : null,
        rules: sw.withRules ? c.rules : null,
      });
      const first = err(() => S.resolveSwap(created.ok, sw.resolve[0], 't1'));
      const second = first.ok ? err(() => S.resolveSwap(first.ok, sw.resolve[1], 't2')) : null;
      const list = sw.apply === 'deleted'
        ? c.shifts.filter((s) => s.id !== shift.id)
        : sw.apply === 'reassigned'
          ? c.shifts.map((s) => (s.id === shift.id ? { ...s, assigneeId: 'someone-else' } : s))
          : c.shifts;
      const applied = err(() => S.applySwap(created.ok, list).map((s) => s.assigneeId ?? null));
      return { created, evaluation, first, second, applied };
    });
    const clocks = c.clocks.map((k) => {
      const shift = c.shifts[k.shift];
      return {
        late: A.lateMinutes(shift, k.at),
        worked: A.workedMinutes(shift.start, k.at),
        elapsed: A.elapsedMinutes(k.at, k.now),
        inProgress: A.isInProgress(k.record),
        runningNow: A.isRunningNow(shift, k.record, k.today),
        clockIn: A.clockInRecord(shift, k.record, k.at, k.grace),
        clockOut: A.clockOutRecord(k.record, k.now),
      };
    });
    const summary = A.summarizeAttendance(Object.values(c.attendance));
    const state = { employees: c.employees, shifts: c.shifts, attendance: c.attendance };
    const perRange = c.ranges.map(([from, to]) => {
      const rows = T.timesheetRows(state, from, to);
      const report = R.buildReport(
        { ...state, availability: c.availability, settings: { coverage: c.coverage, overtimeThresholdHours: c.cost.threshold, overtimeMultiplier: c.cost.multiplier } },
        from,
        to,
      );
      return {
        forRange: A.attendanceForRange(c.shifts, c.attendance, from, to).map((x) => [x.shift.id, x.record?.status ?? null]),
        rows,
        summaries: T.employeeSummaries(rows),
        csv: T.timesheetCSV(state, from, to),
        dates: R.datesBetween(from, to),
        previous: R.previousRange(from, to),
        report,
        reportCsv: [...R.REPORT_SECTIONS, 'nonsense'].map((s) => err(() => R.reportCSV(report, s))),
        ics: I.buildScheduleICS(
          { employees: c.employees, shifts: c.shifts },
          { from, to, employeeId: c.ics.employeeId, generatedAt: c.ics.generatedAt },
        ),
      };
    });
    const labor = P.laborCost(c.shifts, c.employees, c.cost.dates, { thresholdHours: c.cost.threshold, multiplier: c.cost.multiplier });
    return { swaps, clocks, summary, perRange, labor, budget: P.budgetStatus(labor.cost, c.cost.budget) };
  },
};
