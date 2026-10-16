//! Charge persistence and lifecycle (L3-F-001 … F-006): manual charge creation, validation,
//! publication (Dr StudentReceivables · Cr each line's income account), governed revision,
//! suspend / resume / cancel, settlement closure, archival, instalment schedules and disputes.
//!
//! Charges are inserted only with `shared::charge::insert_charge`, their status changes only
//! through `shared::lifecycle::change_charge_status` (on a row locked with
//! `shared::charge::lock_charge`) and their amounts only through adjustments
//! (`shared::charge::apply_adjustment`, see `crate::adjustments::repo`). Every money movement
//! posts through `shared::posting` in the same transaction.

use super::domain::{check_schedule, derive_statuses, dispute_can_transition};
use super::kit::{status_event, subject_of, StatusEvent};
use crate::adjustments::repo as adj;
use crate::shared::account::ensure_student_account;
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::charge::{
    charge_from_row, charge_lines, insert_charge, load_charge, lock_charge, ChargeRow, NewCharge,
    NewChargeLine, CHARGE_COLUMNS,
};
use crate::shared::lifecycle::{change_charge_status, record_history, HistoryEntry, StatusChange};
use crate::shared::money::fmt;
use crate::shared::posting;
use crate::shared::publish::{audit, event};
use crate::shared::time::business_today;
use crate::shared::{Actor, FinanceState};
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::Serialize;
use serde_json::json;
use sqlx::{PgConnection, Row};
use std::collections::BTreeMap;
use sutra_contracts::finance::events::charges::*;
use sutra_contracts::finance::events::ModifiedV1;
use sutra_contracts::finance::ids::{
    ChargeDisputeId, ChargeId, FeeHeadId, InstalmentId, JournalEntryId, StudentAccountId,
};
use sutra_contracts::finance::ports::{
    source, AccountRef, PostingLine, PostingRequest, SystemAccount,
};
use sutra_contracts::finance::status::{
    AdjustmentKind, ChargeDisputeStatus, ChargeSourceType, ChargeStatus, InstalmentStatus,
    JournalEntryKind,
};
use sutra_contracts::student::ids::StudentId;
use sutra_kernel::error::KernelError;
use sutra_kernel::money::CurrencyCode;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use utoipa::ToSchema;
use uuid::Uuid;

// ================================================================ views

/// One fee-head line of a charge.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChargeLineView {
    pub line_no: i32,
    pub fee_head_id: Uuid,
    pub fee_head_code: String,
    pub description: String,
    #[schema(example = "125000.00")]
    pub amount: String,
}

/// One instalment of a charge's due schedule (status derived from the settled amount).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct InstalmentView {
    pub id: Uuid,
    pub sequence: i32,
    pub due_date: NaiveDate,
    #[schema(example = "62500.00")]
    pub amount: String,
    pub status: InstalmentStatus,
    pub version: i64,
}

/// A student charge with its lines and due schedule (contract §5.3).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChargeDetail {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub charge_number: String,
    pub student_account_id: Uuid,
    pub student_id: Option<Uuid>,
    pub admission_id: Option<Uuid>,
    pub display_name: String,
    pub student_number: Option<String>,
    pub description: String,
    pub currency: String,
    #[schema(example = "125000.00")]
    pub total_amount: String,
    pub adjustment_amount: String,
    pub settled_amount: String,
    pub outstanding_amount: String,
    pub due_date: Option<NaiveDate>,
    pub academic_year_id: Option<Uuid>,
    pub academic_term_id: Option<Uuid>,
    pub source_type: ChargeSourceType,
    pub source_id: Option<Uuid>,
    pub status: ChargeStatus,
    pub status_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
    pub journal_entry_id: Option<Uuid>,
    pub lines: Vec<ChargeLineView>,
    pub instalments: Vec<InstalmentView>,
    pub version: i64,
}

/// A charge in a list.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChargeListItem {
    pub id: Uuid,
    pub charge_number: String,
    pub student_account_id: Uuid,
    pub display_name: String,
    pub student_number: Option<String>,
    pub description: String,
    pub currency: String,
    pub total_amount: String,
    pub adjustment_amount: String,
    pub settled_amount: String,
    pub outstanding_amount: String,
    pub due_date: Option<NaiveDate>,
    pub source_type: ChargeSourceType,
    pub status: ChargeStatus,
    pub version: i64,
}

/// The publication journal entry of a charge, if any.
pub async fn publication_entry(
    conn: &mut PgConnection,
    tenant: TenantId,
    charge: ChargeId,
) -> Result<Option<Uuid>, KernelError> {
    sqlx::query_scalar(
        "SELECT id FROM finance.journal_entry WHERE tenant_id = $1 AND source_type = 'charge' \
         AND source_id = $2 AND posting_key = 'publish'",
    )
    .bind(tenant.as_uuid())
    .bind(charge.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)
}

/// Builds the detail view of a charge (instalment statuses derived as of `today`).
pub async fn detail(
    conn: &mut PgConnection,
    c: &ChargeRow,
    today: NaiveDate,
) -> Result<ChargeDetail, KernelError> {
    let lines = sqlx::query(
        "SELECT l.line_no, l.fee_head_id, h.code, l.description, l.amount FROM finance.charge_line l \
         JOIN finance.fee_head h ON h.id = l.fee_head_id WHERE l.tenant_id = $1 AND l.charge_id = $2 \
         ORDER BY l.line_no",
    )
    .bind(c.tenant_id.as_uuid())
    .bind(c.id.as_uuid())
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?
    .iter()
    .map(|r| {
        Ok(ChargeLineView {
            line_no: r.try_get("line_no")?,
            fee_head_id: r.try_get("fee_head_id")?,
            fee_head_code: r.try_get("code")?,
            description: r.try_get("description")?,
            amount: fmt(r.try_get("amount")?),
        })
    })
    .collect::<Result<Vec<_>, sqlx::Error>>()
    .map_err(map_db_error)?;
    let rows = sqlx::query(
        "SELECT i.id, i.sequence, i.due_date, i.amount, i.status, i.version FROM finance.instalment i \
         JOIN finance.instalment_plan p ON p.id = i.instalment_plan_id AND p.status = 'active' \
         WHERE i.tenant_id = $1 AND i.charge_id = $2 ORDER BY i.sequence",
    )
    .bind(c.tenant_id.as_uuid())
    .bind(c.id.as_uuid())
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let mut raw = Vec::with_capacity(rows.len());
    for r in &rows {
        let st: String = r.try_get("status").map_err(map_db_error)?;
        raw.push((
            r.try_get::<Uuid, _>("id").map_err(map_db_error)?,
            r.try_get::<i32, _>("sequence").map_err(map_db_error)?,
            r.try_get::<NaiveDate, _>("due_date")
                .map_err(map_db_error)?,
            r.try_get::<Decimal, _>("amount").map_err(map_db_error)?,
            InstalmentStatus::parse(&st).unwrap_or(InstalmentStatus::Scheduled),
            r.try_get::<i64, _>("version").map_err(map_db_error)?,
        ));
    }
    let derived = derive_statuses(
        &raw.iter().map(|x| (x.2, x.3, x.4)).collect::<Vec<_>>(),
        c.settled_amount,
        today,
    );
    let instalments = raw
        .iter()
        .zip(derived)
        .map(|(x, s)| InstalmentView {
            id: x.0,
            sequence: x.1,
            due_date: x.2,
            amount: fmt(x.3),
            status: if c.status == ChargeStatus::Cancelled {
                InstalmentStatus::Cancelled
            } else {
                s
            },
            version: x.5,
        })
        .collect();
    let acc = sqlx::query(
        "SELECT display_name, student_number FROM finance.student_account WHERE tenant_id = $1 AND id = $2",
    )
    .bind(c.tenant_id.as_uuid())
    .bind(c.student_account_id.as_uuid())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(ChargeDetail {
        id: c.id.as_uuid(),
        institution_id: c.institution_id.as_uuid(),
        charge_number: c.charge_number.clone(),
        student_account_id: c.student_account_id.as_uuid(),
        student_id: c.student_id,
        admission_id: c.admission_id,
        display_name: acc.try_get("display_name").map_err(map_db_error)?,
        student_number: acc.try_get("student_number").map_err(map_db_error)?,
        description: c.description.clone(),
        currency: c.currency.clone(),
        total_amount: fmt(c.total_amount),
        adjustment_amount: fmt(c.adjustment_amount),
        settled_amount: fmt(c.settled_amount),
        outstanding_amount: fmt(c.outstanding_amount),
        due_date: c.due_date,
        academic_year_id: c.academic_year_id,
        academic_term_id: c.academic_term_id,
        source_type: c.source_type,
        source_id: c.source_id,
        status: c.status,
        status_reason: c.status_reason.clone(),
        process_instance_id: c.process_instance_id.clone(),
        published_at: c.published_at,
        journal_entry_id: publication_entry(conn, c.tenant_id, c.id).await?,
        lines,
        instalments,
        version: c.version,
    })
}

