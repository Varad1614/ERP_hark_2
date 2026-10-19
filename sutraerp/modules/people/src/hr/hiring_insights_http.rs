//! **ERP_EXTENSIONS 5A** — follow-ups, reminders and hiring analytics (rules in
//! [`super::hiring_analytics`] and [`super::offers`], ported from HeyPuter/everything hiring,
//! MIT). Tag `people-hr`; operation ids `people_recruitment_followup_*`,
//! `people_recruitment_hiring_*`.
//!
//! The funnel is rebuilt from what SutraERP already records: candidate status history
//! (`people.status_history`) gives every stage move with its time, so analytics need no extra
//! bookkeeping. [`HiringReminderJob`] runs hourly with the sweepers: it marks sent offers whose
//! expiry day has passed as `expired` and records offer / follow-up / onboarding reminders once
//! per stable key.
#![allow(clippy::type_complexity)]

use super::hiring_analytics::{
    self as ha, sutra_stage, HistoryEvent, PipelineCandidate, ReminderPrefs, ACTIVE,
};
use super::offers::{self as of, Checklist, ChecklistTask, Followup, OfferRecord, OfferStatus};
use super::offers_http::settings_row;
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::actor;
use crate::shared::scope::institution_scope;
use crate::shared::{Actor, PeopleState};
use crate::workforce::common::{self, history, invalid, required_text};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, Row};
use std::collections::{HashMap, HashSet};
use sutra_kernel::auth::Permission;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

// ------------------------------------------------------------------ loading

/// The institution's timezone and the local wall clock now.
async fn local_now(
    st: &PeopleState,
    scope: &ExecutionScope,
    inst: InstitutionId,
) -> Result<(String, NaiveDateTime), KernelError> {
    let r = require_institution(st, scope, inst).await?;
    let now: NaiveDateTime = sqlx::query_scalar("SELECT (now() AT TIME ZONE $1)")
        .bind(&r.timezone)
        .fetch_one(&st.pool)
        .await
        .map_err(map_db_error)?;
    Ok((r.timezone, now))
}

/// A candidate row with what the views add to the funnel.
pub struct CandRow {
    pub funnel: PipelineCandidate,
    pub requisition_number: String,
}

