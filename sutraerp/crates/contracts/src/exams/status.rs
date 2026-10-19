//! Lifecycle state and classification enums of the Batch 6 (Domain I, Examinations) aggregates.
//!
//! Foundation-owned. Wire/DB form is the `snake_case` name, stored in a `text` column with a
//! `CHECK (… IN (...))` written by the owning migration. Adding a variant is a contract change
//! (edit `docs/batch6-contracts.md` §4.2). Names follow the L2-I state models (L2-I01 §20 …
//! L2-I16), so the L3 scenarios map onto them without inventing states.
//!
//! Transitions: the **shared** machines live in foundation tables that several slices read and
//! gate on, so their legal transitions are written here (`can_transition`) and only the owning
//! slice moves them through `sutra_exams::shared::lifecycle`:
//!
//! | Machine | Table | Moved by |
//! |---------|-------|----------|
//! | [`ExamSessionStatus`] | `exams.exam_session` | 6A (draft … reopened), 6C (closing → closed → archived) |
//! | [`ExamPaperStatus`] | `exams.exam_paper` | 6A (draft → scheduled → ready → conducted, cancelled), 6B (conducted → evaluation → marks_finalised) |
//! | [`ExamRegistrationStatus`] | `exams.exam_registration` | 6A (6C reads; completion by 6C) |
//! | [`MarkStatus`] | `exams.paper_mark` | 6B (6C revises final marks only through `shared::marks::revise_final`) |
//! | [`ResultStatus`] | `exams.term_result` | 6C |
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
    /// Examination session lifecycle (L2-I02 §20 + amendment 2026-08-24; closure L2-I16).
    ExamSessionStatus {
        Draft => "draft",
        UnderReview => "under_review",
        Approved => "approved",
        /// Approved for a future operational period.
        Scheduled => "scheduled",
        /// Available for operational examination workflows (the only consumable state, with `reopened`).
        Active => "active",
        Suspended => "suspended",
        /// Previously suspended session restored to operational state.
        Reopened => "reopened",
        /// L2-I16 closure initiated (6C).
        Closing => "closing",
        Closed => "closed",
        Archived => "archived",
    }
);

impl ExamSessionStatus {
    /// Downstream operational workflows (registration, conduct, marks, results) may consume the
    /// session (L2-I02 locked decision 4).
    pub fn is_operational(&self) -> bool {
        matches!(self, Self::Active | Self::Reopened)
    }

    /// Setup (papers, schedule, policy binding) may still change.
    pub fn is_configurable(&self) -> bool {
        matches!(
            self,
            Self::Draft | Self::UnderReview | Self::Approved | Self::Scheduled
        ) || self.is_operational()
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Archived)
    }

    /// Legal transitions (L2-I02 locked decision 3; L2-I16 for closure).
    pub fn can_transition(&self, to: Self) -> bool {
        use ExamSessionStatus::*;
        matches!(
            (self, to),
            (Draft, UnderReview)
                | (UnderReview, Approved)
                | (UnderReview, Draft)
                | (Approved, Scheduled)
                | (Approved, Draft)
                | (Scheduled, Active)
                | (Active, Suspended)
                | (Reopened, Suspended)
                | (Suspended, Reopened)
                | (Active, Closing)
                | (Reopened, Closing)
                | (Closing, Closed)
                | (Closing, Reopened)
                | (Closed, Archived)
        )
    }
}

status_enum!(
    /// Examinable paper of a session (one curriculum component / offering).
    ExamPaperStatus {
        Draft => "draft",
        /// Placed on an approved schedule (6A).
        Scheduled => "scheduled",
        /// Hall tickets issued and seating published; conduct may start (6A).
        Ready => "ready",
        /// Conduct completed (6A).
        Conducted => "conducted",
        /// Marks collection / evaluation underway (6B).
        Evaluation => "evaluation",
        /// Final approved marks established for every candidate (6B).
        MarksFinalised => "marks_finalised",
        Cancelled => "cancelled",
    }
);

impl ExamPaperStatus {
    /// Marks may be entered (internal marks are imported earlier, external after conduct).
    pub fn accepts_marks(&self) -> bool {
        matches!(self, Self::Conducted | Self::Evaluation)
    }

