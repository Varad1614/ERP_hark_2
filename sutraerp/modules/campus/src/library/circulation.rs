//! Circulation (L3-G-131 issue → active borrowing `ResourceIssued`, G-132 return → closed
//! circulation, G-133 renewal → extended due date, G-137 overdue sweep → fine, G-139 lost copy →
//! replacement obligation) and reservations (G-134 reservation → waiting-list position, G-135
//! availability → collection, G-136 expiry → closure).
//!
//! Issue rules: active membership without an administrative restriction, no unsettled fines
//! (billed fines re-checked against Finance), under the policy's loan limit for the member kind,
//! a published record, and a copy that is `available` — or `reserved` for this very member (the
//! ready reservation is collected). A returned / repaired / new copy goes to the oldest queued
//! reservation first (`ready`, held for the policy's hold days).

use super::catalogue::{
    copy_by_accession, load_copy, load_record, set_copy_status, LibraryCopyView,
};
use super::common::{
    check_version, clean, expected, invalid, page_limit, required_reason, transition,
    LibraryBulkFailure, LibraryBulkResult, LibraryReasonBody, Transition,
};
use super::domain::{
    days_late, overdue_fine, replacement_charge, CatalogueStatus, LibraryCopyStatus,
    LibraryFineKind, LibraryLoanStatus, LibraryMembershipStatus, LibraryReservationStatus,
};
use super::fines::{assess, sync_billed_fines, unsettled_total, Assessment};
use super::members::{load_membership, LibraryMembershipView};
use super::policy::{active_policy, load_policy};
use crate::shared::decimal::fmt2;
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, with_etag};
use crate::shared::lifecycle::illegal;
use crate::shared::numbering::{self, kind};
use crate::shared::rows::decode;
use crate::shared::scope::institution_scope;
use crate::shared::time::business_today;
use crate::shared::{Actor, CampusState};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{Duration, NaiveDate};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
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

/// Hold days used when no policy is active (a copy can still be offered to the queue).
const DEFAULT_HOLD_DAYS: i64 = 3;

// ================================================================ loans

/// A loan (contract §5.3).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LibraryLoanView {
    pub id: Uuid,
    pub institution_id: Uuid,
    #[schema(example = "LN-000001")]
    pub number: String,
    pub copy_id: Uuid,
    pub accession_number: String,
    pub record_id: Uuid,
    pub title: String,
    /// Library membership.
    pub member_id: Uuid,
    pub member_number: String,
    pub display_name: String,
    pub person_kind: PersonKind,
    pub person_id: Uuid,
    pub issued_on: NaiveDate,
    pub due_on: NaiveDate,
    pub returned_on: Option<NaiveDate>,
    pub renewals: i32,
    pub status: LibraryLoanStatus,
    /// Overdue fine assessed on this loan, when any.
    #[schema(value_type = Option<String>, example = "6.00")]
    pub fine_amount: Option<String>,
    pub version: i64,
    #[serde(skip)]
    pub policy_id: Uuid,
    #[serde(skip)]
    pub subject_ref: Option<String>,
}

/// A page of loans.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LibraryLoanList {
    pub items: Vec<LibraryLoanView>,
    pub next_cursor: Option<Uuid>,
}

pub const LOAN_SELECT: &str = "SELECT l.id, l.institution_id, l.number, l.copy_id, c.accession_number, \
    l.record_id, r.title, l.membership_id, m.number AS member_number, m.display_name, l.person_kind, \
    l.person_id, l.subject_ref, l.issued_on, l.due_on, l.returned_on, l.renewals, l.status, l.policy_id, \
    l.version, (SELECT f.amount FROM campus.library_fine f WHERE f.loan_id = l.id AND f.kind = 'overdue') AS fine_amount \
    FROM campus.library_loan l JOIN campus.library_copy c ON c.id = l.copy_id \
    JOIN campus.catalogue_record r ON r.id = l.record_id \
    JOIN campus.library_membership m ON m.id = l.membership_id";

pub fn loan_from_row(r: &PgRow) -> Result<LibraryLoanView, sqlx::Error> {
    let fine: Option<Decimal> = r.try_get("fine_amount")?;
    Ok(LibraryLoanView {
        id: r.try_get("id")?,
        institution_id: r.try_get("institution_id")?,
        number: r.try_get("number")?,
        copy_id: r.try_get("copy_id")?,
        accession_number: r.try_get("accession_number")?,
        record_id: r.try_get("record_id")?,
        title: r.try_get("title")?,
        member_id: r.try_get("membership_id")?,
        member_number: r.try_get("member_number")?,
        display_name: r.try_get("display_name")?,
        person_kind: decode(r.try_get("person_kind")?, PersonKind::parse)?,
        person_id: r.try_get("person_id")?,
        issued_on: r.try_get("issued_on")?,
        due_on: r.try_get("due_on")?,
        returned_on: r.try_get("returned_on")?,
        renewals: r.try_get("renewals")?,
        status: decode(r.try_get("status")?, LibraryLoanStatus::parse)?,
        fine_amount: fine.map(fmt2),
        version: r.try_get("version")?,
        policy_id: r.try_get("policy_id")?,
        subject_ref: r.try_get("subject_ref")?,
    })
}

