//! **ERP_EXTENSIONS 5A** — the public careers page (rules in [`super::careers`], ported from
//! HeyPuter/everything hiring `jobpage.js`, MIT). Tag `people-hr`; operation ids
//! `people_recruitment_careers_*`.
//!
//! * Admin (`people.read` / `people.staff.manage`): the institution's careers page (on / off,
//!   slug, intro, contact e-mail) and each requisition's public listing (published, description,
//!   location, closing date).
//! * Public (**no session**, like the payment webhook): `GET /people/public/careers/{id}` (id = slug) lists
//!   the open, published requisitions with safe fields only; `POST …/requisitions/{id}/apply`
//!   creates a candidate (`applied`, source "Careers page"). Spam-safe: a honeypot field (bots
//!   that fill it get the same 202 and nothing is stored), a 16 KB body cap, field size limits,
//!   one application per e-mail per requisition (a repeat answers 202 and stores nothing, so
//!   the endpoint never reveals who applied), a daily cap per requisition, and the API's
//!   per-client public rate limit. The slug lookup runs in the system database context; every
//!   later query runs in the institution's tenant context.

use super::careers::{self as cr, Application, Checked};
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::actor;
use crate::shared::scope::institution_scope;
use crate::shared::{Actor, PeopleState};
use crate::workforce::common::{self, history, invalid, optional_text};
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::Row;
use sutra_kernel::auth::Permission;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// Applications accepted per requisition per day from the public page.
pub const DAILY_APPLICATION_CAP: i64 = 200;
/// Actor recorded for public applications.
pub const PUBLIC_ACTOR: &str = "public:careers-page";

// ------------------------------------------------------------------ admin

/// A requisition's public listing.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HiringCareersListing {
    pub requisition_id: Uuid,
    pub number: String,
    pub title: String,
    pub status: String,
    pub published: bool,
    pub description: Option<String>,
    pub location: Option<String>,
    pub closes_on: Option<NaiveDate>,
    /// Candidates who applied through the careers page.
    pub applications: i64,
}

/// The careers page of an institution.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HiringCareersPage {
    pub enabled: bool,
    pub slug: Option<String>,
    /// A slug made from the institution's name (`subdomainBase`).
    pub suggested_slug: String,
    /// Web path of the public page (`/careers/<slug>`), when enabled.
    pub public_path: Option<String>,
    pub intro: Option<String>,
    pub contact_email: Option<String>,
    pub listings: Vec<HiringCareersListing>,
}

/// Careers page settings.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HiringCareersPageInput {
    pub enabled: bool,
    /// 3–40 of `a-z0-9-`; unique across SutraERP.
    pub slug: Option<String>,
    pub intro: Option<String>,
    pub contact_email: Option<String>,
}

async fn page(
    st: &PeopleState,
    scope: &ExecutionScope,
    t: TenantId,
    inst: Uuid,
) -> Result<HiringCareersPage, KernelError> {
    let name = require_institution(st, scope, InstitutionId(inst))
        .await?
        .name;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let row = sqlx::query(
        "SELECT careers_enabled, careers_slug, careers_intro, careers_contact_email FROM people.hiring_settings \
         WHERE tenant_id = $1 AND institution_id = $2",
    )
    .bind(t.as_uuid())
    .bind(inst)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let (enabled, slug, intro, email) = match row {
        None => (false, None, None, None),
        Some(r) => (
            r.try_get("careers_enabled").map_err(map_db_error)?,
            r.try_get::<Option<String>, _>("careers_slug")
                .map_err(map_db_error)?,
            r.try_get("careers_intro").map_err(map_db_error)?,
            r.try_get("careers_contact_email").map_err(map_db_error)?,
        ),
    };
    let rows = sqlx::query(
        "SELECT r.id, r.requisition_number, r.title, r.status, r.careers_published, r.public_description, \
         r.public_location, r.closes_on, (SELECT count(*) FROM people.candidate c WHERE c.requisition_id = r.id \
         AND c.source = 'Careers page') AS applications FROM people.requisition r \
         WHERE r.tenant_id = $1 AND r.institution_id = $2 AND r.status IN ('approved', 'open') \
         ORDER BY r.requisition_number DESC",
    )
    .bind(t.as_uuid())
    .bind(inst)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let listings = rows
        .iter()
        .map(|r| {
            Ok(HiringCareersListing {
                requisition_id: r.try_get("id")?,
                number: r.try_get("requisition_number")?,
                title: r.try_get("title")?,
                status: r.try_get("status")?,
                published: r.try_get("careers_published")?,
                description: r.try_get("public_description")?,
                location: r.try_get("public_location")?,
                closes_on: r.try_get("closes_on")?,
                applications: r.try_get("applications")?,
            })
        })
        .collect::<Result<Vec<_>, sqlx::Error>>()
        .map_err(map_db_error)?;
    Ok(HiringCareersPage {
        public_path: slug
            .as_ref()
            .filter(|_| enabled)
            .map(|s| format!("/careers/{s}")),
        enabled,
        slug,
        suggested_slug: cr::slug_base(&name),
        intro,
        contact_email: email,
        listings,
    })
}

