//! HTTP API of payroll (tag `people-payroll`; contract §5.2): configuration versions, salary
//! structures and assignments, periods, runs, results, adjustments and final settlements.

use super::adjust::{self, AdjustmentLineInput};
use super::common::{
    money, opt_money, opt_reason, rate, Cmd, PayrollDecision, PayrollDecisionBody,
    PayrollReasonBody,
};
use super::config::{self, AssignmentInput, NewConfig, StructureInput};
use super::domain::{ComponentDef, Rounding, RuleDef};
use super::rows::{load_structure, AdjustmentRow, RunRow, SettlementRow};
use super::runs::{self, NewRun, OffCycleItem};
use super::settlement::{self, Recovery};
use super::views::*;
use crate::shared::guard::require_institution;
use crate::shared::http::{actor, if_match, with_etag};
use crate::shared::scope::institution_scope;
use crate::shared::{Actor, PeopleState};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use sutra_contracts::people::status::PayrollConfigStatus;
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::InstitutionId;
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// Reads the required `If-Match` (428 response when missing).
macro_rules! expected {
    ($headers:expr) => {
        match if_match(&$headers)? {
            Ok(v) => v.0,
            Err(resp) => return Ok(resp),
        }
    };
}
pub(crate) use expected;

pub(crate) fn cmd<'a>(st: &'a PeopleState, ctx: &AuthContext, actor: &'a Actor) -> Cmd<'a> {
    Cmd {
        st,
        tenant: ctx.scope.tenant_id,
        actor,
    }
}

pub(crate) fn reason_of(b: &Option<Json<PayrollReasonBody>>) -> Option<&str> {
    b.as_ref().and_then(|j| j.0.reason.as_deref())
}

async fn config_out(
    st: &PeopleState,
    ctx: &AuthContext,
    id: Uuid,
    status: StatusCode,
) -> ApiResult<Response> {
    let d = config::config_detail(st, ctx.scope.tenant_id, id).await?;
    Ok(with_etag(
        status,
        &PayrollConfigView::of(&d.config, &d.components, &d.rules),
        Version(d.config.version),
    ))
}

fn run_res(status: StatusCode, r: &RunRow) -> Response {
    with_etag(status, &PayrollRunView::from(r), Version(r.version))
}

fn adj_res(status: StatusCode, a: &AdjustmentRow) -> Response {
    with_etag(status, &PayrollAdjustmentView::from(a), Version(a.version))
}

fn fnf_res(status: StatusCode, s: &SettlementRow) -> Response {
    with_etag(status, &FinalSettlementView::from(s), Version(s.version))
}

// ================================================================== configuration

/// Payroll configuration versions of an institution (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/institutions/{id}/payroll-configs", operation_id = "people_payroll_config_list", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid), PayrollConfigQuery), security(("session_cookie" = [])),
    responses((status = 200, body = PayrollConfigList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn config_list(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<PayrollConfigQuery>,
) -> ApiResult<Json<PayrollConfigList>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let inst = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    let status = match q.status.as_deref() {
        None => None,
        Some(s) => Some(PayrollConfigStatus::parse(s).ok_or_else(|| {
            super::common::invalid("status", "invalid", "unknown configuration status")
        })?),
    };
    let items = config::list_configs_with_components(&st, ctx.scope.tenant_id, inst, status)
        .await?
        .iter()
        .map(|(c, comps)| PayrollConfigView::of(c, comps, &[]))
        .collect();
    Ok(Json(PayrollConfigList { items }))
}

/// Create a configuration version in `draft` (`people.payroll.manage`; L3-E-074).
#[utoipa::path(post, path = "/api/v1/people/institutions/{id}/payroll-configs", operation_id = "people_payroll_config_create", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = PayrollConfigCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = PayrollConfigView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn config_create(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<PayrollConfigCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PayrollManage)?;
    require_idempotency_key(&headers)?;
    let a = actor(ctx);
    let c = config::create_config(
        &cmd(&st, ctx, &a),
        InstitutionId(id),
        NewConfig {
            name: b.name,
            effective_from: b.effective_from,
            rounding: b.rounding.unwrap_or(Rounding::Round2),
            encashment_divisor: b.encashment_divisor.unwrap_or(30),
            copy_from: b.copy_from,
        },
    )
    .await?;
    config_out(&st, ctx, c.id, StatusCode::CREATED).await
}

