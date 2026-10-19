//! School academic model (L2-B04, L3-B-026): grade levels (classes) with their sections and
//! subjects, aligned to the NEP 2020 5+3+3+4 stages. It is deliberately not a programme /
//! curriculum version. `draft → in_review → published`, successor versions supersede.

use super::domain::{ensure, ensure_editable, parse_status};
use super::publication::{self, head, move_status, supersede_current, Kind};
use super::structure::{self, AcademicModel, PublicationQuery};
use super::support::{
    begin, body, code_format, expect_version, finish, institution_of, key_ok, required_text,
    violation, DecisionBody,
};
use crate::shared::http::{actor, if_match, with_etag};
use crate::shared::scope::institution_scope;
use crate::shared::{guard, publish, Actor, InstitutionState};
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, NaiveDate, Utc};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, PgPool, Row};
use std::collections::HashSet;
use sutra_contracts::institution::events::curriculum::{
    SchoolAcademicModelPublishedV1, SCHOOL_MODEL_PUBLISHED, SCHOOL_MODEL_PUBLISHED_SCHEMA,
};
use sutra_contracts::institution::ids::SchoolAcademicModelId;
use sutra_contracts::institution::status::PublicationStatus;
use sutra_kernel::auth::Permission;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use uuid::Uuid;

const ENTITY: &str = "school_model";
const TABLE: &str = "school_academic_model";

/// NEP 2020 school stage (5+3+3+4).
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    serde::Serialize,
    serde::Deserialize,
    utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum NepStage {
    Foundational,
    Preparatory,
    Middle,
    Secondary,
}

impl NepStage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Foundational => "foundational",
            Self::Preparatory => "preparatory",
            Self::Middle => "middle",
            Self::Secondary => "secondary",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [
            Self::Foundational,
            Self::Preparatory,
            Self::Middle,
            Self::Secondary,
        ]
        .into_iter()
        .find(|x| x.as_str() == s)
    }
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum SubjectKind {
    Core,
    Elective,
    Language,
    CoScholastic,
}

/// A subject taught in a grade level.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SchoolSubject {
    #[schema(example = "MATH")]
    pub code: String,
    pub name: String,
    pub kind: SubjectKind,
}

/// A grade level (class) with its sections and subjects.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GradeLevel {
    /// Lowercase key, e.g. `nursery`, `1` … `12`.
    #[schema(example = "6")]
    pub key: String,
    #[schema(example = "Class 6")]
    pub name: String,
    pub nep_stage: Option<NepStage>,
    #[serde(default)]
    pub sections: Vec<String>,
    #[serde(default)]
    pub subjects: Vec<SchoolSubject>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SchoolModel {
    pub id: SchoolAcademicModelId,
    pub institution_id: InstitutionId,
    pub name: String,
    /// Board / affiliation key, e.g. `cbse`.
    pub board: Option<String>,
    pub nep_stage_aligned: bool,
    pub version_no: i32,
    pub supersedes_id: Option<SchoolAcademicModelId>,
    pub effective_from: Option<NaiveDate>,
    pub status: PublicationStatus,
    pub process_instance_id: Option<String>,
    pub status_reason: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
    pub grade_levels: Vec<GradeLevel>,
    pub version: Version,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct SchoolModelList {
    pub items: Vec<SchoolModel>,
}

/// Draft content (create and full replacement on `PATCH`).
#[derive(Clone, Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SchoolModelDraft {
    pub name: String,
    #[schema(example = "cbse")]
    pub board: Option<String>,
    #[serde(default = "yes")]
    pub nep_stage_aligned: bool,
    pub effective_from: Option<NaiveDate>,
    #[serde(default)]
    pub grade_levels: Vec<GradeLevel>,
}

fn yes() -> bool {
    true
}

/// `POST /school-models/{id}/revise` body.
#[derive(Clone, Debug, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviseSchoolModel {
    pub effective_from: Option<NaiveDate>,
}