/// Loads a charge and its detail view.
pub async fn get_detail(
    state: &FinanceState,
    tenant: TenantId,
    id: ChargeId,
) -> Result<ChargeDetail, KernelError> {
    let mut conn = state.pool.acquire().await.map_err(map_db_error)?;
    let c = load_charge(&mut conn, tenant, id).await?;
    let today = business_today(
        state,
        &ExecutionScope::institution(tenant, c.institution_id),
        c.institution_id,
    )
    .await?;
    detail(&mut conn, &c, today).await
}

/// Charges of an institution or account (newest number first), keyset-paged on the number.
#[allow(clippy::too_many_arguments)]
pub async fn list(
    state: &FinanceState,
    tenant: TenantId,
    institution: Option<InstitutionId>,
    account: Option<StudentAccountId>,
    status: Option<ChargeStatus>,
    q: Option<&str>,
    cursor: Option<&str>,
    limit: i64,
) -> Result<(Vec<ChargeListItem>, Option<String>), KernelError> {
    let pattern = q
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(sutra_persistence::like_contains);
    let rows = sqlx::query(
        "SELECT c.id, c.charge_number, c.student_account_id, a.display_name, a.student_number, \
         c.description, c.currency, c.total_amount, c.adjustment_amount, c.settled_amount, \
         c.outstanding_amount, c.due_date, c.source_type, c.status, c.version \
         FROM finance.charge c JOIN finance.student_account a ON a.id = c.student_account_id \
         WHERE c.tenant_id = $1 AND ($2::uuid IS NULL OR c.institution_id = $2) \
         AND ($3::uuid IS NULL OR c.student_account_id = $3) AND ($4::text IS NULL OR c.status = $4) \
         AND ($5::text IS NULL OR c.charge_number ILIKE $5 OR a.display_name ILIKE $5 \
              OR a.student_number ILIKE $5 OR c.description ILIKE $5) \
         AND ($6::text IS NULL OR c.charge_number < $6) \
         ORDER BY c.charge_number DESC LIMIT $7",
    )
    .bind(tenant.as_uuid())
    .bind(institution.map(|i| i.as_uuid()))
    .bind(account.map(|a| a.as_uuid()))
    .bind(status.map(|s| s.as_str()))
    .bind(pattern)
    .bind(cursor)
    .bind(limit + 1)
    .fetch_all(&state.pool)
    .await
    .map_err(map_db_error)?;
    let mut items = rows
        .iter()
        .map(|r| {
            let src: String = r.try_get("source_type")?;
            let st: String = r.try_get("status")?;
            Ok(ChargeListItem {
                id: r.try_get("id")?,
                charge_number: r.try_get("charge_number")?,
                student_account_id: r.try_get("student_account_id")?,
                display_name: r.try_get("display_name")?,
                student_number: r.try_get("student_number")?,
                description: r.try_get("description")?,
                currency: r.try_get("currency")?,
                total_amount: fmt(r.try_get("total_amount")?),
                adjustment_amount: fmt(r.try_get("adjustment_amount")?),
                settled_amount: fmt(r.try_get("settled_amount")?),
                outstanding_amount: fmt(r.try_get("outstanding_amount")?),
                due_date: r.try_get("due_date")?,
                source_type: ChargeSourceType::parse(&src)
                    .ok_or_else(|| sqlx::Error::Decode(src.clone().into()))?,
                status: ChargeStatus::parse(&st)
                    .ok_or_else(|| sqlx::Error::Decode(st.clone().into()))?,
                version: r.try_get("version")?,
            })
        })
        .collect::<Result<Vec<_>, sqlx::Error>>()
        .map_err(map_db_error)?;
    let next = if items.len() as i64 > limit {
        items.truncate(limit as usize);
        items.last().map(|c| c.charge_number.clone())
    } else {
        None
    };
    Ok((items, next))
}

// ================================================================ creation / validation / publication

/// A charge to raise for a student.
pub struct ChargeInput {
    pub student_id: StudentId,
    pub description: String,
    pub due_date: Option<NaiveDate>,
    pub academic_year_id: Option<Uuid>,
    pub academic_term_id: Option<Uuid>,
    pub source_type: ChargeSourceType,
    pub source_id: Option<Uuid>,
    pub lines: Vec<NewChargeLine>,
}

/// Resolves the student (must have a live context in the institution), opens / reuses their
/// party account and inserts a `draft` charge, in the caller's transaction. Emits no event
/// (drafts are internal); audits `finance.charge.create`.
pub async fn create_in_tx(
    state: &FinanceState,
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    institution: InstitutionId,
    input: &ChargeInput,
    actor: &Actor,
) -> Result<ChargeRow, KernelError> {
    let student = state
        .students
        .student(scope, input.student_id)
        .await?
        .ok_or_else(|| {
            super::kit::invalid("studentId", "not_found", "no such student in this tenant")
        })?;
    if state
        .students
        .live_context(scope, input.student_id, institution)
        .await?
        .is_none()
    {
        return Err(super::kit::invalid(
            "studentId",
            "no_live_context",
            "the student has no live context in this institution",
        ));
    }
    let account =
        ensure_student_account(conn, scope.tenant_id, institution, &student, actor).await?;
    let c = insert_charge(
        conn,
        &NewCharge {
            tenant: scope.tenant_id,
            institution,
            student_account_id: account.id(),
            student_id: Some(student.id.as_uuid()),
            admission_id: None,
            academic_year_id: input.academic_year_id,
            academic_term_id: input.academic_term_id,
            description: input.description.clone(),
            due_date: input.due_date,
            source_type: input.source_type,
            source_id: input.source_id,
            lines: input.lines.clone(),
        },
    )
    .await?;
    audit(
        conn,
        scope,
        "finance.charge.create",
        "charge",
        c.id.to_string(),
        json!({"number": c.charge_number, "total": fmt(c.total_amount), "sourceType": c.source_type, "sourceId": c.source_id}),
        actor,
    )
    .await?;
    Ok(c)
}

