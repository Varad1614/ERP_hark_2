//! Governed offering changes through the WorkflowPort: administrative cancellation (L3-D-004),
//! governed unfreeze (L3-D-013), revision / replacement / rollback (L3-D-009) — one request
//! record per change in `academic.offering_revision` — and merge / split reorganisation
//! (L3-D-011) in `academic.offering_reorganisation`. Auto-approval applies the change in the
//! same transaction; `AwaitDecision` leaves the request `requested` for the decision endpoint.

use super::enrolment::{insert_seat, SeatBasis};
use super::lifecycle::{
    changeable, clear_waitlist, emit_assignment_ended, end_assignments, etag, expect_version,
    move_sections, set_status,
};
use super::sections::insert_section_row;
use super::support::*;
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::http::with_etag;
use crate::shared::lifecycle::{
    change_assignment_status, change_enrolment_status, change_offering_status,
    change_section_status, StatusChange,
};
use crate::shared::offering::{
    assignment_from_row, enrolment_from_row, lock_assignment, lock_enrolment, lock_offering,
    lock_section, offering_from_row, OfferingRow, SectionRow, ASSIGNMENT_COLUMNS,
    ENROLMENT_COLUMNS, OFFERING_COLUMNS,
};
use crate::shared::scope::tenant;
use crate::shared::{publish, AcademicState, Actor};
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, Utc};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use std::collections::BTreeMap;
use sutra_contracts::academic::events::offering as ev;
use sutra_contracts::academic::ids::{CourseOfferingId, SectionEnrolmentId};
use sutra_contracts::academic::status::{
    ChangeRequestStatus, CourseOfferingStatus, FacultyAssignmentStatus, SectionEnrolmentStatus,
    SectionStatus,
};
use sutra_contracts::student::ports::{StudentContextSummary, StudentSummary};
use sutra_contracts::student::status::{StudentContextStatus, StudentStatus};
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use uuid::Uuid;

// ================================================================== views

/// A governed change request on an offering.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OfferingChangeRequest {
    pub id: Uuid,
    pub offering_id: Uuid,
    /// `revision` | `replacement` | `rollback` | `cancellation` | `unfreeze`.
    pub kind: String,
    #[schema(value_type = Object)]
    pub proposed: serde_json::Value,
    #[schema(value_type = Option<Object>)]
    pub baseline: Option<serde_json::Value>,
    #[schema(value_type = Option<Object>)]
    pub snapshot_after: Option<serde_json::Value>,
    pub target_revision_id: Option<Uuid>,
    pub restore_status: Option<String>,
    pub status: ChangeRequestStatus,
    pub reason: String,
    pub decision_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub requested_by: Option<String>,
    pub decided_by: Option<String>,
    pub applied_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct OfferingChangeRequestList {
    pub items: Vec<OfferingChangeRequest>,
}

/// Result of a cancellation / unfreeze command: the offering and the governed request.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OfferingGovernedOutcome {
    pub offering: Offering,
    pub request: OfferingChangeRequest,
}

/// Attribute changes of a revision (all optional).
#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OfferingRevisionProposal {
    pub title: Option<String>,
    pub delivery_mode: Option<String>,
    pub campus_id: Option<Uuid>,
    pub org_unit_id: Option<Uuid>,
    pub capacity: Option<i32>,
    /// `replacement`: the curriculum component (same curriculum version) that replaces the current one.
    pub curriculum_component_id: Option<Uuid>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewOfferingRevision {
    /// `revision` | `replacement` | `rollback`.
    pub kind: String,
    #[serde(default)]
    pub proposed: OfferingRevisionProposal,
    /// `rollback`: an applied revision of this offering to restore; omitted = lifecycle rollback
    /// of an active offering without seats to `approved`.
    pub target_revision_id: Option<Uuid>,
    pub reason: String,
}

const REQUEST_COLUMNS: &str = "id, offering_id, kind, proposed, baseline, snapshot_after, target_revision_id, restore_status, \
    status, reason, decision_reason, process_instance_id, requested_by, decided_by, applied_at, version, created_at";

fn request_from_row(r: &PgRow) -> Result<OfferingChangeRequest, KernelError> {
    let status: String = r.try_get("status").map_err(map_db_error)?;
    Ok(OfferingChangeRequest {
        id: r.try_get("id").map_err(map_db_error)?,
        offering_id: r.try_get("offering_id").map_err(map_db_error)?,
        kind: r.try_get("kind").map_err(map_db_error)?,
        proposed: r.try_get("proposed").map_err(map_db_error)?,
        baseline: r.try_get("baseline").map_err(map_db_error)?,
        snapshot_after: r.try_get("snapshot_after").map_err(map_db_error)?,
        target_revision_id: r.try_get("target_revision_id").map_err(map_db_error)?,
        restore_status: r.try_get("restore_status").map_err(map_db_error)?,
        status: ChangeRequestStatus::parse(&status).unwrap_or(ChangeRequestStatus::Requested),
        reason: r.try_get("reason").map_err(map_db_error)?,
        decision_reason: r.try_get("decision_reason").map_err(map_db_error)?,
        process_instance_id: r.try_get("process_instance_id").map_err(map_db_error)?,
        requested_by: r.try_get("requested_by").map_err(map_db_error)?,
        decided_by: r.try_get("decided_by").map_err(map_db_error)?,
        applied_at: r.try_get("applied_at").map_err(map_db_error)?,
        version: r.try_get("version").map_err(map_db_error)?,
        created_at: r.try_get("created_at").map_err(map_db_error)?,
    })
}

/// Legal change-request transitions (slice-owned machine, shared by revisions, reorganisations,
/// simulations and faculty assignment changes).
pub fn change_can_transition(from: ChangeRequestStatus, to: ChangeRequestStatus) -> bool {
    use ChangeRequestStatus::*;
    matches!(
        (from, to),
        (Requested, Validated)
            | (Requested, Approved)
            | (Requested, Rejected)
            | (Requested, Withdrawn)
            | (Validated, Approved)
            | (Validated, Rejected)
            | (Validated, Withdrawn)
            | (Approved, Applied)
    )
}

fn process_for(kind: &str) -> &'static str {
    match kind {
        "cancellation" => process::COURSE_OFFERING_CANCELLATION,
        "unfreeze" => process::COURSE_OFFERING_UNFREEZE,
        _ => process::COURSE_OFFERING_REVISION,
    }
}

/// Permission a decision on this kind needs.
fn decide_permission(kind: &str) -> Permission {
    match kind {
        "cancellation" | "unfreeze" | "rollback" => Permission::OfferingLifecycle,
        _ => Permission::OfferingDecide,
    }
}

// ================================================================== validation + application

/// Read-only precheck before the approval step (the full check runs again when applying).
fn precheck(
    kind: &str,
    o: &OfferingRow,
    p: &OfferingRevisionProposal,
    target: Option<Uuid>,
) -> Result<(), KernelError> {
    match kind {
        "cancellation" => {
            if !o.status.can_transition(CourseOfferingStatus::Cancelled) {
                return Err(conflict(format!(
                    "the course offering is {}; administrative cancellation is not possible",
                    o.status
                )));
            }
        }
        "unfreeze" => {
            if o.status != CourseOfferingStatus::Frozen {
                return Err(conflict(format!(
                    "the course offering is {}; only frozen offerings are unfrozen",
                    o.status
                )));
            }
        }
        "revision" | "replacement" => {
            changeable(o)?;
            if o.status == CourseOfferingStatus::PendingApproval {
                return Err(conflict(
                    "the course offering awaits approval; decide it first",
                ));
            }
            if kind == "replacement" && p.curriculum_component_id.is_none() {
                return Err(invalid(
                    "proposed.curriculumComponentId",
                    "required",
                    "a replacement names the new component",
                ));
            }
            if kind == "revision"
                && p.title.is_none()
                && p.delivery_mode.is_none()
                && p.campus_id.is_none()
                && p.org_unit_id.is_none()
                && p.capacity.is_none()
            {
                return Err(invalid("proposed", "empty", "propose at least one change"));
            }
            if let Some(t) = &p.title {
                check_text("proposed.title", t, 200)?;
            }
            if let Some(m) = &p.delivery_mode {
                check_delivery_mode(m)?;
            }
            check_capacity("proposed.capacity", p.capacity)?;
        }
        "rollback" => {
            if o.status.is_terminal() || o.status == CourseOfferingStatus::Frozen {
                return Err(conflict(format!(
                    "the course offering is {}; it cannot be rolled back",
                    o.status
                )));
            }
            if target.is_none() && o.status != CourseOfferingStatus::Active {
                return Err(conflict(
                    "a lifecycle rollback applies to an active offering only",
                ));
            }
        }
        _ => {
            return Err(invalid(
                "kind",
                "invalid",
                "one of revision, replacement, rollback",
            ))
        }
    }
    Ok(())
}

