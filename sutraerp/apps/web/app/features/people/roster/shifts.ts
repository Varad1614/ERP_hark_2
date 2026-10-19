/**
 * ERP_EXTENSIONS 5B screen helpers: wording of durations, swap and timesheet statuses, the
 * actions each status allows (mirroring the server's 409/403 checks in `shifts_http.rs`, so
 * buttons only show when the server would accept them) and a short totals summary.
 */
import type { RosterSwap, SwapKind, SwapStatus, TimesheetStatus, TimesheetTotals } from "./shifts-types";

/** 450 → "7h 30m", 480 → "8h", 45 → "45m", 0 → "0m"; negatives keep their sign. */
export function duration(minutes: number | null | undefined): string {
  if (minutes === null || minutes === undefined || !Number.isFinite(minutes)) return "—";
  const total = Math.round(Math.abs(minutes));
  const sign = minutes < 0 && total > 0 ? "-" : "";
  const h = Math.floor(total / 60);
  const m = total % 60;
  if (h === 0) return `${sign}${m}m`;
  return m === 0 ? `${sign}${h}h` : `${sign}${h}h ${m}m`;
}

export const SWAP_KIND_LABELS: Record<SwapKind, string> = {
  give_away: "Give away",
  swap: "Swap",
  cover: "Cover",
  claim: "Open-shift claim",
};

export const SWAP_STATUS_LABELS: Record<SwapStatus, string> = {
  awaiting_peer: "Awaiting colleague",
  awaiting_manager: "Awaiting manager",
  approved: "Approved",
  declined: "Declined",
  cancelled: "Withdrawn",
};

export const TIMESHEET_STATUS_LABELS: Record<TimesheetStatus, string> = {
  submitted: "Submitted",
  returned: "Returned",
  approved: "Approved",
  locked: "Locked",
};

export type Tone = "neutral" | "warning" | "success" | "danger";

export function swapTone(s: SwapStatus): Tone {
  return s === "approved" ? "success" : s === "declined" ? "danger" : s === "cancelled" ? "neutral" : "warning";
}

export function timesheetTone(s: TimesheetStatus): Tone {
  return s === "approved" || s === "locked" ? "success" : s === "returned" ? "danger" : "warning";
}

export const swapStatusLabel = (s: string): string => SWAP_STATUS_LABELS[s as SwapStatus] ?? s;
export const timesheetStatusLabel = (s: string): string => TIMESHEET_STATUS_LABELS[s as TimesheetStatus] ?? s;

export const isPendingSwap = (s: SwapStatus): boolean => s === "awaiting_peer" || s === "awaiting_manager";

export type SwapAction = "respond" | "cancel" | "decide";

/** Who is looking: their staff id (self-service) and whether they hold `people.staff.manage`. */
export interface Viewer {
  staffId?: string | null;
  manager?: boolean;
}

/** The requester: the staff member giving the shift away, or the claimant of an open shift. */
export function requesterOf(s: Pick<RosterSwap, "kind" | "fromStaffId" | "toStaffId">): string | null {
  return s.kind === "claim" ? s.toStaffId : s.fromStaffId;
}

/**
 * Actions the server would accept: the colleague (or anyone but the requester for an open
 * cover) responds while `awaiting_peer`; the requester withdraws while pending; a manager who
 * is neither party decides while `awaiting_manager`.
 */
export function swapActions(s: Pick<RosterSwap, "kind" | "status" | "fromStaffId" | "toStaffId">, viewer: Viewer = {}): SwapAction[] {
  const me = viewer.staffId ?? null;
  const requester = requesterOf(s);
  const mine = me !== null && requester === me;
  const party = me !== null && (s.fromStaffId === me || s.toStaffId === me);
  const out: SwapAction[] = [];
  if (s.status === "awaiting_peer" && me !== null && !mine && s.kind !== "claim" && (s.toStaffId === null || s.toStaffId === me)) out.push("respond");
  if (isPendingSwap(s.status) && mine) out.push("cancel");
  if (s.status === "awaiting_manager" && viewer.manager && !party) out.push("decide");
  return out;
}

export type TimesheetAction = "submit" | "decide" | "lock" | "export";

/**
 * Actions per timesheet status: staff submit when there is none yet or it was returned; a
 * manager (not the staff member) decides a submitted one and locks an approved one; payroll
 * exports approved or locked ones not yet exported.
 */
export function timesheetActions(
  status: TimesheetStatus | null,
  viewer: Viewer & { own?: boolean; payroll?: boolean; exported?: boolean } = {},
): TimesheetAction[] {
  const out: TimesheetAction[] = [];
  if (viewer.own && (status === null || status === "returned")) out.push("submit");
  if (viewer.manager && !viewer.own) {
    if (status === "submitted") out.push("decide");
    if (status === "approved") out.push("lock");
  }
  if (viewer.payroll && !viewer.exported && (status === "approved" || status === "locked")) out.push("export");
  return out;
}

/** "5 shifts · 38h 30m worked of 40h · 2h overtime · 1 late · 1 missed punch". */
export function totalsSummary(t: TimesheetTotals): string {
  const n = (count: number, one: string, many = `${one}s`) => `${count} ${count === 1 ? one : many}`;
  const parts = [n(t.shifts, "shift"), `${duration(t.workedMinutes)} worked of ${duration(t.scheduledMinutes)}`];
  if (t.overtimeMinutes > 0) parts.push(`${duration(t.overtimeMinutes)} overtime`);
  if (t.lateCount > 0) parts.push(`${t.lateCount} late`);
  if (t.earlyLeaveCount > 0) parts.push(`${t.earlyLeaveCount} left early`);
  if (t.missedPunches > 0) parts.push(n(t.missedPunches, "missed punch", "missed punches"));
  if (t.notRecorded > 0) parts.push(`${t.notRecorded} not recorded`);
  if (t.absences > 0) parts.push(n(t.absences, "absence"));
  if (t.lopDays > 0) parts.push(`${t.lopDays} LOP ${t.lopDays === 1 ? "day" : "days"}`);
  if (t.nightShifts > 0) parts.push(n(t.nightShifts, "night shift"));
  if (t.holidayShifts > 0) parts.push(n(t.holidayShifts, "holiday shift"));
  return parts.join(" · ");
}

/** Sums totals of several timesheets (e.g. a manager's list footer). */
export function sumTotals(list: TimesheetTotals[]): TimesheetTotals {
  const zero: TimesheetTotals = {
    shifts: 0,
    scheduledMinutes: 0,
    workedMinutes: 0,
    overtimeMinutes: 0,
    lateCount: 0,
    lateMinutes: 0,
    earlyLeaveCount: 0,
    earlyLeaveMinutes: 0,
    missedPunches: 0,
    notRecorded: 0,
    absences: 0,
    lopDays: 0,
    nightShifts: 0,
    holidayShifts: 0,
  };
  return list.reduce((acc, t) => {
    const out = { ...acc };
    for (const k of Object.keys(zero) as (keyof TimesheetTotals)[]) out[k] = acc[k] + (t[k] ?? 0);
    return out;
  }, zero);
}
