//! Authenticated execution context and the D02 authorization policy (ADR-L5-0024 / ADR-L5-0038).
//!
//! * [`Role`] — coarse application role carried by the authenticated principal.
//! * [`Permission`] — typed, fine-grained action a handler can require.
//! * [`role_permissions`] — the **deny-by-default** role → permission map: a permission that is not
//!   listed for one of the caller's roles is refused. There is no wildcard and no implicit grant.
//! * [`AuthContext`] — what the HTTP layer knows about an authenticated request once its session
//!   has been resolved. Its [`ExecutionScope`] is the *only* scope authority for protected
//!   operations (ADR-L5-0004); client headers never widen or replace it.
//! * [`SessionResolver`] — port the HTTP layer uses to turn an opaque session token into an
//!   [`AuthContext`]. Implemented by the platform module (D01) over its session store.

use crate::error::KernelError;
use crate::scope::ExecutionScope;
use chrono::{DateTime, Utc};
use std::fmt;
use uuid::Uuid;

/// Application role as presented by the web client. Authorization decisions use the
/// [`Permission`] map below, never the role name directly.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum Role {
    Administrator,
    Faculty,
    Student,
}

impl Role {
    pub const ALL: [Role; 3] = [Role::Administrator, Role::Faculty, Role::Student];

    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Administrator => "administrator",
            Role::Faculty => "faculty",
            Role::Student => "student",
        }
    }

    /// Strict parse of the stored/wire name. Unknown names yield `None` (and therefore no grants).
    pub fn parse(s: &str) -> Option<Role> {
        Role::ALL.into_iter().find(|r| r.as_str() == s)
    }

    /// Privileged roles require MFA (ADR-L5-0038 §3.8).
    pub fn is_privileged(&self) -> bool {
        matches!(self, Role::Administrator)
    }
}

/// Authentication assurance of a session (ADR-L5-0038 §3.8).
///
/// `Mfa` means the session was established (or stepped up) with a second factor: a TOTP code
/// verified by SutraERP in dev mode, or Keycloak-asserted `amr`/`acr` values in Keycloak mode.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    serde::Serialize,
    serde::Deserialize,
    utoipa::ToSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum MfaLevel {
    /// Single factor (password / federated login without a second factor).
    #[default]
    None,
    /// A second factor was verified for this session.
    Mfa,
}

impl MfaLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            MfaLevel::None => "none",
            MfaLevel::Mfa => "mfa",
        }
    }

    /// Strict parse of the stored name; unknown values fail closed to `None`.
    pub fn parse(s: &str) -> MfaLevel {
        if s == "mfa" {
            MfaLevel::Mfa
        } else {
            MfaLevel::None
        }
    }
}

