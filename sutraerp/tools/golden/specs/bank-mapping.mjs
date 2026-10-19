// guess_mapping  <-  accounting/src/lib/bankimport.js guessMapping(headers)
import { rng } from '../rng.mjs';
import { pick } from './_bank.mjs';

const POOL = ['Date', 'Transaction Date', 'Posted Date', 'Value Date', 'Booking date', 'Description', 'Payee', 'Name', 'Memo', 'Details', 'Narrative', 'Merchant', 'Amount', 'Transaction Amount', 'Value', 'Amount (USD)', 'Debit', 'Withdrawals', 'Money Out', 'Paid out', 'Charges', 'Credit', 'Deposits', 'Money In', 'Paid in', 'Payments', 'Debit/Credit', 'Balance', 'Reference', 'Category', 'Narration', 'Chq./Ref.No.', 'Withdrawal Amt.', 'Deposit Amt.', 'Closing Balance', 'Dr / Cr', 'Txn Date', 'Particulars', ''];

export default {
  fn: 'guessMapping',
  module: 'src/accounting/src/lib/bankimport.js',
  paths: ['src/accounting/src/lib/'],
  out: 'modules/finance/tests/golden/bankimport_mapping.json',
  cases() {
    const r = rng(1603);
    const out = [['Date', 'Description', 'Amount'], ['Date', 'Debit/Credit', 'Amount'], ['Date', 'Narration', 'Chq./Ref.No.', 'Value Dt', 'Withdrawal Amt.', 'Deposit Amt.', 'Closing Balance'], []];
    for (let i = 0; i < 300; i++) {
      out.push(Array.from({ length: r.int(1, 7) }, () => {
        const h = pick(r, POOL);
        return pick(r, [h, h.toUpperCase(), h.toLowerCase(), ` ${h} `]);
      }));
    }
    return out.map((headers) => ({ headers }));
  },
  run(mod, { headers }) {
    return mod.guessMapping(headers);
  },
};
