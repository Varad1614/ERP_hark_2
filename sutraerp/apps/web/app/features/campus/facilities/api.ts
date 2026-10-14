/**
 * Slice-8A API layer (campus infrastructure, room booking, service desk, maintenance, labs).
 * Same conventions as the Batch 6/7 slices: one interface, an HTTP implementation (CSRF,
 * `Idempotency-Key` on creates / bulk commands, `If-Match` on state commands, problem+json →
 * `ApiProblem`) and an in-memory mock (`./mock`). Every path template is checked against the
 * generated OpenAPI `paths`.
 */
import { ApiProblem, isProblem, isUnsafeMethod, newIdempotencyKey, type paths } from "@sutra/api-client";
import { getCsrfToken } from "../../../lib/api";
import { fill } from "../../institution/api";
import type {
  BulkSummary,
  ChecklistItem,
  Equipment,
  EquipmentStatus,
  InfraNode,
  InfraNodeDetail,
  Items,
  Lab,
  LabDetail,
  NewNode,
  NewPlan,
  NewReservation,
  NewSession,
  NewTicket,
  NodeCommand,
  Plan,
  PlanCommand,
  PlanDetail,
  Reservation,
  Session,
  SessionCommand,
  SpaceCalendar,
  Ticket,
  TicketCommand,
  TicketList,
  TicketPriority,
  WorkOrder,
  WorkOrderCommand,
  WorkOrderDetail,
} from "./types";

export interface FacilitiesApi {
  // infrastructure
  tree(institutionId: string): Promise<InfraNode[]>;
  node(id: string): Promise<InfraNodeDetail>;
  createNode(institutionId: string, input: NewNode): Promise<InfraNodeDetail>;
  nodeCommand(id: string, version: number, cmd: NodeCommand, reason?: string): Promise<InfraNodeDetail>;
  setCapacity(id: string, version: number, capacity: number, reason?: string): Promise<InfraNodeDetail>;
  calendar(spaceId: string, from: string, to: string): Promise<SpaceCalendar>;
  // reservations
  reservations(institutionId: string, status?: string): Promise<Reservation[]>;
  book(institutionId: string, input: NewReservation): Promise<Reservation>;
  reservationCommand(id: string, version: number, cmd: "confirm" | "cancel", reason?: string): Promise<Reservation>;
  // service desk
  tickets(institutionId: string, filter?: { status?: string; breached?: boolean; open?: boolean }): Promise<TicketList>;
  createTicket(institutionId: string, input: NewTicket): Promise<Ticket>;
  ticketCommand(id: string, version: number, cmd: TicketCommand, reason?: string, priority?: TicketPriority): Promise<Ticket>;
  assignTicket(id: string, version: number, assignee: string): Promise<Ticket>;
  resolveTicket(id: string, version: number, resolution: string): Promise<Ticket>;
  slaCheck(institutionId: string): Promise<BulkSummary>;
  // work orders
  workOrders(institutionId: string, status?: string): Promise<WorkOrder[]>;
  raiseWorkOrder(ticketId: string, assignee?: string): Promise<WorkOrderDetail>;
  assignWorkOrder(id: string, version: number, assignee: string): Promise<WorkOrderDetail>;
  workOrderCommand(id: string, version: number, cmd: WorkOrderCommand, reason?: string): Promise<WorkOrderDetail>;
  completeWorkOrder(id: string, version: number, notes: string): Promise<WorkOrderDetail>;
  verifyWorkOrder(id: string, version: number, accepted: boolean, notes?: string): Promise<WorkOrderDetail>;
  // preventive maintenance
  plans(institutionId: string): Promise<Plan[]>;
  plan(id: string): Promise<PlanDetail>;
  createPlan(institutionId: string, input: NewPlan): Promise<PlanDetail>;
  planCommand(id: string, version: number, cmd: PlanCommand, reason?: string): Promise<PlanDetail>;
  generate(planId: string, through: string): Promise<BulkSummary>;
  // labs
  labs(institutionId: string): Promise<Lab[]>;
  lab(id: string): Promise<LabDetail>;
  createLab(institutionId: string, nodeId: string, name?: string): Promise<LabDetail>;
  readiness(id: string, version: number, checklist: ChecklistItem[], notes?: string): Promise<LabDetail>;
  addEquipment(labId: string, code: string, name: string): Promise<Equipment>;
  equipmentStatus(id: string, version: number, status: EquipmentStatus, reason?: string): Promise<Equipment>;
  createSession(labId: string, input: NewSession): Promise<Session>;
  sessionCommand(id: string, version: number, cmd: SessionCommand, reason?: string): Promise<Session>;
}