/// A protected action. Add a variant per new capability; it is denied to every role until it is
/// explicitly granted in [`role_permissions`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Permission {
    /// Read institutions of the caller's tenant.
    InstitutionRead,
    /// Register a new institution in the caller's tenant.
    InstitutionRegister,
    /// Rename an institution of the caller's tenant.
    InstitutionRename,
    /// Read one's own notifications (inbox, unread count, mark read).
    NotificationRead,
    /// Read the notices addressed to one of the caller's roles.
    NoticeRead,
    /// Create, update and archive institution-wide notices (and see every notice).
    NoticeManage,
    /// Use one's own to-do inbox (create for oneself, complete, dismiss, reopen).
    TodoUse,
    /// Assign a to-do to another person of the tenant.
    TodoAssign,
    /// View and update one's own profile, preferences and sessions.
    ProfileManage,
    // ---- Batch 2 (Domain B). Reads of every Domain B resource use `InstitutionRead`. ----
    // 2A
    /// Configure an institution: profile revision, business timezone, configure + submit for
    /// activation, withdraw a returned request.
    InstitutionManage,
    /// Suspend, reactivate and archive an institution (step-up).
    InstitutionLifecycle,
    /// Decide an institution activation request: approve / reject / return (step-up). The
    /// abstract "Institution Activation Authority" of the L2-B01 amendment.
    InstitutionActivationDecide,
    /// Draft and make effective institutional business policies.
    InstitutionPolicyManage,
    /// Organisation units, type blueprints, governance configuration, restructuring.
    OrganisationManage,
    /// Effective-dated position definitions.
    PositionManage,
    // 2B
    /// Register, activate, modify, suspend and restore campuses.
    CampusManage,
    /// Decommission and archive campuses (step-up).
    CampusDecommission,
    /// Draft academic structure, programmes, curriculum versions and the school model.
    CurriculumManage,
    /// Publish / supersede / retire academic structure, programmes, curricula, school model.
    CurriculumPublish,
    // 2C
    /// Draft calendars, years, terms, key dates, assignments, exceptions and simulations.
    CalendarManage,
    /// Publish calendar versions, open/close academic years, approve exceptions, retire.
    CalendarPublish,
    /// Read the institution activation readiness checklist.
    InstitutionReadinessRead,
    // ---- Batch 3 (Domain C). Staff reads use `StudentRead`; students use `StudentSelfService`. ----
    /// Read students, StudentContexts, Student 360 and every Domain C record of the tenant (staff).
    StudentRead,
    /// Self-service on one's **own** student record (resolved from the session subject): view Student 360, request documents/certificates, update communication preferences.
    StudentSelfService,
    /// 3A: applications, review, offers, admission documents and verification, cancellation, withdrawal, expiry, readmission initiation.
    AdmissionManage,
    /// 3A: admission approval decision, financial-clearance decision, confirmation (the Admission Authority).
    AdmissionDecide,
    /// 3A: revoke an approved/confirmed admission (step-up).
    AdmissionRevoke,
    /// 3A: student creation/activation/enrolment, enrolment updates, academic assignment, intra-institute transfer, profile, communication preferences, document/certificate request processing.
    StudentManage,
    /// 3A: suspend, restore and inter-institute transfer of a student (step-up).
    StudentLifecycle,
    /// 3A: governed identity correction and duplicate resolution (step-up).
    StudentIdentityManage,
    /// 3A: student context archival and permanent record closure (step-up).
    StudentArchive,
    /// 3B: registration windows, holds, staff registration actions, approval, cancellation, finalisation.
    RegistrationManage,
    /// 3B: a student's own registration, add/drop and cancellation request inside an open window.
    RegistrationSelf,
    /// 3B: progression evaluation drafts, credit ledger, conditions, probation, detention, credit transfer, programme migration, academic break/rejoin, qualification eligibility, ABC sync.
    ProgressionManage,
    /// 3B: make progression decisions effective and revise them after re-evaluation/appeal.
    ProgressionDecide,
    /// 3B: institutional progression policy lifecycle.
    ProgressionPolicyManage,
    /// 3C: qualification completion evaluation, audit, clearance, deferral; exit cases and exit clearance; administrative closure.
    CompletionManage,
    /// 3C: approve and award (confer) a qualification, finalise the academic record, correct a conferred qualification (step-up).
    QualificationConfer,
    /// 3C: revoke a conferred qualification (step-up).
    QualificationRevoke,
    /// 3C: approve student exits incl. termination and deceased record closure (step-up).
    ExitDecide,
    /// 3C: guardian relationships, custody, consent, communication authorisation, emergency contacts, financial responsibility, pickup authorisation.
    GuardianManage,
    /// 3C: scholarship drafts, eligibility determinations, reassessment, renewal, bulk recalculation, external sync, financial-aid hand-off.
    ScholarshipManage,
    /// 3C: publish/activate/suspend/expire/archive scholarship definitions and eligibility policies.
    ScholarshipPublish,
    /// 3C: student → alumni transition, correction and archival.
    AlumniManage,
    // ---- Batch 4 (Domain D). Staff reads use `AcademicRead`; students use `AcademicSelfService`;
    // faculty act on their own sections with `AcademicTeach`. ----
    /// Read offerings, sections, rosters, timetables, sessions, attendance, coursework and every Domain D record of the tenant (staff).
    AcademicRead,
    /// Self-service on one's **own** academic delivery (resolved from the session subject): timetable, courses, attendance, assignments and submissions, resources, flexible-learning requests.
    AcademicSelfService,
    /// Faculty-scoped teaching actions (sessions, attendance capture, syllabus coverage, coursework marking, resources) — only on sections where the caller holds an **active** faculty assignment (checked by `sutra_academic::shared::faculty`).
    AcademicTeach,
    /// 4A: course offering drafts, sections, capacity, quotas, enrolment windows, templates, simulation, revisions.
    OfferingManage,
    /// 4A: offering approval decision, publication, enrolment open/close, activation, suspension/reactivation, freeze.
    OfferingDecide,
    /// 4A: cancel, archive, merge/split, governed unfreeze and rollback of offerings (step-up).
    OfferingLifecycle,
    /// 4A: seat students in sections, withdraw/transfer seats, waitlist resolution, eligibility overrides.
    OfferingEnrolmentManage,
    /// 4A: faculty assignment drafts, operational changes, governance configuration.
    FacultyAssignmentManage,
    /// 4A: approve / reject / activate / revoke faculty assignments.
    FacultyAssignmentDecide,
    /// 4B: scheduling requirements, timetable drafts and slots, validation, revision requests, session scheduling.
    TimetableManage,
    /// 4B: approve, publish, activate, freeze, revise and retire timetables.
    TimetablePublish,
    /// 4B: attendance policy, capture and finalisation on any section, threshold evaluation, syllabus coverage publication.
    AttendanceManage,
    /// 4B: governed correction of finalised attendance (step-up).
    AttendanceCorrect,
    /// 4B: academic monitoring cases, risk assessment, exceptions, interventions, governance review, closure and reopening.
    MonitoringManage,
    /// 4B: internal assessment blueprints, windows / assignments, moderation on any section.
    AssessmentManage,
    /// 4B: finalise, archive and correct internal assessment (step-up).
    AssessmentFinalize,
    /// 4C: providers, opportunities, policies, credit mapping rules, allocation, external registration, completion, certificate verification, reconciliation, exceptions.
    FlexibleLearningManage,
    /// 4C: approve credit recognition and post credits to the Domain C ledger (step-up).
    CreditRecognitionDecide,
    /// 4C: course resources and library links on any offering / section.
    CourseResourceManage,
    // ---- Batch 5 (Domain F). Staff reads use `FinanceRead`; students use `FinanceSelfService`. ----
    /// Read fee heads, fee structures, charges, party accounts and balances, payments, receipts, refunds, receivables, aid, the ledger, periods and treasury records of the tenant (staff).
    FinanceRead,
    /// Self-service on one's **own** fees (resolved from the session subject): charges, instalments, concessions/aid, payments, receipts, refund requests.
    FinanceSelfService,
    /// 5A: fee heads, fee structures, charge generation / validation / publication, revision, suspension / resumption, disputes, instalment schedules, settlement closure.
    ChargeManage,
    /// 5A: credit notes, debit notes, write-off / waiver adjustments (draft + submit), reversal requests, corrected successors.
    AdjustmentManage,
    /// 5A: financial aid allocation from handed-off scholarship decisions, disbursement / charge adjustment, renewal, revocation and recovery, programme reconciliation.
    AidManage,
    /// 5A: governed finance configuration and financial rule versions (draft, review, activation, rollback).
    FinanceConfigManage,
    /// 5B: counter / bank payments, allocation, receipts, unidentified receipts, failure recovery, cancellation, bulk collection.
    PaymentManage,
    /// 5B: refund requests on behalf of students, processing, retry, closure.
    RefundManage,
    /// 5B: receivable recognition, dunning, settlement review, write-off requests, reopening.
    ReceivableManage,
    /// 5C: chart of accounts, manual journal drafts, accounting batches, reconciliation, trial balance.
    LedgerManage,
    /// 5C: financial years and periods (establish, open, lock), close preparation.
    PeriodManage,
    /// 5C: bank accounts, bank execution, statement import and reconciliation, cash position, fund transfer requests, failed bank transactions.
    TreasuryManage,
    /// Batch 5: decide governed finance approvals — refunds, adjustments / write-offs, manual journals, payment reversals, aid revocation, fund transfers (step-up).
    FinanceApprove,
    /// 5C: close, reopen and archive financial periods / years; accounting close (step-up).
    PeriodClose,
}

