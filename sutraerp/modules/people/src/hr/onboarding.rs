//! Onboarding case → accepted workforce hand-offs (L3-E-033, second half): initiated →
//! documentation_pending → verification_pending → ready_for_joining → joined | cancelled.
//! Joining (`ONBOARDING_ACCEPTANCE`) creates the staff record (shared inserter, optional login
//! link) and a pending engagement, and submits that engagement for activation
//! (`ENGAGEMENT_ACTIVATION`).

use super::recruitment::{load_candidate, load_requisition, set_requisition, StatusQuery};
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, with_etag};
use crate::shared::numbering::{kind, next};
use crate::shared::rows::{decode, load_staff};
use crate::shared::scope::institution_scope;
use crate::shared::staff::{insert_engagement, insert_staff, NewEngagement, NewStaff};
use crate::shared::time::business_today;
use crate::shared::{Actor, PeopleState};
use crate::workforce::common::{
    self, check_campus, check_org_unit, check_version, clean, emit, expected, history, invalid,
    modified, reason_of, required_reason, status_payload, Decision, DecisionBody, ReasonBody,
};
use crate::workforce::engagement::{open_change, validate_manager, ChangeKind, EngagementChanges};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use std::collections::BTreeMap;
use sutra_contracts::people::events::workforce as ev;
use sutra_contracts::people::ids::StaffMemberId;
use sutra_contracts::people::status::{
    CandidateStatus, EmploymentType, EngagementStatus, OnboardingStatus, RequisitionStatus,
    StaffStatus,
};
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// One submitted onboarding document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OnboardingDocument {
    /// `identity`, `address`, `education`, `employment`, `medical`, `other`.
    pub document_type: String,
    /// Document number or storage reference.
    pub reference: String,
}

/// An onboarding case.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OnboardingView {
    pub id: Uuid,
    pub institution_id: Uuid,
    #[schema(example = "ONB-000001")]
    pub number: String,
    pub candidate_id: Uuid,
    pub candidate_name: String,
    pub requisition_id: Uuid,
    pub joining_date: NaiveDate,
    pub subject_ref: Option<String>,
    pub org_unit_id: Option<Uuid>,
    pub campus_id: Option<Uuid>,
    pub reports_to_staff_id: Option<Uuid>,
    pub probation_end_date: Option<NaiveDate>,
    pub documents: Vec<OnboardingDocument>,
    pub documents_verified: bool,
    pub staff_id: Option<Uuid>,
    pub engagement_id: Option<Uuid>,
    pub status: OnboardingStatus,
    pub status_reason: Option<String>,
    /// Set while joining awaits its decision.
    pub process_instance_id: Option<String>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct OnboardingList {
    pub items: Vec<OnboardingView>,
}

const O_SELECT: &str = "SELECT o.id, o.institution_id, o.case_number, o.candidate_id, \
    c.given_name, c.family_name, o.requisition_id, o.joining_date, o.subject_ref, o.org_unit_id, \
    o.campus_id, o.reports_to_staff_id, o.probation_end_date, o.documents, o.documents_verified, \
    o.staff_id, o.engagement_id, o.status, o.status_reason, o.process_instance_id, o.version \
    FROM people.onboarding_case o JOIN people.candidate c ON c.id = o.candidate_id";

