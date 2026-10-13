//! Leave requests → final decision (L3-E-045, `LEAVE_APPROVAL` — the reference approval of
//! Batch 7), reservation on approval, consumption as the leave dates pass (one ledger entry per
//! calendar-month segment, L3-E-047), cancellation (release / reversal, L3-E-048), the staff
//! self-service (`/me/leave-requests`) and the reporting manager's approvals
//! (`/me/leave-approvals`, 404 for anyone who is not the applicant's manager).

use super::common::{
    audit, clean, emit_status, ensure_version, expected, history, illegal, invalid, limit,
    parse_date, subject_of, EventParties, LeaveBulkFailure, LeaveBulkSummary, LeaveDecisionBody,
    LeaveReasonBody, Transition,
};
use super::domain::{split_days, SplitError, WorkCalendar, BLOCKING_REQUEST_STATUSES};
use super::policy::{load_policy, load_type, require_active_policy, LeaveTypeRow, LeaveTypeView};
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::attendance::{record_day, DayRecord};
use crate::shared::decimal::fmt2;
use crate::shared::guard::{
    require_institution, require_institution_writable, require_live_engagement,
};
use crate::shared::http::{actor, with_etag};
use crate::shared::leave::{balance, post_entry, reverse_entry, NewLeaveEntry};
use crate::shared::me::{my_engagement, my_staff};
use crate::shared::numbering::{self, kind};
use crate::shared::rows::{current_engagement, lock_staff};
use crate::shared::scope::{institution_scope, tenant_scope};
use crate::shared::time::business_today;
use crate::shared::{Actor, PeopleState};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use std::collections::BTreeMap;
use sutra_contracts::people::events::leave::{
    LEAVE_REQUEST_APPROVED, LEAVE_REQUEST_APPROVED_SCHEMA, LEAVE_REQUEST_CANCELLED,
    LEAVE_REQUEST_CANCELLED_SCHEMA, LEAVE_REQUEST_CONSUMED, LEAVE_REQUEST_CONSUMED_SCHEMA,
    LEAVE_REQUEST_REJECTED, LEAVE_REQUEST_REJECTED_SCHEMA, LEAVE_REQUEST_SUBMITTED,
    LEAVE_REQUEST_SUBMITTED_SCHEMA,
};
use sutra_contracts::people::ids::{LeaveLedgerEntryId, StaffEngagementId, StaffMemberId};
use sutra_contracts::people::ports::{EngagementSummary, StaffSummary};
use sutra_contracts::people::status::{
    AttendanceDayStatus, AttendanceSource, LeaveEntryKind, LeaveRequestStatus,
};
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// `source_type` of every ledger entry a leave request posts.
pub const SOURCE: &str = "leave_request";

// ------------------------------------------------------------------ rows

/// A stored leave request (+ applicant facts).
#[derive(Clone, Debug)]
pub struct RequestRow {
    pub id: Uuid,
    pub institution_id: InstitutionId,
    pub number: String,
    pub staff_id: StaffMemberId,
    pub staff_number: String,
    pub display_name: String,
    pub subject_ref: Option<String>,
    pub engagement_id: StaffEngagementId,
    pub policy_id: Uuid,
    pub leave_type_id: Uuid,
    pub leave_type_code: String,
    pub is_paid: bool,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    pub half_day_start: bool,
    pub half_day_end: bool,
    pub days: Decimal,
    pub leave_year: i32,
    pub reason: Option<String>,
    pub status: LeaveRequestStatus,
    pub status_reason: Option<String>,
    pub approver_staff_id: Option<StaffMemberId>,
    pub decided_by: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
    pub process_instance_id: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

const REQUEST_SELECT: &str = "SELECT r.id, r.institution_id, r.number, r.staff_id, \
    s.staff_number, s.display_name, s.subject_ref, r.engagement_id, r.policy_id, r.leave_type_id, \
    r.leave_type_code, r.is_paid, r.start_date, r.end_date, r.half_day_start, r.half_day_end, \
    r.days, r.leave_year, r.reason, r.status, r.status_reason, r.approver_staff_id, r.decided_by, \
    r.decided_at, r.process_instance_id, r.version, r.created_at \
    FROM people.leave_request r JOIN people.staff_member s ON s.id = r.staff_id";

fn request_from_row(r: &PgRow) -> Result<RequestRow, sqlx::Error> {
    Ok(RequestRow {
        id: r.try_get("id")?,
        institution_id: InstitutionId(r.try_get("institution_id")?),
        number: r.try_get("number")?,
        staff_id: StaffMemberId(r.try_get("staff_id")?),
        staff_number: r.try_get("staff_number")?,
        display_name: r.try_get("display_name")?,
        subject_ref: r.try_get("subject_ref")?,
        engagement_id: StaffEngagementId(r.try_get("engagement_id")?),
        policy_id: r.try_get("policy_id")?,
        leave_type_id: r.try_get("leave_type_id")?,
        leave_type_code: r.try_get("leave_type_code")?,
        is_paid: r.try_get("is_paid")?,
        start_date: r.try_get("start_date")?,
        end_date: r.try_get("end_date")?,
        half_day_start: r.try_get("half_day_start")?,
        half_day_end: r.try_get("half_day_end")?,
        days: r.try_get("days")?,
        leave_year: r.try_get("leave_year")?,
        reason: r.try_get("reason")?,
        status: crate::shared::rows::decode(r.try_get("status")?, LeaveRequestStatus::parse)?,
        status_reason: r.try_get("status_reason")?,
        approver_staff_id: r
            .try_get::<Option<Uuid>, _>("approver_staff_id")?
            .map(StaffMemberId),
        decided_by: r.try_get("decided_by")?,
        decided_at: r.try_get("decided_at")?,
        process_instance_id: r.try_get("process_instance_id")?,
        version: r.try_get("version")?,
        created_at: r.try_get("created_at")?,
    })
}

/// Loads a request (404 when missing / foreign); `lock` takes `FOR UPDATE OF r`.
pub async fn load_request(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<RequestRow, KernelError> {
    let sql = format!(
        "{REQUEST_SELECT} WHERE r.id = $1 AND r.tenant_id = $2{}",
        if lock { " FOR UPDATE OF r" } else { "" }
    );
    let r = sqlx::query(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "leave request",
        })?;
    request_from_row(&r).map_err(map_db_error)
}

/// One calendar-month segment of a request.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LeaveSegmentView {
    /// `YYYY-MM`.
    #[schema(example = "2026-07")]
    pub month: String,
    pub first_day: NaiveDate,
    pub last_day: NaiveDate,
    #[schema(example = "2.50")]
    pub days: String,
    /// The `consume:<YYYY-MM>` ledger entry, once consumed.
    pub consumed_entry_id: Option<Uuid>,
}

