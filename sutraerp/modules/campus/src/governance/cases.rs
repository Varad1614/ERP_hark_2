//! Governance cases (L3-G-001, 003…013): readiness / health / maturity assessments, operational
//! and executive reviews with recommendations, the campus risk register, multi-domain action
//! plans with verified closure, SLA deviations over 8A service tickets, KPI reviews over the
//! campus-local figures, findings → corrective actions, escalations → executive resolution and
//! evidence / dependency exceptions.
//!
//! Lifecycle: `draft → open → pending_decision → closed` (a rejection returns to `open`;
//! `draft | open → cancelled`). `submit` validates the proposed outcome against the kind's
//! closure rules ([`super::domain::submit_problems`]) and the effective governance configuration,
//! then starts `GOVERNANCE_DECISION`; auto-approval closes the case at once, `AwaitDecision`
//! leaves it for `POST …/decision` (`campus.approve`, step-up).

use super::domain::{
    average_score, maturity_level, submit_problems, ApprovalState, CaseFacts, ExceptionType,
    GovernanceKind, GovernancePolicy, GovernanceStatus, ItemFact, ItemKind, ItemStatus, Severity,
};
use super::policy::effective_policy;
use crate::infrastructure::common::{
    audit, clean, emit_status, ensure_version, expected, history, illegal, invalid, limit,
    parse_date, parse_filter, required, EventExtras, FacilityDecisionBody, FacilityReasonBody,
    Transition,
};
use crate::metrics::PgCampusMetrics;
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, with_etag};
use crate::shared::numbering;
use crate::shared::rows::{decode, decode_opt};
use crate::shared::scope::institution_scope;
use crate::shared::time::business_today;
use crate::shared::{Actor, CampusState};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use std::collections::BTreeMap;
use std::str::FromStr;
use sutra_contracts::campus::events::governance::{
    GOVERNANCE_CASE_DECIDED, GOVERNANCE_CASE_DECIDED_SCHEMA, GOVERNANCE_CASE_ESCALATED,
    GOVERNANCE_CASE_ESCALATED_SCHEMA, GOVERNANCE_CASE_SUBMITTED, GOVERNANCE_CASE_SUBMITTED_SCHEMA,
};
use sutra_contracts::services::ports::OperationalMetricsPort;
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// Number kind of governance cases (`GOV-000001`).
pub const CASE_NUMBER: (&str, &str) = ("governance_case", "GOV");

/// Campus domains a case can span (the service-ticket categories of the foundation).
pub const DOMAINS: &[&str] = &[
    "facility",
    "maintenance",
    "hostel",
    "transport",
    "library",
    "laboratory",
    "it",
    "other",
];

// ------------------------------------------------------------------ rows

#[derive(Clone, Debug)]
pub struct CaseRow {
    pub id: Uuid,
    pub institution_id: InstitutionId,
    pub number: String,
    pub kind: GovernanceKind,
    pub title: String,
    pub description: Option<String>,
    pub domains: Vec<String>,
    pub severity: Severity,
    pub owner: Option<String>,
    pub due_on: Option<NaiveDate>,
    pub likelihood: Option<i32>,
    pub impact: Option<i32>,
    pub residual_score: Option<i32>,
    pub exception_type: Option<ExceptionType>,
    pub expires_on: Option<NaiveDate>,
    pub parent_id: Option<Uuid>,
    pub facts: serde_json::Value,
    pub status: GovernanceStatus,
    pub approval_state: ApprovalState,
    pub outcome: Option<String>,
    pub rationale: Option<String>,
    pub status_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub decided_by: Option<String>,
    pub version: i64,
}

const CASE_COLUMNS: &str = "id, institution_id, number, kind, title, description, domains, \
    severity, owner, due_on, likelihood, impact, residual_score, exception_type, expires_on, \
    parent_id, facts, status, approval_state, outcome, rationale, status_reason, \
    process_instance_id, decided_by, version";

fn case_from_row(r: &PgRow) -> Result<CaseRow, sqlx::Error> {
    Ok(CaseRow {
        id: r.try_get("id")?,
        institution_id: InstitutionId(r.try_get("institution_id")?),
        number: r.try_get("number")?,
        kind: decode(r.try_get("kind")?, GovernanceKind::parse)?,
        title: r.try_get("title")?,
        description: r.try_get("description")?,
        domains: r.try_get("domains")?,
        severity: decode(r.try_get("severity")?, Severity::parse)?,
        owner: r.try_get("owner")?,
        due_on: r.try_get("due_on")?,
        likelihood: r.try_get("likelihood")?,
        impact: r.try_get("impact")?,
        residual_score: r.try_get("residual_score")?,
        exception_type: decode_opt(r.try_get("exception_type")?, ExceptionType::parse)?,
        expires_on: r.try_get("expires_on")?,
        parent_id: r.try_get("parent_id")?,
        facts: r.try_get("facts")?,
        status: decode(r.try_get("status")?, GovernanceStatus::parse)?,
        approval_state: decode(r.try_get("approval_state")?, ApprovalState::parse)?,
        outcome: r.try_get("outcome")?,
        rationale: r.try_get("rationale")?,
        status_reason: r.try_get("status_reason")?,
        process_instance_id: r.try_get("process_instance_id")?,
        decided_by: r.try_get("decided_by")?,
        version: r.try_get("version")?,
    })
}

/// Loads one case of the tenant (404), optionally locked.
pub async fn load_case(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<CaseRow, KernelError> {
    let sql = format!(
        "SELECT {CASE_COLUMNS} FROM campus.governance_case WHERE id = $1 AND tenant_id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    sqlx::query(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .as_ref()
        .map(case_from_row)
        .transpose()
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "governance case",
        })
}

// ------------------------------------------------------------------ views + bodies

/// One item of a case (criterion, recommendation, action, corrective action).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GovernanceItemView {
    pub id: Uuid,
    pub case_id: Uuid,
    pub kind: ItemKind,
    pub title: String,
    pub domain: Option<String>,
    pub owner: Option<String>,
    pub due_on: Option<NaiveDate>,
    pub score: Option<i32>,
    pub status: ItemStatus,
    pub note: Option<String>,
    pub evidence_ref: Option<String>,
    pub done_by: Option<String>,
    pub verified_by: Option<String>,
    pub version: i64,
}

