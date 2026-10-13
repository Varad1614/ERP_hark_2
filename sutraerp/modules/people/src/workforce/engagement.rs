//! Engagements (L3-E-001 … 004, 007, 008, 018, 066): creation through the shared inserter,
//! governed activation (`ENGAGEMENT_ACTIVATION`), modification and transfer (`ENGAGEMENT_CHANGE`)
//! as change requests applied on approval, suspension / restoration, closure (exit), rejoining,
//! cancellation, reporting manager; workforce changes are synchronised into the profile.

use super::common::{
    self, check_campus, check_org_unit, check_version, clean, emit, expected, history, invalid,
    modified, reason_of, required_reason, required_text, staff_or_404, status_payload, subject_of,
    valid_code, Decision, DecisionBody, ReasonBody,
};
use crate::shared::approval::{
    business_key, process, request_approval, ApprovalOutcome, AUTO_APPROVER,
};
use crate::shared::guard::require_institution_writable;
use crate::shared::http::{actor, with_etag};
use crate::shared::lifecycle::{change_engagement_status, change_staff_status, StatusChange};
use crate::shared::rows::{
    current_engagement, engagement_from_row, load_engagement, load_staff, lock_engagement,
    lock_staff, ENGAGEMENT_COLUMNS,
};
use crate::shared::scope::institution_scope;
use crate::shared::staff::{insert_engagement, NewEngagement};
use crate::shared::time::business_today;
use crate::shared::{Actor, PeopleState};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, Row};
use std::collections::BTreeMap;
use sutra_contracts::people::events::workforce as ev;
use sutra_contracts::people::ids::{StaffEngagementId, StaffMemberId};
use sutra_contracts::people::ports::{EngagementSummary, StaffSummary};
use sutra_contracts::people::status::{
    EmploymentType, EngagementStatus, SeparationKind, StaffCategory, StaffStatus,
};
use sutra_kernel::auth::Permission;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

// ------------------------------------------------------------------ change requests

/// Kind of a governed engagement change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Activation,
    Modification,
    Transfer,
}

impl ChangeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Activation => "activation",
            Self::Modification => "modification",
            Self::Transfer => "transfer",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        [Self::Activation, Self::Modification, Self::Transfer]
            .into_iter()
            .find(|k| k.as_str() == s)
    }
}

/// Proposed engagement attributes (modification / transfer); `None` = unchanged.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EngagementChanges {
    pub designation: Option<String>,
    pub grade: Option<String>,
    pub employment_type: Option<EmploymentType>,
    pub staff_category: Option<StaffCategory>,
    pub end_date: Option<NaiveDate>,
    pub probation_end_date: Option<NaiveDate>,
    pub org_unit_id: Option<Uuid>,
    pub campus_id: Option<Uuid>,
}

/// A governed engagement change (activation, modification, transfer).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChangeRequestView {
    pub id: Uuid,
    pub kind: ChangeKind,
    pub proposal: EngagementChanges,
    pub effective_date: Option<NaiveDate>,
    pub reason: Option<String>,
    /// `pending_approval`, `approved` (activation waiting for the start date), `rejected`,
    /// `applied`, `cancelled`.
    pub status: String,
    pub decision_reason: Option<String>,
    pub decided_by: Option<String>,
    pub process_instance_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

const REQUEST_COLUMNS: &str = "id, kind, proposal, effective_date, reason, status, \
    decision_reason, decided_by, process_instance_id, created_at";

fn request_from_row(r: &sqlx::postgres::PgRow) -> Result<ChangeRequestView, sqlx::Error> {
    let kind: String = r.try_get("kind")?;
    let proposal: serde_json::Value = r.try_get("proposal")?;
    Ok(ChangeRequestView {
        id: r.try_get("id")?,
        kind: ChangeKind::parse(&kind)
            .ok_or_else(|| sqlx::Error::Decode(format!("unknown kind `{kind}`").into()))?,
        proposal: serde_json::from_value(proposal).unwrap_or_default(),
        effective_date: r.try_get("effective_date")?,
        reason: r.try_get("reason")?,
        status: r.try_get("status")?,
        decision_reason: r.try_get("decision_reason")?,
        decided_by: r.try_get("decided_by")?,
        process_instance_id: r.try_get("process_instance_id")?,
        created_at: r.try_get("created_at")?,
    })
}

/// The open (pending / approved) request of an engagement, locked.
async fn open_request(
    conn: &mut PgConnection,
    e: StaffEngagementId,
) -> Result<Option<ChangeRequestView>, KernelError> {
    sqlx::query(&format!(
        "SELECT {REQUEST_COLUMNS} FROM people.engagement_change_request WHERE engagement_id = $1 \
         AND status IN ('pending_approval', 'approved') FOR UPDATE"
    ))
    .bind(e.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .as_ref()
    .map(request_from_row)
    .transpose()
    .map_err(map_db_error)
}

async fn set_request_status(
    conn: &mut PgConnection,
    id: Uuid,
    status: &str,
    decision_reason: Option<&str>,
    decided_by: Option<&str>,
) -> Result<(), KernelError> {
    sqlx::query(
        "UPDATE people.engagement_change_request SET status = $2, \
         decision_reason = COALESCE($3, decision_reason), decided_by = COALESCE($4, decided_by), \
         decided_at = CASE WHEN $4::text IS NULL THEN decided_at ELSE now() END, \
         version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(status)
    .bind(decision_reason)
    .bind(decided_by)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(())
}

// ------------------------------------------------------------------ attribute writer

/// Editable engagement attributes (7A writes them with its own repo code, contract §3.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attrs {
    pub designation: String,
    pub grade: Option<String>,
    pub employment_type: EmploymentType,
    pub staff_category: StaffCategory,
    pub end_date: Option<NaiveDate>,
    pub probation_end_date: Option<NaiveDate>,
    pub last_working_date: Option<NaiveDate>,
    pub org_unit_id: Option<Uuid>,
    pub campus_id: Option<Uuid>,
    pub reports_to: Option<StaffMemberId>,
}

impl Attrs {
    pub fn of(e: &EngagementSummary) -> Self {
        Self {
            designation: e.designation.clone(),
            grade: e.grade.clone(),
            employment_type: e.employment_type,
            staff_category: e.staff_category,
            end_date: e.end_date,
            probation_end_date: e.probation_end_date,
            last_working_date: e.last_working_date,
            org_unit_id: e.org_unit_id,
            campus_id: e.campus_id,
            reports_to: e.reports_to_staff_id,
        }
    }

    /// Applies a proposal.
    pub fn with(mut self, c: &EngagementChanges) -> Self {
        if let Some(d) = &c.designation {
            self.designation = d.clone();
        }
        if let Some(g) = &c.grade {
            self.grade = Some(g.clone());
        }
        if let Some(t) = c.employment_type {
            self.employment_type = t;
        }
        if let Some(s) = c.staff_category {
            self.staff_category = s;
        }
        if c.end_date.is_some() {
            self.end_date = c.end_date;
        }
        if c.probation_end_date.is_some() {
            self.probation_end_date = c.probation_end_date;
        }
        if c.org_unit_id.is_some() {
            self.org_unit_id = c.org_unit_id;
        }
        if c.campus_id.is_some() {
            self.campus_id = c.campus_id;
        }
        self
    }

    /// camelCase names of the members that differ (as the staff view shows them).
    pub fn changed(&self, after: &Attrs) -> Vec<&'static str> {
        let mut v = Vec::new();
        if self.designation != after.designation {
            v.push("designation");
        }
        if self.grade != after.grade {
            v.push("grade");
        }
        if self.employment_type != after.employment_type {
            v.push("employmentType");
        }
        if self.staff_category != after.staff_category {
            v.push("staffCategory");
        }
        if self.end_date != after.end_date {
            v.push("endDate");
        }
        if self.probation_end_date != after.probation_end_date {
            v.push("probationEndDate");
        }
        if self.last_working_date != after.last_working_date {
            v.push("lastWorkingDate");
        }
        if self.org_unit_id != after.org_unit_id {
            v.push("orgUnitId");
        }
        if self.campus_id != after.campus_id {
            v.push("campusId");
        }
        if self.reports_to != after.reports_to {
            v.push("reportsToStaffId");
        }
        v
    }
}