impl Permission {
    pub const ALL: [Permission; 77] = [
        Permission::InstitutionRead,
        Permission::InstitutionRegister,
        Permission::InstitutionRename,
        Permission::NotificationRead,
        Permission::NoticeRead,
        Permission::NoticeManage,
        Permission::TodoUse,
        Permission::TodoAssign,
        Permission::ProfileManage,
        Permission::InstitutionManage,
        Permission::InstitutionLifecycle,
        Permission::InstitutionActivationDecide,
        Permission::InstitutionPolicyManage,
        Permission::OrganisationManage,
        Permission::PositionManage,
        Permission::CampusManage,
        Permission::CampusDecommission,
        Permission::CurriculumManage,
        Permission::CurriculumPublish,
        Permission::CalendarManage,
        Permission::CalendarPublish,
        Permission::InstitutionReadinessRead,
        Permission::StudentRead,
        Permission::StudentSelfService,
        Permission::AdmissionManage,
        Permission::AdmissionDecide,
        Permission::AdmissionRevoke,
        Permission::StudentManage,
        Permission::StudentLifecycle,
        Permission::StudentIdentityManage,
        Permission::StudentArchive,
        Permission::RegistrationManage,
        Permission::RegistrationSelf,
        Permission::ProgressionManage,
        Permission::ProgressionDecide,
        Permission::ProgressionPolicyManage,
        Permission::CompletionManage,
        Permission::QualificationConfer,
        Permission::QualificationRevoke,
        Permission::ExitDecide,
        Permission::GuardianManage,
        Permission::ScholarshipManage,
        Permission::ScholarshipPublish,
        Permission::AlumniManage,
        Permission::AcademicRead,
        Permission::AcademicSelfService,
        Permission::AcademicTeach,
        Permission::OfferingManage,
        Permission::OfferingDecide,
        Permission::OfferingLifecycle,
        Permission::OfferingEnrolmentManage,
        Permission::FacultyAssignmentManage,
        Permission::FacultyAssignmentDecide,
        Permission::TimetableManage,
        Permission::TimetablePublish,
        Permission::AttendanceManage,
        Permission::AttendanceCorrect,
        Permission::MonitoringManage,
        Permission::AssessmentManage,
        Permission::AssessmentFinalize,
        Permission::FlexibleLearningManage,
        Permission::CreditRecognitionDecide,
        Permission::CourseResourceManage,
        Permission::FinanceRead,
        Permission::FinanceSelfService,
        Permission::ChargeManage,
        Permission::AdjustmentManage,
        Permission::AidManage,
        Permission::FinanceConfigManage,
        Permission::PaymentManage,
        Permission::RefundManage,
        Permission::ReceivableManage,
        Permission::LedgerManage,
        Permission::PeriodManage,
        Permission::TreasuryManage,
        Permission::FinanceApprove,
        Permission::PeriodClose,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Permission::InstitutionRead => "institution.read",
            Permission::InstitutionRegister => "institution.register",
            Permission::InstitutionRename => "institution.rename",
            Permission::NotificationRead => "notification.read",
            Permission::NoticeRead => "notice.read",
            Permission::NoticeManage => "notice.manage",
            Permission::TodoUse => "todo.use",
            Permission::TodoAssign => "todo.assign",
            Permission::ProfileManage => "profile.manage",
            Permission::InstitutionManage => "institution.manage",
            Permission::InstitutionLifecycle => "institution.lifecycle",
            Permission::InstitutionActivationDecide => "institution.activation.decide",
            Permission::InstitutionPolicyManage => "institution.policy.manage",
            Permission::OrganisationManage => "organisation.manage",
            Permission::PositionManage => "position.manage",
            Permission::CampusManage => "campus.manage",
            Permission::CampusDecommission => "campus.decommission",
            Permission::CurriculumManage => "curriculum.manage",
            Permission::CurriculumPublish => "curriculum.publish",
            Permission::CalendarManage => "calendar.manage",
            Permission::CalendarPublish => "calendar.publish",
            Permission::InstitutionReadinessRead => "institution.readiness.read",
            Permission::StudentRead => "student.read",
            Permission::StudentSelfService => "student.self",
            Permission::AdmissionManage => "admission.manage",
            Permission::AdmissionDecide => "admission.decide",
            Permission::AdmissionRevoke => "admission.revoke",
            Permission::StudentManage => "student.manage",
            Permission::StudentLifecycle => "student.lifecycle",
            Permission::StudentIdentityManage => "student.identity.manage",
            Permission::StudentArchive => "student.archive",
            Permission::RegistrationManage => "registration.manage",
            Permission::RegistrationSelf => "registration.self",
            Permission::ProgressionManage => "progression.manage",
            Permission::ProgressionDecide => "progression.decide",
            Permission::ProgressionPolicyManage => "progression.policy.manage",
            Permission::CompletionManage => "completion.manage",
            Permission::QualificationConfer => "qualification.confer",
            Permission::QualificationRevoke => "qualification.revoke",
            Permission::ExitDecide => "exit.decide",
            Permission::GuardianManage => "guardian.manage",
            Permission::ScholarshipManage => "scholarship.manage",
            Permission::ScholarshipPublish => "scholarship.publish",
            Permission::AlumniManage => "alumni.manage",
            Permission::AcademicRead => "academic.read",
            Permission::AcademicSelfService => "academic.self",
            Permission::AcademicTeach => "academic.teach",
            Permission::OfferingManage => "offering.manage",
            Permission::OfferingDecide => "offering.decide",
            Permission::OfferingLifecycle => "offering.lifecycle",
            Permission::OfferingEnrolmentManage => "offering.enrolment.manage",
            Permission::FacultyAssignmentManage => "faculty_assignment.manage",
            Permission::FacultyAssignmentDecide => "faculty_assignment.decide",
            Permission::TimetableManage => "timetable.manage",
            Permission::TimetablePublish => "timetable.publish",
            Permission::AttendanceManage => "attendance.manage",
            Permission::AttendanceCorrect => "attendance.correct",
            Permission::MonitoringManage => "monitoring.manage",
            Permission::AssessmentManage => "assessment.manage",
            Permission::AssessmentFinalize => "assessment.finalize",
            Permission::FlexibleLearningManage => "flexible_learning.manage",
            Permission::CreditRecognitionDecide => "credit_recognition.decide",
            Permission::CourseResourceManage => "course_resource.manage",
            Permission::FinanceRead => "finance.read",
            Permission::FinanceSelfService => "finance.self",
            Permission::ChargeManage => "charge.manage",
            Permission::AdjustmentManage => "adjustment.manage",
            Permission::AidManage => "aid.manage",
            Permission::FinanceConfigManage => "finance.config.manage",
            Permission::PaymentManage => "payment.manage",
            Permission::RefundManage => "refund.manage",
            Permission::ReceivableManage => "receivable.manage",
            Permission::LedgerManage => "ledger.manage",
            Permission::PeriodManage => "period.manage",
            Permission::TreasuryManage => "treasury.manage",
            Permission::FinanceApprove => "finance.approve",
            Permission::PeriodClose => "period.close",
        }
    }

    /// Admin-sensitive permissions that additionally need an MFA-verified session when step-up
    /// is enforced ([`AuthContext::step_up_enforced`]).
    pub fn requires_step_up(&self) -> bool {
        matches!(
            self,
            Permission::InstitutionRegister
                | Permission::InstitutionLifecycle
                | Permission::InstitutionActivationDecide
                | Permission::CampusDecommission
                | Permission::AdmissionRevoke
                | Permission::StudentLifecycle
                | Permission::StudentIdentityManage
                | Permission::StudentArchive
                | Permission::QualificationConfer
                | Permission::QualificationRevoke
                | Permission::ExitDecide
                | Permission::OfferingLifecycle
                | Permission::AttendanceCorrect
                | Permission::AssessmentFinalize
                | Permission::CreditRecognitionDecide
                | Permission::FinanceApprove
                | Permission::PeriodClose
        )
    }
}