/// A governance case with its items.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GovernanceCaseView {
    pub id: Uuid,
    pub institution_id: Uuid,
    #[schema(example = "GOV-000001")]
    pub number: String,
    pub kind: GovernanceKind,
    pub title: String,
    pub description: Option<String>,
    pub domains: Vec<String>,
    pub severity: Severity,
    pub owner: Option<String>,
    pub due_on: Option<NaiveDate>,
    pub likelihood: Option<i32>,
    pub impact: Option<i32>,
    /// Inherent score likelihood × impact (risks).
    pub inherent_score: Option<i32>,
    pub residual_score: Option<i32>,
    pub exception_type: Option<ExceptionType>,
    pub expires_on: Option<NaiveDate>,
    pub parent_id: Option<Uuid>,
    /// Computed figures (SLA breach / KPI snapshot, assessment score).
    #[schema(value_type = Object)]
    pub facts: serde_json::Value,
    /// Average criterion rating (assessments).
    pub average_score: Option<f64>,
    pub status: GovernanceStatus,
    pub approval_state: ApprovalState,
    pub outcome: Option<String>,
    /// Outcomes this kind may close with.
    pub allowed_outcomes: Vec<String>,
    pub rationale: Option<String>,
    pub status_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub decided_by: Option<String>,
    pub items: Vec<GovernanceItemView>,
    pub version: i64,
}

/// Cases.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct GovernanceCaseList {
    pub items: Vec<GovernanceCaseView>,
}

/// One KPI target of a KPI review (G-009).
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct KpiTarget {
    /// Campus figure key (`campus.tickets_past_sla` …).
    pub key: String,
    /// Decimal string.
    pub target: String,
    /// `lower` (default) or `higher` is better.
    pub better: Option<String>,
}

/// Open a governance case.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GovernanceCaseCreate {
    pub kind: GovernanceKind,
    pub title: String,
    pub description: Option<String>,
    #[serde(default)]
    pub domains: Vec<String>,
    pub severity: Option<Severity>,
    pub owner: Option<String>,
    pub due_on: Option<String>,
    /// Risks: 1..5.
    pub likelihood: Option<i32>,
    pub impact: Option<i32>,
    /// Exceptions.
    pub exception_type: Option<ExceptionType>,
    pub expires_on: Option<String>,
    /// KPI reviews: targets (default: the effective `kpi_thresholds` configuration).
    #[serde(default)]
    pub kpi_targets: Vec<KpiTarget>,
}

/// Submit a case with its proposed outcome.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GovernanceSubmit {
    pub outcome: String,
    pub rationale: Option<String>,
    /// Risks: residual score 1..25 after mitigation.
    pub residual_score: Option<i32>,
}

/// Escalate a case to executive resolution (G-012).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GovernanceEscalate {
    pub title: String,
    pub reason: String,
    pub owner: Option<String>,
}

/// Add an item to a case.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GovernanceItemCreate {
    pub kind: ItemKind,
    pub title: String,
    pub domain: Option<String>,
    pub owner: Option<String>,
    pub due_on: Option<String>,
    pub score: Option<i32>,
}

/// Progress an item (status and / or criterion rating).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GovernanceItemProgress {
    /// `in_progress`, `done` or `cancelled`.
    pub status: Option<ItemStatus>,
    pub score: Option<i32>,
    pub note: Option<String>,
    pub evidence_ref: Option<String>,
}

/// Verify a done item (evidence reference required).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GovernanceItemVerify {
    pub evidence_ref: Option<String>,
    pub note: Option<String>,
}

/// Filters of the case list.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct GovernanceCaseQuery {
    pub kind: Option<String>,
    pub status: Option<String>,
    pub limit: Option<i64>,
}

// ------------------------------------------------------------------ helpers

const ITEM_SQL: &str = "SELECT id, case_id, kind, title, domain, owner, due_on, score, status, \
    note, evidence_ref, done_by, verified_by, version FROM campus.governance_item";

fn item_from_row(r: &PgRow) -> Result<GovernanceItemView, sqlx::Error> {
    Ok(GovernanceItemView {
        id: r.try_get("id")?,
        case_id: r.try_get("case_id")?,
        kind: decode(r.try_get("kind")?, ItemKind::parse)?,
        title: r.try_get("title")?,
        domain: r.try_get("domain")?,
        owner: r.try_get("owner")?,
        due_on: r.try_get("due_on")?,
        score: r.try_get("score")?,
        status: decode(r.try_get("status")?, ItemStatus::parse)?,
        note: r.try_get("note")?,
        evidence_ref: r.try_get("evidence_ref")?,
        done_by: r.try_get("done_by")?,
        verified_by: r.try_get("verified_by")?,
        version: r.try_get("version")?,
    })
}

async fn load_items(
    conn: &mut PgConnection,
    tenant: TenantId,
    case_id: Uuid,
) -> Result<Vec<GovernanceItemView>, KernelError> {
    let sql = format!("{ITEM_SQL} WHERE tenant_id = $1 AND case_id = $2 ORDER BY created_at, id");
    let rows = sqlx::query(&sql)
        .bind(tenant.as_uuid())
        .bind(case_id)
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db_error)?;
    rows.iter()
        .map(item_from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)
}

fn item_facts(items: &[GovernanceItemView]) -> Vec<ItemFact> {
    items
        .iter()
        .map(|i| ItemFact {
            kind: i.kind,
            status: i.status,
            score: i.score,
            domain: i.domain.clone(),
        })
        .collect()
}

fn view(c: &CaseRow, items: Vec<GovernanceItemView>) -> GovernanceCaseView {
    let avg = if c.kind.is_assessment() {
        average_score(&item_facts(&items)).map(|a| (a * 100.0).round() / 100.0)
    } else {
        None
    };
    GovernanceCaseView {
        id: c.id,
        institution_id: c.institution_id.as_uuid(),
        number: c.number.clone(),
        kind: c.kind,
        title: c.title.clone(),
        description: c.description.clone(),
        domains: c.domains.clone(),
        severity: c.severity,
        owner: c.owner.clone(),
        due_on: c.due_on,
        likelihood: c.likelihood,
        impact: c.impact,
        inherent_score: c.likelihood.zip(c.impact).map(|(l, i)| l * i),
        residual_score: c.residual_score,
        exception_type: c.exception_type,
        expires_on: c.expires_on,
        parent_id: c.parent_id,
        facts: c.facts.clone(),
        average_score: avg,
        status: c.status,
        approval_state: c.approval_state,
        outcome: c.outcome.clone(),
        allowed_outcomes: c.kind.outcomes().iter().map(|s| (*s).to_owned()).collect(),
        rationale: c.rationale.clone(),
        status_reason: c.status_reason.clone(),
        process_instance_id: c.process_instance_id.clone(),
        decided_by: c.decided_by.clone(),
        items,
        version: c.version,
    }
}

/// Case + items, read in one connection.
pub async fn case_detail(
    st: &CampusState,
    tenant: TenantId,
    id: Uuid,
) -> Result<GovernanceCaseView, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let c = load_case(&mut conn, tenant, id, false).await?;
    let items = load_items(&mut conn, tenant, id).await?;
    Ok(view(&c, items))
}

