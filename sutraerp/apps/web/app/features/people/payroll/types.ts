/** Slice 7C wire types (money, days and rates are decimal strings, exactly as the API sends them). */

export interface Items<T> {
  items: T[];
}

export interface PayComponent {
  code: string;
  name: string;
  kind: string;
  basis: string;
  amount?: string | null;
  rate?: string | null;
  sequence: number;
}

export interface PayrollConfig {
  id: string;
  institutionId: string;
  versionNo: number;
  name: string;
  effectiveFrom: string;
  rounding: string;
  encashmentDivisor: number;
  status: string;
  statusReason?: string | null;
  components: PayComponent[];
  statutoryRules: Array<{ kind: string; employeeComponentCode: string }>;
  version: number;
}

export interface PayrollPeriod {
  id: string;
  institutionId: string;
  year: number;
  month: number;
  periodStart: string;
  periodEnd: string;
  status: string;
  version: number;
}

export interface PayrollException {
  staffNumber?: string | null;
  code: string;
  message: string;
}

export interface PayrollRun {
  id: string;
  number: string;
  periodId: string;
  kind: string;
  status: string;
  statusReason?: string | null;
  headcount: number;
  grossEarnings: string;
  employeeDeductions: string;
  employerContributions: string;
  netPay: string;
  exceptions: PayrollException[];
  journalEntryNumber?: string | null;
  version: number;
}

export interface PayrollResultLine {
  lineNo: number;
  code: string;
  name: string;
  kind: string;
  amount: string;
  source: string;
}

export interface PayrollResult {
  id: string;
  runId: string;
  staffNumber: string;
  displayName: string;
  status: string;
  revision: number;
  lopDays: string;
  payableDays: string;
  grossEarnings: string;
  totalDeductions: string;
  netPay: string;
  lines: PayrollResultLine[];
  version: number;
}

export interface PayslipLine {
  code: string;
  name: string;
  amount: string;
}

export interface Payslip {
  id: string;
  number: string;
  payrollRunId: string;
  staffNumber: string;
  displayName: string;
  periodStart: string;
  periodEnd: string;
  earnings: PayslipLine[];
  deductions: PayslipLine[];
  grossEarnings: string;
  totalDeductions: string;
  netPay: string;
  lopDays: string;
  status: string;
  version: number;
}

export interface PayComponentInput {
  code: string;
  name: string;
  kind: string;
  basis: string;
  amount?: string;
  rate?: string;
}

export interface SalaryStructure {
  id: string;
  institutionId: string;
  code: string;
  name: string;
  grade?: string | null;
  status: string;
  lines: Array<{ componentCode: string; amount?: string | null; rate?: string | null }>;
  version: number;
}

export interface SalaryAssignment {
  id: string;
  institutionId: string;
  staffId: string;
  engagementId: string;
  structureId: string;
  structureCode: string;
  effectiveFrom: string;
  effectiveTo?: string | null;
  status: string;
  version: number;
}

export interface FinalSettlement {
  id: string;
  number?: string | null;
  staffId?: string;
  engagementId: string;
  status: string;
  lastWorkingDate: string;
  salaryAmount: string;
  encashmentAmount: string;
  recoveryAmount: string;
  netPay: string;
  version: number;
}

export interface BulkSummary {
  processed: number;
  skipped: number;
  failed: Array<{ id: string; reason: string }>;
}

export type RunCommand = "calculate" | "validate" | "submit" | "finalise" | "post" | "close" | "reprocess";
export type Decision = "approve" | "reject";
