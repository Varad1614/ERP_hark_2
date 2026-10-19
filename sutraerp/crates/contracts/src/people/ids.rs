//! Typed UUIDv7 ids of the Batch 7 (Domain E, People & workforce) aggregates (ADR-L5-0036 §3.3).
//!
//! Foundation-owned and **append-only**: a slice that needs a new id adds it at the end of its
//! own block with a `// 7X:` marker; existing ids are never renamed (see
//! `docs/batch7-contracts.md` §4.1).

use sutra_kernel::typed_id;

pub use sutra_kernel::scope::{InstitutionId, TenantId};

// ---- foundation: shared People aggregates (every slice references these) ----
typed_id!(
    /// Canonical workforce member + core employee profile (L2-E01 / L2-E07).
    StaffMemberId
);
typed_id!(
    /// One employment / engagement of a staff member (L2-E01).
    StaffEngagementId
);
typed_id!(
    /// One append-only leave ledger transaction (L2-E05 §6.6).
    LeaveLedgerEntryId
);
typed_id!(
    /// One staff attendance day (payroll input).
    AttendanceDayId
);

// ---- 7A: staff records, positions, faculty, recruitment / onboarding, HR cases, profile ----
typed_id!(
    /// Institutional position of an org unit (L2-E02).
    PositionId
);
typed_id!(
    /// Occupancy of a position by an engagement (L2-E02).
    PositionAssignmentId
);
typed_id!(
    /// Faculty establishment + teaching eligibility of a staff member (L2-E03).
    FacultyProfileId
);
typed_id!(
    /// Hiring requisition (recruitment).
    RequisitionId
);
typed_id!(
    /// Candidate of a requisition.
    CandidateId
);
typed_id!(
    /// Onboarding case → accepted workforce hand-offs (L3-E-033).
    OnboardingCaseId
);
typed_id!(
    /// Probation review → confirmation decision (L3-E-043).
    ProbationReviewId
);
typed_id!(
    /// Separation case → exit hand-offs (L3-E-041).
    SeparationCaseId
);
typed_id!(
    /// Employee certificate request (L3-E-039).
    CertificateRequestId
);
typed_id!(
    /// Registered qualification of a staff member (L3-E-060).
    StaffQualificationId
);
typed_id!(
    /// Profile document of a staff member (L3-E-064).
    StaffDocumentId
);
typed_id!(
    /// Governed profile correction with history (L3-E-065).
    ProfileCorrectionId
);

// ---- 7B: leave, attendance, workload ----
typed_id!(
    /// Leave policy version (L2-E05 §6.3).
    LeavePolicyId
);
typed_id!(
    /// Leave type of a policy (entitlement, accrual, carry forward, encashment rules).
    LeaveTypeId
);
typed_id!(
    /// Employee leave request (L2-E05 §6.4).
    LeaveRequestId
);
typed_id!(
    /// Periodic entitlement accrual run (L3-E-046).
    LeaveAccrualRunId
);
typed_id!(
    /// Year-end carry forward / expiry run (L3-E-050).
    LeaveYearEndRunId
);
typed_id!(
    /// Governed leave balance adjustment (L3-E-049).
    LeaveAdjustmentId
);
typed_id!(
    /// Leave encashment eligibility decision (L3-E-051).
    LeaveEncashmentId
);
typed_id!(
    /// Leave exception review (L3-E-052).
    LeaveExceptionId
);
typed_id!(
    /// Leave case coordination (L3-E-034).
    LeaveCaseId
);
typed_id!(
    /// Long leave of an engagement (L3-E-006).
    LongLeaveId
);
typed_id!(
    /// Faculty availability + workload plan (L3-E-030).
    WorkloadPlanId
);

// ---- 7C: payroll ----
typed_id!(
    /// Payroll configuration version (L2-E06 §7.3, L3-E-074).
    PayrollConfigId
);
typed_id!(
    /// Pay component (earning / deduction / employer contribution).
    PayComponentId
);
typed_id!(
    /// Statutory deduction / contribution rule (PF, ESI, PT, TDS).
    StatutoryRuleId
);
typed_id!(
    /// Salary structure (component amounts / rates for a grade).
    SalaryStructureId
);
typed_id!(
    /// Effective-dated salary assignment of an engagement.
    SalaryAssignmentId
);
typed_id!(
    /// Payroll period (month).
    PayrollPeriodId
);
typed_id!(
    /// Payroll run of a period (L2-E06 §7.1).
    PayrollRunId
);
typed_id!(
    /// Employee-level payroll result of a run (L2-E06 §7.2).
    PayrollResultId
);
typed_id!(
    /// Payroll adjustment (L2-E06 §7.3).
    PayrollAdjustmentId
);
typed_id!(
    /// Payslip issued from a finalised result.
    PayslipId
);
typed_id!(
    /// Separation final settlement (L3-E-056).
    FinalSettlementId
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_uuid_v7_and_transparent() {
        let id = StaffMemberId::new();
        assert_eq!(id.as_uuid().get_version_num(), 7);
        let json = serde_json::to_string(&id).unwrap_or_default();
        assert_eq!(json, format!("\"{id}\""));
    }
}