/// The careers page settings and listings (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/institutions/{id}/careers-page", operation_id = "people_recruitment_careers_page_get", tag = "people-hr",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = HiringCareersPage), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn careers_page_get(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<HiringCareersPage>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let scope = institution_scope(ctx, InstitutionId(id));
    Ok(Json(page(&st, &scope, ctx.scope.tenant_id, id).await?))
}

/// Save the careers page (`people.staff.manage`). Enabling needs a slug; a slug another
/// institution uses is a 409.
#[utoipa::path(put, path = "/api/v1/people/institutions/{id}/careers-page", operation_id = "people_recruitment_careers_page_put", tag = "people-hr",
    params(("id" = String, Path, format = Uuid)), request_body = HiringCareersPageInput, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = HiringCareersPage), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn careers_page_put(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(b): Json<HiringCareersPageInput>,
) -> ApiResult<Json<HiringCareersPage>> {
    let ctx = auth.require(Permission::StaffManage)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let slug = b
        .slug
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty());
    if slug.as_deref().is_some_and(|s| !cr::is_valid_slug(s)) {
        return Err(invalid(
            "slug",
            "invalid_slug",
            "3 to 40 lower-case letters, digits or hyphens",
        )
        .into());
    }
    if b.enabled && slug.is_none() {
        return Err(invalid("slug", "required", "choose a slug to publish the page").into());
    }
    let intro = optional_text("intro", b.intro, 2000)?;
    let email = optional_text("contactEmail", b.contact_email, 254)?.map(|e| e.to_lowercase());
    if email
        .as_deref()
        .is_some_and(|e| e.split('@').count() != 2 || e.contains(char::is_whitespace))
    {
        return Err(invalid("contactEmail", "invalid_email", "an e-mail address").into());
    }
    let t = ctx.scope.tenant_id;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let res = sqlx::query(
        "INSERT INTO people.hiring_settings (tenant_id, institution_id, careers_enabled, careers_slug, careers_intro, \
         careers_contact_email) VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT (tenant_id, institution_id) DO UPDATE SET \
         careers_enabled = EXCLUDED.careers_enabled, careers_slug = EXCLUDED.careers_slug, careers_intro = EXCLUDED.careers_intro, \
         careers_contact_email = EXCLUDED.careers_contact_email, version = people.hiring_settings.version + 1, updated_at = now()",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(b.enabled)
    .bind(&slug)
    .bind(&intro)
    .bind(&email)
    .execute(&mut *tx)
    .await;
    if let Err(e) = res {
        if e.as_database_error()
            .is_some_and(|d| d.is_unique_violation())
        {
            return Err(
                KernelError::Conflict("that careers slug is taken; choose another".into()).into(),
            );
        }
        return Err(map_db_error(e).into());
    }
    let a = actor(ctx);
    common::audit_only(
        &mut tx,
        &scope,
        "institution",
        id,
        "people.careers_page.update",
        serde_json::json!({"enabled": b.enabled, "slug": slug}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(page(&st, &scope, t, id).await?))
}

/// A requisition's public listing.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HiringCareersListingInput {
    pub published: bool,
    /// Plain text; blank lines separate paragraphs, `-` / `*` lines are bullets.
    pub description: Option<String>,
    pub location: Option<String>,
    /// Applications close at the end of this day.
    pub closes_on: Option<NaiveDate>,
}

