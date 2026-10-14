/**
 * In-memory stand-in of the slice-8A API (mock mode and tests): one site → building → floor
 * with a classroom and a lab, a confirmed booking, a few service tickets, a work order, a
 * preventive plan and a lab with equipment and a session. The same rules as the server where a
 * screen depends on them: legal transitions only (409 otherwise), booking overlap / capacity
 * conflicts, a session needs a ready lab, equipment that is not operational restricts the lab.
 */
import { ApiProblem } from "@sutra/api-client";
import type { FacilitiesApi } from "./api";
import type {
  Equipment,
  InfraNode,
  InfraNodeDetail,
  Lab,
  LabDetail,
  Plan,
  PlanDetail,
  Reservation,
  Session,
  Ticket,
  WorkOrder,
  WorkOrderDetail,
} from "./types";

const conflict = (detail: string) => new ApiProblem({ type: "about:blank", title: "Conflict", status: 409, errorCode: "conflict", detail });
const notFound = () => new ApiProblem({ type: "about:blank", title: "Not found", status: 404, errorCode: "not-found" });

const INST = "00000000-0000-7000-8000-0000000b8a01";
const CAMPUS = "00000000-0000-7000-8000-0000000b8a02";
let seq = 100;
const uid = () => `00000000-0000-7000-8000-${String(++seq).padStart(12, "0")}`;
const now = () => new Date().toISOString();
const day = (offset: number) => new Date(Date.now() + offset * 86_400_000).toISOString().slice(0, 10);

/** "09:30" → minutes. */
export function minutes(t: string): number {
  const [h = "0", m = "0"] = t.split(":");
  return Number(h) * 60 + Number(m);
}

/** Half-open overlap of two `HH:MM` windows. */
export function overlaps(a1: string, a2: string, b1: string, b2: string): boolean {
  return minutes(a1) < minutes(b2) && minutes(b1) < minutes(a2);
}

function node(code: string, name: string, nodeType: InfraNode["nodeType"], parentId: string | null, extra: Partial<InfraNode> = {}): InfraNode {
  return { id: uid(), institutionId: INST, campusId: CAMPUS, parentId, nodeType, code, name, attributes: {}, status: "active", version: 1, ...extra } as InfraNode;
}

