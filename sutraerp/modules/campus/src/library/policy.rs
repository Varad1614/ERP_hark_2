//! Borrowing policy versions (L3-G-124…139 rules; `LIBRARY_POLICY_APPROVAL`): loan days and
//! loan limits per member kind, renewals, fine per day (+ cap), reservation hold days and the
//! lost-copy charge factor. Draft → submitted for approval → approved → active (the previous
//! active version is superseded); only the active version governs circulation.

use super::common::{
    check_version, clean, expected, invalid, required_text, transition, LibraryDecision,
    LibraryDecisionBody, LibraryReasonBody, Transition,
};
use super::domain::{rules_for, BorrowingRules, LibraryApprovalState, LibraryPolicyStatus};
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::decimal::{fmt2, parse_decimal};
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, with_etag};
use crate::shared::rows::decode;
use crate::shared::scope::institution_scope;
use crate::shared::{Actor, CampusState};
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
use std::str::FromStr;
use sutra_contracts::campus::events::library as ev;
use sutra_contracts::campus::status::PersonKind;
use sutra_kernel::auth::Permission;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// A stored policy version (decimals kept as `Decimal`).
#[derive(Clone, Debug, PartialEq)]
pub struct Policy {
    pub id: Uuid,
    pub institution_id: InstitutionId,
    pub name: String,
    pub effective_from: NaiveDate,
    pub loan_days_student: i32,
    pub loan_days_staff: i32,
    pub max_loans_student: i32,
    pub max_loans_staff: i32,
    pub max_renewals: i32,
    pub fine_per_day: Decimal,
    pub max_fine: Option<Decimal>,
    pub reservation_hold_days: i32,
    pub lost_charge_factor: Decimal,
    pub status: LibraryPolicyStatus,
    pub approval_state: LibraryApprovalState,
    pub status_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub superseded_by: Option<Uuid>,
    pub version: i64,
}

impl Policy {
    /// The borrowing rules of this version for a member kind.
    pub fn rules(&self, kind: PersonKind) -> BorrowingRules {
        rules_for(
            kind,
            self.loan_days_student,
            self.loan_days_staff,
            self.max_loans_student,
            self.max_loans_staff,
            self.max_renewals,
            self.fine_per_day,
            self.max_fine,
            self.reservation_hold_days,
            self.lost_charge_factor,
        )
    }
}

const COLUMNS: &str =
    "id, institution_id, name, effective_from, loan_days_student, loan_days_staff, \
    max_loans_student, max_loans_staff, max_renewals, fine_per_day, max_fine, \
    reservation_hold_days, lost_charge_factor, status, approval_state, status_reason, \
    process_instance_id, superseded_by, version";

fn from_row(r: &PgRow) -> Result<Policy, sqlx::Error> {
    Ok(Policy {
        id: r.try_get("id")?,
        institution_id: InstitutionId(r.try_get("institution_id")?),
        name: r.try_get("name")?,
        effective_from: r.try_get("effective_from")?,
        loan_days_student: r.try_get("loan_days_student")?,
        loan_days_staff: r.try_get("loan_days_staff")?,
        max_loans_student: r.try_get("max_loans_student")?,
        max_loans_staff: r.try_get("max_loans_staff")?,
        max_renewals: r.try_get("max_renewals")?,
        fine_per_day: r.try_get("fine_per_day")?,
        max_fine: r.try_get("max_fine")?,
        reservation_hold_days: r.try_get("reservation_hold_days")?,
        lost_charge_factor: r.try_get("lost_charge_factor")?,
        status: decode(r.try_get("status")?, LibraryPolicyStatus::parse)?,
        approval_state: decode(r.try_get("approval_state")?, LibraryApprovalState::parse)?,
        status_reason: r.try_get("status_reason")?,
        process_instance_id: r.try_get("process_instance_id")?,
        superseded_by: r.try_get("superseded_by")?,
        version: r.try_get("version")?,
    })
}

/// Loads one policy of the tenant (404), optionally locked.
pub async fn load_policy(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<Policy, KernelError> {
    let sql = format!(
        "SELECT {COLUMNS} FROM campus.library_policy WHERE id = $1 AND tenant_id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    let row = sqlx::query(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "library policy",
        })?;
    from_row(&row).map_err(map_db_error)
}

