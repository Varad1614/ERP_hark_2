// escpos  <-  pos/src/lib/escpos.js
//   encodeCp437 (NFC, transliteration, CP437 accents, '?' fallback), printWidth, wrap,
//   EscposBuilder.columnsLine / wrapped (indent, scale), escposReceipt (32 and 48 columns, kick,
//   voided, kind, items with subs, long and accented names, euro/yen/minus/curly quotes/ellipsis),
//   escposReport, drawerKickBytes. Bytes are arrays of numbers.
//   '₹' is deliberately absent from every input: the Rust port prints it as 'Rs.' (JS: 'INR').
//   Item subs are plain strings in the input (the Rust SlipItem shape); run() wraps them as
//   { text } for the original.
import { rng } from '../rng.mjs';

const pick = (r, a) => a[r.int(0, a.length - 1)];
const some = (r, a, lo, hi) => Array.from({ length: r.int(lo, hi) }, () => pick(r, a));
const bytes = (u8) => [...u8];

const TEXTS = [
  'Latte $4.60', 'Café', 'Cafe\u0301', 'Crème Brûlée', 'Naïve résumé', 'Ångström Æther', 'Señor Muñoz',
  'Straße · Größe', 'CN¥12.50', '¥13', '€24.68', '\u221212.00', '–dash—em', '‘single’ “double”', 'more…',
  '2×', '• bullet → arrow ↑ ↓', '₩1,250', '₺40 ₽99', '日本', 'पुणे विद्यापीठ', 'tab\there', 'cr\rlf',
  'nbsp\u00a0here', '', ' ', 'line one\nline two', '\n\n', '£3 ¢5 ±2 ÷4 °C µg ² ª º ¿', 'Ölçü Ü ö',
  'Sree Saraswati Vidya Mandir', 'Fr. Agnel Ashram Bandstand', 'Deshpande & Sons', 'A'.repeat(60),
];
const WORDS = [
  'one', 'two', 'three', 'four', 'fees', 'Tuition', 'Café', 'Crème', '€5', '¥7', '…', '−3.00', '“quoted”',
  'supercalifragilisticexpialidocious', 'Antidisestablishmentarianism', 'x', 'ab', 'Bedankt…', 'tot',
  'morgen', 'Hôtel', 'Élysée', 'पुणे', '2026-08-20', '#1042', 'Rs.', 'INR', 'GSTIN', '27AAACB1234F1Z5',
];
const SEPS = [' ', ' ', ' ', '  ', '\t', '\u00a0', '\n', ' \n ', '\u2003', '\u3000'];
const sentence = (r, lo, hi) => {
  const words = some(r, WORDS, lo, hi);
  let s = '';
  words.forEach((w, i) => { s += (i ? pick(r, SEPS) : '') + w; });
  if (r.next() < 0.1) s = `  ${s}  `;
  return s;
};

const STORES = [
  'Corner Roast Café', 'The Corner Roast Coffee House', 'Sree Saraswati Vidya Mandir High School',
  'Fergusson College', 'Modern Education Society’s College of Engineering', 'École Française',
  'Shop', 'X', '', 'Ahilyanagar Public School & Junior College, Savedi Road',
];
const ADDRESSES = ['', '12 Market Lane', 'Savedi Road, Ahilyanagar 414003', 'Plot 7, MIDC — Phase II, Pune 411018', 'Rue de l’Église, Pondichéry'];
const REGS = ['GSTIN 27AAACB1234F1Z5', 'PAN AAACB1234F', 'VAT No. GB123', 'UDISE 27260100101', ''];
const KINDS = [null, null, null, 'REFUND', 'DUPLICATE', 'TEST PRINT', 'COPY', ''];
const METAS = [
  'Receipt #1042', 'Receipt FR/2026-27/000123', 'Thursday, August 20, 2026', '20 Aug 2026 · 10:42',
  'Student: Aarav Kulkarni (STU-0042)', 'Class 10 · Div B', 'Cashier: Priya', 'CANCELLED',
  'Paid by: Mr. Rajesh Deshpande, guardian of the student named above', 'Ref: UPI 412345678901',
];
const NAMES = [
  'Espresso', 'Tuition fee — Term 1', 'Transport fee (Route 7)', 'Late fee', 'Exam fee', 'Crème Brûlée',
  'Library deposit (refundable)', 'Hostel mess charges for the month of August 2026',
  'Laboratory consumables and breakage', '“Special” activity', 'Café lunch × 20 days', 'Ölçü kit',
  'Uniform', 'A very long product name that will certainly need shortening on any paper', 'पुस्तके',
];
const AMOUNTS = [
  '$6.40', '6.40', '12,500.00', '1,25,000.00', '€24.68', '€1,234.56', '¥1,300', 'CN¥12.50', '−50.00',
  '\u2212€7.01', '0.00', '£3.20', '₩12,500', '12345678901234567890123456789012345', '', 'Rs. 450.00',
];
const QTYS = ['1x', '2x', '2×', '10 ×', '1', '', '12'];
const SUBS = ['no ice', 'Term 1 · 2026–27', 'Concession: sibling 10%', 'includes GST @ 18%', 'Due 15 Aug — paid late…', 'Instalment 2 of 3', 'Scholarship “Merit” applied'];
const LABELS = ['Subtotal', 'Total', 'Concession', 'Late fee waived', 'Sales tax 7.75%', 'Balance due', 'Amount received', 'Total in words: Rupees One Lakh Twenty-Five Thousand Only', 'Cash', 'UPI', 'Card', 'Change', 'Cheque #001234', 'Total (€)'];
const NOTES = ['', '', 'for Sam', 'Fees once paid are not refundable except as per policy.', 'Paid at the counter… thank you', 'Rupees Twelve Thousand Five Hundred Only'];
const FOOTERS = ['', 'Thanks, see you tomorrow', 'Bedankt… tot morgen', 'This is a computer-generated receipt and needs no signature.', 'Keep this slip for your records — duplicate slips are marked'];
const SECTIONS = ['Takings', 'By channel', 'Café sales', 'Straße', 'cancelled', 'Refunds & voids'];

