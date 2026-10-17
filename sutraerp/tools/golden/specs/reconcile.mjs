// reconcile  <-  accounting/src/lib/{reconcile,banking,reports,ledger}.js
//   candidates / summarize / finishRecord / beginningBalance / lastReconciliation (undo),
//   reconDiscrepancy and findMatches over random books on one bank account.
import { rng } from '../rng.mjs';
import { LIB, pick } from './_bank.mjs';

const hex = (n) => n.toString(16).padStart(12, '0');
const uid = (book, n) => `0192f5a1-7c3e-7000-8000-${hex(book * 1000 + n)}`;
const iso = (n) => new Date(Date.UTC(2025, 3, 1) + n * 86400000).toISOString().slice(0, 10);
const sample = (r, arr, p) => arr.filter(() => r.next() < p);

function book(r, b) {
  const bank = uid(b, 1);
  const card = uid(b, 2);
  const others = [uid(b, 3), uid(b, 4), uid(b, 5)];
  const accounts = [
    { id: bank, name: 'Bank', type: 'bank' },
    { id: card, name: 'Card', type: 'creditCard' },
    { id: others[0], name: 'Fees', type: 'income' },
    { id: others[1], name: 'Salaries', type: 'expense' },
    { id: others[2], name: 'Fund', type: 'equity' },
  ];
  const txns = [];
  const n = r.int(3, 40);
  // A few repeated amounts so matching has ties to break.
  const common = Array.from({ length: 4 }, () => r.int(100, 2_000_000));
  for (let t = 0; t < n; t++) {
    const lines = [];
    const amount = r.next() < 0.35 ? pick(r, common) : r.int(1, 5_000_000);
    const into = r.next() < 0.5;
    const where = r.next() < 0.85 ? bank : card;
    if (r.next() < 0.12) {
      // Two postings on the same account in one entry (they add up).
      const part = r.int(1, amount);
      lines.push({ accountId: where, debit: into ? part : 0, credit: into ? 0 : part });
      lines.push({ accountId: where, debit: into ? amount - part : 0, credit: into ? 0 : amount - part });
    } else {
      lines.push({ accountId: where, debit: into ? amount : 0, credit: into ? 0 : amount });
    }
    lines.push({ accountId: pick(r, others), debit: into ? 0 : amount, credit: into ? amount : 0 });
    if (r.next() < 0.05) {
      // An entry that nets to zero on the bank (in and out the same day).
      lines.push({ accountId: bank, debit: 777, credit: 0 });
      lines.push({ accountId: bank, debit: 0, credit: 777 });
    }
    txns.push({ id: uid(b, 100 + t), type: 'journal', num: '', date: iso(r.int(0, 200)), createdAt: r.int(0, 5), lines });
  }
  return { accounts, bank, card, txns };
}

const postingsOf = (t) => t.lines.map((l) => ({ accountId: l.accountId, amount: l.debit - l.credit }));

