// expense-claims  <-  expense/src/lib/{money,util,dates,currencies,ocr,reports}.js
//   parseAmount (Indian lakh grouping, European decimals, accounting negatives), totalsByCurrency,
//   normalizeText / tokens, parseLooseDate (dayFirst both ways), resolveCurrency, extractJson,
//   matchCategory against Indian claim categories, normalizeOcrResult (raw objects and model
//   text), nextStatus / canTransition / isEditable over every status pair, reportTotals.
//   Run in UTC; `now` is each case's day at 12:00 UTC.
import { rng } from '../rng.mjs';

process.env.TZ = 'UTC';

const LIB = 'src/expense/src/lib/';
const pick = (r, a) => a[r.int(0, a.length - 1)];
const some = (r, a, lo, hi) => Array.from({ length: r.int(lo, hi) }, () => pick(r, a));
const iso = (ms) => new Date(ms).toISOString().slice(0, 10);

const AMOUNTS = [
  '₹ 1,23,456.50', 'Rs. 450', 'Rs.450/-', 'INR 12,00,000', '(12.34)', '12.34-', '-₹ 99', '₹-12', '1.234,56', '12,5', '12,34',
  '1,234', '1,234,567.89', '1.234.567', '0.005', '4.505', ' 7 ', 'abc', '', 'Rs.', '₹ ,', ',', '.', '5.', '.5', '१२३',
  '1,23,456', '€ 40', 'USD -12', 'Total: 2,499.00', '( 300 )', '3.14159', 450, 4.505, -0.5, 0, 12345.678, null,
];
const TEXTS = [
  'Café Coffee Day — MG Road', 'HOTEL SAI PALACE PVT. LTD.', 'शिवाजी नगर पेट्रोल पंप', 'Crème Brûlée & Co.', 'POS PURCHASE 4471 AMAZON.COM',
  'Ola Cabs   Pune\tStation', 'Hôtel Élysée, Paris', 'The Bombay Canteen', 'Śrī Gaṇeśa Bhojanālaya', 'İstanbul Kebab', 'ﬁne STRASSE ß',
  '', '   ', '12 34 a b', 'IndiGo 6E-512 BOM-DEL', 'राज्य परिवहन (MSRTC) Ticket', 'Naïve résumé café', 'x_y-z/w',
];
const JSONS = [
  '{"merchant":"Blue Fern","total":14.6}',
  '```json\n{"total": "₹ 1,250.00", "currency": "INR"}\n```',
  '```\n{"a": 1}\n```',
  'Sure! Here is the data: {"merchant": "Ola", "date": "2026-08-03"} Let me know.',
  '{"note": "brace } inside", "nested": {"x": [1, 2, {"y": "{"}]}}',
  '{"escaped": "quote \\" and } brace", "n": 1}',
  '{"broken": tru}',
  '{"unterminated": "yes"',
  'no json here',
  '',
  '   {"spaced": true}   ',
  'prefix {"a": {"b": {"c": "d"}}} suffix {"second": 1}',
  '{"list": [], "obj": {}, "num": -0.5, "exp": 1e2}',
  '{"dev": "शिवाजी नगर", "acc": "Hôtel"}',
  '}{"after": "stray close"}',
  { already: 'object', total: 5 },
  null,
];
const DATES = [
  '03/08/2026', '08/03/2026', '13/08/2026', '08/13/2026', '3/8/26', '31/12/99', '1/1/70', '2026-08-03', '2026/8/3', '2026.08.03',
  '2026-08-03T10:15:00Z', '2026-08-03 10:15', '12-Aug-2026', '3 Aug 2026', '3rd August 2026', 'Aug 3', 'August 3, 2026', 'Sept. 9 2025',
  '20260803', '31/02/2026', '29/02/2028', '29/02/2027', '2026-13-01', '00/01/2026', '12/08/202', 'Foo 3', '3 Foo', '', '  ', 'yesterday',
  '1899-12-31', '2201-01-01', '15.08.2026', '1-1-2026', 'Dec 31st', '31st Dec, 2025',
];
const CURRENCIES = ['INR', 'inr', ' usd ', '₹', 'Rs.', '$', 'US$', 'R$', '€', '£', 'zł', 'Kč', 'CHF', 'chf', '¥', 'XXX', 'rupees', '', null, 356, 'INR ₹', '₹ INR', 'EUR€'];
const CATEGORIES = [
  ['travel', 'Travel (TA)'], ['local', 'Local conveyance'], ['lodging', 'Lodging'], ['meals', 'Meals (DA)'], ['fuel', 'Fuel'],
  ['conf', 'Conference and registration fees'], ['books', 'Books and stationery'], ['lab', 'Lab consumables'],
  ['print', 'Printing and postage'], ['other', 'Other'],
].map(([id, name]) => ({ id, name }));
const LABELS = [
  'Travel (TA)', 'travel', 'TA', 'Travel', 'Meals', 'DA', 'meal', 'Food', 'Hotel', 'lodging', 'LODGING', 'Petrol', 'fuel',
  'Conference', 'Registration fees', 'conference fees', 'Books', 'Stationery', 'Lab', 'consumables', 'Printing', 'Postage',
  'Local', 'conveyance', 'Auto rickshaw', 'Other', 'Others', 'misc', '', null, 7, 'The', 'Books & Stationery', 'printing and postage',
];
const MERCHANTS = ['Café Coffee Day', '  HOTEL SAI PALACE  ', 'शिवाजी नगर पेट्रोल पंप', 'Ola', '', 'A'.repeat(130), 0, 42];
const OCR_DATES = ['2026-08-03', '03/08/2026', '08/20/2026', '12-Aug-2026', 'Aug 3', '2031-01-01', 'garbage', '', null, 20260803, '31/02/2026'];
const TOTALS = ['₹ 1,23,456.50', 'Rs. 450', '1.234,56', '(12.34)', '-5', 14.6, 0, -3, '', null, 'abc', '12,5', 2499];
const PAYMENTS = ['VISA', 'UPI', 'cash', 'Cash', 'Card', 'MasterCard', 'Credit Card', 'debit', 'Mobile Payment', 'bank transfer', 'AMEX ****1005', 'Paytm', '', null, 7];
const CONFIDENCES = [0.9, 0, 1, 1.4, -0.2, '0.85', ' 0.7 ', '', 'high', null, '1e-1', '.5'];
const ITEM_NAMES = ['Masala dosa', '  Filter coffee  ', '', 'चाय', null, 5, 'Room charge'];