/// `draft → pending_validation → validated` on a locked charge (L3-F-001 validation step):
/// the account must be active and every line positive.
pub async fn validate_locked(
    conn: &mut PgConnection,
    c: &ChargeRow,
    actor: &Actor,
) -> Result<ChargeRow, KernelError> {
    let status: String = sqlx::query_scalar(
        "SELECT status FROM finance.student_account WHERE tenant_id = $1 AND id = $2",
    )
    .bind(c.tenant_id.as_uuid())
    .bind(c.student_account_id.as_uuid())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    if status != "active" {
        return Err(KernelError::Conflict(format!(
            "the student's finance account is {status}; charges cannot be validated"
        )));
    }
    let lines = charge_lines(conn, c.tenant_id, c.id).await?;
    let sum: Decimal = lines.iter().map(|l| l.amount).sum();
    if lines.is_empty() || sum != c.total_amount {
        return Err(KernelError::Conflict(
            "the charge lines do not add up to its total".into(),
        ));
    }
    let change = StatusChange {
        cause_event_type: CHARGE_PUBLISHED,
        reason: Some("validated"),
        process_instance_id: None,
    };
    let mut current = c.clone();
    if current.status == ChargeStatus::Draft {
        current = change_charge_status(
            conn,
            &current,
            ChargeStatus::PendingValidation,
            &change,
            actor,
        )
        .await?;
    }
    current = change_charge_status(conn, &current, ChargeStatus::Validated, &change, actor).await?;
    let scope = ExecutionScope::institution(c.tenant_id, c.institution_id);
    audit(
        conn,
        &scope,
        "finance.charge.validate",
        "charge",
        c.id.to_string(),
        json!({"lines": lines.len(), "total": fmt(sum)}),
        actor,
    )
    .await?;
    Ok(current)
}

/// Publishes a **locked, validated** charge: `published` + the publication entry (Dr
/// StudentReceivables for the party and charge · Cr each line's income account) +
/// `charge.published` (with the student's subject) + audit, in the caller's transaction.
pub async fn publish_locked(
    conn: &mut PgConnection,
    c: &ChargeRow,
    posting_date: NaiveDate,
    process_instance_id: Option<&str>,
    actor: &Actor,
) -> Result<(ChargeRow, JournalEntryId), KernelError> {
    if c.status != ChargeStatus::Validated {
        return Err(KernelError::Conflict(format!(
            "charge {} is {}; validate it before publication",
            c.charge_number, c.status
        )));
    }
    let lines = charge_lines(conn, c.tenant_id, c.id).await?;
    let mut by_account: BTreeMap<Uuid, Decimal> = BTreeMap::new();
    for l in &lines {
        *by_account.entry(l.income_account_id.as_uuid()).or_default() += l.amount;
    }
    let mut posting_lines = vec![PostingLine::debit(
        AccountRef::System(SystemAccount::StudentReceivables),
        c.total_amount,
    )
    .for_charge(c.student_account_id, Some(c.id))
    .memo(c.charge_number.clone())];
    for (account, amount) in by_account {
        posting_lines.push(
            PostingLine::credit(
                AccountRef::Id(sutra_contracts::finance::ids::LedgerAccountId(account)),
                amount,
            )
            .memo(c.charge_number.clone()),
        );
    }
    let receipt = posting::post(
        conn,
        c.tenant_id,
        &PostingRequest {
            institution_id: c.institution_id,
            posting_date,
            kind: JournalEntryKind::Standard,
            source_type: source::CHARGE.into(),
            source_id: c.id.as_uuid(),
            posting_key: "publish".into(),
            description: format!("Charge {} published: {}", c.charge_number, c.description),
            currency: CurrencyCode::INR,
            lines: posting_lines,
        },
        actor,
    )
    .await?;
    let published = change_charge_status(
        conn,
        c,
        ChargeStatus::Published,
        &StatusChange {
            cause_event_type: CHARGE_PUBLISHED,
            reason: None,
            process_instance_id,
        },
        actor,
    )
    .await?;
    let scope = ExecutionScope::institution(c.tenant_id, c.institution_id);
    let subject = subject_of(conn, c.tenant_id, c.student_account_id).await?;
    status_event(
        conn,
        &scope,
        (CHARGE_PUBLISHED, CHARGE_PUBLISHED_SCHEMA),
        StatusEvent {
            aggregate_id: c.id.as_uuid(),
            student_account_id: Some(c.student_account_id.as_uuid()),
            student_id: c.student_id,
            charge_id: Some(c.id.as_uuid()),
            from: Some(c.status.as_str().into()),
            to: published.status.as_str().into(),
            version: published.version,
            amount: Some(published.outstanding_amount),
            reason: Some(c.description.clone()),
            process_instance_id: process_instance_id.map(str::to_owned),
            journal_entry_id: Some(receipt.journal_entry_id.as_uuid()),
            subject,
        },
        actor,
    )
    .await?;
    audit(
        conn,
        &scope,
        "finance.charge.publish",
        "charge",
        c.id.to_string(),
        json!({"number": c.charge_number, "journalEntryId": receipt.journal_entry_id, "entryNumber": receipt.entry_number}),
        actor,
    )
    .await?;
    Ok((published, receipt.journal_entry_id))
}

/// Validates a draft charge (`charge.manage`).
pub async fn validate(
    state: &FinanceState,
    tenant: TenantId,
    id: ChargeId,
    expected: i64,
    actor: &Actor,
) -> Result<ChargeDetail, KernelError> {
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let c = lock_charge(&mut tx, tenant, id).await?;
    super::kit::require_version(c.version, expected)?;
    let scope = ExecutionScope::institution(tenant, c.institution_id);
    crate::shared::guard::require_institution_writable(state, &scope, c.institution_id).await?;
    validate_locked(&mut tx, &c, actor).await?;
    tx.commit().await.map_err(map_db_error)?;
    get_detail(state, tenant, id).await
}

