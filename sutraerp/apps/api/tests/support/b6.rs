//! Batch 6 foundation test helpers (foundation-owned; slices add their own helpers in their own
//! test file, see `docs/batch6-contracts.md` §11).
//!
//! * [`exam_setup`] — Batch 5's finance setup (active institution + published programme /
//!   curriculum / calendar, one seeded active student linked to a login, the default chart and
//!   the student's finance account) plus an `active` CS101 offering (section `A`, the student
//!   enrolled), the ten-point grading scheme, an examination session (any status) and one
//!   theory paper for the offering.
//! * [`seed_session`] / [`seed_paper`] — more sessions / papers (direct inserts into the
//!   foundation tables, exactly the shape 6A's API writes), so 6B / 6C never wait for 6A.
//! * [`seed_registration`] — a registration with papers through
//!   `sutra_exams::shared::registration::insert_registration` (creates the `paper_mark` rows).
//! * [`seed_final_marks`] — walks a candidate's mark row through the shared machine
//!   (`entered → submitted → verified → approved → finalised`) with `shared::marks::update`,
//!   exactly like 6B will, so 6C can compute results without 6B's API.
//! * [`seed_internal_result`] — a finalised Domain D internal assessment result (blueprint +
//!   result row) so 6B's import can be tested without 4B's API.

use super::b4::{self, SeededOffering};
use super::b5::{self, FinanceSetup};
use super::Login;
use axum::Router;
use rust_decimal::Decimal;
use sqlx::PgPool;
use std::str::FromStr;
use sutra_contracts::exams::ids::{
    ExamPaperId, ExamRegistrationPaperId, ExamSessionId, GradingSchemeId,
};
use sutra_contracts::exams::ports::{CandidateSummary, PaperMarkSummary};
use sutra_contracts::exams::status::{
    AttemptKind, ExamRegistrationStatus, MarkStatus, RegistrationChannel,
};
use sutra_contracts::student::ids::{StudentContextId, StudentId};
use sutra_exams::shared::marks::{self, MarkValues};
use sutra_exams::shared::registration::{
    insert_registration, NewRegistration, NewRegistrationPaper,
};
use sutra_exams::shared::Actor;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use uuid::Uuid;

/// Decimal from a literal (`"72.50"`).
pub fn dec(s: &str) -> Decimal {
    Decimal::from_str(s).expect("decimal literal")
}

/// The actor recorded by seeds.
pub fn seed_actor() -> Actor {
    Actor {
        id: Some("test:b6-seed".into()),
        correlation_id: None,
    }
}

/// Domain B/C/D/F/I context a Domain I test needs.
#[derive(Clone, Debug)]
pub struct ExamSetup {
    pub finance: FinanceSetup,
    pub offering: SeededOffering,
    pub scheme_id: GradingSchemeId,
    pub session_id: ExamSessionId,
    pub paper_id: ExamPaperId,
    /// Login subject linked to the seeded student (registrations snapshot it).
    pub subject_ref: Option<String>,
}

impl ExamSetup {
    pub fn tenant(&self) -> TenantId {
        self.finance.tenant()
    }
    pub fn institution(&self) -> InstitutionId {
        self.finance.institution()
    }
    pub fn scope(&self) -> ExecutionScope {
        self.finance.scope()
    }
    pub fn term_id(&self) -> Uuid {
        self.finance.delivery.term_id
    }
    pub fn student_id(&self) -> Uuid {
        self.finance.student.student_id
    }
}

/// Inserts the ten-point grading scheme (`O … F`, see `shared::grading::ten_point_default`).
pub async fn seed_scheme(
    owner: &PgPool,
    tenant: TenantId,
    institution: InstitutionId,
) -> GradingSchemeId {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO exams.grading_scheme (tenant_id, institution_id, code, name) \
         VALUES ($1, $2, $3, 'Ten-point CBCS') RETURNING id",
    )
    .bind(tenant.as_uuid())
    .bind(institution.as_uuid())
    .bind(format!(
        "TEN-{}",
        Uuid::now_v7().simple().to_string()[24..].to_uppercase()
    ))
    .fetch_one(owner)
    .await
    .expect("seed grading scheme");
    for b in sutra_exams::shared::grading::ten_point_default() {
        sqlx::query(
            "INSERT INTO exams.grade_band (tenant_id, scheme_id, grade, min_percent, grade_point, is_pass) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(tenant.as_uuid())
        .bind(id)
        .bind(&b.grade)
        .bind(b.min_percent)
        .bind(b.grade_point)
        .bind(b.is_pass)
        .execute(owner)
        .await
        .expect("seed grade band");
    }
    GradingSchemeId(id)
}

