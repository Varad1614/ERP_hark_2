//! Governance evidence (L3-G-002 campus operations evidence → accreditation evidence readiness,
//! L3-G-015 governance review → accreditation evidence): evidence is built from a **closed**
//! governance case together with the campus operations figures of the moment, then published
//! once (`governance_evidence.published`, payload `GovernanceEvidencePublishedV1`). Accreditation
//! (Domain H09) cites the evidence id from the event; it never reads this table.

use super::cases::load_case;
use super::domain::{EvidenceStatus, GovernanceKind, GovernanceStatus};
use crate::infrastructure::common::{
    audit, ensure_version, expected, history, invalid, limit, parse_filter, FacilityReasonBody,
    Transition,
};
use crate::metrics::PgCampusMetrics;
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, with_etag};
use crate::shared::publish;
use crate::shared::rows::decode;
use crate::shared::scope::institution_scope;
use crate::shared::CampusState;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use sutra_contracts::campus::events::governance::{
    GovernanceEvidencePublishedV1, GOVERNANCE_EVIDENCE_PUBLISHED,
    GOVERNANCE_EVIDENCE_PUBLISHED_SCHEMA,
};
use sutra_contracts::services::ports::OperationalMetricsPort;
use sutra_kernel::auth::Permission;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// One piece of governance evidence.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GovernanceEvidenceView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub case_id: Uuid,
    pub case_number: String,
    pub case_kind: GovernanceKind,
    pub case_outcome: Option<String>,
    pub title: String,
    pub summary: Option<String>,
    /// Campus operations figures + the case decision at creation time.
    #[schema(value_type = Object)]
    pub facts: serde_json::Value,
    pub status: EvidenceStatus,
    /// Published evidence can be cited by accreditation evidence records.
    pub accreditation_ready: bool,
    pub published_at: Option<chrono::DateTime<chrono::Utc>>,
    pub version: i64,
}

/// Evidence list.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct GovernanceEvidenceList {
    pub items: Vec<GovernanceEvidenceView>,
}

/// Build evidence from a closed case.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GovernanceEvidenceCreate {
    pub case_id: Uuid,
    /// Default: the case title.
    pub title: Option<String>,
    pub summary: Option<String>,
}

/// Filters of the evidence list.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct GovernanceEvidenceQuery {
    pub status: Option<String>,
    pub limit: Option<i64>,
}

const SELECT: &str = "SELECT e.id, e.institution_id, e.case_id, c.number AS case_number, \
    c.kind AS case_kind, c.outcome AS case_outcome, e.title, e.summary, e.facts, e.status, \
    e.published_at, e.version FROM campus.governance_evidence e \
    JOIN campus.governance_case c ON c.id = e.case_id";

fn from_row(r: &PgRow) -> Result<GovernanceEvidenceView, sqlx::Error> {
    let status = decode(r.try_get("status")?, EvidenceStatus::parse)?;
    Ok(GovernanceEvidenceView {
        id: r.try_get("id")?,
        institution_id: r.try_get("institution_id")?,
        case_id: r.try_get("case_id")?,
        case_number: r.try_get("case_number")?,
        case_kind: decode(r.try_get("case_kind")?, GovernanceKind::parse)?,
        case_outcome: r.try_get("case_outcome")?,
        title: r.try_get("title")?,
        summary: r.try_get("summary")?,
        facts: r.try_get("facts")?,
        status,
        accreditation_ready: status == EvidenceStatus::Published,
        published_at: r.try_get("published_at")?,
        version: r.try_get("version")?,
    })
}

async fn load(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<GovernanceEvidenceView, KernelError> {
    let sql = format!(
        "{SELECT} WHERE e.id = $1 AND e.tenant_id = $2{}",
        if lock { " FOR UPDATE OF e" } else { "" }
    );
    let row = sqlx::query(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "governance evidence",
        })?;
    from_row(&row).map_err(map_db_error)
}

fn respond(status: StatusCode, v: &GovernanceEvidenceView) -> Response {
    with_etag(status, v, Version(v.version))
}