/// Publishes a charge (a draft is validated first) under the `CHARGE_PUBLICATION` authority:
/// with auto-approval it is published at once; while a decision is awaited it stays
/// `validated` with the process id recorded and a second publish (by `finance.approve`)
/// completes it.
pub async fn publish(
    state: &FinanceState,
    tenant: TenantId,
    id: ChargeId,
    expected: i64,
    can_decide: bool,
    actor: &Actor,
) -> Result<ChargeDetail, KernelError> {
    let current = get_detail(state, tenant, id).await?;
    super::kit::require_version(current.version, expected)?;
    let institution = InstitutionId(current.institution_id);
    let scope = ExecutionScope::institution(tenant, institution);
    crate::shared::guard::require_institution_writable(state, &scope, institution).await?;
    let today = business_today(state, &scope, institution).await?;
    let awaiting =
        current.status == ChargeStatus::Validated && current.process_instance_id.is_some();
    let outcome = if awaiting && can_decide {
        ApprovalOutcome::Approved {
            process_instance_id: current.process_instance_id.clone().unwrap_or_default(),
        }
    } else {
        let mut vars = BTreeMap::new();
        vars.insert("chargeNumber".into(), json!(current.charge_number));
        vars.insert("amount".into(), json!(current.total_amount));
        request_approval(
            state,
            &scope,
            process::CHARGE_PUBLICATION,
            business_key("charge", id, current.version),
            vars,
        )
        .await?
    };
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let mut c = lock_charge(&mut tx, tenant, id).await?;
    super::kit::require_version(c.version, expected)?;
    if matches!(
        c.status,
        ChargeStatus::Draft | ChargeStatus::PendingValidation
    ) {
        c = validate_locked(&mut tx, &c, actor).await?;
    }
    match &outcome {
        ApprovalOutcome::Approved {
            process_instance_id,
        } => {
            publish_locked(&mut tx, &c, today, Some(process_instance_id), actor).await?;
        }
        ApprovalOutcome::Pending {
            process_instance_id,
        } => {
            let n = sqlx::query(
                "UPDATE finance.charge SET process_instance_id = $3, version = version + 1, \
                 updated_at = now() WHERE tenant_id = $1 AND id = $2 AND version = $4",
            )
            .bind(tenant.as_uuid())
            .bind(id.as_uuid())
            .bind(process_instance_id)
            .bind(c.version)
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?
            .rows_affected();
            if n != 1 {
                return Err(KernelError::Conflict("charge changed concurrently".into()));
            }
            audit(
                &mut tx,
                &scope,
                "finance.charge.request_publication",
                "charge",
                id.to_string(),
                json!({"processInstanceId": process_instance_id}),
                actor,
            )
            .await?;
        }
    }
    tx.commit().await.map_err(map_db_error)?;
    get_detail(state, tenant, id).await
}

// ================================================================ revision (F-002)

/// A governed revision request.
pub struct Revision {
    pub reason: String,
    pub description: Option<String>,
    pub due_date: Option<NaiveDate>,
    /// New net amount (total + adjustments); realised as a posted credit / debit note.
    pub new_amount: Option<Decimal>,
}

/// Revises a charge (L3-F-002). Description / due date change in place (with an append-only
/// revision record); an amount change on a published charge becomes a credit or debit note
/// approved through `CHARGE_REVISION` and posted (or left pending for a decision). Lines of an
/// unpublished charge are immutable: cancel it and raise a successor instead.
pub async fn revise(
    state: &FinanceState,
    tenant: TenantId,
    id: ChargeId,
    expected: i64,
    r: Revision,
    actor: &Actor,
) -> Result<ChargeDetail, KernelError> {
    let current = get_detail(state, tenant, id).await?;
    super::kit::require_version(current.version, expected)?;
    let institution = InstitutionId(current.institution_id);
    let scope = ExecutionScope::institution(tenant, institution);
    crate::shared::guard::require_institution_writable(state, &scope, institution).await?;
    let today = business_today(state, &scope, institution).await?;
    if r.description.is_none() && r.due_date.is_none() && r.new_amount.is_none() {
        return Err(super::kit::invalid(
            "newAmount",
            "nothing_to_revise",
            "give a new amount, due date or description",
        ));
    }
    let outcome = if r.new_amount.is_some() {
        Some(
            request_approval(
                state,
                &scope,
                process::CHARGE_REVISION,
                business_key("charge-revision", id, current.version),
                BTreeMap::new(),
            )
            .await?,
        )
    } else {
        None
    };
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let c = lock_charge(&mut tx, tenant, id).await?;
    super::kit::require_version(c.version, expected)?;
    if c.status.is_terminal()
        || matches!(
            c.status,
            ChargeStatus::Cancelled
                | ChargeStatus::Waived
                | ChargeStatus::WrittenOff
                | ChargeStatus::Closed
        )
    {
        return Err(KernelError::Conflict(format!(
            "charge {} is {}; it can no longer be revised",
            c.charge_number, c.status
        )));
    }
    let mut changed = Vec::new();
    let before = json!({"description": c.description, "dueDate": c.due_date, "netAmount": fmt(c.total_amount + c.adjustment_amount)});
    let mut adjustment_id = None;
    let mut c2 = c.clone();
    if r.description.is_some() || r.due_date.is_some() {
        let row = sqlx::query(&format!(
            "UPDATE finance.charge SET description = COALESCE($3, description), \
             due_date = COALESCE($4, due_date), version = version + 1, updated_at = now() \
             WHERE tenant_id = $1 AND id = $2 AND version = $5 RETURNING {CHARGE_COLUMNS}"
        ))
        .bind(tenant.as_uuid())
        .bind(id.as_uuid())
        .bind(r.description.as_deref())
        .bind(r.due_date)
        .bind(c.version)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_db_error)?
        .ok_or_else(|| KernelError::Conflict("charge changed concurrently".into()))?;
        c2 = charge_from_row(&row).map_err(map_db_error)?;
        if r.description.is_some() {
            changed.push("description".to_owned());
        }
        if r.due_date.is_some() {
            changed.push("dueDate".to_owned());
        }
    }
    let mut pid = None;
    if let (Some(target), Some(outcome)) = (r.new_amount, outcome.as_ref()) {
        if !c2.status.accepts_adjustment() {
            return Err(KernelError::Conflict(format!(
                "charge {} is {}; only published charges are revised by amount (cancel an unpublished charge and raise a successor)",
                c2.charge_number, c2.status
            )));
        }
        let net = c2.total_amount + c2.adjustment_amount;
        let diff = target - net;
        if diff.is_zero() {
            return Err(super::kit::invalid(
                "newAmount",
                "unchanged",
                "the new amount equals the current amount",
            ));
        }
        let kind = if diff > Decimal::ZERO {
            AdjustmentKind::DebitNote
        } else {
            AdjustmentKind::CreditNote
        };
        let a = adj::insert(
            &mut tx,
            &c2,
            &adj::NewAdjustment {
                kind,
                amount: diff.abs(),
                reason: &r.reason,
                origin: "charge_revision",
                supersedes: None,
            },
            actor,
        )
        .await?;
        let a = adj::mark_pending(&mut tx, &a, outcome.process_instance_id(), actor).await?;
        pid = Some(outcome.process_instance_id().to_owned());
        if let ApprovalOutcome::Approved { .. } = outcome {
            let locked = lock_charge(&mut tx, tenant, id).await?;
            adj::post_locked(
                &mut tx,
                &a,
                &locked,
                today,
                Some(crate::shared::approval::AUTO_APPROVER),
                actor,
            )
            .await?;
        }
        adjustment_id = Some(a.id.as_uuid());
        changed.push("netAmount".to_owned());
    }
    let after_row = load_charge(&mut tx, tenant, id).await?;
    let revision_no: i32 = sqlx::query_scalar(
        "SELECT COALESCE(max(revision_no), 0) + 1 FROM finance.charge_revision WHERE charge_id = $1",
    )
    .bind(id.as_uuid())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    sqlx::query(
        "INSERT INTO finance.charge_revision (tenant_id, institution_id, charge_id, revision_no, \
         reason, changed_fields, before_state, after_state, adjustment_id, process_instance_id, actor) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
    )
    .bind(tenant.as_uuid())
    .bind(institution.as_uuid())
    .bind(id.as_uuid())
    .bind(revision_no)
    .bind(&r.reason)
    .bind(&changed)
    .bind(&before)
    .bind(json!({"description": after_row.description, "dueDate": after_row.due_date, "netAmount": fmt(after_row.total_amount + after_row.adjustment_amount), "adjustmentId": adjustment_id}))
    .bind(adjustment_id)
    .bind(pid.as_deref())
    .bind(actor.id.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    event(
        &mut tx,
        &scope,
        CHARGE_REVISED,
        CHARGE_REVISED_SCHEMA,
        id.to_string(),
        &ModifiedV1 {
            aggregate_id: id.as_uuid(),
            institution_id: institution.as_uuid(),
            version: after_row.version,
            changed_fields: changed.clone(),
        },
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "finance.charge.revise",
        "charge",
        id.to_string(),
        json!({"revisionNo": revision_no, "reason": r.reason, "changedFields": changed, "adjustmentId": adjustment_id}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get_detail(state, tenant, id).await
}

/// One revision record of a charge.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChargeRevisionView {
    pub revision_no: i32,
    pub reason: String,
    pub changed_fields: Vec<String>,
    pub before_state: serde_json::Value,
    pub after_state: serde_json::Value,
    pub adjustment_id: Option<Uuid>,
    pub actor: Option<String>,
    pub created_at: DateTime<Utc>,
}

pub async fn revisions(
    state: &FinanceState,
    tenant: TenantId,
    id: ChargeId,
) -> Result<Vec<ChargeRevisionView>, KernelError> {
    sqlx::query(
        "SELECT revision_no, reason, changed_fields, before_state, after_state, adjustment_id, actor, created_at \
         FROM finance.charge_revision WHERE tenant_id = $1 AND charge_id = $2 ORDER BY revision_no",
    )
    .bind(tenant.as_uuid())
    .bind(id.as_uuid())
    .fetch_all(&state.pool)
    .await
    .map_err(map_db_error)?
    .iter()
    .map(|r| {
        Ok(ChargeRevisionView {
            revision_no: r.try_get("revision_no")?,
            reason: r.try_get("reason")?,
            changed_fields: r.try_get("changed_fields")?,
            before_state: r.try_get("before_state")?,
            after_state: r.try_get("after_state")?,
            adjustment_id: r.try_get("adjustment_id")?,
            actor: r.try_get("actor")?,
            created_at: r.try_get("created_at")?,
        })
    })
    .collect::<Result<_, sqlx::Error>>()
    .map_err(map_db_error)
}

// ================================================================ suspend / resume / cancel / close / archive

/// Lifecycle commands of a charge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Suspend,
    Resume,
    Cancel,
    Close,
    Archive,
}

