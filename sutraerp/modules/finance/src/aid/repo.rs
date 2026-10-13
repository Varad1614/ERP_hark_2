//! Financial aid persistence (L3-F-062 … F-066). Approved scholarship decisions are read only
//! through `state.aid` (`AidDecisionSource`, Domain C); allocations are idempotent per
//! eligibility id. Applying aid to a charge reduces it through `apply_adjustment` and posts
//! Dr AidExpense · Cr StudentReceivables (`aid_disbursement` / `apply:<charge>`); cash aid posts
//! Dr AidExpense · Cr RefundsPayable (`aid_disbursement` / `disburse`, paid out by 5B).
//! Revocation may raise a published `aid_recovery` charge.

use super::domain::{benefit_for_charge, can_transition};
use crate::charges::kit::{invalid, require_version, status_event, subject_of, StatusEvent};
use crate::shared::account::ensure_student_account;
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::charge::{
    apply_adjustment, charge_lines, insert_charge, lock_charge, NewCharge, NewChargeLine,
};
use crate::shared::lifecycle::{record_history, HistoryEntry, StatusChange};
use crate::shared::money::fmt;
use crate::shared::posting;
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
    AID_ALLOCATED, AID_ALLOCATED_SCHEMA, AID_DISBURSED, AID_DISBURSED_SCHEMA, AID_RENEWED,
    AID_RENEWED_SCHEMA, AID_REVOKED, AID_REVOKED_SCHEMA,
};
use sutra_contracts::finance::ids::{AidAllocationId, ChargeId, FeeHeadId, StudentAccountId};
use sutra_contracts::finance::ports::{
    source, AccountRef, PostingLine, PostingRequest, SystemAccount,
};
use sutra_contracts::finance::status::{AidAllocationStatus, ChargeSourceType, JournalEntryKind};
use sutra_contracts::student::ports::AidDecision;
use sutra_kernel::error::KernelError;
use sutra_kernel::money::CurrencyCode;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use utoipa::ToSchema;
use uuid::Uuid;

/// An aid allocation (amounts are decimal strings, INR).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AidAllocationView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub allocation_number: String,
    pub scholarship_eligibility_id: Uuid,
    pub scholarship_id: Uuid,
    pub scholarship_code: String,
    pub scholarship_name: String,
    pub sponsor_type: String,
    pub student_id: Uuid,
    pub student_account_id: Uuid,
    pub display_name: String,
    pub academic_year_id: Option<Uuid>,
    /// `fixed_amount | fee_percentage | fee_waiver | stipend`.
    pub benefit_type: String,
    pub benefit_value: Option<String>,
    /// Ceiling (fixed / stipend); `null` = bounded by the charges.
    pub allocated_amount: Option<String>,
    pub applied_amount: String,
    pub disbursed_amount: String,
    pub recovered_amount: String,
    /// What is left of the ceiling (`null` when unbounded).
    pub remaining_amount: Option<String>,
    pub currency: String,
    pub valid_from: Option<NaiveDate>,
    pub valid_until: Option<NaiveDate>,
    pub renewal_count: i32,
    pub decision_status: String,
    pub status: AidAllocationStatus,
    pub status_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub disbursements: Vec<AidDisbursementView>,
    pub recovery_charge_id: Option<Uuid>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

/// One utilisation of an allocation.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AidDisbursementView {
    pub id: Uuid,
    /// `charge_adjustment | cash`.
    pub mode: String,
    pub charge_id: Option<Uuid>,
    pub charge_number: Option<String>,
    pub amount: String,
    pub posting_date: NaiveDate,
    pub journal_entry_id: Uuid,
    pub created_at: DateTime<Utc>,
}

/// Internal row (decimal amounts).
#[derive(Debug, Clone)]
pub struct Allocation {
    pub id: AidAllocationId,
    pub institution_id: InstitutionId,
    pub number: String,
    pub eligibility_id: Uuid,
    pub scholarship_code: String,
    pub student_id: Uuid,
    pub student_account_id: StudentAccountId,
    pub benefit_type: String,
    pub benefit_value: Option<Decimal>,
    pub allocated: Option<Decimal>,
    pub applied: Decimal,
    pub disbursed: Decimal,
    pub recovered: Decimal,
    pub renewal_count: i32,
    pub valid_until: Option<NaiveDate>,
    pub status: AidAllocationStatus,
    pub process_instance_id: Option<String>,
    pub version: i64,
}

impl Allocation {
    pub fn remaining(&self) -> Option<Decimal> {
        self.allocated.map(|a| a - self.applied - self.disbursed)
    }
    pub fn utilised(&self) -> Decimal {
        self.applied + self.disbursed
    }
}

fn alloc_from_row(r: &PgRow) -> Result<Allocation, sqlx::Error> {
    let st: String = r.try_get("status")?;
    Ok(Allocation {
        id: AidAllocationId(r.try_get("id")?),
        institution_id: InstitutionId(r.try_get("institution_id")?),
        number: r.try_get("allocation_number")?,
        eligibility_id: r.try_get("scholarship_eligibility_id")?,
        scholarship_code: r.try_get("scholarship_code")?,
        student_id: r.try_get("student_id")?,
        student_account_id: StudentAccountId(r.try_get("student_account_id")?),
        benefit_type: r.try_get("benefit_type")?,
        benefit_value: r.try_get("benefit_value")?,
        allocated: r.try_get("allocated_amount")?,
        applied: r.try_get("applied_amount")?,
        disbursed: r.try_get("disbursed_amount")?,
        recovered: r.try_get("recovered_amount")?,
        renewal_count: r.try_get("renewal_count")?,
        valid_until: r.try_get("valid_until")?,
        status: AidAllocationStatus::parse(&st)
            .ok_or_else(|| sqlx::Error::Decode(st.clone().into()))?,
        process_instance_id: r.try_get("process_instance_id")?,
        version: r.try_get("version")?,
    })
}

