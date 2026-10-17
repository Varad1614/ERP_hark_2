/**
 * In-memory `DeskApi` for mock mode and screen tests. Lightweight on purpose: the server's
 * rules engine (golden-tested against Everything's help desk) is authoritative.
 */
import type { DeskApi } from "./api";
import { PRIORITIES } from "./desk";
import type { Condition, Queue, QueueRow, Rule, SlaPolicy, View } from "./types";

const H = 3_600_000;

function defaults(): SlaPolicy["targets"] {
  const hours: Record<string, number> = { low: 168, medium: 72, high: 24, critical: 4 };
  return Object.fromEntries(PRIORITIES.map((p) => [p, { first: Math.max(60, (hours[p]! * 60) / 10), next: 0, resolve: hours[p]! * 60 }]));
}

const SYSTEM: View[] = [
  { id: "sys-unsolved", title: "All unsolved tickets", conditions: { all: [{ field: "status", op: "less_than", value: "resolved" }], any: [] }, sort: "sla_asc", system: true },
  { id: "sys-unassigned", title: "Unassigned tickets", conditions: { all: [{ field: "assignee", op: "is", value: "" }], any: [] }, sort: "created_asc", system: true },
];

function test(c: Condition, r: QueueRow): boolean {
  const t = r.ticket;
  const v = String(c.value ?? "");
  switch (c.field) {
    case "subject":
      return t.title.toLowerCase().includes(v.toLowerCase()) === (c.op === "contains");
    case "priority":
      return (t.priority === v) === (c.op === "is");
    case "type":
      return (t.category === v) === (c.op === "is");
    case "tags":
      return r.tags.some((x) => v.toLowerCase().split(/[\s,]+/).includes(x)) === (c.op === "includes");
    case "assignee":
      return ((t.assignee ?? "") === v) === (c.op === "is");
    case "status":
      return c.op === "less_than" ? !["resolved", "closed", "cancelled", "merged"].includes(t.status) : (t.status === v) === (c.op === "is");
    default:
      return false;
  }
}

export function createMockDeskApi(seed: QueueRow[] = []): DeskApi {
  let rules: Rule[] = [];
  let views: View[] = [];
  let policy: SlaPolicy = { enabled: true, targets: defaults(), customised: false, version: 0 };
  const rows = [...seed];
  let seq = 0;
  const queue = (title: string, items: QueueRow[]): Queue => ({ title, breached: items.filter((r) => r.sla?.breached).length, items });
  const matching = (all: Condition[]) => rows.filter((r) => all.every((c) => test(c, r)));
  return {
    rules: async () => rules,
    createRule: async (_i, input) => {
      if (input.actions.some((a) => a.field === "status")) throw new Error("status is not a rule action");
      const r: Rule = { id: `rule-${++seq}`, title: input.title, event: input.event ?? "any", position: rules.length * 10 + 10, active: input.active ?? true, conditions: input.conditions ?? { all: [], any: [] }, actions: input.actions, version: 1, updatedAt: new Date().toISOString() };
      rules = [...rules, r];
      return r;
    },
    updateRule: async (id, version, input) => {
      const cur = rules.find((r) => r.id === id);
      if (!cur || cur.version !== version) throw new Error("rule changed");
      const next: Rule = { ...cur, ...input, event: input.event ?? cur.event, active: input.active ?? cur.active, position: input.position ?? cur.position, conditions: input.conditions ?? cur.conditions, version: version + 1 };
      rules = rules.map((r) => (r.id === id ? next : r));
      return next;
    },
    deleteRule: async (id) => {
      rules = rules.filter((r) => r.id !== id);
    },
    preview: async (_i, input) => {
      const hit = matching(input.conditions?.all ?? []);
      return { matched: hit.length, hits: hit.map((r) => ({ ticketId: r.ticket.id, number: r.ticket.number, title: r.ticket.title, changes: {} })) };
    },
    views: async () => [...SYSTEM, ...views],
    createView: async (_i, input) => {
      const v: View = { id: `view-${++seq}`, title: input.title, conditions: input.conditions ?? { all: [], any: [] }, sort: input.sort ?? "updated_desc", system: false };
      views = [...views, v];
      return v;
    },
    deleteView: async (id) => {
      views = views.filter((v) => v.id !== id);
    },
    viewTickets: async (_i, viewId) => {
      const v = [...SYSTEM, ...views].find((x) => x.id === viewId);
      return queue(v?.title ?? "View", matching(v?.conditions.all ?? []));
    },
    board: async () => queue("SLA board", rows.filter((r) => r.sla).sort((a, b) => a.sla!.dueAt - b.sla!.dueAt)),
    policy: async () => policy,
    savePolicy: async (_i, input) => {
      if (input.version !== policy.version) throw new Error("policy changed");
      policy = { enabled: input.enabled, targets: { ...defaults(), ...input.targets }, customised: true, version: policy.version + 1 };
      return policy;
    },
    tags: async (id, _v, add, remove) => {
      const r = rows.find((x) => x.ticket.id === id);
      if (!r) throw new Error("ticket not found");
      r.tags = [...r.tags.filter((t) => !remove.includes(t)), ...add.map((t) => t.toLowerCase()).filter((t) => !r.tags.includes(t))];
      return r;
    },
    automation: async () => [],
  };
}

/** A sample row for tests and mock mode. */
export function sampleRow(n: number, over: Partial<QueueRow["ticket"]> = {}, dueInHours = 2): QueueRow {
  const now = Date.now();
  return {
    ticket: {
      id: `t-${n}`,
      number: `SR-${String(n).padStart(6, "0")}`,
      institutionId: "inst",
      category: "facility",
      source: "self_service",
      sourceRefId: null,
      isComplaint: false,
      nodeId: null,
      requesterKind: null,
      requesterId: null,
      title: `Ticket ${n}`,
      description: null,
      priority: "medium",
      status: "open",
      statusReason: null,
      assignee: null,
      slaDueAt: new Date(now + dueInHours * H).toISOString(),
      slaBreached: false,
      escalationLevel: 0,
      resolvedAt: null,
      closedAt: null,
      resolution: null,
      canonicalTicketId: null,
      createdAt: new Date(now - H).toISOString(),
      version: 1,
      ...over,
    },
    tags: [],
    firstResponseAt: null,
    sla: { metric: "resolve", dueAt: now + dueInHours * H, remaining: dueInHours * H, breached: dueInHours <= 0 },
    slaIn: `${dueInHours}h`,
  };
}