fn validate_grades(grades: &[GradeLevel], v: &mut Vec<FieldViolation>) {
    let mut keys = HashSet::new();
    for (i, g) in grades.iter().enumerate() {
        let f = |m: &str| format!("gradeLevels[{i}].{m}");
        if !key_ok(&g.key, 16) || g.key.contains('.') {
            v.push(violation(
                &f("key"),
                "format",
                "key must be a short lowercase slug",
            ));
        } else if !keys.insert(g.key.as_str()) {
            v.push(violation(&f("key"), "duplicate", "grade key is used twice"));
        }
        required_text(&f("name"), &g.name, 100, v);
        let mut sections = HashSet::new();
        for (j, s) in g.sections.iter().enumerate() {
            let t = s.trim();
            if t.is_empty() || t.chars().count() > 20 {
                v.push(violation(
                    &format!("gradeLevels[{i}].sections[{j}]"),
                    "format",
                    "section names are 1-20 characters",
                ));
            } else if !sections.insert(t) {
                v.push(violation(
                    &format!("gradeLevels[{i}].sections[{j}]"),
                    "duplicate",
                    "section is listed twice",
                ));
            }
        }
        let mut codes = HashSet::new();
        for (j, s) in g.subjects.iter().enumerate() {
            let fs = |m: &str| format!("gradeLevels[{i}].subjects[{j}].{m}");
            code_format(&fs("code"), &s.code, v);
            if !codes.insert(s.code.trim()) {
                v.push(violation(
                    &fs("code"),
                    "duplicate",
                    "subject code is used twice in the grade",
                ));
            }
            required_text(&fs("name"), &s.name, 100, v);
        }
    }
}

impl SchoolModelDraft {
    pub fn validate(&self) -> Result<(), KernelError> {
        let mut v = vec![];
        required_text("name", &self.name, 200, &mut v);
        if let Some(b) = &self.board {
            if !(2..=32).contains(&b.len())
                || !b
                    .bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
            {
                v.push(violation(
                    "board",
                    "format",
                    "board must be a lowercase key such as cbse",
                ));
            }
        }
        if self.grade_levels.len() > 30 {
            v.push(violation(
                "gradeLevels",
                "too-many",
                "at most 30 grade levels",
            ));
        }
        validate_grades(&self.grade_levels, &mut v);
        finish(v)
    }
}

/// Publication rules: at least one grade, each with sections and subjects; NEP-aligned models
/// tag every grade with a stage, in non-decreasing stage order.
pub fn publication_rules(nep: bool, grades: &[GradeLevel]) -> Vec<FieldViolation> {
    let mut v = vec![];
    validate_grades(grades, &mut v);
    if grades.is_empty() {
        v.push(violation(
            "gradeLevels",
            "required-for-publication",
            "at least one grade level is required",
        ));
    }
    let mut last: Option<NepStage> = None;
    for (i, g) in grades.iter().enumerate() {
        if g.sections.is_empty() {
            v.push(violation(
                &format!("gradeLevels[{i}].sections"),
                "required-for-publication",
                "each grade needs a section",
            ));
        }
        if g.subjects.is_empty() {
            v.push(violation(
                &format!("gradeLevels[{i}].subjects"),
                "required-for-publication",
                "each grade needs a subject",
            ));
        }
        if nep {
            match g.nep_stage {
                None => v.push(violation(
                    &format!("gradeLevels[{i}].nepStage"),
                    "nep",
                    "NEP-aligned grades state their stage",
                )),
                Some(s) => {
                    if last.is_some_and(|l| s < l) {
                        v.push(violation(
                            &format!("gradeLevels[{i}].nepStage"),
                            "order",
                            "stages must not go backwards",
                        ));
                    }
                    last = Some(s);
                }
            }
        }
    }
    v
}

const COLUMNS: &str = "id, institution_id, name, board, nep_stage_aligned, version_no, supersedes_id, \
    effective_from, status, process_instance_id, status_reason, published_at, version, created_at, updated_at";

fn from_row(r: &PgRow) -> Result<SchoolModel, sqlx::Error> {
    let status: String = r.try_get("status")?;
    Ok(SchoolModel {
        id: SchoolAcademicModelId(r.try_get("id")?),
        institution_id: InstitutionId(r.try_get("institution_id")?),
        name: r.try_get("name")?,
        board: r.try_get("board")?,
        nep_stage_aligned: r.try_get("nep_stage_aligned")?,
        version_no: r.try_get("version_no")?,
        supersedes_id: r
            .try_get::<Option<Uuid>, _>("supersedes_id")?
            .map(SchoolAcademicModelId),
        effective_from: r.try_get("effective_from")?,
        status: parse_status(&status)?,
        process_instance_id: r.try_get("process_instance_id")?,
        status_reason: r.try_get("status_reason")?,
        published_at: r.try_get("published_at")?,
        grade_levels: vec![],
        version: Version(r.try_get("version")?),
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
    })
}

