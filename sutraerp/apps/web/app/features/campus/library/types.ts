/**
 * Slice-8C library wire types — straight from the generated OpenAPI client. Money (fines,
 * replacement costs) stays the decimal strings the API sends.
 */
import type { components } from "@sutra/api-client";

type S = components["schemas"];

export interface Items<T> {
  items: T[];
}

export type LibraryPolicy = S["LibraryPolicyView"];
export type NewLibraryPolicy = S["LibraryPolicyCreate"];
export type Membership = S["LibraryMembershipView"];
export type NewMembership = S["LibraryMembershipCreate"];
export type CatalogueRecord = S["CatalogueRecordView"];
export type NewCatalogueRecord = S["CatalogueRecordCreate"];
export type Copy = S["LibraryCopyView"];
export type NewCopy = S["LibraryCopyCreate"];
export type Loan = S["LibraryLoanView"];
export type NewLoan = S["LibraryLoanCreate"];
export type Fine = S["LibraryFineView"];
export type LibraryReservation = S["LibraryReservationView"];
export type BulkResult = S["LibraryBulkResult"];
export type MyLibrary = S["MyLibraryView"];
export type LibraryComplaint = S["LibraryComplaintView"];
export type Clearance = S["LibraryClearanceView"];
export type Eligibility = S["LibraryEligibilityView"];
export type CopyCondition = S["LibraryCopyCondition"];

export type PolicyCommand = "submit" | "activate";
export type MembershipCommand = "suspend" | "restore" | "close";
export type LoanCommand = "return" | "renew";
export type FineCommand = "bill" | "waiver";
export type CopyCommand = "lost" | "withdraw";
export type ReservationCommand = "collect" | "cancel" | "expire";
export type Decision = "approve" | "reject";
