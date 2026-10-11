//! Self-service profile (`platform.user_profile`): display name, locale, time zone, contact
//! details and notification preferences. The row is created from the authenticated principal at
//! sign-in ([`ProfileSignInHook`]) or on first read; identity facts (login id, roles) follow
//! the identity provider, everything else belongs to the user.
//!
//! | Endpoint | Permission |
//! |----------|------------|
//! | `GET   /api/v1/profile` | `profile.manage` |
//! | `PATCH /api/v1/profile` | `profile.manage` + CSRF + `If-Match` |

use super::notifications::NotificationCategory;
use super::{
    actor, check_text, if_match, precondition_required, violations, with_etag, InboxState,
};
use crate::auth::http::SignInHook;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use std::collections::HashMap;
use sutra_contracts::platform::auth::Principal;
use sutra_kernel::audit::{ActorKind, AuditEntry, AuditOutcome};
use sutra_kernel::auth::{Permission, Role};
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::ExecutionScope;
use sutra_persistence::{audit, map_db_error};
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa_axum::{router::OpenApiRouter, routes};

/// Per-category notification preference.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "camelCase")]
pub struct CategoryPreference {
    pub category: NotificationCategory,
    /// Show in the in-app inbox (security notifications cannot be muted).
    pub in_app: bool,
    /// Also send by e-mail to `contactEmail`.
    pub email: bool,
}

#[derive(Clone, Copy, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredPref {
    #[serde(default)]
    in_app: Option<bool>,
    #[serde(default)]
    email: Option<bool>,
}

fn effective_prefs(raw: &serde_json::Value) -> Vec<CategoryPreference> {
    let stored: HashMap<String, StoredPref> =
        serde_json::from_value(raw.clone()).unwrap_or_default();
    NotificationCategory::ALL
        .into_iter()
        .map(|c| {
            let s = stored.get(c.as_str()).copied().unwrap_or_default();
            CategoryPreference {
                category: c,
                in_app: !c.mutable() || s.in_app.unwrap_or(true),
                email: s.email.unwrap_or(c.email_by_default()),
            }
        })
        .collect()
}

fn store_prefs(prefs: &[CategoryPreference]) -> serde_json::Value {
    let map: HashMap<&str, StoredPref> = prefs
        .iter()
        .map(|p| {
            (
                p.category.as_str(),
                StoredPref {
                    in_app: Some(p.in_app || !p.category.mutable()),
                    email: Some(p.email),
                },
            )
        })
        .collect();
    serde_json::to_value(map).unwrap_or_default()
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    pub subject: String,
    pub login_id: String,
    pub display_name: String,
    pub roles: Vec<Role>,
    /// BCP 47 language tag, e.g. `en-IN`.
    pub locale: String,
    /// IANA time zone, e.g. `Asia/Kolkata`.
    pub time_zone: String,
    pub contact_email: Option<String>,
    /// E.164, e.g. `+919876543210`.
    pub contact_phone: Option<String>,
    pub notification_preferences: Vec<CategoryPreference>,
    pub version: i64,
    pub updated_at: DateTime<Utc>,
    pub last_sign_in_at: Option<DateTime<Utc>>,
}

/// Partial update; omitted members are unchanged. An empty `contactEmail` / `contactPhone`
/// clears it.
#[derive(Debug, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateProfile {
    pub display_name: Option<String>,
    pub locale: Option<String>,
    pub time_zone: Option<String>,
    pub contact_email: Option<String>,
    pub contact_phone: Option<String>,
    pub notification_preferences: Option<Vec<CategoryPreference>>,
}

pub fn valid_locale(s: &str) -> bool {
    let mut parts = s.split('-');
    let lang = parts.next().unwrap_or_default();
    let region = parts.next();
    parts.next().is_none()
        && (2..=3).contains(&lang.len())
        && lang.bytes().all(|b| b.is_ascii_lowercase())
        && region.is_none_or(|r| r.len() == 2 && r.bytes().all(|b| b.is_ascii_uppercase()))
}

pub fn valid_time_zone(s: &str) -> bool {
    s == "UTC"
        || (s.len() <= 64
            && s.contains('/')
            && !s.starts_with('/')
            && !s.ends_with('/')
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'_' | b'-' | b'+')))
}

pub fn valid_email(s: &str) -> bool {
    let Some((local, domain)) = s.split_once('@') else {
        return false;
    };
    s.len() <= 254
        && !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !domain.contains('@')
        && !s.chars().any(|c| c.is_whitespace() || c.is_control())
}

