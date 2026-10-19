// time-tracking  <-  time-tracking/src/lib/{time,util,csv,importer,store}.js
//   parseDuration / fmtClock / fmtTimer / fmtDuration / toHours / roundSeconds / calendar maths
//   (addDays, daysApart, weekStartOf, weekDays, monthDays, shiftMonth, monthKeysBetween) /
//   timePartOf / msAt, parseAmount / round2, parseCSV / csvField / toCSVLine, detectMapping /
//   parseImportDate / parseImportDuration / rowsToItems, and the store's pure billing maths:
//   rateAt / withRateFrom (effective-dated rates), resolveRate / resolveRounding /
//   resolveMinimumMinutes / billedSeconds / entryAmount / totals / groupEntries /
//   overlappingEntries / filterEntries / elapsedSeconds / durationPatch. Run in UTC with the clock
//   pinned per case (withRateFrom and resolveRate read "today").
import { rng } from '../rng.mjs';

process.env.TZ = 'UTC';

const LIB = 'src/time-tracking/src/lib/';
const pick = (r, a) => a[r.int(0, a.length - 1)];
const pad = (n) => String(n).padStart(2, '0');
const DAY = 86400000;
const iso = (ms) => new Date(ms).toISOString().slice(0, 10);
const nan = (x) => (typeof x === 'number' && Number.isNaN(x) ? null : x);

// Fixed, case-by-case clock: todayISO() and Date.now() inside the original read this.
const RealDate = Date;
function withClock(ms, fn) {
  class Fixed extends RealDate {
    constructor(...a) {
      if (a.length === 0) super(ms);
      else super(...a);
    }
    static now() {
      return ms;
    }
  }
  globalThis.Date = Fixed;
  try {
    return fn();
  } finally {
    globalThis.Date = RealDate;
  }
}

const DURATIONS = ['1:30', '0:05', '10:00', '1:30:15', '90m', '1h', '1.5h', '1h 30m', '2h15m', '1h30', '45s',
  '1.5', '0.25', '8', '.5', '.25', '.5h', ',5', '45', '90', ':45', '1,5', '', 'abc', '1:99', '24', '25', '0',
  ' 2H ', '1:30 trailing', '-1', '1.5.2', '3 hours', '2hrs', '1hr', '30 min', '15 minutes', '20 sec', '2m 30s',
  '1h 30m 20s', '7.75', '1h 15', '0:60', '12:5', '100', '1.0', '30.0', '2.5 h', '5 m', 'h', 'm', '1,25h'];
const IMPORT_DURATIONS = ['8:30:00', '0:45', '7.5', '7,5', '0', '', '1:30:00 trailing', '1,2,3', '1e', '1e308',
  '0.00001', '.5', '25', '1h 30m', '2 hours', '-2', '+1.5', '1.', '3:15', '1e2', '90m', 'abc', '36:00:00', '2,25'];
const TIMES = ['09:30', '2026-08-03 09:30:00', '21:05', '', 'nonsense', '24:30', '1:30 PM', '9:30 am', '11:59 p.m.',
  '2026-08-03 4:05:00 PM', '12:15 PM', '12:15 AM', '13:00 PM', '7:5', '123:45', '0:00', '23:59:59', '6:45PM',
  '10:00 A.M.', '08:00 xm', '5:30 p'];
const AMOUNTS = ['100', '99.5', '1,234.56', '1.234,56', '12,50', '1,234', '', 'abc', '100oops', '12.5.0', '1,2,3',
  '1e', 'Infinity', '1e309', '-12,50', '+12.50', '.5', '1 234,56', "1'234.56", '1,234,567.89', '1e2', '1500',
  '₹1500', '2,50,000', '0', '-0.5', '1.'];
const FIELDS = ['hello', 'a,b', 'say "hi"', 'line\nbreak', '=SUM(A1:A9)', '@HYPERLINK("evil")', '+1234', '-rf notes',
  '2026-08-03', '', 'DST/SERB grant', 'Tab\tstart', '\tlead', 'Kumar, A.', 'multi\r\nline', 'plain text', '₹ 1,500'];
const TASKS = ['Literature review', 'Field survey', 'Lab work', 'Report writing', 'Data analysis', '', 'None', 'none', 'Café visit'];
const NOTES = ['', 'Mid-term review', 'Sampling at site B', 'Résumé of findings', 'with PI'];
const ROUNDINGS = [0, 5, 6, 10, 15, 30];
const MINIMUMS = [0, 6, 10, 15, 30, 60];

