// hiring-scorecards  <-  hiring/src/core/scorecards.js, guides.js, screening.js, reminders.js
//   normalizeRatingFields / validateScorecard / averageRating / summarizeScorecards,
//   normalizeGuides / defaultGuideFor / normalizeGuideAnswers, normalizeScreeningQuestions /
//   normalizeAnswers / screeningMismatches, missingScorecards over random inputs. Run in UTC.
import { rng } from '../rng.mjs';

process.env.TZ = 'UTC';

const CORE = 'src/hiring/src/core/';
const pick = (r, a) => a[r.int(0, a.length - 1)];
const DAY = 86400000;
const iso = (ms) => new Date(ms).toISOString().slice(0, 10);
const KEYS = ['skills', 'experience', 'communication', 'collaboration', 'teaching_demo', 'research'];
const RECS = ['strong_yes', 'yes', 'no', 'strong_no', 'maybe', null, 3];
const ID = () => (r) => pick(r, ['a', 'b', 'c', 'q-1', 'bad id', '', 'x_y', 7, null]);
const TEXT = (r) => pick(r, ['Why teaching?', '  Explain recursion  ', '', '   ', null, 42, 'Demo a lesson on GST', 'x'.repeat(320)]);

function ratings(r) {
  const out = {};
  for (const k of [...KEYS].sort()) {
    if (r.next() < 0.5) continue;
    out[k] = pick(r, [1, 2, 3, 4, 5, 5, 4, 0, 6, 3.5, null, '4']);
  }
  return out;
}

export default {
  fn: 'hiring scorecards / guides / screening / missing feedback',
  module: `${CORE}scorecards.js`,
  modules: { sc: `${CORE}scorecards.js`, guides: `${CORE}guides.js`, scr: `${CORE}screening.js`, rem: `${CORE}reminders.js` },
  paths: [CORE, 'src/hiring/src/lib/'],
  out: 'modules/people/tests/golden/hiring_scorecards.json',
  cases() {
    const r = rng(3202);
    const id = ID();
    const out = [];
    for (let b = 1; b <= 200; b++) {
      const today = iso(Date.UTC(2026, r.int(0, 11), r.int(1, 28)));
      const day = (n) => iso(Date.parse(`${today}T00:00:00Z`) + n * DAY);
      const fields = r.next() < 0.3 ? null : Array.from({ length: r.int(0, 5) }, () => ({ key: r.next() < 0.8 ? pick(r, KEYS) : id(r), label: pick(r, ['Subject knowledge', ' Teaching ', '', 'Research', null]) }));
      const candidates = Array.from({ length: r.int(1, 4) }, (_, i) => ({ id: `c${i}`, status: pick(r, ['active', 'active', 'rejected', 'withdrawn']) }));
      const scorecards = Array.from({ length: r.int(0, 6) }, () => ({
        author: pick(r, ['Dr. Rao', '', '  ', 'Meera']),
        recommendation: pick(r, RECS),
        ratings: ratings(r),
        candidateId: pick(r, candidates).id,
        interviewId: r.next() < 0.5 ? null : `i${r.int(0, 5)}`,
        createdOn: day(-r.int(0, 20)),
      }));
      const guides = Array.from({ length: r.int(0, 4) }, () => ({
        id: id(r),
        name: TEXT(r),
        questions: Array.from({ length: r.int(0, 4) }, () => ({ id: id(r), text: TEXT(r), field: r.next() < 0.5 ? pick(r, KEYS) : id(r) })),
        types: Array.from({ length: r.int(0, 3) }, () => pick(r, ['phone', 'video', 'onsite', 'zoom', 'phone'])),
        stages: Array.from({ length: r.int(0, 3) }, () => pick(r, ['screening', 'interviewed', 'offer', 'bad stage', null, 5])),
      }));
      const answersG = Array.from({ length: r.int(0, 4) }, () => ({ question: TEXT(r), answer: pick(r, ['Good', '', '  ok ', null, 9]), field: id(r) }));
      const questions = Array.from({ length: r.int(0, 10) }, () => ({
        id: id(r),
        label: pick(r, ['Cleared NET/SET?', 'PhD awarded?', 'Notice period', 'Preferred campus', '', null, 12]),
        kind: pick(r, ['short', 'long', 'yesno', 'choice', 'rating', undefined]),
        required: pick(r, [true, false, undefined, 1, '']),
        mustHave: pick(r, [true, false, undefined]),
        options: r.next() < 0.7 ? Array.from({ length: r.int(0, 8) }, () => pick(r, ['Pune', 'Mumbai', ' Pune ', '', 'Nagpur', 'Nashik', 'Thane', 'Satara'])) : 'no',
      }));
      const answers = Array.from({ length: r.int(0, 10) }, () => ({
        label: pick(r, ['cleared net/set?', 'PhD awarded?', 'Notice period ', '', null]),
        answer: pick(r, ['Yes', 'y', ' TRUE ', 'No', 'Not yet', '', null, '30 days']),
      }));
      const interviews = Array.from({ length: r.int(0, 6) }, (_, i) => ({
        id: `i${i}`,
        candidateId: r.next() < 0.1 ? 'gone' : pick(r, candidates).id,
        date: day(-r.int(-2, 18)),
        status: r.next() < 0.15 ? 'proposed' : 'scheduled',
      }));
      out.push({ today, fields, scorecards, guides, answersG, questions, answers, interviews, candidates, kind: pick(r, ['phone', 'video', 'onsite', '']), stage: pick(r, ['screening', 'interviewed', '', 'offer']) });
    }
    return out;
  },
  run(m, c) {
    const { sc, guides, scr, rem } = m;
    const rubric = sc.ratingFieldsForJob({ ratingFields: c.fields });
    const qs = scr.normalizeScreeningQuestions(c.questions);
    const ans = scr.normalizeAnswers(c.answers);
    const gs = guides.normalizeGuides(c.guides);
    const now = new Date(`${c.today}T12:00:00Z`);
    const state = {
      settings: {},
      candidates: c.candidates,
      interviews: c.interviews,
      scorecards: c.scorecards.map((s) => ({ ...s, createdAt: `${s.createdOn}T10:00:00` })),
    };
    return {
      normalizedFields: sc.normalizeRatingFields(c.fields),
      rubric,
      validation: c.scorecards.map((s) => sc.validateScorecard(s)),
      averages: c.scorecards.map((s) => sc.averageRating(s)),
      summary: sc.summarizeScorecards(c.scorecards, rubric),
      guides: gs,
      defaultGuide: guides.defaultGuideFor({ guides: c.guides }, { type: c.kind, stage: c.stage })?.id ?? null,
      guideAnswers: guides.normalizeGuideAnswers(c.answersG),
      questions: qs,
      answers: ans,
      mismatches: scr.screeningMismatches({ answers: ans ?? [] }, { screeningQuestions: c.questions }).map((q) => q.id),
      missing: rem.missingScorecards(state, now).map((i) => i.id),
    };
  },
};
