//! Typed UUIDv7 ids of the Batch 3 (Domain C) aggregates (ADR-L5-0036 §3.3).
//!
//! Foundation-owned and **append-only**: a slice that needs a new id adds it at the end of its
//! own block with a `// 3X:` marker; existing ids are never renamed (see
//! `docs/batch3-contracts.md` §4.1).

use sutra_kernel::typed_id;

pub use sutra_kernel::scope::{InstitutionId, TenantId};

// ---- foundation: shared Student aggregate (every slice references these) ----
typed_id!(
    /// Canonical Student persona (tenant-scoped; L2-C02 §6, L1 amendment 2026-08-26). Never changes.
    StudentId
);
typed_id!(
    /// Per-Institute StudentContext (L2-C02 §11.10): the student's relationship with one institution.
    StudentContextId
);

// ---- 3A: admissions, activation and student record ----
typed_id!(
    /// Admission application of a candidate (draft → submitted → under review → offered/rejected).
    AdmissionApplicationId
);
typed_id!(
    /// Admission Record (L2-C01): created when an offer is made; Admission Initiated → … → Student Created.
    AdmissionId
);
typed_id!(
    /// Document submitted for an admission and its verification outcome (L3-C-007).
    AdmissionDocumentId
);
typed_id!(
    /// Document held on a student's record (identity proofs, transfer certificate received, …).
    StudentDocumentId
);
typed_id!(
    /// Student document or certificate request (bonafide, transcript copy, migration, …; L3-C-018).
    DocumentRequestId
);
typed_id!(
    /// Governed correction of a student's identity attributes (L3-C-016).
    IdentityCorrectionId
);
typed_id!(
    /// Duplicate student identity investigation and resolution (L3-C-017).
    DuplicateCaseId
);
typed_id!(
    /// Intra- or inter-institute transfer of a student (L3-C-012 / L3-C-013).
    StudentTransferId
);
typed_id!(
    /// Suspension of a StudentContext and its restoration (L3-C-010 / L3-C-011).
    StudentSuspensionId
);

// ---- 3B: registration, progression and credits ----
typed_id!(
    /// Academic registration window of an institution/term (L3-C-020).
    RegistrationWindowId
);
typed_id!(
    /// A student's academic registration for one term (L2-C03 Academic Registration aggregate).
    RegistrationId
);
typed_id!(
    /// One registered curriculum component / course line of a registration.
    RegistrationLineId
);
typed_id!(
    /// Hold that blocks registration (academic, administrative, finance-sourced; L3-C-026).
    RegistrationHoldId
);
typed_id!(
    /// Institutional progression policy version (L3-C-075).
    ProgressionPolicyId
);
typed_id!(
    /// Progression decision of a student for a term/year (L3-C-027).
    ProgressionDecisionId
);
typed_id!(
    /// Immutable entry of the Academic Credit Ledger (L3-C-028).
    CreditLedgerEntryId
);
typed_id!(
    /// Condition attached to a conditional promotion (L3-C-029).
    ProgressionConditionId
);
typed_id!(
    /// Academic probation case (L3-C-030).
    ProbationCaseId
);
typed_id!(
    /// Academic detention / repeat determination (L3-C-031).
    DetentionCaseId
);
typed_id!(
    /// Credit transfer and recognition request (L3-C-032).
    CreditTransferId
);
typed_id!(
    /// Programme migration of a student (L3-C-033).
    ProgrammeMigrationId
);
typed_id!(
    /// Academic break and rejoining (L3-C-034 / L3-C-035).
    AcademicBreakId
);
typed_id!(
    /// Qualification eligibility determination (L3-C-036).
    QualificationEligibilityId
);
typed_id!(
    /// Credit-ledger synchronisation batch/record to the Academic Bank of Credits (L3-C-037).
    AbcSyncId
);
typed_id!(
    /// Revision of a progression decision after re-evaluation or appeal (L3-C-038).
    ProgressionRevisionId
);

// ---- 3C: completion, exit, guardians, scholarships, alumni ----
typed_id!(
    /// Qualification Completion aggregate (L2-C05): eligibility → audit → clearance → approval → award → finalisation → exit.
    QualificationCompletionId
);
typed_id!(
    /// Student exit case (withdrawal, transfer, discontinuation, termination, administrative, deceased, MEME).
    StudentExitId
);
typed_id!(
    /// Institutional exit clearance item (finance, library, hostel, department, …; L3-C-046).
    ExitClearanceId
);
typed_id!(
    /// Qualification completion deferral and remediation (L3-C-049).
    CompletionDeferralId
);
typed_id!(
    /// Correction or revocation of a conferred qualification (L3-C-050).
    QualificationCorrectionId
);
typed_id!(
    /// Guardian party (references the canonical Person when Platform A02 lands).
    GuardianId
);
typed_id!(
    /// Guardian ↔ student relationship (L2-C06 aggregate).
    GuardianRelationshipId
);
typed_id!(
    /// Guardian consent authorisation / withdrawal (L3-C-054).
    GuardianConsentId
);
typed_id!(
    /// Student emergency contact assignment (L3-C-056).
    EmergencyContactId
);
typed_id!(
    /// Student pickup authorisation (L3-C-058).
    PickupAuthorisationId
);
typed_id!(
    /// Scholarship definition (L3-C-062).
    ScholarshipDefinitionId
);
typed_id!(
    /// Scholarship eligibility policy version (L3-C-063).
    ScholarshipPolicyId
);
typed_id!(
    /// Student scholarship eligibility determination (L3-C-064 … C-068).
    ScholarshipEligibilityId
);
typed_id!(
    /// Bulk scholarship eligibility recalculation run (L3-C-070).
    ScholarshipRecalculationId
);
typed_id!(
    /// Student → alumni transition aggregate (L2-C08).
    AlumniTransitionId
);
