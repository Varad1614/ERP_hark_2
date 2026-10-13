//! Wire views of slice 6C (decimals as two-place strings, contract §3.1 / §5.3).

use super::common::opt2;
use super::repo::{BatchRow, GradeCardRow, PublicationRow, ResultDecisionRow};
use crate::shared::grading::fmt2;
use crate::shared::results::ResultRow;
use chrono::{DateTime, NaiveDate, Utc};
use serde::Serialize;
use sutra_contracts::exams::status::{
    AttemptKind, GradeCardStatus, PublicationStatus, ResultBatchStatus, ResultOutcome, ResultStatus,
};
use utoipa::ToSchema;
use uuid::Uuid;

/// One line of a term result.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResultLineView {
    pub line_no: i32,
    pub paper_id: Uuid,
    pub curriculum_component_id: Uuid,
    pub paper_code: String,
    pub title: String,
    #[schema(example = "4.00")]
    pub credits: String,
    pub attempt_kind: AttemptKind,
    #[schema(example = "30.00")]
    pub internal_marks: Option<String>,
    #[schema(example = "60.00")]
    pub external_marks: Option<String>,
    #[schema(example = "0.00")]
    pub grace_marks: String,
    #[schema(example = "90.00")]
    pub total_marks: Option<String>,
    #[schema(example = "140.00")]
    pub max_marks: String,
    #[schema(example = "66.00")]
    pub percent: Option<String>,
    #[schema(example = "B+")]
    pub grade: Option<String>,
    #[schema(example = "7.00")]
    pub grade_point: Option<String>,
    pub passed: bool,
    pub remark: Option<String>,
}

/// A candidate's term result (contract §5.3).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TermResultView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub session_id: Uuid,
    pub registration_id: Uuid,
    pub student_id: Uuid,
    pub student_number: String,
    pub display_name: String,
    pub academic_term_id: Uuid,
    pub grading_scheme_id: Uuid,
    #[schema(example = "20.00")]
    pub credits_attempted: String,
    #[schema(example = "20.00")]
    pub credits_earned: String,
    #[schema(example = "169.00")]
    pub grade_points: String,
    #[schema(example = "8.45")]
    pub sgpa: Option<String>,
    #[schema(example = "8.12")]
    pub cgpa: Option<String>,
    pub outcome: ResultOutcome,
    pub status: ResultStatus,
    pub status_reason: Option<String>,
    pub revision: i32,
    pub supersedes_id: Option<Uuid>,
    pub published_at: Option<DateTime<Utc>>,
    /// Student view only: why a withdrawn result shows no details.
    pub notice: Option<String>,
    pub lines: Vec<ResultLineView>,
    pub version: i64,
}

impl From<&ResultRow> for TermResultView {
    fn from(r: &ResultRow) -> Self {
        Self {
            id: r.id.as_uuid(),
            institution_id: r.institution_id.as_uuid(),
            session_id: r.session_id.as_uuid(),
            registration_id: r.registration_id.as_uuid(),
            student_id: r.student_id.as_uuid(),
            student_number: r.student_number.clone(),
            display_name: r.display_name.clone(),
            academic_term_id: r.academic_term_id,
            grading_scheme_id: r.grading_scheme_id.as_uuid(),
            credits_attempted: fmt2(r.credits_attempted),
            credits_earned: fmt2(r.credits_earned),
            grade_points: fmt2(r.grade_points),
            sgpa: opt2(r.sgpa),
            cgpa: opt2(r.cgpa),
            outcome: r.outcome,
            status: r.status,
            status_reason: r.status_reason.clone(),
            revision: r.revision,
            supersedes_id: r.supersedes_id.map(|s| s.as_uuid()),
            published_at: r.published_at,
            notice: None,
            lines: r
                .lines
                .iter()
                .map(|l| ResultLineView {
                    line_no: l.line_no,
                    paper_id: l.paper_id.as_uuid(),
                    curriculum_component_id: l.curriculum_component_id,
                    paper_code: l.paper_code.clone(),
                    title: l.title.clone(),
                    credits: fmt2(l.credits),
                    attempt_kind: l.attempt_kind,
                    internal_marks: opt2(l.internal_marks),
                    external_marks: opt2(l.external_marks),
                    grace_marks: fmt2(l.grace_marks),
                    total_marks: opt2(l.total_marks),
                    max_marks: fmt2(l.max_marks),
                    percent: opt2(l.percent),
                    grade: l.grade.clone(),
                    grade_point: opt2(l.grade_point),
                    passed: l.passed,
                    remark: l.remark.clone(),
                })
                .collect(),
            version: r.version,
        }
    }
}