/// A stored month segment.
struct SegmentRow {
    id: Uuid,
    month: NaiveDate,
    first_day: NaiveDate,
    last_day: NaiveDate,
    days: Decimal,
    consumed_entry_id: Option<Uuid>,
}

async fn segments_of(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
) -> Result<Vec<SegmentRow>, KernelError> {
    let rows = sqlx::query(
        "SELECT id, month, first_day, last_day, days, consumed_entry_id \
         FROM people.leave_request_segment WHERE leave_request_id = $1 AND tenant_id = $2 \
         ORDER BY month",
    )
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    rows.iter()
        .map(|r| {
            Ok(SegmentRow {
                id: r.try_get("id")?,
                month: r.try_get("month")?,
                first_day: r.try_get("first_day")?,
                last_day: r.try_get("last_day")?,
                days: r.try_get("days")?,
                consumed_entry_id: r.try_get("consumed_entry_id")?,
            })
        })
        .collect::<Result<Vec<_>, sqlx::Error>>()
        .map_err(map_db_error)
}

// ------------------------------------------------------------------ views

/// A leave request (contract §5.3).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LeaveRequestView {
    pub id: Uuid,
    pub institution_id: Uuid,
    #[schema(example = "LV-000001")]
    pub number: String,
    pub staff_id: StaffMemberId,
    pub staff_number: String,
    pub display_name: String,
    pub engagement_id: StaffEngagementId,
    pub leave_type_id: Uuid,
    #[schema(example = "CL")]
    pub leave_type_code: String,
    pub is_paid: bool,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    pub half_day_start: bool,
    pub half_day_end: bool,
    #[schema(example = "2.50")]
    pub days: String,
    pub leave_year: i32,
    pub reason: Option<String>,
    pub status: LeaveRequestStatus,
    pub status_reason: Option<String>,
    /// Reporting manager the request was routed to.
    pub approver_staff_id: Option<StaffMemberId>,
    pub decided_by: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
    pub process_instance_id: Option<String>,
    /// Calendar-month segments (filled on single reads and after commands).
    pub segments: Vec<LeaveSegmentView>,
    pub created_at: DateTime<Utc>,
    pub version: i64,
}