/// One configuration version with its components and statutory rules (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/payroll-configs/{id}", operation_id = "people_payroll_config_get", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = PayrollConfigView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn config_get(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleRead)?;
    config_out(&st, ctx, id, StatusCode::OK).await
}

/// Components of a configuration version (the configuration detail; `people.read`).
#[utoipa::path(get, path = "/api/v1/people/payroll-configs/{id}/components", operation_id = "people_pay_component_list", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = PayrollConfigView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn component_list(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleRead)?;
    config_out(&st, ctx, id, StatusCode::OK).await
}

/// Add or replace (by code) a pay component on a draft (`people.payroll.manage`).
#[utoipa::path(post, path = "/api/v1/people/payroll-configs/{id}/components", operation_id = "people_pay_component_put", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the configuration")),
    request_body = PayComponentInput, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = PayrollConfigView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn component_put(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<PayComponentInput>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PayrollManage)?;
    let v = expected!(headers);
    let a = actor(ctx);
    let d = ComponentDef {
        code: b.code.trim().to_owned(),
        name: b.name,
        kind: b.kind,
        basis: b.basis,
        amount: opt_money("amount", b.amount.as_deref())?,
        rate: rate("rate", b.rate.as_deref())?,
        sequence: b.sequence.unwrap_or(100),
    };
    config::put_component(&cmd(&st, ctx, &a), id, v, d).await?;
    config_out(&st, ctx, id, StatusCode::OK).await
}

/// Statutory rules of a configuration version (the configuration detail; `people.read`).
#[utoipa::path(get, path = "/api/v1/people/payroll-configs/{id}/statutory-rules", operation_id = "people_payroll_statutory_rule_list", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = PayrollConfigView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn rule_list(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleRead)?;
    config_out(&st, ctx, id, StatusCode::OK).await
}

/// Add or replace (by kind) a statutory rule on a draft (`people.payroll.manage`): PF / ESI /
/// professional tax / TDS.
#[utoipa::path(post, path = "/api/v1/people/payroll-configs/{id}/statutory-rules", operation_id = "people_payroll_statutory_rule_put", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the configuration")),
    request_body = StatutoryRuleInput, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = PayrollConfigView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn rule_put(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<StatutoryRuleInput>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PayrollManage)?;
    let v = expected!(headers);
    let a = actor(ctx);
    let d = RuleDef {
        kind: b.kind,
        employee_rate: rate("employeeRate", b.employee_rate.as_deref())?,
        employer_rate: rate("employerRate", b.employer_rate.as_deref())?,
        wage_ceiling: opt_money("wageCeiling", b.wage_ceiling.as_deref())?,
        threshold: opt_money("threshold", b.threshold.as_deref())?,
        slabs: b.slabs.unwrap_or_default(),
        fixed_amount: opt_money("fixedAmount", b.fixed_amount.as_deref())?,
        employee_code: b.employee_component_code.trim().to_owned(),
        employer_code: b
            .employer_component_code
            .map(|x| x.trim().to_owned())
            .filter(|x| !x.is_empty()),
    };
    config::put_rule(&cmd(&st, ctx, &a), id, v, d).await?;
    config_out(&st, ctx, id, StatusCode::OK).await
}

/// Submit a draft for review (`people.payroll.manage`; validation 422,
/// `PAYROLL_CONFIG_APPROVAL`).
#[utoipa::path(post, path = "/api/v1/people/payroll-configs/{id}/submit", operation_id = "people_payroll_config_submit", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the configuration")),
    request_body = PayrollReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = PayrollConfigView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn config_submit(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    b: Option<Json<PayrollReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PayrollManage)?;
    let v = expected!(headers);
    let a = actor(ctx);
    config::submit_config(&cmd(&st, ctx, &a), id, v, reason_of(&b)).await?;
    config_out(&st, ctx, id, StatusCode::OK).await
}