/// Writes the attributes of a locked engagement (version + 1; optional process instance).
pub async fn write_attrs(
    conn: &mut PgConnection,
    tenant: TenantId,
    e: &EngagementSummary,
    a: &Attrs,
    process_instance_id: Option<&str>,
) -> Result<EngagementSummary, KernelError> {
    let row = sqlx::query(&format!(
        "UPDATE people.staff_engagement SET designation = $3, grade = $4, employment_type = $5, \
         staff_category = $6, end_date = $7, probation_end_date = $8, last_working_date = $9, \
         org_unit_id = $10, campus_id = $11, reports_to_staff_id = $12, \
         process_instance_id = COALESCE($13, process_instance_id), version = version + 1, \
         updated_at = now() WHERE id = $1 AND tenant_id = $2 AND version = $14 \
         RETURNING {ENGAGEMENT_COLUMNS}"
    ))
    .bind(e.id.as_uuid())
    .bind(tenant.as_uuid())
    .bind(&a.designation)
    .bind(&a.grade)
    .bind(a.employment_type.as_str())
    .bind(a.staff_category.as_str())
    .bind(a.end_date)
    .bind(a.probation_end_date)
    .bind(a.last_working_date)
    .bind(a.org_unit_id)
    .bind(a.campus_id)
    .bind(a.reports_to.map(|s| s.as_uuid()))
    .bind(process_instance_id)
    .bind(e.version)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or_else(|| KernelError::Conflict("engagement changed concurrently".into()))?;
    engagement_from_row(&row).map_err(map_db_error)
}

// ------------------------------------------------------------------ views

/// An engagement with its staff member and open governed change.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EngagementRecordView {
    pub id: Uuid,
    pub staff_id: Uuid,
    pub staff_number: String,
    pub display_name: String,
    #[schema(example = "ENG-000001")]
    pub engagement_number: String,
    pub org_unit_id: Option<Uuid>,
    pub campus_id: Option<Uuid>,
    pub staff_category: StaffCategory,
    pub employment_type: EmploymentType,
    pub designation: String,
    pub grade: Option<String>,
    pub start_date: NaiveDate,
    pub end_date: Option<NaiveDate>,
    pub last_working_date: Option<NaiveDate>,
    pub probation_end_date: Option<NaiveDate>,
    pub reports_to_staff_id: Option<Uuid>,
    pub rejoin_of_engagement_id: Option<Uuid>,
    pub status: EngagementStatus,
    pub pending_change: Option<ChangeRequestView>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EngagementList {
    pub items: Vec<EngagementRecordView>,
}

/// Builds the record view of an engagement.
pub async fn engagement_view(
    conn: &mut PgConnection,
    tenant: TenantId,
    e: &EngagementSummary,
) -> Result<EngagementRecordView, KernelError> {
    let s = load_staff(conn, tenant, e.staff_id)
        .await?
        .ok_or(KernelError::NotFound {
            entity: "staff member",
        })?;
    let pending = sqlx::query(&format!(
        "SELECT {REQUEST_COLUMNS} FROM people.engagement_change_request WHERE engagement_id = $1 \
         AND status IN ('pending_approval', 'approved') LIMIT 1"
    ))
    .bind(e.id.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .as_ref()
    .map(request_from_row)
    .transpose()
    .map_err(map_db_error)?;
    Ok(EngagementRecordView {
        id: e.id.as_uuid(),
        staff_id: e.staff_id.as_uuid(),
        staff_number: s.staff_number,
        display_name: s.display_name,
        engagement_number: e.engagement_number.clone(),
        org_unit_id: e.org_unit_id,
        campus_id: e.campus_id,
        staff_category: e.staff_category,
        employment_type: e.employment_type,
        designation: e.designation.clone(),
        grade: e.grade.clone(),
        start_date: e.start_date,
        end_date: e.end_date,
        last_working_date: e.last_working_date,
        probation_end_date: e.probation_end_date,
        reports_to_staff_id: e.reports_to_staff_id.map(|m| m.as_uuid()),
        rejoin_of_engagement_id: e.rejoin_of_engagement_id.map(|r| r.as_uuid()),
        status: e.status,
        pending_change: pending,
        version: e.version,
    })
}

async fn respond(
    st: &PeopleState,
    tenant: TenantId,
    e: &EngagementSummary,
    status: StatusCode,
) -> ApiResult<Response> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let v = engagement_view(&mut conn, tenant, e).await?;
    Ok(with_etag(status, &v, Version(e.version)))
}

// ------------------------------------------------------------------ domain steps

/// Reporting-manager rules (L3-E-018): not oneself, same institution, a live record with a live
/// engagement, and no cycle up the chain. Returns the manager's login subject.
pub async fn validate_manager(
    conn: &mut PgConnection,
    tenant: TenantId,
    staff: &StaffSummary,
    manager: StaffMemberId,
) -> Result<Option<String>, KernelError> {
    if manager == staff.id {
        return Err(invalid(
            "reportsToStaffId",
            "self_reporting",
            "a staff member cannot report to themselves",
        ));
    }
    let m = load_staff(conn, tenant, manager)
        .await?
        .ok_or_else(|| invalid("reportsToStaffId", "not_found", "unknown staff record"))?;
    if m.institution_id != staff.institution_id {
        return Err(invalid(
            "reportsToStaffId",
            "other_institution",
            "the manager belongs to another institution",
        ));
    }
    if !m.status.is_live() {
        return Err(KernelError::Conflict(format!(
            "manager {} is {}",
            m.staff_number, m.status
        )));
    }
    let me = current_engagement(conn, tenant, m.id)
        .await?
        .filter(|e| e.status.is_live())
        .ok_or_else(|| {
            KernelError::Conflict(format!("manager {} has no live engagement", m.staff_number))
        })?;
    let mut cursor = me.reports_to_staff_id;
    let mut steps = 0;
    while let Some(c) = cursor {
        if c == staff.id {
            return Err(KernelError::Conflict(format!(
                "reporting cycle: {} already reports (directly or indirectly) to {}",
                m.staff_number, staff.staff_number
            )));
        }
        steps += 1;
        if steps > 64 {
            break;
        }
        cursor = current_engagement(conn, tenant, c)
            .await?
            .filter(|e| e.status.is_live())
            .and_then(|e| e.reports_to_staff_id);
    }
    Ok(m.subject_ref)
}