/// Inserts a regular examination session of the setup's term in `status` (e.g. `active`).
pub async fn seed_session(
    owner: &PgPool,
    finance: &FinanceSetup,
    scheme: GradingSchemeId,
    status: &str,
) -> ExamSessionId {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO exams.exam_session (tenant_id, institution_id, academic_year_id, \
         academic_term_id, code, name, kind, grading_scheme_id, starts_on, ends_on, \
         registration_opens_on, registration_closes_on, status) \
         VALUES ($1, $2, $3, $4, $5, 'End-semester examinations', 'regular', $6, '2026-11-20', \
         '2026-12-05', '2026-10-01', '2026-11-10', $7) RETURNING id",
    )
    .bind(finance.tenant().as_uuid())
    .bind(finance.institution().as_uuid())
    .bind(finance.delivery.base.academic_year_id)
    .bind(finance.delivery.term_id)
    .bind(format!(
        "ESE-{}",
        Uuid::now_v7().simple().to_string()[24..].to_uppercase()
    ))
    .bind(scheme.as_uuid())
    .bind(status)
    .fetch_one(owner)
    .await
    .expect("seed session");
    ExamSessionId(id)
}

/// Inserts a theory paper of an offering (`max_marks` 100, pass 40 %, `internal_weight` %).
pub async fn seed_paper(
    owner: &PgPool,
    finance: &FinanceSetup,
    session: ExamSessionId,
    offering: &SeededOffering,
    credits: &str,
    internal_weight: &str,
    status: &str,
) -> ExamPaperId {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO exams.exam_paper (tenant_id, institution_id, session_id, offering_id, \
         curriculum_component_id, academic_term_id, paper_code, title, credits, mode, max_marks, \
         pass_percent, internal_weight, duration_minutes, status) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'theory', 100, 40, $10, 180, $11) RETURNING id",
    )
    .bind(finance.tenant().as_uuid())
    .bind(finance.institution().as_uuid())
    .bind(session.as_uuid())
    .bind(offering.offering_id)
    .bind(offering.component_id)
    .bind(finance.delivery.term_id)
    .bind(
        offering
            .offering_code
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
            .take(32)
            .collect::<String>(),
    )
    .bind(format!("{} — end-semester", offering.offering_code))
    .bind(dec(credits))
    .bind(dec(internal_weight))
    .bind(status)
    .fetch_one(owner)
    .await
    .expect("seed paper");
    ExamPaperId(id)
}

/// See the module docs. The session is created in `session_status`; the paper (4 credits, 40 %
/// internal weight) in `paper_status`.
#[allow(clippy::too_many_arguments)]
pub async fn exam_setup(
    app: &Router,
    admin: &Login,
    owner: &PgPool,
    tenant: &str,
    name: &str,
    subject_ref: Option<&str>,
    session_status: &str,
    paper_status: &str,
) -> ExamSetup {
    let finance = b5::finance_setup(app, admin, owner, tenant, name, subject_ref).await;
    let offering = b4::seed_offering(owner, &finance.delivery, "CS101", "active").await;
    b4::seed_enrolment(
        owner,
        &finance.delivery,
        &offering,
        &finance.student,
        subject_ref,
    )
    .await;
    let scheme_id = seed_scheme(owner, finance.tenant(), finance.institution()).await;
    let session_id = seed_session(owner, &finance, scheme_id, session_status).await;
    let paper_id = seed_paper(
        owner,
        &finance,
        session_id,
        &offering,
        "4",
        "40",
        paper_status,
    )
    .await;
    ExamSetup {
        finance,
        offering,
        scheme_id,
        session_id,
        paper_id,
        subject_ref: subject_ref.map(str::to_owned),
    }
}

