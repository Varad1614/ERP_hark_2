//! Fee heads and versioned fee structures (L3-F-001): draft → in review (WorkflowPort
//! `FEE_STRUCTURE_APPROVAL`) → approved → published (supersedes the previous published version
//! of the same code) → charge generation for a population of students (each charge validated
//! and published, idempotent per structure and student).

use super::domain::structure_can_transition;
use super::kit::{invalid, require_version, status_event, StatusEvent};
use super::repo::{create_in_tx, publish_locked, validate_locked, ChargeInput};
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::charge::{lock_charge, NewChargeLine};
use crate::shared::coa::{ensure_default_chart, resolve};
use crate::shared::lifecycle::{record_history, HistoryEntry, StatusChange};
use crate::shared::money::fmt;
use crate::shared::publish::audit;
use crate::shared::time::business_today;
use crate::shared::{Actor, FinanceState};
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::Serialize;
use serde_json::json;
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use std::collections::BTreeMap;
use sutra_contracts::finance::events::charges::{
    FEE_STRUCTURE_PUBLISHED, FEE_STRUCTURE_PUBLISHED_SCHEMA,
};
use sutra_contracts::finance::ids::{FeeHeadId, FeeStructureId, LedgerAccountId};
use sutra_contracts::finance::ports::{AccountRef, SystemAccount};
use sutra_contracts::finance::status::{
    ChargeSourceType, FeeCategory, FeeHeadStatus, FeeStructureStatus,
};
use sutra_contracts::student::ids::StudentId;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use utoipa::ToSchema;
use uuid::Uuid;

// ================================================================ fee heads

/// A fee head (what a charge line is for, and which income account it credits).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FeeHeadView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub code: String,
    pub name: String,
    pub category: FeeCategory,
    pub income_account_id: Uuid,
    pub income_account_code: String,
    pub refundable: bool,
    pub status: FeeHeadStatus,
    pub version: i64,
}

const HEAD_COLUMNS: &str =
    "h.id, h.institution_id, h.code, h.name, h.category, h.income_account_id, \
    a.code AS account_code, h.refundable, h.status, h.version";

fn head_from_row(r: &PgRow) -> Result<FeeHeadView, sqlx::Error> {
    let cat: String = r.try_get("category")?;
    let st: String = r.try_get("status")?;
    Ok(FeeHeadView {
        id: r.try_get("id")?,
        institution_id: r.try_get("institution_id")?,
        code: r.try_get("code")?,
        name: r.try_get("name")?,
        category: FeeCategory::parse(&cat)
            .ok_or_else(|| sqlx::Error::Decode(cat.clone().into()))?,
        income_account_id: r.try_get("income_account_id")?,
        income_account_code: r.try_get("account_code")?,
        refundable: r.try_get("refundable")?,
        status: FeeHeadStatus::parse(&st).ok_or_else(|| sqlx::Error::Decode(st.clone().into()))?,
        version: r.try_get("version")?,
    })
}

async fn fetch_head(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: FeeHeadId,
) -> Result<FeeHeadView, KernelError> {
    let r = sqlx::query(&format!(
        "SELECT {HEAD_COLUMNS} FROM finance.fee_head h JOIN finance.ledger_account a ON a.id = h.income_account_id \
         WHERE h.tenant_id = $1 AND h.id = $2"
    ))
    .bind(tenant.as_uuid())
    .bind(id.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound { entity: "fee head" })?;
    head_from_row(&r).map_err(map_db_error)
}

pub async fn get_head(
    state: &FinanceState,
    tenant: TenantId,
    id: FeeHeadId,
) -> Result<FeeHeadView, KernelError> {
    let mut conn = state.pool.acquire().await.map_err(map_db_error)?;
    fetch_head(&mut conn, tenant, id).await
}

pub async fn list_heads(
    state: &FinanceState,
    tenant: TenantId,
    institution: InstitutionId,
) -> Result<Vec<FeeHeadView>, KernelError> {
    sqlx::query(&format!(
        "SELECT {HEAD_COLUMNS} FROM finance.fee_head h JOIN finance.ledger_account a ON a.id = h.income_account_id \
         WHERE h.tenant_id = $1 AND h.institution_id = $2 ORDER BY h.code"
    ))
    .bind(tenant.as_uuid())
    .bind(institution.as_uuid())
    .fetch_all(&state.pool)
    .await
    .map_err(map_db_error)?
    .iter()
    .map(head_from_row)
    .collect::<Result<_, _>>()
    .map_err(map_db_error)
}

