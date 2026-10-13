//! Examination policy versions (L2-I01; L3-I-001 definition → review → activation, I-002
//! revision → authoritative superseding version, I-003 retirement with history preserved).
//!
//! A policy is identified by its `code`; every revision is a new row with `version_no + 1`
//! (copying the previous version's rules unless overridden) and `supersedes_id`. Activation
//! supersedes the institution's current active version in the same transaction (one active
//! policy per institution, unique index). The rules here drive 6A: registration window
//! enforcement, late registration + fee, hall-ticket conditions (dues, attendance), attempt
//! limit, invigilation ratio, optional hall-ticket approval, self-service registration.

use super::common::{
    check_version, clean, emit, illegal, invalid, move_status, parse_opt_dec, reason_of,
    require_code, require_name, require_reason, status_event, ExamSetupDecisionBody,
    ExamSetupDecisionKind, Move, ReasonBody,
};
use super::domain::policy_can;
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, if_match, with_etag};
use crate::shared::lifecycle::StatusChange;
use crate::shared::publish::audit;
use crate::shared::scope::institution_scope;
use crate::shared::time::business_today;
use crate::shared::{Actor, ExamsState};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use sutra_contracts::exams::events::setup::{
    EXAM_POLICY_ACTIVATED, EXAM_POLICY_ACTIVATED_SCHEMA, EXAM_POLICY_RETIRED,
    EXAM_POLICY_RETIRED_SCHEMA, EXAM_POLICY_SUPERSEDED, EXAM_POLICY_SUPERSEDED_SCHEMA,
};
use sutra_contracts::exams::status::ExamPolicyStatus;
use sutra_kernel::auth::Permission;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::ToSchema;
use uuid::Uuid;

const TABLE: &str = "exams.exam_policy";
const AGG: &str = "exam_policy";

pub const POLICY_COLUMNS: &str = "id, institution_id, code, version_no, name, \
    effective_from, registration_window_enforced, late_registration_allowed, late_fee_code, \
    late_fee_amount, require_dues_cleared, min_attendance_percent, max_attempts, \
    invigilator_ratio, hall_ticket_approval, self_service_allowed, supersedes_id, status, \
    status_reason, process_instance_id, version";

/// One examination policy version (decimals as strings).
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct ExamPolicyView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub code: String,
    pub version_no: i32,
    pub name: String,
    pub effective_from: Option<NaiveDate>,
    /// Registrations outside the session window are refused (late / override path instead).
    pub registration_window_enforced: bool,
    pub late_registration_allowed: bool,
    pub late_fee_code: Option<String>,
    #[schema(value_type = Option<String>, example = "500.00")]
    pub late_fee_amount: Option<Decimal>,
    /// Hall tickets are withheld while the student has outstanding dues.
    pub require_dues_cleared: bool,
    /// Hall tickets are withheld below this attendance percentage (besides 4B's own threshold).
    #[schema(value_type = Option<String>, example = "75.00")]
    pub min_attendance_percent: Option<Decimal>,
    pub max_attempts: i32,
    /// Candidates per invigilator (shortage detection).
    pub invigilator_ratio: i32,
    pub hall_ticket_approval: bool,
    pub self_service_allowed: bool,
    pub supersedes_id: Option<Uuid>,
    /// `ExamPolicyStatus` wire name.
    #[schema(example = "active")]
    pub status: String,
    pub status_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub version: i64,
}

impl ExamPolicyView {
    fn state(&self) -> Result<ExamPolicyStatus, KernelError> {
        ExamPolicyStatus::parse(&self.status)
            .ok_or_else(|| KernelError::internal_msg(format!("bad policy status {}", self.status)))
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ExamPolicyList {
    pub items: Vec<ExamPolicyView>,
}

/// New policy version / draft edit. On create, `code` + `name` are required; a code that
/// already exists creates the next version, copying the latest version's rules for every
/// member left out.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExamPolicyInput {
    pub code: Option<String>,
    pub name: Option<String>,
    pub effective_from: Option<NaiveDate>,
    pub registration_window_enforced: Option<bool>,
    pub late_registration_allowed: Option<bool>,
    pub late_fee_code: Option<String>,
    #[schema(example = "500.00")]
    pub late_fee_amount: Option<String>,
    pub require_dues_cleared: Option<bool>,
    #[schema(example = "75")]
    pub min_attendance_percent: Option<String>,
    pub max_attempts: Option<i32>,
    pub invigilator_ratio: Option<i32>,
    pub hall_ticket_approval: Option<bool>,
    pub self_service_allowed: Option<bool>,
}

/// The rules 6A applies to a session (the session's policy, else the institution's active one,
/// else these defaults).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyRules {
    pub policy_id: Option<Uuid>,
    pub registration_window_enforced: bool,
    pub late_registration_allowed: bool,
    pub late_fee_code: Option<String>,
    pub late_fee_amount: Option<Decimal>,
    pub require_dues_cleared: bool,
    pub min_attendance_percent: Option<Decimal>,
    pub max_attempts: i32,
    pub invigilator_ratio: i32,
    pub hall_ticket_approval: bool,
    pub self_service_allowed: bool,
}

