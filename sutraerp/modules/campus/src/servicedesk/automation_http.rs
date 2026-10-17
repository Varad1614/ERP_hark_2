//! **ERP_EXTENSIONS 2B** — service-desk rules, saved views, SLA policy and the SLA board
//! (HTTP). The engine is [`super::rules`] (ported from Everything's help desk, MIT); runtime
//! firing is [`super::automation`].
//!
//! Paths under `/api/v1/campus/institutions/{id}/service-desk/…` and
//! `/api/v1/campus/service-desk/…`, plus `/api/v1/campus/tickets/{id}/tags` and
//! `/api/v1/campus/tickets/{id}/automation`. Tag `campus-facilities`, operation ids
//! `campus_servicedesk_*`.

use super::automation::{self, default_sla, diff, load_policy, to_rules};
use super::http::TicketView;
use super::rules::{
    self, matches, run_triggers, short_duration, Action, Condition, Conditions, Ctx, Event, Sla,
    SlaState, SlaTarget, Trigger, Vocab,
};
use crate::infrastructure::common::{audit, ensure_version, expected, invalid};
use crate::shared::guard::require_institution_writable;
use crate::shared::http::{actor, with_etag};
use crate::shared::rows::{lock_ticket, ticket_from_row, ServiceTicket, TICKET_COLUMNS};
use crate::shared::scope::institution_scope;
use crate::shared::CampusState;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use std::collections::BTreeMap;
use sutra_contracts::campus::ids::ServiceTicketId;
use sutra_kernel::auth::Permission;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

// ------------------------------------------------------------------ validation

/// Condition fields and operators the campus desk understands.
const FIELDS: &[(&str, &[&str])] = &[
    ("status", &["is", "is_not", "less_than", "greater_than", "changed_to"]),
    ("priority", &["is", "is_not", "less_than", "greater_than"]),
    ("type", &["is", "is_not"]),
    ("assignee", &["is", "is_not", "changed"]),
    ("requester", &["is", "is_not"]),
    ("channel", &["is", "is_not"]),
    ("tags", &["includes", "excludes"]),
    ("subject", &["contains", "not_contains"]),
    ("created", &["less_than", "greater_than"]),
    ("updated", &["less_than", "greater_than"]),
    ("solved", &["less_than", "greater_than"]),
    ("actor", &["is"]),
    ("field:complaint", &["is", "is_not"]),
    ("field:requester_kind", &["is", "is_not", "present", "absent"]),
    ("field:node", &["is", "is_not", "present", "absent"]),
];

/// Actions a campus trigger may take (status moves only through the lifecycle commands).
const ACTIONS: &[&str] = &["priority", "type", "assignee", "add_tags", "remove_tags", "set_tags"];

const CHANNELS: &[&str] = &[
    "self_service",
    "admin",
    "hostel",
    "transport",
    "library",
    "laboratory",
    "preventive",
];

fn check_conditions(prefix: &str, c: &Conditions, v: &mut Vec<FieldViolation>) {
    let vocab = Vocab::campus();
    for (group, list) in [("all", &c.all), ("any", &c.any)] {
        if list.len() > 20 {
            v.push(FieldViolation::new(format!("{prefix}.{group}"), "too_many", "at most 20 conditions"));
        }
        for (i, k) in list.iter().enumerate() {
            let f = format!("{prefix}.{group}[{i}]");
            let Some((_, ops)) = FIELDS.iter().find(|(name, _)| *name == k.field) else {
                v.push(FieldViolation::new(format!("{f}.field"), "unknown", "unknown condition field"));
                continue;
            };
            if !ops.contains(&k.op.as_str()) {
                v.push(FieldViolation::new(format!("{f}.op"), "unknown", format!("{} supports {}", k.field, ops.join(", "))));
                continue;
            }
            let s = k.value.as_str();
            let ok = match k.field.as_str() {
                "status" => s.is_some_and(|x| vocab.statuses.iter().any(|y| y == x)),
                "priority" => s.is_some_and(|x| vocab.priorities.iter().any(|y| y == x)),
                "type" => s.is_some_and(|x| vocab.types.iter().any(|y| y == x)),
                "channel" => s.is_some_and(|x| CHANNELS.contains(&x)),
                "actor" => matches!(s, Some("agent" | "customer" | "system")),
                "created" | "updated" | "solved" => {
                    k.value.as_f64().is_some_and(|n| n > 0.0)
                        || s.and_then(|x| x.trim().parse::<f64>().ok()).is_some_and(|n| n > 0.0)
                }
                "tags" => !rules::norm_tags(&k.value).is_empty(),
                "assignee" if k.op == "changed" => true,
                _ => k.value.is_string() || k.value.is_boolean() || k.value.is_null() || k.op == "present" || k.op == "absent",
            };
            if !ok {
                v.push(FieldViolation::new(format!("{f}.value"), "invalid", format!("not a valid value for {}", k.field)));
            }
        }
    }
}