pub fn valid_phone(s: &str) -> bool {
    let digits = s.strip_prefix('+').unwrap_or_default();
    (7..=15).contains(&digits.len())
        && digits.bytes().all(|b| b.is_ascii_digit())
        && !digits.starts_with('0')
}

impl UpdateProfile {
    pub fn validate(&self) -> Result<(), KernelError> {
        let mut v = vec![];
        if let Some(n) = &self.display_name {
            check_text("displayName", n, 1, 120, false, &mut v);
        }
        if let Some(l) = &self.locale {
            if !valid_locale(l.trim()) {
                v.push(FieldViolation::new(
                    "locale",
                    "format",
                    "locale must be a language tag such as en-IN",
                ));
            }
        }
        if let Some(t) = &self.time_zone {
            if !valid_time_zone(t.trim()) {
                v.push(FieldViolation::new(
                    "timeZone",
                    "format",
                    "timeZone must be an IANA zone such as Asia/Kolkata",
                ));
            }
        }
        if let Some(e) = self.contact_email.as_deref().map(str::trim) {
            if !e.is_empty() && !valid_email(e) {
                v.push(FieldViolation::new(
                    "contactEmail",
                    "format",
                    "contactEmail must be an e-mail address",
                ));
            }
        }
        if let Some(p) = self.contact_phone.as_deref().map(str::trim) {
            if !p.is_empty() && !valid_phone(p) {
                v.push(FieldViolation::new(
                    "contactPhone",
                    "format",
                    "contactPhone must be in international format such as +919876543210",
                ));
            }
        }
        if let Some(prefs) = &self.notification_preferences {
            let mut seen = std::collections::HashSet::new();
            for p in prefs {
                if !seen.insert(p.category) {
                    v.push(FieldViolation::new(
                        "notificationPreferences",
                        "duplicate",
                        format!("{} is listed twice", p.category.as_str()),
                    ));
                }
                if !p.in_app && !p.category.mutable() {
                    v.push(FieldViolation::new(
                        "notificationPreferences",
                        "not-mutable",
                        "security notifications cannot be muted",
                    ));
                }
            }
        }
        violations(v)
    }
}

const COLUMNS: &str = "subject, login_id, display_name, roles, locale, time_zone, contact_email, \
    contact_phone, notification_prefs, version, updated_at, last_sign_in_at";

fn from_row(r: &sqlx::postgres::PgRow) -> Result<Profile, sqlx::Error> {
    let roles: Vec<String> = r.try_get("roles")?;
    let prefs: serde_json::Value = r.try_get("notification_prefs")?;
    Ok(Profile {
        subject: r.try_get("subject")?,
        login_id: r.try_get("login_id")?,
        display_name: r.try_get("display_name")?,
        roles: roles.iter().filter_map(|x| Role::parse(x)).collect(),
        locale: r.try_get("locale")?,
        time_zone: r.try_get("time_zone")?,
        contact_email: r.try_get("contact_email")?,
        contact_phone: r.try_get("contact_phone")?,
        notification_preferences: effective_prefs(&prefs),
        version: r.try_get("version")?,
        updated_at: r.try_get("updated_at")?,
        last_sign_in_at: r.try_get("last_sign_in_at")?,
    })
}

fn initial_name(p: &Principal) -> String {
    let n: String = p.display_name.trim().chars().take(120).collect();
    if n.is_empty() {
        p.login_id.chars().take(120).collect()
    } else {
        n
    }
}

/// Creates the profile from the principal, or refreshes its identity facts (login id, roles).
/// `signed_in` also stamps `last_sign_in_at`.
pub async fn ensure(pool: &PgPool, p: &Principal, signed_in: bool) -> Result<Profile, KernelError> {
    let roles: Vec<&str> = p.roles.iter().map(Role::as_str).collect();
    let row = sqlx::query(&format!(
        "INSERT INTO platform.user_profile \
           (tenant_id, subject, login_id, display_name, roles, last_sign_in_at) \
         VALUES ($1, $2, $3, $4, $5, CASE WHEN $6 THEN now() END) \
         ON CONFLICT (tenant_id, subject) DO UPDATE SET \
           login_id = EXCLUDED.login_id, roles = EXCLUDED.roles, \
           last_sign_in_at = CASE WHEN $6 THEN now() ELSE platform.user_profile.last_sign_in_at END \
         RETURNING {COLUMNS}"
    ))
    .bind(p.scope.tenant_id.as_uuid())
    .bind(&p.subject)
    .bind(&p.login_id)
    .bind(initial_name(p))
    .bind(&roles)
    .bind(signed_in)
    .fetch_one(pool)
    .await
    .map_err(map_db_error)?;
    from_row(&row).map_err(map_db_error)
}

