//! Lifecycle state enums of the Batch 3 (Domain C) aggregates.
//!
//! Foundation-owned. Wire/DB form is the `snake_case` name, stored in a `text` column with a
//! `CHECK (status IN (...))` written by the owning migration. Adding a variant is a contract
//! change (edit `docs/batch3-contracts.md` §4.2).
//!
//! Transitions: the two **shared** machines ([`StudentStatus`], [`StudentContextStatus`]) carry
//! their legal transitions here (`can_transition`), because more than one slice moves them
//! (3A enrols/suspends/transfers, 3C exits/completes). Every other machine's transitions are
//! slice-owned business rules written as a pure, unit-tested `can_transition(from, to)` in the
//! owning slice's `domain.rs`; an illegal transition is `409 conflict`.

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

// ---- foundation: shared Student aggregate ----
status_enum!(
    /// Student persona lifecycle (L2-C02 §11.2, condensed — see contract doc §4.2: `restored` and `transferred` are transition events that land in `active`).
    StudentStatus {
        /// Created from a confirmed Admission; no Institute relationship yet.
        Created => "created",
        /// Identity active; a `pending_activation` StudentContext exists.
        Activated => "activated",
        /// Enrolment completed; the context is `active`.
        Enrolled => "enrolled",
        /// Programme / department / batch / section assigned.
        AcademicallyAssigned => "academically_assigned",
        /// Steady state.
        Active => "active",
        /// The live context is suspended.
        Suspended => "suspended",
        /// Qualification completed at the Institute of the last live context.
        QualificationCompleted => "qualification_completed",
        /// No live context remains; record retained (immutable except governed readmission).
        Archived => "archived",
    }
);
status_enum!(
    /// StudentContext lifecycle (L2-C02 §11.10.1). Terminal states are immutable (SC-INV-003).
    StudentContextStatus {
        /// Context opened for an Institute; relationship not yet live.
        PendingActivation => "pending_activation",
        /// Live academic relationship.
        Active => "active",
        /// Temporarily restricted; reversible to `active`.
        Suspended => "suspended",
        /// Terminal.
        QualificationCompleted => "qualification_completed",
        /// Terminal: the student voluntarily ended the relationship.
        Withdrawn => "withdrawn",
        /// Terminal: moved to another Institute (a new context opens there) or transferred out.
        Transferred => "transferred",
        /// Terminal: lapsed without a formal exit decision (also administrative closure and deceased closure).
        Discontinued => "discontinued",
        /// Terminal: ended by administrative or disciplinary decision (also admission revocation after student creation).
        Terminated => "terminated",
    }
);

impl StudentStatus {
    /// Legal persona transitions (contract doc §4.2). Guards that need data (e.g. "no live
    /// context remains" before `archived`) are checked by the caller.
    pub fn can_transition(self, to: StudentStatus) -> bool {
        use StudentStatus::*;
        matches!(
            (self, to),
            (Created, Activated)
                | (Activated, Enrolled)
                | (Enrolled, AcademicallyAssigned)
                | (AcademicallyAssigned, Active)
                | (Active, Suspended)
                | (Suspended, Active)
                | (Active, QualificationCompleted)
                | (
                    Created
                        | Activated
                        | Enrolled
                        | AcademicallyAssigned
                        | Active
                        | Suspended
                        | QualificationCompleted,
                    Archived
                )
                // Governed readmission (L3-C-005) reopens a persona with a new context.
                | (Archived | QualificationCompleted, Activated)
        )
    }
}

impl StudentContextStatus {
    /// Terminal (immutable, history only).
    pub fn is_terminal(self) -> bool {
        !self.is_live()
    }

    /// `pending_activation`, `active` or `suspended`: at most one live context per student and
    /// institution (SC-INV-001 / SC-INV-004, enforced by a partial unique index).
    pub fn is_live(self) -> bool {
        matches!(
            self,
            StudentContextStatus::PendingActivation
                | StudentContextStatus::Active
                | StudentContextStatus::Suspended
        )
    }

