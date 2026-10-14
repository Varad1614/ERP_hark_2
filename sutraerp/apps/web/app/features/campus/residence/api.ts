/**
 * Slice-8B API layer (hostel & transport). Same conventions as the Batch 6/7 slices: one
 * interface, an HTTP implementation (CSRF, `Idempotency-Key` on creates / bulk commands,
 * `If-Match` on state commands, problem+json → `ApiProblem`) and an in-memory mock (`./mock`).
 * Every path template is checked against the generated OpenAPI `paths`.
 */
import { ApiProblem, isProblem, isUnsafeMethod, newIdempotencyKey, type paths } from "@sutra/api-client";
import { getCsrfToken } from "../../../lib/api";
import { fill } from "../../institution/api";
import type {
  BulkSummary,
  Complaint,
  Decision,
  DriverAssignment,
  HostelAllocation,
  HostelApplication,
  HostelBed,
  HostelPolicy,
  HostelRoom,
  Items,
  Mess,
  MessAllocation,
  ResidentLeave,
  RoomInspection,
  RoomTransfer,
  RouteVersion,
  TransportAllocation,
  TransportPass,
  TransportRoute,
  TransportStop,
  Trip,
  Vehicle,
} from "./types";

export type Body = Record<string, unknown>;

export interface ResidenceApi {
  // hostel
  policies(inst: string): Promise<HostelPolicy[]>;
  createPolicy(inst: string, body: Body): Promise<HostelPolicy>;
  policyCommand(id: string, version: number, cmd: "submit" | "activate"): Promise<HostelPolicy>;
  decidePolicy(id: string, version: number, decision: Decision, reason?: string): Promise<HostelPolicy>;
  rooms(inst: string): Promise<HostelRoom[]>;
  beds(inst: string): Promise<HostelBed[]>;
  addBed(roomId: string, bedCode: string): Promise<HostelBed>;
  applications(inst: string, status?: string): Promise<HostelApplication[]>;
  apply(inst: string, body: Body): Promise<HostelApplication>;
  decideApplication(id: string, version: number, decision: Decision, reason?: string): Promise<HostelApplication>;
  allocate(id: string, version: number, bedId?: string): Promise<HostelApplication>;
  withdrawApplication(id: string, version: number, reason?: string): Promise<HostelApplication>;
  allocations(inst: string, status?: string): Promise<HostelAllocation[]>;
  allocationCommand(id: string, version: number, cmd: "vacate" | "close", reason?: string): Promise<HostelAllocation>;
  requestLeave(allocationId: string, body: Body): Promise<ResidentLeave>;
  leaves(inst: string, status?: string): Promise<ResidentLeave[]>;
  decideLeave(id: string, version: number, decision: Decision, reason?: string): Promise<ResidentLeave>;
  leaveCommand(id: string, version: number, cmd: "depart" | "return"): Promise<ResidentLeave>;
  overdueCheck(inst: string): Promise<BulkSummary>;
  transfer(allocationId: string, toBedId: string, reason?: string): Promise<RoomTransfer>;
  inspections(inst: string, status?: string): Promise<RoomInspection[]>;
  scheduleInspection(roomId: string, body: Body): Promise<RoomInspection>;
  completeInspection(id: string, version: number, body: Body): Promise<RoomInspection>;
  closeInspection(id: string, version: number, reason?: string): Promise<RoomInspection>;
  messes(inst: string): Promise<Mess[]>;
  createMess(inst: string, body: Body): Promise<Mess>;
  messAllocations(messId: string): Promise<MessAllocation[]>;
  assignMess(messId: string, body: Body): Promise<MessAllocation>;
  changeMess(id: string, version: number, body: Body): Promise<MessAllocation>;
  hostelComplaint(allocationId: string, body: Body): Promise<Complaint>;
  // transport
  routes(inst: string): Promise<TransportRoute[]>;
  createRoute(inst: string, body: Body): Promise<TransportRoute>;
  versions(routeId: string): Promise<RouteVersion[]>;
  createVersion(routeId: string, body: Body): Promise<RouteVersion>;
  setStops(versionId: string, version: number, stops: { stopId: string; sequence: number; pickupTime?: string }[]): Promise<RouteVersion>;
  routeCommand(id: string, version: number, cmd: "activate" | "suspend" | "retire", reason?: string): Promise<TransportRoute>;
  decideRoute(id: string, version: number, decision: Decision, reason?: string): Promise<TransportRoute>;
  stops(inst: string): Promise<TransportStop[]>;
  createStop(inst: string, body: Body): Promise<TransportStop>;
  stopCommand(id: string, version: number, cmd: "suspend" | "restore", reason?: string): Promise<TransportStop>;
  vehicles(inst: string): Promise<Vehicle[]>;
  createVehicle(inst: string, body: Body): Promise<Vehicle>;
  vehicleStatus(id: string, version: number, status: string, reason?: string): Promise<Vehicle>;
  breakdown(id: string, version: number, reason?: string): Promise<Vehicle>;
  drivers(routeId: string): Promise<DriverAssignment[]>;
  assignDriver(routeId: string, body: Body): Promise<DriverAssignment>;
  passengers(inst: string, status?: string): Promise<TransportAllocation[]>;
  allocatePassenger(inst: string, body: Body): Promise<TransportAllocation>;
  passengerCommand(id: string, version: number, cmd: "activate" | "withdraw", reason?: string): Promise<TransportAllocation>;
  changePassenger(id: string, version: number, body: Body): Promise<TransportAllocation>;
  issuePass(allocationId: string, body: Body): Promise<TransportPass>;
  passes(inst: string, status?: string): Promise<TransportPass[]>;
  renewPass(id: string, version: number, validTo: string): Promise<TransportPass>;
  replacePass(id: string, version: number, reason: string, fee?: string): Promise<TransportPass>;
  passCommand(id: string, version: number, cmd: "suspend" | "restore", reason?: string): Promise<TransportPass>;
  trips(inst: string, status?: string): Promise<Trip[]>;
  planTrip(routeId: string, body: Body): Promise<Trip>;
  readyTrip(id: string, version: number, body: Body): Promise<Trip>;
  tripCommand(id: string, version: number, cmd: "start" | "complete" | "cancel", body?: Body): Promise<Trip>;
  replaceVehicle(id: string, version: number, vehicleId: string, reason?: string): Promise<Trip>;
}

