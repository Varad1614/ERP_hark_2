//! Payslips of finalised payroll results (`PS-` numbers): bulk issue per run, reissue after a
//! corrective revision, withdrawal; the staff member's own payslips.

use crate::payroll::common::{
    cause, ensure_version, history, illegal, opt_reason, payload, required_reason, Change, Cmd,
    PayrollBulkFailure, PayrollBulkSummary,
};
use crate::payroll::domain::{payslip_can_transition, run_is_final};
use crate::payroll::rows::{current_results, load_result, load_run, ResultRow};
use crate::shared::numbering::{self, kind};
use crate::shared::publish::{audit, event};
use crate::shared::{Actor, PeopleState};
use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use std::collections::BTreeMap;
use sutra_contracts::people::events::payroll::{PAYSLIP_ISSUED, PAYSLIP_ISSUED_SCHEMA};
use sutra_contracts::people::ids::StaffMemberId;
use sutra_contracts::people::status::{PayrollResultStatus, PayslipStatus};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

const PAYSLIP: &str = "payslip";

#[derive(Clone, Debug)]
pub struct PayslipRow {
    pub id: Uuid,
    pub institution_id: InstitutionId,
    pub run_id: Uuid,
    pub result_id: Uuid,
    pub origin_result_id: Uuid,
    pub staff_id: Uuid,
    pub number: String,
    pub status: PayslipStatus,
    pub status_reason: Option<String>,
    pub issued_at: DateTime<Utc>,
    pub reissued_at: Option<DateTime<Utc>>,
    pub withdrawn_at: Option<DateTime<Utc>>,
    pub version: i64,
    /// The result revision shown.
    pub result: Option<ResultRow>,
}

const COLUMNS: &str = "p.id, p.institution_id, p.run_id, p.result_id, p.origin_result_id, \
    p.staff_id, p.number, p.status, p.status_reason, p.issued_at, p.reissued_at, p.withdrawn_at, \
    p.version";

fn from_row(r: &PgRow) -> Result<PayslipRow, sqlx::Error> {
    Ok(PayslipRow {
        id: r.try_get("id")?,
        institution_id: InstitutionId(r.try_get("institution_id")?),
        run_id: r.try_get("run_id")?,
        result_id: r.try_get("result_id")?,
        origin_result_id: r.try_get("origin_result_id")?,
        staff_id: r.try_get("staff_id")?,
        number: r.try_get("number")?,
        status: crate::shared::rows::decode(r.try_get("status")?, PayslipStatus::parse)?,
        status_reason: r.try_get("status_reason")?,
        issued_at: r.try_get("issued_at")?,
        reissued_at: r.try_get("reissued_at")?,
        withdrawn_at: r.try_get("withdrawn_at")?,
        version: r.try_get("version")?,
        result: None,
    })
}

async fn with_result(
    conn: &mut PgConnection,
    tenant: TenantId,
    mut p: PayslipRow,
) -> Result<PayslipRow, KernelError> {
    p.result = Some(load_result(conn, tenant, p.result_id, false).await?);
    Ok(p)
}

async fn load(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<PayslipRow, KernelError> {
    let sql = format!(
        "SELECT {COLUMNS} FROM people.payslip p WHERE p.id = $1 AND p.tenant_id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    let row = sqlx::query(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound { entity: "payslip" })?;
    let p = from_row(&row).map_err(map_db_error)?;
    with_result(conn, tenant, p).await
}

async fn many(
    conn: &mut PgConnection,
    tenant: TenantId,
    rows: &[PgRow],
) -> Result<Vec<PayslipRow>, KernelError> {
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let p = from_row(r).map_err(map_db_error)?;
        out.push(with_result(conn, tenant, p).await?);
    }
    Ok(out)
}

pub async fn get_payslip(
    st: &PeopleState,
    tenant: TenantId,
    id: Uuid,
) -> Result<PayslipRow, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    load(&mut conn, tenant, id, false).await
}

pub async fn list_for_run(
    st: &PeopleState,
    tenant: TenantId,
    run: Uuid,
) -> Result<Vec<PayslipRow>, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    load_run(&mut conn, tenant, run, false).await?;
    let sql = format!(
        "SELECT {COLUMNS} FROM people.payslip p WHERE p.tenant_id = $1 AND p.run_id = $2 \
         ORDER BY p.number"
    );
    let rows = sqlx::query(&sql)
        .bind(tenant.as_uuid())
        .bind(run)
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db_error)?;
    many(&mut conn, tenant, &rows).await
}

