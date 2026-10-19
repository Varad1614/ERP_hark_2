// servicedesk-rules  <-  help-desk/src/shared/rules.js (+ runTriggers in src/server/tickets.js)
//   testCondition / matches / inView / applyActions / runTriggers / slaStatus / shortDuration /
//   renderPlaceholders / normTags over random tickets, conditions, triggers and SLA targets in
//   Everything's own vocabulary. The Rust engine runs the campus desk on SutraERP's words through
//   the same code (`Vocab`).
import { rng } from '../rng.mjs';

const LIB = 'src/help-desk/src/';
const HOUR = 3600 * 1000;
const pick = (r, a) => a[r.int(0, a.length - 1)];
const maybe = (r, p, f) => (r.next() < p ? f() : undefined);

const STATUSES = ['new', 'open', 'pending', 'hold', 'solved', 'closed', 'weird'];
const PRIORITIES = ['low', 'normal', 'high', 'urgent', '', 'bogus'];
const TYPES = ['question', 'incident', 'problem', 'task', 'other'];
const AGENTS = ['u1', 'u2', 'u3', ''];
const GROUPS = ['g1', 'g2', ''];
const CHANNELS = ['web', 'portal', 'email', 'phone', 'chat', 'api'];
const TAGS = ['vip', 'billing', 'refund', 'Wi-Fi', 'urgent', 'café', 'a b', 'x,y'];
const WORDS = ['Projector', 'leak', 'Wi-Fi down', 'refund please', 'hostel fan', 'lab key'];

function ticket(r, now) {
  const createdAt = now - r.int(0, 400) * HOUR - r.int(0, 59) * 60000;
  const t = {
    status: pick(r, STATUSES.slice(0, 6)),
    subject: `${pick(r, WORDS)} ${r.int(1, 99)}`,
    tags: Array.from({ length: r.int(0, 3) }, () => pick(r, ['vip', 'billing', 'refund', 'urgent', 'wi-fi'])).filter((v, i, a) => a.indexOf(v) === i),
    followers: Array.from({ length: r.int(0, 2) }, () => pick(r, ['u1', 'u2', 'u3'])).filter((v, i, a) => a.indexOf(v) === i),
    createdAt,
    fields: {},
  };
  const p = maybe(r, 0.85, () => pick(r, PRIORITIES.slice(0, 4)));
  if (p) t.priority = p;
  const ty = maybe(r, 0.6, () => pick(r, TYPES.slice(0, 4)));
  if (ty) t.type = ty;
  const a = maybe(r, 0.6, () => pick(r, AGENTS.slice(0, 3)));
  if (a) t.assignee = a;
  const g = maybe(r, 0.5, () => pick(r, GROUPS.slice(0, 2)));
  if (g) t.group = g;
  if (r.next() < 0.7) t.requester = `c${r.int(1, 4)}`;
  if (r.next() < 0.4) t.org = `o${r.int(1, 3)}`;
  if (r.next() < 0.9) t.via = pick(r, CHANNELS);
  if (r.next() < 0.8) t.updatedAt = createdAt + r.int(0, 200) * HOUR;
  if (t.status === 'solved' || t.status === 'closed' || r.next() < 0.1) t.solvedAt = createdAt + r.int(1, 300) * HOUR;
  if (r.next() < 0.3) t.csat = { score: pick(r, ['good', 'bad']) };
  if (r.next() < 0.4) t.fields.room = pick(r, ['LH-1', 'LH-2', '']);
  if (r.next() < 0.3) t.fields.count = r.int(0, 5);
  if (r.next() < 0.05) t.spam = true;
  if (r.next() < 0.05) t.deleted = true;
  if (r.next() < 0.05) t.mergedInto = 't9';
  if (r.next() < 0.6) {
    const agent = maybe(r, 0.6, () => createdAt + r.int(0, 30) * 60000);
    if (agent) t.lastAgentReplyAt = agent;
    if (r.next() < 0.5) t.firstReplyAt = createdAt + r.int(5, 120) * 60000;
    if (r.next() < 0.7) t.lastCustomerAt = createdAt + r.int(0, 300) * 60000;
  }
  return t;
}

