/** Slice 7A wire types (camelCase, dates `YYYY-MM-DD`, exactly as the API sends them). */

export interface Items<T> {
  items: T[];
}

export type Decision = "approve" | "reject";

export interface EngagementSummary {
  id: string;
  engagementNumber: string;
  designation: string;
  grade?: string | null;
  employmentType: string;
  staffCategory: string;
  startDate: string;
  endDate?: string | null;
  status: string;
}

/** Foundation staff view (`GET /staff/{id}`, list) plus the 7A record fields when present. */
export interface Staff {
  id: string;
  institutionId: string;
  staffNumber: string;
  displayName: string;
  givenName: string;
  familyName?: string | null;
  workEmail?: string | null;
  phone?: string | null;
  staffCategory: string;
  status: string;
  subjectRef?: string | null;
  canonicalStaffId?: string | null;
  verifiedAt?: string | null;
  engagement?: EngagementSummary | null;
  version: number;
}

export interface ChangeRequest {
  id: string;
  kind: "activation" | "modification" | "transfer";
  proposal: Record<string, unknown>;
  status: string;
  reason?: string | null;
  processInstanceId?: string | null;
}

export interface Engagement {
  id: string;
  staffId: string;
  staffNumber: string;
  displayName: string;
  engagementNumber: string;
  orgUnitId?: string | null;
  campusId?: string | null;
  staffCategory: string;
  employmentType: string;
  designation: string;
  grade?: string | null;
  startDate: string;
  endDate?: string | null;
  lastWorkingDate?: string | null;
  probationEndDate?: string | null;
  reportsToStaffId?: string | null;
  rejoinOfEngagementId?: string | null;
  status: string;
  pendingChange?: ChangeRequest | null;
  version: number;
}

export interface Position {
  id: string;
  code: string;
  title: string;
  staffCategory?: string | null;
  grade?: string | null;
  allowsMultiple: boolean;
  status: string;
  statusReason?: string | null;
  occupants: number;
  version: number;
}

export interface Assignment {
  id: string;
  positionId: string;
  positionCode: string;
  engagementId: string;
  engagementNumber: string;
  staffNumber: string;
  displayName: string;
  isPrimary: boolean;
  startDate: string;
  endDate?: string | null;
  endOutcome?: string | null;
  status: string;
  version: number;
}

export interface Requisition {
  id: string;
  number: string;
  title: string;
  staffCategory: string;
  employmentType: string;
  designation: string;
  openings: number;
  filled: number;
  status: string;
  statusReason?: string | null;
  processInstanceId?: string | null;
  version: number;
}

export interface Candidate {
  id: string;
  requisitionId: string;
  givenName: string;
  familyName?: string | null;
  email: string;
  status: string;
  statusReason?: string | null;
  offeredDesignation?: string | null;
  proposedJoiningDate?: string | null;
  onboardingCaseId?: string | null;
  version: number;
}

export interface OnboardingCase {
  id: string;
  number: string;
  candidateId: string;
  candidateName: string;
  joiningDate: string;
  documents: Array<{ documentType: string; reference: string }>;
  documentsVerified: boolean;
  staffId?: string | null;
  engagementId?: string | null;
  status: string;
  statusReason?: string | null;
  processInstanceId?: string | null;
  version: number;
}

export interface Qualification {
  id: string;
  qualificationType: string;
  title: string;
  awardingBody: string;
  yearOfCompletion?: number | null;
  status: string;
  version: number;
}

export interface ProfileDocument {
  id: string;
  documentType: string;
  title: string;
  status: string;
  validUntil?: string | null;
  version: number;
}

export interface CertificateRequest {
  id: string;
  kind: "service" | "experience";
  purpose: string;
  requestedVia: string;
  certificateNumber?: string | null;
  status: string;
  version: number;
}

export interface FacultyProfile {
  id: string;
  status: string;
  eligibilityStatus?: string | null;
  eligibilityValidUntil?: string | null;
  version: number;
}

export interface MyProfile {
  /** `null` when the login has no staff record (the lists are then empty). */
  staff: Staff | null;
  engagement?: EngagementSummary | null;
  faculty?: FacultyProfile | null;
  qualifications: Qualification[];
  documents: ProfileDocument[];
  certificateRequests: CertificateRequest[];
}

export interface StaffInput {
  givenName: string;
  familyName?: string;
  workEmail?: string;
  phone?: string;
  staffCategory: string;
  /** Sign-in account (login subject) the record answers for, e.g. `dev|name@college.edu`. */
  subjectRef?: string;
}

export interface EngagementInput {
  employmentType: string;
  designation: string;
  grade?: string;
  startDate: string;
  probationEndDate?: string;
}

export type StaffCommand = "submit-verification" | "verify" | "activate" | "suspend" | "restore" | "archive";
export type EngagementCommand = "submit" | "activate" | "suspend" | "restore" | "cancel";
export type PositionCommand = "activate" | "freeze" | "retire";
export type CandidateCommand = "shortlist" | "interview" | "accept" | "decline" | "reject";