/// Login subject of the engagement's reporting manager.
async fn manager_subject(
    conn: &mut PgConnection,
    tenant: TenantId,
    e: &EngagementSummary,
) -> Result<Option<String>, KernelError> {
    subject_of(conn, tenant, e.reports_to_staff_id).await
}

/// Records which workforce attributes flowed into the profile (L3-E-066) and keeps the
/// profile's staff category in line with the engagement.
pub async fn sync_profile(
    conn: &mut PgConnection,
    tenant: TenantId,
    scope: &ExecutionScope,
    before: &EngagementSummary,
    after: &EngagementSummary,
    cause: &str,
    actor: &Actor,
) -> Result<Vec<&'static str>, KernelError> {
    let (a, b) = (Attrs::of(before), Attrs::of(after));
    let fields: Vec<&'static str> = a
        .changed(&b)
        .into_iter()
        .filter(|f| {
            matches!(
                *f,
                "designation"
                    | "grade"
                    | "employmentType"
                    | "staffCategory"
                    | "orgUnitId"
                    | "campusId"
                    | "probationEndDate"
            )
        })
        .collect();
    if fields.is_empty() {
        return Ok(fields);
    }
    let staff = lock_staff(conn, tenant, after.staff_id).await?;
    let mut staff_version = staff.version;
    if fields.contains(&"staffCategory") && staff.staff_category != after.staff_category {
        staff_version = sqlx::query_scalar(
            "UPDATE people.staff_member SET staff_category = $2, version = version + 1, \
             updated_at = now() WHERE id = $1 RETURNING version",
        )
        .bind(staff.id.as_uuid())
        .bind(after.staff_category.as_str())
        .fetch_one(&mut *conn)
        .await
        .map_err(map_db_error)?;
    }
    let pick = |x: &Attrs| -> serde_json::Value {
        let mut m = serde_json::Map::new();
        for f in &fields {
            let v = match *f {
                "designation" => serde_json::json!(x.designation),
                "grade" => serde_json::json!(x.grade),
                "employmentType" => serde_json::json!(x.employment_type.as_str()),
                "staffCategory" => serde_json::json!(x.staff_category.as_str()),
                "orgUnitId" => serde_json::json!(x.org_unit_id),
                "campusId" => serde_json::json!(x.campus_id),
                _ => serde_json::json!(x.probation_end_date),
            };
            m.insert((*f).to_owned(), v);
        }
        serde_json::Value::Object(m)
    };
    let names: Vec<String> = fields.iter().map(|f| (*f).to_owned()).collect();
    sqlx::query(
        "INSERT INTO people.profile_sync (tenant_id, institution_id, staff_id, engagement_id, \
         cause, fields, before_values, after_values, actor) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(tenant.as_uuid())
    .bind(after.institution_id.as_uuid())
    .bind(after.staff_id.as_uuid())
    .bind(after.id.as_uuid())
    .bind(cause)
    .bind(&names)
    .bind(pick(&a))
    .bind(pick(&b))
    .bind(actor.id.as_deref())
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    emit(
        conn,
        scope,
        ev::STAFF_SYNCHRONISED,
        ev::STAFF_SYNCHRONISED_SCHEMA,
        "staff_member",
        staff.id.as_uuid(),
        "people.staff_member.synchronise",
        &modified(
            staff.id.as_uuid(),
            staff.institution_id,
            staff_version,
            &fields,
        ),
        actor,
    )
    .await?;
    Ok(fields)
}

/// Applies an approved change request to a locked engagement. An activation whose start date is
/// still ahead stays `approved` (scheduled) until `POST /engagements/{id}/activate`.
#[allow(clippy::too_many_arguments)]
async fn apply_request(
    conn: &mut PgConnection,
    tenant: TenantId,
    scope: &ExecutionScope,
    e: &EngagementSummary,
    req: &ChangeRequestView,
    today: NaiveDate,
    actor: &Actor,
) -> Result<EngagementSummary, KernelError> {
    let pid = req.process_instance_id.as_deref();
    match req.kind {
        ChangeKind::Activation => {
            if e.start_date > today {
                return Ok(e.clone());
            }
            let staff = lock_staff(conn, tenant, e.staff_id).await?;
            if staff.status != StaffStatus::Active {
                return Err(KernelError::Conflict(format!(
                    "staff record {} is {}; only an active (verified) profile can start an engagement",
                    staff.staff_number, staff.status
                )));
            }
            let active = change_engagement_status(
                conn,
                tenant,
                e,
                EngagementStatus::Active,
                &StatusChange {
                    cause_event_type: ev::ENGAGEMENT_ACTIVATED,
                    reason: req.reason.as_deref(),
                    process_instance_id: pid,
                },
                actor,
            )
            .await?;
            set_request_status(conn, req.id, "applied", None, None).await?;
            let manager = manager_subject(conn, tenant, &active).await?;
            emit(
                conn,
                scope,
                ev::ENGAGEMENT_ACTIVATED,
                ev::ENGAGEMENT_ACTIVATED_SCHEMA,
                "staff_engagement",
                e.id.as_uuid(),
                "people.staff_engagement.activate",
                &status_payload(
                    e.id.as_uuid(),
                    e.institution_id,
                    Some(&staff),
                    Some(&active),
                    Some(e.status.as_str()),
                    active.status.as_str(),
                    active.version,
                    req.reason.as_deref(),
                    pid,
                    manager,
                ),
                actor,
            )
            .await?;
            Ok(active)
        }
        ChangeKind::Modification | ChangeKind::Transfer => {
            if !e.status.is_live() || e.status == EngagementStatus::Pending {
                return Err(common::illegal(
                    "engagement",
                    &e.engagement_number,
                    e.status.as_str(),
                    req.kind.as_str(),
                ));
            }
            let after = Attrs::of(e).with(&req.proposal);
            let changed = Attrs::of(e).changed(&after);
            let updated = write_attrs(conn, tenant, e, &after, pid).await?;
            set_request_status(conn, req.id, "applied", None, None).await?;
            let (ty, schema, action) = if req.kind == ChangeKind::Transfer {
                (
                    ev::ENGAGEMENT_TRANSFERRED,
                    ev::ENGAGEMENT_TRANSFERRED_SCHEMA,
                    "people.staff_engagement.transfer",
                )
            } else {
                (
                    ev::ENGAGEMENT_MODIFIED,
                    ev::ENGAGEMENT_MODIFIED_SCHEMA,
                    "people.staff_engagement.modify",
                )
            };
            emit(
                conn,
                scope,
                ty,
                schema,
                "staff_engagement",
                e.id.as_uuid(),
                action,
                &modified(e.id.as_uuid(), e.institution_id, updated.version, &changed),
                actor,
            )
            .await?;
            sync_profile(conn, tenant, scope, e, &updated, req.kind.as_str(), actor).await?;
            Ok(updated)
        }
    }
}