async fn grades_of(conn: &mut PgConnection, id: Uuid) -> Result<Vec<GradeLevel>, KernelError> {
    let rows = sqlx::query(
        "SELECT key, name, nep_stage, sections, subjects FROM institution.school_grade_level \
         WHERE school_model_id = $1 ORDER BY sort_order",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    rows.iter()
        .map(|r| {
            let stage: Option<String> = r.try_get("nep_stage")?;
            let subjects: serde_json::Value = r.try_get("subjects")?;
            Ok(GradeLevel {
                key: r.try_get("key")?,
                name: r.try_get("name")?,
                nep_stage: stage.as_deref().and_then(NepStage::parse),
                sections: r.try_get("sections")?,
                subjects: serde_json::from_value(subjects)
                    .map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
            })
        })
        .collect::<Result<_, sqlx::Error>>()
        .map_err(map_db_error)
}

async fn insert_grades(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    id: Uuid,
    grades: &[GradeLevel],
) -> Result<(), KernelError> {
    for (i, g) in grades.iter().enumerate() {
        sqlx::query(
            "INSERT INTO institution.school_grade_level (tenant_id, school_model_id, key, name, \
             nep_stage, sections, subjects, sort_order) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(scope.tenant_id.as_uuid())
        .bind(id)
        .bind(g.key.as_str())
        .bind(g.name.trim())
        .bind(g.nep_stage.map(NepStage::as_str))
        .bind(
            g.sections
                .iter()
                .map(|s| s.trim().to_owned())
                .collect::<Vec<_>>(),
        )
        .bind(serde_json::to_value(&g.subjects).map_err(KernelError::internal)?)
        .bind(i32::try_from(i).unwrap_or(i32::MAX))
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    }
    Ok(())
}

pub(crate) async fn load(
    conn: &mut PgConnection,
    tenant: &ExecutionScope,
    id: Uuid,
) -> Result<SchoolModel, KernelError> {
    let row = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM institution.school_academic_model WHERE id = $1 AND tenant_id = $2"
    ))
    .bind(id)
    .bind(tenant.tenant_id.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound { entity: ENTITY })?;
    let mut m = from_row(&row).map_err(map_db_error)?;
    m.grade_levels = grades_of(conn, id).await?;
    Ok(m)
}

async fn get(pool: &PgPool, tenant: &ExecutionScope, id: Uuid) -> Result<SchoolModel, KernelError> {
    let mut conn = pool.acquire().await.map_err(map_db_error)?;
    load(&mut conn, tenant, id).await
}

pub(crate) async fn validate_publication(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    id: Uuid,
) -> Result<(), KernelError> {
    let m = load(conn, scope, id).await?;
    match structure::published(conn, scope, m.institution_id).await? {
        Some(s) if s.model != AcademicModel::HigherEducation => {}
        _ => {
            return Err(KernelError::Conflict(
                "publish a school or mixed academic structure before the school model".into(),
            ))
        }
    }
    finish(publication_rules(m.nep_stage_aligned, &m.grade_levels))
}

