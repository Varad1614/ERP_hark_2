//! Staff records (L3-E-058, 059, 065, 009, 068): creation through the shared inserter,
//! core-profile maintenance, verification → activation, suspension / restoration, archive,
//! governed corrections with before / after history, duplicates → canonical record.

use super::common::{
    self, check_version, clean, emit, expected, invalid, modified, reason_of, required_reason,
    staff_extra, staff_or_404, status_payload, ReasonBody,
};
use crate::shared::guard::require_institution_writable;
use crate::shared::http::{actor, with_etag};
use crate::shared::lifecycle::{change_staff_status, StatusChange};
use crate::shared::rows::{current_engagement, lock_staff};
use crate::shared::scope::institution_scope;
use crate::shared::staff::{insert_staff, NewStaff};
use crate::shared::{Actor, PeopleState};
use crate::staff_directory::http::EngagementView;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, Row};
use sutra_contracts::people::events::workforce as ev;
use sutra_contracts::people::ids::StaffMemberId;
use sutra_contracts::people::ports::StaffSummary;
use sutra_contracts::people::status::{Gender, StaffCategory, StaffStatus};
use sutra_kernel::auth::Permission;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

// ------------------------------------------------------------------ views

/// A staff record with its full core profile and current engagement.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StaffRecordView {
    pub id: Uuid,
    pub institution_id: Uuid,
    #[schema(example = "EMP-000001")]
    pub staff_number: String,
    pub subject_ref: Option<String>,
    pub given_name: String,
    pub family_name: Option<String>,
    pub display_name: String,
    pub work_email: Option<String>,
    pub phone: Option<String>,
    pub date_of_birth: Option<NaiveDate>,
    pub gender: Option<String>,
    pub staff_category: StaffCategory,
    pub status: StaffStatus,
    pub status_reason: Option<String>,
    pub verified_at: Option<DateTime<Utc>>,
    pub canonical_staff_id: Option<Uuid>,
    pub engagement: Option<EngagementView>,
    pub version: i64,
}

/// Builds the record view of a staff summary.
pub async fn record_view(
    conn: &mut PgConnection,
    tenant: TenantId,
    s: &StaffSummary,
) -> Result<StaffRecordView, KernelError> {
    let x = staff_extra(conn, tenant, s.id).await?;
    let e = current_engagement(conn, tenant, s.id).await?;
    Ok(StaffRecordView {
        id: s.id.as_uuid(),
        institution_id: s.institution_id.as_uuid(),
        staff_number: s.staff_number.clone(),
        subject_ref: s.subject_ref.clone(),
        given_name: s.given_name.clone(),
        family_name: s.family_name.clone(),
        display_name: s.display_name.clone(),
        work_email: s.work_email.clone(),
        phone: x.phone,
        date_of_birth: x.date_of_birth,
        gender: x.gender,
        staff_category: s.staff_category,
        status: s.status,
        status_reason: x.status_reason,
        verified_at: x.verified_at,
        canonical_staff_id: s.canonical_staff_id.map(|c| c.as_uuid()),
        engagement: e.as_ref().map(EngagementView::from),
        version: s.version,
    })
}

async fn respond(
    st: &PeopleState,
    tenant: TenantId,
    s: &StaffSummary,
    status: StatusCode,
) -> ApiResult<Response> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let v = record_view(&mut conn, tenant, s).await?;
    Ok(with_etag(status, &v, Version(s.version)))
}

/// One governed correction (before / after).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CorrectionView {
    pub id: Uuid,
    pub field: String,
    pub before_value: Option<String>,
    pub after_value: Option<String>,
    pub reason: String,
    pub staff_version: i64,
    pub actor: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CorrectionList {
    pub items: Vec<CorrectionView>,
}

// ------------------------------------------------------------------ bodies

/// A new staff record (HR creation, L3-E-058).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StaffCreate {
    /// Login subject to link (`dev|meera.shah@sutra.dev`); one live record per login.
    pub subject_ref: Option<String>,
    pub given_name: String,
    pub family_name: Option<String>,
    pub display_name: Option<String>,
    pub work_email: Option<String>,
    pub phone: Option<String>,
    pub date_of_birth: Option<NaiveDate>,
    pub gender: Option<Gender>,
    pub staff_category: StaffCategory,
    /// `draft` (default), `pending_verification`, or `active` when HR verified the identity.
    pub initial_status: Option<StaffStatus>,
}