function condition(r, timeless = false) {
  const fields = ['status', 'priority', 'type', 'assignee', 'group', 'requester', 'organization', 'channel', 'tags', 'subject', 'followers', 'created', 'updated', 'solved', 'satisfaction', 'actor', 'comment', 'field:room', 'field:count', 'unknown'];
  // runTriggers passes no clock to the original, so trigger conditions skip the hour fields.
  const field = pick(r, timeless ? fields.filter((f) => !['created', 'updated', 'solved'].includes(f)) : fields);
  const op = {
    status: () => pick(r, ['is', 'is_not', 'less_than', 'greater_than', 'changed_to']),
    priority: () => pick(r, ['is', 'is_not', 'less_than', 'greater_than']),
    assignee: () => pick(r, ['is', 'is_not', 'changed']),
    tags: () => pick(r, ['includes', 'excludes']),
    subject: () => pick(r, ['contains', 'not_contains']),
    created: () => pick(r, ['less_than', 'greater_than']),
    updated: () => pick(r, ['less_than', 'greater_than']),
    solved: () => pick(r, ['less_than', 'greater_than']),
    'field:room': () => pick(r, ['is', 'is_not', 'present', 'absent']),
    'field:count': () => pick(r, ['is', 'is_not', 'present', 'absent']),
  }[field]?.() ?? pick(r, ['is', 'is_not']);
  const value = {
    status: () => pick(r, STATUSES),
    priority: () => pick(r, PRIORITIES),
    type: () => pick(r, TYPES),
    assignee: () => pick(r, [...AGENTS, 'me']),
    group: () => pick(r, GROUPS),
    requester: () => `c${r.int(1, 4)}`,
    organization: () => pick(r, ['o1', 'o2', '']),
    channel: () => pick(r, CHANNELS),
    tags: () => (r.next() < 0.5 ? pick(r, TAGS) : [pick(r, TAGS), pick(r, TAGS)]),
    subject: () => pick(r, ['leak', 'WI-FI', 'fan', '', 'zzz']),
    followers: () => '',
    created: () => pick(r, [1, 24, 72, 168, '48', '', ' ', 'x']),
    updated: () => pick(r, [1, 24, 72, 168, '48', '']),
    solved: () => pick(r, [1, 24, 168, '']),
    satisfaction: () => pick(r, ['good', 'bad', 'unrated']),
    actor: () => pick(r, ['agent', 'customer', 'system']),
    comment: () => pick(r, ['public', 'private', 'present', 'absent']),
    'field:room': () => pick(r, ['LH-1', 'LH-2', '']),
    'field:count': () => pick(r, [0, 1, 2, '3', '']),
  }[field]?.() ?? 'x';
  return { field, op, value };
}

function action(r) {
  const field = pick(r, ['status', 'priority', 'type', 'assignee', 'group', 'add_tags', 'remove_tags', 'set_tags', 'add_follower', 'notify_requester', 'notify_email', 'field:room', 'field:count', 'nope']);
  const value = {
    status: () => pick(r, STATUSES),
    priority: () => pick(r, PRIORITIES),
    type: () => pick(r, TYPES),
    assignee: () => pick(r, [...AGENTS, 'me']),
    group: () => pick(r, GROUPS),
    add_tags: () => pick(r, TAGS),
    remove_tags: () => pick(r, TAGS),
    set_tags: () => (r.next() < 0.5 ? `${pick(r, TAGS)} ${pick(r, TAGS)}` : [pick(r, TAGS)]),
    add_follower: () => pick(r, ['u1', 'u4', 'me', '']),
    'field:room': () => pick(r, ['LH-3', '', null]),
    'field:count': () => pick(r, [7, '', null]),
  }[field]?.();
  const a = { field };
  if (value !== undefined) a.value = value;
  if (field.startsWith('notify_')) {
    if (r.next() < 0.5) a.to = pick(r, ['ops@example.in', '']);
    if (r.next() < 0.7) a.subject = 'Re: {{ticket.subject}}';
    if (r.next() < 0.7) a.body = 'Hello {{requester.first_name}}';
  }
  return a;
}