/// Loads one loan of the tenant (404), optionally locked.
pub async fn load_loan(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<LibraryLoanView, KernelError> {
    if lock {
        sqlx::query(
            "SELECT id FROM campus.library_loan WHERE id = $1 AND tenant_id = $2 FOR UPDATE",
        )
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?;
    }
    let row = sqlx::query(&format!(
        "{LOAN_SELECT} WHERE l.id = $1 AND l.tenant_id = $2"
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "library loan",
    })?;
    loan_from_row(&row).map_err(map_db_error)
}

/// Loans of a membership, newest first.
pub async fn loans_of(
    conn: &mut PgConnection,
    tenant: TenantId,
    membership: Uuid,
) -> Result<Vec<LibraryLoanView>, KernelError> {
    let rows = sqlx::query(&format!(
        "{LOAN_SELECT} WHERE l.tenant_id = $1 AND l.membership_id = $2 ORDER BY l.id DESC LIMIT 200"
    ))
    .bind(tenant.as_uuid())
    .bind(membership)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    rows.iter()
        .map(loan_from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)
}

#[allow(clippy::too_many_arguments)]
async fn set_loan_status(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    l: &LibraryLoanView,
    to: LibraryLoanStatus,
    returned_on: Option<NaiveDate>,
    reason: Option<&str>,
    event: Option<(&'static str, &'static str)>,
    action: &'static str,
    actor: &Actor,
) -> Result<(), KernelError> {
    if !l.status.can_transition(to) {
        return Err(illegal(
            "library loan",
            &l.number,
            l.status.as_str(),
            to.as_str(),
        ));
    }
    sqlx::query(
        "UPDATE campus.library_loan SET status = $2, returned_on = COALESCE($3, returned_on), \
         version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(l.id)
    .bind(to.as_str())
    .bind(returned_on)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    transition(
        conn,
        scope,
        &Transition {
            aggregate_type: "library_loan",
            id: l.id,
            institution: InstitutionId(l.institution_id),
            from: Some(l.status.as_str()),
            to: to.as_str(),
            version: l.version + 1,
            reason,
            process_instance_id: None,
            number: Some(&l.number),
            subject_ref: l.subject_ref.as_deref(),
        },
        event,
        action,
        actor,
    )
    .await
}

/// Issues `copy` to `membership` (both locked by the caller) under the active policy.
pub async fn issue_loan(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    m: &LibraryMembershipView,
    copy: &LibraryCopyView,
    today: NaiveDate,
    actor: &Actor,
) -> Result<Uuid, KernelError> {
    let tenant = scope.tenant_id;
    let institution = InstitutionId(m.institution_id);
    if copy.institution_id != m.institution_id {
        return Err(invalid(
            "copyId",
            "other_institution",
            "the copy belongs to another library",
        ));
    }
    if m.status != LibraryMembershipStatus::Active {
        return Err(KernelError::Conflict(format!(
            "membership {} is {}; only an active member borrows",
            m.number, m.status
        )));
    }
    if let Some(r) = &m.restriction_reason {
        return Err(KernelError::Conflict(format!(
            "membership {} is restricted: {r}",
            m.number
        )));
    }
    let unsettled = unsettled_total(conn, m.id).await?;
    if unsettled > Decimal::ZERO {
        return Err(KernelError::Conflict(format!(
            "membership {} has unsettled library fines of {}",
            m.number,
            fmt2(unsettled)
        )));
    }
    let policy = active_policy(conn, tenant, institution).await?;
    let rules = policy.rules(m.person_kind);
    if m.active_loans >= i64::from(rules.max_loans) {
        return Err(KernelError::Conflict(format!(
            "membership {} has reached its limit of {} loans",
            m.number, rules.max_loans
        )));
    }
    let record = load_record(conn, tenant, copy.record_id, false).await?;
    if record.status != CatalogueStatus::Published {
        return Err(KernelError::Conflict(format!(
            "catalogue record {} is {}; only published resources circulate",
            record.title, record.status
        )));
    }
    let ready: Option<(Uuid, Uuid, i64, i32)> = sqlx::query_as(
        "SELECT id, membership_id, version, queue_position FROM campus.library_reservation \
         WHERE copy_id = $1 AND status = 'ready' FOR UPDATE",
    )
    .bind(copy.id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    match copy.status {
        LibraryCopyStatus::Available => {}
        LibraryCopyStatus::Reserved => match ready {
            Some((_, owner, _, _)) if owner == m.id => {}
            _ => {
                return Err(KernelError::Conflict(format!(
                    "copy {} is held for another member's reservation",
                    copy.accession_number
                )))
            }
        },
        s => {
            return Err(illegal(
                "library copy",
                &copy.accession_number,
                s.as_str(),
                "issued",
            ))
        }
    }
    let number = numbering::next(conn, tenant, institution, kind::LIBRARY_LOAN).await?;
    let due = today + Duration::days(i64::from(rules.loan_days));
    let loan_id: Uuid = sqlx::query_scalar(
        "INSERT INTO campus.library_loan (tenant_id, institution_id, number, copy_id, record_id, \
         membership_id, policy_id, person_kind, person_id, subject_ref, issued_on, due_on) VALUES \
         ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) RETURNING id",
    )
    .bind(tenant.as_uuid())
    .bind(institution.as_uuid())
    .bind(&number)
    .bind(copy.id)
    .bind(copy.record_id)
    .bind(m.id)
    .bind(policy.id)
    .bind(m.person_kind.as_str())
    .bind(m.person_id)
    .bind(m.subject_ref.as_deref())
    .bind(today)
    .bind(due)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    if let Some((rid, _, rversion, _)) = ready.filter(|r| r.1 == m.id) {
        sqlx::query(
            "UPDATE campus.library_reservation SET status = 'collected', loan_id = $2, \
             version = version + 1, updated_at = now() WHERE id = $1",
        )
        .bind(rid)
        .bind(loan_id)
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
        transition(
            conn,
            scope,
            &Transition {
                aggregate_type: "library_reservation",
                id: rid,
                institution,
                from: Some("ready"),
                to: "collected",
                version: rversion + 1,
                reason: Some(&number),
                process_instance_id: None,
                number: None,
                subject_ref: m.subject_ref.as_deref(),
            },
            None,
            "campus.library_reservation.collect",
            actor,
        )
        .await?;
    }
    set_copy_status(
        conn,
        scope,
        copy,
        LibraryCopyStatus::Issued,
        None,
        Some(&number),
        "campus.library_copy.issue",
        actor,
    )
    .await?;
    transition(
        conn,
        scope,
        &Transition {
            aggregate_type: "library_loan",
            id: loan_id,
            institution,
            from: None,
            to: "active",
            version: 1,
            reason: Some(&record.title),
            process_instance_id: None,
            number: Some(&number),
            subject_ref: m.subject_ref.as_deref(),
        },
        Some((ev::LIBRARY_LOAN_ISSUED, ev::LIBRARY_LOAN_ISSUED_SCHEMA)),
        "campus.library_loan.issue",
        actor,
    )
    .await?;
    Ok(loan_id)
}

/// Offers a copy that just became free (locked by the caller) to the oldest queued reservation
/// of its record: the reservation becomes `ready` (held for the policy's hold days) and the copy
/// `reserved`; with no queue the copy is `available`.
pub async fn offer_to_queue(
    conn: &mut PgConnection,
    st: &CampusState,
    scope: &ExecutionScope,
    copy: &LibraryCopyView,
    actor: &Actor,
) -> Result<(), KernelError> {
    let tenant = scope.tenant_id;
    let institution = InstitutionId(copy.institution_id);
    let next: Option<(Uuid, i64, Option<String>)> = sqlx::query_as(
        "SELECT id, version, subject_ref FROM campus.library_reservation WHERE tenant_id = $1 \
         AND record_id = $2 AND status = 'queued' ORDER BY created_at, id LIMIT 1 FOR UPDATE",
    )
    .bind(tenant.as_uuid())
    .bind(copy.record_id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let Some((rid, rversion, subject)) = next else {
        if copy.status != LibraryCopyStatus::Available {
            set_copy_status(
                conn,
                scope,
                copy,
                LibraryCopyStatus::Available,
                None,
                None,
                "campus.library_copy.release",
                actor,
            )
            .await?;
        }
        return Ok(());
    };
    let today = business_today(st, scope, institution).await?;
    let hold = active_policy(conn, tenant, institution)
        .await
        .map(|p| i64::from(p.reservation_hold_days))
        .unwrap_or(DEFAULT_HOLD_DAYS);
    let hold_until = today + Duration::days(hold);
    sqlx::query(
        "UPDATE campus.library_reservation SET status = 'ready', copy_id = $2, ready_on = $3, \
         hold_until = $4, version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(rid)
    .bind(copy.id)
    .bind(today)
    .bind(hold_until)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    transition(
        conn,
        scope,
        &Transition {
            aggregate_type: "library_reservation",
            id: rid,
            institution,
            from: Some("queued"),
            to: "ready",
            version: rversion + 1,
            reason: Some(&copy.title),
            process_instance_id: None,
            number: Some(&copy.accession_number),
            subject_ref: subject.as_deref(),
        },
        Some((
            ev::LIBRARY_RESERVATION_READY,
            ev::LIBRARY_RESERVATION_READY_SCHEMA,
        )),
        "campus.library_reservation.ready",
        actor,
    )
    .await?;
    if copy.status != LibraryCopyStatus::Reserved {
        set_copy_status(
            conn,
            scope,
            copy,
            LibraryCopyStatus::Reserved,
            None,
            Some("held for a reservation"),
            "campus.library_copy.hold",
            actor,
        )
        .await?;
    }
    Ok(())
}

/// A loan to issue: the copy by id or accession number.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LibraryLoanCreate {
    pub membership_id: Uuid,
    pub copy_id: Option<Uuid>,
    #[schema(example = "ACC-000001")]
    pub accession_number: Option<String>,
}

/// Issue a copy to a member (`campus.library.manage`; Idempotency-Key; L3-G-131). 409 when the
/// member may not borrow (status, restriction, unsettled fines, limit) or the copy is not
/// available to them.
#[utoipa::path(post, path = "/api/v1/campus/institutions/{id}/library-loans", operation_id = "campus_library_loan_issue", tag = "campus-library",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)),
    request_body = LibraryLoanCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = LibraryLoanView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn issue(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LibraryLoanCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::LibraryManage)?;
    require_idempotency_key(&headers)?;
    let t = ctx.scope.tenant_id;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let copy_id = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        let m = load_membership(&mut conn, t, b.membership_id, false).await?;
        if m.institution_id != id {
            return Err(KernelError::NotFound {
                entity: "library membership",
            }
            .into());
        }
        match (b.copy_id, clean(b.accession_number)) {
            (Some(c), _) => c,
            (None, Some(acc)) => copy_by_accession(&mut conn, t, institution, &acc).await?.id,
            (None, None) => {
                return Err(invalid(
                    "copyId",
                    "required",
                    "give the copy id or its accession number",
                )
                .into())
            }
        }
    };
    sync_billed_fines(&st, &scope, b.membership_id).await?;
    let today = business_today(&st, &scope, institution).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let m = load_membership(&mut tx, t, b.membership_id, true).await?;
    let copy = load_copy(&mut tx, t, copy_id, true).await?;
    let loan_id = issue_loan(&mut tx, &scope, &m, &copy, today, &actor).await?;
    let l = load_loan(&mut tx, t, loan_id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::CREATED, &l, Version(l.version)))
}

/// Filters of the loan list.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct LibraryLoanQuery {
    pub status: Option<LibraryLoanStatus>,
    pub membership_id: Option<Uuid>,
    pub limit: Option<i64>,
    /// `nextCursor` of the previous page.
    pub cursor: Option<Uuid>,
}