impl LeaveRequestView {
    fn of(r: &RequestRow, segments: Vec<LeaveSegmentView>) -> Self {
        Self {
            id: r.id,
            institution_id: r.institution_id.as_uuid(),
            number: r.number.clone(),
            staff_id: r.staff_id,
            staff_number: r.staff_number.clone(),
            display_name: r.display_name.clone(),
            engagement_id: r.engagement_id,
            leave_type_id: r.leave_type_id,
            leave_type_code: r.leave_type_code.clone(),
            is_paid: r.is_paid,
            start_date: r.start_date,
            end_date: r.end_date,
            half_day_start: r.half_day_start,
            half_day_end: r.half_day_end,
            days: fmt2(r.days),
            leave_year: r.leave_year,
            reason: r.reason.clone(),
            status: r.status,
            status_reason: r.status_reason.clone(),
            approver_staff_id: r.approver_staff_id,
            decided_by: r.decided_by.clone(),
            decided_at: r.decided_at,
            process_instance_id: r.process_instance_id.clone(),
            segments,
            created_at: r.created_at,
            version: r.version,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LeaveRequestList {
    pub items: Vec<LeaveRequestView>,
    /// Request number to continue after (`?cursor=`), when more rows exist.
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MyLeaveTypeList {
    pub items: Vec<LeaveTypeView>,
}

/// New leave request (submitted at once: validation, then `LEAVE_APPROVAL`).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LeaveRequestCreate {
    /// HR on-behalf requests only (`POST /institutions/{id}/leave-requests`); ignored on `/me`.
    pub staff_id: Option<Uuid>,
    pub leave_type_id: Uuid,
    /// `YYYY-MM-DD`.
    pub start_date: String,
    /// `YYYY-MM-DD` (inclusive).
    pub end_date: String,
    pub half_day_start: Option<bool>,
    pub half_day_end: Option<bool>,
    pub reason: Option<String>,
}

/// Filters of the leave request lists.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct LeaveRequestQuery {
    /// `LeaveRequestStatus` wire name.
    pub status: Option<String>,
    pub staff_id: Option<Uuid>,
    /// ≤ 200 (default 50).
    pub limit: Option<i64>,
    /// Request number to continue after.
    pub cursor: Option<String>,
}

async fn view(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
) -> Result<LeaveRequestView, KernelError> {
    let r = load_request(conn, tenant, id, false).await?;
    let segs = segments_of(conn, tenant, id)
        .await?
        .into_iter()
        .map(|s| LeaveSegmentView {
            month: s.month.format("%Y-%m").to_string(),
            first_day: s.first_day,
            last_day: s.last_day,
            days: fmt2(s.days),
            consumed_entry_id: s.consumed_entry_id,
        })
        .collect();
    Ok(LeaveRequestView::of(&r, segs))
}

async fn respond(
    st: &PeopleState,
    tenant: TenantId,
    id: Uuid,
    status: StatusCode,
) -> Result<Response, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let v = view(&mut conn, tenant, id).await?;
    Ok(with_etag(status, &v, Version(v.version)))
}

// ------------------------------------------------------------------ rules

fn split_error(e: SplitError) -> KernelError {
    invalid("endDate", e.code(), e.message())
}

/// Overlap (409) and balance (409) rules inside the caller's transaction.
#[allow(clippy::too_many_arguments)]
async fn check_request(
    conn: &mut PgConnection,
    tenant: TenantId,
    staff: StaffMemberId,
    lt: &LeaveTypeRow,
    leave_year: i32,
    start: NaiveDate,
    end: NaiveDate,
    days: Decimal,
) -> Result<(), KernelError> {
    let statuses = BLOCKING_REQUEST_STATUSES
        .iter()
        .map(|s| format!("'{s}'"))
        .collect::<Vec<_>>()
        .join(", ");
    let clash: Option<String> = sqlx::query_scalar(&format!(
        "SELECT number FROM people.leave_request WHERE tenant_id = $1 AND staff_id = $2 \
         AND status IN ({statuses}) AND start_date <= $4 AND end_date >= $3 LIMIT 1"
    ))
    .bind(tenant.as_uuid())
    .bind(staff.as_uuid())
    .bind(start)
    .bind(end)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    if let Some(n) = clash {
        return Err(KernelError::Conflict(format!(
            "the period overlaps leave request {n}"
        )));
    }
    if lt.requires_balance {
        let b = balance(conn, tenant, staff, lt.id, leave_year).await?;
        if b.available < days {
            return Err(KernelError::Conflict(format!(
                "insufficient {} balance: {} days available in {leave_year}, {} requested",
                lt.code,
                fmt2(b.available),
                fmt2(days)
            )));
        }
    }
    Ok(())
}

/// Validates and submits a leave request for `staff` / `e`; returns the new request id.
async fn apply(
    st: &PeopleState,
    ctx: &AuthContext,
    staff: &StaffSummary,
    e: &EngagementSummary,
    b: LeaveRequestCreate,
    a: &Actor,
) -> Result<Uuid, KernelError> {
    let t = ctx.scope.tenant_id;
    let inst = staff.institution_id;
    let scope = institution_scope(ctx, inst);
    require_institution_writable(st, &scope, inst).await?;
    require_live_engagement(e)?;
    if !e.status.is_working() {
        return Err(KernelError::Conflict(format!(
            "engagement {} is {}; leave is requested on a working engagement",
            e.engagement_number, e.status
        )));
    }
    let start = parse_date("startDate", &b.start_date)?;
    let end = parse_date("endDate", &b.end_date)?;
    let (half_s, half_e) = (
        b.half_day_start.unwrap_or(false),
        b.half_day_end.unwrap_or(false),
    );
    let (policy, lt, manager_subject) = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        let policy = require_active_policy(&mut conn, t, inst).await?;
        let lt = load_type(&mut conn, t, b.leave_type_id)
            .await
            .map_err(|_| invalid("leaveTypeId", "unknown", "unknown leave type"))?;
        let manager = e.reports_to_staff_id.filter(|m| *m != staff.id);
        let subject = subject_of(&mut conn, t, manager).await?;
        (policy, lt, subject)
    };
    if lt.policy_id != policy.id {
        return Err(invalid(
            "leaveTypeId",
            "not_in_active_policy",
            "the leave type does not belong to the active leave policy",
        ));
    }
    if !lt.applies_to(e.staff_category) {
        return Err(invalid(
            "leaveTypeId",
            "not_eligible",
            &format!(
                "{} leave does not apply to {} staff",
                lt.code, e.staff_category
            ),
        ));
    }
    if (half_s || half_e) && !lt.half_day_allowed {
        return Err(invalid(
            "halfDayStart",
            "half_day_not_allowed",
            "this leave type is taken in full days",
        ));
    }
    let cal = WorkCalendar {
        weekly_off_days: &policy.weekly_off_days,
        holidays: &policy.holidays,
    };
    let split = split_days(&cal, start, end, half_s, half_e).map_err(split_error)?;
    if start < e.start_date {
        return Err(invalid(
            "startDate",
            "before_engagement",
            "the leave starts before the engagement",
        ));
    }
    if let Some(last) = e.last_working_date.or(e.end_date) {
        if end > last {
            return Err(invalid(
                "endDate",
                "after_engagement",
                "the leave ends after the engagement",
            ));
        }
    }
    let leave_year = start.year();
    let reason = clean(b.reason);
    // Fail fast before starting the approval process (re-checked under the staff lock below).
    {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        check_request(
            &mut conn, t, staff.id, &lt, leave_year, start, end, split.days,
        )
        .await?;
    }
    let id = Uuid::now_v7();
    let approver = e.reports_to_staff_id.filter(|m| *m != staff.id);
    let outcome = request_approval(
        st,
        &scope,
        process::LEAVE_APPROVAL,
        business_key(SOURCE, id, 1),
        BTreeMap::from([
            ("staffId".to_owned(), serde_json::json!(staff.id)),
            ("leaveTypeCode".to_owned(), serde_json::json!(lt.code)),
            ("days".to_owned(), serde_json::json!(fmt2(split.days))),
            ("approverStaffId".to_owned(), serde_json::json!(approver)),
        ]),
    )
    .await?;
    let pid = outcome.process_instance_id().to_owned();
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    lock_staff(&mut tx, t, staff.id).await?;
    check_request(
        &mut tx, t, staff.id, &lt, leave_year, start, end, split.days,
    )
    .await?;
    let number = numbering::next(&mut tx, t, inst, kind::LEAVE_REQUEST).await?;
    sqlx::query(
        "INSERT INTO people.leave_request (id, tenant_id, institution_id, number, staff_id, \
         engagement_id, policy_id, leave_type_id, leave_type_code, is_paid, start_date, end_date, \
         half_day_start, half_day_end, days, leave_year, reason, status, approver_staff_id, \
         process_instance_id, requested_by) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, \
         $12, $13, $14, $15, $16, $17, 'pending_approval', $18, $19, $20)",
    )
    .bind(id)
    .bind(t.as_uuid())
    .bind(inst.as_uuid())
    .bind(&number)
    .bind(staff.id.as_uuid())
    .bind(e.id.as_uuid())
    .bind(policy.id)
    .bind(lt.id)
    .bind(&lt.code)
    .bind(lt.is_paid)
    .bind(start)
    .bind(end)
    .bind(half_s)
    .bind(half_e)
    .bind(split.days)
    .bind(leave_year)
    .bind(reason.as_deref())
    .bind(approver.map(|m| m.as_uuid()))
    .bind(&pid)
    .bind(a.id.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    for s in &split.segments {
        sqlx::query(
            "INSERT INTO people.leave_request_segment (tenant_id, leave_request_id, month, \
             first_day, last_day, days) VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(t.as_uuid())
        .bind(id)
        .bind(s.month)
        .bind(s.first_day)
        .bind(s.last_day)
        .bind(s.days)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    }
    for (from, to) in [
        (None, "submitted"),
        (Some("submitted"), "validated"),
        (Some("validated"), "pending_approval"),
    ] {
        history(
            &mut tx,
            &Transition {
                tenant: t,
                institution: inst,
                aggregate_type: SOURCE,
                aggregate_id: id,
                from,
                to,
                reason: reason.as_deref(),
                process_instance_id: Some(&pid),
                cause: LEAVE_REQUEST_SUBMITTED,
            },
            a,
        )
        .await?;
    }
    emit_status(
        &mut tx,
        &scope,
        LEAVE_REQUEST_SUBMITTED,
        LEAVE_REQUEST_SUBMITTED_SCHEMA,
        &Transition {
            tenant: t,
            institution: inst,
            aggregate_type: SOURCE,
            aggregate_id: id,
            from: Some("validated"),
            to: "pending_approval",
            reason: Some(
                format!(
                    "{number}: {} {} days from {start}",
                    lt.code,
                    fmt2(split.days)
                )
                .as_str(),
            ),
            process_instance_id: Some(&pid),
            cause: LEAVE_REQUEST_SUBMITTED,
        },
        1,
        &EventParties {
            staff_id: Some(staff.id.as_uuid()),
            engagement_id: Some(e.id.as_uuid()),
            staff_subject_ref: staff.subject_ref.clone(),
            manager_subject_ref: manager_subject,
        },
        a,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "people.leave_request.submit",
        SOURCE,
        id,
        serde_json::json!({"number": number, "days": fmt2(split.days), "leaveTypeCode": lt.code,
            "startDate": start, "endDate": end, "approverStaffId": approver}),
        a,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        let row = load_request(&mut tx, t, id, true).await?;
        decide_in_tx(
            &mut tx,
            &scope,
            &row,
            true,
            Some("auto-approved"),
            &Actor::system(a.correlation_id.clone()),
        )
        .await?;
    }
    tx.commit().await.map_err(map_db_error)?;
    Ok(id)
}

fn parties(r: &RequestRow) -> EventParties {
    EventParties {
        staff_id: Some(r.staff_id.as_uuid()),
        engagement_id: Some(r.engagement_id.as_uuid()),
        staff_subject_ref: r.subject_ref.clone(),
        manager_subject_ref: None,
    }
}

fn entry<'a>(
    r: &'a RequestRow,
    tenant: TenantId,
    kind: LeaveEntryKind,
    days: Decimal,
    effective_on: NaiveDate,
    posting_key: &'a str,
    reason: &'a str,
) -> NewLeaveEntry<'a> {
    NewLeaveEntry {
        tenant,
        institution: r.institution_id,
        staff_id: r.staff_id,
        engagement_id: Some(r.engagement_id),
        leave_type_id: r.leave_type_id,
        leave_type_code: &r.leave_type_code,
        is_paid: r.is_paid,
        kind,
        days,
        effective_on,
        leave_year: effective_on.year(),
        source_type: SOURCE,
        source_id: r.id,
        posting_key,
        reason: Some(reason),
    }
}

