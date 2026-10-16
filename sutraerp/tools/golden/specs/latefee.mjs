// latefee  <-  invoicing/src/lib/{money,doc,aging,latefee,reminders}.js
//   scheduleRows (via docTotals), dueDateFor, overdueAmount, lateFeeAmount, daysLate, reminderStage
import { rng } from '../rng.mjs';
import { pick } from './_bank.mjs';

const LIB = 'src/invoicing/src/lib/';
const iso = (d) => d.toISOString().slice(0, 10);
const day = (base, n) => iso(new Date(Date.parse(`${base}T00:00:00Z`) + n * 86400000));

export default {
  fn: 'lateFeeAmount',
  module: `${LIB}latefee.js`,
  modules: { money: `${LIB}money.js`, doc: `${LIB}doc.js`, aging: `${LIB}aging.js`, latefee: `${LIB}latefee.js`, reminders: `${LIB}reminders.js` },
  paths: [LIB],
  out: 'modules/finance/tests/golden/latefee.json',
  cases() {
    const r = rng(1608);
    const out = [];
    while (out.length < 400) {
      const total = r.int(1, 200_000);
      const asOf = day('2026-07-01', r.int(0, 120));
      const n = pick(r, [0, 0, 1, 2, 3, 4]);
      const schedule = Array.from({ length: n }, () => ({
        type: pick(r, ['fixed', 'percent']),
        value: pick(r, [() => r.int(0, 100), () => r.int(0, total), () => r.int(-10, 10)])(),
        dueDate: r.next() < 0.1 ? '' : day('2026-06-01', r.int(0, 150)),
      }));
      const payments = Array.from({ length: pick(r, [0, 1, 2]) }, () => ({ amount: pick(r, [() => r.int(1, total), () => r.int(-500, 0)])() }));
      const doc = {
        items: [{ qty: 1, rate: total }],
        payments,
        dueDate: r.next() < 0.1 ? '' : day('2026-06-15', r.int(0, 100)),
        ...(n ? { schedule } : {}),
      };
      const policy = pick(r, [null, { type: 'fixed', value: r.int(1, 2000) }, { type: 'percent', value: pick(r, [1, 2, 5, 10, 18, 24]) }]);
      out.push({ doc, asOf, policy });
    }
    return out;
  },
  run(m, { doc, asOf, policy }) {
    const t = m.money.docTotals(doc);
    const overdue = m.aging.overdueAmount(doc, asOf);
    const fee = m.latefee.lateFeeAmount(policy, doc, asOf);
    return {
      balance: t.balance,
      installments: t.installments.map((x) => ({ dueDate: x.dueDate, amount: x.amount, paid: x.paid, open: x.open })),
      dueDate: m.doc.dueDateFor(doc),
      overdue,
      // A percentage that lands exactly on half a paisa is where JS floats and exact decimals
      // can round apart; such fees are flagged rather than compared.
      fee: policy?.type === 'percent' && Math.round(overdue * 100) * policy.value % 100 === 50 ? null : fee,
      daysLate: m.reminders.daysLate(doc, asOf),
      stage: m.reminders.reminderStage(doc, asOf),
    };
  },
};
