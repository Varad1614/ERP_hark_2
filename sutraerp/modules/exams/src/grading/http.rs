//! HTTP API of grading schemes (tag `exam-grading`; contract §5.2).

use crate::shared::grading::{fmt2, validate_bands};
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, if_match, with_etag};
use crate::shared::publish::{audit, event};
use crate::shared::rows::load_scheme;
use crate::shared::scope::institution_scope;
use crate::shared::ExamsState;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::str::FromStr;
use sutra_contracts::exams::events::{
    ModifiedV1, StatusChangedV1, GRADING_SCHEME_CREATED, GRADING_SCHEME_CREATED_SCHEMA,
    GRADING_SCHEME_RETIRED, GRADING_SCHEME_RETIRED_SCHEMA,
};
use sutra_contracts::exams::ids::GradingSchemeId;
use sutra_contracts::exams::ports::{GradeBand, GradingSchemeSummary};
use sutra_contracts::exams::status::GradingSchemeStatus;
use sutra_kernel::auth::Permission;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::InstitutionId;
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// One band of a grading scheme (decimals as strings).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GradeBandView {
    #[schema(example = "A+")]
    pub grade: String,
    /// Minimum percentage (0 … 100) earning this grade.
    #[schema(example = "80.00")]
    pub min_percent: String,
    /// Grade point (0 … 10).
    #[schema(example = "9.00")]
    pub grade_point: String,
    pub is_pass: bool,
}

/// A grading scheme with its bands (highest first).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GradingSchemeView {
    pub id: GradingSchemeId,
    pub institution_id: Uuid,
    pub code: String,
    pub name: String,
    pub status: GradingSchemeStatus,
    pub bands: Vec<GradeBandView>,
    pub version: i64,
}

impl From<&GradingSchemeSummary> for GradingSchemeView {
    fn from(s: &GradingSchemeSummary) -> Self {
        Self {
            id: s.id,
            institution_id: s.institution_id.as_uuid(),
            code: s.code.clone(),
            name: s.name.clone(),
            status: s.status,
            bands: s
                .bands
                .iter()
                .map(|b| GradeBandView {
                    grade: b.grade.clone(),
                    min_percent: fmt2(b.min_percent),
                    grade_point: fmt2(b.grade_point),
                    is_pass: b.is_pass,
                })
                .collect(),
            version: s.version,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct GradingSchemeList {
    pub items: Vec<GradingSchemeView>,
}

/// New grading scheme.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GradingSchemeCreate {
    /// `^[A-Z0-9][A-Z0-9_-]{1,31}$`, unique per institution.
    #[schema(example = "TEN-POINT")]
    pub code: String,
    pub name: String,
    pub bands: Vec<GradeBandView>,
}

/// Optional reason of a state command.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct ReasonBody {
    pub reason: Option<String>,
}

fn invalid(field: &str, code: &str, message: &str) -> KernelError {
    KernelError::Validation(vec![FieldViolation::new(field, code, message)])
}

fn parse_dec(field: &str, raw: &str) -> Result<Decimal, KernelError> {
    Decimal::from_str(raw.trim()).map_err(|_| {
        invalid(
            field,
            "invalid_number",
            "must be a decimal string such as \"8.50\"",
        )
    })
}

fn valid_code(code: &str) -> bool {
    let b = code.as_bytes();
    (2..=32).contains(&b.len())
        && (b[0].is_ascii_uppercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == b'_' || *c == b'-')
}

fn valid_grade(grade: &str) -> bool {
    let b = grade.as_bytes();
    (1..=4).contains(&b.len())
        && b[0].is_ascii_uppercase()
        && b[1..]
            .iter()
            .all(|c| c.is_ascii_uppercase() || *c == b'+' || *c == b'-')
}

async fn get_scheme(
    st: &ExamsState,
    tenant: sutra_kernel::scope::TenantId,
    id: GradingSchemeId,
) -> Result<GradingSchemeSummary, KernelError> {
    load_scheme(&st.pool, tenant, id)
        .await?
        .ok_or(KernelError::NotFound {
            entity: "grading scheme",
        })
}

/// Grading schemes of an institution (`exam.read`).
#[utoipa::path(get, path = "/api/v1/exams/institutions/{id}/grading-schemes", operation_id = "exam_grading_scheme_list", tag = "exam-grading",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = GradingSchemeList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn scheme_list(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<GradingSchemeList>> {
    let ctx = auth.require(Permission::ExamRead)?;
    let scope = institution_scope(ctx, InstitutionId(id));
    require_institution(&st, &scope, InstitutionId(id)).await?;
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM exams.grading_scheme WHERE tenant_id = $1 AND institution_id = $2 \
         ORDER BY status, code",
    )
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let mut items = Vec::with_capacity(ids.len());
    for s in ids {
        let scheme = get_scheme(&st, ctx.scope.tenant_id, GradingSchemeId(s)).await?;
        items.push(GradingSchemeView::from(&scheme));
    }
    Ok(Json(GradingSchemeList { items }))
}