/// Applies an approved request inside the command transaction; returns the applied attributes.
async fn apply_request(
    st: &AcademicState,
    c: &mut Cmd,
    req: &OfferingChangeRequest,
) -> Result<(), KernelError> {
    let reason = Some(req.reason.as_str());
    match req.kind.as_str() {
        "cancellation" => {
            if !c.o.status.can_transition(CourseOfferingStatus::Cancelled) {
                return Err(conflict(format!(
                    "the course offering is {}; it can no longer be cancelled",
                    c.o.status
                )));
            }
            if seated_in_offering(&mut *c.tx, c.o.id).await? > 0 {
                return Err(conflict("students are seated; cancellation would touch their enrolment — withdraw them first"));
            }
            move_sections(
                c,
                &[SectionStatus::Planned, SectionStatus::Active],
                SectionStatus::Cancelled,
                ev::COURSE_OFFERING_CANCELLED,
                reason,
            )
            .await?;
            let (scope, o, a) = (c.scope, c.o.clone(), c.actor.clone());
            end_assignments(
                &mut c.tx,
                &scope,
                &o,
                None,
                FacultyAssignmentStatus::Revoked,
                ev::COURSE_OFFERING_CANCELLED,
                reason,
                &a,
            )
            .await?;
            clear_waitlist(c, "offering cancelled").await?;
            sqlx::query("UPDATE academic.offering_enrolment_window SET status = 'closed_permanently', status_reason = 'offering cancelled', version = version + 1, updated_at = now() WHERE offering_id = $1 AND status <> 'closed_permanently'")
                .bind(c.o.id.as_uuid()).execute(&mut *c.tx).await.map_err(map_db_error)?;
            let pid = req.process_instance_id.clone();
            set_status(
                c,
                CourseOfferingStatus::Cancelled,
                ev::COURSE_OFFERING_CANCELLED,
                ev::COURSE_OFFERING_CANCELLED_SCHEMA,
                reason,
                pid.as_deref(),
            )
            .await?;
        }
        "unfreeze" => {
            if c.o.status != CourseOfferingStatus::Frozen {
                return Err(conflict(format!(
                    "the course offering is {}; nothing to unfreeze",
                    c.o.status
                )));
            }
            let restore = req
                .restore_status
                .as_deref()
                .and_then(CourseOfferingStatus::parse)
                .unwrap_or(CourseOfferingStatus::Active);
            let pid = req.process_instance_id.clone();
            set_status(
                c,
                restore,
                ev::COURSE_OFFERING_UNFROZEN,
                ev::COURSE_OFFERING_UNFROZEN_SCHEMA,
                reason,
                pid.as_deref(),
            )
            .await?;
        }
        "revision" | "replacement" => {
            let p: OfferingRevisionProposal =
                serde_json::from_value(req.proposed.clone()).unwrap_or_default();
            precheck(&req.kind, &c.o, &p, None)?;
            apply_attributes(st, c, &p).await?;
        }
        "rollback" => match req.target_revision_id {
            Some(t) => {
                let row = sqlx::query(&format!("SELECT {REQUEST_COLUMNS} FROM academic.offering_revision WHERE id = $1 AND offering_id = $2"))
                    .bind(t).bind(c.o.id.as_uuid()).fetch_optional(&mut *c.tx).await.map_err(map_db_error)?
                    .ok_or_else(|| invalid("targetRevisionId", "not_found", "no such revision of this offering"))?;
                let target = request_from_row(&row)?;
                let snap = match (target.status, target.kind.as_str(), &target.snapshot_after) {
                    (
                        ChangeRequestStatus::Applied,
                        "revision" | "replacement" | "rollback",
                        Some(s),
                    ) => s.clone(),
                    _ => {
                        return Err(invalid(
                            "targetRevisionId",
                            "not_approved",
                            "the rollback target must be an applied (approved) revision",
                        ))
                    }
                };
                changeable(&c.o)?;
                let p = OfferingRevisionProposal {
                    title: snap["title"].as_str().map(str::to_owned),
                    delivery_mode: snap["deliveryMode"].as_str().map(str::to_owned),
                    campus_id: snap["campusId"].as_str().and_then(|s| s.parse().ok()),
                    org_unit_id: snap["orgUnitId"].as_str().and_then(|s| s.parse().ok()),
                    capacity: snap["capacity"]
                        .as_i64()
                        .and_then(|x| i32::try_from(x).ok()),
                    curriculum_component_id: snap["curriculumComponentId"]
                        .as_str()
                        .and_then(|s| s.parse().ok())
                        .filter(|cid| *cid != c.o.curriculum_component_id),
                };
                apply_attributes(st, c, &p).await?;
            }
            None => {
                if c.o.status != CourseOfferingStatus::Active {
                    return Err(conflict(
                        "a lifecycle rollback applies to an active offering only",
                    ));
                }
                if seated_in_offering(&mut *c.tx, c.o.id).await? > 0 {
                    return Err(conflict(
                        "students are seated; the offering cannot roll back to approved",
                    ));
                }
                let pid = req.process_instance_id.clone();
                c.o = change_offering_status(
                    &mut c.tx,
                    &c.o,
                    CourseOfferingStatus::Approved,
                    &StatusChange {
                        cause_event_type: ev::COURSE_OFFERING_REVISED,
                        reason,
                        process_instance_id: pid.as_deref(),
                    },
                    &c.actor,
                )
                .await?;
                let data = modified(c.o.id.as_uuid(), &c.o, None, c.o.version.0, &["status"]);
                offering_event(
                    &mut c.tx,
                    &c.scope,
                    &c.o,
                    ev::COURSE_OFFERING_REVISED,
                    ev::COURSE_OFFERING_REVISED_SCHEMA,
                    &data,
                    &c.actor,
                )
                .await?;
            }
        },
        other => return Err(conflict(format!("unknown request kind {other}"))),
    }
    Ok(())
}

