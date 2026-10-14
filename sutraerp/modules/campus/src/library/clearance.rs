//! Library clearance (L3-G-149; `LIBRARY_CLEARANCE`): a request is verified against loans still
//! out and unsettled fines / replacement obligations (billed fines read through Finance's
//! `ChargeDirectory`); live reservations are reported but do not block. It is approved only when
//! nothing blocks — auto-approval approves a clean request at once, a blocked one stays
//! `requested` until the member settles and an approver decides. Approval does not close the
//! membership (downstream exit processes consume the outcome).

use super::common::{
    check_version, clean, expected, invalid, required_text, transition, LibraryDecision,
    LibraryDecisionBody, Transition,
};
use super::domain::{LibraryClearanceStatus, LibraryMembershipStatus};
use super::fines::{sync_billed_fines, unsettled_total};
use super::members::load_membership;
use crate::shared::approval::{
    business_key, process, request_approval, ApprovalOutcome, AUTO_APPROVER,
};
use crate::shared::decimal::fmt2;
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, with_etag};
use crate::shared::lifecycle::illegal;
use crate::shared::rows::decode;
use crate::shared::scope::institution_scope;
use crate::shared::{Actor, CampusState};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use std::collections::BTreeMap;
use sutra_contracts::campus::events::library as ev;
use sutra_contracts::campus::status::PersonKind;
use sutra_kernel::auth::Permission;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// Verification result of a clearance request.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LibraryClearanceCheck {
    pub active_loans: i64,
    #[schema(value_type = String, example = "0.00")]
    pub unsettled_fines: String,
    pub live_reservations: i64,
    /// Empty = nothing blocks the clearance.
    pub blockers: Vec<String>,
}

/// A clearance request.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LibraryClearanceView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub membership_id: Uuid,
    pub member_number: String,
    pub display_name: String,
    pub person_kind: PersonKind,
    pub purpose: String,
    pub status: LibraryClearanceStatus,
    pub status_reason: Option<String>,
    pub verification: LibraryClearanceCheck,
    pub process_instance_id: Option<String>,
    pub decided_by: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub version: i64,
    #[serde(skip)]
    pub subject_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct LibraryClearanceList {
    pub items: Vec<LibraryClearanceView>,
}

const SELECT: &str = "SELECT k.id, k.institution_id, k.membership_id, m.number AS member_number, \
    m.display_name, k.person_kind, k.subject_ref, k.purpose, k.status, k.status_reason, \
    k.verification, k.process_instance_id, k.decided_by, k.decided_at, k.created_at, k.version \
    FROM campus.library_clearance k JOIN campus.library_membership m ON m.id = k.membership_id";

fn from_row(r: &PgRow) -> Result<LibraryClearanceView, sqlx::Error> {
    let verification: serde_json::Value = r.try_get("verification")?;
    Ok(LibraryClearanceView {
        id: r.try_get("id")?,
        institution_id: r.try_get("institution_id")?,
        membership_id: r.try_get("membership_id")?,
        member_number: r.try_get("member_number")?,
        display_name: r.try_get("display_name")?,
        person_kind: decode(r.try_get("person_kind")?, PersonKind::parse)?,
        purpose: r.try_get("purpose")?,
        status: decode(r.try_get("status")?, LibraryClearanceStatus::parse)?,
        status_reason: r.try_get("status_reason")?,
        verification: serde_json::from_value(verification).unwrap_or_default(),
        process_instance_id: r.try_get("process_instance_id")?,
        decided_by: r.try_get("decided_by")?,
        decided_at: r.try_get("decided_at")?,
        created_at: r.try_get("created_at")?,
        version: r.try_get("version")?,
        subject_ref: r.try_get("subject_ref")?,
    })
}

