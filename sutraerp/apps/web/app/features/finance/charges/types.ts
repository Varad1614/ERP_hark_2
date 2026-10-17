/**
 * Slice-5A wire types (fee heads and structures, charges, instalments, disputes, adjustments,
 * financial aid, configuration). Amounts are decimal strings (never floats); statuses are the
 * snake_case wire names of `sutra_contracts::finance::status`.
 */
export interface Items<T> {
  items: T[];
}

export type FeeCategory = "tuition" | "admission" | "examination" | "hostel" | "transport" | "library" | "laboratory" | "fine" | "deposit" | "miscellaneous";

export interface FeeHead {
  id: string;
  institutionId: string;
  code: string;
  name: string;
  category: FeeCategory;
  incomeAccountId: string;
  incomeAccountCode: string;
  refundable: boolean;
  status: "draft" | "active" | "retired";
  version: number;
}

export interface LineInput {
  feeHeadId: string;
  description?: string;
  amount: string;
}

export interface FeeStructureLine {
  lineNo: number;
  feeHeadId: string;
  feeHeadCode: string;
  description: string;
  amount: string;
}

export interface FeeStructure {
  id: string;
  institutionId: string;
  code: string;
  versionNo: number;
  name: string;
  academicYearId?: string | null;
  academicTermId?: string | null;
  dueDate?: string | null;
  currency: string;
  totalAmount: string;
  status: "draft" | "in_review" | "approved" | "published" | "superseded" | "retired";
  processInstanceId?: string | null;
  chargesGenerated: number;
  lines: FeeStructureLine[];
  version: number;
}

export interface GenerationResult {
  feeStructureId: string;
  created: { studentId: string; chargeId: string; chargeNumber: string; status: string }[];
  skipped: string[];
}

export type ChargeStatus = "draft" | "pending_validation" | "validated" | "published" | "active" | "suspended" | "disputed" | "cancelled" | "waived" | "written_off" | "closed" | "archived";

export interface ChargeListItem {
  id: string;
  chargeNumber: string;
  studentAccountId: string;
  displayName: string;
  studentNumber?: string | null;
  description: string;
  currency: string;
  totalAmount: string;
  adjustmentAmount: string;
  settledAmount: string;
  outstandingAmount: string;
  dueDate?: string | null;
  sourceType: string;
  status: ChargeStatus;
  version: number;
}

export interface Instalment {
  id: string;
  sequence: number;
  dueDate: string;
  amount: string;
  status: "scheduled" | "due" | "overdue" | "paid" | "waived" | "cancelled";
  version: number;
}

export interface ChargeDetail extends ChargeListItem {
  institutionId: string;
  studentId?: string | null;
  academicTermId?: string | null;
  statusReason?: string | null;
  processInstanceId?: string | null;
  publishedAt?: string | null;
  journalEntryId?: string | null;
  lines: { lineNo: number; feeHeadId: string; feeHeadCode: string; description: string; amount: string }[];
  instalments: Instalment[];
}

export interface ChargeList {
  items: ChargeListItem[];
  nextCursor?: string | null;
}

export type ChargeCommand = "validate" | "publish" | "suspend" | "resume" | "cancel" | "close" | "archive";

export interface CreateCharge {
  studentId: string;
  description: string;
  dueDate?: string;
  sourceType?: "manual" | "fine";
  lines: LineInput[];
  publish?: boolean;
}

export interface Dispute {
  id: string;
  chargeId: string;
  chargeNumber: string;
  reason: string;
  raisedByStudent: boolean;
  status: "raised" | "under_review" | "upheld" | "rejected" | "withdrawn";
  resolution?: string | null;
  resolutionNote?: string | null;
  version: number;
  createdAt: string;
}

export type AdjustmentKind = "credit_note" | "debit_note" | "write_off" | "waiver" | "concession";

export interface Adjustment {
  id: string;
  adjustmentNumber: string;
  chargeId: string;
  chargeNumber: string;
  studentAccountId: string;
  kind: AdjustmentKind;
  amount: string;
  effect: string;
  currency: string;
  reason: string;
  origin: string;
  status: "draft" | "pending_approval" | "approved" | "posted" | "rejected" | "reversed" | "superseded";
  processInstanceId?: string | null;
  decidedBy?: string | null;
  journalEntryId?: string | null;
  reversalEntryId?: string | null;
  supersedesId?: string | null;
  supersededById?: string | null;
  version: number;
  createdAt: string;
}

