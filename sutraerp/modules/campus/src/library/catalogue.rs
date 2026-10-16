//! Catalogue records (L3-G-127 identification → published record, G-128 update) and copies
//! (G-129 registration → circulation availability with an `ACC-…` accession number held in a
//! library / reading-room space, G-130 condition change → operational restriction, withdrawal).

use super::common::{
    check_version, clean, expected, invalid, optional_text, page_limit, required_reason,
    required_text, transition, LibraryReasonBody, Transition,
};
use super::domain::{
    valid_isbn, CatalogueResourceType, CatalogueStatus, LibraryCopyCondition, LibraryCopyStatus,
};
use crate::shared::decimal::{fmt2, parse_decimal};
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, with_etag};
use crate::shared::lifecycle::illegal;
use crate::shared::numbering::{self, kind};
use crate::shared::publish;
use crate::shared::rows::{decode, load_node};
use crate::shared::scope::institution_scope;
use crate::shared::{Actor, CampusState};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use sutra_contracts::campus::events::library as ev;
use sutra_contracts::campus::ids::InfrastructureNodeId;
use sutra_contracts::campus::status::{InfrastructureNodeType, SpaceType};
use sutra_kernel::auth::Permission;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

// ================================================================ catalogue records

/// A catalogue record with its copy counts.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CatalogueRecordView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub title: String,
    pub authors: Vec<String>,
    #[schema(example = "9780306406157")]
    pub isbn: Option<String>,
    pub publisher: Option<String>,
    pub publication_year: Option<i32>,
    pub edition: Option<String>,
    pub subjects: Vec<String>,
    pub resource_type: CatalogueResourceType,
    #[schema(value_type = String, example = "499.00")]
    pub replacement_cost: String,
    pub status: CatalogueStatus,
    pub status_reason: Option<String>,
    pub copies: i64,
    pub available_copies: i64,
    /// Reservations still waiting (queued or ready).
    pub queued_reservations: i64,
    pub version: i64,
    #[serde(skip)]
    pub replacement_cost_value: Decimal,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CatalogueRecordList {
    pub items: Vec<CatalogueRecordView>,
}

pub const RECORD_SELECT: &str = "SELECT r.id, r.institution_id, r.title, r.authors, r.isbn, \
    r.publisher, r.publication_year, r.edition, r.subjects, r.resource_type, r.replacement_cost, \
    r.status, r.status_reason, r.version, \
    (SELECT count(*) FROM campus.library_copy c WHERE c.record_id = r.id AND c.status <> 'withdrawn') AS copies, \
    (SELECT count(*) FROM campus.library_copy c WHERE c.record_id = r.id AND c.status = 'available') AS available_copies, \
    (SELECT count(*) FROM campus.library_reservation x WHERE x.record_id = r.id AND x.status IN ('queued', 'ready')) AS queued_reservations \
    FROM campus.catalogue_record r";

pub fn record_from_row(r: &PgRow) -> Result<CatalogueRecordView, sqlx::Error> {
    let cost: Decimal = r.try_get("replacement_cost")?;
    Ok(CatalogueRecordView {
        id: r.try_get("id")?,
        institution_id: r.try_get("institution_id")?,
        title: r.try_get("title")?,
        authors: r.try_get("authors")?,
        isbn: r.try_get("isbn")?,
        publisher: r.try_get("publisher")?,
        publication_year: r.try_get("publication_year")?,
        edition: r.try_get("edition")?,
        subjects: r.try_get("subjects")?,
        resource_type: decode(r.try_get("resource_type")?, CatalogueResourceType::parse)?,
        replacement_cost: fmt2(cost),
        status: decode(r.try_get("status")?, CatalogueStatus::parse)?,
        status_reason: r.try_get("status_reason")?,
        copies: r.try_get("copies")?,
        available_copies: r.try_get("available_copies")?,
        queued_reservations: r.try_get("queued_reservations")?,
        version: r.try_get("version")?,
        replacement_cost_value: cost,
    })
}