async fn lock(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: AidAllocationId,
) -> Result<Allocation, KernelError> {
    let r = sqlx::query(
        "SELECT * FROM finance.aid_allocation WHERE tenant_id = $1 AND id = $2 FOR UPDATE",
    )
    .bind(tenant.as_uuid())
    .bind(id.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "aid allocation",
    })?;
    alloc_from_row(&r).map_err(map_db_error)
}

/// Builds the API view.
pub async fn view(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: AidAllocationId,
) -> Result<AidAllocationView, KernelError> {
    let r = sqlx::query(
        "SELECT al.*, a.display_name, (SELECT recovery_charge_id FROM finance.aid_recovery x \
         WHERE x.aid_allocation_id = al.id) AS recovery_charge_id FROM finance.aid_allocation al \
         JOIN finance.student_account a ON a.id = al.student_account_id \
         WHERE al.tenant_id = $1 AND al.id = $2",
    )
    .bind(tenant.as_uuid())
    .bind(id.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "aid allocation",
    })?;
    let a = alloc_from_row(&r).map_err(map_db_error)?;
    let disbursements = sqlx::query(
        "SELECT d.id, d.mode, d.charge_id, c.charge_number, d.amount, d.posting_date, d.journal_entry_id, d.created_at \
         FROM finance.aid_disbursement d LEFT JOIN finance.charge c ON c.id = d.charge_id \
         WHERE d.tenant_id = $1 AND d.aid_allocation_id = $2 ORDER BY d.created_at, d.id",
    )
    .bind(tenant.as_uuid())
    .bind(id.as_uuid())
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?
    .iter()
    .map(|d| {
        Ok(AidDisbursementView {
            id: d.try_get("id")?,
            mode: d.try_get("mode")?,
            charge_id: d.try_get("charge_id")?,
            charge_number: d.try_get("charge_number")?,
            amount: fmt(d.try_get("amount")?),
            posting_date: d.try_get("posting_date")?,
            journal_entry_id: d.try_get("journal_entry_id")?,
            created_at: d.try_get("created_at")?,
        })
    })
    .collect::<Result<Vec<_>, sqlx::Error>>()
    .map_err(map_db_error)?;
    let g = |c: &str| r.try_get::<String, _>(c).map_err(map_db_error);
    Ok(AidAllocationView {
        id: a.id.as_uuid(),
        institution_id: a.institution_id.as_uuid(),
        allocation_number: a.number.clone(),
        scholarship_eligibility_id: a.eligibility_id,
        scholarship_id: r.try_get("scholarship_id").map_err(map_db_error)?,
        scholarship_code: a.scholarship_code.clone(),
        scholarship_name: g("scholarship_name")?,
        sponsor_type: g("sponsor_type")?,
        student_id: a.student_id,
        student_account_id: a.student_account_id.as_uuid(),
        display_name: g("display_name")?,
        academic_year_id: r.try_get("academic_year_id").map_err(map_db_error)?,
        benefit_type: a.benefit_type.clone(),
        benefit_value: a.benefit_value.map(fmt),
        allocated_amount: a.allocated.map(fmt),
        applied_amount: fmt(a.applied),
        disbursed_amount: fmt(a.disbursed),
        recovered_amount: fmt(a.recovered),
        remaining_amount: a.remaining().map(fmt),
        currency: g("currency")?,
        valid_from: r.try_get("valid_from").map_err(map_db_error)?,
        valid_until: a.valid_until,
        renewal_count: a.renewal_count,
        decision_status: g("decision_status")?,
        status: a.status,
        status_reason: r.try_get("status_reason").map_err(map_db_error)?,
        process_instance_id: a.process_instance_id.clone(),
        disbursements,
        recovery_charge_id: r.try_get("recovery_charge_id").map_err(map_db_error)?,
        version: a.version,
        created_at: r.try_get("created_at").map_err(map_db_error)?,
    })
}

pub async fn get(
    state: &FinanceState,
    tenant: TenantId,
    id: AidAllocationId,
) -> Result<AidAllocationView, KernelError> {
    let mut conn = state.pool.acquire().await.map_err(map_db_error)?;
    view(&mut conn, tenant, id).await
}

/// Allocations of an institution or of party accounts.
pub async fn list(
    state: &FinanceState,
    tenant: TenantId,
    institution: Option<InstitutionId>,
    accounts: Option<&[Uuid]>,
    status: Option<AidAllocationStatus>,
) -> Result<Vec<AidAllocationView>, KernelError> {
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM finance.aid_allocation WHERE tenant_id = $1 \
         AND ($2::uuid IS NULL OR institution_id = $2) AND ($3::uuid[] IS NULL OR student_account_id = ANY($3)) \
         AND ($4::text IS NULL OR status = $4) ORDER BY created_at DESC, id DESC LIMIT 200",
    )
    .bind(tenant.as_uuid())
    .bind(institution.map(|i| i.as_uuid()))
    .bind(accounts)
    .bind(status.map(|s| s.as_str()))
    .fetch_all(&state.pool)
    .await
    .map_err(map_db_error)?;
    let mut conn = state.pool.acquire().await.map_err(map_db_error)?;
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        out.push(view(&mut conn, tenant, AidAllocationId(id)).await?);
    }
    Ok(out)
}

