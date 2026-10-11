//! Per-user to-do inbox: actionable items with a due date and a source link, created by the
//! user, assigned by staff, or created from domain events (the inbox consumer).
//!
//! | Endpoint | Permission |
//! |----------|------------|
//! | `GET  /api/v1/todos?status&limit&cursor` | `todo.use` (own items only) |
//! | `POST /api/v1/todos` | `todo.use`; assigning to someone else needs `todo.assign` (+ CSRF, `Idempotency-Key`) |
//! | `POST /api/v1/todos/{id}/complete` · `/dismiss` · `/reopen` | `todo.use`, assignee only |
//!
//! Assigning to another person emits `sutraerp.platform.todo.assigned` (outbox), which the
//! consumer turns into a notification for the assignee.

use super::{
    actor, check_link, check_text, institution_of, page_limit, profile, violations, with_etag,
    InboxState,
};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, Utc};
use sqlx::{PgConnection, Row};
use sutra_contracts::platform::events::{
    TodoAssignedV1, SOURCE, TODO_ASSIGNED, TODO_ASSIGNED_SCHEMA,
};
use sutra_kernel::audit::{ActorKind, AuditEntry, AuditOutcome};
use sutra_kernel::auth::Permission;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::events::{EventEnvelope, NewEvent};
use sutra_kernel::scope::TenantId;
use sutra_persistence::{audit, map_db_error, outbox};
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum TodoStatus {
    Open,
    Completed,
    Dismissed,
}