/// The role → permission policy. Exhaustive over [`Role`] so a new role cannot compile without
/// an explicit (possibly empty) grant list; anything not listed is denied.
pub fn role_permissions(role: Role) -> &'static [Permission] {
    use Permission::*;
    match role {
        Role::Administrator => &[
            InstitutionRead,
            InstitutionRegister,
            InstitutionRename,
            NotificationRead,
            NoticeRead,
            NoticeManage,
            TodoUse,
            TodoAssign,
            ProfileManage,
            // Batch 2 (Domain B): administrators only.
            InstitutionManage,
            InstitutionLifecycle,
            InstitutionActivationDecide,
            InstitutionPolicyManage,
            OrganisationManage,
            PositionManage,
            CampusManage,
            CampusDecommission,
            CurriculumManage,
            CurriculumPublish,
            CalendarManage,
            CalendarPublish,
            InstitutionReadinessRead,
            // Batch 3 (Domain C): staff reads + every write.
            StudentRead,
            AdmissionManage,
            AdmissionDecide,
            AdmissionRevoke,
            StudentManage,
            StudentLifecycle,
            StudentIdentityManage,
            StudentArchive,
            RegistrationManage,
            ProgressionManage,
            ProgressionDecide,
            ProgressionPolicyManage,
            CompletionManage,
            QualificationConfer,
            QualificationRevoke,
            ExitDecide,
            GuardianManage,
            ScholarshipManage,
            ScholarshipPublish,
            AlumniManage,
            // Batch 4 (Domain D): staff reads + every write. Faculty-scoped teaching is the faculty
            // role's (`AcademicTeach`); administrators act on any section through the manage permissions.
            AcademicRead,
            OfferingManage,
            OfferingDecide,
            OfferingLifecycle,
            OfferingEnrolmentManage,
            FacultyAssignmentManage,
            FacultyAssignmentDecide,
            TimetableManage,
            TimetablePublish,
            AttendanceManage,
            AttendanceCorrect,
            MonitoringManage,
            AssessmentManage,
            AssessmentFinalize,
            FlexibleLearningManage,
            CreditRecognitionDecide,
            CourseResourceManage,
            // Batch 5 (Domain F): staff reads + every write and decision (finance office = administrators).
            FinanceRead,
            ChargeManage,
            AdjustmentManage,
            AidManage,
            FinanceConfigManage,
            PaymentManage,
            RefundManage,
            ReceivableManage,
            LedgerManage,
            PeriodManage,
            TreasuryManage,
            FinanceApprove,
            PeriodClose,
        ],
        Role::Faculty => &[
            InstitutionRead,
            NotificationRead,
            NoticeRead,
            TodoUse,
            TodoAssign,
            ProfileManage,
            // Batch 3: read-only student directory / Student 360 (mentors, advisors).
            StudentRead,
            // Batch 4: academic reads + teaching on sections with an active faculty assignment.
            AcademicRead,
            AcademicTeach,
        ],
        Role::Student => &[
            InstitutionRead,
            NotificationRead,
            NoticeRead,
            TodoUse,
            ProfileManage,
            // Batch 3: own record only (handlers resolve the student from the session subject).
            StudentSelfService,
            RegistrationSelf,
            // Batch 4: own timetable, courses, attendance, assignments, resources.
            AcademicSelfService,
            // Batch 5: own fees, payments, receipts, refund requests.
            FinanceSelfService,
        ],
    }
}