export default {
  fn: 'candidates / summarize / finishRecord / beginningBalance / reconDiscrepancy / findMatches',
  module: `${LIB}reconcile.js`,
  modules: { reconcile: `${LIB}reconcile.js`, banking: `${LIB}banking.js`, reports: `${LIB}reports.js`, ledger: `${LIB}ledger.js` },
  paths: [LIB],
  out: 'modules/finance/tests/golden/reconcile.json',
  cases() {
    const r = rng(1611);
    const out = [];
    for (let b = 1; b <= 120; b++) {
      const { accounts, bank, card, txns } = book(r, b);
      const amountOn = (t, acc) => postingsOf(t).filter((p) => p.accountId === acc).reduce((s, p) => s + p.amount, 0);
      // Finished reconciliations (some on the card, one maybe a draft), recorded amounts mostly
      // the real ones, sometimes missing (fallback to the current amount) or stale (edited since).
      const recons = [];
      const used = new Set();
      const nr = r.int(0, 3);
      for (let k = 0; k < nr; k++) {
        const acc = r.next() < 0.8 ? bank : card;
        const pool = txns.filter((t) => amountOn(t, acc) !== 0 && (!used.has(t.id) || r.next() < 0.1));
        const cleared = sample(r, pool, 0.4).map((t) => t.id);
        cleared.forEach((id) => used.add(id));
        const amounts = {};
        for (const id of cleared) {
          const t = txns.find((x) => x.id === id);
          const roll = r.next();
          if (roll < 0.1) continue;
          amounts[id] = roll < 0.2 ? amountOn(t, acc) + r.int(-5000, 5000) : amountOn(t, acc);
        }
        if (r.next() < 0.05) cleared.push(uid(b, 990 + k)); // a deleted transaction
        recons.push({
          id: uid(b, 900 + k), accountId: acc, statementDate: iso(r.int(20, 220)), finishedAt: r.int(1, 3),
          status: r.next() < 0.9 ? 'done' : 'draft', clearedIds: cleared, amounts,
        });
      }
      // Bank-feed lines: "added" ones clear their transaction; matched ones are taken.
      const bankLines = sample(r, txns, 0.3).map((t, i) => ({
        id: uid(b, 700 + i), accountId: r.next() < 0.9 ? bank : card, txnId: t.id, status: pick(r, ['added', 'added', 'matched']),
      }));
      const statementDate = r.next() < 0.15 ? null : iso(r.int(0, 220));
      const pickTxn = pick(r, txns);
      const lineAmount = r.next() < 0.7 ? amountOn(pickTxn, bank) || r.int(-50000, 50000) || 1 : r.int(-2_000_000, 2_000_000) || 1;
      const line = { id: uid(b, 800), accountId: bank, date: iso(r.int(0, 210)), amount: lineAmount };
      const docs = Array.from({ length: r.int(0, 6) }, (_, i) => {
        const open = r.next() < 0.5 ? Math.abs(lineAmount) : r.int(1, 2_000_000);
        return {
          id: uid(b, 600 + i), type: pick(r, ['invoice', 'bill']), date: iso(r.int(0, 210)),
          dueDate: r.next() < 0.8 ? iso(r.int(0, 240)) : null, voided: r.next() < 0.1, open: r.next() < 0.15 ? 0 : open,
        };
      });
      out.push({
        accounts, txns, recons, bankLines, statementDate, line, docs,
        window: pick(r, [null, null, 3, 30]),
        selectP: pick(r, [0, 0.5, 0.9, 1]),
        endingDelta: pick(r, [0, 0, 0, r.int(-100000, 100000)]),
        selectSeed: r.int(1, 1 << 30),
      });
    }
    return out;
  },
  run(m, c) {
    const accounts = new Map(c.accounts.map((a) => [a.id, a]));
    const txnMap = new Map(c.txns.map((t) => [t.id, t]));
    for (const d of c.docs) txnMap.set(d.id, d);
    const recons = new Map(c.recons.map((x) => [x.id, x]));
    const bankLines = new Map(c.bankLines.map((x) => [x.id, x]));
    let L = null;
    const store = {
      recons,
      bankLines,
      txns: txnMap,
      account: (id) => accounts.get(id) || null,
      accountName: (id) => accounts.get(id)?.name || '',
      txn: (id) => (c.txns.find((t) => t.id === id) ? txnMap.get(id) : null),
      ledgerPostingsOf: (t) => (t.lines ? postingsOf(t) : []),
      ledger: () => (L ||= new m.ledger.Ledger(c.txns, {}, 'accrual', c.accounts.map((a) => a.id))),
      reconciledIds(accountId) {
        const s = new Set();
        for (const x of recons.values()) if (x.accountId === accountId && x.status === 'done') for (const id of x.clearedIds || []) s.add(id);
        return s;
      },
      clearedIds(accountId) {
        const s = new Set();
        for (const l of bankLines.values()) if (l.accountId === accountId && l.txnId && l.status === 'added') s.add(l.txnId);
        return s;
      },
      docInfo: () => ({ open: new Map(c.docs.map((d) => [d.id, d.open])) }),
    };
    const bank = c.accounts[0].id;
    const items = m.reconcile.candidates(store, bank, c.statementDate);
    // Deterministic selection from the seed (a tiny LCG, mirrored in the Rust test).
    let seed = c.selectSeed;
    const selected = new Set();
    for (const it of items) {
      seed = (seed * 48271) % 2147483647;
      if (seed / 2147483647 < c.selectP) selected.add(it.txn.id);
    }
    const begin = m.reconcile.beginningBalance(store, bank);
    let clearedSum = begin;
    for (const it of items) if (selected.has(it.txn.id)) clearedSum += it.amount;
    const endingBalance = clearedSum + c.endingDelta;
    const summary = m.reconcile.summarize(store, bank, { endingBalance, selected, items });
    const fin = m.reconcile.finishRecord(store, { accountId: bank, statementDate: c.statementDate || '2025-12-31', endingBalance, selected });
    const last = m.reconcile.lastReconciliation(store, bank);
    const disc = m.reports.reconDiscrepancy(store).rows.filter((x) => !x.muted).map((x) => [x.values[0], x.values[1], x.txnId, x.values[5], x.values[6], x.values[7]]);
    const matches = m.banking.findMatches(store, c.line, c.window ? { window: c.window } : {})
      .map((x) => [x.kind, x.txn.id, Math.round(x.score * 100)]);
    return {
      candidates: items.map((x) => [x.txn.id, x.amount, x.cleared]),
      selected: [...selected],
      beginning: begin,
      summary,
      finish: { beginningBalance: fin.beginningBalance, amounts: fin.amounts },
      last: last ? last.id : null,
      discrepancies: disc,
      matches,
    };
  },
};
