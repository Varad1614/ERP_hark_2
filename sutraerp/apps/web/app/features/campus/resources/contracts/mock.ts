/** In-memory `ContractsApi` for mock mode and screen tests (a simplified clock, not the rules). */
import type { ContractsApi } from "./api";
import { aqarYear, isMou } from "./contracts";
import type { Activity, Contract, ContractDetail, ContractSettings, Obligation, RenewalInfo } from "./types";

const DAY = 86_400_000;
const iso = (ms: number) => new Date(ms).toISOString().slice(0, 10);
const days = (a: string, b: string) => Math.round((Date.parse(b) - Date.parse(a)) / DAY);

function clock(c: Contract, today: string, soon: number): RenewalInfo {
  const none = { noticeDate: null, noticeDaysLeft: null, decision: c.renewal.decision, handled: false };
  if (c.status !== "active") return { phase: "none", date: null, daysLeft: null, ...none };
  if (!c.endOn) return { phase: "evergreen", date: null, daysLeft: null, ...none };
  const left = days(today, c.endOn);
  const handled = !!c.renewal.decision && c.renewal.decision.forTermEnd === c.endOn;
  if (left < 0) return { phase: "expired", date: c.endOn, daysLeft: left, ...none, handled };
  const noticeDate = c.noticeDays > 0 ? iso(Date.parse(c.endOn) - c.noticeDays * DAY) : null;
  const phase = noticeDate && left <= c.noticeDays ? "notice" : left <= soon ? "soon" : "ok";
  return { phase, date: c.endOn, daysLeft: left, noticeDate, noticeDaysLeft: noticeDate ? days(today, noticeDate) : null, decision: c.renewal.decision, handled };
}

export function sampleContract(n: number, over: Partial<Contract> = {}): Contract {
  return {
    id: `con-${n}`, institutionId: "i", title: `Contract ${n}`, kind: "amc", status: "active", supplierId: null, counterpartyName: `Vendor ${n}`,
    counterpartyScope: "national", contactName: null, contactEmail: null, contactPhone: null, value: "4500.00", currency: "INR", billing: "monthly",
    annualValue: 54000, startOn: null, endOn: null, autoRenew: false, termMonths: 12, noticeDays: 30, purposes: [], tags: [], notes: null,
    renewal: { phase: "evergreen", date: null, daysLeft: null, noticeDate: null, noticeDaysLeft: null, decision: null, handled: false },
    attention: false, agreementEnd: null, openObligations: 0, version: 1, ...over,
  };
}

