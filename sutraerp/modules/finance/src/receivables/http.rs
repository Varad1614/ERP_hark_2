//! Accounts receivable (L2-F07, L3-F-039 … F-043; tag `finance-receivables`).
//!
//! * **Recognition / evaluation** (`POST /institutions/{id}/receivables`): every published
//!   charge (read through the foundation `ChargeDirectory`) gets one receivable; its status is
//!   re-derived from the charge (open → partially settled → overdue → settled → reopened),
//!   emitting `receivable.recognised | overdue | settled | reopened`. A settled receivable
//!   resolves its dunning case.
//! * **Dunning** (F-040): notices at levels 1 … 3 on an overdue receivable (`dunning.notice_sent`),
//!   the third escalates the case; a run notices every overdue receivable once a day.
//! * **Write-off decision** (F-042): requested with `receivable.manage`, decided through the
//!   WorkflowPort (`RECEIVABLE_WRITE_OFF`, auto-approve in Batch 5) or `…/write-off/decision`
//!   (`finance.approve`). Posting Dr bad debts · Cr receivables; the charge's outstanding is
//!   cleared through `shared::charge::apply_settlement` (the only 5B writer of the charge).
//! * **Reopening** (F-043): a write-off is reversed (entry reversed, settlement taken back).

use super::domain::{
    ageing_bucket, can_reopen, can_write_off, days_overdue, derive, dunning_live,
    dunning_status_after, ChargeFacts, MAX_DUNNING_LEVEL,
};
use crate::payments::common::{
    history, illegal, parse_amount, require_text, require_version, subject_ref, Changed,
};
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::charge::{apply_settlement, lock_charge};
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, if_match, with_etag};
use crate::shared::lifecycle::StatusChange;
use crate::shared::money::{display_inr, fmt};
use crate::shared::posting;
use crate::shared::publish::{audit, event};
use crate::shared::scope::{institution_scope, tenant};
use crate::shared::time::business_today;
use crate::shared::{Actor, FinanceState};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use std::collections::{BTreeMap, HashMap};
use sutra_contracts::finance::events::payments::{
    DUNNING_NOTICE_SENT, DUNNING_NOTICE_SENT_SCHEMA, RECEIVABLE_OVERDUE, RECEIVABLE_OVERDUE_SCHEMA,
    RECEIVABLE_RECOGNISED, RECEIVABLE_RECOGNISED_SCHEMA, RECEIVABLE_REOPENED,
    RECEIVABLE_REOPENED_SCHEMA, RECEIVABLE_SETTLED, RECEIVABLE_SETTLED_SCHEMA,
    RECEIVABLE_WRITTEN_OFF, RECEIVABLE_WRITTEN_OFF_SCHEMA,
};
use sutra_contracts::finance::ids::{
    ChargeId, DunningCaseId, JournalEntryId, ReceivableId, StudentAccountId,
};
use sutra_contracts::finance::ports::{
    source, AccountRef, PostingLine, PostingRequest, SystemAccount,
};
use sutra_contracts::finance::status::{DunningStatus, JournalEntryKind, ReceivableStatus};
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::money::CurrencyCode;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

const COLUMNS: &str = "r.id, r.tenant_id, r.institution_id, r.student_account_id, r.charge_id, \
    r.recognised_amount, r.outstanding_amount, r.written_off_amount, r.currency, r.due_date, r.status, \
    r.status_reason, r.recognised_at, r.evaluated_on, r.process_instance_id, r.version, \
    c.charge_number, c.description, a.display_name, a.student_number";
const FROM: &str = "finance.receivable r JOIN finance.charge c ON c.id = r.charge_id \
    JOIN finance.student_account a ON a.id = r.student_account_id";

// ------------------------------------------------------------------ rows and views

#[derive(Clone, Debug)]
struct Recv {
    id: Uuid,
    tenant: TenantId,
    institution: InstitutionId,
    account: StudentAccountId,
    charge_id: Uuid,
    recognised: Decimal,
    outstanding: Decimal,
    written_off: Decimal,
    currency: String,
    due: Option<NaiveDate>,
    status: ReceivableStatus,
    status_reason: Option<String>,
    recognised_at: DateTime<Utc>,
    process_instance_id: Option<String>,
    version: i64,
    charge_number: String,
    description: String,
    account_name: String,
    student_number: Option<String>,
}

fn from_row(r: &PgRow) -> Result<Recv, sqlx::Error> {
    let status: String = r.try_get("status")?;
    Ok(Recv {
        id: r.try_get("id")?,
        tenant: TenantId(r.try_get("tenant_id")?),
        institution: InstitutionId(r.try_get("institution_id")?),
        account: StudentAccountId(r.try_get("student_account_id")?),
        charge_id: r.try_get("charge_id")?,
        recognised: r.try_get("recognised_amount")?,
        outstanding: r.try_get("outstanding_amount")?,
        written_off: r.try_get("written_off_amount")?,
        currency: r.try_get("currency")?,
        due: r.try_get("due_date")?,
        status: ReceivableStatus::parse(&status)
            .ok_or_else(|| sqlx::Error::Decode(format!("receivable status `{status}`").into()))?,
        status_reason: r.try_get("status_reason")?,
        recognised_at: r.try_get("recognised_at")?,
        process_instance_id: r.try_get("process_instance_id")?,
        version: r.try_get("version")?,
        charge_number: r.try_get("charge_number")?,
        description: r.try_get("description")?,
        account_name: r.try_get("display_name")?,
        student_number: r.try_get("student_number")?,
    })
}

async fn load(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<Recv, KernelError> {
    let r = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM {FROM} WHERE r.tenant_id = $1 AND r.id = $2{}",
        if lock { " FOR UPDATE OF r" } else { "" }
    ))
    .bind(tenant.as_uuid())
    .bind(id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "receivable",
    })?;
    from_row(&r).map_err(map_db_error)
}