/// Final decision of a pending request inside the caller's transaction (row locked):
/// approve → balance re-check, `reservation` entry (`reserve`), status `reserved`
/// (`leave_request.approved`); reject → `rejected` (`leave_request.rejected`).
async fn decide_in_tx(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    r: &RequestRow,
    approve: bool,
    reason: Option<&str>,
    a: &Actor,
) -> Result<(), KernelError> {
    let t = scope.tenant_id;
    let to = if approve {
        LeaveRequestStatus::Approved
    } else {
        LeaveRequestStatus::Rejected
    };
    if r.status != LeaveRequestStatus::PendingApproval {
        return Err(illegal(
            &format!("leave request {}", r.number),
            r.status.as_str(),
            to.as_str(),
        ));
    }
    let (final_status, event, schema) = if approve {
        let lt = load_type(conn, t, r.leave_type_id).await?;
        if lt.requires_balance {
            let b = balance(conn, t, r.staff_id, r.leave_type_id, r.leave_year).await?;
            if b.available < r.days {
                return Err(KernelError::Conflict(format!(
                    "insufficient {} balance: {} days available, {} requested",
                    r.leave_type_code,
                    fmt2(b.available),
                    fmt2(r.days)
                )));
            }
        }
        post_entry(
            conn,
            &entry(
                r,
                t,
                LeaveEntryKind::Reservation,
                -r.days,
                r.start_date,
                "reserve",
                "approved leave reserved",
            ),
            a,
        )
        .await?;
        (
            LeaveRequestStatus::Reserved,
            LEAVE_REQUEST_APPROVED,
            LEAVE_REQUEST_APPROVED_SCHEMA,
        )
    } else {
        (
            LeaveRequestStatus::Rejected,
            LEAVE_REQUEST_REJECTED,
            LEAVE_REQUEST_REJECTED_SCHEMA,
        )
    };
    let version: i64 = sqlx::query_scalar(
        "UPDATE people.leave_request SET status = $3, status_reason = $4, decided_by = $5, \
         decided_at = now(), version = version + 1, updated_at = now() \
         WHERE id = $1 AND tenant_id = $2 RETURNING version",
    )
    .bind(r.id)
    .bind(t.as_uuid())
    .bind(final_status.as_str())
    .bind(reason)
    .bind(a.id.as_deref())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let base = Transition {
        tenant: t,
        institution: r.institution_id,
        aggregate_type: SOURCE,
        aggregate_id: r.id,
        from: Some("pending_approval"),
        to: to.as_str(),
        reason,
        process_instance_id: r.process_instance_id.as_deref(),
        cause: event,
    };
    history(conn, &base, a).await?;
    if approve {
        history(
            conn,
            &Transition {
                from: Some("approved"),
                to: "reserved",
                ..base_clone(&base)
            },
            a,
        )
        .await?;
    }
    emit_status(conn, scope, event, schema, &base, version, &parties(r), a).await?;
    audit(
        conn,
        scope,
        "people.leave_request.decide",
        SOURCE,
        r.id,
        serde_json::json!({"decision": if approve { "approve" } else { "reject" }, "reason": reason}),
        a,
    )
    .await
}

