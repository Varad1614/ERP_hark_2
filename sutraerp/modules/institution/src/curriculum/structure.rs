//! Academic structure (L2-B04, L3-B-023): the institution's versioned tree of levels, degree
//! types, streams and disciplines that the programme catalogue refers to by key.

use super::domain::{ensure, ensure_editable, nheqf_ok, parse_status};
use super::publication::{self, head, move_status, supersede_current, Kind};
use super::support::{
    begin, body, expect_version, finish, institution_of, key_ok, required_text, violation,
    DecisionBody, ReasonBody, Transition,
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
use std::collections::{HashMap, HashSet};
use sutra_contracts::institution::events::curriculum::{
    AcademicStructurePublishedV1, ACADEMIC_STRUCTURE_PUBLISHED,
    ACADEMIC_STRUCTURE_PUBLISHED_SCHEMA, ACADEMIC_STRUCTURE_RETIRED,
    ACADEMIC_STRUCTURE_RETIRED_SCHEMA,
};
use sutra_contracts::institution::ids::AcademicStructureId;
use sutra_contracts::institution::status::PublicationStatus;
use sutra_kernel::auth::Permission;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use uuid::Uuid;

const ENTITY: &str = "academic_structure";

/// Which academic model the institution runs.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum AcademicModel {
    /// Programmes and curricula (college / university).
    HigherEducation,
    /// School academic model (grades, sections, subjects).
    School,
    /// Both.
    Mixed,
}

impl AcademicModel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HigherEducation => "higher_education",
            Self::School => "school",
            Self::Mixed => "mixed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [Self::HigherEducation, Self::School, Self::Mixed]
            .into_iter()
            .find(|m| m.as_str() == s)
    }
}

/// Kind of a structure node.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    /// Academic level, e.g. `ug`, `pg`, `doctoral` (programmes refer to it as `level`).
    Level,
    /// Degree type under a level, e.g. `btech`, `msc` (programmes refer to it as `degreeType`).
    DegreeType,
    Stream,
    Discipline,
}

impl NodeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Level => "level",
            Self::DegreeType => "degree_type",
            Self::Stream => "stream",
            Self::Discipline => "discipline",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [
            Self::Level,
            Self::DegreeType,
            Self::Stream,
            Self::Discipline,
        ]
        .into_iter()
        .find(|k| k.as_str() == s)
    }

    /// Allowed parent kinds (`None` = root).
    fn parent_ok(self, parent: Option<NodeKind>) -> bool {
        matches!(
            (self, parent),
            (Self::Level, None)
                | (Self::DegreeType, Some(Self::Level))
                | (Self::Stream, Some(Self::Level | Self::DegreeType))
                | (
                    Self::Discipline,
                    Some(Self::Level | Self::DegreeType | Self::Stream)
                )
        )
    }
}

/// One node of the structure tree.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StructureNode {
    pub kind: NodeKind,
    /// Lowercase key, unique in the structure (e.g. `ug`, `btech`).
    #[schema(example = "ug")]
    pub key: String,
    pub name: String,
    /// Key of the parent node (levels have none).
    pub parent_key: Option<String>,
    /// NHEQF level (4.5 … 8, NEP 2020) for levels / degree types.
    pub nheqf_level: Option<f64>,
    #[serde(default)]
    pub sort_order: i32,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AcademicStructure {
    pub id: AcademicStructureId,
    pub institution_id: InstitutionId,
    pub name: String,
    pub model: AcademicModel,
    pub nep_aligned: bool,
    pub version_no: i32,
    pub supersedes_id: Option<AcademicStructureId>,
    pub effective_from: Option<NaiveDate>,
    pub status: PublicationStatus,
    pub process_instance_id: Option<String>,
    pub status_reason: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
    pub retired_at: Option<DateTime<Utc>>,
    pub nodes: Vec<StructureNode>,
    pub version: Version,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct AcademicStructureList {
    pub items: Vec<AcademicStructure>,
}

/// Draft content (create and full replacement on `PATCH`).
#[derive(Clone, Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StructureDraft {
    pub name: String,
    pub model: AcademicModel,
    #[serde(default = "yes")]
    pub nep_aligned: bool,
    pub effective_from: Option<NaiveDate>,
    #[serde(default)]
    pub nodes: Vec<StructureNode>,
}

