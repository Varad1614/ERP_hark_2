//! Organisation type blueprints (L3-B-010): versioned sets of allowed unit types and their
//! parent rules. Publication goes through the WorkflowPort (`ORG_TYPE_BLUEPRINT_PUBLICATION`).

use super::domain::{ApprovalDecision, Decide, DraftBlueprint, OrgTypeBlueprint, UnitType};
use crate::lifecycle::common::{
    check_version, expect_version, invalid, CommandErrors, CreateErrors, ReadErrors,
};
use crate::shared::approval::{self, process, ApprovalOutcome};
use crate::shared::guard::lock_institution_for_change;
use crate::shared::http::{actor, with_etag};
use crate::shared::publish::{audit, event};
use crate::shared::scope::institution_scope;
use crate::shared::{Actor, InstitutionState};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use sqlx::{PgConnection, PgPool, Row};
use std::collections::BTreeMap;
use sutra_contracts::institution::events::org::{
    OrgTypeBlueprintPublishedV1, ORG_TYPE_BLUEPRINT_PUBLISHED, ORG_TYPE_BLUEPRINT_PUBLISHED_SCHEMA,
};
use sutra_contracts::institution::ids::OrgTypeBlueprintId;
use sutra_contracts::institution::status::PublicationStatus;
use sutra_kernel::auth::Permission;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated};

const COLUMNS: &str = "id, institution_id, version_no, status, supersedes_id, effective_from, \
    process_instance_id, published_at, published_by, decision_reason, version, created_at, updated_at";

fn parse_status(s: &str) -> Result<PublicationStatus, KernelError> {
    PublicationStatus::parse(s)
        .ok_or_else(|| KernelError::internal_msg(format!("unknown publication status `{s}`")))
}

async fn hydrate(
    conn: &mut PgConnection,
    r: &sqlx::postgres::PgRow,
) -> Result<OrgTypeBlueprint, KernelError> {
    let id: uuid::Uuid = r.try_get("id").map_err(map_db_error)?;
    let types = sqlx::query(
        "SELECT key, label, allow_root, allowed_parent_types FROM institution.org_type_blueprint_unit_type \
         WHERE blueprint_id = $1 ORDER BY ordinal",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?
    .iter()
    .map(|t| {
        Ok(UnitType {
            key: t.try_get("key")?,
            label: t.try_get("label")?,
            allow_root: t.try_get("allow_root")?,
            allowed_parent_types: t.try_get("allowed_parent_types")?,
        })
    })
    .collect::<Result<Vec<_>, sqlx::Error>>()
    .map_err(map_db_error)?;
    let status: String = r.try_get("status").map_err(map_db_error)?;
    let sup: Option<uuid::Uuid> = r.try_get("supersedes_id").map_err(map_db_error)?;
    Ok(OrgTypeBlueprint {
        id: OrgTypeBlueprintId(id),
        institution_id: InstitutionId(r.try_get("institution_id").map_err(map_db_error)?),
        version_no: r.try_get("version_no").map_err(map_db_error)?,
        status: parse_status(&status)?,
        supersedes_id: sup.map(OrgTypeBlueprintId),
        effective_from: r.try_get("effective_from").map_err(map_db_error)?,
        unit_types: types,
        process_instance_id: r.try_get("process_instance_id").map_err(map_db_error)?,
        published_at: r.try_get("published_at").map_err(map_db_error)?,
        published_by: r.try_get("published_by").map_err(map_db_error)?,
        decision_reason: r.try_get("decision_reason").map_err(map_db_error)?,
        version: Version(r.try_get("version").map_err(map_db_error)?),
        created_at: r.try_get("created_at").map_err(map_db_error)?,
        updated_at: r.try_get("updated_at").map_err(map_db_error)?,
    })
}

/// One blueprint of the scope's tenant (404 when missing / foreign).
pub(crate) async fn load(
    conn: &mut PgConnection,
    tenant: &ExecutionScope,
    id: OrgTypeBlueprintId,
    for_update: bool,
) -> Result<OrgTypeBlueprint, KernelError> {
    let lock = if for_update { " FOR UPDATE" } else { "" };
    let row = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM institution.org_type_blueprint WHERE id = $1 AND tenant_id = $2{lock}"
    ))
    .bind(id.as_uuid())
    .bind(tenant.tenant_id.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "org_type_blueprint",
    })?;
    hydrate(conn, &row).await
}

