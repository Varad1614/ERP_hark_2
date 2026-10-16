//! Row types and loaders of the shared Student aggregate tables (`student.student`,
//! `student.student_context`), created by the Batch 3 foundation migration.
//!
//! Every slice reads these. Writes: **3A** inserts personas/contexts and changes their
//! attributes (profile, enrolment, assignment); **status** changes go only through
//! [`crate::shared::lifecycle`]; 3B may update `student_context.current_term_no`,
//! `programme_id` and `curriculum_version_id` (progression / programme migration) through
//! [`set_context_progress`]. Nothing else writes these tables.

use chrono::{DateTime, NaiveDate, Utc};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, PgExecutor, Row};
use sutra_contracts::student::ids::{StudentContextId, StudentId};
use sutra_contracts::student::status::{StudentContextStatus, StudentStatus};
use sutra_kernel::auth::AuthContext;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

/// Columns of [`StudentRow`] (use in `SELECT {STUDENT_COLUMNS} FROM student.student …`).
pub const STUDENT_COLUMNS: &str = "id, tenant_id, person_id, student_number, given_name, \
    middle_name, family_name, preferred_name, date_of_birth, gender, primary_email, primary_phone, \
    subject_ref, origin_admission_id, merged_into_id, status, status_changed_at, version, \
    created_at, updated_at";

/// Columns of [`StudentContextRow`].
pub const CONTEXT_COLUMNS: &str = "id, tenant_id, student_id, institution_id, campus_id, \
    programme_id, curriculum_version_id, org_unit_id, admission_term_id, admission_year_id, \
    batch_code, section_code, current_term_no, enrolment_number, roll_number, enrolment_type, \
    enrolled_on, admission_id, predecessor_context_id, status, status_reason, status_changed_at, \
    closed_on, version, created_at, updated_at";

/// One `student.student` row (the tenant-scoped persona).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StudentRow {
    pub id: StudentId,
    pub tenant_id: TenantId,
    pub person_id: Option<Uuid>,
    pub student_number: String,
    pub given_name: String,
    pub middle_name: Option<String>,
    pub family_name: Option<String>,
    pub preferred_name: Option<String>,
    pub date_of_birth: Option<NaiveDate>,
    pub gender: Option<String>,
    pub primary_email: Option<String>,
    pub primary_phone: Option<String>,
    pub subject_ref: Option<String>,
    pub origin_admission_id: Option<Uuid>,
    pub merged_into_id: Option<StudentId>,
    pub status: StudentStatus,
    pub status_changed_at: DateTime<Utc>,
    pub version: Version,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl StudentRow {
    /// "Given Middle Family" without empty parts.
    pub fn display_name(&self) -> String {
        [
            Some(self.given_name.as_str()),
            self.middle_name.as_deref(),
            self.family_name.as_deref(),
        ]
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
    }
}

/// One `student.student_context` row (the per-Institute relationship).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StudentContextRow {
    pub id: StudentContextId,
    pub tenant_id: TenantId,
    pub student_id: StudentId,
    pub institution_id: InstitutionId,
    pub campus_id: Option<Uuid>,
    pub programme_id: Option<Uuid>,
    pub curriculum_version_id: Option<Uuid>,
    pub org_unit_id: Option<Uuid>,
    pub admission_term_id: Option<Uuid>,
    pub admission_year_id: Option<Uuid>,
    pub batch_code: Option<String>,
    pub section_code: Option<String>,
    pub current_term_no: Option<i32>,
    pub enrolment_number: Option<String>,
    pub roll_number: Option<String>,
    pub enrolment_type: String,
    pub enrolled_on: Option<NaiveDate>,
    pub admission_id: Option<Uuid>,
    pub predecessor_context_id: Option<StudentContextId>,
    pub status: StudentContextStatus,
    pub status_reason: Option<String>,
    pub status_changed_at: DateTime<Utc>,
    pub closed_on: Option<NaiveDate>,
    pub version: Version,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

fn decode<T>(raw: String, parse: fn(&str) -> Option<T>, what: &str) -> Result<T, sqlx::Error> {
    parse(&raw).ok_or_else(|| sqlx::Error::Decode(format!("unknown {what} `{raw}`").into()))
}

