//! Library fines (L3-G-137 overdue → fine, G-138 waiver, G-139 lost copy → replacement
//! obligation). Overdue fines are assessed by the overdue sweep and on return (`days × fine per
//! day`, capped); a student's fine is **billed through `CampusChargePort`** (`LIB-FINE` /
//! `LIB-LOST`, category `library`, idempotent on the fine id) and is settled when Finance shows
//! the charge with nothing outstanding. A waiver (`FINE_WAIVER`) happens before billing. Staff
//! fines are recorded but not billed in Batch 8.
//!
//! Part 1 (DEF-G-002): a waiver is a **request** — `campus.library.manage` only asks for it
//! (`waiver_requested`); it is decided by a different user holding `library.fine.waive.approve`
//! (finance checker / administrator) and never by whoever assessed the fine or asked for the
//! waiver (`403`). There is no amount below which a librarian may waive alone: every waiver
//! needs the decision. Request and decision are both audited (`campus.library_fine.*`).

use super::common::{
    check_version, clean, expected, invalid, page_limit, required_reason, transition,
    LibraryDecision, LibraryDecisionBody, LibraryReasonBody, Transition,
};
use super::domain::{LibraryFineKind, LibraryFineStatus};
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::decimal::fmt2;
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, with_etag};
use crate::shared::lifecycle::illegal;
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
use std::collections::BTreeMap;
use sutra_contracts::campus::events::library as ev;
use sutra_contracts::campus::status::PersonKind;
use sutra_contracts::finance::ids::ChargeId;
use sutra_contracts::finance::ports::{CampusChargeCategory, CampusChargeRequest};
use sutra_kernel::auth::Permission;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// Fee codes of library charges (contract §7).
pub const FEE_FINE: &str = "LIB-FINE";
pub const FEE_LOST: &str = "LIB-LOST";

/// A library fine (money as strings).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LibraryFineView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub loan_id: Option<Uuid>,
    pub loan_number: Option<String>,
    pub membership_id: Uuid,
    pub member_number: String,
    pub display_name: String,
    pub person_kind: PersonKind,
    pub person_id: Uuid,
    pub kind: LibraryFineKind,
    pub days: i32,
    #[schema(value_type = String, example = "30.00")]
    pub amount: String,
    pub status: LibraryFineStatus,
    pub status_reason: Option<String>,
    pub assessed_on: NaiveDate,
    pub charge_id: Option<Uuid>,
    #[schema(example = "CHG-000123")]
    pub charge_number: Option<String>,
    pub process_instance_id: Option<String>,
    pub version: i64,
    #[serde(skip)]
    pub amount_value: Decimal,
    #[serde(skip)]
    pub subject_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct LibraryFineList {
    pub items: Vec<LibraryFineView>,
}

pub const SELECT: &str = "SELECT f.id, f.institution_id, f.loan_id, l.number AS loan_number, \
    f.membership_id, m.number AS member_number, m.display_name, f.person_kind, f.person_id, \
    f.subject_ref, f.kind, f.days, f.amount, f.status, f.status_reason, f.assessed_on, f.charge_id, \
    f.charge_number, f.process_instance_id, f.version FROM campus.library_fine f \
    JOIN campus.library_membership m ON m.id = f.membership_id \
    LEFT JOIN campus.library_loan l ON l.id = f.loan_id";

pub fn from_row(r: &PgRow) -> Result<LibraryFineView, sqlx::Error> {
    let amount: Decimal = r.try_get("amount")?;
    Ok(LibraryFineView {
        id: r.try_get("id")?,
        institution_id: r.try_get("institution_id")?,
        loan_id: r.try_get("loan_id")?,
        loan_number: r.try_get("loan_number")?,
        membership_id: r.try_get("membership_id")?,
        member_number: r.try_get("member_number")?,
        display_name: r.try_get("display_name")?,
        person_kind: decode(r.try_get("person_kind")?, PersonKind::parse)?,
        person_id: r.try_get("person_id")?,
        kind: decode(r.try_get("kind")?, LibraryFineKind::parse)?,
        days: r.try_get("days")?,
        amount: fmt2(amount),
        status: decode(r.try_get("status")?, LibraryFineStatus::parse)?,
        status_reason: r.try_get("status_reason")?,
        assessed_on: r.try_get("assessed_on")?,
        charge_id: r.try_get("charge_id")?,
        charge_number: r.try_get("charge_number")?,
        process_instance_id: r.try_get("process_instance_id")?,
        version: r.try_get("version")?,
        amount_value: amount,
        subject_ref: r.try_get("subject_ref")?,
    })
}