impl Default for PolicyRules {
    fn default() -> Self {
        Self {
            policy_id: None,
            registration_window_enforced: true,
            late_registration_allowed: true,
            late_fee_code: None,
            late_fee_amount: None,
            require_dues_cleared: true,
            min_attendance_percent: None,
            max_attempts: 3,
            invigilator_ratio: 30,
            hall_ticket_approval: false,
            self_service_allowed: true,
        }
    }
}

impl From<&ExamPolicyView> for PolicyRules {
    fn from(p: &ExamPolicyView) -> Self {
        Self {
            policy_id: Some(p.id),
            registration_window_enforced: p.registration_window_enforced,
            late_registration_allowed: p.late_registration_allowed,
            late_fee_code: p.late_fee_code.clone(),
            late_fee_amount: p.late_fee_amount,
            require_dues_cleared: p.require_dues_cleared,
            min_attendance_percent: p.min_attendance_percent,
            max_attempts: p.max_attempts,
            invigilator_ratio: p.invigilator_ratio,
            hall_ticket_approval: p.hall_ticket_approval,
            self_service_allowed: p.self_service_allowed,
        }
    }
}

pub async fn load_policy<'c, E>(
    executor: E,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<ExamPolicyView, KernelError>
where
    E: sqlx::Executor<'c, Database = sqlx::Postgres>,
{
    let sql = format!(
        "SELECT {POLICY_COLUMNS} FROM exams.exam_policy WHERE id = $1 AND tenant_id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    sqlx::query_as::<_, ExamPolicyView>(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(executor)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "examination policy",
        })
}

/// The institution's active policy version, if any.
pub async fn active_policy<'c, E>(
    executor: E,
    tenant: TenantId,
    institution: InstitutionId,
) -> Result<Option<ExamPolicyView>, KernelError>
where
    E: sqlx::Executor<'c, Database = sqlx::Postgres>,
{
    let sql = format!(
        "SELECT {POLICY_COLUMNS} FROM exams.exam_policy WHERE tenant_id = $1 \
         AND institution_id = $2 AND status = 'active'"
    );
    sqlx::query_as::<_, ExamPolicyView>(&sql)
        .bind(tenant.as_uuid())
        .bind(institution.as_uuid())
        .fetch_optional(executor)
        .await
        .map_err(map_db_error)
}

/// Rules governing a session: its bound policy, else the institution's active one, else the
/// defaults.
pub async fn rules_for(
    st: &ExamsState,
    tenant: TenantId,
    institution: InstitutionId,
    policy_id: Option<Uuid>,
) -> Result<PolicyRules, KernelError> {
    if let Some(id) = policy_id {
        if let Ok(p) = load_policy(&st.pool, tenant, id, false).await {
            return Ok(PolicyRules::from(&p));
        }
    }
    Ok(active_policy(&st.pool, tenant, institution)
        .await?
        .map(|p| PolicyRules::from(&p))
        .unwrap_or_default())
}

struct Rules {
    effective_from: Option<NaiveDate>,
    registration_window_enforced: bool,
    late_registration_allowed: bool,
    late_fee_code: Option<String>,
    late_fee_amount: Option<Decimal>,
    require_dues_cleared: bool,
    min_attendance_percent: Option<Decimal>,
    max_attempts: i32,
    invigilator_ratio: i32,
    hall_ticket_approval: bool,
    self_service_allowed: bool,
}

