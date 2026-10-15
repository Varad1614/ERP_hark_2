//! Leave policy versions and leave types (L3-E-044): draft → under_review
//! (`LEAVE_POLICY_APPROVAL`) → approved → active (one per institution; the previous active
//! version becomes superseded) → superseded.

use super::common::{
    audit, emit_status, ensure_version, expected, history, illegal, invalid, limit, non_negative,
    parse_date, required, EventParties, LeaveDecisionBody, LeaveReasonBody, Transition,
};
use super::domain::{policy_can_transition, validate_policy, AccrualFrequency, TypeRule};
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::decimal::fmt2;
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, with_etag};
use crate::shared::scope::institution_scope;
use crate::shared::time::business_today;
use crate::shared::{Actor, PeopleState};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use std::collections::BTreeMap;
use sutra_contracts::people::events::leave::{
    LEAVE_POLICY_ACTIVATED, LEAVE_POLICY_ACTIVATED_SCHEMA,
};
use sutra_contracts::people::status::{LeavePolicyStatus, StaffCategory};
use sutra_kernel::auth::Permission;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

// ------------------------------------------------------------------ rows

/// A stored leave policy version.
#[derive(Clone, Debug)]
pub struct PolicyRow {
    pub id: Uuid,
    pub institution_id: InstitutionId,
    pub name: String,
    pub version_no: i32,
    pub effective_from: NaiveDate,
    pub weekly_off_days: Vec<i16>,
    pub holidays: Vec<NaiveDate>,
    pub status: LeavePolicyStatus,
    pub status_reason: Option<String>,
    pub validation: serde_json::Value,
    pub supersedes_policy_id: Option<Uuid>,
    pub process_instance_id: Option<String>,
    pub version: i64,
}

pub const POLICY_COLUMNS: &str = "id, institution_id, name, version_no, effective_from, \
    weekly_off_days, holidays, status, status_reason, validation, supersedes_policy_id, \
    process_instance_id, version";

fn decode<T>(raw: String, parse: fn(&str) -> Option<T>) -> Result<T, sqlx::Error> {
    crate::shared::rows::decode(raw, parse)
}

pub fn policy_from_row(r: &PgRow) -> Result<PolicyRow, sqlx::Error> {
    Ok(PolicyRow {
        id: r.try_get("id")?,
        institution_id: InstitutionId(r.try_get("institution_id")?),
        name: r.try_get("name")?,
        version_no: r.try_get("version_no")?,
        effective_from: r.try_get("effective_from")?,
        weekly_off_days: r.try_get("weekly_off_days")?,
        holidays: r.try_get("holidays")?,
        status: decode(r.try_get("status")?, LeavePolicyStatus::parse)?,
        status_reason: r.try_get("status_reason")?,
        validation: r.try_get("validation")?,
        supersedes_policy_id: r.try_get("supersedes_policy_id")?,
        process_instance_id: r.try_get("process_instance_id")?,
        version: r.try_get("version")?,
    })
}

/// A stored leave type.
#[derive(Clone, Debug)]
pub struct LeaveTypeRow {
    pub id: Uuid,
    pub policy_id: Uuid,
    pub institution_id: InstitutionId,
    pub code: String,
    pub name: String,
    pub annual_entitlement: Decimal,
    pub accrual_frequency: AccrualFrequency,
    pub carry_forward_cap: Decimal,
    pub encashment_cap: Decimal,
    pub is_paid: bool,
    pub requires_balance: bool,
    pub half_day_allowed: bool,
    pub applicable_categories: Vec<String>,
}

impl LeaveTypeRow {
    /// Whether a staff category may take this leave type (eligibility, L4-E-045-BH-002).
    pub fn applies_to(&self, c: StaffCategory) -> bool {
        self.applicable_categories.iter().any(|x| x == c.as_str())
    }
}

pub const TYPE_COLUMNS: &str = "id, policy_id, institution_id, code, name, annual_entitlement, \
    accrual_frequency, carry_forward_cap, encashment_cap, is_paid, requires_balance, \
    half_day_allowed, applicable_categories";