fn from_row(r: &PgRow) -> Result<OnboardingView, sqlx::Error> {
    let given: String = r.try_get("given_name")?;
    let family: Option<String> = r.try_get("family_name")?;
    let docs: serde_json::Value = r.try_get("documents")?;
    Ok(OnboardingView {
        id: r.try_get("id")?,
        institution_id: r.try_get("institution_id")?,
        number: r.try_get("case_number")?,
        candidate_id: r.try_get("candidate_id")?,
        candidate_name: match family {
            Some(f) => format!("{given} {f}"),
            None => given,
        },
        requisition_id: r.try_get("requisition_id")?,
        joining_date: r.try_get("joining_date")?,
        subject_ref: r.try_get("subject_ref")?,
        org_unit_id: r.try_get("org_unit_id")?,
        campus_id: r.try_get("campus_id")?,
        reports_to_staff_id: r.try_get("reports_to_staff_id")?,
        probation_end_date: r.try_get("probation_end_date")?,
        documents: serde_json::from_value(docs).unwrap_or_default(),
        documents_verified: r.try_get("documents_verified")?,
        staff_id: r.try_get("staff_id")?,
        engagement_id: r.try_get("engagement_id")?,
        status: decode(r.try_get("status")?, OnboardingStatus::parse)?,
        status_reason: r.try_get("status_reason")?,
        process_instance_id: r.try_get("process_instance_id")?,
        version: r.try_get("version")?,
    })
}

