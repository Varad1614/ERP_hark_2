//! Lifecycle state and classification enums of the Batch 7 (Domain E, People & workforce)
//! aggregates.
//!
//! Foundation-owned. Wire/DB form is the `snake_case` name, stored in a `text` column with a
//! `CHECK (… IN (...))` written by the owning migration. Adding a variant is a contract change
//! (edit `docs/batch7-contracts.md` §4.2). Names follow the L2-E state models (L2-E01 … L2-E07),
//! so the L3 scenarios map onto them without inventing states.
//!
//! Transitions: the **shared** machines live in foundation tables that several slices read and
//! gate on, so their legal transitions are written here (`can_transition`) and only the owning
//! slice moves them through `sutra_people::shared::lifecycle`:
//!
//! | Machine | Table | Moved by |
//! |---------|-------|----------|
//! | [`StaffStatus`] | `people.staff_member` | 7A |
//! | [`EngagementStatus`] | `people.staff_engagement` | 7A (every move), 7B (`active ↔ long_leave` only) |
//! | [`LeaveEntryKind`] | `people.leave_ledger_entry` (append-only) | 7B through `shared::leave` |
//! | [`AttendanceDayStatus`] | `people.attendance_day` | 7B through `shared::attendance` |
//!
//! Every other machine's transitions are slice-owned business rules written as a pure,
//! unit-tested `can_transition(from, to)` in the owning slice's `domain.rs`; an illegal
//! transition is `409 conflict`.