    /// Legal context transitions (L2-C02 §11.10, contract doc §4.2).
    pub fn can_transition(self, to: StudentContextStatus) -> bool {
        use StudentContextStatus::*;
        matches!(
            (self, to),
            (PendingActivation, Active)
                | (PendingActivation, Withdrawn | Discontinued | Terminated | Transferred)
                | (Active, Suspended)
                | (Suspended, Active)
                | (
                    Active,
                    QualificationCompleted | Withdrawn | Transferred | Discontinued | Terminated
                )
                // Suspended → qualification_completed is prohibited (L2-C02 §11.6).
                | (Suspended, Withdrawn | Transferred | Discontinued | Terminated)
        )
    }
}

// ---- shared helpers used by several slices ----
status_enum!(
    /// Lifecycle of a governed request/decision case (identity correction, duplicate case, transfer, credit transfer, programme migration, progression revision, completion deferral, qualification correction, …).
    CaseStatus {
        /// Raised; editable by the requester.
        Requested => "requested",
        /// Submitted to the approval step (WorkflowPort); `processInstanceId` stored.
        UnderReview => "under_review",
        /// Approved, not yet applied.
        Approved => "approved",
        /// Terminal.
        Rejected => "rejected",
        /// Applied to the owning aggregate. Terminal.
        Completed => "completed",
        /// Withdrawn by the requester before a decision. Terminal.
        Withdrawn => "withdrawn",
    }
);
status_enum!(
    /// Versioned policy lifecycle (progression policy L3-C-075, scholarship eligibility policy L3-C-063). Effective versions are immutable; a change is a successor draft.
    PolicyVersionStatus {
        Draft => "draft",
        InReview => "in_review",
        /// In force from `effectiveFrom`.
        Effective => "effective",
        Superseded => "superseded",
        Retired => "retired",
    }
);
status_enum!(
    /// Lifecycle of a time-bound assignment (emergency contact, pickup authorisation, financial responsibility, communication authorisation).
    AssignmentStatus {
        Active => "active",
        Suspended => "suspended",
        /// Ended before its end date. Terminal.
        Revoked => "revoked",
        /// Reached its end date. Terminal.
        Expired => "expired",
    }
);
status_enum!(
    /// Verification state of a document (admission documents, student record documents).
    DocumentStatus {
        Submitted => "submitted",
        Verified => "verified",
        Rejected => "rejected",
        /// Not required by policy for this case.
        Waived => "waived",
        /// Replaced by a newer submission.
        Superseded => "superseded",
    }
);

// ---- 3A: admissions, activation and student record ----
status_enum!(
    /// Admission application (pre-admission candidate pipeline).
    AdmissionApplicationStatus {
        Draft => "draft",
        Submitted => "submitted",
        UnderReview => "under_review",
        /// An Admission Record was initiated with an offer. Terminal for the application.
        Offered => "offered",
        Rejected => "rejected",
        Withdrawn => "withdrawn",
    }
);
status_enum!(
    /// Admission Record lifecycle (L2-C01 §6.6–6.9 + amendments 2026-08-22 expiry, 2026-10-05 finance-clearance evidence).
    AdmissionStatus {
        /// Admission Initiated: offer outstanding (validity `offerExpiresOn`).
        Initiated => "initiated",
        DocumentsSubmitted => "documents_submitted",
        VerificationCompleted => "verification_completed",
        /// Implementation sub-state of Verification Completed while the WorkflowPort approval is open (AwaitDecision).
        ApprovalPending => "approval_pending",
        /// Admission Approved.
        Approved => "approved",
        /// Financial Clearance decided by Admissions from Finance evidence (FinanceClearancePort).
        FinanciallyCleared => "financially_cleared",
        /// Admission Confirmed: eligible for Student creation.
        Confirmed => "confirmed",
        /// Student + pending StudentContext created.
        StudentCreated => "student_created",
        /// Admission Completed. Terminal.
        Completed => "completed",
        /// Terminal (readmission = new record).
        Cancelled => "cancelled",
        /// Terminal (readmission = new record).
        Withdrawn => "withdrawn",
        /// Policy-controlled terminal.
        Revoked => "revoked",
        /// Terminal (Initiated → Expired, ADR-STU-ADM-EXP-001).
        Expired => "expired",
    }
);
status_enum!(
    /// Student document / certificate request (L3-C-018).
    DocumentRequestStatus {
        Requested => "requested",
        InProgress => "in_progress",
        /// Terminal.
        Issued => "issued",
        /// Terminal.
        Rejected => "rejected",
        /// Terminal.
        Cancelled => "cancelled",
    }
);
status_enum!(
    /// Kind of student transfer.
    StudentTransferKind {
        /// L3-C-012: non-programme organisational/placement reassignment inside one institution.
        IntraInstitute => "intra_institute",
        /// L3-C-013: to another institution of the same tenant; the source context ends `transferred`, a new context opens.
        InterInstitute => "inter_institute",
    }
);