async fn load(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<OnboardingView, KernelError> {
    let lock_sql = if lock { " FOR UPDATE OF o" } else { "" };
    let row = sqlx::query(&format!(
        "{O_SELECT} WHERE o.id = $1 AND o.tenant_id = $2{lock_sql}"
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "onboarding case",
    })?;
    from_row(&row).map_err(map_db_error)
}

/// Legal onboarding moves.
pub fn onboarding_can_transition(from: OnboardingStatus, to: OnboardingStatus) -> bool {
    use OnboardingStatus::*;
    matches!(
        (from, to),
        (Initiated, DocumentationPending)
            | (Initiated | DocumentationPending, VerificationPending)
            | (VerificationPending, DocumentationPending)
            | (VerificationPending, ReadyForJoining)
            | (ReadyForJoining, Joined)
            | (
                Initiated | DocumentationPending | VerificationPending | ReadyForJoining,
                Cancelled
            )
    )
}

const DOC_TYPES: &[&str] = &[
    "identity",
    "address",
    "education",
    "employment",
    "medical",
    "other",
];

#[allow(clippy::too_many_arguments)]
async fn set_status(
    conn: &mut PgConnection,
    tenant: TenantId,
    o: &OnboardingView,
    to: OnboardingStatus,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<(), KernelError> {
    if o.status == to {
        return Ok(());
    }
    if !onboarding_can_transition(o.status, to) {
        return Err(common::illegal(
            "onboarding case",
            &o.number,
            o.status.as_str(),
            to.as_str(),
        ));
    }
    sqlx::query("UPDATE people.onboarding_case SET status = $2, status_reason = $3, updated_at = now() WHERE id = $1")
        .bind(o.id)
        .bind(to.as_str())
        .bind(reason)
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    history(
        conn,
        tenant,
        InstitutionId(o.institution_id),
        "onboarding_case",
        o.id,
        Some(o.status.as_str()),
        to.as_str(),
        "sutraerp.people.onboarding_case.status",
        reason,
        o.process_instance_id.as_deref(),
        actor,
    )
    .await
}

async fn bump(conn: &mut PgConnection, id: Uuid) -> Result<(), KernelError> {
    sqlx::query(
        "UPDATE people.onboarding_case SET version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(())
}

/// Opens an onboarding case.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OnboardingCreate {
    /// Defaults to the offer's proposed joining date.
    pub joining_date: Option<NaiveDate>,
    /// Login to link to the staff record created at joining.
    pub subject_ref: Option<String>,
    pub org_unit_id: Option<Uuid>,
    pub campus_id: Option<Uuid>,
    pub reports_to_staff_id: Option<Uuid>,
    /// Required when the requisition hires on probation.
    pub probation_end_date: Option<NaiveDate>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DocumentsBody {
    pub documents: Vec<OnboardingDocument>,
    /// Hand the documents to HR verification.
    #[serde(default)]
    pub submit_for_verification: bool,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct VerifyBody {
    /// `true` = documents verified; `false` = returned for rework (reason required).
    pub approved: bool,
    pub reason: Option<String>,
}

/// Open the onboarding case of a candidate who accepted the offer (`people.staff.manage`;
/// Idempotency-Key). One live case per candidate (409).
#[utoipa::path(post, path = "/api/v1/people/candidates/{id}/onboarding", operation_id = "people_onboarding_create", tag = "people-hr",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)),
    request_body = OnboardingCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = OnboardingView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn create(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<OnboardingCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    require_idempotency_key(&headers)?;
    let t = ctx.scope.tenant_id;
    let pre = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_candidate(&mut conn, t, id, false).await?
    };
    let institution = InstitutionId(pre.institution_id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    check_org_unit(&st, &scope, institution, "orgUnitId", b.org_unit_id).await?;
    check_campus(&st, &scope, institution, "campusId", b.campus_id).await?;
    let subject = clean(b.subject_ref);
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let c = load_candidate(&mut tx, t, id, true).await?;
    if c.status != CandidateStatus::OfferAccepted {
        return Err(KernelError::Conflict(format!(
            "candidate {} is {}; onboarding follows an accepted offer",
            c.email, c.status
        ))
        .into());
    }
    if c.onboarding_case_id.is_some() {
        return Err(KernelError::Conflict(format!(
            "candidate {} already has an onboarding case",
            c.email
        ))
        .into());
    }
    let r = load_requisition(&mut tx, t, c.requisition_id, false).await?;
    let joining = b.joining_date.or(c.proposed_joining_date).ok_or_else(|| {
        invalid(
            "joiningDate",
            "required",
            "no joining date in the offer; give one",
        )
    })?;
    if r.employment_type == EmploymentType::Probation && b.probation_end_date.is_none() {
        return Err(invalid(
            "probationEndDate",
            "required",
            "the requisition hires on probation",
        )
        .into());
    }
    if b.probation_end_date.is_some_and(|p| p <= joining) {
        return Err(invalid(
            "probationEndDate",
            "before_joining",
            "must be after the joining date",
        )
        .into());
    }
    if let Some(m) = b.reports_to_staff_id {
        let ok = load_staff(&mut tx, t, StaffMemberId(m))
            .await?
            .is_some_and(|s| s.status.is_live() && s.institution_id == institution);
        if !ok {
            return Err(invalid("reportsToStaffId", "not_found", "unknown staff record").into());
        }
    }
    if let Some(s) = &subject {
        let taken: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM people.staff_member WHERE tenant_id = $1 AND subject_ref = $2 \
             AND status NOT IN ('merged', 'archived'))",
        )
        .bind(t.as_uuid())
        .bind(s)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
        if taken {
            return Err(KernelError::Conflict(
                "a live staff record is already linked to this login".into(),
            )
            .into());
        }
    }
    let number = next(&mut tx, t, institution, kind::ONBOARDING).await?;
    let oid: Uuid = sqlx::query_scalar(
        "INSERT INTO people.onboarding_case (tenant_id, institution_id, case_number, candidate_id, \
         requisition_id, joining_date, subject_ref, org_unit_id, campus_id, reports_to_staff_id, \
         probation_end_date) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(c.institution_id)
    .bind(&number)
    .bind(c.id)
    .bind(r.id)
    .bind(joining)
    .bind(&subject)
    .bind(b.org_unit_id.or(r.org_unit_id))
    .bind(b.campus_id.or(r.campus_id))
    .bind(b.reports_to_staff_id)
    .bind(b.probation_end_date)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    history(
        &mut tx,
        t,
        institution,
        "onboarding_case",
        oid,
        None,
        "initiated",
        "sutraerp.people.onboarding_case.created",
        None,
        None,
        &actor,
    )
    .await?;
    common::audit_only(
        &mut tx,
        &scope,
        "onboarding_case",
        oid,
        "people.onboarding_case.create",
        serde_json::json!({ "number": number, "candidateId": c.id }),
        &actor,
    )
    .await?;
    let o = load(&mut tx, t, oid, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::CREATED, &o, Version(o.version)))
}

/// Onboarding cases of an institution (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/institutions/{id}/onboarding-cases", operation_id = "people_onboarding_list", tag = "people-hr",
    params(("id" = String, Path, format = Uuid), StatusQuery), security(("session_cookie" = [])),
    responses((status = 200, body = OnboardingList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn list(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<StatusQuery>,
) -> ApiResult<Json<OnboardingList>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution(&st, &scope, institution).await?;
    let status = match q.status.as_deref() {
        None => None,
        Some(s) => Some(
            OnboardingStatus::parse(s)
                .ok_or_else(|| invalid("status", "invalid", "unknown onboarding status"))?,
        ),
    };
    let rows = sqlx::query(&format!(
        "{O_SELECT} WHERE o.tenant_id = $1 AND o.institution_id = $2 \
         AND ($3::text IS NULL OR o.status = $3) ORDER BY o.case_number DESC"
    ))
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .bind(status.map(|s| s.as_str()))
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let items = rows
        .iter()
        .map(from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)?;
    Ok(Json(OnboardingList { items }))
}

/// One onboarding case (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/onboarding-cases/{id}", operation_id = "people_onboarding_get", tag = "people-hr",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = OnboardingView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn get(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let o = load(&mut conn, ctx.scope.tenant_id, id, false).await?;
    Ok(with_etag(StatusCode::OK, &o, Version(o.version)))
}

async fn case_scope(
    st: &PeopleState,
    ctx: &AuthContext,
    id: Uuid,
) -> Result<(OnboardingView, ExecutionScope), KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let o = load(&mut conn, ctx.scope.tenant_id, id, false).await?;
    let institution = InstitutionId(o.institution_id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(st, &scope, institution).await?;
    Ok((o, scope))
}

/// Record onboarding documents (`people.staff.manage`; If-Match); optionally hand them to
/// verification (422 without documents).
#[utoipa::path(post, path = "/api/v1/people/onboarding-cases/{id}/documents", operation_id = "people_onboarding_documents", tag = "people-hr",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the onboarding case")),
    request_body = DocumentsBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = OnboardingView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn documents(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<DocumentsBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    let v = expected!(headers);
    let mut docs = Vec::with_capacity(b.documents.len());
    for (i, d) in b.documents.iter().enumerate() {
        let ty = d.document_type.trim().to_lowercase();
        if !DOC_TYPES.contains(&ty.as_str()) {
            return Err(invalid(
                &format!("documents[{i}].documentType"),
                "invalid",
                "identity, address, education, employment, medical or other",
            )
            .into());
        }
        let reference = d.reference.trim();
        if reference.is_empty() || reference.chars().count() > 200 {
            return Err(invalid(
                &format!("documents[{i}].reference"),
                "required",
                "1 to 200 characters",
            )
            .into());
        }
        docs.push(OnboardingDocument {
            document_type: ty,
            reference: reference.to_owned(),
        });
    }
    let t = ctx.scope.tenant_id;
    let (_, scope) = case_scope(&st, ctx, id).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let o = load(&mut tx, t, id, true).await?;
    check_version(v, o.version)?;
    if !matches!(
        o.status,
        OnboardingStatus::Initiated | OnboardingStatus::DocumentationPending
    ) {
        return Err(common::illegal(
            "onboarding case",
            &o.number,
            o.status.as_str(),
            "documentation_pending",
        )
        .into());
    }
    let mut all = o.documents.clone();
    for d in docs {
        if !all.contains(&d) {
            all.push(d);
        }
    }
    if b.submit_for_verification && all.is_empty() {
        return Err(invalid(
            "documents",
            "required",
            "submit at least one document for verification",
        )
        .into());
    }
    sqlx::query("UPDATE people.onboarding_case SET documents = $2, documents_verified = false WHERE id = $1")
        .bind(id)
        .bind(serde_json::to_value(&all).map_err(KernelError::internal)?)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let to = if b.submit_for_verification {
        OnboardingStatus::VerificationPending
    } else {
        OnboardingStatus::DocumentationPending
    };
    set_status(&mut tx, t, &o, to, None, &actor).await?;
    bump(&mut tx, id).await?;
    common::audit_only(
        &mut tx,
        &scope,
        "onboarding_case",
        id,
        "people.onboarding_case.documents",
        serde_json::json!({ "documents": all.len(), "submitted": b.submit_for_verification }),
        &actor,
    )
    .await?;
    let out = load(&mut tx, t, id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &out, Version(out.version)))
}

/// Verify the submitted documents (`people.staff.manage`; If-Match): approved marks them
/// verified, otherwise the case returns to documentation (reason required).
#[utoipa::path(post, path = "/api/v1/people/onboarding-cases/{id}/verify", operation_id = "people_onboarding_verify", tag = "people-hr",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the onboarding case")),
    request_body = VerifyBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = OnboardingView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn verify(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<VerifyBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    let v = expected!(headers);
    let reason = if b.approved {
        clean(b.reason)
    } else {
        Some(required_reason(b.reason)?)
    };
    let t = ctx.scope.tenant_id;
    let (_, scope) = case_scope(&st, ctx, id).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let o = load(&mut tx, t, id, true).await?;
    check_version(v, o.version)?;
    if o.status != OnboardingStatus::VerificationPending {
        return Err(
            common::illegal("onboarding case", &o.number, o.status.as_str(), "verified").into(),
        );
    }
    sqlx::query("UPDATE people.onboarding_case SET documents_verified = $2 WHERE id = $1")
        .bind(id)
        .bind(b.approved)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    if !b.approved {
        set_status(
            &mut tx,
            t,
            &o,
            OnboardingStatus::DocumentationPending,
            reason.as_deref(),
            &actor,
        )
        .await?;
    }
    bump(&mut tx, id).await?;
    common::audit_only(
        &mut tx,
        &scope,
        "onboarding_case",
        id,
        "people.onboarding_case.verify",
        serde_json::json!({ "approved": b.approved, "reason": reason }),
        &actor,
    )
    .await?;
    let out = load(&mut tx, t, id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &out, Version(out.version)))
}