fn check_actions(actions: &[Action], v: &mut Vec<FieldViolation>) {
    let vocab = Vocab::campus();
    if actions.is_empty() || actions.len() > 20 {
        v.push(FieldViolation::new("actions", "count", "1 to 20 actions"));
    }
    for (i, a) in actions.iter().enumerate() {
        let f = format!("actions[{i}]");
        if !ACTIONS.contains(&a.field.as_str()) {
            v.push(FieldViolation::new(format!("{f}.field"), "unknown", format!("actions are {}", ACTIONS.join(", "))));
            continue;
        }
        let s = a.value.as_str();
        let ok = match a.field.as_str() {
            "priority" => s.is_some_and(|x| vocab.priorities.iter().any(|y| y == x)),
            "type" => s.is_some_and(|x| vocab.types.iter().any(|y| y == x)),
            "assignee" => s.is_some_and(|x| !x.trim().is_empty() && x.len() <= 200),
            _ => !rules::norm_tags(&a.value).is_empty(),
        };
        if !ok {
            v.push(FieldViolation::new(format!("{f}.value"), "invalid", format!("not a valid value for {}", a.field)));
        }
    }
}

fn finish(v: Vec<FieldViolation>) -> Result<(), KernelError> {
    if v.is_empty() {
        Ok(())
    } else {
        Err(KernelError::Validation(v))
    }
}

// ------------------------------------------------------------------ rules

/// A service-desk trigger.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ServiceDeskRule {
    pub id: Uuid,
    pub title: String,
    /// `created`, `updated` or `any`.
    pub event: String,
    pub position: i32,
    pub active: bool,
    pub conditions: Conditions,
    pub actions: Vec<Action>,
    pub version: i64,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ServiceDeskRuleList {
    pub items: Vec<ServiceDeskRule>,
}

/// A trigger to save (create, or replace with `If-Match`).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServiceDeskRuleInput {
    pub title: String,
    /// `created`, `updated` or `any` (default).
    pub event: Option<String>,
    pub position: Option<i32>,
    pub active: Option<bool>,
    #[serde(default)]
    pub conditions: Conditions,
    pub actions: Vec<Action>,
}

const RULE_COLUMNS: &str = "id, title, event, position, active, conditions, actions, version, updated_at";

fn rule_from_row(r: &PgRow) -> Result<ServiceDeskRule, sqlx::Error> {
    let conditions: serde_json::Value = r.try_get("conditions")?;
    let actions: serde_json::Value = r.try_get("actions")?;
    Ok(ServiceDeskRule {
        id: r.try_get("id")?,
        title: r.try_get("title")?,
        event: r.try_get("event")?,
        position: r.try_get("position")?,
        active: r.try_get("active")?,
        conditions: serde_json::from_value(conditions).unwrap_or_default(),
        actions: serde_json::from_value(actions).unwrap_or_default(),
        version: r.try_get("version")?,
        updated_at: r.try_get("updated_at")?,
    })
}

fn validate_rule(b: &ServiceDeskRuleInput) -> Result<String, KernelError> {
    let mut v = Vec::new();
    let n = b.title.trim().chars().count();
    if n == 0 || n > 120 {
        v.push(FieldViolation::new("title", "invalid", "1-120 characters"));
    }
    let event = b.event.clone().unwrap_or_else(|| "any".into());
    if !matches!(event.as_str(), "created" | "updated" | "any") {
        v.push(FieldViolation::new("event", "unknown", "created, updated or any"));
    }
    check_conditions("conditions", &b.conditions, &mut v);
    check_actions(&b.actions, &mut v);
    finish(v)?;
    Ok(event)
}

/// The service-desk triggers of an institution, in run order (`campus.read`).
#[utoipa::path(get, path = "/api/v1/campus/institutions/{id}/service-desk/rules", operation_id = "campus_servicedesk_rule_list", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ServiceDeskRuleList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn rule_list(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ServiceDeskRuleList>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let rows = sqlx::query(&format!(
        "SELECT {RULE_COLUMNS} FROM campus.ticket_rule WHERE tenant_id = $1 AND institution_id = $2 \
         ORDER BY position, created_at, id"
    ))
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let items = rows.iter().map(rule_from_row).collect::<Result<_, _>>().map_err(map_db_error)?;
    Ok(Json(ServiceDeskRuleList { items }))
}