/// Decide a configuration under review (`people.approve`, step-up).
#[utoipa::path(post, path = "/api/v1/people/payroll-configs/{id}/decision", operation_id = "people_payroll_config_decide", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the configuration")),
    request_body = PayrollDecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = PayrollConfigView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn config_decide(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<PayrollDecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleApprove)?;
    // Part 1 · S3-appr (DEF-X-002): the requester of the pending request never decides it.
    sutra_persistence::approval::guard_decision(&st.pool, ctx, &id, "payroll configuration")
        .await?;
    let v = expected!(headers);
    let a = actor(ctx);
    config::decide_config(
        &cmd(&st, ctx, &a),
        id,
        v,
        b.decision == PayrollDecision::Approve,
        b.reason.as_deref(),
    )
    .await?;
    config_out(&st, ctx, id, StatusCode::OK).await
}

/// Activate an approved configuration (`people.approve`, step-up); the active one is
/// superseded.
#[utoipa::path(post, path = "/api/v1/people/payroll-configs/{id}/activate", operation_id = "people_payroll_config_activate", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the configuration")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = PayrollConfigView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn config_activate(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleApprove)?;
    let v = expected!(headers);
    let a = actor(ctx);
    config::activate_config(&cmd(&st, ctx, &a), id, v).await?;
    config_out(&st, ctx, id, StatusCode::OK).await
}

/// Supersede an active / approved configuration (`people.payroll.manage`; reason required).
#[utoipa::path(post, path = "/api/v1/people/payroll-configs/{id}/supersede", operation_id = "people_payroll_config_supersede", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the configuration")),
    request_body = PayrollReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = PayrollConfigView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn config_supersede(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    b: Option<Json<PayrollReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PayrollManage)?;
    let v = expected!(headers);
    let a = actor(ctx);
    config::supersede_config(&cmd(&st, ctx, &a), id, v, reason_of(&b)).await?;
    config_out(&st, ctx, id, StatusCode::OK).await
}

// ================================================================== structures / assignments

/// Salary structures of an institution (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/institutions/{id}/salary-structures", operation_id = "people_salary_structure_list", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = SalaryStructureList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn structure_list(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<SalaryStructureList>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let inst = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    let items = config::list_structures(&st, ctx.scope.tenant_id, inst)
        .await?
        .iter()
        .map(SalaryStructureView::from)
        .collect();
    Ok(Json(SalaryStructureList { items }))
}

/// Create a salary structure for a grade (`people.payroll.manage`).
#[utoipa::path(post, path = "/api/v1/people/institutions/{id}/salary-structures", operation_id = "people_salary_structure_create", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = SalaryStructureCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = SalaryStructureView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn structure_create(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<SalaryStructureCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PayrollManage)?;
    require_idempotency_key(&headers)?;
    let a = actor(ctx);
    let s = config::create_structure(
        &cmd(&st, ctx, &a),
        InstitutionId(id),
        StructureInput {
            code: b.code.trim().to_owned(),
            name: b.name,
            grade: b.grade.filter(|g| !g.trim().is_empty()),
            lines: structure_lines_in(&b.lines)?,
        },
    )
    .await?;
    Ok(with_etag(
        StatusCode::CREATED,
        &SalaryStructureView::from(&s),
        Version(s.version),
    ))
}

/// One salary structure (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/salary-structures/{id}", operation_id = "people_salary_structure_get", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = SalaryStructureView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn structure_get(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let s = load_structure(&mut conn, ctx.scope.tenant_id, id, false).await?;
    Ok(with_etag(
        StatusCode::OK,
        &SalaryStructureView::from(&s),
        Version(s.version),
    ))
}

/// Replace a structure's name, grade and lines (`people.payroll.manage`; `If-Match`).
#[utoipa::path(put, path = "/api/v1/people/salary-structures/{id}", operation_id = "people_salary_structure_update", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the structure")),
    request_body = SalaryStructureUpdate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = SalaryStructureView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn structure_update(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<SalaryStructureUpdate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PayrollManage)?;
    let v = expected!(headers);
    let a = actor(ctx);
    let s = config::update_structure(
        &cmd(&st, ctx, &a),
        id,
        v,
        StructureInput {
            code: String::new(),
            name: b.name,
            grade: b.grade.filter(|g| !g.trim().is_empty()),
            lines: structure_lines_in(&b.lines)?,
        },
    )
    .await?;
    Ok(with_etag(
        StatusCode::OK,
        &SalaryStructureView::from(&s),
        Version(s.version),
    ))
}