fn base_clone<'a>(t: &Transition<'a>) -> Transition<'a> {
    Transition {
        tenant: t.tenant,
        institution: t.institution,
        aggregate_type: t.aggregate_type,
        aggregate_id: t.aggregate_id,
        from: t.from,
        to: t.to,
        reason: t.reason,
        process_instance_id: t.process_instance_id,
        cause: t.cause,
    }
}

/// Consumption once the leave has started (L3-E-047), inside the caller's transaction (row
/// locked): one `consumption` entry per month segment (`consume:<YYYY-MM>`, effective on the
/// first leave day of the month), the reservation released (`release`), the leave days marked
/// `on_leave`, status `consumed` (`leave_request.consumed`).
async fn consume_in_tx(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    r: &RequestRow,
    today: NaiveDate,
    a: &Actor,
) -> Result<(), KernelError> {
    let t = scope.tenant_id;
    if r.status != LeaveRequestStatus::Reserved {
        return Err(illegal(
            &format!("leave request {}", r.number),
            r.status.as_str(),
            "consumed",
        ));
    }
    if r.start_date > today {
        return Err(KernelError::Conflict(format!(
            "leave request {} starts on {}; leave is consumed once it has started",
            r.number, r.start_date
        )));
    }
    let policy = load_policy(conn, t, r.policy_id, false).await?;
    let cal = WorkCalendar {
        weekly_off_days: &policy.weekly_off_days,
        holidays: &policy.holidays,
    };
    let split = split_days(
        &cal,
        r.start_date,
        r.end_date,
        r.half_day_start,
        r.half_day_end,
    )
    .map_err(|e| KernelError::Conflict(e.message().to_owned()))?;
    let segments = segments_of(conn, t, r.id).await?;
    for seg in &segments {
        let key = format!("consume:{}", seg.month.format("%Y-%m"));
        let receipt = post_entry(
            conn,
            &entry(
                r,
                t,
                LeaveEntryKind::Consumption,
                -seg.days,
                seg.first_day,
                &key,
                "leave consumed",
            ),
            a,
        )
        .await?;
        sqlx::query("UPDATE people.leave_request_segment SET consumed_entry_id = $2 WHERE id = $1")
            .bind(seg.id)
            .bind(receipt.entry_id.as_uuid())
            .execute(&mut *conn)
            .await
            .map_err(map_db_error)?;
    }
    post_entry(
        conn,
        &entry(
            r,
            t,
            LeaveEntryKind::Release,
            r.days,
            r.start_date,
            "release",
            "reservation released on consumption",
        ),
        a,
    )
    .await?;
    for d in &split.working_days {
        let half = (r.half_day_start && *d == r.start_date) || (r.half_day_end && *d == r.end_date);
        record_day(
            conn,
            &DayRecord {
                tenant: t,
                institution: r.institution_id,
                staff_id: r.staff_id,
                work_date: *d,
                status: AttendanceDayStatus::OnLeave,
                source: AttendanceSource::Leave,
                leave_request_id: Some(r.id),
                remark: Some(if half {
                    "half day leave"
                } else {
                    "on approved leave"
                }),
            },
            a,
        )
        .await?;
    }
    let version: i64 = sqlx::query_scalar(
        "UPDATE people.leave_request SET status = 'consumed', version = version + 1, \
         updated_at = now() WHERE id = $1 AND tenant_id = $2 RETURNING version",
    )
    .bind(r.id)
    .bind(t.as_uuid())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let tr = Transition {
        tenant: t,
        institution: r.institution_id,
        aggregate_type: SOURCE,
        aggregate_id: r.id,
        from: Some("reserved"),
        to: "consumed",
        reason: None,
        process_instance_id: r.process_instance_id.as_deref(),
        cause: LEAVE_REQUEST_CONSUMED,
    };
    history(conn, &tr, a).await?;
    emit_status(
        conn,
        scope,
        LEAVE_REQUEST_CONSUMED,
        LEAVE_REQUEST_CONSUMED_SCHEMA,
        &tr,
        version,
        &parties(r),
        a,
    )
    .await?;
    audit(
        conn,
        scope,
        "people.leave_request.consume",
        SOURCE,
        r.id,
        serde_json::json!({"days": fmt2(r.days), "segments": segments.len()}),
        a,
    )
    .await
}