const B = "/api/v1/campus";
export const PATHS = {
  tree: `${B}/institutions/{id}/infrastructure-tree`,
  nodes: `${B}/institutions/{id}/infrastructure-nodes`,
  node: `${B}/infrastructure-nodes/{id}`,
  activate: `${B}/infrastructure-nodes/{id}/activate`,
  suspend: `${B}/infrastructure-nodes/{id}/suspend`,
  restore: `${B}/infrastructure-nodes/{id}/restore`,
  decommission: `${B}/infrastructure-nodes/{id}/decommission`,
  archive: `${B}/infrastructure-nodes/{id}/archive`,
  capacity: `${B}/infrastructure-nodes/{id}/capacity`,
  calendar: `${B}/spaces/{id}/availability`,
  reservations: `${B}/institutions/{id}/reservations`,
  confirm: `${B}/reservations/{id}/confirm`,
  cancelReservation: `${B}/reservations/{id}/cancel`,
  tickets: `${B}/institutions/{id}/tickets`,
  slaCheck: `${B}/institutions/{id}/tickets/sla-check`,
  triage: `${B}/tickets/{id}/triage`,
  assign: `${B}/tickets/{id}/assign`,
  start: `${B}/tickets/{id}/start`,
  hold: `${B}/tickets/{id}/hold`,
  resolve: `${B}/tickets/{id}/resolve`,
  close: `${B}/tickets/{id}/close`,
  reopen: `${B}/tickets/{id}/reopen`,
  cancelTicket: `${B}/tickets/{id}/cancel`,
  escalate: `${B}/tickets/{id}/escalate`,
  ticketWorkOrders: `${B}/tickets/{id}/work-orders`,
  workOrders: `${B}/institutions/{id}/work-orders`,
  woAssign: `${B}/work-orders/{id}/assign`,
  woStart: `${B}/work-orders/{id}/start`,
  woComplete: `${B}/work-orders/{id}/complete`,
  woVerify: `${B}/work-orders/{id}/verify`,
  woReopen: `${B}/work-orders/{id}/reopen`,
  woCancel: `${B}/work-orders/{id}/cancel`,
  plans: `${B}/institutions/{id}/maintenance-plans`,
  plan: `${B}/maintenance-plans/{id}`,
  planSubmit: `${B}/maintenance-plans/{id}/submit`,
  planActivate: `${B}/maintenance-plans/{id}/activate`,
  planSuspend: `${B}/maintenance-plans/{id}/suspend`,
  planRetire: `${B}/maintenance-plans/{id}/retire`,
  generate: `${B}/maintenance-plans/{id}/generate`,
  labs: `${B}/institutions/{id}/labs`,
  lab: `${B}/labs/{id}`,
  readiness: `${B}/labs/{id}/readiness`,
  equipment: `${B}/labs/{id}/equipment`,
  equipmentStatus: `${B}/lab-equipment/{id}/status`,
  sessions: `${B}/labs/{id}/sessions`,
  sessionReady: `${B}/practical-sessions/{id}/ready`,
  sessionStart: `${B}/practical-sessions/{id}/start`,
  sessionComplete: `${B}/practical-sessions/{id}/complete`,
  sessionCancel: `${B}/practical-sessions/{id}/cancel`,
} as const satisfies Record<string, keyof paths>;

