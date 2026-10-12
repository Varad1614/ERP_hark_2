//! Programme catalogue (L2-B04, L3-B-023/025): `draft → published → retired`. A programme refers
//! to the published academic structure by level / degree-type key; its versioned academic
//! content lives in curriculum versions.

use super::domain::{ensure_catalogue, ensure_editable, parse_status};
use super::publication::{head, move_status};
use super::structure::{self, keys_of, NodeKind, PublicationQuery};
use super::support::{
    begin, body, code_format, expect_version, finish, institution_of, key_ok, required_text,
    violation, ReasonBody, Transition,
};
use crate::shared::http::{actor, if_match, with_etag};
use crate::shared::scope::institution_scope;
use crate::shared::{guard, publish, Actor, InstitutionState};
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, Utc};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, PgPool, Row};
use sutra_contracts::institution::events::curriculum::{
    ProgrammeV1, PROGRAMME_PUBLISHED, PROGRAMME_PUBLISHED_SCHEMA, PROGRAMME_REGISTERED,
    PROGRAMME_REGISTERED_SCHEMA, PROGRAMME_RETIRED, PROGRAMME_RETIRED_SCHEMA,
};
use sutra_contracts::institution::ids::{CampusId, OrgUnitId, ProgrammeId};
use sutra_contracts::institution::status::PublicationStatus;
use sutra_kernel::auth::Permission;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use uuid::Uuid;

const ENTITY: &str = "programme";

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Programme {
    pub id: ProgrammeId,
    pub institution_id: InstitutionId,
    /// Owning organisation unit (2A), when assigned.
    pub org_unit_id: Option<OrgUnitId>,
    #[schema(example = "BTECH-CSE")]
    pub code: String,
    pub name: String,
    /// Level key of the academic structure (e.g. `ug`).
    pub level: String,
    /// Degree-type key of the academic structure (e.g. `btech`).
    pub degree_type: Option<String>,
    pub duration_terms: i32,
    /// Campuses offering the programme.
    pub campus_ids: Vec<CampusId>,
    pub version_no: i32,
    pub status: PublicationStatus,
    pub status_reason: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
    pub retired_at: Option<DateTime<Utc>>,
    pub version: Version,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct ProgrammeList {
    pub items: Vec<Programme>,
}

/// `POST /institutions/{id}/programmes`.
#[derive(Clone, Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RegisterProgramme {
    #[schema(example = "BTECH-CSE")]
    pub code: String,
    #[schema(example = "B.Tech Computer Science and Engineering")]
    pub name: String,
    #[schema(example = "ug")]
    pub level: String,
    pub degree_type: Option<String>,
    #[schema(example = 8)]
    pub duration_terms: i32,
    pub org_unit_id: Option<OrgUnitId>,
    #[serde(default)]
    pub campus_ids: Vec<CampusId>,
}

/// `PATCH /programmes/{id}` (draft only): full replacement of the editable members.
#[derive(Clone, Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateProgramme {
    pub name: String,
    pub level: String,
    pub degree_type: Option<String>,
    pub duration_terms: i32,
    pub org_unit_id: Option<OrgUnitId>,
    #[serde(default)]
    pub campus_ids: Vec<CampusId>,
}

fn validate_common(
    name: &str,
    level: &str,
    degree_type: Option<&String>,
    duration: i32,
    v: &mut Vec<FieldViolation>,
) {
    required_text("name", name, 200, v);
    if !key_ok(level, 40) {
        v.push(violation(
            "level",
            "format",
            "level must be a structure key such as `ug`",
        ));
    }
    if let Some(d) = degree_type {
        if !key_ok(d, 40) {
            v.push(violation(
                "degreeType",
                "format",
                "degree type must be a structure key",
            ));
        }
    }
    if !(1..=24).contains(&duration) {
        v.push(violation(
            "durationTerms",
            "range",
            "duration must be 1-24 terms",
        ));
    }
}

impl RegisterProgramme {
    pub fn validate(&self) -> Result<(), KernelError> {
        let mut v = vec![];
        code_format("code", &self.code, &mut v);
        validate_common(
            &self.name,
            &self.level,
            self.degree_type.as_ref(),
            self.duration_terms,
            &mut v,
        );
        finish(v)
    }
}

