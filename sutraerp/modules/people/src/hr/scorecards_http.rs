//! **ERP_EXTENSIONS 3B** — hiring scorecards over requisitions and candidates (rules in
//! [`super::scorecards`], ported from HeyPuter/everything hiring, MIT). Operation ids
//! `people_recruitment_*` (tag `people-hr`).
//!
//! The rubric a requisition is rated on is its own, else the institution's, else the default
//! four. A scorecard snapshots that rubric and the guide questions it answered. Feedback rolls
//! up into per-dimension averages, recommendation counts and a hire / no-hire / split leaning;
//! interviews held more than a day ago without a scorecard show as feedback due.
#![allow(clippy::type_complexity)]

use super::scorecards::{
    self as sc, Answer, Guide, GuideAnswer, RatingField, ScreeningQuestion, Summary,
};
use crate::leave::common::{audit, invalid};
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::actor;
use crate::shared::scope::institution_scope;
use crate::shared::time::business_today;
use crate::shared::PeopleState;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sqlx::PgConnection;
use std::collections::HashSet;
use sutra_kernel::auth::Permission;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

// ------------------------------------------------------------------ loading

async fn settings_row(
    conn: &mut PgConnection,
    t: TenantId,
    inst: Uuid,
) -> Result<(Value, Value), KernelError> {
    let row: Option<(Option<Value>, Value)> = sqlx::query_as(
        "SELECT rating_fields, guides FROM people.hiring_settings WHERE tenant_id = $1 AND institution_id = $2",
    )
    .bind(t.as_uuid())
    .bind(inst)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(row.map_or((Value::Null, Value::Array(vec![])), |(f, g)| {
        (f.unwrap_or(Value::Null), g)
    }))
}

struct Req {
    institution_id: Uuid,
    title: String,
    fields: Value,
    questions: Value,
}

async fn requisition(conn: &mut PgConnection, t: TenantId, id: Uuid) -> Result<Req, KernelError> {
    let row: Option<(Uuid, String, Option<Value>, Option<Value>)> = sqlx::query_as(
        "SELECT institution_id, title, rating_fields, screening_questions FROM people.requisition \
         WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(t.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let (institution_id, title, f, q) = row.ok_or(KernelError::NotFound {
        entity: "requisition",
    })?;
    Ok(Req {
        institution_id,
        title,
        fields: f.unwrap_or(Value::Null),
        questions: q.unwrap_or(Value::Null),
    })
}

/// The rubric in force for a requisition: its own, the institution's, or the default.
async fn rubric(
    conn: &mut PgConnection,
    t: TenantId,
    req: &Req,
) -> Result<Vec<RatingField>, KernelError> {
    if let Some(f) = sc::normalize_rating_fields(&req.fields) {
        return Ok(f);
    }
    let (inst_fields, _) = settings_row(conn, t, req.institution_id).await?;
    Ok(sc::fields_or_default(&inst_fields))
}

struct Cand {
    institution_id: Uuid,
    requisition_id: Uuid,
    name: String,
    status: String,
    answers: Value,
}

async fn candidate(conn: &mut PgConnection, t: TenantId, id: Uuid) -> Result<Cand, KernelError> {
    let row: Option<(Uuid, Uuid, String, Option<String>, String, Option<Value>)> = sqlx::query_as(
        "SELECT institution_id, requisition_id, given_name, family_name, status, screening_answers \
         FROM people.candidate WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(t.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let (institution_id, requisition_id, given, family, status, answers) =
        row.ok_or(KernelError::NotFound {
            entity: "candidate",
        })?;
    let name = family.map_or(given.clone(), |f| format!("{given} {f}"));
    Ok(Cand {
        institution_id,
        requisition_id,
        name,
        status,
        answers: answers.unwrap_or(Value::Null),
    })
}

fn json<T: Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or_default()
}

// ------------------------------------------------------------------ settings

