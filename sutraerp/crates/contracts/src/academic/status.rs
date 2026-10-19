//! Lifecycle state enums of the Batch 4 (Domain D) aggregates.
//!
//! Foundation-owned. Wire/DB form is the `snake_case` name, stored in a `text` column with a
//! `CHECK (status IN (...))` written by the owning migration. Adding a variant is a contract
//! change (edit `docs/batch4-contracts.md` §4.2).
//!
//! Transitions: the **shared** machines ([`CourseOfferingStatus`], [`SectionStatus`],
//! [`FacultyAssignmentStatus`], [`SectionEnrolmentStatus`]) carry their legal transitions here
//! (`can_transition`) because their rows live in foundation tables every slice reads, and
//! 4B/4C gate on them. Only **4A** moves them. Every other machine's transitions are
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

// ================================================================ foundation (shared machines)

status_enum!(
    /// Course Offering lifecycle (L2-D01 §10 + §71 state transition matrix).
    CourseOfferingStatus {
        Draft => "draft",
        PendingApproval => "pending_approval",
        Approved => "approved",
        Published => "published",
        EnrollmentOpen => "enrollment_open",
        EnrollmentClosed => "enrollment_closed",
        /// Teaching under way: sessions, attendance and coursework run only here.
        Active => "active",
        Suspended => "suspended",
        /// Operational changes prohibited until a governed unfreeze (L3-D-013).
        Frozen => "frozen",
        Completed => "completed",
        Archived => "archived",
        Cancelled => "cancelled",
        /// Terminal: absorbed into another offering (L3-D-011).
        Merged => "merged",
        /// Terminal: replaced by split offerings (L3-D-011).
        Split => "split",
    }
);

impl CourseOfferingStatus {
    /// Legal transitions (L2-D01 §71). `active → approved` is the governed rollback of §71.
    pub fn can_transition(self, to: CourseOfferingStatus) -> bool {
        use CourseOfferingStatus::*;
        matches!(
            (self, to),
            (Draft, PendingApproval)
                | (Draft, Cancelled)
                | (PendingApproval, Approved)
                | (PendingApproval, Draft)
                | (Approved, Published)
                | (Approved, Cancelled)
                | (Published, EnrollmentOpen)
                | (Published, Cancelled)
                | (EnrollmentOpen, EnrollmentClosed)
                | (EnrollmentClosed, EnrollmentOpen)
                | (EnrollmentClosed, Active)
                | (Active, Suspended)
                | (Suspended, Active)
                | (Suspended, Cancelled)
                | (Active, Completed)
                | (Completed, Archived)
                | (Cancelled, Archived)
                | (Active, Frozen)
                | (Frozen, Active)
                | (Active, Merged)
                | (Active, Split)
                | (Active, Approved)
        )
    }

    /// Students may hold seats (enrol / be enrolled) in these states.
    pub fn accepts_enrolment(self) -> bool {
        matches!(
            self,
            CourseOfferingStatus::EnrollmentOpen | CourseOfferingStatus::Active
        )
    }

    /// Delivery (timetable, sessions, attendance, coursework) may run in these states.
    pub fn is_deliverable(self) -> bool {
        matches!(
            self,
            CourseOfferingStatus::EnrollmentClosed
                | CourseOfferingStatus::EnrollmentOpen
                | CourseOfferingStatus::Active
        )
    }

    /// No further change of any kind.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            CourseOfferingStatus::Archived
                | CourseOfferingStatus::Merged
                | CourseOfferingStatus::Split
        )
    }
}

status_enum!(
    /// Section of an offering.
    SectionStatus {
        Planned => "planned",
        Active => "active",
        Completed => "completed",
        Cancelled => "cancelled",
        /// Terminal: roster moved into another section (L3-D-011).
        Merged => "merged",
    }
);

impl SectionStatus {
    pub fn can_transition(self, to: SectionStatus) -> bool {
        use SectionStatus::*;
        matches!(
            (self, to),
            (Planned, Active)
                | (Planned, Cancelled)
                | (Active, Completed)
                | (Active, Cancelled)
                | (Active, Merged)
        )
    }
}