function rate(r) {
  const k = r.int(0, 4);
  if (k === 0) return null;
  if (k <= 2) return pick(r, [0, 250, 400, 512.5, 1200, 875.25]);
  return Array.from({ length: r.int(1, 3) }, () => ({
    from: r.next() < 0.3 ? null : iso(Date.UTC(2026, r.int(0, 11), r.int(1, 28))),
    rate: r.next() < 0.15 ? null : pick(r, [300, 450, 600, 999.99, 1500]),
  }));
}

function dateCell(r) {
  const y = pick(r, [2024, 2025, 2026, 2027, 1900, 2000]);
  const m = r.int(0, 13);
  const d = r.int(0, 32);
  switch (r.int(0, 7)) {
    case 0: return `${y}-${pad(m)}-${pad(d)}`;
    case 1: return `${y}-${pad(m)}-${pad(d)} 09:00:00`;
    case 2: return `${m}/${d}/${y}`;
    case 3: return `${pad(d)}.${pad(m)}.${y}`;
    case 4: return `${d}/${m}/${y}`;
    case 5: return pick(r, ['nonsense', '', '2026/08/03', '3-8-2026', '2026-8-3']);
    default: return `${2026}-${pad(r.int(1, 12))}-${pad(r.int(1, 28))}`;
  }
}

function durationCell(r) {
  if (r.next() < 0.5) return pick(r, IMPORT_DURATIONS);
  return pick(r, [`${r.int(0, 9)}:${pad(r.int(0, 59))}:${pad(r.int(0, 59))}`, `${r.int(0, 12)}.${r.int(0, 99)}`, `${r.int(0, 9)},${r.int(0, 9)}`, `${r.int(1, 8)}h ${r.int(0, 59)}m`]);
}

function timeCell(r) {
  return pick(r, [...TIMES, `${pad(r.int(0, 23))}:${pad(r.int(0, 59))}`, `${r.int(1, 12)}:${pad(r.int(0, 59))} ${pick(r, ['AM', 'PM', 'am', 'pm'])}`]);
}

const HEADER_SETS = [
  ['User', 'Email', 'Client', 'Project', 'Description', 'Billable', 'Start date', 'Start time', 'End date', 'End time', 'Duration', 'Tags'],
  ['Project', 'Client', 'Description', 'Billable', 'Start Date', 'Start Time', 'End Date', 'End Time', 'Duration (h)'],
  ['Date', 'Client', 'Project', 'Task', 'Notes', 'Hours', 'Billable?'],
  ['Date', 'Project', 'Hours', 'Start', 'End', 'Notes'],
  ['When', 'How long', 'What'],
  [' DATE ', 'project name', 'duration (decimal)', 'note', 'From', 'To'],
  ['Date', 'Project', 'Duration', 'Start time', 'End time'],
];

function rowFor(r, headers) {
  return headers.map((h) => {
    const k = h.trim().toLowerCase();
    if (r.next() < 0.06) return '';
    if (k.includes('date') || k === 'when') return dateCell(r);
    if (k.includes('duration') || k === 'hours' || k === 'how long') return durationCell(r);
    if (k.includes('time') || k === 'start' || k === 'end' || k === 'from' || k === 'to') return timeCell(r);
    if (k.startsWith('project') || k === 'what') return pick(r, ['SERB-CRG/2025', 'DST Inspire', 'NAAC committee', 'site', '']);
    if (k.startsWith('client')) return pick(r, ['SERB', 'DST', 'UGC', '']);
    if (k.startsWith('billable')) return pick(r, ['Yes', 'No', 'true', '1', 'billable', 'no', '']);
    if (k === 'description' || k === 'task') return pick(r, TASKS);
    if (k.startsWith('note')) return pick(r, NOTES);
    return pick(r, ['x', 'Asha', 'a@b.in', '']);
  });
}

