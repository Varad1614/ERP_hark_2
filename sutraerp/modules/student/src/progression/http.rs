//! HTTP handlers of academic progression (`tag = "progression"`; operation ids
//! `progression_policy_*`, `progression_*`, `credit_ledger_*`, `progression_condition_*`,
//! `probation_*`, `detention_*`, `credit_transfer_*`, `programme_migration_*`,
//! `academic_break_*`, `qualification_eligibility_*`, `abc_sync_*`).

use super::domain::*;
use super::{eligibility, mobility, repo};
use crate::registration::support::{body, StepDecision, StepDecisionBody, StepReason};
use crate::shared::http::{actor, if_match, with_etag};
use crate::shared::read_scope::{require_any_student_read, require_student_read};
use crate::shared::scope::{institution_scope, tenant};
use crate::shared::StudentState;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use sutra_contracts::student::ids::StudentId;
use sutra_contracts::student::status::{
    PolicyVersionStatus, ProbationStatus, ProgressionConditionStatus, ProgressionDecisionStatus,
};
use sutra_kernel::auth::Permission;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::InstitutionId;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use uuid::Uuid;

/// DTOs with an optimistic-concurrency version (ETag).
pub trait Versioned {
    fn version(&self) -> i64;
}
macro_rules! versioned {
    ($($t:ty),+) => { $(impl Versioned for $t { fn version(&self) -> i64 { self.version } })+ };
}
versioned!(
    ProgressionPolicy,
    ProgressionDecision,
    ProgressionCondition,
    ProbationCase,
    DetentionCase,
    CreditTransfer,
    ProgrammeMigration,
    AcademicBreak,
    AbcSync
);
impl Versioned for RevisionResult {
    fn version(&self) -> i64 {
        self.decision.version
    }
}

fn ok<T: serde::Serialize + Versioned>(status: StatusCode, v: &T) -> Response {
    with_etag(status, v, Version(v.version()))
}

fn json_body<T: serde::de::DeserializeOwned>(raw: &Bytes) -> Result<T, KernelError> {
    serde_json::from_slice(raw)
        .map_err(|e| KernelError::BadRequest(format!("invalid JSON body: {e}")))
}

macro_rules! expect_version {
    ($headers:expr) => {
        match if_match(&$headers)? {
            Ok(v) => v,
            Err(res) => return Ok(res),
        }
    };
}

/// `POST …/{id}/<verb>` with `If-Match` and an optional `{reason}` body.
macro_rules! reason_cmd {
    ($fn:ident, $path:literal, $op:literal, $doc:literal, $resp:ident, $perm:expr, $required:expr,
     |$st:ident, $t:ident, $id:ident, $v:ident, $reason:ident, $a:ident| $call:expr) => {
        #[doc = $doc]
        #[utoipa::path(
            post,
            path = $path,
            operation_id = $op,
            tag = "progression",
            params(
                ("id" = String, Path, format = Uuid),
                ("If-Match" = String, Header, description = "ETag of the current version"),
                ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
            ),
            security(("session_cookie" = [], "csrf_token" = [])),
            request_body(content = Option<StepReason>),
            responses(
                (status = 200, body = $resp, headers(("ETag" = String))),
                (status = 403, body = Problem, content_type = "application/problem+json"),
                (status = 404, body = Problem, content_type = "application/problem+json"),
                (status = 409, description = "Illegal transition or precondition not met", body = Problem, content_type = "application/problem+json"),
                (status = 412, body = Problem, content_type = "application/problem+json"),
                (status = 422, body = Problem, content_type = "application/problem+json"),
                (status = 428, body = Problem, content_type = "application/problem+json"),
            )
        )]
        pub async fn $fn(
            State($st): State<StudentState>,
            auth: Authenticated,
            Path($id): Path<Uuid>,
            headers: HeaderMap,
            raw: Bytes,
        ) -> ApiResult<Response> {
            let ctx = auth.require($perm)?;
            let $v = expect_version!(headers);
            let $reason = body::<StepReason>(&raw)?.validated($required)?;
            let $t = tenant(ctx);
            let $a = actor(ctx);
            let out = $call.await?;
            Ok(ok(StatusCode::OK, &out))
        }
    };
}