export default {
  fn: 'testCondition / matches / inView / applyActions / runTriggers / slaStatus / shortDuration / renderPlaceholders / normTags',
  module: `${LIB}shared/rules.js`,
  modules: { rules: `${LIB}shared/rules.js`, model: `${LIB}shared/model.js`, tickets: `${LIB}server/tickets.js` },
  paths: [`${LIB}shared/`, `${LIB}server/`],
  out: 'modules/campus/tests/golden/servicedesk_rules.json',
  cases() {
    const r = rng(2602);
    const out = [];
    for (let b = 1; b <= 200; b++) {
      const now = Date.UTC(2026, r.int(0, 11), r.int(1, 28), r.int(0, 23));
      const t = ticket(r, now);
      const ctx = { now };
      if (r.next() < 0.8) ctx.me = pick(r, ['u1', 'u2', '']);
      if (r.next() < 0.5) {
        const before = { ...t, tags: [...t.tags] };
        if (r.next() < 0.6) before.status = pick(r, STATUSES.slice(0, 6));
        if (r.next() < 0.5) before.assignee = pick(r, AGENTS);
        ctx.before = before;
      }
      if (r.next() < 0.7) ctx.actor = pick(r, ['agent', 'customer', 'system']);
      if (r.next() < 0.5) ctx.comment = { public: r.next() < 0.6 };
      const conditions = { all: Array.from({ length: r.int(0, 3) }, () => condition(r)), any: Array.from({ length: r.int(0, 2) }, () => condition(r)) };
      const actions = Array.from({ length: r.int(0, 5) }, () => action(r));
      const triggers = Array.from({ length: r.int(0, 4) }, (_, i) => ({
        id: `tr${i}`,
        title: `Trigger ${i}`,
        active: r.next() < 0.85,
        event: pick(r, ['created', 'updated', 'any', undefined]),
        conditions: { all: Array.from({ length: r.int(0, 2) }, () => condition(r, true)), any: Array.from({ length: r.int(0, 1) }, () => condition(r, true)) },
        actions: Array.from({ length: r.int(1, 3) }, () => action(r)),
      }));
      const target = () => ({ first: pick(r, [0, 30, 60, 240]), next: pick(r, [0, 60, 120]), resolve: pick(r, [0, 480, 1440, 4320]) });
      const sla = { enabled: r.next() < 0.9, targets: {} };
      for (const p of ['low', 'normal', 'high', 'urgent']) if (r.next() < 0.85) sla.targets[p] = target();
      const durations = Array.from({ length: 4 }, () => (r.next() < 0.3 ? -1 : 1) * pick(r, [500, 30000, 59000, 600000, 3600000, 7200000, 86400000 * 3, 86400000 * 20]) * (0.5 + r.next()));
      const text = pick(r, ['Hi {{requester.first_name}}, re #{{ticket.id}}', '{{ ticket.subject }} — {{agent.name}} {{nope}}', '{{desk.name}}: {{Ticket.ID}} {{ ticket.link}}', '{{comment.text}}{{requester.email}}{{ticket.priority}}', 'plain', '{{{{ticket.status}}}}']);
      const data = { ticket: { id: r.int(1, 999), subject: t.subject, status: t.status }, requester: { name: pick(r, ['Aarav  Desai', 'Meera', '', null]), email: 'a@x.in' }, agent: { name: pick(r, ['Ravi Kumar', undefined]) }, desk: { name: 'Campus desk' }, link: pick(r, ['https://x/1', undefined]), commentText: pick(r, ['ok', '']) };
      const view = pick(r, [{ special: 'spam' }, { special: 'deleted' }, { conditions }, { conditions }]);
      out.push({ ticket: t, ctx, conditions, actions, triggers, event: pick(r, ['created', 'updated']), sla, durations, text, data, view, tagInput: pick(r, TAGS.concat([['A', 'a', ' b '], 'one, two,,three'])) });
    }
    return out;
  },
  run(m, c) {
    const { rules, model, tickets } = m;
    const conds = [...c.conditions.all, ...c.conditions.any];
    const applied = rules.applyActions(c.ticket, c.actions, { me: c.ctx.me });
    const trig = tickets.runTriggers(c.triggers, { before: c.ctx.before, after: c.ticket, event: c.event, actor: c.ctx.actor, comment: c.ctx.comment, me: c.ctx.me });
    return {
      tests: conds.map((x) => rules.testCondition(x, c.ticket, c.ctx)),
      matches: rules.matches(c.conditions, c.ticket, c.ctx),
      inView: rules.inView(c.view, c.ticket, c.ctx),
      applied,
      triggered: { ticket: trig.ticket, fired: trig.fired, notifications: trig.notifications },
      sla: rules.slaStatus(c.ticket, c.sla, c.ctx.now),
      durations: c.durations.map((d) => rules.shortDuration(d)),
      rendered: rules.renderPlaceholders(c.text, c.data),
      tags: model.normTags(c.tagInput),
    };
  },
};