/// The allocation of a decision, if any.
pub async fn allocation_of_decision(
    state: &FinanceState,
    tenant: TenantId,
    eligibility: Uuid,
) -> Result<Option<(Uuid, AidAllocationStatus)>, KernelError> {
    let r: Option<(Uuid, String)> = sqlx::query_as(
        "SELECT id, status FROM finance.aid_allocation WHERE tenant_id = $1 AND scholarship_eligibility_id = $2",
    )
    .bind(tenant.as_uuid())
    .bind(eligibility)
    .fetch_optional(&state.pool)
    .await
    .map_err(map_db_error)?;
    Ok(r.and_then(|(id, s)| AidAllocationStatus::parse(&s).map(|s| (id, s))))
}

#[allow(clippy::too_many_arguments)]
async fn transition(
    conn: &mut PgConnection,
    tenant: TenantId,
    a: &Allocation,
    to: AidAllocationStatus,
    reason: Option<&str>,
    pid: Option<&str>,
    cause: &'static str,
    actor: &Actor,
) -> Result<Allocation, KernelError> {
    if a.status != to && !can_transition(a.status, to) {
        return Err(KernelError::Conflict(format!(
            "aid allocation {} is {}; it cannot become {to}",
            a.number, a.status
        )));
    }
    sqlx::query(
        "UPDATE finance.aid_allocation SET status = $3, status_reason = COALESCE($4, status_reason), \
         process_instance_id = COALESCE($5, process_instance_id), version = version + 1, updated_at = now() \
         WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant.as_uuid())
    .bind(a.id.as_uuid())
    .bind(to.as_str())
    .bind(reason)
    .bind(pid)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    if a.status != to {
        record_history(
            conn,
            HistoryEntry {
                tenant,
                institution: a.institution_id,
                aggregate_type: "aid_allocation",
                aggregate_id: a.id.as_uuid(),
                from: Some(a.status.as_str()),
                to: to.as_str(),
            },
            &StatusChange {
                cause_event_type: cause,
                reason,
                process_instance_id: pid,
            },
            actor,
        )
        .await?;
    }
    lock(conn, tenant, a.id).await
}

#[allow(clippy::too_many_arguments)]
async fn emit(
    conn: &mut PgConnection,
    tenant: TenantId,
    a: &Allocation,
    from: AidAllocationStatus,
    ev: (&'static str, &'static str),
    amount: Option<Decimal>,
    reason: Option<String>,
    journal_entry_id: Option<Uuid>,
    actor: &Actor,
) -> Result<(), KernelError> {
    let scope = ExecutionScope::institution(tenant, a.institution_id);
    let subject = subject_of(conn, tenant, a.student_account_id).await?;
    status_event(
        conn,
        &scope,
        ev,
        StatusEvent {
            aggregate_id: a.id.as_uuid(),
            student_account_id: Some(a.student_account_id.as_uuid()),
            student_id: Some(a.student_id),
            from: Some(from.as_str().into()),
            to: a.status.as_str().into(),
            version: a.version,
            amount,
            reason,
            process_instance_id: a.process_instance_id.clone(),
            journal_entry_id,
            subject,
            ..StatusEvent::default()
        },
        actor,
    )
    .await
}

/// Allocates aid from an approved, handed-off Domain C decision (L3-F-062). Idempotent per
/// eligibility id (a repeated call returns the existing allocation). The allocation is approved
/// through `AID_ALLOCATION` (auto-approval → `allocated` at once; otherwise `proposed` until the
/// approve decision).
pub async fn allocate(
    state: &FinanceState,
    scope: &ExecutionScope,
    institution: InstitutionId,
    eligibility: Uuid,
    actor: &Actor,
) -> Result<(AidAllocationView, bool), KernelError> {
    let tenant = scope.tenant_id;
    if let Some((id, _)) = allocation_of_decision(state, tenant, eligibility).await? {
        return Ok((get(state, tenant, AidAllocationId(id)).await?, false));
    }
    let d: AidDecision = state
        .aid
        .decision(scope, eligibility)
        .await?
        .ok_or_else(|| {
            invalid(
                "eligibilityId",
                "not_found",
                "no scholarship decision was handed off to Financial Aid with this id",
            )
        })?;
    if d.institution_id != institution {
        return Err(invalid(
            "eligibilityId",
            "other_institution",
            "the decision belongs to another institution",
        ));
    }
    if !d.is_live() {
        return Err(KernelError::Conflict(format!(
            "the scholarship decision is {}; only an active decision can be allocated",
            d.status
        )));
    }
    if d.currency != "INR" {
        return Err(invalid("eligibilityId", "unsupported_currency", "only INR"));
    }
    let allocated = match d.benefit_type.as_str() {
        "fixed_amount" | "stipend" => Some(d.benefit_value.ok_or_else(|| {
            KernelError::Conflict("the scholarship has no benefit amount".into())
        })?),
        "fee_percentage" => {
            if d.benefit_value
                .is_none_or(|v| v <= Decimal::ZERO || v > Decimal::ONE_HUNDRED)
            {
                return Err(KernelError::Conflict(
                    "the scholarship percentage is not between 0 and 100".into(),
                ));
            }
            None
        }
        _ => None,
    };
    let student = state
        .students
        .student(scope, d.student_id)
        .await?
        .ok_or_else(|| invalid("eligibilityId", "student_not_found", "unknown student"))?;
    let outcome = request_approval(
        state,
        scope,
        process::AID_ALLOCATION,
        business_key("aid-allocation", eligibility, 1),
        BTreeMap::from([
            ("scholarship".to_owned(), json!(d.scholarship_code)),
            ("benefitType".to_owned(), json!(d.benefit_type)),
        ]),
    )
    .await?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let account = ensure_student_account(&mut tx, tenant, institution, &student, actor).await?;
    let number = crate::shared::numbering::next(
        &mut tx,
        tenant,
        institution,
        crate::shared::numbering::kind::AID_ALLOCATION,
    )
    .await?;
    let id: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO finance.aid_allocation (tenant_id, institution_id, allocation_number, \
         scholarship_eligibility_id, scholarship_id, scholarship_code, scholarship_name, sponsor_type, \
         student_id, student_account_id, academic_year_id, benefit_type, benefit_value, allocated_amount, \
         valid_from, valid_until, renewal_count, decision_status, process_instance_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19) \
         ON CONFLICT (tenant_id, scholarship_eligibility_id) DO NOTHING RETURNING id",
    )
    .bind(tenant.as_uuid())
    .bind(institution.as_uuid())
    .bind(&number)
    .bind(eligibility)
    .bind(d.scholarship_id)
    .bind(&d.scholarship_code)
    .bind(&d.scholarship_name)
    .bind(&d.sponsor_type)
    .bind(d.student_id.as_uuid())
    .bind(account.id().as_uuid())
    .bind(d.academic_year_id)
    .bind(&d.benefit_type)
    .bind(d.benefit_value)
    .bind(allocated)
    .bind(d.valid_from)
    .bind(d.valid_until)
    .bind(d.renewal_count)
    .bind(&d.status)
    .bind(outcome.process_instance_id())
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let Some(id) = id else {
        tx.rollback().await.map_err(map_db_error)?;
        let (id, _) = allocation_of_decision(state, tenant, eligibility)
            .await?
            .ok_or_else(|| KernelError::Conflict("aid allocation changed concurrently".into()))?;
        return Ok((get(state, tenant, AidAllocationId(id)).await?, false));
    };
    let id = AidAllocationId(id);
    audit(
        &mut tx,
        scope,
        "finance.aid_allocation.propose",
        "aid_allocation",
        id.to_string(),
        json!({"number": number, "eligibilityId": eligibility, "scholarship": d.scholarship_code, "benefitType": d.benefit_type, "allocated": allocated.map(fmt)}),
        actor,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        let a = lock(&mut tx, tenant, id).await?;
        approve_locked(
            &mut tx,
            tenant,
            &a,
            &Actor::system(actor.correlation_id.clone()),
        )
        .await?;
    }
    let v = view(&mut tx, tenant, id).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok((v, true))
}

