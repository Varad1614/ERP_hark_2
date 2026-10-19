/**
 * Slice-7B wire types (leave, attendance & workload). Days and hours stay the decimal strings the
 * API sends (`"2.50"`); they are never parsed into floats for display.
 */

export interface Items<T> {
  items: T[];
}

export interface LeaveType {
  id: string;
  policyId: string;
  code: string;
  name: string;
  annualEntitlement: string;
  accrualFrequency: "monthly" | "yearly" | "none";
  carryForwardCap: string;
  encashmentCap: string;
  isPaid: boolean;
  requiresBalance: boolean;
  halfDayAllowed: boolean;
  applicableCategories: string[];
}

export interface LeavePolicy {
  id: string;
  institutionId: string;
  name: string;
  versionNo: number;
  effectiveFrom: string;
  weeklyOffDays: number[];
  holidays: string[];
  status: string;
  statusReason?: string | null;
  validationFindings: string[];
  supersedesPolicyId?: string | null;
  processInstanceId?: string | null;
  leaveTypes: LeaveType[];
  version: number;
}

export interface LeaveSegment {
  month: string;
  firstDay: string;
  lastDay: string;
  days: string;
  consumedEntryId?: string | null;
}

export interface LeaveRequest {
  id: string;
  institutionId: string;
  number: string;
  staffId: string;
  staffNumber: string;
  displayName: string;
  leaveTypeId: string;
  leaveTypeCode: string;
  isPaid: boolean;
  startDate: string;
  endDate: string;
  halfDayStart: boolean;
  halfDayEnd: boolean;
  days: string;
  leaveYear: number;
  reason?: string | null;
  status: string;
  statusReason?: string | null;
  approverStaffId?: string | null;
  decidedBy?: string | null;
  processInstanceId?: string | null;
  segments: LeaveSegment[];
  version: number;
}

export interface LeaveRequestList extends Items<LeaveRequest> {
  nextCursor?: string | null;
}

export interface LeaveBalance {
  leaveTypeId: string;
  leaveTypeCode: string;
  leaveYear: number;
  entitled: string;
  reserved: string;
  consumed: string;
  available: string;
}

export interface BulkSummary {
  processed: number;
  skipped: number;
  failed: { id: string; reason: string }[];
}

export interface AccrualRun {
  id: string;
  period: string;
  leaveYear: number;
  status: string;
  staffCount: number;
  processed: number;
  skipped: number;
  runs: number;
}

export interface YearEndRun {
  id: string;
  leaveYear: number;
  status: string;
  staffCount: number;
  carriedDays: string;
  expiredDays: string;
  runs: number;
}

export interface LeaveAdjustment {
  id: string;
  staffId: string;
  leaveTypeCode: string;
  leaveYear: number;
  days: string;
  reason: string;
  status: string;
  version: number;
}

export interface LeaveEncashment {
  id: string;
  staffId: string;
  leaveTypeCode: string;
  leaveYear: number;
  days: string;
  availableDays: string;
  status: string;
  statusReason?: string | null;
  version: number;
}

export interface LeaveException {
  id: string;
  number: string;
  staffId?: string | null;
  leaveRequestId?: string | null;
  category: string;
  description: string;
  status: string;
  resolution?: string | null;
  version: number;
}

export interface AttendanceDay {
  id: string;
  staffId: string;
  workDate: string;
  status: string;
  source: string;
  leaveRequestId?: string | null;
  remark?: string | null;
  version: number;
}

export interface AttendanceGridRow {
  staffId: string;
  staffNumber: string;
  displayName: string;
  engagementStatus: string;
  available: boolean;
  day?: AttendanceDay | null;
}

export interface AttendanceGrid extends Items<AttendanceGridRow> {
  workDate: string;
}

export interface LongLeave {
  id: string;
  staffId: string;
  engagementId: string;
  reason: string;
  plannedStart: string;
  plannedEnd?: string | null;
  actualStart?: string | null;
  actualEnd?: string | null;
  status: string;
  engagementStatus: string;
  available: boolean;
  version: number;
}

export interface WorkloadPlan {
  id: string;
  staffId: string;
  termLabel: string;
  availability: string;
  plannedWeeklyHours: string;
  maxWeeklyHours?: string | null;
  teachingWeeklyHours?: string | null;
  effectiveMaxHours?: string | null;
  overAllocated: boolean;
  exceptionReason?: string | null;
  status: string;
  version: number;
}

export interface TeachingLoadItem {
  assignmentId: string;
  offeringCode: string;
  title: string;
  role: string;
  status: string;
  weeklyHours?: string | null;
}

export interface StaffWorkload {
  staffId: string;
  engagementStatus?: string | null;
  available: boolean;
  totalWeeklyHours: string;
  policyMaxWeeklyHours?: string | null;
  effectiveMaxHours?: string | null;
  overAllocated: boolean;
  items: TeachingLoadItem[];
  activePlan?: WorkloadPlan | null;
}

/** The foundation staff list row (only what the 7B screens use). */
export interface StaffRow {
  id: string;
  staffNumber: string;
  displayName: string;
  staffCategory: string;
  status: string;
  engagement?: { id: string; status: string; designation: string } | null;
}

export interface LeaveRequestInput {
  staffId?: string;
  leaveTypeId: string;
  startDate: string;
  endDate: string;
  halfDayStart?: boolean;
  halfDayEnd?: boolean;
  reason?: string;
}

export interface LeaveTypeInput {
  code: string;
  name: string;
  annualEntitlement?: string;
  accrualFrequency?: string;
  carryForwardCap?: string;
  encashmentCap?: string;
  isPaid?: boolean;
  requiresBalance?: boolean;
}

export type PolicyCommand = "submit" | "activate" | "supersede";