pub fn student_from_row(r: &PgRow) -> Result<StudentRow, sqlx::Error> {
    Ok(StudentRow {
        id: StudentId(r.try_get("id")?),
        tenant_id: TenantId(r.try_get("tenant_id")?),
        person_id: r.try_get("person_id")?,
        student_number: r.try_get("student_number")?,
        given_name: r.try_get("given_name")?,
        middle_name: r.try_get("middle_name")?,
        family_name: r.try_get("family_name")?,
        preferred_name: r.try_get("preferred_name")?,
        date_of_birth: r.try_get("date_of_birth")?,
        gender: r.try_get("gender")?,
        primary_email: r.try_get("primary_email")?,
        primary_phone: r.try_get("primary_phone")?,
        subject_ref: r.try_get("subject_ref")?,
        origin_admission_id: r.try_get("origin_admission_id")?,
        merged_into_id: r
            .try_get::<Option<Uuid>, _>("merged_into_id")?
            .map(StudentId),
        status: decode(r.try_get("status")?, StudentStatus::parse, "student status")?,
        status_changed_at: r.try_get("status_changed_at")?,
        version: Version(r.try_get("version")?),
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
    })
}

pub fn context_from_row(r: &PgRow) -> Result<StudentContextRow, sqlx::Error> {
    Ok(StudentContextRow {
        id: StudentContextId(r.try_get("id")?),
        tenant_id: TenantId(r.try_get("tenant_id")?),
        student_id: StudentId(r.try_get("student_id")?),
        institution_id: InstitutionId(r.try_get("institution_id")?),
        campus_id: r.try_get("campus_id")?,
        programme_id: r.try_get("programme_id")?,
        curriculum_version_id: r.try_get("curriculum_version_id")?,
        org_unit_id: r.try_get("org_unit_id")?,
        admission_term_id: r.try_get("admission_term_id")?,
        admission_year_id: r.try_get("admission_year_id")?,
        batch_code: r.try_get("batch_code")?,
        section_code: r.try_get("section_code")?,
        current_term_no: r.try_get("current_term_no")?,
        enrolment_number: r.try_get("enrolment_number")?,
        roll_number: r.try_get("roll_number")?,
        enrolment_type: r.try_get("enrolment_type")?,
        enrolled_on: r.try_get("enrolled_on")?,
        admission_id: r.try_get("admission_id")?,
        predecessor_context_id: r
            .try_get::<Option<Uuid>, _>("predecessor_context_id")?
            .map(StudentContextId),
        status: decode(
            r.try_get("status")?,
            StudentContextStatus::parse,
            "student context status",
        )?,
        status_reason: r.try_get("status_reason")?,
        status_changed_at: r.try_get("status_changed_at")?,
        closed_on: r.try_get("closed_on")?,
        version: Version(r.try_get("version")?),
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
    })
}

const STUDENT: KernelError = KernelError::NotFound { entity: "student" };
const CONTEXT: KernelError = KernelError::NotFound {
    entity: "student context",
};

