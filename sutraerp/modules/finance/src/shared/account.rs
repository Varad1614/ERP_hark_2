//! Finance party accounts (`finance.student_account`, contract §3.2): one per student per
//! institution, or one per pre-student admission (applicant). Created only through
//! [`ensure_student_account`] / [`ensure_applicant_account`] (idempotent; the first creation
//! emits `student_account.opened` + audit in the caller's transaction).

use crate::shared::publish::{audit, event};
use crate::shared::Actor;
use serde_json::json;
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use sutra_contracts::finance::events::{
    StudentAccountOpenedV1, STUDENT_ACCOUNT_OPENED, STUDENT_ACCOUNT_OPENED_SCHEMA,
};
use sutra_contracts::finance::ids::StudentAccountId;
use sutra_contracts::finance::ports::StudentAccountSummary;
use sutra_contracts::finance::status::{PartyKind, StudentAccountStatus};
use sutra_contracts::student::ports::StudentSummary;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

/// Columns of [`account_from_row`].
pub const ACCOUNT_COLUMNS: &str = "id, tenant_id, institution_id, party_kind, student_id, \
    admission_id, student_number, display_name, subject_ref, currency, status, version";

/// A party account row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StudentAccountRow {
    pub tenant_id: TenantId,
    pub version: i64,
    pub account: StudentAccountSummary,
}

impl StudentAccountRow {
    pub fn id(&self) -> StudentAccountId {
        self.account.id
    }
}

pub fn account_from_row(r: &PgRow) -> Result<StudentAccountRow, sqlx::Error> {
    let kind: String = r.try_get("party_kind")?;
    let st: String = r.try_get("status")?;
    Ok(StudentAccountRow {
        tenant_id: TenantId(r.try_get("tenant_id")?),
        version: r.try_get("version")?,
        account: StudentAccountSummary {
            id: StudentAccountId(r.try_get("id")?),
            institution_id: InstitutionId(r.try_get("institution_id")?),
            party_kind: PartyKind::parse(&kind)
                .ok_or_else(|| sqlx::Error::Decode(format!("party kind `{kind}`").into()))?,
            student_id: r.try_get("student_id")?,
            admission_id: r.try_get("admission_id")?,
            student_number: r.try_get("student_number")?,
            display_name: r.try_get("display_name")?,
            subject_ref: r.try_get("subject_ref")?,
            currency: r.try_get("currency")?,
            status: StudentAccountStatus::parse(&st)
                .ok_or_else(|| sqlx::Error::Decode(format!("account status `{st}`").into()))?,
        },
    })
}

/// Loads an account (tenant-checked; `None` when missing / foreign).
pub async fn load_account(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: StudentAccountId,
) -> Result<Option<StudentAccountRow>, KernelError> {
    let row = sqlx::query(&format!(
        "SELECT {ACCOUNT_COLUMNS} FROM finance.student_account WHERE tenant_id = $1 AND id = $2"
    ))
    .bind(tenant.as_uuid())
    .bind(id.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    row.as_ref()
        .map(account_from_row)
        .transpose()
        .map_err(map_db_error)
}

/// The account of a student in an institution, created on first use from the Domain C
/// student summary (snapshot of number, name and login subject).
pub async fn ensure_student_account(
    conn: &mut PgConnection,
    tenant: TenantId,
    institution: InstitutionId,
    student: &StudentSummary,
    actor: &Actor,
) -> Result<StudentAccountRow, KernelError> {
    let inserted: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO finance.student_account (tenant_id, institution_id, party_kind, student_id, \
         student_number, display_name, subject_ref) VALUES ($1, $2, 'student', $3, $4, $5, $6) \
         ON CONFLICT (tenant_id, institution_id, student_id) WHERE student_id IS NOT NULL \
         DO NOTHING RETURNING id",
    )
    .bind(tenant.as_uuid())
    .bind(institution.as_uuid())
    .bind(student.id.as_uuid())
    .bind(&student.student_number)
    .bind(&student.display_name)
    .bind(student.subject_ref.as_deref())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let row = sqlx::query(&format!(
        "SELECT {ACCOUNT_COLUMNS} FROM finance.student_account \
         WHERE tenant_id = $1 AND institution_id = $2 AND student_id = $3"
    ))
    .bind(tenant.as_uuid())
    .bind(institution.as_uuid())
    .bind(student.id.as_uuid())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let acc = account_from_row(&row).map_err(map_db_error)?;
    if inserted.is_some() {
        opened(conn, &acc, actor).await?;
    }
    Ok(acc)
}

/// The applicant account of a pre-student admission (L2-F01 amendment 2026-10-05), created on
/// first use.
pub async fn ensure_applicant_account(
    conn: &mut PgConnection,
    tenant: TenantId,
    institution: InstitutionId,
    admission_id: Uuid,
    display_name: &str,
    subject_ref: Option<&str>,
    actor: &Actor,
) -> Result<StudentAccountRow, KernelError> {
    let inserted: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO finance.student_account (tenant_id, institution_id, party_kind, admission_id, \
         display_name, subject_ref) VALUES ($1, $2, 'applicant', $3, $4, $5) \
         ON CONFLICT (tenant_id, institution_id, admission_id) \
         WHERE admission_id IS NOT NULL AND student_id IS NULL DO NOTHING RETURNING id",
    )
    .bind(tenant.as_uuid())
    .bind(institution.as_uuid())
    .bind(admission_id)
    .bind(display_name)
    .bind(subject_ref)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let row = sqlx::query(&format!(
        "SELECT {ACCOUNT_COLUMNS} FROM finance.student_account \
         WHERE tenant_id = $1 AND institution_id = $2 AND admission_id = $3 AND student_id IS NULL"
    ))
    .bind(tenant.as_uuid())
    .bind(institution.as_uuid())
    .bind(admission_id)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let acc = account_from_row(&row).map_err(map_db_error)?;
    if inserted.is_some() {
        opened(conn, &acc, actor).await?;
    }
    Ok(acc)
}

async fn opened(
    conn: &mut PgConnection,
    acc: &StudentAccountRow,
    actor: &Actor,
) -> Result<(), KernelError> {
    let scope = ExecutionScope::institution(acc.tenant_id, acc.account.institution_id);
    let a = &acc.account;
    event(
        conn,
        &scope,
        STUDENT_ACCOUNT_OPENED,
        STUDENT_ACCOUNT_OPENED_SCHEMA,
        a.id.to_string(),
        &StudentAccountOpenedV1 {
            student_account_id: a.id.as_uuid(),
            institution_id: a.institution_id.as_uuid(),
            party_kind: a.party_kind.as_str().to_owned(),
            student_id: a.student_id,
            admission_id: a.admission_id,
            student_subject_ref: a.subject_ref.clone(),
        },
        actor,
    )
    .await?;
    audit(
        conn,
        &scope,
        "finance.student_account.open",
        "student_account",
        a.id.to_string(),
        json!({"partyKind": a.party_kind, "studentId": a.student_id, "admissionId": a.admission_id}),
        actor,
    )
    .await
}
