// contracts-renewals  <-  contracts/src/lib/dates.js, store.js, ical.js
//   nextTermEnd / agreementEnd / completedTerms / renewalInfo / needsAttention /
//   termReconciliation, repeating obligations (withinSeries, advance / rewind / reopen,
//   repeatAnchor), allObligations / dueObligations, contractStats, upcomingRenewals,
//   contractEvents (uid, date, category) and escapeText / foldLine / buildCalendar.
//   Run in UTC with the clock pinned to each case's day (withinSeries reads it).
import { rng } from '../rng.mjs';

process.env.TZ = 'UTC';

const LIB = 'src/contracts/src/lib/';
const pick = (r, a) => a[r.int(0, a.length - 1)];
const DAY = 86400000;
const iso = (ms) => new Date(ms).toISOString().slice(0, 10);
const RealDate = Date;

function pinClock(day) {
  const fixed = RealDate.parse(`${day}T12:00:00Z`);
  globalThis.Date = class extends RealDate {
    constructor(...a) {
      if (a.length) super(...a);
      else super(fixed);
    }
    static now() {
      return fixed;
    }
  };
}

const TEXTS = ['Annual AMC visit', 'Submit MoU activity report, NAAC 3.7', 'Pay rent; Q1', 'Line\nbreak', 'Back\\slash', 'संयुक्त शोध कार्यशाला (joint research workshop) with partner university faculty', ''];