fn yes() -> bool {
    true
}

/// `POST …/revise`: successor draft of a published structure (content copied, then edited).
#[derive(Clone, Debug, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviseBody {
    pub effective_from: Option<NaiveDate>,
}

#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct PublicationQuery {
    #[param(inline)]
    pub status: Option<PublicationStatus>,
}

/// Shape rules of the node set (draft-time).
fn validate_nodes(nodes: &[StructureNode], v: &mut Vec<FieldViolation>) {
    let mut kinds: HashMap<&str, NodeKind> = HashMap::new();
    for (i, n) in nodes.iter().enumerate() {
        let f = |m: &str| format!("nodes[{i}].{m}");
        if !key_ok(&n.key, 40) {
            v.push(violation(
                &f("key"),
                "format",
                "key must be a lowercase slug (a-z, 0-9, _ . -)",
            ));
        } else if kinds.insert(n.key.as_str(), n.kind).is_some() {
            v.push(violation(
                &f("key"),
                "duplicate",
                format!("key `{}` is used twice", n.key),
            ));
        }
        required_text(&f("name"), &n.name, 200, v);
        if let Some(l) = n.nheqf_level {
            if !matches!(n.kind, NodeKind::Level | NodeKind::DegreeType) || !nheqf_ok(l) {
                v.push(violation(
                    &f("nheqfLevel"),
                    "range",
                    "NHEQF level (4.5-8 in half steps) applies to levels and degree types only",
                ));
            }
        }
    }
    for (i, n) in nodes.iter().enumerate() {
        let parent = n.parent_key.as_deref().map(|k| kinds.get(k).copied());
        let parent_kind = match parent {
            None => None,
            Some(None) => {
                v.push(violation(
                    &format!("nodes[{i}].parentKey"),
                    "unknown",
                    "parent key is not a node of this structure",
                ));
                continue;
            }
            Some(Some(k)) => Some(k),
        };
        if !n.kind.parent_ok(parent_kind) {
            v.push(violation(
                &format!("nodes[{i}].parentKey"),
                "hierarchy",
                format!(
                    "a {} cannot sit under {}",
                    n.kind.as_str(),
                    parent_kind.map_or("the root", NodeKind::as_str)
                ),
            ));
        }
    }
}

impl StructureDraft {
    pub fn validate(&self) -> Result<(), KernelError> {
        let mut v = vec![];
        required_text("name", &self.name, 200, &mut v);
        if self.nodes.len() > 500 {
            v.push(violation("nodes", "too-many", "at most 500 nodes"));
        }
        validate_nodes(&self.nodes, &mut v);
        finish(v)
    }
}

const COLUMNS: &str = "id, institution_id, name, model, nep_aligned, version_no, supersedes_id, \
    effective_from, status, process_instance_id, status_reason, published_at, retired_at, \
    version, created_at, updated_at";

fn from_row(r: &PgRow) -> Result<AcademicStructure, sqlx::Error> {
    let model: String = r.try_get("model")?;
    let status: String = r.try_get("status")?;
    Ok(AcademicStructure {
        id: AcademicStructureId(r.try_get("id")?),
        institution_id: InstitutionId(r.try_get("institution_id")?),
        name: r.try_get("name")?,
        model: AcademicModel::parse(&model)
            .ok_or_else(|| sqlx::Error::Decode(format!("model `{model}`").into()))?,
        nep_aligned: r.try_get("nep_aligned")?,
        version_no: r.try_get("version_no")?,
        supersedes_id: r
            .try_get::<Option<Uuid>, _>("supersedes_id")?
            .map(AcademicStructureId),
        effective_from: r.try_get("effective_from")?,
        status: parse_status(&status)?,
        process_instance_id: r.try_get("process_instance_id")?,
        status_reason: r.try_get("status_reason")?,
        published_at: r.try_get("published_at")?,
        retired_at: r.try_get("retired_at")?,
        nodes: vec![],
        version: Version(r.try_get("version")?),
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
    })
}