/// Loans of an institution, newest first (`campus.read`; `?limit` ≤ 200, `nextCursor`).
#[utoipa::path(get, path = "/api/v1/campus/institutions/{id}/library-loans", operation_id = "campus_library_loan_list", tag = "campus-library",
    params(("id" = String, Path, format = Uuid), LibraryLoanQuery), security(("session_cookie" = [])),
    responses((status = 200, body = LibraryLoanList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn list(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<LibraryLoanQuery>,
) -> ApiResult<Json<LibraryLoanList>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let institution = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, institution), institution).await?;
    let limit = page_limit(q.limit);
    let rows = sqlx::query(&format!(
        "{LOAN_SELECT} WHERE l.tenant_id = $1 AND l.institution_id = $2 AND ($3::text IS NULL OR l.status = $3) \
         AND ($4::uuid IS NULL OR l.membership_id = $4) AND ($5::uuid IS NULL OR l.id < $5) \
         ORDER BY l.id DESC LIMIT $6"
    ))
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .bind(q.status.map(|s| s.as_str()))
    .bind(q.membership_id)
    .bind(q.cursor)
    .bind(limit)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let items = rows
        .iter()
        .map(loan_from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)?;
    let next_cursor = if i64::try_from(items.len()).unwrap_or(0) == limit {
        items.last().map(|l| l.id)
    } else {
        None
    };
    Ok(Json(LibraryLoanList { items, next_cursor }))
}