/// The live (or last) dunning case of a receivable.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DunningCaseView {
    pub id: DunningCaseId,
    pub receivable_id: Uuid,
    pub status: DunningStatus,
    pub level: i32,
    pub status_reason: Option<String>,
    pub notices: Vec<DunningNoticeView>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DunningNoticeView {
    pub level: i32,
    #[schema(value_type = String, example = "25000.00")]
    pub amount_due: String,
    pub notice_date: NaiveDate,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WriteOffView {
    pub id: Uuid,
    #[schema(value_type = String, example = "25000.00")]
    pub amount: String,
    pub reason: String,
    /// `pending_approval | posted | rejected | reversed`.
    pub status: String,
    pub process_instance_id: Option<String>,
    pub decided_by: Option<String>,
    pub decision_reason: Option<String>,
    pub journal_entry_id: Option<Uuid>,
    pub reversal_entry_id: Option<Uuid>,
    pub version: i64,
}

/// A receivable.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReceivableView {
    pub id: ReceivableId,
    pub institution_id: Uuid,
    pub student_account_id: StudentAccountId,
    pub account_name: String,
    pub student_number: Option<String>,
    pub charge_id: Uuid,
    pub charge_number: String,
    pub description: String,
    #[schema(value_type = String, example = "125000.00")]
    pub recognised_amount: String,
    #[schema(value_type = String, example = "25000.00")]
    pub outstanding_amount: String,
    #[schema(value_type = String, example = "0.00")]
    pub written_off_amount: String,
    pub currency: String,
    pub due_date: Option<NaiveDate>,
    pub days_overdue: i64,
    /// `current | 1-30 | 31-60 | 61-90 | 90+`.
    pub ageing_bucket: String,
    pub status: ReceivableStatus,
    pub status_reason: Option<String>,
    pub recognised_at: DateTime<Utc>,
    pub process_instance_id: Option<String>,
    pub dunning: Option<DunningCaseView>,
    pub write_offs: Vec<WriteOffView>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReceivableList {
    pub items: Vec<ReceivableView>,
    #[schema(value_type = String, example = "25000.00")]
    pub total_outstanding: String,
    #[schema(value_type = String, example = "25000.00")]
    pub total_overdue: String,
}

/// Result of a recognition / evaluation run.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EvaluationResult {
    pub recognised: i64,
    pub overdue: i64,
    pub settled: i64,
    pub reopened: i64,
    pub evaluated: i64,
    pub as_of: NaiveDate,
}

/// Result of a dunning run.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DunningRunResult {
    pub notices_sent: i64,
    pub escalated: i64,
    pub evaluation: EvaluationResult,
}

async fn dunning_of(
    conn: &mut PgConnection,
    tenant: TenantId,
    receivable: Uuid,
    case: Option<Uuid>,
) -> Result<Option<DunningCaseView>, KernelError> {
    let row = sqlx::query(
        "SELECT id, receivable_id, status, level, status_reason, version FROM finance.dunning_case \
         WHERE tenant_id = $1 AND ($2::uuid IS NULL OR receivable_id = $2) AND ($3::uuid IS NULL OR id = $3) \
         ORDER BY created_at DESC, id DESC LIMIT 1",
    )
    .bind(tenant.as_uuid())
    .bind(if case.is_some() { None } else { Some(receivable) })
    .bind(case)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let Some(r) = row else { return Ok(None) };
    let map = || -> Result<DunningCaseView, sqlx::Error> {
        let status: String = r.try_get("status")?;
        Ok(DunningCaseView {
            id: DunningCaseId(r.try_get("id")?),
            receivable_id: r.try_get("receivable_id")?,
            status: DunningStatus::parse(&status).unwrap_or(DunningStatus::Open),
            level: r.try_get("level")?,
            status_reason: r.try_get("status_reason")?,
            notices: Vec::new(),
            version: r.try_get("version")?,
        })
    };
    let mut v = map().map_err(map_db_error)?;
    v.notices = sqlx::query(
        "SELECT level, amount_due, notice_date, message FROM finance.dunning_notice WHERE dunning_case_id = $1 ORDER BY level",
    )
    .bind(v.id.as_uuid())
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?
    .iter()
    .map(|n| -> Result<DunningNoticeView, sqlx::Error> {
        let amount: Decimal = n.try_get("amount_due")?;
        Ok(DunningNoticeView {
            level: n.try_get("level")?,
            amount_due: fmt(amount),
            notice_date: n.try_get("notice_date")?,
            message: n.try_get("message")?,
        })
    })
    .collect::<Result<Vec<_>, _>>()
    .map_err(map_db_error)?;
    Ok(Some(v))
}

async fn view(
    conn: &mut PgConnection,
    r: Recv,
    today: NaiveDate,
    detail: bool,
) -> Result<ReceivableView, KernelError> {
    let dunning = dunning_of(conn, r.tenant, r.id, None).await?;
    let write_offs = if detail {
        sqlx::query(
            "SELECT id, amount, reason, status, process_instance_id, decided_by, decision_reason, journal_entry_id, \
             reversal_entry_id, version FROM finance.receivable_write_off WHERE tenant_id = $1 AND receivable_id = $2 ORDER BY created_at",
        )
        .bind(r.tenant.as_uuid())
        .bind(r.id)
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db_error)?
        .iter()
        .map(|w| -> Result<WriteOffView, sqlx::Error> {
            let amount: Decimal = w.try_get("amount")?;
            Ok(WriteOffView {
                id: w.try_get("id")?,
                amount: fmt(amount),
                reason: w.try_get("reason")?,
                status: w.try_get("status")?,
                process_instance_id: w.try_get("process_instance_id")?,
                decided_by: w.try_get("decided_by")?,
                decision_reason: w.try_get("decision_reason")?,
                journal_entry_id: w.try_get("journal_entry_id")?,
                reversal_entry_id: w.try_get("reversal_entry_id")?,
                version: w.try_get("version")?,
            })
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)?
    } else {
        Vec::new()
    };
    let days = if matches!(
        r.status,
        ReceivableStatus::Settled | ReceivableStatus::WrittenOff
    ) {
        0
    } else {
        days_overdue(r.due, today)
    };
    Ok(ReceivableView {
        id: ReceivableId(r.id),
        institution_id: r.institution.as_uuid(),
        student_account_id: r.account,
        account_name: r.account_name,
        student_number: r.student_number,
        charge_id: r.charge_id,
        charge_number: r.charge_number,
        description: r.description,
        recognised_amount: fmt(r.recognised),
        outstanding_amount: fmt(r.outstanding),
        written_off_amount: fmt(r.written_off),
        currency: r.currency,
        due_date: r.due,
        days_overdue: days,
        ageing_bucket: ageing_bucket(days).into(),
        status: r.status,
        status_reason: r.status_reason,
        recognised_at: r.recognised_at,
        process_instance_id: r.process_instance_id,
        dunning,
        write_offs,
        version: r.version,
    })
}