/// Declares a status enum with `ALL`, `as_str`, `parse` and serde/OpenAPI `snake_case` names.
macro_rules! status_enum {
    ($(#[$meta:meta])* $name:ident { $($(#[$vmeta:meta])* $variant:ident => $wire:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(
            Clone, Copy, Debug, PartialEq, Eq, Hash,
            serde::Serialize, serde::Deserialize, utoipa::ToSchema,
        )]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($(#[$vmeta])* $variant),+ }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            /// Stored / wire name.
            pub fn as_str(&self) -> &'static str {
                match self { $($name::$variant => $wire),+ }
            }

            /// Strict parse of the stored name.
            pub fn parse(s: &str) -> Option<Self> {
                Self::ALL.iter().copied().find(|v| v.as_str() == s)
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

// ================================================================ foundation: shared machines

status_enum!(
    /// Staff record / employee profile lifecycle (L2-E07 §10; duplicate resolution L3-E-009).
    StaffStatus {
        Draft => "draft",
        PendingVerification => "pending_verification",
        /// Identity verified, not yet operational.
        Verified => "verified",
        /// Operational profile.
        Active => "active",
        /// Temporarily restricted.
        Suspended => "suspended",
        /// Duplicate collapsed onto `canonical_staff_id` (L3-E-009).
        Merged => "merged",
        /// Historical profile (separation, L3-E-068).
        Archived => "archived",
    }
);

impl StaffStatus {
    /// Not merged / archived: the record may change and resolve logins.
    pub fn is_live(&self) -> bool {
        !matches!(self, Self::Merged | Self::Archived)
    }

    /// Legal transitions (L2-E07 §10).
    pub fn can_transition(&self, to: Self) -> bool {
        use StaffStatus::*;
        if to == Merged {
            return self.is_live();
        }
        matches!(
            (self, to),
            (Draft, PendingVerification)
                | (Draft, Active)
                | (PendingVerification, Draft)
                | (PendingVerification, Verified)
                | (Verified, Active)
                | (Active, Suspended)
                | (Suspended, Active)
                | (Active, Archived)
                | (Suspended, Archived)
                | (Archived, Active)
        )
    }
}

status_enum!(
    /// Workforce engagement lifecycle (L2-E01: Pending → Active ↔ Suspended / Deputed / LongLeave → Closed).
    EngagementStatus {
        /// Created (onboarding / HR), awaiting activation (`ENGAGEMENT_ACTIVATION`).
        Pending => "pending",
        /// Working: the state Domain D sees as an active staff member.
        Active => "active",
        Suspended => "suspended",
        /// On deputation (7D; the state exists so the machine matches L2-E01).
        Deputed => "deputed",
        /// Long leave (7B, L3-E-006).
        LongLeave => "long_leave",
        /// Exited (L3-E-007, separation L3-E-041).
        Closed => "closed",
        /// Never started.
        Cancelled => "cancelled",
    }
);

impl EngagementStatus {
    /// Not closed / cancelled (one live engagement per staff member).
    pub fn is_live(&self) -> bool {
        !matches!(self, Self::Closed | Self::Cancelled)
    }

    /// The staff member is working (teaching assignments may start, leave may be requested).
    pub fn is_working(&self) -> bool {
        matches!(self, Self::Active)
    }

    /// Included in a payroll run of a period it overlaps.
    pub fn is_payable(&self) -> bool {
        matches!(
            self,
            Self::Active | Self::Suspended | Self::Deputed | Self::LongLeave
        )
    }

    /// Legal transitions (L2-E01).
    pub fn can_transition(&self, to: Self) -> bool {
        use EngagementStatus::*;
        matches!(
            (self, to),
            (Pending, Active)
                | (Pending, Cancelled)
                | (Active, Suspended)
                | (Suspended, Active)
                | (Active, Deputed)
                | (Deputed, Active)
                | (Active, LongLeave)
                | (LongLeave, Active)
                | (Active, Closed)
                | (Suspended, Closed)
                | (Deputed, Closed)
                | (LongLeave, Closed)
        )
    }
}

status_enum!(
    /// Kind of an append-only leave ledger transaction (L2-E05 §6.6).
    LeaveEntryKind {
        /// Opening entitlement of a leave year.
        Opening => "opening",
        /// Periodic accrual (L3-E-046).
        Accrual => "accrual",
        /// Carried forward from the previous year (L3-E-050).
        CarryForward => "carry_forward",
        /// Governed adjustment (L3-E-049), either sign.
        Adjustment => "adjustment",
        /// Approved leave held against the balance (L3-E-047).
        Reservation => "reservation",
        /// Reservation given back (consumption or cancellation, L3-E-048).
        Release => "release",
        /// Leave actually taken (one entry per calendar-month segment).
        Consumption => "consumption",
        /// Lapsed at year end (L3-E-050).
        Expiry => "expiry",
        /// Days encashed (L3-E-051); paid by 7C payroll.
        Encashment => "encashment",
        /// Mirror of an earlier entry (`reverses_entry_id`), opposite sign.
        Reversal => "reversal",
    }
);

/// Required sign of a ledger entry's `days`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntrySign {
    Positive,
    Negative,
    Either,
}

impl LeaveEntryKind {
    pub fn sign(&self) -> EntrySign {
        use LeaveEntryKind::*;
        match self {
            Opening | Accrual | CarryForward | Release => EntrySign::Positive,
            Reservation | Consumption | Expiry | Encashment => EntrySign::Negative,
            Adjustment | Reversal => EntrySign::Either,
        }
    }

    /// Counts towards the entitlement (opening, accrual, carry forward, adjustment).
    pub fn is_entitlement(&self) -> bool {
        matches!(
            self,
            Self::Opening | Self::Accrual | Self::CarryForward | Self::Adjustment
        )
    }
}

status_enum!(
    /// Attendance of a staff member on one work date.
    AttendanceDayStatus {
        Present => "present",
        Absent => "absent",
        HalfDay => "half_day",
        /// Covered by approved leave (7B marks it).
        OnLeave => "on_leave",
        Holiday => "holiday",
        WeeklyOff => "weekly_off",
    }
);

impl AttendanceDayStatus {
    /// Unauthorised absence weight for payroll (absent 1, half day ½, else 0) in hundredths.
    pub fn absent_hundredths(&self) -> i64 {
        match self {
            Self::Absent => 100,
            Self::HalfDay => 50,
            _ => 0,
        }
    }
}

// ================================================================ foundation: classifications

status_enum!(
    /// Workforce category (L2-E01).
    StaffCategory {
        /// Faculty (teaching staff; Domain D faculty assignments).
        Teaching => "teaching",
        NonTeaching => "non_teaching",
        Administrative => "administrative",
    }
);

status_enum!(
    /// Employment type of an engagement.
    EmploymentType {
        Permanent => "permanent",
        Probation => "probation",
        Contract => "contract",
        Visiting => "visiting",
        Temporary => "temporary",
    }
);

status_enum!(
    /// Gender as recorded on the profile (optional).
    Gender {
        Female => "female",
        Male => "male",
        Other => "other",
        Undisclosed => "undisclosed",
    }
);

status_enum!(
    /// Origin of an attendance day record.
    AttendanceSource {
        Manual => "manual",
        Bulk => "bulk",
        /// Marked `on_leave` by 7B's leave flow.
        Leave => "leave",
        Correction => "correction",
    }
);

// ================================================================ slice enums (transitions slice-owned)

status_enum!(
    /// 7A: institutional position (L2-E02).
    PositionStatus {
        Draft => "draft",
        Active => "active",
        Frozen => "frozen",
        Retired => "retired",
        Archived => "archived",
    }
);

status_enum!(
    /// 7A: position assignment / occupancy (L2-E02).
    PositionAssignmentStatus {
        Draft => "draft",
        PendingApproval => "pending_approval",
        Approved => "approved",
        Scheduled => "scheduled",
        Active => "active",
        Suspended => "suspended",
        Closed => "closed",
        Superseded => "superseded",
        Archived => "archived",
    }
);

status_enum!(
    /// 7A: faculty teaching eligibility (L2-E03).
    FacultyEligibilityStatus {
        Requested => "requested",
        UnderReview => "under_review",
        Approved => "approved",
        Suspended => "suspended",
        Expired => "expired",
        Rejected => "rejected",
    }
);

status_enum!(
    /// 7A: hiring requisition.
    RequisitionStatus {
        Draft => "draft",
        PendingApproval => "pending_approval",
        Approved => "approved",
        Open => "open",
        Filled => "filled",
        Cancelled => "cancelled",
    }
);

status_enum!(
    /// 7A: candidate of a requisition.
    CandidateStatus {
        Applied => "applied",
        Shortlisted => "shortlisted",
        Interviewed => "interviewed",
        Offered => "offered",
        OfferAccepted => "offer_accepted",
        OfferDeclined => "offer_declined",
        Rejected => "rejected",
        Withdrawn => "withdrawn",
    }
);

status_enum!(
    /// 7A: onboarding case (L2-E04 hiring → joining).
    OnboardingStatus {
        Initiated => "initiated",
        DocumentationPending => "documentation_pending",
        VerificationPending => "verification_pending",
        ReadyForJoining => "ready_for_joining",
        Joined => "joined",
        Cancelled => "cancelled",
    }
);

status_enum!(
    /// 7A (7B leave cases reuse it): generic HR case lifecycle (L2-E04).
    HrCaseStatus {
        Initiated => "initiated",
        PendingApproval => "pending_approval",
        Approved => "approved",
        Rejected => "rejected",
        Processing => "processing",
        Completed => "completed",
        Failed => "failed",
        Closed => "closed",
        Archived => "archived",
    }
);

status_enum!(
    /// 7A: probation review outcome (L3-E-043).
    ProbationOutcome {
        Confirmed => "confirmed",
        Extended => "extended",
        Terminated => "terminated",
    }
);

status_enum!(
    /// 7A: why an engagement ends (7C reads it for the final settlement).
    SeparationKind {
        Resignation => "resignation",
        Retirement => "retirement",
        Termination => "termination",
        ContractEnd => "contract_end",
        Death => "death",
        Other => "other",
    }
);

status_enum!(
    /// 7A: qualification / document verification.
    VerificationStatus {
        Pending => "pending",
        Verified => "verified",
        Rejected => "rejected",
        Expired => "expired",
    }
);

status_enum!(
    /// 7B: leave policy version (L2-E05 §6.3).
    LeavePolicyStatus {
        Draft => "draft",
        UnderReview => "under_review",
        Approved => "approved",
        Scheduled => "scheduled",
        Active => "active",
        Superseded => "superseded",
        Archived => "archived",
    }
);

status_enum!(
    /// 7B: leave request (L2-E05 §6.4).
    LeaveRequestStatus {
        Draft => "draft",
        Submitted => "submitted",
        Validated => "validated",
        PendingApproval => "pending_approval",
        Approved => "approved",
        Rejected => "rejected",
        Reserved => "reserved",
        Consumed => "consumed",
        Cancelled => "cancelled",
        Closed => "closed",
    }
);

status_enum!(
    /// 7B: governed leave balance adjustment (L3-E-049).
    LeaveAdjustmentStatus {
        Requested => "requested",
        PendingApproval => "pending_approval",
        Approved => "approved",
        Applied => "applied",
        Rejected => "rejected",
    }
);

status_enum!(
    /// 7B: leave encashment decision (L3-E-051).
    EncashmentStatus {
        Requested => "requested",
        Eligible => "eligible",
        NotEligible => "not_eligible",
        Approved => "approved",
        Paid => "paid",
        Rejected => "rejected",
    }
);

status_enum!(
    /// 7B: leave exception (L3-E-052).
    LeaveExceptionStatus {
        Raised => "raised",
        UnderReview => "under_review",
        Resolved => "resolved",
        Closed => "closed",
    }
);

status_enum!(
    /// 7B: faculty availability + workload plan (L3-E-030).
    WorkloadPlanStatus {
        Draft => "draft",
        Submitted => "submitted",
        Approved => "approved",
        Active => "active",
        Superseded => "superseded",
    }
);

status_enum!(
    /// 7C: payroll configuration version (L2-E06 §7.3).
    PayrollConfigStatus {
        Draft => "draft",
        Review => "review",
        Approved => "approved",
        Active => "active",
        Superseded => "superseded",
        Archived => "archived",
    }
);

status_enum!(
    /// 7C: pay component kind.
    PayComponentKind {
        Earning => "earning",
        Deduction => "deduction",
        EmployerContribution => "employer_contribution",
    }
);

status_enum!(
    /// 7C: how a component amount is computed.
    CalculationBasis {
        Fixed => "fixed",
        PercentOfBasic => "percent_of_basic",
        PercentOfGross => "percent_of_gross",
        Statutory => "statutory",
    }
);

status_enum!(
    /// 7C: payroll run (L2-E06 §7.1).
    PayrollRunStatus {
        Created => "created",
        Calculating => "calculating",
        Calculated => "calculated",
        Validating => "validating",
        PendingApproval => "pending_approval",
        Approved => "approved",
        Finalized => "finalized",
        Posted => "posted",
        Closed => "closed",
        Failed => "failed",
        Reprocessing => "reprocessing",
    }
);

status_enum!(
    /// 7C: payroll run kind.
    PayrollRunKind {
        Regular => "regular",
        OffCycle => "off_cycle",
        FinalSettlement => "final_settlement",
        Retro => "retro",
    }
);

status_enum!(
    /// 7C: employee-level payroll result (L2-E06 §7.2).
    PayrollResultStatus {
        Calculated => "calculated",
        Validated => "validated",
        Finalized => "finalized",
        Superseded => "superseded",
    }
);

status_enum!(
    /// 7C: payroll adjustment (L2-E06 §7.3).
    PayrollAdjustmentStatus {
        Requested => "requested",
        Validated => "validated",
        PendingApproval => "pending_approval",
        Approved => "approved",
        Calculated => "calculated",
        Applied => "applied",
        Closed => "closed",
        Rejected => "rejected",
    }
);

status_enum!(
    /// 7C: payslip.
    PayslipStatus {
        Issued => "issued",
        Reissued => "reissued",
        Withdrawn => "withdrawn",
    }
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_names_round_trip() {
        for s in StaffStatus::ALL {
            assert_eq!(StaffStatus::parse(s.as_str()), Some(*s));
        }
        for s in EngagementStatus::ALL {
            assert_eq!(EngagementStatus::parse(s.as_str()), Some(*s));
        }
        for s in LeaveEntryKind::ALL {
            assert_eq!(LeaveEntryKind::parse(s.as_str()), Some(*s));
        }
        assert_eq!(
            PayrollRunStatus::parse("posted"),
            Some(PayrollRunStatus::Posted)
        );
        assert_eq!(StaffStatus::parse("bogus"), None);
        let json = serde_json::to_string(&EngagementStatus::LongLeave).unwrap_or_default();
        assert_eq!(json, "\"long_leave\"");
    }

    #[test]
    fn engagement_machine_follows_l2_e01() {
        use EngagementStatus::*;
        assert!(Pending.can_transition(Active));
        assert!(Active.can_transition(LongLeave) && LongLeave.can_transition(Active));
        assert!(Suspended.can_transition(Closed));
        assert!(
            !Closed.can_transition(Active),
            "rejoining is a new engagement"
        );
        assert!(!Pending.can_transition(Closed));
        assert!(!LongLeave.can_transition(Suspended));
        assert!(Active.is_working() && !LongLeave.is_working());
        assert!(LongLeave.is_payable() && !Pending.is_payable() && !Closed.is_payable());
        assert!(!Closed.is_live() && !Cancelled.is_live() && Suspended.is_live());
    }

    #[test]
    fn staff_machine_follows_l2_e07() {
        use StaffStatus::*;
        assert!(Draft.can_transition(PendingVerification));
        assert!(PendingVerification.can_transition(Verified));
        assert!(Verified.can_transition(Active));
        assert!(Active.can_transition(Merged) && !Archived.can_transition(Merged));
        assert!(!Merged.can_transition(Active));
        assert!(Archived.can_transition(Active));
        assert!(!Active.can_transition(Draft));
    }

    #[test]
    fn leave_entry_signs() {
        assert_eq!(LeaveEntryKind::Accrual.sign(), EntrySign::Positive);
        assert_eq!(LeaveEntryKind::Consumption.sign(), EntrySign::Negative);
        assert_eq!(LeaveEntryKind::Adjustment.sign(), EntrySign::Either);
        assert!(LeaveEntryKind::Opening.is_entitlement());
        assert!(!LeaveEntryKind::Reservation.is_entitlement());
        assert_eq!(AttendanceDayStatus::HalfDay.absent_hundredths(), 50);
        assert_eq!(AttendanceDayStatus::OnLeave.absent_hundredths(), 0);
    }
}