/// Categories the subject muted in the app (missing profile → none).
pub async fn muted_categories(
    pool: &PgPool,
    scope: &ExecutionScope,
    subject: &str,
) -> Result<Vec<String>, KernelError> {
    let raw: Option<serde_json::Value> = sqlx::query_scalar(
        "SELECT notification_prefs FROM platform.user_profile WHERE tenant_id = $1 AND subject = $2",
    )
    .bind(scope.tenant_id.as_uuid())
    .bind(subject)
    .fetch_optional(pool)
    .await
    .map_err(map_db_error)?;
    Ok(raw
        .map(|r| {
            effective_prefs(&r)
                .into_iter()
                .filter(|p| !p.in_app)
                .map(|p| p.category.as_str().to_owned())
                .collect()
        })
        .unwrap_or_default())
}

/// Resolves a login id to the subject of a profile in the tenant (people who signed in once).
pub async fn subject_for_login(
    pool: &PgPool,
    scope: &ExecutionScope,
    login_id: &str,
) -> Result<Option<(String, String)>, KernelError> {
    let row = sqlx::query(
        "SELECT subject, display_name FROM platform.user_profile \
         WHERE tenant_id = $1 AND lower(login_id) = lower($2) ORDER BY updated_at DESC LIMIT 1",
    )
    .bind(scope.tenant_id.as_uuid())
    .bind(login_id.trim())
    .fetch_optional(pool)
    .await
    .map_err(map_db_error)?;
    row.map(|r| {
        Ok((
            r.try_get("subject").map_err(map_db_error)?,
            r.try_get("display_name").map_err(map_db_error)?,
        ))
    })
    .transpose()
}

/// Profile refresh at every sign-in.
pub struct ProfileSignInHook {
    pub pool: PgPool,
}

#[async_trait::async_trait]
impl SignInHook for ProfileSignInHook {
    async fn signed_in(&self, principal: &Principal) -> Result<(), KernelError> {
        ensure(&self.pool, principal, true).await.map(|_| ())
    }
}

/// The caller's profile (created from the session's principal on first access).
#[utoipa::path(
    get,
    path = "/api/v1/profile",
    operation_id = "get_profile",
    tag = "profile",
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = Profile, headers(("ETag" = String))),
        (status = 401, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn get(State(st): State<InboxState>, auth: Authenticated) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ProfileManage)?;
    let p = ensure(&st.pool, &Principal::from(ctx), false).await?;
    Ok(with_etag(StatusCode::OK, p.version, p))
}