// ---- 3B: registration, progression and credits ----
status_enum!(
    /// Registration window (L3-C-020).
    RegistrationWindowStatus {
        Scheduled => "scheduled",
        Open => "open",
        Closed => "closed",
        Cancelled => "cancelled",
    }
);
status_enum!(
    /// Academic Registration aggregate (L2-C03 §6.9; Frozen + Published = `finalised`).
    RegistrationStatus {
        /// Created; lines being selected and validated.
        Draft => "draft",
        /// Awaiting approval (advisor / WorkflowPort).
        Submitted => "submitted",
        /// Approval returned for changes; editable.
        Returned => "returned",
        Approved => "approved",
        /// Effective registration; add/drop allowed while the window permits.
        Registered => "registered",
        /// Frozen and published (L3-C-025). Changes only by governed revision.
        Finalised => "finalised",
        /// Terminal (L3-C-023).
        Cancelled => "cancelled",
        /// Terminal.
        Archived => "archived",
    }
);
status_enum!(
    /// Kind of a registration line (L3-C-024).
    RegistrationLineKind {
        Regular => "regular",
        Backlog => "backlog",
        Repeat => "repeat",
        Audit => "audit",
        Elective => "elective",
    }
);
status_enum!(
    /// Registration line state (add/drop, L3-C-022).
    RegistrationLineStatus {
        Selected => "selected",
        Registered => "registered",
        Dropped => "dropped",
        Withdrawn => "withdrawn",
    }
);
status_enum!(
    /// Registration hold (L3-C-026).
    RegistrationHoldStatus {
        Active => "active",
        Released => "released",
    }
);
status_enum!(
    /// Who placed a hold.
    HoldSource {
        Academic => "academic",
        Administrative => "administrative",
        Discipline => "discipline",
        Documents => "documents",
        /// Mirrors a Finance-owned fact read through FinanceClearancePort (Batch 5).
        Finance => "finance",
    }
);
status_enum!(
    /// Progression decision record (L3-C-027 / L3-C-038).
    ProgressionDecisionStatus {
        Draft => "draft",
        PendingApproval => "pending_approval",
        Effective => "effective",
        /// Replaced by a revision (L3-C-038).
        Superseded => "superseded",
        Rejected => "rejected",
    }
);
status_enum!(
    /// Outcome of a progression decision (L2-C04 §4.3, amendment 2026-08-22 event names).
    ProgressionOutcome {
        Progressed => "progressed",
        ConditionallyProgressed => "conditionally_progressed",
        Probation => "probation",
        Detained => "detained",
        Repeat => "repeat",
        QualificationEligible => "qualification_eligible",
    }
);
status_enum!(
    /// Current academic standing of a student (3B-owned projection of the effective decisions).
    AcademicStanding {
        GoodStanding => "good_standing",
        Conditional => "conditional",
        Probation => "probation",
        Detained => "detained",
        AcademicBreak => "academic_break",
    }
);
status_enum!(
    /// Academic Credit Ledger entry kind (L3-C-028; ledger is append-only).
    CreditEntryKind {
        Earned => "earned",
        TransferredIn => "transferred_in",
        Recognised => "recognised",
        Adjusted => "adjusted",
        Revoked => "revoked",
        Expired => "expired",
    }
);
status_enum!(
    /// Condition of a conditional promotion (L3-C-029).
    ProgressionConditionStatus {
        Pending => "pending",
        Fulfilled => "fulfilled",
        Failed => "failed",
        Waived => "waived",
    }
);
status_enum!(
    /// Academic probation case (L3-C-030).
    ProbationStatus {
        Active => "active",
        Cleared => "cleared",
        Escalated => "escalated",
    }
);
status_enum!(
    /// Academic break (L3-C-034 / L3-C-035).
    AcademicBreakStatus {
        Requested => "requested",
        Approved => "approved",
        InEffect => "in_effect",
        /// Terminal.
        Rejoined => "rejoined",
        /// Terminal: break exceeded policy limits.
        Lapsed => "lapsed",
        Rejected => "rejected",
        Withdrawn => "withdrawn",
    }
);
status_enum!(
    /// ABC (Academic Bank of Credits) synchronisation record (L3-C-037).
    AbcSyncStatus {
        Pending => "pending",
        Submitted => "submitted",
        Acknowledged => "acknowledged",
        Failed => "failed",
    }
);