/// Cancellation (L3-E-048) inside the caller's transaction (row locked): pending → cancelled;
/// approved / reserved → reservation released; consumed → every consumption entry reversed.
async fn cancel_in_tx(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    r: &RequestRow,
    reason: Option<&str>,
    today: NaiveDate,
    a: &Actor,
) -> Result<(), KernelError> {
    let t = scope.tenant_id;
    if !super::domain::request_can_transition(r.status, LeaveRequestStatus::Cancelled) {
        return Err(illegal(
            &format!("leave request {}", r.number),
            r.status.as_str(),
            "cancelled",
        ));
    }
    let why = reason.unwrap_or("leave cancelled");
    let mut reversed = 0usize;
    match r.status {
        LeaveRequestStatus::Approved | LeaveRequestStatus::Reserved => {
            post_entry(
                conn,
                &entry(
                    r,
                    t,
                    LeaveEntryKind::Release,
                    r.days,
                    r.start_date,
                    "release",
                    "reservation released on cancellation",
                ),
                a,
            )
            .await?;
        }
        LeaveRequestStatus::Consumed => {
            let ids: Vec<Uuid> = sqlx::query_scalar(
                "SELECT id FROM people.leave_ledger_entry WHERE tenant_id = $1 \
                 AND source_type = $2 AND source_id = $3 AND kind = 'consumption' ORDER BY effective_on",
            )
            .bind(t.as_uuid())
            .bind(SOURCE)
            .bind(r.id)
            .fetch_all(&mut *conn)
            .await
            .map_err(map_db_error)?;
            for id in ids {
                reverse_entry(
                    conn,
                    t,
                    LeaveLedgerEntryId(id),
                    &format!("reverse:{id}"),
                    today,
                    why,
                    a,
                )
                .await?;
                reversed += 1;
            }
        }
        _ => {}
    }
    let version: i64 = sqlx::query_scalar(
        "UPDATE people.leave_request SET status = 'cancelled', status_reason = $3, \
         version = version + 1, updated_at = now() WHERE id = $1 AND tenant_id = $2 \
         RETURNING version",
    )
    .bind(r.id)
    .bind(t.as_uuid())
    .bind(why)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let tr = Transition {
        tenant: t,
        institution: r.institution_id,
        aggregate_type: SOURCE,
        aggregate_id: r.id,
        from: Some(r.status.as_str()),
        to: "cancelled",
        reason: Some(why),
        process_instance_id: r.process_instance_id.as_deref(),
        cause: LEAVE_REQUEST_CANCELLED,
    };
    history(conn, &tr, a).await?;
    emit_status(
        conn,
        scope,
        LEAVE_REQUEST_CANCELLED,
        LEAVE_REQUEST_CANCELLED_SCHEMA,
        &tr,
        version,
        &parties(r),
        a,
    )
    .await?;
    audit(
        conn,
        scope,
        "people.leave_request.cancel",
        SOURCE,
        r.id,
        serde_json::json!({"fromStatus": r.status, "reversedEntries": reversed, "reason": why}),
        a,
    )
    .await
}

/// Integration (7A → 7B): when an engagement closes, every open request of it that starts after
/// the last working date is cancelled in the caller's transaction (reservations released, the
/// applicant notified through `leave_request.cancelled`). Returns how many were cancelled.
pub async fn cancel_after_exit(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    engagement: StaffEngagementId,
    last_working_date: NaiveDate,
    a: &Actor,
) -> Result<usize, KernelError> {
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM people.leave_request WHERE tenant_id = $1 AND engagement_id = $2 \
         AND start_date > $3 AND status IN ('draft', 'pending_approval', 'approved', 'reserved') \
         ORDER BY start_date, id",
    )
    .bind(scope.tenant_id.as_uuid())
    .bind(engagement.as_uuid())
    .bind(last_working_date)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    for id in &ids {
        let r = load_request(conn, scope.tenant_id, *id, true).await?;
        cancel_in_tx(
            conn,
            scope,
            &r,
            Some("engagement closed before the leave starts"),
            last_working_date,
            a,
        )
        .await?;
    }
    Ok(ids.len())
}

async fn list_requests(
    st: &PeopleState,
    tenant: TenantId,
    institution: Option<InstitutionId>,
    q: &LeaveRequestQuery,
    staff: Option<StaffMemberId>,
) -> Result<LeaveRequestList, KernelError> {
    let status = match q.status.as_deref() {
        None => None,
        Some(s) => Some(
            LeaveRequestStatus::parse(s)
                .ok_or_else(|| invalid("status", "invalid", "unknown leave request status"))?,
        ),
    };
    let lim = limit(q.limit);
    let staff_filter = staff.map(|s| s.as_uuid()).or(q.staff_id);
    let sql = format!(
        "{REQUEST_SELECT} WHERE r.tenant_id = $1 AND ($2::uuid IS NULL OR r.institution_id = $2) \
         AND ($3::text IS NULL OR r.status = $3) AND ($4::uuid IS NULL OR r.staff_id = $4) \
         AND ($5::text IS NULL OR r.number < $5) ORDER BY r.number DESC LIMIT $6"
    );
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let rows = sqlx::query(&sql)
        .bind(tenant.as_uuid())
        .bind(institution.map(|i| i.as_uuid()))
        .bind(status.map(|s| s.as_str()))
        .bind(staff_filter)
        .bind(q.cursor.as_deref())
        .bind(lim + 1)
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db_error)?;
    let mut items = rows
        .iter()
        .map(request_from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)?;
    let next_cursor = if i64::try_from(items.len()).unwrap_or(i64::MAX) > lim {
        items.truncate(usize::try_from(lim).unwrap_or(50));
        items.last().map(|r| r.number.clone())
    } else {
        None
    };
    Ok(LeaveRequestList {
        items: items
            .iter()
            .map(|r| LeaveRequestView::of(r, Vec::new()))
            .collect(),
        next_cursor,
    })
}

// ------------------------------------------------------------------ HR handlers

/// Leave requests of an institution (`people.read`), newest number first.
#[utoipa::path(get, path = "/api/v1/people/institutions/{id}/leave-requests", operation_id = "people_leave_request_list", tag = "people-leave",
    params(("id" = String, Path, format = Uuid), LeaveRequestQuery), security(("session_cookie" = [])),
    responses((status = 200, body = LeaveRequestList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn request_list(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<LeaveRequestQuery>,
) -> ApiResult<Json<LeaveRequestList>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let institution = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, institution), institution).await?;
    Ok(Json(
        list_requests(&st, ctx.scope.tenant_id, Some(institution), &q, None).await?,
    ))
}

/// Request leave on behalf of a staff member (`people.leave.manage`; `staffId` required).
#[utoipa::path(post, path = "/api/v1/people/institutions/{id}/leave-requests", operation_id = "people_leave_request_create", tag = "people-leave",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = LeaveRequestCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = LeaveRequestView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn request_create(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LeaveRequestCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::LeaveManage)?;
    require_idempotency_key(&headers)?;
    let staff_id = b
        .staff_id
        .ok_or_else(|| invalid("staffId", "required", "the staff member is required"))?;
    let scope = tenant_scope(ctx);
    let staff = st
        .directory
        .staff(&scope, StaffMemberId(staff_id))
        .await?
        .filter(|s| s.institution_id.as_uuid() == id)
        .ok_or(KernelError::NotFound {
            entity: "staff member",
        })?;
    let e = st
        .directory
        .current_engagement(&scope, staff.id)
        .await?
        .ok_or(KernelError::NotFound {
            entity: "engagement",
        })?;
    let rid = apply(&st, ctx, &staff, &e, b, &actor(ctx)).await?;
    Ok(respond(&st, ctx.scope.tenant_id, rid, StatusCode::CREATED).await?)
}