/// Every candidate of an institution (optionally one requisition) with their status history,
/// mapped onto the funnel (`applied → screen → interview → offer → hired`). An archived
/// candidate keeps the stage they left from.
pub async fn load_pipeline(
    conn: &mut PgConnection,
    t: TenantId,
    inst: Uuid,
    tz: &str,
    requisition: Option<Uuid>,
) -> Result<Vec<CandRow>, KernelError> {
    let rows = sqlx::query(
        "SELECT c.id, c.requisition_id, r.requisition_number, c.given_name, c.family_name, c.status, \
         c.source, c.talent_pool, (c.created_at AT TIME ZONE $3) AS created_local \
         FROM people.candidate c JOIN people.requisition r ON r.id = c.requisition_id \
         WHERE c.tenant_id = $1 AND c.institution_id = $2 AND ($4::uuid IS NULL OR c.requisition_id = $4) \
         ORDER BY c.created_at, c.id",
    )
    .bind(t.as_uuid())
    .bind(inst)
    .bind(tz)
    .bind(requisition)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let hist = sqlx::query(
        "SELECT h.aggregate_id, h.from_status, h.to_status, (h.created_at AT TIME ZONE $3) AS at_local \
         FROM people.status_history h WHERE h.tenant_id = $1 AND h.institution_id = $2 \
         AND h.aggregate_type = 'candidate' ORDER BY h.created_at, h.id",
    )
    .bind(t.as_uuid())
    .bind(inst)
    .bind(tz)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let mut by_cand: HashMap<Uuid, Vec<(Option<String>, String, NaiveDateTime)>> = HashMap::new();
    for h in &hist {
        let id: Uuid = h.try_get("aggregate_id").map_err(map_db_error)?;
        by_cand.entry(id).or_default().push((
            h.try_get("from_status").map_err(map_db_error)?,
            h.try_get("to_status").map_err(map_db_error)?,
            h.try_get("at_local").map_err(map_db_error)?,
        ));
    }
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        let id: Uuid = r.try_get("id").map_err(map_db_error)?;
        let status: String = r.try_get("status").map_err(map_db_error)?;
        let created: NaiveDateTime = r.try_get("created_local").map_err(map_db_error)?;
        let given: String = r.try_get("given_name").map_err(map_db_error)?;
        let family: Option<String> = r.try_get("family_name").map_err(map_db_error)?;
        let mut events = Vec::new();
        let mut last_stage_at = None;
        let mut left_from: Option<&'static str> = None;
        for (from, to, at) in by_cand.get(&id).map(Vec::as_slice).unwrap_or(&[]) {
            let (to_stage, to_status) = sutra_stage(to);
            match from {
                None => events.push(HistoryEvent {
                    kind: "created".into(),
                    stage: Some(to_stage.into()),
                    from: None,
                    to: None,
                    at: *at,
                }),
                Some(f) => {
                    let (from_stage, _) = sutra_stage(f);
                    if to_status == ACTIVE {
                        events.push(HistoryEvent {
                            kind: "stage".into(),
                            stage: None,
                            from: Some(from_stage.into()),
                            to: Some(to_stage.into()),
                            at: *at,
                        });
                        last_stage_at = Some(*at);
                    } else {
                        left_from = Some(from_stage);
                        events.push(HistoryEvent {
                            kind: to_status.into(),
                            stage: None,
                            from: None,
                            to: None,
                            at: *at,
                        });
                    }
                }
            }
        }
        if !events.iter().any(|e| e.kind == "created") {
            events.insert(
                0,
                HistoryEvent {
                    kind: "created".into(),
                    stage: Some("applied".into()),
                    from: None,
                    to: None,
                    at: created,
                },
            );
        }
        let (stage, st) = sutra_stage(&status);
        let stage = if st == ACTIVE {
            stage
        } else {
            left_from.unwrap_or("applied")
        };
        let source: Option<String> = r.try_get("source").map_err(map_db_error)?;
        let req: Uuid = r.try_get("requisition_id").map_err(map_db_error)?;
        out.push(CandRow {
            requisition_number: r.try_get("requisition_number").map_err(map_db_error)?,
            funnel: PipelineCandidate {
                id: id.to_string(),
                name: family.map_or(given.clone(), |f| format!("{given} {f}")),
                job_id: Some(req.to_string()),
                stage: stage.into(),
                status: st.into(),
                created_at: created,
                stage_changed_at: last_stage_at,
                source,
                pool: r.try_get("talent_pool").map_err(map_db_error)?,
                history: events,
            },
        });
    }
    Ok(out)
}

/// Offers of an institution as the pure rules read them.
pub async fn load_offers(
    conn: &mut PgConnection,
    t: TenantId,
    inst: Uuid,
    tz: &str,
) -> Result<Vec<OfferRecord>, KernelError> {
    let rows = sqlx::query(
        "SELECT id, candidate_id, status, expires_on, joining_date, \
         (sent_at AT TIME ZONE $3) AS sent_local, (resolved_at AT TIME ZONE $3) AS resolved_local, \
         (created_at AT TIME ZONE $3) AS created_local FROM people.candidate_offer \
         WHERE tenant_id = $1 AND institution_id = $2 ORDER BY created_at, id",
    )
    .bind(t.as_uuid())
    .bind(inst)
    .bind(tz)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        let status: String = r.try_get("status").map_err(map_db_error)?;
        let status = OfferStatus::parse(&status).unwrap_or(OfferStatus::Withdrawn);
        let resolved: Option<NaiveDateTime> = r.try_get("resolved_local").map_err(map_db_error)?;
        out.push(OfferRecord {
            id: r
                .try_get::<Uuid, _>("id")
                .map_err(map_db_error)?
                .to_string(),
            candidate_id: r
                .try_get::<Uuid, _>("candidate_id")
                .map_err(map_db_error)?
                .to_string(),
            status,
            expires: r.try_get("expires_on").map_err(map_db_error)?,
            joining_date: r.try_get("joining_date").map_err(map_db_error)?,
            sent_at: r.try_get("sent_local").map_err(map_db_error)?,
            accepted_at: (status == OfferStatus::Accepted)
                .then_some(resolved)
                .flatten(),
            resolved_at: resolved,
            created_at: r.try_get("created_local").map_err(map_db_error)?,
        });
    }
    Ok(out)
}

