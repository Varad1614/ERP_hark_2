// statements  <-  accounting/src/lib/{reports,ledger,accounts,dates}.js
//   profitAndLoss, balanceSheet, cashFlow, generalLedger over random books (journal entries),
//   fiscal year starting in April as in India.
import { rng } from '../rng.mjs';
import { LIB, pick } from './_bank.mjs';

const TYPES = ['bank', 'ar', 'otherCurrentAsset', 'fixedAsset', 'otherAsset', 'ap', 'creditCard', 'otherCurrentLiability',
  'longTermLiability', 'equity', 'income', 'otherIncome', 'cogs', 'expense', 'otherExpense'];
const CLS = { bank: 'a', ar: 'a', otherCurrentAsset: 'a', fixedAsset: 'a', otherAsset: 'a', ap: 'l', creditCard: 'l', otherCurrentLiability: 'l',
  longTermLiability: 'l', equity: 'e', income: 'i', otherIncome: 'i', cogs: 'x', expense: 'x', otherExpense: 'x' };
const WORDS = ['Alpha', 'Bravo', 'Cedar', 'Delta', 'Ember', 'Falcon', 'Garnet', 'Harbor', 'Indigo', 'Juniper', 'Kestrel', 'Lotus',
  'Maple', 'Nectar', 'Onyx', 'Pepper', 'Quartz', 'Raven', 'Saffron', 'Tamarind', 'Umber', 'Vetiver', 'Willow', 'Xenon', 'Yarrow', 'Zinnia'];
const hex = (n) => n.toString(16).padStart(12, '0');
const uid = (book, n) => `0192f5a0-7c3e-7000-8000-${hex(book * 1000 + n)}`;
const iso = (n) => new Date(Date.UTC(2024, 0, 1) + n * 86400000).toISOString().slice(0, 10);

function book(r, b) {
  const accounts = [];
  const n = r.int(4, 14);
  const words = [...WORDS];
  for (let i = words.length - 1; i > 0; i--) {
    const j = r.int(0, i);
    [words[i], words[j]] = [words[j], words[i]];
  }
  words.length = n + 1;
  // Always a bank, an income and an expense account, and the accumulated fund.
  const types = ['bank', 'income', 'expense', ...Array.from({ length: n - 3 }, () => pick(r, TYPES))];
  types.forEach((type, i) => {
    const sameClass = accounts.filter((a) => CLS[a.type] === CLS[type] && !a.parentId);
    const parentId = sameClass.length && r.next() < 0.25 ? pick(r, sameClass).id : null;
    accounts.push({ id: uid(b, i + 1), name: words[i], type: parentId ? accounts.find((a) => a.id === parentId).type : type, parentId, number: '', active: true });
  });
  accounts.push({ id: uid(b, 999), name: `${words[n]}Fund`, type: 'equity', parentId: null, number: '', role: 'retainedEarnings', active: true });
  const txns = [];
  const m = r.int(3, 40);
  for (let t = 0; t < m; t++) {
    const k = r.int(2, 4);
    const lines = [];
    let total = 0;
    for (let j = 0; j < k - 1; j++) {
      const amt = r.int(1, 500_000);
      const debit = r.next() < 0.5;
      lines.push({ accountId: pick(r, accounts).id, debit: debit ? amt : 0, credit: debit ? 0 : amt });
      total += debit ? amt : -amt;
    }
    if (total === 0) total = 1;
    lines.push({ accountId: pick(r, accounts).id, debit: total < 0 ? -total : 0, credit: total > 0 ? total : 0 });
    txns.push({ id: uid(b, 500 + t), type: 'journal', date: iso(r.int(0, 1100)), createdAt: t, lines });
  }
  return { accounts, txns };
}

export default {
  fn: 'profitAndLoss / balanceSheet / cashFlow / generalLedger',
  module: `${LIB}reports.js`,
  modules: { reports: `${LIB}reports.js`, ledger: `${LIB}ledger.js`, accounts: `${LIB}accounts.js` },
  paths: [LIB],
  out: 'modules/finance/tests/golden/statements.json',
  cases() {
    const r = rng(1610);
    const out = [];
    for (let b = 1; b <= 80; b++) {
      const { accounts, txns } = book(r, b);
      const a = r.int(60, 900);
      const from = iso(a);
      const to = iso(a + pick(r, [0, 29, 30, 89, 180, 364, 500]));
      const asOf = iso(r.int(0, 1150));
      out.push({
        accounts, txns, from, to, asOf,
        columns: pick(r, ['total', 'total', 'month', 'quarter', 'year']),
        compare: pick(r, [null, null, 'prevYear', 'prevPeriod']),
        sheet: pick(r, ['total', 'total', 'month', 'quarter', 'prevYear']),
      });
    }
    return out;
  },
  run(m, c) {
    const byId = new Map(c.accounts.map((a) => [a.id, a]));
    const chart = m.accounts.sortChart(c.accounts);
    let L = null;
    const store = {
      settings: { fiscalStartMonth: 4 },
      account: (id) => byId.get(id) || null,
      chart: () => chart,
      accountName: (id, opts) => (byId.has(id) ? m.accounts.accountLabel(byId.get(id), byId, { withNumber: true, ...opts }) : ''),
      contactName: () => '',
      ledgerPostingsOf: () => [],
      roleMap: () => new Map([['retainedEarnings', c.accounts.find((a) => a.role === 'retainedEarnings').id]]),
      ledger: () => (L ||= new m.ledger.Ledger(c.txns, {}, 'accrual', chart.map((x) => x.account.id))),
    };
    const half = (cur, prev) => prev && ((cur - prev) * 20000) % Math.abs(prev) === 0 && Math.abs(((cur - prev) * 20000) / Math.abs(prev)) % 2 === 1;
    const compact = (rows, pct) => rows.map((x) => {
      const v = x.values.map((y) => (y === undefined ? null : y));
      // A change of exactly half a basis point can round apart between floats and decimals.
      if (pct && v.length === 4 && v[3] !== null && half(v[0], v[1])) v[3] = 'tie';
      return [x.kind, x.depth, x.accountId && !(x.drill && x.drill.report) ? x.accountId : x.label, v];
    });
    const pnl = m.reports.profitAndLoss(store, { from: c.from, to: c.to, columns: c.compare ? 'total' : c.columns, compare: c.compare });
    const bs = m.reports.balanceSheet(store, { asOf: c.asOf, compare: c.sheet === 'prevYear' ? 'prevYear' : null, columns: c.sheet === 'prevYear' ? 'total' : c.sheet });
    const cf = m.reports.cashFlow(store, { from: c.from, to: c.to });
    const gl = m.reports.generalLedger(store, { from: c.from, to: c.to });
    return {
      pnl: { net: pnl.net, cols: pnl.columns.length - 1, rows: compact(pnl.rows, !!c.compare) },
      bs: { balanced: bs.balanced, cols: bs.columns.length - 1, rows: compact(bs.rows, false) },
      cf: { net: cf.check.net, cash: cf.check.cashChange, rows: compact(cf.rows, false) },
      gl: gl.rows.map((x) => [x.kind, x.accountId || (x.label === 'Beginning Balance' ? 'open' : x.values[0]), x.kind === 'section' ? [] : x.values.slice(6).filter((y) => y !== null)]),
    };
  },
};