pub fn type_from_row(r: &PgRow) -> Result<LeaveTypeRow, sqlx::Error> {
    Ok(LeaveTypeRow {
        id: r.try_get("id")?,
        policy_id: r.try_get("policy_id")?,
        institution_id: InstitutionId(r.try_get("institution_id")?),
        code: r.try_get("code")?,
        name: r.try_get("name")?,
        annual_entitlement: r.try_get("annual_entitlement")?,
        accrual_frequency: decode(r.try_get("accrual_frequency")?, AccrualFrequency::parse)?,
        carry_forward_cap: r.try_get("carry_forward_cap")?,
        encashment_cap: r.try_get("encashment_cap")?,
        is_paid: r.try_get("is_paid")?,
        requires_balance: r.try_get("requires_balance")?,
        half_day_allowed: r.try_get("half_day_allowed")?,
        applicable_categories: r.try_get("applicable_categories")?,
    })
}

/// Loads a policy (404 when missing / foreign); `lock` takes `FOR UPDATE`.
pub async fn load_policy(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<PolicyRow, KernelError> {
    let sql = format!(
        "SELECT {POLICY_COLUMNS} FROM people.leave_policy WHERE id = $1 AND tenant_id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    let r = sqlx::query(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "leave policy",
        })?;
    policy_from_row(&r).map_err(map_db_error)
}

/// The active policy of an institution, if any.
pub async fn active_policy(
    conn: &mut PgConnection,
    tenant: TenantId,
    institution: InstitutionId,
) -> Result<Option<PolicyRow>, KernelError> {
    let sql = format!(
        "SELECT {POLICY_COLUMNS} FROM people.leave_policy WHERE tenant_id = $1 \
         AND institution_id = $2 AND status = 'active'"
    );
    sqlx::query(&sql)
        .bind(tenant.as_uuid())
        .bind(institution.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .as_ref()
        .map(policy_from_row)
        .transpose()
        .map_err(map_db_error)
}

/// The active policy (409 when the institution has none).
pub async fn require_active_policy(
    conn: &mut PgConnection,
    tenant: TenantId,
    institution: InstitutionId,
) -> Result<PolicyRow, KernelError> {
    active_policy(conn, tenant, institution)
        .await?
        .ok_or_else(|| KernelError::Conflict("the institution has no active leave policy".into()))
}

/// Leave types of a policy, by code.
pub async fn types_of(
    conn: &mut PgConnection,
    tenant: TenantId,
    policy: Uuid,
) -> Result<Vec<LeaveTypeRow>, KernelError> {
    let sql = format!(
        "SELECT {TYPE_COLUMNS} FROM people.leave_type WHERE policy_id = $1 AND tenant_id = $2 \
         ORDER BY code"
    );
    let rows = sqlx::query(&sql)
        .bind(policy)
        .bind(tenant.as_uuid())
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db_error)?;
    rows.iter()
        .map(type_from_row)
        .collect::<Result<_, _>>()
        .map_err(map_db_error)
}

/// One leave type (404 when missing / foreign).
pub async fn load_type(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
) -> Result<LeaveTypeRow, KernelError> {
    let sql =
        format!("SELECT {TYPE_COLUMNS} FROM people.leave_type WHERE id = $1 AND tenant_id = $2");
    let r = sqlx::query(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "leave type",
        })?;
    type_from_row(&r).map_err(map_db_error)
}

// ------------------------------------------------------------------ views

/// A leave type of a policy version.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LeaveTypeView {
    pub id: Uuid,
    pub policy_id: Uuid,
    #[schema(example = "CL")]
    pub code: String,
    pub name: String,
    #[schema(example = "12.00")]
    pub annual_entitlement: String,
    pub accrual_frequency: AccrualFrequency,
    #[schema(example = "5.00")]
    pub carry_forward_cap: String,
    #[schema(example = "0.00")]
    pub encashment_cap: String,
    pub is_paid: bool,
    pub requires_balance: bool,
    pub half_day_allowed: bool,
    pub applicable_categories: Vec<String>,
}