/// Loads one fine of the tenant (404), optionally locked.
pub async fn load_fine(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<LibraryFineView, KernelError> {
    if lock {
        sqlx::query(
            "SELECT id FROM campus.library_fine WHERE id = $1 AND tenant_id = $2 FOR UPDATE",
        )
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?;
    }
    let row = sqlx::query(&format!("{SELECT} WHERE f.id = $1 AND f.tenant_id = $2"))
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "library fine",
        })?;
    from_row(&row).map_err(map_db_error)
}

/// Fines of a membership, newest first.
pub async fn fines_of(
    conn: &mut PgConnection,
    tenant: TenantId,
    membership: Uuid,
) -> Result<Vec<LibraryFineView>, KernelError> {
    let rows = sqlx::query(&format!(
        "{SELECT} WHERE f.tenant_id = $1 AND f.membership_id = $2 ORDER BY f.created_at DESC"
    ))
    .bind(tenant.as_uuid())
    .bind(membership)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    rows.iter()
        .map(from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)
}

/// Sum of a member's unsettled fines (assessed, waiver requested, billed and not yet paid).
pub async fn unsettled_total(
    conn: &mut PgConnection,
    membership: Uuid,
) -> Result<Decimal, KernelError> {
    sqlx::query_scalar(
        "SELECT COALESCE(sum(amount), 0) FROM campus.library_fine WHERE membership_id = $1 \
         AND status IN ('assessed', 'waiver_requested', 'billed')",
    )
    .bind(membership)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)
}

/// Marks a member's billed fines `paid` once Finance shows their charges with nothing
/// outstanding (read through `ChargeDirectory`; L3-G-149 consumes F-owned settlement).
pub async fn sync_billed_fines(
    st: &CampusState,
    scope: &ExecutionScope,
    membership: Uuid,
) -> Result<(), KernelError> {
    sync_billed(st, scope, Some(membership), None).await
}

async fn sync_billed(
    st: &CampusState,
    scope: &ExecutionScope,
    membership: Option<Uuid>,
    institution: Option<InstitutionId>,
) -> Result<(), KernelError> {
    let tenant = scope.tenant_id;
    let billed: Vec<(Uuid, Uuid, Uuid)> = sqlx::query_as(
        "SELECT id, charge_id, institution_id FROM campus.library_fine WHERE tenant_id = $1 \
         AND status = 'billed' AND charge_id IS NOT NULL AND ($2::uuid IS NULL OR membership_id = $2) \
         AND ($3::uuid IS NULL OR institution_id = $3) ORDER BY created_at LIMIT 200",
    )
    .bind(tenant.as_uuid())
    .bind(membership)
    .bind(institution.map(|i| i.as_uuid()))
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    for (fine_id, charge_id, inst) in billed {
        let Some(charge) = st.finance.charge(scope, ChargeId(charge_id)).await? else {
            continue;
        };
        if charge.outstanding_amount > Decimal::ZERO {
            continue;
        }
        let inst_scope = ExecutionScope::institution(tenant, InstitutionId(inst));
        let actor = Actor::system(None);
        let mut tx = st.pool.begin().await.map_err(map_db_error)?;
        let f = load_fine(&mut tx, tenant, fine_id, true).await?;
        if f.status == LibraryFineStatus::Billed {
            sqlx::query(
                "UPDATE campus.library_fine SET status = 'paid', status_reason = $2, \
                 version = version + 1, updated_at = now() WHERE id = $1",
            )
            .bind(fine_id)
            .bind(format!("charge {} settled", charge.charge_number))
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
            transition(
                &mut tx,
                &inst_scope,
                &Transition {
                    aggregate_type: "library_fine",
                    id: fine_id,
                    institution: InstitutionId(inst),
                    from: Some("billed"),
                    to: "paid",
                    version: f.version + 1,
                    reason: Some("charge settled in Finance"),
                    process_instance_id: None,
                    number: f.charge_number.as_deref(),
                    subject_ref: f.subject_ref.as_deref(),
                },
                None,
                "campus.library_fine.settle",
                &actor,
            )
            .await?;
        }
        tx.commit().await.map_err(map_db_error)?;
    }
    Ok(())
}