impl UpdateProgramme {
    pub fn validate(&self) -> Result<(), KernelError> {
        let mut v = vec![];
        validate_common(
            &self.name,
            &self.level,
            self.degree_type.as_ref(),
            self.duration_terms,
            &mut v,
        );
        finish(v)
    }
}

const COLUMNS: &str = "id, institution_id, org_unit_id, code, name, level, degree_type, \
    duration_terms, campus_ids, version_no, status, status_reason, published_at, retired_at, \
    version, created_at, updated_at";

fn from_row(r: &PgRow) -> Result<Programme, sqlx::Error> {
    let status: String = r.try_get("status")?;
    let campuses: Vec<Uuid> = r.try_get("campus_ids")?;
    Ok(Programme {
        id: ProgrammeId(r.try_get("id")?),
        institution_id: InstitutionId(r.try_get("institution_id")?),
        org_unit_id: r.try_get::<Option<Uuid>, _>("org_unit_id")?.map(OrgUnitId),
        code: r.try_get("code")?,
        name: r.try_get("name")?,
        level: r.try_get("level")?,
        degree_type: r.try_get("degree_type")?,
        duration_terms: r.try_get("duration_terms")?,
        campus_ids: campuses.into_iter().map(CampusId).collect(),
        version_no: r.try_get("version_no")?,
        status: parse_status(&status)?,
        status_reason: r.try_get("status_reason")?,
        published_at: r.try_get("published_at")?,
        retired_at: r.try_get("retired_at")?,
        version: Version(r.try_get("version")?),
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
    })
}

pub(crate) async fn load(
    conn: &mut PgConnection,
    tenant: &ExecutionScope,
    id: Uuid,
) -> Result<Programme, KernelError> {
    let row = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM institution.programme WHERE id = $1 AND tenant_id = $2"
    ))
    .bind(id)
    .bind(tenant.tenant_id.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound { entity: ENTITY })?;
    from_row(&row).map_err(map_db_error)
}

async fn get(pool: &PgPool, tenant: &ExecutionScope, id: Uuid) -> Result<Programme, KernelError> {
    let mut conn = pool.acquire().await.map_err(map_db_error)?;
    load(&mut conn, tenant, id).await
}

/// Validates references that live in other aggregates: offering campuses must be live campuses
/// of the institution; the org unit (2A) must exist when 2A's table is present.
async fn validate_refs(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    institution_id: InstitutionId,
    org_unit: Option<OrgUnitId>,
    campuses: &[CampusId],
) -> Result<(), KernelError> {
    let mut v = vec![];
    let ids: Vec<Uuid> = campuses.iter().map(CampusId::as_uuid).collect();
    let mut unique = ids.clone();
    unique.sort_unstable();
    unique.dedup();
    if unique.len() != ids.len() {
        v.push(violation(
            "campusIds",
            "duplicate",
            "a campus is listed twice",
        ));
    }
    let live = crate::campus::repo::live_campus_ids(conn, scope, institution_id, &ids).await?;
    for (i, c) in ids.iter().enumerate() {
        if !live.contains(c) {
            v.push(violation(
                &format!("campusIds[{i}]"),
                "unknown",
                "not a live campus of this institution",
            ));
        }
    }
    if let Some(ou) = org_unit {
        // Read-only cross-slice check (contract §2): 2A's table, when deployed.
        let has_table: Option<String> =
            sqlx::query_scalar("SELECT to_regclass('institution.org_unit')::text")
                .fetch_one(&mut *conn)
                .await
                .map_err(map_db_error)?;
        if has_table.is_some() {
            let found: Option<i64> = sqlx::query_scalar(
                "SELECT 1::bigint FROM institution.org_unit WHERE id = $1 AND tenant_id = $2 \
                 AND institution_id = $3 AND status NOT IN ('decommissioned', 'archived')",
            )
            .bind(ou.as_uuid())
            .bind(scope.tenant_id.as_uuid())
            .bind(institution_id.as_uuid())
            .fetch_optional(&mut *conn)
            .await
            .map_err(map_db_error)?;
            if found.is_none() {
                v.push(violation(
                    "orgUnitId",
                    "unknown",
                    "not a live organisation unit of this institution",
                ));
            }
        }
    }
    finish(v)
}