/// Build governance evidence from a closed case (`campus.governance.manage`; Idempotency-Key):
/// 409 unless the case is closed (approved outcome). Snapshots the campus operations figures.
#[utoipa::path(post, path = "/api/v1/campus/institutions/{id}/governance-evidence", operation_id = "campus_governance_evidence_create", tag = "campus-governance",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = GovernanceEvidenceCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = GovernanceEvidenceView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn evidence_create(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<GovernanceEvidenceCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::GovernanceManage)?;
    require_idempotency_key(&headers)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let figures = PgCampusMetrics::new(st.pool.clone())
        .metrics(&scope, institution)
        .await?;
    let figures: Vec<serde_json::Value> = figures
        .iter()
        .map(|m| serde_json::json!({"key": m.key, "label": m.label, "value": m.value.to_string(), "unit": m.unit}))
        .collect();
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let c = load_case(&mut tx, t, b.case_id, false).await?;
    if c.institution_id != institution {
        return Err(invalid("caseId", "unknown_case", "no governance case with this id").into());
    }
    if c.status != GovernanceStatus::Closed {
        return Err(KernelError::Conflict(format!(
            "governance case {} is {}; only a closed case becomes evidence",
            c.number, c.status
        ))
        .into());
    }
    let title = b
        .title
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| c.title.clone());
    if title.chars().count() > 200 {
        return Err(invalid("title", "too_long", "at most 200 characters").into());
    }
    let summary = b
        .summary
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());
    let facts = serde_json::json!({
        "caseNumber": c.number, "caseKind": c.kind, "outcome": c.outcome,
        "rationale": c.rationale, "decidedBy": c.decided_by, "caseFacts": c.facts,
        "campusFigures": figures,
    });
    let eid: Uuid = sqlx::query_scalar(
        "INSERT INTO campus.governance_evidence (tenant_id, institution_id, case_id, title, \
         summary, facts, created_by) VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(c.id)
    .bind(&title)
    .bind(summary.as_deref())
    .bind(&facts)
    .bind(a.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    history(
        &mut tx,
        &Transition {
            tenant: t,
            institution,
            aggregate_type: "governance_evidence",
            aggregate_id: eid,
            from: None,
            to: "draft",
            reason: None,
            process_instance_id: None,
            cause: "sutraerp.campus.governance_evidence.created",
        },
        &a,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "campus.governance_evidence.create",
        "governance_evidence",
        eid,
        serde_json::json!({"caseId": c.id, "caseNumber": c.number}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let v = load(&mut conn, t, eid, false).await?;
    Ok(respond(StatusCode::CREATED, &v))
}

/// Governance evidence of an institution (`campus.governance.manage`; `?status=published` lists
/// what accreditation can cite).
#[utoipa::path(get, path = "/api/v1/campus/institutions/{id}/governance-evidence", operation_id = "campus_governance_evidence_list", tag = "campus-governance",
    params(("id" = String, Path, format = Uuid), GovernanceEvidenceQuery), security(("session_cookie" = [])),
    responses((status = 200, body = GovernanceEvidenceList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn evidence_list(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<GovernanceEvidenceQuery>,
) -> ApiResult<Json<GovernanceEvidenceList>> {
    let ctx = auth.require(Permission::GovernanceManage)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution(&st, &scope, institution).await?;
    let status = parse_filter("status", q.status.as_deref(), EvidenceStatus::parse)?;
    let sql = format!(
        "{SELECT} WHERE e.tenant_id = $1 AND e.institution_id = $2 \
         AND ($3::text IS NULL OR e.status = $3) ORDER BY e.created_at DESC LIMIT $4"
    );
    let rows = sqlx::query(&sql)
        .bind(ctx.scope.tenant_id.as_uuid())
        .bind(id)
        .bind(status.map(|s| s.as_str()))
        .bind(limit(q.limit))
        .fetch_all(&st.pool)
        .await
        .map_err(map_db_error)?;
    let items = rows
        .iter()
        .map(from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)?;
    Ok(Json(GovernanceEvidenceList { items }))
}

/// One piece of governance evidence (`campus.governance.manage`).
#[utoipa::path(get, path = "/api/v1/campus/governance-evidence/{id}", operation_id = "campus_governance_evidence_get", tag = "campus-governance",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = GovernanceEvidenceView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn evidence_get(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::GovernanceManage)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let v = load(&mut conn, ctx.scope.tenant_id, id, false).await?;
    Ok(respond(StatusCode::OK, &v))
}

/// Publish draft evidence for accreditation (`campus.governance.manage`): emits
/// `governance_evidence.published`; publishing twice is a 409.
#[utoipa::path(post, path = "/api/v1/campus/governance-evidence/{id}/publish", operation_id = "campus_governance_evidence_publish", tag = "campus-governance",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the evidence")),
    request_body = FacilityReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = GovernanceEvidenceView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn evidence_publish(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(_b): Json<FacilityReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::GovernanceManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let e = load(&mut tx, t, id, true).await?;
    ensure_version(v, e.version)?;
    let institution = InstitutionId(e.institution_id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    if e.status != EvidenceStatus::Draft {
        return Err(KernelError::Conflict(format!(
            "governance evidence {} is already published",
            e.title
        ))
        .into());
    }
    let nv: i64 = sqlx::query_scalar(
        "UPDATE campus.governance_evidence SET status = 'published', published_at = now(), \
         published_by = $3, version = version + 1, updated_at = now() \
         WHERE id = $1 AND tenant_id = $2 RETURNING version",
    )
    .bind(id)
    .bind(t.as_uuid())
    .bind(a.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    history(
        &mut tx,
        &Transition {
            tenant: t,
            institution,
            aggregate_type: "governance_evidence",
            aggregate_id: id,
            from: Some("draft"),
            to: "published",
            reason: None,
            process_instance_id: None,
            cause: GOVERNANCE_EVIDENCE_PUBLISHED,
        },
        &a,
    )
    .await?;
    publish::event(
        &mut tx,
        &scope,
        GOVERNANCE_EVIDENCE_PUBLISHED,
        GOVERNANCE_EVIDENCE_PUBLISHED_SCHEMA,
        id.to_string(),
        &GovernanceEvidencePublishedV1 {
            evidence_id: id,
            institution_id: e.institution_id,
            case_id: e.case_id,
            case_kind: e.case_kind.as_str().to_owned(),
            case_number: e.case_number.clone(),
            case_outcome: e.case_outcome.clone(),
            title: e.title.clone(),
            summary: e.summary.clone(),
            version: nv,
        },
        &a,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "campus.governance_evidence.publish",
        "governance_evidence",
        id,
        serde_json::json!({"caseNumber": e.case_number}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let out = load(&mut conn, t, id, false).await?;
    Ok(respond(StatusCode::OK, &out))
}

pub fn router(state: CampusState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(evidence_create, evidence_list))
        .routes(routes!(evidence_get))
        .routes(routes!(evidence_publish))
        .with_state(state)
}