status_enum!(
    /// Faculty academic assignment lifecycle (L2-D02 §6.2; `rejected` / `withdrawn` / `revoked` close the governed paths).
    FacultyAssignmentStatus {
        Draft => "draft",
        Submitted => "submitted",
        UnderReview => "under_review",
        Approved => "approved",
        /// The faculty member teaches the section (faculty-scoped permissions apply only here).
        Active => "active",
        Completed => "completed",
        Archived => "archived",
        Rejected => "rejected",
        Withdrawn => "withdrawn",
        /// Ended early by an operational change (L3-D-018).
        Revoked => "revoked",
    }
);

impl FacultyAssignmentStatus {
    pub fn can_transition(self, to: FacultyAssignmentStatus) -> bool {
        use FacultyAssignmentStatus::*;
        matches!(
            (self, to),
            (Draft, Submitted)
                | (Draft, Withdrawn)
                | (Submitted, UnderReview)
                | (Submitted, Approved)
                | (Submitted, Rejected)
                | (Submitted, Withdrawn)
                | (UnderReview, Approved)
                | (UnderReview, Rejected)
                | (Approved, Active)
                | (Approved, Withdrawn)
                | (Active, Completed)
                | (Active, Revoked)
                | (Completed | Revoked | Rejected | Withdrawn, Archived)
        )
    }
}

status_enum!(
    /// Teaching role of a faculty assignment.
    FacultyRole {
        PrimaryInstructor => "primary_instructor",
        CoInstructor => "co_instructor",
        Tutor => "tutor",
        LabInstructor => "lab_instructor",
        TeachingAssistant => "teaching_assistant",
        CourseCoordinator => "course_coordinator",
    }
);

status_enum!(
    /// A student's seat in a section.
    SectionEnrolmentStatus {
        Enrolled => "enrolled",
        /// Left the section by own choice (drop inside the window).
        Dropped => "dropped",
        /// Removed after the window (withdrawal, registration cancelled, context closed).
        Withdrawn => "withdrawn",
        /// Moved to another section (the new seat carries `transferred_from_id`).
        Transferred => "transferred",
        Completed => "completed",
    }
);

impl SectionEnrolmentStatus {
    pub fn can_transition(self, to: SectionEnrolmentStatus) -> bool {
        use SectionEnrolmentStatus::*;
        matches!(
            (self, to),
            (Enrolled, Dropped)
                | (Enrolled, Withdrawn)
                | (Enrolled, Transferred)
                | (Enrolled, Completed)
        )
    }

    /// Counts against capacity and appears on attendance / coursework rosters.
    pub fn is_seated(self) -> bool {
        self == SectionEnrolmentStatus::Enrolled
    }
}

// ================================================================ generic (several slices)

status_enum!(
    /// Effective-dated configuration version (4A assignment policy, 4B attendance policy, 4C mapping rules / flexible policy).
    PolicyVersionStatus {
        Draft => "draft",
        InReview => "in_review",
        Effective => "effective",
        Superseded => "superseded",
        Retired => "retired",
    }
);
status_enum!(
    /// Governed change request (L2-D02 operational change: requested → validated → approved → applied).
    ChangeRequestStatus {
        Requested => "requested",
        Validated => "validated",
        Approved => "approved",
        Applied => "applied",
        Rejected => "rejected",
        Withdrawn => "withdrawn",
    }
);

// ================================================================ 4A

status_enum!(
    /// Offering waitlist entry (L3-D-007).
    WaitlistStatus {
        Waiting => "waiting",
        Offered => "offered",
        Accepted => "accepted",
        Expired => "expired",
        Removed => "removed",
    }
);
status_enum!(
    /// Offering enrolment window (L2-D01 enrollment states).
    EnrolmentWindowStatus {
        Scheduled => "scheduled",
        Open => "open",
        Suspended => "suspended",
        Closed => "closed",
        ClosedPermanently => "closed_permanently",
    }
);

// ================================================================ 4B