impl TodoStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Completed => "completed",
            Self::Dismissed => "dismissed",
        }
    }
    fn parse(s: &str) -> Self {
        match s {
            "completed" => Self::Completed,
            "dismissed" => Self::Dismissed,
            _ => Self::Open,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum TodoSource {
    /// Created by a person.
    Manual,
    /// Created from a domain event.
    Event,
}

#[derive(Clone, Debug, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Todo {
    pub id: Uuid,
    pub title: String,
    pub detail: String,
    pub due_at: Option<DateTime<Utc>>,
    /// Application path where the work happens.
    pub link: Option<String>,
    pub status: TodoStatus,
    pub source: TodoSource,
    /// Subject that assigned it (`null` = created by the system or by the assignee).
    pub assigned_by: Option<String>,
    pub overdue: bool,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub dismissed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TodoPage {
    pub items: Vec<Todo>,
    pub next_cursor: Option<Uuid>,
    /// Open items of the caller (all pages).
    pub open_count: i64,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewTodo {
    pub title: String,
    #[serde(default)]
    pub detail: String,
    pub due_at: Option<DateTime<Utc>>,
    /// Application path (e.g. `/academics`).
    pub link: Option<String>,
    /// Assign to this person (login id of someone who has signed in before); omitted = me.
    pub assignee_login_id: Option<String>,
}

impl NewTodo {
    pub fn validate(&self) -> Result<(), KernelError> {
        let mut v = vec![];
        check_text("title", &self.title, 1, 200, false, &mut v);
        check_text("detail", &self.detail, 0, 4000, true, &mut v);
        check_link("link", self.link.as_deref(), &mut v);
        violations(v)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum TodoFilter {
    #[default]
    Open,
    Completed,
    Dismissed,
    All,
}

#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct TodoQuery {
    /// Default `open`.
    #[param(inline)]
    pub status: Option<TodoFilter>,
    pub limit: Option<u32>,
    pub cursor: Option<Uuid>,
}

const COLUMNS: &str =
    "id, title, detail, due_at, link, status, source_kind, assigned_by, version, \
    created_at, updated_at, completed_at, dismissed_at, now() AS db_now";

fn from_row(r: &sqlx::postgres::PgRow) -> Result<Todo, sqlx::Error> {
    let status = TodoStatus::parse(&r.try_get::<String, _>("status")?);
    let due_at: Option<DateTime<Utc>> = r.try_get("due_at")?;
    let now: DateTime<Utc> = r.try_get("db_now")?;
    Ok(Todo {
        id: r.try_get("id")?,
        title: r.try_get("title")?,
        detail: r.try_get("detail")?,
        due_at,
        link: r.try_get("link")?,
        status,
        source: if r.try_get::<String, _>("source_kind")? == "event" {
            TodoSource::Event
        } else {
            TodoSource::Manual
        },
        assigned_by: r.try_get("assigned_by")?,
        overdue: status == TodoStatus::Open && due_at.is_some_and(|d| d < now),
        version: r.try_get("version")?,
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
        completed_at: r.try_get("completed_at")?,
        dismissed_at: r.try_get("dismissed_at")?,
    })
}

/// A to-do created by the system (event consumer). Idempotent per (assignee, `source_ref`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SystemTodo {
    pub tenant: TenantId,
    pub institution: Option<Uuid>,
    pub assignee_subject: String,
    pub title: String,
    pub detail: String,
    pub due_at: Option<DateTime<Utc>>,
    pub link: Option<String>,
    pub source_ref: String,
}

/// Inserts an event-created to-do; `None` when it already existed.
pub async fn create_from_event(
    conn: &mut PgConnection,
    t: &SystemTodo,
) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "INSERT INTO platform.todo \
           (tenant_id, institution_id, assignee_subject, title, detail, due_at, link, \
            source_kind, source_ref) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, 'event', $8) \
         ON CONFLICT (tenant_id, assignee_subject, source_ref) WHERE source_ref IS NOT NULL \
           DO NOTHING RETURNING id",
    )
    .bind(t.tenant.as_uuid())
    .bind(t.institution)
    .bind(&t.assignee_subject)
    .bind(&t.title)
    .bind(&t.detail)
    .bind(t.due_at)
    .bind(&t.link)
    .bind(&t.source_ref)
    .fetch_optional(conn)
    .await
}

/// The caller's to-dos (newest first).
#[utoipa::path(
    get,
    path = "/api/v1/todos",
    operation_id = "list_todos",
    tag = "todos",
    params(TodoQuery),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = TodoPage),
        (status = 401, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn list(
    State(st): State<InboxState>,
    auth: Authenticated,
    Query(q): Query<TodoQuery>,
) -> ApiResult<Json<TodoPage>> {
    let ctx = auth.require(Permission::TodoUse)?;
    let limit = page_limit(q.limit, 50);
    let status: Option<&str> = match q.status.unwrap_or_default() {
        TodoFilter::Open => Some("open"),
        TodoFilter::Completed => Some("completed"),
        TodoFilter::Dismissed => Some("dismissed"),
        TodoFilter::All => None,
    };
    let rows = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM platform.todo \
         WHERE tenant_id = $1 AND assignee_subject = $2 \
           AND (institution_id IS NULL OR $3::uuid IS NULL OR institution_id = $3) \
           AND ($4::text IS NULL OR status = $4) AND ($5::uuid IS NULL OR id < $5) \
         ORDER BY id DESC LIMIT $6"
    ))
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(&ctx.subject)
    .bind(institution_of(&ctx.scope))
    .bind(status)
    .bind(q.cursor)
    .bind(limit + 1)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let mut items = rows
        .iter()
        .map(from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)?;
    let next_cursor = if items.len() as i64 > limit {
        items.truncate(limit as usize);
        items.last().map(|t| t.id)
    } else {
        None
    };
    let open_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM platform.todo \
         WHERE tenant_id = $1 AND assignee_subject = $2 AND status = 'open' \
           AND (institution_id IS NULL OR $3::uuid IS NULL OR institution_id = $3)",
    )
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(&ctx.subject)
    .bind(institution_of(&ctx.scope))
    .fetch_one(&st.pool)
    .await
    .map_err(map_db_error)?;
    Ok(Json(TodoPage {
        items,
        next_cursor,
        open_count,
    }))
}