async fn load(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<LibraryClearanceView, KernelError> {
    if lock {
        sqlx::query(
            "SELECT id FROM campus.library_clearance WHERE id = $1 AND tenant_id = $2 FOR UPDATE",
        )
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?;
    }
    let row = sqlx::query(&format!("{SELECT} WHERE k.id = $1 AND k.tenant_id = $2"))
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "library clearance",
        })?;
    from_row(&row).map_err(map_db_error)
}

/// The latest clearance of a membership, if any.
pub async fn latest_of(
    conn: &mut PgConnection,
    tenant: TenantId,
    membership: Uuid,
) -> Result<Option<LibraryClearanceView>, KernelError> {
    let row = sqlx::query(&format!(
        "{SELECT} WHERE k.tenant_id = $1 AND k.membership_id = $2 ORDER BY k.created_at DESC LIMIT 1"
    ))
    .bind(tenant.as_uuid())
    .bind(membership)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    row.as_ref().map(from_row).transpose().map_err(map_db_error)
}

/// Verifies a membership for clearance (billed fines re-read from Finance first).
pub async fn verify(
    st: &CampusState,
    scope: &ExecutionScope,
    membership: Uuid,
) -> Result<LibraryClearanceCheck, KernelError> {
    sync_billed_fines(st, scope, membership).await?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let (loans, reservations): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM campus.library_loan WHERE membership_id = $1 \
         AND status IN ('active', 'overdue')), (SELECT count(*) FROM campus.library_reservation \
         WHERE membership_id = $1 AND status IN ('queued', 'ready'))",
    )
    .bind(membership)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let unsettled = unsettled_total(&mut conn, membership).await?;
    let mut blockers = Vec::new();
    if loans > 0 {
        blockers.push(format!("{loans} loan(s) not returned"));
    }
    if unsettled > Decimal::ZERO {
        blockers.push(format!(
            "unsettled fines / replacement obligations of {}",
            fmt2(unsettled)
        ));
    }
    Ok(LibraryClearanceCheck {
        active_loans: loans,
        unsettled_fines: fmt2(unsettled),
        live_reservations: reservations,
        blockers,
    })
}

