// admission-funnel  <-  crm/src/lib/funnel.js, forecast.js, dupes.js
//   funnelReport / resolveStages / probabilityOf / forecastByMonth / stageSummary /
//   pipelineValue / weightedPipeline / winRate / winRateBy / closeReasonBreakdown /
//   avgClosedValue / stageVelocity / stalledDeals / avgSalesCycle / contactDuplicates over random
//   enquiry books and stage configurations (custom stages with junk entries, stages removed
//   since, per-deal confidence, stage overrides, stale and future expected-join dates).
//   The Rust port names the terminal stage `admitted`; the test maps Everything's `won`.
//   Won deals always carry a close time at noon UTC mid-month, so the month never depends on
//   the generator's time zone.
import { rng } from '../rng.mjs';

const LIB = 'src/crm/src/lib/';
const DAY = 86400000;
const pick = (r, a) => a[r.int(0, a.length - 1)];
const p2 = (r, lo, hi) => Math.round((lo + r.next() * (hi - lo)) * 100) / 100;
const key = (y, m) => `${y}-${String(m).padStart(2, '0')}`;

const DEFAULT_IDS = ['new', 'qualified', 'proposal', 'negotiation'];
const NAMES = ['Aarav Sharma', 'Diya Patel', 'Ishaan Iyer', 'Ananya Rao', 'Kabir Singh', 'Meera Nair', 'Rohan Das', 'Saanvi Joshi'];
const SOURCES = ['walk_in', 'website', 'referral', 'campaign', 'education_fair', 'social', '', ' '];
const WON = ['Scholarship', 'Programme fit', 'Fee', 'Location', ''];
const LOST = ['Fee', 'Chose another college', 'Did not qualify', 'No response', 'Relocated', ''];

function variant(r, name) {
  const v = r.int(0, 5);
  if (v === 0) return name.toUpperCase();
  if (v === 1) return `  ${name.replace(' ', '   ')} `;
  if (v === 2) return name.toLowerCase();
  if (v === 3) return name.replace('a', 'á');
  return name;
}

function stageConfig(r) {
  if (r.next() < 0.3) return { config: [], ids: DEFAULT_IDS };
  const ids = [];
  const config = [];
  const n = r.int(1, 10);
  for (let i = 0; i < n; i++) {
    const kind = r.int(0, 14);
    if (kind === 0) { config.push(null); continue; }
    if (kind === 1) { config.push(7); continue; }
    if (kind === 2) { config.push({ id: '  ', label: 'Blank' }); continue; }
    if (kind === 3) { config.push({ id: 'won', label: 'Won?', probability: 0.9 }); continue; }
    if (kind === 4 && ids.length) { config.push({ id: ids[0], label: 'Dup', probability: 0.2 }); continue; }
    const id = `s${i}`;
    ids.push(id);
    const prob = pick(r, ['num', 'num', 'num', 'str', 'null', 'none', 'big', 'neg', 'blank']);
    const s = { id: r.next() < 0.1 ? ` ${id} ` : id };
    if (r.next() < 0.8) s.label = r.next() < 0.1 ? '   ' : `Stage ${i}`;
    if (prob === 'num') s.probability = p2(r, 0, 1);
    if (prob === 'str') s.probability = String(p2(r, 0, 1));
    if (prob === 'null') s.probability = null;
    if (prob === 'big') s.probability = 1.5;
    if (prob === 'neg') s.probability = -0.25;
    if (prob === 'blank') s.probability = ' ';
    config.push(s);
  }
  // Mirror sanitize: the first eight distinct valid ids survive.
  return { config, ids: ids.length ? [...new Set(ids)].slice(0, 8) : DEFAULT_IDS };
}