/// The institution's published (effective) blueprint version, if any.
pub(crate) async fn current_published(
    conn: &mut PgConnection,
    tenant: &ExecutionScope,
    institution_id: InstitutionId,
) -> Result<Option<OrgTypeBlueprint>, KernelError> {
    let row = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM institution.org_type_blueprint \
         WHERE institution_id = $1 AND tenant_id = $2 AND status = 'published'"
    ))
    .bind(institution_id.as_uuid())
    .bind(tenant.tenant_id.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    match row {
        Some(r) => Ok(Some(hydrate(conn, &r).await?)),
        None => Ok(None),
    }
}

async fn write_types(
    conn: &mut PgConnection,
    id: OrgTypeBlueprintId,
    types: &[UnitType],
) -> Result<(), KernelError> {
    sqlx::query("DELETE FROM institution.org_type_blueprint_unit_type WHERE blueprint_id = $1")
        .bind(id.as_uuid())
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    for (i, t) in types.iter().enumerate() {
        sqlx::query(
            "INSERT INTO institution.org_type_blueprint_unit_type \
               (blueprint_id, key, label, allow_root, allowed_parent_types, ordinal) VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(id.as_uuid())
        .bind(t.key.trim())
        .bind(t.label.trim())
        .bind(t.allow_root)
        .bind(&t.allowed_parent_types)
        .bind(i32::try_from(i).unwrap_or(i32::MAX))
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    }
    Ok(())
}

fn inst_scope(tenant: &ExecutionScope, id: InstitutionId) -> ExecutionScope {
    ExecutionScope::institution(tenant.tenant_id, id)
}

/// Creates the next draft version (superseding the published one on publication).
pub async fn create(
    pool: &PgPool,
    scope: &ExecutionScope,
    institution_id: InstitutionId,
    cmd: &DraftBlueprint,
    actor: &Actor,
) -> Result<OrgTypeBlueprint, KernelError> {
    cmd.validate()?;
    let mut tx = pool.begin().await.map_err(map_db_error)?;
    lock_institution_for_change(&mut tx, scope, institution_id).await?;
    let current = current_published(&mut tx, scope, institution_id).await?;
    let open: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM institution.org_type_blueprint \
         WHERE institution_id = $1 AND status IN ('draft', 'in_review'))",
    )
    .bind(institution_id.as_uuid())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if open {
        return Err(KernelError::Conflict(
            "a draft or in-review blueprint version already exists".into(),
        ));
    }
    let id = OrgTypeBlueprintId::new();
    sqlx::query(
        "INSERT INTO institution.org_type_blueprint (id, tenant_id, institution_id, version_no, supersedes_id, effective_from) \
         SELECT $1, $2, $3, COALESCE(MAX(version_no), 0) + 1, $4, $5 FROM institution.org_type_blueprint WHERE institution_id = $3",
    )
    .bind(id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(institution_id.as_uuid())
    .bind(current.as_ref().map(|c| c.id.as_uuid()))
    .bind(cmd.effective_from)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    write_types(&mut tx, id, &cmd.unit_types).await?;
    audit(
        &mut tx,
        &inst_scope(scope, institution_id),
        "institution.org_type_blueprint.create",
        "org_type_blueprint",
        id.to_string(),
        serde_json::json!({ "unitTypes": cmd.unit_types.iter().map(|t| &t.key).collect::<Vec<_>>() }),
        actor,
    )
    .await?;
    let bp = load(&mut tx, scope, id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(bp)
}

/// Replaces a draft's content (published versions are immutable).
pub async fn update(
    pool: &PgPool,
    tenant: &ExecutionScope,
    id: OrgTypeBlueprintId,
    expected: Version,
    cmd: &DraftBlueprint,
    actor: &Actor,
) -> Result<OrgTypeBlueprint, KernelError> {
    cmd.validate()?;
    let mut tx = pool.begin().await.map_err(map_db_error)?;
    let bp = load(&mut tx, tenant, id, true).await?;
    lock_institution_for_change(&mut tx, tenant, bp.institution_id).await?;
    check_version(expected, bp.version.0)?;
    if bp.status != PublicationStatus::Draft {
        return Err(KernelError::Conflict(format!(
            "blueprint version is {}; only drafts can be changed (create a successor instead)",
            bp.status
        )));
    }
    sqlx::query(
        "UPDATE institution.org_type_blueprint SET effective_from = $1, version = version + 1, updated_at = now() WHERE id = $2",
    )
    .bind(cmd.effective_from)
    .bind(id.as_uuid())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    write_types(&mut tx, id, &cmd.unit_types).await?;
    audit(
        &mut tx,
        &inst_scope(tenant, bp.institution_id),
        "institution.org_type_blueprint.update",
        "org_type_blueprint",
        id.to_string(),
        serde_json::json!({ "fromVersion": expected.0 }),
        actor,
    )
    .await?;
    let out = load(&mut tx, tenant, id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

async fn apply_publication(
    conn: &mut PgConnection,
    tenant: &ExecutionScope,
    bp: &OrgTypeBlueprint,
    actor: &Actor,
) -> Result<(), KernelError> {
    sqlx::query(
        "UPDATE institution.org_type_blueprint SET status = 'superseded', version = version + 1, updated_at = now() \
         WHERE institution_id = $1 AND status = 'published'",
    )
    .bind(bp.institution_id.as_uuid())
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    sqlx::query(
        "UPDATE institution.org_type_blueprint SET status = 'published', published_at = now(), published_by = $1, \
         version = version + 1, updated_at = now() WHERE id = $2",
    )
    .bind(actor.id.as_deref())
    .bind(bp.id.as_uuid())
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let es = inst_scope(tenant, bp.institution_id);
    event(
        conn,
        &es,
        ORG_TYPE_BLUEPRINT_PUBLISHED,
        ORG_TYPE_BLUEPRINT_PUBLISHED_SCHEMA,
        bp.id.to_string(),
        &OrgTypeBlueprintPublishedV1 {
            blueprint_id: bp.id.as_uuid(),
            institution_id: bp.institution_id.as_uuid(),
            version_no: bp.version_no,
            unit_types: bp.unit_types.iter().map(|t| t.key.clone()).collect(),
            effective_from: bp.effective_from,
        },
        actor,
    )
    .await?;
    audit(
        conn,
        &es,
        "institution.org_type_blueprint.publish",
        "org_type_blueprint",
        bp.id.to_string(),
        serde_json::json!({ "versionNo": bp.version_no, "supersedesId": bp.supersedes_id, "processInstanceId": bp.process_instance_id }),
        actor,
    )
    .await
}

/// Requests publication of a draft (`draft → in_review`, and `→ published` on approval).
pub async fn publish(
    state: &InstitutionState,
    tenant: &ExecutionScope,
    id: OrgTypeBlueprintId,
    expected: Version,
    actor: &Actor,
) -> Result<OrgTypeBlueprint, KernelError> {
    let pre = {
        let mut conn = state.pool.acquire().await.map_err(map_db_error)?;
        let bp = load(&mut conn, tenant, id, false).await?;
        check_version(expected, bp.version.0)?;
        if bp.status != PublicationStatus::Draft {
            return Err(crate::domain::illegal_transition(
                "org_type_blueprint",
                bp.status,
                PublicationStatus::InReview,
            ));
        }
        if let Some(cur) = current_published(&mut conn, tenant, bp.institution_id).await? {
            if bp.effective_from < cur.effective_from {
                return Err(invalid(
                    "effectiveFrom",
                    "before-predecessor",
                    format!(
                        "effectiveFrom must not be before the current version's date ({})",
                        cur.effective_from
                    ),
                ));
            }
        }
        bp
    };
    let es = inst_scope(tenant, pre.institution_id);
    let outcome = approval::request_approval(
        state,
        &es,
        process::ORG_TYPE_BLUEPRINT_PUBLICATION,
        approval::business_key("org_type_blueprint", id, pre.version.0),
        BTreeMap::from([("versionNo".to_owned(), serde_json::json!(pre.version_no))]),
    )
    .await?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let bp = load(&mut tx, tenant, id, true).await?;
    lock_institution_for_change(&mut tx, tenant, bp.institution_id).await?;
    check_version(expected, bp.version.0)?;
    sqlx::query(
        "UPDATE institution.org_type_blueprint SET status = 'in_review', process_instance_id = $1, \
         decision_reason = NULL, version = version + 1, updated_at = now() WHERE id = $2",
    )
    .bind(outcome.process_instance_id())
    .bind(id.as_uuid())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit(
        &mut tx,
        &es,
        "institution.org_type_blueprint.submit",
        "org_type_blueprint",
        id.to_string(),
        serde_json::json!({ "processInstanceId": outcome.process_instance_id() }),
        actor,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        let bp = load(&mut tx, tenant, id, false).await?;
        apply_publication(
            &mut tx,
            tenant,
            &bp,
            &Actor::system(actor.correlation_id.clone()),
        )
        .await?;
    }
    let out = load(&mut tx, tenant, id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

/// Decision on an in-review version: approve publishes it, reject returns it to draft.
pub async fn decide(
    pool: &PgPool,
    tenant: &ExecutionScope,
    id: OrgTypeBlueprintId,
    expected: Version,
    cmd: &Decide,
    actor: &Actor,
) -> Result<OrgTypeBlueprint, KernelError> {
    let reason = crate::lifecycle::domain::CommandBody {
        reason: cmd.reason.clone(),
    };
    let reason = match cmd.decision {
        ApprovalDecision::Approve => reason.optional_reason()?,
        ApprovalDecision::Reject => Some(reason.required_reason()?),
    };
    let mut tx = pool.begin().await.map_err(map_db_error)?;
    let bp = load(&mut tx, tenant, id, true).await?;
    lock_institution_for_change(&mut tx, tenant, bp.institution_id).await?;
    check_version(expected, bp.version.0)?;
    if bp.status != PublicationStatus::InReview {
        return Err(KernelError::Conflict(format!(
            "blueprint version is {}; only in-review versions can be decided",
            bp.status
        )));
    }
    match cmd.decision {
        ApprovalDecision::Approve => apply_publication(&mut tx, tenant, &bp, actor).await?,
        ApprovalDecision::Reject => {
            sqlx::query(
                "UPDATE institution.org_type_blueprint SET status = 'draft', decision_reason = $1, \
                 version = version + 1, updated_at = now() WHERE id = $2",
            )
            .bind(&reason)
            .bind(id.as_uuid())
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
            audit(
                &mut tx,
                &inst_scope(tenant, bp.institution_id),
                "institution.org_type_blueprint.reject",
                "org_type_blueprint",
                id.to_string(),
                serde_json::json!({ "reason": reason }),
                actor,
            )
            .await?;
        }
    }
    let out = load(&mut tx, tenant, id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

/// Blueprint versions of an institution, newest first.
pub async fn list(
    pool: &PgPool,
    scope: &ExecutionScope,
    institution_id: InstitutionId,
    status: Option<PublicationStatus>,
) -> Result<Vec<OrgTypeBlueprint>, KernelError> {
    let mut conn = pool.acquire().await.map_err(map_db_error)?;
    crate::shared::guard::institution_status(&mut *conn, scope, institution_id).await?;
    let rows = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM institution.org_type_blueprint WHERE institution_id = $1 AND tenant_id = $2 \
         AND ($3::text IS NULL OR status = $3) ORDER BY version_no DESC"
    ))
    .bind(institution_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(status.map(|s| s.as_str()))
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let mut out = vec![];
    for r in &rows {
        out.push(hydrate(&mut conn, r).await?);
    }
    Ok(out)
}

// ------------------------------------------------------------------------------------ HTTP

/// Blueprint collection.
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct OrgTypeBlueprintList {
    pub items: Vec<OrgTypeBlueprint>,
}

/// `?status=` filter.
#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct BlueprintListQuery {
    pub status: Option<PublicationStatus>,
}

fn respond(status: StatusCode, bp: OrgTypeBlueprint) -> Response {
    with_etag(status, &bp, bp.version)
}

/// Create the next draft type blueprint version (`organisation.manage`).
#[utoipa::path(
    post,
    path = "/api/v1/institution/institutions/{id}/org-type-blueprints",
    operation_id = "org_type_blueprint_create",
    tag = "organisation",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = DraftBlueprint,
    responses((status = 201, body = OrgTypeBlueprint, headers(("ETag" = String))), CreateErrors)
)]
pub async fn http_create(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    headers: HeaderMap,
    Json(cmd): Json<DraftBlueprint>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::OrganisationManage)?;
    require_idempotency_key(&headers)?;
    let scope = institution_scope(ctx, id);
    Ok(respond(
        StatusCode::CREATED,
        create(&st.pool, &scope, id, &cmd, &actor(ctx)).await?,
    ))
}

/// Type blueprint versions of an institution, newest first (`institution.read`).
#[utoipa::path(
    get,
    path = "/api/v1/institution/institutions/{id}/org-type-blueprints",
    operation_id = "org_type_blueprint_list",
    tag = "organisation",
    params(("id" = String, Path, format = Uuid), BlueprintListQuery),
    security(("session_cookie" = [])),
    responses((status = 200, body = OrgTypeBlueprintList), ReadErrors)
)]
pub async fn http_list(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    Query(q): Query<BlueprintListQuery>,
) -> ApiResult<Json<OrgTypeBlueprintList>> {
    let ctx = auth.require(Permission::InstitutionRead)?;
    let scope = institution_scope(ctx, id);
    Ok(Json(OrgTypeBlueprintList {
        items: list(&st.pool, &scope, id, q.status).await?,
    }))
}