/// Decides the open request of a locked engagement.
#[allow(clippy::too_many_arguments)]
async fn decide_request(
    conn: &mut PgConnection,
    tenant: TenantId,
    scope: &ExecutionScope,
    e: &EngagementSummary,
    req: &ChangeRequestView,
    approve: bool,
    reason: Option<&str>,
    today: NaiveDate,
    actor: &Actor,
) -> Result<EngagementSummary, KernelError> {
    let decider = actor.id.as_deref().unwrap_or(AUTO_APPROVER);
    if !approve {
        set_request_status(conn, req.id, "rejected", reason, Some(decider)).await?;
        let e2 = write_attrs(conn, tenant, e, &Attrs::of(e), None).await?;
        common::audit_only(
            conn,
            scope,
            "staff_engagement",
            e.id.as_uuid(),
            &format!("people.staff_engagement.reject_{}", req.kind.as_str()),
            serde_json::json!({ "requestId": req.id, "reason": reason }),
            actor,
        )
        .await?;
        return Ok(e2);
    }
    set_request_status(conn, req.id, "approved", reason, Some(decider)).await?;
    let mut approved = req.clone();
    approved.status = "approved".into();
    let applied = apply_request(conn, tenant, scope, e, &approved, today, actor).await?;
    if applied.version == e.version {
        // Scheduled activation: still bump the version so the decision is visible in the ETag.
        return write_attrs(conn, tenant, e, &Attrs::of(e), None).await;
    }
    Ok(applied)
}

/// Opens a governed change on a locked engagement (`submitted` event) and applies it at once
/// when the approval step auto-approved.
#[allow(clippy::too_many_arguments)]
pub async fn open_change(
    conn: &mut PgConnection,
    tenant: TenantId,
    scope: &ExecutionScope,
    e: &EngagementSummary,
    kind: ChangeKind,
    proposal: &EngagementChanges,
    effective_date: Option<NaiveDate>,
    reason: Option<&str>,
    outcome: &ApprovalOutcome,
    today: NaiveDate,
    actor: &Actor,
) -> Result<EngagementSummary, KernelError> {
    if open_request(conn, e.id).await?.is_some() {
        return Err(KernelError::Conflict(format!(
            "engagement {} already has a governed change in progress",
            e.engagement_number
        )));
    }
    let pid = outcome.process_instance_id();
    let req_id: Uuid = sqlx::query_scalar(
        "INSERT INTO people.engagement_change_request (tenant_id, institution_id, engagement_id, \
         staff_id, kind, proposal, effective_date, reason, process_instance_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING id",
    )
    .bind(tenant.as_uuid())
    .bind(e.institution_id.as_uuid())
    .bind(e.id.as_uuid())
    .bind(e.staff_id.as_uuid())
    .bind(kind.as_str())
    .bind(serde_json::to_value(proposal).map_err(KernelError::internal)?)
    .bind(effective_date)
    .bind(reason)
    .bind(pid)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let submitted = write_attrs(conn, tenant, e, &Attrs::of(e), Some(pid)).await?;
    let staff = load_staff(conn, tenant, e.staff_id).await?;
    let manager = manager_subject(conn, tenant, e).await?;
    emit(
        conn,
        scope,
        ev::ENGAGEMENT_SUBMITTED,
        ev::ENGAGEMENT_SUBMITTED_SCHEMA,
        "staff_engagement",
        e.id.as_uuid(),
        &format!("people.staff_engagement.submit_{}", kind.as_str()),
        &status_payload(
            e.id.as_uuid(),
            e.institution_id,
            staff.as_ref(),
            Some(&submitted),
            Some(e.status.as_str()),
            e.status.as_str(),
            submitted.version,
            Some(reason.unwrap_or(kind.as_str())),
            Some(pid),
            manager,
        ),
        actor,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        let req = open_request(conn, e.id)
            .await?
            .ok_or(KernelError::NotFound {
                entity: "engagement change",
            })?;
        debug_assert_eq!(req.id, req_id);
        let system = Actor::system(actor.correlation_id.clone());
        return decide_request(
            conn, tenant, scope, &submitted, &req, true, None, today, &system,
        )
        .await;
    }
    Ok(submitted)
}

/// Requests the approval of a governed engagement change (before the transaction).
pub async fn approval_for(
    st: &PeopleState,
    scope: &ExecutionScope,
    key: &'static str,
    e: &EngagementSummary,
    kind: ChangeKind,
) -> Result<ApprovalOutcome, KernelError> {
    let mut vars = BTreeMap::new();
    vars.insert(
        "engagementNumber".to_owned(),
        serde_json::json!(e.engagement_number),
    );
    vars.insert("kind".to_owned(), serde_json::json!(kind.as_str()));
    request_approval(
        st,
        scope,
        key,
        business_key("staff_engagement", e.id, e.version),
        vars,
    )
    .await
}

/// Closes a locked live engagement with its last working date (L3-E-007): open changes are
/// cancelled, active position assignments end, `staff_engagement.closed` is emitted.
pub async fn close_engagement(
    conn: &mut PgConnection,
    tenant: TenantId,
    scope: &ExecutionScope,
    e: &EngagementSummary,
    last_working_date: NaiveDate,
    reason: &str,
    actor: &Actor,
) -> Result<EngagementSummary, KernelError> {
    if !e.status.can_transition(EngagementStatus::Closed) {
        return Err(common::illegal(
            "engagement",
            &e.engagement_number,
            e.status.as_str(),
            "closed",
        ));
    }
    if last_working_date < e.start_date {
        return Err(invalid(
            "lastWorkingDate",
            "before_start",
            "must not be before the engagement start date",
        ));
    }
    let mut a = Attrs::of(e);
    a.last_working_date = Some(last_working_date);
    let dated = write_attrs(conn, tenant, e, &a, None).await?;
    sqlx::query(
        "UPDATE people.engagement_change_request SET status = 'cancelled', version = version + 1, \
         updated_at = now() WHERE engagement_id = $1 AND status IN ('pending_approval', 'approved')",
    )
    .bind(e.id.as_uuid())
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let closed = change_engagement_status(
        conn,
        tenant,
        &dated,
        EngagementStatus::Closed,
        &StatusChange {
            cause_event_type: ev::ENGAGEMENT_CLOSED,
            reason: Some(reason),
            process_instance_id: None,
        },
        actor,
    )
    .await?;
    crate::positions::end_for_engagement(conn, tenant, scope, &closed, last_working_date, actor)
        .await?;
    // Integration: open leave of the engagement that starts after the exit is cancelled (7B).
    crate::leave::request::cancel_after_exit(conn, scope, closed.id, last_working_date, actor)
        .await?;
    let staff = load_staff(conn, tenant, e.staff_id).await?;
    emit(
        conn,
        scope,
        ev::ENGAGEMENT_CLOSED,
        ev::ENGAGEMENT_CLOSED_SCHEMA,
        "staff_engagement",
        e.id.as_uuid(),
        "people.staff_engagement.close",
        &status_payload(
            e.id.as_uuid(),
            e.institution_id,
            staff.as_ref(),
            Some(&closed),
            Some(e.status.as_str()),
            closed.status.as_str(),
            closed.version,
            Some(reason),
            None,
            None,
        ),
        actor,
    )
    .await?;
    Ok(closed)
}