/// Institution hiring settings: the default rubric and the interview guides.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HiringSettings {
    /// The institution's rubric; null = the default four (role skills, experience,
    /// communication, collaboration).
    pub rating_fields: Option<Vec<RatingField>>,
    pub guides: Vec<Guide>,
}

/// Hiring settings (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/institutions/{id}/hiring-settings", operation_id = "people_recruitment_hiring_settings_get", tag = "people-hr",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = HiringSettings), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn settings_get(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<HiringSettings>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let inst = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let (f, g) = settings_row(&mut conn, ctx.scope.tenant_id, id).await?;
    Ok(Json(HiringSettings {
        rating_fields: sc::normalize_rating_fields(&f),
        guides: sc::normalize_guides(&g),
    }))
}

/// Save hiring settings (`staff.manage`). Entries without a safe id or a label / question text
/// are dropped, as Everything does.
#[utoipa::path(put, path = "/api/v1/people/institutions/{id}/hiring-settings", operation_id = "people_recruitment_hiring_settings_put", tag = "people-hr",
    params(("id" = String, Path, format = Uuid)), request_body = HiringSettings, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = HiringSettings), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn settings_put(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(b): Json<Value>,
) -> ApiResult<Json<HiringSettings>> {
    let ctx = auth.require(Permission::StaffManage)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let fields = sc::normalize_rating_fields(b.get("ratingFields").unwrap_or(&Value::Null));
    let guides = sc::normalize_guides(b.get("guides").unwrap_or(&Value::Null));
    let t = ctx.scope.tenant_id;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    sqlx::query(
        "INSERT INTO people.hiring_settings (tenant_id, institution_id, rating_fields, guides) VALUES ($1, $2, $3, $4) \
         ON CONFLICT (tenant_id, institution_id) DO UPDATE SET rating_fields = EXCLUDED.rating_fields, \
         guides = EXCLUDED.guides, version = people.hiring_settings.version + 1, updated_at = now()",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(fields.as_ref().map(json))
    .bind(json(&guides))
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let out = HiringSettings {
        rating_fields: fields,
        guides,
    };
    let a = actor(ctx);
    audit(
        &mut tx,
        &scope,
        "people.hiring_settings.update",
        "institution",
        id,
        json(&out),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(out))
}

/// A requisition's hiring form.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HiringForm {
    pub requisition_id: Uuid,
    /// The requisition's own rubric (null = the institution's or the default).
    pub rating_fields: Option<Vec<RatingField>>,
    /// The rubric in force.
    pub rubric: Vec<RatingField>,
    pub screening_questions: Vec<ScreeningQuestion>,
}

/// The body of a hiring form (entries without a safe id or label are dropped).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HiringFormInput {
    pub rating_fields: Option<Vec<RatingField>>,
    pub screening_questions: Option<Vec<ScreeningQuestion>>,
}

/// Set a requisition's rubric and screening questions (`staff.manage`).
#[utoipa::path(put, path = "/api/v1/people/requisitions/{id}/hiring-form", operation_id = "people_recruitment_requisition_hiring_form", tag = "people-hr",
    params(("id" = String, Path, format = Uuid)), request_body = HiringFormInput, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = HiringForm), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn hiring_form(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(rid): Path<Uuid>,
    Json(b): Json<Value>,
) -> ApiResult<Json<HiringForm>> {
    let ctx = auth.require(Permission::StaffManage)?;
    let t = ctx.scope.tenant_id;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let req = requisition(&mut tx, t, rid).await?;
    let inst = InstitutionId(req.institution_id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let fields = sc::normalize_rating_fields(b.get("ratingFields").unwrap_or(&Value::Null));
    let questions =
        sc::normalize_screening_questions(b.get("screeningQuestions").unwrap_or(&Value::Null));
    sqlx::query(
        "UPDATE people.requisition SET rating_fields = $3, screening_questions = $4, updated_at = now() \
         WHERE id = $1 AND tenant_id = $2",
    )
    .bind(rid)
    .bind(t.as_uuid())
    .bind(fields.as_ref().map(json))
    .bind(questions.as_ref().map(json))
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let req = Req {
        fields: fields.as_ref().map_or(Value::Null, json),
        ..req
    };
    let rub = rubric(&mut tx, t, &req).await?;
    let a = actor(ctx);
    let details = serde_json::json!({"ratingFields": fields, "screeningQuestions": questions});
    audit(
        &mut tx,
        &scope,
        "people.requisition.hiring_form",
        "requisition",
        rid,
        details,
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(HiringForm {
        requisition_id: rid,
        rating_fields: fields,
        rubric: rub,
        screening_questions: questions.unwrap_or_default(),
    }))
}

/// Screening answers.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ScreeningAnswersInput {
    pub answers: Vec<Answer>,
}