async fn load_followups(
    conn: &mut PgConnection,
    t: TenantId,
    inst: Uuid,
    tz: &str,
    candidate: Option<Uuid>,
) -> Result<Vec<(Uuid, Followup)>, KernelError> {
    let rows = sqlx::query(
        "SELECT id, candidate_id, text, due_date, due_time, done, (completed_at AT TIME ZONE $3) AS done_local \
         FROM people.candidate_followup WHERE tenant_id = $1 AND institution_id = $2 \
         AND ($4::uuid IS NULL OR candidate_id = $4) ORDER BY created_at, id",
    )
    .bind(t.as_uuid())
    .bind(inst)
    .bind(tz)
    .bind(candidate)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        let id: Uuid = r.try_get("id").map_err(map_db_error)?;
        let due: NaiveDate = r.try_get("due_date").map_err(map_db_error)?;
        out.push((
            id,
            Followup {
                id: id.to_string(),
                candidate_id: r
                    .try_get::<Uuid, _>("candidate_id")
                    .map_err(map_db_error)?
                    .to_string(),
                text: r.try_get("text").map_err(map_db_error)?,
                due_date: Some(due.format("%Y-%m-%d").to_string()),
                due_time: r.try_get("due_time").map_err(map_db_error)?,
                done: r.try_get("done").map_err(map_db_error)?,
                completed_at: r.try_get("done_local").map_err(map_db_error)?,
            },
        ));
    }
    Ok(out)
}

/// Checklists of live onboarding cases, each dated by its case's joining date (an accepted
/// "offer" record per case carries that date to the pure rules).
async fn load_checklists(
    conn: &mut PgConnection,
    t: TenantId,
    inst: Uuid,
) -> Result<(Vec<Checklist>, Vec<OfferRecord>), KernelError> {
    let rows = sqlx::query(
        "SELECT o.id AS case_id, o.candidate_id, o.joining_date, k.id, k.title, k.category, k.offset_days, k.done \
         FROM people.onboarding_case o JOIN people.onboarding_task k ON k.onboarding_case_id = o.id \
         WHERE o.tenant_id = $1 AND o.institution_id = $2 AND o.status NOT IN ('joined', 'cancelled') \
         ORDER BY o.id, k.position",
    )
    .bind(t.as_uuid())
    .bind(inst)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let mut plans: Vec<Checklist> = Vec::new();
    let mut dates: Vec<OfferRecord> = Vec::new();
    for r in &rows {
        let case: Uuid = r.try_get("case_id").map_err(map_db_error)?;
        let cand = r
            .try_get::<Uuid, _>("candidate_id")
            .map_err(map_db_error)?
            .to_string();
        if !dates.iter().any(|d| d.id == case.to_string()) {
            dates.push(OfferRecord {
                id: case.to_string(),
                candidate_id: cand.clone(),
                status: OfferStatus::Accepted,
                expires: None,
                joining_date: Some(r.try_get("joining_date").map_err(map_db_error)?),
                sent_at: None,
                accepted_at: None,
                resolved_at: None,
                created_at: None,
            });
            plans.push(Checklist {
                candidate_id: cand,
                tasks: vec![],
            });
        }
        let offset: Option<i32> = r.try_get("offset_days").map_err(map_db_error)?;
        if let Some(p) = plans.last_mut() {
            p.tasks.push(ChecklistTask {
                id: r
                    .try_get::<Uuid, _>("id")
                    .map_err(map_db_error)?
                    .to_string(),
                title: r.try_get("title").map_err(map_db_error)?,
                category: r.try_get("category").map_err(map_db_error)?,
                offset_days: offset.map(i64::from),
                done: r.try_get("done").map_err(map_db_error)?,
            });
        }
    }
    Ok((plans, dates))
}

// ------------------------------------------------------------------ follow-ups

/// A follow-up on a candidate.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HiringFollowupView {
    pub id: Uuid,
    pub candidate_id: Uuid,
    pub text: String,
    pub due_date: NaiveDate,
    pub due_time: Option<String>,
    pub done: bool,
    /// Due now (date-only follow-ups at the start of their day, timed ones at their time).
    pub due: bool,
    pub overdue: bool,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HiringFollowupList {
    pub items: Vec<HiringFollowupView>,
}