const NODE_CMD: Record<NodeCommand, string> = {
  activate: PATHS.activate,
  suspend: PATHS.suspend,
  restore: PATHS.restore,
  decommission: PATHS.decommission,
  archive: PATHS.archive,
};
const TICKET_CMD: Record<TicketCommand, string> = {
  triage: PATHS.triage,
  start: PATHS.start,
  hold: PATHS.hold,
  close: PATHS.close,
  cancel: PATHS.cancelTicket,
  escalate: PATHS.escalate,
  reopen: PATHS.reopen,
};
const WO_CMD: Record<WorkOrderCommand, string> = { start: PATHS.woStart, reopen: PATHS.woReopen, cancel: PATHS.woCancel };
const PLAN_CMD: Record<PlanCommand, string> = {
  submit: PATHS.planSubmit,
  activate: PATHS.planActivate,
  suspend: PATHS.planSuspend,
  retire: PATHS.planRetire,
};
const SESSION_CMD: Record<SessionCommand, string> = {
  ready: PATHS.sessionReady,
  start: PATHS.sessionStart,
  complete: PATHS.sessionComplete,
  cancel: PATHS.sessionCancel,
};

type Fetch = (input: string, init: RequestInit) => Promise<Response>;

interface RequestOptions {
  body?: unknown;
  ifMatch?: number;
  idempotent?: boolean;
}

const query = (params: Record<string, string | boolean | undefined>) => {
  const q = Object.entries(params)
    .filter(([, v]) => v !== undefined && v !== "")
    .map(([k, v]) => `${k}=${encodeURIComponent(String(v))}`)
    .join("&");
  return q ? `?${q}` : "";
};