/// The active policy of an institution (409 when the library has none yet).
pub async fn active_policy(
    conn: &mut PgConnection,
    tenant: TenantId,
    institution: InstitutionId,
) -> Result<Policy, KernelError> {
    let sql = format!(
        "SELECT {COLUMNS} FROM campus.library_policy WHERE tenant_id = $1 AND institution_id = $2 \
         AND status = 'active'"
    );
    let row = sqlx::query(&sql)
        .bind(tenant.as_uuid())
        .bind(institution.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or_else(|| {
            KernelError::Conflict("the library has no active borrowing policy".into())
        })?;
    from_row(&row).map_err(map_db_error)
}

/// A borrowing policy version as screens show it.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LibraryPolicyView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub name: String,
    pub effective_from: NaiveDate,
    pub loan_days_student: i32,
    pub loan_days_staff: i32,
    pub max_loans_student: i32,
    pub max_loans_staff: i32,
    pub max_renewals: i32,
    #[schema(value_type = String, example = "2.00")]
    pub fine_per_day: String,
    #[schema(value_type = Option<String>, example = "200.00")]
    pub max_fine: Option<String>,
    pub reservation_hold_days: i32,
    #[schema(value_type = String, example = "1.5000")]
    pub lost_charge_factor: String,
    pub status: LibraryPolicyStatus,
    pub approval_state: LibraryApprovalState,
    pub status_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub superseded_by: Option<Uuid>,
    pub version: i64,
}

fn fmt4(v: Decimal) -> String {
    let mut r = v.round_dp(4);
    r.rescale(4);
    r.to_string()
}

impl From<&Policy> for LibraryPolicyView {
    fn from(p: &Policy) -> Self {
        Self {
            id: p.id,
            institution_id: p.institution_id.as_uuid(),
            name: p.name.clone(),
            effective_from: p.effective_from,
            loan_days_student: p.loan_days_student,
            loan_days_staff: p.loan_days_staff,
            max_loans_student: p.max_loans_student,
            max_loans_staff: p.max_loans_staff,
            max_renewals: p.max_renewals,
            fine_per_day: fmt2(p.fine_per_day),
            max_fine: p.max_fine.map(fmt2),
            reservation_hold_days: p.reservation_hold_days,
            lost_charge_factor: fmt4(p.lost_charge_factor),
            status: p.status,
            approval_state: p.approval_state,
            status_reason: p.status_reason.clone(),
            process_instance_id: p.process_instance_id.clone(),
            superseded_by: p.superseded_by,
            version: p.version,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct LibraryPolicyList {
    pub items: Vec<LibraryPolicyView>,
}

/// A new borrowing policy version.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LibraryPolicyCreate {
    pub name: String,
    pub effective_from: NaiveDate,
    pub loan_days_student: i32,
    pub loan_days_staff: i32,
    pub max_loans_student: i32,
    pub max_loans_staff: i32,
    pub max_renewals: i32,
    #[schema(value_type = String, example = "2.00")]
    pub fine_per_day: String,
    #[schema(value_type = Option<String>, example = "200.00")]
    pub max_fine: Option<String>,
    pub reservation_hold_days: i32,
    /// Multiplier of the replacement cost charged for a lost copy (≤ 4 decimals).
    #[schema(value_type = String, example = "1.5000")]
    pub lost_charge_factor: String,
}

fn in_range(field: &str, v: i32, lo: i32, hi: i32) -> Result<(), KernelError> {
    if (lo..=hi).contains(&v) {
        Ok(())
    } else {
        Err(invalid(
            field,
            "out_of_range",
            &format!("between {lo} and {hi}"),
        ))
    }
}

/// Filters of a policy list.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct LibraryPolicyQuery {
    pub status: Option<LibraryPolicyStatus>,
}

/// Create a draft borrowing policy version (`campus.library.manage`; Idempotency-Key).
#[utoipa::path(post, path = "/api/v1/campus/institutions/{id}/library-policies", operation_id = "campus_library_policy_create", tag = "campus-library",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)),
    request_body = LibraryPolicyCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = LibraryPolicyView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn create(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LibraryPolicyCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::LibraryManage)?;
    require_idempotency_key(&headers)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let name = required_text("name", &b.name, 200)?;
    in_range("loanDaysStudent", b.loan_days_student, 1, 365)?;
    in_range("loanDaysStaff", b.loan_days_staff, 1, 365)?;
    in_range("maxLoansStudent", b.max_loans_student, 0, 100)?;
    in_range("maxLoansStaff", b.max_loans_staff, 0, 100)?;
    in_range("maxRenewals", b.max_renewals, 0, 20)?;
    in_range("reservationHoldDays", b.reservation_hold_days, 1, 60)?;
    let fine = parse_decimal("finePerDay", &b.fine_per_day)?;
    if fine < Decimal::ZERO {
        return Err(invalid("finePerDay", "negative", "must not be negative").into());
    }
    let max_fine = clean(b.max_fine)
        .map(|m| parse_decimal("maxFine", &m))
        .transpose()?;
    if max_fine.is_some_and(|m| m < Decimal::ZERO) {
        return Err(invalid("maxFine", "negative", "must not be negative").into());
    }
    let factor = Decimal::from_str(b.lost_charge_factor.trim())
        .ok()
        .filter(|f| *f >= Decimal::ZERO && f.normalize().scale() <= 4 && *f <= Decimal::from(100))
        .ok_or_else(|| {
            invalid(
                "lostChargeFactor",
                "invalid_number",
                "a decimal string between 0 and 100 with at most 4 decimals",
            )
        })?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let pid: Uuid = sqlx::query_scalar(
        "INSERT INTO campus.library_policy (tenant_id, institution_id, name, effective_from, \
         loan_days_student, loan_days_staff, max_loans_student, max_loans_staff, max_renewals, \
         fine_per_day, max_fine, reservation_hold_days, lost_charge_factor) VALUES ($1, $2, $3, $4, \
         $5, $6, $7, $8, $9, $10, $11, $12, $13) RETURNING id",
    )
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .bind(&name)
    .bind(b.effective_from)
    .bind(b.loan_days_student)
    .bind(b.loan_days_staff)
    .bind(b.max_loans_student)
    .bind(b.max_loans_staff)
    .bind(b.max_renewals)
    .bind(fine)
    .bind(max_fine)
    .bind(b.reservation_hold_days)
    .bind(factor)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    transition(
        &mut tx,
        &scope,
        &Transition {
            aggregate_type: "library_policy",
            id: pid,
            institution,
            from: None,
            to: "draft",
            version: 1,
            reason: None,
            process_instance_id: None,
            number: None,
            subject_ref: None,
        },
        None,
        "campus.library_policy.create",
        &actor,
    )
    .await?;
    let p = load_policy(&mut tx, ctx.scope.tenant_id, pid, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(
        StatusCode::CREATED,
        &LibraryPolicyView::from(&p),
        Version(p.version),
    ))
}