// ------------------------------------------------------------------ bodies

/// A new engagement of a staff record (pending until activation; L3-E-001 / 008).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EngagementCreate {
    pub org_unit_id: Option<Uuid>,
    pub campus_id: Option<Uuid>,
    /// Defaults to the staff record's category.
    pub staff_category: Option<StaffCategory>,
    pub employment_type: EmploymentType,
    pub designation: String,
    pub grade: Option<String>,
    pub start_date: NaiveDate,
    pub end_date: Option<NaiveDate>,
    pub probation_end_date: Option<NaiveDate>,
    pub reports_to_staff_id: Option<Uuid>,
    /// Rejoining (L3-E-008): the closed engagement this one follows.
    pub rejoin_of_engagement_id: Option<Uuid>,
}

/// Engagement modification (L3-E-002; `ENGAGEMENT_CHANGE`).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModifyBody {
    pub designation: Option<String>,
    pub grade: Option<String>,
    pub employment_type: Option<EmploymentType>,
    pub staff_category: Option<StaffCategory>,
    pub end_date: Option<NaiveDate>,
    pub probation_end_date: Option<NaiveDate>,
    pub effective_date: Option<NaiveDate>,
    pub reason: String,
}

/// Transfer between org units / campuses (L3-E-003; `ENGAGEMENT_CHANGE`).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TransferBody {
    pub org_unit_id: Option<Uuid>,
    pub campus_id: Option<Uuid>,
    pub effective_date: Option<NaiveDate>,
    pub reason: String,
}

/// Closure → exit (L3-E-007).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CloseBody {
    pub last_working_date: NaiveDate,
    pub separation_kind: Option<SeparationKind>,
    pub reason: String,
}

/// Reporting manager (L3-E-018); `null` removes it.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReportingBody {
    pub reports_to_staff_id: Option<Uuid>,
    pub reason: Option<String>,
}

// ------------------------------------------------------------------ handlers

/// Engagements of a staff record, newest first (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/staff/{id}/engagements", operation_id = "people_engagement_list", tag = "people-workforce",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = EngagementList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn list(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<EngagementList>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    staff_or_404(&mut conn, t, id).await?;
    let rows = sqlx::query(&format!(
        "SELECT {ENGAGEMENT_COLUMNS} FROM people.staff_engagement WHERE staff_id = $1 \
         AND tenant_id = $2 ORDER BY start_date DESC, created_at DESC"
    ))
    .bind(id)
    .bind(t.as_uuid())
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let mut items = Vec::with_capacity(rows.len());
    for r in &rows {
        let e = engagement_from_row(r).map_err(map_db_error)?;
        items.push(engagement_view(&mut conn, t, &e).await?);
    }
    Ok(Json(EngagementList { items }))
}

/// One engagement (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/engagements/{id}", operation_id = "people_engagement_get", tag = "people-workforce",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = EngagementRecordView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn get(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let e = load_engagement(&mut conn, t, StaffEngagementId(id))
        .await?
        .ok_or(KernelError::NotFound {
            entity: "engagement",
        })?;
    drop(conn);
    respond(&st, t, &e, StatusCode::OK).await
}

/// Create an engagement (`people.staff.manage`; Idempotency-Key). Starts `pending`; one live
/// engagement per staff record (409). Rejoining names the closed engagement it follows and
/// restores an archived profile.
#[utoipa::path(post, path = "/api/v1/people/staff/{id}/engagements", operation_id = "people_engagement_create", tag = "people-workforce",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)),
    request_body = EngagementCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = EngagementRecordView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn create(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<EngagementCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    require_idempotency_key(&headers)?;
    let t = ctx.scope.tenant_id;
    let pre = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        staff_or_404(&mut conn, t, id).await?
    };
    let scope = institution_scope(ctx, pre.institution_id);
    require_institution_writable(&st, &scope, pre.institution_id).await?;
    let designation = required_text("designation", &b.designation, 120)?;
    let grade = valid_code("grade", b.grade)?;
    if b.end_date.is_some_and(|d| d < b.start_date) {
        return Err(invalid("endDate", "before_start", "must not be before startDate").into());
    }
    if b.probation_end_date.is_some_and(|d| d < b.start_date) {
        return Err(invalid(
            "probationEndDate",
            "before_start",
            "must not be before startDate",
        )
        .into());
    }
    if b.employment_type == sutra_contracts::people::status::EmploymentType::Probation
        && b.probation_end_date.is_none()
    {
        return Err(invalid(
            "probationEndDate",
            "required",
            "a probation engagement needs its probation end date",
        )
        .into());
    }
    check_org_unit(&st, &scope, pre.institution_id, "orgUnitId", b.org_unit_id).await?;
    check_campus(&st, &scope, pre.institution_id, "campusId", b.campus_id).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let staff = lock_staff(&mut tx, t, pre.id).await?;
    if let Some(m) = b.reports_to_staff_id {
        validate_manager(&mut tx, t, &staff, StaffMemberId(m)).await?;
    }
    let mut rejoin = None;
    if let Some(prev) = b.rejoin_of_engagement_id {
        let p = load_engagement(&mut tx, t, StaffEngagementId(prev))
            .await?
            .ok_or_else(|| invalid("rejoinOfEngagementId", "not_found", "unknown engagement"))?;
        if p.staff_id != staff.id {
            return Err(invalid(
                "rejoinOfEngagementId",
                "other_staff",
                "the engagement belongs to another staff record",
            )
            .into());
        }
        if p.status != EngagementStatus::Closed {
            return Err(KernelError::Conflict(format!(
                "engagement {} is {}; only a closed engagement can be rejoined",
                p.engagement_number, p.status
            ))
            .into());
        }
        if p.last_working_date.is_some_and(|l| b.start_date <= l) {
            return Err(invalid(
                "startDate",
                "before_exit",
                "rejoining starts after the last working date",
            )
            .into());
        }
        rejoin = Some(p.id);
    }
    let staff = if staff.status == StaffStatus::Archived {
        if rejoin.is_none() {
            return Err(KernelError::Conflict(format!(
                "staff record {} is archived; rejoin a closed engagement to restore it",
                staff.staff_number
            ))
            .into());
        }
        let restored = change_staff_status(
            &mut tx,
            t,
            &staff,
            StaffStatus::Active,
            &StatusChange {
                cause_event_type: ev::STAFF_RESTORED,
                reason: Some("rejoining"),
                process_instance_id: None,
            },
            &actor,
        )
        .await?;
        emit(
            &mut tx,
            &scope,
            ev::STAFF_RESTORED,
            ev::STAFF_RESTORED_SCHEMA,
            "staff_member",
            staff.id.as_uuid(),
            "people.staff_member.restore",
            &status_payload(
                staff.id.as_uuid(),
                staff.institution_id,
                Some(&restored),
                None,
                Some(staff.status.as_str()),
                restored.status.as_str(),
                restored.version,
                Some("rejoining"),
                None,
                None,
            ),
            &actor,
        )
        .await?;
        restored
    } else {
        staff
    };
    let e = insert_engagement(
        &mut tx,
        &NewEngagement {
            tenant: t,
            institution: staff.institution_id,
            staff_id: staff.id,
            org_unit_id: b.org_unit_id,
            campus_id: b.campus_id,
            staff_category: b.staff_category.unwrap_or(staff.staff_category),
            employment_type: b.employment_type,
            designation: &designation,
            grade: grade.as_deref(),
            start_date: b.start_date,
            end_date: b.end_date,
            probation_end_date: b.probation_end_date,
            reports_to_staff_id: b.reports_to_staff_id.map(StaffMemberId),
            rejoin_of: rejoin,
            status: EngagementStatus::Pending,
        },
        &actor,
    )
    .await?;
    common::audit_only(
        &mut tx,
        &scope,
        "staff_engagement",
        e.id.as_uuid(),
        if rejoin.is_some() {
            "people.staff_engagement.rejoin"
        } else {
            "people.staff_engagement.create"
        },
        serde_json::json!({
            "engagementNumber": e.engagement_number,
            "staffId": staff.id,
            "rejoinOf": rejoin,
        }),
        &actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, t, &e, StatusCode::CREATED).await
}