/// Salary assignments of an engagement (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/engagements/{id}/salary-assignments", operation_id = "people_salary_assignment_list", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = SalaryAssignmentList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn assignment_list(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<SalaryAssignmentList>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let items = config::list_assignments(&st, ctx.scope.tenant_id, id)
        .await?
        .iter()
        .map(SalaryAssignmentView::from)
        .collect();
    Ok(Json(SalaryAssignmentList { items }))
}

/// Assign a salary structure to a live engagement from a date (`people.payroll.manage`;
/// overlapping assignments → 409).
#[utoipa::path(post, path = "/api/v1/people/engagements/{id}/salary-assignments", operation_id = "people_salary_assignment_create", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = SalaryAssignmentCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = SalaryAssignmentView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn assignment_create(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<SalaryAssignmentCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PayrollManage)?;
    require_idempotency_key(&headers)?;
    let a = actor(ctx);
    let r = config::create_assignment(
        &cmd(&st, ctx, &a),
        id,
        AssignmentInput {
            structure_id: b.structure_id,
            effective_from: b.effective_from,
            effective_to: b.effective_to,
            reason: opt_reason(b.reason.as_deref()),
        },
    )
    .await?;
    Ok(with_etag(
        StatusCode::CREATED,
        &SalaryAssignmentView::from(&r),
        Version(r.version),
    ))
}

/// One salary assignment (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/salary-assignments/{id}", operation_id = "people_salary_assignment_get", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = SalaryAssignmentView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn assignment_get(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let r = config::load_assignment(&mut conn, ctx.scope.tenant_id, id).await?;
    Ok(with_etag(
        StatusCode::OK,
        &SalaryAssignmentView::from(&r),
        Version(r.version),
    ))
}

/// End a salary assignment on a date (`people.payroll.manage`; may lie in a finalised period —
/// a retro run pays the difference).
#[utoipa::path(post, path = "/api/v1/people/salary-assignments/{id}/end", operation_id = "people_salary_assignment_end", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the assignment")),
    request_body = SalaryAssignmentEnd, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = SalaryAssignmentView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn assignment_end(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<SalaryAssignmentEnd>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PayrollManage)?;
    let v = expected!(headers);
    let a = actor(ctx);
    let r =
        config::end_assignment(&cmd(&st, ctx, &a), id, v, b.end_date, b.reason.as_deref()).await?;
    Ok(with_etag(
        StatusCode::OK,
        &SalaryAssignmentView::from(&r),
        Version(r.version),
    ))
}

// ================================================================== periods / runs

/// Payroll periods of an institution (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/institutions/{id}/payroll-periods", operation_id = "people_payroll_period_list", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = PayrollPeriodList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn period_list(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<PayrollPeriodList>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let inst = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    let items = config::list_periods(&st, ctx.scope.tenant_id, inst)
        .await?
        .iter()
        .map(PayrollPeriodView::from)
        .collect();
    Ok(Json(PayrollPeriodList { items }))
}

/// Open the payroll period of a month (`people.payroll.manage`; duplicate → 409).
#[utoipa::path(post, path = "/api/v1/people/institutions/{id}/payroll-periods", operation_id = "people_payroll_period_create", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = PayrollPeriodCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = PayrollPeriodView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn period_create(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<PayrollPeriodCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PayrollManage)?;
    require_idempotency_key(&headers)?;
    let a = actor(ctx);
    let p = config::create_period(&cmd(&st, ctx, &a), InstitutionId(id), b.year, b.month).await?;
    Ok(with_etag(
        StatusCode::CREATED,
        &PayrollPeriodView::from(&p),
        Version(p.version),
    ))
}

