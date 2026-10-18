//! **ERP_EXTENSIONS 5A** — offer letters and onboarding checklists over candidates (rules in
//! [`super::offers`], ported from HeyPuter/everything hiring, MIT). Tag `people-hr`; operation
//! ids `people_recruitment_offer_*`, `people_recruitment_offer_settings_*` and
//! `people_onboarding_checklist_*`.
//!
//! * An offer is drafted on a shortlisted / interviewed / offered candidate with a monthly CTC
//!   breakup in ₹ (`people.staff.manage`), submitted for approval, decided by someone else with
//!   `people.approve` (separation of duties: drafter / submitter ≠ approver, matrix row
//!   `people_recruitment_offer_decide`), sent (the letter text is snapshotted and the candidate
//!   moves to `offered`), then accepted (candidate → `offer_accepted` with the joining date and
//!   designation, so the existing onboarding case can open), declined, expired or withdrawn.
//! * Opening the existing onboarding case stamps its checklist from the institution's templates
//!   (requisition → department → general → the shipped default); tasks come due at offsets
//!   from the case's joining date.
#![allow(clippy::too_many_arguments)]

use super::offers::{
    self as of, Ctc, OfferStatus, Template, DEFAULT_OFFER_TEMPLATE, OFFER_TEMPLATE_VARIABLES,
};
use super::recruitment::{candidate_can_transition, load_candidate, load_requisition};
use crate::shared::approval::{ApprovalMode, AUTO_APPROVER};
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, with_etag};
use crate::shared::numbering::next;
use crate::shared::scope::institution_scope;
use crate::shared::{Actor, PeopleState};
use crate::workforce::common::{
    self, check_version, clean, expected, history, invalid, optional_text, required_reason,
    required_text, valid_code, Decision, DecisionBody, ReasonBody,
};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use sutra_contracts::people::status::CandidateStatus;
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// Offer number kind (`OFR-000001`).
pub const OFFER_KIND: (&str, &str) = ("offer", "OFR");

// ------------------------------------------------------------------ settings

/// Offer and onboarding settings of an institution.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HiringOfferSettings {
    /// The institution's letter template; null = the default.
    pub offer_template: Option<String>,
    /// The default letter (read-only).
    #[serde(default)]
    pub default_offer_template: String,
    /// Placeholders a template can use (read-only).
    #[serde(default)]
    pub template_variables: Vec<String>,
    /// Checklist templates; empty = the shipped default.
    pub onboarding_templates: Vec<Template>,
    /// The shipped default checklist (read-only).
    #[serde(default = "of::default_template")]
    pub default_onboarding_template: Template,
    /// Days in one stage after which a candidate counts as stuck.
    pub stale_after_days: i32,
    /// Months after which archived candidates are offered for erasure (0 = keep).
    pub retention_months: i32,
}

/// The stored settings row.
#[derive(Debug, Clone, Default)]
pub struct SettingsRow {
    pub offer_template: Option<String>,
    pub onboarding_templates: Value,
    pub stale_after_days: i32,
    pub retention_months: i32,
}

pub async fn settings_row(
    conn: &mut PgConnection,
    t: TenantId,
    inst: Uuid,
) -> Result<SettingsRow, KernelError> {
    let row = sqlx::query(
        "SELECT offer_template, onboarding_templates, stale_after_days, retention_months \
         FROM people.hiring_settings WHERE tenant_id = $1 AND institution_id = $2",
    )
    .bind(t.as_uuid())
    .bind(inst)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(match row {
        None => SettingsRow {
            offer_template: None,
            onboarding_templates: Value::Array(vec![]),
            stale_after_days: 14,
            retention_months: 0,
        },
        Some(r) => SettingsRow {
            offer_template: r.try_get("offer_template").map_err(map_db_error)?,
            onboarding_templates: r.try_get("onboarding_templates").map_err(map_db_error)?,
            stale_after_days: r.try_get("stale_after_days").map_err(map_db_error)?,
            retention_months: r.try_get("retention_months").map_err(map_db_error)?,
        },
    })
}

fn settings_view(s: SettingsRow) -> HiringOfferSettings {
    HiringOfferSettings {
        offer_template: s.offer_template,
        default_offer_template: DEFAULT_OFFER_TEMPLATE.to_owned(),
        template_variables: OFFER_TEMPLATE_VARIABLES
            .iter()
            .map(|v| (*v).to_owned())
            .collect(),
        onboarding_templates: of::normalize_templates(&s.onboarding_templates),
        default_onboarding_template: of::default_template(),
        stale_after_days: s.stale_after_days,
        retention_months: s.retention_months,
    }
}

/// Offer / onboarding settings (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/institutions/{id}/offer-settings", operation_id = "people_recruitment_offer_settings_get", tag = "people-hr",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = HiringOfferSettings), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn settings_get(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<HiringOfferSettings>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let inst = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let s = settings_row(&mut conn, ctx.scope.tenant_id, id).await?;
    Ok(Json(settings_view(s)))
}