pub(crate) async fn finalize_publication(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    id: Uuid,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<(), KernelError> {
    let m = load(conn, scope, id).await?;
    ensure(
        "school academic model",
        m.status,
        PublicationStatus::Published,
    )?;
    supersede_current(
        conn,
        scope,
        TABLE,
        "institution_id",
        m.institution_id.as_uuid(),
        id,
    )
    .await?;
    let v = move_status(
        conn,
        scope,
        TABLE,
        id,
        PublicationStatus::Published,
        reason,
        None,
    )
    .await?;
    let data = SchoolAcademicModelPublishedV1 {
        school_model_id: id,
        institution_id: m.institution_id.as_uuid(),
        version_no: m.version_no,
        grade_levels: m.grade_levels.iter().map(|g| g.key.clone()).collect(),
        board: m.board.clone(),
    };
    publish::event(
        conn,
        scope,
        SCHOOL_MODEL_PUBLISHED,
        SCHOOL_MODEL_PUBLISHED_SCHEMA,
        id.to_string(),
        &data,
        actor,
    )
    .await?;
    publish::audit(
        conn,
        scope,
        "institution.school_model.publish",
        ENTITY,
        id.to_string(),
        serde_json::json!({
            "fromStatus": m.status, "toStatus": "published", "version": v,
            "versionNo": m.version_no, "supersedesId": m.supersedes_id,
            "processInstanceId": m.process_instance_id, "reason": reason,
        }),
        actor,
    )
    .await
}

async fn insert_draft(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    institution_id: InstitutionId,
    d: &SchoolModelDraft,
    supersedes: Option<Uuid>,
    actor: &Actor,
    action: &str,
) -> Result<Uuid, KernelError> {
    let version_no: i32 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(version_no), 0) + 1 FROM institution.school_academic_model \
         WHERE tenant_id = $1 AND institution_id = $2",
    )
    .bind(scope.tenant_id.as_uuid())
    .bind(institution_id.as_uuid())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let id = SchoolAcademicModelId::new().as_uuid();
    sqlx::query(
        "INSERT INTO institution.school_academic_model (id, tenant_id, institution_id, name, board, \
         nep_stage_aligned, version_no, supersedes_id, effective_from) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(id)
    .bind(scope.tenant_id.as_uuid())
    .bind(institution_id.as_uuid())
    .bind(d.name.trim())
    .bind(d.board.as_deref())
    .bind(d.nep_stage_aligned)
    .bind(version_no)
    .bind(supersedes)
    .bind(d.effective_from)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    insert_grades(conn, scope, id, &d.grade_levels).await?;
    publish::audit(
        conn,
        scope,
        action,
        ENTITY,
        id.to_string(),
        serde_json::json!({"versionNo": version_no, "supersedesId": supersedes, "gradeCount": d.grade_levels.len()}),
        actor,
    )
    .await?;
    Ok(id)
}

async fn has_published(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    institution_id: InstitutionId,
) -> Result<bool, KernelError> {
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM institution.school_academic_model WHERE tenant_id = $1 \
         AND institution_id = $2 AND status = 'published'",
    )
    .bind(scope.tenant_id.as_uuid())
    .bind(institution_id.as_uuid())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(n > 0)
}

fn ok(m: &SchoolModel) -> Response {
    with_etag(StatusCode::OK, m, m.version)
}

/// Establish the first school academic model draft (L3-B-026, `curriculum.manage`).
#[utoipa::path(
    post,
    path = "/api/v1/institution/institutions/{id}/school-models",
    operation_id = "school_model_create",
    tag = "curriculum",
    params(
        ("id" = String, Path, format = Uuid, description = "Institution id"),
        ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key."),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = SchoolModelDraft,
    responses(
        (status = 201, body = SchoolModel, headers(("ETag" = String))),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn create(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    headers: HeaderMap,
    Json(d): Json<SchoolModelDraft>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CurriculumManage)?;
    require_idempotency_key(&headers)?;
    d.validate()?;
    let scope = institution_scope(ctx, id);
    let mut tx = begin(&st.pool, &scope, id).await?;
    if has_published(&mut tx, &scope, id).await? {
        return Err(KernelError::Conflict(
            "a school model is already published; revise it to create a successor".into(),
        )
        .into());
    }
    let mid = insert_draft(
        &mut tx,
        &scope,
        id,
        &d,
        None,
        &actor(ctx),
        "institution.school_model.create",
    )
    .await?;
    let m = load(&mut tx, &scope, mid).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::CREATED, &m, m.version))
}