// ---- 3C: completion, exit, guardians, scholarships, alumni ----
status_enum!(
    /// Qualification Completion aggregate (L2-C05 §9.2 + MEME amendment 2026-08-22).
    QualificationCompletionStatus {
        EligibilityEvaluation => "eligibility_evaluation",
        DegreeAudit => "degree_audit",
        InstitutionalClearance => "institutional_clearance",
        ApprovalPending => "approval_pending",
        Approved => "approved",
        /// Qualification Awarded (conferment).
        Awarded => "awarded",
        /// Academic Record Finalized.
        RecordFinalized => "record_finalized",
        /// Student Exit Completed. Terminal.
        ExitCompleted => "exit_completed",
        /// Completion deferred for remediation (L3-C-049).
        Deferred => "deferred",
        /// Terminal.
        Cancelled => "cancelled",
    }
);
status_enum!(
    /// Qualification outcome (L1, MEME amendment §2).
    QualificationOutcome {
        Certificate => "certificate",
        Diploma => "diploma",
        AdvancedDiploma => "advanced_diploma",
        Degree => "degree",
        DegreeWithResearch => "degree_with_research",
    }
);
status_enum!(
    /// State of a conferred qualification (L3-C-050).
    ConferredQualificationStatus {
        Conferred => "conferred",
        /// Superseded by a corrected version.
        Corrected => "corrected",
        /// Terminal.
        Revoked => "revoked",
    }
);
status_enum!(
    /// Kind of student exit; maps to the terminal StudentContext state (contract doc §4.2).
    StudentExitKind {
        /// → qualification_completed (via the Qualification Completion aggregate).
        Completion => "completion",
        /// NEP multiple-exit with an intermediate qualification → qualification_completed.
        MemeExit => "meme_exit",
        /// → withdrawn.
        Withdrawal => "withdrawal",
        /// Transfer-out (external) → transferred.
        Transfer => "transfer",
        /// → discontinued.
        Discontinuation => "discontinuation",
        /// → terminated.
        Termination => "termination",
        /// → discontinued.
        AdministrativeClosure => "administrative_closure",
        /// → discontinued (record closure, L3-C-048).
        Deceased => "deceased",
    }
);
status_enum!(
    /// Student exit case.
    StudentExitStatus {
        Initiated => "initiated",
        ClearancePending => "clearance_pending",
        ApprovalPending => "approval_pending",
        Approved => "approved",
        /// Terminal; the context is closed.
        Completed => "completed",
        /// Terminal.
        Rejected => "rejected",
        /// Terminal.
        Cancelled => "cancelled",
    }
);
status_enum!(
    /// Exit clearance item (L3-C-046).
    ExitClearanceStatus {
        Pending => "pending",
        Cleared => "cleared",
        Waived => "waived",
        Blocked => "blocked",
    }
);
status_enum!(
    /// Guardian relationship (L2-C06 lifecycle; `Modified` is an event, not a state).
    GuardianRelationshipStatus {
        Proposed => "proposed",
        PendingVerification => "pending_verification",
        Verified => "verified",
        Active => "active",
        Suspended => "suspended",
        Terminated => "terminated",
        /// Immutable archive.
        Historical => "historical",
    }
);
status_enum!(
    /// Guardian consent (L3-C-054).
    GuardianConsentStatus {
        Granted => "granted",
        Withdrawn => "withdrawn",
        Expired => "expired",
    }
);
status_enum!(
    /// Scholarship definition (L2-C07 §12).
    ScholarshipDefinitionStatus {
        Draft => "draft",
        Published => "published",
        Active => "active",
        Suspended => "suspended",
        Expired => "expired",
        Archived => "archived",
    }
);
status_enum!(
    /// Student scholarship eligibility (L2-C07 §12).
    ScholarshipEligibilityStatus {
        Created => "created",
        AssessmentPending => "assessment_pending",
        UnderEvaluation => "under_evaluation",
        Eligible => "eligible",
        NotEligible => "not_eligible",
        ConditionallyEligible => "conditionally_eligible",
        RecommendationGenerated => "recommendation_generated",
        Active => "active",
        Reassessment => "reassessment",
        Renewal => "renewal",
        Expired => "expired",
        Archived => "archived",
    }
);
status_enum!(
    /// Student → alumni transition (L2-C08 §12.3; `transitioning` is internal and publishes no event).
    AlumniTransitionStatus {
        Created => "created",
        PendingVerification => "pending_verification",
        Verified => "verified",
        PendingApproval => "pending_approval",
        Approved => "approved",
        Transitioning => "transitioning",
        Activated => "activated",
        Completed => "completed",
        Archived => "archived",
        /// Terminal (not in the L2 happy path; approval refused).
        Rejected => "rejected",
    }
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_names_round_trip() {
        for s in StudentContextStatus::ALL {
            assert_eq!(StudentContextStatus::parse(s.as_str()), Some(*s));
        }
        for s in AdmissionStatus::ALL {
            assert_eq!(AdmissionStatus::parse(s.as_str()), Some(*s));
        }
        assert_eq!(
            serde_json::to_value(StudentStatus::AcademicallyAssigned).unwrap_or_default(),
            "academically_assigned"
        );
    }

    #[test]
    fn student_context_machine() {
        use StudentContextStatus::*;
        assert!(PendingActivation.can_transition(Active));
        assert!(Active.can_transition(Suspended) && Suspended.can_transition(Active));
        assert!(!Suspended.can_transition(QualificationCompleted));
        for t in [
            QualificationCompleted,
            Withdrawn,
            Transferred,
            Discontinued,
            Terminated,
        ] {
            assert!(t.is_terminal());
            for to in StudentContextStatus::ALL {
                assert!(!t.can_transition(*to), "{t} is terminal");
            }
        }
        assert!(Active.is_live() && PendingActivation.is_live() && Suspended.is_live());
    }

    #[test]
    fn student_persona_machine() {
        use StudentStatus::*;
        assert!(Created.can_transition(Activated));
        assert!(!Created.can_transition(Active));
        assert!(!Activated.can_transition(QualificationCompleted));
        assert!(!Suspended.can_transition(QualificationCompleted));
        assert!(Active.can_transition(Archived));
        assert!(!Archived.can_transition(Active));
        assert!(Archived.can_transition(Activated));
        assert!(!Archived.can_transition(Archived));
    }
}
