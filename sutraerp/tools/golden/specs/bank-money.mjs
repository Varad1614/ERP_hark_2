// parse_money  <-  accounting/src/lib/money.js parseMoney(text) -> cents
import { rng } from '../rng.mjs';
import { amountText } from './_bank.mjs';

export default {
  fn: 'parseMoney',
  module: 'src/accounting/src/lib/money.js',
  paths: ['src/accounting/src/lib/'],
  out: 'modules/finance/tests/golden/bankimport_money.json',
  cases() {
    const fixed = ['', '0', '1,00,000.50', '(250.00)', '12,5', '1.234,56', '1.234.567', '10.005', '-₹ 5', '−42.10', '1,234', 'abc', '.5', ',99', '12.', '- 3', '((1))', '1,2,3', '₹ 12,34,567.89 Cr'];
    const r = rng(1604);
    const random = Array.from({ length: 400 }, () => amountText(r));
    return [...fixed, ...random].map((text) => ({ text }));
  },
  run(mod, { text }) {
    return mod.parseMoney(text);
  },
};