/// One loan (`campus.read`).
#[utoipa::path(get, path = "/api/v1/campus/library-loans/{id}", operation_id = "campus_library_loan_get", tag = "campus-library",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = LibraryLoanView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn get(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CampusRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let l = load_loan(&mut conn, ctx.scope.tenant_id, id, false).await?;
    Ok(with_etag(StatusCode::OK, &l, Version(l.version)))
}

/// Return a copy (`campus.library.manage`; If-Match; L3-G-132): the loan closes, a late return
/// assesses the overdue fine (days late × fine per day of the loan's policy, capped) and the copy
/// goes to the reservation queue or back on the shelf.
#[utoipa::path(post, path = "/api/v1/campus/library-loans/{id}/return", operation_id = "campus_library_loan_return", tag = "campus-library",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = LibraryReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = LibraryLoanView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn return_loan(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LibraryReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::LibraryManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let pre = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_loan(&mut conn, t, id, false).await?
    };
    let institution = InstitutionId(pre.institution_id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let today = business_today(&st, &scope, institution).await?;
    let actor = actor(ctx);
    let reason = clean(b.reason);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let l = load_loan(&mut tx, t, id, true).await?;
    check_version(v, l.version)?;
    let copy = load_copy(&mut tx, t, l.copy_id, true).await?;
    set_loan_status(
        &mut tx,
        &scope,
        &l,
        LibraryLoanStatus::Returned,
        Some(today),
        reason.as_deref(),
        Some((ev::LIBRARY_LOAN_RETURNED, ev::LIBRARY_LOAN_RETURNED_SCHEMA)),
        "campus.library_loan.return",
        &actor,
    )
    .await?;
    let late = days_late(l.due_on, today);
    if late > 0 {
        let policy = load_policy(&mut tx, t, l.policy_id, false).await?;
        let rules = policy.rules(l.person_kind);
        assess(
            &mut tx,
            &scope,
            &Assessment {
                institution,
                loan_id: l.id,
                membership_id: l.member_id,
                person_kind: l.person_kind,
                person_id: l.person_id,
                subject_ref: l.subject_ref.as_deref(),
                kind: LibraryFineKind::Overdue,
                days: late,
                amount: overdue_fine(late, rules.fine_per_day, rules.max_fine),
                on: today,
            },
            &actor,
        )
        .await?;
    }
    offer_to_queue(&mut tx, &st, &scope, &copy, &actor).await?;
    let out = load_loan(&mut tx, t, id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &out, Version(out.version)))
}