/// What [`assess`] records.
pub struct Assessment<'a> {
    pub institution: InstitutionId,
    pub loan_id: Uuid,
    pub membership_id: Uuid,
    pub person_kind: PersonKind,
    pub person_id: Uuid,
    pub subject_ref: Option<&'a str>,
    pub kind: LibraryFineKind,
    pub days: i64,
    pub amount: Decimal,
    pub on: NaiveDate,
}

/// Assesses (or re-assesses, while still `assessed`) the fine of a loan: one per loan and kind.
/// Returns the fine id when a row was written.
pub async fn assess(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    a: &Assessment<'_>,
    actor: &Actor,
) -> Result<Option<Uuid>, KernelError> {
    let days = i32::try_from(a.days.max(0)).unwrap_or(i32::MAX);
    let row: Option<(Uuid, bool)> = sqlx::query_as(
        "INSERT INTO campus.library_fine (tenant_id, institution_id, loan_id, membership_id, \
         person_kind, person_id, subject_ref, kind, days, amount, assessed_on) VALUES ($1, $2, $3, \
         $4, $5, $6, $7, $8, $9, $10, $11) \
         ON CONFLICT (loan_id, kind) WHERE loan_id IS NOT NULL DO UPDATE SET days = EXCLUDED.days, \
         amount = EXCLUDED.amount, assessed_on = EXCLUDED.assessed_on, \
         version = campus.library_fine.version + 1, updated_at = now() \
         WHERE campus.library_fine.status = 'assessed' RETURNING id, (xmax = 0) AS inserted",
    )
    .bind(scope.tenant_id.as_uuid())
    .bind(a.institution.as_uuid())
    .bind(a.loan_id)
    .bind(a.membership_id)
    .bind(a.person_kind.as_str())
    .bind(a.person_id)
    .bind(a.subject_ref)
    .bind(a.kind.as_str())
    .bind(days)
    .bind(a.amount)
    .bind(a.on)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let Some((id, inserted)) = row else {
        return Ok(None);
    };
    if inserted {
        transition(
            conn,
            scope,
            &Transition {
                aggregate_type: "library_fine",
                id,
                institution: a.institution,
                from: None,
                to: "assessed",
                version: 1,
                reason: Some(a.kind.as_str()),
                process_instance_id: None,
                number: None,
                subject_ref: a.subject_ref,
            },
            None,
            "campus.library_fine.assess",
            actor,
        )
        .await?;
    }
    Ok(Some(id))
}

async fn loan_is_live(conn: &mut PgConnection, loan: Option<Uuid>) -> Result<bool, KernelError> {
    let Some(loan) = loan else {
        return Ok(false);
    };
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM campus.library_loan WHERE id = $1 AND status IN ('active', 'overdue'))",
    )
    .bind(loan)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)
}

/// Filters of the fine list.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct LibraryFineQuery {
    pub status: Option<LibraryFineStatus>,
    pub membership_id: Option<Uuid>,
    pub limit: Option<i64>,
}