/// Mark the case ready for joining (`people.staff.manage`; If-Match): documents must be verified.
#[utoipa::path(post, path = "/api/v1/people/onboarding-cases/{id}/ready", operation_id = "people_onboarding_ready", tag = "people-hr",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the onboarding case")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = OnboardingView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn ready(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    let v = expected!(headers);
    let reason = reason_of(body);
    let t = ctx.scope.tenant_id;
    let (_, scope) = case_scope(&st, ctx, id).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let o = load(&mut tx, t, id, true).await?;
    check_version(v, o.version)?;
    if o.status == OnboardingStatus::VerificationPending && !o.documents_verified {
        return Err(KernelError::Conflict(format!(
            "onboarding case {}: documents are not verified yet",
            o.number
        ))
        .into());
    }
    if o.status != OnboardingStatus::VerificationPending {
        return Err(common::illegal(
            "onboarding case",
            &o.number,
            o.status.as_str(),
            "ready_for_joining",
        )
        .into());
    }
    set_status(
        &mut tx,
        t,
        &o,
        OnboardingStatus::ReadyForJoining,
        reason.as_deref(),
        &actor,
    )
    .await?;
    bump(&mut tx, id).await?;
    common::audit_only(
        &mut tx,
        &scope,
        "onboarding_case",
        id,
        "people.onboarding_case.ready",
        serde_json::json!({}),
        &actor,
    )
    .await?;
    let out = load(&mut tx, t, id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &out, Version(out.version)))
}