/// The default income account of a category: tuition / admission / examination / laboratory /
/// library → fee income; the rest → other fee income; deposits → refundable deposits held.
pub fn default_income(category: FeeCategory) -> SystemAccount {
    match category {
        FeeCategory::Tuition
        | FeeCategory::Admission
        | FeeCategory::Examination
        | FeeCategory::Laboratory
        | FeeCategory::Library => SystemAccount::FeeIncome,
        FeeCategory::Deposit => SystemAccount::DepositsHeld,
        _ => SystemAccount::OtherIncome,
    }
}

/// A new fee head.
pub struct NewFeeHead {
    pub code: String,
    pub name: String,
    pub category: FeeCategory,
    pub income_account_id: Option<Uuid>,
    pub refundable: bool,
}

/// Creates an active fee head (the default chart is seeded first).
pub async fn create_head(
    state: &FinanceState,
    scope: &ExecutionScope,
    institution: InstitutionId,
    h: NewFeeHead,
    actor: &Actor,
) -> Result<FeeHeadView, KernelError> {
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let v = create_head_in(&mut tx, scope, institution, h, actor).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(v)
}

/// [`create_head`] inside the caller's transaction (Part 2 · S4-import runs it per row).
pub async fn create_head_in(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    institution: InstitutionId,
    h: NewFeeHead,
    actor: &Actor,
) -> Result<FeeHeadView, KernelError> {
    let tenant = scope.tenant_id;
    let code = h.code.trim().to_uppercase();
    if !code
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric())
        || code.len() < 2
        || code.len() > 32
        || !code
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_' || c == '-')
    {
        return Err(invalid(
            "code",
            "invalid_code",
            "2–32 characters: A–Z, 0–9, _ or -",
        ));
    }
    let name = super::kit::require_text("name", Some(&h.name))?;
    ensure_default_chart(&mut *conn, tenant, institution).await?;
    let account = match h.income_account_id {
        Some(id) => {
            resolve(
                &mut *conn,
                tenant,
                institution,
                AccountRef::Id(LedgerAccountId(id)),
            )
            .await?
        }
        None => {
            resolve(
                &mut *conn,
                tenant,
                institution,
                AccountRef::System(default_income(h.category)),
            )
            .await?
        }
    };
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO finance.fee_head (tenant_id, institution_id, code, name, category, income_account_id, refundable) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
    )
    .bind(tenant.as_uuid())
    .bind(institution.as_uuid())
    .bind(&code)
    .bind(&name)
    .bind(h.category.as_str())
    .bind(account.id.as_uuid())
    .bind(h.refundable)
    .fetch_one(&mut *conn)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(d) if d.is_unique_violation() => {
            KernelError::Conflict(format!("fee head {code} already exists"))
        }
        _ => map_db_error(e),
    })?;
    audit(
        &mut *conn,
        scope,
        "finance.fee_head.create",
        "fee_head",
        id.to_string(),
        json!({"code": code, "category": h.category, "incomeAccount": account.code}),
        actor,
    )
    .await?;
    let v = fetch_head(&mut *conn, tenant, FeeHeadId(id)).await?;
    Ok(v)
}

/// Renames / retires / reactivates a fee head (lines already raised keep their snapshot).
#[allow(clippy::too_many_arguments)]
pub async fn update_head(
    state: &FinanceState,
    tenant: TenantId,
    id: FeeHeadId,
    expected: i64,
    name: Option<String>,
    refundable: Option<bool>,
    status: Option<FeeHeadStatus>,
    actor: &Actor,
) -> Result<FeeHeadView, KernelError> {
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let h = fetch_head(&mut tx, tenant, id).await?;
    require_version(h.version, expected)?;
    let n = sqlx::query(
        "UPDATE finance.fee_head SET name = COALESCE($3, name), refundable = COALESCE($4, refundable), \
         status = COALESCE($5, status), version = version + 1, updated_at = now() \
         WHERE tenant_id = $1 AND id = $2 AND version = $6",
    )
    .bind(tenant.as_uuid())
    .bind(id.as_uuid())
    .bind(name.as_deref())
    .bind(refundable)
    .bind(status.map(|s| s.as_str()))
    .bind(expected)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?
    .rows_affected();
    if n != 1 {
        return Err(KernelError::Conflict(
            "fee head changed concurrently".into(),
        ));
    }
    let scope = ExecutionScope::institution(tenant, InstitutionId(h.institution_id));
    audit(
        &mut tx,
        &scope,
        "finance.fee_head.update",
        "fee_head",
        id.to_string(),
        json!({"name": name, "refundable": refundable, "status": status}),
        actor,
    )
    .await?;
    let v = fetch_head(&mut tx, tenant, id).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(v)
}