fn followup_view(id: Uuid, f: &Followup, now: NaiveDateTime) -> HiringFollowupView {
    let due = of::followup_due(f, now);
    HiringFollowupView {
        id,
        candidate_id: f.candidate_id.parse().unwrap_or_default(),
        text: f.text.clone(),
        due_date: f
            .due_date
            .as_deref()
            .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
            .unwrap_or_default(),
        due_time: f.due_time.clone(),
        done: f.done,
        due: due.is_some(),
        overdue: due.unwrap_or(false),
    }
}

async fn candidate_inst(
    conn: &mut PgConnection,
    t: TenantId,
    cid: Uuid,
) -> Result<InstitutionId, KernelError> {
    let inst: Option<Uuid> = sqlx::query_scalar(
        "SELECT institution_id FROM people.candidate WHERE id = $1 AND tenant_id = $2",
    )
    .bind(cid)
    .bind(t.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    inst.map(InstitutionId).ok_or(KernelError::NotFound {
        entity: "candidate",
    })
}

/// Follow-ups of a candidate: open first (soonest due), done last (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/candidates/{id}/followups", operation_id = "people_recruitment_followup_list", tag = "people-hr",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = HiringFollowupList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn followup_list(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(cid): Path<Uuid>,
) -> ApiResult<Json<HiringFollowupList>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let inst = candidate_inst(&mut conn, t, cid).await?;
    let scope = institution_scope(ctx, inst);
    let (tz, now) = local_now(&st, &scope, inst).await?;
    let all = load_followups(&mut conn, t, inst.as_uuid(), &tz, Some(cid)).await?;
    let plain: Vec<Followup> = all.iter().map(|(_, f)| f.clone()).collect();
    let ids: HashMap<String, Uuid> = all.iter().map(|(id, f)| (f.id.clone(), *id)).collect();
    let items = of::sort_followups(&plain)
        .into_iter()
        .map(|f| followup_view(ids[&f.id], f, now))
        .collect();
    Ok(Json(HiringFollowupList { items }))
}

/// A new follow-up.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HiringFollowupInput {
    #[schema(example = "Call about the notice period")]
    pub text: String,
    pub due_date: String,
    /// `HH:MM` (24 h); without it the follow-up is due from the start of its day.
    pub due_time: Option<String>,
}