/// Borrowing policy versions of an institution, newest first (`campus.read`).
#[utoipa::path(get, path = "/api/v1/campus/institutions/{id}/library-policies", operation_id = "campus_library_policy_list", tag = "campus-library",
    params(("id" = String, Path, format = Uuid), LibraryPolicyQuery), security(("session_cookie" = [])),
    responses((status = 200, body = LibraryPolicyList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn list(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<LibraryPolicyQuery>,
) -> ApiResult<Json<LibraryPolicyList>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let institution = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, institution), institution).await?;
    let sql = format!(
        "SELECT {COLUMNS} FROM campus.library_policy WHERE tenant_id = $1 AND institution_id = $2 \
         AND ($3::text IS NULL OR status = $3) ORDER BY created_at DESC"
    );
    let rows = sqlx::query(&sql)
        .bind(ctx.scope.tenant_id.as_uuid())
        .bind(id)
        .bind(q.status.map(|s| s.as_str()))
        .fetch_all(&st.pool)
        .await
        .map_err(map_db_error)?;
    let items = rows
        .iter()
        .map(|r| from_row(r).map(|p| LibraryPolicyView::from(&p)))
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)?;
    Ok(Json(LibraryPolicyList { items }))
}

/// One borrowing policy version (`campus.read`).
#[utoipa::path(get, path = "/api/v1/campus/library-policies/{id}", operation_id = "campus_library_policy_get", tag = "campus-library",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = LibraryPolicyView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn get(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CampusRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let p = load_policy(&mut conn, ctx.scope.tenant_id, id, false).await?;
    Ok(with_etag(
        StatusCode::OK,
        &LibraryPolicyView::from(&p),
        Version(p.version),
    ))
}