/// Core profile maintenance (L3-E-059): the full set of editable fields.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StaffUpdate {
    pub given_name: String,
    pub family_name: Option<String>,
    pub display_name: Option<String>,
    pub work_email: Option<String>,
    pub phone: Option<String>,
    pub date_of_birth: Option<NaiveDate>,
    pub gender: Option<Gender>,
    pub staff_category: StaffCategory,
}

/// Governed correction (L3-E-065): only the fields to correct; an empty string clears a field.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CorrectionChanges {
    pub given_name: Option<String>,
    pub family_name: Option<String>,
    pub display_name: Option<String>,
    pub work_email: Option<String>,
    pub phone: Option<String>,
    /// `YYYY-MM-DD`, or empty to clear.
    pub date_of_birth: Option<String>,
    /// `Gender` wire name, or empty to clear.
    pub gender: Option<String>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = StaffCorrectionBody)]
pub struct CorrectionBody {
    pub changes: CorrectionChanges,
    pub reason: String,
}

/// Duplicate → canonical (L3-E-009).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MergeBody {
    /// The record that survives.
    pub canonical_staff_id: Uuid,
    pub reason: String,
}

// ------------------------------------------------------------------ profile rules

/// The editable core profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub given_name: String,
    pub family_name: Option<String>,
    pub display_name: String,
    pub work_email: Option<String>,
    pub phone: Option<String>,
    pub date_of_birth: Option<NaiveDate>,
    pub gender: Option<String>,
    pub staff_category: StaffCategory,
}

impl Profile {
    /// Validates the profile (422 on the first offending field).
    pub fn validate(&self) -> Result<(), KernelError> {
        let n = self.given_name.chars().count();
        if self.given_name.trim().is_empty() || n > 100 {
            return Err(invalid("givenName", "required", "1 to 100 characters"));
        }
        if self
            .family_name
            .as_deref()
            .is_some_and(|f| f.chars().count() > 100)
        {
            return Err(invalid("familyName", "too_long", "at most 100 characters"));
        }
        let d = self.display_name.chars().count();
        if self.display_name.trim().is_empty() || d > 200 {
            return Err(invalid("displayName", "required", "1 to 200 characters"));
        }
        if let Some(e) = &self.work_email {
            let parts: Vec<&str> = e.split('@').collect();
            if parts.len() != 2
                || parts.iter().any(|p| p.is_empty())
                || e.contains(char::is_whitespace)
            {
                return Err(invalid(
                    "workEmail",
                    "invalid_email",
                    "must be an e-mail address",
                ));
            }
        }
        if let Some(p) = &self.phone {
            let n = p.chars().count();
            if !(3..=32).contains(&n) {
                return Err(invalid("phone", "invalid_phone", "3 to 32 characters"));
            }
        }
        if let Some(b) = self.date_of_birth {
            let min = NaiveDate::from_ymd_opt(1900, 1, 1).unwrap_or(b);
            if b < min || b > Utc::now().date_naive() {
                return Err(invalid(
                    "dateOfBirth",
                    "out_of_range",
                    "must be a past date after 1900-01-01",
                ));
            }
        }
        if let Some(g) = &self.gender {
            if Gender::parse(g).is_none() {
                return Err(invalid("gender", "invalid", "unknown gender"));
            }
        }
        Ok(())
    }

    /// `(field, before, after)` of every changed member (camelCase API names).
    pub fn diff(&self, after: &Profile) -> Vec<(&'static str, Option<String>, Option<String>)> {
        let mut v = Vec::new();
        let mut push = |f: &'static str, a: Option<String>, b: Option<String>| {
            if a != b {
                v.push((f, a, b));
            }
        };
        push(
            "givenName",
            Some(self.given_name.clone()),
            Some(after.given_name.clone()),
        );
        push(
            "familyName",
            self.family_name.clone(),
            after.family_name.clone(),
        );
        push(
            "displayName",
            Some(self.display_name.clone()),
            Some(after.display_name.clone()),
        );
        push(
            "workEmail",
            self.work_email.clone(),
            after.work_email.clone(),
        );
        push("phone", self.phone.clone(), after.phone.clone());
        push(
            "dateOfBirth",
            self.date_of_birth.map(|d| d.to_string()),
            after.date_of_birth.map(|d| d.to_string()),
        );
        push("gender", self.gender.clone(), after.gender.clone());
        push(
            "staffCategory",
            Some(self.staff_category.as_str().to_owned()),
            Some(after.staff_category.as_str().to_owned()),
        );
        v
    }
}