/// Save offer / onboarding settings (`people.staff.manage`). Templates are cleaned as
/// Everything does (safe unique ids, named, tasks with titles, offsets within a year); a blank
/// letter template restores the default.
#[utoipa::path(put, path = "/api/v1/people/institutions/{id}/offer-settings", operation_id = "people_recruitment_offer_settings_put", tag = "people-hr",
    params(("id" = String, Path, format = Uuid)), request_body = HiringOfferSettings, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = HiringOfferSettings), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn settings_put(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(b): Json<Value>,
) -> ApiResult<Json<HiringOfferSettings>> {
    let ctx = auth.require(Permission::StaffManage)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let template = b
        .get("offerTemplate")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    if template
        .as_deref()
        .is_some_and(|s| s.chars().count() > 20_000)
    {
        return Err(invalid("offerTemplate", "too_long", "at most 20,000 characters").into());
    }
    let templates = of::normalize_templates(b.get("onboardingTemplates").unwrap_or(&Value::Null));
    let int = |k: &str, d: i64, lo: i64, hi: i64| -> Result<i32, KernelError> {
        let v = b.get(k).and_then(Value::as_i64).unwrap_or(d);
        if !(lo..=hi).contains(&v) {
            return Err(invalid(k, "out_of_range", &format!("{lo} to {hi}")));
        }
        Ok(v as i32)
    };
    let stale = int("staleAfterDays", 14, 1, 365)?;
    let retention = int("retentionMonths", 0, 0, 120)?;
    let t = ctx.scope.tenant_id;
    let tjson = serde_json::to_value(&templates).map_err(KernelError::internal)?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    sqlx::query(
        "INSERT INTO people.hiring_settings (tenant_id, institution_id, offer_template, onboarding_templates, \
         stale_after_days, retention_months) VALUES ($1, $2, $3, $4, $5, $6) \
         ON CONFLICT (tenant_id, institution_id) DO UPDATE SET offer_template = EXCLUDED.offer_template, \
         onboarding_templates = EXCLUDED.onboarding_templates, stale_after_days = EXCLUDED.stale_after_days, \
         retention_months = EXCLUDED.retention_months, version = people.hiring_settings.version + 1, updated_at = now()",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(&template)
    .bind(&tjson)
    .bind(stale)
    .bind(retention)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let a = actor(ctx);
    common::audit_only(
        &mut tx,
        &scope,
        "institution",
        id,
        "people.offer_settings.update",
        serde_json::json!({"customLetter": template.is_some(), "templates": templates.len(),
            "staleAfterDays": stale, "retentionMonths": retention}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(settings_view(SettingsRow {
        offer_template: template,
        onboarding_templates: tjson,
        stale_after_days: stale,
        retention_months: retention,
    })))
}

// ------------------------------------------------------------------ offer views

/// An offer letter on a candidate.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HiringOfferView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub candidate_id: Uuid,
    pub candidate_name: String,
    pub requisition_id: Uuid,
    pub requisition_number: String,
    #[schema(example = "OFR-000001")]
    pub number: String,
    pub designation: String,
    pub grade: Option<String>,
    pub department: Option<String>,
    pub joining_date: NaiveDate,
    pub expires_on: Option<NaiveDate>,
    pub probation_months: Option<i32>,
    /// Monthly breakup (₹).
    pub ctc: Ctc,
    #[schema(value_type = String)]
    pub ctc_monthly: Decimal,
    #[schema(value_type = String)]
    pub ctc_annual: Decimal,
    #[schema(value_type = Option<String>)]
    pub joining_bonus: Option<Decimal>,
    pub notes: Option<String>,
    pub status: OfferStatus,
    pub status_reason: Option<String>,
    pub drafted_by: Option<String>,
    pub submitted_by: Option<String>,
    pub decided_by: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
    pub sent_at: Option<DateTime<Utc>>,
    pub accepted_by_name: Option<String>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HiringOfferList {
    pub items: Vec<HiringOfferView>,
}

const OF_SELECT: &str = "SELECT o.id, o.institution_id, o.candidate_id, c.given_name, c.family_name, \
    o.requisition_id, r.requisition_number, o.offer_number, o.designation, o.grade, o.department, \
    o.joining_date, o.expires_on, o.probation_months, o.basic, o.hra, o.da, o.allowances, o.employer_pf, \
    o.joining_bonus, o.notes, o.status, o.status_reason, o.letter_text, o.drafted_by, o.submitted_by, \
    o.decided_by, o.decided_at, o.sent_at, o.accepted_by_name, o.resolved_at, o.version \
    FROM people.candidate_offer o JOIN people.candidate c ON c.id = o.candidate_id \
    JOIN people.requisition r ON r.id = o.requisition_id";

struct Loaded {
    view: HiringOfferView,
    letter_text: Option<String>,
}

fn name_of(given: String, family: Option<String>) -> String {
    match family {
        Some(f) => format!("{given} {f}"),
        None => given,
    }
}

fn offer_from_row(r: &PgRow) -> Result<Loaded, sqlx::Error> {
    let status: String = r.try_get("status")?;
    let ctc = Ctc {
        basic: r.try_get("basic")?,
        hra: r.try_get("hra")?,
        da: r.try_get("da")?,
        allowances: r.try_get("allowances")?,
        employer_pf: r.try_get("employer_pf")?,
    };
    Ok(Loaded {
        letter_text: r.try_get("letter_text")?,
        view: HiringOfferView {
            id: r.try_get("id")?,
            institution_id: r.try_get("institution_id")?,
            candidate_id: r.try_get("candidate_id")?,
            candidate_name: name_of(r.try_get("given_name")?, r.try_get("family_name")?),
            requisition_id: r.try_get("requisition_id")?,
            requisition_number: r.try_get("requisition_number")?,
            number: r.try_get("offer_number")?,
            designation: r.try_get("designation")?,
            grade: r.try_get("grade")?,
            department: r.try_get("department")?,
            joining_date: r.try_get("joining_date")?,
            expires_on: r.try_get("expires_on")?,
            probation_months: r.try_get("probation_months")?,
            ctc_monthly: ctc.monthly(),
            ctc_annual: ctc.annual(),
            ctc,
            joining_bonus: r.try_get("joining_bonus")?,
            notes: r.try_get("notes")?,
            status: OfferStatus::parse(&status).ok_or_else(|| {
                sqlx::Error::Decode(format!("unknown offer status {status}").into())
            })?,
            status_reason: r.try_get("status_reason")?,
            drafted_by: r.try_get("drafted_by")?,
            submitted_by: r.try_get("submitted_by")?,
            decided_by: r.try_get("decided_by")?,
            decided_at: r.try_get("decided_at")?,
            sent_at: r.try_get("sent_at")?,
            accepted_by_name: r.try_get("accepted_by_name")?,
            resolved_at: r.try_get("resolved_at")?,
            version: r.try_get("version")?,
        },
    })
}

async fn load_offer(
    conn: &mut PgConnection,
    t: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<Loaded, KernelError> {
    let lock_sql = if lock { " FOR UPDATE OF o" } else { "" };
    let row = sqlx::query(&format!(
        "{OF_SELECT} WHERE o.id = $1 AND o.tenant_id = $2{lock_sql}"
    ))
    .bind(id)
    .bind(t.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound { entity: "offer" })?;
    offer_from_row(&row).map_err(map_db_error)
}

/// Offers of a candidate, newest first (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/candidates/{id}/offers", operation_id = "people_recruitment_offer_list", tag = "people-hr",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = HiringOfferList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn offer_list(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(cid): Path<Uuid>,
) -> ApiResult<Json<HiringOfferList>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let c = load_candidate(&mut conn, t, cid, false).await?;
    let inst = InstitutionId(c.institution_id);
    require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    let rows = sqlx::query(&format!(
        "{OF_SELECT} WHERE o.candidate_id = $1 AND o.tenant_id = $2 ORDER BY o.created_at DESC"
    ))
    .bind(cid)
    .bind(t.as_uuid())
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let items = rows
        .iter()
        .map(|r| offer_from_row(r).map(|l| l.view))
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)?;
    Ok(Json(HiringOfferList { items }))
}

#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct OfferStatusQuery {
    /// One offer status (`draft`, `pending_approval`, `approved`, `sent`, …).
    pub status: Option<String>,
}

/// Offers of an institution, newest first (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/institutions/{id}/offers", operation_id = "people_recruitment_offer_institution_list", tag = "people-hr",
    params(("id" = String, Path, format = Uuid), OfferStatusQuery), security(("session_cookie" = [])),
    responses((status = 200, body = HiringOfferList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn offer_institution_list(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<OfferStatusQuery>,
) -> ApiResult<Json<HiringOfferList>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let inst = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    let status = match q.status.as_deref() {
        None | Some("") => None,
        Some(s) => Some(
            OfferStatus::parse(s)
                .ok_or_else(|| invalid("status", "invalid", "unknown offer status"))?,
        ),
    };
    let rows = sqlx::query(&format!(
        "{OF_SELECT} WHERE o.tenant_id = $1 AND o.institution_id = $2 \
         AND ($3::text IS NULL OR o.status = $3) ORDER BY o.created_at DESC LIMIT 500"
    ))
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .bind(status.map(OfferStatus::as_str))
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let items = rows
        .iter()
        .map(|r| offer_from_row(r).map(|l| l.view))
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)?;
    Ok(Json(HiringOfferList { items }))
}