/// `true` when any of `roles` is granted `permission`. An empty role set is granted nothing.
pub fn is_granted(roles: &[Role], permission: Permission) -> bool {
    roles
        .iter()
        .any(|r| role_permissions(*r).contains(&permission))
}

/// The authenticated caller of a request, resolved from its server-side session.
#[derive(Clone, PartialEq, Eq)]
pub struct AuthContext {
    /// Technical id of the server-side session (safe to log; never the token).
    pub session_id: Uuid,
    /// Stable subject reference (Keycloak `sub` in production).
    pub subject: String,
    pub login_id: String,
    pub display_name: String,
    pub roles: Vec<Role>,
    /// Session-bound execution scope — the scope authority for this request.
    pub scope: ExecutionScope,
    /// Synchronizer CSRF token bound to this session.
    pub csrf_token: String,
    /// Absolute session expiry.
    pub expires_at: DateTime<Utc>,
    /// Expiry if the session stays idle from now on.
    pub idle_expires_at: DateTime<Utc>,
    /// Assurance level of the session (second factor verified or not).
    pub mfa_level: MfaLevel,
    /// Authentication methods references (RFC 8176 style: `pwd`, `otp`, `hwk`, …).
    pub amr: Vec<String>,
    /// Whether step-up permissions ([`Permission::requires_step_up`]) need `mfa_level = Mfa`.
    /// Set by the session resolver from configuration.
    pub step_up_enforced: bool,
}