/// Loads one record of the tenant (404), optionally locked.
pub async fn load_record(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<CatalogueRecordView, KernelError> {
    if lock {
        sqlx::query(
            "SELECT id FROM campus.catalogue_record WHERE id = $1 AND tenant_id = $2 FOR UPDATE",
        )
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?;
    }
    let row = sqlx::query(&format!(
        "{RECORD_SELECT} WHERE r.id = $1 AND r.tenant_id = $2"
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "catalogue record",
    })?;
    record_from_row(&row).map_err(map_db_error)
}

fn clean_list(field: &str, v: Option<Vec<String>>) -> Result<Vec<String>, KernelError> {
    let items: Vec<String> = v
        .unwrap_or_default()
        .into_iter()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect();
    if items.len() > 20 || items.iter().any(|s| s.chars().count() > 200) {
        return Err(invalid(
            field,
            "invalid",
            "at most 20 entries of at most 200 characters",
        ));
    }
    Ok(items)
}

fn clean_isbn(v: Option<String>) -> Result<Option<String>, KernelError> {
    let Some(raw) = clean(v) else {
        return Ok(None);
    };
    let isbn: String = raw
        .chars()
        .filter(|c| !matches!(c, '-' | ' '))
        .collect::<String>()
        .to_uppercase();
    if !valid_isbn(&isbn) {
        return Err(invalid(
            "isbn",
            "invalid_isbn",
            "must be a valid ISBN-10 or ISBN-13",
        ));
    }
    Ok(Some(isbn))
}

fn clean_cost(v: Option<String>) -> Result<Option<Decimal>, KernelError> {
    let Some(raw) = clean(v) else {
        return Ok(None);
    };
    let c = parse_decimal("replacementCost", &raw)?;
    if c < Decimal::ZERO {
        return Err(invalid(
            "replacementCost",
            "negative",
            "must not be negative",
        ));
    }
    Ok(Some(c))
}

fn check_year(y: Option<i32>) -> Result<(), KernelError> {
    if y.is_some_and(|y| !(1400..=2200).contains(&y)) {
        return Err(invalid(
            "publicationYear",
            "out_of_range",
            "between 1400 and 2200",
        ));
    }
    Ok(())
}

/// A resource to catalogue (L3-G-127).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CatalogueRecordCreate {
    pub title: String,
    pub authors: Option<Vec<String>>,
    pub isbn: Option<String>,
    pub publisher: Option<String>,
    pub publication_year: Option<i32>,
    pub edition: Option<String>,
    pub subjects: Option<Vec<String>>,
    pub resource_type: Option<CatalogueResourceType>,
    /// Replacement cost of one copy (lost-copy obligations).
    #[schema(value_type = Option<String>, example = "499.00")]
    pub replacement_cost: Option<String>,
}

/// Catalogue a resource as a draft record (`campus.library.manage`; Idempotency-Key). 409 for a
/// duplicate ISBN.
#[utoipa::path(post, path = "/api/v1/campus/institutions/{id}/catalogue", operation_id = "campus_library_catalogue_create", tag = "campus-library",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)),
    request_body = CatalogueRecordCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = CatalogueRecordView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn create_record(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<CatalogueRecordCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::LibraryManage)?;
    require_idempotency_key(&headers)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let title = required_text("title", &b.title, 300)?;
    let authors = clean_list("authors", b.authors)?;
    let subjects = clean_list("subjects", b.subjects)?;
    let isbn = clean_isbn(b.isbn)?;
    let publisher = optional_text("publisher", b.publisher, 200)?;
    let edition = optional_text("edition", b.edition, 100)?;
    check_year(b.publication_year)?;
    let cost = clean_cost(b.replacement_cost)?.unwrap_or(Decimal::ZERO);
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let rid: Uuid = sqlx::query_scalar(
        "INSERT INTO campus.catalogue_record (tenant_id, institution_id, title, authors, isbn, \
         publisher, publication_year, edition, subjects, resource_type, replacement_cost) VALUES \
         ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) RETURNING id",
    )
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .bind(&title)
    .bind(authors)
    .bind(isbn.as_deref())
    .bind(publisher.as_deref())
    .bind(b.publication_year)
    .bind(edition.as_deref())
    .bind(subjects)
    .bind(
        b.resource_type
            .unwrap_or(CatalogueResourceType::Book)
            .as_str(),
    )
    .bind(cost)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| match map_db_error(e) {
        KernelError::Conflict(_) => {
            KernelError::Conflict("a catalogue record with this ISBN already exists".into())
        }
        other => other,
    })?;
    transition(
        &mut tx,
        &scope,
        &Transition {
            aggregate_type: "catalogue_record",
            id: rid,
            institution,
            from: None,
            to: "draft",
            version: 1,
            reason: None,
            process_instance_id: None,
            number: isbn.as_deref(),
            subject_ref: None,
        },
        None,
        "campus.catalogue_record.create",
        &actor,
    )
    .await?;
    let r = load_record(&mut tx, ctx.scope.tenant_id, rid, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::CREATED, &r, Version(r.version)))
}