    pub fn can_transition(&self, to: Self) -> bool {
        use ExamPaperStatus::*;
        matches!(
            (self, to),
            (Draft, Scheduled)
                | (Draft, Cancelled)
                | (Scheduled, Ready)
                | (Scheduled, Draft)
                | (Scheduled, Cancelled)
                | (Ready, Conducted)
                | (Ready, Scheduled)
                | (Ready, Cancelled)
                | (Conducted, Evaluation)
                | (Conducted, Ready)
                | (Evaluation, MarksFinalised)
                | (MarksFinalised, Evaluation)
        )
    }
}

status_enum!(
    /// Examination registration lifecycle (L2-I04 §20; `rejected` = L3-I-026 ineligibility outcome).
    ExamRegistrationStatus {
        Draft => "draft",
        PendingValidation => "pending_validation",
        PendingReview => "pending_review",
        Approved => "approved",
        /// Operationally valid registration.
        Confirmed => "confirmed",
        /// Frozen for examination operations (population finalised, L3-I-036).
        Locked => "locked",
        /// Approved registration modified (L3-I-021/022).
        Amended => "amended",
        Withdrawn => "withdrawn",
        Rejected => "rejected",
        Completed => "completed",
        Archived => "archived",
    }
);

impl ExamRegistrationStatus {
    /// The registration counts for hall tickets, seating, marks and results.
    pub fn is_effective(&self) -> bool {
        matches!(
            self,
            Self::Approved | Self::Confirmed | Self::Locked | Self::Amended | Self::Completed
        )
    }

    /// Occupies the candidate's single live registration slot in the session.
    pub fn is_live(&self) -> bool {
        !matches!(self, Self::Withdrawn | Self::Rejected | Self::Archived)
    }

    pub fn can_transition(&self, to: Self) -> bool {
        use ExamRegistrationStatus::*;
        matches!(
            (self, to),
            (Draft, PendingValidation)
                | (Draft, Withdrawn)
                | (PendingValidation, PendingReview)
                | (PendingValidation, Approved)
                | (PendingValidation, Rejected)
                | (PendingReview, Approved)
                | (PendingReview, Rejected)
                | (PendingReview, Draft)
                | (Approved, Confirmed)
                | (Approved, Withdrawn)
                | (Confirmed, Locked)
                | (Confirmed, Amended)
                | (Confirmed, Withdrawn)
                | (Amended, Confirmed)
                | (Amended, Locked)
                | (Amended, Withdrawn)
                | (Locked, Amended)
                | (Locked, Withdrawn)
                | (Locked, Completed)
                | (Withdrawn, Confirmed)
                | (Rejected, Archived)
                | (Withdrawn, Archived)
                | (Completed, Archived)
        )
    }
}

status_enum!(
    /// Marks of one candidate on one paper (`exams.paper_mark`; L2-I08 §20 condensed to the
    /// per-candidate record — the per-paper evaluation lifecycle is 6B's mark sheet).
    MarkStatus {
        /// Row exists (created with the registration paper); nothing entered.
        Pending => "pending",
        /// Marks entered (draft, editable by the evaluator).
        Entered => "entered",
        /// Submitted by the evaluator (L3-I-071).
        Submitted => "submitted",
        /// Verified by the exam cell.
        Verified => "verified",
        /// Changed by an approved moderation (L3-I-075).
        Moderated => "moderated",
        /// Institutionally approved.
        Approved => "approved",
        /// Withheld (incomplete evaluation, malpractice pending; L3-I-076).
        Withheld => "withheld",
        /// Final Approved Marks — the only state results are computed from.
        Finalised => "finalised",
    }
);

impl MarkStatus {
    /// Final Approved Marks (result computation input).
    pub fn is_final(&self) -> bool {
        matches!(self, Self::Finalised)
    }

    /// The evaluator may still edit the marks.
    pub fn is_editable(&self) -> bool {
        matches!(self, Self::Pending | Self::Entered)
    }

    pub fn can_transition(&self, to: Self) -> bool {
        use MarkStatus::*;
        matches!(
            (self, to),
            (Pending, Entered)
                | (Pending, Withheld)
                | (Entered, Entered)
                | (Entered, Submitted)
                | (Entered, Withheld)
                | (Submitted, Entered)
                | (Submitted, Verified)
                | (Submitted, Withheld)
                | (Verified, Moderated)
                | (Verified, Approved)
                | (Verified, Withheld)
                | (Moderated, Approved)
                | (Moderated, Withheld)
                | (Approved, Finalised)
                | (Approved, Withheld)
                | (Withheld, Entered)
                | (Withheld, Verified)
                | (Finalised, Finalised)
        )
    }
}

