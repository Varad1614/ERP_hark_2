/**
 * Slice-8A wire types (campus infrastructure, room booking, service desk, maintenance, labs) —
 * straight from the generated OpenAPI client. Decimals (area) stay the strings the API sends.
 */
import type { components } from "@sutra/api-client";

type S = components["schemas"];

export interface Items<T> {
  items: T[];
}

// ---- infrastructure + booking
export type InfraNode = S["InfrastructureNodeView"];
export type InfraNodeDetail = S["InfrastructureNodeDetail"];
export type NodeType = S["InfrastructureNodeType"];
export type SpaceType = S["SpaceType"];
export type InfraStatus = S["InfrastructureStatus"];
export type Reservation = S["ReservationView"];
export type SpaceCalendar = S["SpaceCalendar"];
export type NewNode = S["InfrastructureNodeCreate"];
export type NewReservation = S["ReservationCreate"];

// ---- service desk + maintenance
export type Ticket = S["TicketView"];
export type TicketDetail = S["TicketDetail"];
export type TicketList = S["TicketList"];
export type NewTicket = S["TicketCreate"];
export type TicketCategory = S["TicketCategory"];
export type TicketPriority = S["TicketPriority"];
export type WorkOrder = S["WorkOrderView"];
export type WorkOrderDetail = S["WorkOrderDetail"];
export type Plan = S["MaintenancePlanView"];
export type PlanDetail = S["MaintenancePlanDetail"];
export type NewPlan = S["MaintenancePlanCreate"];
export type BulkSummary = S["FacilityBulkSummary"];

// ---- labs
export type Lab = S["LabView"];
export type LabDetail = S["LabDetail"];
export type Equipment = S["LabEquipmentView"];
export type EquipmentStatus = S["EquipmentStatus"];
export type Session = S["PracticalSessionView"];
export type NewSession = S["PracticalSessionCreate"];
export type ChecklistItem = S["LabChecklistItem"];

export type NodeCommand = "activate" | "suspend" | "restore" | "decommission" | "archive";
export type TicketCommand = "triage" | "start" | "hold" | "close" | "cancel" | "escalate" | "reopen";
export type WorkOrderCommand = "start" | "reopen" | "cancel";
export type PlanCommand = "submit" | "activate" | "suspend" | "retire";
export type SessionCommand = "ready" | "start" | "complete" | "cancel";