impl From<&LeaveTypeRow> for LeaveTypeView {
    fn from(t: &LeaveTypeRow) -> Self {
        Self {
            id: t.id,
            policy_id: t.policy_id,
            code: t.code.clone(),
            name: t.name.clone(),
            annual_entitlement: fmt2(t.annual_entitlement),
            accrual_frequency: t.accrual_frequency,
            carry_forward_cap: fmt2(t.carry_forward_cap),
            encashment_cap: fmt2(t.encashment_cap),
            is_paid: t.is_paid,
            requires_balance: t.requires_balance,
            half_day_allowed: t.half_day_allowed,
            applicable_categories: t.applicable_categories.clone(),
        }
    }
}

/// A leave policy version with its leave types.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LeavePolicyView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub name: String,
    pub version_no: i32,
    pub effective_from: NaiveDate,
    /// ISO weekdays (1 = Monday … 7 = Sunday).
    pub weekly_off_days: Vec<i16>,
    pub holidays: Vec<NaiveDate>,
    pub status: LeavePolicyStatus,
    pub status_reason: Option<String>,
    /// Rule validation findings of the last submission.
    pub validation_findings: Vec<String>,
    pub supersedes_policy_id: Option<Uuid>,
    pub process_instance_id: Option<String>,
    pub leave_types: Vec<LeaveTypeView>,
    pub version: i64,
}

impl LeavePolicyView {
    pub fn of(p: &PolicyRow, types: &[LeaveTypeRow]) -> Self {
        Self {
            id: p.id,
            institution_id: p.institution_id.as_uuid(),
            name: p.name.clone(),
            version_no: p.version_no,
            effective_from: p.effective_from,
            weekly_off_days: p.weekly_off_days.clone(),
            holidays: p.holidays.clone(),
            status: p.status,
            status_reason: p.status_reason.clone(),
            validation_findings: p
                .validation
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default(),
            supersedes_policy_id: p.supersedes_policy_id,
            process_instance_id: p.process_instance_id.clone(),
            leave_types: types.iter().map(LeaveTypeView::from).collect(),
            version: p.version,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct LeavePolicyList {
    pub items: Vec<LeavePolicyView>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct LeaveTypeList {
    pub items: Vec<LeaveTypeView>,
}

// ------------------------------------------------------------------ bodies

/// New leave policy version (draft).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LeavePolicyCreate {
    pub name: Option<String>,
    /// `YYYY-MM-DD`.
    pub effective_from: String,
    /// ISO weekdays (default `[7]` = Sunday).
    pub weekly_off_days: Option<Vec<i16>>,
    /// `YYYY-MM-DD` holidays excluded from leave days.
    pub holidays: Option<Vec<String>>,
}

/// New leave type of a draft policy.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LeaveTypeCreate {
    /// `^[A-Z0-9][A-Z0-9_-]{0,15}$` (e.g. `CL`, `EL`, `SL`, `LOP`).
    pub code: String,
    pub name: Option<String>,
    #[schema(example = "12.00")]
    pub annual_entitlement: Option<String>,
    pub accrual_frequency: Option<AccrualFrequency>,
    #[schema(example = "5.00")]
    pub carry_forward_cap: Option<String>,
    #[schema(example = "0.00")]
    pub encashment_cap: Option<String>,
    pub is_paid: Option<bool>,
    pub requires_balance: Option<bool>,
    pub half_day_allowed: Option<bool>,
    /// `StaffCategory` names (default: every category).
    pub applicable_categories: Option<Vec<String>>,
}

/// Filters of the policy list.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct LeavePolicyQuery {
    /// `LeavePolicyStatus` wire name.
    pub status: Option<String>,
    /// ≤ 200 (default 50).
    pub limit: Option<i64>,
}

// ------------------------------------------------------------------ repository

async fn policy_with_types(
    st: &PeopleState,
    tenant: TenantId,
    id: Uuid,
) -> Result<LeavePolicyView, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let p = load_policy(&mut conn, tenant, id, false).await?;
    let types = types_of(&mut conn, tenant, id).await?;
    Ok(LeavePolicyView::of(&p, &types))
}