async fn current_profile(
    conn: &mut PgConnection,
    tenant: TenantId,
    s: &StaffSummary,
) -> Result<Profile, KernelError> {
    let x = staff_extra(conn, tenant, s.id).await?;
    Ok(Profile {
        given_name: s.given_name.clone(),
        family_name: s.family_name.clone(),
        display_name: s.display_name.clone(),
        work_email: s.work_email.clone(),
        phone: x.phone,
        date_of_birth: x.date_of_birth,
        gender: x.gender,
        staff_category: s.staff_category,
    })
}

/// 409 when another live record of the tenant already uses the e-mail (likely duplicate).
async fn ensure_email_free(
    conn: &mut PgConnection,
    tenant: TenantId,
    email: Option<&str>,
    except: Option<StaffMemberId>,
) -> Result<(), KernelError> {
    let Some(email) = email else { return Ok(()) };
    let other: Option<String> = sqlx::query_scalar(
        "SELECT staff_number FROM people.staff_member WHERE tenant_id = $1 \
         AND lower(work_email) = lower($2) AND status NOT IN ('merged', 'archived') \
         AND ($3::uuid IS NULL OR id <> $3) LIMIT 1",
    )
    .bind(tenant.as_uuid())
    .bind(email)
    .bind(except.map(|e| e.as_uuid()))
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    match other {
        Some(n) => Err(KernelError::Conflict(format!(
            "staff record {n} already uses this work e-mail (possible duplicate)"
        ))),
        None => Ok(()),
    }
}

/// Writes the profile columns of a locked staff row (version + 1).
async fn write_profile(
    conn: &mut PgConnection,
    tenant: TenantId,
    s: &StaffSummary,
    p: &Profile,
) -> Result<StaffSummary, KernelError> {
    sqlx::query(
        "UPDATE people.staff_member SET given_name = $3, family_name = $4, display_name = $5, \
         work_email = $6, phone = $7, date_of_birth = $8, gender = $9, staff_category = $10, \
         version = version + 1, updated_at = now() WHERE id = $1 AND tenant_id = $2 AND version = $11",
    )
    .bind(s.id.as_uuid())
    .bind(tenant.as_uuid())
    .bind(&p.given_name)
    .bind(&p.family_name)
    .bind(&p.display_name)
    .bind(&p.work_email)
    .bind(&p.phone)
    .bind(p.date_of_birth)
    .bind(&p.gender)
    .bind(p.staff_category.as_str())
    .bind(s.version)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    lock_staff(conn, tenant, s.id).await
}

fn scope_of(
    ctx: &sutra_kernel::auth::AuthContext,
    s: &StaffSummary,
) -> sutra_kernel::scope::ExecutionScope {
    institution_scope(ctx, s.institution_id)
}

// ------------------------------------------------------------------ handlers

/// Create a staff record (`people.staff.manage`; Idempotency-Key). Starts `draft`,
/// `pending_verification`, or `active` for HR-verified creation; 409 for a login or work e-mail
/// already used by a live record.
#[utoipa::path(post, path = "/api/v1/people/institutions/{id}/staff", operation_id = "people_staff_create", tag = "people-workforce",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)),
    request_body = StaffCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = StaffRecordView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn create(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<StaffCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    require_idempotency_key(&headers)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let given = b.given_name.trim().to_owned();
    let family = clean(b.family_name);
    let display = clean(b.display_name).unwrap_or_else(|| match &family {
        Some(f) => format!("{given} {f}"),
        None => given.clone(),
    });
    let profile = Profile {
        given_name: given,
        family_name: family,
        display_name: display,
        work_email: clean(b.work_email),
        phone: clean(b.phone),
        date_of_birth: b.date_of_birth,
        gender: b.gender.map(|g| g.as_str().to_owned()),
        staff_category: b.staff_category,
    };
    profile.validate()?;
    let status = b.initial_status.unwrap_or(StaffStatus::Draft);
    let subject = clean(b.subject_ref);
    let actor = actor(ctx);
    let t = ctx.scope.tenant_id;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    ensure_email_free(&mut tx, t, profile.work_email.as_deref(), None).await?;
    let s = insert_staff(
        &mut tx,
        &NewStaff {
            tenant: t,
            institution,
            subject_ref: subject.as_deref(),
            given_name: &profile.given_name,
            family_name: profile.family_name.as_deref(),
            display_name: Some(profile.display_name.as_str()),
            work_email: profile.work_email.as_deref(),
            phone: profile.phone.as_deref(),
            date_of_birth: profile.date_of_birth,
            gender: b.gender,
            staff_category: profile.staff_category,
            status,
        },
        &actor,
    )
    .await?;
    let mut fields = vec!["givenName", "displayName", "staffCategory", "status"];
    for (f, set) in [
        ("subjectRef", s.subject_ref.is_some()),
        ("familyName", profile.family_name.is_some()),
        ("workEmail", profile.work_email.is_some()),
        ("phone", profile.phone.is_some()),
        ("dateOfBirth", profile.date_of_birth.is_some()),
        ("gender", profile.gender.is_some()),
    ] {
        if set {
            fields.push(f);
        }
    }
    emit(
        &mut tx,
        &scope,
        ev::STAFF_CREATED,
        ev::STAFF_CREATED_SCHEMA,
        "staff_member",
        s.id.as_uuid(),
        "people.staff_member.create",
        &modified(s.id.as_uuid(), institution, s.version, &fields),
        &actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, t, &s, StatusCode::CREATED).await
}