fn merge(
    base: &PolicyRules,
    effective: Option<NaiveDate>,
    b: &ExamPolicyInput,
) -> Result<Rules, KernelError> {
    let late_fee_code = match b.late_fee_code.as_deref() {
        Some(raw) if raw.trim().is_empty() => None,
        Some(raw) => Some(require_code("lateFeeCode", raw)?),
        None => base.late_fee_code.clone(),
    };
    let late_fee_amount = match b.late_fee_amount.as_deref() {
        Some(raw) => parse_opt_dec("lateFeeAmount", Some(raw))?,
        None => base.late_fee_amount,
    };
    if late_fee_amount.is_some_and(|a| a < Decimal::ZERO || a.scale() > 2) {
        return Err(invalid(
            "lateFeeAmount",
            "invalid_amount",
            "must be ≥ 0 with at most two decimals",
        ));
    }
    let min_attendance_percent = match b.min_attendance_percent.as_deref() {
        Some(raw) => parse_opt_dec("minAttendancePercent", Some(raw))?,
        None => base.min_attendance_percent,
    };
    if min_attendance_percent.is_some_and(|p| p < Decimal::ZERO || p > Decimal::ONE_HUNDRED) {
        return Err(invalid(
            "minAttendancePercent",
            "out_of_range",
            "must be between 0 and 100",
        ));
    }
    let max_attempts = b.max_attempts.unwrap_or(base.max_attempts);
    if !(1..=20).contains(&max_attempts) {
        return Err(invalid("maxAttempts", "out_of_range", "must be 1 … 20"));
    }
    let invigilator_ratio = b.invigilator_ratio.unwrap_or(base.invigilator_ratio);
    if !(1..=500).contains(&invigilator_ratio) {
        return Err(invalid(
            "invigilatorRatio",
            "out_of_range",
            "must be 1 … 500",
        ));
    }
    if late_fee_amount.is_some_and(|a| a > Decimal::ZERO) && late_fee_code.is_none() {
        return Err(invalid(
            "lateFeeCode",
            "required",
            "a fee code is required when a late fee is charged",
        ));
    }
    Ok(Rules {
        effective_from: b.effective_from.or(effective),
        registration_window_enforced: b
            .registration_window_enforced
            .unwrap_or(base.registration_window_enforced),
        late_registration_allowed: b
            .late_registration_allowed
            .unwrap_or(base.late_registration_allowed),
        late_fee_code,
        late_fee_amount,
        require_dues_cleared: b.require_dues_cleared.unwrap_or(base.require_dues_cleared),
        min_attendance_percent,
        max_attempts,
        invigilator_ratio,
        hall_ticket_approval: b.hall_ticket_approval.unwrap_or(base.hall_ticket_approval),
        self_service_allowed: b.self_service_allowed.unwrap_or(base.self_service_allowed),
    })
}

fn view_response(p: &ExamPolicyView, status: StatusCode) -> Response {
    with_etag(status, p, Version(p.version))
}

/// Policy versions of an institution, newest first (`exam.read`).
#[utoipa::path(get, path = "/api/v1/exams/institutions/{id}/exam-policies", operation_id = "exam_policy_list", tag = "exam-setup",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ExamPolicyList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn policy_list(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ExamPolicyList>> {
    let ctx = auth.require(Permission::ExamRead)?;
    let scope = institution_scope(ctx, InstitutionId(id));
    require_institution(&st, &scope, InstitutionId(id)).await?;
    let sql = format!(
        "SELECT {POLICY_COLUMNS} FROM exams.exam_policy WHERE tenant_id = $1 AND institution_id = $2 \
         ORDER BY code, version_no DESC"
    );
    let items = sqlx::query_as::<_, ExamPolicyView>(&sql)
        .bind(ctx.scope.tenant_id.as_uuid())
        .bind(id)
        .fetch_all(&st.pool)
        .await
        .map_err(map_db_error)?;
    Ok(Json(ExamPolicyList { items }))
}

