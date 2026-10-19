// dedupe  <-  accounting/src/lib/bankimport.js dedupe(newLines, existingLines)
import { rng } from '../rng.mjs';
import { pick } from './_bank.mjs';

function line(r) {
  return {
    date: pick(r, ['2026-07-01', '2026-07-02', '2026-07-03']),
    amount: pick(r, [100, -100, 2500]),
    desc: pick(r, ['Coffee', 'coffee', ' COFFEE  shop', 'coffee shop', 'Tea', '']),
    fitid: pick(r, ['', '', 'F1', 'F2']),
  };
}

export default {
  fn: 'dedupe',
  module: 'src/accounting/src/lib/bankimport.js',
  paths: ['src/accounting/src/lib/'],
  out: 'modules/finance/tests/golden/bankimport_dedupe.json',
  cases() {
    const r = rng(1607);
    return Array.from({ length: 300 }, () => ({
      fresh: Array.from({ length: r.int(0, 6) }, () => line(r)),
      existing: Array.from({ length: r.int(0, 6) }, () => line(r)),
    }));
  },
  run(mod, { fresh, existing }) {
    const res = mod.dedupe(fresh, existing);
    return fresh.map((l) => !res.fresh.includes(l));
  },
};