/// Add a follow-up to a candidate (`people.staff.manage`).
#[utoipa::path(post, path = "/api/v1/people/candidates/{id}/followups", operation_id = "people_recruitment_followup_create", tag = "people-hr",
    params(("id" = String, Path, format = Uuid)), request_body = HiringFollowupInput, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = HiringFollowupView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn followup_create(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(cid): Path<Uuid>,
    Json(b): Json<HiringFollowupInput>,
) -> ApiResult<(StatusCode, Json<HiringFollowupView>)> {
    let ctx = auth.require(Permission::StaffManage)?;
    let t = ctx.scope.tenant_id;
    let inst = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        candidate_inst(&mut conn, t, cid).await?
    };
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let time = b
        .due_time
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let codes = of::validate_followup(&b.text, b.due_date.trim(), time.as_deref());
    if let Some(code) = codes.first() {
        let (field, msg) = match *code {
            "text_required" => ("text", "describe the follow-up"),
            "due_date_required" => ("dueDate", "pick a due date (YYYY-MM-DD)"),
            _ => ("dueTime", "a time like 09:30"),
        };
        return Err(invalid(field, code, msg).into());
    }
    let text = required_text("text", &b.text, 500)?;
    let due = NaiveDate::parse_from_str(b.due_date.trim(), "%Y-%m-%d")
        .map_err(|_| invalid("dueDate", "due_date_required", "not a calendar date"))?;
    let (tz, now) = local_now(&st, &scope, inst).await?;
    let _ = tz;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO people.candidate_followup (tenant_id, institution_id, candidate_id, text, due_date, due_time, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(inst.as_uuid())
    .bind(cid)
    .bind(&text)
    .bind(due)
    .bind(&time)
    .bind(a.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    common::audit_only(
        &mut tx,
        &scope,
        "candidate",
        cid,
        "people.candidate.followup",
        serde_json::json!({"followupId": id, "dueDate": due}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let f = Followup {
        id: id.to_string(),
        candidate_id: cid.to_string(),
        text,
        due_date: Some(due.format("%Y-%m-%d").to_string()),
        due_time: time,
        done: false,
        completed_at: None,
    };
    Ok((StatusCode::CREATED, Json(followup_view(id, &f, now))))
}

/// Complete or reopen.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct HiringFollowupDoneBody {
    pub done: bool,
}

/// Mark a follow-up done or open again (`people.staff.manage`).
#[utoipa::path(post, path = "/api/v1/people/candidate-followups/{id}/complete", operation_id = "people_recruitment_followup_complete", tag = "people-hr",
    params(("id" = String, Path, format = Uuid)), request_body = HiringFollowupDoneBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 204), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn followup_complete(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(b): Json<HiringFollowupDoneBody>,
) -> ApiResult<StatusCode> {
    let ctx = auth.require(Permission::StaffManage)?;
    let t = ctx.scope.tenant_id;
    let row: Option<(Uuid, Uuid)> = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        sqlx::query_as(
            "SELECT institution_id, candidate_id FROM people.candidate_followup WHERE id = $1 AND tenant_id = $2",
        )
        .bind(id)
        .bind(t.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
    };
    let (inst, cid) = row.ok_or(KernelError::NotFound {
        entity: "follow-up",
    })?;
    let inst = InstitutionId(inst);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    sqlx::query(
        "UPDATE people.candidate_followup SET done = $2, completed_at = CASE WHEN $2 THEN COALESCE(completed_at, now()) END \
         WHERE id = $1",
    )
    .bind(id)
    .bind(b.done)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    common::audit_only(
        &mut tx,
        &scope,
        "candidate",
        cid,
        if b.done {
            "people.candidate.followup_done"
        } else {
            "people.candidate.followup_reopen"
        },
        serde_json::json!({"followupId": id}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(StatusCode::NO_CONTENT)
}

// ------------------------------------------------------------------ attention overview

/// A candidate sitting in one stage too long.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HiringStaleRow {
    pub candidate_id: String,
    pub name: String,
    pub requisition_number: String,
    pub stage: String,
    pub stage_label: String,
    pub days: i64,
}

/// An archived candidate past the retention window (DPDP Act 2023: erase personal data once its
/// purpose is served). Listed for a person to confirm; nothing is erased automatically.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HiringRetentionRow {
    pub candidate_id: String,
    pub name: String,
    pub requisition_number: String,
    pub archived_on: NaiveDate,
}

/// A reminder the hourly job recorded.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HiringRecordedReminder {
    pub key: String,
    pub kind: String,
    pub candidate_id: Option<Uuid>,
    pub title: String,
    pub body: String,
    pub created_at: DateTime<Utc>,
}

/// What needs attention in recruitment now.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HiringAttention {
    pub as_of: NaiveDateTime,
    pub stale_after_days: i32,
    pub retention_months: i32,
    /// Offers expiring within three days (or lapsed), due follow-ups, due onboarding tasks.
    pub reminders: Vec<ha::Reminder>,
    pub stale: Vec<HiringStaleRow>,
    pub retention_due: Vec<HiringRetentionRow>,
    /// The last reminders the hourly job recorded.
    pub recorded: Vec<HiringRecordedReminder>,
}