status_enum!(
    /// Term result of a candidate (`exams.term_result`; L2-I09 §20 + L2-I10 publication states
    /// condensed to the per-candidate record — batch-level states are 6C's result batch).
    ResultStatus {
        Pending => "pending",
        Computed => "computed",
        Validated => "validated",
        Approved => "approved",
        /// Withheld from approval / publication (L3-I-085, 097, 102).
        Withheld => "withheld",
        Published => "published",
        /// Published result withdrawn by an authorised decision (L3-I-109).
        Withdrawn => "withdrawn",
        /// Replaced by a newer revision (recompute, correction, revaluation; L3-I-087/088/107/111).
        Superseded => "superseded",
        Archived => "archived",
    }
);

impl ResultStatus {
    pub fn is_published(&self) -> bool {
        matches!(self, Self::Published)
    }

    /// The current (non-superseded) result of the candidate in the session.
    pub fn is_current(&self) -> bool {
        !matches!(self, Self::Superseded | Self::Archived)
    }

    pub fn can_transition(&self, to: Self) -> bool {
        use ResultStatus::*;
        matches!(
            (self, to),
            (Pending, Computed)
                | (Pending, Withheld)
                | (Pending, Superseded)
                | (Computed, Computed)
                | (Computed, Validated)
                | (Computed, Withheld)
                | (Computed, Superseded)
                | (Validated, Approved)
                | (Validated, Computed)
                | (Validated, Withheld)
                | (Validated, Superseded)
                | (Approved, Published)
                | (Approved, Withheld)
                | (Approved, Superseded)
                | (Withheld, Computed)
                | (Withheld, Approved)
                | (Withheld, Superseded)
                | (Published, Withdrawn)
                | (Published, Superseded)
                | (Withdrawn, Published)
                | (Withdrawn, Superseded)
                | (Superseded, Archived)
                | (Published, Archived)
        )
    }
}

// ================================================================ foundation: classifications

status_enum!(
    /// Session classification (L2-I02 locked decision 7: classifications, not lifecycle events).
    ExamSessionKind {
        Regular => "regular",
        Supplementary => "supplementary",
        Improvement => "improvement",
        Makeup => "makeup",
        Special => "special",
    }
);

status_enum!(
    /// Mode of an examinable paper.
    ExamMode {
        Theory => "theory",
        Practical => "practical",
        Viva => "viva",
        Project => "project",
    }
);

status_enum!(
    /// One registered paper of a registration.
    RegistrationPaperStatus {
        Registered => "registered",
        Withdrawn => "withdrawn",
        /// Did not appear (conduct, 6A) — results apply the absence consequence (L3-I-114).
        Absent => "absent",
        Appeared => "appeared",
        /// Entry denied / debarred (failed entry condition or malpractice penalty).
        Debarred => "debarred",
    }
);

status_enum!(
    /// Attempt kind of a registered paper (L2-I12).
    AttemptKind {
        Regular => "regular",
        Backlog => "backlog",
        Supplementary => "supplementary",
        Improvement => "improvement",
    }
);

status_enum!(
    /// How a registration was created (L3-I-017/018/019/034).
    RegistrationChannel {
        SelfService => "self_service",
        Bulk => "bulk",
        Administrative => "administrative",
        Late => "late",
    }
);

status_enum!(
    /// Grading scheme status (foundation API).
    GradingSchemeStatus { Active => "active", Retired => "retired" }
);

status_enum!(
    /// Outcome of a computed term result.
    ResultOutcome {
        Pass => "pass",
        Fail => "fail",
        Withheld => "withheld",
        Incomplete => "incomplete",
        Malpractice => "malpractice",
    }
);

// ================================================================ 6A (transitions slice-owned)

status_enum!(
    /// Examination policy version (L2-I01 §20).
    ExamPolicyStatus {
        Draft => "draft",
        UnderReview => "under_review",
        Approved => "approved",
        Scheduled => "scheduled",
        Active => "active",
        Superseded => "superseded",
        Retired => "retired",
        Archived => "archived",
    }
);

status_enum!(
    /// Examination schedule / timetable (L2-I03 §20).
    ExamScheduleStatus {
        Draft => "draft",
        UnderReview => "under_review",
        Approved => "approved",
        Published => "published",
        Active => "active",
        Revised => "revised",
        Cancelled => "cancelled",
        Completed => "completed",
        Archived => "archived",
    }
);

