// hiring-offers  <-  hiring/src/core/offers.js, onboard.js, followups.js, analytics.js, pipeline.js,
//   retention.js, reminders.js (offers / follow-ups / onboarding reminders), jobpage.js
//   (escapeHtml / descriptionHtml / subdomainBase) and emails.js renderTemplate, over random
//   inputs. Run in UTC with the clock pinned per case (`now`).
import { rng } from '../rng.mjs';

process.env.TZ = 'UTC';

const CORE = 'src/hiring/src/core/';
const pick = (r, a) => a[r.int(0, a.length - 1)];
const DAY = 86400000;
const isoDay = (ms) => new Date(ms).toISOString().slice(0, 10);
const STATUSES = ['draft', 'sent', 'signed', 'declined', 'withdrawn', 'bogus'];
const STAGES = ['applied', 'screen', 'interview', 'offer', 'hired'];
const SOURCES = ['Naukri', 'LinkedIn', 'Referral', 'Campus drive', 'Walk-in', '', '  ', null];
const CATS = ['Paperwork', 'IT & access', 'People ops', 'Team', '', null];
const TITLES = ['Signed offer letter on file', 'PF / ESI enrolment (Form 11)', 'Create ERP and e-mail login', '', '   ', null, 'Issue ID card', 'x'.repeat(150)];
const OFFSETS = [-7, -3, -1, 0, 2, 7, 400, -366, 2.5, '5', '', null, true];
const TEMPLATE_LINES = [
  '{{sentDate}} · {{company}}',
  'Dear {{name}},',
  '',
  'We are pleased to offer you the post of {{job}}{{team}}.',
  '  Annual CTC    {{salary}}',
  '  Joining bonus {{bonus}}',
  '  Joining date  {{ startDate }}',
  'This offer remains open until {{expires}}.',
  '{{notes}}',
  '{{signature}}',
  'Literal {{bad-key}} stays',
  'Prototype {{hasOwnProperty}} {{constructor}}',
  'No placeholders here',
  '',
  '',
];
const VAR_KEYS = ['name', 'job', 'team', 'company', 'salary', 'bonus', 'startDate', 'expires', 'notes', 'sentDate', 'signature'];
const VAR_VALUES = ['Priya Kulkarni', 'Assistant Professor', ' in the Department of Commerce', '', '   ', '₹9,60,000', '01 Jun 2026', 'Sinhgad College', null, 'Line one\n\n\n\nLine two'];
const DESCS = [
  'Teach UG commerce.\nGuide projects.\n\n- NET/SET cleared\n- PhD preferred\n* 2 years experience',
  '',
  '<script>alert(1)</script> & "quotes" \'single\'',
  '  First para line  \n  continues here \n\n\nSecond para',
  '- only bullets\n-not a bullet\n*  star bullet',
];
const NAMES = ['Sinhgad College of Engineering, Pune', 'MIT-WPU', '', '---', "Shri Shivaji Vidya Prasarak Sanstha's Late Bapusaheb Shivajirao Deore Polytechnic", 'Ahilyanagar  Arts & Science   College', null, 'ÉCOLE Normale'];

function history(r, created) {
  // A plausible candidate history: created (applied, or straight into a later stage), stage moves
  // (mostly forward), archive / restore / re-engage events.
  let t = created;
  let stage = r.next() < 0.85 ? 'applied' : pick(r, ['screen', 'hired']);
  let status = 'active';
  const events = [{ type: 'created', stage, at: new Date(t).toISOString() }];
  let changed = t;
  const steps = r.int(0, 6);
  for (let i = 0; i < steps; i++) {
    t += r.int(1, 20) * DAY + r.int(0, 23) * 3600000;
    const at = new Date(t).toISOString();
    const roll = r.next();
    if (status !== 'active') {
      if (roll < 0.5) {
        events.push({ type: 'restored', at });
        status = 'active';
      } else {
        events.push({ type: 'reengaged', at });
        status = 'active';
        stage = 'applied';
        changed = t;
      }
    } else if (roll < 0.65 && stage !== 'hired') {
      const idx = STAGES.indexOf(stage);
      const to = STAGES[Math.min(STAGES.length - 1, idx + (r.next() < 0.8 ? 1 : 2))];
      events.push({ type: 'stage', from: stage, to, at });
      stage = to;
      changed = t;
    } else if (roll < 0.75 && stage !== 'applied') {
      const idx = STAGES.indexOf(stage);
      const to = STAGES[idx - 1];
      events.push({ type: 'stage', from: stage, to, at });
      stage = to;
      changed = t;
    } else if (roll < 0.9) {
      status = pick(r, ['rejected', 'withdrawn']);
      events.push({ type: status, at });
    } else {
      events.push({ type: 'note', at });
    }
  }
  return { events, stage, status, changed };
}