fn respond(status: StatusCode, d: &GovernanceCaseView) -> Response {
    with_etag(status, d, Version(d.version))
}

fn check_domains(domains: &[String]) -> Result<Vec<String>, KernelError> {
    let mut out: Vec<String> = Vec::new();
    for d in domains {
        let d = d.trim().to_lowercase();
        if !DOMAINS.contains(&d.as_str()) {
            return Err(invalid(
                "domains",
                "unknown_domain",
                &format!("`{d}` is not one of {}", DOMAINS.join(", ")),
            ));
        }
        if !out.contains(&d) {
            out.push(d);
        }
    }
    Ok(out)
}

fn short_text(field: &str, v: Option<String>, max: usize) -> Result<Option<String>, KernelError> {
    let v = clean(v);
    if v.as_deref().is_some_and(|s| s.chars().count() > max) {
        return Err(invalid(
            field,
            "too_long",
            &format!("at most {max} characters"),
        ));
    }
    Ok(v)
}

/// Open SLA-breached service tickets of the institution in `domains` (all when empty), read from
/// `campus.service_ticket` (8A, same schema).
async fn sla_snapshot(
    conn: &mut PgConnection,
    tenant: TenantId,
    institution: InstitutionId,
    domains: &[String],
) -> Result<Vec<serde_json::Value>, KernelError> {
    let rows = sqlx::query(
        "SELECT id, number, title, category, status, sla_due_at FROM campus.service_ticket \
         WHERE tenant_id = $1 AND institution_id = $2 \
           AND status NOT IN ('resolved', 'closed', 'cancelled', 'merged') \
           AND (sla_breached OR sla_due_at < now()) \
           AND (cardinality($3::text[]) = 0 OR category = ANY($3)) \
         ORDER BY sla_due_at, number LIMIT 200",
    )
    .bind(tenant.as_uuid())
    .bind(institution.as_uuid())
    .bind(domains)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        let id: Uuid = r.try_get("id").map_err(map_db_error)?;
        let number: String = r.try_get("number").map_err(map_db_error)?;
        let title: String = r.try_get("title").map_err(map_db_error)?;
        let category: String = r.try_get("category").map_err(map_db_error)?;
        let status: String = r.try_get("status").map_err(map_db_error)?;
        let due: chrono::DateTime<chrono::Utc> = r.try_get("sla_due_at").map_err(map_db_error)?;
        out.push(
            serde_json::json!({"ticketId": id, "number": number, "title": title,
            "category": category, "status": status, "slaDueAt": due}),
        );
    }
    Ok(out)
}

/// How many of the snapshotted tickets are still open now.
async fn sla_still_open(
    conn: &mut PgConnection,
    tenant: TenantId,
    facts: &serde_json::Value,
) -> Result<i64, KernelError> {
    let ids: Vec<Uuid> = facts
        .get("tickets")
        .and_then(serde_json::Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|t| t.get("ticketId").and_then(serde_json::Value::as_str))
                .filter_map(|s| Uuid::parse_str(s).ok())
                .collect()
        })
        .unwrap_or_default();
    if ids.is_empty() {
        return Ok(0);
    }
    sqlx::query_scalar(
        "SELECT count(*) FROM campus.service_ticket WHERE tenant_id = $1 AND id = ANY($2) \
         AND status NOT IN ('resolved', 'closed', 'cancelled', 'merged')",
    )
    .bind(tenant.as_uuid())
    .bind(&ids)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)
}

/// Snapshot of the campus figures against the targets (G-009; campus-local figures only).
async fn kpi_snapshot(
    st: &CampusState,
    scope: &ExecutionScope,
    institution: InstitutionId,
    targets: &[KpiTarget],
) -> Result<serde_json::Value, KernelError> {
    if targets.is_empty() {
        return Err(invalid(
            "kpiTargets",
            "required",
            "a KPI review needs at least one target (or an effective kpi_thresholds configuration)",
        ));
    }
    let figures = PgCampusMetrics::new(st.pool.clone())
        .metrics(scope, institution)
        .await?;
    let mut kpis = Vec::new();
    let mut off_track = false;
    for t in targets {
        let fig = figures.iter().find(|m| m.key == t.key).ok_or_else(|| {
            invalid(
                "kpiTargets",
                "unknown_figure",
                &format!("`{}` is not a campus figure", t.key),
            )
        })?;
        let target = Decimal::from_str(t.target.trim()).map_err(|_| {
            invalid(
                "kpiTargets",
                "invalid_target",
                "target must be a decimal string",
            )
        })?;
        let better = match t.better.as_deref().unwrap_or("lower") {
            "lower" => "lower",
            "higher" => "higher",
            _ => {
                return Err(invalid(
                    "kpiTargets",
                    "invalid_direction",
                    "better is `lower` or `higher`",
                ))
            }
        };
        let on_target = if better == "lower" {
            fig.value <= target
        } else {
            fig.value >= target
        };
        off_track |= !on_target;
        kpis.push(serde_json::json!({"key": fig.key, "label": fig.label,
            "value": fig.value.to_string(), "unit": fig.unit, "target": target.to_string(),
            "better": better, "onTarget": on_target}));
    }
    Ok(serde_json::json!({"kpis": kpis, "offTrack": off_track}))
}

/// KPI targets from the effective `kpi_thresholds` configuration (`{key: maximum}`).
async fn configured_targets(
    st: &CampusState,
    tenant: TenantId,
    institution: InstitutionId,
) -> Result<Vec<KpiTarget>, KernelError> {
    let settings: Option<serde_json::Value> = sqlx::query_scalar(
        "SELECT settings FROM campus.governance_config_version WHERE tenant_id = $1 \
         AND institution_id = $2 AND key = 'kpi_thresholds' AND status = 'effective'",
    )
    .bind(tenant.as_uuid())
    .bind(institution.as_uuid())
    .fetch_optional(&st.pool)
    .await
    .map_err(map_db_error)?;
    Ok(settings
        .as_ref()
        .and_then(serde_json::Value::as_object)
        .map(|o| {
            o.iter()
                .map(|(k, v)| KpiTarget {
                    key: k.clone(),
                    target: v.to_string(),
                    better: Some("lower".into()),
                })
                .collect()
        })
        .unwrap_or_default())
}