#[allow(clippy::too_many_arguments)]
async fn set_approval(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    p: &Policy,
    to: LibraryApprovalState,
    reason: Option<&str>,
    pid: Option<&str>,
    action: &'static str,
    actor: &Actor,
) -> Result<Policy, KernelError> {
    sqlx::query(
        "UPDATE campus.library_policy SET approval_state = $2, status_reason = COALESCE($3, status_reason), \
         process_instance_id = COALESCE($4, process_instance_id), version = version + 1, updated_at = now() \
         WHERE id = $1",
    )
    .bind(p.id)
    .bind(to.as_str())
    .bind(reason)
    .bind(pid)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let pid = pid.or(p.process_instance_id.as_deref());
    transition(
        conn,
        scope,
        &Transition {
            aggregate_type: "library_policy",
            id: p.id,
            institution: p.institution_id,
            from: Some(p.approval_state.as_str()),
            to: to.as_str(),
            version: p.version + 1,
            reason,
            process_instance_id: pid,
            number: None,
            subject_ref: None,
        },
        None,
        action,
        actor,
    )
    .await?;
    load_policy(conn, scope.tenant_id, p.id, true).await
}

/// Submit a draft policy for approval (`campus.library.manage`; If-Match;
/// `LIBRARY_POLICY_APPROVAL`). Auto-approval leaves it `approved` and ready to activate.
#[utoipa::path(post, path = "/api/v1/campus/library-policies/{id}/submit", operation_id = "campus_library_policy_submit", tag = "campus-library",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = LibraryReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = LibraryPolicyView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn submit(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LibraryReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::LibraryManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let pre = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_policy(&mut conn, t, id, false).await?
    };
    let scope = institution_scope(ctx, pre.institution_id);
    require_institution_writable(&st, &scope, pre.institution_id).await?;
    check_version(v, pre.version)?;
    let open = matches!(
        pre.approval_state,
        LibraryApprovalState::NotRequested | LibraryApprovalState::Rejected
    );
    if pre.status != LibraryPolicyStatus::Draft || !open {
        return Err(KernelError::Conflict(format!(
            "library policy {} is {} / {}; only an unsubmitted draft can be submitted",
            pre.name, pre.status, pre.approval_state
        ))
        .into());
    }
    let mut vars = BTreeMap::new();
    vars.insert("policyName".to_owned(), serde_json::json!(pre.name));
    let outcome = request_approval(
        &st,
        &scope,
        process::LIBRARY_POLICY_APPROVAL,
        business_key("library_policy", pre.id, pre.version),
        vars,
    )
    .await?;
    let actor = actor(ctx);
    let reason = clean(b.reason);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let p = load_policy(&mut tx, t, id, true).await?;
    check_version(v, p.version)?;
    let p = set_approval(
        &mut tx,
        &scope,
        &p,
        LibraryApprovalState::Pending,
        reason.as_deref(),
        Some(outcome.process_instance_id()),
        "campus.library_policy.submit",
        &actor,
    )
    .await?;
    let p = if let ApprovalOutcome::Approved { .. } = outcome {
        set_approval(
            &mut tx,
            &scope,
            &p,
            LibraryApprovalState::Approved,
            None,
            None,
            "campus.library_policy.approve",
            &Actor::system(actor.correlation_id.clone()),
        )
        .await?
    } else {
        p
    };
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(
        StatusCode::OK,
        &LibraryPolicyView::from(&p),
        Version(p.version),
    ))
}

/// Decide a submitted policy (`campus.approve`, step-up; If-Match).
#[utoipa::path(post, path = "/api/v1/campus/library-policies/{id}/decision", operation_id = "campus_library_policy_decide", tag = "campus-library",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = LibraryDecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = LibraryPolicyView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn decide(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LibraryDecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CampusApprove)?;
    let v = expected!(headers);
    let reason = clean(b.reason);
    if b.decision == LibraryDecision::Reject && reason.is_none() {
        return Err(invalid("reason", "required", "a rejection needs a reason").into());
    }
    let t = ctx.scope.tenant_id;
    let pre = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_policy(&mut conn, t, id, false).await?
    };
    let scope = institution_scope(ctx, pre.institution_id);
    require_institution_writable(&st, &scope, pre.institution_id).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let p = load_policy(&mut tx, t, id, true).await?;
    check_version(v, p.version)?;
    if p.approval_state != LibraryApprovalState::Pending {
        return Err(KernelError::Conflict(format!(
            "library policy {} is not awaiting a decision ({})",
            p.name, p.approval_state
        ))
        .into());
    }
    let (to, action) = match b.decision {
        LibraryDecision::Approve => (
            LibraryApprovalState::Approved,
            "campus.library_policy.approve",
        ),
        LibraryDecision::Reject => (
            LibraryApprovalState::Rejected,
            "campus.library_policy.reject",
        ),
    };
    let p = set_approval(
        &mut tx,
        &scope,
        &p,
        to,
        reason.as_deref(),
        None,
        action,
        &actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(
        StatusCode::OK,
        &LibraryPolicyView::from(&p),
        Version(p.version),
    ))
}

