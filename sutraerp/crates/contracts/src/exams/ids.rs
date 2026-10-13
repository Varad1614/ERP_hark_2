//! Typed UUIDv7 ids of the Batch 6 (Domain I, Examinations) aggregates (ADR-L5-0036 §3.3).
//!
//! Foundation-owned and **append-only**: a slice that needs a new id adds it at the end of its
//! own block with a `// 6X:` marker; existing ids are never renamed (see
//! `docs/batch6-contracts.md` §4.1).

use sutra_kernel::typed_id;

pub use sutra_kernel::scope::{InstitutionId, TenantId};

// ---- foundation: shared examination aggregates (every slice references these) ----
typed_id!(
    /// Grading scheme of an institution (grade bands → grade points; foundation API).
    GradingSchemeId
);
typed_id!(
    /// One band of a grading scheme (minimum percentage → grade + grade point).
    GradeBandId
);
typed_id!(
    /// Examination session (L2-I02): the governed context of one examination cycle.
    ExamSessionId
);
typed_id!(
    /// Examinable component of a session (one course offering / curriculum component, theory or practical).
    ExamPaperId
);
typed_id!(
    /// A candidate's examination registration in a session (L2-I04).
    ExamRegistrationId
);
typed_id!(
    /// One registered paper of a registration (the candidate × paper unit marks attach to).
    ExamRegistrationPaperId
);
typed_id!(
    /// The candidate's marks for one registered paper (internal + external + grace; L2-I08).
    PaperMarkId
);
typed_id!(
    /// Computed / published term result of a candidate in a session (L2-I09/I10; read by Domain C).
    TermResultId
);
typed_id!(
    /// One paper line of a term result (marks, grade, grade point).
    TermResultLineId
);

// ---- 6A: policy, sessions, scheduling, registration, hall tickets, seating, invigilation, conduct ----
typed_id!(
    /// Examination policy version (L2-I01).
    ExamPolicyId
);
typed_id!(
    /// Examination schedule / timetable of a session (L2-I03).
    ExamScheduleId
);
typed_id!(
    /// One scheduled sitting of a paper (date, start, duration) in a schedule.
    ExamSittingId
);
typed_id!(
    /// Examination venue (hall / lab) with seating capacity.
    ExamVenueId
);
typed_id!(
    /// Allocation of a venue to a sitting.
    VenueAllocationId
);
typed_id!(
    /// Seating arrangement of a sitting (L3-I-015).
    SeatingPlanId
);
typed_id!(
    /// One candidate's seat in a seating plan.
    SeatAssignmentId
);
typed_id!(
    /// Invigilation / examiner duty of a staff member on a sitting.
    InvigilationDutyId
);
typed_id!(
    /// Hall ticket (entry authorisation) of a candidate (L2-I05).
    HallTicketId
);
typed_id!(
    /// Conduct record of a sitting (attendance, incidents, completion; L2-I06/I07).
    ConductRecordId
);
typed_id!(
    /// Examination exception / contingency case (L2-I15).
    ExamExceptionId
);

// ---- 6B: marks entry, evaluation, gradebook, moderation, malpractice ----
typed_id!(
    /// Marks collection sheet of a paper (evaluation lifecycle, L2-I08).
    MarkSheetId
);
typed_id!(
    /// One evaluation round of an answer script (first, second, adjudicating).
    EvaluationId
);
typed_id!(
    /// Moderation of a paper's marks under policy (L3-I-075).
    ModerationId
);
typed_id!(
    /// Marks submission exception (L3-I-074/076).
    MarksExceptionId
);
typed_id!(
    /// Malpractice case (L2-I14).
    MalpracticeCaseId
);
typed_id!(
    /// Committee decision on a malpractice case (incl. review outcomes).
    MalpracticeDecisionId
);

// ---- 6C: results, publication, grade cards, revaluation, supplementary, completion ----
typed_id!(
    /// Result computation run of a session (L2-I09).
    ResultBatchId
);
typed_id!(
    /// Verification / approval record of a result batch.
    ResultVerificationId
);
typed_id!(
    /// Result publication of a session scope (L2-I10).
    PublicationId
);
typed_id!(
    /// Grade card issued from a published term result.
    GradeCardId
);
typed_id!(
    /// Revaluation / rechecking request (L2-I11).
    RevaluationRequestId
);
typed_id!(
    /// Subsequent-attempt eligibility of a candidate for a paper (L2-I12).
    SupplementaryEligibilityId
);
typed_id!(
    /// Academic completion evaluation (CGPA, credits; L2-I13).
    CompletionEvaluationId
);
typed_id!(
    /// Examination session closure / archival record (L2-I16).
    SessionClosureId
);