fn policy_response(status: StatusCode, v: &LeavePolicyView) -> Response {
    with_etag(status, v, Version(v.version))
}

async fn move_policy(
    conn: &mut PgConnection,
    tenant: TenantId,
    p: &PolicyRow,
    to: LeavePolicyStatus,
    reason: Option<&str>,
    pid: Option<&str>,
    actor: &Actor,
) -> Result<i64, KernelError> {
    if !policy_can_transition(p.status, to) {
        return Err(illegal(
            &format!("leave policy v{}", p.version_no),
            p.status.as_str(),
            to.as_str(),
        ));
    }
    let v: i64 = sqlx::query_scalar(
        "UPDATE people.leave_policy SET status = $3, status_reason = $4, \
         process_instance_id = COALESCE($5, process_instance_id), \
         approved_at = CASE WHEN $3 = 'approved' THEN now() ELSE approved_at END, \
         activated_at = CASE WHEN $3 = 'active' THEN now() ELSE activated_at END, \
         version = version + 1, updated_at = now() WHERE id = $1 AND tenant_id = $2 \
         RETURNING version",
    )
    .bind(p.id)
    .bind(tenant.as_uuid())
    .bind(to.as_str())
    .bind(reason)
    .bind(pid)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    history(
        conn,
        &Transition {
            tenant,
            institution: p.institution_id,
            aggregate_type: "leave_policy",
            aggregate_id: p.id,
            from: Some(p.status.as_str()),
            to: to.as_str(),
            reason,
            process_instance_id: pid,
            cause: if to == LeavePolicyStatus::Active {
                LEAVE_POLICY_ACTIVATED
            } else {
                "sutraerp.people.leave_policy.transitioned"
            },
        },
        actor,
    )
    .await?;
    Ok(v)
}

// ------------------------------------------------------------------ handlers