/// One offer (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/offers/{id}", operation_id = "people_recruitment_offer_get", tag = "people-hr",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = HiringOfferView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn offer_get(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let o = load_offer(&mut conn, ctx.scope.tenant_id, id, false)
        .await?
        .view;
    let inst = InstitutionId(o.institution_id);
    require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    Ok(with_etag(StatusCode::OK, &o, Version(o.version)))
}

// ------------------------------------------------------------------ draft / edit

/// Offer terms (create and edit a draft).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HiringOfferInput {
    /// Defaults to the requisition's designation.
    pub designation: Option<String>,
    /// Pay level / grade as printed on the letter (defaults to the requisition's grade).
    pub grade: Option<String>,
    /// Department named in the letter.
    pub department: Option<String>,
    pub joining_date: NaiveDate,
    /// The offer lapses after this day (on or before the joining date).
    pub expires_on: Option<NaiveDate>,
    /// Probation in months (0–36).
    pub probation_months: Option<i32>,
    /// Monthly breakup. A missing `employerPf` is 12 % of basic + DA up to the ₹15,000 ceiling.
    pub ctc: CtcInput,
    #[schema(value_type = Option<String>)]
    pub joining_bonus: Option<Decimal>,
    pub notes: Option<String>,
}

/// Monthly CTC breakup input (₹, numbers or decimal strings).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CtcInput {
    #[schema(value_type = String)]
    pub basic: Decimal,
    #[schema(value_type = Option<String>)]
    pub hra: Option<Decimal>,
    #[schema(value_type = Option<String>)]
    pub da: Option<Decimal>,
    #[schema(value_type = Option<String>)]
    pub allowances: Option<Decimal>,
    #[schema(value_type = Option<String>)]
    pub employer_pf: Option<Decimal>,
}

struct Terms {
    designation: String,
    grade: Option<String>,
    department: Option<String>,
    joining: NaiveDate,
    expires: Option<NaiveDate>,
    probation: Option<i32>,
    ctc: Ctc,
    bonus: Option<Decimal>,
    notes: Option<String>,
}

fn problems_to_error(p: Vec<of::Problem>) -> Result<(), KernelError> {
    if p.is_empty() {
        return Ok(());
    }
    Err(KernelError::Validation(
        p.into_iter()
            .map(|x| sutra_kernel::error::FieldViolation::new(x.field, x.code, x.message))
            .collect(),
    ))
}

fn clean_terms(
    b: HiringOfferInput,
    default_designation: &str,
    default_grade: Option<&str>,
) -> Result<Terms, KernelError> {
    let r2 = |d: Decimal| d.round_dp(2);
    let basic = r2(b.ctc.basic);
    let da = r2(b.ctc.da.unwrap_or_default());
    let ctc = Ctc {
        basic,
        hra: r2(b.ctc.hra.unwrap_or_default()),
        da,
        allowances: r2(b.ctc.allowances.unwrap_or_default()),
        employer_pf: r2(b
            .ctc
            .employer_pf
            .unwrap_or_else(|| of::suggested_employer_pf(basic, da))),
    };
    let bonus = b
        .joining_bonus
        .map(r2)
        .filter(|v| !v.is_zero() || v.is_sign_negative());
    let annual = rust_decimal::prelude::ToPrimitive::to_f64(&ctc.annual()).unwrap_or(0.0);
    let joining = b.joining_date.format("%Y-%m-%d").to_string();
    let expires = b.expires_on.map(|e| e.format("%Y-%m-%d").to_string());
    let mut problems = of::validate_offer(
        if basic > Decimal::ZERO { annual } else { 0.0 },
        bonus.map(|v| rust_decimal::prelude::ToPrimitive::to_f64(&v).unwrap_or(-1.0)),
        &joining,
        expires.as_deref(),
    );
    problems.extend(of::validate_ctc(&ctc));
    problems_to_error(problems)?;
    if b.probation_months.is_some_and(|m| !(0..=36).contains(&m)) {
        return Err(invalid("probationMonths", "out_of_range", "0 to 36 months"));
    }
    let designation = match b.designation {
        Some(d) => required_text("designation", &d, 120)?,
        None => default_designation.to_owned(),
    };
    Ok(Terms {
        designation,
        grade: optional_text("grade", b.grade, 60)?.or_else(|| default_grade.map(str::to_owned)),
        department: optional_text("department", b.department, 120)?,
        joining: b.joining_date,
        expires: b.expires_on,
        probation: b.probation_months,
        ctc,
        bonus,
        notes: optional_text("notes", b.notes, 2000)?,
    })
}

/// Draft an offer for a shortlisted, interviewed or offered candidate (`people.staff.manage`;
/// Idempotency-Key). One offer in play per candidate (409).
#[utoipa::path(post, path = "/api/v1/people/candidates/{id}/offers", operation_id = "people_recruitment_offer_create", tag = "people-hr",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)),
    request_body = HiringOfferInput, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = HiringOfferView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn offer_create(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(cid): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<HiringOfferInput>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    require_idempotency_key(&headers)?;
    let t = ctx.scope.tenant_id;
    let pre = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_candidate(&mut conn, t, cid, false).await?
    };
    let inst = InstitutionId(pre.institution_id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let c = load_candidate(&mut tx, t, cid, true).await?;
    if !matches!(
        c.status,
        CandidateStatus::Shortlisted | CandidateStatus::Interviewed | CandidateStatus::Offered
    ) {
        return Err(KernelError::Conflict(format!(
            "candidate {} is {}; offers go to shortlisted or interviewed candidates",
            c.email, c.status
        ))
        .into());
    }
    let r = load_requisition(&mut tx, t, c.requisition_id, false).await?;
    let terms = clean_terms(b, &r.designation, r.grade.as_deref())?;
    let open: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM people.candidate_offer WHERE candidate_id = $1 \
         AND status IN ('draft', 'pending_approval', 'approved', 'sent'))",
    )
    .bind(cid)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if open {
        return Err(KernelError::Conflict(format!(
            "candidate {} already has an offer in play; withdraw it first",
            c.email
        ))
        .into());
    }
    let number = next(&mut tx, t, inst, OFFER_KIND).await?;
    let oid: Uuid = sqlx::query_scalar(
        "INSERT INTO people.candidate_offer (tenant_id, institution_id, candidate_id, requisition_id, \
         offer_number, designation, grade, department, joining_date, expires_on, probation_months, \
         basic, hra, da, allowances, employer_pf, joining_bonus, notes, drafted_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(c.institution_id)
    .bind(cid)
    .bind(r.id)
    .bind(&number)
    .bind(&terms.designation)
    .bind(&terms.grade)
    .bind(&terms.department)
    .bind(terms.joining)
    .bind(terms.expires)
    .bind(terms.probation)
    .bind(terms.ctc.basic)
    .bind(terms.ctc.hra)
    .bind(terms.ctc.da)
    .bind(terms.ctc.allowances)
    .bind(terms.ctc.employer_pf)
    .bind(terms.bonus)
    .bind(&terms.notes)
    .bind(a.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    history(
        &mut tx,
        t,
        inst,
        "candidate_offer",
        oid,
        None,
        "draft",
        "sutraerp.people.candidate_offer.created",
        None,
        None,
        &a,
    )
    .await?;
    common::audit_only(
        &mut tx,
        &scope,
        "candidate_offer",
        oid,
        "people.offer.create",
        serde_json::json!({"number": number, "candidateId": cid, "ctcAnnual": terms.ctc.annual().to_string()}),
        &a,
    )
    .await?;
    let o = load_offer(&mut tx, t, oid, false).await?.view;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::CREATED, &o, Version(o.version)))
}