async fn respond(
    st: &FinanceState,
    ctx: &AuthContext,
    id: Uuid,
    status: StatusCode,
) -> ApiResult<Response> {
    let t = tenant(ctx);
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let r = load(&mut conn, t, id, false).await?;
    let today = business_today(st, &institution_scope(ctx, r.institution), r.institution).await?;
    let version = r.version;
    let v = view(&mut conn, r, today, true).await?;
    Ok(with_etag(status, &v, Version(version)))
}

// ------------------------------------------------------------------ transitions + events

#[allow(clippy::too_many_arguments)]
async fn set_status(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    r: &Recv,
    to: ReceivableStatus,
    outstanding: Decimal,
    reason: Option<&str>,
    process_instance: Option<&str>,
    journal: Option<Uuid>,
    actor: &Actor,
) -> Result<(), KernelError> {
    sqlx::query(
        "UPDATE finance.receivable SET status = $3, outstanding_amount = $4, status_reason = COALESCE($5, status_reason), \
         process_instance_id = COALESCE($6, process_instance_id), version = version + 1, updated_at = now() \
         WHERE tenant_id = $1 AND id = $2",
    )
    .bind(r.tenant.as_uuid())
    .bind(r.id)
    .bind(to.as_str())
    .bind(outstanding)
    .bind(reason)
    .bind(process_instance)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    if to == r.status {
        return Ok(());
    }
    let (ev, schema) = match to {
        ReceivableStatus::Overdue => (Some(RECEIVABLE_OVERDUE), RECEIVABLE_OVERDUE_SCHEMA),
        ReceivableStatus::Settled => (Some(RECEIVABLE_SETTLED), RECEIVABLE_SETTLED_SCHEMA),
        ReceivableStatus::WrittenOff => {
            (Some(RECEIVABLE_WRITTEN_OFF), RECEIVABLE_WRITTEN_OFF_SCHEMA)
        }
        ReceivableStatus::Reopened => (Some(RECEIVABLE_REOPENED), RECEIVABLE_REOPENED_SCHEMA),
        _ => (None, ""),
    };
    history(
        conn,
        r.tenant,
        r.institution,
        "receivable",
        r.id,
        Some(r.status.as_str()),
        to.as_str(),
        &StatusChange {
            cause_event_type: ev.unwrap_or(""),
            reason,
            process_instance_id: process_instance,
        },
        actor,
    )
    .await?;
    if let Some(ev) = ev {
        let subject = subject_ref(conn, r.tenant, r.account).await?;
        let payload = Changed::new(r.id, r.institution, to.as_str(), r.version + 1)
            .from(r.status.as_str())
            .account(r.account, subject)
            .charge(r.charge_id)
            .amount(outstanding, &r.currency)
            .reason(reason.or(Some(&r.charge_number)))
            .process(process_instance)
            .journal(journal);
        event(conn, scope, ev, schema, r.id.to_string(), &payload.v, actor).await?;
    }
    if to == ReceivableStatus::Settled || to == ReceivableStatus::WrittenOff {
        // The debt is gone: resolve a live dunning case.
        sqlx::query(
            "UPDATE finance.dunning_case SET status = 'resolved', status_reason = $3, version = version + 1, updated_at = now() \
             WHERE tenant_id = $1 AND receivable_id = $2 AND status IN ('open', 'notice_sent', 'escalated')",
        )
        .bind(r.tenant.as_uuid())
        .bind(r.id)
        .bind(if to == ReceivableStatus::Settled { "receivable settled" } else { "receivable written off" })
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    }
    Ok(())
}

/// Recognises receivables for every published charge of the institution and re-derives each
/// live receivable's status (pure `derive`).
async fn evaluate(
    st: &FinanceState,
    scope: &ExecutionScope,
    t: TenantId,
    inst: InstitutionId,
    actor: &Actor,
) -> Result<EvaluationResult, KernelError> {
    let today = business_today(st, scope, inst).await?;
    let accounts: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM finance.student_account WHERE tenant_id = $1 AND institution_id = $2 ORDER BY id",
    )
    .bind(t.as_uuid())
    .bind(inst.as_uuid())
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let mut out = EvaluationResult {
        recognised: 0,
        overdue: 0,
        settled: 0,
        reopened: 0,
        evaluated: 0,
        as_of: today,
    };
    for acc in accounts {
        let charges = st
            .charges
            .charges_of_account(scope, StudentAccountId(acc))
            .await?;
        let existing: HashMap<Uuid, Uuid> = sqlx::query_as::<_, (Uuid, Uuid)>(
            "SELECT charge_id, id FROM finance.receivable WHERE tenant_id = $1 AND student_account_id = $2",
        )
        .bind(t.as_uuid())
        .bind(acc)
        .fetch_all(&st.pool)
        .await
        .map_err(map_db_error)?
        .into_iter()
        .collect();
        for c in charges {
            let published = c.status.is_receivable()
                || matches!(
                    c.status,
                    sutra_contracts::finance::status::ChargeStatus::Closed
                        | sutra_contracts::finance::status::ChargeStatus::Suspended
                        | sutra_contracts::finance::status::ChargeStatus::WrittenOff
                );
            let facts = ChargeFacts {
                status: c.status,
                owed: c.total_amount + c.adjustment_amount,
                outstanding: c.outstanding_amount,
                due: c.due_date,
            };
            let mut tx = st.pool.begin().await.map_err(map_db_error)?;
            let rid = match existing.get(&c.id.as_uuid()) {
                Some(id) => *id,
                None => {
                    if !published {
                        continue;
                    }
                    let initial = derive(ReceivableStatus::Open, facts, today);
                    let initial = if initial == ReceivableStatus::Reopened {
                        ReceivableStatus::Open
                    } else {
                        initial
                    };
                    let id: Option<Uuid> = sqlx::query_scalar(
                        "INSERT INTO finance.receivable (tenant_id, institution_id, student_account_id, charge_id, \
                         recognised_amount, outstanding_amount, due_date, status, evaluated_on) \
                         VALUES ($1, $2, $3, $4, $5, $6, $7, 'open', $8) ON CONFLICT (tenant_id, charge_id) DO NOTHING RETURNING id",
                    )
                    .bind(t.as_uuid())
                    .bind(inst.as_uuid())
                    .bind(acc)
                    .bind(c.id.as_uuid())
                    .bind(facts.owed)
                    .bind(facts.outstanding)
                    .bind(c.due_date)
                    .bind(today)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(map_db_error)?;
                    let Some(id) = id else { continue };
                    history(
                        &mut tx,
                        t,
                        inst,
                        "receivable",
                        id,
                        None,
                        "open",
                        &StatusChange {
                            cause_event_type: RECEIVABLE_RECOGNISED,
                            reason: None,
                            process_instance_id: None,
                        },
                        actor,
                    )
                    .await?;
                    let subject = subject_ref(&mut tx, t, StudentAccountId(acc)).await?;
                    let payload = Changed::new(id, inst, "open", 1)
                        .account(StudentAccountId(acc), subject)
                        .charge(c.id.as_uuid())
                        .amount(facts.owed, &c.currency)
                        .reason(Some(&c.charge_number));
                    event(
                        &mut tx,
                        scope,
                        RECEIVABLE_RECOGNISED,
                        RECEIVABLE_RECOGNISED_SCHEMA,
                        id.to_string(),
                        &payload.v,
                        actor,
                    )
                    .await?;
                    out.recognised += 1;
                    let _ = initial;
                    id
                }
            };
            let r = load(&mut tx, t, rid, true).await?;
            let to = derive(r.status, facts, today);
            if to != r.status || r.outstanding != facts.outstanding {
                set_status(
                    &mut tx,
                    scope,
                    &r,
                    to,
                    facts.outstanding,
                    None,
                    None,
                    None,
                    actor,
                )
                .await?;
                if to != r.status {
                    match to {
                        ReceivableStatus::Overdue => out.overdue += 1,
                        ReceivableStatus::Settled => out.settled += 1,
                        ReceivableStatus::Reopened => out.reopened += 1,
                        _ => {}
                    }
                }
            }
            sqlx::query("UPDATE finance.receivable SET evaluated_on = $2 WHERE id = $1")
                .bind(rid)
                .bind(today)
                .execute(&mut *tx)
                .await
                .map_err(map_db_error)?;
            tx.commit().await.map_err(map_db_error)?;
            out.evaluated += 1;
        }
    }
    Ok(out)
}