/// Loads an engagement outside the transaction to resolve its institution scope.
async fn pre_load(
    st: &PeopleState,
    ctx: &sutra_kernel::auth::AuthContext,
    id: Uuid,
) -> Result<(EngagementSummary, ExecutionScope), KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let e = load_engagement(&mut conn, ctx.scope.tenant_id, StaffEngagementId(id))
        .await?
        .ok_or(KernelError::NotFound {
            entity: "engagement",
        })?;
    let scope = institution_scope(ctx, e.institution_id);
    require_institution_writable(st, &scope, e.institution_id).await?;
    Ok((e, scope))
}

/// Submit a pending engagement for activation (`people.staff.manage`; If-Match;
/// `ENGAGEMENT_ACTIVATION`). The staff record must be active (verified). Auto-approval activates it
/// at once when the start date has come; otherwise it waits for `/activate`.
#[utoipa::path(post, path = "/api/v1/people/engagements/{id}/submit", operation_id = "people_engagement_submit", tag = "people-workforce",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the engagement")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = EngagementRecordView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn submit(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    let v = expected!(headers);
    let reason = reason_of(body);
    let t = ctx.scope.tenant_id;
    let (pre, scope) = pre_load(&st, ctx, id).await?;
    check_version(v, pre.version)?;
    if pre.status != EngagementStatus::Pending {
        return Err(common::illegal(
            "engagement",
            &pre.engagement_number,
            pre.status.as_str(),
            "active",
        )
        .into());
    }
    {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        let staff = staff_or_404(&mut conn, t, pre.staff_id.as_uuid()).await?;
        if staff.status != StaffStatus::Active {
            return Err(KernelError::Conflict(format!(
                "staff record {} is {}; verify and activate the profile first",
                staff.staff_number, staff.status
            ))
            .into());
        }
    }
    let today = business_today(&st, &scope, pre.institution_id).await?;
    let outcome = approval_for(
        &st,
        &scope,
        process::ENGAGEMENT_ACTIVATION,
        &pre,
        ChangeKind::Activation,
    )
    .await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let e = lock_engagement(&mut tx, t, pre.id).await?;
    check_version(v, e.version)?;
    let out = open_change(
        &mut tx,
        t,
        &scope,
        &e,
        ChangeKind::Activation,
        &EngagementChanges::default(),
        Some(e.start_date),
        reason.as_deref(),
        &outcome,
        today,
        &actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, t, &out, StatusCode::OK).await
}

/// Decide the open governed change of an engagement (`people.approve`, step-up; If-Match).
#[utoipa::path(post, path = "/api/v1/people/engagements/{id}/decision", operation_id = "people_engagement_decide", tag = "people-workforce",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the engagement")),
    request_body = DecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = EngagementRecordView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn decide(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<DecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleApprove)?;
    let v = expected!(headers);
    let reason = clean(b.reason);
    if b.decision == Decision::Reject && reason.is_none() {
        return Err(invalid("reason", "required", "a rejection needs a reason").into());
    }
    let t = ctx.scope.tenant_id;
    let (pre, scope) = pre_load(&st, ctx, id).await?;
    let today = business_today(&st, &scope, pre.institution_id).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let e = lock_engagement(&mut tx, t, pre.id).await?;
    check_version(v, e.version)?;
    let req = open_request(&mut tx, e.id)
        .await?
        .filter(|r| r.status == "pending_approval")
        .ok_or_else(|| {
            KernelError::Conflict(format!(
                "engagement {} has no change awaiting a decision",
                e.engagement_number
            ))
        })?;
    let out = decide_request(
        &mut tx,
        t,
        &scope,
        &e,
        &req,
        b.decision == Decision::Approve,
        reason.as_deref(),
        today,
        &actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, t, &out, StatusCode::OK).await
}

/// Activate an approved engagement whose start date has come (`people.staff.manage`; If-Match).
#[utoipa::path(post, path = "/api/v1/people/engagements/{id}/activate", operation_id = "people_engagement_activate", tag = "people-workforce",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the engagement")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = EngagementRecordView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn activate(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let (pre, scope) = pre_load(&st, ctx, id).await?;
    let today = business_today(&st, &scope, pre.institution_id).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let e = lock_engagement(&mut tx, t, pre.id).await?;
    check_version(v, e.version)?;
    let req = open_request(&mut tx, e.id)
        .await?
        .filter(|r| r.status == "approved" && r.kind == ChangeKind::Activation)
        .ok_or_else(|| {
            KernelError::Conflict(format!(
                "engagement {} has no approved activation",
                e.engagement_number
            ))
        })?;
    if e.start_date > today {
        return Err(KernelError::Conflict(format!(
            "engagement {} starts on {}",
            e.engagement_number, e.start_date
        ))
        .into());
    }
    let out = apply_request(&mut tx, t, &scope, &e, &req, today, &actor).await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, t, &out, StatusCode::OK).await
}