/// Update the caller's profile (optimistic concurrency: `If-Match: "<version>"`).
#[utoipa::path(
    patch,
    path = "/api/v1/profile",
    operation_id = "update_profile",
    tag = "profile",
    params(("If-Match" = String, Header, description = "ETag of the profile version being modified")),
    request_body = UpdateProfile,
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 200, body = Profile, headers(("ETag" = String))),
        (status = 401, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
        (status = 428, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn update(
    State(st): State<InboxState>,
    auth: Authenticated,
    headers: HeaderMap,
    Json(cmd): Json<UpdateProfile>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ProfileManage)?;
    let Some(expected) = if_match(&headers)? else {
        return Ok(precondition_required());
    };
    cmd.validate()?;
    let principal = Principal::from(ctx);
    let current = ensure(&st.pool, &principal, false).await?;
    let prefs = cmd.notification_preferences.as_ref().map(|new| {
        // Merge: categories not mentioned keep their current preference.
        let mut merged = current.notification_preferences.clone();
        for p in new {
            if let Some(m) = merged.iter_mut().find(|m| m.category == p.category) {
                *m = *p;
            }
        }
        store_prefs(&merged)
    });
    let opt = |v: &Option<String>| v.as_deref().map(|s| s.trim().to_owned());
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let row = sqlx::query(&format!(
        "UPDATE platform.user_profile SET \
           display_name = COALESCE($4, display_name), \
           locale = COALESCE($5, locale), \
           time_zone = COALESCE($6, time_zone), \
           contact_email = CASE WHEN $7::text IS NULL THEN contact_email ELSE NULLIF($7, '') END, \
           contact_phone = CASE WHEN $8::text IS NULL THEN contact_phone ELSE NULLIF($8, '') END, \
           notification_prefs = COALESCE($9, notification_prefs), \
           version = version + 1, updated_at = now() \
         WHERE tenant_id = $1 AND subject = $2 AND version = $3 RETURNING {COLUMNS}"
    ))
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(&ctx.subject)
    .bind(expected)
    .bind(opt(&cmd.display_name))
    .bind(opt(&cmd.locale))
    .bind(opt(&cmd.time_zone))
    .bind(opt(&cmd.contact_email))
    .bind(opt(&cmd.contact_phone))
    .bind(prefs)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let Some(row) = row else {
        return Err(KernelError::VersionConflict {
            expected,
            actual: current.version,
        }
        .into());
    };
    let p = from_row(&row).map_err(map_db_error)?;
    let (actor_id, correlation) = actor(ctx);
    let changed: Vec<&str> = [
        ("displayName", cmd.display_name.is_some()),
        ("locale", cmd.locale.is_some()),
        ("timeZone", cmd.time_zone.is_some()),
        ("contactEmail", cmd.contact_email.is_some()),
        ("contactPhone", cmd.contact_phone.is_some()),
        (
            "notificationPreferences",
            cmd.notification_preferences.is_some(),
        ),
    ]
    .into_iter()
    .filter_map(|(k, on)| on.then_some(k))
    .collect();
    audit::insert(
        &mut *tx,
        &AuditEntry::new(
            ctx.scope,
            "platform.profile.update",
            AuditOutcome::Succeeded,
        )
        .actor(ActorKind::User, Some(actor_id))
        .target("profile", ctx.subject.clone())
        .correlation(correlation)
        .details(serde_json::json!({ "fields": changed, "version": p.version })),
    )
    .await
    .map_err(map_db_error)?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, p.version, p))
}

pub fn routes() -> OpenApiRouter<InboxState> {
    OpenApiRouter::new().routes(routes!(get, update))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_formats() {
        assert!(valid_locale("en-IN") && valid_locale("hi") && valid_locale("mar"));
        assert!(!valid_locale("EN") && !valid_locale("en-in") && !valid_locale("en-IN-x"));
        assert!(valid_time_zone("Asia/Kolkata") && valid_time_zone("UTC"));
        assert!(valid_time_zone("America/Argentina/Buenos_Aires"));
        assert!(!valid_time_zone("IST") && !valid_time_zone("/x") && !valid_time_zone("a/b c"));
        assert!(valid_email("a.b@sutra.dev"));
        for bad in ["a@b", "@b.c", "a b@c.d", "a@b@c.d", "a@.b", "plain"] {
            assert!(!valid_email(bad), "{bad}");
        }
        assert!(valid_phone("+919876543210"));
        assert!(!valid_phone("9876543210") && !valid_phone("+0123456") && !valid_phone("+91-98"));
    }

    #[test]
    fn preferences_default_merge_and_security_is_not_mutable() {
        let d = effective_prefs(&serde_json::json!({}));
        assert_eq!(d.len(), NotificationCategory::ALL.len());
        assert!(d.iter().all(|p| p.in_app));
        assert!(d
            .iter()
            .find(|p| p.category == NotificationCategory::Security)
            .is_some_and(|p| p.email));
        let stored = store_prefs(&[CategoryPreference {
            category: NotificationCategory::Security,
            in_app: false,
            email: false,
        }]);
        let back = effective_prefs(&stored);
        let sec = back
            .iter()
            .find(|p| p.category == NotificationCategory::Security);
        assert!(sec.is_some_and(|p| p.in_app && !p.email));
        let bad = UpdateProfile {
            notification_preferences: Some(vec![CategoryPreference {
                category: NotificationCategory::Security,
                in_app: false,
                email: true,
            }]),
            locale: Some("english".into()),
            contact_email: Some(String::new()),
            ..Default::default()
        };
        let Err(KernelError::Validation(v)) = bad.validate() else {
            panic!("expected violations")
        };
        let fields: Vec<_> = v.iter().map(|x| x.field.as_str()).collect();
        assert_eq!(fields, ["locale", "notificationPreferences"]);
    }
}
