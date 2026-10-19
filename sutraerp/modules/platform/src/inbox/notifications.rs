//! Notifications: tenant-scoped, addressed to one subject or to every holder of a role, with
//! per-reader read receipts, muting by category (profile preferences) and an e-mail channel.
//!
//! | Endpoint | Permission |
//! |----------|------------|
//! | `GET  /api/v1/notifications?unreadOnly&limit&cursor` | `notification.read` |
//! | `GET  /api/v1/notifications/unread-count` | `notification.read` |
//! | `POST /api/v1/notifications/{id}/read` | `notification.read` (+ CSRF) |
//! | `POST /api/v1/notifications/read-all` | `notification.read` (+ CSRF) |
//!
//! Notifications are created by the in-process event consumer ([`super::consumer`]) through
//! [`create`], which also queues e-mail deliveries for recipients whose profile opted in.

use super::{institution_of, page_limit, profile, role_names, InboxState};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, Utc};
use sqlx::{PgConnection, Row};
use sutra_kernel::auth::{Permission, Role};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::TenantId;
use sutra_persistence::map_db_error;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// What a notification is about (also the unit of notification preferences).
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum NotificationCategory {
    Institution,
    Notice,
    Todo,
    Security,
    System,
}

impl NotificationCategory {
    pub const ALL: [NotificationCategory; 5] = [
        Self::Institution,
        Self::Notice,
        Self::Todo,
        Self::Security,
        Self::System,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Institution => "institution",
            Self::Notice => "notice",
            Self::Todo => "todo",
            Self::Security => "security",
            Self::System => "system",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == s)
    }

    /// Default e-mail opt-in: only security notifications are e-mailed unless chosen.
    pub fn email_by_default(&self) -> bool {
        matches!(self, Self::Security)
    }

    /// Security notifications cannot be muted in the app.
    pub fn mutable(&self) -> bool {
        !matches!(self, Self::Security)
    }
}

#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    utoipa::ToSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum Severity {
    #[default]
    Info,
    Success,
    Warning,
    Critical,
}

impl Severity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Success => "success",
            Self::Warning => "warning",
            Self::Critical => "critical",
        }
    }
    fn parse(s: &str) -> Self {
        match s {
            "success" => Self::Success,
            "warning" => Self::Warning,
            "critical" => Self::Critical,
            _ => Self::Info,
        }
    }
}

/// Addressee of a notification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Recipient {
    Subject(String),
    Role(Role),
}

/// A notification to create.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewNotification {
    pub tenant: TenantId,
    pub institution: Option<Uuid>,
    pub recipient: Recipient,
    pub category: NotificationCategory,
    pub severity: Severity,
    pub title: String,
    pub body: String,
    pub link: Option<String>,
    pub source_event_id: Option<Uuid>,
    /// Hidden until this instant (defaults to now).
    pub visible_at: Option<DateTime<Utc>>,
}

fn clip(s: &str, max: usize) -> String {
    let t = s.trim();
    if t.chars().count() <= max {
        t.to_owned()
    } else {
        let mut out: String = t.chars().take(max - 1).collect();
        out.push('…');
        out
    }
}

/// Inserts a notification (idempotent per `source_event_id` + recipient) and, when `email` is
/// on, queues e-mail deliveries for recipients whose profile has a contact address and has the
/// category's e-mail preference on. Returns the new id, or `None` when it already existed.
pub async fn create(
    conn: &mut PgConnection,
    n: &NewNotification,
    email: bool,
) -> Result<Option<Uuid>, sqlx::Error> {
    let (subject, role) = match &n.recipient {
        Recipient::Subject(s) => (Some(s.as_str()), None),
        Recipient::Role(r) => (None, Some(r.as_str())),
    };
    let id: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO platform.notification \
           (tenant_id, institution_id, recipient_subject, recipient_role, category, severity, \
            title, body, link, source_event_id, visible_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, COALESCE($11, now())) \
         ON CONFLICT (source_event_id, recipient_subject, recipient_role) \
           WHERE source_event_id IS NOT NULL DO NOTHING \
         RETURNING id",
    )
    .bind(n.tenant.as_uuid())
    .bind(n.institution)
    .bind(subject)
    .bind(role)
    .bind(n.category.as_str())
    .bind(n.severity.as_str())
    .bind(clip(&n.title, 200))
    .bind(clip(&n.body, 2000))
    .bind(&n.link)
    .bind(n.source_event_id)
    .bind(n.visible_at)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(id) = id else {
        return Ok(None);
    };
    if email {
        sqlx::query(
            "INSERT INTO platform.notification_delivery \
               (notification_id, tenant_id, channel, recipient_subject, address, next_attempt_at) \
             SELECT $1, p.tenant_id, 'email', p.subject, p.contact_email, COALESCE($7, now()) \
             FROM platform.user_profile p \
             WHERE p.tenant_id = $2 AND p.contact_email IS NOT NULL \
               AND (($3::text IS NOT NULL AND p.subject = $3) \
                 OR ($4::text IS NOT NULL AND $4 = ANY(p.roles))) \
               AND COALESCE((p.notification_prefs -> $5 ->> 'email')::boolean, $6) \
             ON CONFLICT DO NOTHING",
        )
        .bind(id)
        .bind(n.tenant.as_uuid())
        .bind(subject)
        .bind(role)
        .bind(n.category.as_str())
        .bind(n.category.email_by_default())
        .bind(n.visible_at)
        .execute(&mut *conn)
        .await?;
    }
    Ok(Some(id))
}