async fn approve_locked(
    conn: &mut PgConnection,
    tenant: TenantId,
    a: &Allocation,
    actor: &Actor,
) -> Result<Allocation, KernelError> {
    let approved = transition(
        conn,
        tenant,
        a,
        AidAllocationStatus::Approved,
        None,
        None,
        AID_ALLOCATED,
        actor,
    )
    .await?;
    let allocated = transition(
        conn,
        tenant,
        &approved,
        AidAllocationStatus::Allocated,
        None,
        None,
        AID_ALLOCATED,
        actor,
    )
    .await?;
    sqlx::query(
        "UPDATE finance.aid_allocation SET decided_by = $3 WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant.as_uuid())
    .bind(a.id.as_uuid())
    .bind(actor.id.as_deref())
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    emit(
        conn,
        tenant,
        &allocated,
        a.status,
        (AID_ALLOCATED, AID_ALLOCATED_SCHEMA),
        allocated.allocated,
        Some(format!(
            "{} ({})",
            a.scholarship_code,
            a.benefit_type.replace('_', " ")
        )),
        None,
        actor,
    )
    .await?;
    Ok(allocated)
}

/// Decision on a proposed allocation (`finance.approve`): approve → allocated; reject → closed.
pub async fn decide(
    state: &FinanceState,
    tenant: TenantId,
    id: AidAllocationId,
    expected: i64,
    approve: bool,
    reason: Option<String>,
    actor: &Actor,
) -> Result<AidAllocationView, KernelError> {
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let a = lock(&mut tx, tenant, id).await?;
    require_version(a.version, expected)?;
    if a.status != AidAllocationStatus::Proposed {
        return Err(KernelError::Conflict(format!(
            "aid allocation {} is {}; there is no pending decision",
            a.number, a.status
        )));
    }
    if approve {
        approve_locked(&mut tx, tenant, &a, actor).await?;
    } else {
        let r = crate::charges::kit::require_text("reason", reason.as_deref())?;
        transition(
            &mut tx,
            tenant,
            &a,
            AidAllocationStatus::Closed,
            Some(&r),
            None,
            AID_ALLOCATED,
            actor,
        )
        .await?;
    }
    let scope = ExecutionScope::institution(tenant, a.institution_id);
    audit(
        &mut tx,
        &scope,
        "finance.aid_allocation.decide",
        "aid_allocation",
        id.to_string(),
        json!({"approve": approve, "reason": reason}),
        actor,
    )
    .await?;
    let v = view(&mut tx, tenant, id).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(v)
}

/// How aid is disbursed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Applied to one of the student's charges (charge adjustment).
    ChargeAdjustment(ChargeId),
    /// Paid out in cash (stipend; 5B pays RefundsPayable out).
    Cash,
}