/// One type blueprint version (`institution.read`).
#[utoipa::path(
    get,
    path = "/api/v1/institution/org-type-blueprints/{id}",
    operation_id = "org_type_blueprint_get",
    tag = "organisation",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses((status = 200, body = OrgTypeBlueprint, headers(("ETag" = String))), ReadErrors)
)]
pub async fn http_get(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<OrgTypeBlueprintId>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::InstitutionRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    Ok(respond(
        StatusCode::OK,
        load(&mut conn, &ctx.scope, id, false).await?,
    ))
}

/// Replace a draft version's unit types and effective date (`organisation.manage`).
#[utoipa::path(
    put,
    path = "/api/v1/institution/org-type-blueprints/{id}",
    operation_id = "org_type_blueprint_update",
    tag = "organisation",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the version being changed")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = DraftBlueprint,
    responses((status = 200, body = OrgTypeBlueprint, headers(("ETag" = String))), CommandErrors)
)]
pub async fn http_update(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<OrgTypeBlueprintId>,
    headers: HeaderMap,
    Json(cmd): Json<DraftBlueprint>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::OrganisationManage)?;
    let expected = expect_version!(headers);
    Ok(respond(
        StatusCode::OK,
        update(&st.pool, &ctx.scope, id, expected, &cmd, &actor(ctx)).await?,
    ))
}