/// Publish or unpublish a requisition on the careers page (`people.staff.manage`). Only an
/// approved or open requisition can be published; it is listed while open.
#[utoipa::path(put, path = "/api/v1/people/requisitions/{id}/careers-listing", operation_id = "people_recruitment_careers_listing_put", tag = "people-hr",
    params(("id" = String, Path, format = Uuid)), request_body = HiringCareersListingInput, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = HiringCareersPage), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn careers_listing_put(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(rid): Path<Uuid>,
    Json(b): Json<HiringCareersListingInput>,
) -> ApiResult<Json<HiringCareersPage>> {
    let ctx = auth.require(Permission::StaffManage)?;
    let t = ctx.scope.tenant_id;
    let row: Option<(Uuid, String)> = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        sqlx::query_as(
            "SELECT institution_id, status FROM people.requisition WHERE id = $1 AND tenant_id = $2",
        )
        .bind(rid)
        .bind(t.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
    };
    let (inst_id, status) = row.ok_or(KernelError::NotFound {
        entity: "requisition",
    })?;
    let inst = InstitutionId(inst_id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    if b.published && !matches!(status.as_str(), "approved" | "open") {
        return Err(KernelError::Conflict(format!(
            "the requisition is {status}; only approved or open requisitions can be published"
        ))
        .into());
    }
    let description = optional_text("description", b.description, 6000)?;
    let location = optional_text("location", b.location, 120)?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    sqlx::query(
        "UPDATE people.requisition SET careers_published = $3, public_description = $4, public_location = $5, \
         closes_on = $6, updated_at = now() WHERE id = $1 AND tenant_id = $2",
    )
    .bind(rid)
    .bind(t.as_uuid())
    .bind(b.published)
    .bind(&description)
    .bind(&location)
    .bind(b.closes_on)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let a = actor(ctx);
    common::audit_only(
        &mut tx,
        &scope,
        "requisition",
        rid,
        "people.requisition.careers_listing",
        serde_json::json!({"published": b.published, "closesOn": b.closes_on}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(page(&st, &scope, t, inst_id).await?))
}

// ------------------------------------------------------------------ public

/// One opening on the public page (safe fields only).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HiringPublicOpening {
    pub id: Uuid,
    pub title: String,
    pub designation: String,
    /// `teaching`, `non_teaching`, …
    pub staff_category: String,
    pub employment_type: String,
    pub openings: i32,
    pub location: Option<String>,
    pub closes_on: Option<NaiveDate>,
    /// The description as escaped paragraphs and bullet lists.
    pub description_html: String,
}

/// The public careers page of an institution.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HiringPublicCareers {
    pub institution: String,
    pub intro_html: String,
    pub contact_email: Option<String>,
    pub openings: Vec<HiringPublicOpening>,
}

struct Site {
    tenant: Uuid,
    institution: Uuid,
    intro: Option<String>,
    email: Option<String>,
}

/// Finds an enabled careers page by slug across tenants (system context; 404 otherwise).
async fn site(st: &PeopleState, slug: &str) -> Result<Site, KernelError> {
    if !cr::is_valid_slug(slug) {
        return Err(KernelError::NotFound {
            entity: "careers page",
        });
    }
    let pool = st.pool.clone();
    let slug = slug.to_owned();
    let row: Option<(Uuid, Uuid, Option<String>, Option<String>)> =
        sutra_kernel::db_context::system(async move {
            sqlx::query_as(
                "SELECT tenant_id, institution_id, careers_intro, careers_contact_email FROM people.hiring_settings \
                 WHERE careers_slug = $1 AND careers_enabled",
            )
            .bind(slug)
            .fetch_optional(&pool)
            .await
        })
        .await
        .map_err(map_db_error)?;
    let (tenant, institution, intro, email) = row.ok_or(KernelError::NotFound {
        entity: "careers page",
    })?;
    Ok(Site {
        tenant,
        institution,
        intro,
        email,
    })
}