// ================================================================ fee structures

/// One line of a fee structure.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FeeStructureLineView {
    pub line_no: i32,
    pub fee_head_id: Uuid,
    pub fee_head_code: String,
    pub description: String,
    #[schema(example = "60000.00")]
    pub amount: String,
}

/// A fee structure version.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FeeStructureView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub code: String,
    pub version_no: i32,
    pub name: String,
    pub academic_year_id: Option<Uuid>,
    pub academic_term_id: Option<Uuid>,
    pub programme_id: Option<Uuid>,
    pub due_date: Option<NaiveDate>,
    pub currency: String,
    #[schema(example = "125000.00")]
    pub total_amount: String,
    pub status: FeeStructureStatus,
    pub status_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub approved_by: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
    pub superseded_by_id: Option<Uuid>,
    /// Charges generated from this version (any status).
    pub charges_generated: i64,
    pub lines: Vec<FeeStructureLineView>,
    pub version: i64,
}

async fn fetch_structure(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: FeeStructureId,
    lock: bool,
) -> Result<FeeStructureView, KernelError> {
    let r = sqlx::query(&format!(
        "SELECT s.*, (SELECT count(*) FROM finance.charge c WHERE c.tenant_id = s.tenant_id \
         AND c.source_type = 'fee_structure' AND c.source_id = s.id) AS charges_generated \
         FROM finance.fee_structure s WHERE s.tenant_id = $1 AND s.id = $2{}",
        if lock { " FOR UPDATE OF s" } else { "" }
    ))
    .bind(tenant.as_uuid())
    .bind(id.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "fee structure",
    })?;
    let lines = sqlx::query(
        "SELECT l.line_no, l.fee_head_id, h.code, l.description, l.amount FROM finance.fee_structure_line l \
         JOIN finance.fee_head h ON h.id = l.fee_head_id WHERE l.tenant_id = $1 AND l.fee_structure_id = $2 \
         ORDER BY l.line_no",
    )
    .bind(tenant.as_uuid())
    .bind(id.as_uuid())
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?
    .iter()
    .map(|l| {
        Ok(FeeStructureLineView {
            line_no: l.try_get("line_no")?,
            fee_head_id: l.try_get("fee_head_id")?,
            fee_head_code: l.try_get("code")?,
            description: l.try_get("description")?,
            amount: fmt(l.try_get("amount")?),
        })
    })
    .collect::<Result<Vec<_>, sqlx::Error>>()
    .map_err(map_db_error)?;
    let st: String = r.try_get("status").map_err(map_db_error)?;
    let v = (|| -> Result<FeeStructureView, sqlx::Error> {
        Ok(FeeStructureView {
            id: r.try_get("id")?,
            institution_id: r.try_get("institution_id")?,
            code: r.try_get("code")?,
            version_no: r.try_get("version_no")?,
            name: r.try_get("name")?,
            academic_year_id: r.try_get("academic_year_id")?,
            academic_term_id: r.try_get("academic_term_id")?,
            programme_id: r.try_get("programme_id")?,
            due_date: r.try_get("due_date")?,
            currency: r.try_get("currency")?,
            total_amount: fmt(r.try_get("total_amount")?),
            status: FeeStructureStatus::parse(&st)
                .ok_or_else(|| sqlx::Error::Decode(st.clone().into()))?,
            status_reason: r.try_get("status_reason")?,
            process_instance_id: r.try_get("process_instance_id")?,
            approved_by: r.try_get("approved_by")?,
            published_at: r.try_get("published_at")?,
            superseded_by_id: r.try_get("superseded_by_id")?,
            charges_generated: r.try_get("charges_generated")?,
            lines,
            version: r.try_get("version")?,
        })
    })()
    .map_err(map_db_error)?;
    Ok(v)
}