export default {
  fn: 'time tracking: durations, calendar, CSV import / export, billing maths',
  module: `${LIB}time.js`,
  modules: { time: `${LIB}time.js`, util: `${LIB}util.js`, csv: `${LIB}csv.js`, importer: `${LIB}importer.js`, store: `${LIB}store.js` },
  paths: ['src/time-tracking/src/lib/'],
  out: 'modules/people/tests/golden/time_tracking.json',
  cases() {
    const r = rng(5203);
    const out = [];
    for (let b = 1; b <= 200; b++) {
      const base = Date.UTC(r.int(2024, 2027), r.int(0, 11), r.int(1, 28));
      const day = (n) => iso(base + n * DAY);
      const now = base + r.int(0, 86399) * 1000;
      const durations = Array.from({ length: 6 }, () => (r.next() < 0.6 ? pick(r, DURATIONS)
        : pick(r, [`${r.int(0, 30)}:${pad(r.int(0, 59))}`, `${r.int(0, 9)}.${r.int(0, 99)}`, `${r.int(0, 200)}`, `${r.int(0, 9)}h ${r.int(0, 59)}m`, `${r.int(1, 300)}m`, `${r.int(0, 4)}h${r.int(0, 59)}`, `:${r.int(0, 59)}`, `${r.int(1, 90)} min`])));
      const seconds = Array.from({ length: 5 }, () => (r.next() < 0.1 ? -r.int(1, 100) : r.int(0, 40000)));
      const headers = pick(r, HEADER_SETS).slice();
      if (r.next() < 0.3) headers.reverse();
      const rows = Array.from({ length: r.int(2, 6) }, () => rowFor(r, headers));
      const csvRows = Array.from({ length: r.int(1, 4) }, () => Array.from({ length: r.int(1, 4) }, () => pick(r, FIELDS)));
      const settings = { defaultRate: rate(r), rounding: pick(r, [...ROUNDINGS, null]) };
      const clients = ['c1', 'c2'].map((id) => ({
        id,
        rate: rate(r),
        rounding: r.next() < 0.5 ? null : pick(r, ROUNDINGS),
        minimumMinutes: r.next() < 0.5 ? null : pick(r, MINIMUMS),
        name: pick(r, ['SERB', 'DST', 'UGC', 'ICSSR']),
      }));
      const projects = ['p1', 'p2', 'p3'].map((id) => ({
        id, clientId: pick(r, ['c1', 'c2', 'ghost']), rate: rate(r), name: pick(r, ['Monsoon study', 'Café survey', 'NAAC SSR']),
      }));
      const entries = Array.from({ length: r.int(2, 8) }, (_, i) => {
        const date = day(r.int(-3, 3));
        const timed = r.next() < 0.5;
        const startedAt = timed ? RealDate.UTC(...date.split('-').map((x, k) => (k === 1 ? +x - 1 : +x)), r.int(0, 23), pick(r, [0, 15, 30, 45])) : null;
        const secs = r.next() < 0.05 ? 0 : r.int(1, 20000);
        return {
          id: `e${i}`,
          date: r.next() < 0.05 ? '' : date,
          projectId: pick(r, ['p1', 'p2', 'p3', 'gone']),
          seconds: secs,
          billable: r.next() < 0.75,
          rate: r.next() < 0.2 ? pick(r, [0, 333, 1000]) : null,
          task: pick(r, TASKS),
          notes: pick(r, NOTES),
          startedAt,
          endedAt: timed ? startedAt + secs * 1000 : null,
        };
      });
      const probeStart = RealDate.UTC(...day(r.int(-1, 1)).split('-').map((x, k) => (k === 1 ? +x - 1 : +x)), r.int(0, 23), pick(r, [0, 30]));
      const probe = r.next() < 0.1 ? { startedAt: null, endedAt: probeStart + 3600000, id: null }
        : { startedAt: probeStart, endedAt: probeStart + r.int(1, 8) * 1800000, id: r.next() < 0.3 ? pick(r, entries).id : null };
      const rateOps = Array.from({ length: r.int(1, 4) }, () => ({
        rate: r.next() < 0.2 ? null : pick(r, [400, 550.5, 800, 1250]),
        from: r.next() < 0.4 ? null : day(r.int(-400, 200)),
      }));
      out.push({
        now,
        today: iso(now),
        durations,
        importDurations: Array.from({ length: 4 }, () => durationCell(r)),
        importDates: Array.from({ length: 4 }, () => dateCell(r)),
        times: Array.from({ length: 3 }, () => timeCell(r)),
        amounts: Array.from({ length: 3 }, () => (r.next() < 0.7 ? pick(r, AMOUNTS) : `${r.int(0, 99999)}.${pad(r.int(0, 99))}`)),
        seconds,
        roundTo: pick(r, ROUNDINGS),
        date: day(0),
        date2: day(r.int(-90, 90)),
        shift: r.int(-30, 30),
        fields: Array.from({ length: 4 }, () => (r.next() < 0.15 ? pick(r, [null, 42, 150, 1.5]) : pick(r, FIELDS))),
        csvRows,
        csvExtra: pick(r, ['', '\ufeff', '\n\n', '\r\n', 'a,"b, c","say ""hi""","line\nbreak"\r\n1,2,3,4', 'Date,Hours\n\n2026-08-03,1.5\n\n', ' , \n', '"unterminated, field']),
        headers,
        rows,
        settings,
        clients,
        projects,
        entries,
        probe,
        rateValue: rate(r),
        rateDates: Array.from({ length: 3 }, () => day(r.int(-300, 300))),
        rateStart: rate(r),
        rateOps,
        filter: { query: pick(r, ['', 'survey', 'CAFE', 'serb', 'review', 'resume', 'zzz']), clientId: pick(r, ['', 'c1', 'c2']), billing: pick(r, ['all', 'billable', 'nonbillable', 'unbilled']) },
        timerStart: r.next() < 0.2 ? null : now - r.int(-60, 50000) * 1000,
        patchSeconds: r.int(-10, 20000) + (r.next() < 0.3 ? 0.4 : 0),
      });
    }
    return out;
  },
  run(m, c) {
    const { time: T, util: U, csv: C, importer: I, store: S } = m;
    return withClock(c.now, () => {
      const st = S.createStore();
      st.settings = { ...S.DEFAULT_SETTINGS, ...c.settings };
      st.clients = c.clients;
      st.projects = c.projects;
      st.timer = c.timerStart == null ? null : { startedAt: c.timerStart, projectId: 'p1', task: '', notes: '', billable: true };
      const { source, mapping } = I.detectMapping(c.headers);
      let rv = c.rateStart;
      const csvText = c.csvRows.map((row) => C.toCSVLine(row)).join(lineEnd(c.now)) + c.csvExtra;
      const month = c.date.slice(0, 7);
      const [a, b] = c.date <= c.date2 ? [c.date, c.date2] : [c.date2, c.date];
      return {
        durations: c.durations.map((s) => nan(T.parseDuration(s))),
        importDurations: c.importDurations.map((s) => I.parseImportDuration(s)),
        importDates: c.importDates.map((s) => I.parseImportDate(s)),
        times: c.times.map((s) => T.timePartOf(s)),
        msAt: c.times.map((s) => T.msAt(c.date, T.timePartOf(s), c.shift > 20 ? 1 : 0)),
        amounts: c.amounts.map((s) => nan(U.parseAmount(s))),
        round2: c.seconds.map((s) => U.round2(s / 7)),
        seconds: c.seconds.map((s) => ({
          clock: T.fmtClock(s), timer: T.fmtTimer(s), human: T.fmtDuration(s),
          hours: T.toHours(s), hours4: T.toHours(s, 4), rounded: T.roundSeconds(s, c.roundTo),
        })),
        calendar: {
          addDays: T.addDays(c.date, c.shift),
          daysApart: T.daysApart(c.date, c.date2),
          weekMon: T.weekStartOf(c.date, 1),
          weekSun: T.weekStartOf(c.date, 0),
          weekDays: T.weekDays(T.weekStartOf(c.date, 1)),
          monthDays: T.monthDays(month).length,
          shiftMonth: T.shiftMonth(month, c.shift),
          monthKeysBetween: T.monthKeysBetween(a, b),
          monthKeyOf: T.monthKeyOf(c.date),
        },
        fields: c.fields.map((f) => C.csvField(f)),
        csvLine: C.toCSVLine(c.fields),
        parsedCsv: C.parseCSV(csvText),
        csvText,
        source,
        mapping,
        items: I.rowsToItems(c.rows, mapping),
        rateAt: c.rateDates.map((d) => S.rateAt(c.rateValue, d)),
        withRateFrom: c.rateOps.map((op) => {
          rv = S.withRateFrom(rv, op.rate, op.from);
          return rv;
        }),
        entries: c.entries.map((e) => ({
          rate: st.resolveRate(e),
          rounding: st.resolveRounding(e),
          minimum: st.resolveMinimumMinutes(e),
          billed: st.billedSeconds(e),
          amount: st.entryAmount(e),
          patch: st.durationPatch(e, c.patchSeconds),
        })),
        totals: st.totals(c.entries),
        groups: ['project', 'client', 'task'].map((by) => st.groupEntries(c.entries, by)
          .map((g) => ({ key: g.key, label: g.label, sublabel: g.sublabel, ids: g.entries.map((e) => e.id), seconds: g.seconds, billableSeconds: g.billableSeconds, amount: g.amount }))),
        overlaps: st.overlappingEntries(c.entries, c.probe).map((e) => e.id),
        filtered: st.filterEntries(c.entries, c.filter).map((e) => e.id),
        elapsed: st.elapsedSeconds(c.now),
      };
    });
  },
};

// Line ending of the generated CSV text: CRLF or LF, fixed by the case clock.
function lineEnd(now) {
  return now % 2 ? '\r\n' : '\n';
}
