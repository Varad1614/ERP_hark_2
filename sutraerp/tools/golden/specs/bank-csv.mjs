// parse_csv  <-  accounting/src/lib/csv.js parseCSV(text)
import { rng } from '../rng.mjs';
import { pick } from './_bank.mjs';

const TOKENS = ['a', 'b c', ' x ', '"q,x"', '"he said ""hi"""', '"multi\nline"', '', '12.50', '"1,00,000.00"', '₹', 'Dr', '  "sp"', 'ab"cd'];
const SEPS = [',', ';', '\t', '|'];
const EOLS = ['\n', '\r\n', '\r'];

export default {
  fn: 'parseCSV',
  module: 'src/accounting/src/lib/csv.js',
  paths: ['src/accounting/src/lib/'],
  out: 'modules/finance/tests/golden/bankimport_csv.json',
  cases() {
    const fixed = ['', 'a', 'a,b\nc,d', '\ufeffDate;Amount\r\n1;2\r\n', 'a,"b\n\nc",d', '\n\n,\n', 'x|y|z', 'a\tb,c\td', '"unterminated,x'];
    const r = rng(1602);
    const random = [];
    for (let i = 0; i < 250; i++) {
      const sep = pick(r, SEPS);
      const eol = pick(r, EOLS);
      const rows = Array.from({ length: r.int(1, 5) }, () => Array.from({ length: r.int(1, 5) }, () => pick(r, TOKENS)).join(sep));
      random.push(rows.join(eol) + (r.next() < 0.5 ? eol : ''));
    }
    return [...fixed, ...random].map((text) => ({ text }));
  },
  run(mod, { text }) {
    return mod.parseCSV(text);
  },
};