/// Fines of an institution, newest first (`campus.read`); billed fines are re-checked against
/// Finance first.
#[utoipa::path(get, path = "/api/v1/campus/institutions/{id}/library-fines", operation_id = "campus_library_fine_list", tag = "campus-library",
    params(("id" = String, Path, format = Uuid), LibraryFineQuery), security(("session_cookie" = [])),
    responses((status = 200, body = LibraryFineList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn list(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<LibraryFineQuery>,
) -> ApiResult<Json<LibraryFineList>> {
    let ctx = fine_reader(&auth)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution(&st, &scope, institution).await?;
    sync_billed(&st, &scope, q.membership_id, Some(institution)).await?;
    let rows = sqlx::query(&format!(
        "{SELECT} WHERE f.tenant_id = $1 AND f.institution_id = $2 AND ($3::text IS NULL OR f.status = $3) \
         AND ($4::uuid IS NULL OR f.membership_id = $4) ORDER BY f.created_at DESC LIMIT $5"
    ))
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .bind(q.status.map(|s| s.as_str()))
    .bind(q.membership_id)
    .bind(page_limit(q.limit))
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let items = rows
        .iter()
        .map(from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)?;
    Ok(Json(LibraryFineList { items }))
}

/// One fine (`campus.read`).
#[utoipa::path(get, path = "/api/v1/campus/library-fines/{id}", operation_id = "campus_library_fine_get", tag = "campus-library",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = LibraryFineView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn get(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = fine_reader(&auth)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let f = load_fine(&mut conn, ctx.scope.tenant_id, id, false).await?;
    Ok(with_etag(StatusCode::OK, &f, Version(f.version)))
}

/// Fines are read with `campus.read`, or by a waiver checker (`library.fine.waive.approve`) who
/// must see the fine they decide (Part 1 DEF-G-002).
fn fine_reader(
    auth: &Authenticated,
) -> Result<&sutra_kernel::auth::AuthContext, sutra_web::ApiError> {
    if auth.0.has_permission(Permission::LibraryFineWaiveApprove)
        && !auth.0.has_permission(Permission::CampusRead)
    {
        return auth.require(Permission::LibraryFineWaiveApprove);
    }
    auth.require(Permission::CampusRead)
}

#[allow(clippy::too_many_arguments)]
async fn set_status(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    f: &LibraryFineView,
    to: LibraryFineStatus,
    reason: Option<&str>,
    pid: Option<&str>,
    event: Option<(&'static str, &'static str)>,
    action: &'static str,
    actor: &Actor,
) -> Result<LibraryFineView, KernelError> {
    if !f.status.can_transition(to) {
        return Err(illegal(
            "library fine",
            &f.id.to_string(),
            f.status.as_str(),
            to.as_str(),
        ));
    }
    sqlx::query(
        "UPDATE campus.library_fine SET status = $2, status_reason = COALESCE($3, status_reason), \
         process_instance_id = COALESCE($4, process_instance_id), version = version + 1, \
         updated_at = now() WHERE id = $1",
    )
    .bind(f.id)
    .bind(to.as_str())
    .bind(reason)
    .bind(pid)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    transition(
        conn,
        scope,
        &Transition {
            aggregate_type: "library_fine",
            id: f.id,
            institution: InstitutionId(f.institution_id),
            from: Some(f.status.as_str()),
            to: to.as_str(),
            version: f.version + 1,
            reason,
            process_instance_id: pid.or(f.process_instance_id.as_deref()),
            number: f.loan_number.as_deref(),
            subject_ref: f.subject_ref.as_deref(),
        },
        event,
        action,
        actor,
    )
    .await?;
    load_fine(conn, scope.tenant_id, f.id, true).await
}

/// Request a waiver of an assessed fine (`campus.library.manage`; If-Match; reason required;
/// `FINE_WAIVER`). Only before billing and once the loan is closed (the amount is final). Part 1
/// (DEF-G-002): the fine stays `waiver_requested` until another user with
/// `library.fine.waive.approve` decides it.
#[utoipa::path(post, path = "/api/v1/campus/library-fines/{id}/waiver", operation_id = "campus_library_fine_waiver", tag = "campus-library",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = LibraryReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = LibraryFineView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn waiver(
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
        let f = load_fine(&mut conn, t, id, false).await?;
        if loan_is_live(&mut conn, f.loan_id).await? {
            return Err(KernelError::Conflict(
                "the loan is still out; a fine is waived once its amount is final".into(),
            )
            .into());
        }
        f
    };
    let institution = InstitutionId(pre.institution_id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    check_version(v, pre.version)?;
    if pre.status != LibraryFineStatus::Assessed {
        return Err(illegal(
            "library fine",
            &pre.id.to_string(),
            pre.status.as_str(),
            "waiver_requested",
        )
        .into());
    }
    let mut vars = BTreeMap::new();
    vars.insert("amount".to_owned(), serde_json::json!(pre.amount));
    vars.insert("reason".to_owned(), serde_json::json!(reason));
    let outcome = request_approval(
        &st,
        &scope,
        process::FINE_WAIVER,
        business_key("library_fine", pre.id, pre.version),
        vars,
    )
    .await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let f = load_fine(&mut tx, t, id, true).await?;
    check_version(v, f.version)?;
    let f = set_status(
        &mut tx,
        &scope,
        &f,
        LibraryFineStatus::WaiverRequested,
        Some(&reason),
        Some(outcome.process_instance_id()),
        None,
        "campus.library_fine.request_waiver",
        &actor,
    )
    .await?;
    let f = if let ApprovalOutcome::Approved { .. } = outcome {
        set_status(
            &mut tx,
            &scope,
            &f,
            LibraryFineStatus::Waived,
            Some(&reason),
            None,
            Some((ev::LIBRARY_FINE_WAIVED, ev::LIBRARY_FINE_WAIVED_SCHEMA)),
            "campus.library_fine.waive",
            &Actor::system(actor.correlation_id.clone()),
        )
        .await?
    } else {
        f
    };
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &f, Version(f.version)))
}