const C = "/api/v1/campus";
export const PATHS = {
  policies: `${C}/institutions/{id}/hostel-policies`,
  policySubmit: `${C}/hostel-policies/{id}/submit`,
  policyActivate: `${C}/hostel-policies/{id}/activate`,
  policyDecision: `${C}/hostel-policies/{id}/decision`,
  tree: `${C}/institutions/{id}/infrastructure-tree`,
  beds: `${C}/institutions/{id}/hostel-beds`,
  roomBeds: `${C}/hostel-rooms/{id}/beds`,
  applications: `${C}/institutions/{id}/hostel-applications`,
  applicationDecision: `${C}/hostel-applications/{id}/decision`,
  allocate: `${C}/hostel-applications/{id}/allocate`,
  applicationWithdraw: `${C}/hostel-applications/{id}/withdraw`,
  allocations: `${C}/institutions/{id}/hostel-allocations`,
  vacate: `${C}/hostel-allocations/{id}/vacate`,
  close: `${C}/hostel-allocations/{id}/close`,
  leave: `${C}/hostel-allocations/{id}/leave`,
  transfer: `${C}/hostel-allocations/{id}/transfer`,
  hostelComplaints: `${C}/hostel-allocations/{id}/complaints`,
  leaves: `${C}/institutions/{id}/resident-leaves`,
  overdue: `${C}/institutions/{id}/resident-leaves/overdue-check`,
  leaveDecision: `${C}/resident-leaves/{id}/decision`,
  leaveDepart: `${C}/resident-leaves/{id}/depart`,
  leaveReturn: `${C}/resident-leaves/{id}/return`,
  inspections: `${C}/institutions/{id}/room-inspections`,
  roomInspections: `${C}/hostel-rooms/{id}/inspections`,
  inspectionComplete: `${C}/room-inspections/{id}/complete`,
  inspectionClose: `${C}/room-inspections/{id}/close`,
  messes: `${C}/institutions/{id}/messes`,
  messAllocations: `${C}/messes/{id}/allocations`,
  messChange: `${C}/mess-allocations/{id}/change`,
  routes: `${C}/institutions/{id}/transport-routes`,
  versions: `${C}/transport-routes/{id}/versions`,
  versionStops: `${C}/route-versions/{id}/stops`,
  routeActivate: `${C}/transport-routes/{id}/activate`,
  routeSuspend: `${C}/transport-routes/{id}/suspend`,
  routeRetire: `${C}/transport-routes/{id}/retire`,
  routeDecision: `${C}/transport-routes/{id}/decision`,
  stops: `${C}/institutions/{id}/transport-stops`,
  stopSuspend: `${C}/transport-stops/{id}/suspend`,
  stopRestore: `${C}/transport-stops/{id}/restore`,
  vehicles: `${C}/institutions/{id}/vehicles`,
  vehicleStatus: `${C}/vehicles/{id}/status`,
  vehicleBreakdown: `${C}/vehicles/{id}/breakdown`,
  drivers: `${C}/transport-routes/{id}/drivers`,
  passengers: `${C}/institutions/{id}/transport-allocations`,
  passengerActivate: `${C}/transport-allocations/{id}/activate`,
  passengerWithdraw: `${C}/transport-allocations/{id}/withdraw`,
  passengerChange: `${C}/transport-allocations/{id}/change`,
  passIssue: `${C}/transport-allocations/{id}/pass`,
  passes: `${C}/institutions/{id}/transport-passes`,
  passRenew: `${C}/transport-passes/{id}/renew`,
  passReplace: `${C}/transport-passes/{id}/replace`,
  passSuspend: `${C}/transport-passes/{id}/suspend`,
  passRestore: `${C}/transport-passes/{id}/restore`,
  trips: `${C}/institutions/{id}/trips`,
  routeTrips: `${C}/transport-routes/{id}/trips`,
  tripReady: `${C}/trips/{id}/ready`,
  tripStart: `${C}/trips/{id}/start`,
  tripComplete: `${C}/trips/{id}/complete`,
  tripCancel: `${C}/trips/{id}/cancel`,
  tripReplace: `${C}/trips/{id}/replace-vehicle`,
} as const satisfies Record<string, keyof paths>;