impl TermResultView {
    /// What a student may see: a published result in full, a withdrawn one as a notice only.
    pub fn for_student(r: &ResultRow) -> Self {
        let mut v = Self::from(r);
        if r.status == ResultStatus::Withdrawn {
            v.lines.clear();
            v.sgpa = None;
            v.cgpa = None;
            v.notice = Some("This result has been withdrawn by the examinations office.".into());
        }
        v
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TermResultList {
    pub items: Vec<TermResultView>,
}

/// A result computation batch.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResultBatchView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub session_id: Uuid,
    pub status: ResultBatchStatus,
    pub status_reason: Option<String>,
    pub run_count: i32,
    pub candidates: i32,
    pub computed_count: i32,
    pub withheld_count: i32,
    pub unchanged_count: i32,
    pub computed_at: Option<DateTime<Utc>>,
    pub process_instance_id: Option<String>,
    pub version: i64,
}

impl From<&BatchRow> for ResultBatchView {
    fn from(b: &BatchRow) -> Self {
        Self {
            id: b.id,
            institution_id: b.institution_id.as_uuid(),
            session_id: b.session_id.as_uuid(),
            status: b.status,
            status_reason: b.status_reason.clone(),
            run_count: b.run_count,
            candidates: b.candidates,
            computed_count: b.computed_count,
            withheld_count: b.withheld_count,
            unchanged_count: b.unchanged_count,
            computed_at: b.computed_at,
            process_instance_id: b.process_instance_id.clone(),
            version: b.version,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ResultBatchList {
    pub items: Vec<ResultBatchView>,
}

/// Summary of a computation run (bulk command, contract §5.1).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ComputeSummaryView {
    pub processed: i32,
    pub computed: i32,
    pub withheld: i32,
    pub unchanged: i32,
    pub skipped: i32,
    pub failed: Vec<String>,
    pub batch: ResultBatchView,
}

/// A governed result decision (correction / withdrawal).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResultDecisionView {
    pub id: Uuid,
    pub result_id: Uuid,
    pub session_id: Uuid,
    /// `correction` | `withdrawal`
    pub kind: String,
    pub paper_id: Option<Uuid>,
    pub internal_marks: Option<String>,
    pub external_marks: Option<String>,
    pub grace_marks: Option<String>,
    pub reason: String,
    /// `pending` | `approved` | `rejected`
    pub status: String,
    pub decision_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub version: i64,
}

impl From<&ResultDecisionRow> for ResultDecisionView {
    fn from(d: &ResultDecisionRow) -> Self {
        Self {
            id: d.id,
            result_id: d.result_id.as_uuid(),
            session_id: d.session_id.as_uuid(),
            kind: d.kind.clone(),
            paper_id: d.paper_id,
            internal_marks: opt2(d.internal_marks),
            external_marks: opt2(d.external_marks),
            grace_marks: opt2(d.grace_marks),
            reason: d.reason.clone(),
            status: d.status.clone(),
            decision_reason: d.decision_reason.clone(),
            process_instance_id: d.process_instance_id.clone(),
            version: d.version,
        }
    }
}

/// A decision with the result as it stands afterwards.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ResultDecisionOutcome {
    pub decision: ResultDecisionView,
    pub result: TermResultView,
}

/// A result publication.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PublicationView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub session_id: Uuid,
    pub status: PublicationStatus,
    pub status_reason: Option<String>,
    pub scheduled_on: Option<NaiveDate>,
    pub delayed_until: Option<NaiveDate>,
    pub authorised_at: Option<DateTime<Utc>>,
    pub published_at: Option<DateTime<Utc>>,
    pub attempts: i32,
    pub published_count: i32,
    pub skipped_count: i32,
    pub last_error: Option<String>,
    pub process_instance_id: Option<String>,
    pub version: i64,
}

impl From<&PublicationRow> for PublicationView {
    fn from(p: &PublicationRow) -> Self {
        Self {
            id: p.id,
            institution_id: p.institution_id.as_uuid(),
            session_id: p.session_id.as_uuid(),
            status: p.status,
            status_reason: p.status_reason.clone(),
            scheduled_on: p.scheduled_on,
            delayed_until: p.delayed_until,
            authorised_at: p.authorised_at,
            published_at: p.published_at,
            attempts: p.attempts,
            published_count: p.published_count,
            skipped_count: p.skipped_count,
            last_error: p.last_error.clone(),
            process_instance_id: p.process_instance_id.clone(),
            version: p.version,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PublicationList {
    pub items: Vec<PublicationView>,
}

/// Summary of a publication run.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PublishSummaryView {
    pub processed: i32,
    pub published: i32,
    pub revised: i32,
    pub skipped: i32,
    pub failed: Vec<String>,
    pub publication: PublicationView,
}

/// A grade card (with the result it certifies when loaded).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GradeCardView {
    pub id: Uuid,
    #[schema(example = "GC-000001")]
    pub number: String,
    pub status: GradeCardStatus,
    pub status_reason: Option<String>,
    pub session_id: Uuid,
    pub registration_id: Uuid,
    pub result_id: Uuid,
    pub student_id: Uuid,
    pub supersedes_id: Option<Uuid>,
    pub issued_at: DateTime<Utc>,
    pub result: Option<TermResultView>,
    pub version: i64,
}

impl GradeCardView {
    pub fn of(c: &GradeCardRow, result: Option<&ResultRow>) -> Self {
        Self {
            id: c.id,
            number: c.card_number.clone(),
            status: c.status,
            status_reason: c.status_reason.clone(),
            session_id: c.session_id.as_uuid(),
            registration_id: c.registration_id,
            result_id: c.result_id.as_uuid(),
            student_id: c.student_id,
            supersedes_id: c.supersedes_id,
            issued_at: c.issued_at,
            result: result.map(TermResultView::from),
            version: c.version,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct GradeCardList {
    pub items: Vec<GradeCardView>,
}
