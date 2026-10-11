//! Institution-wide notices: author, role audience, publish/expiry window, pin, archive.
//!
//! | Endpoint | Permission |
//! |----------|------------|
//! | `GET  /api/v1/notices?view=current\|manage` | `notice.read` (`manage` view: `notice.manage`) |
//! | `GET  /api/v1/notices/{id}` | `notice.read` (audience-filtered unless `notice.manage`) |
//! | `POST /api/v1/notices` | `notice.manage` + CSRF + `Idempotency-Key` |
//! | `PATCH /api/v1/notices/{id}` | `notice.manage` + CSRF + `If-Match` |
//! | `POST /api/v1/notices/{id}/archive` | `notice.manage` + CSRF |
//!
//! Readers only ever see **live** notices (active, `publishAt <= now < expiresAt`) whose
//! audience contains one of their roles; anything else is a 404. Every change commits with an
//! audit entry and an outbox event (`sutraerp.platform.notice.published|updated|archived`); the
//! inbox consumer turns `published` into notifications for the audience.

use super::{
    actor, check_text, if_match, in_window, institution_of, page_limit, precondition_required,
    role_names, violations, with_etag, InboxState,
};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, Utc};
use sqlx::{PgConnection, Row};
use sutra_contracts::platform::events::{
    NoticeChangedV1, NOTICE_ARCHIVED, NOTICE_ARCHIVED_SCHEMA, NOTICE_PUBLISHED,
    NOTICE_PUBLISHED_SCHEMA, NOTICE_UPDATED, NOTICE_UPDATED_SCHEMA, SOURCE,
};
use sutra_kernel::audit::{ActorKind, AuditEntry, AuditOutcome};
use sutra_kernel::auth::{AuthContext, Permission, Role};
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::events::{EventEnvelope, NewEvent};
use sutra_persistence::{audit, map_db_error, outbox};
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum NoticeState {
    /// Published in the future.
    Scheduled,
    /// Visible to its audience now.
    Live,
    /// Past its expiry.
    Expired,
    Archived,
}