export function createMockContractsApi(today: string = iso(Date.now())): ContractsApi {
  let settings: ContractSettings = { soonDays: 60, obligationDays: 30, defaultNoticeDays: 30, alarmDays: [7, 1] };
  const at = (n: number) => iso(Date.parse(today) + n * DAY);
  const contracts: Contract[] = [
    sampleContract(1, { title: "RO plant AMC", counterpartyName: "AquaPure Services", endOn: at(20), autoRenew: true }),
    sampleContract(2, { title: "MoU: Tata Motors internships", kind: "mou_industry", counterpartyName: "Tata Motors Ltd, Pune", value: null, billing: "one-time", annualValue: 0, startOn: at(-200), endOn: at(500), purposes: ["internship"] }),
  ];
  const obligations: Obligation[] = [
    { id: "ob-1", contractId: "con-1", text: "Service visit report", owner: "them", dueOn: at(-3), done: false, repeat: { every: "monthly", anchor: at(-3), until: null, count: null, contractEnd: false }, completed: 0, daysLeft: -3, version: 1 },
  ];
  const activities: Activity[] = [];
  let seq = 10;
  const refresh = (c: Contract): Contract => {
    const renewal = clock(c, today, settings.soonDays);
    const open = obligations.filter((o) => o.contractId === c.id && !o.done).length;
    return Object.assign(c, { renewal, attention: c.status === "active" && !renewal.handled && (renewal.phase === "notice" || renewal.phase === "expired"), openObligations: open });
  };
  const find = (id: string) => {
    const c = contracts.find((x) => x.id === id);
    if (!c) throw new Error("contract not found");
    return refresh(c);
  };
  const detail = (id: string): ContractDetail => ({ contract: find(id), obligations: obligations.filter((o) => o.contractId === id), terms: [], activities: activities.filter((a) => a.contractId === id) });
  const ob = (id: string) => {
    const o = obligations.find((x) => x.id === id);
    if (!o) throw new Error("obligation not found");
    return o;
  };
  return {
    settings: async () => settings,
    saveSettings: async (_i, s) => {
      if (s.soonDays < 1 || s.soonDays > 365) throw new Error("soon horizon must be 1 to 365 days");
      settings = s;
      return s;
    },
    list: async (_i, q = {}) => contracts.map(refresh)
      .filter((c) => (q.kind === "mou" ? isMou(c.kind) : !q.kind || c.kind === q.kind))
      .filter((c) => !q.attention || c.attention)
      .filter((c) => !q.q || `${c.title} ${c.counterpartyName}`.toLowerCase().includes(q.q.toLowerCase())),
    create: async (_i, input) => {
      if (!input.title.trim()) throw new Error("title is required");
      if (input.autoRenew && !input.endOn) throw new Error("an auto-renewing contract needs its first term's end");
      const c = sampleContract(++seq, { ...input, id: `con-${seq}`, counterpartyName: input.counterpartyName ?? "", value: input.value ?? null, noticeDays: input.noticeDays ?? settings.defaultNoticeDays } as Partial<Contract>);
      contracts.push(c);
      return detail(c.id);
    },
    get: async (id) => detail(id),
    update: async (id, input) => {
      const c = find(id);
      if (input.version !== c.version) throw new Error("version conflict");
      Object.assign(c, input, { version: c.version + 1 });
      return detail(id);
    },
    decide: async (id, d) => {
      const c = find(id);
      c.renewal.decision = d.kind ? { kind: d.kind, date: today, note: d.note ?? "", forTermEnd: c.renewal.date, until: d.until ?? null } : null;
      return detail(id);
    },
    addObligation: async (contractId, input) => {
      const o: Obligation = {
        id: `ob-${++seq}`, contractId, text: input.text, owner: input.owner ?? "us", dueOn: input.dueOn ?? null, done: false,
        repeat: input.repeat && input.repeat.every !== "none" && input.dueOn ? { every: input.repeat.every, anchor: input.dueOn, until: input.repeat.until ?? null, count: input.repeat.count ?? null, contractEnd: !!input.repeat.contractEnd } : null,
        completed: 0, daysLeft: input.dueOn ? days(today, input.dueOn) : null, version: 1,
      };
      obligations.push(o);
      return o;
    },
    complete: async (id) => {
      const o = ob(id);
      if (o.done) throw new Error("obligation is already done");
      o.completed += 1;
      if (o.repeat && o.dueOn && (o.repeat.count === null || o.completed < o.repeat.count)) {
        const months = { monthly: 1, quarterly: 3, annually: 12 }[o.repeat.every];
        const d = new Date(`${o.repeat.anchor}T00:00:00Z`);
        d.setUTCMonth(d.getUTCMonth() + months * o.completed);
        o.dueOn = iso(d.getTime());
        o.daysLeft = days(today, o.dueOn);
      } else o.done = true;
      return o;
    },
    undo: async (id) => {
      const o = ob(id);
      if (!o.completed && !o.done) throw new Error("nothing to undo");
      o.done = false;
      o.completed = Math.max(0, o.completed - 1);
      return o;
    },
    removeObligation: async (id) => {
      obligations.splice(obligations.indexOf(ob(id)), 1);
    },
    addActivity: async (contractId, input) => {
      if (input.heldOn > today) throw new Error("an activity cannot be in the future");
      const a: Activity = { id: `act-${++seq}`, contractId, heldOn: input.heldOn, kind: input.kind, title: input.title, students: input.students ?? 0, staff: input.staff ?? 0, outcome: input.outcome ?? null, evidenceRef: input.evidenceRef ?? null };
      activities.push(a);
      return a;
    },
    removeActivity: async (id) => {
      const i = activities.findIndex((a) => a.id === id);
      if (i >= 0) activities.splice(i, 1);
    },
    dashboard: async () => {
      const all = contracts.map(refresh);
      const active = all.filter((c) => c.status === "active" && c.renewal.phase !== "expired");
      const inr = active.reduce((s, c) => s + c.annualValue, 0);
      return {
        today,
        stats: { active: active.length, expiring60: active.filter((c) => c.renewal.daysLeft !== null && c.renewal.daysLeft <= 60).length, openObligations: obligations.filter((o) => !o.done).length, expired: all.filter((c) => c.renewal.phase === "expired").length, annualByCurrency: inr ? { INR: inr } : {} },
        attention: all.filter((c) => c.attention),
        upcoming: all.filter((c) => c.renewal.daysLeft !== null && c.renewal.daysLeft >= 0 && c.renewal.daysLeft <= settings.soonDays),
        dueObligations: obligations.filter((o) => !o.done && o.daysLeft !== null && o.daysLeft <= settings.obligationDays)
          .map((o) => ({ obligation: o, contractTitle: find(o.contractId).title, counterpartyName: find(o.contractId).counterpartyName })),
      };
    },
    reconcile: async () => ({ contractsUpdated: 0, termsRecorded: 0, terminated: 0, decisionsCleared: 0 }),
    evidence: async (_i, year = aqarYear(today)) => {
      const start = Number(year.slice(0, 4));
      const from = `${start}-07-01`;
      const to = `${start + 1}-06-30`;
      const rows = contracts.filter((c) => isMou(c.kind)).map((c) => {
        const acts = activities.filter((a) => a.contractId === c.id && a.heldOn >= from && a.heldOn <= to);
        return { contractId: c.id, title: c.title, partner: c.counterpartyName, kind: c.kind, scope: c.counterpartyScope, status: c.status, startOn: c.startOn, endsOn: c.agreementEnd, purposes: c.purposes, functional: acts.length > 0, activities: acts };
      });
      const fn = rows.filter((r) => r.functional);
      const acts = fn.flatMap((r) => r.activities);
      const count = (xs: string[]) => xs.reduce<Record<string, number>>((m, k) => ({ ...m, [k]: (m[k] ?? 0) + 1 }), {});
      return {
        year, from, to, mous: rows,
        totals: { inForce: rows.length, functional: fn.length, activities: acts.length, students: acts.reduce((s, a) => s + a.students, 0), staff: acts.reduce((s, a) => s + a.staff, 0), functionalByKind: count(fn.map((r) => r.kind)), functionalByScope: count(fn.map((r) => r.scope)), activitiesByKind: count(acts.map((a) => a.kind)) },
      };
    },
    calendarUrl: (i, contractId) => (contractId ? `/api/v1/resources/contracts/${contractId}/calendar.ics` : `/api/v1/resources/institutions/${i}/contracts/calendar.ics`),
  };
}