export default {
  fn: 'funnelReport / forecastByMonth / winRateBy / stageVelocity / contactDuplicates …',
  module: `${LIB}funnel.js`,
  modules: { funnel: `${LIB}funnel.js`, forecast: `${LIB}forecast.js`, dupes: `${LIB}dupes.js` },
  paths: [LIB],
  out: 'modules/student/tests/golden/admission_funnel.json',
  cases() {
    const r = rng(2026);
    const out = [];
    for (let b = 1; b <= 120; b++) {
      const { config, ids } = stageConfig(r);
      const year = r.int(2024, 2027);
      const month = r.int(1, 12);
      const from = key(year, month);
      const base = Date.UTC(year, month - 1, 1) - r.int(0, 400) * DAY;
      const deals = Array.from({ length: r.int(0, 22) }, (_, i) => {
        const hist = [];
        let at = base + r.int(0, 300) * DAY;
        const createdAt = at;
        const steps = r.int(0, 5);
        let pos = r.int(0, ids.length - 1);
        for (let s = 0; s < steps; s++) {
          const st = r.next() < 0.06 ? 'retired_stage' : ids[Math.min(ids.length - 1, pos)];
          hist.push({ stage: st, at });
          at += r.int(0, 40) * DAY;
          pos += r.int(-1, 2);
          if (pos < 0) pos = 0;
        }
        const end = r.next();
        let stage = hist.length ? hist[hist.length - 1].stage : pick(r, ids);
        let closedAt;
        if (end < 0.25) {
          stage = 'won';
          const cm = r.int(-8, 8);
          const d = new Date(Date.UTC(year, month - 1 + cm, r.int(2, 27), 12));
          closedAt = d.getTime();
          hist.push({ stage: 'won', at });
        } else if (end < 0.45) {
          stage = 'lost';
          if (r.next() < 0.6) closedAt = at;
          hist.push({ stage: 'lost', at });
        }
        const deal = { id: `e${b}-${i}`, stage, stageHistory: hist, value: r.next() < 0.1 ? 0 : r.int(1, 400) * 1000 + r.int(0, 999), createdAt };
        if (r.next() < 0.3) deal.updatedAt = at;
        if (closedAt !== undefined) deal.closedAt = closedAt;
        if (r.next() < 0.3) deal.confidence = r.next() < 0.1 ? null : p2(r, -0.2, 1.2);
        if (r.next() < 0.8) {
          const d = new Date(Date.UTC(year, month - 1 + r.int(-6, 9), r.int(1, 28)));
          deal.expectedClose = d.toISOString().slice(0, 10);
        }
        if (stage === 'won' || stage === 'lost') {
          const reason = pick(r, stage === 'won' ? WON : LOST);
          if (reason || r.next() < 0.5) deal.closeReason = reason;
        }
        const src = pick(r, SOURCES);
        if (src || r.next() < 0.5) deal.source = src;
        return deal;
      });
      const overrides = {};
      for (const id of ids) if (r.next() < 0.2) overrides[id] = r.next() < 0.1 ? 1.3 : p2(r, 0, 1);
      const contacts = Array.from({ length: r.int(0, 16) }, (_, i) => {
        const name = pick(r, NAMES);
        const slug = name.split(' ')[0].toLowerCase();
        const c = { id: `c${b}-${i}-${r.int(0, 9)}`, name: variant(r, name), createdAt: r.int(0, 20) };
        if (r.next() < 0.6) c.email = r.next() < 0.3 ? ` ${slug.toUpperCase()}@MAIL.IN ` : `${slug}@mail.in`;
        if (r.next() < 0.5) {
          const digits = `98${String(NAMES.indexOf(name)).padStart(3, '0')}12345`;
          c.phone = pick(r, [digits, `+91 ${digits.slice(0, 5)} ${digits.slice(5)}`, `${digits.slice(0, 5)}-${digits.slice(5)}`, '112', '']);
        }
        return c;
      });
      out.push({
        stagesConfig: config,
        deals,
        overrides,
        from,
        months: r.int(1, 12),
        staleDays: pick(r, [0, 7, 14, 30]),
        now: base + r.int(200, 500) * DAY,
        contacts,
      });
    }
    return out;
  },
  run(m, c) {
    const f = m.forecast;
    const stages = f.resolveStages({ stages: c.stagesConfig });
    return {
      stages,
      funnel: m.funnel.funnelReport(c.deals, stages),
      probabilities: c.deals.map((d) => f.probabilityOf(d, c.overrides, stages)),
      forecast: f.forecastByMonth(c.deals, { months: c.months, from: c.from, overrides: c.overrides, stages }),
      summary: f.stageSummary(c.deals, c.overrides, stages),
      pipeline: f.pipelineValue(c.deals),
      weighted: f.weightedPipeline(c.deals, c.overrides, stages),
      winRate: f.winRate(c.deals),
      bySource: f.winRateBy(c.deals, (d) => d.source),
      reasonsWon: f.closeReasonBreakdown(c.deals, 'won'),
      reasonsLost: f.closeReasonBreakdown(c.deals, 'lost'),
      avgClosed: f.avgClosedValue(c.deals),
      velocity: f.stageVelocity(c.deals, stages),
      stalled: f.stalledDeals(c.deals, c.staleDays, c.now).map((d) => d.id),
      cycle: f.avgSalesCycle(c.deals),
      dupes: m.dupes.contactDuplicates(c.contacts).map((p) => ({ a: p.a.id, b: p.b.id, reasons: p.reasons })),
    };
  },
};