/// Applies revision / replacement attributes (validated) and emits `course_offering.revised`.
async fn apply_attributes(
    st: &AcademicState,
    c: &mut Cmd,
    p: &OfferingRevisionProposal,
) -> Result<(), KernelError> {
    let mut fields: Vec<&str> = Vec::new();
    let mut component = None;
    if let Some(cid) = p.curriculum_component_id {
        if seated_in_offering(&mut *c.tx, c.o.id).await? > 0 {
            return Err(conflict(
                "students are seated; the course component cannot be replaced",
            ));
        }
        if !matches!(
            c.o.status,
            CourseOfferingStatus::Draft
                | CourseOfferingStatus::Approved
                | CourseOfferingStatus::Published
        ) {
            return Err(conflict(format!(
                "the course offering is {}; replacement happens before enrolment",
                c.o.status
            )));
        }
        let comp = st
            .references
            .curriculum_components(&c.scope, c.o.curriculum_version_id)
            .await?
            .into_iter()
            .find(|x| x.id == cid)
            .ok_or_else(|| {
                invalid(
                    "proposed.curriculumComponentId",
                    "not_found",
                    "not a component of the offering's curriculum version",
                )
            })?;
        component = Some(comp);
        fields.extend(["curriculumComponentId", "credits"]);
    }
    if let Some(cap) = p.capacity {
        let seated = seated_in_offering(&mut *c.tx, c.o.id).await?;
        if i64::from(cap) < seated {
            return Err(conflict(format!(
                "{seated} student(s) are seated; the capacity cannot drop to {cap}"
            )));
        }
        fields.push("capacity");
    }
    if p.campus_id.is_some() || p.org_unit_id.is_some() {
        super::lifecycle::validate_refs(
            st,
            &c.scope,
            c.o.institution_id,
            c.o.academic_term_id,
            c.o.curriculum_version_id,
            c.o.curriculum_component_id,
            p.campus_id,
            p.org_unit_id,
        )
        .await
        .or_else(|e| match e {
            // The term may have ended since; attribute references are what matters here.
            KernelError::Conflict(_) => Ok(super::lifecycle::OfferingRefs {
                academic_year_id: Uuid::nil(),
                component: placeholder_component(),
            }),
            other => Err(other),
        })?;
        if p.campus_id.is_some() {
            fields.push("campusId");
        }
        if p.org_unit_id.is_some() {
            fields.push("orgUnitId");
        }
    }
    if p.title.is_some() || component.is_some() {
        fields.push("title");
    }
    if p.delivery_mode.is_some() {
        fields.push("deliveryMode");
    }
    let title = p
        .title
        .as_deref()
        .map(str::trim)
        .map(str::to_owned)
        .or_else(|| component.as_ref().map(|x| x.title.clone()));
    sqlx::query(
        "UPDATE academic.course_offering SET title = COALESCE($2, title), delivery_mode = COALESCE($3, delivery_mode), \
         campus_id = COALESCE($4, campus_id), org_unit_id = COALESCE($5, org_unit_id), \
         capacity = CASE WHEN $6::boolean THEN $7 ELSE capacity END, \
         curriculum_component_id = COALESCE($8, curriculum_component_id), course_code = CASE WHEN $8::uuid IS NULL THEN course_code ELSE $9 END, \
         category = CASE WHEN $8::uuid IS NULL THEN category ELSE $10 END, credits = COALESCE($11, credits), \
         version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(c.o.id.as_uuid())
    .bind(title)
    .bind(&p.delivery_mode)
    .bind(p.campus_id)
    .bind(p.org_unit_id)
    .bind(p.capacity.is_some())
    .bind(p.capacity)
    .bind(component.as_ref().map(|x| x.id))
    .bind(component.as_ref().and_then(|x| x.course_code.clone()))
    .bind(component.as_ref().map(|x| x.category.clone()))
    .bind(component.as_ref().map(|x| x.credits))
    .execute(&mut *c.tx)
    .await
    .map_err(map_db_error)?;
    c.o = lock_offering(&mut c.tx, c.o.tenant_id, c.o.id).await?;
    let data = modified(c.o.id.as_uuid(), &c.o, None, c.o.version.0, &fields);
    offering_event(
        &mut c.tx,
        &c.scope,
        &c.o,
        ev::COURSE_OFFERING_REVISED,
        ev::COURSE_OFFERING_REVISED_SCHEMA,
        &data,
        &c.actor,
    )
    .await
}

fn placeholder_component() -> sutra_contracts::institution::reference::CurriculumComponentRef {
    sutra_contracts::institution::reference::CurriculumComponentRef {
        id: Uuid::nil(),
        curriculum_version_id: Uuid::nil(),
        term_no: 0,
        slot_code: String::new(),
        title: String::new(),
        category: String::new(),
        credits: 0.0,
        course_code: None,
    }
}

/// Marks a request applied (snapshot after) — or sets another status — and returns it.
async fn set_request(
    conn: &mut PgConnection,
    req: &OfferingChangeRequest,
    to: ChangeRequestStatus,
    decision_reason: Option<&str>,
    decided_by: Option<&str>,
    snapshot_after: Option<serde_json::Value>,
) -> Result<OfferingChangeRequest, KernelError> {
    let row = sqlx::query(&format!(
        "UPDATE academic.offering_revision SET status = $2, decision_reason = COALESCE($3, decision_reason), \
         decided_by = COALESCE($4, decided_by), snapshot_after = COALESCE($5, snapshot_after), \
         applied_at = CASE WHEN $2 = 'applied' THEN now() ELSE applied_at END, version = version + 1, updated_at = now() \
         WHERE id = $1 AND version = $6 RETURNING {REQUEST_COLUMNS}"
    ))
    .bind(req.id)
    .bind(to.as_str())
    .bind(decision_reason)
    .bind(decided_by)
    .bind(snapshot_after)
    .bind(req.version)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or_else(|| conflict("the change request changed concurrently"))?;
    request_from_row(&row)
}

/// Approves (validated → approved → applied) inside the command, as `decider`.
async fn approve_and_apply(
    st: &AcademicState,
    c: &mut Cmd,
    req: OfferingChangeRequest,
    decider: &Actor,
    reason: Option<&str>,
) -> Result<OfferingChangeRequest, KernelError> {
    let user = std::mem::replace(&mut c.actor, decider.clone());
    let result = async {
        let req = set_request(
            &mut c.tx,
            &req,
            ChangeRequestStatus::Approved,
            reason,
            decider.id.as_deref(),
            None,
        )
        .await?;
        apply_request(st, c, &req).await?;
        let snap = snapshot(&c.o);
        let req = set_request(
            &mut c.tx,
            &req,
            ChangeRequestStatus::Applied,
            None,
            None,
            Some(snap),
        )
        .await?;
        c.audit(
            &format!("academic.course_offering.apply_{}", req.kind),
            serde_json::json!({"requestId": req.id, "processInstanceId": req.process_instance_id}),
        )
        .await?;
        Ok::<_, KernelError>(req)
    }
    .await;
    c.actor = user;
    result
}

/// Creates a governed request (approval before the tx) and applies it at once when approved.
#[allow(clippy::too_many_arguments)]
async fn submit_request(
    st: &AcademicState,
    ctx: &AuthContext,
    id: CourseOfferingId,
    expected: Option<Version>,
    kind: &str,
    proposed: OfferingRevisionProposal,
    target: Option<Uuid>,
    reason: String,
) -> Result<(OfferingChangeRequest, Offering), KernelError> {
    let o = crate::shared::offering::load_offering(&st.pool, tenant(ctx), id).await?;
    if let Some(v) = expected {
        check_version(o.version, v)?;
    }
    precheck(kind, &o, &proposed, target)?;
    if let Some(t) = target {
        // An unknown target would otherwise surface as a foreign-key violation (500).
        let known: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM academic.offering_revision WHERE id = $1 AND offering_id = $2",
        )
        .bind(t)
        .bind(o.id.as_uuid())
        .fetch_optional(&st.pool)
        .await
        .map_err(map_db_error)?;
        if known.is_none() {
            return Err(invalid(
                "targetRevisionId",
                "not_found",
                "no such revision of this offering",
            ));
        }
    }
    let outcome = request_approval(
        st,
        &crate::shared::scope::institution_scope(ctx, o.institution_id),
        process_for(kind),
        business_key(&format!("course_offering_{kind}"), o.id, o.version.0),
        BTreeMap::from([
            ("offeringId".to_owned(), serde_json::json!(o.id)),
            ("kind".to_owned(), serde_json::json!(kind)),
            ("reason".to_owned(), serde_json::json!(reason)),
        ]),
    )
    .await?;
    let mut c = begin(st, ctx, id, expected).await?;
    precheck(kind, &c.o, &proposed, target)?;
    let restore = (kind == "unfreeze").then_some("active");
    let row = sqlx::query(&format!(
        "INSERT INTO academic.offering_revision (tenant_id, institution_id, offering_id, kind, proposed, baseline, \
         target_revision_id, restore_status, reason, process_instance_id, requested_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) RETURNING {REQUEST_COLUMNS}"
    ))
    .bind(c.o.tenant_id.as_uuid())
    .bind(c.o.institution_id.as_uuid())
    .bind(c.o.id.as_uuid())
    .bind(kind)
    .bind(serde_json::to_value(&proposed).map_err(KernelError::internal)?)
    .bind(snapshot(&c.o))
    .bind(target)
    .bind(restore)
    .bind(&reason)
    .bind(outcome.process_instance_id())
    .bind(c.actor.id.as_deref())
    .fetch_one(&mut *c.tx)
    .await
    .map_err(|e| super::lifecycle::unique_code(e, "kind", "an open request of this kind already exists for the offering"))?;
    let mut req = request_from_row(&row)?;
    c.audit(&format!("academic.course_offering.request_{kind}"), serde_json::json!({"requestId": req.id, "reason": reason, "processInstanceId": req.process_instance_id})).await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        let system = Actor::system(c.actor.correlation_id.clone());
        req = approve_and_apply(st, &mut c, req, &system, Some("auto-approved")).await?;
    }
    let o = c.finish().await?;
    Ok((req, o))
}