/// Renews a live loan (shared by the office and `/me/library/loans/{id}/renew`); `owner`
/// restricts it to one membership (self-service; another member's loan is 404).
pub async fn renew_loan(
    st: &CampusState,
    scope_of: impl Fn(InstitutionId) -> ExecutionScope,
    tenant: TenantId,
    id: Uuid,
    expected_version: i64,
    owner: Option<Uuid>,
    actor: &Actor,
) -> Result<LibraryLoanView, KernelError> {
    let pre = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_loan(&mut conn, tenant, id, false).await?
    };
    if owner.is_some_and(|o| o != pre.member_id) {
        return Err(KernelError::NotFound {
            entity: "library loan",
        });
    }
    let institution = InstitutionId(pre.institution_id);
    let scope = scope_of(institution);
    require_institution_writable(st, &scope, institution).await?;
    let today = business_today(st, &scope, institution).await?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let l = load_loan(&mut tx, tenant, id, true).await?;
    check_version(expected_version, l.version)?;
    if l.status != LibraryLoanStatus::Active {
        return Err(KernelError::Conflict(format!(
            "loan {} is {}; only an active loan that is not overdue can be renewed",
            l.number, l.status
        )));
    }
    let m = load_membership(&mut tx, tenant, l.member_id, false).await?;
    if m.status != LibraryMembershipStatus::Active || m.restriction_reason.is_some() {
        return Err(KernelError::Conflict(format!(
            "membership {} may not renew loans ({})",
            m.number, m.status
        )));
    }
    let policy = load_policy(&mut tx, tenant, l.policy_id, false).await?;
    let rules = policy.rules(l.person_kind);
    if l.renewals >= rules.max_renewals {
        return Err(KernelError::Conflict(format!(
            "loan {} has used all {} renewals",
            l.number, rules.max_renewals
        )));
    }
    let waiting: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM campus.library_reservation WHERE record_id = $1 \
         AND status IN ('queued', 'ready') AND membership_id <> $2",
    )
    .bind(l.record_id)
    .bind(l.member_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if waiting > 0 {
        return Err(KernelError::Conflict(format!(
            "{} is reserved by another member; the loan cannot be renewed",
            l.title
        )));
    }
    let due = l.due_on.max(today) + Duration::days(i64::from(rules.loan_days));
    sqlx::query(
        "UPDATE campus.library_loan SET due_on = $2, renewals = renewals + 1, version = version + 1, \
         updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(due)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let note = format!("due {due}");
    transition(
        &mut tx,
        &scope,
        &Transition {
            aggregate_type: "library_loan",
            id,
            institution,
            from: Some("active"),
            to: "active",
            version: l.version + 1,
            reason: Some(&note),
            process_instance_id: None,
            number: Some(&l.number),
            subject_ref: l.subject_ref.as_deref(),
        },
        Some((ev::LIBRARY_LOAN_RENEWED, ev::LIBRARY_LOAN_RENEWED_SCHEMA)),
        "campus.library_loan.renew",
        actor,
    )
    .await?;
    let out = load_loan(&mut tx, tenant, id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

/// Renew a loan (`campus.library.manage`; If-Match; L3-G-133): 409 when overdue, out of
/// renewals, the member may not borrow or another member reserved the title.
#[utoipa::path(post, path = "/api/v1/campus/library-loans/{id}/renew", operation_id = "campus_library_loan_renew", tag = "campus-library",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = LibraryReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = LibraryLoanView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn renew(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(_b): Json<LibraryReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::LibraryManage)?;
    let v = expected!(headers);
    let actor = actor(ctx);
    let l = renew_loan(
        &st,
        |i| institution_scope(ctx, i),
        ctx.scope.tenant_id,
        id,
        v,
        None,
        &actor,
    )
    .await?;
    Ok(with_etag(StatusCode::OK, &l, Version(l.version)))
}

/// Overdue sweep of one institution (bulk; Idempotency-Key; `campus.library.manage`;
/// L3-G-137): every live loan past its due date becomes `overdue` (once, `library_loan.overdue`)
/// and its fine is (re)assessed for today — idempotent per loan per business day. Ready
/// reservations past their hold expire (L3-G-136).
#[utoipa::path(post, path = "/api/v1/campus/institutions/{id}/library-loans/overdue-sweep", operation_id = "campus_library_loan_overdue_sweep", tag = "campus-library",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)),
    request_body = LibraryReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = LibraryBulkResult), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn overdue_sweep(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(_b): Json<LibraryReasonBody>,
) -> ApiResult<Json<LibraryBulkResult>> {
    let ctx = auth.require(Permission::LibraryManage)?;
    require_idempotency_key(&headers)?;
    let t = ctx.scope.tenant_id;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let today = business_today(&st, &scope, institution).await?;
    let actor = Actor::system(actor(ctx).correlation_id);
    let mut out = LibraryBulkResult::default();
    let due: Vec<(Uuid, Option<NaiveDate>)> = sqlx::query_as(
        "SELECT id, last_swept_on FROM campus.library_loan WHERE tenant_id = $1 AND institution_id = $2 \
         AND status IN ('active', 'overdue') AND due_on < $3 ORDER BY due_on, id",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(today)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    for (loan_id, swept) in due {
        if swept.is_some_and(|d| d >= today) {
            out.skipped += 1;
            continue;
        }
        match sweep_one(&st, &scope, loan_id, today, &actor).await {
            Ok(true) => out.processed += 1,
            Ok(false) => out.skipped += 1,
            Err(e) => out.failed.push(LibraryBulkFailure {
                id: loan_id,
                reason: e.to_string(),
            }),
        }
    }
    let holds: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM campus.library_reservation WHERE tenant_id = $1 AND institution_id = $2 \
         AND status = 'ready' AND hold_until < $3",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(today)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    for rid in holds {
        match close_reservation(
            &st,
            &scope,
            rid,
            None,
            None,
            LibraryReservationStatus::Expired,
            Some("hold expired".into()),
            &actor,
        )
        .await
        {
            Ok(_) => out.processed += 1,
            Err(e) => out.failed.push(LibraryBulkFailure {
                id: rid,
                reason: e.to_string(),
            }),
        }
    }
    Ok(Json(out))
}

async fn sweep_one(
    st: &CampusState,
    scope: &ExecutionScope,
    loan_id: Uuid,
    today: NaiveDate,
    actor: &Actor,
) -> Result<bool, KernelError> {
    let t = scope.tenant_id;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let l = load_loan(&mut tx, t, loan_id, true).await?;
    let swept: Option<NaiveDate> =
        sqlx::query_scalar("SELECT last_swept_on FROM campus.library_loan WHERE id = $1")
            .bind(loan_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(map_db_error)?;
    if !l.status.is_live() || l.due_on >= today || swept.is_some_and(|d| d >= today) {
        return Ok(false);
    }
    if l.status == LibraryLoanStatus::Active {
        set_loan_status(
            &mut tx,
            scope,
            &l,
            LibraryLoanStatus::Overdue,
            None,
            Some(&format!("due {}", l.due_on)),
            Some((ev::LIBRARY_LOAN_OVERDUE, ev::LIBRARY_LOAN_OVERDUE_SCHEMA)),
            "campus.library_loan.overdue",
            actor,
        )
        .await?;
    }
    let policy = load_policy(&mut tx, t, l.policy_id, false).await?;
    let rules = policy.rules(l.person_kind);
    let late = days_late(l.due_on, today);
    assess(
        &mut tx,
        scope,
        &Assessment {
            institution: InstitutionId(l.institution_id),
            loan_id: l.id,
            membership_id: l.member_id,
            person_kind: l.person_kind,
            person_id: l.person_id,
            subject_ref: l.subject_ref.as_deref(),
            kind: LibraryFineKind::Overdue,
            days: late,
            amount: overdue_fine(late, rules.fine_per_day, rules.max_fine),
            on: today,
        },
        actor,
    )
    .await?;
    sqlx::query("UPDATE campus.library_loan SET last_swept_on = $2 WHERE id = $1")
        .bind(loan_id)
        .bind(today)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(true)
}

/// Declare a copy lost (`campus.library.manage`; If-Match; reason required; L3-G-139). An issued
/// copy closes its loan as `lost`, assesses any overdue fine up to today and a replacement
/// obligation (replacement cost × the policy's lost-copy factor), billable to a student as
/// `LIB-LOST`. A shelf copy is written off without a charge; a copy held for a reservation is
/// 409 (cancel the reservation first).
#[utoipa::path(post, path = "/api/v1/campus/library-copies/{id}/lost", operation_id = "campus_library_copy_lost", tag = "campus-library",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = LibraryReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = LibraryCopyView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn lost(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LibraryReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::LibraryManage)?;
    let v = expected!(headers);
    let reason = required_reason(b.reason)?;
    let t = ctx.scope.tenant_id;
    let pre = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_copy(&mut conn, t, id, false).await?
    };
    let institution = InstitutionId(pre.institution_id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let today = business_today(&st, &scope, institution).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let copy = load_copy(&mut tx, t, id, true).await?;
    check_version(v, copy.version)?;
    match copy.status {
        LibraryCopyStatus::Issued => {
            let loan_id: Uuid = sqlx::query_scalar(
                "SELECT id FROM campus.library_loan WHERE copy_id = $1 AND status IN ('active', 'overdue')",
            )
            .bind(copy.id)
            .fetch_one(&mut *tx)
            .await
            .map_err(map_db_error)?;
            let l = load_loan(&mut tx, t, loan_id, true).await?;
            set_loan_status(
                &mut tx,
                &scope,
                &l,
                LibraryLoanStatus::Lost,
                None,
                Some(&reason),
                None,
                "campus.library_loan.lost",
                &actor,
            )
            .await?;
            let policy = load_policy(&mut tx, t, l.policy_id, false).await?;
            let rules = policy.rules(l.person_kind);
            let record = load_record(&mut tx, t, copy.record_id, false).await?;
            let late = days_late(l.due_on, today);
            let base = Assessment {
                institution,
                loan_id: l.id,
                membership_id: l.member_id,
                person_kind: l.person_kind,
                person_id: l.person_id,
                subject_ref: l.subject_ref.as_deref(),
                kind: LibraryFineKind::Overdue,
                days: late,
                amount: overdue_fine(late, rules.fine_per_day, rules.max_fine),
                on: today,
            };
            if late > 0 {
                assess(&mut tx, &scope, &base, &actor).await?;
            }
            assess(
                &mut tx,
                &scope,
                &Assessment {
                    kind: LibraryFineKind::Replacement,
                    days: 0,
                    amount: replacement_charge(
                        record.replacement_cost_value,
                        rules.lost_charge_factor,
                    ),
                    ..base
                },
                &actor,
            )
            .await?;
        }
        LibraryCopyStatus::Reserved => {
            return Err(KernelError::Conflict(format!(
                "copy {} is held for a reservation; cancel it first",
                copy.accession_number
            ))
            .into())
        }
        _ => {}
    }
    set_copy_status(
        &mut tx,
        &scope,
        &copy,
        LibraryCopyStatus::Lost,
        None,
        Some(&reason),
        "campus.library_copy.lost",
        &actor,
    )
    .await?;
    let out = load_copy(&mut tx, t, id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &out, Version(out.version)))
}

