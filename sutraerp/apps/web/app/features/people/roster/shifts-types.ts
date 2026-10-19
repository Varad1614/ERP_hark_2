/**
 * ERP_EXTENSIONS 5B: wire types of shift swaps, open-shift claims, clock punches, timesheets,
 * the payroll export, roster reports and timesheet settings. They mirror the Rust structs of
 * `modules/people/src/roster/shifts_http.rs` (serde camelCase); dates are ISO `YYYY-MM-DD`,
 * instants ISO 8601 strings, times `HH:MM`.
 */
import type { DutyShift } from "./types";

// ---- swaps ------------------------------------------------------------------------------

/** `RosterSwapKind` (snake_case). Claims are made through `…/claim`, never `swaps`. */
export type SwapKind = "give_away" | "swap" | "cover" | "claim";

export type SwapStatus = "awaiting_peer" | "awaiting_manager" | "approved" | "declined" | "cancelled";

export interface SwapShiftSnapshot {
  date: string;
  start: string;
  end: string;
  role: string;
}

/** `{ shift: {date, start, end, role}, other: {…} | null }` as asked (server: free JSON). */
export interface SwapSnapshot {
  shift?: SwapShiftSnapshot | null;
  other?: SwapShiftSnapshot | null;
  [k: string]: unknown;
}

/** `RosterSwapView`. */
export interface RosterSwap {
  id: string;
  institutionId: string;
  kind: SwapKind;
  status: SwapStatus;
  shiftId: string | null;
  otherShiftId: string | null;
  fromStaffId: string | null;
  fromStaffName: string | null;
  toStaffId: string | null;
  toStaffName: string | null;
  reason: string | null;
  snapshot: SwapSnapshot;
  warnings: string[];
  requestedAt: string;
  peerRespondedAt: string | null;
  decidedAt: string | null;
  decisionReason: string | null;
  version: number;
}

/** `RosterSwapList`. */
export interface RosterSwapList {
  items: RosterSwap[];
}

/** `RosterSwapRequestInput` (`kind` is `give_away | swap | cover`). */
export interface SwapRequestInput {
  shiftId: string;
  kind: Exclude<SwapKind, "claim">;
  /** Required for `give_away` and `swap`; optional for `cover`. */
  toStaffId?: string | null;
  /** `swap` only. */
  otherShiftId?: string | null;
  reason?: string | null;
}

/** `RosterClaimInput`. */
export interface ClaimInput {
  reason?: string | null;
}

/** `RosterSwapRespond`. */
export interface SwapRespondInput {
  accept: boolean;
  reason?: string | null;
}

/** `LeaveDecisionBody` (shared 7B decision body). */
export type Decision = "approve" | "reject";
export interface DecisionInput {
  decision: Decision;
  reason?: string | null;
}

/** `RosterOpenShifts`. */
export interface RosterOpenShifts {
  from: string;
  to: string;
  items: DutyShift[];
}

// ---- punches ----------------------------------------------------------------------------

export type PunchStatus = "present" | "late" | "absent";

/** `RosterPunchView`. */
export interface RosterPunch {
  shiftId: string;
  staffId: string;
  status: PunchStatus;
  clockIn: string | null;
  clockOut: string | null;
  note: string | null;
  source: "self" | "manager";
  lateMinutes: number;
  workedMinutes: number | null;
  version: number;
}

/** `RosterClockInput`. */
export interface ClockInput {
  action: "in" | "out";
}

/** `RosterPunchInput` (manager record / correction). */
export interface PunchInput {
  status: PunchStatus;
  /** `HH:MM`. */
  clockIn?: string | null;
  /** `HH:MM` (earlier than the clock-in = after midnight). */
  clockOut?: string | null;
  note?: string | null;
}

// ---- timesheets -------------------------------------------------------------------------

export type TimesheetStatus = "submitted" | "returned" | "approved" | "locked";

export type LineStatus = "present" | "late" | "absent" | "half_day" | "on_leave" | "missed_punch" | "not_recorded" | "upcoming";

export type LineFlag = "late" | "early_leave" | "missed_punch" | "not_recorded" | "absent" | "on_leave" | "day_absent_but_punched" | "holiday";

/** `RosterTimesheetLine`. */
export interface TimesheetLine {
  shiftId: string;
  date: string;
  start: string;
  end: string;
  role: string;
  scheduledMinutes: number;
  workedMinutes: number;
  clockIn: string | null;
  clockOut: string | null;
  status: LineStatus;
  lateMinutes: number;
  earlyLeaveMinutes: number;
  night: boolean;
  holiday: boolean;
  flags: LineFlag[];
}