/// List the school academic model versions of an institution (`institution.read`).
#[utoipa::path(
    get,
    path = "/api/v1/institution/institutions/{id}/school-models",
    operation_id = "school_model_list",
    tag = "curriculum",
    params(("id" = String, Path, format = Uuid, description = "Institution id"), PublicationQuery),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = SchoolModelList),
        (status = 404, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn list(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    Query(q): Query<PublicationQuery>,
) -> ApiResult<Json<SchoolModelList>> {
    let ctx = auth.require(Permission::InstitutionRead)?;
    let scope = institution_scope(ctx, id);
    guard::institution_status(&st.pool, &scope, id).await?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM institution.school_academic_model WHERE tenant_id = $1 AND institution_id = $2 \
         AND ($3::text IS NULL OR status = $3) ORDER BY version_no DESC",
    )
    .bind(scope.tenant_id.as_uuid())
    .bind(id.as_uuid())
    .bind(q.status.map(|s| s.as_str()))
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let mut items = Vec::with_capacity(ids.len());
    for mid in ids {
        items.push(load(&mut conn, &scope, mid).await?);
    }
    Ok(Json(SchoolModelList { items }))
}

/// Get one school academic model version (`institution.read`).
#[utoipa::path(
    get,
    path = "/api/v1/institution/school-models/{schoolModelId}",
    operation_id = "school_model_get",
    tag = "curriculum",
    params(("schoolModelId" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = SchoolModel, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn get_one(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(model_id): Path<SchoolAcademicModelId>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::InstitutionRead)?;
    Ok(ok(&get(&st.pool, &ctx.scope, model_id.as_uuid()).await?))
}

/// Replace the content of a draft school model (`curriculum.manage`).
#[utoipa::path(
    patch,
    path = "/api/v1/institution/school-models/{schoolModelId}",
    operation_id = "school_model_update",
    tag = "curriculum",
    params(
        ("schoolModelId" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the version being modified"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = SchoolModelDraft,
    responses(
        (status = 200, body = SchoolModel, headers(("ETag" = String))),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
        (status = 428, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn update(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(model_id): Path<SchoolAcademicModelId>,
    headers: HeaderMap,
    Json(d): Json<SchoolModelDraft>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CurriculumManage)?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(res) => return Ok(res),
    };
    d.validate()?;
    let id = model_id.as_uuid();
    let inst = institution_of(&st.pool, &ctx.scope, TABLE, ENTITY, id).await?;
    let scope = institution_scope(ctx, inst);
    let mut tx = begin(&st.pool, &scope, inst).await?;
    let (status, version) = head(&mut tx, &scope, TABLE, ENTITY, id, true).await?;
    expect_version(expected, version)?;
    ensure_editable("school academic model", status)?;
    sqlx::query(
        "UPDATE institution.school_academic_model SET name = $1, board = $2, nep_stage_aligned = $3, \
         effective_from = $4, version = version + 1, updated_at = now() WHERE id = $5 AND tenant_id = $6",
    )
    .bind(d.name.trim())
    .bind(d.board.as_deref())
    .bind(d.nep_stage_aligned)
    .bind(d.effective_from)
    .bind(id)
    .bind(scope.tenant_id.as_uuid())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    sqlx::query("DELETE FROM institution.school_grade_level WHERE school_model_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    insert_grades(&mut tx, &scope, id, &d.grade_levels).await?;
    publish::audit(
        &mut tx,
        &scope,
        "institution.school_model.update",
        ENTITY,
        id.to_string(),
        serde_json::json!({"fromVersion": expected.0, "gradeCount": d.grade_levels.len()}),
        &actor(ctx),
    )
    .await?;
    let m = load(&mut tx, &scope, id).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(ok(&m))
}

/// Submit a draft school model for publication (`curriculum.publish`; WorkflowPort
/// `SCHOOL_MODEL_PUBLICATION`). Needs a published school / mixed academic structure.
#[utoipa::path(
    post,
    path = "/api/v1/institution/school-models/{schoolModelId}/publish",
    operation_id = "school_model_publish",
    tag = "curriculum",
    params(
        ("schoolModelId" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the current version"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 200, body = SchoolModel, headers(("ETag" = String))),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
        (status = 428, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn publish_one(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(model_id): Path<SchoolAcademicModelId>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CurriculumPublish)?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(res) => return Ok(res),
    };
    let id = model_id.as_uuid();
    publication::submit(
        &st,
        &ctx.scope,
        Kind::SchoolModel,
        id,
        expected,
        &actor(ctx),
    )
    .await?;
    Ok(ok(&get(&st.pool, &ctx.scope, id).await?))
}

/// Decide an `in_review` school model (`curriculum.publish`). A rejected proposal leaves the
/// published model authoritative.
#[utoipa::path(
    post,
    path = "/api/v1/institution/school-models/{schoolModelId}/decision",
    operation_id = "school_model_decision",
    tag = "curriculum",
    params(
        ("schoolModelId" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the current version"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = DecisionBody,
    responses(
        (status = 200, body = SchoolModel, headers(("ETag" = String))),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn decision(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(model_id): Path<SchoolAcademicModelId>,
    headers: HeaderMap,
    raw: Bytes,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CurriculumPublish)?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(res) => return Ok(res),
    };
    let (d, reason) = DecisionBody::parse(&raw)?;
    let id = model_id.as_uuid();
    publication::decide(
        &st.pool,
        &ctx.scope,
        Kind::SchoolModel,
        id,
        expected,
        d,
        reason,
        &actor(ctx),
    )
    .await?;
    Ok(ok(&get(&st.pool, &ctx.scope, id).await?))
}

/// Create the successor draft of the published school model (`curriculum.manage`).
#[utoipa::path(
    post,
    path = "/api/v1/institution/school-models/{schoolModelId}/revise",
    operation_id = "school_model_revise",
    tag = "curriculum",
    params(
        ("schoolModelId" = String, Path, format = Uuid),
        ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key."),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body(content = Option<ReviseSchoolModel>),
    responses(
        (status = 201, body = SchoolModel, headers(("ETag" = String))),
        (status = 409, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn revise(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(model_id): Path<SchoolAcademicModelId>,
    headers: HeaderMap,
    raw: Bytes,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CurriculumManage)?;
    require_idempotency_key(&headers)?;
    let b: ReviseSchoolModel = body(&raw)?;
    let id = model_id.as_uuid();
    let inst = institution_of(&st.pool, &ctx.scope, TABLE, ENTITY, id).await?;
    let scope = institution_scope(ctx, inst);
    let mut tx = begin(&st.pool, &scope, inst).await?;
    let (status, _) = head(&mut tx, &scope, TABLE, ENTITY, id, true).await?;
    if status != PublicationStatus::Published {
        return Err(KernelError::Conflict(format!(
            "only the published school model can be revised (this one is {status})"
        ))
        .into());
    }
    let cur = load(&mut tx, &scope, id).await?;
    let d = SchoolModelDraft {
        name: cur.name.clone(),
        board: cur.board.clone(),
        nep_stage_aligned: cur.nep_stage_aligned,
        effective_from: b.effective_from.or(cur.effective_from),
        grade_levels: cur.grade_levels.clone(),
    };
    let mid = insert_draft(
        &mut tx,
        &scope,
        inst,
        &d,
        Some(id),
        &actor(ctx),
        "institution.school_model.revise",
    )
    .await?;
    let m = load(&mut tx, &scope, mid).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::CREATED, &m, m.version))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grade(key: &str, stage: Option<NepStage>) -> GradeLevel {
        GradeLevel {
            key: key.into(),
            name: format!("Class {key}"),
            nep_stage: stage,
            sections: vec!["A".into(), "B".into()],
            subjects: vec![SchoolSubject {
                code: "MATH".into(),
                name: "Mathematics".into(),
                kind: SubjectKind::Core,
            }],
        }
    }

    #[test]
    fn school_rules() {
        let good = vec![
            grade("1", Some(NepStage::Foundational)),
            grade("6", Some(NepStage::Middle)),
        ];
        assert!(publication_rules(true, &good).is_empty());
        let mut bad = vec![
            grade("6", Some(NepStage::Middle)),
            grade("1", Some(NepStage::Foundational)),
            grade("x", None),
        ];
        bad[2].sections.clear();
        let dup = bad[2].subjects[0].clone();
        bad[2].subjects.push(dup);
        let codes: Vec<_> = publication_rules(true, &bad)
            .into_iter()
            .map(|v| v.code)
            .collect();
        for expect in ["order", "nep", "required-for-publication", "duplicate"] {
            assert!(
                codes.iter().any(|c| c == expect),
                "{expect} missing in {codes:?}"
            );
        }
        assert!(publication_rules(false, &[])
            .iter()
            .any(|v| v.code == "required-for-publication"));
        assert_eq!(NepStage::parse("secondary"), Some(NepStage::Secondary));
    }
}