/// Runs of a period (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/payroll-periods/{id}/runs", operation_id = "people_payroll_run_list", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = PayrollRunList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn run_list(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<PayrollRunList>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let items = runs::list_runs(&st, ctx.scope.tenant_id, id)
        .await?
        .iter()
        .map(PayrollRunView::from)
        .collect();
    Ok(Json(PayrollRunList { items }))
}

/// Create a run of a period (`people.payroll.manage`): `regular` (one per period), `retro`
/// (after a finalised regular run) or `off_cycle` (items).
#[utoipa::path(post, path = "/api/v1/people/payroll-periods/{id}/runs", operation_id = "people_payroll_run_create", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = PayrollRunCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = PayrollRunView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn run_create(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<PayrollRunCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PayrollManage)?;
    require_idempotency_key(&headers)?;
    let a = actor(ctx);
    let items = b
        .items
        .unwrap_or_default()
        .into_iter()
        .map(|i| {
            Ok(OffCycleItem {
                engagement_id: i.engagement_id,
                component_code: i.component_code.trim().to_owned(),
                amount: money("items.amount", &i.amount)?,
                note: opt_reason(i.note.as_deref()),
            })
        })
        .collect::<Result<Vec<_>, KernelError>>()?;
    let r = runs::create_run(
        &cmd(&st, ctx, &a),
        id,
        NewRun {
            kind: b.kind,
            description: b.description,
            items,
        },
    )
    .await?;
    Ok(run_res(StatusCode::CREATED, &r))
}

/// One run with totals (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/payroll-runs/{id}", operation_id = "people_payroll_run_get", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = PayrollRunView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn run_get(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let r = runs::get_run(&st, ctx.scope.tenant_id, id).await?;
    Ok(run_res(StatusCode::OK, &r))
}

/// Calculate a run (`people.payroll.manage`; bulk: `If-Match` + `Idempotency-Key`).
#[utoipa::path(post, path = "/api/v1/people/payroll-runs/{id}/calculate", operation_id = "people_payroll_run_calculate", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the run"), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = PayrollRunView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn run_calculate(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PayrollManage)?;
    let v = expected!(headers);
    require_idempotency_key(&headers)?;
    let a = actor(ctx);
    let r = runs::calculate_run(&cmd(&st, ctx, &a), id, v).await?;
    Ok(run_res(StatusCode::OK, &r))
}

/// Validate a calculated run (`people.payroll.manage`): exceptions → `failed`.
#[utoipa::path(post, path = "/api/v1/people/payroll-runs/{id}/validate", operation_id = "people_payroll_run_validate", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the run")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = PayrollRunView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn run_validate(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PayrollManage)?;
    let v = expected!(headers);
    let a = actor(ctx);
    let r = runs::validate_run(&cmd(&st, ctx, &a), id, v).await?;
    Ok(run_res(StatusCode::OK, &r))
}

/// Submit a validated run for approval (`people.payroll.manage`; `PAYROLL_APPROVAL` /
/// `OFF_CYCLE_APPROVAL`).
#[utoipa::path(post, path = "/api/v1/people/payroll-runs/{id}/submit", operation_id = "people_payroll_run_submit", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the run")),
    request_body = PayrollReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = PayrollRunView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn run_submit(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    b: Option<Json<PayrollReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PayrollManage)?;
    let v = expected!(headers);
    let a = actor(ctx);
    let r = runs::submit_run(&cmd(&st, ctx, &a), id, v, reason_of(&b)).await?;
    Ok(run_res(StatusCode::OK, &r))
}

/// Decide a pending run (`people.payroll.approve`, step-up).
#[utoipa::path(post, path = "/api/v1/people/payroll-runs/{id}/decision", operation_id = "people_payroll_run_decide", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the run")),
    request_body = PayrollDecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = PayrollRunView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn run_decide(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<PayrollDecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PayrollApprove)?;
    let v = expected!(headers);
    let a = actor(ctx);
    let r = runs::decide_run(
        &cmd(&st, ctx, &a),
        id,
        v,
        b.decision == PayrollDecision::Approve,
        b.reason.as_deref(),
    )
    .await?;
    Ok(run_res(StatusCode::OK, &r))
}