/** `RosterTimesheetTotals`. */
export interface TimesheetTotals {
  shifts: number;
  scheduledMinutes: number;
  workedMinutes: number;
  overtimeMinutes: number;
  lateCount: number;
  lateMinutes: number;
  earlyLeaveCount: number;
  earlyLeaveMinutes: number;
  missedPunches: number;
  notRecorded: number;
  absences: number;
  lopDays: number;
  nightShifts: number;
  holidayShifts: number;
}

/** `RosterTimesheetView`. */
export interface RosterTimesheet {
  id: string;
  institutionId: string;
  staffId: string;
  staffName: string;
  staffNumber: string;
  periodKind: "weekly" | "monthly";
  periodFrom: string;
  periodTo: string;
  status: TimesheetStatus;
  totals: TimesheetTotals;
  lines: TimesheetLine[];
  note: string | null;
  submittedAt: string;
  decidedAt: string | null;
  decisionReason: string | null;
  lockedAt: string | null;
  exportedAt: string | null;
  version: number;
}

/** `RosterTimesheetList`. */
export interface RosterTimesheetList {
  items: RosterTimesheet[];
}

/** `RosterMyTimesheet`. */
export interface RosterMyTimesheet {
  from: string;
  to: string;
  lines: TimesheetLine[];
  totals: TimesheetTotals;
  saved: RosterTimesheet | null;
}

/** `RosterTimesheetSubmit`: a Monday–Sunday week or a calendar month. */
export interface TimesheetSubmitInput {
  from: string;
  to: string;
  note?: string | null;
}

/** `TimesheetQuery`. */
export interface TimesheetQuery {
  from?: string;
  to?: string;
  status?: TimesheetStatus;
}

// ---- payroll export ---------------------------------------------------------------------

/** `RosterExportRange` (at most 93 days). */
export interface ExportRange {
  from: string;
  to: string;
}

export type PayrollQuantity = "overtime_hours" | "lop_days" | "night_shifts" | "holiday_shifts";

/** `RosterExportItem`. */
export interface RosterExportItem {
  timesheetId: string;
  staffId: string;
  staffName: string;
  periodFrom: string;
  periodTo: string;
  /** Non-zero quantities only. */
  quantities: Partial<Record<PayrollQuantity, number>>;
}

/** `RosterPayrollExport`. */
export interface RosterPayrollExport {
  exported: number;
  locked: number;
  inputs: number;
  pending: number;
  items: RosterExportItem[];
}

// ---- reports ----------------------------------------------------------------------------

export type ReportSection = "hours" | "cost" | "roles" | "attendance" | "coverage" | "departments";

/** `ReportQuery` (`section` is for the CSV only). */
export interface ReportQuery {
  from?: string;
  to?: string;
  section?: ReportSection;
}

/** `RosterDepartmentHours`. */
export interface RosterDepartmentHours {
  department: string;
  staff: number;
  shifts: number;
  scheduledMinutes: number;
  workedMinutes: number;
  overtimeMinutes: number;
}

/** `RosterOvertimeRow`. */
export interface RosterOvertimeRow {
  staffId: string;
  name: string;
  department: string;
  workedMinutes: number;
  overtimeMinutes: number;
  lateMinutes: number;
  earlyLeaveMinutes: number;
  missedPunches: number;
  nightShifts: number;
  holidayShifts: number;
}

/** `RosterSwapCounts`. */
export interface RosterSwapCounts {
  total: number;
  pending: number;
  approved: number;
  declined: number;
  cancelled: number;
  byKind: Partial<Record<SwapKind, number>>;
}

/** `RosterReport`. `report` / `previous` are Everything's free-form report objects. */
export interface RosterReport {
  from: string;
  to: string;
  report: Record<string, unknown>;
  previous: Record<string, unknown>;
  departments: RosterDepartmentHours[];
  overtime: RosterOvertimeRow[];
  swaps: RosterSwapCounts;
}

// ---- settings ---------------------------------------------------------------------------

/** `RosterTimesheetSettings` (Indian defaults: Factories Act 1948 s.51/54/59). */
export interface RosterTimesheetSettings {
  lateGraceMinutes: number;
  earlyLeaveGraceMinutes: number;
  dailyOvertimeHours: number | null;
  weeklyOvertimeHours: number | null;
  overtimeMultiplier: number;
  overtimeRatePerHour: number | null;
  standardHoursPerDay: number;
  nightShiftAllowance: number | null;
  holidayShiftAllowance: number | null;
}

/** A `from`/`to` window (`RosterRange`). */
export interface DateRange {
  from?: string;
  to?: string;
}