/// Publish a draft version through the approval step; the previous published version becomes
/// `superseded` (`organisation.manage`). Units keep their version until migrated.
#[utoipa::path(
    post,
    path = "/api/v1/institution/org-type-blueprints/{id}/publish",
    operation_id = "org_type_blueprint_publish",
    tag = "organisation",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the version being changed")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = OrgTypeBlueprint, headers(("ETag" = String))), CommandErrors)
)]
pub async fn http_publish(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<OrgTypeBlueprintId>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::OrganisationManage)?;
    let expected = expect_version!(headers);
    Ok(respond(
        StatusCode::OK,
        publish(&st, &ctx.scope, id, expected, &actor(ctx)).await?,
    ))
}

/// Decide an in-review publication: `approve` publishes, `reject` (reason required) returns it
/// to draft (`organisation.manage`).
#[utoipa::path(
    post,
    path = "/api/v1/institution/org-type-blueprints/{id}/decision",
    operation_id = "org_type_blueprint_decide",
    tag = "organisation",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the version being changed")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = Decide,
    responses((status = 200, body = OrgTypeBlueprint, headers(("ETag" = String))), CommandErrors)
)]
pub async fn http_decide(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<OrgTypeBlueprintId>,
    headers: HeaderMap,
    Json(cmd): Json<Decide>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::OrganisationManage)?;
    let expected = expect_version!(headers);
    Ok(respond(
        StatusCode::OK,
        decide(&st.pool, &ctx.scope, id, expected, &cmd, &actor(ctx)).await?,
    ))
}