fn event(p: &Programme) -> ProgrammeV1 {
    ProgrammeV1 {
        programme_id: p.id.as_uuid(),
        institution_id: p.institution_id.as_uuid(),
        org_unit_id: p.org_unit_id.map(|o| o.as_uuid()),
        code: p.code.clone(),
        name: p.name.clone(),
        level: p.level.clone(),
        duration_terms: p.duration_terms,
        version: p.version.0,
    }
}

async fn emit(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    p: &Programme,
    published: bool,
    actor: &Actor,
) -> Result<(), KernelError> {
    let (t, s, action) = if published {
        (
            PROGRAMME_PUBLISHED,
            PROGRAMME_PUBLISHED_SCHEMA,
            "institution.programme.publish",
        )
    } else {
        (
            PROGRAMME_REGISTERED,
            PROGRAMME_REGISTERED_SCHEMA,
            "institution.programme.register",
        )
    };
    publish::event(conn, scope, t, s, p.id.to_string(), &event(p), actor).await?;
    publish::audit(
        conn,
        scope,
        action,
        ENTITY,
        p.id.to_string(),
        serde_json::json!({"code": p.code, "level": p.level, "status": p.status, "version": p.version}),
        actor,
    )
    .await
}

fn ok(p: &Programme) -> Response {
    with_etag(StatusCode::OK, p, p.version)
}

/// Register a draft programme in the catalogue (`curriculum.manage`).
#[utoipa::path(
    post,
    path = "/api/v1/institution/institutions/{id}/programmes",
    operation_id = "programme_register",
    tag = "curriculum",
    params(
        ("id" = String, Path, format = Uuid, description = "Institution id"),
        ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key."),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = RegisterProgramme,
    responses(
        (status = 201, body = Programme, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn register(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    headers: HeaderMap,
    Json(cmd): Json<RegisterProgramme>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CurriculumManage)?;
    require_idempotency_key(&headers)?;
    cmd.validate()?;
    let scope = institution_scope(ctx, id);
    let mut tx = begin(&st.pool, &scope, id).await?;
    validate_refs(&mut tx, &scope, id, cmd.org_unit_id, &cmd.campus_ids).await?;
    let pid = ProgrammeId::new().as_uuid();
    sqlx::query(
        "INSERT INTO institution.programme (id, tenant_id, institution_id, org_unit_id, code, name, \
         level, degree_type, duration_terms, campus_ids) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(pid)
    .bind(scope.tenant_id.as_uuid())
    .bind(id.as_uuid())
    .bind(cmd.org_unit_id.map(|o| o.as_uuid()))
    .bind(cmd.code.trim())
    .bind(cmd.name.trim())
    .bind(cmd.level.as_str())
    .bind(cmd.degree_type.as_deref())
    .bind(cmd.duration_terms)
    .bind(cmd.campus_ids.iter().map(CampusId::as_uuid).collect::<Vec<_>>())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let p = load(&mut tx, &scope, pid).await?;
    emit(&mut tx, &scope, &p, false, &actor(ctx)).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::CREATED, &p, p.version))
}

/// List the programme catalogue of an institution (`institution.read`).
#[utoipa::path(
    get,
    path = "/api/v1/institution/institutions/{id}/programmes",
    operation_id = "programme_list",
    tag = "curriculum",
    params(("id" = String, Path, format = Uuid, description = "Institution id"), PublicationQuery),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = ProgrammeList),
        (status = 404, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn list(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    Query(q): Query<PublicationQuery>,
) -> ApiResult<Json<ProgrammeList>> {
    let ctx = auth.require(Permission::InstitutionRead)?;
    let scope = institution_scope(ctx, id);
    guard::institution_status(&st.pool, &scope, id).await?;
    let rows = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM institution.programme WHERE tenant_id = $1 AND institution_id = $2 \
         AND ($3::text IS NULL OR status = $3) ORDER BY code"
    ))
    .bind(scope.tenant_id.as_uuid())
    .bind(id.as_uuid())
    .bind(q.status.map(|s| s.as_str()))
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let items = rows
        .iter()
        .map(from_row)
        .collect::<Result<_, _>>()
        .map_err(map_db_error)?;
    Ok(Json(ProgrammeList { items }))
}

