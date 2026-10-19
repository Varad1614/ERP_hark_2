// parse_csv_statement_original  <-  accounting/src/lib/bankimport.js parseCSVStatement(text, opts)
import { rng } from '../rng.mjs';
import { amountText, dateText, pick } from './_bank.mjs';

const LAYOUTS = [
  ['Date', 'Description', 'Amount'],
  ['Transaction Date', 'Payee', 'Debit', 'Credit', 'Balance'],
  ['Posted Date', 'Memo', 'Withdrawals', 'Deposits'],
  ['Date', 'Details', 'Money Out', 'Money In'],
  ['Value Date', 'Narrative', 'Debit/Credit', 'Amount'],
  ['Date', 'Narration', 'Chq./Ref.No.', 'Value Dt', 'Withdrawal Amt.', 'Deposit Amt.', 'Closing Balance'],
  null, // no header row
];

export default {
  fn: 'parseCSVStatement',
  module: 'src/accounting/src/lib/bankimport.js',
  paths: ['src/accounting/src/lib/'],
  out: 'modules/finance/tests/golden/bankimport_statement.json',
  cases() {
    const r = rng(1605);
    const out = [];
    for (let i = 0; i < 150; i++) {
      const layout = pick(r, LAYOUTS);
      const width = layout ? layout.length : 3;
      const rows = layout ? [layout] : [];
      for (let k = 0, n = r.int(0, 8); k < n; k++) {
        const row = Array.from({ length: width }, (_, c) => {
          const h = (layout ? layout[c] : ['Date', 'Desc', 'Amount'][c]).toLowerCase();
          if (h.includes('date') || h.includes('dt')) return dateText(r);
          if (/amount|debit|credit|withdraw|deposit|money|balance/.test(h) && h !== 'debit/credit') return r.next() < 0.3 ? '' : amountText(r);
          if (h === 'debit/credit') return pick(r, ['Debit', 'Credit', 'DR', 'CR']);
          return pick(r, ['UPI/Ravi  Kumar', 'NEFT-SBIN-Fees', 'ATM', '"Cafe, Pune"', '', 'Chq 000123']);
        });
        rows.push(row.map((c) => (/[,"\n]/.test(c) && !c.startsWith('"') ? `"${c.replace(/"/g, '""')}"` : c)).join(','));
      }
      const text = rows.map((x) => (Array.isArray(x) ? x.join(',') : x)).join('\n');
      out.push({ text, dateStyle: pick(r, ['mdy', 'dmy']), invert: r.next() < 0.2 });
    }
    return out;
  },
  run(mod, { text, dateStyle, invert }) {
    const res = mod.parseCSVStatement(text, { dateStyle, invert });
    return { headers: res.headers, hasHeader: res.hasHeader ?? null, mapping: res.mapping, lines: res.lines, skipped: res.skipped };
  },
};
