/**
 * ERP_EXTENSIONS 5A: offer letters, onboarding checklists, follow-ups / attention, hiring
 * analytics and the careers page. Local mirrors of the people-hr schemas in
 * `modules/people/src/hr/{offers_http,careers_http,hiring_insights_http,offers,hiring_analytics}.rs`
 * (serde camelCase; `Decimal` fields travel as strings, dates as `YYYY-MM-DD`, instants as ISO).
 * schema.d.ts does not carry these yet, so they are kept here.
 */

/** Decimal rupee amount as sent by the API (`"45000"`, `"1800.50"`). */
export type Money = string;
/** `YYYY-MM-DD`. */
export type IsoDate = string;
/** ISO-8601 instant (UTC). */
export type IsoDateTime = string;

// ------------------------------------------------------------------ offers

export type OfferStatus = "draft" | "pending_approval" | "approved" | "sent" | "accepted" | "declined" | "expired" | "withdrawn";
export type OfferDecision = "approve" | "reject";
export type TaskOwner = "hr" | "it" | "department" | "accounts" | "admin";

/** `HiringOfferCtc`: monthly cost-to-institution breakup (₹). */
export interface OfferCtc {
  basic: Money;
  hra: Money;
  da: Money;
  allowances: Money;
  employerPf: Money;
}

/** `HiringOfferView`. */
export interface OfferView {
  id: string;
  institutionId: string;
  candidateId: string;
  candidateName: string;
  requisitionId: string;
  requisitionNumber: string;
  /** `OFR-000001`. */
  number: string;
  designation: string;
  grade: string | null;
  department: string | null;
  joiningDate: IsoDate;
  expiresOn: IsoDate | null;
  probationMonths: number | null;
  ctc: OfferCtc;
  ctcMonthly: Money;
  ctcAnnual: Money;
  joiningBonus: Money | null;
  notes: string | null;
  status: OfferStatus;
  statusReason: string | null;
  draftedBy: string | null;
  submittedBy: string | null;
  decidedBy: string | null;
  decidedAt: IsoDateTime | null;
  sentAt: IsoDateTime | null;
  acceptedByName: string | null;
  resolvedAt: IsoDateTime | null;
  version: number;
}

/** `HiringOfferList`. */
export interface OfferList { items: OfferView[] }

/** `CtcInput`: numbers or decimal strings; a missing `employerPf` is the statutory suggestion. */
export interface OfferCtcInput {
  basic: Money | number;
  hra?: Money | number | null;
  da?: Money | number | null;
  allowances?: Money | number | null;
  employerPf?: Money | number | null;
}

/** `HiringOfferInput` (create and edit a draft). */
export interface OfferInput {
  designation?: string | null;
  grade?: string | null;
  department?: string | null;
  joiningDate: IsoDate;
  expiresOn?: IsoDate | null;
  probationMonths?: number | null;
  ctc: OfferCtcInput;
  joiningBonus?: Money | number | null;
  notes?: string | null;
}

/** `DecisionBody`. */
export interface OfferDecisionBody { decision: OfferDecision; reason?: string | null }
/** `ReasonBody` (decline / withdraw). */
export interface ReasonBody { reason?: string | null }
/** `HiringOfferAcceptBody`. */
export interface OfferAcceptBody { acceptedByName?: string | null }

/** `HiringOfferLetter`. */
export interface OfferLetter {
  offerId: string;
  number: string;
  /** `true` when this is the text snapshotted at sending. */
  snapshot: boolean;
  text: string;
  html: string;
}

// ------------------------------------------------------------------ settings

/** `HiringOnboardingTemplateTask`. */
export interface OnboardingTemplateTask {
  title: string;
  category: string;
  /** Days from the joining date (negative = before joining). */
  offsetDays?: number;
  owner?: TaskOwner | string;
}

/** `HiringOnboardingTemplate`. */
export interface OnboardingTemplate {
  id: string;
  name: string;
  tasks: OnboardingTemplateTask[];
  jobIds: string[];
  departments: string[];
}

/** `HiringOfferSettings` (the `default*` and `templateVariables` fields are read-only). */
export interface OfferSettings {
  offerTemplate: string | null;
  defaultOfferTemplate: string;
  templateVariables: string[];
  onboardingTemplates: OnboardingTemplate[];
  defaultOnboardingTemplate: OnboardingTemplate;
  staleAfterDays: number;
  retentionMonths: number;
}

// ------------------------------------------------------------------ onboarding checklist

/** `HiringChecklistTask`. */
export interface ChecklistTask {
  id: string;
  title: string;
  category: string;
  owner: TaskOwner | string | null;
  offsetDays: number | null;
  /** "7 days before the joining date". */
  offsetLabel: string;
  dueDate: IsoDate | null;
  overdue: boolean;
  done: boolean;
  doneAt: IsoDateTime | null;
  doneBy: string | null;
}

/** `HiringChecklistGroup`. */
export interface ChecklistGroup { category: string; tasks: ChecklistTask[] }
/** `HiringChecklistProgress`. */
export interface ChecklistProgress { done: number; total: number; pct: number }

/** `HiringChecklist`. */
export interface Checklist {
  onboardingCaseId: string;
  candidateName: string;
  joiningDate: IsoDate;
  status: string;
  progress: ChecklistProgress;
  overdue: number;
  groups: ChecklistGroup[];
}

/** `HiringChecklistTaskInput`. */
export interface ChecklistTaskInput {
  title: string;
  category?: string | null;
  owner?: TaskOwner | string | null;
  offsetDays?: number | null;
}