/// Create a draft leave policy version (`people.leave.manage`).
#[utoipa::path(post, path = "/api/v1/people/institutions/{id}/leave-policies", operation_id = "people_leave_policy_create", tag = "people-leave",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = LeavePolicyCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = LeavePolicyView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn policy_create(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LeavePolicyCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::LeaveManage)?;
    require_idempotency_key(&headers)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let name = required("name", b.name)?;
    let effective_from = parse_date("effectiveFrom", &b.effective_from)?;
    let mut offs = b.weekly_off_days.unwrap_or_else(|| vec![7]);
    offs.sort_unstable();
    offs.dedup();
    if offs.iter().any(|d| !(1..=7).contains(d)) || offs.len() >= 7 {
        return Err(invalid(
            "weeklyOffDays",
            "invalid",
            "ISO weekdays 1 (Monday) … 7 (Sunday); at least one working weekday",
        )
        .into());
    }
    let mut holidays = Vec::new();
    for h in b.holidays.unwrap_or_default() {
        holidays.push(parse_date("holidays", &h)?);
    }
    holidays.sort_unstable();
    holidays.dedup();
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    // Serialise version numbers per institution.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text, 0))")
        .bind(format!("leave_policy:{id}"))
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let pid: Uuid = sqlx::query_scalar(
        "INSERT INTO people.leave_policy (tenant_id, institution_id, name, version_no, \
         effective_from, weekly_off_days, holidays, created_by) \
         SELECT $1, $2, $3, COALESCE(max(version_no), 0) + 1, $4, $5, $6, $7 \
         FROM people.leave_policy WHERE tenant_id = $1 AND institution_id = $2 RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(&name)
    .bind(effective_from)
    .bind(&offs)
    .bind(&holidays)
    .bind(a.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    history(
        &mut tx,
        &Transition {
            tenant: t,
            institution,
            aggregate_type: "leave_policy",
            aggregate_id: pid,
            from: None,
            to: "draft",
            reason: None,
            process_instance_id: None,
            cause: "sutraerp.people.leave_policy.created",
        },
        &a,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "people.leave_policy.create",
        "leave_policy",
        pid,
        serde_json::json!({"name": name, "effectiveFrom": effective_from}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let v = policy_with_types(&st, t, pid).await?;
    Ok(policy_response(StatusCode::CREATED, &v))
}

/// Leave policy versions of an institution (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/institutions/{id}/leave-policies", operation_id = "people_leave_policy_list", tag = "people-leave",
    params(("id" = String, Path, format = Uuid), LeavePolicyQuery), security(("session_cookie" = [])),
    responses((status = 200, body = LeavePolicyList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn policy_list(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<LeavePolicyQuery>,
) -> ApiResult<Json<LeavePolicyList>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution(&st, &scope, institution).await?;
    let status = match q.status.as_deref() {
        None => None,
        Some(s) => Some(
            LeavePolicyStatus::parse(s)
                .ok_or_else(|| invalid("status", "invalid", "unknown leave policy status"))?,
        ),
    };
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let sql = format!(
        "SELECT {POLICY_COLUMNS} FROM people.leave_policy WHERE tenant_id = $1 \
         AND institution_id = $2 AND ($3::text IS NULL OR status = $3) \
         ORDER BY version_no DESC LIMIT $4"
    );
    let rows = sqlx::query(&sql)
        .bind(t.as_uuid())
        .bind(id)
        .bind(status.map(|s| s.as_str()))
        .bind(limit(q.limit))
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db_error)?;
    let mut items = Vec::with_capacity(rows.len());
    for r in &rows {
        let p = policy_from_row(r).map_err(map_db_error)?;
        let types = types_of(&mut conn, t, p.id).await?;
        items.push(LeavePolicyView::of(&p, &types));
    }
    Ok(Json(LeavePolicyList { items }))
}

/// One leave policy version with its leave types (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/leave-policies/{id}", operation_id = "people_leave_policy_get", tag = "people-leave",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = LeavePolicyView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn policy_get(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let v = policy_with_types(&st, ctx.scope.tenant_id, id).await?;
    Ok(policy_response(StatusCode::OK, &v))
}

/// Add a leave type to a draft policy (`people.leave.manage`; 409 once submitted).
#[utoipa::path(post, path = "/api/v1/people/leave-policies/{id}/leave-types", operation_id = "people_leave_type_create", tag = "people-leave",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = LeaveTypeCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = LeavePolicyView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn type_create(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LeaveTypeCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::LeaveManage)?;
    require_idempotency_key(&headers)?;
    let t = ctx.scope.tenant_id;
    let code = b.code.trim().to_owned();
    if !super::domain::valid_code(&code) {
        return Err(invalid(
            "code",
            "invalid_code",
            "must match ^[A-Z0-9][A-Z0-9_-]{0,15}$",
        )
        .into());
    }
    let name = required("name", b.name)?;
    let dec_or = |field: &str, raw: Option<String>| -> Result<Decimal, KernelError> {
        match raw {
            None => Ok(Decimal::ZERO),
            Some(r) => non_negative(field, &r),
        }
    };
    let entitlement = dec_or("annualEntitlement", b.annual_entitlement)?;
    let carry = dec_or("carryForwardCap", b.carry_forward_cap)?;
    let encash = dec_or("encashmentCap", b.encashment_cap)?;
    if entitlement > Decimal::from(366) {
        return Err(invalid("annualEntitlement", "out_of_range", "at most 366 days").into());
    }
    let categories = match b.applicable_categories {
        None => StaffCategory::ALL
            .iter()
            .map(|c| c.as_str().to_owned())
            .collect::<Vec<_>>(),
        Some(v) => {
            let mut out = Vec::new();
            for c in v {
                let parsed = StaffCategory::parse(c.trim()).ok_or_else(|| {
                    invalid(
                        "applicableCategories",
                        "invalid",
                        "teaching, non_teaching or administrative",
                    )
                })?;
                if !out.iter().any(|x: &String| x == parsed.as_str()) {
                    out.push(parsed.as_str().to_owned());
                }
            }
            if out.is_empty() {
                return Err(invalid(
                    "applicableCategories",
                    "required",
                    "at least one staff category",
                )
                .into());
            }
            out
        }
    };
    let accrual = b.accrual_frequency.unwrap_or(AccrualFrequency::Yearly);
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let p = load_policy(&mut tx, t, id, true).await?;
    if p.status != LeavePolicyStatus::Draft {
        return Err(KernelError::Conflict(format!(
            "leave policy v{} is {}; leave types change only while it is a draft",
            p.version_no, p.status
        ))
        .into());
    }
    let scope = institution_scope(ctx, p.institution_id);
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM people.leave_type WHERE policy_id = $1 AND code = $2)",
    )
    .bind(id)
    .bind(&code)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if exists {
        return Err(KernelError::Conflict(format!(
            "leave type {code} already exists in this policy"
        ))
        .into());
    }
    let type_id: Uuid = sqlx::query_scalar(
        "INSERT INTO people.leave_type (tenant_id, institution_id, policy_id, code, name, \
         annual_entitlement, accrual_frequency, carry_forward_cap, encashment_cap, is_paid, \
         requires_balance, half_day_allowed, applicable_categories) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(p.institution_id.as_uuid())
    .bind(id)
    .bind(&code)
    .bind(&name)
    .bind(entitlement)
    .bind(accrual.as_str())
    .bind(carry)
    .bind(encash)
    .bind(b.is_paid.unwrap_or(true))
    .bind(b.requires_balance.unwrap_or(true))
    .bind(b.half_day_allowed.unwrap_or(true))
    .bind(&categories)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    sqlx::query(
        "UPDATE people.leave_policy SET version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit(
        &mut tx,
        &scope,
        "people.leave_type.create",
        "leave_type",
        type_id,
        serde_json::json!({"policyId": id, "code": code}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let v = policy_with_types(&st, t, id).await?;
    Ok(policy_response(StatusCode::CREATED, &v))
}

/// Leave types of a policy version (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/leave-policies/{id}/leave-types", operation_id = "people_leave_type_list", tag = "people-leave",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = LeaveTypeList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn type_list(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<LeaveTypeList>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    load_policy(&mut conn, t, id, false).await?;
    let types = types_of(&mut conn, t, id).await?;
    Ok(Json(LeaveTypeList {
        items: types.iter().map(LeaveTypeView::from).collect(),
    }))
}

/// Submit a draft for review (`people.leave.manage`): rule validation (conflicts → 422 and the
/// policy stays a draft with its findings), then `LEAVE_POLICY_APPROVAL` → under_review
/// (auto-approval: approved).
#[utoipa::path(post, path = "/api/v1/people/leave-policies/{id}/submit", operation_id = "people_leave_policy_submit", tag = "people-leave",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the policy")),
    request_body = LeaveReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = LeavePolicyView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn policy_submit(
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
    let (p, types) = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        let p = load_policy(&mut conn, t, id, false).await?;
        let types = types_of(&mut conn, t, id).await?;
        (p, types)
    };
    ensure_version(v, p.version)?;
    if !policy_can_transition(p.status, LeavePolicyStatus::UnderReview) {
        return Err(illegal(
            &format!("leave policy v{}", p.version_no),
            p.status.as_str(),
            "under_review",
        )
        .into());
    }
    let scope = institution_scope(ctx, p.institution_id);
    require_institution_writable(&st, &scope, p.institution_id).await?;
    let rules: Vec<TypeRule<'_>> = types
        .iter()
        .map(|x| TypeRule {
            code: &x.code,
            annual_entitlement: x.annual_entitlement,
            accrual: x.accrual_frequency,
            encashment_cap: x.encashment_cap,
            carry_forward_cap: x.carry_forward_cap,
            is_paid: x.is_paid,
            requires_balance: x.requires_balance,
        })
        .collect();
    let findings = validate_policy(&p.weekly_off_days, &rules);
    if !findings.is_empty() {
        // Preserve the findings on the draft (L4-E-044-BH-003), then refuse.
        sqlx::query(
            "UPDATE people.leave_policy SET validation = $2, updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(serde_json::json!(findings))
        .execute(&st.pool)
        .await
        .map_err(map_db_error)?;
        return Err(KernelError::Validation(
            findings
                .iter()
                .map(|f| FieldViolation::new("leaveTypes", "policy_conflict", f))
                .collect(),
        )
        .into());
    }
    let outcome = request_approval(
        &st,
        &scope,
        process::LEAVE_POLICY_APPROVAL,
        business_key("leave_policy", id, p.version),
        BTreeMap::from([
            ("policyId".to_owned(), serde_json::json!(id)),
            ("versionNo".to_owned(), serde_json::json!(p.version_no)),
        ]),
    )
    .await?;
    let pid = outcome.process_instance_id().to_owned();
    let reason = super::common::clean(b.reason);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let cur = load_policy(&mut tx, t, id, true).await?;
    ensure_version(v, cur.version)?;
    sqlx::query("UPDATE people.leave_policy SET validation = '[]'::jsonb WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    move_policy(
        &mut tx,
        t,
        &cur,
        LeavePolicyStatus::UnderReview,
        reason.as_deref(),
        Some(&pid),
        &a,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        let cur = load_policy(&mut tx, t, id, true).await?;
        move_policy(
            &mut tx,
            t,
            &cur,
            LeavePolicyStatus::Approved,
            Some("auto-approved"),
            Some(&pid),
            &Actor::system(a.correlation_id.clone()),
        )
        .await?;
    }
    audit(
        &mut tx,
        &scope,
        "people.leave_policy.submit",
        "leave_policy",
        id,
        serde_json::json!({"processInstanceId": pid}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let v = policy_with_types(&st, t, id).await?;
    Ok(policy_response(StatusCode::OK, &v))
}

/// Decide a policy under review (`people.approve`, step-up): approve → approved, reject → back to
/// draft with the reason.
#[utoipa::path(post, path = "/api/v1/people/leave-policies/{id}/decision", operation_id = "people_leave_policy_decide", tag = "people-leave",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the policy")),
    request_body = LeaveDecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = LeavePolicyView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn policy_decide(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LeaveDecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleApprove)?;
    // Part 1 · S3-appr (DEF-X-002): the requester of the pending request never decides it.
    sutra_persistence::approval::guard_decision(&st.pool, ctx, &id, "leave policy").await?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let p = load_policy(&mut tx, t, id, true).await?;
    ensure_version(v, p.version)?;
    let scope = institution_scope(ctx, p.institution_id);
    let to = if b.approve() {
        LeavePolicyStatus::Approved
    } else {
        LeavePolicyStatus::Draft
    };
    if p.status != LeavePolicyStatus::UnderReview {
        return Err(illegal(
            &format!("leave policy v{}", p.version_no),
            p.status.as_str(),
            to.as_str(),
        )
        .into());
    }
    let reason = super::common::clean(b.reason.clone());
    if !b.approve() && reason.is_none() {
        return Err(invalid("reason", "required", "a rejection needs a reason").into());
    }
    move_policy(
        &mut tx,
        t,
        &p,
        to,
        reason.as_deref(),
        p.process_instance_id.as_deref(),
        &a,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "people.leave_policy.decide",
        "leave_policy",
        id,
        serde_json::json!({"decision": b.decision, "reason": reason}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let v = policy_with_types(&st, t, id).await?;
    Ok(policy_response(StatusCode::OK, &v))
}

/// Activate an approved policy (`people.approve`, step-up): it becomes the institution's active
/// version and the previous active one is superseded (`leave_policy.activated`). A policy is
/// activated only once its `effectiveFrom` date has come (409 before).
#[utoipa::path(post, path = "/api/v1/people/leave-policies/{id}/activate", operation_id = "people_leave_policy_activate", tag = "people-leave",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the policy")),
    request_body = LeaveReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = LeavePolicyView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn policy_activate(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LeaveReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleApprove)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let p = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_policy(&mut conn, t, id, false).await?
    };
    let scope = institution_scope(ctx, p.institution_id);
    require_institution_writable(&st, &scope, p.institution_id).await?;
    let today = business_today(&st, &scope, p.institution_id).await?;
    if p.effective_from > today {
        return Err(KernelError::Conflict(format!(
            "leave policy v{} takes effect on {}; activate it then",
            p.version_no, p.effective_from
        ))
        .into());
    }
    let reason = super::common::clean(b.reason);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let p = load_policy(&mut tx, t, id, true).await?;
    ensure_version(v, p.version)?;
    if !policy_can_transition(p.status, LeavePolicyStatus::Active) {
        return Err(illegal(
            &format!("leave policy v{}", p.version_no),
            p.status.as_str(),
            "active",
        )
        .into());
    }
    let mut superseded = None;
    if let Some(prev) = active_policy(&mut tx, t, p.institution_id).await? {
        let prev = load_policy(&mut tx, t, prev.id, true).await?;
        move_policy(
            &mut tx,
            t,
            &prev,
            LeavePolicyStatus::Superseded,
            Some(format!("superseded by v{}", p.version_no).as_str()),
            None,
            &a,
        )
        .await?;
        superseded = Some(prev.id);
    }
    sqlx::query("UPDATE people.leave_policy SET supersedes_policy_id = $2 WHERE id = $1")
        .bind(id)
        .bind(superseded)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let version = move_policy(
        &mut tx,
        t,
        &p,
        LeavePolicyStatus::Active,
        reason.as_deref(),
        None,
        &a,
    )
    .await?;
    emit_status(
        &mut tx,
        &scope,
        LEAVE_POLICY_ACTIVATED,
        LEAVE_POLICY_ACTIVATED_SCHEMA,
        &Transition {
            tenant: t,
            institution: p.institution_id,
            aggregate_type: "leave_policy",
            aggregate_id: id,
            from: Some(p.status.as_str()),
            to: "active",
            reason: reason.as_deref(),
            process_instance_id: p.process_instance_id.as_deref(),
            cause: LEAVE_POLICY_ACTIVATED,
        },
        version,
        &EventParties::default(),
        &a,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "people.leave_policy.activate",
        "leave_policy",
        id,
        serde_json::json!({"supersededPolicyId": superseded}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let v = policy_with_types(&st, t, id).await?;
    Ok(policy_response(StatusCode::OK, &v))
}

/// Retire the active policy without a successor (`people.leave.manage`; active → superseded).
#[utoipa::path(post, path = "/api/v1/people/leave-policies/{id}/supersede", operation_id = "people_leave_policy_supersede", tag = "people-leave",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the policy")),
    request_body = LeaveReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = LeavePolicyView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn policy_supersede(
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
    let reason = required("reason", b.reason)?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let p = load_policy(&mut tx, t, id, true).await?;
    ensure_version(v, p.version)?;
    let scope = institution_scope(ctx, p.institution_id);
    move_policy(
        &mut tx,
        t,
        &p,
        LeavePolicyStatus::Superseded,
        Some(&reason),
        None,
        &a,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "people.leave_policy.supersede",
        "leave_policy",
        id,
        serde_json::json!({"reason": reason}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let v = policy_with_types(&st, t, id).await?;
    Ok(policy_response(StatusCode::OK, &v))
}

pub fn router(state: PeopleState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(policy_create, policy_list))
        .routes(routes!(policy_get))
        .routes(routes!(type_create, type_list))
        .routes(routes!(policy_submit))
        .routes(routes!(policy_decide))
        .routes(routes!(policy_activate))
        .routes(routes!(policy_supersede))
        .with_state(state)
}