#[allow(clippy::too_many_arguments)]
async fn decide_in_tx(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    k: &LibraryClearanceView,
    to: LibraryClearanceStatus,
    check: &LibraryClearanceCheck,
    reason: Option<&str>,
    decided_by: &str,
    actor: &Actor,
) -> Result<(), KernelError> {
    sqlx::query(
        "UPDATE campus.library_clearance SET status = $2, status_reason = $3, verification = $4, \
         decided_by = $5, decided_at = now(), version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(k.id)
    .bind(to.as_str())
    .bind(reason)
    .bind(serde_json::to_value(check).map_err(KernelError::internal)?)
    .bind(decided_by)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    transition(
        conn,
        scope,
        &Transition {
            aggregate_type: "library_clearance",
            id: k.id,
            institution: InstitutionId(k.institution_id),
            from: Some(k.status.as_str()),
            to: to.as_str(),
            version: k.version + 1,
            reason,
            process_instance_id: k.process_instance_id.as_deref(),
            number: Some(&k.member_number),
            subject_ref: k.subject_ref.as_deref(),
        },
        Some((
            ev::LIBRARY_CLEARANCE_DECIDED,
            ev::LIBRARY_CLEARANCE_DECIDED_SCHEMA,
        )),
        if to == LibraryClearanceStatus::Approved {
            "campus.library_clearance.approve"
        } else {
            "campus.library_clearance.reject"
        },
        actor,
    )
    .await
}

/// Requests clearance for a membership (shared by the office and `/me/library/clearance`).
pub async fn request_clearance(
    st: &CampusState,
    scope: &ExecutionScope,
    membership: Uuid,
    purpose: &str,
    actor: &Actor,
) -> Result<LibraryClearanceView, KernelError> {
    let t = scope.tenant_id;
    let m = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_membership(&mut conn, t, membership, false).await?
    };
    let institution = InstitutionId(m.institution_id);
    require_institution_writable(st, scope, institution).await?;
    if !may_request(m.status) {
        return Err(KernelError::Conflict(format!(
            "membership {} is {}; it has nothing to clear yet",
            m.number, m.status
        )));
    }
    let check = verify(st, scope, membership).await?;
    let mut vars = BTreeMap::new();
    vars.insert("memberNumber".to_owned(), serde_json::json!(m.number));
    vars.insert("blockers".to_owned(), serde_json::json!(check.blockers));
    let outcome = request_approval(
        st,
        scope,
        process::LIBRARY_CLEARANCE,
        business_key("library_clearance", m.id, m.version),
        vars,
    )
    .await?;
    let pid = outcome.process_instance_id().to_owned();
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let m = load_membership(&mut tx, t, membership, true).await?;
    let open: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM campus.library_clearance WHERE membership_id = $1 AND status = 'requested')",
    )
    .bind(m.id)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if open {
        return Err(KernelError::Conflict(format!(
            "membership {} already has a clearance request awaiting a decision",
            m.number
        )));
    }
    let pending = (!check.blockers.is_empty()).then(|| check.blockers.join("; "));
    let kid: Uuid = sqlx::query_scalar(
        "INSERT INTO campus.library_clearance (tenant_id, institution_id, membership_id, person_kind, \
         person_id, subject_ref, purpose, verification, process_instance_id, status_reason) VALUES \
         ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(m.institution_id)
    .bind(m.id)
    .bind(m.person_kind.as_str())
    .bind(m.person_id)
    .bind(m.subject_ref.as_deref())
    .bind(purpose)
    .bind(serde_json::to_value(&check).map_err(KernelError::internal)?)
    .bind(&pid)
    .bind(pending.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    transition(
        &mut tx,
        scope,
        &Transition {
            aggregate_type: "library_clearance",
            id: kid,
            institution,
            from: None,
            to: "requested",
            version: 1,
            reason: Some(purpose),
            process_instance_id: Some(&pid),
            number: Some(&m.number),
            subject_ref: m.subject_ref.as_deref(),
        },
        Some((
            ev::LIBRARY_CLEARANCE_REQUESTED,
            ev::LIBRARY_CLEARANCE_REQUESTED_SCHEMA,
        )),
        "campus.library_clearance.request",
        actor,
    )
    .await?;
    if matches!(outcome, ApprovalOutcome::Approved { .. }) && check.blockers.is_empty() {
        let k = load(&mut tx, t, kid, true).await?;
        decide_in_tx(
            &mut tx,
            scope,
            &k,
            LibraryClearanceStatus::Approved,
            &check,
            None,
            AUTO_APPROVER,
            &Actor::system(actor.correlation_id.clone()),
        )
        .await?;
    }
    let out = load(&mut tx, t, kid, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

/// A clearance request by the office.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LibraryClearanceCreate {
    pub membership_id: Uuid,
    /// Why clearance is needed (programme exit, transfer, separation …).
    pub purpose: String,
}

/// Request library clearance for a member (`campus.library.manage`; Idempotency-Key;
/// `LIBRARY_CLEARANCE`). 409 while another request awaits a decision.
#[utoipa::path(post, path = "/api/v1/campus/institutions/{id}/library-clearances", operation_id = "campus_library_clearance_create", tag = "campus-library",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)),
    request_body = LibraryClearanceCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = LibraryClearanceView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn create(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LibraryClearanceCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::LibraryManage)?;
    require_idempotency_key(&headers)?;
    let purpose = required_text("purpose", &b.purpose, 200)?;
    let scope = institution_scope(ctx, InstitutionId(id));
    {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        let m = load_membership(&mut conn, ctx.scope.tenant_id, b.membership_id, false).await?;
        if m.institution_id != id {
            return Err(KernelError::NotFound {
                entity: "library membership",
            }
            .into());
        }
    }
    let k = request_clearance(&st, &scope, b.membership_id, &purpose, &actor(ctx)).await?;
    Ok(with_etag(StatusCode::CREATED, &k, Version(k.version)))
}

/// Filters of the clearance list.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct LibraryClearanceQuery {
    pub status: Option<LibraryClearanceStatus>,
}