#[allow(clippy::too_many_arguments)]
async fn move_case(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    c: &CaseRow,
    to: GovernanceStatus,
    approval: Option<ApprovalState>,
    reason: Option<&str>,
    pid: Option<&str>,
    event: Option<(&'static str, &'static str)>,
    actor: &Actor,
) -> Result<i64, KernelError> {
    if !c.status.can_transition(to) {
        return Err(illegal(
            &format!("governance case {}", c.number),
            c.status.as_str(),
            to.as_str(),
        ));
    }
    let decided = to == GovernanceStatus::Closed;
    let v: i64 = sqlx::query_scalar(
        "UPDATE campus.governance_case SET status = $3, \
         approval_state = COALESCE($4, approval_state), status_reason = $5, \
         process_instance_id = COALESCE($6, process_instance_id), \
         decided_by = CASE WHEN $7 THEN $8 ELSE decided_by END, \
         decided_at = CASE WHEN $7 THEN now() ELSE decided_at END, \
         version = version + 1, updated_at = now() \
         WHERE id = $1 AND tenant_id = $2 RETURNING version",
    )
    .bind(c.id)
    .bind(scope.tenant_id.as_uuid())
    .bind(to.as_str())
    .bind(approval.map(|a| a.as_str()))
    .bind(reason)
    .bind(pid)
    .bind(decided)
    .bind(actor.id.as_deref())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let t = Transition {
        tenant: scope.tenant_id,
        institution: c.institution_id,
        aggregate_type: "governance_case",
        aggregate_id: c.id,
        from: Some(c.status.as_str()),
        to: to.as_str(),
        reason,
        process_instance_id: pid.or(c.process_instance_id.as_deref()),
        cause: event.map_or("", |e| e.0),
    };
    history(conn, &t, actor).await?;
    if let Some(e) = event {
        emit_status(
            conn,
            scope,
            e,
            &t,
            v,
            &EventExtras {
                number: Some(c.number.clone()),
                subject_ref: None,
            },
            actor,
        )
        .await?;
    }
    Ok(v)
}

async fn begin(
    st: &CampusState,
    ctx: &AuthContext,
    id: Uuid,
    expected_version: i64,
) -> Result<
    (
        sqlx::Transaction<'static, sqlx::Postgres>,
        CaseRow,
        ExecutionScope,
    ),
    KernelError,
> {
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let c = load_case(&mut tx, ctx.scope.tenant_id, id, true).await?;
    ensure_version(expected_version, c.version)?;
    let scope = institution_scope(ctx, c.institution_id);
    require_institution_writable(st, &scope, c.institution_id).await?;
    Ok((tx, c, scope))
}

fn editable(c: &CaseRow) -> Result<(), KernelError> {
    if matches!(c.status, GovernanceStatus::Draft | GovernanceStatus::Open) {
        Ok(())
    } else {
        Err(KernelError::Conflict(format!(
            "governance case {} is {}; its items are frozen",
            c.number, c.status
        )))
    }
}

struct NewCase<'a> {
    institution: InstitutionId,
    kind: GovernanceKind,
    title: &'a str,
    description: Option<&'a str>,
    domains: &'a [String],
    severity: Severity,
    owner: Option<&'a str>,
    due_on: Option<NaiveDate>,
    likelihood: Option<i32>,
    impact: Option<i32>,
    exception_type: Option<ExceptionType>,
    expires_on: Option<NaiveDate>,
    parent_id: Option<Uuid>,
    facts: serde_json::Value,
    status: GovernanceStatus,
}

async fn insert_case(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    n: NewCase<'_>,
    actor: &Actor,
) -> Result<(Uuid, String), KernelError> {
    let t = scope.tenant_id;
    let number = numbering::next(&mut *conn, t, n.institution, CASE_NUMBER).await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO campus.governance_case (tenant_id, institution_id, number, kind, title, \
         description, domains, severity, owner, due_on, likelihood, impact, exception_type, \
         expires_on, parent_id, facts, status, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18) \
         RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(n.institution.as_uuid())
    .bind(&number)
    .bind(n.kind.as_str())
    .bind(n.title)
    .bind(n.description)
    .bind(n.domains)
    .bind(n.severity.as_str())
    .bind(n.owner)
    .bind(n.due_on)
    .bind(n.likelihood)
    .bind(n.impact)
    .bind(n.exception_type.map(|e| e.as_str()))
    .bind(n.expires_on)
    .bind(n.parent_id)
    .bind(&n.facts)
    .bind(n.status.as_str())
    .bind(actor.id.as_deref())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    history(
        conn,
        &Transition {
            tenant: t,
            institution: n.institution,
            aggregate_type: "governance_case",
            aggregate_id: id,
            from: None,
            to: n.status.as_str(),
            reason: None,
            process_instance_id: None,
            cause: "sutraerp.campus.governance_case.created",
        },
        actor,
    )
    .await?;
    Ok((id, number))
}

// ------------------------------------------------------------------ handlers

