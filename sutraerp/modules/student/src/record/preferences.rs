//! Communication preferences (L3-C-015): staff (`student.manage`) and self-service
//! (`student.self`). Updating preferences is not message delivery.

use super::common::home_institution;
use super::support::{
    check_version, expect_version, finish, modified, record_history, CommandErrors, ReadErrors,
};
use crate::shared::guard::{require_institution_writable, require_student_writable};
use crate::shared::http::{actor, with_etag};
use crate::shared::scope::{institution_scope, tenant};
use crate::shared::student::{load_student, lock_student, me, StudentRow};
use crate::shared::{publish, StudentState};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use sqlx::{PgConnection, Row};
use sutra_contracts::student::events::record::{
    STUDENT_COMMUNICATION_PREFERENCES_UPDATED, STUDENT_COMMUNICATION_PREFERENCES_UPDATED_SCHEMA,
};
use sutra_contracts::student::ids::StudentId;
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_persistence::map_db_error;
use sutra_web::{ApiResult, Authenticated};

/// Preferred channel.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
#[schema(as = CommunicationChannel)]
pub enum Channel {
    Email,
    Sms,
    InApp,
    Whatsapp,
    Post,
}

impl Channel {
    fn as_str(self) -> &'static str {
        match self {
            Self::Email => "email",
            Self::Sms => "sms",
            Self::InApp => "in_app",
            Self::Whatsapp => "whatsapp",
            Self::Post => "post",
        }
    }
    fn parse(s: &str) -> Self {
        match s {
            "sms" => Self::Sms,
            "in_app" => Self::InApp,
            "whatsapp" => Self::Whatsapp,
            "post" => Self::Post,
            _ => Self::Email,
        }
    }
}

/// A student's communication preferences. A student without a stored row has the defaults at
/// version 1.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CommunicationPreferences {
    pub student_id: StudentId,
    pub preferred_channel: Channel,
    /// BCP-47-ish language code (`en`, `hi`, `mr-IN`, …).
    pub language: String,
    pub email_opt_in: bool,
    pub sms_opt_in: bool,
    pub whatsapp_opt_in: bool,
    pub marketing_opt_in: bool,
    #[schema(value_type = i64)]
    pub version: Version,
}

/// Body of `PUT …/communication-preferences` (full replacement).
#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schema(as = CommunicationPreferencesUpdate)]
pub struct PreferencesUpdate {
    pub preferred_channel: Channel,
    pub language: String,
    pub email_opt_in: bool,
    pub sms_opt_in: bool,
    pub whatsapp_opt_in: bool,
    pub marketing_opt_in: bool,
}