/// Record a candidate's screening answers (`staff.manage`); stored with the question text.
#[utoipa::path(put, path = "/api/v1/people/candidates/{id}/screening-answers", operation_id = "people_recruitment_candidate_screening_answers", tag = "people-hr",
    params(("id" = String, Path, format = Uuid)), request_body = ScreeningAnswersInput, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 204), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn screening_answers(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(cid): Path<Uuid>,
    Json(b): Json<Value>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    let t = ctx.scope.tenant_id;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let c = candidate(&mut tx, t, cid).await?;
    let inst = InstitutionId(c.institution_id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let answers = sc::normalize_answers(b.get("answers").unwrap_or(&Value::Null));
    sqlx::query("UPDATE people.candidate SET screening_answers = $3, updated_at = now() WHERE id = $1 AND tenant_id = $2")
        .bind(cid)
        .bind(t.as_uuid())
        .bind(answers.as_ref().map(json))
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let a = actor(ctx);
    let details = serde_json::json!({"answers": answers});
    audit(
        &mut tx,
        &scope,
        "people.candidate.screening_answers",
        "candidate",
        cid,
        details,
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

// ------------------------------------------------------------------ interviews

/// Schedule an interview.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CandidateInterviewInput {
    /// `phone | video | onsite`.
    #[serde(rename = "type")]
    pub kind: String,
    pub date: NaiveDate,
    #[schema(example = "11:00")]
    pub start: String,
    #[schema(example = "11:45")]
    pub end: String,
    #[serde(default)]
    pub interviewers: Vec<String>,
    pub location: Option<String>,
    /// A guide id; default: the best guide for the format and the candidate's stage.
    pub guide_id: Option<String>,
}

/// An interview.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CandidateInterviewView {
    pub id: Uuid,
    pub candidate_id: Uuid,
    #[serde(rename = "type")]
    pub kind: String,
    pub date: NaiveDate,
    pub start: String,
    pub end: String,
    pub interviewers: Vec<String>,
    pub location: Option<String>,
    pub guide: Option<Guide>,
    /// `scheduled | cancelled`.
    pub status: String,
    /// Whether a scorecard is linked to it.
    pub has_scorecard: bool,
}

fn valid_time(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 5
        && b[2] == b':'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 2 || c.is_ascii_digit())
        && s[..2].parse::<u8>().is_ok_and(|h| h < 24)
        && s[3..].parse::<u8>().is_ok_and(|m| m < 60)
}