/// Finalise an approved run (`people.payroll.approve`, step-up): results immutable.
#[utoipa::path(post, path = "/api/v1/people/payroll-runs/{id}/finalise", operation_id = "people_payroll_run_finalise", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the run")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = PayrollRunView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn run_finalise(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PayrollApprove)?;
    let v = expected!(headers);
    let a = actor(ctx);
    let r = runs::finalise_run(&cmd(&st, ctx, &a), id, v).await?;
    Ok(run_res(StatusCode::OK, &r))
}

/// Post a finalised run's liability to Finance (`people.payroll.approve`, step-up).
#[utoipa::path(post, path = "/api/v1/people/payroll-runs/{id}/post", operation_id = "people_payroll_run_post", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the run")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = PayrollRunView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn run_post(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PayrollApprove)?;
    let v = expected!(headers);
    let a = actor(ctx);
    let r = runs::post_run(&cmd(&st, ctx, &a), id, v).await?;
    Ok(run_res(StatusCode::OK, &r))
}

/// Close a posted run (`people.payroll.manage`).
#[utoipa::path(post, path = "/api/v1/people/payroll-runs/{id}/close", operation_id = "people_payroll_run_close", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the run")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = PayrollRunView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn run_close(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PayrollManage)?;
    let v = expected!(headers);
    let a = actor(ctx);
    let r = runs::close_run(&cmd(&st, ctx, &a), id, v).await?;
    Ok(run_res(StatusCode::OK, &r))
}

/// Reopen a failed run for recalculation (`people.payroll.manage`).
#[utoipa::path(post, path = "/api/v1/people/payroll-runs/{id}/reprocess", operation_id = "people_payroll_run_reprocess", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the run")),
    request_body = PayrollReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = PayrollRunView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn run_reprocess(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    b: Option<Json<PayrollReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PayrollManage)?;
    let v = expected!(headers);
    let a = actor(ctx);
    let r = runs::reprocess_run(&cmd(&st, ctx, &a), id, v, reason_of(&b)).await?;
    Ok(run_res(StatusCode::OK, &r))
}

/// Results of a run (`people.read`; `?limit=`, `?cursor=`, `?includeSuperseded=`).
#[utoipa::path(get, path = "/api/v1/people/payroll-runs/{id}/results", operation_id = "people_payroll_result_list", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid), PayrollResultQuery), security(("session_cookie" = [])),
    responses((status = 200, body = PayrollResultList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn result_list(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<PayrollResultQuery>,
) -> ApiResult<Json<PayrollResultList>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let (rows, next_cursor) = runs::list_results(
        &st,
        ctx.scope.tenant_id,
        id,
        q.include_superseded.unwrap_or(false),
        limit,
        q.cursor.as_deref(),
    )
    .await?;
    Ok(Json(PayrollResultList {
        items: rows.iter().map(PayrollResultView::from).collect(),
        next_cursor,
    }))
}

/// One result with its lines and calculation snapshot (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/payroll-results/{id}", operation_id = "people_payroll_result_get", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = PayrollResultView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn result_get(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let r = runs::get_result(&st, ctx.scope.tenant_id, id).await?;
    Ok(with_etag(
        StatusCode::OK,
        &PayrollResultView::from(&r),
        Version(r.version),
    ))
}

// ================================================================== adjustments

/// Adjustments raised on a result's chain (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/payroll-results/{id}/adjustments", operation_id = "people_payroll_adjustment_list", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = PayrollAdjustmentList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn adjustment_list(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<PayrollAdjustmentList>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let items = adjust::list_adjustments(&st, ctx.scope.tenant_id, id)
        .await?
        .iter()
        .map(PayrollAdjustmentView::from)
        .collect();
    Ok(Json(PayrollAdjustmentList { items }))
}

