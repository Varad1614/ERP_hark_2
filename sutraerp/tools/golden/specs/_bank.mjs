// Shared input builders for the bankimport golden specs (not a spec itself: no default export).
export const LIB = 'src/accounting/src/lib/';
const pick = (r, a) => a[r.int(0, a.length - 1)];
export { pick };

export function amountText(r) {
  const rupees = r.int(0, 9_999_999);
  const paise = r.int(0, 99);
  const neg = r.next() < 0.3;
  const inGroup = (n) => {
    const s = String(n);
    if (s.length <= 3) return s;
    const last3 = s.slice(-3);
    const rest = s.slice(0, -3).replace(/\B(?=(\d{2})+(?!\d))/g, ',');
    return `${rest},${last3}`;
  };
  const west = (n) => String(n).replace(/\B(?=(\d{3})+(?!\d))/g, ',');
  const eu = (n) => String(n).replace(/\B(?=(\d{3})+(?!\d))/g, '.');
  const p2 = String(paise).padStart(2, '0');
  const body = pick(r, [
    () => `${rupees}.${p2}`,
    () => `${inGroup(rupees)}.${p2}`,
    () => `${west(rupees)}.${p2}`,
    () => `${eu(rupees)},${p2}`,
    () => `${rupees}`,
    () => inGroup(rupees),
    () => `${rupees},${String(paise % 10)}`,
    () => `${rupees}.${p2}${r.int(0, 9)}`,
  ])();
  const wrap = pick(r, [
    (b) => (neg ? `-${b}` : b),
    (b) => (neg ? `(${b})` : b),
    (b) => `₹${neg ? '-' : ''}${b}`,
    (b) => `Rs. ${b}${neg ? ' Dr' : ''}`,
    (b) => (neg ? `−${b}` : ` ${b} `),
    (b) => `INR ${b}`,
  ]);
  return wrap(body);
}

const MON = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];
export function dateText(r) {
  const y = r.int(2019, 2031);
  const m = r.int(1, 12);
  const d = r.int(1, 31);
  const p = (n) => String(n).padStart(2, '0');
  return pick(r, [
    () => `${y}-${p(m)}-${p(d)}`,
    () => `${y}-${p(m)}-${p(d)}T10:20:00`,
    () => `${y}${p(m)}${p(d)}`,
    () => `${p(d)}/${p(m)}/${y}`,
    () => `${d}/${m}/${y}`,
    () => `${p(m)}/${p(d)}/${String(y).slice(2)}`,
    () => `${p(d)}-${p(m)}-${y}`,
    () => `${p(d)}.${p(m)}.${y}`,
    () => `${y}/${m}/${d}`,
    () => `${MON[m - 1]} ${d}, ${y}`,
    () => `${MON[m - 1].toUpperCase()} ${d} ${y}`,
    () => `${d} ${MON[m - 1]} ${y}`,
    () => `${p(d)}-${MON[m - 1]}-${String(y).slice(2)}`,
    () => `${d} ${MON[m - 1]}ember ${y}`,
    () => `${d}/${m}/${String(y).slice(1)}`,
    () => '',
    () => 'Opening balance',
  ])();
}
