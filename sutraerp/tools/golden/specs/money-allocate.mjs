// kernel::money::allocate  <-  accounting/src/lib/money.js allocate(totalCents, weights)
import { rng } from '../rng.mjs';

export default {
  fn: 'allocate',
  module: 'src/accounting/src/lib/money.js',
  paths: ['src/accounting/src/lib/money.js'],
  out: 'crates/kernel/tests/golden/money_allocate.json',
  // Inputs are integer minor units (paise) and integer weights. Magnitudes stay small enough
  // (|total * weight| < 2^53) that the original float implementation is exact.
  cases() {
    const fixed = [
      [10000, [1, 1, 1]],
      [100, [1, 1, 1]],
      [1, [3, 3, 3]],
      [9999, [12345, 1, 77]],
      [-100, [1, 2]],
      [55, [-100, 10, -9]],
      [-99, [-100, 10, -9]],
      [500, [0, 0]],
      [0, [1, 2, 3]],
      [10000100, [40, 30, 30]],
      [1234567, [1, 1, 1, 1, 1, 1, 1]],
      [100, []],
    ];
    const r = rng(20261016);
    const random = [];
    for (let i = 0; i < 300; i++) {
      const n = r.int(1, 8);
      const total = r.int(-5_000_000, 50_000_000);
      const neg = r.next() < 0.15;
      const weights = Array.from({ length: n }, () => (neg && r.next() < 0.3 ? -1 : 1) * r.int(0, 10_000));
      random.push([total, weights]);
    }
    return [...fixed, ...random].map(([total, weights]) => ({ total, weights }));
  },
  run(mod, { total, weights }) {
    return mod.allocate(total, weights);
  },
};