const row = (r) => ({ label: pick(r, LABELS), value: pick(r, AMOUNTS), strong: r.next() < 0.3 });

function slipDoc(r) {
  return {
    storeName: pick(r, STORES),
    address: pick(r, ADDRESSES),
    registrations: some(r, REGS, 0, 2),
    voided: r.next() < 0.2,
    kind: pick(r, KINDS),
    meta: some(r, METAS, 0, 4),
    items: Array.from({ length: r.int(0, r.next() < 0.15 ? 25 : 5) }, () => ({
      qty: pick(r, QTYS),
      name: pick(r, NAMES),
      amount: pick(r, AMOUNTS),
      subs: some(r, SUBS, 0, r.next() < 0.2 ? 4 : 1),
    })),
    totals: Array.from({ length: r.int(0, 4) }, () => row(r)),
    payments: Array.from({ length: r.int(0, 3) }, () => row(r)),
    note: pick(r, NOTES),
    reward: pick(r, ['', '', 'a free espresso', 'Early-bird concession applied']),
    loyalty: pick(r, ['', '', '1 of 3 toward a free espresso', 'Next instalment due 15 Nov 2026']),
    footer: pick(r, FOOTERS),
  };
}

export default {
  fn: 'escpos: encodeCp437 / printWidth / wrap / EscposBuilder / escposReceipt / escposReport / drawerKickBytes',
  module: 'src/pos/src/lib/escpos.js',
  paths: ['src/pos/src/lib/escpos.js'],
  out: 'modules/finance/tests/golden/escpos.json',
  cases() {
    const r = rng(3507);
    const out = [];
    for (let i = 0; i < 200; i++) {
      const texts = [...some(r, TEXTS, 2, 5), sentence(r, 1, 6)];
      const wraps = Array.from({ length: r.int(1, 4) }, () => [
        r.next() < 0.5 ? sentence(r, 0, 14) : pick(r, TEXTS),
        pick(r, [1, 2, 3, 5, 6, 9, 10, 16, 20, 24, 32, 48]),
      ]);
      const ops = Array.from({ length: r.int(1, 6) }, () => (r.next() < 0.55
        ? { op: 'columnsLine', label: r.next() < 0.5 ? pick(r, NAMES) : pick(r, LABELS), amount: pick(r, AMOUNTS), indent: pick(r, [0, 0, 0, 2, 4, 30]) }
        : { op: 'wrapped', value: sentence(r, 0, 12), indent: pick(r, [0, 0, 2, 5]), scale: pick(r, [1, 1, 2, 3]) }));
      const report = {
        head: {
          storeName: pick(r, STORES),
          address: pick(r, ADDRESSES),
          kind: pick(r, ['Z-REPORT', 'COUNTER DAY CLOSE', 'X-REPORT', '']),
          meta: some(r, METAS, 0, 3),
          footer: pick(r, FOOTERS),
        },
        rows: Array.from({ length: r.int(0, 8) }, () => (r.next() < 0.25
          ? { label: '', value: '', section: pick(r, SECTIONS), strong: false }
          : { ...row(r), section: r.next() < 0.1 ? '' : null })),
        columns: pick(r, [32, 48]),
      };
      out.push({
        texts,
        wraps,
        builder: { columns: pick(r, [16, 20, 32, 48]), ops },
        receipt: { doc: slipDoc(r), columns: pick(r, [32, 48]), kick: r.next() < 0.4 },
        report,
        connector: pick(r, [0, 1]),
      });
    }
    return out;
  },
  run(m, c) {
    const b = new m.EscposBuilder({ columns: c.builder.columns });
    for (const op of c.builder.ops) {
      if (op.op === 'columnsLine') b.columnsLine(op.label, op.amount, { indent: op.indent });
      else b.wrapped(op.value, { indent: op.indent, scale: op.scale });
    }
    const doc = {
      ...c.receipt.doc,
      items: c.receipt.doc.items.map((it) => ({ ...it, subs: it.subs.map((text) => ({ text })) })),
    };
    return {
      cp437: c.texts.map((t) => m.encodeCp437(t)),
      widths: c.texts.map((t) => m.printWidth(t)),
      wraps: c.wraps.map(([t, w]) => m.wrap(t, w)),
      builder: bytes(b.build()),
      receipt: bytes(m.escposReceipt(doc, { columns: c.receipt.columns, kick: c.receipt.kick })),
      report: bytes(m.escposReport(c.report.head, c.report.rows, { columns: c.report.columns })),
      kick: bytes(m.drawerKickBytes({ connector: c.connector })),
    };
  },
};