// ================================================================== handlers: cancel / unfreeze

macro_rules! governed_command {
    ($fn:ident, $path:literal, $op:literal, $doc:literal, $kind:literal) => {
        #[doc = $doc]
        #[utoipa::path(
            post,
            path = $path,
            operation_id = $op,
            tag = "offerings",
            params(
                ("id" = String, Path, format = Uuid),
                ("If-Match" = String, Header, description = "ETag of the offering"),
                ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
            ),
            security(("session_cookie" = [], "csrf_token" = [])),
            request_body = StepReason,
            responses(
                (status = 200, description = "Offering after the request (unchanged while the decision is pending) and the request", body = OfferingGovernedOutcome),
                (status = 403, body = Problem, content_type = "application/problem+json"),
                (status = 404, body = Problem, content_type = "application/problem+json"),
                (status = 409, body = Problem, content_type = "application/problem+json"),
                (status = 412, body = Problem, content_type = "application/problem+json"),
                (status = 422, body = Problem, content_type = "application/problem+json"),
                (status = 428, body = Problem, content_type = "application/problem+json"),
            )
        )]
        pub async fn $fn(
            State(st): State<AcademicState>,
            auth: Authenticated,
            Path(id): Path<CourseOfferingId>,
            headers: HeaderMap,
            raw: Bytes,
        ) -> ApiResult<Response> {
            let ctx = auth.require(Permission::OfferingLifecycle)?;
            let v = expect_version!(headers);
            let reason = body::<StepReason>(&raw)?.validated(true)?.unwrap_or_default();
            let (request, offering) = submit_request(&st, ctx, id, Some(v), $kind, OfferingRevisionProposal::default(), None, reason).await?;
            let version = Version(offering.version);
            Ok(with_etag(StatusCode::OK, &OfferingGovernedOutcome { offering, request }, version))
        }
    };
}

governed_command!(cancel, "/api/v1/academic/offerings/{id}/cancel", "offering_cancel",
    "Administrative cancellation through the WorkflowPort (L3-D-004, reason required, `offering.lifecycle`, step-up). Only offerings without seats; registrations are never touched.",
    "cancellation");
governed_command!(unfreeze, "/api/v1/academic/offerings/{id}/unfreeze", "offering_unfreeze",
    "Governed unfreeze through the WorkflowPort (L3-D-013, reason required, `offering.lifecycle`, step-up): restores the preserved pre-freeze position.",
    "unfreeze");

// ================================================================== handlers: revisions

/// Request a revision, replacement or rollback (L3-D-009). Revision / replacement need
/// `offering.manage`, rollback `offering.lifecycle`.
#[utoipa::path(
    post,
    path = "/api/v1/academic/offerings/{id}/revisions",
    operation_id = "offering_revision_create",
    tag = "offerings",
    params(
        ("id" = String, Path, format = Uuid),
        ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key."),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = NewOfferingRevision,
    responses(
        (status = 201, body = OfferingChangeRequest),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn revision_create(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<CourseOfferingId>,
    headers: HeaderMap,
    Json(cmd): Json<NewOfferingRevision>,
) -> ApiResult<(StatusCode, Json<OfferingChangeRequest>)> {
    let perm = if cmd.kind == "rollback" {
        Permission::OfferingLifecycle
    } else {
        Permission::OfferingManage
    };
    let ctx = auth.require(perm)?;
    require_idempotency_key(&headers)?;
    if !matches!(cmd.kind.as_str(), "revision" | "replacement" | "rollback") {
        return Err(invalid("kind", "invalid", "one of revision, replacement, rollback").into());
    }
    let reason = check_text("reason", &cmd.reason, 1000)?;
    let (req, _) = submit_request(
        &st,
        ctx,
        id,
        None,
        &cmd.kind,
        cmd.proposed,
        cmd.target_revision_id,
        reason,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(req)))
}

/// Governed requests of an offering, newest first (`academic.read`).
#[utoipa::path(
    get,
    path = "/api/v1/academic/offerings/{id}/revisions",
    operation_id = "offering_revision_list",
    tag = "offerings",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses((status = 200, body = OfferingChangeRequestList), (status = 404, body = Problem, content_type = "application/problem+json"))
)]
pub async fn revision_list(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<CourseOfferingId>,
) -> ApiResult<Json<OfferingChangeRequestList>> {
    let ctx = auth.require(Permission::AcademicRead)?;
    let o = crate::shared::offering::load_offering(&st.pool, tenant(ctx), id).await?;
    let rows = sqlx::query(&format!("SELECT {REQUEST_COLUMNS} FROM academic.offering_revision WHERE offering_id = $1 ORDER BY created_at DESC"))
        .bind(o.id.as_uuid()).fetch_all(&st.pool).await.map_err(map_db_error)?;
    let items = rows
        .iter()
        .map(request_from_row)
        .collect::<Result<_, _>>()?;
    Ok(Json(OfferingChangeRequestList { items }))
}

/// Decide a pending governed request (`offering.decide`; cancellation / unfreeze / rollback
/// need `offering.lifecycle`). Approve applies it; reject keeps the offering as it was.
#[utoipa::path(
    post,
    path = "/api/v1/academic/offerings/{id}/revisions/{revisionId}/decision",
    operation_id = "offering_revision_decision",
    tag = "offerings",
    params(
        ("id" = String, Path, format = Uuid),
        ("revisionId" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag (version) of the request"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = StepDecisionBody,
    responses(
        (status = 200, body = OfferingChangeRequest),
        (status = 403, body = Problem, content_type = "application/problem+json"),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
        (status = 428, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn revision_decision(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path((id, rid)): Path<(CourseOfferingId, Uuid)>,
    headers: HeaderMap,
    Json(d): Json<StepDecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AcademicRead)?;
    // Part 1 · S3-appr (DEF-X-002): the requester of the pending request never decides it.
    sutra_persistence::approval::guard_decision(&st.pool, ctx, &id, "offering revision").await?;
    let v = expect_version!(headers);
    let reason = clean_reason(d.reason, d.decision == StepDecision::Reject)?;
    let mut c = begin(&st, ctx, id, None).await?;
    let row = sqlx::query(&format!("SELECT {REQUEST_COLUMNS} FROM academic.offering_revision WHERE id = $1 AND offering_id = $2 FOR UPDATE"))
        .bind(rid).bind(c.o.id.as_uuid()).fetch_optional(&mut *c.tx).await.map_err(map_db_error)?
        .ok_or(KernelError::NotFound { entity: "change request" })?;
    let req = request_from_row(&row)?;
    auth.require(decide_permission(&req.kind))?;
    check_version(Version(req.version), v)?;
    if !matches!(
        req.status,
        ChangeRequestStatus::Requested | ChangeRequestStatus::Validated
    ) {
        return Err(conflict(format!(
            "the request is {}; nothing awaits a decision",
            req.status
        ))
        .into());
    }
    let decider = c.actor.clone();
    let req = match d.decision {
        StepDecision::Approve => {
            approve_and_apply(&st, &mut c, req, &decider, reason.as_deref()).await?
        }
        StepDecision::Reject => {
            let r = set_request(
                &mut c.tx,
                &req,
                ChangeRequestStatus::Rejected,
                reason.as_deref(),
                decider.id.as_deref(),
                None,
            )
            .await?;
            c.audit(
                &format!("academic.course_offering.reject_{}", r.kind),
                serde_json::json!({"requestId": r.id, "reason": reason}),
            )
            .await?;
            r
        }
    };
    c.tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &req, Version(req.version)))
}

/// Withdraw a pending governed request (`offering.manage`).
#[utoipa::path(
    post,
    path = "/api/v1/academic/offerings/{id}/revisions/{revisionId}/withdraw",
    operation_id = "offering_revision_withdraw",
    tag = "offerings",
    params(
        ("id" = String, Path, format = Uuid),
        ("revisionId" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag (version) of the request"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 200, body = OfferingChangeRequest),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 428, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn revision_withdraw(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path((id, rid)): Path<(CourseOfferingId, Uuid)>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::OfferingManage)?;
    let v = expect_version!(headers);
    let mut c = begin(&st, ctx, id, None).await?;
    let row = sqlx::query(&format!("SELECT {REQUEST_COLUMNS} FROM academic.offering_revision WHERE id = $1 AND offering_id = $2 FOR UPDATE"))
        .bind(rid).bind(c.o.id.as_uuid()).fetch_optional(&mut *c.tx).await.map_err(map_db_error)?
        .ok_or(KernelError::NotFound { entity: "change request" })?;
    let req = request_from_row(&row)?;
    check_version(Version(req.version), v)?;
    if !change_can_transition(req.status, ChangeRequestStatus::Withdrawn) {
        return Err(conflict(format!("the request is {}", req.status)).into());
    }
    let req = set_request(
        &mut c.tx,
        &req,
        ChangeRequestStatus::Withdrawn,
        None,
        None,
        None,
    )
    .await?;
    c.audit(
        "academic.course_offering.withdraw_request",
        serde_json::json!({"requestId": req.id}),
    )
    .await?;
    c.tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &req, Version(req.version)))
}