impl Command {
    fn target(self) -> ChargeStatus {
        match self {
            Self::Suspend => ChargeStatus::Suspended,
            Self::Resume => ChargeStatus::Active,
            Self::Cancel => ChargeStatus::Cancelled,
            Self::Close => ChargeStatus::Closed,
            Self::Archive => ChargeStatus::Archived,
        }
    }
    fn event(self) -> (&'static str, &'static str) {
        match self {
            Self::Suspend => (CHARGE_SUSPENDED, CHARGE_SUSPENDED_SCHEMA),
            Self::Resume => (CHARGE_RESUMED, CHARGE_RESUMED_SCHEMA),
            Self::Cancel => (CHARGE_CANCELLED, CHARGE_CANCELLED_SCHEMA),
            Self::Close => (CHARGE_CLOSED, CHARGE_CLOSED_SCHEMA),
            Self::Archive => (CHARGE_ARCHIVED, CHARGE_ARCHIVED_SCHEMA),
        }
    }
    fn action(self) -> &'static str {
        match self {
            Self::Suspend => "finance.charge.suspend",
            Self::Resume => "finance.charge.resume",
            Self::Cancel => "finance.charge.cancel",
            Self::Close => "finance.charge.close",
            Self::Archive => "finance.charge.archive",
        }
    }
}