/// Open, published requisitions of an institution's careers page — **public, no session**
/// (safe fields only; 404 for an unknown or disabled page).
#[utoipa::path(get, path = "/api/v1/people/public/careers/{id}", operation_id = "people_recruitment_careers_public", tag = "people-hr",
    params(("id" = String, Path, description = "careers page slug")),
    responses((status = 200, body = HiringPublicCareers), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn careers_public(
    State(st): State<PeopleState>,
    Path(slug): Path<String>,
) -> ApiResult<Json<HiringPublicCareers>> {
    let s = site(&st, &slug).await?;
    let scope = ExecutionScope::institution(TenantId(s.tenant), InstitutionId(s.institution));
    let out = sutra_kernel::db_context::scoped(scope, async move {
        let inst = require_institution(&st, &scope, InstitutionId(s.institution)).await?;
        let today = crate::shared::time::business_today(&st, &scope, InstitutionId(s.institution)).await?;
        let rows = sqlx::query(
            "SELECT id, title, designation, staff_category, employment_type, openings, public_location, closes_on, \
             public_description FROM people.requisition WHERE tenant_id = $1 AND institution_id = $2 \
             AND status = 'open' AND careers_published AND (closes_on IS NULL OR closes_on >= $3) \
             ORDER BY created_at DESC LIMIT 100",
        )
        .bind(s.tenant)
        .bind(s.institution)
        .bind(today)
        .fetch_all(&st.pool)
        .await
        .map_err(map_db_error)?;
        let openings = rows
            .iter()
            .map(|r| {
                let desc: Option<String> = r.try_get("public_description")?;
                Ok(HiringPublicOpening {
                    id: r.try_get("id")?,
                    title: r.try_get("title")?,
                    designation: r.try_get("designation")?,
                    staff_category: r.try_get("staff_category")?,
                    employment_type: r.try_get("employment_type")?,
                    openings: r.try_get("openings")?,
                    location: r.try_get("public_location")?,
                    closes_on: r.try_get("closes_on")?,
                    description_html: cr::description_html(desc.as_deref().unwrap_or("")),
                })
            })
            .collect::<Result<Vec<_>, sqlx::Error>>()
            .map_err(map_db_error)?;
        Ok::<_, KernelError>(HiringPublicCareers {
            institution: inst.name,
            intro_html: cr::description_html(s.intro.as_deref().unwrap_or("")),
            contact_email: s.email,
            openings,
        })
    })
    .await?;
    Ok(Json(out))
}

/// The public apply form.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HiringCareersApplication {
    pub name: String,
    pub email: String,
    pub phone: Option<String>,
    /// A CV / profile link (http or https).
    pub link: Option<String>,
    pub note: Option<String>,
    /// Leave empty (hidden from people; bots fill it).
    pub website: Option<String>,
}

/// The apply receipt (the same for every accepted, duplicate or filtered submission).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HiringCareersReceipt {
    pub received: bool,
}