/// Clearance requests of an institution, newest first (`campus.read`).
#[utoipa::path(get, path = "/api/v1/campus/institutions/{id}/library-clearances", operation_id = "campus_library_clearance_list", tag = "campus-library",
    params(("id" = String, Path, format = Uuid), LibraryClearanceQuery), security(("session_cookie" = [])),
    responses((status = 200, body = LibraryClearanceList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn list(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<LibraryClearanceQuery>,
) -> ApiResult<Json<LibraryClearanceList>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let institution = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, institution), institution).await?;
    let rows = sqlx::query(&format!(
        "{SELECT} WHERE k.tenant_id = $1 AND k.institution_id = $2 AND ($3::text IS NULL OR k.status = $3) \
         ORDER BY k.created_at DESC LIMIT 200"
    ))
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .bind(q.status.map(|s| s.as_str()))
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let items = rows
        .iter()
        .map(from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)?;
    Ok(Json(LibraryClearanceList { items }))
}

/// Decide a clearance request (`campus.approve`, step-up; If-Match). Approval re-verifies the
/// member: 409 while loans are out or fines are unsettled.
#[utoipa::path(post, path = "/api/v1/campus/library-clearances/{id}/decision", operation_id = "campus_library_clearance_decide", tag = "campus-library",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = LibraryDecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = LibraryClearanceView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn decide(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LibraryDecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CampusApprove)?;
    let v = expected!(headers);
    let reason = clean(b.reason);
    if b.decision == LibraryDecision::Reject && reason.is_none() {
        return Err(invalid("reason", "required", "a rejection needs a reason").into());
    }
    let t = ctx.scope.tenant_id;
    let pre = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load(&mut conn, t, id, false).await?
    };
    let institution = InstitutionId(pre.institution_id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let check = verify(&st, &scope, pre.membership_id).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let k = load(&mut tx, t, id, true).await?;
    check_version(v, k.version)?;
    if k.status != LibraryClearanceStatus::Requested {
        return Err(illegal(
            "library clearance of",
            &k.member_number,
            k.status.as_str(),
            "decided",
        )
        .into());
    }
    let to = match b.decision {
        LibraryDecision::Approve => {
            if !check.blockers.is_empty() {
                return Err(KernelError::Conflict(format!(
                    "clearance of {} is blocked: {}",
                    k.member_number,
                    check.blockers.join("; ")
                ))
                .into());
            }
            LibraryClearanceStatus::Approved
        }
        LibraryDecision::Reject => LibraryClearanceStatus::Rejected,
    };
    let decider = actor.id.clone().unwrap_or_default();
    decide_in_tx(
        &mut tx,
        &scope,
        &k,
        to,
        &check,
        reason.as_deref(),
        &decider,
        &actor,
    )
    .await?;
    let out = load(&mut tx, t, id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &out, Version(out.version)))
}

/// `true` when a membership status may still request clearance.
pub fn may_request(status: LibraryMembershipStatus) -> bool {
    status != LibraryMembershipStatus::Requested
}

pub fn router(state: CampusState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(create, list))
        .routes(routes!(decide))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_round_trips_as_json() {
        let c = LibraryClearanceCheck {
            active_loans: 1,
            unsettled_fines: "3.00".into(),
            live_reservations: 0,
            blockers: vec!["1 loan(s) not returned".into()],
        };
        let v = serde_json::to_value(&c).unwrap_or_default();
        assert_eq!(v["activeLoans"], 1);
        let back: LibraryClearanceCheck = serde_json::from_value(v).unwrap_or_default();
        assert_eq!(back.blockers.len(), 1);
        assert!(may_request(LibraryMembershipStatus::Active));
    }
}