/// `POST …/{id}/<verb>` with `If-Match` and a required JSON body.
macro_rules! body_cmd {
    ($fn:ident, $path:literal, $op:literal, $doc:literal, $req:ident, $resp:ident, $perm:expr,
     |$st:ident, $t:ident, $id:ident, $v:ident, $b:ident, $a:ident| $call:expr) => {
        #[doc = $doc]
        #[utoipa::path(
            post,
            path = $path,
            operation_id = $op,
            tag = "progression",
            params(
                ("id" = String, Path, format = Uuid),
                ("If-Match" = String, Header, description = "ETag of the current version"),
                ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
            ),
            security(("session_cookie" = [], "csrf_token" = [])),
            request_body = $req,
            responses(
                (status = 200, body = $resp, headers(("ETag" = String))),
                (status = 400, body = Problem, content_type = "application/problem+json"),
                (status = 403, body = Problem, content_type = "application/problem+json"),
                (status = 404, body = Problem, content_type = "application/problem+json"),
                (status = 409, description = "Illegal transition or precondition not met", body = Problem, content_type = "application/problem+json"),
                (status = 412, body = Problem, content_type = "application/problem+json"),
                (status = 422, body = Problem, content_type = "application/problem+json"),
                (status = 428, body = Problem, content_type = "application/problem+json"),
            )
        )]
        pub async fn $fn(
            State($st): State<StudentState>,
            auth: Authenticated,
            Path($id): Path<Uuid>,
            headers: HeaderMap,
            raw: Bytes,
        ) -> ApiResult<Response> {
            let ctx = auth.require($perm)?;
            let $v = expect_version!(headers);
            let $b: $req = json_body(&raw)?;
            let $t = tenant(ctx);
            let $a = actor(ctx);
            let out = $call.await?;
            Ok(ok(StatusCode::OK, &out))
        }
    };
}

/// `GET /<collection>/{id}`.
macro_rules! get_one {
    ($fn:ident, $path:literal, $op:literal, $doc:literal, $resp:ident, |$st:ident, $t:ident, $id:ident| $call:expr) => {
        #[doc = $doc]
        #[utoipa::path(
                    get,
                    path = $path,
                    operation_id = $op,
                    tag = "progression",
                    params(("id" = String, Path, format = Uuid)),
                    security(("session_cookie" = [])),
                    responses(
                        (status = 200, body = $resp, headers(("ETag" = String))),
                        (status = 404, body = Problem, content_type = "application/problem+json"),
                    )
                )]
        pub async fn $fn(
            State($st): State<StudentState>,
            auth: Authenticated,
            Path($id): Path<Uuid>,
        ) -> ApiResult<Response> {
            let ctx = auth.require(Permission::StudentRead)?;
            let $t = tenant(ctx);
            let out = $call.await?;
            Ok(ok(StatusCode::OK, &out))
        }
    };
}

/// `GET /students/{id}/<collection>` → `{ items }`.
macro_rules! list_of_student {
    ($fn:ident, $path:literal, $op:literal, $doc:literal, $list:ident, |$st:ident, $t:ident, $sid:ident| $call:expr) => {
        #[doc = $doc]
        #[utoipa::path(
                    get,
                    path = $path,
                    operation_id = $op,
                    tag = "progression",
                    params(("id" = String, Path, format = Uuid, description = "Student id")),
                    security(("session_cookie" = [])),
                    responses(
                        (status = 200, body = $list),
                        (status = 404, body = Problem, content_type = "application/problem+json"),
                    )
                )]
        pub async fn $fn(
            State($st): State<StudentState>,
            auth: Authenticated,
            Path($sid): Path<StudentId>,
        ) -> ApiResult<Json<$list>> {
            let ctx = auth.require(Permission::StudentRead)?;
            let $t = tenant(ctx);
            Ok(Json($list {
                items: $call.await?,
            }))
        }
    };
}

/// `POST /students/{id}/<collection>` (Idempotency-Key) → 201.
macro_rules! create_for_student {
    ($fn:ident, $path:literal, $op:literal, $doc:literal, $req:ident, $resp:ident, $perm:expr,
     |$st:ident, $t:ident, $sid:ident, $b:ident, $a:ident| $call:expr) => {
        #[doc = $doc]
        #[utoipa::path(
            post,
            path = $path,
            operation_id = $op,
            tag = "progression",
            params(
                ("id" = String, Path, format = Uuid, description = "Student id"),
                ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key."),
                ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
            ),
            security(("session_cookie" = [], "csrf_token" = [])),
            request_body = $req,
            responses(
                (status = 201, body = $resp, headers(("ETag" = String))),
                (status = 400, body = Problem, content_type = "application/problem+json"),
                (status = 403, body = Problem, content_type = "application/problem+json"),
                (status = 404, body = Problem, content_type = "application/problem+json"),
                (status = 409, body = Problem, content_type = "application/problem+json"),
                (status = 422, body = Problem, content_type = "application/problem+json"),
            )
        )]
        pub async fn $fn(
            State($st): State<StudentState>,
            auth: Authenticated,
            Path($sid): Path<StudentId>,
            headers: HeaderMap,
            Json($b): Json<$req>,
        ) -> ApiResult<Response> {
            let ctx = auth.require($perm)?;
            require_idempotency_key(&headers)?;
            let $t = tenant(ctx);
            let $a = actor(ctx);
            let out = $call.await?;
            Ok(ok(StatusCode::CREATED, &out))
        }
    };
}

