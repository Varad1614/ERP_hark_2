/**
 * Slice 3C wire types (camelCase JSON of the `completion`, `guardians`, `scholarships` and
 * `alumni` API tags). Only the members the screens read are listed.
 */

export interface Items<T> {
  items: T[];
}

export interface Clearance {
  id: string;
  item: "finance" | "library" | "hostel" | "department" | "laboratory" | "sports" | "other";
  status: "pending" | "cleared" | "waived" | "blocked";
  note?: string | null;
  version: number;
}

export type CompletionStatus =
  | "eligibility_evaluation"
  | "degree_audit"
  | "institutional_clearance"
  | "approval_pending"
  | "approved"
  | "awarded"
  | "record_finalized"
  | "exit_completed"
  | "deferred"
  | "cancelled";

export interface QualificationCompletion {
  id: string;
  studentId: string;
  outcome: string;
  pathway: "programme_completion" | "meme_exit";
  status: CompletionStatus;
  eligibilityResult?: { eligible: boolean; unmet: string[] } | null;
  conferredQualificationId?: string | null;
  conferredOn?: string | null;
  clearances: Clearance[];
  version: number;
  createdAt: string;
}

export interface ConferredQualification {
  id: string;
  title: string;
  outcome: string;
  conferredOn: string;
  status: "conferred" | "corrected" | "revoked";
  version: number;
}

export type ExitKind = "completion" | "meme_exit" | "withdrawal" | "transfer" | "discontinuation" | "termination" | "administrative_closure" | "deceased";

export interface StudentExit {
  id: string;
  studentId: string;
  kind: ExitKind;
  status: "initiated" | "clearance_pending" | "approval_pending" | "approved" | "completed" | "rejected" | "cancelled";
  reason: string;
  effectiveOn?: string | null;
  clearances: Clearance[];
  version: number;
  createdAt: string;
}

export interface NewExit {
  kind: ExitKind;
  reason: string;
  destinationInstitution?: string;
  evidenceRef?: string;
}

export interface Guardian {
  id: string;
  givenName: string;
  familyName?: string | null;
  email?: string | null;
  phone?: string | null;
  version: number;
}

export interface GuardianRelationship {
  id: string;
  guardianId: string;
  studentId: string;
  relationshipType: string;
  isPrimary: boolean;
  hasLegalCustody: boolean;
  status: "proposed" | "pending_verification" | "verified" | "active" | "suspended" | "terminated" | "historical";
  guardian?: Guardian | null;
  version: number;
}

export interface EmergencyContact {
  id: string;
  name: string;
  relation: string;
  phone: string;
  priority: number;
  status: "active" | "suspended" | "revoked" | "expired";
  version: number;
}

export interface PickupAuthorisation {
  id: string;
  personName: string;
  validUntil?: string | null;
  status: "active" | "suspended" | "revoked" | "expired";
  version: number;
}

export interface Scholarship {
  id: string;
  code: string;
  name: string;
  sponsorType: string;
  benefitType: string;
  status: "draft" | "published" | "active" | "suspended" | "expired" | "archived";
  renewable: boolean;
  effectivePolicy?: { id: string; versionNo: number } | null;
  version: number;
}

export interface ScholarshipPolicy {
  id: string;
  versionNo: number;
  status: "draft" | "in_review" | "effective" | "superseded" | "retired";
  version: number;
}

export interface ScholarshipEligibility {
  id: string;
  scholarshipId: string;
  studentId: string;
  status:
    | "created"
    | "assessment_pending"
    | "under_evaluation"
    | "eligible"
    | "not_eligible"
    | "conditionally_eligible"
    | "recommendation_generated"
    | "active"
    | "reassessment"
    | "renewal"
    | "expired"
    | "archived";
  conditions: string[];
  validUntil?: string | null;
  recommendationPending: boolean;
  version: number;
}

export interface Recalculation {
  id: string;
  evaluated: number;
  changed: number;
}

export interface AlumniTransition {
  id: string;
  status: "created" | "pending_verification" | "verified" | "pending_approval" | "approved" | "rejected" | "transitioning" | "activated" | "completed" | "archived";
  basis?: string | null;
  eligibilityResult?: { eligible: boolean; unmet: string[] } | null;
  version: number;
}