/// Maintain the core profile (`people.staff.manage`; If-Match; L3-E-059). Merged / archived
/// records are read-only (409).
#[utoipa::path(put, path = "/api/v1/people/staff/{id}", operation_id = "people_staff_update", tag = "people-workforce",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the staff record")),
    request_body = StaffUpdate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = StaffRecordView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn update(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<StaffUpdate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let s = lock_staff(&mut tx, t, StaffMemberId(id)).await?;
    check_version(v, s.version)?;
    let scope = scope_of(ctx, &s);
    require_institution_writable(&st, &scope, s.institution_id).await?;
    if !s.status.is_live() {
        return Err(KernelError::Conflict(format!(
            "staff record {} is {}; use a governed correction",
            s.staff_number, s.status
        ))
        .into());
    }
    let before = current_profile(&mut tx, t, &s).await?;
    let given = b.given_name.trim().to_owned();
    let family = clean(b.family_name);
    let after = Profile {
        display_name: clean(b.display_name).unwrap_or_else(|| match &family {
            Some(f) => format!("{given} {f}"),
            None => given.clone(),
        }),
        given_name: given,
        family_name: family,
        work_email: clean(b.work_email),
        phone: clean(b.phone),
        date_of_birth: b.date_of_birth,
        gender: b.gender.map(|g| g.as_str().to_owned()),
        staff_category: b.staff_category,
    };
    after.validate()?;
    let changes = before.diff(&after);
    if changes.is_empty() {
        tx.commit().await.map_err(map_db_error)?;
        return respond(&st, t, &s, StatusCode::OK).await;
    }
    if after.work_email != before.work_email {
        ensure_email_free(&mut tx, t, after.work_email.as_deref(), Some(s.id)).await?;
    }
    let updated = write_profile(&mut tx, t, &s, &after).await?;
    let fields: Vec<&str> = changes.iter().map(|c| c.0).collect();
    emit(
        &mut tx,
        &scope,
        ev::STAFF_MODIFIED,
        ev::STAFF_MODIFIED_SCHEMA,
        "staff_member",
        s.id.as_uuid(),
        "people.staff_member.update",
        &modified(s.id.as_uuid(), s.institution_id, updated.version, &fields),
        &actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, t, &updated, StatusCode::OK).await
}

/// One staff-record state command.
struct Move {
    to: StaffStatus,
    /// Allowed sources (subset of the shared machine for this command).
    from: &'static [StaffStatus],
    event: Option<(&'static str, &'static str)>,
    action: &'static str,
    reason_required: bool,
}