/// Disburses an allocation (L3-F-063): applies the benefit to a charge or pays it in cash. The
/// Domain C decision must still be live.
pub async fn disburse(
    state: &FinanceState,
    tenant: TenantId,
    id: AidAllocationId,
    expected: i64,
    mode: Mode,
    amount: Option<Decimal>,
    actor: &Actor,
) -> Result<AidAllocationView, KernelError> {
    let current = get(state, tenant, id).await?;
    require_version(current.version, expected)?;
    let institution = InstitutionId(current.institution_id);
    let scope = ExecutionScope::institution(tenant, institution);
    crate::shared::guard::require_institution_writable(state, &scope, institution).await?;
    let decision = state
        .aid
        .decision(&scope, current.scholarship_eligibility_id)
        .await?;
    if !decision.as_ref().is_some_and(AidDecision::is_live) {
        return Err(KernelError::Conflict(
            "the scholarship decision is no longer active; revoke the allocation instead".into(),
        ));
    }
    let today = business_today(state, &scope, institution).await?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let a = lock(&mut tx, tenant, id).await?;
    require_version(a.version, expected)?;
    if !matches!(
        a.status,
        AidAllocationStatus::Allocated | AidAllocationStatus::Disbursed
    ) {
        return Err(KernelError::Conflict(format!(
            "aid allocation {} is {}; only allocated aid is disbursed",
            a.number, a.status
        )));
    }
    let disbursement_id = Uuid::now_v7();
    let account = a.student_account_id;
    let (mode_wire, charge_id, value, lines, key) = match mode {
        Mode::ChargeAdjustment(charge) => {
            let c = lock_charge(&mut tx, tenant, charge).await?;
            if c.student_account_id != account {
                return Err(invalid(
                    "chargeId",
                    "other_party",
                    "the charge belongs to another student account",
                ));
            }
            if !c.status.accepts_adjustment() {
                return Err(KernelError::Conflict(format!(
                    "charge {} is {}; aid cannot be applied to it",
                    c.charge_number, c.status
                )));
            }
            let computed = benefit_for_charge(
                &a.benefit_type,
                a.benefit_value,
                a.remaining(),
                c.total_amount,
                c.outstanding_amount,
            )
            .ok_or_else(|| {
                KernelError::Conflict(
                    "no benefit left to apply to this charge (stipends are disbursed in cash)"
                        .into(),
                )
            })?;
            let value = match amount {
                Some(v) if v > computed => {
                    return Err(invalid(
                        "amount",
                        "exceeds_benefit",
                        "exceeds the benefit available for this charge",
                    ))
                }
                Some(v) => v,
                None => computed,
            };
            apply_adjustment(&mut tx, &c, -value).await?;
            let lines = vec![
                PostingLine::debit(AccountRef::System(SystemAccount::AidExpense), value)
                    .memo(a.number.clone()),
                PostingLine::credit(AccountRef::System(SystemAccount::StudentReceivables), value)
                    .for_charge(account, Some(charge))
                    .memo(a.number.clone()),
            ];
            (
                "charge_adjustment",
                Some(charge),
                value,
                lines,
                format!("apply:{charge}"),
            )
        }
        Mode::Cash => {
            let remaining = a.remaining().ok_or_else(|| {
                KernelError::Conflict(
                    "percentage and waiver aid is applied to charges, not paid in cash".into(),
                )
            })?;
            let value = amount.unwrap_or(remaining);
            if value <= Decimal::ZERO || value > remaining {
                return Err(invalid(
                    "amount",
                    "exceeds_benefit",
                    "exceeds what remains of the allocation",
                ));
            }
            let lines = vec![
                PostingLine::debit(AccountRef::System(SystemAccount::AidExpense), value)
                    .memo(a.number.clone()),
                PostingLine::credit(AccountRef::System(SystemAccount::RefundsPayable), value)
                    .for_charge(account, None)
                    .memo(a.number.clone()),
            ];
            ("cash", None, value, lines, "disburse".to_owned())
        }
    };
    if a.remaining().is_some_and(|r| value > r) {
        return Err(invalid(
            "amount",
            "exceeds_benefit",
            "exceeds what remains of the allocation",
        ));
    }
    let receipt = posting::post(
        &mut tx,
        tenant,
        &PostingRequest {
            institution_id: institution,
            posting_date: today,
            kind: JournalEntryKind::Standard,
            source_type: source::AID_DISBURSEMENT.into(),
            source_id: disbursement_id,
            posting_key: key,
            description: format!("Aid {} ({}) {mode_wire}", a.number, a.scholarship_code),
            currency: CurrencyCode::INR,
            lines,
        },
        actor,
    )
    .await?;
    sqlx::query(
        "INSERT INTO finance.aid_disbursement (id, tenant_id, institution_id, aid_allocation_id, mode, \
         charge_id, amount, posting_date, journal_entry_id, actor) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(disbursement_id)
    .bind(tenant.as_uuid())
    .bind(institution.as_uuid())
    .bind(id.as_uuid())
    .bind(mode_wire)
    .bind(charge_id.map(|c| c.as_uuid()))
    .bind(value)
    .bind(today)
    .bind(receipt.journal_entry_id.as_uuid())
    .bind(actor.id.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let column = if mode_wire == "cash" {
        "disbursed_amount"
    } else {
        "applied_amount"
    };
    sqlx::query(&format!(
        "UPDATE finance.aid_allocation SET {column} = {column} + $3 WHERE tenant_id = $1 AND id = $2"
    ))
    .bind(tenant.as_uuid())
    .bind(id.as_uuid())
    .bind(value)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let a2 = lock(&mut tx, tenant, id).await?;
    let next = transition(
        &mut tx,
        tenant,
        &a2,
        AidAllocationStatus::Disbursed,
        None,
        None,
        AID_DISBURSED,
        actor,
    )
    .await?;
    emit(
        &mut tx,
        tenant,
        &next,
        a.status,
        (AID_DISBURSED, AID_DISBURSED_SCHEMA),
        Some(value),
        Some(mode_wire.to_owned()),
        Some(receipt.journal_entry_id.as_uuid()),
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "finance.aid_allocation.disburse",
        "aid_allocation",
        id.to_string(),
        json!({"mode": mode_wire, "chargeId": charge_id, "amount": fmt(value), "journalEntryId": receipt.journal_entry_id}),
        actor,
    )
    .await?;
    let v = view(&mut tx, tenant, id).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(v)
}

/// Renews an allocation from a continuing Domain C decision (L3-F-064): the decision must be
/// live and renewed (higher renewal count or later validity). Fixed / stipend ceilings grow by
/// one more benefit period.
pub async fn renew(
    state: &FinanceState,
    tenant: TenantId,
    id: AidAllocationId,
    expected: i64,
    actor: &Actor,
) -> Result<AidAllocationView, KernelError> {
    let current = get(state, tenant, id).await?;
    require_version(current.version, expected)?;
    let institution = InstitutionId(current.institution_id);
    let scope = ExecutionScope::institution(tenant, institution);
    let d = state
        .aid
        .decision(&scope, current.scholarship_eligibility_id)
        .await?
        .ok_or_else(|| KernelError::Conflict("the scholarship decision is gone".into()))?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let a = lock(&mut tx, tenant, id).await?;
    require_version(a.version, expected)?;
    if !matches!(
        a.status,
        AidAllocationStatus::Allocated
            | AidAllocationStatus::Disbursed
            | AidAllocationStatus::Suspended
    ) {
        return Err(KernelError::Conflict(format!(
            "aid allocation {} is {}; it cannot be renewed",
            a.number, a.status
        )));
    }
    let renewed = d.renewal_count > a.renewal_count
        || matches!((d.valid_until, a.valid_until), (Some(n), Some(o)) if n > o);
    if !d.is_live() || !renewed {
        return Err(KernelError::Conflict(format!(
            "no renewal decision from Student Records (decision {}, renewal {})",
            d.status, d.renewal_count
        )));
    }
    let grow = match a.benefit_type.as_str() {
        "fixed_amount" | "stipend" => d.benefit_value,
        _ => None,
    };
    sqlx::query(
        "UPDATE finance.aid_allocation SET renewal_count = $3, valid_from = $4, valid_until = $5, \
         academic_year_id = COALESCE($6, academic_year_id), decision_status = $7, \
         allocated_amount = CASE WHEN $8::numeric IS NULL THEN allocated_amount ELSE allocated_amount + $8 END \
         WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant.as_uuid())
    .bind(id.as_uuid())
    .bind(d.renewal_count)
    .bind(d.valid_from)
    .bind(d.valid_until)
    .bind(d.academic_year_id)
    .bind(&d.status)
    .bind(grow)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let a2 = lock(&mut tx, tenant, id).await?;
    let to = if a.status == AidAllocationStatus::Suspended {
        AidAllocationStatus::Allocated
    } else {
        a.status
    };
    let next = transition(
        &mut tx,
        tenant,
        &a2,
        to,
        Some("renewed"),
        None,
        AID_RENEWED,
        actor,
    )
    .await?;
    emit(
        &mut tx,
        tenant,
        &next,
        a.status,
        (AID_RENEWED, AID_RENEWED_SCHEMA),
        grow,
        Some(format!("renewal {}", d.renewal_count)),
        None,
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "finance.aid_allocation.renew",
        "aid_allocation",
        id.to_string(),
        json!({"renewalCount": d.renewal_count, "validUntil": d.valid_until, "added": grow.map(fmt)}),
        actor,
    )
    .await?;
    let v = view(&mut tx, tenant, id).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(v)
}

/// Raises the published recovery charge of a revoked allocation, in the caller's transaction.
#[allow(clippy::too_many_arguments)]
async fn recover_locked(
    conn: &mut PgConnection,
    tenant: TenantId,
    a: &Allocation,
    amount: Option<Decimal>,
    fee_head: Option<FeeHeadId>,
    reason: &str,
    today: NaiveDate,
    actor: &Actor,
) -> Result<Allocation, KernelError> {
    if a.status != AidAllocationStatus::Revoked {
        return Err(KernelError::Conflict(format!(
            "aid allocation {} is {}; only revoked aid is recovered",
            a.number, a.status
        )));
    }
    let open = a.utilised() - a.recovered;
    let value = amount.unwrap_or(open);
    if value <= Decimal::ZERO || value > open {
        return Err(invalid(
            "amount",
            "invalid",
            "must be positive and at most the utilised amount not yet recovered",
        ));
    }
    let head = match fee_head {
        Some(h) => h,
        None => {
            let first_charge: Option<Uuid> = sqlx::query_scalar(
                "SELECT charge_id FROM finance.aid_disbursement WHERE tenant_id = $1 AND aid_allocation_id = $2 \
                 AND charge_id IS NOT NULL ORDER BY created_at LIMIT 1",
            )
            .bind(tenant.as_uuid())
            .bind(a.id.as_uuid())
            .fetch_optional(&mut *conn)
            .await
            .map_err(map_db_error)?;
            let line = match first_charge {
                Some(c) => charge_lines(conn, tenant, ChargeId(c))
                    .await?
                    .into_iter()
                    .next(),
                None => None,
            };
            line.map(|l| l.fee_head_id).ok_or_else(|| {
                invalid(
                    "feeHeadId",
                    "required",
                    "choose the fee head of the recovery charge",
                )
            })?
        }
    };
    let c = insert_charge(
        conn,
        &NewCharge {
            tenant,
            institution: a.institution_id,
            student_account_id: a.student_account_id,
            student_id: Some(a.student_id),
            admission_id: None,
            academic_year_id: None,
            academic_term_id: None,
            description: format!(
                "Recovery of financial aid {} ({})",
                a.number, a.scholarship_code
            ),
            due_date: Some(today + chrono::Duration::days(30)),
            source_type: ChargeSourceType::AidRecovery,
            source_id: Some(a.id.as_uuid()),
            lines: vec![NewChargeLine {
                fee_head_id: head,
                description: format!("Aid recovery — {reason}"),
                amount: value,
            }],
        },
    )
    .await?;
    let c = lock_charge(conn, tenant, c.id).await?;
    let c = crate::charges::repo::validate_locked(conn, &c, actor).await?;
    let (c, _) = crate::charges::repo::publish_locked(conn, &c, today, None, actor).await?;
    sqlx::query(
        "INSERT INTO finance.aid_recovery (tenant_id, institution_id, aid_allocation_id, amount, recovery_charge_id, reason, actor) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(tenant.as_uuid())
    .bind(a.institution_id.as_uuid())
    .bind(a.id.as_uuid())
    .bind(value)
    .bind(c.id.as_uuid())
    .bind(reason)
    .bind(actor.id.as_deref())
    .execute(&mut *conn)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(d) if d.is_unique_violation() => {
            KernelError::Conflict("a recovery was already raised for this allocation".into())
        }
        _ => map_db_error(e),
    })?;
    sqlx::query(
        "UPDATE finance.aid_allocation SET recovered_amount = recovered_amount + $3 WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant.as_uuid())
    .bind(a.id.as_uuid())
    .bind(value)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let a2 = lock(conn, tenant, a.id).await?;
    transition(
        conn,
        tenant,
        &a2,
        AidAllocationStatus::Recovered,
        Some(reason),
        None,
        AID_REVOKED,
        actor,
    )
    .await
}

/// Revokes an allocation after a breach or a Domain C reversal (L3-F-065; `finance.approve`,
/// the decision itself — `AID_REVOCATION` is started for evidence). With `recover`, the utilised
/// aid is recovered at once through a published `aid_recovery` charge.
#[allow(clippy::too_many_arguments)]
pub async fn revoke(
    state: &FinanceState,
    tenant: TenantId,
    id: AidAllocationId,
    expected: i64,
    reason: &str,
    recover: bool,
    fee_head: Option<FeeHeadId>,
    actor: &Actor,
) -> Result<AidAllocationView, KernelError> {
    let current = get(state, tenant, id).await?;
    require_version(current.version, expected)?;
    let institution = InstitutionId(current.institution_id);
    let scope = ExecutionScope::institution(tenant, institution);
    crate::shared::guard::require_institution_writable(state, &scope, institution).await?;
    let today = business_today(state, &scope, institution).await?;
    let outcome = request_approval(
        state,
        &scope,
        process::AID_REVOCATION,
        business_key("aid-revocation", id, current.version),
        BTreeMap::new(),
    )
    .await?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let a = lock(&mut tx, tenant, id).await?;
    require_version(a.version, expected)?;
    let revoked = transition(
        &mut tx,
        tenant,
        &a,
        AidAllocationStatus::Revoked,
        Some(reason),
        Some(outcome.process_instance_id()),
        AID_REVOKED,
        actor,
    )
    .await?;
    sqlx::query(
        "UPDATE finance.aid_allocation SET revoked_at = now() WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant.as_uuid())
    .bind(id.as_uuid())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let open = revoked.utilised() - revoked.recovered;
    let after = if recover && open > Decimal::ZERO {
        recover_locked(
            &mut tx, tenant, &revoked, None, fee_head, reason, today, actor,
        )
        .await?
    } else {
        revoked
    };
    emit(
        &mut tx,
        tenant,
        &after,
        a.status,
        (AID_REVOKED, AID_REVOKED_SCHEMA),
        Some(open).filter(|o| *o > Decimal::ZERO),
        Some(reason.to_owned()),
        None,
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "finance.aid_allocation.revoke",
        "aid_allocation",
        id.to_string(),
        json!({"reason": reason, "recover": recover, "utilised": fmt(open)}),
        actor,
    )
    .await?;
    let v = view(&mut tx, tenant, id).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(v)
}

/// Recovers the utilised aid of a revoked allocation through a published recovery charge.
#[allow(clippy::too_many_arguments)]
pub async fn recover(
    state: &FinanceState,
    tenant: TenantId,
    id: AidAllocationId,
    expected: i64,
    amount: Option<Decimal>,
    fee_head: Option<FeeHeadId>,
    reason: &str,
    actor: &Actor,
) -> Result<AidAllocationView, KernelError> {
    let current = get(state, tenant, id).await?;
    require_version(current.version, expected)?;
    let institution = InstitutionId(current.institution_id);
    let scope = ExecutionScope::institution(tenant, institution);
    crate::shared::guard::require_institution_writable(state, &scope, institution).await?;
    let today = business_today(state, &scope, institution).await?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let a = lock(&mut tx, tenant, id).await?;
    require_version(a.version, expected)?;
    recover_locked(&mut tx, tenant, &a, amount, fee_head, reason, today, actor).await?;
    audit(
        &mut tx,
        &scope,
        "finance.aid_allocation.recover",
        "aid_allocation",
        id.to_string(),
        json!({"amount": amount.map(fmt), "reason": reason}),
        actor,
    )
    .await?;
    let v = view(&mut tx, tenant, id).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(v)
}

// ================================================================ programme reconciliation (F-066)

/// A programme-level reconciliation run (new fact each time).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AidReconciliationView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub scholarship_id: Option<Uuid>,
    pub academic_year_id: Option<Uuid>,
    pub allocation_count: i32,
    pub allocated_total: String,
    pub utilised_total: String,
    pub recovered_total: String,
    /// Aid expense posted in the ledger for these allocations.
    pub ledger_total: String,
    /// `ledgerTotal − utilisedTotal`.
    pub variance: String,
    /// `matched | exceptions`.
    pub status: String,
    pub exceptions: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

fn recon_from_row(r: &PgRow) -> Result<AidReconciliationView, sqlx::Error> {
    Ok(AidReconciliationView {
        id: r.try_get("id")?,
        institution_id: r.try_get("institution_id")?,
        scholarship_id: r.try_get("scholarship_id")?,
        academic_year_id: r.try_get("academic_year_id")?,
        allocation_count: r.try_get("allocation_count")?,
        allocated_total: fmt(r.try_get("allocated_total")?),
        utilised_total: fmt(r.try_get("utilised_total")?),
        recovered_total: fmt(r.try_get("recovered_total")?),
        ledger_total: fmt(r.try_get("ledger_total")?),
        variance: fmt(r.try_get("variance")?),
        status: r.try_get("status")?,
        exceptions: r.try_get("exceptions")?,
        created_at: r.try_get("created_at")?,
    })
}

pub async fn reconciliations(
    state: &FinanceState,
    tenant: TenantId,
    institution: InstitutionId,
) -> Result<Vec<AidReconciliationView>, KernelError> {
    sqlx::query(
        "SELECT * FROM finance.aid_programme_reconciliation WHERE tenant_id = $1 AND institution_id = $2 \
         ORDER BY created_at DESC LIMIT 50",
    )
    .bind(tenant.as_uuid())
    .bind(institution.as_uuid())
    .fetch_all(&state.pool)
    .await
    .map_err(map_db_error)?
    .iter()
    .map(recon_from_row)
    .collect::<Result<_, _>>()
    .map_err(map_db_error)
}

/// Reconciles funding (allocations) and utilisation (disbursements) of a programme with the
/// ledger (L3-F-066): each run is recorded as a new fact; variances become exceptions.
pub async fn reconcile(
    state: &FinanceState,
    scope: &ExecutionScope,
    institution: InstitutionId,
    scholarship: Option<Uuid>,
    academic_year: Option<Uuid>,
    actor: &Actor,
) -> Result<AidReconciliationView, KernelError> {
    let tenant = scope.tenant_id;
    let rows = sqlx::query(
        "SELECT al.*, COALESCE((SELECT sum(d.amount) FROM finance.aid_disbursement d \
           WHERE d.aid_allocation_id = al.id), 0) AS disbursement_total, \
         COALESCE((SELECT sum(l.debit - l.credit) FROM finance.journal_line l \
           JOIN finance.journal_entry e ON e.id = l.journal_entry_id \
           JOIN finance.ledger_account acc ON acc.id = l.ledger_account_id \
           JOIN finance.aid_disbursement d ON d.id = e.source_id \
           WHERE e.tenant_id = al.tenant_id AND e.source_type = 'aid_disbursement' \
             AND d.aid_allocation_id = al.id AND acc.system_key = 'aid_expense'), 0) AS ledger_total \
         FROM finance.aid_allocation al WHERE al.tenant_id = $1 AND al.institution_id = $2 \
         AND ($3::uuid IS NULL OR al.scholarship_id = $3) AND ($4::uuid IS NULL OR al.academic_year_id = $4) \
         ORDER BY al.allocation_number",
    )
    .bind(tenant.as_uuid())
    .bind(institution.as_uuid())
    .bind(scholarship)
    .bind(academic_year)
    .fetch_all(&state.pool)
    .await
    .map_err(map_db_error)?;
    let mut allocated_total = Decimal::ZERO;
    let mut utilised_total = Decimal::ZERO;
    let mut recovered_total = Decimal::ZERO;
    let mut ledger_total = Decimal::ZERO;
    let mut exceptions = Vec::new();
    for r in &rows {
        let a = alloc_from_row(r).map_err(map_db_error)?;
        let disbursements: Decimal = r.try_get("disbursement_total").map_err(map_db_error)?;
        let ledger: Decimal = r.try_get("ledger_total").map_err(map_db_error)?;
        allocated_total += a.allocated.unwrap_or(a.utilised());
        utilised_total += a.utilised();
        recovered_total += a.recovered;
        ledger_total += ledger;
        if disbursements != a.utilised() {
            exceptions.push(
                json!({"allocation": a.number, "issue": "utilisation_mismatch",
                "recorded": fmt(a.utilised()), "disbursements": fmt(disbursements)}),
            );
        }
        if ledger != disbursements {
            exceptions.push(json!({"allocation": a.number, "issue": "ledger_mismatch",
                "ledger": fmt(ledger), "disbursements": fmt(disbursements)}));
        }
        if a.status == AidAllocationStatus::Revoked && a.utilised() > a.recovered {
            exceptions.push(
                json!({"allocation": a.number, "issue": "unrecovered_revocation",
                "open": fmt(a.utilised() - a.recovered)}),
            );
        }
        if matches!(
            a.status,
            AidAllocationStatus::Allocated | AidAllocationStatus::Disbursed
        ) {
            let live = state
                .aid
                .decision(scope, a.eligibility_id)
                .await?
                .is_some_and(|d| d.is_live());
            if !live {
                exceptions.push(json!({"allocation": a.number, "issue": "decision_not_active"}));
            }
        }
    }
    let variance = ledger_total - utilised_total;
    let status = if exceptions.is_empty() && variance.is_zero() {
        "matched"
    } else {
        "exceptions"
    };
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let r = sqlx::query(
        "INSERT INTO finance.aid_programme_reconciliation (tenant_id, institution_id, scholarship_id, \
         academic_year_id, allocation_count, allocated_total, utilised_total, recovered_total, ledger_total, \
         variance, exceptions, status, actor) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) RETURNING *",
    )
    .bind(tenant.as_uuid())
    .bind(institution.as_uuid())
    .bind(scholarship)
    .bind(academic_year)
    .bind(i32::try_from(rows.len()).unwrap_or(i32::MAX))
    .bind(allocated_total)
    .bind(utilised_total)
    .bind(recovered_total)
    .bind(ledger_total)
    .bind(variance)
    .bind(serde_json::Value::Array(exceptions))
    .bind(status)
    .bind(actor.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let v = recon_from_row(&r).map_err(map_db_error)?;
    audit(
        &mut tx,
        scope,
        "finance.aid_programme_reconciliation.run",
        "aid_programme_reconciliation",
        v.id.to_string(),
        json!({"status": status, "variance": v.variance, "allocations": v.allocation_count}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(v)
}