export function createMockFacilitiesApi(): FacilitiesApi {
  const site = node("MAIN", "Main site", "site", null);
  const building = node("MAIN-B1", "Science block", "building", site.id);
  const floor = node("MAIN-B1-F1", "Ground floor", "floor", building.id, { floorNumber: 0 });
  const room = node("MAIN-B1-101", "Room 101", "space", floor.id, { spaceType: "classroom", capacity: 40 });
  const labSpace = node("MAIN-B1-LAB1", "Chemistry lab", "space", floor.id, { spaceType: "lab", capacity: 30 });
  const nodes: InfraNode[] = [site, building, floor, room, labSpace];
  const reservations: Reservation[] = [
    {
      id: uid(), number: "RSV-000001", institutionId: INST, nodeId: room.id, nodeCode: room.code, nodeName: room.name, purpose: "Guest lecture",
      attendees: 30, date: day(3), startTime: "09:00", endTime: "11:00", status: "confirmed", flagged: false, createdAt: now(), version: 2,
    } as Reservation,
  ];
  const tickets: Ticket[] = [
    {
      id: uid(), number: "SR-000001", institutionId: INST, category: "facility", source: "self_service", isComplaint: true, nodeId: room.id,
      requesterKind: "student", title: "Projector not working", priority: "high", status: "open", slaDueAt: new Date(Date.now() + 86_400_000).toISOString(),
      slaBreached: false, escalationLevel: 0, createdAt: now(), version: 1,
    } as Ticket,
    {
      id: uid(), number: "SR-000002", institutionId: INST, category: "maintenance", source: "admin", isComplaint: false, nodeId: building.id,
      title: "Leaking tap, 1st floor", priority: "medium", status: "in_progress", assignee: "tech.ravi", slaDueAt: new Date(Date.now() - 3_600_000).toISOString(),
      slaBreached: true, escalationLevel: 1, createdAt: now(), version: 4,
    } as Ticket,
  ];
  const workOrders: WorkOrder[] = [
    {
      id: uid(), number: "WO-000001", institutionId: INST, kind: "corrective", ticketId: tickets[1]!.id, ticketNumber: "SR-000002", nodeId: building.id,
      nodeCode: building.code, title: "Leaking tap, 1st floor", assignee: "tech.ravi", status: "in_progress", createdAt: now(), version: 3,
    } as WorkOrder,
  ];
  const plans: Plan[] = [
    {
      id: uid(), institutionId: INST, nodeId: building.id, nodeCode: building.code, title: "Fire extinguisher check", frequency: "monthly",
      startDate: day(-30), status: "active", approvalState: "approved", version: 3,
    } as Plan,
  ];
  const lab: Lab = {
    id: uid(), institutionId: INST, nodeId: labSpace.id, nodeCode: labSpace.code, name: "Chemistry lab", capacity: 30, status: "ready",
    checklist: [{ item: "Gas valves closed", done: true }, { item: "First-aid kit", done: true }], version: 2,
  } as Lab;
  const labs: Lab[] = [lab];
  const equipment: Equipment[] = [{ id: uid(), labId: lab.id, code: "FUME-01", name: "Fume hood", status: "operational", version: 1 } as Equipment];
  const sessions: Session[] = [
    { id: uid(), labId: lab.id, title: "Titration", courseCode: "CHE101", sessionDate: day(2), startTime: "14:00", endTime: "16:00", capacity: 30, allocated: 0, status: "planned", version: 1 } as Session,
  ];

  const find = <T extends { id: string }>(list: T[], id: string): T => {
    const x = list.find((i) => i.id === id);
    if (!x) throw notFound();
    return x;
  };
  const check = (current: number, expected: number) => {
    if (current !== expected) throw new ApiProblem({ type: "about:blank", title: "Precondition failed", status: 412, errorCode: "version-conflict" });
  };
  const move = <T extends { status: string; version: number }>(x: T, from: string[], to: string, what: string): T => {
    if (!from.includes(x.status)) throw conflict(`${what} is ${x.status}; it cannot become ${to}`);
    (x as { status: string }).status = to;
    x.version += 1;
    return x;
  };
  const detail = (n: InfraNode): InfraNodeDetail =>
    ({
      node: n,
      children: nodes.filter((c) => c.parentId === n.id),
      capabilities: [],
      futureReservations: reservations.filter((r) => r.nodeId === n.id && ["requested", "confirmed", "rescheduled"].includes(r.status)).length,
      version: n.version,
    }) as InfraNodeDetail;
  const woDetail = (w: WorkOrder): WorkOrderDetail => ({ workOrder: w, assignments: [], version: w.version }) as WorkOrderDetail;
  const planDetail = (p: Plan): PlanDetail => ({ plan: p, cycles: [], version: p.version }) as PlanDetail;
  const labDetail = (l: Lab): LabDetail =>
    ({ lab: l, equipment: equipment.filter((e) => e.labId === l.id), sessions: sessions.filter((s) => s.labId === l.id), version: l.version }) as LabDetail;
  const refreshLab = (l: Lab) => {
    const complete = l.checklist.length > 0 && l.checklist.every((i) => i.done);
    const bad = equipment.some((e) => e.labId === l.id && e.status !== "operational");
    const next = !complete ? "not_ready" : bad ? "restricted" : "ready";
    if (next !== l.status) {
      l.status = next;
      l.version += 1;
    }
  };

  const nodeMoves: Record<string, [string[], string]> = {
    activate: [["planned"], "active"],
    suspend: [["active"], "suspended"],
    restore: [["suspended"], "active"],
    decommission: [["active", "suspended"], "decommissioned"],
    archive: [["planned", "decommissioned"], "archived"],
  };
  const ticketMoves: Record<string, [string[], string]> = {
    triage: [["open"], "triaged"],
    start: [["assigned", "on_hold", "reopened"], "in_progress"],
    hold: [["assigned", "in_progress"], "on_hold"],
    close: [["resolved"], "closed"],
    cancel: [["open", "triaged", "assigned", "reopened"], "cancelled"],
    reopen: [["resolved"], "reopened"],
  };

  return {
    tree: async () => [...nodes].sort((a, b) => a.code.localeCompare(b.code)),
    node: async (id) => detail(find(nodes, id)),
    createNode: async (_inst, input) => {
      if (nodes.some((n) => n.code === input.code)) throw conflict(`infrastructure code ${input.code} is already used`);
      const parent = input.parentId ? find(nodes, input.parentId) : null;
      const n = node(input.code, input.name, input.nodeType, parent?.id ?? null, {
        spaceType: input.spaceType ?? undefined,
        capacity: input.capacity ?? undefined,
        status: input.status ?? "planned",
      });
      nodes.push(n);
      return detail(n);
    },
    nodeCommand: async (id, v, cmd) => {
      const n = find(nodes, id);
      check(n.version, v);
      if (cmd === "decommission" && detail(n).futureReservations > 0) throw conflict(`infrastructure node ${n.code} has reservations from today on`);
      const [from, to] = nodeMoves[cmd]!;
      return detail(move(n, from, to, `infrastructure node ${n.code}`));
    },
    setCapacity: async (id, v, capacity) => {
      const n = find(nodes, id);
      check(n.version, v);
      n.capacity = capacity;
      n.version += 1;
      return detail(n);
    },
    calendar: async (id, from) => {
      const n = find(nodes, id);
      const busy = reservations.filter((r) => r.nodeId === id && r.date === from && ["requested", "confirmed", "rescheduled"].includes(r.status));
      return {
        nodeId: n.id, code: n.code, status: n.status, capacity: n.capacity,
        days: [{ date: from, open: [{ startTime: "00:00", endTime: "23:59" }], busy: busy.map((r) => ({ reservationId: r.id, number: r.number, startTime: r.startTime, endTime: r.endTime, status: r.status, purpose: r.purpose })), free: [] }],
      } as Awaited<ReturnType<FacilitiesApi["calendar"]>>;
    },
    reservations: async (_inst, status) => reservations.filter((r) => !status || r.status === status),
    book: async (_inst, input) => {
      const n = find(nodes, input.nodeId);
      if (n.status !== "active") throw conflict(`space ${n.code} is ${n.status}; it takes no bookings`);
      if (n.capacity != null && input.attendees > n.capacity) throw conflict(`space ${n.code} seats ${n.capacity}`);
      const clash = reservations.find(
        (r) => r.nodeId === n.id && r.date === input.date && ["requested", "confirmed", "rescheduled"].includes(r.status) && overlaps(input.startTime, input.endTime, r.startTime, r.endTime),
      );
      if (clash) throw conflict(`space ${n.code} is already booked ${clash.startTime}–${clash.endTime} (${clash.number})`);
      const r = {
        id: uid(), number: `RSV-${String(reservations.length + 1).padStart(6, "0")}`, institutionId: INST, nodeId: n.id, nodeCode: n.code, nodeName: n.name,
        purpose: input.purpose ?? "Booking", attendees: input.attendees, date: input.date, startTime: input.startTime, endTime: input.endTime,
        status: "confirmed", flagged: false, createdAt: now(), version: 2,
      } as Reservation;
      reservations.push(r);
      return r;
    },
    reservationCommand: async (id, v, cmd) => {
      const r = find(reservations, id);
      check(r.version, v);
      return cmd === "confirm" ? move(r, ["requested"], "confirmed", `reservation ${r.number}`) : move(r, ["requested", "confirmed", "rescheduled"], "cancelled", `reservation ${r.number}`);
    },
    tickets: async (_inst, f = {}) => ({
      items: tickets.filter((t) => (!f.status || t.status === f.status) && (f.breached === undefined || t.slaBreached === f.breached) && (!f.open || !["closed", "cancelled", "merged"].includes(t.status))),
    }),
    createTicket: async (_inst, input) => {
      const t = {
        id: uid(), number: `SR-${String(tickets.length + 1).padStart(6, "0")}`, institutionId: INST, category: input.category, source: "admin",
        isComplaint: input.isComplaint ?? false, nodeId: input.nodeId ?? null, title: input.title, priority: input.priority ?? "medium", status: "open",
        slaDueAt: new Date(Date.now() + 72 * 3_600_000).toISOString(), slaBreached: false, escalationLevel: 0, createdAt: now(), version: 1,
      } as Ticket;
      tickets.push(t);
      return t;
    },
    ticketCommand: async (id, v, cmd, _reason, priority) => {
      const t = find(tickets, id);
      check(t.version, v);
      if (cmd === "escalate") {
        t.escalationLevel += 1;
        t.version += 1;
        return t;
      }
      if (cmd === "triage" && priority) t.priority = priority;
      const [from, to] = ticketMoves[cmd]!;
      return move(t, from, to, `service ticket ${t.number}`);
    },
    assignTicket: async (id, v, assignee) => {
      const t = find(tickets, id);
      check(t.version, v);
      t.assignee = assignee;
      if (["assigned", "in_progress"].includes(t.status)) {
        t.version += 1;
        return t;
      }
      return move(t, ["open", "triaged", "on_hold", "reopened"], "assigned", `service ticket ${t.number}`);
    },
    resolveTicket: async (id, v, resolution) => {
      const t = find(tickets, id);
      check(t.version, v);
      t.resolution = resolution;
      return move(t, ["assigned", "in_progress"], "resolved", `service ticket ${t.number}`);
    },
    slaCheck: async () => {
      let processed = 0;
      for (const t of tickets) {
        if (!t.slaBreached && ["open", "triaged", "assigned", "in_progress", "reopened"].includes(t.status) && Date.parse(t.slaDueAt) < Date.now()) {
          t.slaBreached = true;
          t.escalationLevel += 1;
          t.version += 1;
          processed += 1;
        }
      }
      return { processed, skipped: 0, failed: [] };
    },
    workOrders: async (_inst, status) => workOrders.filter((w) => !status || w.status === status),
    raiseWorkOrder: async (ticketId, assignee) => {
      const t = find(tickets, ticketId);
      if (["resolved", "closed", "cancelled", "merged"].includes(t.status)) throw conflict(`service ticket ${t.number} is ${t.status}`);
      const w = {
        id: uid(), number: `WO-${String(workOrders.length + 1).padStart(6, "0")}`, institutionId: INST, kind: "corrective", ticketId: t.id, ticketNumber: t.number,
        nodeId: t.nodeId ?? null, title: t.title, assignee: assignee ?? null, status: assignee ? "assigned" : "created", createdAt: now(), version: 1,
      } as WorkOrder;
      workOrders.push(w);
      return woDetail(w);
    },
    assignWorkOrder: async (id, v, assignee) => {
      const w = find(workOrders, id);
      check(w.version, v);
      w.assignee = assignee;
      if (["assigned", "in_progress"].includes(w.status)) {
        w.version += 1;
        return woDetail(w);
      }
      return woDetail(move(w, ["created", "on_hold", "reopened"], "assigned", `work order ${w.number}`));
    },
    workOrderCommand: async (id, v, cmd) => {
      const w = find(workOrders, id);
      check(w.version, v);
      const moves: Record<string, [string[], string]> = {
        start: [["assigned", "on_hold", "reopened"], "in_progress"],
        reopen: [["completed", "verified"], "reopened"],
        cancel: [["created", "assigned", "on_hold", "reopened"], "cancelled"],
      };
      const [from, to] = moves[cmd]!;
      return woDetail(move(w, from, to, `work order ${w.number}`));
    },
    completeWorkOrder: async (id, v, notes) => {
      const w = find(workOrders, id);
      check(w.version, v);
      w.completionNotes = notes;
      return woDetail(move(w, ["in_progress"], "completed", `work order ${w.number}`));
    },
    verifyWorkOrder: async (id, v, accepted) => {
      const w = find(workOrders, id);
      check(w.version, v);
      move(w, ["completed"], accepted ? "verified" : "reopened", `work order ${w.number}`);
      if (accepted && w.ticketId) {
        const t = tickets.find((x) => x.id === w.ticketId);
        if (t && ["assigned", "in_progress"].includes(t.status)) {
          t.status = "resolved";
          t.version += 1;
        }
      }
      return woDetail(w);
    },
    plans: async () => plans,
    plan: async (id) => planDetail(find(plans, id)),
    createPlan: async (_inst, input) => {
      const n = find(nodes, input.nodeId);
      const p = { id: uid(), institutionId: INST, nodeId: n.id, nodeCode: n.code, title: input.title, frequency: input.frequency, startDate: input.startDate, status: "draft", approvalState: "not_requested", version: 1 } as Plan;
      plans.push(p);
      return planDetail(p);
    },
    planCommand: async (id, v, cmd) => {
      const p = find(plans, id);
      check(p.version, v);
      if (cmd === "submit") {
        if (p.status !== "draft") throw conflict(`maintenance plan ${p.title} is ${p.status}`);
        p.approvalState = "approved";
        return planDetail(move(p, ["draft"], "active", `maintenance plan ${p.title}`));
      }
      const moves: Record<string, [string[], string]> = {
        activate: [["suspended"], "active"],
        suspend: [["active"], "suspended"],
        retire: [["draft", "active", "suspended"], "retired"],
      };
      const [from, to] = moves[cmd]!;
      return planDetail(move(p, from, to, `maintenance plan ${p.title}`));
    },
    generate: async (id) => {
      const p = find(plans, id);
      if (p.status !== "active") throw conflict(`maintenance plan ${p.title} is ${p.status}; only an active plan generates work`);
      return { processed: 1, skipped: 0, failed: [] };
    },
    labs: async () => labs,
    lab: async (id) => labDetail(find(labs, id)),
    createLab: async (_inst, nodeId, name) => {
      const n = find(nodes, nodeId);
      if (n.spaceType !== "lab") throw new ApiProblem({ type: "about:blank", title: "Validation failed", status: 422, errorCode: "validation-failed", detail: "a lab profile belongs to a `lab` space" });
      if (labs.some((l) => l.nodeId === nodeId)) throw conflict(`space ${n.code} already has a lab profile`);
      const l = { id: uid(), institutionId: INST, nodeId, nodeCode: n.code, name: name ?? n.name, capacity: n.capacity ?? 1, status: "not_ready", checklist: [], version: 1 } as Lab;
      labs.push(l);
      return labDetail(l);
    },
    readiness: async (id, v, checklist) => {
      const l = find(labs, id);
      check(l.version, v);
      l.checklist = checklist;
      l.version += 1;
      refreshLab(l);
      return labDetail(l);
    },
    addEquipment: async (labId, code, name) => {
      const l = find(labs, labId);
      const e = { id: uid(), labId: l.id, code: code.toUpperCase(), name, status: "operational", version: 1 } as Equipment;
      equipment.push(e);
      return e;
    },
    equipmentStatus: async (id, v, status) => {
      const e = find(equipment, id);
      check(e.version, v);
      e.status = status;
      e.version += 1;
      const l = labs.find((x) => x.id === e.labId);
      if (l && l.status !== "not_ready") refreshLab(l);
      return e;
    },
    createSession: async (labId, input) => {
      const l = find(labs, labId);
      const clash = sessions.find(
        (s) => s.labId === l.id && s.sessionDate === input.sessionDate && ["planned", "ready", "in_progress"].includes(s.status) && overlaps(input.startTime, input.endTime, s.startTime, s.endTime),
      );
      if (clash) throw conflict(`the lab already hosts "${clash.title}"`);
      const s = {
        id: uid(), labId: l.id, title: input.title, courseCode: input.courseCode ?? null, sessionDate: input.sessionDate, startTime: input.startTime, endTime: input.endTime,
        capacity: input.capacity ?? l.capacity, allocated: 0, status: "planned", version: 1,
      } as Session;
      sessions.push(s);
      return s;
    },
    sessionCommand: async (id, v, cmd) => {
      const s = find(sessions, id);
      check(s.version, v);
      const l = find(labs, s.labId);
      if ((cmd === "ready" || cmd === "start") && l.status !== "ready") throw conflict(`lab ${l.name} is ${l.status}; a session needs a ready lab`);
      const moves: Record<string, [string[], string]> = {
        ready: [["planned", "postponed"], "ready"],
        start: [["ready"], "in_progress"],
        complete: [["in_progress"], "completed"],
        cancel: [["planned", "ready", "postponed"], "cancelled"],
      };
      const [from, to] = moves[cmd]!;
      return move(s, from, to, `practical session "${s.title}"`);
    },
  };
}