/// Open a governance case (`campus.governance.manage`; Idempotency-Key). SLA deviations snapshot
/// the open SLA-breached tickets of their domains (422 `no_breach` without one); KPI reviews
/// snapshot the campus figures against their targets.
#[utoipa::path(post, path = "/api/v1/campus/institutions/{id}/governance-cases", operation_id = "campus_governance_case_create", tag = "campus-governance",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = GovernanceCaseCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = GovernanceCaseView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn case_create(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<GovernanceCaseCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::GovernanceManage)?;
    require_idempotency_key(&headers)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let title = required("title", Some(b.title))?;
    if title.chars().count() > 200 {
        return Err(invalid("title", "too_long", "at most 200 characters").into());
    }
    let description = short_text("description", b.description, 4000)?;
    let domains = check_domains(&b.domains)?;
    let due_on = b
        .due_on
        .as_deref()
        .map(|r| parse_date("dueOn", r))
        .transpose()?;
    let expires_on = b
        .expires_on
        .as_deref()
        .map(|r| parse_date("expiresOn", r))
        .transpose()?;
    let mut facts = serde_json::json!({});
    match b.kind {
        GovernanceKind::Escalation => {
            return Err(invalid(
                "kind",
                "use_escalate",
                "escalations are opened with POST /governance-cases/{id}/escalate",
            )
            .into())
        }
        GovernanceKind::Risk => {
            for (field, v) in [("likelihood", b.likelihood), ("impact", b.impact)] {
                if !v.is_some_and(|x| (1..=5).contains(&x)) {
                    return Err(invalid(field, "required", "a risk needs a rating 1..5").into());
                }
            }
        }
        GovernanceKind::Exception => {
            if b.exception_type.is_none() {
                return Err(invalid(
                    "exceptionType",
                    "required",
                    "an exception is `evidence` or `dependency`",
                )
                .into());
            }
            let today = business_today(&st, &scope, institution).await?;
            match expires_on {
                Some(e) if e >= today => {}
                _ => {
                    return Err(invalid(
                        "expiresOn",
                        "required",
                        "an exception needs a future expiry date",
                    )
                    .into())
                }
            }
        }
        GovernanceKind::SlaDeviation => {
            let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
            let tickets = sla_snapshot(&mut conn, t, institution, &domains).await?;
            if tickets.is_empty() {
                return Err(invalid(
                    "domains",
                    "no_breach",
                    "no open service ticket of these domains is past its SLA",
                )
                .into());
            }
            facts = serde_json::json!({"tickets": tickets, "breached": tickets.len()});
        }
        GovernanceKind::KpiReview => {
            let targets = if b.kpi_targets.is_empty() {
                configured_targets(&st, t, institution).await?
            } else {
                b.kpi_targets.clone()
            };
            facts = kpi_snapshot(&st, &scope, institution, &targets).await?;
            facts["targets"] = serde_json::to_value(&targets).map_err(KernelError::internal)?;
        }
        _ => {}
    }
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let (cid, number) = insert_case(
        &mut tx,
        &scope,
        NewCase {
            institution,
            kind: b.kind,
            title: &title,
            description: description.as_deref(),
            domains: &domains,
            severity: b.severity.unwrap_or(Severity::Medium),
            owner: clean(b.owner).as_deref(),
            due_on,
            likelihood: b.likelihood.filter(|_| b.kind == GovernanceKind::Risk),
            impact: b.impact.filter(|_| b.kind == GovernanceKind::Risk),
            exception_type: b
                .exception_type
                .filter(|_| b.kind == GovernanceKind::Exception),
            expires_on: expires_on.filter(|_| b.kind == GovernanceKind::Exception),
            parent_id: None,
            facts,
            status: GovernanceStatus::Draft,
        },
        &a,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "campus.governance_case.create",
        "governance_case",
        cid,
        serde_json::json!({"kind": b.kind, "number": number, "title": title}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let d = case_detail(&st, t, cid).await?;
    Ok(respond(StatusCode::CREATED, &d))
}

/// Governance cases of an institution (`campus.governance.manage`).
#[utoipa::path(get, path = "/api/v1/campus/institutions/{id}/governance-cases", operation_id = "campus_governance_case_list", tag = "campus-governance",
    params(("id" = String, Path, format = Uuid), GovernanceCaseQuery), security(("session_cookie" = [])),
    responses((status = 200, body = GovernanceCaseList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn case_list(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<GovernanceCaseQuery>,
) -> ApiResult<Json<GovernanceCaseList>> {
    let ctx = auth.require(Permission::GovernanceManage)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution(&st, &scope, institution).await?;
    let kind = parse_filter("kind", q.kind.as_deref(), GovernanceKind::parse)?;
    let status = parse_filter("status", q.status.as_deref(), GovernanceStatus::parse)?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let sql = format!(
        "SELECT {CASE_COLUMNS} FROM campus.governance_case WHERE tenant_id = $1 \
         AND institution_id = $2 AND ($3::text IS NULL OR kind = $3) \
         AND ($4::text IS NULL OR status = $4) ORDER BY created_at DESC LIMIT $5"
    );
    let rows = sqlx::query(&sql)
        .bind(t.as_uuid())
        .bind(id)
        .bind(kind.map(|k| k.as_str()))
        .bind(status.map(|s| s.as_str()))
        .bind(limit(q.limit))
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db_error)?;
    let mut items = Vec::with_capacity(rows.len());
    for r in &rows {
        let c = case_from_row(r).map_err(map_db_error)?;
        let its = load_items(&mut conn, t, c.id).await?;
        items.push(view(&c, its));
    }
    Ok(Json(GovernanceCaseList { items }))
}

/// One governance case with its items (`campus.governance.manage`).
#[utoipa::path(get, path = "/api/v1/campus/governance-cases/{id}", operation_id = "campus_governance_case_get", tag = "campus-governance",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = GovernanceCaseView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn case_get(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::GovernanceManage)?;
    let d = case_detail(&st, ctx.scope.tenant_id, id).await?;
    Ok(respond(StatusCode::OK, &d))
}

/// Start work on a draft case (`campus.governance.manage`): draft → open.
#[utoipa::path(post, path = "/api/v1/campus/governance-cases/{id}/start", operation_id = "campus_governance_case_start", tag = "campus-governance",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the case")),
    request_body = FacilityReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = GovernanceCaseView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn case_start(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<FacilityReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::GovernanceManage)?;
    let v = expected!(headers);
    let a = actor(ctx);
    let reason = clean(b.reason);
    let (mut tx, c, scope) = begin(&st, ctx, id, v).await?;
    move_case(
        &mut tx,
        &scope,
        &c,
        GovernanceStatus::Open,
        None,
        reason.as_deref(),
        None,
        None,
        &a,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "campus.governance_case.start",
        "governance_case",
        id,
        serde_json::json!({"reason": reason}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let d = case_detail(&st, ctx.scope.tenant_id, id).await?;
    Ok(respond(StatusCode::OK, &d))
}

/// Re-read the figures behind an SLA deviation or KPI review (`campus.governance.manage`;
/// draft / open only): the SLA snapshot keeps its tickets and refreshes their status, the KPI
/// snapshot is recomputed against the same targets.
#[utoipa::path(post, path = "/api/v1/campus/governance-cases/{id}/refresh", operation_id = "campus_governance_case_refresh", tag = "campus-governance",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the case")),
    request_body = FacilityReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = GovernanceCaseView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn case_refresh(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(_b): Json<FacilityReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::GovernanceManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let probe = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_case(&mut conn, t, id, false).await?
    };
    ensure_version(v, probe.version)?;
    editable(&probe)?;
    let scope = institution_scope(ctx, probe.institution_id);
    let facts = match probe.kind {
        GovernanceKind::KpiReview => {
            let targets: Vec<KpiTarget> = probe
                .facts
                .get("targets")
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .map_err(KernelError::internal)?
                .unwrap_or_default();
            let mut f = kpi_snapshot(&st, &scope, probe.institution_id, &targets).await?;
            f["targets"] = serde_json::to_value(&targets).map_err(KernelError::internal)?;
            f
        }
        GovernanceKind::SlaDeviation => {
            let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
            let open = sla_still_open(&mut conn, t, &probe.facts).await?;
            let mut f = probe.facts.clone();
            f["stillOpen"] = serde_json::json!(open);
            f
        }
        _ => {
            return Err(KernelError::Conflict(format!(
                "a {} has no figures to refresh",
                probe.kind
            ))
            .into())
        }
    };
    let (mut tx, c, scope) = begin(&st, ctx, id, v).await?;
    sqlx::query(
        "UPDATE campus.governance_case SET facts = $3, version = version + 1, updated_at = now() \
         WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(t.as_uuid())
    .bind(&facts)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit(
        &mut tx,
        &scope,
        "campus.governance_case.refresh",
        "governance_case",
        c.id,
        serde_json::json!({"kind": c.kind}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let d = case_detail(&st, t, id).await?;
    Ok(respond(StatusCode::OK, &d))
}

/// Submit an open case with its proposed outcome for decision (`campus.governance.manage`;
/// `GOVERNANCE_DECISION`). 422 with every closure rule the outcome breaks; auto-approval closes
/// the case at once (`governance_case.decided`).
#[utoipa::path(post, path = "/api/v1/campus/governance-cases/{id}/submit", operation_id = "campus_governance_case_submit", tag = "campus-governance",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the case")),
    request_body = GovernanceSubmit, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = GovernanceCaseView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn case_submit(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<GovernanceSubmit>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::GovernanceManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let outcome = b.outcome.trim().to_owned();
    let rationale = short_text("rationale", b.rationale, 4000)?;
    if b.residual_score.is_some_and(|r| !(1..=25).contains(&r)) {
        return Err(invalid("residualScore", "out_of_range", "1..25").into());
    }
    let (probe, items, still_open) = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        let c = load_case(&mut conn, t, id, false).await?;
        let items = load_items(&mut conn, t, id).await?;
        let open = if c.kind == GovernanceKind::SlaDeviation {
            sla_still_open(&mut conn, t, &c.facts).await?
        } else {
            0
        };
        (c, items, open)
    };
    ensure_version(v, probe.version)?;
    if probe.status != GovernanceStatus::Open {
        return Err(KernelError::Conflict(format!(
            "governance case {} is {}; only an open case is submitted",
            probe.number, probe.status
        ))
        .into());
    }
    let scope = institution_scope(ctx, probe.institution_id);
    require_institution_writable(&st, &scope, probe.institution_id).await?;
    let today = business_today(&st, &scope, probe.institution_id).await?;
    let policy: GovernancePolicy = effective_policy(&st.pool, t, probe.institution_id).await?;
    let facts = item_facts(&items);
    let residual = if probe.kind == GovernanceKind::Risk {
        b.residual_score
    } else {
        None
    };
    let problems = submit_problems(
        &CaseFacts {
            kind: probe.kind,
            severity: probe.severity,
            domains: &probe.domains,
            items: &facts,
            residual_score: residual,
            outcome: &outcome,
            rationale: rationale.as_deref(),
            expires_on: probe.expires_on,
            today,
            sla_still_open: still_open,
            kpi_off_track: probe
                .facts
                .get("offTrack")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
        },
        &policy,
    );
    if !problems.is_empty() {
        return Err(KernelError::Validation(
            problems
                .into_iter()
                .map(|p| FieldViolation::new("outcome", "closure_rule", p))
                .collect(),
        )
        .into());
    }
    let mut computed = probe.facts.clone();
    if probe.kind.is_assessment() {
        if let Some(avg) = average_score(&facts) {
            computed["averageScore"] = serde_json::json!((avg * 100.0).round() / 100.0);
            computed["maturityLevel"] = serde_json::json!(maturity_level(avg));
        }
        computed["policy"] = serde_json::to_value(policy).map_err(KernelError::internal)?;
    }
    let outcome_req = request_approval(
        &st,
        &scope,
        process::GOVERNANCE_DECISION,
        business_key("governance_case", id, probe.version),
        BTreeMap::from([
            ("caseId".to_owned(), serde_json::json!(id)),
            ("kind".to_owned(), serde_json::json!(probe.kind)),
            ("outcome".to_owned(), serde_json::json!(outcome)),
        ]),
    )
    .await?;
    let pid = outcome_req.process_instance_id().to_owned();
    let (mut tx, c, scope) = begin(&st, ctx, id, v).await?;
    sqlx::query(
        "UPDATE campus.governance_case SET outcome = $3, rationale = $4, \
         residual_score = COALESCE($5, residual_score), facts = $6 \
         WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(t.as_uuid())
    .bind(&outcome)
    .bind(rationale.as_deref())
    .bind(residual)
    .bind(&computed)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    move_case(
        &mut tx,
        &scope,
        &c,
        GovernanceStatus::PendingDecision,
        Some(ApprovalState::Pending),
        rationale.as_deref(),
        Some(&pid),
        Some((GOVERNANCE_CASE_SUBMITTED, GOVERNANCE_CASE_SUBMITTED_SCHEMA)),
        &a,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome_req {
        let cur = load_case(&mut tx, t, id, true).await?;
        move_case(
            &mut tx,
            &scope,
            &cur,
            GovernanceStatus::Closed,
            Some(ApprovalState::Approved),
            Some("auto-approved"),
            Some(&pid),
            Some((GOVERNANCE_CASE_DECIDED, GOVERNANCE_CASE_DECIDED_SCHEMA)),
            &Actor::system(a.correlation_id.clone()),
        )
        .await?;
    }
    audit(
        &mut tx,
        &scope,
        "campus.governance_case.submit",
        "governance_case",
        id,
        serde_json::json!({"outcome": outcome, "processInstanceId": pid}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let d = case_detail(&st, t, id).await?;
    Ok(respond(StatusCode::OK, &d))
}

/// Decide a submitted case (`campus.approve`, step-up): approve → closed with its outcome;
/// reject (reason required) → open again, outcome cleared.
#[utoipa::path(post, path = "/api/v1/campus/governance-cases/{id}/decision", operation_id = "campus_governance_case_decide", tag = "campus-governance",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the case")),
    request_body = FacilityDecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = GovernanceCaseView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn case_decide(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<FacilityDecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CampusApprove)?;
    let v = expected!(headers);
    let reason = clean(b.reason.clone());
    if !b.approve() && reason.is_none() {
        return Err(invalid("reason", "required", "a rejection needs a reason").into());
    }
    let a = actor(ctx);
    let (mut tx, c, scope) = begin(&st, ctx, id, v).await?;
    if c.status != GovernanceStatus::PendingDecision || c.approval_state != ApprovalState::Pending {
        return Err(KernelError::Conflict(format!(
            "governance case {} is not waiting for a decision",
            c.number
        ))
        .into());
    }
    let ev = Some((GOVERNANCE_CASE_DECIDED, GOVERNANCE_CASE_DECIDED_SCHEMA));
    if b.approve() {
        move_case(
            &mut tx,
            &scope,
            &c,
            GovernanceStatus::Closed,
            Some(ApprovalState::Approved),
            reason.as_deref(),
            None,
            ev,
            &a,
        )
        .await?;
    } else {
        sqlx::query("UPDATE campus.governance_case SET outcome = NULL WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
        move_case(
            &mut tx,
            &scope,
            &c,
            GovernanceStatus::Open,
            Some(ApprovalState::Rejected),
            reason.as_deref(),
            None,
            ev,
            &a,
        )
        .await?;
    }
    audit(
        &mut tx,
        &scope,
        "campus.governance_case.decide",
        "governance_case",
        id,
        serde_json::json!({"decision": b.decision, "reason": reason, "outcome": c.outcome}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let d = case_detail(&st, ctx.scope.tenant_id, id).await?;
    Ok(respond(StatusCode::OK, &d))
}

/// Cancel a draft or open case (`campus.governance.manage`; reason required).
#[utoipa::path(post, path = "/api/v1/campus/governance-cases/{id}/cancel", operation_id = "campus_governance_case_cancel", tag = "campus-governance",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the case")),
    request_body = FacilityReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = GovernanceCaseView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn case_cancel(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<FacilityReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::GovernanceManage)?;
    let v = expected!(headers);
    let reason = required("reason", b.reason)?;
    let a = actor(ctx);
    let (mut tx, c, scope) = begin(&st, ctx, id, v).await?;
    move_case(
        &mut tx,
        &scope,
        &c,
        GovernanceStatus::Cancelled,
        None,
        Some(&reason),
        None,
        None,
        &a,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "campus.governance_case.cancel",
        "governance_case",
        id,
        serde_json::json!({"reason": reason}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let d = case_detail(&st, ctx.scope.tenant_id, id).await?;
    Ok(respond(StatusCode::OK, &d))
}

/// Escalate an open / pending case to executive resolution (G-012; `campus.governance.manage`;
/// Idempotency-Key): opens an `escalation` case (severity at least high) linked to it and emits
/// `governance_case.escalated`. Returns the new escalation case.
#[utoipa::path(post, path = "/api/v1/campus/governance-cases/{id}/escalate", operation_id = "campus_governance_case_escalate", tag = "campus-governance",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = GovernanceEscalate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = GovernanceCaseView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn case_escalate(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<GovernanceEscalate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::GovernanceManage)?;
    require_idempotency_key(&headers)?;
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let title = required("title", Some(b.title))?;
    let reason = required("reason", Some(b.reason))?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let parent = load_case(&mut tx, t, id, true).await?;
    let scope = institution_scope(ctx, parent.institution_id);
    require_institution_writable(&st, &scope, parent.institution_id).await?;
    if parent.kind == GovernanceKind::Escalation
        || !matches!(
            parent.status,
            GovernanceStatus::Open | GovernanceStatus::PendingDecision
        )
    {
        return Err(KernelError::Conflict(format!(
            "governance case {} ({}, {}) cannot be escalated",
            parent.number, parent.kind, parent.status
        ))
        .into());
    }
    let live: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM campus.governance_case WHERE tenant_id = $1 AND parent_id = $2 \
         AND kind = 'escalation' AND status NOT IN ('closed', 'cancelled')",
    )
    .bind(t.as_uuid())
    .bind(id)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if live > 0 {
        return Err(KernelError::Conflict(format!(
            "governance case {} already has a live escalation",
            parent.number
        ))
        .into());
    }
    let severity = if matches!(parent.severity, Severity::Critical) {
        Severity::Critical
    } else {
        Severity::High
    };
    let (eid, number) = insert_case(
        &mut tx,
        &scope,
        NewCase {
            institution: parent.institution_id,
            kind: GovernanceKind::Escalation,
            title: &title,
            description: Some(&reason),
            domains: &parent.domains,
            severity,
            owner: clean(b.owner).as_deref(),
            due_on: None,
            likelihood: None,
            impact: None,
            exception_type: None,
            expires_on: None,
            parent_id: Some(id),
            facts: serde_json::json!({"parentNumber": parent.number, "parentKind": parent.kind}),
            status: GovernanceStatus::Open,
        },
        &a,
    )
    .await?;
    let t2 = Transition {
        tenant: t,
        institution: parent.institution_id,
        aggregate_type: "governance_case",
        aggregate_id: eid,
        from: None,
        to: GovernanceStatus::Open.as_str(),
        reason: Some(&reason),
        process_instance_id: None,
        cause: GOVERNANCE_CASE_ESCALATED,
    };
    emit_status(
        &mut tx,
        &scope,
        (GOVERNANCE_CASE_ESCALATED, GOVERNANCE_CASE_ESCALATED_SCHEMA),
        &t2,
        1,
        &EventExtras {
            number: Some(number.clone()),
            subject_ref: None,
        },
        &a,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "campus.governance_case.escalate",
        "governance_case",
        id,
        serde_json::json!({"escalationId": eid, "number": number, "reason": reason}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let d = case_detail(&st, t, eid).await?;
    Ok(respond(StatusCode::CREATED, &d))
}

/// Add an item to a draft / open case (`campus.governance.manage`; Idempotency-Key). The kind
/// must fit the case (criteria for assessments, recommendations for executive reviews, corrective
/// actions for findings, actions elsewhere); action-plan actions name one of the plan's domains.
#[utoipa::path(post, path = "/api/v1/campus/governance-cases/{id}/items", operation_id = "campus_governance_item_add", tag = "campus-governance",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = GovernanceItemCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = GovernanceCaseView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn item_add(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<GovernanceItemCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::GovernanceManage)?;
    require_idempotency_key(&headers)?;
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let title = required("title", Some(b.title))?;
    if title.chars().count() > 200 {
        return Err(invalid("title", "too_long", "at most 200 characters").into());
    }
    if b.score.is_some_and(|s| !(1..=5).contains(&s)) {
        return Err(invalid("score", "out_of_range", "a rating is 1..5").into());
    }
    let due_on = b
        .due_on
        .as_deref()
        .map(|r| parse_date("dueOn", r))
        .transpose()?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let c = load_case(&mut tx, t, id, true).await?;
    let scope = institution_scope(ctx, c.institution_id);
    require_institution_writable(&st, &scope, c.institution_id).await?;
    editable(&c)?;
    if !c.kind.item_kinds().contains(&b.kind) {
        return Err(invalid(
            "kind",
            "not_for_case",
            &format!("a {} does not take {} items", c.kind, b.kind),
        )
        .into());
    }
    if b.score.is_some() && b.kind != ItemKind::Criterion {
        return Err(invalid("score", "not_a_criterion", "only criteria are rated").into());
    }
    let domain = clean(b.domain).map(|d| d.to_lowercase());
    if let Some(d) = domain.as_deref() {
        if !c.domains.is_empty() && !c.domains.iter().any(|x| x == d) {
            return Err(invalid("domain", "not_in_case", "not one of the case's domains").into());
        }
    } else if c.kind == GovernanceKind::ActionPlan && !c.domains.is_empty() {
        return Err(invalid(
            "domain",
            "required",
            "action-plan actions name their domain",
        )
        .into());
    }
    let item: Uuid = sqlx::query_scalar(
        "INSERT INTO campus.governance_item (tenant_id, institution_id, case_id, kind, title, \
         domain, owner, due_on, score) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(c.institution_id.as_uuid())
    .bind(id)
    .bind(b.kind.as_str())
    .bind(&title)
    .bind(domain.as_deref())
    .bind(clean(b.owner).as_deref())
    .bind(due_on)
    .bind(b.score)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit(
        &mut tx,
        &scope,
        "campus.governance_item.add",
        "governance_item",
        item,
        serde_json::json!({"caseId": id, "kind": b.kind, "title": title}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let d = case_detail(&st, t, id).await?;
    Ok(respond(StatusCode::CREATED, &d))
}

async fn load_item_locked(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
) -> Result<GovernanceItemView, KernelError> {
    let sql = format!("{ITEM_SQL} WHERE id = $1 AND tenant_id = $2 FOR UPDATE");
    let row = sqlx::query(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "governance item",
        })?;
    item_from_row(&row).map_err(map_db_error)
}

/// Progress an item (`campus.governance.manage`; If-Match = the item's version): move it to
/// `in_progress` / `done` / `cancelled` and / or rate a criterion. Returns the case.
#[utoipa::path(post, path = "/api/v1/campus/governance-items/{id}/progress", operation_id = "campus_governance_item_progress", tag = "campus-governance",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag (version) of the item")),
    request_body = GovernanceItemProgress, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = GovernanceCaseView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn item_progress(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<GovernanceItemProgress>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::GovernanceManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    if b.score.is_some_and(|s| !(1..=5).contains(&s)) {
        return Err(invalid("score", "out_of_range", "a rating is 1..5").into());
    }
    if b.status == Some(ItemStatus::Verified) {
        return Err(invalid("status", "use_verify", "verify with POST …/verify").into());
    }
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let item = load_item_locked(&mut tx, t, id).await?;
    ensure_version(v, item.version)?;
    let c = load_case(&mut tx, t, item.case_id, true).await?;
    let scope = institution_scope(ctx, c.institution_id);
    require_institution_writable(&st, &scope, c.institution_id).await?;
    editable(&c)?;
    if b.score.is_some() && item.kind != ItemKind::Criterion {
        return Err(invalid("score", "not_a_criterion", "only criteria are rated").into());
    }
    if let Some(to) = b.status {
        if to != item.status && !item.status.can_transition(to) {
            return Err(illegal(
                &format!("governance item {}", item.title),
                item.status.as_str(),
                to.as_str(),
            )
            .into());
        }
    }
    let to = b.status.unwrap_or(item.status);
    let done = to == ItemStatus::Done && item.status != ItemStatus::Done;
    sqlx::query(
        "UPDATE campus.governance_item SET status = $3, score = COALESCE($4, score), \
         note = COALESCE($5, note), evidence_ref = COALESCE($6, evidence_ref), \
         done_by = CASE WHEN $7 THEN $8 ELSE done_by END, \
         done_at = CASE WHEN $7 THEN now() ELSE done_at END, \
         version = version + 1, updated_at = now() WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(t.as_uuid())
    .bind(to.as_str())
    .bind(b.score)
    .bind(clean(b.note).as_deref())
    .bind(clean(b.evidence_ref).as_deref())
    .bind(done)
    .bind(a.id.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit(
        &mut tx,
        &scope,
        "campus.governance_item.progress",
        "governance_item",
        id,
        serde_json::json!({"from": item.status, "to": to, "score": b.score}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let d = case_detail(&st, t, item.case_id).await?;
    Ok(respond(StatusCode::OK, &d))
}

/// Verify a done item (`campus.governance.manage`; If-Match = the item's version): an evidence
/// reference is required (on the item or in the body) — verified closure. Returns the case.
#[utoipa::path(post, path = "/api/v1/campus/governance-items/{id}/verify", operation_id = "campus_governance_item_verify", tag = "campus-governance",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag (version) of the item")),
    request_body = GovernanceItemVerify, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = GovernanceCaseView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn item_verify(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<GovernanceItemVerify>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::GovernanceManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let item = load_item_locked(&mut tx, t, id).await?;
    ensure_version(v, item.version)?;
    let c = load_case(&mut tx, t, item.case_id, true).await?;
    let scope = institution_scope(ctx, c.institution_id);
    require_institution_writable(&st, &scope, c.institution_id).await?;
    editable(&c)?;
    if item.status != ItemStatus::Done {
        return Err(illegal(
            &format!("governance item {}", item.title),
            item.status.as_str(),
            ItemStatus::Verified.as_str(),
        )
        .into());
    }
    let evidence = clean(b.evidence_ref).or(item.evidence_ref.clone());
    if evidence.is_none() {
        return Err(invalid(
            "evidenceRef",
            "required",
            "verification needs an evidence reference",
        )
        .into());
    }
    sqlx::query(
        "UPDATE campus.governance_item SET status = 'verified', evidence_ref = $3, \
         note = COALESCE($4, note), verified_by = $5, verified_at = now(), \
         version = version + 1, updated_at = now() WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(t.as_uuid())
    .bind(evidence.as_deref())
    .bind(clean(b.note).as_deref())
    .bind(a.id.as_deref().unwrap_or("unknown"))
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit(
        &mut tx,
        &scope,
        "campus.governance_item.verify",
        "governance_item",
        id,
        serde_json::json!({"caseId": item.case_id, "evidenceRef": evidence}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let d = case_detail(&st, t, item.case_id).await?;
    Ok(respond(StatusCode::OK, &d))
}

pub fn router(state: CampusState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(case_create, case_list))
        .routes(routes!(case_get))
        .routes(routes!(case_start))
        .routes(routes!(case_refresh))
        .routes(routes!(case_submit))
        .routes(routes!(case_decide))
        .routes(routes!(case_cancel))
        .routes(routes!(case_escalate))
        .routes(routes!(item_add))
        .routes(routes!(item_progress))
        .routes(routes!(item_verify))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domains_are_validated_and_deduplicated() {
        let ok = check_domains(&["Hostel".into(), "hostel".into(), "library".into()]);
        assert_eq!(
            ok.unwrap_or_default(),
            vec!["hostel".to_owned(), "library".to_owned()]
        );
        assert!(check_domains(&["moon".into()]).is_err());
        assert_eq!(numbering::format_number(CASE_NUMBER.1, 3), "GOV-000003");
    }
}