/// The staff member's payslips (not withdrawn), newest period first.
pub async fn list_for_staff(
    st: &PeopleState,
    tenant: TenantId,
    staff: StaffMemberId,
) -> Result<Vec<PayslipRow>, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let sql = format!(
        "SELECT {COLUMNS} FROM people.payslip p \
         JOIN people.payroll_result r ON r.id = p.result_id \
         WHERE p.tenant_id = $1 AND p.staff_id = $2 AND p.status <> 'withdrawn' \
         ORDER BY r.period_start DESC, p.number DESC"
    );
    let rows = sqlx::query(&sql)
        .bind(tenant.as_uuid())
        .bind(staff.as_uuid())
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db_error)?;
    many(&mut conn, tenant, &rows).await
}

/// Issues one payslip per current finalised result of a final run that has none (bulk,
/// idempotent: results already holding a live payslip are skipped). `payslip.issued` each.
pub async fn issue_payslips(c: &Cmd<'_>, run_id: Uuid) -> Result<PayrollBulkSummary, KernelError> {
    let mut conn = c.st.pool.acquire().await.map_err(map_db_error)?;
    let run = load_run(&mut conn, c.tenant, run_id, false).await?;
    if !run_is_final(run.status) {
        return Err(KernelError::Conflict(format!(
            "payroll run {} is {}; payslips are issued from finalised results",
            run.number, run.status
        )));
    }
    let scope = c.scope(run.institution_id);
    let results: Vec<ResultRow> = current_results(&mut conn, c.tenant, run_id)
        .await?
        .into_iter()
        .filter(|r| r.status == PayrollResultStatus::Finalized)
        .collect();
    drop(conn);
    let mut subjects = BTreeMap::new();
    for r in &results {
        let s =
            c.st.directory
                .staff(&scope, StaffMemberId(r.staff_id))
                .await?;
        subjects.insert(r.staff_id, s.and_then(|s| s.subject_ref));
    }
    let mut summary = PayrollBulkSummary::default();
    let mut tx = c.st.pool.begin().await.map_err(map_db_error)?;
    load_run(&mut tx, c.tenant, run_id, true).await?;
    for r in &results {
        let live: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM people.payslip WHERE origin_result_id = $1 \
             AND status <> 'withdrawn')",
        )
        .bind(r.origin_result_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
        if live {
            summary.skipped += 1;
            continue;
        }
        if r.net_pay.is_sign_negative() {
            summary.failed.push(PayrollBulkFailure {
                id: r.id,
                reason: "negative net pay".into(),
            });
            continue;
        }
        let number = numbering::next(&mut tx, c.tenant, run.institution_id, kind::PAYSLIP).await?;
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO people.payslip (tenant_id, institution_id, run_id, result_id, \
             origin_result_id, staff_id, number) VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
        )
        .bind(c.tenant.as_uuid())
        .bind(run.institution_id.as_uuid())
        .bind(run_id)
        .bind(r.id)
        .bind(r.origin_result_id)
        .bind(r.staff_id)
        .bind(&number)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
        history(
            &mut tx,
            c.tenant,
            run.institution_id,
            PAYSLIP,
            id,
            None,
            PayslipStatus::Issued.as_str(),
            &cause("people.payslip.issued", None, None),
            c.actor,
        )
        .await?;
        let subject = subjects.get(&r.staff_id).cloned().flatten();
        event(
            &mut tx,
            &scope,
            PAYSLIP_ISSUED,
            PAYSLIP_ISSUED_SCHEMA,
            id.to_string(),
            &payload(&Change {
                aggregate_id: id,
                institution: Some(run.institution_id),
                staff_id: Some(r.staff_id),
                engagement_id: Some(r.engagement_id),
                to: PayslipStatus::Issued.as_str(),
                version: 1,
                reason: Some(&number),
                staff_subject_ref: subject.as_deref(),
                ..Change::default()
            }),
            c.actor,
        )
        .await?;
        summary.processed += 1;
    }
    audit(
        &mut tx,
        &scope,
        "people.payslip.issue",
        "payroll_run",
        run_id.to_string(),
        json!({"processed": summary.processed, "skipped": summary.skipped,
               "failed": summary.failed.len()}),
        c.actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(summary)
}

