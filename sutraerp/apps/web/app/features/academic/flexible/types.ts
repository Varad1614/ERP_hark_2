/** Slice-4C wire types (flexible / external learning, credit recognition, course resources). */

export interface Items<T> {
  items: T[];
}

export interface HistoryEntry {
  fromStatus: string | null;
  toStatus: string;
  reason: string | null;
  actor: string | null;
  createdAt: string;
}

export type LearningMode = "open_elective" | "mooc" | "swayam" | "nptel" | "internship" | "external_course";

export interface LearningProvider {
  id: string;
  institutionId: string;
  code: string;
  name: string;
  providerKind: string;
  accreditationRef: string | null;
  accreditationValidUntil: string | null;
  modes: LearningMode[];
  status: "proposed" | "under_review" | "active" | "suspended" | "offboarded";
  statusReason: string | null;
  version: number;
  history?: HistoryEntry[];
}

export interface CreateProvider {
  code: string;
  name: string;
  providerKind: string;
  accreditationRef?: string | null;
  accreditationValidUntil?: string | null;
  modes: LearningMode[];
}

export interface LearningOpportunity {
  id: string;
  institutionId: string;
  code: string;
  title: string;
  mode: LearningMode;
  providerId: string | null;
  offeringId: string | null;
  academicTermId: string;
  credits: number;
  minHours: number | null;
  capacity: number;
  reservedSeats: number;
  waitlistEnabled: boolean;
  requiresApproval: boolean;
  enrolmentOpensOn: string | null;
  enrolmentClosesOn: string | null;
  status: "draft" | "pending_approval" | "active" | "suspended" | "archived";
  statusReason: string | null;
  seatsAllocated: number;
  waitlistCount: number;
  overAllocated: boolean;
  providerName: string | null;
  offeringCode: string | null;
  version: number;
}

export interface CreateOpportunity {
  code: string;
  title: string;
  mode: LearningMode;
  providerId?: string | null;
  offeringId?: string | null;
  academicTermId: string;
  credits?: number | null;
  minHours?: number | null;
  capacity: number;
  reservedSeats?: number;
  waitlistEnabled?: boolean;
  requiresApproval?: boolean;
  enrolmentOpensOn?: string | null;
  enrolmentClosesOn?: string | null;
}

export type EnrolmentStatus = "requested" | "eligible" | "allocated" | "waitlisted" | "registered" | "in_progress" | "completed" | "withdrawn" | "rejected";

export interface ExternalRegistration {
  id: string;
  status: "requested" | "accepted" | "rejected" | "recovery_required";
  providerReference: string | null;
  outcomeReason: string | null;
}

export interface FlexibleEnrolment {
  id: string;
  institutionId: string;
  opportunityId: string;
  studentId: string;
  studentNumber: string;
  displayName: string;
  priority: number;
  status: EnrolmentStatus;
  statusReason: string | null;
  waitlistPosition: number | null;
  waitlistExpiresOn: string | null;
  hoursCompleted: number | null;
  opportunityCode: string | null;
  opportunityTitle: string | null;
  registrations?: ExternalRegistration[];
  completion?: { completedOn: string; hoursCompleted: number; assessmentStatus: string } | null;
  version: number;
}

export interface AllocationRun {
  opportunity: LearningOpportunity | null;
  allocated: string[];
  waitlisted: string[];
  unallocated: string[];
  expired: string[];
  ineligible: string[];
  released: string[];
}

export interface PolicyRules {
  maxConcurrentEnrolments: number;
  maxCreditsPerTerm: number;
  requireHumanApproval: boolean;
  waitlistValidityDays: number;
  withdrawalAllowedUntil: "allocated" | "registered" | "in_progress";
}

export interface FlexiblePolicy {
  id: string;
  name: string;
  versionNo: number;
  rules: PolicyRules;
  effectiveFrom: string;
  effectiveTo: string | null;
  status: "draft" | "in_review" | "effective" | "superseded" | "retired";
  version: number;
}

export interface ExceptionCase {
  id: string;
  category: string;
  title: string;
  ownerSubject: string;
  correctivePlan: string | null;
  resolutionEvidence: string | null;
  onHold: boolean;
  status: "open" | "plan_pending" | "plan_approved" | "resolved" | "closed";
  version: number;
}

export interface CertificateVerification {
  id: string;
  enrolmentId: string;
  studentId: string;
  certificateNumber: string;
  certificateUrl: string | null;
  issuedOn: string | null;
  status: "submitted" | "under_verification" | "accepted" | "rejected" | "flagged";
  checks: { authenticityConfirmed: boolean | null; identityMatched: boolean | null; providerAccredited: boolean | null; fraudIndicators: string[] };
  decisionReason: string | null;
  version: number;
}

export interface MappingRule {
  id: string;
  opportunityId: string;
  curriculumComponentId: string | null;
  equivalency: string;
  credits: number;
  versionNo: number;
  effectiveFrom: string;
  status: FlexiblePolicy["status"];
  version: number;
}

export interface CreditRecognition {
  id: string;
  studentId: string;
  flexibleEnrolmentId: string;
  opportunityId: string;
  credits: number;
  requestedCredits: number;
  curriculumComponentId: string | null;
  status: "requested" | "under_review" | "approved" | "posted" | "rejected";
  ledgerEntryId: string | null;
  decisionReason: string | null;
  version: number;
}

export interface ProviderRecord {
  studentNumber: string;
  providerReference?: string | null;
  completed: boolean;
  hoursCompleted?: number | null;
}

export interface ReconciliationRun {
  id: string;
  opportunityId: string;
  discrepancies: { studentNumber: string; kind: string; institutional: string | null; provider: string | null }[];
  status: "open" | "discrepancies" | "reconciled" | "escalated";
  resolution: string | null;
  exceptionCaseId: string | null;
  createdAt: string;
  version: number;
}

export type ResourceKind = "document" | "link" | "video" | "reading" | "library_item";

export interface CourseResource {
  id: string;
  offeringId: string | null;
  sectionId: string | null;
  title: string;
  kind: ResourceKind;
  url: string;
  description: string | null;
  status: "draft" | "published" | "withdrawn";
  version: number;
}

export interface LibraryItem {
  id: string;
  offeringId: string | null;
  sectionId: string | null;
  title: string;
  kind: ResourceKind;
  url: string;
  description: string | null;
  offeringCode: string;
  offeringTitle: string;
  sectionCode: string | null;
  publishedAt: string | null;
}

export interface TaughtSection {
  sectionId: string;
  offeringId: string;
  offeringCode: string;
  title: string;
  sectionCode: string;
}

export interface MyResources {
  sections: TaughtSection[];
  items: CourseResource[];
}

export interface MyFlexibleLearning {
  opportunities: LearningOpportunity[];
  enrolments: FlexibleEnrolment[];
}
