//! Academic monitoring cases (L3-D-027…033): case → risk assessment → outcome → governance
//! review/decision → intervention → certification → closure → reopening (new case).

use super::domain;
use crate::shared::approval::{self, business_key, process, ApprovalOutcome};
use crate::shared::guard::require_institution_writable;
use crate::shared::scope::institution_scope;
use crate::shared::{AcademicState, Actor};
use crate::timetable::kit::{self, check_version, illegal, invalid, required_text, StatusEvent};
use chrono::{DateTime, NaiveDate, Utc};
use serde_json::{json, Value};
use sqlx::PgConnection;
use std::collections::BTreeMap;
use sutra_contracts::academic::events::delivery::*;
use sutra_contracts::academic::ids::{CourseOfferingId, OfferingSectionId};
use sutra_contracts::academic::status::{InterventionStatus, MonitoringCaseStatus, RiskLevel};
use sutra_kernel::auth::AuthContext;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

// ------------------------------------------------------------------ DTOs

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RiskAssessment {
    pub id: Uuid,
    pub case_id: Uuid,
    pub category: String,
    pub level: RiskLevel,
    pub priority: i16,
    /// `attendance_rule` (derived from finalised attendance) | `reviewer`.
    pub basis: String,
    #[schema(value_type = Object)]
    pub indicators: Value,
    pub rationale: Option<String>,
    pub source_key: Option<String>,
    pub assessed_by: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MonitoringException {
    pub id: Uuid,
    pub case_id: Uuid,
    pub category: String,
    pub severity: RiskLevel,
    pub description: String,
    pub evidence: Option<String>,
    pub owner_subject: String,
    pub reviewer_subject: String,
    pub resolution_plan: Option<String>,
    pub resolution_notes: Option<String>,
    /// `raised` | `planned` | `resolved` | `closed` | `escalated` | `withdrawn`.
    pub status: String,
    pub status_reason: Option<String>,
    pub validated_by: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MonitoringDecision {
    pub id: Uuid,
    pub case_id: Uuid,
    /// `governance` | `closure` | `reopening`.
    pub kind: String,
    /// `approve` | `reject`.
    pub decision: String,
    pub requires_intervention: bool,
    pub reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub decided_by: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MonitoringIntervention {
    pub id: Uuid,
    pub case_id: Uuid,
    pub decision_id: Uuid,
    pub title: String,
    pub plan: Option<String>,
    pub owner_subject: Option<String>,
    pub due_on: Option<NaiveDate>,
    pub status: InterventionStatus,
    pub status_reason: Option<String>,
    pub outcome: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MonitoringCase {
    pub id: Uuid,
    pub institution_id: Uuid,
    /// `section` | `offering` | `student`.
    pub scope_kind: String,
    pub offering_id: Option<Uuid>,
    pub section_id: Option<Uuid>,
    pub student_id: Option<Uuid>,
    pub title: String,
    #[schema(value_type = Object)]
    pub plan: Value,
    pub owner_subject: String,
    pub reviewer_subject: Option<String>,
    pub status: MonitoringCaseStatus,
    pub status_reason: Option<String>,
    pub status_changed_at: DateTime<Utc>,
    pub pending_decision: Option<String>,
    pub process_instance_id: Option<String>,
    pub outcome_summary: Option<String>,
    #[schema(value_type = Option<Object>)]
    pub outcome_snapshot: Option<Value>,
    pub outcome_published_at: Option<DateTime<Utc>>,
    pub certified_at: Option<DateTime<Utc>>,
    pub closed_at: Option<DateTime<Utc>>,
    #[schema(value_type = Option<Object>)]
    pub closure_snapshot: Option<Value>,
    pub cycle_no: i32,
    pub reopened_from_id: Option<Uuid>,
    pub reopened_as_id: Option<Uuid>,
    pub source_key: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub risk_assessments: Vec<RiskAssessment>,
    #[serde(default)]
    pub exceptions: Vec<MonitoringException>,
    #[serde(default)]
    pub interventions: Vec<MonitoringIntervention>,
    #[serde(default)]
    pub decisions: Vec<MonitoringDecision>,
    #[serde(skip_serializing, default)]
    pending_payload: Option<Value>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct MonitoringCaseList {
    pub items: Vec<MonitoringCase>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewMonitoringCase {
    /// `section` | `offering` | `student`.
    pub scope_kind: String,
    pub section_id: Option<Uuid>,
    pub offering_id: Option<Uuid>,
    pub student_id: Option<Uuid>,
    pub title: String,
    #[schema(value_type = Option<Object>)]
    pub plan: Option<Value>,
    /// Defaults to the caller.
    pub owner_subject: Option<String>,
    pub reviewer_subject: Option<String>,
    /// Authoritative fact that triggered the case; a repeat returns the existing case.
    pub source_key: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewRiskAssessment {
    pub category: String,
    /// Used only when no attendance evidence exists (reviewer judgement).
    pub level: Option<RiskLevel>,
    pub rationale: Option<String>,
    pub source_key: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PublishOutcome {
    pub summary: String,
}

#[derive(Debug, Clone, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GovernanceReview {
    pub reviewer_subject: Option<String>,
    pub rationale: Option<String>,
    #[serde(default)]
    pub requires_intervention: bool,
    pub intervention_title: Option<String>,
    pub intervention_plan: Option<String>,
    pub due_on: Option<NaiveDate>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewMonitoringException {
    pub category: String,
    pub severity: RiskLevel,
    pub description: String,
    pub evidence: Option<String>,
    pub owner_subject: String,
    pub reviewer_subject: String,
}

/// Body of exception / intervention commands (fields used by the command that needs them).
#[derive(Debug, Clone, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MonitoringStep {
    pub reason: Option<String>,
    pub resolution_plan: Option<String>,
    pub resolution_notes: Option<String>,
    pub owner_subject: Option<String>,
    pub due_on: Option<NaiveDate>,
    pub outcome: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReopenCase {
    pub reason: String,
    pub evidence: Option<String>,
}

const MC: &str = "monitoring_case";
const MC_ENTITY: &str = "monitoring case";

// ------------------------------------------------------------------ reads

pub async fn get(
    state: &AcademicState,
    tenant: TenantId,
    id: Uuid,
) -> Result<MonitoringCase, KernelError> {
    let c: MonitoringCase = kit::fetch(&state.pool, MC, MC_ENTITY, tenant, id).await?;
    hydrate(state, tenant, c).await
}

/// Attaches the case's risk assessments, exceptions, interventions and decisions.
async fn hydrate(
    state: &AcademicState,
    tenant: TenantId,
    mut c: MonitoringCase,
) -> Result<MonitoringCase, KernelError> {
    let id = c.id;
    c.risk_assessments = kit::list_by(
        &state.pool,
        "risk_assessment",
        tenant,
        "case_id",
        id,
        None,
        "t.created_at DESC",
        200,
    )
    .await?;
    c.exceptions = kit::list_by(
        &state.pool,
        "monitoring_exception",
        tenant,
        "case_id",
        id,
        None,
        "t.created_at",
        200,
    )
    .await?;
    c.interventions = kit::list_by(
        &state.pool,
        "monitoring_intervention",
        tenant,
        "case_id",
        id,
        None,
        "t.created_at",
        200,
    )
    .await?;
    c.decisions = kit::list_by(
        &state.pool,
        "monitoring_decision",
        tenant,
        "case_id",
        id,
        None,
        "t.created_at",
        200,
    )
    .await?;
    Ok(c)
}

pub async fn list(
    state: &AcademicState,
    tenant: TenantId,
    institution: Uuid,
    status: Option<&str>,
) -> Result<Vec<MonitoringCase>, KernelError> {
    let cases: Vec<MonitoringCase> = kit::list_by(
        &state.pool,
        MC,
        tenant,
        "institution_id",
        institution,
        status,
        "t.created_at DESC",
        200,
    )
    .await?;
    // The list shows each case's latest risk and drives its row actions, so it carries the
    // same children as a single read.
    let mut out = Vec::with_capacity(cases.len());
    for c in cases {
        out.push(hydrate(state, tenant, c).await?);
    }
    Ok(out)
}

// ------------------------------------------------------------------ helpers

struct Locked {
    c: MonitoringCase,
    scope: ExecutionScope,
}

async fn lock_case(
    state: &AcademicState,
    conn: &mut PgConnection,
    auth: &AuthContext,
    id: Uuid,
    expected: Option<Version>,
) -> Result<Locked, KernelError> {
    let c: MonitoringCase = kit::lock(conn, MC, MC_ENTITY, auth.scope.tenant_id, id).await?;
    if let Some(v) = expected {
        check_version(c.version, v)?;
    }
    let scope = institution_scope(auth, InstitutionId(c.institution_id));
    require_institution_writable(state, &scope, InstitutionId(c.institution_id)).await?;
    Ok(Locked { c, scope })
}

async fn move_case(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    c: &MonitoringCase,
    to: MonitoringCaseStatus,
    reason: Option<&str>,
    extra: Value,
) -> Result<MonitoringCase, KernelError> {
    if c.status != to && !domain::can_transition(c.status, to) {
        return Err(illegal("monitoring case", c.status, to));
    }
    let mut changes =
        json!({"status": to.as_str(), "status_reason": reason, "status_changed_at": Utc::now()});
    if let (Some(m), Some(e)) = (changes.as_object_mut(), extra.as_object()) {
        for (k, v) in e {
            m.insert(k.clone(), v.clone());
        }
    }
    kit::patch(conn, MC, scope.tenant_id, c.id, changes).await
}

async fn emit(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    c: &MonitoringCase,
    from: Option<&str>,
    ev: (&'static str, &'static str),
    reason: Option<&str>,
    actor: &Actor,
) -> Result<(), KernelError> {
    StatusEvent {
        aggregate_id: c.id,
        institution_id: c.institution_id,
        offering_id: c.offering_id,
        section_id: c.section_id,
        student_id: c.student_id,
        from,
        to: c.status.as_str(),
        version: c.version,
        reason,
        process_instance_id: c.process_instance_id.as_deref(),
        ..Default::default()
    }
    .emit(conn, scope, ev, actor)
    .await
}

fn no_pending(c: &MonitoringCase) -> Result<(), KernelError> {
    match &c.pending_decision {
        Some(k) => Err(KernelError::Conflict(format!(
            "the case is waiting for a {k} decision"
        ))),
        None => Ok(()),
    }
}

#[allow(clippy::too_many_arguments)]
async fn record_decision(
    conn: &mut PgConnection,
    tenant: TenantId,
    c: &MonitoringCase,
    kind: &str,
    approve: bool,
    requires_intervention: bool,
    reason: Option<&str>,
    payload: Option<&Value>,
    actor: &Actor,
) -> Result<MonitoringDecision, KernelError> {
    kit::insert(conn, "monitoring_decision", json!({
        "tenant_id": tenant.as_uuid(), "case_id": c.id, "kind": kind, "decision": if approve { "approve" } else { "reject" },
        "requires_intervention": requires_intervention, "reason": reason, "payload": payload,
        "process_instance_id": c.process_instance_id, "decided_by": actor.id,
    }))
    .await
}

// ------------------------------------------------------------------ open

pub async fn open(
    state: &AcademicState,
    auth: &AuthContext,
    institution: Uuid,
    cmd: NewMonitoringCase,
    actor: &Actor,
) -> Result<(MonitoringCase, bool), KernelError> {
    let tenant = auth.scope.tenant_id;
    let scope = institution_scope(auth, InstitutionId(institution));
    require_institution_writable(state, &scope, InstitutionId(institution)).await?;
    let title = required_text("title", Some(&cmd.title))?;
    let source_key = kit::opt_text(cmd.source_key.as_deref());
    if let Some(k) = &source_key {
        let existing: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM academic.monitoring_case WHERE tenant_id = $1 AND source_key = $2",
        )
        .bind(tenant.as_uuid())
        .bind(k)
        .fetch_optional(&state.pool)
        .await
        .map_err(map_db_error)?;
        if let Some(id) = existing {
            return Ok((get(state, tenant, id).await?, false));
        }
    }
    let tscope = ExecutionScope::tenant(tenant);
    let (offering_id, section_id) = match cmd.scope_kind.as_str() {
        "section" | "student" => {
            let sid = cmd.section_id.ok_or_else(|| {
                invalid(
                    "sectionId",
                    "required",
                    "a section is required for this scope",
                )
            })?;
            let sec = state
                .offerings
                .section(&tscope, OfferingSectionId(sid))
                .await?
                .filter(|s| s.institution_id.as_uuid() == institution)
                .ok_or_else(|| {
                    invalid(
                        "sectionId",
                        "not_found",
                        "no such section in this institution",
                    )
                })?;
            (Some(sec.offering_id.as_uuid()), Some(sid))
        }
        "offering" => {
            let oid = cmd.offering_id.ok_or_else(|| {
                invalid(
                    "offeringId",
                    "required",
                    "an offering is required for this scope",
                )
            })?;
            state
                .offerings
                .offering(&tscope, CourseOfferingId(oid))
                .await?
                .filter(|o| o.institution_id.as_uuid() == institution)
                .ok_or_else(|| {
                    invalid(
                        "offeringId",
                        "not_found",
                        "no such offering in this institution",
                    )
                })?;
            (Some(oid), None)
        }
        _ => {
            return Err(invalid(
                "scopeKind",
                "enum",
                "scopeKind is section, offering or student",
            ))
        }
    };
    let student_id = if cmd.scope_kind == "student" {
        let st = cmd.student_id.ok_or_else(|| {
            invalid(
                "studentId",
                "required",
                "a student is required for this scope",
            )
        })?;
        let roster = state
            .offerings
            .roster(
                &tscope,
                OfferingSectionId(section_id.unwrap_or_default()),
                false,
            )
            .await?;
        if !roster.iter().any(|r| r.student_id.as_uuid() == st) {
            return Err(invalid(
                "studentId",
                "not_enrolled",
                "the student is not on the section roster",
            ));
        }
        Some(st)
    } else {
        None
    };
    let owner = kit::opt_text(cmd.owner_subject.as_deref())
        .or_else(|| actor.id.clone())
        .unwrap_or_default();
    let reviewer = kit::opt_text(cmd.reviewer_subject.as_deref());
    if reviewer.as_deref() == Some(owner.as_str()) {
        return Err(invalid(
            "reviewerSubject",
            "independence",
            "the reviewer must be independent of the owner",
        ));
    }
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let c: MonitoringCase = kit::insert(&mut tx, MC, json!({
        "tenant_id": tenant.as_uuid(), "institution_id": institution, "scope_kind": cmd.scope_kind,
        "offering_id": offering_id, "section_id": section_id, "student_id": student_id, "title": title,
        "plan": cmd.plan.unwrap_or_else(|| json!({})), "owner_subject": owner, "reviewer_subject": reviewer,
        "source_key": source_key,
    })).await?;
    emit(
        &mut tx,
        &scope,
        &c,
        None,
        (MONITORING_CASE_OPENED, MONITORING_CASE_OPENED_SCHEMA),
        None,
        actor,
    )
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        MC,
        "open",
        c.id,
        json!({"scopeKind": c.scope_kind}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok((get(state, tenant, c.id).await?, true))
}

// ------------------------------------------------------------------ risk + outcome

/// Attendance evidence of the case scope: (level, indicators) or `None` when no finalised attendance.
async fn attendance_evidence(
    state: &AcademicState,
    auth: &AuthContext,
    c: &MonitoringCase,
) -> Result<Option<(RiskLevel, Value)>, KernelError> {
    let Some(section) = c.section_id else {
        return Ok(None);
    };
    let s = crate::attendance::repo::summary(state, auth, section).await?;
    if s.finalised_sessions == 0 {
        return Ok(None);
    }
    if let Some(student) = c.student_id {
        let Some(line) = s.items.iter().find(|i| i.student_id == student) else {
            return Ok(None);
        };
        if line.total == 0 {
            return Ok(None);
        }
        let level = domain::student_attendance_risk(line.percent, s.threshold_percent);
        return Ok(Some((
            level,
            json!({"attendancePercent": line.percent, "attended": line.attended, "sessions": line.total, "thresholdPercent": s.threshold_percent}),
        )));
    }
    let below = s.items.iter().filter(|i| i.below_threshold).count();
    let level = domain::section_attendance_risk(below, s.items.len());
    Ok(Some((
        level,
        json!({"studentsBelowThreshold": below, "students": s.items.len(), "finalisedSessions": s.finalised_sessions, "thresholdPercent": s.threshold_percent}),
    )))
}

pub async fn assess_risk(
    state: &AcademicState,
    auth: &AuthContext,
    id: Uuid,
    cmd: NewRiskAssessment,
    actor: &Actor,
) -> Result<MonitoringCase, KernelError> {
    let tenant = auth.scope.tenant_id;
    let category = required_text("category", Some(&cmd.category))?;
    let source_key = kit::opt_text(cmd.source_key.as_deref());
    let cur: MonitoringCase = kit::fetch(&state.pool, MC, MC_ENTITY, tenant, id).await?;
    let evidence = attendance_evidence(state, auth, &cur).await?;
    let (level, basis, indicators) = match (evidence, cmd.level) {
        (Some((l, ind)), _) => (l, "attendance_rule", ind),
        (None, Some(l)) => {
            if kit::opt_text(cmd.rationale.as_deref()).is_none() {
                return Err(invalid("rationale", "required", "a reviewer-set risk level needs a rationale"));
            }
            (l, "reviewer", json!({}))
        }
        (None, None) => {
            return Err(KernelError::Conflict("no finalised attendance evidence exists for the case scope; give a reviewer level and rationale".into()))
        }
    };
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let Locked { c, scope } = lock_case(state, &mut tx, auth, id, None).await?;
    if matches!(
        c.status,
        MonitoringCaseStatus::Certified | MonitoringCaseStatus::Closed
    ) {
        return Err(KernelError::Conflict(format!(
            "a {} case takes no new assessments",
            c.status
        )));
    }
    if let Some(k) = &source_key {
        let dup: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM academic.risk_assessment WHERE case_id = $1 AND source_key = $2",
        )
        .bind(id)
        .bind(k)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_db_error)?;
        if dup.is_some() {
            tx.rollback().await.map_err(map_db_error)?;
            return get(state, tenant, id).await;
        }
    }
    let ra: RiskAssessment = kit::insert(&mut tx, "risk_assessment", json!({
        "tenant_id": tenant.as_uuid(), "case_id": id, "category": category, "level": level.as_str(),
        "priority": domain::priority(level), "basis": basis, "indicators": indicators,
        "rationale": kit::opt_text(cmd.rationale.as_deref()), "source_key": source_key, "assessed_by": actor.id,
    })).await?;
    let from = c.status;
    let nc = if matches!(
        c.status,
        MonitoringCaseStatus::Open | MonitoringCaseStatus::Reopened
    ) {
        move_case(
            &mut tx,
            &scope,
            &c,
            MonitoringCaseStatus::UnderAssessment,
            None,
            json!({}),
        )
        .await?
    } else {
        kit::patch(&mut tx, MC, tenant, id, json!({})).await?
    };
    emit(
        &mut tx,
        &scope,
        &nc,
        Some(from.as_str()),
        (MONITORING_RISK_ASSESSED, MONITORING_RISK_ASSESSED_SCHEMA),
        Some(level.as_str()),
        actor,
    )
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        MC,
        "assess_risk",
        id,
        json!({"riskAssessmentId": ra.id, "level": level, "basis": basis}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get(state, tenant, id).await
}

pub async fn publish_outcome(
    state: &AcademicState,
    auth: &AuthContext,
    id: Uuid,
    expected: Version,
    cmd: PublishOutcome,
    actor: &Actor,
) -> Result<MonitoringCase, KernelError> {
    let tenant = auth.scope.tenant_id;
    let summary = required_text("summary", Some(&cmd.summary))?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let Locked { c, scope } = lock_case(state, &mut tx, auth, id, Some(expected)).await?;
    if c.outcome_published_at.is_some() {
        return Err(KernelError::Conflict(
            "the monitoring outcome of this cycle is already published".into(),
        ));
    }
    if !matches!(
        c.status,
        MonitoringCaseStatus::UnderAssessment
            | MonitoringCaseStatus::UnderReview
            | MonitoringCaseStatus::Intervention
    ) {
        return Err(KernelError::Conflict(format!(
            "a {} case has no outcome to publish",
            c.status
        )));
    }
    let risks: Vec<RiskAssessment> = kit::list_by(
        &mut *tx,
        "risk_assessment",
        tenant,
        "case_id",
        id,
        None,
        "t.created_at DESC",
        50,
    )
    .await?;
    let Some(latest) = risks.first() else {
        return Err(KernelError::Conflict(
            "assess the risk before publishing an outcome".into(),
        ));
    };
    let snapshot = json!({"riskAssessmentId": latest.id, "level": latest.level, "category": latest.category, "indicators": latest.indicators, "assessments": risks.len(), "takenAt": Utc::now()});
    kit::patch::<MonitoringCase>(&mut tx, MC, tenant, id, json!({"outcome_summary": summary, "outcome_snapshot": snapshot, "outcome_published_at": Utc::now()})).await?;
    kit::audit(
        &mut tx,
        &scope,
        MC,
        "publish_outcome",
        id,
        json!({"level": latest.level}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get(state, tenant, id).await
}

// ------------------------------------------------------------------ governance review + decision

pub async fn submit_review(
    state: &AcademicState,
    auth: &AuthContext,
    id: Uuid,
    expected: Version,
    cmd: GovernanceReview,
    actor: &Actor,
) -> Result<MonitoringCase, KernelError> {
    let tenant = auth.scope.tenant_id;
    let cur: MonitoringCase = kit::fetch(&state.pool, MC, MC_ENTITY, tenant, id).await?;
    check_version(cur.version, expected)?;
    no_pending(&cur)?;
    if !domain::can_transition(cur.status, MonitoringCaseStatus::UnderReview) {
        return Err(illegal(
            "monitoring case",
            cur.status,
            MonitoringCaseStatus::UnderReview,
        ));
    }
    let reviewer = kit::opt_text(cmd.reviewer_subject.as_deref())
        .or(cur.reviewer_subject.clone())
        .ok_or_else(|| {
            invalid(
                "reviewerSubject",
                "required",
                "an independent reviewer is required",
            )
        })?;
    if reviewer == cur.owner_subject {
        return Err(invalid(
            "reviewerSubject",
            "independence",
            "the reviewer must be independent of the case owner",
        ));
    }
    if cmd.requires_intervention && kit::opt_text(cmd.intervention_title.as_deref()).is_none() {
        return Err(invalid(
            "interventionTitle",
            "required",
            "a corrective intervention needs a title",
        ));
    }
    let scope = institution_scope(auth, InstitutionId(cur.institution_id));
    let mut vars = BTreeMap::new();
    vars.insert("caseId".to_owned(), json!(id));
    vars.insert("kind".to_owned(), json!("governance"));
    let outcome = approval::request_approval(
        state,
        &scope,
        process::MONITORING_DECISION,
        business_key(MC, id, cur.version),
        vars,
    )
    .await?;
    let payload = json!({
        "rationale": kit::opt_text(cmd.rationale.as_deref()), "requiresIntervention": cmd.requires_intervention,
        "interventionTitle": kit::opt_text(cmd.intervention_title.as_deref()), "interventionPlan": kit::opt_text(cmd.intervention_plan.as_deref()),
        "dueOn": cmd.due_on,
    });
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let Locked { c, scope } = lock_case(state, &mut tx, auth, id, Some(expected)).await?;
    let nc = move_case(&mut tx, &scope, &c, MonitoringCaseStatus::UnderReview, None, json!({
        "reviewer_subject": reviewer, "pending_decision": "governance", "pending_payload": payload,
        "process_instance_id": outcome.process_instance_id(),
    })).await?;
    kit::audit(
        &mut tx,
        &scope,
        MC,
        "submit_review",
        id,
        json!({"reviewer": reviewer}),
        actor,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        apply_decision(
            &mut tx,
            &scope,
            nc,
            true,
            None,
            &Actor::system(actor.correlation_id.clone()),
        )
        .await?;
    }
    tx.commit().await.map_err(map_db_error)?;
    get(state, tenant, id).await
}

async fn apply_decision(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    c: MonitoringCase,
    approve: bool,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<MonitoringCase, KernelError> {
    let tenant = scope.tenant_id;
    let kind = c
        .pending_decision
        .clone()
        .ok_or_else(|| KernelError::Conflict("the case has no pending decision".into()))?;
    let payload = c.pending_payload.clone().unwrap_or_else(|| json!({}));
    let clear = json!({"pending_decision": null, "pending_payload": null});
    match kind.as_str() {
        "governance" => {
            let requires = approve && payload["requiresIntervention"].as_bool().unwrap_or(false);
            let d = record_decision(
                conn,
                tenant,
                &c,
                "governance",
                approve,
                requires,
                reason,
                Some(&payload),
                actor,
            )
            .await?;
            let nc = if requires {
                let iv: MonitoringIntervention = kit::insert(conn, "monitoring_intervention", json!({
                    "tenant_id": tenant.as_uuid(), "case_id": c.id, "decision_id": d.id,
                    "title": payload["interventionTitle"].as_str().unwrap_or("Corrective intervention"),
                    "plan": payload["interventionPlan"], "due_on": payload["dueOn"],
                })).await?;
                kit::audit(
                    conn,
                    scope,
                    "monitoring_intervention",
                    "create",
                    iv.id,
                    json!({"decisionId": d.id}),
                    actor,
                )
                .await?;
                move_case(
                    conn,
                    scope,
                    &c,
                    MonitoringCaseStatus::Intervention,
                    reason,
                    clear,
                )
                .await?
            } else {
                move_case(
                    conn,
                    scope,
                    &c,
                    MonitoringCaseStatus::UnderAssessment,
                    reason,
                    clear,
                )
                .await?
            };
            kit::audit(
                conn,
                scope,
                MC,
                if approve {
                    "governance_approve"
                } else {
                    "governance_reject"
                },
                c.id,
                json!({"decisionId": d.id, "requiresIntervention": requires}),
                actor,
            )
            .await?;
            Ok(nc)
        }
        "closure" => {
            let d = record_decision(
                conn, tenant, &c, "closure", approve, false, reason, None, actor,
            )
            .await?;
            let nc = if approve {
                move_case(conn, scope, &c, MonitoringCaseStatus::Certified, reason, json!({"pending_decision": null, "pending_payload": null, "certified_at": Utc::now()})).await?
            } else {
                kit::patch(conn, MC, tenant, c.id, json!({"pending_decision": null, "pending_payload": null, "status_reason": reason})).await?
            };
            kit::audit(
                conn,
                scope,
                MC,
                if approve {
                    "certify"
                } else {
                    "certification_reject"
                },
                c.id,
                json!({"decisionId": d.id}),
                actor,
            )
            .await?;
            Ok(nc)
        }
        "reopening" => {
            let d = record_decision(
                conn,
                tenant,
                &c,
                "reopening",
                approve,
                false,
                reason,
                Some(&payload),
                actor,
            )
            .await?;
            if !approve {
                let nc = kit::patch(conn, MC, tenant, c.id, clear).await?;
                kit::audit(
                    conn,
                    scope,
                    MC,
                    "reopening_reject",
                    c.id,
                    json!({"decisionId": d.id, "reason": reason}),
                    actor,
                )
                .await?;
                return Ok(nc);
            }
            // a distinct new lifecycle; the closed case is preserved (L4-D-033-BH-005)
            let new_case: MonitoringCase = kit::insert(conn, MC, json!({
                "tenant_id": tenant.as_uuid(), "institution_id": c.institution_id, "scope_kind": c.scope_kind,
                "offering_id": c.offering_id, "section_id": c.section_id, "student_id": c.student_id,
                "title": c.title, "plan": c.plan, "owner_subject": c.owner_subject, "reviewer_subject": c.reviewer_subject,
                "status": "reopened", "status_reason": payload["reason"], "cycle_no": c.cycle_no + 1, "reopened_from_id": c.id,
                "process_instance_id": c.process_instance_id,
            })).await?;
            let nc = kit::patch(conn, MC, tenant, c.id, json!({"pending_decision": null, "pending_payload": null, "reopened_as_id": new_case.id})).await?;
            emit(
                conn,
                scope,
                &new_case,
                Some("closed"),
                (MONITORING_CASE_REOPENED, MONITORING_CASE_REOPENED_SCHEMA),
                payload["reason"].as_str(),
                actor,
            )
            .await?;
            kit::audit(
                conn,
                scope,
                MC,
                "reopen",
                c.id,
                json!({"decisionId": d.id, "newCaseId": new_case.id}),
                actor,
            )
            .await?;
            Ok(nc)
        }
        other => Err(KernelError::internal_msg(format!(
            "unknown pending decision {other}"
        ))),
    }
}

pub async fn decide(
    state: &AcademicState,
    auth: &AuthContext,
    id: Uuid,
    expected: Version,
    approve: bool,
    reason: Option<String>,
    actor: &Actor,
) -> Result<MonitoringCase, KernelError> {
    let tenant = auth.scope.tenant_id;
    if !approve && reason.is_none() {
        return Err(invalid("reason", "required", "a rejection needs a reason"));
    }
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let Locked { c, scope } = lock_case(state, &mut tx, auth, id, Some(expected)).await?;
    if c.pending_decision.is_none() {
        return Err(KernelError::Conflict(
            "the case has no pending decision".into(),
        ));
    }
    // the governance authority is independent of the case owner (L4-D-031-BH-002)
    if c.pending_decision.as_deref() == Some("governance")
        && actor.id.as_deref() == Some(c.owner_subject.as_str())
    {
        return Err(KernelError::Forbidden(
            "the case owner cannot decide its own governance review".into(),
        ));
    }
    apply_decision(&mut tx, &scope, c, approve, reason.as_deref(), actor).await?;
    tx.commit().await.map_err(map_db_error)?;
    get(state, tenant, id).await
}

// ------------------------------------------------------------------ exceptions

const EX: &str = "monitoring_exception";

pub async fn raise_exception(
    state: &AcademicState,
    auth: &AuthContext,
    id: Uuid,
    cmd: NewMonitoringException,
    actor: &Actor,
) -> Result<MonitoringCase, KernelError> {
    let tenant = auth.scope.tenant_id;
    let category = required_text("category", Some(&cmd.category))?;
    let description = required_text("description", Some(&cmd.description))?;
    let owner = required_text("ownerSubject", Some(&cmd.owner_subject))?;
    let reviewer = required_text("reviewerSubject", Some(&cmd.reviewer_subject))?;
    if owner == reviewer {
        return Err(invalid(
            "reviewerSubject",
            "independence",
            "the reviewer must be independent of the owner",
        ));
    }
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let Locked { c, scope } = lock_case(state, &mut tx, auth, id, None).await?;
    if c.status == MonitoringCaseStatus::Closed {
        return Err(KernelError::Conflict(
            "a closed case takes no new exceptions".into(),
        ));
    }
    let e: MonitoringException = kit::insert(&mut tx, EX, json!({
        "tenant_id": tenant.as_uuid(), "case_id": id, "category": category, "severity": cmd.severity.as_str(),
        "description": description, "evidence": kit::opt_text(cmd.evidence.as_deref()), "owner_subject": owner, "reviewer_subject": reviewer,
    })).await?;
    kit::patch::<MonitoringCase>(&mut tx, MC, tenant, id, json!({})).await?;
    kit::audit(
        &mut tx,
        &scope,
        EX,
        "raise",
        e.id,
        json!({"caseId": id, "severity": cmd.severity}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get(state, tenant, id).await
}

/// Exception commands (`plan`, `resolve`, `validate`, `escalate`, `withdraw`).
#[allow(clippy::too_many_arguments)]
pub async fn exception_step(
    state: &AcademicState,
    auth: &AuthContext,
    id: Uuid,
    exception_id: Uuid,
    expected: Version,
    to: &str,
    step: MonitoringStep,
    actor: &Actor,
) -> Result<MonitoringCase, KernelError> {
    let tenant = auth.scope.tenant_id;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let Locked { c, scope } = lock_case(state, &mut tx, auth, id, None).await?;
    let e: MonitoringException =
        kit::lock(&mut tx, EX, "monitoring exception", tenant, exception_id).await?;
    if e.case_id != c.id {
        return Err(KernelError::NotFound {
            entity: "monitoring exception",
        });
    }
    check_version(e.version, expected)?;
    if !domain::exception_can_transition(&e.status, to) {
        return Err(illegal("governance exception", &e.status, to));
    }
    let reason = kit::opt_text(step.reason.as_deref());
    let mut changes = json!({"status": to, "status_reason": reason});
    let m = changes
        .as_object_mut()
        .ok_or_else(|| KernelError::internal_msg("object"))?;
    match to {
        "planned" => {
            let plan = required_text("resolutionPlan", step.resolution_plan.as_deref())?;
            m.insert("resolution_plan".into(), json!(plan));
        }
        "resolved" => {
            let notes = required_text("resolutionNotes", step.resolution_notes.as_deref())?;
            m.insert("resolution_notes".into(), json!(notes));
        }
        "closed" => {
            // independent validation of the resolution (L4-D-029-BH-006)
            if actor.id.as_deref() == Some(e.owner_subject.as_str()) {
                return Err(KernelError::Forbidden(
                    "the exception owner cannot validate their own resolution".into(),
                ));
            }
            m.insert("validated_by".into(), json!(actor.id));
        }
        "escalated" | "withdrawn" => {
            required_text("reason", reason.as_deref())?;
        }
        _ => {}
    }
    kit::patch::<MonitoringException>(&mut tx, EX, tenant, exception_id, changes).await?;
    kit::patch::<MonitoringCase>(&mut tx, MC, tenant, id, json!({})).await?;
    kit::audit(
        &mut tx,
        &scope,
        EX,
        to,
        exception_id,
        json!({"caseId": id, "from": e.status}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get(state, tenant, id).await
}

// ------------------------------------------------------------------ interventions

const IV: &str = "monitoring_intervention";

/// Intervention commands (`assign`, `trigger`, `start`, `complete`, `cancel`).
#[allow(clippy::too_many_arguments)]
pub async fn intervention_step(
    state: &AcademicState,
    auth: &AuthContext,
    id: Uuid,
    intervention_id: Uuid,
    expected: Version,
    cmd: &str,
    step: MonitoringStep,
    actor: &Actor,
) -> Result<MonitoringCase, KernelError> {
    use InterventionStatus as I;
    let tenant = auth.scope.tenant_id;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let Locked { c, scope } = lock_case(state, &mut tx, auth, id, None).await?;
    let iv: MonitoringIntervention = kit::lock(
        &mut tx,
        IV,
        "monitoring intervention",
        tenant,
        intervention_id,
    )
    .await?;
    if iv.case_id != c.id {
        return Err(KernelError::NotFound {
            entity: "monitoring intervention",
        });
    }
    check_version(iv.version, expected)?;
    let reason = kit::opt_text(step.reason.as_deref());
    let now = Utc::now();
    let (to, changes) = match cmd {
        "assign" => {
            if iv.status != I::Planned {
                return Err(KernelError::Conflict(format!(
                    "an owner is assigned while the intervention is planned (it is {})",
                    iv.status
                )));
            }
            let owner = required_text("ownerSubject", step.owner_subject.as_deref())?;
            (
                I::Planned,
                json!({"owner_subject": owner, "due_on": step.due_on.or(iv.due_on)}),
            )
        }
        "trigger" => {
            if iv.owner_subject.is_none() {
                return Err(KernelError::Conflict(
                    "assign an accountable owner before triggering the intervention".into(),
                ));
            }
            (I::Triggered, json!({"status": "triggered"}))
        }
        "start" => (
            I::InProgress,
            json!({"status": "in_progress", "started_at": now}),
        ),
        "complete" => {
            let outcome = required_text("outcome", step.outcome.as_deref())?;
            (
                I::Completed,
                json!({"status": "completed", "completed_at": now, "outcome": outcome}),
            )
        }
        "cancel" => {
            required_text("reason", reason.as_deref())?;
            (
                I::Cancelled,
                json!({"status": "cancelled", "status_reason": reason}),
            )
        }
        _ => return Err(KernelError::internal_msg("unknown intervention command")),
    };
    if to != iv.status && !domain::intervention_can_transition(iv.status, to) {
        return Err(illegal("intervention", iv.status, to));
    }
    let niv: MonitoringIntervention =
        kit::patch(&mut tx, IV, tenant, intervention_id, changes).await?;
    if cmd == "trigger" {
        StatusEvent {
            aggregate_id: niv.id,
            institution_id: c.institution_id,
            offering_id: c.offering_id,
            section_id: c.section_id,
            student_id: c.student_id,
            from: Some(iv.status.as_str()),
            to: niv.status.as_str(),
            version: niv.version,
            reason: Some(niv.title.as_str()),
            faculty_subject_ref: niv.owner_subject.as_deref(),
            ..Default::default()
        }
        .emit(
            &mut tx,
            &scope,
            (
                MONITORING_INTERVENTION_TRIGGERED,
                MONITORING_INTERVENTION_TRIGGERED_SCHEMA,
            ),
            actor,
        )
        .await?;
    }
    kit::patch::<MonitoringCase>(&mut tx, MC, tenant, id, json!({})).await?;
    kit::audit(
        &mut tx,
        &scope,
        IV,
        cmd,
        intervention_id,
        json!({"caseId": id, "from": iv.status, "to": to}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get(state, tenant, id).await
}

// ------------------------------------------------------------------ certification, closure, reopening

async fn blockers(
    conn: &mut PgConnection,
    tenant: TenantId,
    c: &MonitoringCase,
) -> Result<Vec<String>, KernelError> {
    let mut out = Vec::new();
    if c.outcome_published_at.is_none() {
        out.push("the monitoring outcome is not published".to_owned());
    }
    let ex: Vec<MonitoringException> = kit::list_by(
        &mut *conn,
        EX,
        tenant,
        "case_id",
        c.id,
        None,
        "t.created_at",
        500,
    )
    .await?;
    let open_ex = ex
        .iter()
        .filter(|e| domain::exception_open(&e.status))
        .count();
    if open_ex > 0 {
        out.push(format!("{open_ex} governance exception(s) are unresolved"));
    }
    let ivs: Vec<MonitoringIntervention> = kit::list_by(
        &mut *conn,
        IV,
        tenant,
        "case_id",
        c.id,
        None,
        "t.created_at",
        500,
    )
    .await?;
    let open_iv = ivs
        .iter()
        .filter(|i| domain::intervention_open(i.status))
        .count();
    if open_iv > 0 {
        out.push(format!("{open_iv} intervention(s) are still open"));
    }
    Ok(out)
}

pub async fn certify(
    state: &AcademicState,
    auth: &AuthContext,
    id: Uuid,
    expected: Version,
    reason: Option<String>,
    actor: &Actor,
) -> Result<MonitoringCase, KernelError> {
    let tenant = auth.scope.tenant_id;
    let cur: MonitoringCase = kit::fetch(&state.pool, MC, MC_ENTITY, tenant, id).await?;
    check_version(cur.version, expected)?;
    no_pending(&cur)?;
    if !domain::can_transition(cur.status, MonitoringCaseStatus::Certified) {
        return Err(illegal(
            "monitoring case",
            cur.status,
            MonitoringCaseStatus::Certified,
        ));
    }
    {
        let mut conn = state.pool.acquire().await.map_err(map_db_error)?;
        let b = blockers(&mut conn, tenant, &cur).await?;
        if !b.is_empty() {
            return Err(KernelError::Conflict(format!(
                "certification is blocked: {}",
                b.join("; ")
            )));
        }
    }
    let scope = institution_scope(auth, InstitutionId(cur.institution_id));
    let mut vars = BTreeMap::new();
    vars.insert("caseId".to_owned(), json!(id));
    let outcome = approval::request_approval(
        state,
        &scope,
        process::MONITORING_CLOSURE,
        business_key(MC, id, cur.version),
        vars,
    )
    .await?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let Locked { c, scope } = lock_case(state, &mut tx, auth, id, Some(expected)).await?;
    let nc: MonitoringCase = kit::patch(&mut tx, MC, tenant, c.id, json!({"pending_decision": "closure", "pending_payload": {"reason": reason}, "process_instance_id": outcome.process_instance_id()})).await?;
    kit::audit(
        &mut tx,
        &scope,
        MC,
        "request_certification",
        id,
        json!({}),
        actor,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        apply_decision(
            &mut tx,
            &scope,
            nc,
            true,
            None,
            &Actor::system(actor.correlation_id.clone()),
        )
        .await?;
    }
    tx.commit().await.map_err(map_db_error)?;
    get(state, tenant, id).await
}

pub async fn close(
    state: &AcademicState,
    auth: &AuthContext,
    id: Uuid,
    expected: Version,
    reason: Option<String>,
    actor: &Actor,
) -> Result<MonitoringCase, KernelError> {
    let tenant = auth.scope.tenant_id;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let Locked { c, scope } = lock_case(state, &mut tx, auth, id, Some(expected)).await?;
    no_pending(&c)?;
    if !domain::can_transition(c.status, MonitoringCaseStatus::Closed) {
        return Err(illegal(
            "monitoring case",
            c.status,
            MonitoringCaseStatus::Closed,
        ));
    }
    let b = blockers(&mut tx, tenant, &c).await?;
    if !b.is_empty() {
        return Err(KernelError::Conflict(format!(
            "closure is blocked: {}",
            b.join("; ")
        )));
    }
    let full = {
        let risks: Vec<RiskAssessment> = kit::list_by(
            &mut *tx,
            "risk_assessment",
            tenant,
            "case_id",
            id,
            None,
            "t.created_at",
            500,
        )
        .await?;
        let ivs: Vec<MonitoringIntervention> = kit::list_by(
            &mut *tx,
            IV,
            tenant,
            "case_id",
            id,
            None,
            "t.created_at",
            500,
        )
        .await?;
        let ex: Vec<MonitoringException> = kit::list_by(
            &mut *tx,
            EX,
            tenant,
            "case_id",
            id,
            None,
            "t.created_at",
            500,
        )
        .await?;
        let ds: Vec<MonitoringDecision> = kit::list_by(
            &mut *tx,
            "monitoring_decision",
            tenant,
            "case_id",
            id,
            None,
            "t.created_at",
            500,
        )
        .await?;
        json!({"outcome": c.outcome_snapshot, "outcomeSummary": c.outcome_summary, "riskAssessments": risks, "interventions": ivs, "exceptions": ex, "decisions": ds, "closedAt": Utc::now(), "cycleNo": c.cycle_no})
    };
    let from = c.status;
    let nc = move_case(
        &mut tx,
        &scope,
        &c,
        MonitoringCaseStatus::Closed,
        reason.as_deref(),
        json!({"closed_at": Utc::now(), "closure_snapshot": full}),
    )
    .await?;
    emit(
        &mut tx,
        &scope,
        &nc,
        Some(from.as_str()),
        (MONITORING_CASE_CLOSED, MONITORING_CASE_CLOSED_SCHEMA),
        reason.as_deref(),
        actor,
    )
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        MC,
        "close",
        id,
        json!({"reason": reason}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get(state, tenant, id).await
}

pub async fn reopen(
    state: &AcademicState,
    auth: &AuthContext,
    id: Uuid,
    expected: Version,
    cmd: ReopenCase,
    actor: &Actor,
) -> Result<MonitoringCase, KernelError> {
    let tenant = auth.scope.tenant_id;
    let reason = required_text("reason", Some(&cmd.reason))?;
    let cur: MonitoringCase = kit::fetch(&state.pool, MC, MC_ENTITY, tenant, id).await?;
    check_version(cur.version, expected)?;
    no_pending(&cur)?;
    if cur.status != MonitoringCaseStatus::Closed {
        return Err(KernelError::Conflict(format!(
            "only a closed case can be reopened (it is {})",
            cur.status
        )));
    }
    if cur.reopened_as_id.is_some() {
        return Err(KernelError::Conflict(
            "the case was already reopened as a new monitoring cycle".into(),
        ));
    }
    let scope = institution_scope(auth, InstitutionId(cur.institution_id));
    let mut vars = BTreeMap::new();
    vars.insert("caseId".to_owned(), json!(id));
    vars.insert("kind".to_owned(), json!("reopening"));
    let outcome = approval::request_approval(
        state,
        &scope,
        process::MONITORING_DECISION,
        business_key(MC, id, cur.version),
        vars,
    )
    .await?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let Locked { c, scope } = lock_case(state, &mut tx, auth, id, Some(expected)).await?;
    let nc: MonitoringCase = kit::patch(&mut tx, MC, tenant, c.id, json!({
        "pending_decision": "reopening", "pending_payload": {"reason": reason, "evidence": kit::opt_text(cmd.evidence.as_deref())},
        "process_instance_id": outcome.process_instance_id(),
    })).await?;
    kit::audit(
        &mut tx,
        &scope,
        MC,
        "request_reopening",
        id,
        json!({"reason": reason}),
        actor,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        apply_decision(
            &mut tx,
            &scope,
            nc,
            true,
            None,
            &Actor::system(actor.correlation_id.clone()),
        )
        .await?;
    }
    tx.commit().await.map_err(map_db_error)?;
    get(state, tenant, id).await
}