/// Sends the next dunning notice of an overdue receivable (in the caller's tx).
async fn send_notice(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    r: &Recv,
    message: Option<&str>,
    today: NaiveDate,
    actor: &Actor,
) -> Result<DunningStatus, KernelError> {
    if r.status != ReceivableStatus::Overdue {
        return Err(KernelError::Conflict(format!(
            "only an overdue receivable is dunned (receivable is {})",
            r.status
        )));
    }
    let case: Option<(Uuid, i32, String)> = sqlx::query_as(
        "SELECT id, level, status FROM finance.dunning_case WHERE tenant_id = $1 AND receivable_id = $2 \
         AND status IN ('open', 'notice_sent', 'escalated') FOR UPDATE",
    )
    .bind(r.tenant.as_uuid())
    .bind(r.id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let (case_id, level, from) = match case {
        Some(c) => c,
        None => {
            let id: Uuid = sqlx::query_scalar(
                "INSERT INTO finance.dunning_case (tenant_id, institution_id, receivable_id, student_account_id) \
                 VALUES ($1, $2, $3, $4) RETURNING id",
            )
            .bind(r.tenant.as_uuid())
            .bind(r.institution.as_uuid())
            .bind(r.id)
            .bind(r.account.as_uuid())
            .fetch_one(&mut *conn)
            .await
            .map_err(map_db_error)?;
            history(
                conn,
                r.tenant,
                r.institution,
                "dunning_case",
                id,
                None,
                "open",
                &StatusChange::default(),
                actor,
            )
            .await?;
            (id, 0, "open".to_owned())
        }
    };
    if level >= MAX_DUNNING_LEVEL {
        return Err(KernelError::Conflict(
            "the dunning case is escalated; resolve it or decide a write-off".into(),
        ));
    }
    let next = level + 1;
    let text = message.map(str::to_owned).unwrap_or_else(|| {
        format!(
            "Reminder {next}: {} is overdue on {} ({}). Please pay at the earliest.",
            display_inr(r.outstanding),
            r.charge_number,
            r.description
        )
    });
    sqlx::query(
        "INSERT INTO finance.dunning_notice (tenant_id, dunning_case_id, level, amount_due, notice_date, message, sent_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(r.tenant.as_uuid())
    .bind(case_id)
    .bind(next)
    .bind(r.outstanding)
    .bind(today)
    .bind(&text)
    .bind(actor.id.as_deref())
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let to = dunning_status_after(next);
    sqlx::query("UPDATE finance.dunning_case SET level = $2, status = $3, version = version + 1, updated_at = now() WHERE id = $1")
        .bind(case_id)
        .bind(next)
        .bind(to.as_str())
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    if to.as_str() != from {
        history(
            conn,
            r.tenant,
            r.institution,
            "dunning_case",
            case_id,
            Some(&from),
            to.as_str(),
            &StatusChange {
                cause_event_type: DUNNING_NOTICE_SENT,
                reason: None,
                process_instance_id: None,
            },
            actor,
        )
        .await?;
    }
    let subject = subject_ref(conn, r.tenant, r.account).await?;
    let payload = Changed::new(case_id, r.institution, to.as_str(), i64::from(next))
        .from(&from)
        .account(r.account, subject)
        .charge(r.charge_id)
        .amount(r.outstanding, &r.currency)
        .reason(Some(&text));
    event(
        conn,
        scope,
        DUNNING_NOTICE_SENT,
        DUNNING_NOTICE_SENT_SCHEMA,
        case_id.to_string(),
        &payload.v,
        actor,
    )
    .await?;
    audit(
        conn,
        scope,
        "finance.dunning.notice",
        "dunning_case",
        case_id.to_string(),
        json!({"receivableId": r.id, "level": next, "amountDue": fmt(r.outstanding)}),
        actor,
    )
    .await?;
    Ok(to)
}

/// Posts a pending write-off (Dr bad debts · Cr receivables) and clears the charge's outstanding.
#[allow(clippy::too_many_arguments)]
async fn post_write_off(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    r: &Recv,
    wo: Uuid,
    amount: Decimal,
    reason: Option<&str>,
    date: NaiveDate,
    actor: &Actor,
) -> Result<(), KernelError> {
    let c = lock_charge(conn, r.tenant, ChargeId(r.charge_id)).await?;
    let c = apply_settlement(conn, &c, amount, RECEIVABLE_WRITTEN_OFF, actor).await?;
    let entry = posting::post(
        conn,
        r.tenant,
        &PostingRequest {
            institution_id: r.institution,
            posting_date: date,
            kind: JournalEntryKind::Standard,
            source_type: source::RECEIVABLE_WRITE_OFF.into(),
            source_id: wo,
            posting_key: "post".into(),
            description: format!("Receivable {} written off", r.charge_number),
            currency: CurrencyCode::INR,
            lines: vec![
                PostingLine::debit(AccountRef::System(SystemAccount::BadDebtExpense), amount)
                    .for_charge(r.account, None),
                PostingLine::credit(
                    AccountRef::System(SystemAccount::StudentReceivables),
                    amount,
                )
                .for_charge(r.account, Some(ChargeId(r.charge_id))),
            ],
        },
        actor,
    )
    .await?;
    sqlx::query(
        "UPDATE finance.receivable_write_off SET status = 'posted', journal_entry_id = $2, decided_by = $3, decided_at = now(), \
         decision_reason = COALESCE($4, decision_reason), version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(wo)
    .bind(entry.journal_entry_id.as_uuid())
    .bind(actor.id.as_deref())
    .bind(reason)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    sqlx::query(
        "UPDATE finance.receivable SET written_off_amount = written_off_amount + $2 WHERE id = $1",
    )
    .bind(r.id)
    .bind(amount)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let to = if c.outstanding_amount == Decimal::ZERO {
        ReceivableStatus::WrittenOff
    } else {
        r.status
    };
    let r2 = Recv {
        written_off: r.written_off + amount,
        ..r.clone()
    };
    set_status(
        conn,
        scope,
        &r2,
        to,
        c.outstanding_amount,
        reason,
        None,
        Some(entry.journal_entry_id.as_uuid()),
        actor,
    )
    .await?;
    if to == r.status {
        // partial write-off: still announce the decision
        let subject = subject_ref(conn, r.tenant, r.account).await?;
        let payload = Changed::new(r.id, r.institution, to.as_str(), r.version + 1)
            .account(r.account, subject)
            .charge(r.charge_id)
            .amount(amount, &r.currency)
            .reason(reason)
            .journal(Some(entry.journal_entry_id.as_uuid()));
        event(
            conn,
            scope,
            RECEIVABLE_WRITTEN_OFF,
            RECEIVABLE_WRITTEN_OFF_SCHEMA,
            r.id.to_string(),
            &payload.v,
            actor,
        )
        .await?;
    }
    audit(
        conn,
        scope,
        "finance.receivable.write_off",
        "receivable",
        r.id.to_string(),
        json!({"writeOffId": wo, "amount": fmt(amount), "journalEntryId": entry.journal_entry_id}),
        actor,
    )
    .await
}

// ------------------------------------------------------------------ handlers

#[derive(Debug, Clone, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
pub struct ReceivableListQuery {
    pub status: Option<ReceivableStatus>,
    pub student_account_id: Option<Uuid>,
}

/// Receivables of an institution with ageing (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/institutions/{id}/receivables", operation_id = "finance_receivable_list", tag = "finance-receivables",
    params(("id" = String, Path, format = Uuid), ReceivableListQuery), security(("session_cookie" = [])),
    responses((status = 200, body = ReceivableList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn receivable_list(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<ReceivableListQuery>,
) -> ApiResult<Json<ReceivableList>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution(&st, &scope, inst).await?;
    let today = business_today(&st, &scope, inst).await?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let rows = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM {FROM} WHERE r.tenant_id = $1 AND r.institution_id = $2 \
         AND ($3::text IS NULL OR r.status = $3) AND ($4::uuid IS NULL OR r.student_account_id = $4) \
         ORDER BY r.due_date NULLS LAST, c.charge_number LIMIT 500"
    ))
    .bind(tenant(ctx).as_uuid())
    .bind(id)
    .bind(q.status.map(|s| s.as_str()))
    .bind(q.student_account_id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let (mut total, mut overdue) = (Decimal::ZERO, Decimal::ZERO);
    let mut items = Vec::with_capacity(rows.len());
    for r in &rows {
        let rec = from_row(r).map_err(map_db_error)?;
        if !matches!(
            rec.status,
            ReceivableStatus::Settled | ReceivableStatus::WrittenOff
        ) {
            total += rec.outstanding;
            if rec.status == ReceivableStatus::Overdue {
                overdue += rec.outstanding;
            }
        }
        items.push(view(&mut conn, rec, today, false).await?);
    }
    Ok(Json(ReceivableList {
        items,
        total_outstanding: fmt(total),
        total_overdue: fmt(overdue),
    }))
}

/// Recognises receivables for published charges and re-evaluates every receivable (overdue,
/// settled, reopened) as of the institution's business date (`receivable.manage`).
#[utoipa::path(post, path = "/api/v1/finance/institutions/{id}/receivables", operation_id = "finance_receivable_evaluate", tag = "finance-receivables",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = EvaluationResult), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn receivable_evaluate(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<EvaluationResult>> {
    let ctx = auth.require(Permission::ReceivableManage)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    Ok(Json(
        evaluate(&st, &scope, tenant(ctx), inst, &actor(ctx)).await?,
    ))
}

/// One receivable with its dunning case and write-off decisions (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/receivables/{id}", operation_id = "finance_receivable_get", tag = "finance-receivables",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ReceivableView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn receivable_get(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FinanceRead)?;
    respond(&st, ctx, id, StatusCode::OK).await
}

/// Optional notice text.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DunningRequest {
    pub message: Option<String>,
}