/// Apply to an open, published requisition — **public, no session**. 202 on success (also for
/// a repeat e-mail or a filled honeypot, which store nothing); 413 over 16 KB; 422 on bad fields;
/// 429 past the daily cap of the requisition.
#[utoipa::path(post, path = "/api/v1/people/public/careers/{id}/requisitions/{requisition_id}/apply", operation_id = "people_recruitment_careers_apply", tag = "people-hr",
    params(("id" = String, Path, description = "careers page slug"), ("requisition_id" = String, Path, format = Uuid)),
    request_body = HiringCareersApplication,
    responses((status = 202, body = HiringCareersReceipt), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn careers_apply(
    State(st): State<PeopleState>,
    Path((slug, rid)): Path<(String, Uuid)>,
    body: Bytes,
) -> ApiResult<Response> {
    if body.len() > cr::MAX_BODY {
        return Ok(Problem::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload-too-large",
            "an application is at most 16 KB",
        )
        .into_response());
    }
    let v: Value = serde_json::from_slice(&body)
        .map_err(|_| KernelError::BadRequest("the body must be a JSON object".into()))?;
    let text = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
    let app = Application {
        name: text("name").unwrap_or_default(),
        email: text("email").unwrap_or_default(),
        phone: text("phone"),
        link: text("link"),
        note: text("note"),
        website: text("website"),
    };
    let received = (
        StatusCode::ACCEPTED,
        Json(HiringCareersReceipt { received: true }),
    )
        .into_response();
    let clean = match cr::check_application(&app) {
        Checked::Spam => {
            tracing::info!(slug = %slug, "careers honeypot filled; application dropped");
            return Ok(received);
        }
        Checked::Invalid(bad) => {
            return Err(KernelError::Validation(
                bad.into_iter()
                    .map(|(f, m)| sutra_kernel::error::FieldViolation::new(f, "invalid", m))
                    .collect(),
            )
            .into());
        }
        Checked::Ok(c) => c,
    };
    let s = site(&st, &slug).await?;
    let scope = ExecutionScope::institution(TenantId(s.tenant), InstitutionId(s.institution));
    let capped = sutra_kernel::db_context::scoped(scope, async move {
        let inst = InstitutionId(s.institution);
        let today = crate::shared::time::business_today(&st, &scope, inst).await?;
        let a = Actor {
            id: Some(PUBLIC_ACTOR.into()),
            correlation_id: sutra_web::request_id::current_correlation_id(),
        };
        let mut tx = st.pool.begin().await.map_err(map_db_error)?;
        let open: Option<i32> = sqlx::query_scalar(
            "SELECT 1 FROM people.requisition WHERE id = $1 AND tenant_id = $2 AND institution_id = $3 \
             AND status = 'open' AND careers_published AND (closes_on IS NULL OR closes_on >= $4) FOR UPDATE",
        )
        .bind(rid)
        .bind(s.tenant)
        .bind(s.institution)
        .bind(today)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_db_error)?;
        if open.is_none() {
            return Err(KernelError::NotFound { entity: "opening" });
        }
        let today_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM people.candidate WHERE requisition_id = $1 AND source = 'Careers page' \
             AND created_at >= now() - interval '1 day'",
        )
        .bind(rid)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
        if today_count >= DAILY_APPLICATION_CAP {
            return Ok(true);
        }
        let dup: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM people.candidate WHERE requisition_id = $1 AND lower(email) = $2)",
        )
        .bind(rid)
        .bind(&clean.email)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
        if dup {
            return Ok(false);
        }
        let cid: Uuid = sqlx::query_scalar(
            "INSERT INTO people.candidate (tenant_id, institution_id, requisition_id, given_name, family_name, email, \
             phone, source, notes) VALUES ($1, $2, $3, $4, $5, $6, $7, 'Careers page', $8) RETURNING id",
        )
        .bind(s.tenant)
        .bind(s.institution)
        .bind(rid)
        .bind(&clean.given_name)
        .bind(&clean.family_name)
        .bind(&clean.email)
        .bind(&clean.phone)
        .bind(&clean.notes)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
        history(
            &mut tx,
            TenantId(s.tenant),
            inst,
            "candidate",
            cid,
            None,
            "applied",
            "sutraerp.people.candidate.created",
            None,
            None,
            &a,
        )
        .await?;
        common::audit_only(
            &mut tx,
            &scope,
            "candidate",
            cid,
            "people.candidate.public_apply",
            serde_json::json!({"requisitionId": rid, "via": "careers page"}),
            &a,
        )
        .await?;
        tx.commit().await.map_err(map_db_error)?;
        Ok::<bool, KernelError>(false)
    })
    .await?;
    if capped {
        return Ok(Problem::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate-limited",
            "this opening has received many applications today; please try again tomorrow",
        )
        .into_response());
    }
    Ok(received)
}

pub fn router(state: PeopleState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(careers_page_get, careers_page_put))
        .routes(routes!(careers_listing_put))
        .routes(routes!(careers_public))
        .routes(routes!(careers_apply))
        .with_state(state)
}