/// Recruitment follow-ups (`people.read`): reminders due now, candidates stuck in a stage, and
/// archived candidates past the retention window.
#[utoipa::path(get, path = "/api/v1/people/institutions/{id}/hiring-followups", operation_id = "people_recruitment_hiring_followups", tag = "people-hr",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = HiringAttention), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn hiring_followups(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<HiringAttention>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    let (tz, now) = local_now(&st, &scope, inst).await?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let s = settings_row(&mut conn, t, id).await?;
    let (reminders, cands) = compute(&mut conn, t, id, &tz, now).await?;
    let funnel: Vec<PipelineCandidate> = cands.iter().map(|c| c.funnel.clone()).collect();
    let req_of: HashMap<&str, &str> = cands
        .iter()
        .map(|c| (c.funnel.id.as_str(), c.requisition_number.as_str()))
        .collect();
    let stale = ha::stale_candidates(&funnel, i64::from(s.stale_after_days), now)
        .into_iter()
        .map(|c| HiringStaleRow {
            candidate_id: c.id.clone(),
            name: c.name.clone(),
            requisition_number: req_of.get(c.id.as_str()).copied().unwrap_or("").into(),
            stage: c.stage.clone(),
            stage_label: ha::stage_label(&c.stage),
            days: ha::days_in_stage(c, now),
        })
        .collect();
    let retention_due = ha::retention_due(&funnel, i64::from(s.retention_months), now)
        .into_iter()
        .map(|(c, at)| HiringRetentionRow {
            candidate_id: c.id.clone(),
            name: c.name.clone(),
            requisition_number: req_of.get(c.id.as_str()).copied().unwrap_or("").into(),
            archived_on: at.date(),
        })
        .collect();
    let rows = sqlx::query(
        "SELECT reminder_key, kind, candidate_id, title, body, created_at FROM people.hiring_reminder \
         WHERE tenant_id = $1 AND institution_id = $2 ORDER BY created_at DESC LIMIT 20",
    )
    .bind(t.as_uuid())
    .bind(id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let recorded = rows
        .iter()
        .map(|r| {
            Ok(HiringRecordedReminder {
                key: r.try_get("reminder_key")?,
                kind: r.try_get("kind")?,
                candidate_id: r.try_get("candidate_id")?,
                title: r.try_get("title")?,
                body: r.try_get("body")?,
                created_at: r.try_get("created_at")?,
            })
        })
        .collect::<Result<Vec<_>, sqlx::Error>>()
        .map_err(map_db_error)?;
    Ok(Json(HiringAttention {
        as_of: now,
        stale_after_days: s.stale_after_days,
        retention_months: s.retention_months,
        reminders,
        stale,
        retention_due,
        recorded,
    }))
}

/// Reminders of one institution now, plus the loaded funnel.
async fn compute(
    conn: &mut PgConnection,
    t: TenantId,
    inst: Uuid,
    tz: &str,
    now: NaiveDateTime,
) -> Result<(Vec<ha::Reminder>, Vec<CandRow>), KernelError> {
    let cands = load_pipeline(conn, t, inst, tz, None).await?;
    let offers = load_offers(conn, t, inst, tz).await?;
    let fus: Vec<Followup> = load_followups(conn, t, inst, tz, None)
        .await?
        .into_iter()
        .map(|(_, f)| f)
        .collect();
    let (plans, case_dates) = load_checklists(conn, t, inst).await?;
    let funnel: Vec<PipelineCandidate> = cands.iter().map(|c| c.funnel.clone()).collect();
    let prefs = ReminderPrefs::default();
    // Offer and follow-up reminders read the offers; onboarding reads the cases' joining dates.
    let mut out = ha::compute_reminders(
        &funnel,
        &offers,
        &fus,
        &[],
        ReminderPrefs {
            onboarding: false,
            ..prefs
        },
        now,
    );
    out.extend(ha::compute_reminders(
        &funnel,
        &case_dates,
        &[],
        &plans,
        ReminderPrefs {
            offers: false,
            followups: false,
            onboarding: true,
        },
        now,
    ));
    Ok((out, cands))
}

// ------------------------------------------------------------------ analytics

#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct HiringAnalyticsQuery {
    /// One requisition only.
    pub requisition_id: Option<Uuid>,
    /// Hires / offers in the last N days (time to hire and offer outcomes); omit for all time.
    pub since_days: Option<i64>,
}

/// One funnel stage with its active candidates.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HiringFunnelStage {
    pub stage: String,
    pub label: String,
    pub count: u64,
}

/// Figures of one requisition.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HiringRequisitionFigures {
    pub requisition_id: String,
    pub requisition_number: String,
    pub candidates: u64,
    pub active: u64,
    pub hired: u64,
    pub time_to_hire: ha::TimeToHire,
    pub offers: ha::OfferOutcomes,
}

/// Offer figures of the scope.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HiringOfferFigures {
    #[serde(flatten)]
    pub outcomes: ha::OfferOutcomes,
    pub expired: u64,
    pub withdrawn: u64,
    /// Drafts, awaiting approval, approved and sent.
    pub in_play: u64,
}