/// Activate an approved policy (`campus.library.manage`; If-Match): the previous active version
/// becomes `superseded`; loans already issued keep their due dates.
#[utoipa::path(post, path = "/api/v1/campus/library-policies/{id}/activate", operation_id = "campus_library_policy_activate", tag = "campus-library",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = LibraryReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = LibraryPolicyView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn activate(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LibraryReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::LibraryManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let pre = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_policy(&mut conn, t, id, false).await?
    };
    let scope = institution_scope(ctx, pre.institution_id);
    require_institution_writable(&st, &scope, pre.institution_id).await?;
    let actor = actor(ctx);
    let reason = clean(b.reason);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let p = load_policy(&mut tx, t, id, true).await?;
    check_version(v, p.version)?;
    if p.status != LibraryPolicyStatus::Draft || p.approval_state != LibraryApprovalState::Approved
    {
        return Err(KernelError::Conflict(format!(
            "library policy {} is {} / {}; only an approved draft can be activated",
            p.name, p.status, p.approval_state
        ))
        .into());
    }
    let previous: Vec<(Uuid, i64)> = sqlx::query_as(
        "SELECT id, version FROM campus.library_policy WHERE tenant_id = $1 AND institution_id = $2 \
         AND status = 'active' FOR UPDATE",
    )
    .bind(t.as_uuid())
    .bind(p.institution_id.as_uuid())
    .fetch_all(&mut *tx)
    .await
    .map_err(map_db_error)?;
    for (old, old_version) in previous {
        sqlx::query(
            "UPDATE campus.library_policy SET status = 'superseded', superseded_by = $2, \
             version = version + 1, updated_at = now() WHERE id = $1",
        )
        .bind(old)
        .bind(p.id)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
        transition(
            &mut tx,
            &scope,
            &Transition {
                aggregate_type: "library_policy",
                id: old,
                institution: p.institution_id,
                from: Some("active"),
                to: "superseded",
                version: old_version + 1,
                reason: Some("superseded by a newer policy version"),
                process_instance_id: None,
                number: None,
                subject_ref: None,
            },
            None,
            "campus.library_policy.supersede",
            &actor,
        )
        .await?;
    }
    sqlx::query(
        "UPDATE campus.library_policy SET status = 'active', activated_at = now(), \
         version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(p.id)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    transition(
        &mut tx,
        &scope,
        &Transition {
            aggregate_type: "library_policy",
            id: p.id,
            institution: p.institution_id,
            from: Some("draft"),
            to: "active",
            version: p.version + 1,
            reason: reason.as_deref(),
            process_instance_id: p.process_instance_id.as_deref(),
            number: None,
            subject_ref: None,
        },
        Some((
            ev::LIBRARY_POLICY_ACTIVATED,
            ev::LIBRARY_POLICY_ACTIVATED_SCHEMA,
        )),
        "campus.library_policy.activate",
        &actor,
    )
    .await?;
    let out = load_policy(&mut tx, t, id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(
        StatusCode::OK,
        &LibraryPolicyView::from(&out),
        Version(out.version),
    ))
}

pub fn router(state: CampusState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(create, list))
        .routes(routes!(get))
        .routes(routes!(submit))
        .routes(routes!(decide))
        .routes(routes!(activate))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factor_formatting() {
        assert_eq!(fmt4(Decimal::new(15, 1)), "1.5000");
        assert!(in_range("x", 5, 1, 4).is_err());
        assert!(in_range("x", 4, 1, 4).is_ok());
    }
}