/// Edit a draft offer (`people.staff.manage`; If-Match).
#[utoipa::path(put, path = "/api/v1/people/offers/{id}", operation_id = "people_recruitment_offer_update", tag = "people-hr",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the offer")),
    request_body = HiringOfferInput, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = HiringOfferView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn offer_update(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<HiringOfferInput>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let (scope, inst) = offer_scope(&st, ctx, id).await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let o = load_offer(&mut tx, t, id, true).await?.view;
    check_version(v, o.version)?;
    if o.status != OfferStatus::Draft {
        return Err(KernelError::Conflict(format!(
            "offer {} is {}; only a draft can be edited",
            o.number,
            o.status.as_str()
        ))
        .into());
    }
    let r = load_requisition(&mut tx, t, o.requisition_id, false).await?;
    let terms = clean_terms(b, &r.designation, r.grade.as_deref())?;
    sqlx::query(
        "UPDATE people.candidate_offer SET designation = $2, grade = $3, department = $4, joining_date = $5, \
         expires_on = $6, probation_months = $7, basic = $8, hra = $9, da = $10, allowances = $11, \
         employer_pf = $12, joining_bonus = $13, notes = $14, version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(&terms.designation)
    .bind(&terms.grade)
    .bind(&terms.department)
    .bind(terms.joining)
    .bind(terms.expires)
    .bind(terms.probation)
    .bind(terms.ctc.basic)
    .bind(terms.ctc.hra)
    .bind(terms.ctc.da)
    .bind(terms.ctc.allowances)
    .bind(terms.ctc.employer_pf)
    .bind(terms.bonus)
    .bind(&terms.notes)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    common::audit_only(
        &mut tx,
        &scope,
        "candidate_offer",
        id,
        "people.offer.update",
        serde_json::json!({"number": o.number, "ctcAnnual": terms.ctc.annual().to_string()}),
        &a,
    )
    .await?;
    let _ = inst;
    let out = load_offer(&mut tx, t, id, false).await?.view;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &out, Version(out.version)))
}

async fn offer_scope(
    st: &PeopleState,
    ctx: &AuthContext,
    id: Uuid,
) -> Result<(ExecutionScope, InstitutionId), KernelError> {
    let pre = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_offer(&mut conn, ctx.scope.tenant_id, id, false)
            .await?
            .view
    };
    let inst = InstitutionId(pre.institution_id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(st, &scope, inst).await?;
    Ok((scope, inst))
}

// ------------------------------------------------------------------ status commands

/// Moves a locked offer (history + audit + version), stamping the columns of the move.
async fn move_offer(
    conn: &mut PgConnection,
    t: TenantId,
    scope: &ExecutionScope,
    o: &HiringOfferView,
    to: OfferStatus,
    reason: Option<&str>,
    a: &Actor,
    action: &str,
) -> Result<(), KernelError> {
    if !of::can_transition(o.status, to) {
        return Err(common::illegal(
            "offer",
            &o.number,
            o.status.as_str(),
            to.as_str(),
        ));
    }
    let who = a.id.as_deref();
    sqlx::query(
        "UPDATE people.candidate_offer SET status = $2, status_reason = $3, \
         submitted_by = CASE WHEN $2 = 'pending_approval' THEN $4 ELSE submitted_by END, \
         decided_by = CASE WHEN $2 = 'approved' OR ($2 = 'draft' AND status = 'pending_approval') THEN $4 \
                           WHEN $2 = 'pending_approval' THEN NULL ELSE decided_by END, \
         decided_at = CASE WHEN $2 = 'approved' OR ($2 = 'draft' AND status = 'pending_approval') THEN now() \
                           WHEN $2 = 'pending_approval' THEN NULL ELSE decided_at END, \
         sent_at = CASE WHEN $2 = 'sent' THEN now() ELSE sent_at END, \
         resolved_at = CASE WHEN $2 IN ('accepted', 'declined', 'expired', 'withdrawn') THEN now() ELSE resolved_at END, \
         version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(o.id)
    .bind(to.as_str())
    .bind(reason)
    .bind(who)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    history(
        conn,
        t,
        InstitutionId(o.institution_id),
        "candidate_offer",
        o.id,
        Some(o.status.as_str()),
        to.as_str(),
        "sutraerp.people.candidate_offer.status",
        reason,
        None,
        a,
    )
    .await?;
    common::audit_only(
        conn,
        scope,
        "candidate_offer",
        o.id,
        action,
        serde_json::json!({"number": o.number, "from": o.status, "to": to, "reason": reason}),
        a,
    )
    .await
}

/// Submit a draft for approval (`people.staff.manage`; If-Match). Under dev / test
/// auto-approval (`SUTRA_APPROVAL_MODE=auto`) it is approved at once by the system approver.
#[utoipa::path(post, path = "/api/v1/people/offers/{id}/submit", operation_id = "people_recruitment_offer_submit", tag = "people-hr",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the offer")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = HiringOfferView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn offer_submit(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let (scope, _) = offer_scope(&st, ctx, id).await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let o = load_offer(&mut tx, t, id, true).await?.view;
    check_version(v, o.version)?;
    move_offer(
        &mut tx,
        t,
        &scope,
        &o,
        OfferStatus::PendingApproval,
        None,
        &a,
        "people.offer.submit",
    )
    .await?;
    if st.approvals == ApprovalMode::AutoApprove {
        let o = load_offer(&mut tx, t, id, true).await?.view;
        let system = Actor {
            id: Some(AUTO_APPROVER.into()),
            correlation_id: a.correlation_id.clone(),
        };
        move_offer(
            &mut tx,
            t,
            &scope,
            &o,
            OfferStatus::Approved,
            None,
            &system,
            "people.offer.approve",
        )
        .await?;
    }
    let out = load_offer(&mut tx, t, id, false).await?.view;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &out, Version(out.version)))
}