/// Filters of the catalogue.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct CatalogueQuery {
    pub status: Option<CatalogueStatus>,
    /// Matches title, authors, ISBN or subjects.
    pub q: Option<String>,
    pub limit: Option<i64>,
}

/// Searches records of an institution (shared by the office and `/me/library/catalogue`).
pub async fn search(
    conn: &mut PgConnection,
    tenant: TenantId,
    institution: Option<InstitutionId>,
    status: Option<CatalogueStatus>,
    q: Option<String>,
    limit: Option<i64>,
) -> Result<Vec<CatalogueRecordView>, KernelError> {
    let needle = clean(q).map(|s| sutra_persistence::like_contains(&s.to_lowercase()));
    let rows = sqlx::query(&format!(
        "{RECORD_SELECT} WHERE r.tenant_id = $1 AND ($2::uuid IS NULL OR r.institution_id = $2) \
         AND ($3::text IS NULL OR r.status = $3) AND ($4::text IS NULL OR lower(r.title || ' ' || \
         array_to_string(r.authors, ' ') || ' ' || COALESCE(r.isbn, '') || ' ' || \
         array_to_string(r.subjects, ' ')) LIKE $4) ORDER BY r.title, r.id LIMIT $5"
    ))
    .bind(tenant.as_uuid())
    .bind(institution.map(|i| i.as_uuid()))
    .bind(status.map(|s| s.as_str()))
    .bind(needle)
    .bind(page_limit(limit))
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    rows.iter()
        .map(record_from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)
}

/// The catalogue of an institution (`campus.read`).
#[utoipa::path(get, path = "/api/v1/campus/institutions/{id}/catalogue", operation_id = "campus_library_catalogue_list", tag = "campus-library",
    params(("id" = String, Path, format = Uuid), CatalogueQuery), security(("session_cookie" = [])),
    responses((status = 200, body = CatalogueRecordList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn list_records(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<CatalogueQuery>,
) -> ApiResult<Json<CatalogueRecordList>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let institution = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, institution), institution).await?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let items = search(
        &mut conn,
        ctx.scope.tenant_id,
        Some(institution),
        q.status,
        q.q,
        q.limit,
    )
    .await?;
    Ok(Json(CatalogueRecordList { items }))
}

/// One catalogue record (`campus.read`).
#[utoipa::path(get, path = "/api/v1/campus/catalogue-records/{id}", operation_id = "campus_library_catalogue_get", tag = "campus-library",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = CatalogueRecordView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn get_record(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CampusRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let r = load_record(&mut conn, ctx.scope.tenant_id, id, false).await?;
    Ok(with_etag(StatusCode::OK, &r, Version(r.version)))
}