async fn staff_move(
    st: &PeopleState,
    auth: &Authenticated,
    id: Uuid,
    headers: &HeaderMap,
    reason: Option<String>,
    m: Move,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    let v = match crate::shared::http::if_match(headers)? {
        Ok(v) => v.0,
        Err(resp) => return Ok(resp),
    };
    let reason = if m.reason_required {
        Some(required_reason(reason)?)
    } else {
        reason
    };
    let t = ctx.scope.tenant_id;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let s = lock_staff(&mut tx, t, StaffMemberId(id)).await?;
    check_version(v, s.version)?;
    let scope = scope_of(ctx, &s);
    require_institution_writable(st, &scope, s.institution_id).await?;
    if !m.from.contains(&s.status) {
        return Err(common::illegal(
            "staff record",
            &s.staff_number,
            s.status.as_str(),
            m.to.as_str(),
        )
        .into());
    }
    if m.to == StaffStatus::Archived {
        let live: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM people.staff_engagement WHERE staff_id = $1 \
             AND status NOT IN ('closed', 'cancelled'))",
        )
        .bind(s.id.as_uuid())
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
        if live {
            return Err(KernelError::Conflict(format!(
                "staff record {} still has a live engagement; close it (separation) before archiving",
                s.staff_number
            ))
            .into());
        }
    }
    let cause = m
        .event
        .map(|e| e.0)
        .unwrap_or("sutraerp.people.staff_member.status");
    let moved = change_staff_status(
        &mut tx,
        t,
        &s,
        m.to,
        &StatusChange {
            cause_event_type: cause,
            reason: reason.as_deref(),
            process_instance_id: None,
        },
        &actor,
    )
    .await?;
    let payload = status_payload(
        s.id.as_uuid(),
        s.institution_id,
        Some(&moved),
        None,
        Some(s.status.as_str()),
        m.to.as_str(),
        moved.version,
        reason.as_deref(),
        None,
        None,
    );
    match m.event {
        Some((ty, schema)) => {
            emit(
                &mut tx,
                &scope,
                ty,
                schema,
                "staff_member",
                s.id.as_uuid(),
                m.action,
                &payload,
                &actor,
            )
            .await?
        }
        None => {
            common::audit_only(
                &mut tx,
                &scope,
                "staff_member",
                s.id.as_uuid(),
                m.action,
                serde_json::to_value(&payload).map_err(KernelError::internal)?,
                &actor,
            )
            .await?
        }
    }
    tx.commit().await.map_err(map_db_error)?;
    respond(st, t, &moved, StatusCode::OK).await
}