/// Decide a fine waiver (`library.fine.waive.approve`, step-up; If-Match; Part 1 DEF-G-002):
/// approve → `waived`, reject → back to `assessed` (billable). The decider must not be whoever
/// assessed the fine or requested the waiver (`403`, separation of duties).
#[utoipa::path(post, path = "/api/v1/campus/library-fines/{id}/decision", operation_id = "campus_library_fine_decide", tag = "campus-library",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = LibraryDecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = LibraryFineView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn decide(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LibraryDecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::LibraryFineWaiveApprove)?;
    let v = expected!(headers);
    let reason = clean(b.reason);
    if b.decision == LibraryDecision::Reject && reason.is_none() {
        return Err(invalid("reason", "required", "a rejection needs a reason").into());
    }
    let t = ctx.scope.tenant_id;
    let pre = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_fine(&mut conn, t, id, false).await?
    };
    let institution = InstitutionId(pre.institution_id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let f = load_fine(&mut tx, t, id, true).await?;
    check_version(v, f.version)?;
    if f.status != LibraryFineStatus::WaiverRequested {
        return Err(KernelError::Conflict(format!(
            "fine {} has no waiver awaiting a decision ({})",
            f.id, f.status
        ))
        .into());
    }
    // Part 1 (DEF-G-002): whoever assessed the fine or asked for the waiver never decides it.
    crate::shared::approval::ensure_independent_decider(
        &mut tx,
        t,
        "library_fine",
        f.id,
        &[LibraryFineStatus::WaiverRequested.as_str()],
        &[],
        actor.id.as_deref(),
        "library fine waiver",
    )
    .await?;
    let f = match b.decision {
        LibraryDecision::Approve => {
            set_status(
                &mut tx,
                &scope,
                &f,
                LibraryFineStatus::Waived,
                reason.as_deref(),
                None,
                Some((ev::LIBRARY_FINE_WAIVED, ev::LIBRARY_FINE_WAIVED_SCHEMA)),
                "campus.library_fine.waive",
                &actor,
            )
            .await?
        }
        LibraryDecision::Reject => {
            set_status(
                &mut tx,
                &scope,
                &f,
                LibraryFineStatus::Assessed,
                reason.as_deref(),
                None,
                None,
                "campus.library_fine.reject_waiver",
                &actor,
            )
            .await?
        }
    };
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &f, Version(f.version)))
}