// ---------------------------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------------------------

#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Notification {
    pub id: Uuid,
    pub category: NotificationCategory,
    pub severity: Severity,
    pub title: String,
    pub body: String,
    /// Application path to open (e.g. `/todos`).
    pub link: Option<String>,
    pub created_at: DateTime<Utc>,
    /// When the caller read it; `null` = unread.
    pub read_at: Option<DateTime<Utc>>,
}

#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NotificationPage {
    pub items: Vec<Notification>,
    /// Pass as `cursor` to fetch the next (older) page; absent on the last page.
    pub next_cursor: Option<Uuid>,
    pub unread_count: i64,
}

#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UnreadCount {
    pub unread: i64,
}

#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MarkedCount {
    pub marked: u64,
}

#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct NotificationQuery {
    /// Only unread notifications.
    pub unread_only: Option<bool>,
    /// Page size (1–100, default 20).
    pub limit: Option<u32>,
    /// `nextCursor` of the previous page.
    pub cursor: Option<Uuid>,
}

/// Visibility predicate shared by every query: `$1` tenant, `$2` subject, `$3` role names,
/// `$4` session institution, `$5` muted categories.
const VISIBLE: &str = "n.tenant_id = $1 \
    AND (n.recipient_subject = $2 OR n.recipient_role = ANY($3)) \
    AND (n.institution_id IS NULL OR $4::uuid IS NULL OR n.institution_id = $4) \
    AND NOT (n.category = ANY($5)) \
    AND n.visible_at <= now()";

struct Reader {
    tenant: Uuid,
    subject: String,
    roles: Vec<String>,
    institution: Option<Uuid>,
    muted: Vec<String>,
}

async fn reader(
    st: &InboxState,
    ctx: &sutra_kernel::auth::AuthContext,
) -> Result<Reader, KernelError> {
    Ok(Reader {
        tenant: ctx.scope.tenant_id.as_uuid(),
        subject: ctx.subject.clone(),
        roles: role_names(&ctx.roles),
        institution: institution_of(&ctx.scope),
        muted: profile::muted_categories(&st.pool, &ctx.scope, &ctx.subject).await?,
    })
}

async fn unread(st: &InboxState, r: &Reader) -> Result<i64, KernelError> {
    sqlx::query_scalar(&format!(
        "SELECT count(*) FROM platform.notification n \
         WHERE {VISIBLE} AND NOT EXISTS (SELECT 1 FROM platform.notification_read x \
           WHERE x.notification_id = n.id AND x.subject = $2)"
    ))
    .bind(r.tenant)
    .bind(&r.subject)
    .bind(&r.roles)
    .bind(r.institution)
    .bind(&r.muted)
    .fetch_one(&st.pool)
    .await
    .map_err(map_db_error)
}

