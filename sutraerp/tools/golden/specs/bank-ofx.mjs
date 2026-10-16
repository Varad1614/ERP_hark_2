// parse_ofx  <-  accounting/src/lib/bankimport.js parseOFX(text)
import { rng } from '../rng.mjs';
import { amountText, pick } from './_bank.mjs';

function trn(r, xml) {
  const tags = {
    TRNTYPE: pick(r, ['DEBIT', 'CREDIT', 'CHECK']),
    DTPOSTED: `2026${String(r.int(1, 12)).padStart(2, '0')}${String(r.int(1, 30)).padStart(2, '0')}${pick(r, ['', '120000', '120000.000[-5:EST]'])}`,
    TRNAMT: pick(r, [() => amountText(r), () => `${r.int(-99999, 99999)}.${String(r.int(0, 99)).padStart(2, '0')}`, () => '0.00'])(),
    FITID: pick(r, ['', `F${r.int(1, 50)}`, `  2026${r.int(100, 999)}  `]),
    NAME: pick(r, ['AT&amp;T', 'Fee &lt;Sem 3&gt;', 'Ravi', '', 'O&apos;Brien']),
    MEMO: pick(r, ['', 'Ravi', 'UPI ref 1234', 'Fees  &quot;Q2&quot;']),
    CHECKNUM: pick(r, ['', '000123']),
  };
  const body = Object.entries(tags).filter(([, v]) => v !== '' || r.next() < 0.3).map(([k, v]) => (xml ? `<${k}>${v}</${k}>` : `<${k}>${v}`)).join('\n');
  return `<STMTTRN>\n${body}\n${xml || r.next() < 0.5 ? '</STMTTRN>' : ''}`;
}

function stmt(r, xml) {
  const card = r.next() < 0.3;
  const tag = card ? 'CCSTMTRS' : 'STMTRS';
  const acct = card ? `<CCACCTFROM><ACCTID>4111${r.int(1000, 9999)}` : `<BANKACCTFROM><BANKID>HDFC<ACCTID> 5020${r.int(1000, 9999)} <ACCTTYPE>${pick(r, ['checking', 'SAVINGS', ''])}`;
  const trns = Array.from({ length: r.int(0, 4) }, () => trn(r, xml)).join('\n');
  const bal = r.next() < 0.7 ? `<LEDGERBAL>\n<BALAMT>${amountText(r)}\n<DTASOF>20260731` : '';
  return `<${tag}>\n<CURDEF>INR\n${acct}\n<BANKTRANLIST>\n${trns}\n</BANKTRANLIST>\n${bal}\n</${tag}>`;
}

export default {
  fn: 'parseOFX',
  module: 'src/accounting/src/lib/bankimport.js',
  paths: ['src/accounting/src/lib/'],
  out: 'modules/finance/tests/golden/bankimport_ofx.json',
  cases() {
    const r = rng(1606);
    const out = ['', 'OFXHEADER:100\n<OFX></OFX>', '<OFX><STMTTRN><DTPOSTED>20260704<TRNAMT>-12.50<NAME>Tea</STMTTRN></OFX>'];
    for (let i = 0; i < 120; i++) {
      const xml = r.next() < 0.4;
      const head = xml ? '<?xml version="1.0"?>\n<?OFX OFXHEADER="200"?>\n<OFX>' : 'OFXHEADER:100\nDATA:OFXSGML\n\n<OFX>';
      const n = r.int(1, 3);
      out.push(`${head}\n${Array.from({ length: n }, () => stmt(r, xml)).join('\n')}\n</OFX>`);
    }
    return out.map((text) => ({ text }));
  },
  run(mod, { text }) {
    return mod.parseOFX(text).statements;
  },
};