#[derive(Clone, Debug, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Notice {
    pub id: Uuid,
    pub title: String,
    pub body: String,
    pub audience_roles: Vec<Role>,
    pub pinned: bool,
    pub publish_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub state: NoticeState,
    pub author_name: String,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NoticeList {
    pub items: Vec<Notice>,
    pub total: i64,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewNotice {
    #[schema(example = "Innovation Week begins 23 September")]
    pub title: String,
    pub body: String,
    /// Roles that see the notice (at least one).
    pub audience_roles: Vec<Role>,
    #[serde(default)]
    pub pinned: bool,
    /// Defaults to now.
    pub publish_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
}

/// Partial update; omitted members are unchanged. `clearExpiry: true` removes the expiry.
#[derive(Debug, Default, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateNotice {
    pub title: Option<String>,
    pub body: Option<String>,
    pub audience_roles: Option<Vec<Role>>,
    pub pinned: Option<bool>,
    pub publish_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub clear_expiry: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum NoticeView {
    /// Live notices addressed to the caller (default).
    #[default]
    Current,
    /// Every notice of the tenant, any state (`notice.manage`).
    Manage,
}

#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct NoticeQuery {
    #[param(inline)]
    pub view: Option<NoticeView>,
    /// Page size (1–100, default 50).
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

fn check_audience(field: &str, roles: &[Role], v: &mut Vec<FieldViolation>) {
    if roles.is_empty() {
        v.push(FieldViolation::new(
            field,
            "required",
            "choose at least one audience role",
        ));
    }
}

fn check_window(
    publish_at: DateTime<Utc>,
    expires_at: Option<DateTime<Utc>>,
    v: &mut Vec<FieldViolation>,
) {
    if expires_at.is_some_and(|e| e <= publish_at) {
        v.push(FieldViolation::new(
            "expiresAt",
            "window",
            "expiresAt must be after publishAt",
        ));
    }
}

impl NewNotice {
    pub fn validate(&self, now: DateTime<Utc>) -> Result<(), KernelError> {
        let mut v = vec![];
        check_text("title", &self.title, 1, 200, false, &mut v);
        check_text("body", &self.body, 1, 10_000, true, &mut v);
        check_audience("audienceRoles", &self.audience_roles, &mut v);
        let publish = self.publish_at.unwrap_or(now);
        check_window(publish, self.expires_at, &mut v);
        if self.expires_at.is_some_and(|e| e <= now) {
            v.push(FieldViolation::new(
                "expiresAt",
                "past",
                "expiresAt must be in the future",
            ));
        }
        violations(v)
    }
}

/// Lifecycle state of a notice at `now`.
pub fn state_at(
    archived: bool,
    publish_at: DateTime<Utc>,
    expires_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> NoticeState {
    if archived {
        NoticeState::Archived
    } else if now < publish_at {
        NoticeState::Scheduled
    } else if in_window(now, publish_at, expires_at) {
        NoticeState::Live
    } else {
        NoticeState::Expired
    }
}

/// Audience rule (mirrored by the SQL of the reader queries): live and addressed to one of
/// the reader's roles.
pub fn visible_to(n: &Notice, roles: &[Role]) -> bool {
    n.state == NoticeState::Live && n.audience_roles.iter().any(|r| roles.contains(r))
}

const COLUMNS: &str = "id, title, body, audience_roles, pinned, publish_at, expires_at, status, \
    author_name, version, created_at, updated_at, now() AS db_now";

/// Reader predicate: `$1` tenant, `$2` session institution, `$3` role names.
const LIVE_FOR_READER: &str = "tenant_id = $1 \
    AND (institution_id IS NULL OR $2::uuid IS NULL OR institution_id = $2) \
    AND status = 'active' AND publish_at <= now() \
    AND (expires_at IS NULL OR expires_at > now()) \
    AND audience_roles && $3";

/// Manager predicate: `$1` tenant, `$2` session institution.
const ALL_IN_SCOPE: &str = "tenant_id = $1 \
    AND (institution_id IS NULL OR $2::uuid IS NULL OR institution_id = $2)";

fn from_row(r: &sqlx::postgres::PgRow) -> Result<Notice, sqlx::Error> {
    let roles: Vec<String> = r.try_get("audience_roles")?;
    let status: String = r.try_get("status")?;
    let publish_at: DateTime<Utc> = r.try_get("publish_at")?;
    let expires_at: Option<DateTime<Utc>> = r.try_get("expires_at")?;
    let now: DateTime<Utc> = r.try_get("db_now")?;
    Ok(Notice {
        id: r.try_get("id")?,
        title: r.try_get("title")?,
        body: r.try_get("body")?,
        audience_roles: roles.iter().filter_map(|x| Role::parse(x)).collect(),
        pinned: r.try_get("pinned")?,
        publish_at,
        expires_at,
        state: state_at(status == "archived", publish_at, expires_at, now),
        author_name: r.try_get("author_name")?,
        version: r.try_get("version")?,
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
    })
}

async fn emit(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    n: &Notice,
    kind: (&'static str, &'static str),
    action: &'static str,
) -> Result<(), KernelError> {
    let (actor_id, correlation) = actor(ctx);
    let data = NoticeChangedV1 {
        notice_id: n.id,
        title: n.title.clone(),
        audience_roles: n
            .audience_roles
            .iter()
            .map(|r| r.as_str().to_owned())
            .collect(),
        publish_at: n.publish_at,
        expires_at: n.expires_at,
        pinned: n.pinned,
        version: n.version,
    };
    let env = EventEnvelope::new(
        &ctx.scope,
        NewEvent {
            source: SOURCE,
            event_type: kind.0,
            dataschema: kind.1,
            subject: Some(n.id.to_string()),
            data: serde_json::to_value(&data).map_err(KernelError::internal)?,
        },
    )
    .with_correlation(correlation.clone())
    .with_actor(Some(actor_id.clone()));
    outbox::enqueue(&mut *conn, &env)
        .await
        .map_err(map_db_error)?;
    audit::insert(
        &mut *conn,
        &AuditEntry::new(ctx.scope, action, AuditOutcome::Succeeded)
            .actor(ActorKind::User, Some(actor_id))
            .target("notice", n.id.to_string())
            .correlation(correlation)
            .details(serde_json::json!({ "version": n.version })),
    )
    .await
    .map_err(map_db_error)?;
    Ok(())
}

async fn load_for_manager(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    id: Uuid,
) -> Result<Notice, KernelError> {
    let row = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM platform.notice WHERE id = $3 AND {ALL_IN_SCOPE}"
    ))
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(institution_of(&ctx.scope))
    .bind(id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound { entity: "notice" })?;
    from_row(&row).map_err(map_db_error)
}