/// Joining: staff record + pending engagement, submitted for activation.
#[allow(clippy::too_many_arguments)]
async fn complete_join(
    conn: &mut PgConnection,
    tenant: TenantId,
    scope: &ExecutionScope,
    o: &OnboardingView,
    engagement_outcome: &ApprovalOutcome,
    today: NaiveDate,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<(), KernelError> {
    let institution = InstitutionId(o.institution_id);
    let c = load_candidate(conn, tenant, o.candidate_id, true).await?;
    let r = load_requisition(conn, tenant, o.requisition_id, true).await?;
    let taken: Option<String> = sqlx::query_scalar(
        "SELECT staff_number FROM people.staff_member WHERE tenant_id = $1 \
         AND lower(work_email) = lower($2) AND status NOT IN ('merged', 'archived') LIMIT 1",
    )
    .bind(tenant.as_uuid())
    .bind(&c.email)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    if let Some(n) = taken {
        return Err(KernelError::Conflict(format!(
            "staff record {n} already uses {} (possible duplicate); resolve it before joining",
            c.email
        )));
    }
    let staff = insert_staff(
        conn,
        &NewStaff {
            tenant,
            institution,
            subject_ref: o.subject_ref.as_deref(),
            given_name: &c.given_name,
            family_name: c.family_name.as_deref(),
            display_name: None,
            work_email: Some(&c.email),
            phone: c.phone.as_deref(),
            date_of_birth: None,
            gender: None,
            staff_category: r.staff_category,
            status: StaffStatus::Active,
        },
        actor,
    )
    .await?;
    emit(
        conn,
        scope,
        ev::STAFF_CREATED,
        ev::STAFF_CREATED_SCHEMA,
        "staff_member",
        staff.id.as_uuid(),
        "people.staff_member.create",
        &modified(
            staff.id.as_uuid(),
            institution,
            staff.version,
            &[
                "givenName",
                "displayName",
                "workEmail",
                "staffCategory",
                "status",
                "onboardingCase",
            ],
        ),
        actor,
    )
    .await?;
    if let Some(m) = o.reports_to_staff_id {
        validate_manager(conn, tenant, &staff, StaffMemberId(m)).await?;
    }
    let designation = c
        .offered_designation
        .clone()
        .unwrap_or_else(|| r.designation.clone());
    let grade = c.offered_grade.clone().or_else(|| r.grade.clone());
    let e = insert_engagement(
        conn,
        &NewEngagement {
            tenant,
            institution,
            staff_id: staff.id,
            org_unit_id: o.org_unit_id,
            campus_id: o.campus_id,
            staff_category: r.staff_category,
            employment_type: r.employment_type,
            designation: &designation,
            grade: grade.as_deref(),
            start_date: o.joining_date,
            end_date: None,
            probation_end_date: o.probation_end_date,
            reports_to_staff_id: o.reports_to_staff_id.map(StaffMemberId),
            rejoin_of: None,
            status: EngagementStatus::Pending,
        },
        actor,
    )
    .await?;
    open_change(
        conn,
        tenant,
        scope,
        &e,
        ChangeKind::Activation,
        &EngagementChanges::default(),
        Some(o.joining_date),
        Some("onboarding"),
        engagement_outcome,
        today,
        actor,
    )
    .await?;
    sqlx::query(
        "UPDATE people.onboarding_case SET status = 'joined', staff_id = $2, engagement_id = $3, \
         status_reason = $4, version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(o.id)
    .bind(staff.id.as_uuid())
    .bind(e.id.as_uuid())
    .bind(reason)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    history(
        conn,
        tenant,
        institution,
        "onboarding_case",
        o.id,
        Some(o.status.as_str()),
        "joined",
        ev::ONBOARDING_COMPLETED,
        reason,
        o.process_instance_id.as_deref(),
        actor,
    )
    .await?;
    let version: i64 =
        sqlx::query_scalar("SELECT version FROM people.onboarding_case WHERE id = $1")
            .bind(o.id)
            .fetch_one(&mut *conn)
            .await
            .map_err(map_db_error)?;
    emit(
        conn,
        scope,
        ev::ONBOARDING_COMPLETED,
        ev::ONBOARDING_COMPLETED_SCHEMA,
        "onboarding_case",
        o.id,
        "people.onboarding_case.join",
        &status_payload(
            o.id,
            institution,
            Some(&staff),
            Some(&e),
            Some(o.status.as_str()),
            "joined",
            version,
            reason,
            o.process_instance_id.as_deref(),
            None,
        ),
        actor,
    )
    .await?;
    let joined: i64 = sqlx::query_scalar("SELECT count(*) FROM people.onboarding_case WHERE requisition_id = $1 AND status = 'joined'")
        .bind(r.id)
        .fetch_one(&mut *conn)
        .await
        .map_err(map_db_error)?;
    if r.status == RequisitionStatus::Open && joined >= i64::from(r.openings) {
        set_requisition(
            conn,
            tenant,
            &r,
            RequisitionStatus::Filled,
            Some("all openings joined"),
            None,
            actor,
        )
        .await?;
    }
    Ok(())
}

async fn engagement_approval(
    st: &PeopleState,
    scope: &ExecutionScope,
    o: &OnboardingView,
) -> Result<ApprovalOutcome, KernelError> {
    let mut vars = BTreeMap::new();
    vars.insert("onboardingCase".to_owned(), serde_json::json!(o.number));
    request_approval(
        st,
        scope,
        process::ENGAGEMENT_ACTIVATION,
        business_key("onboarding_engagement", o.id, o.version),
        vars,
    )
    .await
}

/// Join: request the acceptance (`people.staff.manage`; If-Match; `ONBOARDING_ACCEPTANCE`). On
/// approval the staff record and a pending engagement are created and the engagement is
/// submitted for activation.
#[utoipa::path(post, path = "/api/v1/people/onboarding-cases/{id}/join", operation_id = "people_onboarding_join", tag = "people-hr",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the onboarding case")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = OnboardingView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn join(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    let v = expected!(headers);
    let reason = reason_of(body);
    let t = ctx.scope.tenant_id;
    let (pre, scope) = case_scope(&st, ctx, id).await?;
    check_version(v, pre.version)?;
    if pre.status != OnboardingStatus::ReadyForJoining || pre.process_instance_id.is_some() {
        return Err(common::illegal(
            "onboarding case",
            &pre.number,
            pre.status.as_str(),
            "joined",
        )
        .into());
    }
    let today = business_today(&st, &scope, InstitutionId(pre.institution_id)).await?;
    let mut vars = BTreeMap::new();
    vars.insert("onboardingCase".to_owned(), serde_json::json!(pre.number));
    let outcome = request_approval(
        &st,
        &scope,
        process::ONBOARDING_ACCEPTANCE,
        business_key("onboarding_case", pre.id, pre.version),
        vars,
    )
    .await?;
    let eng_outcome = engagement_approval(&st, &scope, &pre).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let o = load(&mut tx, t, id, true).await?;
    check_version(v, o.version)?;
    let pid = outcome.process_instance_id();
    sqlx::query("UPDATE people.onboarding_case SET process_instance_id = $2, version = version + 1, updated_at = now() WHERE id = $1")
        .bind(id)
        .bind(pid)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let o = load(&mut tx, t, id, true).await?;
    emit(
        &mut tx,
        &scope,
        ev::ONBOARDING_SUBMITTED,
        ev::ONBOARDING_SUBMITTED_SCHEMA,
        "onboarding_case",
        id,
        "people.onboarding_case.submit",
        &status_payload(
            id,
            InstitutionId(o.institution_id),
            None,
            None,
            Some(o.status.as_str()),
            o.status.as_str(),
            o.version,
            reason.as_deref(),
            Some(pid),
            None,
        ),
        &actor,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        complete_join(
            &mut tx,
            t,
            &scope,
            &o,
            &eng_outcome,
            today,
            reason.as_deref(),
            &Actor::system(actor.correlation_id.clone()),
        )
        .await?;
    }
    let out = load(&mut tx, t, id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &out, Version(out.version)))
}