function ocrRaw(r) {
  const o = {};
  const maybe = (p, k, v) => { if (r.next() < p) o[k] = v(); };
  maybe(0.8, 'merchant', () => pick(r, MERCHANTS));
  maybe(0.85, 'date', () => pick(r, OCR_DATES));
  const tk = pick(r, ['total', 'total', 'amount', 'grand_total', 'none', 'nullTotal']);
  if (tk === 'nullTotal') { o.total = null; o.amount = pick(r, TOTALS); }
  else if (tk !== 'none') o[tk] = pick(r, TOTALS);
  maybe(0.5, 'tax', () => pick(r, ['18.00', 2.25, null, '₹ 81', '']));
  maybe(0.3, 'tip', () => pick(r, ['10', 0, null, 'none']));
  maybe(0.8, 'currency', () => pick(r, CURRENCIES));
  maybe(0.7, 'category', () => pick(r, LABELS));
  maybe(0.7, 'paymentMethod', () => pick(r, PAYMENTS));
  maybe(0.4, 'cardLast4', () => pick(r, ['**** 4471', 'XXXX-XXXX-XXXX-1005', '12', 'none', '', null, 4471]));
  maybe(0.6, 'items', () => (r.next() < 0.1 ? 'not a list' : Array.from({ length: r.int(0, 4) }, () => {
    if (r.next() < 0.1) return pick(r, [null, 'Tea', 3]);
    const it = {};
    const nameKey = pick(r, ['name', 'name', 'description', 'none']);
    if (nameKey !== 'none') it[nameKey] = pick(r, ITEM_NAMES);
    const amtKey = pick(r, ['amount', 'amount', 'price', 'none']);
    if (amtKey !== 'none') it[amtKey] = pick(r, ['120', 45.5, '₹ 1,200', null, '', 'x']);
    return it;
  })));
  maybe(0.4, 'notes', () => pick(r, ['2 guests', '  GSTIN 27AAACB1234F1Z5  ', '', null, 'नोट']));
  maybe(0.7, 'confidence', () => pick(r, CONFIDENCES));
  const mode = r.next();
  if (mode < 0.45) return o;
  if (mode < 0.65) return JSON.stringify(o);
  if (mode < 0.8) return `\`\`\`json\n${JSON.stringify(o, null, 2)}\n\`\`\``;
  if (mode < 0.95) return `Here is what I read from the receipt:\n${JSON.stringify(o)}\nPlease verify.`;
  return pick(r, ['I could not read this receipt.', '', null, '{"total": 12']);
}