pub async fn get_structure(
    state: &FinanceState,
    tenant: TenantId,
    id: FeeStructureId,
) -> Result<FeeStructureView, KernelError> {
    let mut conn = state.pool.acquire().await.map_err(map_db_error)?;
    fetch_structure(&mut conn, tenant, id, false).await
}

pub async fn list_structures(
    state: &FinanceState,
    tenant: TenantId,
    institution: InstitutionId,
    status: Option<FeeStructureStatus>,
) -> Result<Vec<FeeStructureView>, KernelError> {
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM finance.fee_structure WHERE tenant_id = $1 AND institution_id = $2 \
         AND ($3::text IS NULL OR status = $3) ORDER BY code, version_no DESC",
    )
    .bind(tenant.as_uuid())
    .bind(institution.as_uuid())
    .bind(status.map(|s| s.as_str()))
    .fetch_all(&state.pool)
    .await
    .map_err(map_db_error)?;
    let mut conn = state.pool.acquire().await.map_err(map_db_error)?;
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        out.push(fetch_structure(&mut conn, tenant, FeeStructureId(id), false).await?);
    }
    Ok(out)
}

/// A new fee structure version (validated lines).
pub struct NewStructure {
    pub code: String,
    pub name: String,
    pub academic_year_id: Option<Uuid>,
    pub academic_term_id: Option<Uuid>,
    pub programme_id: Option<Uuid>,
    pub due_date: Option<NaiveDate>,
    pub lines: Vec<NewChargeLine>,
}

/// Creates a draft version (next version number of the code); every fee head must be active.
pub async fn create_structure(
    state: &FinanceState,
    scope: &ExecutionScope,
    institution: InstitutionId,
    s: NewStructure,
    actor: &Actor,
) -> Result<FeeStructureView, KernelError> {
    let tenant = scope.tenant_id;
    let code = s.code.trim().to_uppercase();
    if code.len() < 2 || code.len() > 32 {
        return Err(invalid("code", "invalid_code", "2–32 characters"));
    }
    if s.lines.is_empty() {
        return Err(invalid("lines", "required", "at least one line"));
    }
    if let Some(term) = s.academic_term_id {
        let found = state.references.academic_term(scope, term).await?;
        if found.is_none() {
            return Err(invalid(
                "academicTermId",
                "not_found",
                "unknown academic term",
            ));
        }
    }
    let total: Decimal = s.lines.iter().map(|l| l.amount).sum();
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let version_no: i32 = sqlx::query_scalar(
        "SELECT COALESCE(max(version_no), 0) + 1 FROM finance.fee_structure \
         WHERE tenant_id = $1 AND institution_id = $2 AND code = $3",
    )
    .bind(tenant.as_uuid())
    .bind(institution.as_uuid())
    .bind(&code)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO finance.fee_structure (tenant_id, institution_id, code, version_no, name, \
         academic_year_id, academic_term_id, programme_id, due_date, total_amount) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING id",
    )
    .bind(tenant.as_uuid())
    .bind(institution.as_uuid())
    .bind(&code)
    .bind(version_no)
    .bind(s.name.trim())
    .bind(s.academic_year_id)
    .bind(s.academic_term_id)
    .bind(s.programme_id)
    .bind(s.due_date)
    .bind(total)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    for (i, l) in s.lines.iter().enumerate() {
        let n = sqlx::query(
            "INSERT INTO finance.fee_structure_line (tenant_id, fee_structure_id, line_no, fee_head_id, description, amount) \
             SELECT $1, $2, $3, h.id, $5, $6 FROM finance.fee_head h WHERE h.tenant_id = $1 \
             AND h.institution_id = $7 AND h.id = $4 AND h.status = 'active'",
        )
        .bind(tenant.as_uuid())
        .bind(id)
        .bind(i32::try_from(i + 1).unwrap_or(i32::MAX))
        .bind(l.fee_head_id.as_uuid())
        .bind(&l.description)
        .bind(l.amount)
        .bind(institution.as_uuid())
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?
        .rows_affected();
        if n != 1 {
            return Err(invalid(
                &format!("lines[{i}].feeHeadId"),
                "not_active",
                "not an active fee head of this institution",
            ));
        }
    }
    audit(
        &mut tx,
        scope,
        "finance.fee_structure.create",
        "fee_structure",
        id.to_string(),
        json!({"code": code, "versionNo": version_no, "total": fmt(total)}),
        actor,
    )
    .await?;
    let v = fetch_structure(&mut tx, tenant, FeeStructureId(id), false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(v)
}