export default {
  fn: 'contract renewals / term history / obligations / stats / iCal',
  module: `${LIB}dates.js`,
  modules: { dates: `${LIB}dates.js`, store: `${LIB}store.js`, ical: `${LIB}ical.js` },
  paths: [LIB],
  out: 'modules/resources/tests/golden/contracts_renewals.json',
  cases() {
    const r = rng(3303);
    const out = [];
    for (let b = 1; b <= 200; b++) {
      const today = iso(Date.UTC(2026, r.int(0, 11), r.int(1, 28)));
      const day = (n) => iso(Date.parse(`${today}T00:00:00Z`) + n * DAY);
      const contracts = Array.from({ length: r.int(1, 4) }, (_, ci) => {
        const end = r.next() < 0.12 ? null : pick(r, [day(r.int(-900, 400)), '2025-01-31', '2024-02-29', day(r.int(-40, 40))]);
        const autoRenew = r.next() < 0.6;
        const termMonths = pick(r, [12, 12, 1, 3, 6, 24, 36]);
        const start = end && r.next() < 0.8 ? iso(Date.parse(`${end}T00:00:00Z`) - r.int(30, 720) * DAY) : null;
        let decision = null;
        if (end && r.next() < 0.45) {
          const kind = pick(r, ['notice-given', 'renewing', 'not-renewing', 'snoozed']);
          // A real term end, a stale one, or none.
          const k = r.int(-1, 4);
          const forTermEnd = pick(r, [end, null, day(r.int(-60, 60))]);
          const real = autoRenew && k >= 0 ? (() => {
            const [y, m, d] = end.split('-').map(Number);
            const total = y * 12 + (m - 1) + termMonths * k;
            const ty = Math.floor(total / 12);
            const tm = total % 12;
            const last = new RealDate(RealDate.UTC(ty, tm + 1, 0)).getUTCDate();
            return iso(RealDate.UTC(ty, tm, Math.min(d, last)));
          })() : null;
          decision = {
            kind,
            date: today,
            note: pick(r, ['', 'Board approved', 'Vendor slow']),
            forTermEnd: real && r.next() < 0.6 ? real : forTermEnd,
            ...(kind === 'snoozed' ? { until: r.next() < 0.85 ? day(r.int(-10, 30)) : null } : {}),
          };
        }
        const obligations = Array.from({ length: r.int(0, 4) }, (_, oi) => {
          const every = r.next() < 0.5 ? pick(r, ['monthly', 'quarterly', 'annually']) : null;
          const anchor = pick(r, [day(r.int(-200, 60)), '2026-01-31', '2025-08-31']);
          const h = every ? r.int(0, 5) : r.int(0, 1);
          const months = { monthly: 1, quarterly: 3, annually: 12 }[every] || 0;
          const occ = (i) => {
            const [y, m, d] = anchor.split('-').map(Number);
            const total = y * 12 + (m - 1) + months * i;
            const ty = Math.floor(total / 12);
            const tm = total % 12;
            const last = new RealDate(RealDate.UTC(ty, tm + 1, 0)).getUTCDate();
            return iso(RealDate.UTC(ty, tm, Math.min(d, last)));
          };
          let repeat = null;
          if (every) {
            const endKind = pick(r, ['none', 'until', 'count', 'contractEnd']);
            repeat = {
              every,
              anchor,
              until: endKind === 'until' ? day(r.int(-30, 300)) : null,
              count: endKind === 'count' ? r.int(1, 8) : null,
              contractEnd: endKind === 'contractEnd',
            };
          }
          const due = r.next() < 0.1 ? null : every ? occ(h) : day(r.int(-40, 60));
          return {
            id: `o${oi}`,
            text: pick(r, TEXTS) || 'Renewal paperwork',
            owner: pick(r, ['us', 'them', 'mutual']),
            due,
            done: r.next() < 0.2,
            repeat,
            history: Array.from({ length: h }, (_, i) => ({ due: every ? occ(i) : due, doneAt: 1 })),
          };
        });
        return {
          id: `c${ci}`,
          title: pick(r, ['AMC: RO plant', 'MoU: IIT Bombay', 'Bus lease', 'ERP licence']),
          status: pick(r, ['active', 'active', 'active', 'draft', 'terminated']),
          value: r.next() < 0.25 ? null : pick(r, [120000, 4500.5, 0, -3, 2500000]),
          currency: pick(r, ['INR', 'INR', 'USD', 'eur']),
          billing: pick(r, ['one-time', 'monthly', 'quarterly', 'annual']),
          startDate: start,
          endDate: end,
          autoRenew,
          termMonths,
          noticeDays: pick(r, [0, 30, 60, 90, 15]),
          decision,
          termHistory: [],
          obligations,
          updatedAt: Date.UTC(2026, 0, 1) + r.int(0, 1e6) * 60000,
        };
      });
      for (const c of contracts) {
        // Some contracts already recorded their first finished term.
        if (c.endDate && c.endDate < today && r.next() < 0.3) {
          c.termHistory = [{ start: c.startDate, end: c.endDate, ended: 'renewed', value: c.value, currency: 'INR', note: '', recordedAt: 1 }];
        }
      }
      const calendar = Array.from({ length: r.int(0, 3) }, (_, i) => ({
        uid: `u${i}@x`,
        date: day(r.int(0, 90)),
        sequence: r.int(0, 9999),
        summary: pick(r, TEXTS),
        description: r.next() < 0.5 ? '' : `${pick(r, TEXTS)}\n${pick(r, TEXTS)}`,
        url: r.next() < 0.5 ? '' : 'https://erp.example.in/contracts/c1',
        categories: r.next() < 0.3 ? [] : ['Contract notice', 'a,b'],
        alarms: pick(r, [[], [7, 1], [3]]),
      }));
      out.push({ today, soonDays: pick(r, [60, 30, 90]), days: pick(r, [30, 7, 60]), renewDays: pick(r, [90, 30, 365]), contracts, calendar, line: pick(r, TEXTS).repeat(r.int(1, 4)) });
    }
    return out;
  },
  run(m, c) {
    const { dates, store, ical } = m;
    pinClock(c.today);
    try {
      const today = c.today;
      const per = c.contracts.map((k) => {
        const info = dates.renewalInfo(k, today, c.soonDays);
        const patch = store.termReconciliation(k, today);
        const known = k.termHistory.length;
        return {
          nextTermEnd: dates.nextTermEnd(k, today),
          agreementEnd: dates.agreementEnd(k, today),
          completedTerms: dates.completedTerms(k, today),
          info: { ...info, decision: info.decision ? info.decision.kind : null },
          attention: dates.needsAttention(k, info),
          patch: patch === null ? null : {
            additions: (patch.termHistory || []).slice(known).map(({ start, end, ended, note }) => ({ start, end, ended, note })),
            terminate: patch.status === 'terminated',
            clearDecision: patch.decision === null,
          },
          obligations: k.obligations.map((ob) => {
            const view = (o) => ({ due: o.due, done: !!o.done, history: o.history.map((h) => h.due) });
            const res = {
              advance: ob.repeat ? view(store.advanceOccurrence(k, ob)) : null,
              rewind: view(store.rewindOccurrence(ob)),
              reopen: view(store.reopenedSeries(k, ob)),
              within: ob.repeat ? [0, 1, 2, 3, 5, 8].map((i) => store.withinSeries(ob.repeat, i, k)) : null,
              anchor: ob.repeat && ob.due ? store.repeatAnchor(ob.due, ob.repeat.every, ob.history.length, ob.repeat) : null,
              freshAnchor: ob.repeat && ob.due ? store.repeatAnchor(ob.due, ob.repeat.every, ob.history.length, null) : null,
            };
            return res;
          }),
          events: ical.contractEvents(k, { today, terms: 3, occurrences: 4 }).map((e) => ({ uid: e.uid, date: e.date, categories: e.categories })),
        };
      });
      const flat = (rows) => rows.map(({ contract, obligation, daysLeft }) => [c.contracts.indexOf(contract), contract.obligations.indexOf(obligation), daysLeft]);
      const stats = store.contractStats(c.contracts, today);
      return {
        per,
        all: flat(store.allObligations(c.contracts, { today, includeDone: true })),
        due: flat(store.dueObligations(c.contracts, { days: c.days, today })),
        stats: { ...stats, annualByCurrency: stats.annualByCurrency },
        upcoming: store.upcomingRenewals(c.contracts, { days: c.renewDays, today }).map(({ contract }) => c.contracts.indexOf(contract)),
        escaped: ical.escapeText(c.line),
        folded: ical.foldLine(`DESCRIPTION:${ical.escapeText(c.line)}`),
        calendar: ical.buildCalendar(c.calendar, { name: 'Contract deadlines, MoUs', stamp: '20261017T000000Z' }),
      };
    } finally {
      globalThis.Date = RealDate;
    }
  },
};