/// Changes to a catalogue record (absent = unchanged; L3-G-128).
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CatalogueRecordUpdate {
    pub title: Option<String>,
    pub authors: Option<Vec<String>>,
    pub isbn: Option<String>,
    pub publisher: Option<String>,
    pub publication_year: Option<i32>,
    pub edition: Option<String>,
    pub subjects: Option<Vec<String>>,
    pub resource_type: Option<CatalogueResourceType>,
    #[schema(value_type = Option<String>, example = "520.00")]
    pub replacement_cost: Option<String>,
}

/// Update the descriptive information of a record (`campus.library.manage`; If-Match). The
/// change is effective at once for the catalogue and new obligations; 409 once withdrawn.
#[utoipa::path(patch, path = "/api/v1/campus/catalogue-records/{id}", operation_id = "campus_library_catalogue_update", tag = "campus-library",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = CatalogueRecordUpdate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = CatalogueRecordView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn update_record(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<CatalogueRecordUpdate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::LibraryManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let mut changed: Vec<&'static str> = Vec::new();
    let title = b
        .title
        .as_deref()
        .map(|s| required_text("title", s, 300))
        .transpose()?;
    if title.is_some() {
        changed.push("title");
    }
    let authors = match b.authors {
        Some(a) => {
            changed.push("authors");
            Some(clean_list("authors", Some(a))?)
        }
        None => None,
    };
    let subjects = match b.subjects {
        Some(a) => {
            changed.push("subjects");
            Some(clean_list("subjects", Some(a))?)
        }
        None => None,
    };
    let isbn = clean_isbn(b.isbn)?;
    if isbn.is_some() {
        changed.push("isbn");
    }
    let publisher = optional_text("publisher", b.publisher, 200)?;
    if publisher.is_some() {
        changed.push("publisher");
    }
    let edition = optional_text("edition", b.edition, 100)?;
    if edition.is_some() {
        changed.push("edition");
    }
    check_year(b.publication_year)?;
    if b.publication_year.is_some() {
        changed.push("publicationYear");
    }
    if b.resource_type.is_some() {
        changed.push("resourceType");
    }
    let cost = clean_cost(b.replacement_cost)?;
    if cost.is_some() {
        changed.push("replacementCost");
    }
    if changed.is_empty() {
        return Err(invalid("body", "empty", "nothing to change").into());
    }
    let pre = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_record(&mut conn, t, id, false).await?
    };
    let institution = InstitutionId(pre.institution_id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let r = load_record(&mut tx, t, id, true).await?;
    check_version(v, r.version)?;
    if r.status == CatalogueStatus::Withdrawn {
        return Err(
            KernelError::Conflict(format!("catalogue record {} is withdrawn", r.title)).into(),
        );
    }
    sqlx::query(
        "UPDATE campus.catalogue_record SET title = COALESCE($2, title), authors = COALESCE($3, authors), \
         isbn = COALESCE($4, isbn), publisher = COALESCE($5, publisher), \
         publication_year = COALESCE($6, publication_year), edition = COALESCE($7, edition), \
         subjects = COALESCE($8, subjects), resource_type = COALESCE($9, resource_type), \
         replacement_cost = COALESCE($10, replacement_cost), version = version + 1, updated_at = now() \
         WHERE id = $1",
    )
    .bind(id)
    .bind(title.as_deref())
    .bind(authors)
    .bind(isbn.as_deref())
    .bind(publisher.as_deref())
    .bind(b.publication_year)
    .bind(edition.as_deref())
    .bind(subjects)
    .bind(b.resource_type.map(|x| x.as_str()))
    .bind(cost)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    publish::audit(
        &mut tx,
        &scope,
        "campus.catalogue_record.update",
        "catalogue_record",
        id.to_string(),
        serde_json::json!({ "changedFields": changed }),
        &actor,
    )
    .await?;
    let out = load_record(&mut tx, t, id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &out, Version(out.version)))
}