/// Decide a joining awaiting acceptance (`people.approve`, step-up; If-Match). Rejection keeps
/// the case ready for a new attempt.
#[utoipa::path(post, path = "/api/v1/people/onboarding-cases/{id}/decision", operation_id = "people_onboarding_decide", tag = "people-hr",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the onboarding case")),
    request_body = DecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = OnboardingView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn decide(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<DecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleApprove)?;
    let v = expected!(headers);
    let reason = clean(b.reason);
    if b.decision == Decision::Reject && reason.is_none() {
        return Err(invalid("reason", "required", "a rejection needs a reason").into());
    }
    let t = ctx.scope.tenant_id;
    let (pre, scope) = case_scope(&st, ctx, id).await?;
    check_version(v, pre.version)?;
    if pre.status != OnboardingStatus::ReadyForJoining || pre.process_instance_id.is_none() {
        return Err(KernelError::Conflict(format!(
            "onboarding case {} has no joining awaiting a decision",
            pre.number
        ))
        .into());
    }
    let today = business_today(&st, &scope, InstitutionId(pre.institution_id)).await?;
    let eng_outcome = engagement_approval(&st, &scope, &pre).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let o = load(&mut tx, t, id, true).await?;
    check_version(v, o.version)?;
    if b.decision == Decision::Approve {
        complete_join(
            &mut tx,
            t,
            &scope,
            &o,
            &eng_outcome,
            today,
            reason.as_deref(),
            &actor,
        )
        .await?;
    } else {
        sqlx::query("UPDATE people.onboarding_case SET process_instance_id = NULL, status_reason = $2, version = version + 1, updated_at = now() WHERE id = $1")
            .bind(id)
            .bind(reason.as_deref())
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
        common::audit_only(
            &mut tx,
            &scope,
            "onboarding_case",
            id,
            "people.onboarding_case.reject",
            serde_json::json!({ "reason": reason }),
            &actor,
        )
        .await?;
    }
    let out = load(&mut tx, t, id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &out, Version(out.version)))
}