status_enum!(
    /// Timetable version lifecycle (L2-D03 §12.4).
    TimetableStatus {
        Planning => "planning",
        Draft => "draft",
        Validated => "validated",
        Approved => "approved",
        Published => "published",
        Active => "active",
        Frozen => "frozen",
        Revised => "revised",
        Retired => "retired",
        Archived => "archived",
    }
);
status_enum!(
    /// Teaching session lifecycle (L2-D04 §21.1).
    TeachingSessionStatus {
        Scheduled => "scheduled",
        Started => "started",
        InProgress => "in_progress",
        Paused => "paused",
        Completed => "completed",
        Cancelled => "cancelled",
        Abandoned => "abandoned",
        Locked => "locked",
        Archived => "archived",
    }
);
status_enum!(
    /// One attendance mark (L2-D04 attendance status).
    AttendanceMark {
        Present => "present",
        Absent => "absent",
        Late => "late",
        Excused => "excused",
    }
);
status_enum!(
    /// Attendance capture state of a session (L3-D-024).
    AttendanceCaptureStatus {
        NotStarted => "not_started",
        Capturing => "capturing",
        Submitted => "submitted",
        Finalised => "finalised",
    }
);
status_enum!(
    /// Topic coverage state (L2-D04 syllabus progress).
    TopicCoverageStatus {
        Planned => "planned",
        Started => "started",
        Completed => "completed",
        Deferred => "deferred",
        Cancelled => "cancelled",
    }
);
status_enum!(
    /// Syllabus coverage record (L3-D-026).
    SyllabusCoverageStatus {
        Draft => "draft",
        Submitted => "submitted",
        Published => "published",
    }
);
status_enum!(
    /// Academic monitoring case (L2-D05, L3-D-027…033).
    MonitoringCaseStatus {
        Open => "open",
        UnderAssessment => "under_assessment",
        Intervention => "intervention",
        UnderReview => "under_review",
        Certified => "certified",
        Closed => "closed",
        Reopened => "reopened",
    }
);
status_enum!(
    /// Academic risk level (L3-D-028).
    RiskLevel {
        Low => "low",
        Moderate => "moderate",
        High => "high",
        Critical => "critical",
    }
);
status_enum!(
    /// Monitoring intervention (L3-D-030).
    InterventionStatus {
        Planned => "planned",
        Triggered => "triggered",
        InProgress => "in_progress",
        Completed => "completed",
        Cancelled => "cancelled",
    }
);
status_enum!(
    /// Internal assessment blueprint (L2-D07 §5).
    AssessmentBlueprintStatus {
        Draft => "draft",
        Published => "published",
        Superseded => "superseded",
        Archived => "archived",
    }
);
status_enum!(
    /// Kind of an assessment window (coursework assignments are `assignment`).
    AssessmentKind {
        Assignment => "assignment",
        Quiz => "quiz",
        Test => "test",
        Lab => "lab",
        Project => "project",
        Presentation => "presentation",
        Viva => "viva",
    }
);
status_enum!(
    /// Assessment window / assignment (L3-D-052).
    AssessmentWindowStatus {
        Scheduled => "scheduled",
        Open => "open",
        Extended => "extended",
        Closed => "closed",
        Cancelled => "cancelled",
    }
);
status_enum!(
    /// A student's submission (L3-D-053/054).
    AssessmentSubmissionStatus {
        Pending => "pending",
        Submitted => "submitted",
        Late => "late",
        Marked => "marked",
        Moderated => "moderated",
        Finalized => "finalized",
        Absent => "absent",
    }
);
status_enum!(
    /// Section-level internal assessment (L2-D07 §5: … → moderated → finalized → archived).
    InternalAssessmentStatus {
        InProgress => "in_progress",
        Moderated => "moderated",
        Finalized => "finalized",
        Corrected => "corrected",
        Archived => "archived",
    }
);

// ================================================================ 4C