export default {
  fn: 'hiring offers / onboarding / follow-ups / analytics / retention / reminders / job page',
  module: `${CORE}offers.js`,
  modules: {
    offers: `${CORE}offers.js`,
    onboard: `${CORE}onboard.js`,
    fu: `${CORE}followups.js`,
    an: `${CORE}analytics.js`,
    pipe: `${CORE}pipeline.js`,
    ret: `${CORE}retention.js`,
    rem: `${CORE}reminders.js`,
    job: `${CORE}jobpage.js`,
    mail: `${CORE}emails.js`,
  },
  paths: [CORE, 'src/hiring/src/lib/'],
  out: 'modules/people/tests/golden/hiring_offers.json',
  cases() {
    const r = rng(5101);
    const out = [];
    for (let b = 1; b <= 200; b++) {
      const base = Date.UTC(2026, r.int(0, 11), r.int(1, 28));
      const today = isoDay(base);
      const nowTime = `${String(r.int(0, 23)).padStart(2, '0')}:${pick(r, ['00', '15', '30', '45'])}`;
      const day = (n) => isoDay(base + n * DAY);
      const stamp = (n) => new Date(base + n * DAY + r.int(0, 23) * 3600000).toISOString();

      const candidates = Array.from({ length: r.int(1, 6) }, (_, i) => {
        const created = base - r.int(5, 200) * DAY + r.int(0, 23) * 3600000;
        const h = history(r, created);
        return {
          id: `c${i}`,
          name: `Candidate ${i}`,
          jobId: pick(r, ['j1', 'j2']),
          stage: h.stage,
          status: h.status,
          createdAt: new Date(created).toISOString(),
          stageChangedAt: new Date(h.changed).toISOString(),
          source: pick(r, SOURCES),
          pool: r.next() < 0.2,
          history: h.events,
          followups: Array.from({ length: r.int(0, 3) }, (_, k) => ({
            id: `f${i}${k}`,
            text: pick(r, ['Call about notice period', '', '  ', 'Share PF transfer form']),
            dueDate: r.next() < 0.1 ? pick(r, ['', 'soon', null]) : day(r.int(-5, 3)),
            dueTime: r.next() < 0.5 ? null : pick(r, ['09:00', '12:30', '18:45', '23:59', '9:75', '24:00', '7:05']),
            done: r.next() < 0.25,
            completedAt: r.next() < 0.5 ? stamp(-r.int(0, 5)) : null,
          })),
        };
      });
      const offers = Array.from({ length: r.int(0, 6) }, (_, i) => {
        const status = pick(r, STATUSES);
        const sent = status !== 'draft' && r.next() < 0.9 ? stamp(-r.int(0, 40)) : null;
        return {
          id: `o${i}`,
          candidateId: r.next() < 0.1 ? 'gone' : pick(r, candidates).id,
          status,
          expires: r.next() < 0.25 ? null : day(r.int(-4, 8)),
          startDate: r.next() < 0.1 ? null : day(r.int(-10, 30)),
          sentAt: sent,
          signedAt: status === 'signed' && r.next() < 0.7 ? stamp(-r.int(0, 30)) : null,
          resolvedAt: (status === 'signed' || status === 'declined') && r.next() < 0.6 ? stamp(-r.int(0, 30)) : null,
          createdAt: stamp(-r.int(30, 60)),
        };
      });
      const terms = Array.from({ length: 3 }, () => ({
        salary: pick(r, [960000, 0, -5, 'abc', '450000', '', null, 12.5]),
        bonus: pick(r, [null, '', 50000, -500, 'lots', 0, '2000']),
        startDate: pick(r, [day(30), day(10), 'soon', '', null, '2026-6-1']),
        expires: pick(r, [null, '', day(5), day(40), day(10), 'soon', '2026-06']),
      }));
      const pairs = Array.from({ length: 4 }, () => [pick(r, STATUSES), pick(r, STATUSES)]);
      const template = Array.from({ length: r.int(3, 12) }, () => pick(r, TEMPLATE_LINES)).join('\n');
      const vars = {};
      for (const k of VAR_KEYS) if (r.next() < 0.8) vars[k] = pick(r, VAR_VALUES);

      const templates = r.next() < 0.3 ? pick(r, [null, 'x', []]) : Array.from({ length: r.int(0, 4) }, (_, i) => ({
        id: pick(r, [`t${i}`, 'bad id', '', `t${i}`, 'eng', 'eng']),
        name: pick(r, ['Teaching staff', ' Non-teaching ', '', null, 'Lab assistants']),
        tasks: Array.from({ length: r.int(0, 5) }, () => {
          const t = { title: pick(r, TITLES), category: pick(r, CATS) };
          if (r.next() < 0.8) t.offsetDays = pick(r, OFFSETS);
          return t;
        }),
        jobIds: Array.from({ length: r.int(0, 2) }, () => pick(r, ['j1', 'j2', 'bad id', 'j1'])),
        departments: Array.from({ length: r.int(0, 2) }, () => pick(r, ['Commerce', ' commerce ', 'Physics', '', null])),
      }));
      const job = r.next() < 0.2 ? null : { id: pick(r, ['j1', 'j2', 'j3']), department: pick(r, ['Commerce', 'COMMERCE', 'Physics', '', null]) };
      const onboarding = candidates.filter(() => r.next() < 0.6).map((c) => ({
        candidateId: c.id,
        tasks: Array.from({ length: r.int(0, 6) }, (_, k) => {
          const t = { id: `${c.id}t${k}`, title: `Task ${k}`, category: pick(r, ['Paperwork', 'IT & access', 'Team', '']), done: r.next() < 0.3 };
          if (r.next() < 0.75) t.offsetDays = pick(r, [-7, -3, -2, -1, 0, 1, 7, 2.5]);
          return t;
        }),
      }));
      const stages = r.next() < 0.3 ? [{ key: 'screen', label: 'Screening' }, { key: 'demo', label: 'Teaching demo' }, { key: 'interview', label: 'Interview' }, { key: 'bad key', label: 'x' }, { key: 'applied', label: 'Again' }] : null;
      const since = r.next() < 0.4 ? null : stamp(-r.int(0, 90));
      const scoped = r.next() < 0.5 ? null : candidates.filter(() => r.next() < 0.6).map((c) => c.id);
      out.push({
        today,
        nowTime,
        candidates,
        offers,
        terms,
        pairs,
        template,
        vars,
        templates,
        job,
        onboarding,
        stages,
        since,
        scoped,
        withinDays: pick(r, [1, 3, 7]),
        threshold: pick(r, [7, 14, 30]),
        retentionMonths: pick(r, [0, 6, 12, 24, 1]),
        offsets: [pick(r, OFFSETS), pick(r, OFFSETS), r.int(-30, 30)],
        desc: pick(r, DESCS),
        company: pick(r, NAMES),
        medianOf: Array.from({ length: r.int(0, 6) }, () => r.int(0, 40) / 2),
      });
    }
    return out;
  },
  run(m, c) {
    const { offers, onboard, fu, an, pipe, ret, rem, job, mail } = m;
    const now = new Date(`${c.today}T${c.nowTime}:00Z`);
    const nowIso = now.toISOString();
    const settings = c.stages ? { stages: c.stages } : null;
    const CODES = {
      'Enter a base salary.': 'ctc_required',
      'The signing bonus cannot be negative.': 'bonus_negative',
      'Pick a start date.': 'joining_date_required',
      'The expiry date is malformed.': 'expiry_malformed',
      'The offer should expire before the start date.': 'expiry_after_joining',
      'Describe the follow-up.': 'text_required',
      'Pick a due date.': 'due_date_required',
      'The due time is malformed.': 'due_time_malformed',
    };
    const codes = (errors) => errors.map((e) => CODES[e] ?? e);
    const plan = onboard.buildOnboardingPlan(onboard.templateForJob({ onboardingTemplates: c.templates }, c.job), (() => {
      let n = 0;
      return () => `task_${n++}`;
    })());
    const startDate = c.offers.find((o) => o.startDate)?.startDate ?? null;
    const state = {
      settings: { retentionMonths: c.retentionMonths, reminders: { interviews: false, proposals: false, scorecards: false } },
      candidates: c.candidates,
      offers: c.offers,
      interviews: [],
      scorecards: [],
      onboarding: Object.fromEntries(c.onboarding.map((p) => [p.candidateId, { tasks: p.tasks }])),
    };
    return {
      transitions: c.pairs.map(([f, t]) => offers.canTransition(f, t)),
      open: c.offers.map((o) => offers.isOpenOffer(o)),
      validation: c.terms.map((t) => codes(offers.validateOffer(t))),
      expiring: offers.expiringOffers(c.offers, { withinDays: c.withinDays, now }).map((o) => o.id),
      letter: offers.renderOfferLetter(c.template, c.vars),
      rendered: mail.renderTemplate(c.template, c.vars),
      templates: onboard.normalizeOnboardingTemplates(c.templates),
      templateId: onboard.templateForJob({ onboardingTemplates: c.templates }, c.job).id,
      plan,
      planDue: plan.map((t) => onboard.taskDueDate(t, startDate)),
      offsetLabels: c.offsets.map((o) => onboard.offsetLabel(o)),
      offsetShort: c.offsets.map((o) => onboard.offsetShort(o)),
      dueTasks: onboard.dueOnboardingTasks(state, { now, withinDays: c.withinDays }).map((d) => ({ candidateId: d.candidateId, taskId: d.task.id, dueDate: d.dueDate, overdue: d.overdue, daysLeft: d.daysLeft })),
      progress: c.onboarding.map((p) => onboard.onboardingProgress(p.tasks)),
      groups: c.onboarding.map((p) => onboard.groupTasks(p.tasks).map(([k, ts]) => [k, ts.map((t) => t.id)])),
      hireStart: c.candidates.map((x) => onboard.hireStartDate(c.offers, x.id)),
      followupDue: c.candidates.flatMap((x) => x.followups.map((f) => fu.followupDue(f, now))),
      dueFollowups: fu.dueFollowups(state, now).map((d) => ({ candidateId: d.candidate.id, followupId: d.followup.id, overdue: d.overdue })),
      sortedFollowups: c.candidates.map((x) => fu.sortFollowups(x.followups).map((f) => f.id)),
      followupValidation: c.candidates.flatMap((x) => x.followups.map((f) => codes(fu.validateFollowup(f)))),
      visits: c.candidates.map((x) => an.stageVisits(x, nowIso).map((v) => ({ stage: v.stage, days: v.days, open: v.to === null }))),
      reached: c.candidates.map((x) => [...an.reachedStages(x)].sort()),
      conversion: an.stageConversion(c.candidates, settings),
      timeToHire: an.timeToHire(c.candidates, { since: c.since }),
      averageDays: an.averageDaysInStage(c.candidates, nowIso, settings),
      sources: an.sourceBreakdown(c.candidates),
      outcomes: an.offerOutcomes(c.offers, c.scoped ? new Set(c.scoped) : null, { since: c.since }),
      median: an.median(c.medianOf),
      stageKeys: pipe.stagesFrom(settings).map((s) => s.key),
      funnel: pipe.funnelCounts(c.candidates, c.job?.id ?? null, settings),
      daysInStage: c.candidates.map((x) => pipe.daysInStage(x, now)),
      stale: pipe.staleCandidates(c.candidates, { thresholdDays: c.threshold, now }).map((x) => x.id),
      retention: ret.retentionDue(state, now).map((e) => ({ id: e.candidate.id, archivedAt: e.archivedAt })),
      reminders: rem.computeReminders(state, now).map((x) => ({ key: x.key, kind: x.kind })),
      escaped: job.escapeHtml(c.desc),
      descriptionHtml: job.descriptionHtml(c.desc),
      slug: job.subdomainBase(c.company),
    };
  },
};
