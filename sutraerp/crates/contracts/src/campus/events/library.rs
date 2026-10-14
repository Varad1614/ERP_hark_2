//! **8C** — library memberships, catalogue, copies, circulation, reservations, fines, clearance (L2-G05;
//! L3-G-124…139, 149, 152).
//!
//! Owned by slice 8C (`docs/batch8-contracts.md` §4.3). Payload: [`super::StatusChangedV1`] unless noted.

use super::event_type;

event_type!(
    /// Membership active (L3-G-124).
    LIBRARY_MEMBERSHIP_ACTIVATED, LIBRARY_MEMBERSHIP_ACTIVATED_SCHEMA, "library_membership.activated");
event_type!(
    /// Membership suspended (L3-G-125).
    LIBRARY_MEMBERSHIP_SUSPENDED, LIBRARY_MEMBERSHIP_SUSPENDED_SCHEMA, "library_membership.suspended");
event_type!(
    /// Catalogue record published (L3-G-127).
    CATALOGUE_RECORD_PUBLISHED, CATALOGUE_RECORD_PUBLISHED_SCHEMA, "catalogue_record.published");
event_type!(
    /// Copy issued — `ResourceIssued` (L3-G-131).
    LIBRARY_LOAN_ISSUED, LIBRARY_LOAN_ISSUED_SCHEMA, "library_loan.issued");
event_type!(
    /// Copy returned (L3-G-132).
    LIBRARY_LOAN_RETURNED, LIBRARY_LOAN_RETURNED_SCHEMA, "library_loan.returned");
event_type!(
    /// Loan overdue (L3-G-137).
    LIBRARY_LOAN_OVERDUE, LIBRARY_LOAN_OVERDUE_SCHEMA, "library_loan.overdue");
event_type!(
    /// Reserved copy ready for collection (L3-G-135).
    LIBRARY_RESERVATION_READY, LIBRARY_RESERVATION_READY_SCHEMA, "library_reservation.ready");
event_type!(
    /// Fine billed to the student through Finance (L3-G-137 / 139).
    LIBRARY_FINE_RAISED, LIBRARY_FINE_RAISED_SCHEMA, "library_fine.raised");
event_type!(
    /// Fine waived (L3-G-138).
    LIBRARY_FINE_WAIVED, LIBRARY_FINE_WAIVED_SCHEMA, "library_fine.waived");
event_type!(
    /// Clearance decided (L3-G-149).
    LIBRARY_CLEARANCE_DECIDED, LIBRARY_CLEARANCE_DECIDED_SCHEMA, "library_clearance.decided");

// 8C additions (slice-owned file; listed in the 8C PR).
event_type!(
    /// Borrowing policy version activated (supersedes the previous one).
    LIBRARY_POLICY_ACTIVATED, LIBRARY_POLICY_ACTIVATED_SCHEMA, "library_policy.activated");
event_type!(
    /// Loan renewed — `ResourceRenewed` (L3-G-133).
    LIBRARY_LOAN_RENEWED, LIBRARY_LOAN_RENEWED_SCHEMA, "library_loan.renewed");
event_type!(
    /// Clearance requested — `ClearanceRequested` (L3-G-149).
    LIBRARY_CLEARANCE_REQUESTED, LIBRARY_CLEARANCE_REQUESTED_SCHEMA, "library_clearance.requested");

/// Event types of this file (keep in sync; `all_event_types` collects them).
pub const EVENT_TYPES: &[&str] = &[
    LIBRARY_MEMBERSHIP_ACTIVATED,
    LIBRARY_MEMBERSHIP_SUSPENDED,
    CATALOGUE_RECORD_PUBLISHED,
    LIBRARY_LOAN_ISSUED,
    LIBRARY_LOAN_RETURNED,
    LIBRARY_LOAN_OVERDUE,
    LIBRARY_RESERVATION_READY,
    LIBRARY_FINE_RAISED,
    LIBRARY_FINE_WAIVED,
    LIBRARY_CLEARANCE_DECIDED,
    LIBRARY_POLICY_ACTIVATED,
    LIBRARY_LOAN_RENEWED,
    LIBRARY_CLEARANCE_REQUESTED,
];