/// Add a trigger (`campus.facility.manage`).
#[utoipa::path(post, path = "/api/v1/campus/institutions/{id}/service-desk/rules", operation_id = "campus_servicedesk_rule_create", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = ServiceDeskRuleInput, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = ServiceDeskRule, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn rule_create(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ServiceDeskRuleInput>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacilityManage)?;
    require_idempotency_key(&headers)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let event = validate_rule(&b)?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let position = match b.position {
        Some(p) => p,
        None => sqlx::query_scalar::<_, i32>(
            "SELECT coalesce(max(position), 0) + 10 FROM campus.ticket_rule \
             WHERE tenant_id = $1 AND institution_id = $2",
        )
        .bind(scope.tenant_id.as_uuid())
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?,
    };
    let row = sqlx::query(&format!(
        "INSERT INTO campus.ticket_rule (tenant_id, institution_id, title, event, position, active, \
         conditions, actions, created_by) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9) RETURNING {RULE_COLUMNS}"
    ))
    .bind(scope.tenant_id.as_uuid())
    .bind(id)
    .bind(b.title.trim())
    .bind(&event)
    .bind(position)
    .bind(b.active.unwrap_or(true))
    .bind(serde_json::to_value(&b.conditions).unwrap_or_default())
    .bind(serde_json::to_value(&b.actions).unwrap_or_default())
    .bind(a.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let rule = rule_from_row(&row).map_err(map_db_error)?;
    audit(&mut tx, &scope, "campus.ticket_rule.create", "ticket_rule", rule.id, serde_json::json!({"title": rule.title, "event": rule.event}), &a).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::CREATED, &rule, Version(rule.version)))
}

async fn rule_institution(st: &CampusState, tenant: TenantId, id: Uuid) -> Result<(InstitutionId, i64), KernelError> {
    let row: Option<(Uuid, i64)> = sqlx::query_as(
        "SELECT institution_id, version FROM campus.ticket_rule WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(&st.pool)
    .await
    .map_err(map_db_error)?;
    row.map(|(i, v)| (InstitutionId(i), v))
        .ok_or(KernelError::NotFound { entity: "service desk rule" })
}

/// Replace a trigger (`campus.facility.manage`, `If-Match`).
#[utoipa::path(put, path = "/api/v1/campus/service-desk/rules/{id}", operation_id = "campus_servicedesk_rule_update", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the rule")),
    request_body = ServiceDeskRuleInput, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ServiceDeskRule, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn rule_update(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ServiceDeskRuleInput>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacilityManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let (inst, _) = rule_institution(&st, t, id).await?;
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let event = validate_rule(&b)?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let cur: i64 = sqlx::query_scalar("SELECT version FROM campus.ticket_rule WHERE id = $1 AND tenant_id = $2 FOR UPDATE")
        .bind(id)
        .bind(t.as_uuid())
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
    ensure_version(v, cur)?;
    let row = sqlx::query(&format!(
        "UPDATE campus.ticket_rule SET title = $3, event = $4, position = coalesce($5, position), \
         active = coalesce($6, active), conditions = $7, actions = $8, version = version + 1, \
         updated_at = now() WHERE id = $1 AND tenant_id = $2 RETURNING {RULE_COLUMNS}"
    ))
    .bind(id)
    .bind(t.as_uuid())
    .bind(b.title.trim())
    .bind(&event)
    .bind(b.position)
    .bind(b.active)
    .bind(serde_json::to_value(&b.conditions).unwrap_or_default())
    .bind(serde_json::to_value(&b.actions).unwrap_or_default())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let rule = rule_from_row(&row).map_err(map_db_error)?;
    audit(&mut tx, &scope, "campus.ticket_rule.update", "ticket_rule", id, serde_json::json!({"title": rule.title, "active": rule.active, "version": rule.version}), &a).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &rule, Version(rule.version)))
}

/// Delete a trigger (`campus.facility.manage`). Its past firings stay in the log.
#[utoipa::path(delete, path = "/api/v1/campus/service-desk/rules/{id}", operation_id = "campus_servicedesk_rule_delete", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 204), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn rule_delete(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let ctx = auth.require(Permission::FacilityManage)?;
    let t = ctx.scope.tenant_id;
    let (inst, _) = rule_institution(&st, t, id).await?;
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    sqlx::query("DELETE FROM campus.ticket_rule WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(t.as_uuid())
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    audit(&mut tx, &scope, "campus.ticket_rule.delete", "ticket_rule", id, serde_json::json!({}), &a).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(StatusCode::NO_CONTENT)
}

// ------------------------------------------------------------------ ticket rows

/// A ticket on a queue or the SLA board.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ServiceDeskTicketRow {
    pub ticket: TicketView,
    pub tags: Vec<String>,
    pub first_response_at: Option<DateTime<Utc>>,
    /// The SLA target due soonest (`null` when none applies, e.g. resolved).
    pub sla: Option<SlaState>,
    /// `3h`, `-2d` … until that target.
    pub sla_in: Option<String>,
}

struct Loaded {
    ticket: ServiceTicket,
    tags: Vec<String>,
    first: Option<DateTime<Utc>>,
    updated: DateTime<Utc>,
}