/// Schedule an interview (`staff.manage`; Idempotency-Key).
#[utoipa::path(post, path = "/api/v1/people/candidates/{id}/interviews", operation_id = "people_recruitment_candidate_interview_schedule", tag = "people-hr",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = CandidateInterviewInput, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = CandidateInterviewView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn interview_schedule(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(cid): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<CandidateInterviewInput>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    require_idempotency_key(&headers)?;
    if !sc::INTERVIEW_TYPES.iter().any(|t| t.0 == b.kind) {
        return Err(invalid("type", "invalid", "phone, video or onsite").into());
    }
    if !valid_time(&b.start) || !valid_time(&b.end) || b.end <= b.start {
        return Err(invalid("end", "invalid", "start and end as HH:MM, end after start").into());
    }
    let interviewers: Vec<String> = b
        .interviewers
        .iter()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .take(10)
        .collect();
    let t = ctx.scope.tenant_id;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let c = candidate(&mut tx, t, cid).await?;
    if matches!(
        c.status.as_str(),
        "rejected" | "withdrawn" | "offer_declined"
    ) {
        return Err(KernelError::Conflict(format!(
            "the candidate is {}",
            c.status.replace('_', " ")
        ))
        .into());
    }
    let inst = InstitutionId(c.institution_id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let (_, g) = settings_row(&mut tx, t, c.institution_id).await?;
    let guides = sc::normalize_guides(&g);
    let guide = match b.guide_id.as_deref().filter(|g| !g.is_empty()) {
        Some(id) => Some(
            guides
                .iter()
                .find(|g| g.id == id)
                .ok_or_else(|| invalid("guideId", "unknown", "no such guide"))?
                .clone(),
        ),
        None => sc::default_guide_for(&guides, &b.kind, &c.status).cloned(),
    };
    let location = b
        .location
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().take(200).collect::<String>());
    let iid: Uuid = sqlx::query_scalar(
        "INSERT INTO people.candidate_interview (tenant_id, institution_id, candidate_id, interview_type, \
         interview_date, start_time, end_time, interviewers, location, guide_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(c.institution_id)
    .bind(cid)
    .bind(&b.kind)
    .bind(b.date)
    .bind(&b.start)
    .bind(&b.end)
    .bind(&interviewers)
    .bind(&location)
    .bind(guide.as_ref().map(|g| g.id.clone()))
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let a = actor(ctx);
    let details = serde_json::json!({
        "interviewId": iid, "type": b.kind, "date": b.date, "start": b.start,
        "guide": guide.as_ref().map(|g| &g.id)
    });
    audit(
        &mut tx,
        &scope,
        "people.candidate_interview.schedule",
        "candidate",
        cid,
        details,
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let view = CandidateInterviewView {
        id: iid,
        candidate_id: cid,
        kind: b.kind,
        date: b.date,
        start: b.start,
        end: b.end,
        interviewers,
        location,
        guide,
        status: "scheduled".into(),
        has_scorecard: false,
    };
    Ok((StatusCode::CREATED, Json(view)).into_response())
}

/// Cancel an interview (`staff.manage`); a cancelled interview owes no feedback.
#[utoipa::path(post, path = "/api/v1/people/candidate-interviews/{id}/cancel", operation_id = "people_recruitment_candidate_interview_cancel", tag = "people-hr",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 204), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn interview_cancel(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(iid): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    let t = ctx.scope.tenant_id;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let row: Option<(Uuid, Uuid, String)> = sqlx::query_as(
        "SELECT institution_id, candidate_id, status FROM people.candidate_interview \
         WHERE id = $1 AND tenant_id = $2 FOR UPDATE",
    )
    .bind(iid)
    .bind(t.as_uuid())
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let (inst_id, cid, status) = row.ok_or(KernelError::NotFound {
        entity: "interview",
    })?;
    let inst = InstitutionId(inst_id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    if status == "cancelled" {
        return Err(KernelError::Conflict("the interview is already cancelled".into()).into());
    }
    sqlx::query("UPDATE people.candidate_interview SET status = 'cancelled', version = version + 1 WHERE id = $1")
        .bind(iid)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let a = actor(ctx);
    let details = serde_json::json!({"interviewId": iid});
    audit(
        &mut tx,
        &scope,
        "people.candidate_interview.cancel",
        "candidate",
        cid,
        details,
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

// ------------------------------------------------------------------ scorecards

/// Write a scorecard.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScorecardInput {
    /// The interview it is for (optional).
    pub interview_id: Option<Uuid>,
    /// The reviewer's name as it should read.
    pub author_name: String,
    /// `strong_yes | yes | no | strong_no`.
    pub recommendation: String,
    /// Rubric key → 1–5 (or null when not rated).
    #[serde(default)]
    #[schema(value_type = Object)]
    pub ratings: Map<String, Value>,
    #[serde(default)]
    pub guide_answers: Vec<GuideAnswer>,
    pub notes: Option<String>,
}

/// A scorecard.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScorecardView {
    pub id: Uuid,
    pub candidate_id: Uuid,
    pub interview_id: Option<Uuid>,
    pub author_name: String,
    pub recommendation: String,
    #[schema(value_type = Object)]
    pub ratings: Map<String, Value>,
    /// The rubric it was written with.
    pub fields: Vec<RatingField>,
    pub guide_answers: Vec<GuideAnswer>,
    pub notes: Option<String>,
    pub average: Option<f64>,
    pub created_on: NaiveDate,
}