const STATUSES = ['draft', 'submitted', 'approved', 'reimbursed', 'rejected', '', 'Draft'];

export default {
  fn: 'expense claims: parseAmount / totals / normalizeText / dates / currency / OCR / report lifecycle',
  module: `${LIB}ocr.js`,
  modules: {
    money: `${LIB}money.js`, util: `${LIB}util.js`, dates: `${LIB}dates.js`, currencies: `${LIB}currencies.js`,
    ocr: `${LIB}ocr.js`, reports: `${LIB}reports.js`,
  },
  paths: [LIB],
  out: 'modules/resources/tests/golden/expense_claims.json',
  cases() {
    const r = rng(3404);
    const out = [];
    for (let b = 1; b <= 200; b++) {
      const today = iso(Date.UTC(pick(r, [2025, 2026, 2026, 2027]), r.int(0, 11), r.int(1, 28)));
      const cats = r.next() < 0.08 ? [] : CATEGORIES;
      const lines = Array.from({ length: r.int(0, 7) }, () => ({
        reportId: pick(r, ['r1', 'r1', 'r2']),
        amount: pick(r, [r.int(1, 500000) / 100, r.int(1, 99) / 10, 0.1, 0.2, -50, 0]),
        currency: pick(r, ['INR', 'INR', 'USD', 'EUR']),
        receiptPath: pick(r, ['receipts/a.jpg', '', null]),
        reconciled: r.next() < 0.4,
      }));
      out.push({
        today,
        baseCurrency: pick(r, ['INR', 'INR', 'USD']),
        amounts: some(r, AMOUNTS, 3, 6),
        texts: some(r, TEXTS, 2, 4),
        jsons: some(r, JSONS, 1, 3),
        dates: some(r, DATES, 3, 6).map((d) => [d, r.next() < 0.5]),
        currencies: some(r, CURRENCIES, 2, 4),
        categories: cats,
        labels: some(r, LABELS, 2, 4),
        ocr: Array.from({ length: r.int(1, 3) }, () => ocrRaw(r)),
        statuses: STATUSES,
        reports: [null, ...STATUSES.map((status) => ({ status }))],
        lines,
        reportId: 'r1',
      });
    }
    return out;
  },
  run(m, c) {
    const { money, util, dates, currencies, ocr, reports } = m;
    const now = new Date(`${c.today}T12:00:00Z`);
    const pairs = (map) => [...map.entries()];
    const totals = reports.reportTotals({ id: c.reportId }, c.lines);
    return {
      amounts: c.amounts.map((a) => money.parseAmount(a)),
      normalized: c.texts.map((t) => util.normalizeText(t)),
      tokens: c.texts.map((t) => util.tokens(t)),
      jsons: c.jsons.map((t) => ocr.extractJson(t)),
      dates: c.dates.map(([d, dayFirst]) => dates.parseLooseDate(d, { dayFirst, now })),
      currencies: c.currencies.map((h) => currencies.resolveCurrency(h, null)),
      categories: c.labels.map((l) => ocr.matchCategory(l, c.categories)?.id ?? null),
      ocr: c.ocr.map((raw) => ocr.normalizeOcrResult(raw, { categories: c.categories, baseCurrency: c.baseCurrency, now })),
      next: c.statuses.map((s) => reports.nextStatus(s)),
      transitions: c.statuses.map((a) => c.statuses.map((b) => reports.canTransition(a, b))),
      editable: c.reports.map((rep) => reports.isEditable(rep)),
      totals: pairs(money.totalsByCurrency(c.lines)),
      report: { count: totals.count, byCurrency: pairs(totals.byCurrency), withReceipt: totals.withReceipt, reconciled: totals.reconciled },
    };
  },
};