async fn load_open_and_recent(conn: &mut PgConnection, tenant: TenantId, inst: Uuid) -> Result<Vec<Loaded>, KernelError> {
    let rows = sqlx::query(&format!(
        "SELECT {TICKET_COLUMNS}, tags, first_response_at, updated_at FROM campus.service_ticket \
         WHERE tenant_id = $1 AND institution_id = $2 \
           AND (status NOT IN ('closed', 'cancelled', 'merged') OR updated_at > now() - interval '30 days') \
         ORDER BY updated_at DESC LIMIT 2000"
    ))
    .bind(tenant.as_uuid())
    .bind(inst)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    rows.iter()
        .map(|r| {
            Ok(Loaded {
                ticket: ticket_from_row(r)?,
                tags: r.try_get("tags")?,
                first: r.try_get("first_response_at")?,
                updated: r.try_get("updated_at")?,
            })
        })
        .collect::<Result<_, sqlx::Error>>()
        .map_err(map_db_error)
}

fn row_of(l: &Loaded, sla: &Sla, now: DateTime<Utc>) -> ServiceDeskTicketRow {
    let snap = to_rules(&l.ticket, l.tags.clone(), l.first, l.updated);
    let state = automation::board_state(&snap, sla, now);
    ServiceDeskTicketRow {
        ticket: TicketView::from(&l.ticket),
        tags: l.tags.clone(),
        first_response_at: l.first,
        sla_in: state.as_ref().map(|s| short_duration(s.remaining)),
        sla: state,
    }
}

async fn effective_policy(conn: &mut PgConnection, tenant: TenantId, inst: Uuid) -> Result<Sla, KernelError> {
    Ok(load_policy(conn, tenant, InstitutionId(inst)).await?.unwrap_or_else(default_sla))
}

/// The SLA board: unsolved tickets by the target due soonest, breaches first (`campus.read`).
#[utoipa::path(get, path = "/api/v1/campus/institutions/{id}/service-desk/sla-board", operation_id = "campus_servicedesk_sla_board", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ServiceDeskQueue), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn sla_board(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ServiceDeskQueue>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let sla = effective_policy(&mut conn, t, id).await?;
    let loaded = load_open_and_recent(&mut conn, t, id).await?;
    let now = Utc::now();
    let mut items: Vec<ServiceDeskTicketRow> = loaded
        .iter()
        .map(|l| row_of(l, &sla, now))
        .filter(|r| r.sla.is_some())
        .collect();
    items.sort_by(|a, b| {
        let due = |r: &ServiceDeskTicketRow| r.sla.as_ref().map_or(f64::MAX, |s| s.due_at);
        due(a).partial_cmp(&due(b)).unwrap_or(std::cmp::Ordering::Equal)
    });
    let breached = items.iter().filter(|r| r.sla.as_ref().is_some_and(|s| s.breached)).count() as u32;
    Ok(Json(ServiceDeskQueue { title: "SLA board".into(), breached, items }))
}

/// Tickets of a view or the SLA board.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ServiceDeskQueue {
    pub title: String,
    /// How many listed tickets are past a target.
    pub breached: u32,
    pub items: Vec<ServiceDeskTicketRow>,
}

// ------------------------------------------------------------------ views

/// A saved queue: conditions over tickets.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ServiceDeskView {
    /// `sys-…` for the built-in views.
    pub id: String,
    pub title: String,
    pub conditions: Conditions,
    /// `updated_desc`, `created_asc`, `created_desc` or `sla_asc`.
    pub sort: String,
    pub system: bool,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ServiceDeskViewList {
    pub items: Vec<ServiceDeskView>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServiceDeskViewInput {
    pub title: String,
    #[serde(default)]
    pub conditions: Conditions,
    pub sort: Option<String>,
}

fn c(field: &str, op: &str, value: serde_json::Value) -> Condition {
    Condition { field: field.into(), op: op.into(), value }
}

/// Built-in views (Everything's system views in campus words).
pub fn system_views() -> Vec<ServiceDeskView> {
    let v = |id: &str, title: &str, all: Vec<Condition>, sort: &str| ServiceDeskView {
        id: id.into(),
        title: title.into(),
        conditions: Conditions { all, any: vec![] },
        sort: sort.into(),
        system: true,
    };
    let unsolved = || c("status", "less_than", serde_json::json!("resolved"));
    vec![
        v("sys-mine", "Your unsolved tickets", vec![c("assignee", "is", serde_json::json!("me")), unsolved()], "updated_desc"),
        v("sys-unassigned", "Unassigned tickets", vec![c("assignee", "is", serde_json::json!("")), unsolved()], "created_asc"),
        v("sys-unsolved", "All unsolved tickets", vec![unsolved()], "sla_asc"),
        v("sys-complaints", "Open complaints", vec![c("field:complaint", "is", serde_json::json!(true)), unsolved()], "sla_asc"),
        v("sys-on-hold", "On hold", vec![c("status", "is", serde_json::json!("on_hold"))], "updated_desc"),
        v("sys-recent", "Recently updated", vec![c("updated", "less_than", serde_json::json!(168))], "updated_desc"),
        v("sys-solved", "Recently resolved", vec![c("status", "greater_than", serde_json::json!("on_hold")), c("solved", "less_than", serde_json::json!(168))], "updated_desc"),
    ]
}

fn view_from_row(r: &PgRow) -> Result<ServiceDeskView, sqlx::Error> {
    let id: Uuid = r.try_get("id")?;
    let conditions: serde_json::Value = r.try_get("conditions")?;
    Ok(ServiceDeskView {
        id: id.to_string(),
        title: r.try_get("title")?,
        conditions: serde_json::from_value(conditions).unwrap_or_default(),
        sort: r.try_get("sort")?,
        system: false,
    })
}

/// Built-in and saved views (`campus.read`).
#[utoipa::path(get, path = "/api/v1/campus/institutions/{id}/service-desk/views", operation_id = "campus_servicedesk_view_list", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ServiceDeskViewList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn view_list(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ServiceDeskViewList>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let rows = sqlx::query(
        "SELECT id, title, conditions, sort FROM campus.ticket_view WHERE tenant_id = $1 \
         AND institution_id = $2 ORDER BY position, created_at",
    )
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let mut items = system_views();
    for r in &rows {
        items.push(view_from_row(r).map_err(map_db_error)?);
    }
    Ok(Json(ServiceDeskViewList { items }))
}

