/**
 * Slice-8B wire types (hostel & transport). They mirror the `campus-hostel` / `campus-transport`
 * OpenAPI schemas (money and dates stay the strings the API sends — never floats).
 */

export interface Items<T> {
  items: T[];
}

export interface BulkSummary {
  processed: number;
  skipped: number;
  failed: { id: string; error: string }[];
}

// ------------------------------------------------------------------ hostel

export interface HostelPolicy {
  id: string;
  institutionId: string;
  name: string;
  feePerTerm: string;
  deposit: string;
  curfewTime?: string | null;
  eligibilityNote?: string | null;
  maxStayTerms?: number | null;
  effectiveFrom: string;
  status: string;
  statusReason?: string | null;
  processInstanceId?: string | null;
  activatedAt?: string | null;
  version: number;
}

export interface HostelBed {
  id: string;
  institutionId: string;
  roomNodeId: string;
  roomCode: string;
  roomName: string;
  bedCode: string;
  status: string;
  allocationId?: string | null;
  occupantNumber?: string | null;
  occupantName?: string | null;
  version: number;
}

export interface HostelRoom {
  id: string;
  code: string;
  name: string;
  capacity?: number | null;
  status: string;
}

export interface HostelApplication {
  id: string;
  institutionId: string;
  number: string;
  personKind: string;
  personId: string;
  personNumber: string;
  personName: string;
  policyId: string;
  preference?: string | null;
  channel: string;
  status: string;
  statusReason?: string | null;
  processInstanceId?: string | null;
  waitlistedAt?: string | null;
  decidedAt?: string | null;
  allocationId?: string | null;
  createdAt: string;
  version: number;
}

export interface HostelAllocation {
  id: string;
  institutionId: string;
  applicationId: string;
  personKind: string;
  personId: string;
  personNumber: string;
  personName: string;
  policyId: string;
  bedId: string;
  bedCode: string;
  roomNodeId: string;
  roomCode: string;
  startDate: string;
  endDate?: string | null;
  status: string;
  statusReason?: string | null;
  feeAmount: string;
  chargeId?: string | null;
  chargeNumber?: string | null;
  closedAt?: string | null;
  createdAt: string;
  version: number;
}

export interface ResidentLeave {
  id: string;
  institutionId: string;
  allocationId: string;
  personNumber: string;
  personName: string;
  fromDate: string;
  toDate: string;
  destination?: string | null;
  reason?: string | null;
  status: string;
  statusReason?: string | null;
  processInstanceId?: string | null;
  departedAt?: string | null;
  returnedAt?: string | null;
  returnedLate: boolean;
  version: number;
}

export interface RoomTransfer {
  id: string;
  allocationId: string;
  personNumber: string;
  fromBedCode: string;
  fromRoomCode: string;
  toBedCode: string;
  toRoomCode: string;
  status: string;
  version: number;
}

export interface RoomInspection {
  id: string;
  institutionId: string;
  roomNodeId: string;
  roomCode: string;
  allocationId?: string | null;
  personNumber?: string | null;
  scheduledOn: string;
  status: string;
  statusReason?: string | null;
  findings?: string | null;
  damageAmount?: string | null;
  chargeId?: string | null;
  chargeNumber?: string | null;
  completedAt?: string | null;
  version: number;
}

export interface Mess {
  id: string;
  institutionId: string;
  code: string;
  name: string;
  menuType: string;
  capacity: number;
  assigned: number;
  status: string;
  version: number;
}

export interface MessAllocation {
  id: string;
  messId: string;
  messCode: string;
  hostelAllocationId: string;
  personNumber: string;
  personName: string;
  startDate: string;
  endDate?: string | null;
  status: string;
  previousId?: string | null;
  version: number;
}

export interface Complaint {
  id: string;
  number: string;
  category: string;
  source: string;
  sourceRefId?: string | null;
  title: string;
  description?: string | null;
  priority: string;
  status: string;
  slaDueAt: string;
  version: number;
}

// ------------------------------------------------------------------ transport

export interface TransportRoute {
  id: string;
  institutionId: string;
  code: string;
  name: string;
  status: string;
  statusReason?: string | null;
  currentVersionId?: string | null;
  currentVersionNo?: number | null;
  fare?: string | null;
  capacity?: number | null;
  effectiveFrom?: string | null;
  passengers: number;
  hasDraft: boolean;
  processInstanceId?: string | null;
  version: number;
}

export interface RouteStop {
  stopId: string;
  code: string;
  name: string;
  sequence: number;
  pickupTime?: string | null;
  stopStatus: string;
}

export interface RouteVersion {
  id: string;
  routeId: string;
  versionNo: number;
  effectiveFrom: string;
  effectiveTo?: string | null;
  capacity: number;
  fare: string;
  note?: string | null;
  status: string;
  processInstanceId?: string | null;
  version: number;
  stops: RouteStop[];
}

export interface TransportStop {
  id: string;
  institutionId: string;
  code: string;
  name: string;
  landmark?: string | null;
  status: string;
  passengers: number;
  version: number;
}

export interface Vehicle {
  id: string;
  institutionId: string;
  registrationNo: string;
  label: string;
  capacity: number;
  status: string;
  statusReason?: string | null;
  openTrips: number;
  version: number;
}

export interface DriverAssignment {
  id: string;
  routeId: string;
  routeCode: string;
  staffId: string;
  staffNumber: string;
  driverName: string;
  licenceNo: string;
  validFrom: string;
  validTo?: string | null;
  status: string;
  version: number;
}

export interface TransportAllocation {
  id: string;
  institutionId: string;
  personKind: string;
  personId: string;
  personNumber: string;
  personName: string;
  routeId: string;
  routeCode: string;
  stopId: string;
  stopCode: string;
  stopName: string;
  channel: string;
  startDate: string;
  endDate?: string | null;
  status: string;
  flagged: boolean;
  flagReason?: string | null;
  previousId?: string | null;
  passId?: string | null;
  passNumber?: string | null;
  version: number;
}

export interface TransportPass {
  id: string;
  institutionId: string;
  number: string;
  allocationId: string;
  personKind: string;
  personNumber: string;
  personName: string;
  routeId: string;
  routeCode: string;
  validFrom: string;
  validTo: string;
  kind: string;
  status: string;
  feeAmount: string;
  chargeNumber?: string | null;
  version: number;
}

export interface Trip {
  id: string;
  institutionId: string;
  routeId: string;
  routeCode: string;
  tripDate: string;
  direction: string;
  status: string;
  statusReason?: string | null;
  vehicleId?: string | null;
  vehicleRegistration?: string | null;
  driverAssignmentId?: string | null;
  driverName?: string | null;
  needsVehicle: boolean;
  passengerCount?: number | null;
  allocatedPassengers: number;
  version: number;
}

export type Decision = "approve" | "reject";