/// The caller's notifications, newest first (cursor pagination).
#[utoipa::path(
    get,
    path = "/api/v1/notifications",
    operation_id = "list_notifications",
    tag = "notifications",
    params(NotificationQuery),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = NotificationPage),
        (status = 401, body = Problem, content_type = "application/problem+json"),
        (status = 403, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn list(
    State(st): State<InboxState>,
    auth: Authenticated,
    Query(q): Query<NotificationQuery>,
) -> ApiResult<Json<NotificationPage>> {
    let ctx = auth.require(Permission::NotificationRead)?;
    let r = reader(&st, ctx).await?;
    let limit = page_limit(q.limit, 20);
    let rows = sqlx::query(&format!(
        "SELECT n.id, n.category, n.severity, n.title, n.body, n.link, n.created_at, x.read_at \
         FROM platform.notification n \
         LEFT JOIN platform.notification_read x ON x.notification_id = n.id AND x.subject = $2 \
         WHERE {VISIBLE} AND ($6 = false OR x.read_at IS NULL) \
           AND ($7::uuid IS NULL OR n.id < $7) \
         ORDER BY n.id DESC LIMIT $8"
    ))
    .bind(r.tenant)
    .bind(&r.subject)
    .bind(&r.roles)
    .bind(r.institution)
    .bind(&r.muted)
    .bind(q.unread_only.unwrap_or(false))
    .bind(q.cursor)
    .bind(limit + 1)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let mut items = rows
        .iter()
        .map(|row| {
            let category: String = row.try_get("category")?;
            let severity: String = row.try_get("severity")?;
            Ok(Notification {
                id: row.try_get("id")?,
                category: NotificationCategory::parse(&category)
                    .unwrap_or(NotificationCategory::System),
                severity: Severity::parse(&severity),
                title: row.try_get("title")?,
                body: row.try_get("body")?,
                link: row.try_get("link")?,
                created_at: row.try_get("created_at")?,
                read_at: row.try_get("read_at")?,
            })
        })
        .collect::<Result<Vec<_>, sqlx::Error>>()
        .map_err(map_db_error)?;
    let next_cursor = if items.len() as i64 > limit {
        items.truncate(limit as usize);
        items.last().map(|n| n.id)
    } else {
        None
    };
    Ok(Json(NotificationPage {
        items,
        next_cursor,
        unread_count: unread(&st, &r).await?,
    }))
}

/// Number of unread notifications (the bell badge).
#[utoipa::path(
    get,
    path = "/api/v1/notifications/unread-count",
    operation_id = "unread_notification_count",
    tag = "notifications",
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = UnreadCount),
        (status = 401, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn unread_count(
    State(st): State<InboxState>,
    auth: Authenticated,
) -> ApiResult<Json<UnreadCount>> {
    let ctx = auth.require(Permission::NotificationRead)?;
    let r = reader(&st, ctx).await?;
    Ok(Json(UnreadCount {
        unread: unread(&st, &r).await?,
    }))
}

/// Mark one notification read (idempotent). Notifications not addressed to the caller → 404.
#[utoipa::path(
    post,
    path = "/api/v1/notifications/{id}/read",
    operation_id = "mark_notification_read",
    tag = "notifications",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 204, description = "Marked read"),
        (status = 401, body = Problem, content_type = "application/problem+json"),
        (status = 404, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn mark_read(
    State(st): State<InboxState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::NotificationRead)?;
    let r = reader(&st, ctx).await?;
    let visible: bool = sqlx::query_scalar(&format!(
        "SELECT EXISTS (SELECT 1 FROM platform.notification n WHERE n.id = $6 AND {VISIBLE})"
    ))
    .bind(r.tenant)
    .bind(&r.subject)
    .bind(&r.roles)
    .bind(r.institution)
    .bind(&r.muted)
    .bind(id)
    .fetch_one(&st.pool)
    .await
    .map_err(map_db_error)?;
    if !visible {
        return Err(KernelError::NotFound {
            entity: "notification",
        }
        .into());
    }
    sqlx::query(
        "INSERT INTO platform.notification_read (notification_id, tenant_id, subject) \
         VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
    )
    .bind(id)
    .bind(r.tenant)
    .bind(&r.subject)
    .execute(&st.pool)
    .await
    .map_err(map_db_error)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Mark every visible notification read.
#[utoipa::path(
    post,
    path = "/api/v1/notifications/read-all",
    operation_id = "mark_all_notifications_read",
    tag = "notifications",
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 200, body = MarkedCount),
        (status = 401, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn mark_all_read(
    State(st): State<InboxState>,
    auth: Authenticated,
) -> ApiResult<Json<MarkedCount>> {
    let ctx = auth.require(Permission::NotificationRead)?;
    let r = reader(&st, ctx).await?;
    let marked = sqlx::query(&format!(
        "INSERT INTO platform.notification_read (notification_id, tenant_id, subject) \
         SELECT n.id, n.tenant_id, $2 FROM platform.notification n WHERE {VISIBLE} \
         ON CONFLICT DO NOTHING"
    ))
    .bind(r.tenant)
    .bind(&r.subject)
    .bind(&r.roles)
    .bind(r.institution)
    .bind(&r.muted)
    .execute(&st.pool)
    .await
    .map_err(map_db_error)?
    .rows_affected();
    Ok(Json(MarkedCount { marked }))
}

pub fn routes() -> OpenApiRouter<InboxState> {
    OpenApiRouter::new()
        .routes(routes!(list))
        .routes(routes!(unread_count))
        .routes(routes!(mark_read))
        .routes(routes!(mark_all_read))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categories_roundtrip_and_defaults() {
        for c in NotificationCategory::ALL {
            assert_eq!(NotificationCategory::parse(c.as_str()), Some(c));
        }
        assert!(NotificationCategory::Security.email_by_default());
        assert!(!NotificationCategory::Notice.email_by_default());
        assert!(!NotificationCategory::Security.mutable());
        assert_eq!(Severity::parse("bogus"), Severity::Info);
        assert_eq!(clip("  abc  ", 5), "abc");
        assert_eq!(clip("abcdef", 4).chars().count(), 4);
    }
}