async fn move_payslip(
    conn: &mut PgConnection,
    c: &Cmd<'_>,
    p: &PayslipRow,
    to: PayslipStatus,
    reason: Option<&str>,
    result_id: Uuid,
    actor: &Actor,
) -> Result<PayslipRow, KernelError> {
    if !payslip_can_transition(p.status, to) {
        return Err(illegal(
            "payslip",
            &p.number,
            p.status.as_str(),
            to.as_str(),
        ));
    }
    sqlx::query(
        "UPDATE people.payslip SET status = $2, status_reason = $3, result_id = $4, \
         reissued_at = CASE WHEN $2 = 'reissued' THEN now() ELSE reissued_at END, \
         withdrawn_at = CASE WHEN $2 = 'withdrawn' THEN now() ELSE withdrawn_at END, \
         version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(p.id)
    .bind(to.as_str())
    .bind(reason)
    .bind(result_id)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    history(
        conn,
        c.tenant,
        p.institution_id,
        PAYSLIP,
        p.id,
        Some(p.status.as_str()),
        to.as_str(),
        &cause("people.payslip.status", reason, None),
        actor,
    )
    .await?;
    load(conn, c.tenant, p.id, false).await
}

/// Reissues a payslip on the current revision of its result (after an applied adjustment).
pub async fn reissue_payslip(
    c: &Cmd<'_>,
    id: Uuid,
    expected: i64,
    reason: Option<&str>,
) -> Result<PayslipRow, KernelError> {
    let reason = opt_reason(reason);
    let current = get_payslip(c.st, c.tenant, id).await?;
    let scope = c.scope(current.institution_id);
    let subject =
        c.st.directory
            .staff(&scope, StaffMemberId(current.staff_id))
            .await?
            .and_then(|s| s.subject_ref);
    let mut tx = c.st.pool.begin().await.map_err(map_db_error)?;
    let p = load(&mut tx, c.tenant, id, true).await?;
    ensure_version(expected, p.version)?;
    let revision: Uuid = sqlx::query_scalar(
        "SELECT id FROM people.payroll_result WHERE origin_result_id = $1 \
         AND status = 'finalized' ORDER BY revision DESC LIMIT 1",
    )
    .bind(p.origin_result_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?
    .ok_or_else(|| {
        KernelError::Conflict("the payslip's result has no finalised revision".into())
    })?;
    let out = move_payslip(
        &mut tx,
        c,
        &p,
        PayslipStatus::Reissued,
        reason.as_deref(),
        revision,
        c.actor,
    )
    .await?;
    event(
        &mut tx,
        &scope,
        PAYSLIP_ISSUED,
        PAYSLIP_ISSUED_SCHEMA,
        id.to_string(),
        &payload(&Change {
            aggregate_id: id,
            institution: Some(p.institution_id),
            staff_id: Some(p.staff_id),
            from: Some(p.status.as_str()),
            to: out.status.as_str(),
            version: out.version,
            reason: Some(&out.number),
            staff_subject_ref: subject.as_deref(),
            ..Change::default()
        }),
        c.actor,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "people.payslip.reissue",
        PAYSLIP,
        id.to_string(),
        json!({"resultId": revision, "reason": reason}),
        c.actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

/// Withdraws a payslip (reason required); a new one may then be issued for the result.
pub async fn withdraw_payslip(
    c: &Cmd<'_>,
    id: Uuid,
    expected: i64,
    reason: Option<&str>,
) -> Result<PayslipRow, KernelError> {
    let reason = required_reason(reason)?;
    let mut tx = c.st.pool.begin().await.map_err(map_db_error)?;
    let p = load(&mut tx, c.tenant, id, true).await?;
    ensure_version(expected, p.version)?;
    let result_id = p.result_id;
    let out = move_payslip(
        &mut tx,
        c,
        &p,
        PayslipStatus::Withdrawn,
        Some(&reason),
        result_id,
        c.actor,
    )
    .await?;
    audit(
        &mut tx,
        &c.scope(p.institution_id),
        "people.payslip.withdraw",
        PAYSLIP,
        id.to_string(),
        json!({"reason": reason}),
        c.actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}