async fn change_record(
    st: &CampusState,
    ctx: &sutra_kernel::auth::AuthContext,
    id: Uuid,
    expected_version: i64,
    to: CatalogueStatus,
    reason: Option<String>,
) -> Result<CatalogueRecordView, KernelError> {
    let t = ctx.scope.tenant_id;
    let pre = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_record(&mut conn, t, id, false).await?
    };
    let institution = InstitutionId(pre.institution_id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(st, &scope, institution).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let r = load_record(&mut tx, t, id, true).await?;
    check_version(expected_version, r.version)?;
    if !r.status.can_transition(to) {
        return Err(illegal(
            "catalogue record",
            &r.title,
            r.status.as_str(),
            to.as_str(),
        ));
    }
    if to == CatalogueStatus::Withdrawn {
        let out: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM campus.library_copy WHERE record_id = $1 AND status IN ('issued', 'reserved')",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
        if out > 0 {
            return Err(KernelError::Conflict(format!(
                "{out} copies of {} are issued or reserved",
                r.title
            )));
        }
    }
    sqlx::query(
        "UPDATE campus.catalogue_record SET status = $2, status_reason = $3, version = version + 1, \
         updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(to.as_str())
    .bind(reason.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let (event, action) = match to {
        CatalogueStatus::Published => (
            Some((
                ev::CATALOGUE_RECORD_PUBLISHED,
                ev::CATALOGUE_RECORD_PUBLISHED_SCHEMA,
            )),
            "campus.catalogue_record.publish",
        ),
        _ => (None, "campus.catalogue_record.withdraw"),
    };
    transition(
        &mut tx,
        &scope,
        &Transition {
            aggregate_type: "catalogue_record",
            id,
            institution,
            from: Some(r.status.as_str()),
            to: to.as_str(),
            version: r.version + 1,
            reason: reason.as_deref(),
            process_instance_id: None,
            number: r.isbn.as_deref(),
            subject_ref: None,
        },
        event,
        action,
        &actor,
    )
    .await?;
    let out = load_record(&mut tx, t, id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

/// Publish a draft record to the catalogue (`campus.library.manage`; If-Match).
#[utoipa::path(post, path = "/api/v1/campus/catalogue-records/{id}/publish", operation_id = "campus_library_catalogue_publish", tag = "campus-library",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = LibraryReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = CatalogueRecordView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn publish_record(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LibraryReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::LibraryManage)?;
    let v = expected!(headers);
    let r = change_record(&st, ctx, id, v, CatalogueStatus::Published, clean(b.reason)).await?;
    Ok(with_etag(StatusCode::OK, &r, Version(r.version)))
}

/// Withdraw a record from the catalogue (`campus.library.manage`; If-Match; reason required).
/// 409 while a copy is issued or reserved.
#[utoipa::path(post, path = "/api/v1/campus/catalogue-records/{id}/withdraw", operation_id = "campus_library_catalogue_withdraw", tag = "campus-library",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = LibraryReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = CatalogueRecordView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn withdraw_record(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LibraryReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::LibraryManage)?;
    let v = expected!(headers);
    let reason = required_reason(b.reason)?;
    let r = change_record(&st, ctx, id, v, CatalogueStatus::Withdrawn, Some(reason)).await?;
    Ok(with_etag(StatusCode::OK, &r, Version(r.version)))
}

// ================================================================ copies

/// A physical copy.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LibraryCopyView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub record_id: Uuid,
    pub title: String,
    #[schema(example = "ACC-000001")]
    pub accession_number: String,
    pub node_id: Option<Uuid>,
    pub node_code: Option<String>,
    pub shelf_mark: Option<String>,
    pub condition: LibraryCopyCondition,
    pub status: LibraryCopyStatus,
    pub status_reason: Option<String>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct LibraryCopyList {
    pub items: Vec<LibraryCopyView>,
}

pub const COPY_SELECT: &str = "SELECT c.id, c.institution_id, c.record_id, r.title, c.accession_number, \
    c.node_id, n.code AS node_code, c.shelf_mark, c.condition, c.status, c.status_reason, c.version \
    FROM campus.library_copy c JOIN campus.catalogue_record r ON r.id = c.record_id \
    LEFT JOIN campus.infrastructure_node n ON n.id = c.node_id";

pub fn copy_from_row(r: &PgRow) -> Result<LibraryCopyView, sqlx::Error> {
    Ok(LibraryCopyView {
        id: r.try_get("id")?,
        institution_id: r.try_get("institution_id")?,
        record_id: r.try_get("record_id")?,
        title: r.try_get("title")?,
        accession_number: r.try_get("accession_number")?,
        node_id: r.try_get("node_id")?,
        node_code: r.try_get("node_code")?,
        shelf_mark: r.try_get("shelf_mark")?,
        condition: decode(r.try_get("condition")?, LibraryCopyCondition::parse)?,
        status: decode(r.try_get("status")?, LibraryCopyStatus::parse)?,
        status_reason: r.try_get("status_reason")?,
        version: r.try_get("version")?,
    })
}

/// Loads one copy of the tenant (404), optionally locked.
pub async fn load_copy(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<LibraryCopyView, KernelError> {
    if lock {
        sqlx::query(
            "SELECT id FROM campus.library_copy WHERE id = $1 AND tenant_id = $2 FOR UPDATE",
        )
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?;
    }
    let row = sqlx::query(&format!(
        "{COPY_SELECT} WHERE c.id = $1 AND c.tenant_id = $2"
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "library copy",
    })?;
    copy_from_row(&row).map_err(map_db_error)
}

/// Finds a copy by accession number in an institution (404).
pub async fn copy_by_accession(
    conn: &mut PgConnection,
    tenant: TenantId,
    institution: InstitutionId,
    accession: &str,
) -> Result<LibraryCopyView, KernelError> {
    let row = sqlx::query(&format!(
        "{COPY_SELECT} WHERE c.tenant_id = $1 AND c.institution_id = $2 AND c.accession_number = $3"
    ))
    .bind(tenant.as_uuid())
    .bind(institution.as_uuid())
    .bind(accession.trim().to_uppercase())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "library copy",
    })?;
    copy_from_row(&row).map_err(map_db_error)
}

/// Moves a copy to `to` (illegal → 409) with history and audit; the caller holds the lock.
#[allow(clippy::too_many_arguments)]
pub async fn set_copy_status(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    c: &LibraryCopyView,
    to: LibraryCopyStatus,
    condition: Option<LibraryCopyCondition>,
    reason: Option<&str>,
    action: &'static str,
    actor: &Actor,
) -> Result<(), KernelError> {
    if c.status != to && !c.status.can_transition(to) {
        return Err(illegal(
            "library copy",
            &c.accession_number,
            c.status.as_str(),
            to.as_str(),
        ));
    }
    sqlx::query(
        "UPDATE campus.library_copy SET status = $2, condition = COALESCE($3, condition), \
         status_reason = $4, version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(c.id)
    .bind(to.as_str())
    .bind(condition.map(|x| x.as_str()))
    .bind(reason)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    transition(
        conn,
        scope,
        &Transition {
            aggregate_type: "library_copy",
            id: c.id,
            institution: InstitutionId(c.institution_id),
            from: Some(c.status.as_str()),
            to: to.as_str(),
            version: c.version + 1,
            reason,
            process_instance_id: None,
            number: Some(&c.accession_number),
            subject_ref: None,
        },
        None,
        action,
        actor,
    )
    .await
}

/// A copy to register (L3-G-129).
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LibraryCopyCreate {
    /// Library or reading-room space holding the copy (an active `space` node).
    pub node_id: Option<Uuid>,
    pub shelf_mark: Option<String>,
    pub condition: Option<LibraryCopyCondition>,
}

/// Register a copy of a record (`campus.library.manage`; Idempotency-Key): accession number
/// `ACC-…`, status `available` (a copy registered as `damaged` stays restricted). 409 for a
/// withdrawn record; 422 unless the location is an active library / reading-room space.
#[utoipa::path(post, path = "/api/v1/campus/catalogue-records/{id}/copies", operation_id = "campus_library_copy_create", tag = "campus-library",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)),
    request_body = LibraryCopyCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = LibraryCopyView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn create_copy(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LibraryCopyCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::LibraryManage)?;
    require_idempotency_key(&headers)?;
    let t = ctx.scope.tenant_id;
    let shelf = optional_text("shelfMark", b.shelf_mark, 60)?;
    let pre = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        let r = load_record(&mut conn, t, id, false).await?;
        if let Some(node) = b.node_id {
            let n = load_node(&mut conn, t, InfrastructureNodeId(node))
                .await?
                .filter(|n| n.institution_id.as_uuid() == r.institution_id)
                .ok_or_else(|| invalid("nodeId", "unknown_node", "no such space"))?;
            let library_space = n.node_type == InfrastructureNodeType::Space
                && matches!(
                    n.space_type,
                    Some(SpaceType::Library | SpaceType::ReadingRoom)
                );
            if !library_space || !n.status.is_operational() {
                return Err(invalid(
                    "nodeId",
                    "not_a_library_space",
                    "must be an active library or reading-room space",
                )
                .into());
            }
        }
        r
    };
    let institution = InstitutionId(pre.institution_id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    if pre.status == CatalogueStatus::Withdrawn {
        return Err(
            KernelError::Conflict(format!("catalogue record {} is withdrawn", pre.title)).into(),
        );
    }
    let condition = b.condition.unwrap_or(LibraryCopyCondition::Good);
    let status = if condition == LibraryCopyCondition::Damaged {
        LibraryCopyStatus::Damaged
    } else {
        LibraryCopyStatus::Available
    };
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let accession = numbering::next(&mut tx, t, institution, kind::LIBRARY_COPY).await?;
    let cid: Uuid = sqlx::query_scalar(
        "INSERT INTO campus.library_copy (tenant_id, institution_id, record_id, accession_number, \
         node_id, shelf_mark, condition, status) VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(institution.as_uuid())
    .bind(id)
    .bind(&accession)
    .bind(b.node_id)
    .bind(shelf.as_deref())
    .bind(condition.as_str())
    .bind(status.as_str())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    transition(
        &mut tx,
        &scope,
        &Transition {
            aggregate_type: "library_copy",
            id: cid,
            institution,
            from: None,
            to: status.as_str(),
            version: 1,
            reason: None,
            process_instance_id: None,
            number: Some(&accession),
            subject_ref: None,
        },
        None,
        "campus.library_copy.register",
        &actor,
    )
    .await?;
    // A newly available copy goes to the head of the reservation queue first.
    if status == LibraryCopyStatus::Available {
        let c = load_copy(&mut tx, t, cid, true).await?;
        super::circulation::offer_to_queue(&mut tx, &st, &scope, &c, &actor).await?;
    }
    let c = load_copy(&mut tx, t, cid, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::CREATED, &c, Version(c.version)))
}

/// Copies of a record (`campus.read`).
#[utoipa::path(get, path = "/api/v1/campus/catalogue-records/{id}/copies", operation_id = "campus_library_copy_list", tag = "campus-library",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = LibraryCopyList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn list_copies(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<LibraryCopyList>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let rows = sqlx::query(&format!(
        "{COPY_SELECT} WHERE c.record_id = $1 AND c.tenant_id = $2 ORDER BY c.accession_number"
    ))
    .bind(id)
    .bind(ctx.scope.tenant_id.as_uuid())
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let items = rows
        .iter()
        .map(copy_from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)?;
    Ok(Json(LibraryCopyList { items }))
}