/// Approve or send back an offer awaiting approval (`people.approve`; If-Match). Separation of
/// duties: whoever drafted or submitted the offer cannot decide it (403).
#[utoipa::path(post, path = "/api/v1/people/offers/{id}/decision", operation_id = "people_recruitment_offer_decide", tag = "people-hr",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the offer")),
    request_body = DecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = HiringOfferView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn offer_decide(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<DecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleApprove)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let (scope, _) = offer_scope(&st, ctx, id).await?;
    let reason = match b.decision {
        Decision::Approve => clean(b.reason),
        Decision::Reject => Some(required_reason(b.reason)?),
    };
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let o = load_offer(&mut tx, t, id, true).await?.view;
    check_version(v, o.version)?;
    if o.status != OfferStatus::PendingApproval {
        return Err(KernelError::Conflict(format!(
            "offer {} is {}; only an offer awaiting approval can be decided",
            o.number,
            o.status.as_str()
        ))
        .into());
    }
    sutra_kernel::approval::ensure_independent_decider(
        a.id.as_deref(),
        [o.drafted_by.as_deref(), o.submitted_by.as_deref()]
            .into_iter()
            .flatten(),
        "offer",
    )?;
    let (to, action) = match b.decision {
        Decision::Approve => (OfferStatus::Approved, "people.offer.approve"),
        Decision::Reject => (OfferStatus::Draft, "people.offer.send_back"),
    };
    move_offer(&mut tx, t, &scope, &o, to, reason.as_deref(), &a, action).await?;
    let out = load_offer(&mut tx, t, id, false).await?.view;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &out, Version(out.version)))
}

/// Moves the locked candidate of an offer (history + audit), optionally stamping the offer terms.
async fn move_candidate(
    conn: &mut PgConnection,
    t: TenantId,
    scope: &ExecutionScope,
    cid: Uuid,
    to: CandidateStatus,
    reason: Option<&str>,
    terms: Option<&HiringOfferView>,
    a: &Actor,
) -> Result<(), KernelError> {
    let c = load_candidate(conn, t, cid, true).await?;
    if c.status == to {
        return Ok(());
    }
    if !candidate_can_transition(c.status, to) {
        return Err(common::illegal(
            "candidate",
            &c.email,
            c.status.as_str(),
            to.as_str(),
        ));
    }
    let grade = terms.and_then(|o| valid_code("grade", o.grade.clone()).ok().flatten());
    sqlx::query(
        "UPDATE people.candidate SET status = $2, status_reason = $3, \
         offered_designation = COALESCE($4, offered_designation), offered_grade = COALESCE($5, offered_grade), \
         proposed_joining_date = COALESCE($6, proposed_joining_date), version = version + 1, updated_at = now() \
         WHERE id = $1",
    )
    .bind(cid)
    .bind(to.as_str())
    .bind(reason)
    .bind(terms.map(|o| o.designation.clone()))
    .bind(grade)
    .bind(terms.map(|o| o.joining_date))
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    history(
        conn,
        t,
        InstitutionId(c.institution_id),
        "candidate",
        cid,
        Some(c.status.as_str()),
        to.as_str(),
        "sutraerp.people.candidate.status",
        reason,
        None,
        a,
    )
    .await?;
    common::audit_only(
        conn,
        scope,
        "candidate",
        cid,
        "people.candidate.offer_flow",
        serde_json::json!({"from": c.status, "to": to, "offer": terms.map(|o| o.number.clone())}),
        a,
    )
    .await
}

/// The institution name and business "today" for letters.
async fn letter_context(
    st: &PeopleState,
    scope: &ExecutionScope,
    inst: InstitutionId,
) -> Result<(String, NaiveDate), KernelError> {
    let r = require_institution(st, scope, inst).await?;
    let today = crate::shared::time::business_today(st, scope, inst).await?;
    Ok((r.name, today))
}

fn render_letter(
    template: Option<&str>,
    o: &HiringOfferView,
    institution: &str,
    sent_on: Option<NaiveDate>,
    accepted_on: Option<NaiveDate>,
) -> String {
    let input = of::LetterInput {
        offer_number: &o.number,
        candidate_name: &o.candidate_name,
        designation: &o.designation,
        department: o.department.as_deref(),
        institution,
        grade: o.grade.as_deref(),
        joining_date: o.joining_date,
        probation_months: o.probation_months,
        ctc: &o.ctc,
        joining_bonus: o.joining_bonus,
        expires: o.expires_on,
        notes: o.notes.as_deref(),
        status: o.status,
        sent_on,
        accepted_by: o.accepted_by_name.as_deref(),
        accepted_on,
    };
    of::render_offer_letter(of::offer_template(template), &of::letter_vars(&input))
}

/// Send an approved offer (`people.staff.manage`; If-Match): snapshots the letter text and moves
/// the candidate to `offered`. 409 when the requisition is no longer open or the offer has
/// already lapsed.
#[utoipa::path(post, path = "/api/v1/people/offers/{id}/send", operation_id = "people_recruitment_offer_send", tag = "people-hr",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the offer")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = HiringOfferView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn offer_send(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let (scope, inst) = offer_scope(&st, ctx, id).await?;
    let (institution, today) = letter_context(&st, &scope, inst).await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let o = load_offer(&mut tx, t, id, true).await?.view;
    check_version(v, o.version)?;
    if o.expires_on.is_some_and(|e| e < today) {
        return Err(KernelError::Conflict(format!(
            "offer {} expired on {}; withdraw it and draft a new one",
            o.number,
            of::letter_date(o.expires_on.unwrap_or(today))
        ))
        .into());
    }
    let r = load_requisition(&mut tx, t, o.requisition_id, false).await?;
    if r.status != sutra_contracts::people::status::RequisitionStatus::Open {
        return Err(
            KernelError::Conflict(format!("requisition {} is {}", r.number, r.status)).into(),
        );
    }
    move_offer(
        &mut tx,
        t,
        &scope,
        &o,
        OfferStatus::Sent,
        None,
        &a,
        "people.offer.send",
    )
    .await?;
    move_candidate(
        &mut tx,
        t,
        &scope,
        o.candidate_id,
        CandidateStatus::Offered,
        None,
        Some(&o),
        &a,
    )
    .await?;
    let settings = settings_row(&mut tx, t, inst.as_uuid()).await?;
    let sent = load_offer(&mut tx, t, id, false).await?.view;
    let text = render_letter(
        settings.offer_template.as_deref(),
        &sent,
        &institution,
        Some(today),
        None,
    );
    sqlx::query("UPDATE people.candidate_offer SET letter_text = $2 WHERE id = $1")
        .bind(id)
        .bind(&text)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &sent, Version(sent.version)))
}

/// The candidate's acceptance.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HiringOfferAcceptBody {
    /// The name the candidate signed with (defaults to the candidate's name).
    pub accepted_by_name: Option<String>,
}

