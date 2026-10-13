//! Template-based offering generation (L3-D-012) and change simulation → governed impact
//! decision (L3-D-016).

use super::lifecycle::{expect_version, insert_offering, unique_code, validate_refs};
use super::sections::insert_section_row;
use super::support::*;
use crate::shared::approval::{business_key, process, request_approval};
use crate::shared::guard::require_institution_writable;
use crate::shared::http::{actor, with_etag};
use crate::shared::scope::{institution_scope, tenant};
use crate::shared::{publish, AcademicState};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, Utc};
use std::collections::BTreeMap;
use sutra_contracts::academic::ids::CourseOfferingId;
use sutra_contracts::academic::status::{
    ChangeRequestStatus, CourseOfferingStatus, FacultyAssignmentStatus, SectionStatus,
};
use sutra_kernel::auth::Permission;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::InstitutionId;
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use uuid::Uuid;

// ================================================================== templates

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct OfferingTemplate {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub name: String,
    pub delivery_mode: String,
    pub capacity: Option<i32>,
    pub section_codes: Vec<String>,
    pub section_capacity: Option<i32>,
    pub campus_id: Option<Uuid>,
    pub code_suffix: Option<String>,
    /// `draft` | `active` | `retired`.
    pub status: String,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct OfferingTemplateList {
    pub items: Vec<OfferingTemplate>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewOfferingTemplate {
    pub name: String,
    pub delivery_mode: Option<String>,
    pub capacity: Option<i32>,
    /// Defaults to `["A"]`.
    #[serde(default)]
    pub section_codes: Vec<String>,
    pub section_capacity: Option<i32>,
    pub campus_id: Option<Uuid>,
    /// Offering codes are `{course or slot code}-{suffix}` (suffix defaults to the term code).
    pub code_suffix: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OfferingGenerateRequest {
    pub academic_term_id: Uuid,
    pub curriculum_version_id: Uuid,
    /// Components to generate offerings for; omitted = every component of `termNo`.
    #[serde(default)]
    pub component_ids: Vec<Uuid>,
    pub term_no: Option<i32>,
    /// All-or-nothing: any failure generates nothing (409).
    #[serde(default)]
    pub atomic: bool,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OfferingGenerationItem {
    pub curriculum_component_id: Uuid,
    /// `generated` | `failed`.
    pub outcome: String,
    pub offering_id: Option<Uuid>,
    pub offering_code: Option<String>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OfferingGenerationResult {
    pub run_id: Uuid,
    pub template_id: Uuid,
    pub items: Vec<OfferingGenerationItem>,
}

const TEMPLATE_COLUMNS: &str = "id, institution_id, name, delivery_mode, capacity, section_codes, section_capacity, campus_id, code_suffix, status, version, created_at";

/// Define an offering template in `draft` (L3-D-012, `offering.manage`).
#[utoipa::path(
    post,
    path = "/api/v1/academic/institutions/{id}/offering-templates",
    operation_id = "offering_template_create",
    tag = "offerings",
    params(
        ("id" = String, Path, format = Uuid, description = "Institution id"),
        ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key."),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = NewOfferingTemplate,
    responses(
        (status = 201, body = OfferingTemplate),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn template_create(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    headers: HeaderMap,
    Json(cmd): Json<NewOfferingTemplate>,
) -> ApiResult<(StatusCode, Json<OfferingTemplate>)> {
    let ctx = auth.require(Permission::OfferingManage)?;
    require_idempotency_key(&headers)?;
    let scope = institution_scope(ctx, id);
    require_institution_writable(&st, &scope, id).await?;
    let name = check_text("name", &cmd.name, 200)?;
    let mode = check_delivery_mode(cmd.delivery_mode.as_deref().unwrap_or("in_person"))?;
    let capacity = check_capacity("capacity", cmd.capacity)?;
    let section_capacity = check_capacity("sectionCapacity", cmd.section_capacity)?;
    let mut codes = Vec::new();
    for c in if cmd.section_codes.is_empty() {
        vec!["A".to_owned()]
    } else {
        cmd.section_codes.clone()
    } {
        let c = check_code("sectionCodes", &c, 20, false)?;
        if codes.iter().any(|x: &String| x.eq_ignore_ascii_case(&c)) {
            return Err(invalid("sectionCodes", "duplicate", "section codes must differ").into());
        }
        codes.push(c);
    }
    let suffix = match cmd
        .code_suffix
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(s) => Some(check_code("codeSuffix", s, 16, false)?),
        None => None,
    };
    if let Some(c) = cmd.campus_id {
        let ok = st
            .references
            .campus(&scope, c)
            .await?
            .is_some_and(|x| x.institution_id == id);
        if !ok {
            return Err(invalid(
                "campusId",
                "not_found",
                "no such campus in this institution",
            )
            .into());
        }
    }
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let t = sqlx::query_as::<_, OfferingTemplate>(&format!(
        "INSERT INTO academic.offering_template (tenant_id, institution_id, name, delivery_mode, capacity, section_codes, \
         section_capacity, campus_id, code_suffix) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING {TEMPLATE_COLUMNS}"
    ))
    .bind(scope.tenant_id.as_uuid()).bind(id.as_uuid()).bind(&name).bind(&mode).bind(capacity).bind(&codes)
    .bind(section_capacity).bind(cmd.campus_id).bind(&suffix)
    .fetch_one(&mut *tx).await.map_err(map_db_error)?;
    publish::audit(
        &mut tx,
        &scope,
        "academic.offering_template.create",
        "offering_template",
        t.id.to_string(),
        serde_json::json!({"name": name}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok((StatusCode::CREATED, Json(t)))
}

/// Offering templates of an institution (`academic.read`).
#[utoipa::path(
    get,
    path = "/api/v1/academic/institutions/{id}/offering-templates",
    operation_id = "offering_template_list",
    tag = "offerings",
    params(("id" = String, Path, format = Uuid, description = "Institution id")),
    security(("session_cookie" = [])),
    responses((status = 200, body = OfferingTemplateList))
)]
pub async fn template_list(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<OfferingTemplateList>> {
    let ctx = auth.require(Permission::AcademicRead)?;
    let items = sqlx::query_as::<_, OfferingTemplate>(&format!("SELECT {TEMPLATE_COLUMNS} FROM academic.offering_template WHERE tenant_id = $1 AND institution_id = $2 ORDER BY name"))
        .bind(tenant(ctx).as_uuid()).bind(id).fetch_all(&st.pool).await.map_err(map_db_error)?;
    Ok(Json(OfferingTemplateList { items }))
}

async fn load_template<'e, E: sqlx::PgExecutor<'e>>(
    exec: E,
    tenant: sutra_kernel::scope::TenantId,
    id: Uuid,
    lock: bool,
) -> Result<OfferingTemplate, KernelError> {
    sqlx::query_as::<_, OfferingTemplate>(&format!(
        "SELECT {TEMPLATE_COLUMNS} FROM academic.offering_template WHERE id = $1 AND tenant_id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    ))
    .bind(id).bind(tenant.as_uuid()).fetch_optional(exec).await.map_err(map_db_error)?
    .ok_or(KernelError::NotFound { entity: "offering template" })
}

/// One offering template (`academic.read`).
#[utoipa::path(
    get,
    path = "/api/v1/academic/offering-templates/{id}",
    operation_id = "offering_template_get",
    tag = "offerings",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses((status = 200, body = OfferingTemplate, headers(("ETag" = String))), (status = 404, body = Problem, content_type = "application/problem+json"))
)]
pub async fn template_get(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AcademicRead)?;
    let t = load_template(&st.pool, tenant(ctx), id, false).await?;
    Ok(with_etag(StatusCode::OK, &t, Version(t.version)))
}

async fn template_status(
    st: &AcademicState,
    ctx: &sutra_kernel::auth::AuthContext,
    id: Uuid,
    v: Version,
    from: &str,
    to: &str,
) -> Result<OfferingTemplate, KernelError> {
    let t = tenant(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let tpl = load_template(&mut *tx, t, id, true).await?;
    check_version(Version(tpl.version), v)?;
    if tpl.status != from {
        return Err(conflict(format!(
            "the template is {}; it cannot become {to}",
            tpl.status
        )));
    }
    let scope = institution_scope(ctx, InstitutionId(tpl.institution_id));
    let tpl = sqlx::query_as::<_, OfferingTemplate>(&format!("UPDATE academic.offering_template SET status = $2, version = version + 1, updated_at = now() WHERE id = $1 RETURNING {TEMPLATE_COLUMNS}"))
        .bind(id).bind(to).fetch_one(&mut *tx).await.map_err(map_db_error)?;
    publish::audit(
        &mut tx,
        &scope,
        &format!(
            "academic.offering_template.{}",
            if to == "active" { "activate" } else { "retire" }
        ),
        "offering_template",
        id.to_string(),
        serde_json::json!({}),
        &actor(ctx),
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(tpl)
}

/// Approve a draft template for generation (draft → active, `offering.decide`).
#[utoipa::path(
    post,
    path = "/api/v1/academic/offering-templates/{id}/activate",
    operation_id = "offering_template_activate",
    tag = "offerings",
    params(
        ("id" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the template"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 200, body = OfferingTemplate, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 428, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn template_activate(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::OfferingDecide)?;
    let v = expect_version!(headers);
    let t = template_status(&st, ctx, id, v, "draft", "active").await?;
    Ok(with_etag(StatusCode::OK, &t, Version(t.version)))
}

/// Retire an active template (`offering.decide`).
#[utoipa::path(
    post,
    path = "/api/v1/academic/offering-templates/{id}/retire",
    operation_id = "offering_template_retire",
    tag = "offerings",
    params(
        ("id" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the template"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 200, body = OfferingTemplate, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 428, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn template_retire(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::OfferingDecide)?;
    let v = expect_version!(headers);
    let t = template_status(&st, ctx, id, v, "active", "retired").await?;
    Ok(with_etag(StatusCode::OK, &t, Version(t.version)))
}

/// Generate independent draft offerings from an active template (L3-D-012,
/// `offering.manage`). Duplicates (a live offering of the component in the term, or a taken
/// code) fail per component; `atomic` turns any failure into a 409 with nothing generated.
#[utoipa::path(
    post,
    path = "/api/v1/academic/offering-templates/{id}/generate",
    operation_id = "offering_template_generate",
    tag = "offerings",
    params(
        ("id" = String, Path, format = Uuid),
        ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key."),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = OfferingGenerateRequest,
    responses(
        (status = 201, body = OfferingGenerationResult),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, description = "Template not active, or an atomic run had a failure", body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn generate(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<OfferingGenerateRequest>,
) -> ApiResult<(StatusCode, Json<OfferingGenerationResult>)> {
    let ctx = auth.require(Permission::OfferingManage)?;
    require_idempotency_key(&headers)?;
    let t = tenant(ctx);
    let tpl = load_template(&st.pool, t, id, false).await?;
    if tpl.status != "active" {
        return Err(conflict(format!(
            "the template is {}; only approved, active templates generate offerings",
            tpl.status
        ))
        .into());
    }
    let institution = InstitutionId(tpl.institution_id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let term = st
        .references
        .academic_term(&scope, cmd.academic_term_id)
        .await?
        .filter(|x| x.institution_id == institution)
        .ok_or_else(|| {
            invalid(
                "academicTermId",
                "not_found",
                "no such academic term in this institution",
            )
        })?;
    let components = st
        .references
        .curriculum_components(&scope, cmd.curriculum_version_id)
        .await?;
    let wanted: Vec<_> = if cmd.component_ids.is_empty() {
        let n = cmd
            .term_no
            .ok_or_else(|| invalid("termNo", "required", "give componentIds or termNo"))?;
        components
            .iter()
            .filter(|c| c.term_no == n)
            .cloned()
            .collect()
    } else {
        let mut v = Vec::new();
        for cid in &cmd.component_ids {
            v.push(
                components
                    .iter()
                    .find(|c| c.id == *cid)
                    .cloned()
                    .ok_or_else(|| {
                        invalid(
                            "componentIds",
                            "not_found",
                            format!("{cid} is not a component of the curriculum version"),
                        )
                    })?,
            );
        }
        v
    };
    if wanted.is_empty() {
        return Err(invalid("termNo", "empty", "no components to generate").into());
    }
    let a = actor(ctx);
    let suffix = tpl.code_suffix.clone().unwrap_or_else(|| term.code.clone());
    let run = Uuid::now_v7();
    let mut items = Vec::new();
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    for comp in &wanted {
        let base = comp
            .course_code
            .clone()
            .unwrap_or_else(|| comp.slot_code.clone());
        let mut code: String = format!("{base}-{suffix}")
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
            .collect();
        code.truncate(40);
        // Per-component savepoint: a failure never leaves a half-generated offering.
        let mut sp = sqlx::Acquire::begin(&mut *tx).await.map_err(map_db_error)?;
        let outcome: Result<OfferingRowOut, KernelError> = async {
            let dup: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM academic.course_offering WHERE tenant_id = $1 AND institution_id = $2 AND academic_term_id = $3 \
                 AND curriculum_component_id = $4 AND status NOT IN ('cancelled', 'archived', 'merged', 'split')",
            )
            .bind(t.as_uuid()).bind(institution.as_uuid()).bind(term.id).bind(comp.id)
            .fetch_one(&mut *sp).await.map_err(map_db_error)?;
            if dup > 0 {
                return Err(conflict("a live offering of this component already exists in the term"));
            }
            let refs = validate_refs(&st, &scope, institution, term.id, cmd.curriculum_version_id, comp.id, tpl.campus_id, None).await?;
            let o = insert_offering(&mut sp, &scope, institution, &refs, term.id, cmd.curriculum_version_id, &code, &comp.title,
                tpl.campus_id, None, &tpl.delivery_mode, tpl.capacity, CourseOfferingStatus::Draft, None).await?;
            for sc in &tpl.section_codes {
                insert_section_row(&mut sp, &o, sc, tpl.section_capacity, tpl.campus_id, SectionStatus::Planned).await?;
            }
            sqlx::query("INSERT INTO academic.offering_template_generation (tenant_id, template_id, run_id, academic_term_id, curriculum_component_id, offering_id, outcome) VALUES ($1, $2, $3, $4, $5, $6, 'generated')")
                .bind(t.as_uuid()).bind(tpl.id).bind(run).bind(term.id).bind(comp.id).bind(o.id.as_uuid())
                .execute(&mut *sp).await.map_err(map_db_error)?;
            publish::audit(&mut sp, &scope, "academic.course_offering.generate", "course_offering", o.id.to_string(), serde_json::json!({"templateId": tpl.id, "runId": run}), &a).await?;
            Ok(OfferingRowOut { id: o.id.as_uuid(), code: o.offering_code })
        }
        .await;
        match outcome {
            Ok(o) => {
                sp.commit().await.map_err(map_db_error)?;
                items.push(OfferingGenerationItem {
                    curriculum_component_id: comp.id,
                    outcome: "generated".into(),
                    offering_id: Some(o.id),
                    offering_code: Some(o.code),
                    detail: None,
                });
            }
            Err(e) => {
                sp.rollback().await.map_err(map_db_error)?;
                if cmd.atomic {
                    return Err(conflict(format!(
                        "atomic generation stopped at component {}: {e}",
                        comp.slot_code
                    ))
                    .into());
                }
                let detail = match &e {
                    KernelError::Conflict(m) => m.clone(),
                    KernelError::Validation(v) => v
                        .iter()
                        .map(|f| f.message.clone())
                        .collect::<Vec<_>>()
                        .join("; "),
                    other => other.to_string(),
                };
                sqlx::query("INSERT INTO academic.offering_template_generation (tenant_id, template_id, run_id, academic_term_id, curriculum_component_id, outcome, detail) VALUES ($1, $2, $3, $4, $5, 'failed', $6)")
                    .bind(t.as_uuid()).bind(tpl.id).bind(run).bind(term.id).bind(comp.id).bind(&detail)
                    .execute(&mut *tx).await.map_err(map_db_error)?;
                items.push(OfferingGenerationItem {
                    curriculum_component_id: comp.id,
                    outcome: "failed".into(),
                    offering_id: None,
                    offering_code: Some(code.clone()),
                    detail: Some(detail),
                });
            }
        }
    }
    publish::audit(&mut tx, &scope, "academic.offering_template.generate", "offering_template", tpl.id.to_string(),
        serde_json::json!({"runId": run, "generated": items.iter().filter(|i| i.outcome == "generated").count(), "failed": items.iter().filter(|i| i.outcome == "failed").count()}), &a).await?;
    tx.commit().await.map_err(map_db_error)?;
    let _ = unique_code;
    Ok((
        StatusCode::CREATED,
        Json(OfferingGenerationResult {
            run_id: run,
            template_id: tpl.id,
            items,
        }),
    ))
}

struct OfferingRowOut {
    id: Uuid,
    code: String,
}

// ================================================================== simulations

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewOfferingSimulation {
    /// `capacity` | `cancellation` | `suspension` | `section_cancellation` | `merge` | `faculty_change`.
    pub change_kind: String,
    pub proposed_capacity: Option<i32>,
    pub section_id: Option<Uuid>,
    /// Decision criteria the governance body applies (free text).
    pub criteria: Option<String>,
}

/// Read-only impact report (L4-D-016-BH-003).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OfferingImpactReport {
    pub offering_status: CourseOfferingStatus,
    pub seated: i64,
    pub waitlisted: i64,
    pub active_sections: i64,
    pub active_faculty: i64,
    pub current_capacity: Option<i32>,
    /// Students who would lose (or be unable to keep) their seat.
    pub students_affected: i64,
    pub faculty_affected: i64,
    pub effective_quota_seats: i64,
    /// The change is possible without breaking a rule (it may still need a decision).
    pub feasible: bool,
    pub notes: Vec<String>,
    pub facts_read_at: DateTime<Utc>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OfferingSimulation {
    pub id: Uuid,
    pub offering_id: Uuid,
    pub change_kind: String,
    #[schema(value_type = Object)]
    pub proposal: serde_json::Value,
    pub criteria: Option<String>,
    pub report: OfferingImpactReport,
    pub status: ChangeRequestStatus,
    /// `proceed` | `revise` | `reject` once decided.
    pub decision: Option<String>,
    pub decision_reason: Option<String>,
    pub decided_by: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
    pub process_instance_id: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct OfferingSimulationList {
    pub items: Vec<OfferingSimulation>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OfferingSimulationDecision {
    /// `proceed` | `revise` | `reject`.
    pub decision: String,
    pub reason: Option<String>,
}

type SimRow = (
    Uuid,
    Uuid,
    String,
    serde_json::Value,
    Option<String>,
    serde_json::Value,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<DateTime<Utc>>,
    Option<String>,
    i64,
    DateTime<Utc>,
);
const SIM_COLUMNS: &str = "id, offering_id, change_kind, proposal, criteria, report, status, decision, decision_reason, decided_by, decided_at, process_instance_id, version, created_at";

fn sim_view(r: SimRow) -> Result<OfferingSimulation, KernelError> {
    Ok(OfferingSimulation {
        id: r.0,
        offering_id: r.1,
        change_kind: r.2,
        proposal: r.3,
        criteria: r.4,
        report: serde_json::from_value(r.5).map_err(KernelError::internal)?,
        status: ChangeRequestStatus::parse(&r.6).unwrap_or(ChangeRequestStatus::Requested),
        decision: r.7,
        decision_reason: r.8,
        decided_by: r.9,
        decided_at: r.10,
        process_instance_id: r.11,
        version: r.12,
        created_at: r.13,
    })
}

/// Simulate a proposed change and store the read-only impact report (L3-D-016,
/// `offering.manage`). Nothing operational changes.
#[utoipa::path(
    post,
    path = "/api/v1/academic/offerings/{id}/simulations",
    operation_id = "offering_simulation_create",
    tag = "offerings",
    params(
        ("id" = String, Path, format = Uuid),
        ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key."),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = NewOfferingSimulation,
    responses(
        (status = 201, body = OfferingSimulation),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn simulation_create(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<CourseOfferingId>,
    headers: HeaderMap,
    Json(cmd): Json<NewOfferingSimulation>,
) -> ApiResult<(StatusCode, Json<OfferingSimulation>)> {
    let ctx = auth.require(Permission::OfferingManage)?;
    require_idempotency_key(&headers)?;
    if !matches!(
        cmd.change_kind.as_str(),
        "capacity"
            | "cancellation"
            | "suspension"
            | "section_cancellation"
            | "merge"
            | "faculty_change"
    ) {
        return Err(invalid(
            "changeKind",
            "invalid",
            "capacity, cancellation, suspension, section_cancellation, merge or faculty_change",
        )
        .into());
    }
    if cmd.change_kind == "capacity" && cmd.proposed_capacity.is_none() {
        return Err(invalid(
            "proposedCapacity",
            "required",
            "a capacity simulation needs the proposed capacity",
        )
        .into());
    }
    if cmd.change_kind == "section_cancellation" && cmd.section_id.is_none() {
        return Err(invalid("sectionId", "required", "name the section").into());
    }
    check_capacity("proposedCapacity", cmd.proposed_capacity)?;
    let criteria = clean_reason(cmd.criteria.clone(), false)?;
    let t = tenant(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    // Read-only facts (no locks: the simulation changes nothing).
    let o = crate::shared::offering::load_offering(&mut *tx, t, id).await?;
    let scope = institution_scope(ctx, o.institution_id);
    let seated = seated_in_offering(&mut *tx, o.id).await?;
    let (waitlisted, active_sections, active_faculty, quota): (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM academic.offering_waitlist_entry WHERE offering_id = $1 AND status IN ('waiting', 'offered')), \
                (SELECT count(*) FROM academic.offering_section WHERE offering_id = $1 AND status = 'active'), \
                (SELECT count(*) FROM academic.faculty_assignment WHERE offering_id = $1 AND status = 'active'), \
                (SELECT COALESCE(sum(seats), 0)::int8 FROM academic.offering_quota WHERE offering_id = $1 AND status = 'effective')",
    )
    .bind(o.id.as_uuid()).fetch_one(&mut *tx).await.map_err(map_db_error)?;
    let mut notes = Vec::new();
    let mut feasible = true;
    let (mut students_affected, mut faculty_affected) = (0i64, 0i64);
    match cmd.change_kind.as_str() {
        "capacity" => {
            let cap = i64::from(cmd.proposed_capacity.unwrap_or_default());
            if cap < seated {
                feasible = false;
                students_affected = seated - cap;
                notes.push(format!(
                    "{seated} seated exceed the proposed capacity {cap}"
                ));
            }
            if cap < quota {
                feasible = false;
                notes.push(format!("effective quotas reserve {quota} seats"));
            }
            if cap > seated && waitlisted > 0 {
                notes.push(format!(
                    "{} waitlisted student(s) could be offered a seat",
                    (cap - seated).min(waitlisted)
                ));
            }
        }
        "cancellation" => {
            students_affected = seated;
            faculty_affected = active_faculty;
            if !o.status.can_transition(CourseOfferingStatus::Cancelled) {
                feasible = false;
                notes.push(format!(
                    "the offering is {}; administrative cancellation is not possible",
                    o.status
                ));
            }
            if seated > 0 {
                feasible = false;
                notes.push("seated students would first have to be withdrawn".into());
            }
        }
        "suspension" => {
            students_affected = seated;
            faculty_affected = active_faculty;
            if o.status != CourseOfferingStatus::Active {
                feasible = false;
                notes.push(format!(
                    "the offering is {}; only active offerings are suspended",
                    o.status
                ));
            }
        }
        "section_cancellation" => {
            let sid = cmd.section_id.unwrap_or_default();
            let (n, f): (i64, i64) = sqlx::query_as(
                "SELECT (SELECT count(*) FROM academic.section_enrolment WHERE section_id = $1 AND offering_id = $2 AND status = 'enrolled'), \
                        (SELECT count(*) FROM academic.faculty_assignment WHERE section_id = $1 AND offering_id = $2 AND status = 'active')",
            )
            .bind(sid).bind(o.id.as_uuid()).fetch_one(&mut *tx).await.map_err(map_db_error)?;
            students_affected = n;
            faculty_affected = f;
            if n > 0 {
                feasible = false;
                notes.push(format!("{n} seated student(s) need a transfer first"));
            }
        }
        "merge" => {
            students_affected = seated;
            faculty_affected = active_faculty;
            if o.status != CourseOfferingStatus::Active {
                feasible = false;
                notes.push("only active offerings merge".into());
            }
            notes.push("seats and active faculty would move to successor sections".into());
        }
        _ => {
            faculty_affected = active_faculty;
            if active_faculty == 0 {
                notes.push("no active faculty assignment to change".into());
            }
        }
    }
    let report = OfferingImpactReport {
        offering_status: o.status,
        seated,
        waitlisted,
        active_sections,
        active_faculty,
        current_capacity: o.capacity,
        students_affected,
        faculty_affected,
        effective_quota_seats: quota,
        feasible,
        notes,
        facts_read_at: Utc::now(),
    };
    let proposal =
        serde_json::json!({"proposedCapacity": cmd.proposed_capacity, "sectionId": cmd.section_id});
    let a = actor(ctx);
    let r: SimRow = sqlx::query_as(&format!(
        "INSERT INTO academic.offering_change_simulation (tenant_id, institution_id, offering_id, change_kind, proposal, criteria, report, requested_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING {SIM_COLUMNS}"
    ))
    .bind(t.as_uuid()).bind(o.institution_id.as_uuid()).bind(o.id.as_uuid()).bind(&cmd.change_kind).bind(&proposal)
    .bind(&criteria).bind(serde_json::to_value(&report).map_err(KernelError::internal)?).bind(a.id.as_deref())
    .fetch_one(&mut *tx).await.map_err(map_db_error)?;
    let sim = sim_view(r)?;
    publish::audit(
        &mut tx,
        &scope,
        "academic.offering_simulation.run",
        "offering_change_simulation",
        sim.id.to_string(),
        serde_json::json!({"changeKind": sim.change_kind, "feasible": feasible}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok((StatusCode::CREATED, Json(sim)))
}

/// Simulations of an offering, newest first (`academic.read`).
#[utoipa::path(
    get,
    path = "/api/v1/academic/offerings/{id}/simulations",
    operation_id = "offering_simulation_list",
    tag = "offerings",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses((status = 200, body = OfferingSimulationList), (status = 404, body = Problem, content_type = "application/problem+json"))
)]
pub async fn simulation_list(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<CourseOfferingId>,
) -> ApiResult<Json<OfferingSimulationList>> {
    let ctx = auth.require(Permission::AcademicRead)?;
    let o = crate::shared::offering::load_offering(&st.pool, tenant(ctx), id).await?;
    let rows: Vec<SimRow> = sqlx::query_as(&format!("SELECT {SIM_COLUMNS} FROM academic.offering_change_simulation WHERE offering_id = $1 ORDER BY created_at DESC"))
        .bind(o.id.as_uuid()).fetch_all(&st.pool).await.map_err(map_db_error)?;
    let items = rows.into_iter().map(sim_view).collect::<Result<_, _>>()?;
    Ok(Json(OfferingSimulationList { items }))
}

/// Record the governed impact decision on a simulation (`offering.decide`): proceed, revise or
/// reject. The decision never executes the change; that is a separate governed command.
#[utoipa::path(
    post,
    path = "/api/v1/academic/offerings/{id}/simulations/{simulationId}/decision",
    operation_id = "offering_simulation_decision",
    tag = "offerings",
    params(
        ("id" = String, Path, format = Uuid),
        ("simulationId" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag (version) of the simulation"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = OfferingSimulationDecision,
    responses(
        (status = 200, body = OfferingSimulation),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
        (status = 428, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn simulation_decision(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path((id, sid)): Path<(CourseOfferingId, Uuid)>,
    headers: HeaderMap,
    Json(d): Json<OfferingSimulationDecision>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::OfferingDecide)?;
    let v = expect_version!(headers);
    let to = match d.decision.as_str() {
        "proceed" => ChangeRequestStatus::Approved,
        "revise" => ChangeRequestStatus::Withdrawn,
        "reject" => ChangeRequestStatus::Rejected,
        _ => return Err(invalid("decision", "invalid", "proceed, revise or reject").into()),
    };
    let reason = clean_reason(d.reason, d.decision != "proceed")?;
    let t = tenant(ctx);
    let o = crate::shared::offering::load_offering(&st.pool, t, id).await?;
    let scope = institution_scope(ctx, o.institution_id);
    let outcome = request_approval(
        &st,
        &scope,
        process::OFFERING_CHANGE_IMPACT,
        business_key("offering_change_simulation", sid, v.0),
        BTreeMap::from([("decision".to_owned(), serde_json::json!(d.decision))]),
    )
    .await?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let r: SimRow = sqlx::query_as(&format!("SELECT {SIM_COLUMNS} FROM academic.offering_change_simulation WHERE id = $1 AND offering_id = $2 FOR UPDATE"))
        .bind(sid).bind(o.id.as_uuid()).fetch_optional(&mut *tx).await.map_err(map_db_error)?
        .ok_or(KernelError::NotFound { entity: "simulation" })?;
    let sim = sim_view(r)?;
    check_version(Version(sim.version), v)?;
    if sim.status != ChangeRequestStatus::Requested {
        return Err(conflict(format!(
            "the simulation is already decided ({})",
            sim.decision.unwrap_or_default()
        ))
        .into());
    }
    let a = actor(ctx);
    let r: SimRow = sqlx::query_as(&format!(
        "UPDATE academic.offering_change_simulation SET status = $2, decision = $3, decision_reason = $4, decided_by = $5, decided_at = now(), \
         process_instance_id = $6, version = version + 1, updated_at = now() WHERE id = $1 RETURNING {SIM_COLUMNS}"
    ))
    .bind(sid).bind(to.as_str()).bind(&d.decision).bind(&reason).bind(a.id.as_deref()).bind(outcome.process_instance_id())
    .fetch_one(&mut *tx).await.map_err(map_db_error)?;
    let sim = sim_view(r)?;
    publish::audit(
        &mut tx,
        &scope,
        "academic.offering_simulation.decide",
        "offering_change_simulation",
        sim.id.to_string(),
        serde_json::json!({"decision": d.decision, "reason": reason}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let _ = FacultyAssignmentStatus::Active;
    Ok(with_etag(StatusCode::OK, &sim, Version(sim.version)))
}