/// Create a to-do for yourself, or assign one (`todo.assign`; `Idempotency-Key` required).
#[utoipa::path(
    post,
    path = "/api/v1/todos",
    operation_id = "create_todo",
    tag = "todos",
    params(("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = NewTodo,
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 201, body = Todo, headers(("ETag" = String))),
        (status = 401, body = Problem, content_type = "application/problem+json"),
        (status = 403, description = "Assigning needs `todo.assign`", body = Problem, content_type = "application/problem+json"),
        (status = 422, description = "Invalid, or unknown assignee", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn create(
    State(st): State<InboxState>,
    auth: Authenticated,
    headers: HeaderMap,
    Json(cmd): Json<NewTodo>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::TodoUse)?;
    require_idempotency_key(&headers)?;
    cmd.validate()?;
    let assignee_login = cmd
        .assignee_login_id
        .as_deref()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.eq_ignore_ascii_case(&ctx.login_id));
    let (assignee, assigned_by) = match assignee_login {
        None => (ctx.subject.clone(), None),
        Some(login) => {
            auth.require(Permission::TodoAssign)?;
            let (subject, _) = profile::subject_for_login(&st.pool, &ctx.scope, login)
                .await?
                .ok_or_else(|| {
                    KernelError::Validation(vec![FieldViolation::new(
                        "assigneeLoginId",
                        "unknown",
                        "no one with this login has signed in to this tenant yet",
                    )])
                })?;
            if subject == ctx.subject {
                (subject, None)
            } else {
                (subject, Some(ctx.subject.clone()))
            }
        }
    };
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let row = sqlx::query(&format!(
        "INSERT INTO platform.todo \
           (tenant_id, institution_id, assignee_subject, assigned_by, title, detail, due_at, link) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING {COLUMNS}"
    ))
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(institution_of(&ctx.scope))
    .bind(&assignee)
    .bind(&assigned_by)
    .bind(cmd.title.trim())
    .bind(cmd.detail.trim())
    .bind(cmd.due_at)
    .bind(&cmd.link)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let t = from_row(&row).map_err(map_db_error)?;
    if assigned_by.is_some() {
        let (actor_id, correlation) = actor(ctx);
        let env = EventEnvelope::new(
            &ctx.scope,
            NewEvent {
                source: SOURCE,
                event_type: TODO_ASSIGNED,
                dataschema: TODO_ASSIGNED_SCHEMA,
                subject: Some(t.id.to_string()),
                data: serde_json::to_value(TodoAssignedV1 {
                    todo_id: t.id,
                    assignee_subject: assignee.clone(),
                    title: t.title.clone(),
                    assigned_by: actor_id.clone(),
                    assigned_by_name: ctx.display_name.clone(),
                    due_at: t.due_at,
                })
                .map_err(KernelError::internal)?,
            },
        )
        .with_correlation(correlation.clone())
        .with_actor(Some(actor_id.clone()));
        outbox::enqueue(&mut *tx, &env)
            .await
            .map_err(map_db_error)?;
        audit::insert(
            &mut *tx,
            &AuditEntry::new(ctx.scope, "platform.todo.assign", AuditOutcome::Succeeded)
                .actor(ActorKind::User, Some(actor_id))
                .target("todo", t.id.to_string())
                .correlation(correlation)
                .details(serde_json::json!({ "assignee": assignee })),
        )
        .await
        .map_err(map_db_error)?;
    }
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::CREATED, t.version, t))
}