/// Record the candidate's acceptance (`people.staff.manage`; If-Match): the candidate moves to
/// `offer_accepted` with the offer's joining date and designation, ready for the onboarding
/// case. 409 when the requisition has no opening left or the offer has lapsed.
#[utoipa::path(post, path = "/api/v1/people/offers/{id}/accept", operation_id = "people_recruitment_offer_accept", tag = "people-hr",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the offer")),
    request_body = HiringOfferAcceptBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = HiringOfferView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn offer_accept(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<HiringOfferAcceptBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    let v = expected!(headers);
    let b = body.map(|b| b.0).unwrap_or_default();
    let t = ctx.scope.tenant_id;
    let (scope, inst) = offer_scope(&st, ctx, id).await?;
    let (institution, today) = letter_context(&st, &scope, inst).await?;
    let signer = optional_text("acceptedByName", b.accepted_by_name, 120)?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let o = load_offer(&mut tx, t, id, true).await?.view;
    check_version(v, o.version)?;
    if o.status == OfferStatus::Sent && o.expires_on.is_some_and(|e| e < today) {
        return Err(KernelError::Conflict(format!(
            "offer {} lapsed on {}",
            o.number,
            of::letter_date(o.expires_on.unwrap_or(today))
        ))
        .into());
    }
    let r = load_requisition(&mut tx, t, o.requisition_id, true).await?;
    let accepted: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM people.candidate WHERE requisition_id = $1 AND status = 'offer_accepted'",
    )
    .bind(r.id)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if accepted >= i64::from(r.openings) {
        return Err(KernelError::Conflict(format!(
            "requisition {} has no opening left ({} of {} accepted)",
            r.number, accepted, r.openings
        ))
        .into());
    }
    move_offer(
        &mut tx,
        t,
        &scope,
        &o,
        OfferStatus::Accepted,
        None,
        &a,
        "people.offer.accept",
    )
    .await?;
    let name = signer.unwrap_or_else(|| o.candidate_name.clone());
    sqlx::query("UPDATE people.candidate_offer SET accepted_by_name = $2 WHERE id = $1")
        .bind(id)
        .bind(&name)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    move_candidate(
        &mut tx,
        t,
        &scope,
        o.candidate_id,
        CandidateStatus::OfferAccepted,
        None,
        Some(&o),
        &a,
    )
    .await?;
    let settings = settings_row(&mut tx, t, inst.as_uuid()).await?;
    let out = load_offer(&mut tx, t, id, false).await?.view;
    let sent_on = out.sent_at.map(|s| s.date_naive());
    let text = render_letter(
        settings.offer_template.as_deref(),
        &out,
        &institution,
        sent_on,
        Some(today),
    );
    sqlx::query("UPDATE people.candidate_offer SET letter_text = $2 WHERE id = $1")
        .bind(id)
        .bind(&text)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &out, Version(out.version)))
}

