// parse_statement_date  <-  accounting/src/lib/bankimport.js parseStatementDate(text, style)
import { rng } from '../rng.mjs';
import { dateText } from './_bank.mjs';

export default {
  fn: 'parseStatementDate',
  module: 'src/accounting/src/lib/bankimport.js',
  paths: ['src/accounting/src/lib/'],
  out: 'modules/finance/tests/golden/bankimport_date.json',
  cases() {
    const fixed = ['03/04/2026', '13/04/2026', '04/13/2026', '31/02/2026', '29/02/2028', '2026-02-30', '20260704', '2026070', 'Mar 4, 2026', 'Sept 4 2026', '4 Mar 2026', '04-Mar-26', 'Foo 4, 2026', '3/4/26', '3/4/202', '2026/7/4', '  2026-07-04  '];
    const r = rng(1601);
    const random = Array.from({ length: 400 }, () => dateText(r));
    return [...fixed, ...random].flatMap((text) => [{ text, style: 'mdy' }, { text, style: 'dmy' }]);
  },
  run(mod, { text, style }) {
    return mod.parseStatementDate(text, style);
  },
};