// ================================================================ reservations

/// A reservation on a title.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LibraryReservationView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub record_id: Uuid,
    pub title: String,
    pub membership_id: Uuid,
    pub member_number: String,
    pub display_name: String,
    pub person_kind: PersonKind,
    /// Position when placed.
    pub queue_position: i32,
    /// Current place among the queued reservations of the title (queued only).
    pub current_position: Option<i64>,
    pub status: LibraryReservationStatus,
    pub status_reason: Option<String>,
    pub copy_id: Option<Uuid>,
    pub accession_number: Option<String>,
    pub ready_on: Option<NaiveDate>,
    pub hold_until: Option<NaiveDate>,
    pub loan_id: Option<Uuid>,
    pub version: i64,
    #[serde(skip)]
    pub subject_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct LibraryReservationList {
    pub items: Vec<LibraryReservationView>,
}

pub const RESERVATION_SELECT: &str = "SELECT x.id, x.institution_id, x.record_id, r.title, x.membership_id, \
    m.number AS member_number, m.display_name, x.person_kind, x.subject_ref, x.queue_position, x.status, \
    x.status_reason, x.copy_id, c.accession_number, x.ready_on, x.hold_until, x.loan_id, x.version, \
    CASE WHEN x.status = 'queued' THEN (SELECT count(*) FROM campus.library_reservation y \
    WHERE y.record_id = x.record_id AND y.status = 'queued' AND (y.created_at, y.id) <= (x.created_at, x.id)) END AS current_position \
    FROM campus.library_reservation x JOIN campus.catalogue_record r ON r.id = x.record_id \
    JOIN campus.library_membership m ON m.id = x.membership_id \
    LEFT JOIN campus.library_copy c ON c.id = x.copy_id";

pub fn reservation_from_row(r: &PgRow) -> Result<LibraryReservationView, sqlx::Error> {
    Ok(LibraryReservationView {
        id: r.try_get("id")?,
        institution_id: r.try_get("institution_id")?,
        record_id: r.try_get("record_id")?,
        title: r.try_get("title")?,
        membership_id: r.try_get("membership_id")?,
        member_number: r.try_get("member_number")?,
        display_name: r.try_get("display_name")?,
        person_kind: decode(r.try_get("person_kind")?, PersonKind::parse)?,
        queue_position: r.try_get("queue_position")?,
        current_position: r.try_get("current_position")?,
        status: decode(r.try_get("status")?, LibraryReservationStatus::parse)?,
        status_reason: r.try_get("status_reason")?,
        copy_id: r.try_get("copy_id")?,
        accession_number: r.try_get("accession_number")?,
        ready_on: r.try_get("ready_on")?,
        hold_until: r.try_get("hold_until")?,
        loan_id: r.try_get("loan_id")?,
        version: r.try_get("version")?,
        subject_ref: r.try_get("subject_ref")?,
    })
}