#[allow(clippy::too_many_arguments)]
async fn move_structure(
    conn: &mut PgConnection,
    tenant: TenantId,
    s: &FeeStructureView,
    to: FeeStructureStatus,
    reason: Option<&str>,
    pid: Option<&str>,
    extra_sql: &str,
    actor: &Actor,
) -> Result<(), KernelError> {
    if !structure_can_transition(s.status, to) {
        return Err(KernelError::Conflict(format!(
            "fee structure {} v{} is {}; it cannot become {to}",
            s.code, s.version_no, s.status
        )));
    }
    let n = sqlx::query(&format!(
        "UPDATE finance.fee_structure SET status = $3, status_reason = COALESCE($4, status_reason), \
         process_instance_id = COALESCE($5, process_instance_id), version = version + 1, \
         updated_at = now(){extra_sql} WHERE tenant_id = $1 AND id = $2 AND version = $6"
    ))
    .bind(tenant.as_uuid())
    .bind(s.id)
    .bind(to.as_str())
    .bind(reason)
    .bind(pid)
    .bind(s.version)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?
    .rows_affected();
    if n != 1 {
        return Err(KernelError::Conflict(
            "fee structure changed concurrently".into(),
        ));
    }
    record_history(
        conn,
        HistoryEntry {
            tenant,
            institution: InstitutionId(s.institution_id),
            aggregate_type: "fee_structure",
            aggregate_id: s.id,
            from: Some(s.status.as_str()),
            to: to.as_str(),
        },
        &StatusChange {
            cause_event_type: FEE_STRUCTURE_PUBLISHED,
            reason,
            process_instance_id: pid,
        },
        actor,
    )
    .await
}