/// Raise an adjustment on a finalised result (`people.payroll.manage`; L3-E-054).
#[utoipa::path(post, path = "/api/v1/people/payroll-results/{id}/adjustments", operation_id = "people_payroll_adjustment_create", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = PayrollAdjustmentCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = PayrollAdjustmentView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn adjustment_create(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<PayrollAdjustmentCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PayrollManage)?;
    require_idempotency_key(&headers)?;
    let a = actor(ctx);
    let lines = b
        .lines
        .iter()
        .map(|l| {
            Ok(AdjustmentLineInput {
                code: l.code.clone(),
                kind: l.kind,
                name: l.name.clone(),
                amount: money("lines.amount", &l.amount)?,
            })
        })
        .collect::<Result<Vec<_>, KernelError>>()?;
    let r = adjust::create_adjustment(&cmd(&st, ctx, &a), id, Some(&b.reason), lines).await?;
    Ok(adj_res(StatusCode::CREATED, &r))
}

/// One adjustment (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/payroll-adjustments/{id}", operation_id = "people_payroll_adjustment_get", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = PayrollAdjustmentView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn adjustment_get(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let a = adjust::get_adjustment(&st, ctx.scope.tenant_id, id).await?;
    Ok(adj_res(StatusCode::OK, &a))
}

/// Validate and submit an adjustment (`people.payroll.manage`; `PAYROLL_ADJUSTMENT_APPROVAL`).
#[utoipa::path(post, path = "/api/v1/people/payroll-adjustments/{id}/validate", operation_id = "people_payroll_adjustment_validate", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the adjustment")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = PayrollAdjustmentView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn adjustment_validate(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PayrollManage)?;
    let v = expected!(headers);
    let a = actor(ctx);
    let r = adjust::validate_adjustment(&cmd(&st, ctx, &a), id, v).await?;
    Ok(adj_res(StatusCode::OK, &r))
}

/// Decide a pending adjustment (`people.payroll.approve`, step-up).
#[utoipa::path(post, path = "/api/v1/people/payroll-adjustments/{id}/decision", operation_id = "people_payroll_adjustment_decide", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the adjustment")),
    request_body = PayrollDecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = PayrollAdjustmentView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn adjustment_decide(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<PayrollDecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PayrollApprove)?;
    let v = expected!(headers);
    let a = actor(ctx);
    let r = adjust::decide_adjustment(
        &cmd(&st, ctx, &a),
        id,
        v,
        b.decision == PayrollDecision::Approve,
        b.reason.as_deref(),
    )
    .await?;
    Ok(adj_res(StatusCode::OK, &r))
}

/// Apply an approved adjustment (`people.payroll.manage`): corrective revision + Finance delta.
#[utoipa::path(post, path = "/api/v1/people/payroll-adjustments/{id}/apply", operation_id = "people_payroll_adjustment_apply", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the adjustment")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = PayrollAdjustmentView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn adjustment_apply(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PayrollManage)?;
    let v = expected!(headers);
    let a = actor(ctx);
    let r = adjust::apply_adjustment(&cmd(&st, ctx, &a), id, v).await?;
    Ok(adj_res(StatusCode::OK, &r))
}

// ================================================================== final settlements

/// Final settlements of an institution (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/institutions/{id}/final-settlements", operation_id = "people_settlement_list", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = FinalSettlementList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn settlement_list(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<FinalSettlementList>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let inst = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    let items = settlement::list_settlements(&st, ctx.scope.tenant_id, inst)
        .await?
        .iter()
        .map(FinalSettlementView::from)
        .collect();
    Ok(Json(FinalSettlementList { items }))
}

/// The final settlement of an engagement (`people.read`; 404 before initiation).
#[utoipa::path(get, path = "/api/v1/people/engagements/{id}/final-settlement", operation_id = "people_settlement_of_engagement", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = FinalSettlementView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn settlement_of_engagement(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let s = settlement::settlement_of_engagement(&st, ctx.scope.tenant_id, id).await?;
    Ok(fnf_res(StatusCode::OK, &s))
}

/// Initiate the final settlement of a closed engagement (`people.payroll.manage`; L3-E-056).
#[utoipa::path(post, path = "/api/v1/people/engagements/{id}/final-settlement", operation_id = "people_settlement_create", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = FinalSettlementCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = FinalSettlementView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn settlement_create(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<FinalSettlementCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PayrollManage)?;
    require_idempotency_key(&headers)?;
    let a = actor(ctx);
    let recoveries = b
        .recoveries
        .unwrap_or_default()
        .into_iter()
        .map(|r| {
            Ok(Recovery {
                code: r.code.trim().to_owned(),
                name: r.name.trim().to_owned(),
                amount: money("recoveries.amount", &r.amount)?,
            })
        })
        .collect::<Result<Vec<_>, KernelError>>()?;
    let s = settlement::create_settlement(&cmd(&st, ctx, &a), id, recoveries, b.notes.as_deref())
        .await?;
    Ok(fnf_res(StatusCode::CREATED, &s))
}