async fn load(
    conn: &mut PgConnection,
    s: &StudentRow,
    lock: bool,
) -> Result<CommunicationPreferences, KernelError> {
    let sql = format!(
        "SELECT preferred_channel, language, email_opt_in, sms_opt_in, whatsapp_opt_in, \
         marketing_opt_in, version FROM student.communication_preference \
         WHERE student_id = $1 AND tenant_id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    let row = sqlx::query(&sql)
        .bind(s.id.as_uuid())
        .bind(s.tenant_id.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?;
    Ok(match row {
        Some(r) => CommunicationPreferences {
            student_id: s.id,
            preferred_channel: Channel::parse(
                &r.try_get::<String, _>("preferred_channel")
                    .map_err(map_db_error)?,
            ),
            language: r.try_get("language").map_err(map_db_error)?,
            email_opt_in: r.try_get("email_opt_in").map_err(map_db_error)?,
            sms_opt_in: r.try_get("sms_opt_in").map_err(map_db_error)?,
            whatsapp_opt_in: r.try_get("whatsapp_opt_in").map_err(map_db_error)?,
            marketing_opt_in: r.try_get("marketing_opt_in").map_err(map_db_error)?,
            version: Version(r.try_get("version").map_err(map_db_error)?),
        },
        None => CommunicationPreferences {
            student_id: s.id,
            preferred_channel: Channel::Email,
            language: "en".into(),
            email_opt_in: s.primary_email.is_some(),
            sms_opt_in: false,
            whatsapp_opt_in: false,
            marketing_opt_in: false,
            version: Version(1),
        },
    })
}

/// Policy checks (L4-C-015-BH-002): the preferred channel must be opted in and reachable.
pub fn validate(s: &StudentRow, p: &PreferencesUpdate) -> Result<(), KernelError> {
    let mut v = vec![];
    let lang = p.language.trim();
    let lang_ok = {
        let (base, region) = lang.split_once('-').unwrap_or((lang, ""));
        (2..=3).contains(&base.len())
            && base.bytes().all(|b| b.is_ascii_lowercase())
            && (region.is_empty()
                || (region.len() == 2 && region.bytes().all(|b| b.is_ascii_uppercase())))
    };
    if !lang_ok {
        v.push(FieldViolation::new(
            "language",
            "format",
            "language must look like `en` or `mr-IN`".to_owned(),
        ));
    }
    let channel_ok = match p.preferred_channel {
        Channel::Email => p.email_opt_in && s.primary_email.is_some(),
        Channel::Sms => p.sms_opt_in && s.primary_phone.is_some(),
        Channel::Whatsapp => p.whatsapp_opt_in && s.primary_phone.is_some(),
        Channel::InApp => s.subject_ref.is_some(),
        Channel::Post => true,
    };
    if !channel_ok {
        v.push(FieldViolation::new(
            "preferredChannel",
            "unreachable",
            "the preferred channel must be opted in and have a contact (e-mail, phone or linked login)"
                .to_owned(),
        ));
    }
    if (p.sms_opt_in || p.whatsapp_opt_in) && s.primary_phone.is_none() {
        v.push(FieldViolation::new(
            "smsOptIn",
            "no-phone",
            "SMS / WhatsApp need a primary phone".to_owned(),
        ));
    }
    finish(v)
}

async fn put(
    st: &StudentState,
    ctx: &AuthContext,
    s: StudentRow,
    expected: Version,
    p: PreferencesUpdate,
) -> ApiResult<Response> {
    require_student_writable(&s)?;
    validate(&s, &p)?;
    let inst = home_institution(&st.pool, &s).await?;
    let scope = institution_scope(ctx, inst);
    require_institution_writable(st, &scope, inst).await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let s = lock_student(&mut tx, scope.tenant_id, s.id).await?;
    let cur = load(&mut tx, &s, true).await?;
    check_version(expected, cur.version.0)?;
    let version: i64 = sqlx::query_scalar(
        "INSERT INTO student.communication_preference (student_id, tenant_id, preferred_channel, \
         language, email_opt_in, sms_opt_in, whatsapp_opt_in, marketing_opt_in, version) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 2) \
         ON CONFLICT (student_id) DO UPDATE SET preferred_channel = $3, language = $4, \
           email_opt_in = $5, sms_opt_in = $6, whatsapp_opt_in = $7, marketing_opt_in = $8, \
           version = student.communication_preference.version + 1, updated_at = now() \
         RETURNING version",
    )
    .bind(s.id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(p.preferred_channel.as_str())
    .bind(p.language.trim())
    .bind(p.email_opt_in)
    .bind(p.sms_opt_in)
    .bind(p.whatsapp_opt_in)
    .bind(p.marketing_opt_in)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let next = CommunicationPreferences {
        student_id: s.id,
        preferred_channel: p.preferred_channel,
        language: p.language.trim().to_owned(),
        email_opt_in: p.email_opt_in,
        sms_opt_in: p.sms_opt_in,
        whatsapp_opt_in: p.whatsapp_opt_in,
        marketing_opt_in: p.marketing_opt_in,
        version: Version(version),
    };
    let before = serde_json::to_value(&cur).map_err(KernelError::internal)?;
    let after = serde_json::to_value(&next).map_err(KernelError::internal)?;
    let changed: Vec<String> = match (before.as_object(), after.as_object()) {
        (Some(b), Some(a)) => a
            .iter()
            .filter(|(k, v)| k.as_str() != "version" && b.get(*k) != Some(v))
            .map(|(k, _)| k.clone())
            .collect(),
        _ => vec![],
    };
    record_history(
        &mut tx,
        scope.tenant_id,
        s.id.as_uuid(),
        None,
        "communication",
        before,
        after,
        None,
        &a,
    )
    .await?;
    publish::event(
        &mut tx,
        &scope,
        STUDENT_COMMUNICATION_PREFERENCES_UPDATED,
        STUDENT_COMMUNICATION_PREFERENCES_UPDATED_SCHEMA,
        s.id.to_string(),
        &modified(
            s.id.as_uuid(),
            inst,
            Some(s.id.as_uuid()),
            version,
            changed.clone(),
            s.subject_ref.clone(),
        ),
        &a,
    )
    .await?;
    publish::audit(
        &mut tx,
        &scope,
        "student.student.update_communication_preferences",
        "student",
        s.id.to_string(),
        serde_json::json!({ "changedFields": changed }),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &next, next.version))
}

/// A student's communication preferences (`student.read`).
#[utoipa::path(
    get,
    path = "/api/v1/student/students/{id}/communication-preferences",
    operation_id = "student_get_communication_preferences",
    tag = "students",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses((status = 200, body = CommunicationPreferences, headers(("ETag" = String))), ReadErrors)
)]
pub async fn get_staff(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentId>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let s = load_student(&mut *conn, tenant(ctx), id).await?;
    let p = load(&mut conn, &s, false).await?;
    Ok(with_etag(StatusCode::OK, &p, p.version))
}