/// Record interview feedback (`staff.manage`; Idempotency-Key). Ratings are 1–5 on the rubric
/// in force, which the scorecard snapshots.
#[utoipa::path(post, path = "/api/v1/people/candidates/{id}/scorecards", operation_id = "people_recruitment_candidate_scorecard_create", tag = "people-hr",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = ScorecardInput, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = ScorecardView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn scorecard_create(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(cid): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ScorecardInput>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    require_idempotency_key(&headers)?;
    let errors = sc::validate_scorecard(
        &b.author_name,
        &Value::String(b.recommendation.clone()),
        &b.ratings,
    );
    if let Some(first) = errors.first() {
        return Err(invalid("scorecard", "invalid", first).into());
    }
    let t = ctx.scope.tenant_id;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let c = candidate(&mut tx, t, cid).await?;
    let inst = InstitutionId(c.institution_id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let req = requisition(&mut tx, t, c.requisition_id).await?;
    let fields = rubric(&mut tx, t, &req).await?;
    let keys: HashSet<&str> = fields.iter().map(|f| f.key.as_str()).collect();
    if let Some(k) = b.ratings.keys().find(|k| !keys.contains(k.as_str())) {
        return Err(invalid(
            "ratings",
            "unknown_field",
            &format!("{k} is not on this rubric"),
        )
        .into());
    }
    let ratings: Map<String, Value> = b
        .ratings
        .into_iter()
        .filter(|(_, v)| !v.is_null())
        .collect();
    if let Some(iid) = b.interview_id {
        let ok: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM people.candidate_interview WHERE id = $1 AND candidate_id = $2 AND tenant_id = $3",
        )
        .bind(iid)
        .bind(cid)
        .bind(t.as_uuid())
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_db_error)?;
        if ok.is_none() {
            return Err(invalid(
                "interviewId",
                "unknown",
                "not an interview of this candidate",
            )
            .into());
        }
    }
    let answers = sc::normalize_guide_answers(&json(&b.guide_answers));
    let notes = b
        .notes
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().take(4000).collect::<String>());
    let today = business_today(&st, &scope, inst).await?;
    let author = b.author_name.trim().chars().take(120).collect::<String>();
    let sid: Uuid = sqlx::query_scalar(
        "INSERT INTO people.candidate_scorecard (tenant_id, institution_id, candidate_id, interview_id, author_name, \
         author_subject, recommendation, ratings, fields, guide_answers, notes, created_on) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(c.institution_id)
    .bind(cid)
    .bind(b.interview_id)
    .bind(&author)
    .bind(ctx.subject.to_string())
    .bind(&b.recommendation)
    .bind(Value::Object(ratings.clone()))
    .bind(json(&fields))
    .bind(answers.as_ref().map(json))
    .bind(&notes)
    .bind(today)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let a = actor(ctx);
    let details = serde_json::json!({
        "scorecardId": sid, "recommendation": b.recommendation, "interviewId": b.interview_id
    });
    audit(
        &mut tx,
        &scope,
        "people.candidate_scorecard.create",
        "candidate",
        cid,
        details,
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let card = sc::Scorecard {
        ratings: ratings.clone(),
        ..sc::Scorecard::default()
    };
    let view = ScorecardView {
        id: sid,
        candidate_id: cid,
        interview_id: b.interview_id,
        author_name: author,
        recommendation: b.recommendation,
        average: sc::average_rating(&card),
        ratings,
        fields,
        guide_answers: answers.unwrap_or_default(),
        notes,
        created_on: today,
    };
    Ok((StatusCode::CREATED, Json(view)).into_response())
}

async fn scorecards_of(
    conn: &mut PgConnection,
    t: TenantId,
    where_sql: &str,
    id: Uuid,
) -> Result<Vec<ScorecardView>, KernelError> {
    let rows: Vec<(Uuid, Uuid, Option<Uuid>, String, String, Value, Value, Option<Value>, Option<String>, NaiveDate)> =
        sqlx::query_as(&format!(
            "SELECT id, candidate_id, interview_id, author_name, recommendation, ratings, fields, guide_answers, notes, \
             created_on FROM people.candidate_scorecard WHERE tenant_id = $1 AND {where_sql} ORDER BY created_at, id"
        ))
        .bind(t.as_uuid())
        .bind(id)
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db_error)?;
    Ok(rows
        .into_iter()
        .map(
            |(id, cid, iid, author, rec, ratings, fields, answers, notes, on)| {
                let ratings = ratings.as_object().cloned().unwrap_or_default();
                let card = sc::Scorecard {
                    ratings: ratings.clone(),
                    ..sc::Scorecard::default()
                };
                ScorecardView {
                    id,
                    candidate_id: cid,
                    interview_id: iid,
                    author_name: author,
                    recommendation: rec,
                    average: sc::average_rating(&card),
                    ratings,
                    fields: sc::fields_or_default(&fields),
                    guide_answers: answers
                        .as_ref()
                        .and_then(sc::normalize_guide_answers)
                        .unwrap_or_default(),
                    notes,
                    created_on: on,
                }
            },
        )
        .collect())
}