export interface AidDecision {
  eligibilityId: string;
  scholarshipId: string;
  scholarshipCode: string;
  scholarshipName: string;
  sponsorType: string;
  benefitType: string;
  benefitValue?: string | null;
  studentId: string;
  status: string;
  live: boolean;
  validUntil?: string | null;
  renewalCount: number;
  allocationId?: string | null;
  allocationStatus?: string | null;
}

export interface AidDisbursement {
  id: string;
  mode: "charge_adjustment" | "cash";
  chargeId?: string | null;
  chargeNumber?: string | null;
  amount: string;
  postingDate: string;
}

export interface AidAllocation {
  id: string;
  allocationNumber: string;
  scholarshipEligibilityId: string;
  scholarshipCode: string;
  scholarshipName: string;
  studentId: string;
  studentAccountId: string;
  displayName: string;
  benefitType: string;
  benefitValue?: string | null;
  allocatedAmount?: string | null;
  appliedAmount: string;
  disbursedAmount: string;
  recoveredAmount: string;
  remainingAmount?: string | null;
  validUntil?: string | null;
  renewalCount: number;
  status: "proposed" | "approved" | "allocated" | "disbursed" | "suspended" | "revoked" | "recovered" | "closed";
  disbursements: AidDisbursement[];
  recoveryChargeId?: string | null;
  version: number;
}

export interface AidReconciliation {
  id: string;
  allocationCount: number;
  allocatedTotal: string;
  utilisedTotal: string;
  recoveredTotal: string;
  ledgerTotal: string;
  variance: string;
  status: string;
  exceptions: { allocation?: string; issue: string }[];
  createdAt: string;
}

export interface PolicyVersion {
  id: string;
  institutionId: string;
  ruleKey?: string | null;
  versionNo: number;
  values: Record<string, unknown>;
  changeNote?: string | null;
  status: "draft" | "in_review" | "effective" | "superseded" | "rolled_back" | "retired";
  basedOnId?: string | null;
  rollbackReason?: string | null;
  approvedAt?: string | null;
  effectiveFrom?: string | null;
  version: number;
}

export type PolicyKind = "config" | "rule";

// ERP_EXTENSIONS 1E — periodic billing runs.
export type BillingFrequency = "weekly" | "monthly" | "quarterly" | "half_yearly" | "annual";

export interface BillingPlan {
  id: string;
  institutionId: string;
  code: string;
  name: string;
  description?: string | null;
  feeHeadId: string;
  feeHeadCode: string;
  amount: string;
  frequency: BillingFrequency;
  proration: "none" | "daily";
  status: "active" | "retired";
  enrolled: number;
  version: number;
}

export interface BillingEnrolment {
  id: string;
  planId: string;
  studentId: string;
  studentName: string;
  studentNumber?: string | null;
  amount?: string | null;
  startDate?: string | null;
  endDate?: string | null;
  onHold: boolean;
}

export interface BillingRunRequest {
  frequency: BillingFrequency;
  day?: string;
  dueDays?: number;
  weekStart?: number;
  excludeStudentIds?: string[];
}

export interface BillingLine {
  planId: string;
  description: string;
  amount: string;
  proratedDays?: number | null;
  periodDays?: number | null;
}

export interface BillingRunItem {
  studentId: string;
  studentName: string;
  studentNumber?: string | null;
  dueDate: string;
  total: string;
  lines: BillingLine[];
  chargeId?: string | null;
  chargeNumber?: string | null;
  error?: string | null;
}

export interface BillingRun {
  id?: string | null;
  preview: boolean;
  frequency: BillingFrequency;
  periodKey: string;
  periodLabel: string;
  periodStart: string;
  periodEnd: string;
  runDate: string;
  dueDays: number;
  trigger: "manual" | "schedule";
  billed: number;
  skipped: number;
  failed: number;
  total: string;
  items: BillingRunItem[];
  skips: { studentId: string; studentName: string; reason: string }[];
}

export interface BillingRunSummary {
  id: string;
  frequency: BillingFrequency;
  periodKey: string;
  periodLabel: string;
  runDate: string;
  trigger: "manual" | "schedule";
  billed: number;
  skipped: number;
  failed: number;
  total: string;
  createdBy?: string | null;
  createdAt: string;
}

export interface BillingSchedule {
  frequency: BillingFrequency;
  enabled: boolean;
  runDay: number;
  dueDays: number;
  lastPeriodKey?: string | null;
  lastRunId?: string | null;
  currentPeriodLabel: string;
}