impl fmt::Debug for AuthContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthContext")
            .field("session_id", &self.session_id)
            .field("subject", &self.subject)
            .field("roles", &self.roles)
            .field("scope", &self.scope)
            .field("csrf_token", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .field("mfa_level", &self.mfa_level)
            .finish_non_exhaustive()
    }
}

impl AuthContext {
    pub fn has_permission(&self, permission: Permission) -> bool {
        is_granted(&self.roles, permission)
    }

    /// Deny-by-default permission check: `Forbidden` unless one of the caller's roles grants it;
    /// `StepUpRequired` when the permission is admin-sensitive, step-up is enforced and the
    /// session has not verified a second factor.
    pub fn require(&self, permission: Permission) -> Result<(), KernelError> {
        if !self.has_permission(permission) {
            return Err(KernelError::Forbidden(format!(
                "missing permission `{}`",
                permission.as_str()
            )));
        }
        if self.step_up_enforced && permission.requires_step_up() && self.mfa_level < MfaLevel::Mfa
        {
            return Err(KernelError::StepUpRequired(permission.as_str()));
        }
        Ok(())
    }

    /// Constant-time check of a presented synchronizer token.
    pub fn csrf_matches(&self, presented: &str) -> bool {
        !self.csrf_token.is_empty()
            && crate::secret::ct_eq(self.csrf_token.as_bytes(), presented.as_bytes())
    }
}