/// Notices: the caller's live notices (pinned first), or every notice (`view=manage`).
#[utoipa::path(
    get,
    path = "/api/v1/notices",
    operation_id = "list_notices",
    tag = "notices",
    params(NoticeQuery),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = NoticeList),
        (status = 401, body = Problem, content_type = "application/problem+json"),
        (status = 403, description = "`view=manage` without `notice.manage`", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn list(
    State(st): State<InboxState>,
    auth: Authenticated,
    Query(q): Query<NoticeQuery>,
) -> ApiResult<Json<NoticeList>> {
    let ctx = auth.require(Permission::NoticeRead)?;
    let manage = q.view.unwrap_or_default() == NoticeView::Manage;
    if manage {
        auth.require(Permission::NoticeManage)?;
    }
    let limit = page_limit(q.limit, 50);
    let offset = i64::from(q.offset.unwrap_or(0));
    let (filter, order) = if manage {
        (ALL_IN_SCOPE, "created_at DESC, id DESC")
    } else {
        (LIVE_FOR_READER, "pinned DESC, publish_at DESC, id DESC")
    };
    let tenant = ctx.scope.tenant_id.as_uuid();
    let inst = institution_of(&ctx.scope);
    let roles = role_names(&ctx.roles);
    let sql = format!(
        "SELECT {COLUMNS}, count(*) OVER () AS total FROM platform.notice WHERE {filter} \
         ORDER BY {order} LIMIT {limit} OFFSET {offset}"
    );
    let mut query = sqlx::query(&sql).bind(tenant).bind(inst);
    if !manage {
        query = query.bind(&roles);
    }
    let rows = query.fetch_all(&st.pool).await.map_err(map_db_error)?;
    let total = match rows.first() {
        Some(r) => r.try_get("total").map_err(map_db_error)?,
        None => 0,
    };
    let items = rows
        .iter()
        .map(from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)?;
    Ok(Json(NoticeList { items, total }))
}

/// One notice. Readers only see live notices addressed to one of their roles (otherwise 404).
#[utoipa::path(
    get,
    path = "/api/v1/notices/{id}",
    operation_id = "get_notice",
    tag = "notices",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = Notice, headers(("ETag" = String))),
        (status = 401, body = Problem, content_type = "application/problem+json"),
        (status = 404, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn get(
    State(st): State<InboxState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::NoticeRead)?;
    let n = if ctx.has_permission(Permission::NoticeManage) {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_for_manager(&mut conn, ctx, id).await?
    } else {
        let row = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM platform.notice WHERE id = $4 AND {LIVE_FOR_READER}"
        ))
        .bind(ctx.scope.tenant_id.as_uuid())
        .bind(institution_of(&ctx.scope))
        .bind(role_names(&ctx.roles))
        .bind(id)
        .fetch_optional(&st.pool)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound { entity: "notice" })?;
        from_row(&row).map_err(map_db_error)?
    };
    Ok(with_etag(StatusCode::OK, n.version, n))
}

