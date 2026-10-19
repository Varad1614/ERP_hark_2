// gst_invoicing  <-  invoicing/src/lib/{money,taxnote,doc}.js
//   docTotals (named taxes per line, document defaults, tax-inclusive prices, whole-document
//   discount), taxNoteValue / taxNoteText / chargesUntaxed, formatDocNumber — fed with GST-shaped
//   inputs: CGST + SGST halves (intra-state) or IGST (inter-state) at the Indian slabs, exempt
//   lines opting out with `taxes: []`. Ported to `modules/finance/src/gst/rules.rs`.
import { rng } from '../rng.mjs';
import { pick } from './_bank.mjs';

const LIB = 'src/invoicing/src/lib/';
const SLABS = [0, 5, 12, 18, 28, 40];

// GST as Everything's named taxes: one row per (label, rate).
function gst(rate, inter) {
  if (!rate) return [];
  return inter ? [{ label: 'IGST', rate }] : [{ label: 'CGST', rate: rate / 2 }, { label: 'SGST', rate: rate / 2 }];
}

// A JS float landing within a hair of half a paisa can round apart from exact decimals; such
// figures are flagged (null) rather than compared.
const halfPaisa = (v) => {
  const x = Math.abs(v * 100);
  return Math.abs(x - Math.floor(x) - 0.5) < 1e-6;
};

function totalsCase(r) {
  const inter = r.next() < 0.35;
  const docRate = pick(r, SLABS);
  const n = r.int(1, 5);
  const items = Array.from({ length: n }, () => {
    const it = { qty: pick(r, [1, 1, 1, 2, 3, 12]), rate: r.int(1, 15_000_000) / 100 };
    const roll = r.next();
    if (roll < 0.25) it.taxes = []; // exempt / nil line
    else if (roll < 0.55) it.taxes = gst(pick(r, SLABS), inter);
    return it;
  });
  const doc = { items, taxes: gst(docRate, inter), taxInclusive: r.next() < 0.4 };
  const d = r.next();
  if (d < 0.15) {
    doc.discountType = 'percent';
    doc.discountValue = pick(r, [5, 10, 12.5, 25, 50, 100, 120]);
  } else if (d < 0.25) {
    doc.discountType = 'fixed';
    doc.discountValue = r.int(0, 2_000_000) / 100;
  }
  return { kind: 'totals', doc };
}

function numberCase(r) {
  const prefix = pick(r, ['TI/', 'BOS/', 'CN/', 'INV-{YYYY}-', 'TI/{YYYY}/', '', 'R{YYYY}{YYYY}/']);
  const n = pick(r, [() => r.int(1, 9999), () => r.int(10_000, 2_000_000), () => 0, () => -3, () => r.int(1, 99) + 0.7]);
  const date = `${r.int(2024, 2031)}-${String(r.int(1, 12)).padStart(2, '0')}-${String(r.int(1, 28)).padStart(2, '0')}`;
  return { kind: 'number', prefix, n: n(), date };
}

function noteCase(r) {
  const values = ['', '   ', 'none', 'preset:exempt', 'Exempt under Notification 12/2017', 'preset:zeroRated'];
  const pickNote = () => (r.next() < 0.3 ? {} : { taxNote: pick(r, values) });
  const inter = r.next() < 0.3;
  const items = Array.from({ length: r.int(1, 3) }, () => {
    const it = { qty: 1, rate: r.int(100, 500_000) / 100 };
    const roll = r.next();
    if (roll < 0.3) it.taxes = [];
    else if (roll < 0.5) it.taxes = gst(pick(r, SLABS), inter);
    return it;
  });
  return { kind: 'note', settings: pickNote(), doc: { ...pickNote(), items, taxes: gst(pick(r, SLABS), inter) }, client: r.next() < 0.2 ? null : pickNote() };
}

export default {
  fn: 'docTotals',
  module: `${LIB}money.js`,
  modules: { money: `${LIB}money.js`, taxnote: `${LIB}taxnote.js`, doc: `${LIB}doc.js` },
  paths: [LIB],
  out: 'modules/finance/tests/golden/gst_invoicing.json',
  cases() {
    const r = rng(2709);
    const out = [];
    for (let i = 0; i < 160; i++) out.push(totalsCase(r));
    for (let i = 0; i < 25; i++) out.push(numberCase(r));
    for (let i = 0; i < 30; i++) out.push(noteCase(r));
    return out;
  },
  run(m, c) {
    if (c.kind === 'number') return { number: m.doc.formatDocNumber(c.prefix, c.n, c.date) };
    if (c.kind === 'note') {
      const value = m.taxnote.taxNoteValue(c.settings, c.doc, c.client);
      return {
        value,
        // Presets print Indianised wording in the port; only the user's own words are compared.
        text: m.taxnote.isTaxNotePreset(value) ? null : m.taxnote.taxNoteText(value),
        untaxed: m.taxnote.chargesUntaxed(c.doc),
      };
    }
    const t = m.money.docTotals(c.doc);
    const flag = (v) => (halfPaisa(v) ? null : v);
    // The unrounded row sums, to flag the rows that land on half a paisa.
    const raw = (() => {
      const sub = (c.doc.items || []).reduce((a, it) => a + m.money.lineTotal(it), 0);
      const factor = sub > 0 ? t.taxable / sub : 1;
      const rows = {};
      for (const it of c.doc.items) {
        const own = (Array.isArray(it.taxes) ? it.taxes : c.doc.taxes).filter((x) => x.rate > 0);
        const sum = own.reduce((a, x) => a + x.rate, 0);
        const gross = m.money.lineTotal(it) * factor;
        const base = c.doc.taxInclusive ? gross / (1 + sum / 100) : gross;
        for (const x of own) rows[`${x.label}|${x.rate}`] = (rows[`${x.label}|${x.rate}`] || 0) + base * x.rate / 100;
      }
      return rows;
    })();
    const discountRaw = c.doc.discountType === 'percent' ? t.subtotal * (c.doc.discountValue || 0) / 100 : 0;
    const shaky = halfPaisa(discountRaw) || Object.values(raw).some(halfPaisa);
    return {
      subtotal: t.subtotal,
      discount: shaky ? null : t.discount,
      taxable: shaky ? null : t.taxable,
      tax: shaky ? null : flag(t.tax),
      taxes: shaky ? null : t.taxes.map((x) => ({ label: x.label, rate: x.rate, amount: x.amount })),
      total: shaky ? null : t.total,
    };
  },
};
