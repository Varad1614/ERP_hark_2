//! HTTP API of governed finance configuration and financial rules (tag `finance-config`).

use super::repo::{self, Kind, PolicyVersionView};
use crate::charges::kit::{
    if_match, opt_text, require_text, DecisionBody, DecisionKind, ReasonBody,
};
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, with_etag};
use crate::shared::scope::institution_scope;
use crate::shared::FinanceState;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use serde::{Deserialize, Serialize};
use sutra_kernel::auth::Permission;
use sutra_kernel::concurrency::Version;
use sutra_kernel::scope::InstitutionId;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PolicyVersionList {
    pub items: Vec<PolicyVersionView>,
}

/// A new configuration version (`values` = settings: `defaultDueDays`, `maxInstalments`,
/// `lateFeeGraceDays`, `allowPartialPayments`, `studentDisputesEnabled`, `receiptFooter`).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConfigCreate {
    pub values: serde_json::Value,
    pub change_note: Option<String>,
}

/// A new rule version; decimals as strings:
/// * `late_fee` {`mode` flat | percent | per_day | slabs | none, `amount`, `percent`,
///   `frequency` once | monthly, `graceDays`, `cap`, `capPercent`, `slabs` [{`fromDay`, `amount`}],
///   `roundToRupee`, `minOverdue`, `feeHeadCode`} (the 5A shape {percent, graceDays, cap} is a
///   one-off percentage);
/// * `reminder_schedule` {`beforeDays` [7, 1], `afterDays` [1, 15, 30], `lateFromDays`};
/// * `rounding` {mode}, `instalment_policy` {maxInstalments, minAmount}, `refund_window` {days},
///   `write_off_threshold` {amount}.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RuleCreate {
    pub rule_key: String,
    pub values: serde_json::Value,
    pub change_note: Option<String>,
}

#[derive(Debug, Clone, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
pub struct RuleQuery {
    pub rule_key: Option<String>,
}

fn ok(v: &PolicyVersionView, status: StatusCode) -> Response {
    with_etag(status, v, Version(v.version))
}

async fn list(
    st: &FinanceState,
    auth: &Authenticated,
    kind: Kind,
    id: Uuid,
    rule_key: Option<&str>,
) -> ApiResult<Json<PolicyVersionList>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let scope = institution_scope(ctx, InstitutionId(id));
    require_institution(st, &scope, InstitutionId(id)).await?;
    let items = repo::list(st, kind, ctx.scope.tenant_id, InstitutionId(id), rule_key).await?;
    Ok(Json(PolicyVersionList { items }))
}

#[allow(clippy::too_many_arguments)]
async fn create(
    st: &FinanceState,
    auth: &Authenticated,
    headers: &HeaderMap,
    kind: Kind,
    id: Uuid,
    rule_key: Option<&str>,
    values: serde_json::Value,
    note: Option<String>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FinanceConfigManage)?;
    require_idempotency_key(headers)?;
    let scope = institution_scope(ctx, InstitutionId(id));
    require_institution_writable(st, &scope, InstitutionId(id)).await?;
    let v = repo::create(
        st,
        kind,
        &scope,
        InstitutionId(id),
        rule_key,
        values,
        opt_text(note.as_deref()),
        &actor(ctx),
    )
    .await?;
    Ok(ok(&v, StatusCode::CREATED))
}

/// Configuration versions of an institution, newest first (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/institutions/{id}/finance-config", operation_id = "finance_config_list", tag = "finance-config",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = PolicyVersionList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn config_list(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<PolicyVersionList>> {
    list(&st, &auth, Kind::Config, id, None).await
}

/// Draft a new configuration version (`finance.config.manage`).
#[utoipa::path(post, path = "/api/v1/finance/institutions/{id}/finance-config", operation_id = "finance_config_create", tag = "finance-config",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = ConfigCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = PolicyVersionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn config_create(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ConfigCreate>,
) -> ApiResult<Response> {
    create(
        &st,
        &auth,
        &headers,
        Kind::Config,
        id,
        None,
        b.values,
        b.change_note,
    )
    .await
}