/// Sends the next dunning notice of an overdue receivable (L3-F-040; `receivable.manage`, `If-Match`).
#[utoipa::path(post, path = "/api/v1/finance/receivables/{id}/dunning", operation_id = "finance_receivable_dunning", tag = "finance-receivables",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)), request_body = DunningRequest, security(("session_cookie" = [])),
    responses((status = 200, body = ReceivableView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn receivable_dunning(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<DunningRequest>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ReceivableManage)?;
    let v = match if_match(&headers)? {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let t = tenant(ctx);
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let r0 = load(&mut conn, t, id, false).await?;
    drop(conn);
    let scope = institution_scope(ctx, r0.institution);
    let today = business_today(&st, &scope, r0.institution).await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let r = load(&mut tx, t, id, true).await?;
    require_version(v, r.version)?;
    let msg = body
        .message
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    send_notice(&mut tx, &scope, &r, msg, today, &a).await?;
    sqlx::query(
        "UPDATE finance.receivable SET version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, ctx, id, StatusCode::OK).await
}

/// Evaluates the institution's receivables, then sends the next notice to every overdue one
/// not noticed today (`receivable.manage`).
#[utoipa::path(post, path = "/api/v1/finance/institutions/{id}/dunning-runs", operation_id = "finance_dunning_run", tag = "finance-receivables",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = DunningRunResult), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn dunning_run(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<DunningRunResult>> {
    let ctx = auth.require(Permission::ReceivableManage)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let t = tenant(ctx);
    let a = actor(ctx);
    let evaluation = evaluate(&st, &scope, t, inst, &a).await?;
    let today = evaluation.as_of;
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT r.id FROM finance.receivable r WHERE r.tenant_id = $1 AND r.institution_id = $2 AND r.status = 'overdue' \
         AND NOT EXISTS (SELECT 1 FROM finance.dunning_case d JOIN finance.dunning_notice n ON n.dunning_case_id = d.id \
              WHERE d.receivable_id = r.id AND n.notice_date = $3) \
         AND NOT EXISTS (SELECT 1 FROM finance.dunning_case d WHERE d.receivable_id = r.id AND d.status = 'escalated') ORDER BY r.id",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(today)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    // ERP_EXTENSIONS 1D: with an effective `reminder_schedule`, notice n goes out from
    // `afterDays[n-1]` days late (without one, every run notices as before).
    let schedule = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        super::latefee_http::reminder_schedule(&mut conn, t, inst).await?
    };
    let (mut sent, mut escalated) = (0, 0);
    for rid in ids {
        let mut tx = st.pool.begin().await.map_err(map_db_error)?;
        let r = load(&mut tx, t, rid, true).await?;
        if let Some((_, sched)) = &schedule {
            let level: i32 = sqlx::query_scalar(
                "SELECT coalesce(max(level), 0) FROM finance.dunning_case WHERE tenant_id = $1 AND receivable_id = $2 \
                 AND status IN ('open', 'notice_sent', 'escalated')",
            )
            .bind(t.as_uuid())
            .bind(rid)
            .fetch_one(&mut *tx)
            .await
            .map_err(map_db_error)?;
            if !sched.notice_due(days_overdue(r.due, today), level) {
                continue;
            }
        }
        let to = send_notice(&mut tx, &scope, &r, None, today, &a).await?;
        sqlx::query(
            "UPDATE finance.receivable SET version = version + 1, updated_at = now() WHERE id = $1",
        )
        .bind(rid)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
        tx.commit().await.map_err(map_db_error)?;
        sent += 1;
        if to == DunningStatus::Escalated {
            escalated += 1;
        }
    }
    Ok(Json(DunningRunResult {
        notices_sent: sent,
        escalated,
        evaluation,
    }))
}

/// One dunning case with its notices (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/dunning-cases/{id}", operation_id = "finance_dunning_get", tag = "finance-receivables",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = DunningCaseView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn dunning_get(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let v = dunning_of(&mut conn, tenant(ctx), Uuid::nil(), Some(id))
        .await?
        .ok_or(KernelError::NotFound {
            entity: "dunning case",
        })?;
    let version = v.version;
    Ok(with_etag(StatusCode::OK, &v, Version(version)))
}