async fn nodes_of(conn: &mut PgConnection, id: Uuid) -> Result<Vec<StructureNode>, KernelError> {
    let rows = sqlx::query(
        "SELECT kind, key, name, parent_key, nheqf_level::float8 AS nheqf_level, sort_order \
         FROM institution.academic_structure_node WHERE academic_structure_id = $1 \
         ORDER BY sort_order, key",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    rows.iter()
        .map(|r| {
            let kind: String = r.try_get("kind")?;
            Ok(StructureNode {
                kind: NodeKind::parse(&kind)
                    .ok_or_else(|| sqlx::Error::Decode(format!("node kind `{kind}`").into()))?,
                key: r.try_get("key")?,
                name: r.try_get("name")?,
                parent_key: r.try_get("parent_key")?,
                nheqf_level: r.try_get("nheqf_level")?,
                sort_order: r.try_get("sort_order")?,
            })
        })
        .collect::<Result<_, sqlx::Error>>()
        .map_err(map_db_error)
}

async fn insert_nodes(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    id: Uuid,
    nodes: &[StructureNode],
) -> Result<(), KernelError> {
    for n in nodes {
        sqlx::query(
            "INSERT INTO institution.academic_structure_node (tenant_id, academic_structure_id, \
             parent_key, kind, key, name, nheqf_level, sort_order) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(scope.tenant_id.as_uuid())
        .bind(id)
        .bind(n.parent_key.as_deref())
        .bind(n.kind.as_str())
        .bind(n.key.as_str())
        .bind(n.name.trim())
        .bind(n.nheqf_level)
        .bind(n.sort_order)
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
) -> Result<AcademicStructure, KernelError> {
    let row = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM institution.academic_structure WHERE id = $1 AND tenant_id = $2"
    ))
    .bind(id)
    .bind(tenant.tenant_id.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound { entity: ENTITY })?;
    let mut s = from_row(&row).map_err(map_db_error)?;
    s.nodes = nodes_of(conn, id).await?;
    Ok(s)
}

async fn get(
    pool: &PgPool,
    tenant: &ExecutionScope,
    id: Uuid,
) -> Result<AcademicStructure, KernelError> {
    let mut conn = pool.acquire().await.map_err(map_db_error)?;
    load(&mut conn, tenant, id).await
}

/// The published structure of an institution, if any.
pub(crate) async fn published(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    institution_id: InstitutionId,
) -> Result<Option<AcademicStructure>, KernelError> {
    let id: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM institution.academic_structure WHERE tenant_id = $1 \
         AND institution_id = $2 AND status = 'published'",
    )
    .bind(scope.tenant_id.as_uuid())
    .bind(institution_id.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    match id {
        Some(id) => Ok(Some(load(conn, scope, id).await?)),
        None => Ok(None),
    }
}

/// Publication coherence (L4-B-023-BH-002/004): at least one level; NEP-aligned higher
/// education levels carry their NHEQF level.
pub(crate) async fn validate_publication(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    id: Uuid,
) -> Result<(), KernelError> {
    let s = load(conn, scope, id).await?;
    let mut v = vec![];
    validate_nodes(&s.nodes, &mut v);
    if !s.nodes.iter().any(|n| n.kind == NodeKind::Level) {
        v.push(violation(
            "nodes",
            "required-for-publication",
            "at least one level is required",
        ));
    }
    if s.nep_aligned && s.model != AcademicModel::School {
        for (i, n) in s.nodes.iter().enumerate() {
            if n.kind == NodeKind::Level && n.nheqf_level.is_none() {
                v.push(violation(
                    &format!("nodes[{i}].nheqfLevel"),
                    "required-for-publication",
                    "NEP-aligned levels need their NHEQF level",
                ));
            }
        }
    }
    finish(v)
}

pub(crate) async fn finalize_publication(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    id: Uuid,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<(), KernelError> {
    let s = load(conn, scope, id).await?;
    ensure("academic structure", s.status, PublicationStatus::Published)?;
    supersede_current(
        conn,
        scope,
        "academic_structure",
        "institution_id",
        s.institution_id.as_uuid(),
        id,
    )
    .await?;
    let v = move_status(
        conn,
        scope,
        "academic_structure",
        id,
        PublicationStatus::Published,
        reason,
        None,
    )
    .await?;
    let data = AcademicStructurePublishedV1 {
        academic_structure_id: id,
        institution_id: s.institution_id.as_uuid(),
        version_no: s.version_no,
        supersedes_id: s.supersedes_id.map(|x| x.as_uuid()),
        node_count: i32::try_from(s.nodes.len()).unwrap_or(i32::MAX),
    };
    publish::event(
        conn,
        scope,
        ACADEMIC_STRUCTURE_PUBLISHED,
        ACADEMIC_STRUCTURE_PUBLISHED_SCHEMA,
        id.to_string(),
        &data,
        actor,
    )
    .await?;
    publish::audit(
        conn,
        scope,
        "institution.academic_structure.publish",
        ENTITY,
        id.to_string(),
        serde_json::json!({
            "fromStatus": s.status, "toStatus": "published", "version": v,
            "versionNo": s.version_no, "supersedesId": s.supersedes_id,
            "processInstanceId": s.process_instance_id, "reason": reason,
        }),
        actor,
    )
    .await
}

async fn next_version_no(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    institution_id: InstitutionId,
) -> Result<i32, KernelError> {
    sqlx::query_scalar(
        "SELECT COALESCE(MAX(version_no), 0) + 1 FROM institution.academic_structure \
         WHERE tenant_id = $1 AND institution_id = $2",
    )
    .bind(scope.tenant_id.as_uuid())
    .bind(institution_id.as_uuid())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)
}

#[allow(clippy::too_many_arguments)]
async fn insert_draft(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    institution_id: InstitutionId,
    d: &StructureDraft,
    supersedes: Option<Uuid>,
    actor: &Actor,
    action: &str,
) -> Result<Uuid, KernelError> {
    let version_no = next_version_no(conn, scope, institution_id).await?;
    let id = AcademicStructureId::new().as_uuid();
    sqlx::query(
        "INSERT INTO institution.academic_structure (id, tenant_id, institution_id, name, model, \
         nep_aligned, version_no, supersedes_id, effective_from) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(id)
    .bind(scope.tenant_id.as_uuid())
    .bind(institution_id.as_uuid())
    .bind(d.name.trim())
    .bind(d.model.as_str())
    .bind(d.nep_aligned)
    .bind(version_no)
    .bind(supersedes)
    .bind(d.effective_from)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    insert_nodes(conn, scope, id, &d.nodes).await?;
    publish::audit(
        conn,
        scope,
        action,
        ENTITY,
        id.to_string(),
        serde_json::json!({"versionNo": version_no, "supersedesId": supersedes, "nodeCount": d.nodes.len()}),
        actor,
    )
    .await?;
    Ok(id)
}

fn ok(s: &AcademicStructure) -> Response {
    with_etag(StatusCode::OK, s, s.version)
}

// ---------------------------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------------------------

/// Create the first academic structure draft of an institution (`curriculum.manage`). Once a
/// structure is published, changes go through `POST /academic-structures/{id}/revise`.
#[utoipa::path(
    post,
    path = "/api/v1/institution/institutions/{id}/academic-structures",
    operation_id = "academic_structure_create",
    tag = "curriculum",
    params(
        ("id" = String, Path, format = Uuid, description = "Institution id"),
        ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key."),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = StructureDraft,
    responses(
        (status = 201, body = AcademicStructure, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, description = "A draft is already open, a structure is published, or the institution is read-only", body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn create(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    headers: HeaderMap,
    Json(d): Json<StructureDraft>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CurriculumManage)?;
    require_idempotency_key(&headers)?;
    d.validate()?;
    let scope = institution_scope(ctx, id);
    let mut tx = begin(&st.pool, &scope, id).await?;
    if published(&mut tx, &scope, id).await?.is_some() {
        return Err(KernelError::Conflict(
            "a structure is already published; revise it to create a successor".into(),
        )
        .into());
    }
    let sid = insert_draft(
        &mut tx,
        &scope,
        id,
        &d,
        None,
        &actor(ctx),
        "institution.academic_structure.create",
    )
    .await?;
    let s = load(&mut tx, &scope, sid).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::CREATED, &s, s.version))
}

/// List the academic structure versions of an institution (`institution.read`).
#[utoipa::path(
    get,
    path = "/api/v1/institution/institutions/{id}/academic-structures",
    operation_id = "academic_structure_list",
    tag = "curriculum",
    params(("id" = String, Path, format = Uuid, description = "Institution id"), PublicationQuery),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = AcademicStructureList),
        (status = 404, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn list(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    Query(q): Query<PublicationQuery>,
) -> ApiResult<Json<AcademicStructureList>> {
    let ctx = auth.require(Permission::InstitutionRead)?;
    let scope = institution_scope(ctx, id);
    guard::institution_status(&st.pool, &scope, id).await?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM institution.academic_structure WHERE tenant_id = $1 AND institution_id = $2 \
         AND ($3::text IS NULL OR status = $3) ORDER BY version_no DESC",
    )
    .bind(scope.tenant_id.as_uuid())
    .bind(id.as_uuid())
    .bind(q.status.map(|s| s.as_str()))
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let mut items = Vec::with_capacity(ids.len());
    for sid in ids {
        items.push(load(&mut conn, &scope, sid).await?);
    }
    Ok(Json(AcademicStructureList { items }))
}

/// Get one academic structure version with its nodes (`institution.read`).
#[utoipa::path(
    get,
    path = "/api/v1/institution/academic-structures/{structureId}",
    operation_id = "academic_structure_get",
    tag = "curriculum",
    params(("structureId" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = AcademicStructure, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn get_one(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(structure_id): Path<AcademicStructureId>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::InstitutionRead)?;
    Ok(ok(&get(&st.pool, &ctx.scope, structure_id.as_uuid()).await?))
}

/// Replace the content of a draft structure (`curriculum.manage`). Published versions are
/// immutable (409).
#[utoipa::path(
    patch,
    path = "/api/v1/institution/academic-structures/{structureId}",
    operation_id = "academic_structure_update",
    tag = "curriculum",
    params(
        ("structureId" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the version being modified"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = StructureDraft,
    responses(
        (status = 200, body = AcademicStructure, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, description = "Not a draft", body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
        (status = 428, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn update(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(structure_id): Path<AcademicStructureId>,
    headers: HeaderMap,
    Json(d): Json<StructureDraft>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CurriculumManage)?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(res) => return Ok(res),
    };
    d.validate()?;
    let id = structure_id.as_uuid();
    let inst = institution_of(&st.pool, &ctx.scope, "academic_structure", ENTITY, id).await?;
    let scope = institution_scope(ctx, inst);
    let mut tx = begin(&st.pool, &scope, inst).await?;
    let (status, version) = head(&mut tx, &scope, "academic_structure", ENTITY, id, true).await?;
    expect_version(expected, version)?;
    ensure_editable("academic structure", status)?;
    sqlx::query(
        "UPDATE institution.academic_structure SET name = $1, model = $2, nep_aligned = $3, \
         effective_from = $4, version = version + 1, updated_at = now() WHERE id = $5 AND tenant_id = $6",
    )
    .bind(d.name.trim())
    .bind(d.model.as_str())
    .bind(d.nep_aligned)
    .bind(d.effective_from)
    .bind(id)
    .bind(scope.tenant_id.as_uuid())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    sqlx::query("DELETE FROM institution.academic_structure_node WHERE academic_structure_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    insert_nodes(&mut tx, &scope, id, &d.nodes).await?;
    publish::audit(
        &mut tx,
        &scope,
        "institution.academic_structure.update",
        ENTITY,
        id.to_string(),
        serde_json::json!({"fromVersion": expected.0, "nodeCount": d.nodes.len()}),
        &actor(ctx),
    )
    .await?;
    let s = load(&mut tx, &scope, id).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(ok(&s))
}

/// Submit a draft structure for publication (`curriculum.publish`; WorkflowPort
/// `ACADEMIC_STRUCTURE_PUBLICATION`). Auto-approval publishes it at once and supersedes the
/// previously published version in the same transaction.
#[utoipa::path(
    post,
    path = "/api/v1/institution/academic-structures/{structureId}/publish",
    operation_id = "academic_structure_publish",
    tag = "curriculum",
    params(
        ("structureId" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the current version"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 200, body = AcademicStructure, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 422, description = "Publication validation failed", body = Problem, content_type = "application/problem+json"),
        (status = 428, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn publish_one(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(structure_id): Path<AcademicStructureId>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CurriculumPublish)?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(res) => return Ok(res),
    };
    let id = structure_id.as_uuid();
    publication::submit(&st, &ctx.scope, Kind::Structure, id, expected, &actor(ctx)).await?;
    Ok(ok(&get(&st.pool, &ctx.scope, id).await?))
}

/// Decide an `in_review` structure (`curriculum.publish`): approve publishes it; reject or
/// return sends it back to draft (reason required).
#[utoipa::path(
    post,
    path = "/api/v1/institution/academic-structures/{structureId}/decision",
    operation_id = "academic_structure_decision",
    tag = "curriculum",
    params(
        ("structureId" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the current version"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = DecisionBody,
    responses(
        (status = 200, body = AcademicStructure, headers(("ETag" = String))),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn decision(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(structure_id): Path<AcademicStructureId>,
    headers: HeaderMap,
    raw: Bytes,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CurriculumPublish)?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(res) => return Ok(res),
    };
    let (d, reason) = DecisionBody::parse(&raw)?;
    let id = structure_id.as_uuid();
    publication::decide(
        &st.pool,
        &ctx.scope,
        Kind::Structure,
        id,
        expected,
        d,
        reason,
        &actor(ctx),
    )
    .await?;
    Ok(ok(&get(&st.pool, &ctx.scope, id).await?))
}

/// Create the successor draft of a published structure (`curriculum.manage`); the published
/// version stays authoritative until the successor is published.
#[utoipa::path(
    post,
    path = "/api/v1/institution/academic-structures/{structureId}/revise",
    operation_id = "academic_structure_revise",
    tag = "curriculum",
    params(
        ("structureId" = String, Path, format = Uuid, description = "Published structure"),
        ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key."),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body(content = Option<ReviseBody>),
    responses(
        (status = 201, body = AcademicStructure, headers(("ETag" = String))),
        (status = 409, description = "Not published, or a successor draft is already open", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn revise(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(structure_id): Path<AcademicStructureId>,
    headers: HeaderMap,
    raw: Bytes,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CurriculumManage)?;
    require_idempotency_key(&headers)?;
    let b: ReviseBody = body(&raw)?;
    let id = structure_id.as_uuid();
    let inst = institution_of(&st.pool, &ctx.scope, "academic_structure", ENTITY, id).await?;
    let scope = institution_scope(ctx, inst);
    let mut tx = begin(&st.pool, &scope, inst).await?;
    let (status, _) = head(&mut tx, &scope, "academic_structure", ENTITY, id, true).await?;
    if status != PublicationStatus::Published {
        return Err(KernelError::Conflict(format!(
            "only the published structure can be revised (this one is {status})"
        ))
        .into());
    }
    let cur = load(&mut tx, &scope, id).await?;
    let d = StructureDraft {
        name: cur.name.clone(),
        model: cur.model,
        nep_aligned: cur.nep_aligned,
        effective_from: b.effective_from.or(cur.effective_from),
        nodes: cur.nodes.clone(),
    };
    let sid = insert_draft(
        &mut tx,
        &scope,
        inst,
        &d,
        Some(id),
        &actor(ctx),
        "institution.academic_structure.revise",
    )
    .await?;
    let s = load(&mut tx, &scope, sid).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::CREATED, &s, s.version))
}

/// Retire the published structure (L3-B-025, `curriculum.publish`, reason required). Blocked
/// while published programmes depend on it.
#[utoipa::path(
    post,
    path = "/api/v1/institution/academic-structures/{structureId}/retire",
    operation_id = "academic_structure_retire",
    tag = "curriculum",
    params(
        ("structureId" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the current version"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = ReasonBody,
    responses(
        (status = 200, body = AcademicStructure, headers(("ETag" = String))),
        (status = 409, description = "Not published or still in use", body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn retire(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(structure_id): Path<AcademicStructureId>,
    headers: HeaderMap,
    raw: Bytes,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CurriculumPublish)?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(res) => return Ok(res),
    };
    let reason = body::<ReasonBody>(&raw)?.validated(true)?;
    let id = structure_id.as_uuid();
    let inst = institution_of(&st.pool, &ctx.scope, "academic_structure", ENTITY, id).await?;
    let scope = institution_scope(ctx, inst);
    let mut tx = begin(&st.pool, &scope, inst).await?;
    let (status, version) = head(&mut tx, &scope, "academic_structure", ENTITY, id, true).await?;
    expect_version(expected, version)?;
    ensure("academic structure", status, PublicationStatus::Retired)?;
    let users: Vec<String> = sqlx::query_scalar(
        "SELECT code FROM institution.programme WHERE tenant_id = $1 AND institution_id = $2 \
         AND status = 'published' ORDER BY code",
    )
    .bind(scope.tenant_id.as_uuid())
    .bind(inst.as_uuid())
    .fetch_all(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if !users.is_empty() {
        return Err(KernelError::Conflict(format!(
            "published programmes depend on this structure: {}",
            users.join(", ")
        ))
        .into());
    }
    let v = move_status(
        &mut tx,
        &scope,
        "academic_structure",
        id,
        PublicationStatus::Retired,
        reason.as_deref(),
        None,
    )
    .await?;
    Transition {
        event_type: ACADEMIC_STRUCTURE_RETIRED,
        dataschema: ACADEMIC_STRUCTURE_RETIRED_SCHEMA,
        audit_action: "institution.academic_structure.retire",
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
    let s = load(&mut tx, &scope, id).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(ok(&s))
}

/// Keys of a given kind in a structure (used by the programme catalogue).
pub(crate) fn keys_of(s: &AcademicStructure, kind: NodeKind) -> HashSet<&str> {
    s.nodes
        .iter()
        .filter(|n| n.kind == kind)
        .map(|n| n.key.as_str())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(kind: NodeKind, key: &str, parent: Option<&str>) -> StructureNode {
        StructureNode {
            kind,
            key: key.into(),
            name: key.to_uppercase(),
            parent_key: parent.map(Into::into),
            nheqf_level: None,
            sort_order: 0,
        }
    }

    #[test]
    fn node_hierarchy_rules() {
        let good = vec![
            node(NodeKind::Level, "ug", None),
            node(NodeKind::DegreeType, "btech", Some("ug")),
            node(NodeKind::Stream, "engineering", Some("btech")),
            node(NodeKind::Discipline, "cse", Some("engineering")),
        ];
        let mut v = vec![];
        validate_nodes(&good, &mut v);
        assert!(v.is_empty(), "{v:?}");
        let bad = vec![
            node(NodeKind::Level, "ug", Some("pg")),
            node(NodeKind::DegreeType, "btech", None),
            node(NodeKind::Discipline, "cse", Some("nope")),
            node(NodeKind::Level, "ug", None),
            StructureNode {
                nheqf_level: Some(9.0),
                ..node(NodeKind::Stream, "Bad Key", Some("btech"))
            },
        ];
        let mut v = vec![];
        validate_nodes(&bad, &mut v);
        let codes: Vec<_> = v.iter().map(|x| x.code.as_str()).collect();
        assert!(codes.contains(&"duplicate"));
        assert!(codes.contains(&"format"));
        assert!(codes.contains(&"range"));
        assert!(codes.contains(&"unknown"));
        assert!(codes.contains(&"hierarchy"));
        for m in [
            AcademicModel::HigherEducation,
            AcademicModel::School,
            AcademicModel::Mixed,
        ] {
            assert_eq!(AcademicModel::parse(m.as_str()), Some(m));
        }
    }
}
