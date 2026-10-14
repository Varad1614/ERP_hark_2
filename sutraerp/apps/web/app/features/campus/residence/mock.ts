/**
 * In-memory slice-8B API for the mock build and component tests: a small hostel (one active
 * policy, three beds, one resident, one waiting application) and a transport network (one active
 * route, two stops, two vehicles, one passenger with a pass, one planned trip). Rules are a
 * simplified copy of the API's (state machines, capacity, one live pass); money stays strings.
 */
import { ApiProblem } from "@sutra/api-client";
import type { ResidenceApi } from "./api";
import type {
  Complaint,
  DriverAssignment,
  HostelAllocation,
  HostelApplication,
  HostelBed,
  HostelPolicy,
  HostelRoom,
  Mess,
  MessAllocation,
  ResidentLeave,
  RoomInspection,
  RouteVersion,
  TransportAllocation,
  TransportPass,
  TransportRoute,
  TransportStop,
  Trip,
  Vehicle,
} from "./types";

const conflict = (detail: string) => new ApiProblem({ type: "about:blank", title: "Conflict", status: 409, errorCode: "conflict", detail });
const missing = () => new ApiProblem({ type: "about:blank", title: "Not found", status: 404, errorCode: "not-found" });

let seq = 0;
const nid = (p: string) => `${p}-${(++seq).toString(16).padStart(8, "0")}-mock`;
const today = () => new Date().toISOString().slice(0, 10);

function find<T extends { id: string }>(rows: T[], id: string): T {
  const r = rows.find((x) => x.id === id);
  if (!r) throw missing();
  return r;
}

function check(row: { version: number }, version: number) {
  if (row.version !== version) throw new ApiProblem({ type: "about:blank", title: "Version conflict", status: 412, errorCode: "precondition-failed" });
}

function move<T extends { status: string; version: number }>(row: T, from: string[], to: string): T {
  if (!from.includes(row.status)) throw conflict(`${row.status} cannot become ${to}`);
  row.status = to;
  row.version += 1;
  return { ...row };
}

