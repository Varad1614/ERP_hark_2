// inventory-demand  <-  inventory/src/lib/demand.js, reorder.js, lots.js, reports.js, units.js
//   demandRates / coverDays / suggestedThreshold / runningOut / rateLabel / coverLabel /
//   suggestedQty / onOrderQty / stockStatus and the FEFO lot rules (normalizeLots, addLot,
//   consumeLots, consumeLotAt, resizeLots, expiryStatus, expiringProducts) over random logs.
//   Run in UTC so "local" calendar dates are UTC dates.
import { rng } from '../rng.mjs';

process.env.TZ = 'UTC';

const LIB = 'src/inventory/src/lib/';
const DAY = 86400000;
const pick = (r, a) => a[r.int(0, a.length - 1)];
const UNITS = ['each', 'kg', 'L', 'EACH', undefined, 'box'];
const REASONS = [undefined, 'damaged', 'expired', 'internal', 'lost', 'sold', 'used'];

function iso(ms) {
  return new Date(ms).toISOString().slice(0, 10);
}

function lotList(r, now) {
  return Array.from({ length: r.int(0, 5) }, () => ({
    date: r.next() < 0.2 ? pick(r, ['', 'junk', '2027-1-1']) : iso(now + r.int(-40, 120) * DAY),
    qty: r.next() < 0.1 ? pick(r, [0, -2]) : r.int(1, 40) / pick(r, [1, 1, 2, 4]),
  }));
}

export default {
  fn: 'demandRates / coverDays / suggestedThreshold / runningOut / rateLabel / coverLabel / suggestedQty / onOrderQty / stockStatus / lots',
  module: `${LIB}demand.js`,
  modules: { demand: `${LIB}demand.js`, reorder: `${LIB}reorder.js`, lots: `${LIB}lots.js`, inventory: `${LIB}inventory.js` },
  paths: [`${LIB}`],
  out: 'modules/resources/tests/golden/inventory_demand.json',
  cases() {
    const r = rng(2603);
    const out = [];
    for (let b = 1; b <= 200; b++) {
      const now = Date.UTC(2026, r.int(0, 11), r.int(1, 28), r.int(0, 23), r.int(0, 59));
      const reach = pick(r, [3, 6, 7, 10, 25, 30, 60, 90]);
      const ids = ['p1', 'p2', 'p3', 'p4'];
      const entries = Array.from({ length: r.int(0, 25) }, () => {
        const kind = pick(r, ['add', 'remove', 'remove', 'recount', 'note']);
        const mag = r.int(1, 30) / pick(r, [1, 1, 2]);
        const delta = kind === 'add' ? mag : kind === 'remove' ? -mag : (r.next() < 0.5 ? mag : -mag);
        const e = { productId: pick(r, [...ids, '']), kind, delta, at: now - r.next() * reach * DAY };
        const why = pick(r, REASONS);
        if (why) e.reason = why;
        return e;
      });
      const days = pick(r, [7, 14, 30, 30, 60]);
      const products = ids.map((id) => {
        const p = { id, quantity: r.next() < 0.15 ? 0 : r.int(0, 80) / pick(r, [1, 1, 2]) };
        const u = pick(r, UNITS);
        if (u !== undefined) p.unit = u;
        if (r.next() < 0.4) p.packSize = pick(r, [1, 6, 12, 2.5, 24]);
        if (r.next() < 0.6) p.lowStock = pick(r, [0, 3, 5, 10, 20]);
        if (r.next() < 0.25) p.reorderQty = pick(r, [10, 24, 50]);
        return p;
      });
      const plans = products.map(() => ({ leadTimeDays: pick(r, [0, 2, 5, 7, 14, 3.6]), safetyDays: pick(r, [0, 3, 3, 5]), coverageDays: pick(r, [0, 0, 14, 30]), incoming: pick(r, [0, 0, 5, 12, 100]) }));
      const orders = Array.from({ length: r.int(0, 3) }, () => ({
        status: pick(r, ['ordered', 'partial', 'received', 'cancelled']),
        lines: Array.from({ length: r.int(1, 3) }, () => {
          const qty = r.int(1, 30);
          return { productId: pick(r, [...ids, '']), qty, received: pick(r, [0, Math.floor(qty / 2), qty, qty + 2]) };
        }),
      }));
      const lots = lotList(r, now);
      const ops = Array.from({ length: 3 }, () => ({ op: pick(r, ['add', 'consume', 'consumeAt', 'resize']), date: r.next() < 0.5 ? (lots[0]?.date ?? '') : iso(now + r.int(-10, 60) * DAY), qty: r.int(0, 50) / pick(r, [1, 2]) }));
      const tracked = ids.map((id) => ({ id, cost: r.int(0, 500) / pick(r, [1, 4]), archived: r.next() < 0.1, trackExpiry: r.next() < 0.8, lots: lotList(r, now) }));
      out.push({ now, today: iso(now), days, entries, products, plans, orders, lots, ops, horizon: pick(r, [7, 30, 60]), tracked, covers: [null, 0, 0.4, 1, 1.04, 9.96, 12.4] });
    }
    return out;
  },
  run(m, c) {
    const { demand, reorder, lots, inventory } = m;
    const dr = demand.demandRates(c.entries, { days: c.days, now: c.now });
    const rows = c.products.map((p, i) => {
      const plan = c.plans[i];
      const perDay = dr.rates.get(p.id) || 0;
      return {
        perDay,
        cover: demand.coverDays(p, perDay),
        threshold: demand.suggestedThreshold(p, { perDay, leadTimeDays: plan.leadTimeDays, safetyDays: plan.safetyDays }),
        runningOut: demand.runningOut(p, { perDay, leadTimeDays: plan.leadTimeDays, safetyDays: plan.safetyDays }),
        restock: demand.restockDays(plan.leadTimeDays, plan.safetyDays),
        rateLabel: demand.rateLabel(perDay, p.unit),
        qty: reorder.suggestedQty(p, 5, plan.incoming, { perDay, coverageDays: plan.coverageDays }),
        status: inventory.stockStatus(p, 5),
      };
    });
    let cur = c.lots;
    const steps = c.ops.map((o) => {
      if (o.op === 'add') cur = lots.addLot(cur, o.date, o.qty);
      else if (o.op === 'resize') cur = lots.resizeLots(cur, o.qty);
      else {
        const res = o.op === 'consume' ? lots.consumeLots(cur, o.qty) : lots.consumeLotAt(cur, o.date, o.qty);
        cur = res.lots;
        return { lots: cur, taken: res.taken };
      }
      return { lots: cur, taken: null };
    });
    return {
      rates: { days: dr.days, enough: dr.enough, rates: Object.fromEntries([...dr.rates.entries()].sort()) },
      rows,
      onOrder: Object.fromEntries([...reorder.onOrderQty(c.orders).entries()].sort()),
      normalized: lots.normalizeLots(c.lots),
      total: lots.lotsTotal(c.lots),
      steps,
      expiry: lots.expiryStatus({ trackExpiry: true, lots: { a: c.lots } }, { days: c.horizon, now: c.now }),
      expiring: lots.expiringProducts(c.tracked.map((t) => ({ ...t, lots: { main: t.lots } })), { days: c.horizon, now: c.now })
        .map((e) => ({ id: e.product.id, expired: e.expired, soon: e.soon, soonest: e.soonest, value: e.value })),
      coverLabels: c.covers.map((x) => demand.coverLabel(x)),
    };
  },
};