/// One leave request with its month segments (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/leave-requests/{id}", operation_id = "people_leave_request_get", tag = "people-leave",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = LeaveRequestView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn request_get(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleRead)?;
    Ok(respond(&st, ctx.scope.tenant_id, id, StatusCode::OK).await?)
}

/// Decide a pending request on behalf of any manager (`people.leave.decide`; nobody decides
/// their own request).
#[utoipa::path(post, path = "/api/v1/people/leave-requests/{id}/decision", operation_id = "people_leave_request_decide", tag = "people-leave",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the request")),
    request_body = LeaveDecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = LeaveRequestView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn request_decide(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LeaveDecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::LeaveDecide)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let own = st
        .directory
        .staff_by_subject(&tenant_scope(ctx), &ctx.subject)
        .await?;
    let reason = clean(b.reason.clone());
    if !b.approve() && reason.is_none() {
        return Err(invalid("reason", "required", "a rejection needs a reason").into());
    }
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let r = load_request(&mut tx, t, id, true).await?;
    if own.is_some_and(|s| s.id == r.staff_id) {
        return Err(KernelError::Forbidden("nobody decides their own leave request".into()).into());
    }
    ensure_version(v, r.version)?;
    let scope = institution_scope(ctx, r.institution_id);
    decide_in_tx(&mut tx, &scope, &r, b.approve(), reason.as_deref(), &a).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(respond(&st, t, id, StatusCode::OK).await?)
}

/// Cancel a request (`people.leave.manage`): pending → cancelled, reserved → released,
/// consumed → consumption reversed.
#[utoipa::path(post, path = "/api/v1/people/leave-requests/{id}/cancel", operation_id = "people_leave_request_cancel", tag = "people-leave",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the request")),
    request_body = LeaveReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = LeaveRequestView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn request_cancel(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LeaveReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::LeaveManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let reason = clean(b.reason);
    let today = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        let r = load_request(&mut conn, t, id, false).await?;
        business_today(
            &st,
            &institution_scope(ctx, r.institution_id),
            r.institution_id,
        )
        .await?
    };
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let r = load_request(&mut tx, t, id, true).await?;
    ensure_version(v, r.version)?;
    let scope = institution_scope(ctx, r.institution_id);
    cancel_in_tx(&mut tx, &scope, &r, reason.as_deref(), today, &a).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(respond(&st, t, id, StatusCode::OK).await?)
}

/// Consume a reserved request whose leave has started (`people.leave.manage`).
#[utoipa::path(post, path = "/api/v1/people/leave-requests/{id}/consume", operation_id = "people_leave_request_consume", tag = "people-leave",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the request")),
    request_body = LeaveReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = LeaveRequestView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn request_consume(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(_b): Json<LeaveReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::LeaveManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let today = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        let r = load_request(&mut conn, t, id, false).await?;
        business_today(
            &st,
            &institution_scope(ctx, r.institution_id),
            r.institution_id,
        )
        .await?
    };
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let r = load_request(&mut tx, t, id, true).await?;
    ensure_version(v, r.version)?;
    let scope = institution_scope(ctx, r.institution_id);
    consume_in_tx(&mut tx, &scope, &r, today, &a).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(respond(&st, t, id, StatusCode::OK).await?)
}

/// Idempotent consumption sweep (`people.leave.manage`): every reserved request of the
/// institution whose leave has started is consumed; not yet started ones are skipped.
#[utoipa::path(post, path = "/api/v1/people/institutions/{id}/leave-consumption", operation_id = "people_leave_consumption_sweep", tag = "people-leave",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = LeaveBulkSummary), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn consumption_sweep(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Json<LeaveBulkSummary>> {
    let ctx = auth.require(Permission::LeaveManage)?;
    require_idempotency_key(&headers)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let today = business_today(&st, &scope, institution).await?;
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let rows: Vec<(Uuid, NaiveDate)> = sqlx::query_as(
        "SELECT id, start_date FROM people.leave_request WHERE tenant_id = $1 \
         AND institution_id = $2 AND status = 'reserved' ORDER BY start_date, number",
    )
    .bind(t.as_uuid())
    .bind(id)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let mut summary = LeaveBulkSummary::default();
    for (rid, start) in rows {
        if start > today {
            summary.skipped += 1;
            continue;
        }
        let res: Result<(), KernelError> = async {
            let mut tx = st.pool.begin().await.map_err(map_db_error)?;
            let r = load_request(&mut tx, t, rid, true).await?;
            consume_in_tx(&mut tx, &scope, &r, today, &a).await?;
            tx.commit().await.map_err(map_db_error)
        }
        .await;
        match res {
            Ok(()) => summary.processed += 1,
            Err(e) => summary.failed.push(LeaveBulkFailure {
                id: rid,
                reason: e.to_string(),
            }),
        }
    }
    Ok(Json(summary))
}

// ------------------------------------------------------------------ self-service

/// Request leave for myself (`people.self`; my own live engagement, never a client staff id).
#[utoipa::path(post, path = "/api/v1/people/me/leave-requests", operation_id = "people_leave_self_request_create", tag = "people-leave",
    params(("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = LeaveRequestCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = LeaveRequestView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn my_request_create(
    State(st): State<PeopleState>,
    auth: Authenticated,
    headers: HeaderMap,
    Json(b): Json<LeaveRequestCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleSelfService)?;
    require_idempotency_key(&headers)?;
    let (staff, e) = my_engagement(&st, ctx).await?;
    let rid = apply(&st, ctx, &staff, &e, b, &actor(ctx)).await?;
    Ok(respond(&st, ctx.scope.tenant_id, rid, StatusCode::CREATED).await?)
}