async fn transition(
    st: &InboxState,
    auth: &Authenticated,
    id: Uuid,
    to: TodoStatus,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::TodoUse)?;
    let (set, from_ok) = match to {
        TodoStatus::Completed => (
            "status = 'completed', completed_at = now(), dismissed_at = NULL",
            "status = 'open'",
        ),
        TodoStatus::Dismissed => (
            "status = 'dismissed', dismissed_at = now(), completed_at = NULL",
            "status = 'open'",
        ),
        TodoStatus::Open => (
            "status = 'open', completed_at = NULL, dismissed_at = NULL",
            "status <> 'open'",
        ),
    };
    let scope_filter = "tenant_id = $2 AND assignee_subject = $3 \
        AND (institution_id IS NULL OR $4::uuid IS NULL OR institution_id = $4)";
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let row = sqlx::query(&format!(
        "UPDATE platform.todo SET {set}, version = version + 1, updated_at = now() \
         WHERE id = $1 AND {scope_filter} AND {from_ok} RETURNING {COLUMNS}"
    ))
    .bind(id)
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(&ctx.subject)
    .bind(institution_of(&ctx.scope))
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let t = match row {
        Some(r) => from_row(&r).map_err(map_db_error)?,
        None => {
            // Already in the target state (idempotent) or not the caller's (404).
            let r = sqlx::query(&format!(
                "SELECT {COLUMNS} FROM platform.todo WHERE id = $1 AND {scope_filter}"
            ))
            .bind(id)
            .bind(ctx.scope.tenant_id.as_uuid())
            .bind(&ctx.subject)
            .bind(institution_of(&ctx.scope))
            .fetch_optional(&mut *tx)
            .await
            .map_err(map_db_error)?
            .ok_or(KernelError::NotFound { entity: "todo" })?;
            let t = from_row(&r).map_err(map_db_error)?;
            if t.status == to {
                return Ok(with_etag(StatusCode::OK, t.version, t));
            }
            return Err(KernelError::Conflict(format!(
                "a {} to-do cannot become {}",
                t.status.as_str(),
                to.as_str()
            ))
            .into());
        }
    };
    let (actor_id, correlation) = actor(ctx);
    audit::insert(
        &mut *tx,
        &AuditEntry::new(
            ctx.scope,
            match to {
                TodoStatus::Completed => "platform.todo.complete",
                TodoStatus::Dismissed => "platform.todo.dismiss",
                TodoStatus::Open => "platform.todo.reopen",
            },
            AuditOutcome::Succeeded,
        )
        .actor(ActorKind::User, Some(actor_id))
        .target("todo", t.id.to_string())
        .correlation(correlation),
    )
    .await
    .map_err(map_db_error)?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, t.version, t))
}

/// Mark one of your to-dos done.
#[utoipa::path(
    post,
    path = "/api/v1/todos/{id}/complete",
    operation_id = "complete_todo",
    tag = "todos",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 200, body = Todo),
        (status = 404, description = "Not one of your to-dos", body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn complete(
    State(st): State<InboxState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    transition(&st, &auth, id, TodoStatus::Completed).await
}

/// Dismiss one of your to-dos (no longer relevant).
#[utoipa::path(
    post,
    path = "/api/v1/todos/{id}/dismiss",
    operation_id = "dismiss_todo",
    tag = "todos",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 200, body = Todo),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn dismiss(
    State(st): State<InboxState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    transition(&st, &auth, id, TodoStatus::Dismissed).await
}

/// Reopen a completed or dismissed to-do.
#[utoipa::path(
    post,
    path = "/api/v1/todos/{id}/reopen",
    operation_id = "reopen_todo",
    tag = "todos",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 200, body = Todo),
        (status = 404, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn reopen(
    State(st): State<InboxState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    transition(&st, &auth, id, TodoStatus::Open).await
}

pub fn routes() -> OpenApiRouter<InboxState> {
    OpenApiRouter::new()
        .routes(routes!(list, create))
        .routes(routes!(complete))
        .routes(routes!(dismiss))
        .routes(routes!(reopen))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation() {
        let ok = NewTodo {
            title: "Submit report".into(),
            detail: String::new(),
            due_at: None,
            link: Some("/reports".into()),
            assignee_login_id: None,
        };
        assert!(ok.validate().is_ok());
        let bad = NewTodo {
            title: " ".into(),
            link: Some("https://evil.example".into()),
            ..ok
        };
        let Err(KernelError::Validation(v)) = bad.validate() else {
            panic!("expected violations")
        };
        let fields: Vec<_> = v.iter().map(|x| x.field.as_str()).collect();
        assert_eq!(fields, ["title", "link"]);
        assert_eq!(TodoStatus::parse("completed"), TodoStatus::Completed);
        assert_eq!(TodoStatus::parse("x"), TodoStatus::Open);
    }
}