fn as_cards(v: &[ScorecardView]) -> Vec<sc::Scorecard> {
    v.iter()
        .map(|s| sc::Scorecard {
            candidate_id: s.candidate_id.to_string(),
            interview_id: s.interview_id.map(|i| i.to_string()),
            recommendation: Some(s.recommendation.clone()),
            ratings: s.ratings.clone(),
            created_on: Some(s.created_on.to_string()),
        })
        .collect()
}

/// Screening on a candidate.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CandidateScreening {
    pub questions: Vec<ScreeningQuestion>,
    pub answers: Vec<Answer>,
    /// Must-have questions answered with something other than yes.
    pub mismatches: Vec<String>,
}

/// A candidate's feedback file.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CandidateFeedback {
    pub candidate_id: Uuid,
    pub name: String,
    pub status: String,
    pub requisition_title: String,
    pub rubric: Vec<RatingField>,
    pub interviews: Vec<CandidateInterviewView>,
    pub scorecards: Vec<ScorecardView>,
    pub summary: Summary,
    pub screening: CandidateScreening,
}

/// A candidate's interviews, scorecards, panel summary and screening (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/candidates/{id}/feedback", operation_id = "people_recruitment_candidate_feedback", tag = "people-hr",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = CandidateFeedback), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn feedback(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(cid): Path<Uuid>,
) -> ApiResult<Json<CandidateFeedback>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let c = candidate(&mut conn, t, cid).await?;
    let inst = InstitutionId(c.institution_id);
    require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    let req = requisition(&mut conn, t, c.requisition_id).await?;
    let rub = rubric(&mut conn, t, &req).await?;
    let cards = scorecards_of(&mut conn, t, "candidate_id = $2", cid).await?;
    let (_, g) = settings_row(&mut conn, t, c.institution_id).await?;
    let guides = sc::normalize_guides(&g);
    let linked: HashSet<Uuid> = cards.iter().filter_map(|s| s.interview_id).collect();
    let rows: Vec<(Uuid, String, NaiveDate, String, String, Vec<String>, Option<String>, Option<String>, String)> =
        sqlx::query_as(
            "SELECT id, interview_type, interview_date, start_time, end_time, interviewers, location, guide_id, status \
             FROM people.candidate_interview WHERE tenant_id = $1 AND candidate_id = $2 \
             ORDER BY interview_date, start_time",
        )
        .bind(t.as_uuid())
        .bind(cid)
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db_error)?;
    let interviews = rows
        .into_iter()
        .map(
            |(id, kind, date, start, end, interviewers, location, guide_id, status)| {
                CandidateInterviewView {
                    id,
                    candidate_id: cid,
                    guide: guide_id.and_then(|g| guides.iter().find(|x| x.id == g).cloned()),
                    kind,
                    date,
                    start,
                    end,
                    interviewers,
                    location,
                    status,
                    has_scorecard: linked.contains(&id),
                }
            },
        )
        .collect();
    let questions = sc::normalize_screening_questions(&req.questions).unwrap_or_default();
    let answers = sc::normalize_answers(&c.answers).unwrap_or_default();
    let mismatches = sc::screening_mismatches(&answers, &questions)
        .iter()
        .map(|q| q.id.clone())
        .collect();
    Ok(Json(CandidateFeedback {
        candidate_id: cid,
        name: c.name,
        status: c.status,
        requisition_title: req.title,
        summary: sc::summarize(&as_cards(&cards), &rub),
        rubric: rub,
        interviews,
        scorecards: cards,
        screening: CandidateScreening {
            questions,
            answers,
            mismatches,
        },
    }))
}