// ================================================================== merge / split

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OfferingReorganisation {
    pub id: Uuid,
    pub offering_id: Uuid,
    /// `merge` | `split`.
    pub kind: String,
    pub source_offering_ids: Vec<Uuid>,
    #[schema(value_type = Object)]
    pub plan: serde_json::Value,
    pub result_offering_ids: Vec<Uuid>,
    pub status: ChangeRequestStatus,
    pub reason: String,
    pub decision_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub applied_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct OfferingReorganisationList {
    pub items: Vec<OfferingReorganisation>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OfferingMergeRequest {
    /// Active offerings of the same term and course component absorbed into this one.
    pub source_offering_ids: Vec<Uuid>,
    pub reason: String,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OfferingSplitPart {
    pub offering_code: String,
    pub title: Option<String>,
    pub section_ids: Vec<Uuid>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OfferingSplitRequest {
    /// Two or more successor offerings; every live section goes to exactly one.
    pub parts: Vec<OfferingSplitPart>,
    pub reason: String,
}

const REORG_COLUMNS: &str =
    "id, offering_id, kind, source_offering_ids, plan, result_offering_ids, status, reason, \
    decision_reason, process_instance_id, applied_at, version, created_at";

fn reorg_from_row(r: &PgRow) -> Result<OfferingReorganisation, KernelError> {
    let status: String = r.try_get("status").map_err(map_db_error)?;
    Ok(OfferingReorganisation {
        id: r.try_get("id").map_err(map_db_error)?,
        offering_id: r.try_get("offering_id").map_err(map_db_error)?,
        kind: r.try_get("kind").map_err(map_db_error)?,
        source_offering_ids: r.try_get("source_offering_ids").map_err(map_db_error)?,
        plan: r.try_get("plan").map_err(map_db_error)?,
        result_offering_ids: r.try_get("result_offering_ids").map_err(map_db_error)?,
        status: ChangeRequestStatus::parse(&status).unwrap_or(ChangeRequestStatus::Requested),
        reason: r.try_get("reason").map_err(map_db_error)?,
        decision_reason: r.try_get("decision_reason").map_err(map_db_error)?,
        process_instance_id: r.try_get("process_instance_id").map_err(map_db_error)?,
        applied_at: r.try_get("applied_at").map_err(map_db_error)?,
        version: r.try_get("version").map_err(map_db_error)?,
        created_at: r.try_get("created_at").map_err(map_db_error)?,
    })
}

/// Moves one seated enrolment into `to_section` of `to` (origin `transfer`); a student already
/// seated in the target keeps that seat and the old one is withdrawn.
async fn move_seat(
    c: &mut Cmd,
    to: &OfferingRow,
    to_section: &SectionRow,
    enrolment: SectionEnrolmentId,
    reason: &str,
) -> Result<(), KernelError> {
    let e = lock_enrolment(&mut c.tx, c.o.tenant_id, enrolment).await?;
    let already: i64 = sqlx::query_scalar("SELECT count(*) FROM academic.section_enrolment WHERE offering_id = $1 AND student_id = $2 AND status = 'enrolled'")
        .bind(to.id.as_uuid()).bind(e.student_id.as_uuid()).fetch_one(&mut *c.tx).await.map_err(map_db_error)?;
    let change = StatusChange {
        cause_event_type: ev::COURSE_OFFERING_REORGANISED,
        reason: Some(reason),
        process_instance_id: None,
    };
    if already > 0 {
        change_enrolment_status(
            &mut c.tx,
            &e,
            SectionEnrolmentStatus::Withdrawn,
            &change,
            &c.actor,
        )
        .await?;
        return Ok(());
    }
    let old = change_enrolment_status(
        &mut c.tx,
        &e,
        SectionEnrolmentStatus::Transferred,
        &change,
        &c.actor,
    )
    .await?;
    let basis = SeatBasis {
        student: StudentSummary {
            id: old.student_id,
            student_number: old.student_number.clone(),
            display_name: old.display_name.clone(),
            status: StudentStatus::Active,
            subject_ref: old.subject_ref.clone(),
        },
        context: StudentContextSummary {
            id: old.student_context_id,
            student_id: old.student_id,
            institution_id: to.institution_id,
            status: StudentContextStatus::Active,
            programme_id: None,
            curriculum_version_id: None,
            campus_id: None,
            current_term_no: None,
        },
        origin: "transfer",
        registration_id: old.registration_id,
        registration_line_id: old.registration_line_id,
        reason: Some(reason.to_owned()),
    };
    let scope = c.scope;
    let actor = c.actor.clone();
    insert_seat(
        &mut c.tx,
        &scope,
        to,
        to_section,
        &basis,
        "transfer",
        Some(old.id),
        &actor,
    )
    .await?;
    Ok(())
}

/// Moves a section of `c.o` into offering `to` as a new section (seats + active faculty move;
/// the old section becomes merged / cancelled).
async fn move_section(
    c: &mut Cmd,
    s: &SectionRow,
    to: &OfferingRow,
    code: &str,
    reason: &str,
) -> Result<SectionRow, KernelError> {
    let new = insert_section_row(&mut c.tx, to, code, s.capacity, s.campus_id, s.status).await?;
    let seats: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM academic.section_enrolment WHERE section_id = $1 AND status = 'enrolled' ORDER BY student_number")
        .bind(s.id.as_uuid()).fetch_all(&mut *c.tx).await.map_err(map_db_error)?;
    for e in seats {
        move_seat(c, to, &new, SectionEnrolmentId(e), reason).await?;
    }
    let rows = sqlx::query(&format!("SELECT {ASSIGNMENT_COLUMNS} FROM academic.faculty_assignment WHERE section_id = $1 AND status = 'active'"))
        .bind(s.id.as_uuid()).fetch_all(&mut *c.tx).await.map_err(map_db_error)?;
    for r in &rows {
        let a = assignment_from_row(r).map_err(map_db_error)?;
        let a = lock_assignment(&mut c.tx, c.o.tenant_id, a.id).await?;
        let ended = change_assignment_status(
            &mut c.tx,
            &a,
            FacultyAssignmentStatus::Completed,
            &StatusChange {
                cause_event_type: ev::COURSE_OFFERING_REORGANISED,
                reason: Some(reason),
                process_instance_id: None,
            },
            &c.actor,
        )
        .await?;
        let scope = c.scope;
        let actor = c.actor.clone();
        emit_assignment_ended(&mut c.tx, &scope, &ended, &actor).await?;
        let nr = sqlx::query(&format!(
            "INSERT INTO academic.faculty_assignment (tenant_id, institution_id, offering_id, section_id, faculty_subject_ref, \
             faculty_name, staff_id, role, weekly_hours, effective_from, effective_to, status, status_reason) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, 'active', $12) RETURNING {ASSIGNMENT_COLUMNS}"
        ))
        .bind(to.tenant_id.as_uuid()).bind(to.institution_id.as_uuid()).bind(to.id.as_uuid()).bind(new.id.as_uuid())
        .bind(&a.faculty_subject_ref).bind(&a.faculty_name).bind(a.staff_id).bind(a.role.as_str()).bind(a.weekly_hours)
        .bind(a.effective_from).bind(a.effective_to).bind(reason)
        .fetch_one(&mut *c.tx).await.map_err(map_db_error)?;
        let na = assignment_from_row(&nr).map_err(map_db_error)?;
        publish::event(
            &mut c.tx,
            &scope,
            ev::FACULTY_ASSIGNMENT_ACTIVATED,
            ev::FACULTY_ASSIGNMENT_ACTIVATED_SCHEMA,
            na.id.to_string(),
            &super::lifecycle::assignment_v1(&na),
            &actor,
        )
        .await?;
    }
    let old = lock_section(&mut c.tx, c.o.tenant_id, s.id).await?;
    let change = StatusChange {
        cause_event_type: ev::COURSE_OFFERING_REORGANISED,
        reason: Some(reason),
        process_instance_id: None,
    };
    if old.status == SectionStatus::Active {
        sqlx::query("UPDATE academic.offering_section SET merged_into_id = $2 WHERE id = $1")
            .bind(old.id.as_uuid())
            .bind(new.id.as_uuid())
            .execute(&mut *c.tx)
            .await
            .map_err(map_db_error)?;
        let old = lock_section(&mut c.tx, c.o.tenant_id, s.id).await?;
        change_section_status(&mut c.tx, &old, SectionStatus::Merged, &change, &c.actor).await?;
    } else if old.status == SectionStatus::Planned {
        change_section_status(&mut c.tx, &old, SectionStatus::Cancelled, &change, &c.actor).await?;
    }
    Ok(new)
}

fn free_code(base: &str, taken: &[String]) -> String {
    if !taken.iter().any(|t| t.eq_ignore_ascii_case(base)) {
        return base.to_owned();
    }
    for k in 1..1000 {
        let mut cand = format!("M{k}-{base}");
        cand.truncate(20);
        if !taken.iter().any(|t| t.eq_ignore_ascii_case(&cand)) {
            return cand;
        }
    }
    format!("M-{}", &Uuid::now_v7().simple().to_string()[..8])
}

async fn live_codes(
    conn: &mut PgConnection,
    offering: CourseOfferingId,
) -> Result<Vec<String>, KernelError> {
    sqlx::query_scalar("SELECT section_code FROM academic.offering_section WHERE offering_id = $1 AND status NOT IN ('cancelled', 'merged')")
        .bind(offering.as_uuid()).fetch_all(&mut *conn).await.map_err(map_db_error)
}

/// Applies a merge: `c.o` is the target.
async fn apply_merge(c: &mut Cmd, sources: &[Uuid], reason: &str) -> Result<(), KernelError> {
    if !matches!(
        c.o.status,
        CourseOfferingStatus::Active
            | CourseOfferingStatus::EnrollmentClosed
            | CourseOfferingStatus::EnrollmentOpen
    ) {
        return Err(conflict(format!(
            "the target offering is {}; it cannot absorb others",
            c.o.status
        )));
    }
    let target = c.o.clone();
    let mut capacity = target.capacity;
    for sid in sources {
        let src = lock_offering(&mut c.tx, c.o.tenant_id, CourseOfferingId(*sid)).await?;
        if src.institution_id != target.institution_id
            || src.academic_term_id != target.academic_term_id
            || src.curriculum_component_id != target.curriculum_component_id
        {
            return Err(invalid(
                "sourceOfferingIds",
                "incompatible",
                format!(
                    "{} is not the same course in the same term",
                    src.offering_code
                ),
            ));
        }
        if src.status != CourseOfferingStatus::Active {
            return Err(conflict(format!(
                "{} is {}; only active offerings merge",
                src.offering_code, src.status
            )));
        }
        let mut taken = live_codes(&mut c.tx, target.id).await?;
        let sections = sections_of(&mut c.tx, src.tenant_id, src.id).await?;
        let original = std::mem::replace(&mut c.o, src.clone());
        for s in sections
            .iter()
            .filter(|s| matches!(s.status, SectionStatus::Active | SectionStatus::Planned))
        {
            let code = free_code(&s.section_code, &taken);
            taken.push(code.clone());
            move_section(c, s, &target, &code, reason).await?;
        }
        clear_waitlist(c, "offering merged").await?;
        sqlx::query("UPDATE academic.course_offering SET merged_into_id = $2 WHERE id = $1")
            .bind(src.id.as_uuid())
            .bind(target.id.as_uuid())
            .execute(&mut *c.tx)
            .await
            .map_err(map_db_error)?;
        let src = lock_offering(&mut c.tx, src.tenant_id, src.id).await?;
        let src = change_offering_status(
            &mut c.tx,
            &src,
            CourseOfferingStatus::Merged,
            &StatusChange {
                cause_event_type: ev::COURSE_OFFERING_REORGANISED,
                reason: Some(reason),
                process_instance_id: None,
            },
            &c.actor,
        )
        .await?;
        let data = modified(
            src.id.as_uuid(),
            &src,
            None,
            src.version.0,
            &["status", "mergedIntoId"],
        );
        offering_event(
            &mut c.tx,
            &c.scope,
            &src,
            ev::COURSE_OFFERING_REORGANISED,
            ev::COURSE_OFFERING_REORGANISED_SCHEMA,
            &data,
            &c.actor,
        )
        .await?;
        c.o = original;
        capacity = match (capacity, src.capacity) {
            (Some(a), Some(b)) => Some(a.saturating_add(b)),
            _ => None,
        };
    }
    sqlx::query("UPDATE academic.course_offering SET capacity = $2, version = version + 1, updated_at = now() WHERE id = $1")
        .bind(target.id.as_uuid()).bind(capacity).execute(&mut *c.tx).await.map_err(map_db_error)?;
    c.o = lock_offering(&mut c.tx, target.tenant_id, target.id).await?;
    let data = modified(
        c.o.id.as_uuid(),
        &c.o,
        None,
        c.o.version.0,
        &["sections", "capacity"],
    );
    offering_event(
        &mut c.tx,
        &c.scope,
        &c.o,
        ev::COURSE_OFFERING_REORGANISED,
        ev::COURSE_OFFERING_REORGANISED_SCHEMA,
        &data,
        &c.actor,
    )
    .await
}

/// Applies a split: `c.o` is the source. Returns the successor offering ids.
async fn apply_split(
    c: &mut Cmd,
    parts: &[OfferingSplitPart],
    reason: &str,
) -> Result<Vec<Uuid>, KernelError> {
    if c.o.status != CourseOfferingStatus::Active {
        return Err(conflict(format!(
            "the course offering is {}; only active offerings split",
            c.o.status
        )));
    }
    let source = c.o.clone();
    let sections = sections_of(&mut c.tx, source.tenant_id, source.id).await?;
    let live: Vec<&SectionRow> = sections
        .iter()
        .filter(|s| matches!(s.status, SectionStatus::Active | SectionStatus::Planned))
        .collect();
    validate_split(
        parts,
        &live.iter().map(|s| s.id.as_uuid()).collect::<Vec<_>>(),
    )?;
    let mut results = Vec::new();
    for p in parts {
        let code = check_code("parts.offeringCode", &p.offering_code, 40, true)?;
        let title = match &p.title {
            Some(t) => check_text("parts.title", t, 200)?,
            None => source.title.clone(),
        };
        let mine: Vec<&&SectionRow> = live
            .iter()
            .filter(|s| p.section_ids.contains(&s.id.as_uuid()))
            .collect();
        let capacity = mine
            .iter()
            .try_fold(0i32, |acc, s| s.capacity.map(|c| acc.saturating_add(c)));
        let row = sqlx::query(&format!(
            "INSERT INTO academic.course_offering (tenant_id, institution_id, campus_id, org_unit_id, academic_year_id, \
             academic_term_id, curriculum_version_id, curriculum_component_id, offering_code, title, course_code, category, \
             credits, delivery_mode, capacity, status, split_from_id) \
             SELECT tenant_id, institution_id, campus_id, org_unit_id, academic_year_id, academic_term_id, curriculum_version_id, \
             curriculum_component_id, $2, $3, course_code, category, credits, delivery_mode, $4, 'active', id \
             FROM academic.course_offering WHERE id = $1 RETURNING {OFFERING_COLUMNS}"
        ))
        .bind(source.id.as_uuid()).bind(&code).bind(&title).bind(capacity)
        .fetch_one(&mut *c.tx).await
        .map_err(|e| super::lifecycle::unique_code(e, "parts.offeringCode", "an offering with this code is already live in the term"))?;
        let child = offering_from_row(&row).map_err(map_db_error)?;
        for s in mine {
            move_section(c, s, &child, &s.section_code, reason).await?;
        }
        let data = offering_v1(&child);
        offering_event(
            &mut c.tx,
            &c.scope,
            &child,
            ev::COURSE_OFFERING_ACTIVATED,
            ev::COURSE_OFFERING_ACTIVATED_SCHEMA,
            &data,
            &c.actor,
        )
        .await?;
        let data = modified(
            child.id.as_uuid(),
            &child,
            None,
            child.version.0,
            &["splitFromId", "sections"],
        );
        offering_event(
            &mut c.tx,
            &c.scope,
            &child,
            ev::COURSE_OFFERING_REORGANISED,
            ev::COURSE_OFFERING_REORGANISED_SCHEMA,
            &data,
            &c.actor,
        )
        .await?;
        results.push(child.id.as_uuid());
    }
    clear_waitlist(c, "offering split").await?;
    c.o = lock_offering(&mut c.tx, source.tenant_id, source.id).await?;
    c.o = change_offering_status(
        &mut c.tx,
        &c.o,
        CourseOfferingStatus::Split,
        &StatusChange {
            cause_event_type: ev::COURSE_OFFERING_REORGANISED,
            reason: Some(reason),
            process_instance_id: None,
        },
        &c.actor,
    )
    .await?;
    let data = modified(
        c.o.id.as_uuid(),
        &c.o,
        None,
        c.o.version.0,
        &["status", "sections"],
    );
    offering_event(
        &mut c.tx,
        &c.scope,
        &c.o,
        ev::COURSE_OFFERING_REORGANISED,
        ev::COURSE_OFFERING_REORGANISED_SCHEMA,
        &data,
        &c.actor,
    )
    .await?;
    Ok(results)
}

/// A split plan covers every live section exactly once with ≥ 2 non-empty parts and distinct codes.
pub fn validate_split(parts: &[OfferingSplitPart], live: &[Uuid]) -> Result<(), KernelError> {
    if parts.len() < 2 {
        return Err(invalid(
            "parts",
            "too_few",
            "a split creates at least two offerings",
        ));
    }
    let mut seen = std::collections::HashSet::new();
    let mut codes = std::collections::HashSet::new();
    for p in parts {
        if p.section_ids.is_empty() {
            return Err(invalid(
                "parts.sectionIds",
                "empty",
                "every part takes at least one section",
            ));
        }
        if !codes.insert(p.offering_code.trim().to_lowercase()) {
            return Err(invalid(
                "parts.offeringCode",
                "duplicate",
                "successor codes must differ",
            ));
        }
        for s in &p.section_ids {
            if !live.contains(s) {
                return Err(invalid(
                    "parts.sectionIds",
                    "unknown",
                    format!("{s} is not a live section of the offering"),
                ));
            }
            if !seen.insert(*s) {
                return Err(invalid(
                    "parts.sectionIds",
                    "duplicate",
                    format!("{s} is in two parts"),
                ));
            }
        }
    }
    if seen.len() != live.len() {
        return Err(invalid(
            "parts.sectionIds",
            "incomplete",
            "every live section must go to one part",
        ));
    }
    Ok(())
}

async fn set_reorg(
    conn: &mut PgConnection,
    r: &OfferingReorganisation,
    to: ChangeRequestStatus,
    decision_reason: Option<&str>,
    decided_by: Option<&str>,
    results: Option<&[Uuid]>,
) -> Result<OfferingReorganisation, KernelError> {
    let row = sqlx::query(&format!(
        "UPDATE academic.offering_reorganisation SET status = $2, decision_reason = COALESCE($3, decision_reason), \
         decided_by = COALESCE($4, decided_by), result_offering_ids = COALESCE($5, result_offering_ids), \
         applied_at = CASE WHEN $2 = 'applied' THEN now() ELSE applied_at END, version = version + 1, updated_at = now() \
         WHERE id = $1 AND version = $6 RETURNING {REORG_COLUMNS}"
    ))
    .bind(r.id).bind(to.as_str()).bind(decision_reason).bind(decided_by).bind(results.map(<[Uuid]>::to_vec)).bind(r.version)
    .fetch_optional(&mut *conn).await.map_err(map_db_error)?
    .ok_or_else(|| conflict("the reorganisation changed concurrently"))?;
    reorg_from_row(&row)
}

async fn approve_reorg(
    c: &mut Cmd,
    r: OfferingReorganisation,
    decider: &Actor,
    reason: Option<&str>,
) -> Result<OfferingReorganisation, KernelError> {
    let user = std::mem::replace(&mut c.actor, decider.clone());
    let res = async {
        let r = set_reorg(
            &mut c.tx,
            &r,
            ChangeRequestStatus::Approved,
            reason,
            decider.id.as_deref(),
            None,
        )
        .await?;
        let results = if r.kind == "merge" {
            apply_merge(c, &r.source_offering_ids, &r.reason).await?;
            Vec::new()
        } else {
            let parts: Vec<OfferingSplitPart> = serde_json::from_value(r.plan["parts"].clone())
                .map_err(|e| KernelError::BadRequest(e.to_string()))?;
            apply_split(c, &parts, &r.reason).await?
        };
        let r = set_reorg(
            &mut c.tx,
            &r,
            ChangeRequestStatus::Applied,
            None,
            None,
            Some(&results),
        )
        .await?;
        c.audit(
            &format!("academic.course_offering.apply_{}", r.kind),
            serde_json::json!({"reorganisationId": r.id, "results": results}),
        )
        .await?;
        Ok::<_, KernelError>(r)
    }
    .await;
    c.actor = user;
    res
}

async fn submit_reorg(
    st: &AcademicState,
    ctx: &AuthContext,
    id: CourseOfferingId,
    kind: &str,
    sources: Vec<Uuid>,
    plan: serde_json::Value,
    reason: String,
) -> Result<OfferingReorganisation, KernelError> {
    let o = crate::shared::offering::load_offering(&st.pool, tenant(ctx), id).await?;
    if o.status.is_terminal() {
        return Err(conflict(format!("the course offering is {}", o.status)));
    }
    let outcome = request_approval(
        st,
        &crate::shared::scope::institution_scope(ctx, o.institution_id),
        process::COURSE_OFFERING_REORGANISATION,
        business_key(&format!("course_offering_{kind}"), o.id, o.version.0),
        BTreeMap::from([
            ("kind".to_owned(), serde_json::json!(kind)),
            ("sources".to_owned(), serde_json::json!(sources)),
        ]),
    )
    .await?;
    let mut c = begin(st, ctx, id, None).await?;
    let row = sqlx::query(&format!(
        "INSERT INTO academic.offering_reorganisation (tenant_id, institution_id, offering_id, kind, source_offering_ids, plan, \
         reason, process_instance_id, requested_by) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING {REORG_COLUMNS}"
    ))
    .bind(c.o.tenant_id.as_uuid()).bind(c.o.institution_id.as_uuid()).bind(c.o.id.as_uuid()).bind(kind)
    .bind(&sources).bind(&plan).bind(&reason).bind(outcome.process_instance_id()).bind(c.actor.id.as_deref())
    .fetch_one(&mut *c.tx).await.map_err(map_db_error)?;
    let mut r = reorg_from_row(&row)?;
    c.audit(
        &format!("academic.course_offering.request_{kind}"),
        serde_json::json!({"reorganisationId": r.id, "reason": reason}),
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        let system = Actor::system(c.actor.correlation_id.clone());
        r = approve_reorg(&mut c, r, &system, Some("auto-approved")).await?;
    }
    c.tx.commit().await.map_err(map_db_error)?;
    Ok(r)
}

/// Merge other active offerings of the same course and term into this one (L3-D-011,
/// `offering.lifecycle`, step-up). Seats and active faculty move to successor sections.
#[utoipa::path(
    post,
    path = "/api/v1/academic/offerings/{id}/merge",
    operation_id = "offering_merge",
    tag = "offerings",
    params(
        ("id" = String, Path, format = Uuid, description = "Target offering"),
        ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key."),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = OfferingMergeRequest,
    responses(
        (status = 201, body = OfferingReorganisation),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn merge(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<CourseOfferingId>,
    headers: HeaderMap,
    Json(cmd): Json<OfferingMergeRequest>,
) -> ApiResult<(StatusCode, Json<OfferingReorganisation>)> {
    let ctx = auth.require(Permission::OfferingLifecycle)?;
    require_idempotency_key(&headers)?;
    let reason = check_text("reason", &cmd.reason, 1000)?;
    let mut sources = cmd.source_offering_ids.clone();
    sources.sort();
    sources.dedup();
    if sources.is_empty() || sources.contains(&id.as_uuid()) {
        return Err(invalid(
            "sourceOfferingIds",
            "invalid",
            "list other offerings to merge into this one",
        )
        .into());
    }
    let r = submit_reorg(
        &st,
        ctx,
        id,
        "merge",
        sources,
        serde_json::json!({}),
        reason,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(r)))
}

/// Split an active offering into successor offerings (L3-D-011, `offering.lifecycle`, step-up).
#[utoipa::path(
    post,
    path = "/api/v1/academic/offerings/{id}/split",
    operation_id = "offering_split",
    tag = "offerings",
    params(
        ("id" = String, Path, format = Uuid),
        ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key."),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = OfferingSplitRequest,
    responses(
        (status = 201, body = OfferingReorganisation),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn split(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<CourseOfferingId>,
    headers: HeaderMap,
    Json(cmd): Json<OfferingSplitRequest>,
) -> ApiResult<(StatusCode, Json<OfferingReorganisation>)> {
    let ctx = auth.require(Permission::OfferingLifecycle)?;
    require_idempotency_key(&headers)?;
    let reason = check_text("reason", &cmd.reason, 1000)?;
    {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        let o = crate::shared::offering::load_offering(&mut *conn, tenant(ctx), id).await?;
        let live: Vec<Uuid> = sections_of(&mut conn, o.tenant_id, o.id)
            .await?
            .iter()
            .filter(|s| matches!(s.status, SectionStatus::Active | SectionStatus::Planned))
            .map(|s| s.id.as_uuid())
            .collect();
        validate_split(&cmd.parts, &live)?;
        for p in &cmd.parts {
            check_code("parts.offeringCode", &p.offering_code, 40, true)?;
        }
    }
    let plan = serde_json::json!({"parts": cmd.parts});
    let r = submit_reorg(&st, ctx, id, "split", Vec::new(), plan, reason).await?;
    Ok((StatusCode::CREATED, Json(r)))
}

/// Reorganisations requested on an offering (`academic.read`).
#[utoipa::path(
    get,
    path = "/api/v1/academic/offerings/{id}/reorganisations",
    operation_id = "offering_reorganisation_list",
    tag = "offerings",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses((status = 200, body = OfferingReorganisationList), (status = 404, body = Problem, content_type = "application/problem+json"))
)]
pub async fn reorganisation_list(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<CourseOfferingId>,
) -> ApiResult<Json<OfferingReorganisationList>> {
    let ctx = auth.require(Permission::AcademicRead)?;
    let o = crate::shared::offering::load_offering(&st.pool, tenant(ctx), id).await?;
    let rows = sqlx::query(&format!("SELECT {REORG_COLUMNS} FROM academic.offering_reorganisation WHERE offering_id = $1 OR $1 = ANY(source_offering_ids) ORDER BY created_at DESC"))
        .bind(o.id.as_uuid()).fetch_all(&st.pool).await.map_err(map_db_error)?;
    let items = rows.iter().map(reorg_from_row).collect::<Result<_, _>>()?;
    Ok(Json(OfferingReorganisationList { items }))
}

/// Decide a pending merge / split (`offering.lifecycle`, step-up).
#[utoipa::path(
    post,
    path = "/api/v1/academic/offerings/{id}/reorganisations/{reorganisationId}/decision",
    operation_id = "offering_reorganisation_decision",
    tag = "offerings",
    params(
        ("id" = String, Path, format = Uuid),
        ("reorganisationId" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag (version) of the reorganisation"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = StepDecisionBody,
    responses(
        (status = 200, body = OfferingReorganisation),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
        (status = 428, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn reorganisation_decision(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path((id, rid)): Path<(CourseOfferingId, Uuid)>,
    headers: HeaderMap,
    Json(d): Json<StepDecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::OfferingLifecycle)?;
    // Part 1 · S3-appr (DEF-X-002): the requester of the pending request never decides it.
    sutra_persistence::approval::guard_decision(&st.pool, ctx, &id, "offering reorganisation").await?;
    let v = expect_version!(headers);
    let reason = clean_reason(d.reason, d.decision == StepDecision::Reject)?;
    let mut c = begin(&st, ctx, id, None).await?;
    let row = sqlx::query(&format!("SELECT {REORG_COLUMNS} FROM academic.offering_reorganisation WHERE id = $1 AND offering_id = $2 FOR UPDATE"))
        .bind(rid).bind(c.o.id.as_uuid()).fetch_optional(&mut *c.tx).await.map_err(map_db_error)?
        .ok_or(KernelError::NotFound { entity: "reorganisation" })?;
    let r = reorg_from_row(&row)?;
    check_version(Version(r.version), v)?;
    if !matches!(
        r.status,
        ChangeRequestStatus::Requested | ChangeRequestStatus::Validated
    ) {
        return Err(conflict(format!(
            "the reorganisation is {}; nothing awaits a decision",
            r.status
        ))
        .into());
    }
    let decider = c.actor.clone();
    let r = match d.decision {
        StepDecision::Approve => approve_reorg(&mut c, r, &decider, reason.as_deref()).await?,
        StepDecision::Reject => {
            let r = set_reorg(
                &mut c.tx,
                &r,
                ChangeRequestStatus::Rejected,
                reason.as_deref(),
                decider.id.as_deref(),
                None,
            )
            .await?;
            c.audit(
                &format!("academic.course_offering.reject_{}", r.kind),
                serde_json::json!({"reorganisationId": r.id, "reason": reason}),
            )
            .await?;
            r
        }
    };
    c.tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &r, Version(r.version)))
}

/// Keeps helper imports referenced (row decoders used by sibling modules in tests).
#[allow(dead_code)]
fn _decoders(r: &PgRow) -> Result<(), sqlx::Error> {
    let _ = enrolment_from_row(r)?;
    let _ = ENROLMENT_COLUMNS;
    Ok(())
}

/// Offering-level ETag response for sibling handlers.
#[allow(dead_code)]
fn _etag(o: &Offering) -> Response {
    etag(StatusCode::OK, o)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn change_request_machine() {
        use ChangeRequestStatus::*;
        assert!(change_can_transition(Requested, Approved));
        assert!(change_can_transition(Approved, Applied));
        assert!(change_can_transition(Requested, Withdrawn));
        assert!(!change_can_transition(Applied, Requested));
        assert!(!change_can_transition(Rejected, Approved));
    }

    #[test]
    fn split_plans() {
        let a = Uuid::now_v7();
        let b = Uuid::now_v7();
        let part = |code: &str, ids: Vec<Uuid>| OfferingSplitPart {
            offering_code: code.into(),
            title: None,
            section_ids: ids,
        };
        assert!(validate_split(&[part("X", vec![a]), part("Y", vec![b])], &[a, b]).is_ok());
        assert!(validate_split(&[part("X", vec![a, b])], &[a, b]).is_err());
        assert!(validate_split(&[part("X", vec![a]), part("Y", vec![a])], &[a, b]).is_err());
        assert!(validate_split(&[part("X", vec![a]), part("X", vec![b])], &[a, b]).is_err());
        assert!(validate_split(&[part("X", vec![a]), part("Y", vec![])], &[a, b]).is_err());
        assert_eq!(free_code("A", &["B".into()]), "A");
        assert_eq!(free_code("A", &["a".into()]), "M1-A");
    }
}