/// Loads one reservation of the tenant (404), optionally locked.
pub async fn load_reservation(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<LibraryReservationView, KernelError> {
    if lock {
        sqlx::query(
            "SELECT id FROM campus.library_reservation WHERE id = $1 AND tenant_id = $2 FOR UPDATE",
        )
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?;
    }
    let row = sqlx::query(&format!(
        "{RESERVATION_SELECT} WHERE x.id = $1 AND x.tenant_id = $2"
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "library reservation",
    })?;
    reservation_from_row(&row).map_err(map_db_error)
}

/// Reservations matching the filters, oldest first.
pub async fn reservations_where(
    conn: &mut PgConnection,
    tenant: TenantId,
    institution: Option<Uuid>,
    record: Option<Uuid>,
    membership: Option<Uuid>,
    status: Option<LibraryReservationStatus>,
) -> Result<Vec<LibraryReservationView>, KernelError> {
    let rows = sqlx::query(&format!(
        "{RESERVATION_SELECT} WHERE x.tenant_id = $1 AND ($2::uuid IS NULL OR x.institution_id = $2) \
         AND ($3::uuid IS NULL OR x.record_id = $3) AND ($4::uuid IS NULL OR x.membership_id = $4) \
         AND ($5::text IS NULL OR x.status = $5) ORDER BY x.created_at, x.id LIMIT 200"
    ))
    .bind(tenant.as_uuid())
    .bind(institution)
    .bind(record)
    .bind(membership)
    .bind(status.map(|s| s.as_str()))
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    rows.iter()
        .map(reservation_from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)
}

/// Places a reservation of `record_id` for the (locked) member (L3-G-134).
pub async fn place_reservation(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    m: &LibraryMembershipView,
    record_id: Uuid,
    actor: &Actor,
) -> Result<Uuid, KernelError> {
    let tenant = scope.tenant_id;
    let record = load_record(conn, tenant, record_id, true).await?;
    if record.institution_id != m.institution_id {
        return Err(KernelError::NotFound {
            entity: "catalogue record",
        });
    }
    if m.status != LibraryMembershipStatus::Active || m.restriction_reason.is_some() {
        return Err(KernelError::Conflict(format!(
            "membership {} may not reserve ({})",
            m.number, m.status
        )));
    }
    if record.status != CatalogueStatus::Published {
        return Err(KernelError::Conflict(format!(
            "catalogue record {} is {}",
            record.title, record.status
        )));
    }
    if record.available_copies > 0 {
        return Err(KernelError::Conflict(format!(
            "a copy of {} is on the shelf; issue it instead of reserving",
            record.title
        )));
    }
    if record.copies == 0 {
        return Err(KernelError::Conflict(format!(
            "{} has no copies in circulation",
            record.title
        )));
    }
    let holding: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM campus.library_loan WHERE membership_id = $1 AND record_id = $2 \
         AND status IN ('active', 'overdue'))",
    )
    .bind(m.id)
    .bind(record_id)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    if holding {
        return Err(KernelError::Conflict(format!(
            "membership {} already has {} on loan",
            m.number, record.title
        )));
    }
    let position = i32::try_from(record.queued_reservations + 1).unwrap_or(i32::MAX);
    let rid: Uuid = sqlx::query_scalar(
        "INSERT INTO campus.library_reservation (tenant_id, institution_id, record_id, membership_id, \
         person_kind, person_id, subject_ref, queue_position) VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
         RETURNING id",
    )
    .bind(tenant.as_uuid())
    .bind(m.institution_id)
    .bind(record_id)
    .bind(m.id)
    .bind(m.person_kind.as_str())
    .bind(m.person_id)
    .bind(m.subject_ref.as_deref())
    .bind(position)
    .fetch_one(&mut *conn)
    .await
    .map_err(|e| match map_db_error(e) {
        KernelError::Conflict(_) => KernelError::Conflict(format!(
            "membership {} already reserved {}",
            m.number, record.title
        )),
        other => other,
    })?;
    transition(
        conn,
        scope,
        &Transition {
            aggregate_type: "library_reservation",
            id: rid,
            institution: InstitutionId(m.institution_id),
            from: None,
            to: "queued",
            version: 1,
            reason: Some(&record.title),
            process_instance_id: None,
            number: None,
            subject_ref: m.subject_ref.as_deref(),
        },
        None,
        "campus.library_reservation.place",
        actor,
    )
    .await?;
    Ok(rid)
}

/// A reservation placed by the office.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LibraryReservationCreate {
    pub membership_id: Uuid,
}

/// Reserve a title for a member (`campus.library.manage`; Idempotency-Key; L3-G-134): queued at
/// the end of the waiting list. 409 when a copy is on the shelf or the member already reserved /
/// borrowed it.
#[utoipa::path(post, path = "/api/v1/campus/catalogue-records/{id}/reservations", operation_id = "campus_library_reservation_create", tag = "campus-library",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)),
    request_body = LibraryReservationCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = LibraryReservationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn reserve(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LibraryReservationCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::LibraryManage)?;
    require_idempotency_key(&headers)?;
    let t = ctx.scope.tenant_id;
    let pre = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_membership(&mut conn, t, b.membership_id, false).await?
    };
    let institution = InstitutionId(pre.institution_id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let m = load_membership(&mut tx, t, b.membership_id, true).await?;
    let rid = place_reservation(&mut tx, &scope, &m, id, &actor).await?;
    let r = load_reservation(&mut tx, t, rid, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::CREATED, &r, Version(r.version)))
}

/// The waiting list of a title (`campus.read`).
#[utoipa::path(get, path = "/api/v1/campus/catalogue-records/{id}/reservations", operation_id = "campus_library_reservation_list", tag = "campus-library",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = LibraryReservationList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn reservations_of_record(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<LibraryReservationList>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let items =
        reservations_where(&mut conn, ctx.scope.tenant_id, None, Some(id), None, None).await?;
    Ok(Json(LibraryReservationList { items }))
}

/// Filters of the institution reservation list.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct LibraryReservationQuery {
    pub status: Option<LibraryReservationStatus>,
}