/// Replace a student's communication preferences (`student.manage`; `If-Match`).
#[utoipa::path(
    put,
    path = "/api/v1/student/students/{id}/communication-preferences",
    operation_id = "student_put_communication_preferences",
    tag = "students",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "Preferences version (1 when never stored)")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = PreferencesUpdate,
    responses((status = 200, body = CommunicationPreferences, headers(("ETag" = String))), CommandErrors)
)]
pub async fn put_staff(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentId>,
    headers: HeaderMap,
    Json(p): Json<PreferencesUpdate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentManage)?;
    let expected = expect_version!(headers);
    let s = load_student(&st.pool, tenant(ctx), id).await?;
    put(&st, ctx, s, expected, p).await
}

/// Own communication preferences (`student.self`).
#[utoipa::path(
    get,
    path = "/api/v1/student/me/communication-preferences",
    operation_id = "student_self_get_communication_preferences",
    tag = "students",
    security(("session_cookie" = [])),
    responses((status = 200, body = CommunicationPreferences, headers(("ETag" = String))), ReadErrors)
)]
pub async fn get_me(State(st): State<StudentState>, auth: Authenticated) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentSelfService)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let s = me(&mut *conn, ctx).await?;
    let p = load(&mut conn, &s, false).await?;
    Ok(with_etag(StatusCode::OK, &p, p.version))
}

/// Replace own communication preferences (`student.self`; `If-Match`).
#[utoipa::path(
    put,
    path = "/api/v1/student/me/communication-preferences",
    operation_id = "student_self_put_communication_preferences",
    tag = "students",
    params(("If-Match" = String, Header, description = "Preferences version (1 when never stored)")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = PreferencesUpdate,
    responses((status = 200, body = CommunicationPreferences, headers(("ETag" = String))), CommandErrors)
)]
pub async fn put_me(
    State(st): State<StudentState>,
    auth: Authenticated,
    headers: HeaderMap,
    Json(p): Json<PreferencesUpdate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentSelfService)?;
    let expected = expect_version!(headers);
    let s = me(&st.pool, ctx).await?;
    put(&st, ctx, s, expected, p).await
}
