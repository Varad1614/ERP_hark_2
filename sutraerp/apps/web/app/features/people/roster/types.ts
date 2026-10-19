/** ERP_EXTENSIONS 3A: wire types of the duty roster. */
export interface RuleWarning {
  rule: "rest" | "daily" | "consecutive";
  short: string;
  message: string;
}

export interface DutyShift {
  id: string;
  date: string;
  start: string;
  end: string;
  breakMinutes: number;
  netMinutes: number;
  overnight: boolean;
  role: string;
  location: string | null;
  note: string | null;
  staffId: string | null;
  staffName: string | null;
  status: "draft" | "published";
  conflicts: string[];
  availability: "available" | "partial" | "unavailable";
  ruleWarnings: RuleWarning[];
  version: number;
}

export interface CoverageGap {
  date: string;
  requirementId: string;
  role: string;
  scheduled: number;
  missing: number;
}

export interface RosterStaffRow {
  staffId: string;
  name: string;
  role: string;
  rosterable: boolean;
  targetHours: number | null;
  maxHours: number | null;
  weekMinutes: number;
  hoursStatus: "over" | "under" | "ok";
  ruleWarnings: RuleWarning[];
}

export interface RosterBoard {
  from: string;
  to: string;
  weekStart: string;
  openShifts: number;
  shifts: DutyShift[];
  gaps: CoverageGap[];
  staff: RosterStaffRow[];
}

export interface DutyShiftInput {
  date: string;
  start: string;
  end: string;
  breakMinutes?: number | null;
  role: string;
  location?: string | null;
  note?: string | null;
  staffId?: string | null;
}

export interface DutyShiftResult {
  shift: DutyShift;
  warnings: string[];
}

export interface Candidate {
  employeeId: string;
  name: string;
  status: "available" | "partial";
  deficitMinutes: number;
  roleMatch: boolean;
  projectedMinutes: number;
  ruleWarnings: RuleWarning[];
  reasons: string[];
}

export interface FillRow {
  shiftId: string;
  candidates: Candidate[];
  proposal: string | null;
  skipped: boolean;
  stale: boolean;
  pinned: boolean;
}

export interface FillPlan {
  rows: FillRow[];
  total: number;
  filling: number;
  skipped: number;
  nobody: number;
  applied: number;
}

export interface RosterRules {
  minRestHours: number | null;
  maxDailyHours: number | null;
  maxConsecutiveDays: number | null;
}

export interface RosterRequirement {
  role: string;
  days: number[];
  count: number;
  start: string | null;
  end: string | null;
}

export interface RosterSettings {
  rules: RosterRules;
  requirements: RosterRequirement[];
}