/// Get one programme (`institution.read`).
#[utoipa::path(
    get,
    path = "/api/v1/institution/programmes/{programmeId}",
    operation_id = "programme_get",
    tag = "curriculum",
    params(("programmeId" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = Programme, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn get_one(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(programme_id): Path<ProgrammeId>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::InstitutionRead)?;
    Ok(ok(&get(&st.pool, &ctx.scope, programme_id.as_uuid()).await?))
}

/// Edit a draft programme (`curriculum.manage`). Published programmes are immutable (409).
#[utoipa::path(
    patch,
    path = "/api/v1/institution/programmes/{programmeId}",
    operation_id = "programme_update",
    tag = "curriculum",
    params(
        ("programmeId" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the version being modified"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = UpdateProgramme,
    responses(
        (status = 200, body = Programme, headers(("ETag" = String))),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
        (status = 428, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn update(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(programme_id): Path<ProgrammeId>,
    headers: HeaderMap,
    Json(cmd): Json<UpdateProgramme>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CurriculumManage)?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(res) => return Ok(res),
    };
    cmd.validate()?;
    let id = programme_id.as_uuid();
    let inst = institution_of(&st.pool, &ctx.scope, "programme", ENTITY, id).await?;
    let scope = institution_scope(ctx, inst);
    let mut tx = begin(&st.pool, &scope, inst).await?;
    let (status, version) = head(&mut tx, &scope, "programme", ENTITY, id, true).await?;
    expect_version(expected, version)?;
    ensure_editable("programme", status)?;
    validate_refs(&mut tx, &scope, inst, cmd.org_unit_id, &cmd.campus_ids).await?;
    sqlx::query(
        "UPDATE institution.programme SET name = $1, level = $2, degree_type = $3, \
         duration_terms = $4, org_unit_id = $5, campus_ids = $6, version = version + 1, \
         updated_at = now() WHERE id = $7 AND tenant_id = $8",
    )
    .bind(cmd.name.trim())
    .bind(cmd.level.as_str())
    .bind(cmd.degree_type.as_deref())
    .bind(cmd.duration_terms)
    .bind(cmd.org_unit_id.map(|o| o.as_uuid()))
    .bind(
        cmd.campus_ids
            .iter()
            .map(CampusId::as_uuid)
            .collect::<Vec<_>>(),
    )
    .bind(id)
    .bind(scope.tenant_id.as_uuid())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    publish::audit(
        &mut tx,
        &scope,
        "institution.programme.update",
        ENTITY,
        id.to_string(),
        serde_json::json!({"fromVersion": expected.0}),
        &actor(ctx),
    )
    .await?;
    let p = load(&mut tx, &scope, id).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(ok(&p))
}

/// Publish a draft programme (`curriculum.publish`). Its level / degree type must exist in the
/// published academic structure (coherent publication set, L4-B-023-BH-002).
#[utoipa::path(
    post,
    path = "/api/v1/institution/programmes/{programmeId}/publish",
    operation_id = "programme_publish",
    tag = "curriculum",
    params(
        ("programmeId" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the current version"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 200, body = Programme, headers(("ETag" = String))),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn publish_one(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(programme_id): Path<ProgrammeId>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CurriculumPublish)?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(res) => return Ok(res),
    };
    let id = programme_id.as_uuid();
    let inst = institution_of(&st.pool, &ctx.scope, "programme", ENTITY, id).await?;
    let scope = institution_scope(ctx, inst);
    let mut tx = begin(&st.pool, &scope, inst).await?;
    let (status, version) = head(&mut tx, &scope, "programme", ENTITY, id, true).await?;
    expect_version(expected, version)?;
    ensure_catalogue("programme", status, PublicationStatus::Published)?;
    let p = load(&mut tx, &scope, id).await?;
    let Some(s) = structure::published(&mut tx, &scope, inst).await? else {
        return Err(KernelError::Conflict(
            "publish the academic structure before its programmes".into(),
        )
        .into());
    };
    let mut v = vec![];
    if !keys_of(&s, NodeKind::Level).contains(p.level.as_str()) {
        v.push(violation(
            "level",
            "unknown",
            "level is not in the published academic structure",
        ));
    }
    if let Some(d) = &p.degree_type {
        if !keys_of(&s, NodeKind::DegreeType).contains(d.as_str()) {
            v.push(violation(
                "degreeType",
                "unknown",
                "degree type is not in the published academic structure",
            ));
        }
    }
    finish(v)?;
    validate_refs(&mut tx, &scope, inst, p.org_unit_id, &p.campus_ids).await?;
    move_status(
        &mut tx,
        &scope,
        "programme",
        id,
        PublicationStatus::Published,
        None,
        None,
    )
    .await?;
    let p = load(&mut tx, &scope, id).await?;
    emit(&mut tx, &scope, &p, true, &actor(ctx)).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(ok(&p))
}

/// Retire a published programme (L3-B-025, `curriculum.publish`, reason required). Blocked while
/// one of its curricula is still published.
#[utoipa::path(
    post,
    path = "/api/v1/institution/programmes/{programmeId}/retire",
    operation_id = "programme_retire",
    tag = "curriculum",
    params(
        ("programmeId" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the current version"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = ReasonBody,
    responses(
        (status = 200, body = Programme, headers(("ETag" = String))),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn retire(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(programme_id): Path<ProgrammeId>,
    headers: HeaderMap,
    raw: Bytes,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CurriculumPublish)?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(res) => return Ok(res),
    };
    let reason = body::<ReasonBody>(&raw)?.validated(true)?;
    let id = programme_id.as_uuid();
    let inst = institution_of(&st.pool, &ctx.scope, "programme", ENTITY, id).await?;
    let scope = institution_scope(ctx, inst);
    let mut tx = begin(&st.pool, &scope, inst).await?;
    let (status, version) = head(&mut tx, &scope, "programme", ENTITY, id, true).await?;
    expect_version(expected, version)?;
    ensure_catalogue("programme", status, PublicationStatus::Retired)?;
    let live: Vec<String> = sqlx::query_scalar(
        "SELECT code FROM institution.curriculum WHERE tenant_id = $1 AND programme_id = $2 \
         AND status = 'published' ORDER BY code",
    )
    .bind(scope.tenant_id.as_uuid())
    .bind(id)
    .fetch_all(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if !live.is_empty() {
        return Err(KernelError::Conflict(format!(
            "retire the programme's published curricula first: {}",
            live.join(", ")
        ))
        .into());
    }
    let v = move_status(
        &mut tx,
        &scope,
        "programme",
        id,
        PublicationStatus::Retired,
        reason.as_deref(),
        None,
    )
    .await?;
    Transition {
        event_type: PROGRAMME_RETIRED,
        dataschema: PROGRAMME_RETIRED_SCHEMA,
        audit_action: "institution.programme.retire",
        target_type: ENTITY,
        aggregate_id: id,
        from: status.as_str(),
        to: PublicationStatus::Retired.as_str(),
        version: v,
        reason,
        process_instance_id: None,
    }
    .emit(&mut tx, &scope, &actor(ctx))
    .await?;
    let p = load(&mut tx, &scope, id).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(ok(&p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registration_validation() {
        let mut r = RegisterProgramme {
            code: "BTECH-CSE".into(),
            name: "B.Tech CSE".into(),
            level: "ug".into(),
            degree_type: Some("btech".into()),
            duration_terms: 8,
            org_unit_id: None,
            campus_ids: vec![],
        };
        assert!(r.validate().is_ok());
        r.level = "UG".into();
        r.duration_terms = 0;
        r.code = "x".into();
        let Err(KernelError::Validation(v)) = r.validate() else {
            panic!("expected violations")
        };
        let f: Vec<_> = v.iter().map(|x| x.field.as_str()).collect();
        assert_eq!(f, vec!["code", "level", "durationTerms"]);
    }
}