/// Resolves an opaque session token (from the session cookie) to the caller's [`AuthContext`].
///
/// Must return `Ok(None)` for unknown, revoked, idle-expired or absolutely-expired sessions.
#[async_trait::async_trait]
pub trait SessionResolver: Send + Sync + 'static {
    async fn resolve(&self, token: &str) -> Result<Option<AuthContext>, KernelError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scope::TenantId;

    fn ctx(roles: Vec<Role>) -> AuthContext {
        AuthContext {
            session_id: Uuid::now_v7(),
            subject: "s".into(),
            login_id: "l".into(),
            display_name: "d".into(),
            roles,
            scope: ExecutionScope::tenant(TenantId::new()),
            csrf_token: "a".repeat(64),
            expires_at: Utc::now(),
            idle_expires_at: Utc::now(),
            mfa_level: MfaLevel::None,
            amr: vec!["pwd".into()],
            step_up_enforced: false,
        }
    }

    #[test]
    fn step_up_is_required_only_when_enforced() {
        let mut a = ctx(vec![Role::Administrator]);
        assert!(a.require(Permission::InstitutionRegister).is_ok());
        a.step_up_enforced = true;
        assert!(matches!(
            a.require(Permission::InstitutionRegister),
            Err(KernelError::StepUpRequired("institution.register"))
        ));
        assert!(
            a.require(Permission::InstitutionRead).is_ok(),
            "not sensitive"
        );
        a.mfa_level = MfaLevel::Mfa;
        assert!(a.require(Permission::InstitutionRegister).is_ok());
        // Step-up never grants a permission the role does not hold.
        let mut s = ctx(vec![Role::Student]);
        s.step_up_enforced = true;
        s.mfa_level = MfaLevel::Mfa;
        assert!(matches!(
            s.require(Permission::InstitutionRegister),
            Err(KernelError::Forbidden(_))
        ));
        assert_eq!(MfaLevel::parse("mfa"), MfaLevel::Mfa);
        assert_eq!(MfaLevel::parse("bogus"), MfaLevel::None);
    }

    #[test]
    fn administrator_holds_institution_admin_permissions() {
        let a = ctx(vec![Role::Administrator]);
        // Student self-service permissions belong to the student role only.
        for p in Permission::ALL.into_iter().filter(|p| {
            !matches!(
                p,
                Permission::StudentSelfService
                    | Permission::RegistrationSelf
                    | Permission::AcademicSelfService
                    | Permission::AcademicTeach
                    | Permission::FinanceSelfService
            )
        }) {
            assert!(a.require(p).is_ok(), "{p:?}");
        }
    }

    #[test]
    fn non_admin_roles_may_only_read() {
        for r in [Role::Faculty, Role::Student] {
            let c = ctx(vec![r]);
            assert!(c.require(Permission::InstitutionRead).is_ok());
            for p in [
                Permission::InstitutionRegister,
                Permission::InstitutionRename,
                Permission::NoticeManage,
            ] {
                assert!(
                    matches!(c.require(p), Err(KernelError::Forbidden(m)) if m.contains(p.as_str())),
                    "{r:?} must not hold {p:?}"
                );
            }
        }
    }

    #[test]
    fn batch2_domain_b_writes_are_administrator_only() {
        let writes = [
            Permission::InstitutionManage,
            Permission::InstitutionLifecycle,
            Permission::InstitutionActivationDecide,
            Permission::InstitutionPolicyManage,
            Permission::OrganisationManage,
            Permission::PositionManage,
            Permission::CampusManage,
            Permission::CampusDecommission,
            Permission::CurriculumManage,
            Permission::CurriculumPublish,
            Permission::CalendarManage,
            Permission::CalendarPublish,
            Permission::InstitutionReadinessRead,
        ];
        for p in writes {
            assert!(is_granted(&[Role::Administrator], p), "{p:?}");
            assert!(!is_granted(&[Role::Faculty, Role::Student], p), "{p:?}");
        }
        for p in [
            Permission::InstitutionLifecycle,
            Permission::InstitutionActivationDecide,
            Permission::CampusDecommission,
        ] {
            assert!(p.requires_step_up(), "{p:?}");
        }
        assert!(!Permission::CampusManage.requires_step_up());
    }

    #[test]
    fn batch3_domain_c_grants() {
        let admin_only = [
            Permission::AdmissionManage,
            Permission::AdmissionDecide,
            Permission::AdmissionRevoke,
            Permission::StudentManage,
            Permission::StudentLifecycle,
            Permission::StudentIdentityManage,
            Permission::StudentArchive,
            Permission::RegistrationManage,
            Permission::ProgressionManage,
            Permission::ProgressionDecide,
            Permission::ProgressionPolicyManage,
            Permission::CompletionManage,
            Permission::QualificationConfer,
            Permission::QualificationRevoke,
            Permission::ExitDecide,
            Permission::GuardianManage,
            Permission::ScholarshipManage,
            Permission::ScholarshipPublish,
            Permission::AlumniManage,
        ];
        for p in admin_only {
            assert!(is_granted(&[Role::Administrator], p), "{p:?}");
            assert!(!is_granted(&[Role::Faculty, Role::Student], p), "{p:?}");
        }
        assert!(is_granted(&[Role::Faculty], Permission::StudentRead));
        assert!(!is_granted(&[Role::Student], Permission::StudentRead));
        for p in [Permission::StudentSelfService, Permission::RegistrationSelf] {
            assert!(is_granted(&[Role::Student], p), "{p:?}");
            assert!(!is_granted(&[Role::Faculty], p), "{p:?}");
        }
        for p in [
            Permission::AdmissionRevoke,
            Permission::StudentLifecycle,
            Permission::StudentIdentityManage,
            Permission::StudentArchive,
            Permission::QualificationConfer,
            Permission::QualificationRevoke,
            Permission::ExitDecide,
        ] {
            assert!(p.requires_step_up(), "{p:?}");
        }
        assert!(!Permission::StudentManage.requires_step_up());
        let names: std::collections::HashSet<_> =
            Permission::ALL.iter().map(|p| p.as_str()).collect();
        assert_eq!(
            names.len(),
            Permission::ALL.len(),
            "duplicate permission name"
        );
    }

    #[test]
    fn batch4_domain_d_grants() {
        let admin_only = [
            Permission::OfferingManage,
            Permission::OfferingDecide,
            Permission::OfferingLifecycle,
            Permission::OfferingEnrolmentManage,
            Permission::FacultyAssignmentManage,
            Permission::FacultyAssignmentDecide,
            Permission::TimetableManage,
            Permission::TimetablePublish,
            Permission::AttendanceManage,
            Permission::AttendanceCorrect,
            Permission::MonitoringManage,
            Permission::AssessmentManage,
            Permission::AssessmentFinalize,
            Permission::FlexibleLearningManage,
            Permission::CreditRecognitionDecide,
            Permission::CourseResourceManage,
        ];
        for p in admin_only {
            assert!(is_granted(&[Role::Administrator], p), "{p:?}");
            assert!(!is_granted(&[Role::Faculty, Role::Student], p), "{p:?}");
        }
        for r in [Role::Administrator, Role::Faculty] {
            assert!(is_granted(&[r], Permission::AcademicRead), "{r:?}");
        }
        assert!(!is_granted(&[Role::Student], Permission::AcademicRead));
        assert!(is_granted(&[Role::Faculty], Permission::AcademicTeach));
        assert!(!is_granted(
            &[Role::Administrator, Role::Student],
            Permission::AcademicTeach
        ));
        assert!(is_granted(
            &[Role::Student],
            Permission::AcademicSelfService
        ));
        assert!(!is_granted(
            &[Role::Administrator, Role::Faculty],
            Permission::AcademicSelfService
        ));
        for p in [
            Permission::OfferingLifecycle,
            Permission::AttendanceCorrect,
            Permission::AssessmentFinalize,
            Permission::CreditRecognitionDecide,
        ] {
            assert!(p.requires_step_up(), "{p:?}");
        }
        assert!(!Permission::OfferingManage.requires_step_up());
        assert!(!Permission::AcademicTeach.requires_step_up());
    }

    #[test]
    fn batch5_domain_f_grants() {
        let admin_only = [
            Permission::FinanceRead,
            Permission::ChargeManage,
            Permission::AdjustmentManage,
            Permission::AidManage,
            Permission::FinanceConfigManage,
            Permission::PaymentManage,
            Permission::RefundManage,
            Permission::ReceivableManage,
            Permission::LedgerManage,
            Permission::PeriodManage,
            Permission::TreasuryManage,
            Permission::FinanceApprove,
            Permission::PeriodClose,
        ];
        for p in admin_only {
            assert!(is_granted(&[Role::Administrator], p), "{p:?}");
            assert!(!is_granted(&[Role::Faculty, Role::Student], p), "{p:?}");
        }
        assert!(is_granted(&[Role::Student], Permission::FinanceSelfService));
        assert!(!is_granted(
            &[Role::Administrator, Role::Faculty],
            Permission::FinanceSelfService
        ));
        for p in [Permission::FinanceApprove, Permission::PeriodClose] {
            assert!(p.requires_step_up(), "{p:?}");
        }
        assert!(!Permission::ChargeManage.requires_step_up());
        assert!(!Permission::FinanceRead.requires_step_up());
    }

    #[test]
    fn deny_by_default_without_roles() {
        let none = ctx(vec![]);
        for p in Permission::ALL {
            assert!(!none.has_permission(p));
            assert!(none.require(p).is_err());
        }
    }

    #[test]
    fn grants_are_the_union_of_roles() {
        assert!(is_granted(
            &[Role::Student, Role::Administrator],
            Permission::InstitutionRename
        ));
        assert!(!is_granted(
            &[Role::Student, Role::Faculty],
            Permission::InstitutionRename
        ));
    }

    #[test]
    fn role_names_roundtrip_and_unknown_is_rejected() {
        for r in Role::ALL {
            assert_eq!(Role::parse(r.as_str()), Some(r));
        }
        assert_eq!(Role::parse("superuser"), None);
        assert_eq!(Role::parse("Administrator"), None);
    }

    #[test]
    fn csrf_compare_and_redacted_debug() {
        let c = ctx(vec![Role::Student]);
        assert!(c.csrf_matches(&"a".repeat(64)));
        assert!(!c.csrf_matches(&"b".repeat(64)));
        assert!(!c.csrf_matches(""));
        assert!(!format!("{c:?}").contains(&"a".repeat(64)));
        let mut empty = c;
        empty.csrf_token.clear();
        assert!(!empty.csrf_matches(""), "an empty token never matches");
    }

    #[test]
    fn self_service_permissions_for_every_role_and_assignment_for_staff() {
        for r in Role::ALL {
            let c = ctx(vec![r]);
            for p in [
                Permission::NotificationRead,
                Permission::NoticeRead,
                Permission::TodoUse,
                Permission::ProfileManage,
            ] {
                assert!(c.require(p).is_ok(), "{r:?} needs {p:?}");
            }
        }
        assert!(ctx(vec![Role::Faculty]).has_permission(Permission::TodoAssign));
        assert!(!ctx(vec![Role::Student]).has_permission(Permission::TodoAssign));
        assert!(!ctx(vec![Role::Faculty]).has_permission(Permission::NoticeManage));
        let names: std::collections::HashSet<_> =
            Permission::ALL.iter().map(Permission::as_str).collect();
        assert_eq!(
            names.len(),
            Permission::ALL.len(),
            "permission names are unique"
        );
    }

    #[test]
    fn administrator_is_privileged() {
        assert!(Role::Administrator.is_privileged());
        assert!(!Role::Student.is_privileged());
    }
}