status_enum!(
    /// Hall ticket (L2-I05 §20; `withheld` = L3-I-038 unsatisfied conditions).
    HallTicketStatus {
        Draft => "draft",
        UnderReview => "under_review",
        Approved => "approved",
        Published => "published",
        Active => "active",
        Withheld => "withheld",
        Reissued => "reissued",
        Replaced => "replaced",
        Withdrawn => "withdrawn",
        Cancelled => "cancelled",
        Completed => "completed",
        Archived => "archived",
    }
);

status_enum!(
    /// Seating plan of a sitting (L3-I-015/059).
    SeatingPlanStatus {
        Draft => "draft",
        Published => "published",
        Revised => "revised",
        Cancelled => "cancelled",
    }
);

status_enum!(
    /// Invigilation / examiner duty (L3-I-010/011/012/060).
    InvigilationDutyStatus {
        Assigned => "assigned",
        Confirmed => "confirmed",
        Replaced => "replaced",
        Released => "released",
        Completed => "completed",
    }
);

status_enum!(
    /// Conduct of a sitting (L2-I06/I07 condensed; L3-I-043/055/174/175/185).
    ConductStatus {
        Scheduled => "scheduled",
        InProgress => "in_progress",
        Suspended => "suspended",
        Completed => "completed",
        Terminated => "terminated",
        Postponed => "postponed",
    }
);

status_enum!(
    /// Examination exception case (L2-I15; L3-I-172).
    ExamExceptionStatus {
        Raised => "raised",
        Classified => "classified",
        UnderReview => "under_review",
        Resolved => "resolved",
        Closed => "closed",
    }
);

// ================================================================ 6B (transitions slice-owned)

status_enum!(
    /// Per-paper evaluation lifecycle of a mark sheet (L2-I08 §20).
    EvaluationStatus {
        Pending => "pending",
        Planned => "planned",
        Assigned => "assigned",
        InEvaluation => "in_evaluation",
        Evaluated => "evaluated",
        Moderation => "moderation",
        Validated => "validated",
        Approved => "approved",
        Finalised => "finalised",
        Closed => "closed",
        Archived => "archived",
    }
);

status_enum!(
    /// Evaluation round of an answer script (L3-I-080/081/082).
    EvaluationRound {
        First => "first",
        Second => "second",
        Adjudication => "adjudication",
    }
);

status_enum!(
    /// Moderation of a paper (L3-I-075).
    ModerationStatus {
        Requested => "requested",
        Validated => "validated",
        Approved => "approved",
        Applied => "applied",
        Rejected => "rejected",
        Withdrawn => "withdrawn",
    }
);

status_enum!(
    /// Malpractice case (L2-I14 §21; L3-I-162…171).
    MalpracticeCaseStatus {
        Reported => "reported",
        UnderInvestigation => "under_investigation",
        Hearing => "hearing",
        Decided => "decided",
        Appealed => "appealed",
        UnderReview => "under_review",
        Upheld => "upheld",
        Modified => "modified",
        Overturned => "overturned",
        Closed => "closed",
        Archived => "archived",
    }
);

status_enum!(
    /// Penalty of a malpractice decision (consumed by 6C result processing, L3-I-095/112).
    MalpracticePenalty {
        None => "none",
        Warning => "warning",
        MarksDeduction => "marks_deduction",
        PaperCancelled => "paper_cancelled",
        SessionCancelled => "session_cancelled",
        Debarred => "debarred",
    }
);

// ================================================================ 6C (transitions slice-owned)

status_enum!(
    /// Result computation run of a session (L2-I09 §20).
    ResultBatchStatus {
        Pending => "pending",
        Aggregated => "aggregated",
        RuleProcessing => "rule_processing",
        Computed => "computed",
        Validated => "validated",
        Approved => "approved",
        Finalised => "finalised",
        ReadyForPublication => "ready_for_publication",
        Closed => "closed",
        Archived => "archived",
    }
);

status_enum!(
    /// Result publication (L2-I10 §20 + delay / cancellation / withdrawal of L3-I-104…110).
    PublicationStatus {
        Planned => "planned",
        Scheduled => "scheduled",
        Authorised => "authorised",
        Ready => "ready",
        Published => "published",
        Accessible => "accessible",
        Delayed => "delayed",
        Cancelled => "cancelled",
        Failed => "failed",
        Withdrawn => "withdrawn",
        Closed => "closed",
        Archived => "archived",
    }
);