/// My leave requests (`people.self`).
#[utoipa::path(get, path = "/api/v1/people/me/leave-requests", operation_id = "people_leave_self_request_list", tag = "people-leave",
    params(LeaveRequestQuery), security(("session_cookie" = [])),
    responses((status = 200, body = LeaveRequestList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn my_request_list(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Query(q): Query<LeaveRequestQuery>,
) -> ApiResult<Json<LeaveRequestList>> {
    let ctx = auth.require(Permission::PeopleSelfService)?;
    let me = my_staff(&st, ctx).await?;
    Ok(Json(
        list_requests(&st, ctx.scope.tenant_id, None, &q, Some(me.id)).await?,
    ))
}

/// Cancel my own request before it is consumed (`people.self`; consumed leave is cancelled by HR).
#[utoipa::path(post, path = "/api/v1/people/me/leave-requests/{id}/cancel", operation_id = "people_leave_self_request_cancel", tag = "people-leave",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the request")),
    request_body = LeaveReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = LeaveRequestView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn my_request_cancel(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LeaveReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleSelfService)?;
    let v = expected!(headers);
    let me = my_staff(&st, ctx).await?;
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let reason = clean(b.reason);
    let today = business_today(
        &st,
        &institution_scope(ctx, me.institution_id),
        me.institution_id,
    )
    .await?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let r = load_request(&mut tx, t, id, true).await?;
    if r.staff_id != me.id {
        return Err(KernelError::NotFound {
            entity: "leave request",
        }
        .into());
    }
    ensure_version(v, r.version)?;
    if r.status == LeaveRequestStatus::Consumed {
        return Err(KernelError::Conflict(
            "consumed leave is cancelled by HR (the consumption is reversed)".into(),
        )
        .into());
    }
    let scope = institution_scope(ctx, r.institution_id);
    cancel_in_tx(&mut tx, &scope, &r, reason.as_deref(), today, &a).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(respond(&st, t, id, StatusCode::OK).await?)
}

/// Leave types of my institution's active policy that apply to me (`people.self`).
#[utoipa::path(get, path = "/api/v1/people/me/leave-types", operation_id = "people_leave_self_types", tag = "people-leave",
    security(("session_cookie" = [])),
    responses((status = 200, body = MyLeaveTypeList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn my_leave_types(
    State(st): State<PeopleState>,
    auth: Authenticated,
) -> ApiResult<Json<MyLeaveTypeList>> {
    let ctx = auth.require(Permission::PeopleSelfService)?;
    let (staff, e) = my_engagement(&st, ctx).await?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let Some(p) = super::policy::active_policy(&mut conn, t, staff.institution_id).await? else {
        return Ok(Json(MyLeaveTypeList { items: vec![] }));
    };
    let types = super::policy::types_of(&mut conn, t, p.id).await?;
    Ok(Json(MyLeaveTypeList {
        items: types
            .iter()
            .filter(|x| x.applies_to(e.staff_category))
            .map(LeaveTypeView::from)
            .collect(),
    }))
}

/// Pending requests of the staff who report to me (`people.self`, reporting manager).
#[utoipa::path(get, path = "/api/v1/people/me/leave-approvals", operation_id = "people_leave_self_approval_list", tag = "people-leave",
    security(("session_cookie" = [])),
    responses((status = 200, body = LeaveRequestList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn my_approval_list(
    State(st): State<PeopleState>,
    auth: Authenticated,
) -> ApiResult<Json<LeaveRequestList>> {
    let ctx = auth.require(Permission::PeopleSelfService)?;
    let me = my_staff(&st, ctx).await?;
    let sql = format!(
        "{REQUEST_SELECT} JOIN people.staff_engagement e ON e.staff_id = r.staff_id \
         AND e.status NOT IN ('closed', 'cancelled') WHERE r.tenant_id = $1 \
         AND r.status = 'pending_approval' AND e.reports_to_staff_id = $2 AND r.staff_id <> $2 \
         ORDER BY r.start_date, r.number LIMIT 200"
    );
    let rows = sqlx::query(&sql)
        .bind(ctx.scope.tenant_id.as_uuid())
        .bind(me.id.as_uuid())
        .fetch_all(&st.pool)
        .await
        .map_err(map_db_error)?;
    let items = rows
        .iter()
        .map(|r| request_from_row(r).map(|x| LeaveRequestView::of(&x, Vec::new())))
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)?;
    Ok(Json(LeaveRequestList {
        items,
        next_cursor: None,
    }))
}

/// Decide a pending request of someone who reports to me (`people.self`; 404 unless the
/// applicant's current engagement names me as reporting manager).
#[utoipa::path(post, path = "/api/v1/people/me/leave-approvals/{id}/decision", operation_id = "people_leave_self_approval_decide", tag = "people-leave",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the request")),
    request_body = LeaveDecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = LeaveRequestView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn my_approval_decide(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LeaveDecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleSelfService)?;
    let v = expected!(headers);
    let me = my_staff(&st, ctx).await?;
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let reason = clean(b.reason.clone());
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let r = load_request(&mut tx, t, id, true).await?;
    let manager = current_engagement(&mut tx, t, r.staff_id)
        .await?
        .and_then(|e| e.reports_to_staff_id);
    if r.staff_id == me.id || manager != Some(me.id) {
        return Err(KernelError::NotFound {
            entity: "leave request",
        }
        .into());
    }
    ensure_version(v, r.version)?;
    if !b.approve() && reason.is_none() {
        return Err(invalid("reason", "required", "a rejection needs a reason").into());
    }
    let scope = institution_scope(ctx, r.institution_id);
    decide_in_tx(&mut tx, &scope, &r, b.approve(), reason.as_deref(), &a).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(respond(&st, t, id, StatusCode::OK).await?)
}

pub fn router(state: PeopleState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(request_list, request_create))
        .routes(routes!(request_get))
        .routes(routes!(request_decide))
        .routes(routes!(request_cancel))
        .routes(routes!(request_consume))
        .routes(routes!(consumption_sweep))
        .routes(routes!(my_request_create, my_request_list))
        .routes(routes!(my_request_cancel))
        .routes(routes!(my_leave_types))
        .routes(routes!(my_approval_list))
        .routes(routes!(my_approval_decide))
        .with_state(state)
}