/// Create a grading scheme (`exam.setup.manage`). Bands are validated (one band from 0 %, at
/// least one pass and one failing band, unique grades / minimums, monotonic grade points).
#[utoipa::path(post, path = "/api/v1/exams/institutions/{id}/grading-schemes", operation_id = "exam_grading_scheme_create", tag = "exam-grading",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = GradingSchemeCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = GradingSchemeView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn scheme_create(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<GradingSchemeCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamSetupManage)?;
    require_idempotency_key(&headers)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let code = b.code.trim().to_owned();
    if !valid_code(&code) {
        return Err(invalid(
            "code",
            "invalid_code",
            "must match ^[A-Z0-9][A-Z0-9_-]{1,31}$",
        )
        .into());
    }
    let name = b.name.trim().to_owned();
    if name.is_empty() || name.chars().count() > 200 {
        return Err(invalid("name", "invalid_length", "must be 1 … 200 characters").into());
    }
    let mut bands = Vec::with_capacity(b.bands.len());
    for (i, band) in b.bands.iter().enumerate() {
        let grade = band.grade.trim().to_owned();
        if !valid_grade(&grade) {
            return Err(invalid(
                &format!("bands[{i}].grade"),
                "invalid_grade",
                "1 … 4 characters: an uppercase letter, then letters, + or -",
            )
            .into());
        }
        bands.push(GradeBand {
            grade,
            min_percent: parse_dec(&format!("bands[{i}].minPercent"), &band.min_percent)?,
            grade_point: parse_dec(&format!("bands[{i}].gradePoint"), &band.grade_point)?,
            is_pass: band.is_pass,
        });
    }
    validate_bands(&bands)?;
    let act = actor(ctx);
    let tenant = ctx.scope.tenant_id;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let scheme_id: Uuid = sqlx::query_scalar(
        "INSERT INTO exams.grading_scheme (tenant_id, institution_id, code, name, created_by) \
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(tenant.as_uuid())
    .bind(id)
    .bind(&code)
    .bind(&name)
    .bind(act.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    for band in &bands {
        sqlx::query(
            "INSERT INTO exams.grade_band (tenant_id, scheme_id, grade, min_percent, grade_point, \
             is_pass) VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(tenant.as_uuid())
        .bind(scheme_id)
        .bind(&band.grade)
        .bind(band.min_percent)
        .bind(band.grade_point)
        .bind(band.is_pass)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    }
    event(
        &mut tx,
        &scope,
        GRADING_SCHEME_CREATED,
        GRADING_SCHEME_CREATED_SCHEMA,
        scheme_id.to_string(),
        &ModifiedV1 {
            aggregate_id: scheme_id,
            institution_id: id,
            version: 1,
            changed_fields: vec!["code".into(), "name".into(), "bands".into()],
        },
        &act,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "exams.grading_scheme.create",
        "grading_scheme",
        scheme_id.to_string(),
        json!({"code": code, "bands": bands.len()}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let s = get_scheme(&st, tenant, GradingSchemeId(scheme_id)).await?;
    Ok(with_etag(
        StatusCode::CREATED,
        &GradingSchemeView::from(&s),
        Version(s.version),
    ))
}

/// One grading scheme (`exam.read`).
#[utoipa::path(get, path = "/api/v1/exams/grading-schemes/{id}", operation_id = "exam_grading_scheme_get", tag = "exam-grading",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = GradingSchemeView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn scheme_get(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamRead)?;
    let s = get_scheme(&st, ctx.scope.tenant_id, GradingSchemeId(id)).await?;
    Ok(with_etag(
        StatusCode::OK,
        &GradingSchemeView::from(&s),
        Version(s.version),
    ))
}

/// Retire a grading scheme (`exam.setup.manage`): no new session may bind it; results already
/// computed under it keep it.
#[utoipa::path(post, path = "/api/v1/exams/grading-schemes/{id}/retire", operation_id = "exam_grading_scheme_retire", tag = "exam-grading",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the grading scheme")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = GradingSchemeView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn scheme_retire(
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
    let tenant = ctx.scope.tenant_id;
    let current = get_scheme(&st, tenant, GradingSchemeId(id)).await?;
    let scope = institution_scope(ctx, current.institution_id);
    require_institution_writable(&st, &scope, current.institution_id).await?;
    if current.version != expected.0 {
        return Err(KernelError::VersionConflict {
            expected: expected.0,
            actual: current.version,
        }
        .into());
    }
    if current.status != GradingSchemeStatus::Active {
        return Err(KernelError::Conflict("the grading scheme is already retired".into()).into());
    }
    let reason = body
        .and_then(|Json(b)| b.reason)
        .map(|r| r.trim().to_owned())
        .filter(|r| !r.is_empty());
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let version: i64 = sqlx::query_scalar(
        "UPDATE exams.grading_scheme SET status = 'retired', status_reason = $3, \
         version = version + 1, updated_at = now() \
         WHERE id = $1 AND tenant_id = $2 AND version = $4 RETURNING version",
    )
    .bind(id)
    .bind(tenant.as_uuid())
    .bind(reason.as_deref())
    .bind(current.version)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?
    .ok_or_else(|| KernelError::Conflict("grading scheme changed concurrently".into()))?;
    event(
        &mut tx,
        &scope,
        GRADING_SCHEME_RETIRED,
        GRADING_SCHEME_RETIRED_SCHEMA,
        id.to_string(),
        &StatusChangedV1 {
            aggregate_id: id,
            institution_id: current.institution_id.as_uuid(),
            session_id: None,
            paper_id: None,
            registration_id: None,
            student_id: None,
            from_status: Some("active".into()),
            to_status: "retired".into(),
            version,
            reason: reason.clone(),
            process_instance_id: None,
            student_subject_ref: None,
        },
        &act,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "exams.grading_scheme.retire",
        "grading_scheme",
        id.to_string(),
        json!({"reason": reason}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let s = get_scheme(&st, tenant, GradingSchemeId(id)).await?;
    Ok(with_etag(
        StatusCode::OK,
        &GradingSchemeView::from(&s),
        Version(s.version),
    ))
}

pub fn router(state: ExamsState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(scheme_list, scheme_create))
        .routes(routes!(scheme_get))
        .routes(routes!(scheme_retire))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_and_grades_are_validated() {
        assert!(valid_code("TEN-POINT") && valid_code("G1"));
        assert!(!valid_code("x") && !valid_code("-AB") && !valid_code("ab"));
        assert!(valid_grade("A+") && valid_grade("O") && valid_grade("B-"));
        assert!(!valid_grade("a") && !valid_grade("+A") && !valid_grade("ABCDE"));
    }
}