// ================================================================== policies (L3-C-075)

#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct PolicyQuery {
    #[param(inline)]
    pub status: Option<PolicyVersionStatus>,
}

/// Define a draft progression policy (`progression.policy.manage`).
#[utoipa::path(
    post,
    path = "/api/v1/student/institutions/{id}/progression-policies",
    operation_id = "progression_policy_create",
    tag = "progression",
    params(
        ("id" = String, Path, format = Uuid, description = "Institution id"),
        ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key."),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = CreateProgressionPolicy,
    responses(
        (status = 201, body = ProgressionPolicy, headers(("ETag" = String))),
        (status = 403, body = Problem, content_type = "application/problem+json"),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn policy_create(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    headers: HeaderMap,
    Json(cmd): Json<CreateProgressionPolicy>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ProgressionPolicyManage)?;
    require_idempotency_key(&headers)?;
    let p = repo::create_policy(&st, &institution_scope(ctx, id), id, cmd, &actor(ctx)).await?;
    Ok(ok(StatusCode::CREATED, &p))
}

/// Progression policies of an institution, every version (`student.read` or
/// `student.read.assigned`).
#[utoipa::path(
    get,
    path = "/api/v1/student/institutions/{id}/progression-policies",
    operation_id = "progression_policy_list",
    tag = "progression",
    params(("id" = String, Path, format = Uuid, description = "Institution id"), PolicyQuery),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = ProgressionPolicyList),
        (status = 404, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn policy_list(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    Query(q): Query<PolicyQuery>,
) -> ApiResult<Json<ProgressionPolicyList>> {
    // Audit C3: any student-read permission (faculty included).
    let ctx = require_any_student_read(&auth)?;
    let items = repo::list_policies(&st, &institution_scope(ctx, id), id, q.status).await?;
    Ok(Json(ProgressionPolicyList { items }))
}

/// One progression policy version (`student.read` or `student.read.assigned`: policies are
/// institution rules, not student data).
#[utoipa::path(
    get,
    path = "/api/v1/student/progression-policies/{id}",
    operation_id = "progression_policy_get",
    tag = "progression",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = ProgressionPolicy, headers(("ETag" = String))),
        (status = 403, body = Problem, content_type = "application/problem+json"),
        (status = 404, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn policy_get(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    // Audit C3: any student-read permission (faculty included).
    let ctx = require_any_student_read(&auth)?;
    let p = repo::get_policy(&st.pool, tenant(ctx), id).await?;
    Ok(ok(StatusCode::OK, &p))
}

/// Change a draft policy version.
#[utoipa::path(
    patch,
    path = "/api/v1/student/progression-policies/{id}",
    operation_id = "progression_policy_update",
    tag = "progression",
    params(
        ("id" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the policy"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = UpdateProgressionPolicy,
    responses(
        (status = 200, body = ProgressionPolicy, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
        (status = 428, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn policy_update(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<UpdateProgressionPolicy>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ProgressionPolicyManage)?;
    let v = expect_version!(headers);
    let p = repo::update_policy(&st, tenant(ctx), id, v, cmd, &actor(ctx)).await?;
    Ok(ok(StatusCode::OK, &p))
}

reason_cmd!(policy_submit, "/api/v1/student/progression-policies/{id}/submit", "progression_policy_submit",
    "Submit a draft for institutional governance (WorkflowPort `student.progression_policy`; auto-approved → effective, superseding the previous version).",
    ProgressionPolicy, Permission::ProgressionPolicyManage, false,
    |st, t, id, v, _r, a| repo::submit_policy(&st, t, id, v, &a));
body_cmd!(
    policy_decision,
    "/api/v1/student/progression-policies/{id}/decision",
    "progression_policy_decision",
    "Decide a policy in review (approve → effective; reject → draft).",
    StepDecisionBody,
    ProgressionPolicy,
    Permission::ProgressionPolicyManage,
    |st, t, id, v, b, a| async {
        // Part 1 · S3-appr (DEF-X-002): the requester of the pending request never decides it.
        sutra_persistence::approval::guard_decision_by(
            &st.pool,
            t.as_uuid(),
            a.id.as_deref(),
            &id,
            &[],
            "progression policy",
        )
        .await?;
        repo::decide_policy(
            &st,
            t,
            id,
            v,
            b.decision == StepDecision::Approve,
            b.reason.filter(|r| !r.trim().is_empty()),
            &a,
        )
        .await
    }
);
reason_cmd!(
    policy_retire,
    "/api/v1/student/progression-policies/{id}/retire",
    "progression_policy_retire",
    "Retire an effective version (or discard a draft).",
    ProgressionPolicy,
    Permission::ProgressionPolicyManage,
    true,
    |st, t, id, v, r, a| repo::retire_policy(&st, t, id, v, r, &a)
);

/// Create the next draft version of a policy (history and version continuity).
#[utoipa::path(
    post,
    path = "/api/v1/student/progression-policies/{id}/revise",
    operation_id = "progression_policy_revise",
    tag = "progression",
    params(
        ("id" = String, Path, format = Uuid),
        ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key."),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 201, body = ProgressionPolicy, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn policy_revise(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ProgressionPolicyManage)?;
    require_idempotency_key(&headers)?;
    let p = repo::revise_policy(&st, tenant(ctx), id, &actor(ctx)).await?;
    Ok(ok(StatusCode::CREATED, &p))
}

// ================================================================== summary

/// Progression overview: standing, term, credits, policy, probation/detention, conditions, eligibility.
#[utoipa::path(
    get,
    path = "/api/v1/student/students/{id}/progression",
    operation_id = "progression_summary",
    tag = "progression",
    params(("id" = String, Path, format = Uuid, description = "Student id")),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = ProgressionSummary),
        (status = 404, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn summary(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentId>,
) -> ApiResult<Json<ProgressionSummary>> {
    let ctx = auth.require(Permission::StudentRead)?;
    Ok(Json(repo::summary(&st, tenant(ctx), id).await?))
}

// ================================================================== decisions (L3-C-027, 031, 038)

#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct DecisionQuery {
    #[param(inline)]
    pub status: Option<ProgressionDecisionStatus>,
}

create_for_student!(decision_create, "/api/v1/student/students/{id}/progression-decisions", "progression_decision_create",
    "Draft a progression decision. Evidence comes from the Exams ResultsPort when published, otherwise `creditsEarned` (+ attempted, failed components) is entered here; the outcome defaults to the effective policy's recommendation.",
    CreateDecision, ProgressionDecision, Permission::ProgressionManage,
    |st, t, sid, b, a| repo::create_decision(&st, t, sid, b, &a));

/// Progression decisions of a student, newest first (`student.read`, or `student.read.assigned`
/// for a student the caller teaches).
#[utoipa::path(
    get,
    path = "/api/v1/student/students/{id}/progression-decisions",
    operation_id = "progression_decision_list",
    tag = "progression",
    params(("id" = String, Path, format = Uuid, description = "Student id"), DecisionQuery),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = ProgressionDecisionList),
        (status = 404, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn decision_list(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentId>,
    Query(q): Query<DecisionQuery>,
) -> ApiResult<Json<ProgressionDecisionList>> {
    // Audit C3: faculty list the decisions of students they teach only (404 otherwise).
    let ctx = require_student_read(&st, &auth, id).await?;
    let items = repo::list_decisions(&st.pool, tenant(ctx), id, q.status).await?;
    Ok(Json(ProgressionDecisionList { items }))
}

get_one!(
    decision_get,
    "/api/v1/student/progression-decisions/{id}",
    "progression_decision_get",
    "One progression decision with its conditions.",
    ProgressionDecision,
    |st, t, id| repo::get_decision(&st.pool, t, id)
);
reason_cmd!(decision_submit, "/api/v1/student/progression-decisions/{id}/submit", "progression_decision_submit",
    "Submit a draft for approval (WorkflowPort `student.progression_decision`; auto-approved → effective: term, ledger, standing, probation/detention).",
    ProgressionDecision, Permission::ProgressionManage, false,
    |st, t, id, v, _r, a| repo::submit_decision(&st, t, id, v, &a));
body_cmd!(
    decision_decide,
    "/api/v1/student/progression-decisions/{id}/decision",
    "progression_decision_decide",
    "Decide a pending decision (approve → effective; reject → rejected).",
    StepDecisionBody,
    ProgressionDecision,
    Permission::ProgressionDecide,
    |st, t, id, v, b, a| async {
        // Part 1 · S3-appr (DEF-X-002): the requester of the pending request never decides it.
        sutra_persistence::approval::guard_decision_by(
            &st.pool,
            t.as_uuid(),
            a.id.as_deref(),
            &id,
            &[],
            "progression decision",
        )
        .await?;
        repo::decide_decision(
            &st,
            t,
            id,
            v,
            b.decision == StepDecision::Approve,
            b.reason.filter(|r| !r.trim().is_empty()),
            &a,
        )
        .await
    }
);
reason_cmd!(
    decision_discard,
    "/api/v1/student/progression-decisions/{id}/discard",
    "progression_decision_discard",
    "Discard a draft decision.",
    ProgressionDecision,
    Permission::ProgressionManage,
    false,
    |st, t, id, v, r, a| repo::discard_decision(&st, t, id, v, r, &a)
);

/// Fresh evaluation of an effective decision after re-evaluation, supplementary result, appeal or
/// correction (L3-C-038). A changed outcome supersedes the decision (approval
/// `student.progression_revision`); the same outcome is recorded without a new decision.
#[utoipa::path(
    post,
    path = "/api/v1/student/progression-decisions/{id}/revisions",
    operation_id = "progression_revision_create",
    tag = "progression",
    params(
        ("id" = String, Path, format = Uuid),
        ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key."),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = CreateRevision,
    responses(
        (status = 201, body = RevisionResult, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, description = "Decision not effective or the policy forbids fresh evaluation", body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn revision_create(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<CreateRevision>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ProgressionDecide)?;
    require_idempotency_key(&headers)?;
    let r = repo::revise_decision(&st, tenant(ctx), id, cmd, &actor(ctx)).await?;
    Ok(ok(StatusCode::CREATED, &r))
}

/// Revision history of a decision (as original or as revised decision).
#[utoipa::path(
    get,
    path = "/api/v1/student/progression-decisions/{id}/revisions",
    operation_id = "progression_revision_list",
    tag = "progression",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = ProgressionRevisionList),
        (status = 404, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn revision_list(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ProgressionRevisionList>> {
    let ctx = auth.require(Permission::StudentRead)?;
    Ok(Json(ProgressionRevisionList {
        items: repo::list_revisions(&st.pool, tenant(ctx), id).await?,
    }))
}

// ================================================================== conditions / probation / detention

get_one!(
    condition_get,
    "/api/v1/student/progression-conditions/{id}",
    "progression_condition_get",
    "One condition of a conditional promotion.",
    ProgressionCondition,
    |st, t, id| repo::get_condition(&st.pool, t, id)
);
reason_cmd!(
    condition_fulfil,
    "/api/v1/student/progression-conditions/{id}/fulfil",
    "progression_condition_fulfil",
    "Record a condition as fulfilled (L3-C-029); all resolved → good standing.",
    ProgressionCondition,
    Permission::ProgressionManage,
    false,
    |st, t, id, v, r, a| repo::resolve_condition(
        &st,
        t,
        id,
        v,
        ProgressionConditionStatus::Fulfilled,
        r,
        &a
    )
);
reason_cmd!(
    condition_fail,
    "/api/v1/student/progression-conditions/{id}/fail",
    "progression_condition_fail",
    "Record a condition as failed; the student is placed on probation.",
    ProgressionCondition,
    Permission::ProgressionManage,
    true,
    |st, t, id, v, r, a| repo::resolve_condition(
        &st,
        t,
        id,
        v,
        ProgressionConditionStatus::Failed,
        r,
        &a
    )
);
reason_cmd!(
    condition_waive,
    "/api/v1/student/progression-conditions/{id}/waive",
    "progression_condition_waive",
    "Waive a condition (reason required).",
    ProgressionCondition,
    Permission::ProgressionManage,
    true,
    |st, t, id, v, r, a| repo::resolve_condition(
        &st,
        t,
        id,
        v,
        ProgressionConditionStatus::Waived,
        r,
        &a
    )
);

get_one!(
    probation_get,
    "/api/v1/student/probation-cases/{id}",
    "probation_get",
    "One probation case.",
    ProbationCase,
    |st, t, id| repo::get_probation(&st.pool, t, id)
);
reason_cmd!(
    probation_clear,
    "/api/v1/student/probation-cases/{id}/clear",
    "probation_clear",
    "Clear an active probation (L3-C-030); the student returns to good standing.",
    ProbationCase,
    Permission::ProgressionManage,
    true,
    |st, t, id, v, r, a| repo::resolve_probation(
        &st,
        t,
        id,
        v,
        ProbationStatus::Cleared,
        r.unwrap_or_default(),
        &a
    )
);
reason_cmd!(
    probation_escalate,
    "/api/v1/student/probation-cases/{id}/escalate",
    "probation_escalate",
    "Escalate a failed probation review; the next evaluation leads to detention.",
    ProbationCase,
    Permission::ProgressionManage,
    true,
    |st, t, id, v, r, a| repo::resolve_probation(
        &st,
        t,
        id,
        v,
        ProbationStatus::Escalated,
        r.unwrap_or_default(),
        &a
    )
);

get_one!(
    detention_get,
    "/api/v1/student/detention-cases/{id}",
    "detention_get",
    "One detention case.",
    DetentionCase,
    |st, t, id| repo::get_detention(&st.pool, t, id)
);
body_cmd!(
    detention_determine,
    "/api/v1/student/detention-cases/{id}/determine",
    "detention_determine",
    "Determine an undetermined repeat consequence (term or year, L3-C-031).",
    DetermineRepeat,
    DetentionCase,
    Permission::ProgressionManage,
    |st, t, id, v, b, a| repo::determine_repeat(&st, t, id, v, b, &a)
);

// ================================================================== ledger (L3-C-028)

/// Credit ledger of the student's current context (append-only entries + balance).
#[utoipa::path(
    get,
    path = "/api/v1/student/students/{id}/credit-ledger",
    operation_id = "credit_ledger_get",
    tag = "progression",
    params(("id" = String, Path, format = Uuid, description = "Student id")),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = CreditLedger),
        (status = 404, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn ledger_get(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentId>,
) -> ApiResult<Json<CreditLedger>> {
    let ctx = auth.require(Permission::StudentRead)?;
    Ok(Json(repo::ledger(&st, tenant(ctx), id).await?))
}

/// Append a governed manual entry (earned / adjusted / revoked / expired; note required).
#[utoipa::path(
    post,
    path = "/api/v1/student/students/{id}/credit-ledger",
    operation_id = "credit_ledger_append",
    tag = "progression",
    params(
        ("id" = String, Path, format = Uuid, description = "Student id"),
        ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key."),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = AppendLedgerEntry,
    responses(
        (status = 201, body = CreditLedger),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn ledger_append(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentId>,
    headers: HeaderMap,
    Json(cmd): Json<AppendLedgerEntry>,
) -> ApiResult<(StatusCode, Json<CreditLedger>)> {
    let ctx = auth.require(Permission::ProgressionManage)?;
    require_idempotency_key(&headers)?;
    let l = repo::append_manual(&st, tenant(ctx), id, cmd, &actor(ctx)).await?;
    Ok((StatusCode::CREATED, Json(l)))
}

// ================================================================== credit transfer (L3-C-032)

create_for_student!(transfer_create, "/api/v1/student/students/{id}/credit-transfers", "credit_transfer_create",
    "Request credit transfer / recognition (items mapped to curriculum components where equivalent).",
    CreateCreditTransfer, CreditTransfer, Permission::ProgressionManage,
    |st, t, sid, b, a| mobility::create_transfer(&st, t, sid, b, &a));
list_of_student!(
    transfer_list,
    "/api/v1/student/students/{id}/credit-transfers",
    "credit_transfer_list",
    "Credit transfer cases of a student.",
    CreditTransferList,
    |st, t, sid| mobility::list_transfers(&st.pool, t, sid)
);
get_one!(
    transfer_get,
    "/api/v1/student/credit-transfers/{id}",
    "credit_transfer_get",
    "One credit transfer case.",
    CreditTransfer,
    |st, t, id| mobility::get_transfer(&st.pool, t, id)
);

/// Change a requested credit transfer (evidence, items).
#[utoipa::path(
    patch,
    path = "/api/v1/student/credit-transfers/{id}",
    operation_id = "credit_transfer_update",
    tag = "progression",
    params(
        ("id" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the case"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = UpdateCreditTransfer,
    responses(
        (status = 200, body = CreditTransfer, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
        (status = 428, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn transfer_update(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<UpdateCreditTransfer>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ProgressionManage)?;
    let v = expect_version!(headers);
    let t = mobility::update_transfer(&st, tenant(ctx), id, v, cmd, &actor(ctx)).await?;
    Ok(ok(StatusCode::OK, &t))
}

reason_cmd!(transfer_submit, "/api/v1/student/credit-transfers/{id}/submit", "credit_transfer_submit",
    "Submit for academic recognition (WorkflowPort `student.credit_transfer`; auto-approved → credits in the ledger).",
    CreditTransfer, Permission::ProgressionManage, false,
    |st, t, id, v, _r, a| mobility::submit_transfer(&st, t, id, v, &a));
body_cmd!(
    transfer_decision,
    "/api/v1/student/credit-transfers/{id}/decision",
    "credit_transfer_decision",
    "Recognition decision: approve, reject or revise (revision required).",
    RecognitionDecisionBody,
    CreditTransfer,
    Permission::ProgressionDecide,
    |st, t, id, v, b, a| async {
        // Part 1 · S3-appr (DEF-X-002): the requester of the pending request never decides it.
        sutra_persistence::approval::guard_decision_by(
            &st.pool,
            t.as_uuid(),
            a.id.as_deref(),
            &id,
            &[],
            "credit transfer",
        )
        .await?;
        mobility::decide_transfer(
            &st,
            t,
            id,
            v,
            b.decision,
            b.reason.filter(|r| !r.trim().is_empty()),
            &a,
        )
        .await
    }
);
reason_cmd!(
    transfer_withdraw,
    "/api/v1/student/credit-transfers/{id}/withdraw",
    "credit_transfer_withdraw",
    "Withdraw a credit transfer request.",
    CreditTransfer,
    Permission::ProgressionManage,
    false,
    |st, t, id, v, r, a| mobility::withdraw_transfer(&st, t, id, v, r, &a)
);

// ================================================================== programme migration (L3-C-033)

create_for_student!(migration_create, "/api/v1/student/students/{id}/programme-migrations", "programme_migration_create",
    "Request a programme migration (published destination programme + curriculum, placement, credit mapping).",
    CreateMigration, ProgrammeMigration, Permission::ProgressionManage,
    |st, t, sid, b, a| mobility::create_migration(&st, t, sid, b, &a));
list_of_student!(
    migration_list,
    "/api/v1/student/students/{id}/programme-migrations",
    "programme_migration_list",
    "Programme migrations of a student.",
    ProgrammeMigrationList,
    |st, t, sid| mobility::list_migrations(&st.pool, t, sid)
);
get_one!(
    migration_get,
    "/api/v1/student/programme-migrations/{id}",
    "programme_migration_get",
    "One programme migration.",
    ProgrammeMigration,
    |st, t, id| mobility::get_migration(&st.pool, t, id)
);
reason_cmd!(migration_submit, "/api/v1/student/programme-migrations/{id}/submit", "programme_migration_submit",
    "Submit for approval (WorkflowPort `student.programme_migration`; auto-approved → the context moves programme).",
    ProgrammeMigration, Permission::ProgressionManage, false,
    |st, t, id, v, _r, a| mobility::submit_migration(&st, t, id, v, &a));
body_cmd!(
    migration_decision,
    "/api/v1/student/programme-migrations/{id}/decision",
    "programme_migration_decision",
    "Migration decision: approve, reject or revise.",
    RecognitionDecisionBody,
    ProgrammeMigration,
    Permission::ProgressionDecide,
    |st, t, id, v, b, a| async {
        // Part 1 · S3-appr (DEF-X-002): the requester of the pending request never decides it.
        sutra_persistence::approval::guard_decision_by(
            &st.pool,
            t.as_uuid(),
            a.id.as_deref(),
            &id,
            &[],
            "programme migration",
        )
        .await?;
        mobility::decide_migration(
            &st,
            t,
            id,
            v,
            b.decision,
            b.reason.filter(|r| !r.trim().is_empty()),
            &a,
        )
        .await
    }
);
reason_cmd!(
    migration_withdraw,
    "/api/v1/student/programme-migrations/{id}/withdraw",
    "programme_migration_withdraw",
    "Withdraw a migration request.",
    ProgrammeMigration,
    Permission::ProgressionManage,
    false,
    |st, t, id, v, r, a| mobility::withdraw_migration(&st, t, id, v, r, &a)
);

// ================================================================== academic break (L3-C-034/035)

create_for_student!(break_create, "/api/v1/student/students/{id}/academic-breaks", "academic_break_create",
    "Request an academic break (approval `student.academic_break` starts at once; auto-approved, in effect when it has started).",
    CreateBreak, AcademicBreak, Permission::ProgressionManage,
    |st, t, sid, b, a| mobility::create_break(&st, t, sid, b, &a));
list_of_student!(
    break_list,
    "/api/v1/student/students/{id}/academic-breaks",
    "academic_break_list",
    "Academic breaks of a student.",
    AcademicBreakList,
    |st, t, sid| mobility::list_breaks(&st.pool, t, sid)
);
get_one!(
    break_get,
    "/api/v1/student/academic-breaks/{id}",
    "academic_break_get",
    "One academic break.",
    AcademicBreak,
    |st, t, id| mobility::get_break(&st.pool, t, id)
);
body_cmd!(
    break_decision,
    "/api/v1/student/academic-breaks/{id}/decision",
    "academic_break_decision",
    "Decide a requested break (approve / reject).",
    StepDecisionBody,
    AcademicBreak,
    Permission::ProgressionDecide,
    |st, t, id, v, b, a| async {
        // Part 1 · S3-appr (DEF-X-002): the requester of the pending request never decides it.
        sutra_persistence::approval::guard_decision_by(
            &st.pool,
            t.as_uuid(),
            a.id.as_deref(),
            &id,
            &[],
            "academic break",
        )
        .await?;
        mobility::decide_break(
            &st,
            t,
            id,
            v,
            b.decision == StepDecision::Approve,
            b.reason.filter(|r| !r.trim().is_empty()),
            &a,
        )
        .await
    }
);
reason_cmd!(
    break_start,
    "/api/v1/student/academic-breaks/{id}/start",
    "academic_break_start",
    "Put an approved break into effect once its start date is reached.",
    AcademicBreak,
    Permission::ProgressionManage,
    false,
    |st, t, id, v, _r, a| mobility::start_break(&st, t, id, v, &a)
);
body_cmd!(break_rejoin, "/api/v1/student/academic-breaks/{id}/rejoin", "academic_break_rejoin",
    "Rejoin after the break: placement term, curriculum change mapping; registration eligibility restored.", RejoinBody, AcademicBreak, Permission::ProgressionManage,
    |st, t, id, v, b, a| mobility::rejoin(&st, t, id, v, b, &a));
reason_cmd!(
    break_withdraw,
    "/api/v1/student/academic-breaks/{id}/withdraw",
    "academic_break_withdraw",
    "Withdraw a requested or approved break.",
    AcademicBreak,
    Permission::ProgressionManage,
    false,
    |st, t, id, v, r, a| mobility::withdraw_break(&st, t, id, v, r, &a)
);

// ================================================================== eligibility (L3-C-036) + ABC (L3-C-037)

/// Evaluate qualification eligibility from the credit ledger and curriculum requirements.
#[utoipa::path(
    post,
    path = "/api/v1/student/students/{id}/qualification-eligibility",
    operation_id = "qualification_eligibility_evaluate",
    tag = "progression",
    params(
        ("id" = String, Path, format = Uuid, description = "Student id"),
        ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key."),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body(content = Option<EvaluateEligibility>),
    responses(
        (status = 201, body = QualificationEligibility),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn eligibility_evaluate(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentId>,
    headers: HeaderMap,
    raw: Bytes,
) -> ApiResult<(StatusCode, Json<QualificationEligibility>)> {
    let ctx = auth.require(Permission::ProgressionManage)?;
    require_idempotency_key(&headers)?;
    let cmd = body::<EvaluateEligibility>(&raw)?;
    let e = eligibility::evaluate(&st, tenant(ctx), id, cmd, &actor(ctx)).await?;
    Ok((StatusCode::CREATED, Json(e)))
}

/// Current qualification eligibility determination (404 when never evaluated). Read by 3C.
#[utoipa::path(
    get,
    path = "/api/v1/student/students/{id}/qualification-eligibility",
    operation_id = "qualification_eligibility_get",
    tag = "progression",
    params(("id" = String, Path, format = Uuid, description = "Student id")),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = QualificationEligibility),
        (status = 404, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn eligibility_get(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentId>,
) -> ApiResult<Json<QualificationEligibility>> {
    let ctx = auth.require(Permission::StudentRead)?;
    crate::shared::student::load_student(&st.pool, tenant(ctx), id).await?;
    eligibility::current(&st.pool, tenant(ctx), id)
        .await?
        .map(Json)
        .ok_or_else(|| {
            KernelError::NotFound {
                entity: "qualification eligibility",
            }
            .into()
        })
}

list_of_student!(
    eligibility_history,
    "/api/v1/student/students/{id}/qualification-eligibility/history",
    "qualification_eligibility_history",
    "Every eligibility determination, newest first.",
    QualificationEligibilityList,
    |st, t, sid| eligibility::history(&st.pool, t, sid)
);

/// Synchronise institution-approved credits to the Academic Bank of Credits (no-op adapter in
/// Batch 3 → recorded `failed` with `not_configured`).
#[utoipa::path(
    post,
    path = "/api/v1/student/students/{id}/abc-syncs",
    operation_id = "abc_sync_create",
    tag = "progression",
    params(
        ("id" = String, Path, format = Uuid, description = "Student id"),
        ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key."),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 201, body = AbcSync, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, description = "ABC not enabled or nothing to synchronise", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn abc_create(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentId>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ProgressionManage)?;
    require_idempotency_key(&headers)?;
    let s = eligibility::synchronise(&st, tenant(ctx), id, &actor(ctx)).await?;
    Ok(ok(StatusCode::CREATED, &s))
}

list_of_student!(
    abc_list,
    "/api/v1/student/students/{id}/abc-syncs",
    "abc_sync_list",
    "ABC synchronisation attempts of a student.",
    AbcSyncList,
    |st, t, sid| eligibility::list_abc(&st.pool, t, sid)
);
get_one!(
    abc_get,
    "/api/v1/student/abc-syncs/{id}",
    "abc_sync_get",
    "One ABC synchronisation attempt.",
    AbcSync,
    |st, t, id| eligibility::get_abc(&st.pool, t, id)
);
