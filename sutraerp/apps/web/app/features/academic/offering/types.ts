/** Slice-4A wire types (generated OpenAPI schemas, `@sutra/api-client`). */
import type { components } from "@sutra/api-client";

type S = components["schemas"];

export type Offering = S["Offering"];
export type OfferingSection = S["OfferingSection"];
export type FacultyBrief = S["FacultyBrief"];
export type CreateOffering = S["CreateOffering"];
export type NewOfferingSection = S["NewOfferingSection"];
export type OfferingGovernedOutcome = S["OfferingGovernedOutcome"];
export type OfferingReadiness = S["OfferingReadiness"];
export type SectionEnrolment = S["SectionEnrolment"];
export type OfferingWaitlistEntry = S["OfferingWaitlistEntry"];
export type FacultyAssignment = S["FacultyAssignment"];
export type NewFacultyAssignment = S["NewFacultyAssignment"];
export type FacultyAssignmentPolicy = S["FacultyAssignmentPolicy"];
export type FacultyPolicyDraft = S["FacultyPolicyDraft"];
export type MyCourse = S["MyCourse"];
export type AvailableCourse = S["AvailableCourse"];
export type MyClass = S["MyClass"];
export type MyStudent = S["MyStudent"];
export type CourseOfferingStatus = S["CourseOfferingStatus"];
export type FacultyRole = S["FacultyRole"];

export interface Items<T> {
  items: T[];
}

/** Offering state commands with a literal path each. */
export type OfferingCommand = "submit" | "publish" | "open-enrolment" | "close-enrolment" | "activate" | "suspend" | "reactivate" | "freeze" | "complete" | "archive";
/** Governed commands (WorkflowPort request). */
export type GovernedCommand = "cancel" | "unfreeze";
export type AssignmentCommand = "submit" | "activate" | "revoke" | "complete" | "withdraw";
export type Decision = "approve" | "reject";