/// A condition change (L3-G-130).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LibraryCopyConditionBody {
    pub condition: LibraryCopyCondition,
    pub reason: Option<String>,
}

/// Record the condition of a copy (`campus.library.manage`; If-Match). `damaged` takes an
/// available copy out of circulation; a repaired copy returns to `available` (and to the head of
/// the reservation queue). 409 for lost / withdrawn copies.
#[utoipa::path(post, path = "/api/v1/campus/library-copies/{id}/condition", operation_id = "campus_library_copy_condition", tag = "campus-library",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = LibraryCopyConditionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = LibraryCopyView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn condition(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LibraryCopyConditionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::LibraryManage)?;
    let v = expected!(headers);
    let reason = optional_text("reason", b.reason, 1000)?;
    let t = ctx.scope.tenant_id;
    let pre = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_copy(&mut conn, t, id, false).await?
    };
    let institution = InstitutionId(pre.institution_id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let c = load_copy(&mut tx, t, id, true).await?;
    check_version(v, c.version)?;
    let damaged = b.condition == LibraryCopyCondition::Damaged;
    let to = match c.status {
        LibraryCopyStatus::Lost | LibraryCopyStatus::Withdrawn => {
            return Err(illegal(
                "library copy",
                &c.accession_number,
                c.status.as_str(),
                b.condition.as_str(),
            )
            .into())
        }
        LibraryCopyStatus::Available if damaged => LibraryCopyStatus::Damaged,
        LibraryCopyStatus::Damaged if !damaged => LibraryCopyStatus::Available,
        s => s,
    };
    set_copy_status(
        &mut tx,
        &scope,
        &c,
        to,
        Some(b.condition),
        reason.as_deref().or(Some(b.condition.as_str())),
        "campus.library_copy.condition",
        &actor,
    )
    .await?;
    if c.status == LibraryCopyStatus::Damaged && to == LibraryCopyStatus::Available {
        let c = load_copy(&mut tx, t, id, true).await?;
        super::circulation::offer_to_queue(&mut tx, &st, &scope, &c, &actor).await?;
    }
    let out = load_copy(&mut tx, t, id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &out, Version(out.version)))
}

