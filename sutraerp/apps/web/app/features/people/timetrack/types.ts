/**
 * ERP_EXTENSIONS 5C wire types (local: the generated schema picks these up from the CI OpenAPI
 * artifact later). Money is a decimal string, rates are ₹/hour numbers, durations are seconds.
 */
export type ProjectKind = "sponsored_research" | "consultancy" | "internal_project" | "committee" | "other";
export type MemberRole = "pi" | "co_pi" | "member" | "jrf" | "srf" | "research_associate" | "project_assistant" | "technical_staff";
export type TimesheetStatus = "draft" | "submitted" | "approved" | "rejected";
export type RatePeriod = { from: string | null; rate: number | null };
export type RateValue = number | RatePeriod[];

export interface TimeSettings {
  weekStart: number;
  roundingMinutes: number;
  standardMonthlyHours: number;
  defaultHourlyRate: RateValue | null;
  timerCapHours: number;
}

export interface ProjectMember {
  staffId: string;
  name: string;
  staffNumber: string;
  role: MemberRole;
  commitmentPercent: number | null;
}

export interface BudgetProgress {
  kind: "hours" | "amount";
  period: "total" | "monthly";
  spent: number;
  value: number;
  ratio: number;
}

export interface TimeProject {
  id: string;
  institutionId: string;
  code: string;
  name: string;
  kind: ProjectKind;
  fundingAgency: string | null;
  scheme: string | null;
  sanctionNumber: string | null;
  sanctionDate: string | null;
  sanctionedAmount: string | null;
  manpowerBudget: string | null;
  startDate: string | null;
  endDate: string | null;
  piStaffId: string | null;
  piName: string | null;
  costCentre: string | null;
  contractId: string | null;
  budget: { kind: "hours" | "amount"; period: "total" | "monthly"; value: string } | null;
  budgetProgress: BudgetProgress | null;
  roundingMinutes: number | null;
  minimumMinutes: number | null;
  billableDefault: boolean;
  openToAll: boolean;
  status: "active" | "closed" | "archived";
  notes: string | null;
  members: ProjectMember[];
  loggedSeconds: number;
  version: number;
}

export interface TimeProjectInput {
  code: string;
  name: string;
  kind: ProjectKind;
  fundingAgency?: string | null;
  scheme?: string | null;
  sanctionNumber?: string | null;
  sanctionDate?: string | null;
  sanctionedAmount?: string | null;
  manpowerBudget?: string | null;
  startDate?: string | null;
  endDate?: string | null;
  piStaffId?: string | null;
  costCentre?: string | null;
  contractId?: string | null;
  budgetKind?: "hours" | "amount" | null;
  budgetPeriod?: "total" | "monthly" | null;
  budgetValue?: string | null;
  roundingMinutes?: number | null;
  minimumMinutes?: number | null;
  billableDefault?: boolean;
  openToAll?: boolean;
  status?: "active" | "closed" | "archived";
  notes?: string | null;
  members: { staffId: string; role: MemberRole; commitmentPercent?: number | null }[];
}

export interface TimeEntry {
  id: string;
  staffId: string;
  staffName: string;
  projectId: string;
  projectCode: string;
  projectName: string;
  date: string;
  seconds: number;
  duration: string;
  start: string | null;
  end: string | null;
  endsNextDay: boolean;
  task: string;
  notes: string;
  billable: boolean;
  source: "manual" | "timer" | "import";
  locked: boolean;
  version: number;
}

export interface TimeEntryInput {
  projectId: string;
  date: string;
  duration?: string | null;
  start?: string | null;
  end?: string | null;
  task?: string | null;
  notes?: string | null;
  billable?: boolean;
}

export interface TimerView {
  projectId: string;
  projectCode: string;
  projectName: string;
  startedAt: string;
  elapsedSeconds: number;
  elapsed: string;
  task: string;
  notes: string;
  billable: boolean;
}

export interface TimesheetBrief {
  id: string;
  status: TimesheetStatus;
  decisionNote: string | null;
  version: number;
}

export interface MyProjectWeek {
  projectId: string;
  code: string;
  name: string;
  kind: ProjectKind;
  fundingAgency: string | null;
  billableDefault: boolean;
  daySeconds: number[];
  seconds: number;
  timesheet: TimesheetBrief | null;
}

export interface MyTimeWeek {
  institutionId: string;
  staffId: string;
  today: string;
  weekStart: string;
  days: string[];
  settings: TimeSettings;
  projects: MyProjectWeek[];
  entries: TimeEntry[];
  timer: TimerView | null;
  weekSeconds: number;
  weekTotal: string;
}

export interface Timesheet {
  id: string;
  staffId: string;
  staffName: string;
  projectId: string;
  projectCode: string;
  projectName: string;
  weekStart: string;
  weekEnd: string;
  status: TimesheetStatus;
  totalSeconds: number;
  billedSeconds: number;
  submittedAt: string | null;
  decidedAt: string | null;
  decidedBy: string | null;
  decisionNote: string | null;
  version: number;
}

export interface StaffRate {
  staffId: string;
  name: string;
  staffNumber: string;
  rate: RateValue | null;
  periods: RatePeriod[];
  payrollHourly: number | null;
  effectiveToday: number;
  source: "staff" | "payroll" | "default" | "none";
}

export interface EffortRow {
  staffId: string;
  staffName: string;
  staffNumber: string;
  projectId: string;
  projectCode: string;
  projectName: string;
  month: string;
  seconds: number;
  hours: number;
  billedSeconds: number;
  percent: number;
  personMonths: number;
  cost: string;
}

export interface EffortProjectSummary {
  projectId: string;
  code: string;
  name: string;
  fundingAgency: string | null;
  sanctionNumber: string | null;
  sanctionedAmount: string | null;
  manpowerBudget: string | null;
  hours: number;
  personMonths: number;
  cost: string;
  utilisationPercent: number | null;
  staffCount: number;
}

export interface EffortReport {
  from: string;
  to: string;
  approvedOnly: boolean;
  standardMonthlyHours: number;
  rows: EffortRow[];
  staffTotals: { staffId: string; staffName: string; projectId: string; projectCode: string; hours: number; personMonths: number; cost: string }[];
  projects: EffortProjectSummary[];
}

export interface ImportResult {
  source: string;
  mapping: Record<string, number>;
  headers: string[];
  rows: number;
  added: number;
  skipped: number;
  invalid: { line: number; reason: string }[];
  dryRun: boolean;
  seconds: number;
}