/// Hiring analytics of an institution (or one requisition).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HiringAnalytics {
    pub as_of: NaiveDateTime,
    pub since_days: Option<i64>,
    pub candidates: u64,
    pub funnel: Vec<HiringFunnelStage>,
    pub conversion: Vec<ha::Conversion>,
    pub time_to_hire: ha::TimeToHire,
    pub average_days_in_stage: Vec<ha::StageTime>,
    pub sources: Vec<ha::SourceRow>,
    pub offers: HiringOfferFigures,
    pub requisitions: Vec<HiringRequisitionFigures>,
}

/// Hiring analytics (`people.read`): pipeline conversion per stage, time to hire, average time in
/// stage, source effectiveness, offer acceptance — for the institution and per requisition.
#[utoipa::path(get, path = "/api/v1/people/institutions/{id}/hiring-analytics", operation_id = "people_recruitment_hiring_analytics", tag = "people-hr",
    params(("id" = String, Path, format = Uuid), HiringAnalyticsQuery), security(("session_cookie" = [])),
    responses((status = 200, body = HiringAnalytics), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn hiring_analytics(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<HiringAnalyticsQuery>,
) -> ApiResult<Json<HiringAnalytics>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    if q.since_days.is_some_and(|d| !(1..=3650).contains(&d)) {
        return Err(invalid("sinceDays", "out_of_range", "1 to 3650").into());
    }
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    let (tz, now) = local_now(&st, &scope, inst).await?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let cands = load_pipeline(&mut conn, t, id, &tz, q.requisition_id).await?;
    let offers = load_offers(&mut conn, t, id, &tz).await?;
    let funnel: Vec<PipelineCandidate> = cands.iter().map(|c| c.funnel.clone()).collect();
    let ids: HashSet<String> = funnel.iter().map(|c| c.id.clone()).collect();
    let scoped: Vec<OfferRecord> = offers
        .iter()
        .filter(|o| ids.contains(&o.candidate_id))
        .cloned()
        .collect();
    let since = q.since_days.map(|d| ha::days_ago(now, d));
    let stages = ha::default_stage_keys();
    let count = |s: OfferStatus| scoped.iter().filter(|o| o.status == s).count() as u64;
    let mut by_req: Vec<(String, String)> = Vec::new();
    for c in &cands {
        let rid = c.funnel.job_id.clone().unwrap_or_default();
        if !by_req.iter().any(|(r, _)| *r == rid) {
            by_req.push((rid, c.requisition_number.clone()));
        }
    }
    let requisitions = by_req
        .into_iter()
        .map(|(rid, number)| {
            let mine: Vec<PipelineCandidate> = funnel
                .iter()
                .filter(|c| c.job_id.as_deref() == Some(rid.as_str()))
                .cloned()
                .collect();
            let mids: HashSet<String> = mine.iter().map(|c| c.id.clone()).collect();
            HiringRequisitionFigures {
                requisition_id: rid,
                requisition_number: number,
                candidates: mine.len() as u64,
                active: mine.iter().filter(|c| c.is_active()).count() as u64,
                hired: mine
                    .iter()
                    .filter(|c| ha::reached_stages(c).contains("hired"))
                    .count() as u64,
                time_to_hire: ha::time_to_hire(&mine, since),
                offers: ha::offer_outcomes(&offers, Some(&mids), since),
            }
        })
        .collect();
    Ok(Json(HiringAnalytics {
        as_of: now,
        since_days: q.since_days,
        candidates: funnel.len() as u64,
        funnel: ha::funnel_counts(&funnel, None, &stages)
            .into_iter()
            .map(|(stage, count)| HiringFunnelStage {
                label: ha::stage_label(&stage),
                stage,
                count,
            })
            .collect(),
        conversion: ha::stage_conversion(&funnel, &stages),
        time_to_hire: ha::time_to_hire(&funnel, since),
        average_days_in_stage: ha::average_days_in_stage(&funnel, now, &stages),
        sources: ha::source_breakdown(&funnel),
        offers: HiringOfferFigures {
            outcomes: ha::offer_outcomes(&scoped, None, since),
            expired: count(OfferStatus::Expired),
            withdrawn: count(OfferStatus::Withdrawn),
            in_play: scoped.iter().filter(|o| of::is_open(o.status)).count() as u64,
        },
        requisitions,
    }))
}

// ------------------------------------------------------------------ hourly job

/// Actor of the reminder job.
pub const REMINDER_ACTOR: &str = "system:hiring-reminders";