/// Publish a notice (`notice.manage`; `Idempotency-Key` required).
#[utoipa::path(
    post,
    path = "/api/v1/notices",
    operation_id = "create_notice",
    tag = "notices",
    params(("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = NewNotice,
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 201, body = Notice, headers(("ETag" = String))),
        (status = 401, body = Problem, content_type = "application/problem+json"),
        (status = 403, description = "Missing `notice.manage`", body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn create(
    State(st): State<InboxState>,
    auth: Authenticated,
    headers: HeaderMap,
    Json(cmd): Json<NewNotice>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::NoticeManage)?;
    require_idempotency_key(&headers)?;
    cmd.validate(Utc::now())?;
    let roles: Vec<&str> = {
        let mut r: Vec<&str> = cmd.audience_roles.iter().map(Role::as_str).collect();
        r.sort_unstable();
        r.dedup();
        r
    };
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let row = sqlx::query(&format!(
        "INSERT INTO platform.notice \
           (tenant_id, institution_id, title, body, audience_roles, pinned, publish_at, \
            expires_at, author_subject, author_name) \
         VALUES ($1, $2, $3, $4, $5, $6, COALESCE($7, now()), $8, $9, $10) RETURNING {COLUMNS}"
    ))
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(institution_of(&ctx.scope))
    .bind(cmd.title.trim())
    .bind(cmd.body.trim())
    .bind(&roles)
    .bind(cmd.pinned)
    .bind(cmd.publish_at)
    .bind(cmd.expires_at)
    .bind(&ctx.subject)
    .bind(&ctx.display_name)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let n = from_row(&row).map_err(map_db_error)?;
    emit(
        &mut tx,
        ctx,
        &n,
        (NOTICE_PUBLISHED, NOTICE_PUBLISHED_SCHEMA),
        "platform.notice.publish",
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::CREATED, n.version, n))
}

/// Update a notice (`notice.manage`, `If-Match` required). Archived notices cannot change.
#[utoipa::path(
    patch,
    path = "/api/v1/notices/{id}",
    operation_id = "update_notice",
    tag = "notices",
    params(
        ("id" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the version being modified"),
    ),
    request_body = UpdateNotice,
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 200, body = Notice, headers(("ETag" = String))),
        (status = 403, body = Problem, content_type = "application/problem+json"),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, description = "Archived", body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
        (status = 428, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn update(
    State(st): State<InboxState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<UpdateNotice>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::NoticeManage)?;
    let Some(expected) = if_match(&headers)? else {
        return Ok(precondition_required());
    };
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let current = load_for_manager(&mut tx, ctx, id).await?;
    if current.state == NoticeState::Archived {
        return Err(KernelError::Conflict("an archived notice cannot be changed".into()).into());
    }
    if current.version != expected {
        return Err(KernelError::VersionConflict {
            expected,
            actual: current.version,
        }
        .into());
    }
    let mut v = vec![];
    if let Some(t) = &cmd.title {
        check_text("title", t, 1, 200, false, &mut v);
    }
    if let Some(b) = &cmd.body {
        check_text("body", b, 1, 10_000, true, &mut v);
    }
    if let Some(r) = &cmd.audience_roles {
        check_audience("audienceRoles", r, &mut v);
    }
    let publish_at = cmd.publish_at.unwrap_or(current.publish_at);
    let expires_at = if cmd.clear_expiry {
        None
    } else {
        cmd.expires_at.or(current.expires_at)
    };
    check_window(publish_at, expires_at, &mut v);
    violations(v)?;
    let roles: Option<Vec<&str>> = cmd.audience_roles.as_ref().map(|r| {
        let mut r: Vec<&str> = r.iter().map(Role::as_str).collect();
        r.sort_unstable();
        r.dedup();
        r
    });
    let row = sqlx::query(&format!(
        "UPDATE platform.notice SET \
           title = COALESCE($3, title), body = COALESCE($4, body), \
           audience_roles = COALESCE($5, audience_roles), pinned = COALESCE($6, pinned), \
           publish_at = $7, expires_at = $8, version = version + 1, updated_at = now() \
         WHERE id = $1 AND version = $2 RETURNING {COLUMNS}"
    ))
    .bind(id)
    .bind(expected)
    .bind(cmd.title.as_deref().map(str::trim))
    .bind(cmd.body.as_deref().map(str::trim))
    .bind(roles)
    .bind(cmd.pinned)
    .bind(publish_at)
    .bind(expires_at)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::VersionConflict {
        expected,
        actual: current.version + 1,
    })?;
    let n = from_row(&row).map_err(map_db_error)?;
    emit(
        &mut tx,
        ctx,
        &n,
        (NOTICE_UPDATED, NOTICE_UPDATED_SCHEMA),
        "platform.notice.update",
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, n.version, n))
}