/// A registration of the setup's student for `papers` (regular attempts) in `status`.
pub async fn seed_registration(
    owner: &PgPool,
    setup: &ExamSetup,
    session: ExamSessionId,
    papers: &[ExamPaperId],
    status: ExamRegistrationStatus,
) -> CandidateSummary {
    let mut tx = owner.begin().await.expect("tx");
    let reg = insert_registration(
        &mut tx,
        &NewRegistration {
            tenant: setup.tenant(),
            institution: setup.institution(),
            session_id: session,
            student_id: StudentId(setup.student_id()),
            student_context_id: StudentContextId(setup.finance.student.context_id),
            student_number: setup.finance.student.student_number.clone(),
            display_name: "Aarav Seeded".into(),
            subject_ref: setup.subject_ref.clone(),
            channel: RegistrationChannel::Administrative,
            status,
            papers: papers
                .iter()
                .map(|p| NewRegistrationPaper {
                    paper_id: *p,
                    attempt_kind: AttemptKind::Regular,
                    attempt_no: 1,
                })
                .collect(),
        },
        "test.seed",
        &seed_actor(),
    )
    .await
    .expect("seed registration");
    tx.commit().await.expect("commit");
    reg
}

/// Walks the candidate's mark row of `registration_paper` to `finalised` with the given marks.
pub async fn seed_final_marks(
    owner: &PgPool,
    setup: &ExamSetup,
    registration_paper: ExamRegistrationPaperId,
    internal: Option<(&str, &str)>,
    external: Option<&str>,
) -> PaperMarkSummary {
    let t = setup.tenant();
    let id: Uuid = sqlx::query_scalar(
        "SELECT id FROM exams.paper_mark WHERE tenant_id = $1 AND registration_paper_id = $2",
    )
    .bind(t.as_uuid())
    .bind(registration_paper.as_uuid())
    .fetch_one(owner)
    .await
    .expect("mark row");
    let actor = seed_actor();
    let mut tx = owner.begin().await.expect("tx");
    let mut m = marks::lock_mark(&mut tx, t, sutra_contracts::exams::ids::PaperMarkId(id))
        .await
        .expect("lock mark");
    let values = MarkValues {
        internal_marks: internal.map(|(m, _)| dec(m)),
        internal_max: internal.map(|(_, max)| dec(max)),
        internal_source_id: None,
        external_marks: external.map(dec),
        grace_marks: Decimal::ZERO,
        absent: external.is_none(),
    };
    m = marks::update(
        &mut tx,
        t,
        &m,
        Some(values),
        MarkStatus::Entered,
        "entry",
        None,
        &actor,
    )
    .await
    .expect("enter marks");
    for (to, cause) in [
        (MarkStatus::Submitted, "submit"),
        (MarkStatus::Verified, "verify"),
        (MarkStatus::Approved, "approve"),
        (MarkStatus::Finalised, "finalise"),
    ] {
        m = marks::update(&mut tx, t, &m, None, to, cause, None, &actor)
            .await
            .expect("mark step");
    }
    tx.commit().await.expect("commit");
    m
}

/// A finalised Domain D internal assessment result of the setup's student on the offering.
pub async fn seed_internal_result(
    owner: &PgPool,
    setup: &ExamSetup,
    marks_obtained: &str,
    max_marks: &str,
) -> Uuid {
    let (t, i) = (setup.tenant().as_uuid(), setup.institution().as_uuid());
    let blueprint: Uuid = sqlx::query_scalar(
        "INSERT INTO academic.assessment_blueprint (tenant_id, institution_id, offering_id, \
         section_id, title, total_marks, status, published_at) \
         VALUES ($1, $2, $3, $4, 'Internal assessment', $5, 'published', now()) RETURNING id",
    )
    .bind(t)
    .bind(i)
    .bind(setup.offering.offering_id)
    .bind(setup.offering.section_id)
    .bind(dec(max_marks))
    .fetch_one(owner)
    .await
    .expect("seed blueprint");
    sqlx::query_scalar(
        "INSERT INTO academic.internal_assessment_result (tenant_id, institution_id, offering_id, \
         section_id, blueprint_id, academic_term_id, curriculum_component_id, student_id, \
         student_number, display_name, marks_obtained, max_marks, status, finalized_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'Aarav Seeded', $10, $11, 'finalized', now()) \
         RETURNING id",
    )
    .bind(t)
    .bind(i)
    .bind(setup.offering.offering_id)
    .bind(setup.offering.section_id)
    .bind(blueprint)
    .bind(setup.term_id())
    .bind(setup.offering.component_id)
    .bind(setup.student_id())
    .bind(&setup.finance.student.student_number)
    .bind(dec(marks_obtained))
    .bind(dec(max_marks))
    .fetch_one(owner)
    .await
    .expect("seed internal result")
}
