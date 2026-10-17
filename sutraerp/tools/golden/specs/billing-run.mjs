// billing-run  <-  childcare/src/core/billing.js
//   periodFor / planBillingRun / planTotals over random rosters (weekly and monthly plans,
//   per-student overrides, inactive students, future start dates, periods already billed).
//   Labels and line descriptions are locale-formatted in the original, so only the plan name
//   part of each line is compared.
import { rng } from '../rng.mjs';

const LIB = 'src/childcare/src/core/';
const pick = (r, a) => a[r.int(0, a.length - 1)];
const hex = (n) => n.toString(16).padStart(12, '0');
const uid = (book, n) => `0192f5a1-7c3e-7000-8000-${hex(book * 1000 + n)}`;
const iso = (n) => new Date(Date.UTC(2025, 0, 1) + n * 86400000).toISOString().slice(0, 10);

export default {
  fn: 'periodFor / planBillingRun / planTotals',
  module: `${LIB}billing.js`,
  paths: [LIB],
  out: 'modules/finance/tests/golden/billing_run.json',
  cases() {
    const r = rng(1702);
    const out = [];
    for (let b = 1; b <= 150; b++) {
      const plans = Array.from({ length: r.int(1, 5) }, (_, i) => ({
        id: uid(b, 10 + i),
        name: `Plan ${b}-${i}`,
        amountCents: r.int(1, 25_000_000),
        frequency: r.next() < 0.6 ? 'monthly' : 'weekly',
      }));
      const day = iso(r.int(0, 900));
      const students = Array.from({ length: r.int(0, 12) }, (_, i) => {
        const mine = plans.filter(() => r.next() < 0.45).map((p) => {
          const sp = { planId: p.id };
          if (r.next() < 0.25) sp.amountCents = r.next() < 0.1 ? 0 : r.int(1, 25_000_000);
          return sp;
        });
        if (r.next() < 0.05) mine.push({ planId: uid(b, 99) }); // a plan since deleted
        const s = { id: uid(b, 100 + i), status: r.next() < 0.85 ? 'active' : 'inactive', plans: mine };
        if (r.next() < 0.4) s.startDate = iso(r.int(0, 960));
        return s;
      });
      const frequency = r.next() < 0.6 ? 'monthly' : 'weekly';
      const weekStart = r.int(0, 6);
      const invoices = students
        .filter(() => r.next() < 0.2)
        .map((s) => ({ studentId: s.id, period: r.next() < 0.7 ? null : 'x', status: r.next() < 0.25 ? 'void' : 'open' }));
      out.push({ plans, students, invoices, frequency, day, dueDays: pick(r, [0, 7, 7, 10, 15, 30]), weekStart });
    }
    return out;
  },
  run(m, c) {
    const period = m.periodFor(c.frequency, c.day, c.weekStart);
    // Already-billed invoices name this period (null in the input means "this period").
    const invoices = c.invoices.map((i) => ({ ...i, period: i.period === null ? period.key : 'monthly:1999-01' }));
    const run = m.planBillingRun({ students: c.students, plans: c.plans, invoices }, { frequency: c.frequency, day: c.day, dueDays: c.dueDays, weekStart: c.weekStart });
    return {
      period: { key: run.period.key, start: run.period.start },
      drafts: run.drafts.map((d) => ({
        studentId: d.studentId,
        issueDate: d.issueDate,
        dueDate: d.dueDate,
        period: d.period,
        items: d.items.map((it) => [it.description.split(', ')[0], it.amountCents]),
      })),
      totals: c.students.map((s) => m.planTotals(s, c.plans)),
    };
  },
};
