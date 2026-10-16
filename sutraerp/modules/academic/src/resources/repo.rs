//! Course resources and library links: offering-wide or per section; faculty manage resources
//! of sections they actively teach (`academic.teach` + an active assignment), administrators any
//! (`course_resource.manage`); students see the published resources of the sections they are
//! seated in plus the offering-wide ones of those offerings. Links only.

use crate::flexible::kit::{self, begin, check_version, commit, invalid, require, StatusEvent};
use crate::shared::faculty::require_teaching;
use crate::shared::guard::require_institution_writable;
use crate::shared::scope::{institution_scope, tenant_scope};
use crate::shared::{AcademicState, Actor};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use sutra_contracts::academic::events::flexible::*;
use sutra_contracts::academic::ids::{CourseOfferingId, OfferingSectionId};
use sutra_contracts::academic::ports::OfferingSummary;
use sutra_contracts::academic::status::{CourseResourceKind, CourseResourceStatus as S};
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::TenantId;
use sutra_persistence::map_db_error;
use uuid::Uuid;

pub const TABLE: &str = "course_resource";
const ENTITY: &str = "course resource";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CourseResource {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub offering_id: Option<Uuid>,
    pub section_id: Option<Uuid>,
    pub title: String,
    pub kind: CourseResourceKind,
    pub url: String,
    pub description: Option<String>,
    pub status: S,
    pub status_reason: Option<String>,
    pub created_by: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct CourseResourceList {
    pub items: Vec<CourseResource>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewCourseResource {
    pub title: String,
    pub kind: CourseResourceKind,
    pub url: String,
    pub description: Option<String>,
    /// Publish at once (default false = draft).
    #[serde(default)]
    pub publish: bool,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CourseResourcePatch {
    pub title: Option<String>,
    pub kind: Option<CourseResourceKind>,
    pub url: Option<String>,
    pub description: Option<String>,
}

/// One entry of the student Library.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LibraryItem {
    pub id: Uuid,
    pub offering_id: Option<Uuid>,
    pub section_id: Option<Uuid>,
    pub title: String,
    pub kind: CourseResourceKind,
    pub url: String,
    pub description: Option<String>,
    pub status: S,
    pub offering_code: String,
    pub offering_title: String,
    pub section_code: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct LibraryList {
    pub items: Vec<LibraryItem>,
}

/// A section the caller teaches (faculty Resources picker).
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResourceSection {
    pub section_id: Uuid,
    pub offering_id: Uuid,
    pub offering_code: String,
    pub title: String,
    pub section_code: String,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MyResources {
    pub sections: Vec<ResourceSection>,
    pub items: Vec<CourseResource>,
}

fn validate_url(url: &str) -> Result<String, KernelError> {
    // SEC-P1-APPSEC-004: shared http(s) link rule (host required, no whitespace / control chars).
    sutra_kernel::weblink::http_link("url", url)
}

pub async fn get(
    st: &AcademicState,
    tenant: TenantId,
    id: Uuid,
) -> Result<CourseResource, KernelError> {
    kit::fetch(&st.pool, TABLE, ENTITY, tenant, id).await
}

/// Access gate for a resource: section resources → `require_teaching`; offering-wide →
/// `course_resource.manage`.
async fn require_manage(
    st: &AcademicState,
    ctx: &AuthContext,
    section: Option<Uuid>,
) -> Result<(), KernelError> {
    match section {
        Some(s) => require_teaching(
            st,
            ctx,
            OfferingSectionId(s),
            Permission::CourseResourceManage,
        )
        .await
        .map(|_| ()),
        None => ctx.require(Permission::CourseResourceManage).map(|_| ()),
    }
}

pub async fn list_for_section(
    st: &AcademicState,
    ctx: &AuthContext,
    section: Uuid,
) -> Result<Vec<CourseResource>, KernelError> {
    require_teaching(
        st,
        ctx,
        OfferingSectionId(section),
        Permission::CourseResourceManage,
    )
    .await?;
    kit::list_by(
        &st.pool,
        TABLE,
        ctx.scope.tenant_id,
        "section_id",
        section,
        None,
        "t.created_at DESC",
        200,
    )
    .await
}

pub async fn list_for_offering(
    st: &AcademicState,
    tenant: TenantId,
    offering: Uuid,
) -> Result<Vec<CourseResource>, KernelError> {
    kit::list_by(
        &st.pool,
        TABLE,
        tenant,
        "offering_id",
        offering,
        None,
        "t.section_id NULLS FIRST, t.created_at DESC",
        500,
    )
    .await
}

async fn insert(
    st: &AcademicState,
    ctx: &AuthContext,
    offering: &OfferingSummary,
    section: Option<Uuid>,
    cmd: NewCourseResource,
    actor: &Actor,
) -> Result<CourseResource, KernelError> {
    let title = kit::required_text("title", Some(&cmd.title))?;
    let url = validate_url(&cmd.url)?;
    let scope = institution_scope(ctx, offering.institution_id);
    let tenant = ctx.scope.tenant_id;
    let mut tx = begin(st).await?;
    let r: CourseResource = kit::insert(&mut tx, TABLE, json!({
        "tenant_id": tenant.as_uuid(), "institution_id": offering.institution_id.as_uuid(), "offering_id": offering.id.as_uuid(),
        "section_id": section, "title": title, "kind": cmd.kind.as_str(), "url": url,
        "description": kit::opt_text(cmd.description.as_deref()), "created_by": actor.id,
    })).await?;
    kit::audit(
        &mut tx,
        &scope,
        TABLE,
        "create",
        r.id,
        json!({"offeringId": r.offering_id, "sectionId": section}),
        actor,
    )
    .await?;
    let r = if cmd.publish {
        publish_in(&mut tx, &scope, tenant, &r, actor).await?
    } else {
        r
    };
    commit(tx).await?;
    Ok(r)
}

/// Faculty (own section) or staff: add a resource to a deliverable section.
pub async fn create_for_section(
    st: &AcademicState,
    ctx: &AuthContext,
    section: Uuid,
    cmd: NewCourseResource,
    actor: &Actor,
) -> Result<CourseResource, KernelError> {
    require_teaching(
        st,
        ctx,
        OfferingSectionId(section),
        Permission::CourseResourceManage,
    )
    .await?;
    let s = kit::deliverable_section(st, ctx, section).await?;
    insert(st, ctx, &s.offering, Some(section), cmd, actor).await
}

/// Staff: add an offering-wide resource (offering not terminal / cancelled).
pub async fn create_for_offering(
    st: &AcademicState,
    ctx: &AuthContext,
    offering: Uuid,
    cmd: NewCourseResource,
    actor: &Actor,
) -> Result<CourseResource, KernelError> {
    ctx.require(Permission::CourseResourceManage)?;
    let o = st
        .offerings
        .offering(&tenant_scope(ctx), CourseOfferingId(offering))
        .await?
        .ok_or(KernelError::NotFound {
            entity: "course offering",
        })?;
    let scope = institution_scope(ctx, o.institution_id);
    require_institution_writable(st, &scope, o.institution_id).await?;
    require(
        !o.status.is_terminal() && o.status.as_str() != "cancelled",
        format!("the course offering is {}", o.status),
    )?;
    insert(st, ctx, &o, None, cmd, actor).await
}

async fn publish_in(
    conn: &mut sqlx::PgConnection,
    scope: &sutra_kernel::scope::ExecutionScope,
    tenant: TenantId,
    r: &CourseResource,
    actor: &Actor,
) -> Result<CourseResource, KernelError> {
    let n: CourseResource = kit::patch(
        conn,
        TABLE,
        tenant,
        r.id,
        json!({"status": "published", "status_reason": null, "published_at": kit::now()}),
    )
    .await?;
    kit::audit(
        conn,
        scope,
        TABLE,
        "publish",
        r.id,
        json!({"from": r.status}),
        actor,
    )
    .await?;
    StatusEvent {
        aggregate_id: r.id,
        institution_id: r.institution_id,
        offering_id: r.offering_id,
        section_id: r.section_id,
        from: Some(r.status.as_str()),
        to: "published",
        version: n.version,
        ..Default::default()
    }
    .emit(
        conn,
        scope,
        (COURSE_RESOURCE_PUBLISHED, COURSE_RESOURCE_PUBLISHED_SCHEMA),
        actor,
    )
    .await?;
    Ok(n)
}

async fn load(
    st: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
) -> Result<(CourseResource, sutra_kernel::scope::ExecutionScope), KernelError> {
    crate::shared::faculty::require_teaching_role(ctx, Permission::CourseResourceManage)?;
    let r = get(st, ctx.scope.tenant_id, id).await?;
    require_manage(st, ctx, r.section_id).await?;
    check_version(r.version, expected)?;
    let scope = kit::row_scope(ctx, r.institution_id);
    require_institution_writable(
        st,
        &scope,
        sutra_kernel::scope::InstitutionId(r.institution_id),
    )
    .await?;
    Ok((r, scope))
}

/// Publish (draft / withdrawn → published) — visible in the students' Library.
pub async fn publish(
    st: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    actor: &Actor,
) -> Result<CourseResource, KernelError> {
    let (r, scope) = load(st, ctx, id, expected).await?;
    require(
        r.status != S::Published,
        "the resource is already published",
    )?;
    if let Some(s) = r.section_id {
        kit::deliverable_section(st, ctx, s).await?;
    }
    let tenant = ctx.scope.tenant_id;
    let mut tx = begin(st).await?;
    let locked: CourseResource = kit::lock(&mut tx, TABLE, ENTITY, tenant, id).await?;
    check_version(locked.version, expected)?;
    publish_in(&mut tx, &scope, tenant, &locked, actor).await?;
    commit(tx).await?;
    get(st, tenant, id).await
}

/// Withdraw a published resource (kept for history; hidden from the Library).
pub async fn withdraw(
    st: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    reason: Option<String>,
    actor: &Actor,
) -> Result<CourseResource, KernelError> {
    let (r, scope) = load(st, ctx, id, expected).await?;
    require(
        r.status != S::Withdrawn,
        "the resource is already withdrawn",
    )?;
    let tenant = ctx.scope.tenant_id;
    let mut tx = begin(st).await?;
    let locked: CourseResource = kit::lock(&mut tx, TABLE, ENTITY, tenant, id).await?;
    check_version(locked.version, expected)?;
    kit::patch::<CourseResource>(
        &mut tx,
        TABLE,
        tenant,
        id,
        json!({"status": "withdrawn", "status_reason": reason}),
    )
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        TABLE,
        "withdraw",
        id,
        json!({"reason": reason}),
        actor,
    )
    .await?;
    commit(tx).await?;
    get(st, tenant, id).await
}

/// Edit title / kind / url / description (not of a withdrawn resource).
pub async fn update(
    st: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    cmd: CourseResourcePatch,
    actor: &Actor,
) -> Result<CourseResource, KernelError> {
    let (r, scope) = load(st, ctx, id, expected).await?;
    require(
        r.status != S::Withdrawn,
        "a withdrawn resource cannot be edited",
    )?;
    let mut changes = serde_json::Map::new();
    if let Some(t) = &cmd.title {
        changes.insert("title".into(), json!(kit::required_text("title", Some(t))?));
    }
    if let Some(k) = cmd.kind {
        changes.insert("kind".into(), json!(k.as_str()));
    }
    if let Some(u) = &cmd.url {
        changes.insert("url".into(), json!(validate_url(u)?));
    }
    if let Some(d) = &cmd.description {
        changes.insert("description".into(), json!(kit::opt_text(Some(d.as_str()))));
    }
    if changes.is_empty() {
        return Err(invalid("body", "empty", "nothing to change"));
    }
    let fields: Vec<String> = changes.keys().cloned().collect();
    let tenant = ctx.scope.tenant_id;
    let mut tx = begin(st).await?;
    let locked: CourseResource = kit::lock(&mut tx, TABLE, ENTITY, tenant, id).await?;
    check_version(locked.version, expected)?;
    kit::patch::<CourseResource>(&mut tx, TABLE, tenant, id, Value::Object(changes)).await?;
    kit::audit(
        &mut tx,
        &scope,
        TABLE,
        "update",
        id,
        json!({"changedFields": fields}),
        actor,
    )
    .await?;
    commit(tx).await?;
    get(st, tenant, id).await
}

/// Student Library: published resources of the caller's seated sections (and offering-wide
/// resources of those offerings).
pub async fn library(
    st: &AcademicState,
    ctx: &AuthContext,
) -> Result<Vec<LibraryItem>, KernelError> {
    let (_s, sections) = crate::shared::me::my_sections(st, ctx).await?;
    if sections.is_empty() {
        return Ok(Vec::new());
    }
    let section_ids: Vec<Uuid> = sections.iter().map(|s| s.id.as_uuid()).collect();
    let offering_ids: Vec<Uuid> = sections.iter().map(|s| s.offering_id.as_uuid()).collect();
    let rows: Vec<Value> = sqlx::query_scalar(
        "SELECT to_jsonb(t) FROM academic.course_resource t WHERE t.tenant_id = $1 AND t.status = 'published' \
         AND (t.section_id = ANY($2) OR (t.section_id IS NULL AND t.offering_id = ANY($3))) ORDER BY t.published_at DESC LIMIT 500",
    )
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(&section_ids)
    .bind(&offering_ids)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let mut out = Vec::new();
    for v in rows {
        let r: CourseResource = kit::decode(v)?;
        let sec = sections.iter().find(|s| {
            Some(s.id.as_uuid()) == r.section_id
                || (r.section_id.is_none() && Some(s.offering_id.as_uuid()) == r.offering_id)
        });
        let Some(sec) = sec else { continue };
        out.push(LibraryItem {
            id: r.id,
            offering_id: r.offering_id,
            section_id: r.section_id,
            title: r.title,
            kind: r.kind,
            url: r.url,
            description: r.description,
            status: r.status,
            offering_code: sec.offering_code.clone(),
            offering_title: sec.offering_title.clone(),
            section_code: r.section_id.map(|_| sec.section_code.clone()),
            published_at: r.published_at,
        });
    }
    Ok(out)
}

/// Faculty Resources: the sections the caller actively teaches and their resources.
pub async fn mine(st: &AcademicState, ctx: &AuthContext) -> Result<MyResources, KernelError> {
    let sections = crate::shared::faculty::my_sections(st, ctx).await?;
    let ids: Vec<Uuid> = sections.iter().map(|s| s.id.as_uuid()).collect();
    let rows: Vec<Value> = sqlx::query_scalar(
        "SELECT to_jsonb(t) FROM academic.course_resource t WHERE t.tenant_id = $1 AND t.section_id = ANY($2) ORDER BY t.created_at DESC LIMIT 500",
    )
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(&ids)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    Ok(MyResources {
        sections: sections
            .iter()
            .map(|s| ResourceSection {
                section_id: s.id.as_uuid(),
                offering_id: s.offering_id.as_uuid(),
                offering_code: s.offering_code.clone(),
                title: s.offering_title.clone(),
                section_code: s.section_code.clone(),
            })
            .collect(),
        items: rows
            .into_iter()
            .map(kit::decode)
            .collect::<Result<_, _>>()?,
    })
}