export function createMockResidenceApi(): ResidenceApi {
  const inst = "mock-institution";
  const rooms: HostelRoom[] = [
    { id: "room-101", code: "HST-B1-101", name: "Room 101", capacity: 2, status: "active" },
    { id: "room-102", code: "HST-B1-102", name: "Room 102", capacity: 1, status: "active" },
  ];
  const policies: HostelPolicy[] = [
    { id: "pol-1", institutionId: inst, name: "Hostel policy 2026", feePerTerm: "45000.00", deposit: "5000.00", curfewTime: "21:30", effectiveFrom: "2026-06-01", status: "active", version: 3 },
  ];
  const beds: HostelBed[] = [
    { id: "bed-1", institutionId: inst, roomNodeId: "room-101", roomCode: "HST-B1-101", roomName: "Room 101", bedCode: "A", status: "occupied", allocationId: "alloc-1", occupantNumber: "STU-000001", occupantName: "Aarav Desai", version: 2 },
    { id: "bed-2", institutionId: inst, roomNodeId: "room-101", roomCode: "HST-B1-101", roomName: "Room 101", bedCode: "B", status: "available", version: 1 },
    { id: "bed-3", institutionId: inst, roomNodeId: "room-102", roomCode: "HST-B1-102", roomName: "Room 102", bedCode: "A", status: "available", version: 1 },
  ];
  const applications: HostelApplication[] = [
    { id: "app-1", institutionId: inst, number: "HAP-000001", personKind: "student", personId: "stu-1", personNumber: "STU-000001", personName: "Aarav Desai", policyId: "pol-1", channel: "self_service", status: "allocated", allocationId: "alloc-1", createdAt: "2026-06-02T10:00:00Z", version: 4 },
    { id: "app-2", institutionId: inst, number: "HAP-000002", personKind: "student", personId: "stu-2", personNumber: "STU-000002", personName: "Diya Rao", policyId: "pol-1", channel: "office", status: "under_review", processInstanceId: "noop-1", createdAt: "2026-06-03T10:00:00Z", version: 2 },
  ];
  const allocations: HostelAllocation[] = [
    { id: "alloc-1", institutionId: inst, applicationId: "app-1", personKind: "student", personId: "stu-1", personNumber: "STU-000001", personName: "Aarav Desai", policyId: "pol-1", bedId: "bed-1", bedCode: "A", roomNodeId: "room-101", roomCode: "HST-B1-101", startDate: "2026-06-05", status: "active", feeAmount: "45000.00", chargeId: "chg-1", chargeNumber: "CHG-000001", createdAt: "2026-06-05T10:00:00Z", version: 1 },
  ];
  const leaves: ResidentLeave[] = [];
  const inspections: RoomInspection[] = [];
  const messes: Mess[] = [{ id: "mess-1", institutionId: inst, code: "MAIN", name: "Main mess", menuType: "mixed", capacity: 120, assigned: 0, status: "active", version: 1 }];
  const messAllocations: MessAllocation[] = [];
  const stops: TransportStop[] = [
    { id: "stop-1", institutionId: inst, code: "ST-GATE", name: "Main gate", status: "active", passengers: 1, version: 1 },
    { id: "stop-2", institutionId: inst, code: "ST-MKT", name: "Market", status: "active", passengers: 0, version: 1 },
  ];
  const routes: TransportRoute[] = [
    { id: "route-1", institutionId: inst, code: "R-01", name: "City loop", status: "active", currentVersionId: "rv-1", currentVersionNo: 1, fare: "6000.00", capacity: 40, effectiveFrom: "2026-06-01", passengers: 1, hasDraft: false, version: 3 },
  ];
  const versions: RouteVersion[] = [
    {
      id: "rv-1", routeId: "route-1", versionNo: 1, effectiveFrom: "2026-06-01", capacity: 40, fare: "6000.00", status: "active", version: 2,
      stops: [
        { stopId: "stop-1", code: "ST-GATE", name: "Main gate", sequence: 1, pickupTime: "07:30", stopStatus: "active" },
        { stopId: "stop-2", code: "ST-MKT", name: "Market", sequence: 2, pickupTime: "07:45", stopStatus: "active" },
      ],
    },
  ];
  const vehicles: Vehicle[] = [
    { id: "veh-1", institutionId: inst, registrationNo: "KA01AB1234", label: "Bus 1", capacity: 40, status: "available", openTrips: 1, version: 1 },
    { id: "veh-2", institutionId: inst, registrationNo: "KA01AB5678", label: "Bus 2", capacity: 40, status: "under_maintenance", openTrips: 0, version: 1 },
  ];
  const drivers: DriverAssignment[] = [
    { id: "drv-1", routeId: "route-1", routeCode: "R-01", staffId: "staff-1", staffNumber: "EMP-000007", driverName: "Ravi Kumar", licenceNo: "DL-042011", validFrom: "2026-06-01", status: "active", version: 1 },
  ];
  const passengers: TransportAllocation[] = [
    { id: "ta-1", institutionId: inst, personKind: "student", personId: "stu-1", personNumber: "STU-000001", personName: "Aarav Desai", routeId: "route-1", routeCode: "R-01", stopId: "stop-1", stopCode: "ST-GATE", stopName: "Main gate", channel: "self_service", startDate: "2026-06-05", status: "active", flagged: false, passId: "pass-1", passNumber: "TP-000001", version: 2 },
  ];
  const passes: TransportPass[] = [
    { id: "pass-1", institutionId: inst, number: "TP-000001", allocationId: "ta-1", personKind: "student", personNumber: "STU-000001", personName: "Aarav Desai", routeId: "route-1", routeCode: "R-01", validFrom: "2026-06-05", validTo: "2026-12-31", kind: "new", status: "active", feeAmount: "6000.00", chargeNumber: "CHG-000002", version: 1 },
  ];
  const trips: Trip[] = [
    { id: "trip-1", institutionId: inst, routeId: "route-1", routeCode: "R-01", tripDate: today(), direction: "pickup", status: "planned", vehicleId: "veh-1", vehicleRegistration: "KA01AB1234", needsVehicle: false, allocatedPassengers: 1, version: 1 },
  ];
  let number = 10;
  const next = (p: string) => `${p}-${String(++number).padStart(6, "0")}`;
  const str = (v: unknown) => (typeof v === "string" ? v : "");
  const num = (v: unknown, d = 0) => (typeof v === "number" ? v : Number(v ?? d) || d);

  return {
    policies: async () => policies.map((p) => ({ ...p })),
    createPolicy: async (_inst, b) => {
      const p: HostelPolicy = { id: nid("pol"), institutionId: inst, name: str(b.name), feePerTerm: str(b.feePerTerm) || "0.00", deposit: str(b.deposit) || "0.00", effectiveFrom: str(b.effectiveFrom), status: "draft", version: 1 };
      policies.unshift(p);
      return { ...p };
    },
    policyCommand: async (id, v, cmd) => {
      const p = find(policies, id);
      check(p, v);
      if (cmd === "submit") return move(p, ["draft"], "approved");
      policies.filter((x) => x.status === "active").forEach((x) => move(x, ["active"], "superseded"));
      return move(p, ["approved"], "active");
    },
    decidePolicy: async (id, v, d) => {
      const p = find(policies, id);
      check(p, v);
      return move(p, ["draft"], d === "approve" ? "approved" : "draft");
    },
    rooms: async () => rooms.map((r) => ({ ...r })),
    beds: async () => beds.map((b) => ({ ...b })),
    addBed: async (roomId, bedCode) => {
      const room = find(rooms, roomId);
      if (beds.filter((b) => b.roomNodeId === roomId).length >= (room.capacity ?? 0)) throw conflict("the room is at capacity");
      const b: HostelBed = { id: nid("bed"), institutionId: inst, roomNodeId: roomId, roomCode: room.code, roomName: room.name, bedCode: bedCode.toUpperCase(), status: "available", version: 1 };
      beds.push(b);
      return { ...b };
    },
    applications: async (_i, status) => applications.filter((a) => !status || a.status === status).map((a) => ({ ...a })),
    apply: async (_i, b) => {
      const a: HostelApplication = { id: nid("app"), institutionId: inst, number: next("HAP"), personKind: str(b.personKind), personId: str(b.personId), personNumber: str(b.personId).slice(0, 10), personName: "Applicant", policyId: "pol-1", channel: "office", status: "approved", processInstanceId: "noop", createdAt: new Date().toISOString(), version: 3 };
      applications.unshift(a);
      return { ...a };
    },
    decideApplication: async (id, v, d) => {
      const a = find(applications, id);
      check(a, v);
      return move(a, ["under_review"], d === "approve" ? "approved" : "rejected");
    },
    allocate: async (id, v, bedId) => {
      const a = find(applications, id);
      check(a, v);
      const bed = bedId ? find(beds, bedId) : beds.find((b) => b.status === "available");
      if (!bed) return move(a, ["approved"], "waitlisted");
      if (bed.status !== "available") throw conflict("the bed is not available");
      const al: HostelAllocation = { id: nid("alloc"), institutionId: inst, applicationId: a.id, personKind: a.personKind, personId: a.personId, personNumber: a.personNumber, personName: a.personName, policyId: a.policyId, bedId: bed.id, bedCode: bed.bedCode, roomNodeId: bed.roomNodeId, roomCode: bed.roomCode, startDate: today(), status: "active", feeAmount: "45000.00", chargeNumber: a.personKind === "student" ? next("CHG") : null, createdAt: new Date().toISOString(), version: 1 };
      allocations.push(al);
      Object.assign(bed, { status: "occupied", allocationId: al.id, occupantNumber: a.personNumber, occupantName: a.personName });
      a.allocationId = al.id;
      return move(a, ["approved", "waitlisted"], "allocated");
    },
    withdrawApplication: async (id, v) => {
      const a = find(applications, id);
      check(a, v);
      return move(a, ["submitted", "under_review", "approved", "waitlisted"], "withdrawn");
    },
    allocations: async (_i, status) => allocations.filter((a) => !status || a.status === status).map((a) => ({ ...a })),
    allocationCommand: async (id, v, cmd) => {
      const a = find(allocations, id);
      check(a, v);
      if (cmd === "vacate") return move(a, ["active"], "vacating");
      const bed = find(beds, a.bedId);
      Object.assign(bed, { status: "available", allocationId: null, occupantNumber: null, occupantName: null });
      return move(a, ["vacating"], "closed");
    },
    requestLeave: async (allocationId, b) => {
      const a = find(allocations, allocationId);
      const l: ResidentLeave = { id: nid("leave"), institutionId: inst, allocationId, personNumber: a.personNumber, personName: a.personName, fromDate: str(b.fromDate), toDate: str(b.toDate), destination: str(b.destination) || null, status: "approved", returnedLate: false, version: 2 };
      leaves.unshift(l);
      return { ...l };
    },
    leaves: async (_i, status) => leaves.filter((l) => !status || l.status === status).map((l) => ({ ...l })),
    decideLeave: async (id, v, d) => {
      const l = find(leaves, id);
      check(l, v);
      return move(l, ["requested"], d === "approve" ? "approved" : "rejected");
    },
    leaveCommand: async (id, v, cmd) => {
      const l = find(leaves, id);
      check(l, v);
      if (cmd === "depart") return move(l, ["approved"], "out");
      l.returnedLate = today() > l.toDate;
      return move(l, ["out", "overdue"], "returned");
    },
    overdueCheck: async () => {
      let processed = 0;
      leaves.filter((l) => l.status === "out" && l.toDate < today()).forEach((l) => {
        move(l, ["out"], "overdue");
        processed += 1;
      });
      return { processed, skipped: 0, failed: [] };
    },
    transfer: async (allocationId, toBedId) => {
      const a = find(allocations, allocationId);
      const to = find(beds, toBedId);
      if (to.status !== "available") throw conflict("the target bed is not available");
      const from = find(beds, a.bedId);
      Object.assign(from, { status: "available", allocationId: null, occupantNumber: null, occupantName: null });
      Object.assign(to, { status: "occupied", allocationId: a.id, occupantNumber: a.personNumber, occupantName: a.personName });
      Object.assign(a, { bedId: to.id, bedCode: to.bedCode, roomCode: to.roomCode, roomNodeId: to.roomNodeId, version: a.version + 2 });
      return { id: nid("tr"), allocationId, personNumber: a.personNumber, fromBedCode: from.bedCode, fromRoomCode: from.roomCode, toBedCode: to.bedCode, toRoomCode: to.roomCode, status: "completed", version: 3 };
    },
    inspections: async (_i, status) => inspections.filter((x) => !status || x.status === status).map((x) => ({ ...x })),
    scheduleInspection: async (roomId, b) => {
      const room = find(rooms, roomId);
      const x: RoomInspection = { id: nid("insp"), institutionId: inst, roomNodeId: roomId, roomCode: room.code, allocationId: str(b.allocationId) || null, scheduledOn: str(b.scheduledOn), status: "scheduled", version: 1 };
      inspections.unshift(x);
      return { ...x };
    },
    completeInspection: async (id, v, b) => {
      const x = find(inspections, id);
      check(x, v);
      const damage = str(b.damageAmount);
      x.findings = str(b.findings) || null;
      if (damage && Number(damage) > 0) {
        x.damageAmount = damage;
        if (x.allocationId) {
          x.chargeNumber = next("CHG");
          move(x, ["scheduled"], "damage_found");
          return move(x, ["damage_found"], "charged");
        }
        return move(x, ["scheduled"], "damage_found");
      }
      return move(x, ["scheduled"], "completed");
    },
    closeInspection: async (id, v) => {
      const x = find(inspections, id);
      check(x, v);
      return move(x, ["completed", "damage_found", "charged"], "closed");
    },
    messes: async () => messes.map((m) => ({ ...m, assigned: messAllocations.filter((a) => a.messId === m.id && a.status === "active").length })),
    createMess: async (_i, b) => {
      const m: Mess = { id: nid("mess"), institutionId: inst, code: str(b.code).toUpperCase(), name: str(b.name), menuType: str(b.menuType) || "mixed", capacity: num(b.capacity, 1), assigned: 0, status: "active", version: 1 };
      messes.push(m);
      return { ...m };
    },
    messAllocations: async (messId) => messAllocations.filter((a) => a.messId === messId).map((a) => ({ ...a })),
    assignMess: async (messId, b) => {
      const m = find(messes, messId);
      const res = find(allocations, str(b.hostelAllocationId));
      if (messAllocations.some((a) => a.hostelAllocationId === res.id && a.status === "active")) throw conflict("one active mess per residency");
      if (messAllocations.filter((a) => a.messId === messId && a.status === "active").length >= m.capacity) throw conflict("the mess is full");
      const a: MessAllocation = { id: nid("ma"), messId, messCode: m.code, hostelAllocationId: res.id, personNumber: res.personNumber, personName: res.personName, startDate: str(b.startDate) || today(), status: "active", version: 1 };
      messAllocations.push(a);
      return { ...a };
    },
    changeMess: async (id, v, b) => {
      const old = find(messAllocations, id);
      check(old, v);
      const m = find(messes, str(b.messId));
      move(old, ["active"], "changed");
      const a: MessAllocation = { ...old, id: nid("ma"), messId: m.id, messCode: m.code, startDate: str(b.effectiveFrom) || today(), status: "active", previousId: old.id, version: 1 };
      messAllocations.push(a);
      return { ...a };
    },
    hostelComplaint: async (allocationId, b) => {
      const c: Complaint = { id: nid("sr"), number: next("SR"), category: "hostel", source: "hostel", sourceRefId: allocationId, title: str(b.title), priority: str(b.priority) || "medium", status: "open", slaDueAt: new Date(Date.now() + 72 * 3600_000).toISOString(), version: 1 };
      return c;
    },
    routes: async () => routes.map((r) => ({ ...r, hasDraft: versions.some((v) => v.routeId === r.id && v.status === "draft") })),
    createRoute: async (_i, b) => {
      const r: TransportRoute = { id: nid("route"), institutionId: inst, code: str(b.code).toUpperCase(), name: str(b.name), status: "draft", passengers: 0, hasDraft: true, version: 1 };
      routes.push(r);
      versions.push({ id: nid("rv"), routeId: r.id, versionNo: 1, effectiveFrom: str(b.effectiveFrom), capacity: num(b.capacity, 40), fare: str(b.fare) || "0.00", status: "draft", version: 1, stops: [] });
      return { ...r };
    },
    versions: async (routeId) => versions.filter((v) => v.routeId === routeId).sort((a, b) => b.versionNo - a.versionNo).map((v) => ({ ...v, stops: [...v.stops] })),
    createVersion: async (routeId, b) => {
      if (versions.some((v) => v.routeId === routeId && v.status === "draft")) throw conflict("the route already has a draft version");
      const cur = versions.filter((v) => v.routeId === routeId).sort((a, c) => c.versionNo - a.versionNo)[0];
      const v: RouteVersion = { id: nid("rv"), routeId, versionNo: (cur?.versionNo ?? 0) + 1, effectiveFrom: str(b.effectiveFrom), capacity: num(b.capacity, cur?.capacity ?? 40), fare: str(b.fare) || cur?.fare || "0.00", status: "draft", version: 1, stops: cur ? [...cur.stops] : [] };
      versions.push(v);
      return { ...v };
    },
    setStops: async (versionId, ver, list) => {
      const v = find(versions, versionId);
      check(v, ver);
      if (v.status !== "draft") throw conflict("only a draft version's stops can change");
      v.stops = list.map((s) => {
        const st = find(stops, s.stopId);
        return { stopId: st.id, code: st.code, name: st.name, sequence: s.sequence, pickupTime: s.pickupTime ?? null, stopStatus: st.status };
      });
      v.version += 1;
      return { ...v };
    },
    routeCommand: async (id, ver, cmd) => {
      const r = find(routes, id);
      check(r, ver);
      if (cmd === "suspend") return move(r, ["active"], "suspended");
      if (cmd === "retire") return move(r, ["draft", "active", "suspended"], "retired");
      const draft = versions.find((v) => v.routeId === id && v.status === "draft");
      if (draft) {
        if (draft.stops.length < 2) throw conflict("a route version needs at least two stops before activation");
        versions.filter((v) => v.routeId === id && v.status === "active").forEach((v) => move(v, ["active"], "superseded"));
        move(draft, ["draft"], "active");
        Object.assign(r, { currentVersionId: draft.id, currentVersionNo: draft.versionNo, fare: draft.fare, capacity: draft.capacity, effectiveFrom: draft.effectiveFrom });
      }
      r.status = "active";
      r.version += 1;
      return { ...r };
    },
    decideRoute: async (id, ver) => {
      const r = find(routes, id);
      check(r, ver);
      r.processInstanceId = null;
      r.version += 1;
      return { ...r };
    },
    stops: async () => stops.map((s) => ({ ...s })),
    createStop: async (_i, b) => {
      const s: TransportStop = { id: nid("stop"), institutionId: inst, code: str(b.code).toUpperCase(), name: str(b.name), landmark: str(b.landmark) || null, status: "active", passengers: 0, version: 1 };
      stops.push(s);
      return { ...s };
    },
    stopCommand: async (id, v, cmd) => {
      const s = find(stops, id);
      check(s, v);
      return cmd === "suspend" ? move(s, ["active"], "suspended") : move(s, ["suspended"], "active");
    },
    vehicles: async () => vehicles.map((v) => ({ ...v })),
    createVehicle: async (_i, b) => {
      const v: Vehicle = { id: nid("veh"), institutionId: inst, registrationNo: str(b.registrationNo).toUpperCase(), label: str(b.label), capacity: num(b.capacity, 40), status: "available", openTrips: 0, version: 1 };
      vehicles.push(v);
      return { ...v };
    },
    vehicleStatus: async (id, ver, status) => {
      const v = find(vehicles, id);
      check(v, ver);
      return move(v, ["available", "under_maintenance", "broken_down"], status);
    },
    breakdown: async (id, ver) => {
      const v = find(vehicles, id);
      check(v, ver);
      trips.filter((t) => t.vehicleId === id && ["planned", "ready", "in_progress"].includes(t.status)).forEach((t) => {
        t.needsVehicle = true;
      });
      return move(v, ["available", "assigned"], "broken_down");
    },
    drivers: async (routeId) => drivers.filter((d) => d.routeId === routeId).map((d) => ({ ...d })),
    assignDriver: async (routeId, b) => {
      const r = find(routes, routeId);
      const d: DriverAssignment = { id: nid("drv"), routeId, routeCode: r.code, staffId: str(b.staffId), staffNumber: "EMP-NEW", driverName: "New driver", licenceNo: str(b.licenceNo), validFrom: str(b.validFrom), status: "active", version: 1 };
      drivers.push(d);
      return { ...d };
    },
    passengers: async (_i, status) => passengers.filter((p) => !status || p.status === status).map((p) => ({ ...p })),
    allocatePassenger: async (_i, b) => {
      const r = find(routes, str(b.routeId));
      const s = find(stops, str(b.stopId));
      const used = passengers.filter((p) => p.routeId === r.id && p.status === "active").length;
      const p: TransportAllocation = { id: nid("ta"), institutionId: inst, personKind: str(b.personKind), personId: str(b.personId), personNumber: str(b.personId).slice(0, 10), personName: "Passenger", routeId: r.id, routeCode: r.code, stopId: s.id, stopCode: s.code, stopName: s.name, channel: "office", startDate: str(b.startDate) || today(), status: used < (r.capacity ?? 0) ? "active" : "requested", flagged: false, version: 1 };
      passengers.push(p);
      return { ...p };
    },
    passengerCommand: async (id, v, cmd) => {
      const p = find(passengers, id);
      check(p, v);
      if (cmd === "activate") return move(p, ["requested"], "active");
      passes.filter((x) => x.allocationId === id && ["issued", "active", "suspended"].includes(x.status)).forEach((x) => move(x, [x.status], "cancelled"));
      p.passId = null;
      p.passNumber = null;
      return move(p, ["requested", "active"], "withdrawn");
    },
    changePassenger: async (id, v, b) => {
      const p = find(passengers, id);
      check(p, v);
      const s = find(stops, str(b.stopId));
      move(p, ["active"], "changed");
      const n: TransportAllocation = { ...p, id: nid("ta"), stopId: s.id, stopCode: s.code, stopName: s.name, status: "active", previousId: p.id, startDate: str(b.effectiveFrom) || today(), version: 1 };
      passengers.push(n);
      return { ...n };
    },
    issuePass: async (allocationId, b) => {
      const p = find(passengers, allocationId);
      if (p.passId) throw conflict("the allocation already has a live pass");
      const r = find(routes, p.routeId);
      const x: TransportPass = { id: nid("pass"), institutionId: inst, number: next("TP"), allocationId, personKind: p.personKind, personNumber: p.personNumber, personName: p.personName, routeId: r.id, routeCode: r.code, validFrom: str(b.validFrom), validTo: str(b.validTo), kind: "new", status: "active", feeAmount: r.fare ?? "0.00", chargeNumber: p.personKind === "student" ? next("CHG") : null, version: 1 };
      passes.push(x);
      Object.assign(p, { passId: x.id, passNumber: x.number });
      return { ...x };
    },
    passes: async (_i, status) => passes.filter((x) => !status || x.status === status).map((x) => ({ ...x })),
    renewPass: async (id, v, validTo) => {
      const x = find(passes, id);
      check(x, v);
      move(x, ["issued", "active"], "expired");
      const n: TransportPass = { ...x, id: nid("pass"), number: next("TP"), validFrom: x.validTo, validTo, kind: "renewal", status: "active", version: 1 };
      passes.push(n);
      return { ...n };
    },
    replacePass: async (id, v, _reason, fee) => {
      const x = find(passes, id);
      check(x, v);
      move(x, ["issued", "active", "suspended"], "replaced");
      const n: TransportPass = { ...x, id: nid("pass"), number: next("TP"), kind: "replacement", status: "active", feeAmount: fee || "0.00", version: 1 };
      passes.push(n);
      return { ...n };
    },
    passCommand: async (id, v, cmd) => {
      const x = find(passes, id);
      check(x, v);
      return cmd === "suspend" ? move(x, ["issued", "active"], "suspended") : move(x, ["suspended"], "active");
    },
    trips: async (_i, status) => trips.filter((t) => !status || t.status === status).map((t) => ({ ...t })),
    planTrip: async (routeId, b) => {
      const r = find(routes, routeId);
      const t: Trip = { id: nid("trip"), institutionId: inst, routeId, routeCode: r.code, tripDate: str(b.tripDate), direction: str(b.direction) || "pickup", status: "planned", needsVehicle: false, allocatedPassengers: r.passengers, version: 1 };
      trips.unshift(t);
      return { ...t };
    },
    readyTrip: async (id, v, b) => {
      const t = find(trips, id);
      check(t, v);
      const vehicle = find(vehicles, str(b.vehicleId) || t.vehicleId || "");
      const driver = find(drivers, str(b.driverAssignmentId) || t.driverAssignmentId || "");
      Object.assign(t, { vehicleId: vehicle.id, vehicleRegistration: vehicle.registrationNo, driverAssignmentId: driver.id, driverName: driver.driverName, needsVehicle: false });
      if (vehicle.status === "available") vehicle.status = "assigned";
      return move(t, ["planned"], "ready");
    },
    tripCommand: async (id, v, cmd, b) => {
      const t = find(trips, id);
      check(t, v);
      if (cmd === "start") {
        if (t.needsVehicle) throw conflict("replace the vehicle first");
        return move(t, ["ready"], "in_progress");
      }
      if (cmd === "complete") {
        t.passengerCount = num(b?.passengerCount, t.allocatedPassengers);
        return move(t, ["in_progress"], "completed");
      }
      t.statusReason = str(b?.reason);
      return move(t, ["planned", "ready"], "cancelled");
    },
    replaceVehicle: async (id, v, vehicleId) => {
      const t = find(trips, id);
      check(t, v);
      const vehicle = find(vehicles, vehicleId);
      Object.assign(t, { vehicleId: vehicle.id, vehicleRegistration: vehicle.registrationNo, needsVehicle: false, version: t.version + 1 });
      return { ...t };
    },
  };
}