/// Hourly: marks sent offers past their expiry day `expired` and records reminders once per
/// stable key, per institution, in that institution's tenant context.
pub struct HiringReminderJob {
    pub state: PeopleState,
    pub interval: std::time::Duration,
}

impl HiringReminderJob {
    async fn one(&self, tenant: Uuid, inst: Uuid) -> Result<u64, KernelError> {
        let st = self.state.clone();
        let t = TenantId(tenant);
        let i = InstitutionId(inst);
        let scope = ExecutionScope::institution(t, i);
        sutra_kernel::db_context::scoped(scope, async move {
            let (tz, now) = local_now(&st, &scope, i).await?;
            let today = now.date();
            let a = Actor {
                id: Some(REMINDER_ACTOR.into()),
                correlation_id: None,
            };
            let mut tx = st.pool.begin().await.map_err(map_db_error)?;
            let lapsed: Vec<(Uuid, String)> = sqlx::query_as(
                "SELECT id, offer_number FROM people.candidate_offer WHERE tenant_id = $1 AND institution_id = $2 \
                 AND status = 'sent' AND expires_on < $3 FOR UPDATE",
            )
            .bind(tenant)
            .bind(inst)
            .bind(today)
            .fetch_all(&mut *tx)
            .await
            .map_err(map_db_error)?;
            let mut n = 0u64;
            for (oid, number) in &lapsed {
                sqlx::query(
                    "UPDATE people.candidate_offer SET status = 'expired', status_reason = 'lapsed unanswered', \
                     resolved_at = now(), version = version + 1, updated_at = now() WHERE id = $1",
                )
                .bind(oid)
                .execute(&mut *tx)
                .await
                .map_err(map_db_error)?;
                history(
                    &mut tx,
                    t,
                    i,
                    "candidate_offer",
                    *oid,
                    Some("sent"),
                    "expired",
                    "sutraerp.people.candidate_offer.status",
                    Some("lapsed unanswered"),
                    None,
                    &a,
                )
                .await?;
                common::audit_only(
                    &mut tx,
                    &scope,
                    "candidate_offer",
                    *oid,
                    "people.offer.expire",
                    serde_json::json!({"number": number}),
                    &a,
                )
                .await?;
                n += 1;
            }
            let (reminders, _) = compute(&mut tx, t, inst, &tz, now).await?;
            for r in reminders {
                let cand: Option<Uuid> = r.candidate_id.parse().ok();
                let res = sqlx::query(
                    "INSERT INTO people.hiring_reminder (tenant_id, institution_id, reminder_key, kind, candidate_id, title, body) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT (tenant_id, reminder_key) DO NOTHING",
                )
                .bind(tenant)
                .bind(inst)
                .bind(&r.key)
                .bind(&r.kind)
                .bind(cand)
                .bind(&r.title)
                .bind(&r.body)
                .execute(&mut *tx)
                .await
                .map_err(map_db_error)?;
                n += res.rows_affected();
            }
            tx.commit().await.map_err(map_db_error)?;
            Ok::<u64, KernelError>(n)
        })
        .await
    }
}

#[async_trait::async_trait]
impl sutra_persistence::jobs::SweepJob for HiringReminderJob {
    fn name(&self) -> &'static str {
        "people_hiring_reminders"
    }

    fn interval(&self) -> std::time::Duration {
        self.interval
    }

    async fn run(&self, conn: &mut PgConnection) -> Result<u64, sqlx::Error> {
        let rows: Vec<(Uuid, Uuid)> = sqlx::query_as(
            "SELECT DISTINCT tenant_id, institution_id FROM people.candidate \
             WHERE status IN ('shortlisted', 'interviewed', 'offered', 'offer_accepted') ORDER BY 1, 2",
        )
        .fetch_all(&mut *conn)
        .await?;
        let mut total = 0;
        for (tenant, inst) in rows {
            match self.one(tenant, inst).await {
                Ok(n) => total += n,
                Err(e) => tracing::warn!(error = %e, tenant = %tenant, institution = %inst,
                    "hiring reminders failed"),
            }
        }
        Ok(total)
    }
}

pub fn router(state: PeopleState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(followup_list, followup_create))
        .routes(routes!(followup_complete))
        .routes(routes!(hiring_followups))
        .routes(routes!(hiring_analytics))
        .with_state(state)
}