/// Define a policy (draft) or the next version of an existing code (`exam.setup.manage`).
#[utoipa::path(post, path = "/api/v1/exams/institutions/{id}/exam-policies", operation_id = "exam_policy_create", tag = "exam-setup",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = ExamPolicyInput, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = ExamPolicyView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn policy_create(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ExamPolicyInput>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamSetupManage)?;
    require_idempotency_key(&headers)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let code = require_code("code", b.code.as_deref().unwrap_or_default())?;
    let tenant = ctx.scope.tenant_id;
    let latest_sql = format!(
        "SELECT {POLICY_COLUMNS} FROM exams.exam_policy WHERE tenant_id = $1 AND institution_id = $2 \
         AND code = $3 ORDER BY version_no DESC LIMIT 1"
    );
    let latest = sqlx::query_as::<_, ExamPolicyView>(&latest_sql)
        .bind(tenant.as_uuid())
        .bind(id)
        .bind(&code)
        .fetch_optional(&st.pool)
        .await
        .map_err(map_db_error)?;
    let name = match (b.name.as_deref(), latest.as_ref()) {
        (Some(n), _) => require_name("name", n)?,
        (None, Some(l)) => l.name.clone(),
        (None, None) => return Err(invalid("name", "required", "a name is required").into()),
    };
    if let Some(l) = latest.as_ref() {
        if matches!(
            l.state()?,
            ExamPolicyStatus::Draft | ExamPolicyStatus::UnderReview
        ) {
            return Err(KernelError::Conflict(format!(
                "policy {code} already has an open version {} ({}); edit that draft instead",
                l.version_no, l.status
            ))
            .into());
        }
    }
    let base = latest.as_ref().map(PolicyRules::from).unwrap_or_default();
    let r = merge(&base, None, &b)?;
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let new_id: Uuid = sqlx::query_scalar(
        "INSERT INTO exams.exam_policy (tenant_id, institution_id, code, version_no, name, \
         effective_from, registration_window_enforced, late_registration_allowed, late_fee_code, \
         late_fee_amount, require_dues_cleared, min_attendance_percent, max_attempts, \
         invigilator_ratio, hall_ticket_approval, self_service_allowed, supersedes_id, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18) \
         RETURNING id",
    )
    .bind(tenant.as_uuid())
    .bind(id)
    .bind(&code)
    .bind(latest.as_ref().map_or(1, |l| l.version_no + 1))
    .bind(&name)
    .bind(r.effective_from)
    .bind(r.registration_window_enforced)
    .bind(r.late_registration_allowed)
    .bind(r.late_fee_code.as_deref())
    .bind(r.late_fee_amount)
    .bind(r.require_dues_cleared)
    .bind(r.min_attendance_percent)
    .bind(r.max_attempts)
    .bind(r.invigilator_ratio)
    .bind(r.hall_ticket_approval)
    .bind(r.self_service_allowed)
    .bind(latest.as_ref().map(|l| l.id))
    .bind(act.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit(
        &mut tx,
        &scope,
        "exams.exam_policy.create",
        AGG,
        new_id.to_string(),
        json!({"code": code, "supersedes": latest.as_ref().map(|l| l.id)}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let p = load_policy(&st.pool, tenant, new_id, false).await?;
    Ok(view_response(&p, StatusCode::CREATED))
}

/// One policy version (`exam.read`).
#[utoipa::path(get, path = "/api/v1/exams/exam-policies/{id}", operation_id = "exam_policy_get", tag = "exam-setup",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ExamPolicyView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn policy_get(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamRead)?;
    let p = load_policy(&st.pool, ctx.scope.tenant_id, id, false).await?;
    Ok(view_response(&p, StatusCode::OK))
}

/// Edit a draft policy version (`exam.setup.manage`, `If-Match`).
#[utoipa::path(patch, path = "/api/v1/exams/exam-policies/{id}", operation_id = "exam_policy_update", tag = "exam-setup",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the policy")),
    request_body = ExamPolicyInput, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamPolicyView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn policy_update(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ExamPolicyInput>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamSetupManage)?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let tenant = ctx.scope.tenant_id;
    let current = load_policy(&st.pool, tenant, id, false).await?;
    let institution = InstitutionId(current.institution_id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    check_version(expected, current.version)?;
    if current.state()? != ExamPolicyStatus::Draft {
        return Err(KernelError::Conflict(format!(
            "policy {} v{} is {}; only a draft version can be edited — create the next version instead",
            current.code, current.version_no, current.status
        ))
        .into());
    }
    if b.code.as_deref().is_some_and(|c| c.trim() != current.code) {
        return Err(invalid("code", "immutable", "the policy code cannot change").into());
    }
    let name = match b.name.as_deref() {
        Some(n) => require_name("name", n)?,
        None => current.name.clone(),
    };
    let r = merge(&PolicyRules::from(&current), current.effective_from, &b)?;
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    sqlx::query(
        "UPDATE exams.exam_policy SET name = $3, effective_from = $4, \
         registration_window_enforced = $5, late_registration_allowed = $6, late_fee_code = $7, \
         late_fee_amount = $8, require_dues_cleared = $9, min_attendance_percent = $10, \
         max_attempts = $11, invigilator_ratio = $12, hall_ticket_approval = $13, \
         self_service_allowed = $14, version = version + 1, updated_at = now() \
         WHERE id = $1 AND tenant_id = $2 AND version = $15",
    )
    .bind(id)
    .bind(tenant.as_uuid())
    .bind(&name)
    .bind(r.effective_from)
    .bind(r.registration_window_enforced)
    .bind(r.late_registration_allowed)
    .bind(r.late_fee_code.as_deref())
    .bind(r.late_fee_amount)
    .bind(r.require_dues_cleared)
    .bind(r.min_attendance_percent)
    .bind(r.max_attempts)
    .bind(r.invigilator_ratio)
    .bind(r.hall_ticket_approval)
    .bind(r.self_service_allowed)
    .bind(current.version)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit(
        &mut tx,
        &scope,
        "exams.exam_policy.update",
        AGG,
        id.to_string(),
        json!({}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let p = load_policy(&st.pool, tenant, id, false).await?;
    Ok(view_response(&p, StatusCode::OK))
}

/// Locks the policy, checks version / institution / transition legality.
async fn begin_move(
    st: &ExamsState,
    ctx: &sutra_kernel::auth::AuthContext,
    id: Uuid,
    expected: Version,
    to: ExamPolicyStatus,
) -> Result<(ExamPolicyView, sutra_kernel::scope::ExecutionScope), KernelError> {
    let current = load_policy(&st.pool, ctx.scope.tenant_id, id, false).await?;
    let institution = InstitutionId(current.institution_id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(st, &scope, institution).await?;
    check_version(expected, current.version)?;
    let from = current.state()?;
    if !policy_can(from, to) {
        return Err(illegal(
            &format!("policy {} v{}", current.code, current.version_no),
            from.as_str(),
            to.as_str(),
        ));
    }
    Ok((current, scope))
}

fn mv<'a>(p: &'a ExamPolicyView, tenant: TenantId, version: i64, to: &'a str) -> Move<'a> {
    Move {
        table: TABLE,
        aggregate: AGG,
        has_process: true,
        tenant,
        institution: InstitutionId(p.institution_id),
        id: p.id,
        version,
        from: &p.status,
        to,
    }
}

/// Submit a draft policy for review (`exam.setup.manage`); `POLICY_APPROVAL` decides
/// (auto-approved in Batch 6).
#[utoipa::path(post, path = "/api/v1/exams/exam-policies/{id}/submit", operation_id = "exam_policy_submit", tag = "exam-setup",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the policy")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamPolicyView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn policy_submit(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamSetupManage)?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let reason = reason_of(body);
    let (p, scope) = begin_move(&st, ctx, id, expected, ExamPolicyStatus::UnderReview).await?;
    let tenant = ctx.scope.tenant_id;
    let outcome = request_approval(
        &st,
        &scope,
        process::POLICY_APPROVAL,
        business_key(AGG, id, p.version),
        BTreeMap::from([
            ("code".to_owned(), json!(p.code)),
            ("versionNo".to_owned(), json!(p.version_no)),
        ]),
    )
    .await?;
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let pid = outcome.process_instance_id().to_owned();
    let v = move_status(
        &mut tx,
        mv(&p, tenant, p.version, "under_review"),
        &StatusChange {
            cause_event_type: "exams.exam_policy.submit",
            reason: reason.as_deref(),
            process_instance_id: Some(&pid),
        },
        &act,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        let sys = Actor::system(act.correlation_id.clone());
        move_status(
            &mut tx,
            Move {
                from: "under_review",
                ..mv(&p, tenant, v, "approved")
            },
            &StatusChange {
                cause_event_type: "exams.exam_policy.decision",
                reason: Some("auto-approved"),
                process_instance_id: Some(&pid),
            },
            &sys,
        )
        .await?;
    }
    audit(
        &mut tx,
        &scope,
        "exams.exam_policy.submit",
        AGG,
        id.to_string(),
        json!({"processInstanceId": pid}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let p = load_policy(&st.pool, tenant, id, false).await?;
    Ok(view_response(&p, StatusCode::OK))
}

/// Decide a policy under review (`exam.approve`): approve → `approved`, reject → `draft`.
#[utoipa::path(post, path = "/api/v1/exams/exam-policies/{id}/decision", operation_id = "exam_policy_decision", tag = "exam-setup",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the policy")),
    request_body = ExamSetupDecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamPolicyView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn policy_decision(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ExamSetupDecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamApprove)?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let to = match b.decision {
        ExamSetupDecisionKind::Approve => ExamPolicyStatus::Approved,
        ExamSetupDecisionKind::Reject => ExamPolicyStatus::Draft,
    };
    let reason = clean(b.reason.as_deref());
    if to == ExamPolicyStatus::Draft {
        require_reason(&reason)?;
    }
    let current = load_policy(&st.pool, ctx.scope.tenant_id, id, false).await?;
    if current.state()? != ExamPolicyStatus::UnderReview {
        return Err(KernelError::Conflict(format!(
            "policy {} v{} is {}; only a policy under review can be decided",
            current.code, current.version_no, current.status
        ))
        .into());
    }
    simple_move(&st, ctx, id, expected, to, reason, None).await
}

/// Shared body of the plain moves (decision, supersede); `kind` = optional event.
async fn simple_move(
    st: &ExamsState,
    ctx: &sutra_kernel::auth::AuthContext,
    id: Uuid,
    expected: Version,
    to: ExamPolicyStatus,
    reason: Option<String>,
    kind: Option<(&'static str, &'static str)>,
) -> ApiResult<Response> {
    let (p, scope) = begin_move(st, ctx, id, expected, to).await?;
    let tenant = ctx.scope.tenant_id;
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let v = move_status(
        &mut tx,
        mv(&p, tenant, p.version, to.as_str()),
        &StatusChange {
            cause_event_type: kind.map_or("exams.exam_policy.move", |k| k.0),
            reason: reason.as_deref(),
            process_instance_id: None,
        },
        &act,
    )
    .await?;
    if let Some(k) = kind {
        let mut e = status_event(
            id,
            InstitutionId(p.institution_id),
            Some(&p.status),
            to.as_str(),
            v,
        );
        e.reason.clone_from(&reason);
        emit(&mut tx, &scope, k, &e, &act).await?;
    }
    audit(
        &mut tx,
        &scope,
        &format!("exams.exam_policy.{}", to.as_str()),
        AGG,
        id.to_string(),
        json!({"reason": reason}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let p = load_policy(&st.pool, tenant, id, false).await?;
    Ok(view_response(&p, StatusCode::OK))
}

/// Activate an approved / scheduled version (`exam.setup.manage`): a future `effectiveFrom`
/// schedules it; otherwise the institution's active version is superseded and this one becomes
/// the single authoritative policy.
#[utoipa::path(post, path = "/api/v1/exams/exam-policies/{id}/activate", operation_id = "exam_policy_activate", tag = "exam-setup",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the policy")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamPolicyView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn policy_activate(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamSetupManage)?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let reason = reason_of(body);
    let tenant = ctx.scope.tenant_id;
    let current = load_policy(&st.pool, tenant, id, false).await?;
    let institution = InstitutionId(current.institution_id);
    let scope = institution_scope(ctx, institution);
    let today = business_today(&st, &scope, institution).await?;
    let future = current.effective_from.is_some_and(|d| d > today);
    let to = if future && current.state()? == ExamPolicyStatus::Approved {
        ExamPolicyStatus::Scheduled
    } else {
        ExamPolicyStatus::Active
    };
    if to == ExamPolicyStatus::Active && future {
        return Err(KernelError::Conflict(format!(
            "policy {} v{} takes effect on {}; it cannot be activated earlier",
            current.code,
            current.version_no,
            current
                .effective_from
                .map(|d| d.to_string())
                .unwrap_or_default()
        ))
        .into());
    }
    let (p, scope) = begin_move(&st, ctx, id, expected, to).await?;
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    if to == ExamPolicyStatus::Active {
        if let Some(old) = active_policy(&mut *tx, tenant, institution).await? {
            let old = load_policy(&mut *tx, tenant, old.id, true).await?;
            let v = move_status(
                &mut tx,
                mv(&old, tenant, old.version, "superseded"),
                &StatusChange {
                    cause_event_type: EXAM_POLICY_SUPERSEDED,
                    reason: Some("superseded by a newer active version"),
                    process_instance_id: None,
                },
                &act,
            )
            .await?;
            let mut e = status_event(old.id, institution, Some("active"), "superseded", v);
            e.reason = Some(format!("superseded by {} v{}", p.code, p.version_no));
            emit(
                &mut tx,
                &scope,
                (EXAM_POLICY_SUPERSEDED, EXAM_POLICY_SUPERSEDED_SCHEMA),
                &e,
                &act,
            )
            .await?;
        }
    }
    let v = move_status(
        &mut tx,
        mv(&p, tenant, p.version, to.as_str()),
        &StatusChange {
            cause_event_type: EXAM_POLICY_ACTIVATED,
            reason: reason.as_deref(),
            process_instance_id: None,
        },
        &act,
    )
    .await?;
    if to == ExamPolicyStatus::Active {
        let mut e = status_event(id, institution, Some(&p.status), "active", v);
        e.reason.clone_from(&reason);
        emit(
            &mut tx,
            &scope,
            (EXAM_POLICY_ACTIVATED, EXAM_POLICY_ACTIVATED_SCHEMA),
            &e,
            &act,
        )
        .await?;
    }
    audit(
        &mut tx,
        &scope,
        "exams.exam_policy.activate",
        AGG,
        id.to_string(),
        json!({"to": to.as_str()}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let p = load_policy(&st.pool, tenant, id, false).await?;
    Ok(view_response(&p, StatusCode::OK))
}

/// Explicitly supersede the active version without activating another (`exam.setup.manage`,
/// reason required).
#[utoipa::path(post, path = "/api/v1/exams/exam-policies/{id}/supersede", operation_id = "exam_policy_supersede", tag = "exam-setup",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the policy")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamPolicyView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn policy_supersede(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamSetupManage)?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let reason = reason_of(body);
    require_reason(&reason)?;
    simple_move(
        &st,
        ctx,
        id,
        expected,
        ExamPolicyStatus::Superseded,
        reason,
        Some((EXAM_POLICY_SUPERSEDED, EXAM_POLICY_SUPERSEDED_SCHEMA)),
    )
    .await
}

/// Retire a policy version with its history preserved (`exam.setup.manage`, reason required).
/// A version still governing an operational session cannot be retired (`409`).
#[utoipa::path(post, path = "/api/v1/exams/exam-policies/{id}/retire", operation_id = "exam_policy_retire", tag = "exam-setup",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the policy")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamPolicyView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn policy_retire(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamSetupManage)?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let reason = reason_of(body);
    require_reason(&reason)?;
    let in_use: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM exams.exam_session WHERE tenant_id = $1 AND policy_id = $2 \
         AND status IN ('active', 'reopened', 'suspended')",
    )
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .fetch_one(&st.pool)
    .await
    .map_err(map_db_error)?;
    if in_use > 0 {
        return Err(KernelError::Conflict(format!(
            "the policy still governs {in_use} operational examination session(s)"
        ))
        .into());
    }
    simple_move(
        &st,
        ctx,
        id,
        expected,
        ExamPolicyStatus::Retired,
        reason,
        Some((EXAM_POLICY_RETIRED, EXAM_POLICY_RETIRED_SCHEMA)),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_validates_and_inherits() {
        let base = PolicyRules {
            max_attempts: 4,
            ..PolicyRules::default()
        };
        let r = merge(&base, None, &ExamPolicyInput::default()).map_err(|e| e.to_string());
        assert_eq!(r.as_ref().map(|r| r.max_attempts).ok(), Some(4));
        let bad = ExamPolicyInput {
            late_fee_amount: Some("100".into()),
            ..ExamPolicyInput::default()
        };
        assert!(merge(&base, None, &bad).is_err(), "fee needs a code");
        let pct = ExamPolicyInput {
            min_attendance_percent: Some("120".into()),
            ..ExamPolicyInput::default()
        };
        assert!(merge(&base, None, &pct).is_err());
    }
}