async fn one_student<'e, E: PgExecutor<'e>>(
    executor: E,
    sql: &str,
    tenant: TenantId,
    id: Uuid,
) -> Result<StudentRow, KernelError> {
    let row = sqlx::query(sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(executor)
        .await
        .map_err(map_db_error)?
        .ok_or(STUDENT)?;
    student_from_row(&row).map_err(map_db_error)
}

/// The persona of the tenant (404 when missing / foreign).
pub async fn load_student<'e, E: PgExecutor<'e>>(
    executor: E,
    tenant: TenantId,
    id: StudentId,
) -> Result<StudentRow, KernelError> {
    let sql =
        format!("SELECT {STUDENT_COLUMNS} FROM student.student WHERE id = $1 AND tenant_id = $2");
    one_student(executor, &sql, tenant, id.as_uuid()).await
}

/// Same as [`load_student`] with `FOR UPDATE` (call inside the owner-local transaction).
pub async fn lock_student(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: StudentId,
) -> Result<StudentRow, KernelError> {
    let sql = format!(
        "SELECT {STUDENT_COLUMNS} FROM student.student WHERE id = $1 AND tenant_id = $2 FOR UPDATE"
    );
    one_student(&mut *conn, &sql, tenant, id.as_uuid()).await
}

async fn one_context<'e, E: PgExecutor<'e>>(
    executor: E,
    sql: &str,
    tenant: TenantId,
    id: Uuid,
) -> Result<StudentContextRow, KernelError> {
    let row = sqlx::query(sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(executor)
        .await
        .map_err(map_db_error)?
        .ok_or(CONTEXT)?;
    context_from_row(&row).map_err(map_db_error)
}

pub async fn load_context<'e, E: PgExecutor<'e>>(
    executor: E,
    tenant: TenantId,
    id: StudentContextId,
) -> Result<StudentContextRow, KernelError> {
    let sql = format!(
        "SELECT {CONTEXT_COLUMNS} FROM student.student_context WHERE id = $1 AND tenant_id = $2"
    );
    one_context(executor, &sql, tenant, id.as_uuid()).await
}

/// Same as [`load_context`] with `FOR UPDATE`.
pub async fn lock_context(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: StudentContextId,
) -> Result<StudentContextRow, KernelError> {
    let sql = format!(
        "SELECT {CONTEXT_COLUMNS} FROM student.student_context WHERE id = $1 AND tenant_id = $2 FOR UPDATE"
    );
    one_context(&mut *conn, &sql, tenant, id.as_uuid()).await
}

/// Every context of a student, newest first.
pub async fn contexts_of<'e, E: PgExecutor<'e>>(
    executor: E,
    tenant: TenantId,
    student_id: StudentId,
) -> Result<Vec<StudentContextRow>, KernelError> {
    let sql = format!(
        "SELECT {CONTEXT_COLUMNS} FROM student.student_context \
         WHERE student_id = $1 AND tenant_id = $2 ORDER BY created_at DESC, id DESC"
    );
    let rows = sqlx::query(&sql)
        .bind(student_id.as_uuid())
        .bind(tenant.as_uuid())
        .fetch_all(executor)
        .await
        .map_err(map_db_error)?;
    rows.iter()
        .map(context_from_row)
        .collect::<Result<_, _>>()
        .map_err(map_db_error)
}

/// Contexts in one institution of many students in a single statement (directory page): the
/// same rows and order per student as [`contexts_of`] filtered to `institution_id`, without one
/// round trip (and pool checkout) per student.
pub async fn contexts_in_institution<'e, E: PgExecutor<'e>>(
    executor: E,
    tenant: TenantId,
    institution_id: InstitutionId,
    student_ids: &[Uuid],
) -> Result<Vec<StudentContextRow>, KernelError> {
    if student_ids.is_empty() {
        return Ok(Vec::new());
    }
    let sql = format!(
        "SELECT {CONTEXT_COLUMNS} FROM student.student_context \
         WHERE student_id = ANY($1) AND tenant_id = $2 AND institution_id = $3 \
         ORDER BY student_id, created_at DESC, id DESC"
    );
    let rows = sqlx::query(&sql)
        .bind(student_ids)
        .bind(tenant.as_uuid())
        .bind(institution_id.as_uuid())
        .fetch_all(executor)
        .await
        .map_err(map_db_error)?;
    rows.iter()
        .map(context_from_row)
        .collect::<Result<_, _>>()
        .map_err(map_db_error)
}

/// The live (`pending_activation | active | suspended`) context of a student in an institution.
pub async fn live_context<'e, E: PgExecutor<'e>>(
    executor: E,
    tenant: TenantId,
    student_id: StudentId,
    institution_id: InstitutionId,
) -> Result<Option<StudentContextRow>, KernelError> {
    let sql = format!(
        "SELECT {CONTEXT_COLUMNS} FROM student.student_context \
         WHERE student_id = $1 AND tenant_id = $2 AND institution_id = $3 \
           AND status IN ('pending_activation', 'active', 'suspended')"
    );
    let row = sqlx::query(&sql)
        .bind(student_id.as_uuid())
        .bind(tenant.as_uuid())
        .bind(institution_id.as_uuid())
        .fetch_optional(executor)
        .await
        .map_err(map_db_error)?;
    row.as_ref()
        .map(context_from_row)
        .transpose()
        .map_err(map_db_error)
}

/// The most recent live context of a student in any institution (Student 360 default).
pub async fn current_context<'e, E: PgExecutor<'e>>(
    executor: E,
    tenant: TenantId,
    student_id: StudentId,
) -> Result<Option<StudentContextRow>, KernelError> {
    let sql = format!(
        "SELECT {CONTEXT_COLUMNS} FROM student.student_context \
         WHERE student_id = $1 AND tenant_id = $2 \
           AND status IN ('pending_activation', 'active', 'suspended') \
         ORDER BY created_at DESC, id DESC LIMIT 1"
    );
    let row = sqlx::query(&sql)
        .bind(student_id.as_uuid())
        .bind(tenant.as_uuid())
        .fetch_optional(executor)
        .await
        .map_err(map_db_error)?;
    row.as_ref()
        .map(context_from_row)
        .transpose()
        .map_err(map_db_error)
}

/// Self-service: the (non-archived) student linked to the session subject; 404 when the
/// account has no student record (e.g. an administrator).
pub async fn me<'e, E: PgExecutor<'e>>(
    executor: E,
    ctx: &AuthContext,
) -> Result<StudentRow, KernelError> {
    let sql = format!(
        "SELECT {STUDENT_COLUMNS} FROM student.student \
         WHERE subject_ref = $1 AND tenant_id = $2 AND status <> 'archived'"
    );
    let row = sqlx::query(&sql)
        .bind(&ctx.subject)
        .bind(ctx.scope.tenant_id.as_uuid())
        .fetch_optional(executor)
        .await
        .map_err(map_db_error)?
        .ok_or(STUDENT)?;
    student_from_row(&row).map_err(map_db_error)
}

/// Academic progress fields of a **live** context that 3B owns (progression advances the term;
/// programme migration changes programme + curriculum version). Bumps the context version.
/// The caller holds the row lock ([`lock_context`]) and emits its own event.
pub async fn set_context_progress(
    conn: &mut PgConnection,
    ctx: &StudentContextRow,
    current_term_no: Option<i32>,
    programme_id: Option<Uuid>,
    curriculum_version_id: Option<Uuid>,
) -> Result<StudentContextRow, KernelError> {
    if !ctx.status.is_live() {
        return Err(KernelError::Conflict(format!(
            "student context is {}; its academic progress is read-only",
            ctx.status
        )));
    }
    let sql = format!(
        "UPDATE student.student_context SET current_term_no = $3, programme_id = $4, \
         curriculum_version_id = $5, version = version + 1, updated_at = now() \
         WHERE id = $1 AND tenant_id = $2 AND version = $6 RETURNING {CONTEXT_COLUMNS}"
    );
    let row = sqlx::query(&sql)
        .bind(ctx.id.as_uuid())
        .bind(ctx.tenant_id.as_uuid())
        .bind(current_term_no)
        .bind(programme_id)
        .bind(curriculum_version_id)
        .bind(ctx.version.0)
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or_else(|| KernelError::Conflict("student context changed concurrently".into()))?;
    context_from_row(&row).map_err(map_db_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_name_skips_empty_parts() {
        let s = StudentRow {
            id: StudentId::new(),
            tenant_id: TenantId::new(),
            person_id: None,
            student_number: "S1".into(),
            given_name: "Aarav".into(),
            middle_name: Some(" ".into()),
            family_name: Some("Desai".into()),
            preferred_name: None,
            date_of_birth: None,
            gender: None,
            primary_email: None,
            primary_phone: None,
            subject_ref: None,
            origin_admission_id: None,
            merged_into_id: None,
            status: StudentStatus::Active,
            status_changed_at: Utc::now(),
            version: Version(1),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        assert_eq!(s.display_name(), "Aarav Desai");
        assert!(STUDENT_COLUMNS.contains("subject_ref"));
        assert!(CONTEXT_COLUMNS.contains("current_term_no"));
    }
}