/// Save a view (`campus.facility.manage`).
#[utoipa::path(post, path = "/api/v1/campus/institutions/{id}/service-desk/views", operation_id = "campus_servicedesk_view_create", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = ServiceDeskViewInput, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = ServiceDeskView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn view_create(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ServiceDeskViewInput>,
) -> ApiResult<(StatusCode, Json<ServiceDeskView>)> {
    let ctx = auth.require(Permission::FacilityManage)?;
    require_idempotency_key(&headers)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let mut v = Vec::new();
    let n = b.title.trim().chars().count();
    if n == 0 || n > 120 {
        v.push(FieldViolation::new("title", "invalid", "1-120 characters"));
    }
    let sort = b.sort.clone().unwrap_or_else(|| "updated_desc".into());
    if !matches!(sort.as_str(), "updated_desc" | "created_asc" | "created_desc" | "sla_asc") {
        v.push(FieldViolation::new("sort", "unknown", "updated_desc, created_asc, created_desc or sla_asc"));
    }
    check_conditions("conditions", &b.conditions, &mut v);
    if b.conditions.all.is_empty() && b.conditions.any.is_empty() {
        v.push(FieldViolation::new("conditions", "required", "a view needs at least one condition"));
    }
    finish(v)?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let row = sqlx::query(
        "INSERT INTO campus.ticket_view (tenant_id, institution_id, title, conditions, sort, created_by) \
         VALUES ($1,$2,$3,$4,$5,$6) RETURNING id, title, conditions, sort",
    )
    .bind(scope.tenant_id.as_uuid())
    .bind(id)
    .bind(b.title.trim())
    .bind(serde_json::to_value(&b.conditions).unwrap_or_default())
    .bind(&sort)
    .bind(a.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let view = view_from_row(&row).map_err(map_db_error)?;
    let vid = Uuid::parse_str(&view.id).unwrap_or_default();
    audit(&mut tx, &scope, "campus.ticket_view.create", "ticket_view", vid, serde_json::json!({"title": view.title}), &a).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok((StatusCode::CREATED, Json(view)))
}

/// Delete a saved view (`campus.facility.manage`).
#[utoipa::path(delete, path = "/api/v1/campus/service-desk/views/{id}", operation_id = "campus_servicedesk_view_delete", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 204), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn view_delete(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let ctx = auth.require(Permission::FacilityManage)?;
    let t = ctx.scope.tenant_id;
    let inst: Option<Uuid> = sqlx::query_scalar("SELECT institution_id FROM campus.ticket_view WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(t.as_uuid())
        .fetch_optional(&st.pool)
        .await
        .map_err(map_db_error)?;
    let inst = InstitutionId(inst.ok_or(KernelError::NotFound { entity: "service desk view" })?);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    sqlx::query("DELETE FROM campus.ticket_view WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(t.as_uuid())
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    audit(&mut tx, &scope, "campus.ticket_view.delete", "ticket_view", id, serde_json::json!({}), &a).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(StatusCode::NO_CONTENT)
}

/// The tickets of a view, built-in (`sys-…`) or saved (`campus.read`).
#[utoipa::path(get, path = "/api/v1/campus/institutions/{id}/service-desk/views/{viewId}/tickets", operation_id = "campus_servicedesk_view_tickets", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid), ("viewId" = String, Path)), security(("session_cookie" = [])),
    responses((status = 200, body = ServiceDeskQueue), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn view_tickets(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path((id, view_id)): Path<(Uuid, String)>,
) -> ApiResult<Json<ServiceDeskQueue>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let t = ctx.scope.tenant_id;
    let view = match system_views().into_iter().find(|v| v.id == view_id) {
        Some(v) => v,
        None => {
            let vid = Uuid::parse_str(&view_id).map_err(|_| KernelError::NotFound { entity: "service desk view" })?;
            let row = sqlx::query(
                "SELECT id, title, conditions, sort FROM campus.ticket_view WHERE id = $1 \
                 AND tenant_id = $2 AND institution_id = $3",
            )
            .bind(vid)
            .bind(t.as_uuid())
            .bind(id)
            .fetch_optional(&st.pool)
            .await
            .map_err(map_db_error)?
            .ok_or(KernelError::NotFound { entity: "service desk view" })?;
            view_from_row(&row).map_err(map_db_error)?
        }
    };
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let sla = effective_policy(&mut conn, t, id).await?;
    let loaded = load_open_and_recent(&mut conn, t, id).await?;
    let now = Utc::now();
    let vocab = Vocab::campus();
    let me = ctx.subject.clone();
    let rctx = Ctx { me: Some(&me), now: now.timestamp_millis() as f64, ..Ctx::default() };
    let mut picked: Vec<&Loaded> = loaded
        .iter()
        .filter(|l| {
            let snap = to_rules(&l.ticket, l.tags.clone(), l.first, l.updated);
            rules::in_view(None, &view.conditions, &snap, &rctx, &vocab)
        })
        .collect();
    match view.sort.as_str() {
        "created_asc" => picked.sort_by_key(|l| l.ticket.created_at),
        "created_desc" => picked.sort_by_key(|l| std::cmp::Reverse(l.ticket.created_at)),
        "sla_asc" => picked.sort_by_key(|l| l.ticket.sla_due_at),
        _ => picked.sort_by_key(|l| std::cmp::Reverse(l.updated)),
    }
    let items: Vec<ServiceDeskTicketRow> = picked.into_iter().take(500).map(|l| row_of(l, &sla, now)).collect();
    let breached = items.iter().filter(|r| r.sla.as_ref().is_some_and(|s| s.breached)).count() as u32;
    Ok(Json(ServiceDeskQueue { title: view.title, breached, items }))
}

// ------------------------------------------------------------------ rule preview

/// A trigger to try against current tickets without saving it.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServiceDeskRulePreviewInput {
    #[serde(default)]
    pub conditions: Conditions,
    pub actions: Vec<Action>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ServiceDeskRulePreviewHit {
    pub ticket_id: Uuid,
    pub number: String,
    pub title: String,
    #[schema(value_type = Object)]
    pub changes: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ServiceDeskRulePreview {
    /// Unsolved tickets the conditions match now.
    pub matched: u32,
    pub hits: Vec<ServiceDeskRulePreviewHit>,
}

/// Which unsolved tickets a trigger would match now and what it would change (`campus.read`).
#[utoipa::path(post, path = "/api/v1/campus/institutions/{id}/service-desk/rules/preview", operation_id = "campus_servicedesk_rule_preview", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid)), request_body = ServiceDeskRulePreviewInput, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ServiceDeskRulePreview), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn rule_preview(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(b): Json<ServiceDeskRulePreviewInput>,
) -> ApiResult<Json<ServiceDeskRulePreview>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let mut v = Vec::new();
    check_conditions("conditions", &b.conditions, &mut v);
    check_actions(&b.actions, &mut v);
    finish(v)?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let loaded = load_open_and_recent(&mut conn, t, id).await?;
    let vocab = Vocab::campus();
    let trig = Trigger { id: "preview".into(), title: "Preview".into(), active: true, event: None, conditions: b.conditions.clone(), actions: b.actions.clone() };
    let me = ctx.subject.clone();
    let rctx = Ctx { me: Some(&me), now: Utc::now().timestamp_millis() as f64, actor: Some("agent"), ..Ctx::default() };
    let mut matched = 0;
    let mut hits = Vec::new();
    for l in loaded.iter().filter(|l| vocab.is_unsolved(l.ticket.status.as_str())) {
        let snap = to_rules(&l.ticket, l.tags.clone(), l.first, l.updated);
        if !matches(&b.conditions, &snap, &rctx, &vocab) {
            continue;
        }
        matched += 1;
        let (after, _, _) = run_triggers(std::slice::from_ref(&trig), &snap, Event::Updated, &rctx, &vocab);
        if hits.len() < 100 {
            hits.push(ServiceDeskRulePreviewHit {
                ticket_id: l.ticket.id.as_uuid(),
                number: l.ticket.number.clone(),
                title: l.ticket.title.clone(),
                changes: serde_json::to_value(diff(&snap, &after)).unwrap_or_default(),
            });
        }
    }
    Ok(Json(ServiceDeskRulePreview { matched, hits }))
}

// ------------------------------------------------------------------ SLA policy

/// The SLA policy (effective: the defaults until the institution saves one).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ServiceDeskSlaPolicy {
    pub enabled: bool,
    /// Minutes per priority (`low`, `medium`, `high`, `critical`).
    pub targets: BTreeMap<String, SlaTarget>,
    /// `false` while the defaults apply.
    pub customised: bool,
    pub version: i64,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServiceDeskSlaPolicyInput {
    pub enabled: bool,
    pub targets: BTreeMap<String, SlaTarget>,
    /// The version read (0 before anything was saved).
    pub version: i64,
}

async fn policy_wire(conn: &mut PgConnection, tenant: TenantId, inst: Uuid) -> Result<ServiceDeskSlaPolicy, KernelError> {
    let version: Option<i64> = sqlx::query_scalar("SELECT version FROM campus.ticket_sla_policy WHERE tenant_id = $1 AND institution_id = $2")
        .bind(tenant.as_uuid())
        .bind(inst)
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?;
    let sla = effective_policy(conn, tenant, inst).await?;
    Ok(ServiceDeskSlaPolicy { enabled: sla.enabled, targets: sla.targets, customised: version.is_some(), version: version.unwrap_or(0) })
}

/// The service desk's SLA targets (`campus.read`).
#[utoipa::path(get, path = "/api/v1/campus/institutions/{id}/service-desk/sla-policy", operation_id = "campus_servicedesk_sla_policy_get", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ServiceDeskSlaPolicy), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn sla_policy_get(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ServiceDeskSlaPolicy>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    Ok(Json(policy_wire(&mut conn, ctx.scope.tenant_id, id).await?))
}