macro_rules! offer_close {
    ($(#[$m:meta])* $fn:ident, $path:literal, $op:literal, $to:expr, $action:literal, $cand:expr) => {
        $(#[$m])*
        #[utoipa::path(post, path = $path, operation_id = $op, tag = "people-hr",
            params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the offer")),
            request_body = ReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
            responses((status = 200, body = HiringOfferView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
        pub async fn $fn(
            State(st): State<PeopleState>,
            auth: Authenticated,
            Path(id): Path<Uuid>,
            headers: HeaderMap,
            Json(b): Json<ReasonBody>,
        ) -> ApiResult<Response> {
            let ctx = auth.require(Permission::StaffManage)?;
            let v = expected!(headers);
            let reason = required_reason(b.reason)?;
            let t = ctx.scope.tenant_id;
            let (scope, _) = offer_scope(&st, ctx, id).await?;
            let a = actor(ctx);
            let mut tx = st.pool.begin().await.map_err(map_db_error)?;
            let o = load_offer(&mut tx, t, id, true).await?.view;
            check_version(v, o.version)?;
            move_offer(&mut tx, t, &scope, &o, $to, Some(&reason), &a, $action).await?;
            let cand: Option<CandidateStatus> = $cand;
            if let Some(to) = cand {
                move_candidate(&mut tx, t, &scope, o.candidate_id, to, Some(&reason), None, &a).await?;
            }
            let out = load_offer(&mut tx, t, id, false).await?.view;
            tx.commit().await.map_err(map_db_error)?;
            Ok(with_etag(StatusCode::OK, &out, Version(out.version)))
        }
    };
}

offer_close!(
    /// The candidate declined (`people.staff.manage`; reason required): the candidate moves to
    /// `offer_declined`.
    offer_decline, "/api/v1/people/offers/{id}/decline", "people_recruitment_offer_decline",
    OfferStatus::Declined, "people.offer.decline", Some(CandidateStatus::OfferDeclined));
offer_close!(
    /// Withdraw an offer that is not yet answered (`people.staff.manage`; reason required). The
    /// candidate keeps their stage; a revised offer can be drafted.
    offer_withdraw, "/api/v1/people/offers/{id}/withdraw", "people_recruitment_offer_withdraw",
    OfferStatus::Withdrawn, "people.offer.withdraw", None);

/// The offer letter.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HiringOfferLetter {
    pub offer_id: Uuid,
    pub number: String,
    /// `true` when this is the text snapshotted at sending.
    pub snapshot: bool,
    pub text: String,
    /// The same letter as a small self-contained HTML document (print / save as PDF).
    pub html: String,
}

/// The offer letter as text and HTML (`people.read`): the snapshot once sent, else a live
/// preview from the institution's template.
#[utoipa::path(get, path = "/api/v1/people/offers/{id}/letter", operation_id = "people_recruitment_offer_letter", tag = "people-hr",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = HiringOfferLetter), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn offer_letter(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<HiringOfferLetter>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let l = load_offer(&mut conn, t, id, false).await?;
    let inst = InstitutionId(l.view.institution_id);
    let scope = institution_scope(ctx, inst);
    let institution = require_institution(&st, &scope, inst).await?.name;
    let (text, snapshot) = match l.letter_text {
        Some(text) => (text, true),
        None => {
            let s = settings_row(&mut conn, t, inst.as_uuid()).await?;
            (
                render_letter(
                    s.offer_template.as_deref(),
                    &l.view,
                    &institution,
                    None,
                    None,
                ),
                false,
            )
        }
    };
    let html = of::letter_html(
        &format!("{} · {}", l.view.number, l.view.candidate_name),
        &text,
    );
    Ok(Json(HiringOfferLetter {
        offer_id: id,
        number: l.view.number,
        snapshot,
        text,
        html,
    }))
}

// ------------------------------------------------------------------ onboarding checklist

/// Stamps the checklist of a new onboarding case from the institution's templates (requisition →
/// department → general → the shipped default). Called by the onboarding case create.
pub async fn stamp_checklist(
    conn: &mut PgConnection,
    t: TenantId,
    inst: InstitutionId,
    case_id: Uuid,
    requisition_id: Uuid,
    candidate_id: Uuid,
) -> Result<(), KernelError> {
    let s = settings_row(conn, t, inst.as_uuid()).await?;
    let stored = of::normalize_templates(&s.onboarding_templates);
    let department: Option<String> = sqlx::query_scalar(
        "SELECT department FROM people.candidate_offer WHERE candidate_id = $1 AND status = 'accepted' \
         ORDER BY resolved_at DESC NULLS LAST LIMIT 1",
    )
    .bind(candidate_id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .flatten();
    let fallback = of::default_template();
    let job = requisition_id.to_string();
    let tpl = of::template_for_job(&stored, Some(&job), department.as_deref(), &fallback);
    for (pos, task) in tpl.tasks.iter().enumerate() {
        sqlx::query(
            "INSERT INTO people.onboarding_task (tenant_id, institution_id, onboarding_case_id, position, \
             title, category, owner, offset_days, template_id) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(t.as_uuid())
        .bind(inst.as_uuid())
        .bind(case_id)
        .bind(pos as i32)
        .bind(&task.title)
        .bind(&task.category)
        .bind(&task.owner)
        .bind(task.offset_days.map(|o| o as i32))
        .bind(&tpl.id)
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    }
    Ok(())
}

/// One checklist task.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HiringChecklistTask {
    pub id: Uuid,
    pub title: String,
    pub category: String,
    /// `hr`, `it`, `department`, `accounts`, `admin`.
    pub owner: Option<String>,
    pub offset_days: Option<i32>,
    /// "7 days before the joining date".
    pub offset_label: String,
    pub due_date: Option<NaiveDate>,
    pub overdue: bool,
    pub done: bool,
    pub done_at: Option<DateTime<Utc>>,
    pub done_by: Option<String>,
}

/// A category of the checklist, in template order.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HiringChecklistGroup {
    pub category: String,
    pub tasks: Vec<HiringChecklistTask>,
}

/// The onboarding checklist of a case.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HiringChecklist {
    pub onboarding_case_id: Uuid,
    pub candidate_name: String,
    pub joining_date: NaiveDate,
    pub status: String,
    pub progress: of::Progress,
    pub overdue: usize,
    pub groups: Vec<HiringChecklistGroup>,
}

async fn checklist(
    conn: &mut PgConnection,
    t: TenantId,
    case_id: Uuid,
    today: NaiveDate,
) -> Result<HiringChecklist, KernelError> {
    let head = sqlx::query(
        "SELECT o.joining_date, o.status, c.given_name, c.family_name FROM people.onboarding_case o \
         JOIN people.candidate c ON c.id = o.candidate_id WHERE o.id = $1 AND o.tenant_id = $2",
    )
    .bind(case_id)
    .bind(t.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "onboarding case",
    })?;
    let joining: NaiveDate = head.try_get("joining_date").map_err(map_db_error)?;
    let rows = sqlx::query(
        "SELECT id, title, category, owner, offset_days, done, done_at, done_by FROM people.onboarding_task \
         WHERE onboarding_case_id = $1 AND tenant_id = $2 ORDER BY position, created_at",
    )
    .bind(case_id)
    .bind(t.as_uuid())
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let mut tasks = Vec::with_capacity(rows.len());
    for r in &rows {
        let offset: Option<i32> = r.try_get("offset_days").map_err(map_db_error)?;
        let done: bool = r.try_get("done").map_err(map_db_error)?;
        let due = of::task_due_date(offset.map(i64::from), Some(joining));
        tasks.push(HiringChecklistTask {
            id: r.try_get("id").map_err(map_db_error)?,
            title: r.try_get("title").map_err(map_db_error)?,
            category: r.try_get("category").map_err(map_db_error)?,
            owner: r.try_get("owner").map_err(map_db_error)?,
            offset_days: offset,
            offset_label: of::offset_label(offset.map(i64::from)),
            due_date: due,
            overdue: !done && due.is_some_and(|d| d < today),
            done,
            done_at: r.try_get("done_at").map_err(map_db_error)?,
            done_by: r.try_get("done_by").map_err(map_db_error)?,
        });
    }
    let progress = of::progress(tasks.iter().map(|t| t.done));
    let overdue = tasks.iter().filter(|t| t.overdue).count();
    let groups = of::group_tasks(&tasks, |t| t.category.as_str())
        .into_iter()
        .map(|(category, ts)| HiringChecklistGroup {
            category,
            tasks: ts.into_iter().cloned().collect(),
        })
        .collect();
    Ok(HiringChecklist {
        onboarding_case_id: case_id,
        candidate_name: name_of(
            head.try_get("given_name").map_err(map_db_error)?,
            head.try_get("family_name").map_err(map_db_error)?,
        ),
        joining_date: joining,
        status: head.try_get("status").map_err(map_db_error)?,
        progress,
        overdue,
        groups,
    })
}

async fn case_institution(
    conn: &mut PgConnection,
    t: TenantId,
    case_id: Uuid,
) -> Result<InstitutionId, KernelError> {
    let inst: Option<Uuid> = sqlx::query_scalar(
        "SELECT institution_id FROM people.onboarding_case WHERE id = $1 AND tenant_id = $2",
    )
    .bind(case_id)
    .bind(t.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    inst.map(InstitutionId).ok_or(KernelError::NotFound {
        entity: "onboarding case",
    })
}

/// The onboarding checklist of a case, grouped by category with due dates (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/onboarding-cases/{id}/checklist", operation_id = "people_onboarding_checklist_get", tag = "people-hr",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = HiringChecklist), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn checklist_get(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<HiringChecklist>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let inst = case_institution(&mut conn, t, id).await?;
    let scope = institution_scope(ctx, inst);
    let today = crate::shared::time::business_today(&st, &scope, inst).await?;
    Ok(Json(checklist(&mut conn, t, id, today).await?))
}

/// A task added by hand.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HiringChecklistTaskInput {
    pub title: String,
    pub category: Option<String>,
    pub owner: Option<String>,
    pub offset_days: Option<i32>,
}