/// One candidate in a comparison.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScorecardCompareRow {
    pub candidate_id: Uuid,
    pub name: String,
    pub status: String,
    pub summary: Summary,
    /// Must-have screening questions not answered yes.
    pub screening_flags: usize,
}

/// Candidates of a requisition side by side.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScorecardCompare {
    pub requisition_id: Uuid,
    pub rubric: Vec<RatingField>,
    pub items: Vec<ScorecardCompareRow>,
}

fn leaning_rank(s: &Summary) -> u8 {
    match s.leaning.as_deref() {
        Some("hire") => 0,
        Some("split") => 1,
        Some("no_hire") => 2,
        _ => 3,
    }
}

/// Compare a requisition's candidates on their feedback (`people.read`): leaning hire first,
/// then the higher average, then those with no feedback yet.
#[utoipa::path(get, path = "/api/v1/people/requisitions/{id}/scorecards", operation_id = "people_recruitment_requisition_scorecards", tag = "people-hr",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ScorecardCompare), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn compare(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(rid): Path<Uuid>,
) -> ApiResult<Json<ScorecardCompare>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let req = requisition(&mut conn, t, rid).await?;
    let inst = InstitutionId(req.institution_id);
    require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    let rub = rubric(&mut conn, t, &req).await?;
    let questions = sc::normalize_screening_questions(&req.questions).unwrap_or_default();
    let cands: Vec<(Uuid, String, Option<String>, String, Option<Value>)> = sqlx::query_as(
        "SELECT id, given_name, family_name, status, screening_answers FROM people.candidate \
         WHERE tenant_id = $1 AND requisition_id = $2 ORDER BY created_at, id",
    )
    .bind(t.as_uuid())
    .bind(rid)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let cards = scorecards_of(
        &mut conn,
        t,
        "candidate_id IN (SELECT id FROM people.candidate WHERE requisition_id = $2)",
        rid,
    )
    .await?;
    let mut items: Vec<ScorecardCompareRow> = cands
        .into_iter()
        .map(|(id, given, family, status, answers)| {
            let mine: Vec<ScorecardView> = cards
                .iter()
                .filter(|s| s.candidate_id == id)
                .cloned()
                .collect();
            let answers = answers
                .as_ref()
                .and_then(sc::normalize_answers)
                .unwrap_or_default();
            ScorecardCompareRow {
                candidate_id: id,
                name: family.map_or(given.clone(), |f| format!("{given} {f}")),
                status,
                summary: sc::summarize(&as_cards(&mine), &rub),
                screening_flags: sc::screening_mismatches(&answers, &questions).len(),
            }
        })
        .collect();
    items.sort_by(|a, b| {
        let (x, y) = (
            a.summary.average.unwrap_or(0.0),
            b.summary.average.unwrap_or(0.0),
        );
        leaning_rank(&a.summary)
            .cmp(&leaning_rank(&b.summary))
            .then_with(|| y.partial_cmp(&x).unwrap_or(std::cmp::Ordering::Equal))
    });
    Ok(Json(ScorecardCompare {
        requisition_id: rid,
        rubric: rub,
        items,
    }))
}