/// Withdraw a copy from circulation (`campus.library.manage`; If-Match; reason required). 409
/// while issued or reserved.
#[utoipa::path(post, path = "/api/v1/campus/library-copies/{id}/withdraw", operation_id = "campus_library_copy_withdraw", tag = "campus-library",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = LibraryReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = LibraryCopyView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn withdraw_copy(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LibraryReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::LibraryManage)?;
    let v = expected!(headers);
    let reason = required_reason(b.reason)?;
    let t = ctx.scope.tenant_id;
    let pre = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_copy(&mut conn, t, id, false).await?
    };
    let institution = InstitutionId(pre.institution_id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let c = load_copy(&mut tx, t, id, true).await?;
    check_version(v, c.version)?;
    if c.status == LibraryCopyStatus::Withdrawn {
        return Err(illegal(
            "library copy",
            &c.accession_number,
            "withdrawn",
            "withdrawn",
        )
        .into());
    }
    set_copy_status(
        &mut tx,
        &scope,
        &c,
        LibraryCopyStatus::Withdrawn,
        None,
        Some(&reason),
        "campus.library_copy.withdraw",
        &actor,
    )
    .await?;
    let out = load_copy(&mut tx, t, id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &out, Version(out.version)))
}

pub fn router(state: CampusState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(create_record, list_records))
        .routes(routes!(get_record, update_record))
        .routes(routes!(publish_record))
        .routes(routes!(withdraw_record))
        .routes(routes!(create_copy, list_copies))
        .routes(routes!(condition))
        .routes(routes!(withdraw_copy))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isbn_is_normalised_and_checked() {
        assert_eq!(
            clean_isbn(Some("978-0-306-40615-7".into())).unwrap_or_default(),
            Some("9780306406157".into())
        );
        assert!(clean_isbn(Some("978-0-306-40615-8".into())).is_err());
        assert_eq!(clean_isbn(None).unwrap_or_default(), None);
        assert!(clean_list("authors", Some(vec!["a".repeat(201)])).is_err());
        assert!(check_year(Some(1300)).is_err());
        assert!(clean_cost(Some("-1".into())).is_err());
    }
}