/// Reservations of an institution (`campus.read`).
#[utoipa::path(get, path = "/api/v1/campus/institutions/{id}/library-reservations", operation_id = "campus_library_reservation_institution_list", tag = "campus-library",
    params(("id" = String, Path, format = Uuid), LibraryReservationQuery), security(("session_cookie" = [])),
    responses((status = 200, body = LibraryReservationList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn reservations_of_institution(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<LibraryReservationQuery>,
) -> ApiResult<Json<LibraryReservationList>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let items = reservations_where(
        &mut conn,
        ctx.scope.tenant_id,
        Some(id),
        None,
        None,
        q.status,
    )
    .await?;
    Ok(Json(LibraryReservationList { items }))
}

/// Closes a reservation (`cancelled` from queued / ready, `expired` from ready past its hold);
/// a copy that was held goes to the next in the queue or back on the shelf. `owner` restricts it
/// to one membership (self-service).
#[allow(clippy::too_many_arguments)]
pub async fn close_reservation(
    st: &CampusState,
    scope: &ExecutionScope,
    id: Uuid,
    expected_version: Option<i64>,
    owner: Option<Uuid>,
    to: LibraryReservationStatus,
    reason: Option<String>,
    actor: &Actor,
) -> Result<LibraryReservationView, KernelError> {
    let t = scope.tenant_id;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let r = load_reservation(&mut tx, t, id, true).await?;
    let today = business_today(st, scope, InstitutionId(r.institution_id)).await?;
    if owner.is_some_and(|o| o != r.membership_id) {
        return Err(KernelError::NotFound {
            entity: "library reservation",
        });
    }
    if let Some(v) = expected_version {
        check_version(v, r.version)?;
    }
    if !r.status.can_transition(to) {
        return Err(illegal(
            "library reservation of",
            &r.member_number,
            r.status.as_str(),
            to.as_str(),
        ));
    }
    if to == LibraryReservationStatus::Expired && r.hold_until.is_some_and(|h| h >= today) {
        return Err(KernelError::Conflict(format!(
            "the hold of this reservation runs until {}",
            r.hold_until.unwrap_or(today)
        )));
    }
    sqlx::query(
        "UPDATE campus.library_reservation SET status = $2, status_reason = $3, version = version + 1, \
         updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(to.as_str())
    .bind(reason.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    transition(
        &mut tx,
        scope,
        &Transition {
            aggregate_type: "library_reservation",
            id,
            institution: InstitutionId(r.institution_id),
            from: Some(r.status.as_str()),
            to: to.as_str(),
            version: r.version + 1,
            reason: reason.as_deref(),
            process_instance_id: None,
            number: r.accession_number.as_deref(),
            subject_ref: r.subject_ref.as_deref(),
        },
        None,
        if to == LibraryReservationStatus::Expired {
            "campus.library_reservation.expire"
        } else {
            "campus.library_reservation.cancel"
        },
        actor,
    )
    .await?;
    if let (LibraryReservationStatus::Ready, Some(copy_id)) = (r.status, r.copy_id) {
        let copy = load_copy(&mut tx, t, copy_id, true).await?;
        if copy.status == LibraryCopyStatus::Reserved {
            offer_to_queue(&mut tx, st, scope, &copy, actor).await?;
        }
    }
    let out = load_reservation(&mut tx, t, id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

async fn reservation_scope(
    st: &CampusState,
    ctx: &sutra_kernel::auth::AuthContext,
    id: Uuid,
) -> Result<ExecutionScope, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let r = load_reservation(&mut conn, ctx.scope.tenant_id, id, false).await?;
    let institution = InstitutionId(r.institution_id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(st, &scope, institution).await?;
    Ok(scope)
}

/// Issue the held copy to the member (`campus.library.manage`; If-Match; L3-G-135).
#[utoipa::path(post, path = "/api/v1/campus/library-reservations/{id}/collect", operation_id = "campus_library_reservation_collect", tag = "campus-library",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = LibraryReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = LibraryReservationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn collect(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(_b): Json<LibraryReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::LibraryManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let scope = reservation_scope(&st, ctx, id).await?;
    let pre = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_reservation(&mut conn, t, id, false).await?
    };
    sync_billed_fines(&st, &scope, pre.membership_id).await?;
    let today = business_today(&st, &scope, InstitutionId(pre.institution_id)).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let r = load_reservation(&mut tx, t, id, true).await?;
    check_version(v, r.version)?;
    let Some(copy_id) = r
        .copy_id
        .filter(|_| r.status == LibraryReservationStatus::Ready)
    else {
        return Err(illegal(
            "library reservation of",
            &r.member_number,
            r.status.as_str(),
            "collected",
        )
        .into());
    };
    let m = load_membership(&mut tx, t, r.membership_id, true).await?;
    let copy = load_copy(&mut tx, t, copy_id, true).await?;
    issue_loan(&mut tx, &scope, &m, &copy, today, &actor).await?;
    let out = load_reservation(&mut tx, t, id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &out, Version(out.version)))
}

/// Cancel a queued or ready reservation (`campus.library.manage`; If-Match).
#[utoipa::path(post, path = "/api/v1/campus/library-reservations/{id}/cancel", operation_id = "campus_library_reservation_cancel", tag = "campus-library",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = LibraryReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = LibraryReservationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn cancel(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LibraryReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::LibraryManage)?;
    let v = expected!(headers);
    let scope = reservation_scope(&st, ctx, id).await?;
    let r = close_reservation(
        &st,
        &scope,
        id,
        Some(v),
        None,
        LibraryReservationStatus::Cancelled,
        clean(b.reason),
        &actor(ctx),
    )
    .await?;
    Ok(with_etag(StatusCode::OK, &r, Version(r.version)))
}

/// Expire a ready reservation whose hold has passed (`campus.library.manage`; If-Match;
/// L3-G-136). 409 while the hold still runs.
#[utoipa::path(post, path = "/api/v1/campus/library-reservations/{id}/expire", operation_id = "campus_library_reservation_expire", tag = "campus-library",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = LibraryReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = LibraryReservationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn expire(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LibraryReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::LibraryManage)?;
    let v = expected!(headers);
    let scope = reservation_scope(&st, ctx, id).await?;
    let r = close_reservation(
        &st,
        &scope,
        id,
        Some(v),
        None,
        LibraryReservationStatus::Expired,
        clean(b.reason).or_else(|| Some("hold expired".into())),
        &actor(ctx),
    )
    .await?;
    Ok(with_etag(StatusCode::OK, &r, Version(r.version)))
}

pub fn router(state: CampusState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(issue, list))
        .routes(routes!(get))
        .routes(routes!(return_loan))
        .routes(routes!(renew))
        .routes(routes!(overdue_sweep))
        .routes(routes!(lost))
        .routes(routes!(reserve, reservations_of_record))
        .routes(routes!(reservations_of_institution))
        .routes(routes!(collect))
        .routes(routes!(cancel))
        .routes(routes!(expire))
        .with_state(state)
}