/// Reason of a dunning case outcome.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CaseReason {
    pub reason: Option<String>,
}

async fn close_case(
    st: &FinanceState,
    ctx: &AuthContext,
    headers: &HeaderMap,
    id: Uuid,
    to: DunningStatus,
    reason: Option<&str>,
) -> ApiResult<Response> {
    let v = match if_match(headers)? {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let t = tenant(ctx);
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let row: (Uuid, String, i64, Uuid) = sqlx::query_as(
        "SELECT institution_id, status, version, receivable_id FROM finance.dunning_case WHERE tenant_id = $1 AND id = $2 FOR UPDATE",
    )
    .bind(t.as_uuid())
    .bind(id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound { entity: "dunning case" })?;
    require_version(v, row.2)?;
    let from = DunningStatus::parse(&row.1).unwrap_or(DunningStatus::Open);
    if !dunning_live(from) {
        return Err(illegal("dunning case", from.as_str(), to.as_str()).into());
    }
    sqlx::query("UPDATE finance.dunning_case SET status = $2, status_reason = $3, version = version + 1, updated_at = now() WHERE id = $1")
        .bind(id)
        .bind(to.as_str())
        .bind(reason)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let inst = InstitutionId(row.0);
    history(
        &mut tx,
        t,
        inst,
        "dunning_case",
        id,
        Some(from.as_str()),
        to.as_str(),
        &StatusChange {
            cause_event_type: "",
            reason,
            process_instance_id: None,
        },
        &a,
    )
    .await?;
    audit(
        &mut tx,
        &institution_scope(ctx, inst),
        if to == DunningStatus::Resolved {
            "finance.dunning.resolve"
        } else {
            "finance.dunning.cancel"
        },
        "dunning_case",
        id.to_string(),
        json!({"reason": reason, "receivableId": row.3}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let v = dunning_of(&mut conn, t, Uuid::nil(), Some(id))
        .await?
        .ok_or(KernelError::NotFound {
            entity: "dunning case",
        })?;
    let version = v.version;
    Ok(with_etag(StatusCode::OK, &v, Version(version)))
}

/// Resolves a dunning case (e.g. payment plan agreed; `receivable.manage`, `If-Match`).
#[utoipa::path(post, path = "/api/v1/finance/dunning-cases/{id}/resolve", operation_id = "finance_dunning_resolve", tag = "finance-receivables",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)), request_body = CaseReason, security(("session_cookie" = [])),
    responses((status = 200, body = DunningCaseView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn dunning_resolve(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<CaseReason>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ReceivableManage)?;
    let reason = require_text("reason", body.reason.as_deref())?;
    close_case(
        &st,
        ctx,
        &headers,
        id,
        DunningStatus::Resolved,
        Some(&reason),
    )
    .await
}

/// Cancels a dunning case (raised in error; `receivable.manage`, `If-Match`).
#[utoipa::path(post, path = "/api/v1/finance/dunning-cases/{id}/cancel", operation_id = "finance_dunning_cancel", tag = "finance-receivables",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)), request_body = CaseReason, security(("session_cookie" = [])),
    responses((status = 200, body = DunningCaseView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn dunning_cancel(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<CaseReason>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ReceivableManage)?;
    let reason = require_text("reason", body.reason.as_deref())?;
    close_case(
        &st,
        ctx,
        &headers,
        id,
        DunningStatus::Cancelled,
        Some(&reason),
    )
    .await
}

/// A write-off request (amount defaults to the whole outstanding).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WriteOffRequest {
    #[schema(value_type = Option<String>, example = "25000.00")]
    pub amount: Option<String>,
    pub reason: String,
}

/// Requests the write-off of an uncollectible receivable (L3-F-042; `receivable.manage`,
/// `If-Match`, `Idempotency-Key`): approval through the WorkflowPort, auto-approved in Batch 5.
#[utoipa::path(post, path = "/api/v1/finance/receivables/{id}/write-off", operation_id = "finance_receivable_write_off", tag = "finance-receivables",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header), ("Idempotency-Key" = String, Header)), request_body = WriteOffRequest, security(("session_cookie" = [])),
    responses((status = 200, body = ReceivableView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn receivable_write_off(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<WriteOffRequest>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ReceivableManage)?;
    require_idempotency_key(&headers)?;
    let v = match if_match(&headers)? {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let reason = require_text("reason", Some(&body.reason))?;
    let t = tenant(ctx);
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let r0 = load(&mut conn, t, id, false).await?;
    drop(conn);
    require_version(v, r0.version)?;
    if !can_write_off(r0.status) || r0.outstanding <= Decimal::ZERO {
        return Err(illegal("receivable", r0.status.as_str(), "written_off").into());
    }
    let amount = match body.amount.as_deref() {
        Some(a) => parse_amount("amount", a)?,
        None => r0.outstanding,
    };
    if amount > r0.outstanding {
        return Err(KernelError::Validation(vec![FieldViolation::new(
            "amount",
            "exceeds_outstanding",
            "cannot exceed the outstanding amount",
        )])
        .into());
    }
    let scope = institution_scope(ctx, r0.institution);
    require_institution_writable(&st, &scope, r0.institution).await?;
    let wo = Uuid::now_v7();
    let mut vars = BTreeMap::new();
    vars.insert("amount".to_owned(), json!(fmt(amount)));
    vars.insert("chargeNumber".to_owned(), json!(r0.charge_number));
    let outcome = request_approval(
        &st,
        &scope,
        process::RECEIVABLE_WRITE_OFF,
        business_key("receivable_write_off", wo, 1),
        vars,
    )
    .await?;
    let today = business_today(&st, &scope, r0.institution).await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let r = load(&mut tx, t, id, true).await?;
    require_version(v, r.version)?;
    sqlx::query(
        "INSERT INTO finance.receivable_write_off (id, tenant_id, institution_id, receivable_id, charge_id, amount, reason, \
         status, process_instance_id, requested_by) VALUES ($1, $2, $3, $4, $5, $6, $7, 'pending_approval', $8, $9)",
    )
    .bind(wo)
    .bind(t.as_uuid())
    .bind(r.institution.as_uuid())
    .bind(r.id)
    .bind(r.charge_id)
    .bind(amount)
    .bind(&reason)
    .bind(outcome.process_instance_id())
    .bind(a.id.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    history(
        &mut tx,
        t,
        r.institution,
        "receivable_write_off",
        wo,
        None,
        "pending_approval",
        &StatusChange {
            cause_event_type: "",
            reason: Some(&reason),
            process_instance_id: Some(outcome.process_instance_id()),
        },
        &a,
    )
    .await?;
    sqlx::query("UPDATE finance.receivable SET process_instance_id = $2, version = version + 1, updated_at = now() WHERE id = $1")
        .bind(id)
        .bind(outcome.process_instance_id())
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        let r = load(&mut tx, t, id, true).await?;
        let system = Actor::system(a.correlation_id.clone());
        post_write_off(
            &mut tx,
            &scope,
            &r,
            wo,
            amount,
            Some(&reason),
            today,
            &system,
        )
        .await?;
    } else {
        audit(
            &mut tx,
            &scope,
            "finance.receivable.write_off_request",
            "receivable",
            id.to_string(),
            json!({"writeOffId": wo, "amount": fmt(amount)}),
            &a,
        )
        .await?;
    }
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, ctx, id, StatusCode::OK).await
}

/// Decision on a pending write-off.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WriteOffDecision {
    /// `approve` or `reject`.
    pub decision: String,
    pub reason: Option<String>,
}

/// Decides the pending write-off of a receivable (`finance.approve` + step-up, `If-Match` of the
/// receivable, `Idempotency-Key`).
#[utoipa::path(post, path = "/api/v1/finance/receivables/{id}/write-off/decision", operation_id = "finance_receivable_write_off_decide", tag = "finance-receivables",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header), ("Idempotency-Key" = String, Header)), request_body = WriteOffDecision, security(("session_cookie" = [])),
    responses((status = 200, body = ReceivableView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn receivable_write_off_decide(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<WriteOffDecision>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FinanceApprove)?;
    require_idempotency_key(&headers)?;
    let v = match if_match(&headers)? {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let approve = match body.decision.as_str() {
        "approve" => true,
        "reject" => false,
        _ => {
            return Err(KernelError::Validation(vec![FieldViolation::new(
                "decision",
                "invalid",
                "approve or reject",
            )])
            .into())
        }
    };
    let reason = body
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let t = tenant(ctx);
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let r0 = load(&mut conn, t, id, false).await?;
    drop(conn);
    let scope = institution_scope(ctx, r0.institution);
    let today = business_today(&st, &scope, r0.institution).await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let r = load(&mut tx, t, id, true).await?;
    require_version(v, r.version)?;
    let pending: (Uuid, Decimal, Option<String>) = sqlx::query_as(
        "SELECT id, amount, requested_by FROM finance.receivable_write_off WHERE tenant_id = $1 AND receivable_id = $2 AND status = 'pending_approval' FOR UPDATE",
    )
    .bind(t.as_uuid())
    .bind(id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?
    .ok_or_else(|| KernelError::Conflict("no write-off is awaiting a decision".into()))?;
    // Part 1 (DEF-X-002): the requester of a write-off never decides it.
    crate::shared::approval::ensure_independent_decider(
        &mut tx,
        t,
        "receivable_write_off",
        pending.0,
        &["pending_approval"],
        &[pending.2.as_deref()],
        a.id.as_deref(),
        "write-off",
    )
    .await?;
    if approve {
        if pending.1 > r.outstanding {
            return Err(KernelError::Conflict("the outstanding amount dropped below the requested write-off; reject and request again".into()).into());
        }
        post_write_off(&mut tx, &scope, &r, pending.0, pending.1, reason, today, &a).await?;
    } else {
        sqlx::query(
            "UPDATE finance.receivable_write_off SET status = 'rejected', decided_by = $2, decided_at = now(), decision_reason = $3, \
             version = version + 1, updated_at = now() WHERE id = $1",
        )
        .bind(pending.0)
        .bind(a.id.as_deref())
        .bind(reason)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
        history(
            &mut tx,
            t,
            r.institution,
            "receivable_write_off",
            pending.0,
            Some("pending_approval"),
            "rejected",
            &StatusChange {
                cause_event_type: "",
                reason,
                process_instance_id: None,
            },
            &a,
        )
        .await?;
        sqlx::query(
            "UPDATE finance.receivable SET version = version + 1, updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
        audit(
            &mut tx,
            &scope,
            "finance.receivable.write_off_reject",
            "receivable",
            id.to_string(),
            json!({"writeOffId": pending.0, "reason": reason}),
            &a,
        )
        .await?;
    }
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, ctx, id, StatusCode::OK).await
}

/// Reopens a written-off (write-off reversed) or settled receivable on new evidence
/// (L3-F-043; `receivable.manage`, `If-Match`). The decision is recorded through the
/// WorkflowPort (`RECEIVABLE_REOPEN`).
#[utoipa::path(post, path = "/api/v1/finance/receivables/{id}/reopen", operation_id = "finance_receivable_reopen", tag = "finance-receivables",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)), request_body = CaseReason, security(("session_cookie" = [])),
    responses((status = 200, body = ReceivableView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn receivable_reopen(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<CaseReason>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ReceivableManage)?;
    let v = match if_match(&headers)? {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let reason = require_text("reason", body.reason.as_deref())?;
    let t = tenant(ctx);
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let r0 = load(&mut conn, t, id, false).await?;
    drop(conn);
    require_version(v, r0.version)?;
    if !can_reopen(r0.status) {
        return Err(illegal("receivable", r0.status.as_str(), "reopened").into());
    }
    let scope = institution_scope(ctx, r0.institution);
    require_institution_writable(&st, &scope, r0.institution).await?;
    let mut vars = BTreeMap::new();
    vars.insert("chargeNumber".to_owned(), json!(r0.charge_number));
    let outcome = request_approval(
        &st,
        &scope,
        process::RECEIVABLE_REOPEN,
        business_key("receivable", id, r0.version),
        vars,
    )
    .await?;
    let today = business_today(&st, &scope, r0.institution).await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let r = load(&mut tx, t, id, true).await?;
    require_version(v, r.version)?;
    let mut journal = None;
    if r.status == ReceivableStatus::WrittenOff {
        let posted: Vec<(Uuid, Decimal, Option<Uuid>)> = sqlx::query_as(
            "SELECT id, amount, journal_entry_id FROM finance.receivable_write_off WHERE tenant_id = $1 AND receivable_id = $2 AND status = 'posted' FOR UPDATE",
        )
        .bind(t.as_uuid())
        .bind(id)
        .fetch_all(&mut *tx)
        .await
        .map_err(map_db_error)?;
        for (wo, amount, entry) in posted {
            let c = lock_charge(&mut tx, t, ChargeId(r.charge_id)).await?;
            apply_settlement(&mut tx, &c, -amount, RECEIVABLE_REOPENED, &a).await?;
            if let Some(e) = entry {
                journal = Some(
                    posting::reverse(&mut tx, t, JournalEntryId(e), today, &reason, &a)
                        .await?
                        .journal_entry_id
                        .as_uuid(),
                );
            }
            sqlx::query("UPDATE finance.receivable_write_off SET status = 'reversed', reversal_entry_id = $2, version = version + 1, updated_at = now() WHERE id = $1")
                .bind(wo)
                .bind(journal)
                .execute(&mut *tx)
                .await
                .map_err(map_db_error)?;
            history(
                &mut tx,
                t,
                r.institution,
                "receivable_write_off",
                wo,
                Some("posted"),
                "reversed",
                &StatusChange {
                    cause_event_type: RECEIVABLE_REOPENED,
                    reason: Some(&reason),
                    process_instance_id: Some(outcome.process_instance_id()),
                },
                &a,
            )
            .await?;
        }
        sqlx::query("UPDATE finance.receivable SET written_off_amount = 0 WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
    }
    let c = crate::shared::charge::load_charge(&mut tx, t, ChargeId(r.charge_id)).await?;
    if c.outstanding_amount <= Decimal::ZERO {
        return Err(KernelError::Conflict("nothing is outstanding on the charge; record the new evidence (e.g. reverse the payment) first".into()).into());
    }
    set_status(
        &mut tx,
        &scope,
        &r,
        ReceivableStatus::Reopened,
        c.outstanding_amount,
        Some(&reason),
        Some(outcome.process_instance_id()),
        journal,
        &a,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "finance.receivable.reopen",
        "receivable",
        id.to_string(),
        json!({"reason": reason, "reversalEntryId": journal}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, ctx, id, StatusCode::OK).await
}

pub fn router(state: FinanceState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(receivable_list, receivable_evaluate))
        .routes(routes!(receivable_get))
        .routes(routes!(receivable_dunning))
        .routes(routes!(receivable_write_off))
        .routes(routes!(receivable_write_off_decide))
        .routes(routes!(receivable_reopen))
        .routes(routes!(dunning_run))
        .routes(routes!(dunning_get))
        .routes(routes!(dunning_resolve))
        .routes(routes!(dunning_cancel))
        .routes(routes!(super::latefee_http::late_fee_preview))
        .routes(routes!(super::latefee_http::late_fee_run))
        .routes(routes!(super::latefee_http::fee_reminder_run))
        .with_state(state)
}