/// An interview still owed feedback.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FeedbackDueRow {
    pub interview_id: Uuid,
    pub candidate_id: Uuid,
    pub candidate_name: String,
    pub requisition_title: String,
    pub date: NaiveDate,
    #[serde(rename = "type")]
    pub kind: String,
    pub interviewers: Vec<String>,
}

/// Feedback due.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FeedbackDue {
    pub items: Vec<FeedbackDueRow>,
}

/// Interviews held before yesterday (within 14 days) with no scorecard, for candidates still in
/// play, newest first (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/institutions/{id}/feedback-due", operation_id = "people_recruitment_feedback_due", tag = "people-hr",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = FeedbackDue), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn feedback_due(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<FeedbackDue>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution(&st, &scope, inst).await?;
    let today = business_today(&st, &scope, inst).await?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let rows: Vec<(Uuid, Uuid, NaiveDate, String, String, Vec<String>, String, Option<String>, String, String)> =
        sqlx::query_as(
            "SELECT i.id, i.candidate_id, i.interview_date, i.interview_type, i.status, i.interviewers, c.given_name, \
             c.family_name, c.status, r.title FROM people.candidate_interview i \
             JOIN people.candidate c ON c.id = i.candidate_id JOIN people.requisition r ON r.id = c.requisition_id \
             WHERE i.tenant_id = $1 AND i.institution_id = $2 AND i.interview_date >= $3 AND i.interview_date < $4",
        )
        .bind(t.as_uuid())
        .bind(id)
        .bind(today - chrono::Duration::days(14))
        .bind(today)
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db_error)?;
    let cards = scorecards_of(&mut conn, t, "institution_id = $2", id).await?;
    let interviews: Vec<sc::Interview> = rows
        .iter()
        .map(|r| sc::Interview {
            id: r.0.to_string(),
            candidate_id: r.1.to_string(),
            date: r.2.to_string(),
            status: r.4.clone(),
        })
        .collect();
    let live: HashSet<String> = rows
        .iter()
        .filter(|r| !matches!(r.8.as_str(), "rejected" | "withdrawn" | "offer_declined"))
        .map(|r| r.1.to_string())
        .collect();
    let due = sc::missing_scorecards(&interviews, &as_cards(&cards), &live, &today.to_string());
    let items = due
        .iter()
        .filter_map(|i| rows.iter().find(|r| r.0.to_string() == i.id))
        .map(|r| FeedbackDueRow {
            interview_id: r.0,
            candidate_id: r.1,
            candidate_name: r.7.as_ref().map_or(r.6.clone(), |f| format!("{} {f}", r.6)),
            requisition_title: r.9.clone(),
            date: r.2,
            kind: r.3.clone(),
            interviewers: r.5.clone(),
        })
        .collect();
    Ok(Json(FeedbackDue { items }))
}

/// Routes of hiring scorecards.
pub fn router(st: PeopleState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(settings_get, settings_put))
        .routes(routes!(hiring_form))
        .routes(routes!(screening_answers))
        .routes(routes!(interview_schedule))
        .routes(routes!(interview_cancel))
        .routes(routes!(scorecard_create))
        .routes(routes!(feedback))
        .routes(routes!(compare))
        .routes(routes!(feedback_due))
        .with_state(st)
}

#[cfg(test)]
mod tests {
    use super::valid_time;

    #[test]
    fn times() {
        assert!(valid_time("09:30"));
        assert!(!valid_time("24:00"));
        assert!(!valid_time("9:30"));
    }
}
