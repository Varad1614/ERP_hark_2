//! Typed UUIDv7 ids of the Batch 4 (Domain D) aggregates (ADR-L5-0036 §3.3).
//!
//! Foundation-owned and **append-only**: a slice that needs a new id adds it at the end of its
//! own block with a `// 4X:` marker; existing ids are never renamed (see
//! `docs/batch4-contracts.md` §4.1).

use sutra_kernel::typed_id;

pub use sutra_kernel::scope::{InstitutionId, TenantId};

// ---- foundation: shared delivery aggregates (every slice references these) ----
typed_id!(
    /// Course Offering (L2-D01): one curriculum component offered in one academic term at one institution/campus.
    CourseOfferingId
);
typed_id!(
    /// Teaching section of an offering (class group with its own roster, faculty and timetable).
    OfferingSectionId
);
typed_id!(
    /// Faculty Academic Assignment (L2-D02): a faculty member assigned to a section in a teaching role.
    FacultyAssignmentId
);
typed_id!(
    /// A student's seat in a section (the roster attendance, coursework and assessment use).
    SectionEnrolmentId
);

// ---- 4A: offerings, capacity, enrolment governance, faculty assignment ----
typed_id!(
    /// Waitlist entry of a student for an offering section (L3-D-007).
    OfferingWaitlistEntryId
);
typed_id!(
    /// Reserved-seat quota of an offering (L3-D-015).
    OfferingQuotaId
);
typed_id!(
    /// Enrolment window of an offering (L3-D-014).
    OfferingEnrolmentWindowId
);
typed_id!(
    /// Governed revision / replacement / rollback of an offering (L3-D-009).
    OfferingRevisionId
);
typed_id!(
    /// Offering generation template (L3-D-012).
    OfferingTemplateId
);
typed_id!(
    /// Change simulation and impact decision (L3-D-016).
    OfferingChangeSimulationId
);
typed_id!(
    /// Merge / split reorganisation (L3-D-011).
    OfferingReorganisationId
);
typed_id!(
    /// Faculty assignment governance configuration version (L3-D-019).
    FacultyAssignmentPolicyId
);
typed_id!(
    /// Operational change of an active faculty assignment (L3-D-018).
    FacultyAssignmentChangeId
);

// ---- 4B: timetable, sessions, attendance, syllabus coverage, monitoring, internal assessment ----
typed_id!(
    /// Academic scheduling requirement (L3-D-020).
    SchedulingRequirementId
);
typed_id!(
    /// Governed operational timetable version (L2-D03).
    TimetableId
);
typed_id!(
    /// One recurring weekly slot of a timetable (section × day × period × room).
    TimetableSlotId
);
typed_id!(
    /// Governed timetable revision request (L3-D-021).
    TimetableRevisionId
);
typed_id!(
    /// Teaching session instantiated from a slot (L2-D04).
    TeachingSessionId
);
typed_id!(
    /// One student's attendance mark in one session.
    AttendanceRecordId
);
typed_id!(
    /// Governed correction of recorded attendance (L3-D-025).
    AttendanceCorrectionId
);
typed_id!(
    /// Attendance threshold policy version (minimum %, condonation band).
    AttendancePolicyId
);
typed_id!(
    /// Published syllabus coverage record of a section (L3-D-026).
    SyllabusCoverageId
);
typed_id!(
    /// Academic monitoring case (L2-D05, L3-D-027…033).
    MonitoringCaseId
);
typed_id!(
    /// Academic risk assessment inside a monitoring case (L3-D-028).
    RiskAssessmentId
);
typed_id!(
    /// Triggered monitoring intervention (L3-D-030).
    MonitoringInterventionId
);
typed_id!(
    /// Internal assessment blueprint (L2-D07, L3-D-051).
    AssessmentBlueprintId
);
typed_id!(
    /// Assessment window / coursework assignment (L3-D-052).
    AssessmentWindowId
);
typed_id!(
    /// A student's assessment submission (L3-D-053).
    AssessmentSubmissionId
);
typed_id!(
    /// Finalised internal assessment result of a student in a section (L3-D-055).
    InternalAssessmentResultId
);
typed_id!(
    /// Governed correction of a finalised internal assessment (L3-D-057).
    AssessmentCorrectionId
);

// ---- 4C: flexible / external learning, providers, certificates, credit mapping, resources ----
typed_id!(
    /// External learning provider (SWAYAM, NPTEL, partner university, …; L3-D-044).
    LearningProviderId
);
typed_id!(
    /// Flexible / external learning opportunity (open elective, MOOC, …; L3-D-040).
    LearningOpportunityId
);
typed_id!(
    /// A student's flexible-learning enrolment (demand → allocation → registration → completion).
    FlexibleEnrolmentId
);
typed_id!(
    /// Uploaded external certificate and its verification (L3-D-043).
    CertificateVerificationId
);
typed_id!(
    /// Credit recognition and posting request (L3-D-039).
    CreditRecognitionId
);
typed_id!(
    /// Credit mapping rule version (L3-D-046).
    CreditMappingRuleId
);
typed_id!(
    /// Flexible learning policy / approval configuration version (L3-D-045).
    FlexibleLearningPolicyId
);
typed_id!(
    /// External learning record reconciliation run (L3-D-049).
    ReconciliationRunId
);
typed_id!(
    /// Flexible learning exception case (L3-D-050).
    FlexibleExceptionCaseId
);
typed_id!(
    /// Course resource / library link attached to an offering or section (faculty Resources, student Library).
    CourseResourceId
);