/// Archive a notice (`notice.manage`; idempotent). It disappears for readers immediately.
#[utoipa::path(
    post,
    path = "/api/v1/notices/{id}/archive",
    operation_id = "archive_notice",
    tag = "notices",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 200, body = Notice, headers(("ETag" = String))),
        (status = 403, body = Problem, content_type = "application/problem+json"),
        (status = 404, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn archive(
    State(st): State<InboxState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::NoticeManage)?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let current = load_for_manager(&mut tx, ctx, id).await?;
    if current.state == NoticeState::Archived {
        return Ok(with_etag(StatusCode::OK, current.version, current));
    }
    let row = sqlx::query(&format!(
        "UPDATE platform.notice SET status = 'archived', archived_at = now(), \
           version = version + 1, updated_at = now() \
         WHERE id = $1 AND status = 'active' RETURNING {COLUMNS}"
    ))
    .bind(id)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let n = from_row(&row).map_err(map_db_error)?;
    emit(
        &mut tx,
        ctx,
        &n,
        (NOTICE_ARCHIVED, NOTICE_ARCHIVED_SCHEMA),
        "platform.notice.archive",
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, n.version, n))
}

pub fn routes() -> OpenApiRouter<InboxState> {
    OpenApiRouter::new()
        .routes(routes!(list, create))
        .routes(routes!(get, update))
        .routes(routes!(archive))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn notice(roles: Vec<Role>, state: NoticeState) -> Notice {
        let now = Utc::now();
        Notice {
            id: Uuid::now_v7(),
            title: "t".into(),
            body: "b".into(),
            audience_roles: roles,
            pinned: false,
            publish_at: now,
            expires_at: None,
            state,
            author_name: "a".into(),
            version: 1,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn lifecycle_states() {
        let now = Utc::now();
        let h = Duration::hours(1);
        assert_eq!(state_at(false, now + h, None, now), NoticeState::Scheduled);
        assert_eq!(state_at(false, now - h, None, now), NoticeState::Live);
        assert_eq!(
            state_at(false, now - h, Some(now + h), now),
            NoticeState::Live
        );
        assert_eq!(
            state_at(false, now - h - h, Some(now - h), now),
            NoticeState::Expired
        );
        assert_eq!(state_at(true, now - h, None, now), NoticeState::Archived);
    }

    #[test]
    fn audience_filtering() {
        let students = notice(vec![Role::Student], NoticeState::Live);
        assert!(visible_to(&students, &[Role::Student]));
        assert!(!visible_to(&students, &[Role::Faculty]));
        assert!(!visible_to(&students, &[]));
        let staff = notice(vec![Role::Faculty, Role::Administrator], NoticeState::Live);
        assert!(visible_to(&staff, &[Role::Administrator]));
        for s in [
            NoticeState::Scheduled,
            NoticeState::Expired,
            NoticeState::Archived,
        ] {
            assert!(
                !visible_to(&notice(vec![Role::Student], s), &[Role::Student]),
                "{s:?}"
            );
        }
    }

    #[test]
    fn validation() {
        let now = Utc::now();
        let ok = NewNotice {
            title: "Exams".into(),
            body: "Line 1\nLine 2".into(),
            audience_roles: vec![Role::Student],
            pinned: true,
            publish_at: None,
            expires_at: Some(now + Duration::days(1)),
        };
        assert!(ok.validate(now).is_ok());
        let bad = NewNotice {
            title: String::new(),
            audience_roles: vec![],
            publish_at: Some(now + Duration::days(2)),
            ..ok
        };
        let Err(KernelError::Validation(v)) = bad.validate(now) else {
            panic!("expected violations")
        };
        let fields: Vec<_> = v.iter().map(|x| x.field.as_str()).collect();
        assert_eq!(fields, ["title", "audienceRoles", "expiresAt"]);
    }
}