/// Save the SLA targets (`campus.facility.manage`). New tickets and re-prioritised ones are
/// timed from them; existing due times stay.
#[utoipa::path(put, path = "/api/v1/campus/institutions/{id}/service-desk/sla-policy", operation_id = "campus_servicedesk_sla_policy_put", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid)), request_body = ServiceDeskSlaPolicyInput, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ServiceDeskSlaPolicy), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn sla_policy_put(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(b): Json<ServiceDeskSlaPolicyInput>,
) -> ApiResult<Json<ServiceDeskSlaPolicy>> {
    let ctx = auth.require(Permission::FacilityManage)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let mut v = Vec::new();
    for (p, tg) in &b.targets {
        if !Vocab::campus().priorities.iter().any(|x| x == p) {
            v.push(FieldViolation::new(format!("targets.{p}"), "unknown", "priorities are low, medium, high, critical"));
        }
        for (name, m) in [("first", tg.first), ("next", tg.next), ("resolve", tg.resolve)] {
            if !(0..=525_600).contains(&m) {
                v.push(FieldViolation::new(format!("targets.{p}.{name}"), "range", "0 to 525600 minutes"));
            }
        }
        if tg.resolve == 0 {
            v.push(FieldViolation::new(format!("targets.{p}.resolve"), "required", "every priority needs a resolution target"));
        }
        if tg.first > 0 && tg.first > tg.resolve {
            v.push(FieldViolation::new(format!("targets.{p}.first"), "range", "the first response is due before resolution"));
        }
    }
    finish(v)?;
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
        .bind(format!("campus.ticket_sla_policy:{id}"))
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let cur = policy_wire(&mut tx, t, id).await?;
    ensure_version(b.version, cur.version)?;
    // Priorities left out keep their default targets.
    let mut targets = default_sla().targets;
    targets.extend(b.targets.clone());
    sqlx::query(
        "INSERT INTO campus.ticket_sla_policy (tenant_id, institution_id, enabled, targets, updated_by) \
         VALUES ($1,$2,$3,$4,$5) ON CONFLICT (tenant_id, institution_id) DO UPDATE SET \
         enabled = EXCLUDED.enabled, targets = EXCLUDED.targets, updated_by = EXCLUDED.updated_by, \
         version = campus.ticket_sla_policy.version + 1, updated_at = now()",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(b.enabled)
    .bind(serde_json::to_value(&targets).unwrap_or_default())
    .bind(a.id.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit(&mut tx, &scope, "campus.ticket_sla_policy.update", "ticket_sla_policy", id, serde_json::json!({"enabled": b.enabled, "targets": targets}), &a).await?;
    let out = policy_wire(&mut tx, t, id).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(out))
}

// ------------------------------------------------------------------ tags + automation log

/// Tags to add and remove.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServiceDeskTagChange {
    #[serde(default)]
    pub add: Vec<String>,
    #[serde(default)]
    pub remove: Vec<String>,
}

