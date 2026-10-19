// fleet-maintenance  <-  fleet/src/core/reminders.js, fuel.js, costs.js, dates.js
//   reminderNextDue / reminderState / completeReminder / compliance, renewalState /
//   completeRenewal (anchored repeats), fuelDerived / fuelSummary / fuelTotal / unitPrice,
//   costsByVehicle / usageByVehicle, addInterval / diffDays over random fleets. Run in UTC.
import { rng } from '../rng.mjs';

process.env.TZ = 'UTC';

const CORE = 'src/fleet/src/core/';
const pick = (r, a) => a[r.int(0, a.length - 1)];
const DAY = 86400000;
const iso = (ms) => new Date(ms).toISOString().slice(0, 10);
const UNITS = ['days', 'weeks', 'months', 'years'];
const interval = (r) => (r.next() < 0.25 ? null : { every: pick(r, [0, 1, 2, 3, 6, 12, 30]), unit: pick(r, UNITS) });
const maybe = (r, p, f) => (r.next() < p ? f() : null);

export default {
  fn: 'reminders / renewals / fuel / costs',
  module: `${CORE}reminders.js`,
  modules: { rem: `${CORE}reminders.js`, fuel: `${CORE}fuel.js`, costs: `${CORE}costs.js`, dates: `${CORE}dates.js` },
  paths: [CORE],
  out: 'modules/campus/tests/golden/fleet_maintenance.json',
  cases() {
    const r = rng(2604);
    const out = [];
    for (let b = 1; b <= 200; b++) {
      const base = Date.UTC(2026, r.int(0, 11), r.int(1, 28));
      const day = (n) => iso(base + n * DAY);
      const today = day(0);
      const due = { days: pick(r, [7, 14, 30]), meter: pick(r, [250, 500, 1000]) };
      const reminders = Array.from({ length: 3 }, () => {
        const lastMeter = maybe(r, 0.7, () => r.int(0, 60000));
        const rem = {
          interval: interval(r),
          meterInterval: maybe(r, 0.6, () => pick(r, [0, 5000, 10000, 15000])),
          dueSoonDays: maybe(r, 0.3, () => pick(r, [3, 10, 21])),
          dueSoonMeter: maybe(r, 0.3, () => pick(r, [100, 800])),
          lastDate: maybe(r, 0.7, () => day(-r.int(0, 500))),
          lastMeter,
          nextDate: maybe(r, 0.15, () => day(r.int(-30, 60))),
          nextMeter: maybe(r, 0.15, () => r.int(0, 70000)),
          snoozedUntil: maybe(r, 0.25, () => day(r.int(-5, 20))),
          createdDay: day(-r.int(0, 700)),
          startDate: maybe(r, 0.5, () => day(-r.int(400, 800))),
          startMeter: maybe(r, 0.5, () => r.int(0, 10000)),
          history: [],
        };
        let d = -r.int(200, 600);
        for (let k = 0; k < r.int(0, 4); k++) {
          d += r.int(10, 120);
          rem.history.push({ date: day(d), meter: maybe(r, 0.8, () => r.int(0, 60000)), entryId: maybe(r, 0.5, () => `e${k}`), onTime: r.next() < 0.7 });
        }
        return rem;
      });
      const vehicleMeter = maybe(r, 0.85, () => r.int(0, 70000));
      const completions = reminders.map(() => ({ date: day(r.int(-300, 5)), meter: maybe(r, 0.7, () => r.int(0, 70000)), entryId: maybe(r, 0.5, () => 'svc') }));
      const renewals = Array.from({ length: 2 }, () => ({
        dueDate: maybe(r, 0.9, () => day(r.int(-60, 120))),
        dueSoonDays: maybe(r, 0.3, () => pick(r, [5, 30])),
        repeat: maybe(r, 0.75, () => ({ every: pick(r, [1, 3, 6, 12, 1]), unit: pick(r, ['months', 'years', 'days', 'weeks']) })),
        repeatAnchor: null,
        snoozedUntil: maybe(r, 0.2, () => day(r.int(-3, 10))),
        history: [],
        done: false,
      }));
      const renewOps = Array.from({ length: 3 }, () => ({ date: day(r.int(-10, 10)), nextDue: maybe(r, 0.15, () => day(r.int(30, 400))), cost: maybe(r, 0.5, () => r.int(100, 900000)) }));
      const vid = ['v1', 'v2', 'v3'];
      let meter = r.int(0, 20000);
      const fuel = Array.from({ length: r.int(0, 10) }, (_, i) => {
        meter += r.next() < 0.1 ? -r.int(1, 50) : r.int(0, 600);
        const volume = r.int(5, 600) / 10;
        const e = { id: `f${i}`, date: day(-r.int(0, 90)), time: maybe(r, 0.5, () => `${String(r.int(0, 23)).padStart(2, '0')}:${String(r.int(0, 59)).padStart(2, '0')}`), meter: r.next() < 0.15 ? null : meter, volume, total: r.int(500, 900000), partial: r.next() < 0.25, reset: r.next() < 0.1 };
        return e;
      });
      const meterUnit = pick(r, ['km', 'km', 'mi', 'hr']);
      const volumeUnit = pick(r, ['L', 'L', 'gal', 'kWh']);
      const prices = Array.from({ length: 4 }, () => ({ volume: pick(r, [0, 12.5, 33.33, r.int(1, 999) / 10]), price: pick(r, [102.37, 94.72, 0, r.int(1, 30000) / 100, -1]) }));
      const lines = Array.from({ length: r.int(0, 15) }, () => ({ vehicleId: pick(r, vid), date: r.next() < 0.05 ? '' : day(-r.int(0, 120)), kind: pick(r, ['fuel', 'service', 'other']), amount: r.int(0, 500000) }));
      const readings = Array.from({ length: r.int(0, 15) }, () => ({ vehicleId: pick(r, vid), date: day(-r.int(0, 120)), value: r.next() < 0.08 ? null : r.int(0, 90000), void: r.next() < 0.1 }));
      const from = maybe(r, 0.8, () => day(-r.int(30, 90)));
      const to = maybe(r, 0.7, () => day(-r.int(0, 29)));
      const intervals = Array.from({ length: 4 }, () => ({ date: day(r.int(-400, 400)), interval: { every: pick(r, [0, 1, 2, 5, 13, 1.5]), unit: pick(r, [...UNITS, 'fortnights']) }, times: r.int(0, 3) }));
      out.push({ today, due, reminders, vehicleMeter, completions, renewals, renewOps, fuel, meterUnit, volumeUnit, prices, lines, readings, from, to, intervals });
    }
    return out;
  },
  run(m, c) {
    const { rem, fuel, costs, dates } = m;
    const settings = { dueSoon: c.due };
    const toRem = (x) => ({ ...x, createdAt: x.createdDay ? `${x.createdDay}T12:00:00.000Z` : undefined });
    const reminders = c.reminders.map((x, i) => {
      const rr = toRem(x);
      const st = rem.reminderState(rr, { meter: c.vehicleMeter }, c.today, settings);
      const comp = c.completions[i];
      const done = rem.completeReminder(rr, comp, { meter: c.vehicleMeter }, settings);
      return {
        next: rem.reminderNextDue(rr),
        state: st,
        completed: { lastDate: done.lastDate ?? null, lastMeter: done.lastMeter ?? null, nextDate: done.nextDate ?? null, nextMeter: done.nextMeter ?? null, snoozedUntil: done.snoozedUntil ?? null, history: done.history },
        compliance: rem.compliance(done),
      };
    });
    const renewals = c.renewals.map((x) => {
      const state = rem.renewalState(x, c.today, settings);
      let cur = x;
      const steps = c.renewOps.map((o) => {
        cur = rem.completeRenewal(cur, o);
        return { dueDate: cur.dueDate ?? null, repeatAnchor: cur.repeatAnchor ?? null, done: cur.done, history: cur.history };
      });
      return { state, steps };
    });
    const derived = fuel.fuelDerived(c.fuel, c.meterUnit, c.volumeUnit);
    return {
      reminders,
      renewals,
      fuel: { derived: Object.fromEntries([...derived.entries()].sort()), summary: fuel.fuelSummary(c.fuel, derived, { meterUnit: c.meterUnit, volumeUnit: c.volumeUnit }) },
      prices: c.prices.map((p) => ({ total: fuel.fuelTotal(p.volume, p.price), unit: fuel.unitPrice(fuel.fuelTotal(p.volume, p.price), p.volume) })),
      costs: Object.fromEntries([...costs.costsByVehicle(c.lines, c.from, c.to).entries()].sort()),
      usage: Object.fromEntries([...costs.usageByVehicle(c.readings, c.from, c.to).entries()].sort()),
      intervals: c.intervals.map((x) => ({ added: dates.addInterval(x.date, x.interval, x.times), diff: dates.diffDays(c.today, x.date) })),
    };
  },
};