/// Add a task to a case's checklist (`people.staff.manage`).
#[utoipa::path(post, path = "/api/v1/people/onboarding-cases/{id}/checklist", operation_id = "people_onboarding_checklist_add", tag = "people-hr",
    params(("id" = String, Path, format = Uuid)), request_body = HiringChecklistTaskInput, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = HiringChecklist), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn checklist_add(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(b): Json<HiringChecklistTaskInput>,
) -> ApiResult<(StatusCode, Json<HiringChecklist>)> {
    let ctx = auth.require(Permission::StaffManage)?;
    let t = ctx.scope.tenant_id;
    let inst = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        case_institution(&mut conn, t, id).await?
    };
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let title = required_text("title", &b.title, 140)?;
    let category = optional_text("category", b.category, 40)?.unwrap_or_else(|| "Other".into());
    let owner = clean(b.owner).map(|o| o.to_lowercase());
    if owner
        .as_deref()
        .is_some_and(|o| !of::TASK_OWNERS.contains(&o))
    {
        return Err(invalid("owner", "invalid", "hr, it, department, accounts or admin").into());
    }
    if b.offset_days.is_some_and(|o| !(-365..=365).contains(&o)) {
        return Err(invalid("offsetDays", "out_of_range", "within a year of joining").into());
    }
    let today = crate::shared::time::business_today(&st, &scope, inst).await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let status: String = sqlx::query_scalar(
        "SELECT status FROM people.onboarding_case WHERE id = $1 AND tenant_id = $2 FOR UPDATE",
    )
    .bind(id)
    .bind(t.as_uuid())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if status == "cancelled" {
        return Err(KernelError::Conflict("the onboarding case is cancelled".into()).into());
    }
    let pos: i32 = sqlx::query_scalar(
        "SELECT COALESCE(max(position) + 1, 0) FROM people.onboarding_task WHERE onboarding_case_id = $1",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let task: Uuid = sqlx::query_scalar(
        "INSERT INTO people.onboarding_task (tenant_id, institution_id, onboarding_case_id, position, title, \
         category, owner, offset_days) VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(inst.as_uuid())
    .bind(id)
    .bind(pos)
    .bind(&title)
    .bind(&category)
    .bind(&owner)
    .bind(b.offset_days)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    common::audit_only(
        &mut tx,
        &scope,
        "onboarding_case",
        id,
        "people.onboarding_task.add",
        serde_json::json!({"taskId": task, "title": title}),
        &a,
    )
    .await?;
    let out = checklist(&mut tx, t, id, today).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok((StatusCode::CREATED, Json(out)))
}

/// Tick or untick a task.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct HiringChecklistDoneBody {
    pub done: bool,
}

/// Mark a checklist task done or not done (`people.staff.manage`).
#[utoipa::path(post, path = "/api/v1/people/onboarding-tasks/{id}/complete", operation_id = "people_onboarding_checklist_complete", tag = "people-hr",
    params(("id" = String, Path, format = Uuid)), request_body = HiringChecklistDoneBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = HiringChecklist), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn checklist_complete(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(b): Json<HiringChecklistDoneBody>,
) -> ApiResult<Json<HiringChecklist>> {
    let ctx = auth.require(Permission::StaffManage)?;
    let t = ctx.scope.tenant_id;
    let (inst, case_id) = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        let row: Option<(Uuid, Uuid)> = sqlx::query_as(
            "SELECT institution_id, onboarding_case_id FROM people.onboarding_task WHERE id = $1 AND tenant_id = $2",
        )
        .bind(id)
        .bind(t.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?;
        let (i, c) = row.ok_or(KernelError::NotFound {
            entity: "onboarding task",
        })?;
        (InstitutionId(i), c)
    };
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let today = crate::shared::time::business_today(&st, &scope, inst).await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    sqlx::query(
        "UPDATE people.onboarding_task SET done = $2, done_at = CASE WHEN $2 THEN COALESCE(done_at, now()) END, \
         done_by = CASE WHEN $2 THEN COALESCE(done_by, $3) END WHERE id = $1",
    )
    .bind(id)
    .bind(b.done)
    .bind(a.id.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    common::audit_only(
        &mut tx,
        &scope,
        "onboarding_case",
        case_id,
        if b.done {
            "people.onboarding_task.done"
        } else {
            "people.onboarding_task.reopen"
        },
        serde_json::json!({"taskId": id}),
        &a,
    )
    .await?;
    let out = checklist(&mut tx, t, case_id, today).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(out))
}

/// A due onboarding task across the institution.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HiringDueTask {
    pub task_id: Uuid,
    pub onboarding_case_id: Uuid,
    pub case_number: String,
    pub candidate_name: String,
    pub title: String,
    pub owner: Option<String>,
    pub due_date: NaiveDate,
    pub overdue: bool,
    pub days_left: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HiringDueTaskList {
    pub items: Vec<HiringDueTask>,
}

/// Unfinished onboarding tasks due within three days or overdue, soonest first (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/institutions/{id}/onboarding-due", operation_id = "people_onboarding_checklist_due", tag = "people-hr",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = HiringDueTaskList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn checklist_due(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<HiringDueTaskList>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution(&st, &scope, inst).await?;
    let today = crate::shared::time::business_today(&st, &scope, inst).await?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let items = due_tasks(&mut conn, ctx.scope.tenant_id, id, today).await?;
    Ok(Json(HiringDueTaskList { items }))
}

/// Due tasks of live (not joined / cancelled) cases.
pub async fn due_tasks(
    conn: &mut PgConnection,
    t: TenantId,
    inst: Uuid,
    today: NaiveDate,
) -> Result<Vec<HiringDueTask>, KernelError> {
    let rows = sqlx::query(
        "SELECT k.id, k.onboarding_case_id, o.case_number, c.given_name, c.family_name, k.title, k.owner, \
         k.offset_days, o.joining_date FROM people.onboarding_task k \
         JOIN people.onboarding_case o ON o.id = k.onboarding_case_id \
         JOIN people.candidate c ON c.id = o.candidate_id \
         WHERE k.tenant_id = $1 AND k.institution_id = $2 AND NOT k.done AND k.offset_days IS NOT NULL \
         AND o.status NOT IN ('joined', 'cancelled')",
    )
    .bind(t.as_uuid())
    .bind(inst)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let horizon = today + chrono::Duration::days(3);
    let mut out = Vec::new();
    for r in &rows {
        let joining: NaiveDate = r.try_get("joining_date").map_err(map_db_error)?;
        let offset: Option<i32> = r.try_get("offset_days").map_err(map_db_error)?;
        let Some(due) = of::task_due_date(offset.map(i64::from), Some(joining)) else {
            continue;
        };
        if due > horizon {
            continue;
        }
        out.push(HiringDueTask {
            task_id: r.try_get("id").map_err(map_db_error)?,
            onboarding_case_id: r.try_get("onboarding_case_id").map_err(map_db_error)?,
            case_number: r.try_get("case_number").map_err(map_db_error)?,
            candidate_name: name_of(
                r.try_get("given_name").map_err(map_db_error)?,
                r.try_get("family_name").map_err(map_db_error)?,
            ),
            title: r.try_get("title").map_err(map_db_error)?,
            owner: r.try_get("owner").map_err(map_db_error)?,
            due_date: due,
            overdue: due < today,
            days_left: of::days_between(today, due),
        });
    }
    out.sort_by_key(|d| d.due_date);
    Ok(out)
}

pub fn router(state: PeopleState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(settings_get, settings_put))
        .routes(routes!(offer_institution_list))
        .routes(routes!(offer_list, offer_create))
        .routes(routes!(offer_get, offer_update))
        .routes(routes!(offer_submit))
        .routes(routes!(offer_decide))
        .routes(routes!(offer_send))
        .routes(routes!(offer_accept))
        .routes(routes!(offer_decline))
        .routes(routes!(offer_withdraw))
        .routes(routes!(offer_letter))
        .routes(routes!(checklist_get, checklist_add))
        .routes(routes!(checklist_complete))
        .routes(routes!(checklist_due))
        .with_state(state)
}