export function createHttpFacilitiesApi(fetchImpl: Fetch = (i, init) => fetch(i, init), baseUrl: string = import.meta.env.VITE_API_BASE ?? ""): FacilitiesApi {
  async function request<T>(method: string, path: string, opts: RequestOptions = {}): Promise<T> {
    const headers: Record<string, string> = { Accept: "application/json" };
    const unsafe = isUnsafeMethod(method);
    if (unsafe) {
      headers["Content-Type"] = "application/json";
      const csrf = getCsrfToken();
      if (csrf) headers["X-CSRF-Token"] = csrf;
    }
    if (opts.ifMatch !== undefined) headers["If-Match"] = `"${opts.ifMatch}"`;
    if (opts.idempotent) headers["Idempotency-Key"] = newIdempotencyKey();
    const res = await fetchImpl(`${baseUrl}${path}`, { method, headers, credentials: "include", body: unsafe ? JSON.stringify(opts.body ?? {}) : undefined });
    const text = await res.text();
    let data: unknown = undefined;
    if (text) {
      try {
        data = JSON.parse(text);
      } catch {
        data = undefined;
      }
    }
    if (!res.ok) {
      if (isProblem(data)) throw new ApiProblem(data);
      throw new ApiProblem({ type: "about:blank", title: `Request failed (${res.status})`, status: res.status, errorCode: `http-${res.status}` });
    }
    return data as T;
  }
  const items = async <T>(path: string) => (await request<Items<T>>("GET", path)).items;
  const id = (template: string, value: string) => fill(template, { id: value });
  const reason = (r?: string) => ({ reason: r || undefined });
  return {
    tree: (inst) => items(id(PATHS.tree, inst)),
    node: (nid) => request("GET", id(PATHS.node, nid)),
    createNode: (inst, input) => request("POST", id(PATHS.nodes, inst), { body: input, idempotent: true }),
    nodeCommand: (nid, v, cmd, r) => request("POST", id(NODE_CMD[cmd], nid), { body: reason(r), ifMatch: v }),
    setCapacity: (nid, v, capacity, r) => request("PUT", id(PATHS.capacity, nid), { body: { capacity, reason: r || undefined }, ifMatch: v }),
    calendar: (sid, from, to) => request("GET", `${id(PATHS.calendar, sid)}${query({ from, to })}`),
    reservations: (inst, status) => items(`${id(PATHS.reservations, inst)}${query({ status, limit: "200" })}`),
    book: (inst, input) => request("POST", id(PATHS.reservations, inst), { body: input, idempotent: true }),
    reservationCommand: (rid, v, cmd, r) => request("POST", id(cmd === "confirm" ? PATHS.confirm : PATHS.cancelReservation, rid), { body: reason(r), ifMatch: v }),
    tickets: (inst, f = {}) => request("GET", `${id(PATHS.tickets, inst)}${query({ status: f.status, breached: f.breached, open: f.open, limit: "200" })}`),
    createTicket: (inst, input) => request("POST", id(PATHS.tickets, inst), { body: input, idempotent: true }),
    ticketCommand: (tid, v, cmd, r, priority) =>
      request("POST", id(TICKET_CMD[cmd], tid), { body: cmd === "triage" ? { priority, reason: r || undefined } : reason(r), ifMatch: v }),
    assignTicket: (tid, v, assignee) => request("POST", id(PATHS.assign, tid), { body: { assignee }, ifMatch: v }),
    resolveTicket: (tid, v, resolution) => request("POST", id(PATHS.resolve, tid), { body: { resolution }, ifMatch: v }),
    slaCheck: (inst) => request("POST", id(PATHS.slaCheck, inst), { body: {}, idempotent: true }),
    workOrders: (inst, status) => items(`${id(PATHS.workOrders, inst)}${query({ status, limit: "200" })}`),
    raiseWorkOrder: (tid, assignee) => request("POST", id(PATHS.ticketWorkOrders, tid), { body: { assignee: assignee || undefined }, idempotent: true }),
    assignWorkOrder: (wid, v, assignee) => request("POST", id(PATHS.woAssign, wid), { body: { assignee }, ifMatch: v }),
    workOrderCommand: (wid, v, cmd, r) => request("POST", id(WO_CMD[cmd], wid), { body: reason(r), ifMatch: v }),
    completeWorkOrder: (wid, v, notes) => request("POST", id(PATHS.woComplete, wid), { body: { notes }, ifMatch: v }),
    verifyWorkOrder: (wid, v, accepted, notes) => request("POST", id(PATHS.woVerify, wid), { body: { accepted, notes: notes || undefined }, ifMatch: v }),
    plans: (inst) => items(id(PATHS.plans, inst)),
    plan: (pid) => request("GET", id(PATHS.plan, pid)),
    createPlan: (inst, input) => request("POST", id(PATHS.plans, inst), { body: input, idempotent: true }),
    planCommand: (pid, v, cmd, r) => request("POST", id(PLAN_CMD[cmd], pid), { body: reason(r), ifMatch: v }),
    generate: (pid, through) => request("POST", id(PATHS.generate, pid), { body: { through }, idempotent: true }),
    labs: (inst) => items(id(PATHS.labs, inst)),
    lab: (lid) => request("GET", id(PATHS.lab, lid)),
    createLab: (inst, nodeId, name) => request("POST", id(PATHS.labs, inst), { body: { nodeId, name: name || undefined }, idempotent: true }),
    readiness: (lid, v, checklist, notes) => request("POST", id(PATHS.readiness, lid), { body: { checklist, notes: notes || undefined }, ifMatch: v }),
    addEquipment: (lid, code, name) => request("POST", id(PATHS.equipment, lid), { body: { code, name }, idempotent: true }),
    equipmentStatus: (eid, v, status, r) => request("POST", id(PATHS.equipmentStatus, eid), { body: { status, reason: r || undefined }, ifMatch: v }),
    createSession: (lid, input) => request("POST", id(PATHS.sessions, lid), { body: input, idempotent: true }),
    sessionCommand: (sid, v, cmd, r) => request("POST", id(SESSION_CMD[cmd], sid), { body: reason(r), ifMatch: v }),
  };
}