/// Bill an assessed fine to the student through Finance (`campus.library.manage`; If-Match):
/// `CampusChargePort` raises and publishes the charge (`LIB-FINE` / `LIB-LOST`, category
/// `library`, idempotent on the fine id) and the fine stores `chargeId` / `chargeNumber`. 409
/// for staff (not billed in Batch 8), a zero amount or a loan still out.
#[utoipa::path(post, path = "/api/v1/campus/library-fines/{id}/bill", operation_id = "campus_library_fine_bill", tag = "campus-library",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = LibraryReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = LibraryFineView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn bill(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(_b): Json<LibraryReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::LibraryManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let pre = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        let f = load_fine(&mut conn, t, id, false).await?;
        if loan_is_live(&mut conn, f.loan_id).await? {
            return Err(KernelError::Conflict(
                "the loan is still out; a fine is billed once its amount is final".into(),
            )
            .into());
        }
        f
    };
    let institution = InstitutionId(pre.institution_id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    check_version(v, pre.version)?;
    if pre.status != LibraryFineStatus::Assessed {
        return Err(illegal(
            "library fine",
            &pre.id.to_string(),
            pre.status.as_str(),
            "billed",
        )
        .into());
    }
    if pre.person_kind != PersonKind::Student {
        return Err(KernelError::Conflict(
            "only students are billed through Finance; staff fines stay on the library record"
                .into(),
        )
        .into());
    }
    if pre.amount_value <= Decimal::ZERO {
        return Err(KernelError::Conflict("a zero fine is not billed".into()).into());
    }
    let today = business_today(&st, &scope, institution).await?;
    let (fee, description) = match pre.kind {
        LibraryFineKind::Overdue => (
            FEE_FINE,
            format!(
                "Library overdue fine {} ({} days)",
                pre.loan_number.as_deref().unwrap_or("-"),
                pre.days
            ),
        ),
        LibraryFineKind::Replacement => (
            FEE_LOST,
            format!(
                "Library lost copy replacement {}",
                pre.loan_number.as_deref().unwrap_or("-")
            ),
        ),
    };
    let actor = actor(ctx);
    let charge = st
        .charges
        .raise_campus_charge(
            &scope,
            CampusChargeRequest {
                institution_id: institution,
                student_id: pre.person_id,
                fee_code: fee.to_owned(),
                category: CampusChargeCategory::Library,
                description,
                amount: pre.amount_value,
                due_date: Some(today + Duration::days(15)),
                source_id: pre.id,
                actor: actor.id.clone(),
            },
        )
        .await?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let f = load_fine(&mut tx, t, id, true).await?;
    check_version(v, f.version)?;
    if f.status != LibraryFineStatus::Assessed {
        return Err(illegal(
            "library fine",
            &f.id.to_string(),
            f.status.as_str(),
            "billed",
        )
        .into());
    }
    sqlx::query("UPDATE campus.library_fine SET charge_id = $2, charge_number = $3 WHERE id = $1")
        .bind(f.id)
        .bind(charge.charge_id.as_uuid())
        .bind(&charge.charge_number)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let f = set_status(
        &mut tx,
        &scope,
        &f,
        LibraryFineStatus::Billed,
        Some(&format!("charge {}", charge.charge_number)),
        None,
        Some((ev::LIBRARY_FINE_RAISED, ev::LIBRARY_FINE_RAISED_SCHEMA)),
        "campus.library_fine.bill",
        &actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &f, Version(f.version)))
}

pub fn router(state: CampusState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(list))
        .routes(routes!(get))
        .routes(routes!(waiver))
        .routes(routes!(decide))
        .routes(routes!(bill))
        .with_state(state)
}