/// Financial rule versions of an institution (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/institutions/{id}/finance-rules", operation_id = "finance_rule_list", tag = "finance-config",
    params(("id" = String, Path, format = Uuid), RuleQuery), security(("session_cookie" = [])),
    responses((status = 200, body = PolicyVersionList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn rule_list(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<RuleQuery>,
) -> ApiResult<Json<PolicyVersionList>> {
    list(&st, &auth, Kind::Rule, id, q.rule_key.as_deref()).await
}

/// Draft a new financial rule version (`finance.config.manage`).
#[utoipa::path(post, path = "/api/v1/finance/institutions/{id}/finance-rules", operation_id = "finance_rule_create", tag = "finance-config",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = RuleCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = PolicyVersionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn rule_create(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<RuleCreate>,
) -> ApiResult<Response> {
    create(
        &st,
        &auth,
        &headers,
        Kind::Rule,
        id,
        Some(b.rule_key.trim()),
        b.values,
        b.change_note,
    )
    .await
}

macro_rules! policy_routes {
    ($kind:expr, $_tagdoc:literal,
     $get:ident, $get_path:literal, $get_op:literal,
     $submit:ident, $submit_path:literal, $submit_op:literal,
     $decide:ident, $decide_path:literal, $decide_op:literal,
     $activate:ident, $activate_path:literal, $activate_op:literal,
     $rollback:ident, $rollback_path:literal, $rollback_op:literal) => {
        /// One governed version (`finance.read`).
        #[utoipa::path(get, path = $get_path, operation_id = $get_op, tag = "finance-config",
            params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
            responses((status = 200, body = PolicyVersionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
        pub async fn $get(State(st): State<FinanceState>, auth: Authenticated, Path(id): Path<Uuid>) -> ApiResult<Response> {
            let ctx = auth.require(Permission::FinanceRead)?;
            let v = repo::get(&st, $kind, ctx.scope.tenant_id, id).await?;
            Ok(ok(&v, StatusCode::OK))
        }

        /// Submit a draft version for approval through the WorkflowPort (`finance.config.manage`).
        #[utoipa::path(post, path = $submit_path, operation_id = $submit_op, tag = "finance-config",
            params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the version")),
            request_body(content = Option<ReasonBody>), security(("session_cookie" = [], "csrf_token" = [])),
            responses((status = 200, body = PolicyVersionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
        pub async fn $submit(State(st): State<FinanceState>, auth: Authenticated, Path(id): Path<Uuid>, headers: HeaderMap, _body: Option<Json<ReasonBody>>) -> ApiResult<Response> {
            let ctx = auth.require(Permission::FinanceConfigManage)?;
            let v = if_match!(headers);
            let out = repo::submit(&st, $kind, ctx.scope.tenant_id, id, v, &actor(ctx)).await?;
            Ok(ok(&out, StatusCode::OK))
        }

        /// Approve or return a version in review (`finance.approve`, step-up).
        #[utoipa::path(post, path = $decide_path, operation_id = $decide_op, tag = "finance-config",
            params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the version")),
            request_body = DecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
            responses((status = 200, body = PolicyVersionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
        pub async fn $decide(State(st): State<FinanceState>, auth: Authenticated, Path(id): Path<Uuid>, headers: HeaderMap, Json(b): Json<DecisionBody>) -> ApiResult<Response> {
            let ctx = auth.require(Permission::FinanceApprove)?;
            // Part 1 · S3-appr (DEF-X-002): the requester of the pending version never decides it.
            sutra_persistence::approval::guard_decision(&st.pool, ctx, &id, "finance configuration").await?;
            let v = if_match!(headers);
            let out = repo::decide(&st, $kind, ctx.scope.tenant_id, id, v, b.decision == DecisionKind::Approve, opt_text(b.reason.as_deref()), &actor(ctx)).await?;
            Ok(ok(&out, StatusCode::OK))
        }

        /// Activate an approved version (the previous effective version is superseded / rolled back; `finance.config.manage`).
        #[utoipa::path(post, path = $activate_path, operation_id = $activate_op, tag = "finance-config",
            params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the version")),
            request_body(content = Option<ReasonBody>), security(("session_cookie" = [], "csrf_token" = [])),
            responses((status = 200, body = PolicyVersionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
        pub async fn $activate(State(st): State<FinanceState>, auth: Authenticated, Path(id): Path<Uuid>, headers: HeaderMap, _body: Option<Json<ReasonBody>>) -> ApiResult<Response> {
            let ctx = auth.require(Permission::FinanceConfigManage)?;
            let v = if_match!(headers);
            let out = repo::activate(&st, $kind, ctx.scope.tenant_id, id, v, &actor(ctx)).await?;
            Ok(ok(&out, StatusCode::OK))
        }

        /// Authorise a rollback to this prior version: creates a new draft based on it (`finance.approve`, step-up; reason required).
        #[utoipa::path(post, path = $rollback_path, operation_id = $rollback_op, tag = "finance-config",
            params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
            request_body = ReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
            responses((status = 201, body = PolicyVersionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
        pub async fn $rollback(State(st): State<FinanceState>, auth: Authenticated, Path(id): Path<Uuid>, headers: HeaderMap, Json(b): Json<ReasonBody>) -> ApiResult<Response> {
            let ctx = auth.require(Permission::FinanceApprove)?;
            require_idempotency_key(&headers)?;
            let reason = require_text("reason", b.reason.as_deref())?;
            let out = repo::rollback(&st, $kind, ctx.scope.tenant_id, id, &reason, &actor(ctx)).await?;
            Ok(ok(&out, StatusCode::CREATED))
        }
    };
}

policy_routes!(
    Kind::Config,
    "configuration",
    config_get,
    "/api/v1/finance/finance-config/{id}",
    "finance_config_get",
    config_submit,
    "/api/v1/finance/finance-config/{id}/submit",
    "finance_config_submit",
    config_decide,
    "/api/v1/finance/finance-config/{id}/decision",
    "finance_config_decide",
    config_activate,
    "/api/v1/finance/finance-config/{id}/activate",
    "finance_config_activate",
    config_rollback,
    "/api/v1/finance/finance-config/{id}/rollback",
    "finance_config_rollback"
);
policy_routes!(
    Kind::Rule,
    "financial rule",
    rule_get,
    "/api/v1/finance/finance-rules/{id}",
    "finance_rule_get",
    rule_submit,
    "/api/v1/finance/finance-rules/{id}/submit",
    "finance_rule_submit",
    rule_decide,
    "/api/v1/finance/finance-rules/{id}/decision",
    "finance_rule_decide",
    rule_activate,
    "/api/v1/finance/finance-rules/{id}/activate",
    "finance_rule_activate",
    rule_rollback,
    "/api/v1/finance/finance-rules/{id}/rollback",
    "finance_rule_rollback"
);

pub fn router(state: FinanceState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(config_list, config_create))
        .routes(routes!(config_get))
        .routes(routes!(config_submit))
        .routes(routes!(config_decide))
        .routes(routes!(config_activate))
        .routes(routes!(config_rollback))
        .routes(routes!(rule_list, rule_create))
        .routes(routes!(rule_get))
        .routes(routes!(rule_submit))
        .routes(routes!(rule_decide))
        .routes(routes!(rule_activate))
        .routes(routes!(rule_rollback))
        .with_state(state)
}