status_enum!(
    /// External learning provider (L3-D-044).
    LearningProviderStatus {
        Proposed => "proposed",
        UnderReview => "under_review",
        Active => "active",
        Suspended => "suspended",
        Offboarded => "offboarded",
    }
);
status_enum!(
    /// Flexible learning opportunity (L3-D-040…042).
    LearningOpportunityStatus {
        Draft => "draft",
        PendingApproval => "pending_approval",
        Active => "active",
        Suspended => "suspended",
        Archived => "archived",
    }
);
status_enum!(
    /// Mode of a learning opportunity (L2-D06).
    LearningMode {
        OpenElective => "open_elective",
        Mooc => "mooc",
        Swayam => "swayam",
        Nptel => "nptel",
        Internship => "internship",
        ExternalCourse => "external_course",
    }
);
status_enum!(
    /// Flexible-learning enrolment (L3-D-034…038, 048).
    FlexibleEnrolmentStatus {
        Requested => "requested",
        Eligible => "eligible",
        Allocated => "allocated",
        Waitlisted => "waitlisted",
        Registered => "registered",
        InProgress => "in_progress",
        Completed => "completed",
        Withdrawn => "withdrawn",
        Rejected => "rejected",
    }
);
status_enum!(
    /// External certificate verification (L3-D-043).
    CertificateVerificationStatus {
        Submitted => "submitted",
        UnderVerification => "under_verification",
        Accepted => "accepted",
        Rejected => "rejected",
        Flagged => "flagged",
    }
);
status_enum!(
    /// Credit recognition and posting request (L3-D-039).
    CreditRecognitionStatus {
        Requested => "requested",
        UnderReview => "under_review",
        Approved => "approved",
        /// Credits written to the Domain C ledger through `CreditPostingPort`.
        Posted => "posted",
        Rejected => "rejected",
    }
);
status_enum!(
    /// Course resource / library link.
    CourseResourceStatus {
        Draft => "draft",
        Published => "published",
        Withdrawn => "withdrawn",
    }
);
status_enum!(
    /// Kind of a course resource.
    CourseResourceKind {
        Document => "document",
        Link => "link",
        Video => "video",
        Reading => "reading",
        LibraryItem => "library_item",
    }
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_names_roundtrip() {
        for s in CourseOfferingStatus::ALL {
            assert_eq!(CourseOfferingStatus::parse(s.as_str()), Some(*s));
        }
        for s in FacultyAssignmentStatus::ALL {
            assert_eq!(FacultyAssignmentStatus::parse(s.as_str()), Some(*s));
        }
        assert_eq!(
            serde_json::to_value(CourseOfferingStatus::EnrollmentOpen).ok(),
            Some(serde_json::json!("enrollment_open"))
        );
        assert_eq!(CourseOfferingStatus::parse("Active"), None);
    }

    #[test]
    fn offering_matrix_matches_l2_d01() {
        use CourseOfferingStatus::*;
        assert!(Draft.can_transition(PendingApproval));
        assert!(PendingApproval.can_transition(Draft));
        assert!(EnrollmentClosed.can_transition(Active));
        assert!(Active.can_transition(Approved), "governed rollback");
        assert!(!Draft.can_transition(Active));
        assert!(!Archived.can_transition(Active));
        for s in CourseOfferingStatus::ALL {
            if s.is_terminal() {
                assert!(
                    CourseOfferingStatus::ALL
                        .iter()
                        .all(|t| !s.can_transition(*t)),
                    "{s}"
                );
            }
        }
        assert!(Active.accepts_enrolment() && !Draft.accepts_enrolment());
    }

    #[test]
    fn shared_machines() {
        assert!(SectionStatus::Planned.can_transition(SectionStatus::Active));
        assert!(!SectionStatus::Merged.can_transition(SectionStatus::Active));
        use FacultyAssignmentStatus as F;
        assert!(F::Approved.can_transition(F::Active));
        assert!(!F::Draft.can_transition(F::Active));
        assert!(F::Revoked.can_transition(F::Archived));
        use SectionEnrolmentStatus as E;
        assert!(E::Enrolled.can_transition(E::Dropped));
        assert!(!E::Dropped.can_transition(E::Enrolled));
        assert!(E::Enrolled.is_seated() && !E::Withdrawn.is_seated());
    }
}