/// Runs a lifecycle command (L3-F-003 / F-006). Cancelling a published charge requires that
/// nothing was settled (refund the settled part first) and clears the receivable in the ledger:
/// the publication entry is reversed when no adjustment was posted, otherwise the remaining
/// outstanding amount is posted back (`charge` / `cancel`). Closing requires a zero outstanding
/// amount; archiving a closed / cancelled / waived / written-off charge freezes it.
pub async fn command(
    state: &FinanceState,
    tenant: TenantId,
    id: ChargeId,
    expected: i64,
    cmd: Command,
    reason: Option<String>,
    actor: &Actor,
) -> Result<ChargeDetail, KernelError> {
    if matches!(cmd, Command::Suspend | Command::Cancel) && reason.is_none() {
        return Err(super::kit::invalid("reason", "required", "is required"));
    }
    let current = get_detail(state, tenant, id).await?;
    let institution = InstitutionId(current.institution_id);
    let scope = ExecutionScope::institution(tenant, institution);
    crate::shared::guard::require_institution_writable(state, &scope, institution).await?;
    let today = business_today(state, &scope, institution).await?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let c = lock_charge(&mut tx, tenant, id).await?;
    super::kit::require_version(c.version, expected)?;
    let to = cmd.target();
    if !c.status.can_transition(to) {
        return Err(KernelError::Conflict(format!(
            "charge {} is {}; it cannot become {to}",
            c.charge_number, c.status
        )));
    }
    let mut journal = None;
    match cmd {
        Command::Cancel => {
            if c.published_at.is_some() {
                if !c.settled_amount.is_zero() {
                    return Err(KernelError::Conflict(format!(
                        "charge {} has settled payments; refund or credit them before cancelling",
                        c.charge_number
                    )));
                }
                let publication = publication_entry(&mut tx, tenant, id).await?;
                if let (true, Some(entry)) = (c.adjustment_amount.is_zero(), publication) {
                    let r = posting::reverse(
                        &mut tx,
                        tenant,
                        JournalEntryId(entry),
                        today,
                        reason.as_deref().unwrap_or("cancelled"),
                        actor,
                    )
                    .await?;
                    journal = Some(r.journal_entry_id.as_uuid());
                } else if c.outstanding_amount > Decimal::ZERO {
                    let income = charge_lines(&mut tx, tenant, id)
                        .await?
                        .first()
                        .map(|l| AccountRef::Id(l.income_account_id))
                        .unwrap_or(AccountRef::System(SystemAccount::OtherIncome));
                    let r = posting::post(
                        &mut tx,
                        tenant,
                        &PostingRequest {
                            institution_id: c.institution_id,
                            posting_date: today,
                            kind: JournalEntryKind::Standard,
                            source_type: source::CHARGE.into(),
                            source_id: id.as_uuid(),
                            posting_key: "cancel".into(),
                            description: format!("Charge {} cancelled", c.charge_number),
                            currency: CurrencyCode::INR,
                            lines: vec![
                                PostingLine::debit(income, c.outstanding_amount),
                                PostingLine::credit(
                                    AccountRef::System(SystemAccount::StudentReceivables),
                                    c.outstanding_amount,
                                )
                                .for_charge(c.student_account_id, Some(id)),
                            ],
                        },
                        actor,
                    )
                    .await?;
                    journal = Some(r.journal_entry_id.as_uuid());
                }
            }
            sqlx::query(
                "UPDATE finance.instalment_plan SET status = 'cancelled', version = version + 1, \
                 updated_at = now() WHERE tenant_id = $1 AND charge_id = $2 AND status = 'active'",
            )
            .bind(tenant.as_uuid())
            .bind(id.as_uuid())
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
        }
        Command::Close if !c.outstanding_amount.is_zero() => {
            return Err(KernelError::Conflict(format!(
                "charge {} still has {} outstanding",
                c.charge_number,
                crate::shared::money::display_inr(c.outstanding_amount)
            )));
        }
        _ => {}
    }
    let next = change_charge_status(
        &mut tx,
        &c,
        to,
        &StatusChange {
            cause_event_type: cmd.event().0,
            reason: reason.as_deref(),
            process_instance_id: None,
        },
        actor,
    )
    .await?;
    let subject = subject_of(&mut tx, tenant, c.student_account_id).await?;
    status_event(
        &mut tx,
        &scope,
        cmd.event(),
        StatusEvent {
            aggregate_id: id.as_uuid(),
            student_account_id: Some(c.student_account_id.as_uuid()),
            student_id: c.student_id,
            charge_id: Some(id.as_uuid()),
            from: Some(c.status.as_str().into()),
            to: to.as_str().into(),
            version: next.version,
            amount: Some(c.outstanding_amount),
            reason: reason.clone(),
            journal_entry_id: journal,
            subject,
            ..StatusEvent::default()
        },
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        cmd.action(),
        "charge",
        id.to_string(),
        json!({"from": c.status, "to": to, "reason": reason, "journalEntryId": journal}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get_detail(state, tenant, id).await
}

// ================================================================ instalments (F-005)

/// Sets (or replaces) the due schedule of a charge: the instalments must sum to the charge's
/// net amount. Instalments already due are marked `due` and announced (`instalment.due`) when
/// the charge is visible to the student.
pub async fn set_schedule(
    state: &FinanceState,
    tenant: TenantId,
    id: ChargeId,
    items: Vec<(NaiveDate, Decimal)>,
    reason: Option<String>,
    actor: &Actor,
) -> Result<ChargeDetail, KernelError> {
    let current = get_detail(state, tenant, id).await?;
    let institution = InstitutionId(current.institution_id);
    let scope = ExecutionScope::institution(tenant, institution);
    crate::shared::guard::require_institution_writable(state, &scope, institution).await?;
    let today = business_today(state, &scope, institution).await?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let c = lock_charge(&mut tx, tenant, id).await?;
    if matches!(
        c.status,
        ChargeStatus::Cancelled
            | ChargeStatus::Waived
            | ChargeStatus::WrittenOff
            | ChargeStatus::Closed
            | ChargeStatus::Archived
    ) {
        return Err(KernelError::Conflict(format!(
            "charge {} is {}; it has no due schedule",
            c.charge_number, c.status
        )));
    }
    let net = c.total_amount + c.adjustment_amount;
    check_schedule(net, &items)
        .map_err(|m| super::kit::invalid("instalments", "invalid_schedule", &m))?;
    sqlx::query(
        "UPDATE finance.instalment_plan SET status = 'replaced', version = version + 1, updated_at = now() \
         WHERE tenant_id = $1 AND charge_id = $2 AND status = 'active'",
    )
    .bind(tenant.as_uuid())
    .bind(id.as_uuid())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let plan: Uuid = sqlx::query_scalar(
        "INSERT INTO finance.instalment_plan (tenant_id, institution_id, charge_id, student_account_id, \
         instalment_count, total_amount, reason) VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
    )
    .bind(tenant.as_uuid())
    .bind(institution.as_uuid())
    .bind(id.as_uuid())
    .bind(c.student_account_id.as_uuid())
    .bind(i32::try_from(items.len()).unwrap_or(i32::MAX))
    .bind(net)
    .bind(reason.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let subject = subject_of(&mut tx, tenant, c.student_account_id).await?;
    let visible = c.status.is_visible_to_student();
    for (i, (due, amount)) in items.iter().enumerate() {
        let is_due = visible && *due <= today;
        let inst: Uuid = sqlx::query_scalar(
            "INSERT INTO finance.instalment (tenant_id, instalment_plan_id, charge_id, sequence, due_date, \
             amount, status, due_notified_at) VALUES ($1, $2, $3, $4, $5, $6, $7, \
             CASE WHEN $8 THEN now() END) RETURNING id",
        )
        .bind(tenant.as_uuid())
        .bind(plan)
        .bind(id.as_uuid())
        .bind(i32::try_from(i + 1).unwrap_or(i32::MAX))
        .bind(due)
        .bind(amount)
        .bind(if is_due { "due" } else { "scheduled" })
        .bind(is_due)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
        if is_due {
            status_event(
                &mut tx,
                &scope,
                (INSTALMENT_DUE, INSTALMENT_DUE_SCHEMA),
                StatusEvent {
                    aggregate_id: inst,
                    student_account_id: Some(c.student_account_id.as_uuid()),
                    student_id: c.student_id,
                    charge_id: Some(id.as_uuid()),
                    from: Some("scheduled".into()),
                    to: "due".into(),
                    version: 1,
                    amount: Some(*amount),
                    reason: Some(format!(
                        "{} instalment {} due {due}",
                        c.charge_number,
                        i + 1
                    )),
                    subject: subject.clone(),
                    ..StatusEvent::default()
                },
                actor,
            )
            .await?;
        }
    }
    status_event(
        &mut tx,
        &scope,
        (INSTALMENT_PLAN_CREATED, INSTALMENT_PLAN_CREATED_SCHEMA),
        StatusEvent {
            aggregate_id: plan,
            student_account_id: Some(c.student_account_id.as_uuid()),
            student_id: c.student_id,
            charge_id: Some(id.as_uuid()),
            to: "active".into(),
            version: 1,
            amount: Some(net),
            reason: reason.clone(),
            subject,
            ..StatusEvent::default()
        },
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "finance.instalment_plan.create",
        "instalment_plan",
        plan.to_string(),
        json!({"chargeId": id, "count": items.len(), "total": fmt(net)}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get_detail(state, tenant, id).await
}

/// Moves the due date of one unpaid instalment (keeping the order of the schedule).
pub async fn reschedule(
    state: &FinanceState,
    tenant: TenantId,
    id: ChargeId,
    instalment: InstalmentId,
    expected: i64,
    due_date: NaiveDate,
    actor: &Actor,
) -> Result<ChargeDetail, KernelError> {
    let current = get_detail(state, tenant, id).await?;
    let mine = current
        .instalments
        .iter()
        .find(|i| i.id == instalment.as_uuid())
        .ok_or(KernelError::NotFound {
            entity: "instalment",
        })?;
    super::kit::require_version(mine.version, expected)?;
    if matches!(
        mine.status,
        InstalmentStatus::Paid | InstalmentStatus::Waived | InstalmentStatus::Cancelled
    ) {
        return Err(KernelError::Conflict(format!(
            "instalment {} is {}; it cannot be rescheduled",
            mine.sequence, mine.status
        )));
    }
    let prev = current
        .instalments
        .iter()
        .filter(|i| i.sequence < mine.sequence)
        .map(|i| i.due_date)
        .max();
    let next = current
        .instalments
        .iter()
        .filter(|i| i.sequence > mine.sequence)
        .map(|i| i.due_date)
        .min();
    if prev.is_some_and(|p| due_date <= p) || next.is_some_and(|n| due_date >= n) {
        return Err(super::kit::invalid(
            "dueDate",
            "out_of_order",
            "the due date must stay between the neighbouring instalments",
        ));
    }
    let scope = ExecutionScope::institution(tenant, InstitutionId(current.institution_id));
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let n = sqlx::query(
        "UPDATE finance.instalment SET due_date = $3, status = 'scheduled', version = version + 1, \
         updated_at = now() WHERE tenant_id = $1 AND id = $2 AND version = $4",
    )
    .bind(tenant.as_uuid())
    .bind(instalment.as_uuid())
    .bind(due_date)
    .bind(expected)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?
    .rows_affected();
    if n != 1 {
        return Err(KernelError::Conflict(
            "instalment changed concurrently".into(),
        ));
    }
    audit(
        &mut tx,
        &scope,
        "finance.instalment.reschedule",
        "instalment",
        instalment.to_string(),
        json!({"chargeId": id, "from": mine.due_date, "to": due_date}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get_detail(state, tenant, id).await
}

// ================================================================ disputes (F-004)

/// A charge dispute.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DisputeView {
    pub id: Uuid,
    pub charge_id: Uuid,
    pub charge_number: String,
    pub student_account_id: Uuid,
    pub reason: String,
    pub raised_by_student: bool,
    pub disputed_amount: Option<String>,
    pub prior_charge_status: ChargeStatus,
    pub status: ChargeDisputeStatus,
    /// `none | credit | waive` once upheld.
    pub resolution: Option<String>,
    pub resolution_note: Option<String>,
    pub adjustment_id: Option<Uuid>,
    pub decided_by: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
    pub process_instance_id: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

const DISPUTE_COLUMNS: &str = "d.id, d.tenant_id, d.institution_id, d.charge_id, c.charge_number, \
    d.student_account_id, d.reason, d.raised_by_student, d.disputed_amount, d.prior_charge_status, \
    d.status, d.resolution, d.resolution_note, d.adjustment_id, d.decided_by, d.decided_at, \
    d.process_instance_id, d.version, d.created_at";

fn dispute_from_row(r: &sqlx::postgres::PgRow) -> Result<DisputeView, sqlx::Error> {
    let prior: String = r.try_get("prior_charge_status")?;
    let st: String = r.try_get("status")?;
    Ok(DisputeView {
        id: r.try_get("id")?,
        charge_id: r.try_get("charge_id")?,
        charge_number: r.try_get("charge_number")?,
        student_account_id: r.try_get("student_account_id")?,
        reason: r.try_get("reason")?,
        raised_by_student: r.try_get("raised_by_student")?,
        disputed_amount: r.try_get::<Option<Decimal>, _>("disputed_amount")?.map(fmt),
        prior_charge_status: ChargeStatus::parse(&prior)
            .ok_or_else(|| sqlx::Error::Decode(prior.clone().into()))?,
        status: ChargeDisputeStatus::parse(&st)
            .ok_or_else(|| sqlx::Error::Decode(st.clone().into()))?,
        resolution: r.try_get("resolution")?,
        resolution_note: r.try_get("resolution_note")?,
        adjustment_id: r.try_get("adjustment_id")?,
        decided_by: r.try_get("decided_by")?,
        decided_at: r.try_get("decided_at")?,
        process_instance_id: r.try_get("process_instance_id")?,
        version: r.try_get("version")?,
        created_at: r.try_get("created_at")?,
    })
}

async fn fetch_dispute(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: ChargeDisputeId,
    lock: bool,
) -> Result<DisputeView, KernelError> {
    let row = sqlx::query(&format!(
        "SELECT {DISPUTE_COLUMNS} FROM finance.charge_dispute d JOIN finance.charge c ON c.id = d.charge_id \
         WHERE d.tenant_id = $1 AND d.id = $2{}",
        if lock { " FOR UPDATE OF d" } else { "" }
    ))
    .bind(tenant.as_uuid())
    .bind(id.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound { entity: "dispute" })?;
    dispute_from_row(&row).map_err(map_db_error)
}

pub async fn get_dispute(
    state: &FinanceState,
    tenant: TenantId,
    id: ChargeDisputeId,
) -> Result<DisputeView, KernelError> {
    let mut conn = state.pool.acquire().await.map_err(map_db_error)?;
    fetch_dispute(&mut conn, tenant, id, false).await
}

pub async fn disputes_of_charge(
    state: &FinanceState,
    tenant: TenantId,
    charge: ChargeId,
) -> Result<Vec<DisputeView>, KernelError> {
    sqlx::query(&format!(
        "SELECT {DISPUTE_COLUMNS} FROM finance.charge_dispute d JOIN finance.charge c ON c.id = d.charge_id \
         WHERE d.tenant_id = $1 AND d.charge_id = $2 ORDER BY d.created_at DESC"
    ))
    .bind(tenant.as_uuid())
    .bind(charge.as_uuid())
    .fetch_all(&state.pool)
    .await
    .map_err(map_db_error)?
    .iter()
    .map(dispute_from_row)
    .collect::<Result<_, _>>()
    .map_err(map_db_error)
}

/// Raises a dispute on a published / active charge (staff or the student themself): the
/// charge becomes `disputed` (not demanded, not payable) until the dispute is resolved.
pub async fn raise_dispute(
    state: &FinanceState,
    tenant: TenantId,
    charge: ChargeId,
    reason: &str,
    amount: Option<Decimal>,
    by_student: bool,
    actor: &Actor,
) -> Result<DisputeView, KernelError> {
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let c = lock_charge(&mut tx, tenant, charge).await?;
    let scope = ExecutionScope::institution(tenant, c.institution_id);
    crate::shared::guard::require_institution_writable(state, &scope, c.institution_id).await?;
    if let Some(a) = amount {
        if a > c.outstanding_amount {
            return Err(super::kit::invalid(
                "amount",
                "exceeds_outstanding",
                "the disputed amount exceeds the outstanding amount",
            ));
        }
    }
    let next = change_charge_status(
        &mut tx,
        &c,
        ChargeStatus::Disputed,
        &StatusChange {
            cause_event_type: CHARGE_DISPUTED,
            reason: Some(reason),
            process_instance_id: None,
        },
        actor,
    )
    .await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO finance.charge_dispute (tenant_id, institution_id, charge_id, student_account_id, \
         raised_by, raised_by_student, reason, disputed_amount, prior_charge_status) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING id",
    )
    .bind(tenant.as_uuid())
    .bind(c.institution_id.as_uuid())
    .bind(charge.as_uuid())
    .bind(c.student_account_id.as_uuid())
    .bind(actor.id.as_deref())
    .bind(by_student)
    .bind(reason)
    .bind(amount)
    .bind(c.status.as_str())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let subject = subject_of(&mut tx, tenant, c.student_account_id).await?;
    status_event(
        &mut tx,
        &scope,
        (CHARGE_DISPUTED, CHARGE_DISPUTED_SCHEMA),
        StatusEvent {
            aggregate_id: charge.as_uuid(),
            student_account_id: Some(c.student_account_id.as_uuid()),
            student_id: c.student_id,
            charge_id: Some(charge.as_uuid()),
            from: Some(c.status.as_str().into()),
            to: next.status.as_str().into(),
            version: next.version,
            amount: amount.or(Some(c.outstanding_amount)),
            reason: Some(reason.to_owned()),
            subject,
            ..StatusEvent::default()
        },
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "finance.charge_dispute.raise",
        "charge_dispute",
        id.to_string(),
        json!({"chargeId": charge, "reason": reason, "byStudent": by_student}),
        actor,
    )
    .await?;
    let d = fetch_dispute(&mut tx, tenant, ChargeDisputeId(id), false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(d)
}

/// How an upheld dispute is resolved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    /// Upheld without a financial change (e.g. a due date explanation).
    None,
    /// A credit note of the given amount is posted.
    Credit(Decimal),
    /// The outstanding amount is waived (posted waiver).
    Waive,
}

/// Decides a dispute (`finance.approve`): upheld (optionally with a posted credit note or a
/// waiver of the outstanding amount) or rejected, or withdrawn by the party. The charge leaves
/// `disputed` (→ `active`, or `waived` when a waiver cleared an unpaid charge). The
/// `CHARGE_DISPUTE_RESOLUTION` process is started for evidence; the caller is the deciding
/// authority.
#[allow(clippy::too_many_arguments)]
pub async fn resolve_dispute(
    state: &FinanceState,
    tenant: TenantId,
    id: ChargeDisputeId,
    expected: i64,
    to: ChargeDisputeStatus,
    resolution: Resolution,
    note: Option<String>,
    actor: &Actor,
) -> Result<DisputeView, KernelError> {
    let current = get_dispute(state, tenant, id).await?;
    super::kit::require_version(current.version, expected)?;
    let institution = {
        let mut conn = state.pool.acquire().await.map_err(map_db_error)?;
        load_charge(&mut conn, tenant, ChargeId(current.charge_id))
            .await?
            .institution_id
    };
    let scope = ExecutionScope::institution(tenant, institution);
    let today = business_today(state, &scope, institution).await?;
    let pid = if to == ChargeDisputeStatus::Withdrawn {
        None
    } else {
        Some(
            request_approval(
                state,
                &scope,
                process::CHARGE_DISPUTE_RESOLUTION,
                business_key("charge-dispute", id, current.version),
                BTreeMap::new(),
            )
            .await?
            .process_instance_id()
            .to_owned(),
        )
    };
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let d = fetch_dispute(&mut tx, tenant, id, true).await?;
    super::kit::require_version(d.version, expected)?;
    if !dispute_can_transition(d.status, to) {
        return Err(KernelError::Conflict(format!(
            "dispute is {}; it cannot become {to}",
            d.status
        )));
    }
    let charge_id = ChargeId(d.charge_id);
    let mut adjustment_id = None;
    let mut resolution_wire = None;
    if to == ChargeDisputeStatus::Upheld {
        resolution_wire = Some(match resolution {
            Resolution::None => "none",
            Resolution::Credit(_) => "credit",
            Resolution::Waive => "waive",
        });
        let c = lock_charge(&mut tx, tenant, charge_id).await?;
        let kind_amount = match resolution {
            Resolution::None => None,
            Resolution::Credit(a) => Some((AdjustmentKind::CreditNote, a)),
            Resolution::Waive => Some((AdjustmentKind::Waiver, c.outstanding_amount)),
        };
        if let Some((kind, amount)) = kind_amount.filter(|(_, a)| *a > Decimal::ZERO) {
            let reason = note.clone().unwrap_or_else(|| "dispute upheld".into());
            let a = adj::insert(
                &mut tx,
                &c,
                &adj::NewAdjustment {
                    kind,
                    amount,
                    reason: &reason,
                    origin: "dispute",
                    supersedes: None,
                },
                actor,
            )
            .await?;
            let a =
                adj::mark_pending(&mut tx, &a, pid.as_deref().unwrap_or_default(), actor).await?;
            adj::post_locked(&mut tx, &a, &c, today, actor.id.as_deref(), actor).await?;
            adjustment_id = Some(a.id.as_uuid());
        }
    }
    // The charge leaves `disputed` unless the waiver already settled its fate.
    let c = lock_charge(&mut tx, tenant, charge_id).await?;
    if c.status == ChargeStatus::Disputed {
        change_charge_status(
            &mut tx,
            &c,
            ChargeStatus::Active,
            &StatusChange {
                cause_event_type: CHARGE_DISPUTE_RESOLVED,
                reason: note.as_deref(),
                process_instance_id: pid.as_deref(),
            },
            actor,
        )
        .await?;
    }
    let n = sqlx::query(
        "UPDATE finance.charge_dispute SET status = $3, resolution = $4, resolution_note = $5, \
         adjustment_id = $6, decided_by = $7, decided_at = now(), process_instance_id = $8, \
         version = version + 1, updated_at = now() WHERE tenant_id = $1 AND id = $2 AND version = $9",
    )
    .bind(tenant.as_uuid())
    .bind(id.as_uuid())
    .bind(to.as_str())
    .bind(resolution_wire)
    .bind(note.as_deref())
    .bind(adjustment_id)
    .bind(actor.id.as_deref())
    .bind(pid.as_deref())
    .bind(d.version)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?
    .rows_affected();
    if n != 1 {
        return Err(KernelError::Conflict("dispute changed concurrently".into()));
    }
    record_history(
        &mut tx,
        HistoryEntry {
            tenant,
            institution,
            aggregate_type: "charge_dispute",
            aggregate_id: id.as_uuid(),
            from: Some(d.status.as_str()),
            to: to.as_str(),
        },
        &StatusChange {
            cause_event_type: CHARGE_DISPUTE_RESOLVED,
            reason: note.as_deref(),
            process_instance_id: pid.as_deref(),
        },
        actor,
    )
    .await?;
    let after = load_charge(&mut tx, tenant, charge_id).await?;
    let subject = subject_of(&mut tx, tenant, after.student_account_id).await?;
    status_event(
        &mut tx,
        &scope,
        (CHARGE_DISPUTE_RESOLVED, CHARGE_DISPUTE_RESOLVED_SCHEMA),
        StatusEvent {
            aggregate_id: id.as_uuid(),
            student_account_id: Some(after.student_account_id.as_uuid()),
            student_id: after.student_id,
            charge_id: Some(charge_id.as_uuid()),
            from: Some(d.status.as_str().into()),
            to: to.as_str().into(),
            version: d.version + 1,
            amount: Some(after.outstanding_amount),
            reason: note.clone(),
            process_instance_id: pid.clone(),
            subject,
            ..StatusEvent::default()
        },
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "finance.charge_dispute.resolve",
        "charge_dispute",
        id.to_string(),
        json!({"to": to, "resolution": resolution_wire, "adjustmentId": adjustment_id, "note": note}),
        actor,
    )
    .await?;
    let out = fetch_dispute(&mut tx, tenant, id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

/// Line input of a manual charge / fee structure (already validated).
pub fn line(fee_head: Uuid, description: String, amount: Decimal) -> NewChargeLine {
    NewChargeLine {
        fee_head_id: FeeHeadId(fee_head),
        description,
        amount,
    }
}