/// One final settlement (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/final-settlements/{id}", operation_id = "people_settlement_get", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = FinalSettlementView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn settlement_get(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let s = settlement::get_settlement(&st, ctx.scope.tenant_id, id).await?;
    Ok(fnf_res(StatusCode::OK, &s))
}

/// Calculate a settlement and submit it for approval (`people.payroll.manage`;
/// `FINAL_SETTLEMENT_APPROVAL`).
#[utoipa::path(post, path = "/api/v1/people/final-settlements/{id}/calculate", operation_id = "people_settlement_calculate", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the settlement")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = FinalSettlementView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn settlement_calculate(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PayrollManage)?;
    let v = expected!(headers);
    let a = actor(ctx);
    let s = settlement::calculate_settlement(&cmd(&st, ctx, &a), id, v).await?;
    Ok(fnf_res(StatusCode::OK, &s))
}

/// Decide a pending settlement (`people.payroll.approve`, step-up).
#[utoipa::path(post, path = "/api/v1/people/final-settlements/{id}/decision", operation_id = "people_settlement_decide", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the settlement")),
    request_body = PayrollDecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = FinalSettlementView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn settlement_decide(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<PayrollDecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PayrollApprove)?;
    let v = expected!(headers);
    let a = actor(ctx);
    let s = settlement::decide_settlement(
        &cmd(&st, ctx, &a),
        id,
        v,
        b.decision == PayrollDecision::Approve,
        b.reason.as_deref(),
    )
    .await?;
    Ok(fnf_res(StatusCode::OK, &s))
}

/// Complete an approved settlement (`people.payroll.manage`): finalise, post, `FNF-` number.
#[utoipa::path(post, path = "/api/v1/people/final-settlements/{id}/complete", operation_id = "people_settlement_complete", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the settlement")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = FinalSettlementView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn settlement_complete(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PayrollManage)?;
    let v = expected!(headers);
    let a = actor(ctx);
    let s = settlement::complete_settlement(&cmd(&st, ctx, &a), id, v).await?;
    Ok(fnf_res(StatusCode::OK, &s))
}

pub fn router(state: PeopleState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(config_list, config_create))
        .routes(routes!(config_get))
        .routes(routes!(component_list, component_put))
        .routes(routes!(rule_list, rule_put))
        .routes(routes!(config_submit))
        .routes(routes!(config_decide))
        .routes(routes!(config_activate))
        .routes(routes!(config_supersede))
        .routes(routes!(structure_list, structure_create))
        .routes(routes!(structure_get, structure_update))
        .routes(routes!(assignment_list, assignment_create))
        .routes(routes!(assignment_get))
        .routes(routes!(assignment_end))
        .routes(routes!(period_list, period_create))
        .routes(routes!(run_list, run_create))
        .routes(routes!(run_get))
        .routes(routes!(run_calculate))
        .routes(routes!(run_validate))
        .routes(routes!(run_submit))
        .routes(routes!(run_decide))
        .routes(routes!(run_finalise))
        .routes(routes!(run_post))
        .routes(routes!(run_close))
        .routes(routes!(run_reprocess))
        .routes(routes!(result_list))
        .routes(routes!(result_get))
        .routes(routes!(adjustment_list, adjustment_create))
        .routes(routes!(adjustment_get))
        .routes(routes!(adjustment_validate))
        .routes(routes!(adjustment_decide))
        .routes(routes!(adjustment_apply))
        .routes(routes!(settlement_list))
        .routes(routes!(settlement_of_engagement, settlement_create))
        .routes(routes!(settlement_get))
        .routes(routes!(settlement_calculate))
        .routes(routes!(settlement_decide))
        .routes(routes!(settlement_complete))
        .with_state(state)
}