/// Cancel an onboarding case before joining (`people.staff.manage`; If-Match; reason required).
#[utoipa::path(post, path = "/api/v1/people/onboarding-cases/{id}/cancel", operation_id = "people_onboarding_cancel", tag = "people-hr",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the onboarding case")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = OnboardingView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn cancel(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    let v = expected!(headers);
    let reason = required_reason(reason_of(body))?;
    let t = ctx.scope.tenant_id;
    let (_, scope) = case_scope(&st, ctx, id).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let o = load(&mut tx, t, id, true).await?;
    check_version(v, o.version)?;
    set_status(
        &mut tx,
        t,
        &o,
        OnboardingStatus::Cancelled,
        Some(reason.as_str()),
        &actor,
    )
    .await?;
    sqlx::query("UPDATE people.onboarding_case SET process_instance_id = NULL WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    bump(&mut tx, id).await?;
    common::audit_only(
        &mut tx,
        &scope,
        "onboarding_case",
        id,
        "people.onboarding_case.cancel",
        serde_json::json!({ "reason": reason }),
        &actor,
    )
    .await?;
    let out = load(&mut tx, t, id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &out, Version(out.version)))
}

pub fn router(state: PeopleState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(create))
        .routes(routes!(list))
        .routes(routes!(get))
        .routes(routes!(documents))
        .routes(routes!(verify))
        .routes(routes!(ready))
        .routes(routes!(join))
        .routes(routes!(decide))
        .routes(routes!(cancel))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn onboarding_machine() {
        use OnboardingStatus::*;
        assert!(onboarding_can_transition(Initiated, DocumentationPending));
        assert!(onboarding_can_transition(
            DocumentationPending,
            VerificationPending
        ));
        assert!(onboarding_can_transition(
            VerificationPending,
            ReadyForJoining
        ));
        assert!(onboarding_can_transition(ReadyForJoining, Joined));
        assert!(!onboarding_can_transition(Initiated, Joined));
        assert!(!onboarding_can_transition(Joined, Cancelled));
    }
}