status_enum!(
    /// Grade card issued from a published result.
    GradeCardStatus {
        Issued => "issued",
        Reissued => "reissued",
        Withdrawn => "withdrawn",
    }
);

status_enum!(
    /// Kind of a post-result review (L2-I11).
    ReviewKind { Revaluation => "revaluation", Rechecking => "rechecking" }
);

status_enum!(
    /// Revaluation / rechecking request (L2-I11 §20 + exception amendment 2026-09-03).
    RevaluationStatus {
        Submitted => "submitted",
        EligibilityVerified => "eligibility_verified",
        /// Fee charge raised in Finance, awaiting settlement (`ExamFeePort`).
        FeePending => "fee_pending",
        FeeVerified => "fee_verified",
        Approved => "approved",
        ReviewInProgress => "review_in_progress",
        ReviewCompleted => "review_completed",
        OutcomeVerified => "outcome_verified",
        RevisedMarksApproved => "revised_marks_approved",
        ResultRevisionInitiated => "result_revision_initiated",
        CorrectionReview => "correction_review",
        Withdrawn => "withdrawn",
        Rejected => "rejected",
        Closed => "closed",
        Archived => "archived",
    }
);

status_enum!(
    /// Subsequent-attempt eligibility (L2-I12; L3-I-133/138/142).
    SupplementaryEligibilityStatus {
        Eligible => "eligible",
        Applied => "applied",
        Authorised => "authorised",
        Exhausted => "exhausted",
        Cancelled => "cancelled",
    }
);

status_enum!(
    /// Academic completion evaluation (L2-I13; L3-I-145/150/153).
    CompletionEvaluationStatus {
        Pending => "pending",
        Eligible => "eligible",
        NotEligible => "not_eligible",
        Blocked => "blocked",
        Superseded => "superseded",
    }
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_names_roundtrip() {
        for s in ExamSessionStatus::ALL {
            assert_eq!(ExamSessionStatus::parse(s.as_str()), Some(*s));
        }
        for s in MarkStatus::ALL {
            assert_eq!(MarkStatus::parse(s.as_str()), Some(*s));
        }
        assert_eq!(ResultStatus::parse("Published"), None);
        assert_eq!(
            serde_json::to_value(ExamRegistrationStatus::PendingValidation).ok(),
            Some(serde_json::json!("pending_validation"))
        );
    }

    #[test]
    fn session_lifecycle_follows_the_locked_l2_decisions() {
        use ExamSessionStatus::*;
        assert!(Draft.can_transition(UnderReview));
        assert!(
            !Draft.can_transition(Active),
            "only reviewed sessions are approved"
        );
        assert!(
            !Approved.can_transition(Active),
            "approved → scheduled → active"
        );
        assert!(Scheduled.can_transition(Active));
        assert!(Active.can_transition(Suspended) && Suspended.can_transition(Reopened));
        assert!(!Suspended.can_transition(Active));
        assert!(Active.can_transition(Closing) && Closing.can_transition(Closed));
        assert!(!Archived.can_transition(Draft));
        let operational: Vec<_> = ExamSessionStatus::ALL
            .iter()
            .filter(|s| s.is_operational())
            .collect();
        assert_eq!(operational, [&Active, &Reopened]);
    }

    #[test]
    fn marks_and_results_only_move_forward_through_review() {
        use MarkStatus as M;
        assert!(M::Entered.can_transition(M::Submitted));
        assert!(!M::Entered.can_transition(M::Finalised));
        assert!(M::Approved.can_transition(M::Finalised));
        assert!(
            !M::Finalised.can_transition(M::Entered),
            "revisions go through revise_final"
        );
        assert!(M::Finalised.is_final() && !M::Approved.is_final());
        use ResultStatus as R;
        assert!(R::Approved.can_transition(R::Published));
        assert!(
            !R::Computed.can_transition(R::Published),
            "publication needs approval"
        );
        assert!(
            R::Published.can_transition(R::Withdrawn) && R::Withdrawn.can_transition(R::Published)
        );
        assert!(!R::Superseded.is_current());
        use ExamRegistrationStatus as G;
        assert!(G::Locked.is_effective() && !G::Withdrawn.is_live());
        assert!(
            G::Withdrawn.can_transition(G::Confirmed),
            "reinstatement (L3-I-024)"
        );
        use ExamPaperStatus as P;
        assert!(P::Conducted.accepts_marks() && !P::Ready.accepts_marks());
        assert!(P::Evaluation.can_transition(P::MarksFinalised));
    }
}