/** `HiringChecklistDoneBody`. */
export interface ChecklistDoneBody { done: boolean }

/** `HiringDueTask`. */
export interface DueTask {
  taskId: string;
  onboardingCaseId: string;
  caseNumber: string;
  candidateName: string;
  title: string;
  owner: string | null;
  dueDate: IsoDate;
  overdue: boolean;
  daysLeft: number;
}
/** `HiringDueTaskList`. */
export interface DueTaskList { items: DueTask[] }

// ------------------------------------------------------------------ follow-ups / attention

/** `HiringFollowupView`. */
export interface FollowupView {
  id: string;
  candidateId: string;
  text: string;
  dueDate: IsoDate;
  dueTime: string | null;
  done: boolean;
  due: boolean;
  overdue: boolean;
}
/** `HiringFollowupList`. */
export interface FollowupList { items: FollowupView[] }
/** `HiringFollowupInput`. */
export interface FollowupInput { text: string; dueDate: IsoDate; dueTime?: string | null }
/** `HiringFollowupDoneBody`. */
export interface FollowupDoneBody { done: boolean }

export type ReminderKind = "offer" | "followup" | "onboarding";

/** `Reminder` (hiring_analytics). */
export interface Reminder { key: string; kind: ReminderKind | string; title: string; body: string; candidateId: string }
/** `HiringStaleRow`. */
export interface StaleRow { candidateId: string; name: string; requisitionNumber: string; stage: string; stageLabel: string; days: number }
/** `HiringRetentionRow`. */
export interface RetentionRow { candidateId: string; name: string; requisitionNumber: string; archivedOn: IsoDate }
/** `HiringRecordedReminder`. */
export interface RecordedReminder { key: string; kind: string; candidateId: string | null; title: string; body: string; createdAt: IsoDateTime }

/** `HiringAttention` (`asOf` is the institution's local time, no zone). */
export interface HiringAttention {
  asOf: string;
  staleAfterDays: number;
  retentionMonths: number;
  reminders: Reminder[];
  stale: StaleRow[];
  retentionDue: RetentionRow[];
  recorded: RecordedReminder[];
}

// ------------------------------------------------------------------ analytics

/** `HiringFunnelStage`. */
export interface FunnelStage { stage: string; label: string; count: number }
/** `HiringStageConversion`. */
export interface StageConversion { stage: string; next: string; reached: number; advanced: number; rate: number | null }
/** `HiringTimeToHire`. */
export interface TimeToHire { count: number; median: number | null }
/** `HiringStageTime`. */
export interface StageTime { stage: string; average: number | null }
/** `HiringSourceRow`. */
export interface SourceRow { source: string; total: number; hired: number }
/** `HiringOfferOutcomes`. */
export interface OfferOutcomes { sent: number; accepted: number; declined: number; acceptance: number | null }
/** `HiringOfferFigures` (outcomes flattened in). */
export interface OfferFigures extends OfferOutcomes { expired: number; withdrawn: number; inPlay: number }

/** `HiringRequisitionFigures`. */
export interface RequisitionFigures {
  requisitionId: string;
  requisitionNumber: string;
  candidates: number;
  active: number;
  hired: number;
  timeToHire: TimeToHire;
  offers: OfferOutcomes;
}

/** `HiringAnalytics`. */
export interface HiringAnalytics {
  asOf: string;
  sinceDays: number | null;
  candidates: number;
  funnel: FunnelStage[];
  conversion: StageConversion[];
  timeToHire: TimeToHire;
  averageDaysInStage: StageTime[];
  sources: SourceRow[];
  offers: OfferFigures;
  requisitions: RequisitionFigures[];
}

/** `HiringAnalyticsQuery`. */
export interface HiringAnalyticsQuery { requisitionId?: string | null; sinceDays?: number | null }

// ------------------------------------------------------------------ careers page

/** `HiringCareersListing`. */
export interface CareersListing {
  requisitionId: string;
  number: string;
  title: string;
  status: string;
  published: boolean;
  description: string | null;
  location: string | null;
  closesOn: IsoDate | null;
  applications: number;
}

/** `HiringCareersPage`. */
export interface CareersPage {
  enabled: boolean;
  slug: string | null;
  suggestedSlug: string;
  publicPath: string | null;
  intro: string | null;
  contactEmail: string | null;
  listings: CareersListing[];
}

/** `HiringCareersPageInput`. */
export interface CareersPageInput { enabled: boolean; slug?: string | null; intro?: string | null; contactEmail?: string | null }
/** `HiringCareersListingInput`. */
export interface CareersListingInput { published: boolean; description?: string | null; location?: string | null; closesOn?: IsoDate | null }

/** `HiringPublicOpening`. */
export interface PublicOpening {
  id: string;
  title: string;
  designation: string;
  staffCategory: string;
  employmentType: string;
  openings: number;
  location: string | null;
  closesOn: IsoDate | null;
  descriptionHtml: string;
}
/** `HiringPublicCareers`. */
export interface PublicCareers { institution: string; introHtml: string; contactEmail: string | null; openings: PublicOpening[] }
/** `HiringCareersApplication` (`website` is the honeypot: leave empty). */
export interface CareersApplication { name: string; email: string; phone?: string | null; link?: string | null; note?: string | null; website?: string | null }
/** `HiringCareersReceipt`. */
export interface CareersReceipt { received: boolean }