/// Request a modification of a live engagement (`people.staff.manage`; If-Match;
/// `ENGAGEMENT_CHANGE`; L3-E-002). Applied (and synchronised into the profile) on approval.
#[utoipa::path(post, path = "/api/v1/people/engagements/{id}/modify", operation_id = "people_engagement_modify", tag = "people-workforce",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the engagement")),
    request_body = ModifyBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = EngagementRecordView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn modify(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ModifyBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    let v = expected!(headers);
    let reason = required_reason(Some(b.reason))?;
    let designation = match b.designation {
        Some(d) => Some(required_text("designation", &d, 120)?),
        None => None,
    };
    let changes = EngagementChanges {
        designation,
        grade: valid_code("grade", b.grade)?,
        employment_type: b.employment_type,
        staff_category: b.staff_category,
        end_date: b.end_date,
        probation_end_date: b.probation_end_date,
        org_unit_id: None,
        campus_id: None,
    };
    governed_change(
        &st,
        ctx,
        id,
        v,
        ChangeKind::Modification,
        changes,
        b.effective_date,
        reason,
    )
    .await
}

/// Request a transfer to another org unit / campus (`people.staff.manage`; If-Match;
/// `ENGAGEMENT_CHANGE`; L3-E-003). Both are validated through Domain B (422 on the field).
#[utoipa::path(post, path = "/api/v1/people/engagements/{id}/transfer", operation_id = "people_engagement_transfer", tag = "people-workforce",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the engagement")),
    request_body = TransferBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = EngagementRecordView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn transfer(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<TransferBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    let v = expected!(headers);
    let reason = required_reason(Some(b.reason))?;
    if b.org_unit_id.is_none() && b.campus_id.is_none() {
        return Err(invalid(
            "orgUnitId",
            "required",
            "name the target org unit and / or campus",
        )
        .into());
    }
    let changes = EngagementChanges {
        org_unit_id: b.org_unit_id,
        campus_id: b.campus_id,
        ..EngagementChanges::default()
    };
    governed_change(
        &st,
        ctx,
        id,
        v,
        ChangeKind::Transfer,
        changes,
        b.effective_date,
        reason,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn governed_change(
    st: &PeopleState,
    ctx: &sutra_kernel::auth::AuthContext,
    id: Uuid,
    v: i64,
    kind: ChangeKind,
    changes: EngagementChanges,
    effective_date: Option<NaiveDate>,
    reason: String,
) -> ApiResult<Response> {
    let t = ctx.scope.tenant_id;
    let (pre, scope) = pre_load(st, ctx, id).await?;
    check_version(v, pre.version)?;
    let allowed = match kind {
        ChangeKind::Transfer => pre.status == EngagementStatus::Active,
        _ => pre.status.is_live() && pre.status != EngagementStatus::Pending,
    };
    if !allowed {
        return Err(common::illegal(
            "engagement",
            &pre.engagement_number,
            pre.status.as_str(),
            kind.as_str(),
        )
        .into());
    }
    if kind == ChangeKind::Transfer {
        check_org_unit(
            st,
            &scope,
            pre.institution_id,
            "orgUnitId",
            changes.org_unit_id,
        )
        .await?;
        check_campus(
            st,
            &scope,
            pre.institution_id,
            "campusId",
            changes.campus_id,
        )
        .await?;
    }
    let after = Attrs::of(&pre).with(&changes);
    if after.end_date.is_some_and(|d| d < pre.start_date) {
        return Err(invalid(
            "endDate",
            "before_start",
            "must not be before the start date",
        )
        .into());
    }
    if Attrs::of(&pre).changed(&after).is_empty() {
        return Err(invalid("changes", "no_change", "the request changes nothing").into());
    }
    let today = business_today(st, &scope, pre.institution_id).await?;
    let outcome = approval_for(st, &scope, process::ENGAGEMENT_CHANGE, &pre, kind).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let e = lock_engagement(&mut tx, t, pre.id).await?;
    check_version(v, e.version)?;
    let out = open_change(
        &mut tx,
        t,
        &scope,
        &e,
        kind,
        &changes,
        effective_date,
        Some(reason.as_str()),
        &outcome,
        today,
        &actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(st, t, &out, StatusCode::OK).await
}

struct EngMove {
    to: EngagementStatus,
    from: &'static [EngagementStatus],
    event: Option<(&'static str, &'static str)>,
    action: &'static str,
    reason_required: bool,
}

async fn engagement_move(
    st: &PeopleState,
    auth: &Authenticated,
    id: Uuid,
    headers: &HeaderMap,
    reason: Option<String>,
    m: EngMove,
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
    let (pre, scope) = pre_load(st, ctx, id).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let e = lock_engagement(&mut tx, t, pre.id).await?;
    check_version(v, e.version)?;
    if !m.from.contains(&e.status) {
        return Err(common::illegal(
            "engagement",
            &e.engagement_number,
            e.status.as_str(),
            m.to.as_str(),
        )
        .into());
    }
    if m.to == EngagementStatus::Cancelled {
        sqlx::query(
            "UPDATE people.engagement_change_request SET status = 'cancelled', version = version + 1, \
             updated_at = now() WHERE engagement_id = $1 AND status IN ('pending_approval', 'approved')",
        )
        .bind(e.id.as_uuid())
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    }
    let cause = m
        .event
        .map(|x| x.0)
        .unwrap_or("sutraerp.people.staff_engagement.status");
    let moved = change_engagement_status(
        &mut tx,
        t,
        &e,
        m.to,
        &StatusChange {
            cause_event_type: cause,
            reason: reason.as_deref(),
            process_instance_id: None,
        },
        &actor,
    )
    .await?;
    let staff = load_staff(&mut tx, t, e.staff_id).await?;
    let payload = status_payload(
        e.id.as_uuid(),
        e.institution_id,
        staff.as_ref(),
        Some(&moved),
        Some(e.status.as_str()),
        moved.status.as_str(),
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
                "staff_engagement",
                e.id.as_uuid(),
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
                "staff_engagement",
                e.id.as_uuid(),
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

macro_rules! engagement_command {
    ($(#[$m:meta])* $fn:ident, $path:literal, $op:literal, $move:expr) => {
        $(#[$m])*
        #[utoipa::path(post, path = $path, operation_id = $op, tag = "people-workforce",
            params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the engagement")),
            request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
            responses((status = 200, body = EngagementRecordView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
        pub async fn $fn(
            State(st): State<PeopleState>,
            auth: Authenticated,
            Path(id): Path<Uuid>,
            headers: HeaderMap,
            body: Option<Json<ReasonBody>>,
        ) -> ApiResult<Response> {
            engagement_move(&st, &auth, id, &headers, reason_of(body), $move).await
        }
    };
}

engagement_command!(
    /// Suspend an active engagement (`people.staff.manage`; reason required; L3-E-004). Domain D
    /// then sees the staff member as inactive.
    suspend,
    "/api/v1/people/engagements/{id}/suspend",
    "people_engagement_suspend",
    EngMove {
        to: EngagementStatus::Suspended,
        from: &[EngagementStatus::Active],
        event: Some((ev::ENGAGEMENT_SUSPENDED, ev::ENGAGEMENT_SUSPENDED_SCHEMA)),
        action: "people.staff_engagement.suspend",
        reason_required: true,
    }
);
engagement_command!(
    /// Restore a suspended engagement to operational use (`people.staff.manage`; L3-E-004).
    restore,
    "/api/v1/people/engagements/{id}/restore",
    "people_engagement_restore",
    EngMove {
        to: EngagementStatus::Active,
        from: &[EngagementStatus::Suspended],
        event: Some((ev::ENGAGEMENT_RESTORED, ev::ENGAGEMENT_RESTORED_SCHEMA)),
        action: "people.staff_engagement.restore",
        reason_required: false,
    }
);
engagement_command!(
    /// Cancel an engagement that never started (`people.staff.manage`; reason required).
    cancel,
    "/api/v1/people/engagements/{id}/cancel",
    "people_engagement_cancel",
    EngMove {
        to: EngagementStatus::Cancelled,
        from: &[EngagementStatus::Pending],
        event: None,
        action: "people.staff_engagement.cancel",
        reason_required: true,
    }
);

/// Close a live engagement — institutional exit with the last working date
/// (`people.staff.manage`; If-Match; L3-E-007). 409 while a separation case is open (it closes the
/// engagement itself).
#[utoipa::path(post, path = "/api/v1/people/engagements/{id}/close", operation_id = "people_engagement_close", tag = "people-workforce",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the engagement")),
    request_body = CloseBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = EngagementRecordView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn close(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<CloseBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    let v = expected!(headers);
    let reason = required_reason(Some(b.reason))?;
    let reason = match b.separation_kind {
        Some(k) => format!("{}: {reason}", k.as_str()),
        None => reason,
    };
    let t = ctx.scope.tenant_id;
    let (pre, scope) = pre_load(&st, ctx, id).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let e = lock_engagement(&mut tx, t, pre.id).await?;
    check_version(v, e.version)?;
    let open_case: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM people.separation_case WHERE engagement_id = $1 \
         AND status IN ('initiated', 'pending_approval', 'approved', 'processing'))",
    )
    .bind(e.id.as_uuid())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if open_case {
        return Err(KernelError::Conflict(format!(
            "engagement {} has an open separation case; complete it instead",
            e.engagement_number
        ))
        .into());
    }
    let closed =
        close_engagement(&mut tx, t, &scope, &e, b.last_working_date, &reason, &actor).await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, t, &closed, StatusCode::OK).await
}

/// Set or remove the reporting manager (`people.staff.manage`; If-Match; L3-E-018). The manager
/// needs a live engagement in the same institution; cycles are refused (409).
#[utoipa::path(put, path = "/api/v1/people/engagements/{id}/reporting", operation_id = "people_reporting_set", tag = "people-workforce",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the engagement")),
    request_body = ReportingBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = EngagementRecordView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn set_reporting(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ReportingBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    let v = expected!(headers);
    let reason = clean(b.reason);
    let t = ctx.scope.tenant_id;
    let (pre, scope) = pre_load(&st, ctx, id).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let e = lock_engagement(&mut tx, t, pre.id).await?;
    check_version(v, e.version)?;
    if !e.status.is_live() {
        return Err(KernelError::Conflict(format!(
            "engagement {} is {}",
            e.engagement_number, e.status
        ))
        .into());
    }
    let staff = load_staff(&mut tx, t, e.staff_id)
        .await?
        .ok_or(KernelError::NotFound {
            entity: "staff member",
        })?;
    let manager = b.reports_to_staff_id.map(StaffMemberId);
    if let Some(m) = manager {
        validate_manager(&mut tx, t, &staff, m).await?;
    }
    if manager == e.reports_to_staff_id {
        tx.commit().await.map_err(map_db_error)?;
        return respond(&st, t, &e, StatusCode::OK).await;
    }
    let mut a = Attrs::of(&e);
    a.reports_to = manager;
    let updated = write_attrs(&mut tx, t, &e, &a, None).await?;
    history(
        &mut tx,
        t,
        e.institution_id,
        "reporting_line",
        e.id.as_uuid(),
        e.reports_to_staff_id.map(|m| m.to_string()).as_deref(),
        &manager
            .map(|m| m.to_string())
            .unwrap_or_else(|| "none".into()),
        ev::REPORTING_LINE_SET,
        reason.as_deref(),
        None,
        &actor,
    )
    .await?;
    emit(
        &mut tx,
        &scope,
        ev::REPORTING_LINE_SET,
        ev::REPORTING_LINE_SET_SCHEMA,
        "staff_engagement",
        e.id.as_uuid(),
        "people.staff_engagement.set_reporting",
        &modified(
            e.id.as_uuid(),
            e.institution_id,
            updated.version,
            &["reportsToStaffId"],
        ),
        &actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, t, &updated, StatusCode::OK).await
}

pub fn router(state: PeopleState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(list, create))
        .routes(routes!(get))
        .routes(routes!(submit))
        .routes(routes!(decide))
        .routes(routes!(activate))
        .routes(routes!(modify))
        .routes(routes!(transfer))
        .routes(routes!(suspend))
        .routes(routes!(restore))
        .routes(routes!(cancel))
        .routes(routes!(close))
        .routes(routes!(set_reporting))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eng() -> EngagementSummary {
        EngagementSummary {
            id: StaffEngagementId(Uuid::nil()),
            staff_id: StaffMemberId(Uuid::nil()),
            institution_id: sutra_kernel::scope::InstitutionId(Uuid::nil()),
            engagement_number: "ENG-000001".into(),
            org_unit_id: None,
            campus_id: None,
            staff_category: StaffCategory::Teaching,
            employment_type: EmploymentType::Probation,
            designation: "Assistant Professor".into(),
            grade: Some("AP-1".into()),
            start_date: NaiveDate::from_ymd_opt(2024, 1, 1).unwrap_or_default(),
            end_date: None,
            last_working_date: None,
            probation_end_date: None,
            reports_to_staff_id: None,
            rejoin_of_engagement_id: None,
            status: EngagementStatus::Active,
            version: 1,
        }
    }

    #[test]
    fn proposals_apply_and_report_changed_fields() {
        let before = Attrs::of(&eng());
        let after = before.clone().with(&EngagementChanges {
            designation: Some("Associate Professor".into()),
            employment_type: Some(EmploymentType::Permanent),
            campus_id: Some(Uuid::nil()),
            ..EngagementChanges::default()
        });
        assert_eq!(
            before.changed(&after),
            vec!["designation", "employmentType", "campusId"]
        );
        assert!(before.changed(&before.clone()).is_empty());
        assert_eq!(ChangeKind::parse("transfer"), Some(ChangeKind::Transfer));
        assert_eq!(ChangeKind::parse("x"), None);
        let json = serde_json::to_value(EngagementChanges {
            grade: Some("AP-2".into()),
            ..EngagementChanges::default()
        })
        .unwrap_or_default();
        assert_eq!(json["grade"], "AP-2");
        assert!(json["orgUnitId"].is_null());
    }
}