type Fetch = (input: string, init: RequestInit) => Promise<Response>;

interface RequestOptions {
  body?: unknown;
  ifMatch?: number;
  idempotent?: boolean;
}

const reasonBody = (reason?: string) => ({ reason: reason || undefined });
const decisionBody = (decision: Decision, reason?: string) => ({ decision, reason: reason || undefined });
const q = (status?: string) => (status ? `?status=${encodeURIComponent(status)}` : "");

export function createHttpResidenceApi(fetchImpl: Fetch = (i, init) => fetch(i, init), baseUrl: string = import.meta.env.VITE_API_BASE ?? ""): ResidenceApi {
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
  const create = <T>(template: string, owner: string, body: unknown) => request<T>("POST", id(template, owner), { body, idempotent: true });
  const command = <T>(template: string, target: string, version: number, body: unknown = {}) => request<T>("POST", id(template, target), { body, ifMatch: version });
  return {
    policies: (inst) => items(id(PATHS.policies, inst)),
    createPolicy: (inst, body) => create(PATHS.policies, inst, body),
    policyCommand: (pid, v, cmd) => command(cmd === "submit" ? PATHS.policySubmit : PATHS.policyActivate, pid, v),
    decidePolicy: (pid, v, d, reason) => command(PATHS.policyDecision, pid, v, decisionBody(d, reason)),
    rooms: (inst) => items(`${id(PATHS.tree, inst)}?spaceType=hostel_room`),
    beds: (inst) => items(id(PATHS.beds, inst)),
    addBed: (room, bedCode) => create(PATHS.roomBeds, room, { bedCode }),
    applications: (inst, status) => items(`${id(PATHS.applications, inst)}${q(status)}`),
    apply: (inst, body) => create(PATHS.applications, inst, body),
    decideApplication: (aid, v, d, reason) => command(PATHS.applicationDecision, aid, v, decisionBody(d, reason)),
    allocate: (aid, v, bedId) => command(PATHS.allocate, aid, v, { bedId: bedId || undefined }),
    withdrawApplication: (aid, v, reason) => command(PATHS.applicationWithdraw, aid, v, reasonBody(reason)),
    allocations: (inst, status) => items(`${id(PATHS.allocations, inst)}${q(status)}`),
    allocationCommand: (aid, v, cmd, reason) => command(cmd === "vacate" ? PATHS.vacate : PATHS.close, aid, v, reasonBody(reason)),
    requestLeave: (aid, body) => create(PATHS.leave, aid, body),
    leaves: (inst, status) => items(`${id(PATHS.leaves, inst)}${q(status)}`),
    decideLeave: (lid, v, d, reason) => command(PATHS.leaveDecision, lid, v, decisionBody(d, reason)),
    leaveCommand: (lid, v, cmd) => command(cmd === "depart" ? PATHS.leaveDepart : PATHS.leaveReturn, lid, v),
    overdueCheck: (inst) => create(PATHS.overdue, inst, {}),
    transfer: (aid, toBedId, reason) => create(PATHS.transfer, aid, { toBedId, reason: reason || undefined }),
    inspections: (inst, status) => items(`${id(PATHS.inspections, inst)}${q(status)}`),
    scheduleInspection: (room, body) => create(PATHS.roomInspections, room, body),
    completeInspection: (iid, v, body) => command(PATHS.inspectionComplete, iid, v, body),
    closeInspection: (iid, v, reason) => command(PATHS.inspectionClose, iid, v, reasonBody(reason)),
    messes: (inst) => items(id(PATHS.messes, inst)),
    createMess: (inst, body) => create(PATHS.messes, inst, body),
    messAllocations: (mid) => items(id(PATHS.messAllocations, mid)),
    assignMess: (mid, body) => create(PATHS.messAllocations, mid, body),
    changeMess: (maid, v, body) => command(PATHS.messChange, maid, v, body),
    hostelComplaint: (aid, body) => create(PATHS.hostelComplaints, aid, body),
    routes: (inst) => items(id(PATHS.routes, inst)),
    createRoute: (inst, body) => create(PATHS.routes, inst, body),
    versions: (rid) => items(id(PATHS.versions, rid)),
    createVersion: (rid, body) => create(PATHS.versions, rid, body),
    setStops: (vid, v, stops) => command(PATHS.versionStops, vid, v, { stops }),
    routeCommand: (rid, v, cmd, reason) =>
      command(cmd === "activate" ? PATHS.routeActivate : cmd === "suspend" ? PATHS.routeSuspend : PATHS.routeRetire, rid, v, cmd === "activate" ? {} : reasonBody(reason)),
    decideRoute: (rid, v, d, reason) => command(PATHS.routeDecision, rid, v, decisionBody(d, reason)),
    stops: (inst) => items(id(PATHS.stops, inst)),
    createStop: (inst, body) => create(PATHS.stops, inst, body),
    stopCommand: (sid, v, cmd, reason) => command(cmd === "suspend" ? PATHS.stopSuspend : PATHS.stopRestore, sid, v, reasonBody(reason)),
    vehicles: (inst) => items(id(PATHS.vehicles, inst)),
    createVehicle: (inst, body) => create(PATHS.vehicles, inst, body),
    vehicleStatus: (vid, v, status, reason) => command(PATHS.vehicleStatus, vid, v, { status, reason: reason || undefined }),
    breakdown: (vid, v, reason) => command(PATHS.vehicleBreakdown, vid, v, reasonBody(reason)),
    drivers: (rid) => items(id(PATHS.drivers, rid)),
    assignDriver: (rid, body) => create(PATHS.drivers, rid, body),
    passengers: (inst, status) => items(`${id(PATHS.passengers, inst)}${q(status)}`),
    allocatePassenger: (inst, body) => create(PATHS.passengers, inst, body),
    passengerCommand: (aid, v, cmd, reason) => command(cmd === "activate" ? PATHS.passengerActivate : PATHS.passengerWithdraw, aid, v, cmd === "activate" ? {} : reasonBody(reason)),
    changePassenger: (aid, v, body) => command(PATHS.passengerChange, aid, v, body),
    issuePass: (aid, body) => create(PATHS.passIssue, aid, body),
    passes: (inst, status) => items(`${id(PATHS.passes, inst)}${q(status)}`),
    renewPass: (pid, v, validTo) => command(PATHS.passRenew, pid, v, { validTo }),
    replacePass: (pid, v, reason, fee) => command(PATHS.passReplace, pid, v, { reason, fee: fee || undefined }),
    passCommand: (pid, v, cmd, reason) => command(cmd === "suspend" ? PATHS.passSuspend : PATHS.passRestore, pid, v, reasonBody(reason)),
    trips: (inst, status) => items(`${id(PATHS.trips, inst)}${q(status)}`),
    planTrip: (rid, body) => create(PATHS.routeTrips, rid, body),
    readyTrip: (tid, v, body) => command(PATHS.tripReady, tid, v, body),
    tripCommand: (tid, v, cmd, body) => command(cmd === "start" ? PATHS.tripStart : cmd === "complete" ? PATHS.tripComplete : PATHS.tripCancel, tid, v, body ?? {}),
    replaceVehicle: (tid, v, vehicleId, reason) => command(PATHS.tripReplace, tid, v, { vehicleId, reason: reason || undefined }),
  };
}