/// Submits a draft for approval (`FEE_STRUCTURE_APPROVAL`); auto-approval approves it at once.
pub async fn submit_structure(
    state: &FinanceState,
    tenant: TenantId,
    id: FeeStructureId,
    expected: i64,
    actor: &Actor,
) -> Result<FeeStructureView, KernelError> {
    let s = get_structure(state, tenant, id).await?;
    require_version(s.version, expected)?;
    let scope = ExecutionScope::institution(tenant, InstitutionId(s.institution_id));
    crate::shared::guard::require_institution_writable(
        state,
        &scope,
        InstitutionId(s.institution_id),
    )
    .await?;
    if s.status != FeeStructureStatus::Draft {
        return Err(KernelError::Conflict(format!(
            "fee structure is {}; only drafts are submitted",
            s.status
        )));
    }
    let mut vars = BTreeMap::new();
    vars.insert("code".into(), json!(s.code));
    vars.insert("total".into(), json!(s.total_amount));
    let outcome = request_approval(
        state,
        &scope,
        process::FEE_STRUCTURE_APPROVAL,
        business_key("fee-structure", id, s.version),
        vars,
    )
    .await?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let cur = fetch_structure(&mut tx, tenant, id, true).await?;
    require_version(cur.version, expected)?;
    move_structure(
        &mut tx,
        tenant,
        &cur,
        FeeStructureStatus::InReview,
        None,
        Some(outcome.process_instance_id()),
        "",
        actor,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        let cur = fetch_structure(&mut tx, tenant, id, true).await?;
        move_structure(
            &mut tx,
            tenant,
            &cur,
            FeeStructureStatus::Approved,
            None,
            None,
            ", approved_by = 'system:auto-approval'",
            &Actor::system(actor.correlation_id.clone()),
        )
        .await?;
    }
    audit(
        &mut tx,
        &scope,
        "finance.fee_structure.submit",
        "fee_structure",
        id.to_string(),
        json!({"processInstanceId": outcome.process_instance_id()}),
        actor,
    )
    .await?;
    let v = fetch_structure(&mut tx, tenant, id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(v)
}

/// Decision on a structure in review (`finance.approve`): approve → approved; reject → draft.
pub async fn decide_structure(
    state: &FinanceState,
    tenant: TenantId,
    id: FeeStructureId,
    expected: i64,
    approve: bool,
    reason: Option<String>,
    actor: &Actor,
) -> Result<FeeStructureView, KernelError> {
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let cur = fetch_structure(&mut tx, tenant, id, true).await?;
    require_version(cur.version, expected)?;
    if cur.status != FeeStructureStatus::InReview {
        return Err(KernelError::Conflict(format!(
            "fee structure is {}; there is no pending decision",
            cur.status
        )));
    }
    if approve {
        move_structure_with_approver(&mut tx, tenant, &cur, reason.as_deref(), actor).await?;
    } else {
        let r = super::kit::require_text("reason", reason.as_deref())?;
        move_structure(
            &mut tx,
            tenant,
            &cur,
            FeeStructureStatus::Draft,
            Some(&r),
            None,
            "",
            actor,
        )
        .await?;
    }
    let scope = ExecutionScope::institution(tenant, InstitutionId(cur.institution_id));
    audit(
        &mut tx,
        &scope,
        "finance.fee_structure.decide",
        "fee_structure",
        id.to_string(),
        json!({"approve": approve, "reason": reason}),
        actor,
    )
    .await?;
    let v = fetch_structure(&mut tx, tenant, id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(v)
}

async fn move_structure_with_approver(
    conn: &mut PgConnection,
    tenant: TenantId,
    s: &FeeStructureView,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<(), KernelError> {
    move_structure(
        conn,
        tenant,
        s,
        FeeStructureStatus::Approved,
        reason,
        None,
        "",
        actor,
    )
    .await?;
    sqlx::query(
        "UPDATE finance.fee_structure SET approved_by = $3 WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant.as_uuid())
    .bind(s.id)
    .bind(actor.id.as_deref())
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(())
}

/// Publishes an approved version; the previously published version of the same code becomes
/// `superseded` (charges already raised keep their source).
pub async fn publish_structure(
    state: &FinanceState,
    tenant: TenantId,
    id: FeeStructureId,
    expected: i64,
    actor: &Actor,
) -> Result<FeeStructureView, KernelError> {
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let cur = fetch_structure(&mut tx, tenant, id, true).await?;
    require_version(cur.version, expected)?;
    let institution = InstitutionId(cur.institution_id);
    let scope = ExecutionScope::institution(tenant, institution);
    crate::shared::guard::require_institution_writable(state, &scope, institution).await?;
    if cur.status != FeeStructureStatus::Approved {
        return Err(KernelError::Conflict(format!(
            "fee structure is {}; only approved versions are published",
            cur.status
        )));
    }
    let previous: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM finance.fee_structure WHERE tenant_id = $1 AND institution_id = $2 \
         AND code = $3 AND status = 'published' FOR UPDATE",
    )
    .bind(tenant.as_uuid())
    .bind(institution.as_uuid())
    .bind(&cur.code)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if let Some(prev) = previous {
        let p = fetch_structure(&mut tx, tenant, FeeStructureId(prev), true).await?;
        move_structure(
            &mut tx,
            tenant,
            &p,
            FeeStructureStatus::Superseded,
            Some("superseded by a newer version"),
            None,
            "",
            actor,
        )
        .await?;
        sqlx::query("UPDATE finance.fee_structure SET superseded_by_id = $3 WHERE tenant_id = $1 AND id = $2")
            .bind(tenant.as_uuid())
            .bind(prev)
            .bind(id.as_uuid())
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
    }
    move_structure(
        &mut tx,
        tenant,
        &cur,
        FeeStructureStatus::Published,
        None,
        None,
        ", published_at = now()",
        actor,
    )
    .await?;
    let after = fetch_structure(&mut tx, tenant, id, false).await?;
    status_event(
        &mut tx,
        &scope,
        (FEE_STRUCTURE_PUBLISHED, FEE_STRUCTURE_PUBLISHED_SCHEMA),
        StatusEvent {
            aggregate_id: id.as_uuid(),
            from: Some(cur.status.as_str().into()),
            to: after.status.as_str().into(),
            version: after.version,
            amount: Some(super::kit::parse_decimal("total", &after.total_amount)?),
            reason: Some(format!("{} v{}", after.code, after.version_no)),
            process_instance_id: after.process_instance_id.clone(),
            ..StatusEvent::default()
        },
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "finance.fee_structure.publish",
        "fee_structure",
        id.to_string(),
        json!({"supersedes": previous}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get_structure(state, tenant, id).await
}

/// Retires a version (`supersede` endpoint): a published version stops generating charges.
pub async fn retire_structure(
    state: &FinanceState,
    tenant: TenantId,
    id: FeeStructureId,
    expected: i64,
    reason: Option<String>,
    actor: &Actor,
) -> Result<FeeStructureView, KernelError> {
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let cur = fetch_structure(&mut tx, tenant, id, true).await?;
    require_version(cur.version, expected)?;
    let to = if cur.status == FeeStructureStatus::Published {
        FeeStructureStatus::Superseded
    } else {
        FeeStructureStatus::Retired
    };
    move_structure(
        &mut tx,
        tenant,
        &cur,
        to,
        reason.as_deref(),
        None,
        "",
        actor,
    )
    .await?;
    let scope = ExecutionScope::institution(tenant, InstitutionId(cur.institution_id));
    audit(
        &mut tx,
        &scope,
        "finance.fee_structure.supersede",
        "fee_structure",
        id.to_string(),
        json!({"to": to, "reason": reason}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get_structure(state, tenant, id).await
}

/// Outcome of a charge generation run.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GenerationResult {
    pub fee_structure_id: Uuid,
    /// Charges created by this run.
    pub created: Vec<GeneratedCharge>,
    /// Students that already had a live charge from this structure (idempotent re-run).
    pub skipped: Vec<Uuid>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GeneratedCharge {
    pub student_id: Uuid,
    pub charge_id: Uuid,
    pub charge_number: String,
    pub status: sutra_contracts::finance::status::ChargeStatus,
}

/// Generates one charge per student from a published structure (business obligation →
/// charge → validation → publication, L3-F-001). Idempotent per (structure, student): a
/// student who already has a non-cancelled charge from this structure is skipped. With
/// `publish = false` the charges stay `validated` for review.
pub async fn generate_charges(
    state: &FinanceState,
    tenant: TenantId,
    id: FeeStructureId,
    students: Vec<Uuid>,
    publish: bool,
    actor: &Actor,
) -> Result<GenerationResult, KernelError> {
    let s = get_structure(state, tenant, id).await?;
    if s.status != FeeStructureStatus::Published {
        return Err(KernelError::Conflict(format!(
            "fee structure is {}; only a published structure generates charges",
            s.status
        )));
    }
    if students.is_empty() || students.len() > 500 {
        return Err(invalid("studentIds", "invalid", "1–500 students"));
    }
    let institution = InstitutionId(s.institution_id);
    let scope = ExecutionScope::institution(tenant, institution);
    crate::shared::guard::require_institution_writable(state, &scope, institution).await?;
    let today = business_today(state, &scope, institution).await?;
    let lines: Vec<NewChargeLine> = s
        .lines
        .iter()
        .map(|l| {
            Ok(NewChargeLine {
                fee_head_id: FeeHeadId(l.fee_head_id),
                description: l.description.clone(),
                amount: super::kit::parse_decimal("amount", &l.amount)?,
            })
        })
        .collect::<Result<_, KernelError>>()?;
    let mut created = Vec::new();
    let mut skipped = Vec::new();
    for student in students {
        let mut tx = state.pool.begin().await.map_err(map_db_error)?;
        let existing: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM finance.charge WHERE tenant_id = $1 AND source_type = 'fee_structure' \
             AND source_id = $2 AND student_id = $3 AND status <> 'cancelled' LIMIT 1",
        )
        .bind(tenant.as_uuid())
        .bind(id.as_uuid())
        .bind(student)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_db_error)?;
        if existing.is_some() {
            skipped.push(student);
            continue;
        }
        let c = create_in_tx(
            state,
            &mut tx,
            &scope,
            institution,
            &ChargeInput {
                student_id: StudentId(student),
                description: format!("{} ({} v{})", s.name, s.code, s.version_no),
                due_date: s.due_date,
                academic_year_id: s.academic_year_id,
                academic_term_id: s.academic_term_id,
                source_type: ChargeSourceType::FeeStructure,
                source_id: Some(id.as_uuid()),
                lines: lines.clone(),
            },
            actor,
        )
        .await?;
        let c = lock_charge(&mut tx, tenant, c.id).await?;
        let mut c = validate_locked(&mut tx, &c, actor).await?;
        if publish {
            c = publish_locked(&mut tx, &c, today, None, actor).await?.0;
        }
        tx.commit().await.map_err(map_db_error)?;
        created.push(GeneratedCharge {
            student_id: student,
            charge_id: c.id.as_uuid(),
            charge_number: c.charge_number.clone(),
            status: c.status,
        });
    }
    Ok(GenerationResult {
        fee_structure_id: id.as_uuid(),
        created,
        skipped,
    })
}