/// Tag or untag a ticket (`campus.facility.manage`, `If-Match`); triggers run as on any change.
#[utoipa::path(post, path = "/api/v1/campus/tickets/{id}/tags", operation_id = "campus_servicedesk_ticket_tags", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the ticket")),
    request_body = ServiceDeskTagChange, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ServiceDeskTicketRow, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn ticket_tags(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ServiceDeskTagChange>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacilityManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let inst: Option<Uuid> = sqlx::query_scalar("SELECT institution_id FROM campus.service_ticket WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(t.as_uuid())
        .fetch_optional(&st.pool)
        .await
        .map_err(map_db_error)?;
    let inst = InstitutionId(inst.ok_or(KernelError::NotFound { entity: "service ticket" })?);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let add = rules::norm_tags(&serde_json::json!(b.add));
    let remove = rules::norm_tags(&serde_json::json!(b.remove));
    if add.is_empty() && remove.is_empty() {
        return Err(invalid("add", "required", "add or remove at least one tag").into());
    }
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let before = lock_ticket(&mut tx, t, ServiceTicketId(id)).await?;
    ensure_version(v, before.version)?;
    let current: Vec<String> = sqlx::query_scalar("SELECT tags FROM campus.service_ticket WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(t.as_uuid())
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let mut tags: Vec<String> = current.into_iter().filter(|x| !remove.contains(x)).collect();
    for x in add {
        if !tags.contains(&x) {
            tags.push(x);
        }
    }
    if tags.len() > rules::MAX_TAGS {
        return Err(invalid("add", "too_many", "at most 30 tags").into());
    }
    sqlx::query("UPDATE campus.service_ticket SET tags = $3, version = version + 1, updated_at = now() WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(t.as_uuid())
        .bind(&tags)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let moved = lock_ticket(&mut tx, t, ServiceTicketId(id)).await?;
    let out = automation::after_event(&mut tx, &scope, moved, Some(&before), Event::Updated, &a).await?;
    audit(&mut tx, &scope, "campus.service_ticket.tags", "service_ticket", id, serde_json::json!({"tags": tags}), &a).await?;
    let row = sqlx::query(&format!("SELECT {TICKET_COLUMNS}, tags, first_response_at, updated_at FROM campus.service_ticket WHERE id = $1 AND tenant_id = $2"))
        .bind(id)
        .bind(t.as_uuid())
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let loaded = Loaded {
        ticket: out,
        tags: row.try_get("tags").map_err(map_db_error)?,
        first: row.try_get("first_response_at").map_err(map_db_error)?,
        updated: row.try_get("updated_at").map_err(map_db_error)?,
    };
    let sla = effective_policy(&mut tx, t, inst.as_uuid()).await?;
    tx.commit().await.map_err(map_db_error)?;
    let r = row_of(&loaded, &sla, Utc::now());
    Ok(with_etag(StatusCode::OK, &r, Version(loaded.ticket.version)))
}

/// One trigger firing on a ticket.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ServiceDeskRuleRun {
    pub rule_id: Option<Uuid>,
    pub rule_title: String,
    pub event: String,
    #[schema(value_type = Object)]
    pub changes: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ServiceDeskRuleRunList {
    pub items: Vec<ServiceDeskRuleRun>,
}

/// What the service-desk triggers did to a ticket (`campus.read`).
#[utoipa::path(get, path = "/api/v1/campus/tickets/{id}/automation", operation_id = "campus_servicedesk_ticket_automation", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ServiceDeskRuleRunList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn ticket_automation(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ServiceDeskRuleRunList>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let rows = sqlx::query(
        "SELECT rule_id, rule_title, event, changes, created_at FROM campus.ticket_rule_run \
         WHERE tenant_id = $1 AND ticket_id = $2 ORDER BY created_at, id",
    )
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let items = rows
        .iter()
        .map(|r| {
            Ok(ServiceDeskRuleRun {
                rule_id: r.try_get("rule_id")?,
                rule_title: r.try_get("rule_title")?,
                event: r.try_get("event")?,
                changes: r.try_get("changes")?,
                created_at: r.try_get("created_at")?,
            })
        })
        .collect::<Result<_, sqlx::Error>>()
        .map_err(map_db_error)?;
    Ok(Json(ServiceDeskRuleRunList { items }))
}

pub fn router(state: CampusState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(rule_list, rule_create))
        .routes(routes!(rule_update, rule_delete))
        .routes(routes!(rule_preview))
        .routes(routes!(view_list, view_create))
        .routes(routes!(view_delete))
        .routes(routes!(view_tickets))
        .routes(routes!(sla_board))
        .routes(routes!(sla_policy_get, sla_policy_put))
        .routes(routes!(ticket_tags))
        .routes(routes!(ticket_automation))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_rejects_status_actions_and_unknown_fields() {
        let mut v = Vec::new();
        check_actions(&[Action { field: "status".into(), value: serde_json::json!("resolved"), to: None, subject: None, body: None }], &mut v);
        assert_eq!(v.len(), 1);
        let mut v = Vec::new();
        check_conditions(
            "c",
            &Conditions {
                all: vec![c("priority", "greater_than", serde_json::json!("medium")), c("budget", "is", serde_json::json!("x")), c("created", "greater_than", serde_json::json!("abc"))],
                any: vec![],
            },
            &mut v,
        );
        assert_eq!(v.len(), 2, "{v:?}");
        assert_eq!(system_views().len(), 7);
    }
}