macro_rules! staff_command {
    ($(#[$m:meta])* $fn:ident, $path:literal, $op:literal, $move:expr) => {
        $(#[$m])*
        #[utoipa::path(post, path = $path, operation_id = $op, tag = "people-workforce",
            params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the staff record")),
            request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
            responses((status = 200, body = StaffRecordView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
        pub async fn $fn(
            State(st): State<PeopleState>,
            auth: Authenticated,
            Path(id): Path<Uuid>,
            headers: HeaderMap,
            body: Option<Json<ReasonBody>>,
        ) -> ApiResult<Response> {
            staff_move(&st, &auth, id, &headers, reason_of(body), $move).await
        }
    };
}

staff_command!(
    /// Submit a draft profile for identity verification (`people.staff.manage`; L3-E-058).
    submit_verification,
    "/api/v1/people/staff/{id}/submit-verification",
    "people_staff_submit_verification",
    Move {
        to: StaffStatus::PendingVerification,
        from: &[StaffStatus::Draft],
        event: None,
        action: "people.staff_member.submit_verification",
        reason_required: false,
    }
);
staff_command!(
    /// Record the identity verification of a pending profile (`people.staff.manage`; L3-E-058).
    verify,
    "/api/v1/people/staff/{id}/verify",
    "people_staff_verify",
    Move {
        to: StaffStatus::Verified,
        from: &[StaffStatus::PendingVerification],
        event: Some((ev::STAFF_VERIFIED, ev::STAFF_VERIFIED_SCHEMA)),
        action: "people.staff_member.verify",
        reason_required: false,
    }
);
staff_command!(
    /// Activate a verified profile (`people.staff.manage`; L3-E-058).
    activate,
    "/api/v1/people/staff/{id}/activate",
    "people_staff_activate",
    Move {
        to: StaffStatus::Active,
        from: &[StaffStatus::Verified],
        event: Some((ev::STAFF_ACTIVATED, ev::STAFF_ACTIVATED_SCHEMA)),
        action: "people.staff_member.activate",
        reason_required: false,
    }
);
staff_command!(
    /// Suspend an active profile (`people.staff.manage`; reason required).
    suspend,
    "/api/v1/people/staff/{id}/suspend",
    "people_staff_suspend",
    Move {
        to: StaffStatus::Suspended,
        from: &[StaffStatus::Active],
        event: Some((ev::STAFF_SUSPENDED, ev::STAFF_SUSPENDED_SCHEMA)),
        action: "people.staff_member.suspend",
        reason_required: true,
    }
);
staff_command!(
    /// Restore a suspended profile (`people.staff.manage`).
    restore,
    "/api/v1/people/staff/{id}/restore",
    "people_staff_restore",
    Move {
        to: StaffStatus::Active,
        from: &[StaffStatus::Suspended],
        event: Some((ev::STAFF_RESTORED, ev::STAFF_RESTORED_SCHEMA)),
        action: "people.staff_member.restore",
        reason_required: false,
    }
);
staff_command!(
    /// Archive the profile after separation (`people.staff.manage`; L3-E-068). 409 while an engagement is live.
    archive,
    "/api/v1/people/staff/{id}/archive",
    "people_staff_archive",
    Move {
        to: StaffStatus::Archived,
        from: &[StaffStatus::Active, StaffStatus::Suspended],
        event: Some((ev::STAFF_ARCHIVED, ev::STAFF_ARCHIVED_SCHEMA)),
        action: "people.staff_member.archive",
        reason_required: true,
    }
);

/// Merge a duplicate onto its canonical record (`people.staff.manage`; If-Match of the duplicate;
/// L3-E-009). The duplicate's engagements must be closed / cancelled first; its qualifications,
/// documents and login link move to the canonical record.
#[utoipa::path(post, path = "/api/v1/people/staff/{id}/merge", operation_id = "people_staff_merge", tag = "people-workforce",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the duplicate")),
    request_body = MergeBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = StaffRecordView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn merge(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<MergeBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    let v = expected!(headers);
    let reason = required_reason(Some(b.reason))?;
    if b.canonical_staff_id == id {
        return Err(invalid(
            "canonicalStaffId",
            "same_record",
            "a record cannot merge into itself",
        )
        .into());
    }
    let t = ctx.scope.tenant_id;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    // Lock in id order (no deadlock between two merges of the same pair).
    let (dup, canon) = if id < b.canonical_staff_id {
        let d = lock_staff(&mut tx, t, StaffMemberId(id)).await?;
        let c = lock_staff(&mut tx, t, StaffMemberId(b.canonical_staff_id)).await;
        (d, c)
    } else {
        let c = lock_staff(&mut tx, t, StaffMemberId(b.canonical_staff_id)).await;
        let d = lock_staff(&mut tx, t, StaffMemberId(id)).await?;
        (d, c)
    };
    let canon =
        canon.map_err(|_| invalid("canonicalStaffId", "not_found", "unknown staff record"))?;
    check_version(v, dup.version)?;
    let scope = scope_of(ctx, &dup);
    require_institution_writable(&st, &scope, dup.institution_id).await?;
    if canon.institution_id != dup.institution_id {
        return Err(invalid(
            "canonicalStaffId",
            "other_institution",
            "the canonical record belongs to another institution",
        )
        .into());
    }
    if !canon.status.is_live() {
        return Err(KernelError::Conflict(format!(
            "canonical record {} is {}",
            canon.staff_number, canon.status
        ))
        .into());
    }
    if !dup.status.is_live() {
        return Err(common::illegal(
            "staff record",
            &dup.staff_number,
            dup.status.as_str(),
            "merged",
        )
        .into());
    }
    let live: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM people.staff_engagement WHERE staff_id = $1 \
         AND status NOT IN ('closed', 'cancelled'))",
    )
    .bind(dup.id.as_uuid())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if live {
        return Err(KernelError::Conflict(format!(
            "duplicate {} still has a live engagement; close or cancel it first",
            dup.staff_number
        ))
        .into());
    }
    sqlx::query(
        "UPDATE people.staff_member SET canonical_staff_id = $2, version = version + 1, \
         updated_at = now() WHERE id = $1",
    )
    .bind(dup.id.as_uuid())
    .bind(canon.id.as_uuid())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let dup = lock_staff(&mut tx, t, dup.id).await?;
    let merged = change_staff_status(
        &mut tx,
        t,
        &dup,
        StaffStatus::Merged,
        &StatusChange {
            cause_event_type: ev::STAFF_MERGED,
            reason: Some(reason.as_str()),
            process_instance_id: None,
        },
        &actor,
    )
    .await?;
    for table in ["staff_qualification", "staff_document"] {
        sqlx::query(&format!(
            "UPDATE people.{table} SET staff_id = $2, version = version + 1, updated_at = now() \
             WHERE staff_id = $1 AND tenant_id = $3"
        ))
        .bind(dup.id.as_uuid())
        .bind(canon.id.as_uuid())
        .bind(t.as_uuid())
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    }
    let mut canon_fields = vec!["qualifications", "documents"];
    if canon.subject_ref.is_none() {
        if let Some(subject) = &dup.subject_ref {
            sqlx::query(
                "UPDATE people.staff_member SET subject_ref = $2, version = version + 1, \
                 updated_at = now() WHERE id = $1",
            )
            .bind(canon.id.as_uuid())
            .bind(subject)
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
            canon_fields.push("subjectRef");
        }
    }
    let canon_after = lock_staff(&mut tx, t, canon.id).await?;
    let note = format!("merged into {}: {reason}", canon.staff_number);
    let mut payload = status_payload(
        dup.id.as_uuid(),
        dup.institution_id,
        Some(&merged),
        None,
        Some(dup.status.as_str()),
        StaffStatus::Merged.as_str(),
        merged.version,
        Some(note.as_str()),
        None,
        None,
    );
    payload.staff_subject_ref = dup.subject_ref.clone();
    emit(
        &mut tx,
        &scope,
        ev::STAFF_MERGED,
        ev::STAFF_MERGED_SCHEMA,
        "staff_member",
        dup.id.as_uuid(),
        "people.staff_member.merge",
        &payload,
        &actor,
    )
    .await?;
    emit(
        &mut tx,
        &scope,
        ev::STAFF_MODIFIED,
        ev::STAFF_MODIFIED_SCHEMA,
        "staff_member",
        canon.id.as_uuid(),
        "people.staff_member.absorb_duplicate",
        &modified(
            canon.id.as_uuid(),
            canon.institution_id,
            canon_after.version,
            &canon_fields,
        ),
        &actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, t, &merged, StatusCode::OK).await
}

fn corrected_profile(before: &Profile, c: &CorrectionChanges) -> Result<Profile, KernelError> {
    let mut p = before.clone();
    if let Some(g) = &c.given_name {
        p.given_name = g.trim().to_owned();
    }
    if let Some(f) = &c.family_name {
        p.family_name = clean(Some(f.clone()));
    }
    if let Some(d) = &c.display_name {
        p.display_name = d.trim().to_owned();
    }
    if let Some(e) = &c.work_email {
        p.work_email = clean(Some(e.clone()));
    }
    if let Some(ph) = &c.phone {
        p.phone = clean(Some(ph.clone()));
    }
    if let Some(d) = &c.date_of_birth {
        p.date_of_birth = match clean(Some(d.clone())) {
            None => None,
            Some(raw) => Some(
                NaiveDate::parse_from_str(&raw, "%Y-%m-%d")
                    .map_err(|_| invalid("dateOfBirth", "invalid_date", "YYYY-MM-DD"))?,
            ),
        };
    }
    if let Some(g) = &c.gender {
        p.gender = clean(Some(g.clone()));
    }
    Ok(p)
}

/// Apply a governed correction with before / after history (`people.staff.manage`; If-Match;
/// reason required; L3-E-065). Allowed on any record that is not merged (archived history can be
/// corrected); 422 when nothing changes.
#[utoipa::path(post, path = "/api/v1/people/staff/{id}/corrections", operation_id = "people_staff_correct", tag = "people-workforce",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the staff record")),
    request_body = CorrectionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = StaffRecordView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn correct(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<CorrectionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    let v = expected!(headers);
    let reason = required_reason(Some(b.reason))?;
    let t = ctx.scope.tenant_id;
    let actor: Actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let s = lock_staff(&mut tx, t, StaffMemberId(id)).await?;
    check_version(v, s.version)?;
    let scope = scope_of(ctx, &s);
    require_institution_writable(&st, &scope, s.institution_id).await?;
    if s.status == StaffStatus::Merged {
        return Err(KernelError::Conflict(format!(
            "staff record {} is merged; correct its canonical record",
            s.staff_number
        ))
        .into());
    }
    let before = current_profile(&mut tx, t, &s).await?;
    let after = corrected_profile(&before, &b.changes)?;
    after.validate()?;
    let changes = before.diff(&after);
    if changes.is_empty() {
        return Err(invalid("changes", "no_change", "the correction changes nothing").into());
    }
    if after.work_email != before.work_email {
        ensure_email_free(&mut tx, t, after.work_email.as_deref(), Some(s.id)).await?;
    }
    let updated = write_profile(&mut tx, t, &s, &after).await?;
    for (field, from, to) in &changes {
        sqlx::query(
            "INSERT INTO people.profile_correction (tenant_id, institution_id, staff_id, field, \
             before_value, after_value, reason, staff_version, actor, correlation_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(t.as_uuid())
        .bind(s.institution_id.as_uuid())
        .bind(s.id.as_uuid())
        .bind(*field)
        .bind(from)
        .bind(to)
        .bind(&reason)
        .bind(updated.version)
        .bind(actor.id.as_deref())
        .bind(actor.correlation_id.as_deref())
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    }
    let fields: Vec<&str> = changes.iter().map(|c| c.0).collect();
    emit(
        &mut tx,
        &scope,
        ev::STAFF_CORRECTED,
        ev::STAFF_CORRECTED_SCHEMA,
        "staff_member",
        s.id.as_uuid(),
        "people.staff_member.correct",
        &modified(s.id.as_uuid(), s.institution_id, updated.version, &fields),
        &actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, t, &updated, StatusCode::OK).await
}

/// Correction history of a staff record (`people.read`), newest first.
#[utoipa::path(get, path = "/api/v1/people/staff/{id}/corrections", operation_id = "people_staff_correction_list", tag = "people-workforce",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = CorrectionList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn corrections(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<CorrectionList>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    staff_or_404(&mut conn, t, id).await?;
    let rows = sqlx::query(
        "SELECT id, field, before_value, after_value, reason, staff_version, actor, created_at \
         FROM people.profile_correction WHERE staff_id = $1 AND tenant_id = $2 \
         ORDER BY created_at DESC, field",
    )
    .bind(id)
    .bind(t.as_uuid())
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let mut items = Vec::with_capacity(rows.len());
    for r in &rows {
        items.push(CorrectionView {
            id: r.try_get("id").map_err(map_db_error)?,
            field: r.try_get("field").map_err(map_db_error)?,
            before_value: r.try_get("before_value").map_err(map_db_error)?,
            after_value: r.try_get("after_value").map_err(map_db_error)?,
            reason: r.try_get("reason").map_err(map_db_error)?,
            staff_version: r.try_get("staff_version").map_err(map_db_error)?,
            actor: r.try_get("actor").map_err(map_db_error)?,
            created_at: r.try_get("created_at").map_err(map_db_error)?,
        });
    }
    Ok(Json(CorrectionList { items }))
}

pub fn router(state: PeopleState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(create))
        .routes(routes!(update))
        .routes(routes!(submit_verification))
        .routes(routes!(verify))
        .routes(routes!(activate))
        .routes(routes!(suspend))
        .routes(routes!(restore))
        .routes(routes!(archive))
        .routes(routes!(merge))
        .routes(routes!(correct, corrections))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p() -> Profile {
        Profile {
            given_name: "Meera".into(),
            family_name: Some("Shah".into()),
            display_name: "Meera Shah".into(),
            work_email: Some("meera@sutra.dev".into()),
            phone: None,
            date_of_birth: None,
            gender: None,
            staff_category: StaffCategory::Teaching,
        }
    }

    #[test]
    fn profile_validation_and_diff() {
        assert!(p().validate().is_ok());
        let mut bad = p();
        bad.work_email = Some("no-at".into());
        assert!(bad.validate().is_err());
        bad = p();
        bad.phone = Some("1".into());
        assert!(bad.validate().is_err());
        bad = p();
        bad.gender = Some("x".into());
        assert!(bad.validate().is_err());
        let mut after = p();
        after.family_name = None;
        after.phone = Some("+91 98200 00000".into());
        let d = p().diff(&after);
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].0, "familyName");
        assert!(p().diff(&p()).is_empty());
        let c = corrected_profile(
            &p(),
            &CorrectionChanges {
                date_of_birth: Some("1990-02-03".into()),
                family_name: Some(String::new()),
                ..CorrectionChanges::default()
            },
        );
        let c = c.ok().unwrap_or_else(p);
        assert_eq!(c.family_name, None);
        assert!(c.date_of_birth.is_some());
    }
}
