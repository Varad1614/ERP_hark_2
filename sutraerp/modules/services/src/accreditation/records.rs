//! Accreditation records (L3-H-063…071): compliance obligations → requirement lifecycle,
//! evidence requests → approved evidence records (citing validated 8C regulatory snapshots and
//! published G00 governance evidence by id), internal audits → closed audit reports, external
//! assessment notices → accepted outcomes, compliance findings → verified CAPA closure, quality
//! improvement proposals → verified outcomes (figure measured through the metrics port before /
//! after), source evidence → institutional readiness decisions, periodic compliance reviews →
//! compliance status, and approved evidence → self-study report approval + hand-off.
//!
//! Lifecycle: `draft → open → pending_decision → closed` (rejection → `open`; `draft | open →
//! cancelled`). `submit` validates the proposed outcome ([`super::domain::record_problems`]) and
//! starts `ACCREDITATION_DECISION`; decisions need `services.report.approve` (step-up).

use super::cycles::{load_cycle, set_cycle_status, CycleRow};
use super::domain::{
    compliance_status, record_problems, AccreditationApprovalState, CycleStatus, ItemFact,
    Measurement, RecordFacts, RecordItemKind, RecordItemStatus, RecordKind, RecordSeverity,
    RecordStatus,
};
use super::frameworks::load_framework;
use crate::dashboards::common::{
    check_version, clean, decode, expected, invalid, optional_text, page_limit, required_reason,
    required_text, transition, ReportDecision, ReportDecisionBody, ReportReasonBody, Transition,
};
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, with_etag};
use crate::shared::numbering;
use crate::shared::scope::institution_scope;
use crate::shared::time::business_today;
use crate::shared::{Actor, ServicesState};
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
use sutra_contracts::services::events::accreditation::{
    ACCREDITATION_RECORD_DECIDED, ACCREDITATION_RECORD_DECIDED_SCHEMA, SELF_STUDY_HANDED_OFF,
    SELF_STUDY_HANDED_OFF_SCHEMA,
};
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

/// Number kind of accreditation records (`ACR-000001`).
pub const RECORD_NUMBER: (&str, &str) = ("accreditation_record", "ACR");

// ------------------------------------------------------------------ rows / views

/// One item of a record (requirement, checklist entry, CAPA / improvement action).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AccreditationItemView {
    pub id: Uuid,
    pub record_id: Uuid,
    pub kind: RecordItemKind,
    pub title: String,
    pub owner: Option<String>,
    pub due_on: Option<NaiveDate>,
    pub status: RecordItemStatus,
    pub note: Option<String>,
    pub evidence_ref: Option<String>,
    pub done_by: Option<String>,
    pub verified_by: Option<String>,
    pub version: i64,
}

/// An accreditation record with its items.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AccreditationRecordView {
    pub id: Uuid,
    pub institution_id: Uuid,
    #[schema(example = "ACR-000001")]
    pub number: String,
    pub kind: RecordKind,
    pub cycle_id: Option<Uuid>,
    pub criterion_code: Option<String>,
    pub parent_id: Option<Uuid>,
    pub title: String,
    pub description: Option<String>,
    pub severity: RecordSeverity,
    pub owner: Option<String>,
    pub due_on: Option<NaiveDate>,
    pub metric_key: Option<String>,
    pub better: Option<String>,
    /// `{regulatorySnapshotIds[], governanceEvidenceIds[], sourceUrl}`.
    #[schema(value_type = Object)]
    pub refs: serde_json::Value,
    /// Computed figures (baseline / current, coverage, compliance counts, report contents).
    #[schema(value_type = Object)]
    pub facts: serde_json::Value,
    pub status: RecordStatus,
    pub approval_state: AccreditationApprovalState,
    pub outcome: Option<String>,
    pub allowed_outcomes: Vec<String>,
    pub rationale: Option<String>,
    pub status_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub decided_by: Option<String>,
    pub handed_off_at: Option<chrono::DateTime<chrono::Utc>>,
    pub handoff_reference: Option<String>,
    pub items: Vec<AccreditationItemView>,
    pub version: i64,
}

/// Records.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AccreditationRecordList {
    pub items: Vec<AccreditationRecordView>,
}

/// Open a record.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AccreditationRecordCreate {
    pub kind: RecordKind,
    pub title: String,
    pub description: Option<String>,
    /// Required for evidence, internal audits, external assessments, readiness and self-study.
    pub cycle_id: Option<Uuid>,
    /// Evidence: a criterion of the cycle's framework.
    pub criterion_code: Option<String>,
    /// Findings raised by an internal audit / external assessment.
    pub parent_id: Option<Uuid>,
    pub severity: Option<RecordSeverity>,
    pub owner: Option<String>,
    pub due_on: Option<String>,
    /// Improvement: figure tracked (`<domain>.<key>`) and the better direction.
    pub metric_key: Option<String>,
    pub better: Option<String>,
    /// Evidence citations: validated regulatory snapshots (8C).
    #[serde(default)]
    pub regulatory_snapshot_ids: Vec<Uuid>,
    /// Evidence citations: published governance evidence (G-015 event ids).
    #[serde(default)]
    pub governance_evidence_ids: Vec<Uuid>,
    pub source_url: Option<String>,
}

/// Submit with the proposed outcome.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AccreditationSubmit {
    pub outcome: String,
    pub rationale: Option<String>,
    /// External assessment: awarded grade / score (`A++`, `3.42` …).
    pub grade: Option<String>,
}

/// Hand an approved self-study report off to the accrediting body.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AccreditationHandoff {
    /// Portal submission / acknowledgement reference.
    pub reference: String,
}

/// Add an item.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AccreditationItemCreate {
    pub kind: RecordItemKind,
    pub title: String,
    pub owner: Option<String>,
    pub due_on: Option<String>,
}

/// Progress an item.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AccreditationItemProgress {
    /// `in_progress`, `done` or `cancelled`.
    pub status: RecordItemStatus,
    pub note: Option<String>,
    pub evidence_ref: Option<String>,
}

/// Verify a done item.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AccreditationItemVerify {
    pub evidence_ref: Option<String>,
    pub note: Option<String>,
}

/// Filters of the record list.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct AccreditationRecordQuery {
    pub kind: Option<String>,
    pub status: Option<String>,
    pub cycle_id: Option<Uuid>,
    pub limit: Option<i64>,
}

#[derive(Debug, Clone)]
struct RecordRow {
    id: Uuid,
    institution_id: Uuid,
    number: String,
    kind: RecordKind,
    cycle_id: Option<Uuid>,
    criterion_code: Option<String>,
    parent_id: Option<Uuid>,
    title: String,
    description: Option<String>,
    severity: RecordSeverity,
    owner: Option<String>,
    due_on: Option<NaiveDate>,
    metric_key: Option<String>,
    better: Option<String>,
    refs: serde_json::Value,
    facts: serde_json::Value,
    status: RecordStatus,
    approval_state: AccreditationApprovalState,
    outcome: Option<String>,
    rationale: Option<String>,
    status_reason: Option<String>,
    process_instance_id: Option<String>,
    decided_by: Option<String>,
    handed_off_at: Option<chrono::DateTime<chrono::Utc>>,
    handoff_reference: Option<String>,
    version: i64,
}

const COLUMNS: &str = "id, institution_id, number, kind, cycle_id, criterion_code, parent_id, \
    title, description, severity, owner, due_on, metric_key, better, refs, facts, status, \
    approval_state, outcome, rationale, status_reason, process_instance_id, decided_by, \
    handed_off_at, handoff_reference, version";

fn from_row(r: &PgRow) -> Result<RecordRow, sqlx::Error> {
    Ok(RecordRow {
        id: r.try_get("id")?,
        institution_id: r.try_get("institution_id")?,
        number: r.try_get("number")?,
        kind: decode(r.try_get("kind")?, RecordKind::parse)?,
        cycle_id: r.try_get("cycle_id")?,
        criterion_code: r.try_get("criterion_code")?,
        parent_id: r.try_get("parent_id")?,
        title: r.try_get("title")?,
        description: r.try_get("description")?,
        severity: decode(r.try_get("severity")?, RecordSeverity::parse)?,
        owner: r.try_get("owner")?,
        due_on: r.try_get("due_on")?,
        metric_key: r.try_get("metric_key")?,
        better: r.try_get("better")?,
        refs: r.try_get("refs")?,
        facts: r.try_get("facts")?,
        status: decode(r.try_get("status")?, RecordStatus::parse)?,
        approval_state: decode(
            r.try_get("approval_state")?,
            AccreditationApprovalState::parse,
        )?,
        outcome: r.try_get("outcome")?,
        rationale: r.try_get("rationale")?,
        status_reason: r.try_get("status_reason")?,
        process_instance_id: r.try_get("process_instance_id")?,
        decided_by: r.try_get("decided_by")?,
        handed_off_at: r.try_get("handed_off_at")?,
        handoff_reference: r.try_get("handoff_reference")?,
        version: r.try_get("version")?,
    })
}

async fn load_record(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<RecordRow, KernelError> {
    let sql = format!(
        "SELECT {COLUMNS} FROM services.accreditation_record WHERE id = $1 AND tenant_id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    let row = sqlx::query(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "accreditation record",
        })?;
    from_row(&row).map_err(map_db_error)
}

const ITEM_SQL: &str = "SELECT id, record_id, kind, title, owner, due_on, status, note, \
    evidence_ref, done_by, verified_by, version FROM services.accreditation_item";

fn item_from_row(r: &PgRow) -> Result<AccreditationItemView, sqlx::Error> {
    Ok(AccreditationItemView {
        id: r.try_get("id")?,
        record_id: r.try_get("record_id")?,
        kind: decode(r.try_get("kind")?, RecordItemKind::parse)?,
        title: r.try_get("title")?,
        owner: r.try_get("owner")?,
        due_on: r.try_get("due_on")?,
        status: decode(r.try_get("status")?, RecordItemStatus::parse)?,
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
    record: Uuid,
) -> Result<Vec<AccreditationItemView>, KernelError> {
    let sql = format!("{ITEM_SQL} WHERE tenant_id = $1 AND record_id = $2 ORDER BY created_at, id");
    let rows = sqlx::query(&sql)
        .bind(tenant.as_uuid())
        .bind(record)
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db_error)?;
    rows.iter()
        .map(item_from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)
}

fn view(r: RecordRow, items: Vec<AccreditationItemView>) -> AccreditationRecordView {
    AccreditationRecordView {
        id: r.id,
        institution_id: r.institution_id,
        number: r.number,
        kind: r.kind,
        cycle_id: r.cycle_id,
        criterion_code: r.criterion_code,
        parent_id: r.parent_id,
        title: r.title,
        description: r.description,
        severity: r.severity,
        owner: r.owner,
        due_on: r.due_on,
        metric_key: r.metric_key,
        better: r.better,
        refs: r.refs,
        facts: r.facts,
        status: r.status,
        approval_state: r.approval_state,
        outcome: r.outcome,
        allowed_outcomes: r.kind.outcomes().iter().map(|s| (*s).to_owned()).collect(),
        rationale: r.rationale,
        status_reason: r.status_reason,
        process_instance_id: r.process_instance_id,
        decided_by: r.decided_by,
        handed_off_at: r.handed_off_at,
        handoff_reference: r.handoff_reference,
        items,
        version: r.version,
    }
}

async fn detail(
    st: &ServicesState,
    tenant: TenantId,
    id: Uuid,
) -> Result<AccreditationRecordView, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let r = load_record(&mut conn, tenant, id, false).await?;
    let items = load_items(&mut conn, tenant, id).await?;
    Ok(view(r, items))
}

fn respond(status: StatusCode, v: &AccreditationRecordView) -> Response {
    with_etag(status, v, Version(v.version))
}

fn parse_day(field: &str, raw: &str) -> Result<NaiveDate, KernelError> {
    NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d")
        .map_err(|_| invalid(field, "invalid_date", "must be a date YYYY-MM-DD"))
}

/// Current value of one figure through the metrics port (`None` when not reported).
async fn figure(
    st: &ServicesState,
    scope: &ExecutionScope,
    institution: InstitutionId,
    key: &str,
) -> Result<Option<Decimal>, KernelError> {
    Ok(st
        .all_metrics(scope, institution)
        .await?
        .into_iter()
        .find(|m| m.key == key)
        .map(|m| m.value))
}

#[allow(clippy::too_many_arguments)]
async fn move_record(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    r: &RecordRow,
    to: RecordStatus,
    approval: Option<AccreditationApprovalState>,
    reason: Option<&str>,
    pid: Option<&str>,
    event: Option<(&'static str, &'static str)>,
    action: &'static str,
    actor: &Actor,
) -> Result<(), KernelError> {
    if !r.status.can_transition(to) {
        return Err(KernelError::Conflict(format!(
            "accreditation record {} is {}; it cannot become {}",
            r.number, r.status, to
        )));
    }
    let decided = to == RecordStatus::Closed;
    let v: i64 = sqlx::query_scalar(
        "UPDATE services.accreditation_record SET status = $3, \
         approval_state = COALESCE($4, approval_state), status_reason = $5, \
         process_instance_id = COALESCE($6, process_instance_id), \
         decided_by = CASE WHEN $7 THEN $8 ELSE decided_by END, \
         decided_at = CASE WHEN $7 THEN now() ELSE decided_at END, \
         version = version + 1, updated_at = now() \
         WHERE id = $1 AND tenant_id = $2 RETURNING version",
    )
    .bind(r.id)
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
    transition(
        conn,
        scope,
        &Transition {
            aggregate_type: "accreditation_record",
            id: r.id,
            institution: InstitutionId(r.institution_id),
            from: Some(r.status.as_str()),
            to: to.as_str(),
            version: v,
            reason,
            process_instance_id: pid.or(r.process_instance_id.as_deref()),
            number: Some(&r.number),
            subject_ref: None,
        },
        event,
        action,
        actor,
    )
    .await
}

async fn begin(
    st: &ServicesState,
    ctx: &AuthContext,
    id: Uuid,
    v: i64,
) -> Result<
    (
        sqlx::Transaction<'static, sqlx::Postgres>,
        RecordRow,
        ExecutionScope,
    ),
    KernelError,
> {
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let r = load_record(&mut tx, ctx.scope.tenant_id, id, true).await?;
    check_version(v, r.version)?;
    let inst = InstitutionId(r.institution_id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(st, &scope, inst).await?;
    Ok((tx, r, scope))
}

fn editable(r: &RecordRow) -> Result<(), KernelError> {
    if matches!(r.status, RecordStatus::Draft | RecordStatus::Open) {
        Ok(())
    } else {
        Err(KernelError::Conflict(format!(
            "accreditation record {} is {}; it is frozen",
            r.number, r.status
        )))
    }
}

async fn active_cycle(
    conn: &mut PgConnection,
    tenant: TenantId,
    institution: Uuid,
    id: Uuid,
) -> Result<CycleRow, KernelError> {
    let c = load_cycle(&mut *conn, tenant, id, false)
        .await
        .ok()
        .filter(|c| c.institution_id == institution)
        .ok_or_else(|| invalid("cycleId", "unknown_cycle", "no such accreditation cycle"))?;
    if c.status != CycleStatus::Active {
        return Err(KernelError::Conflict(format!(
            "cycle {} is {}; records are added to an active cycle (approved plan)",
            c.number, c.status
        )));
    }
    Ok(c)
}

// ------------------------------------------------------------------ handlers

/// Open an accreditation record (`services.accreditation.manage`; Idempotency-Key). Evidence
/// names a criterion of the cycle's framework and may cite validated regulatory snapshots,
/// published governance evidence ids and a source URL; improvements snapshot their figure's
/// baseline through the metrics port.
#[utoipa::path(post, path = "/api/v1/services/institutions/{id}/accreditation-records", operation_id = "services_accreditation_record_create", tag = "services-accreditation",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)),
    request_body = AccreditationRecordCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = AccreditationRecordView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn record_create(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<AccreditationRecordCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AccreditationManage)?;
    require_idempotency_key(&headers)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let title = required_text("title", &b.title, 200)?;
    let description = optional_text("description", b.description, 4000)?;
    let due_on = b
        .due_on
        .as_deref()
        .map(|r| parse_day("dueOn", r))
        .transpose()?;
    let mut facts = serde_json::json!({});
    let mut metric_key = None;
    let mut better = None;
    if b.kind == RecordKind::Improvement {
        let key = clean(b.metric_key.clone())
            .ok_or_else(|| invalid("metricKey", "required", "an improvement tracks a figure"))?;
        let dir = match b.better.as_deref().map(str::trim) {
            Some("higher") => "higher",
            Some("lower") => "lower",
            _ => return Err(invalid("better", "required", "`higher` or `lower` is better").into()),
        };
        let baseline = figure(&st, &scope, institution, &key)
            .await?
            .ok_or_else(|| {
                invalid(
                    "metricKey",
                    "unknown_metric",
                    "the figure is not reported by its owning domain",
                )
            })?;
        facts = serde_json::json!({"baseline": baseline.to_string()});
        metric_key = Some(key);
        better = Some(dir.to_owned());
    }
    let source_url = clean(b.source_url.clone());
    if source_url
        .as_deref()
        .is_some_and(|u| !(u.starts_with("https://") || u.starts_with("http://")) || u.len() > 500)
    {
        return Err(invalid("sourceUrl", "invalid_url", "an http(s) URL").into());
    }
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let cycle = match b.cycle_id {
        Some(c) => Some(active_cycle(&mut tx, t, id, c).await?),
        None if b.kind.needs_cycle() => {
            return Err(invalid("cycleId", "required", "this record belongs to a cycle").into())
        }
        None => None,
    };
    let mut criterion = None;
    if b.kind == RecordKind::Evidence {
        let code = clean(b.criterion_code.clone())
            .map(|c| c.to_uppercase())
            .ok_or_else(|| invalid("criterionCode", "required", "evidence names a criterion"))?;
        if let Some(c) = &cycle {
            let f = load_framework(&mut tx, t, c.framework_id, false).await?;
            if !f.criteria.iter().any(|x| x.code == code) {
                return Err(invalid(
                    "criterionCode",
                    "unknown_criterion",
                    "not a criterion of the cycle's framework",
                )
                .into());
            }
        }
        criterion = Some(code);
    }
    let mut snaps = b.regulatory_snapshot_ids.clone();
    snaps.sort();
    snaps.dedup();
    if !snaps.is_empty() {
        let n: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM services.report_snapshot WHERE tenant_id = $1 \
             AND institution_id = $2 AND id = ANY($3) AND status = 'validated'",
        )
        .bind(t.as_uuid())
        .bind(id)
        .bind(&snaps)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
        if n != snaps.len() as i64 {
            return Err(invalid(
                "regulatorySnapshotIds",
                "not_validated",
                "every cited regulatory snapshot must exist and be validated",
            )
            .into());
        }
    }
    let mut gov = b.governance_evidence_ids.clone();
    gov.sort();
    gov.dedup();
    let parent = match b.parent_id {
        Some(p) => {
            let pr = load_record(&mut tx, t, p, false)
                .await
                .ok()
                .filter(|r| r.institution_id == id)
                .ok_or_else(|| invalid("parentId", "unknown_record", "no such record"))?;
            if b.kind != RecordKind::Finding
                || !matches!(
                    pr.kind,
                    RecordKind::InternalAudit | RecordKind::ExternalAssessment
                )
            {
                return Err(invalid(
                    "parentId",
                    "not_allowed",
                    "only findings belong to an audit or an external assessment",
                )
                .into());
            }
            if !matches!(pr.status, RecordStatus::Draft | RecordStatus::Open) {
                return Err(KernelError::Conflict(format!(
                    "record {} is {}; findings are raised while it is open",
                    pr.number, pr.status
                ))
                .into());
            }
            Some(pr)
        }
        None => None,
    };
    let cycle_id = cycle
        .as_ref()
        .map(|c| c.id)
        .or(parent.as_ref().and_then(|p| p.cycle_id));
    let refs = serde_json::json!({"regulatorySnapshotIds": snaps, "governanceEvidenceIds": gov,
        "sourceUrl": source_url});
    let number = numbering::next(&mut tx, t, institution, RECORD_NUMBER).await?;
    let rid: Uuid = sqlx::query_scalar(
        "INSERT INTO services.accreditation_record (tenant_id, institution_id, number, kind, \
         cycle_id, criterion_code, parent_id, title, description, severity, owner, due_on, \
         metric_key, better, refs, facts, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17) \
         RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(&number)
    .bind(b.kind.as_str())
    .bind(cycle_id)
    .bind(criterion.as_deref())
    .bind(parent.as_ref().map(|p| p.id))
    .bind(&title)
    .bind(description.as_deref())
    .bind(b.severity.unwrap_or(RecordSeverity::Medium).as_str())
    .bind(clean(b.owner).as_deref())
    .bind(due_on)
    .bind(metric_key.as_deref())
    .bind(better.as_deref())
    .bind(&refs)
    .bind(&facts)
    .bind(a.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    transition(
        &mut tx,
        &scope,
        &Transition {
            aggregate_type: "accreditation_record",
            id: rid,
            institution,
            from: None,
            to: "draft",
            version: 1,
            reason: None,
            process_instance_id: None,
            number: Some(&number),
            subject_ref: None,
        },
        None,
        "services.accreditation_record.create",
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let d = detail(&st, t, rid).await?;
    Ok(respond(StatusCode::CREATED, &d))
}

/// Records of an institution (`services.accreditation.manage`).
#[utoipa::path(get, path = "/api/v1/services/institutions/{id}/accreditation-records", operation_id = "services_accreditation_record_list", tag = "services-accreditation",
    params(("id" = String, Path, format = Uuid), AccreditationRecordQuery), security(("session_cookie" = [])),
    responses((status = 200, body = AccreditationRecordList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn record_list(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<AccreditationRecordQuery>,
) -> ApiResult<Json<AccreditationRecordList>> {
    let ctx = auth.require(Permission::AccreditationManage)?;
    let institution = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, institution), institution).await?;
    let kind = match q.kind.as_deref().filter(|s| !s.is_empty()) {
        Some(k) => {
            Some(RecordKind::parse(k).ok_or_else(|| invalid("kind", "invalid", "unknown value"))?)
        }
        None => None,
    };
    let status = match q.status.as_deref().filter(|s| !s.is_empty()) {
        Some(s) => Some(
            RecordStatus::parse(s).ok_or_else(|| invalid("status", "invalid", "unknown value"))?,
        ),
        None => None,
    };
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let rows = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM services.accreditation_record WHERE tenant_id = $1 \
         AND institution_id = $2 AND ($3::text IS NULL OR kind = $3) \
         AND ($4::text IS NULL OR status = $4) AND ($5::uuid IS NULL OR cycle_id = $5) \
         ORDER BY created_at DESC LIMIT $6"
    ))
    .bind(t.as_uuid())
    .bind(id)
    .bind(kind.map(|k| k.as_str()))
    .bind(status.map(|s| s.as_str()))
    .bind(q.cycle_id)
    .bind(page_limit(q.limit))
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let mut items = Vec::with_capacity(rows.len());
    for r in &rows {
        let rec = from_row(r).map_err(map_db_error)?;
        let its = load_items(&mut conn, t, rec.id).await?;
        items.push(view(rec, its));
    }
    Ok(Json(AccreditationRecordList { items }))
}

/// One record with its items (`services.accreditation.manage`).
#[utoipa::path(get, path = "/api/v1/services/accreditation-records/{id}", operation_id = "services_accreditation_record_get", tag = "services-accreditation",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = AccreditationRecordView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn record_get(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AccreditationManage)?;
    let d = detail(&st, ctx.scope.tenant_id, id).await?;
    Ok(respond(StatusCode::OK, &d))
}

/// Start work on a draft record (`services.accreditation.manage`): draft → open.
#[utoipa::path(post, path = "/api/v1/services/accreditation-records/{id}/start", operation_id = "services_accreditation_record_start", tag = "services-accreditation",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = ReportReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = AccreditationRecordView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn record_start(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ReportReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AccreditationManage)?;
    let v = expected!(headers);
    let a = actor(ctx);
    let reason = clean(b.reason);
    let (mut tx, r, scope) = begin(&st, ctx, id, v).await?;
    move_record(
        &mut tx,
        &scope,
        &r,
        RecordStatus::Open,
        None,
        reason.as_deref(),
        None,
        None,
        "services.accreditation_record.start",
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let d = detail(&st, ctx.scope.tenant_id, id).await?;
    Ok(respond(StatusCode::OK, &d))
}

/// Cancel a draft / open record (`services.accreditation.manage`; reason required).
#[utoipa::path(post, path = "/api/v1/services/accreditation-records/{id}/cancel", operation_id = "services_accreditation_record_cancel", tag = "services-accreditation",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = ReportReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = AccreditationRecordView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn record_cancel(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ReportReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AccreditationManage)?;
    let v = expected!(headers);
    let a = actor(ctx);
    let reason = required_reason(b.reason)?;
    let (mut tx, r, scope) = begin(&st, ctx, id, v).await?;
    move_record(
        &mut tx,
        &scope,
        &r,
        RecordStatus::Cancelled,
        None,
        Some(&reason),
        None,
        None,
        "services.accreditation_record.cancel",
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let d = detail(&st, ctx.scope.tenant_id, id).await?;
    Ok(respond(StatusCode::OK, &d))
}

/// Facts gathered for the decision rules and stored on the record.
struct Gathered {
    facts: serde_json::Value,
    child_findings: i64,
    measurement: Option<Measurement>,
    coverage_missing: Vec<String>,
    open_major_findings: i64,
    computed_compliance: Option<&'static str>,
    cycle_ready: bool,
    approved_evidence: i64,
}

async fn gather(
    st: &ServicesState,
    scope: &ExecutionScope,
    r: &RecordRow,
) -> Result<Gathered, KernelError> {
    let t = scope.tenant_id;
    let inst = InstitutionId(r.institution_id);
    let mut g = Gathered {
        facts: r.facts.clone(),
        child_findings: 0,
        measurement: None,
        coverage_missing: Vec::new(),
        open_major_findings: 0,
        computed_compliance: None,
        cycle_ready: false,
        approved_evidence: 0,
    };
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    match r.kind {
        RecordKind::InternalAudit => {
            g.child_findings = sqlx::query_scalar(
                "SELECT count(*) FROM services.accreditation_record WHERE tenant_id = $1 \
                 AND parent_id = $2 AND kind = 'finding' AND status <> 'cancelled'",
            )
            .bind(t.as_uuid())
            .bind(r.id)
            .fetch_one(&mut *conn)
            .await
            .map_err(map_db_error)?;
            g.facts["findings"] = serde_json::json!(g.child_findings);
        }
        RecordKind::Improvement => {
            let baseline = g
                .facts
                .get("baseline")
                .and_then(serde_json::Value::as_str)
                .and_then(|s| Decimal::from_str(s).ok());
            let current = match r.metric_key.as_deref() {
                Some(k) => figure(st, scope, inst, k).await?,
                None => None,
            };
            if let (Some(b), Some(c)) = (baseline, current) {
                g.measurement = Some(Measurement {
                    baseline: b,
                    current: c,
                    higher_is_better: r.better.as_deref() == Some("higher"),
                });
                g.facts["current"] = serde_json::json!(c.to_string());
            }
        }
        RecordKind::Readiness | RecordKind::SelfStudy => {
            let cycle_id = r.cycle_id.unwrap_or_default();
            let c = load_cycle(&mut conn, t, cycle_id, false).await?;
            let f = load_framework(&mut conn, t, c.framework_id, false).await?;
            let evidence: Vec<(Uuid, String, Option<String>)> = sqlx::query_as(
                "SELECT id, number, criterion_code FROM services.accreditation_record \
                 WHERE tenant_id = $1 AND cycle_id = $2 AND kind = 'evidence' \
                 AND status = 'closed' AND outcome = 'approved_evidence' ORDER BY number",
            )
            .bind(t.as_uuid())
            .bind(cycle_id)
            .fetch_all(&mut *conn)
            .await
            .map_err(map_db_error)?;
            g.approved_evidence = evidence.len() as i64;
            g.coverage_missing = f
                .criteria
                .iter()
                .filter(|cr| !evidence.iter().any(|e| e.2.as_deref() == Some(&cr.code)))
                .map(|cr| cr.code.clone())
                .collect();
            g.open_major_findings = sqlx::query_scalar(
                "SELECT count(*) FROM services.accreditation_record WHERE tenant_id = $1 \
                 AND cycle_id = $2 AND kind = 'finding' AND severity IN ('high', 'critical') \
                 AND status NOT IN ('closed', 'cancelled')",
            )
            .bind(t.as_uuid())
            .bind(cycle_id)
            .fetch_one(&mut *conn)
            .await
            .map_err(map_db_error)?;
            let ready: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM services.accreditation_record WHERE tenant_id = $1 \
                 AND cycle_id = $2 AND kind = 'readiness' AND status = 'closed' AND outcome = 'ready'",
            )
            .bind(t.as_uuid())
            .bind(cycle_id)
            .fetch_one(&mut *conn)
            .await
            .map_err(map_db_error)?;
            g.cycle_ready = ready > 0;
            let ev: Vec<serde_json::Value> = evidence
                .iter()
                .map(|(id, n, c)| serde_json::json!({"id": id, "number": n, "criterionCode": c}))
                .collect();
            g.facts["coverageMissing"] = serde_json::json!(g.coverage_missing);
            g.facts["openMajorFindings"] = serde_json::json!(g.open_major_findings);
            g.facts["approvedEvidence"] = serde_json::json!(ev);
            g.facts["framework"] = serde_json::json!(format!("{} v{}", f.code, f.version_no));
        }
        RecordKind::ComplianceReview => {
            let today = business_today(st, scope, inst).await?;
            let (compliant, non_compliant, overdue): (i64, i64, i64) = sqlx::query_as(
                "SELECT count(*) FILTER (WHERE status = 'closed' AND outcome = 'compliant'), \
                 count(*) FILTER (WHERE status = 'closed' AND outcome = 'non_compliant'), \
                 count(*) FILTER (WHERE status IN ('draft', 'open', 'pending_decision') \
                                  AND due_on IS NOT NULL AND due_on < $3) \
                 FROM services.accreditation_record WHERE tenant_id = $1 AND institution_id = $2 \
                 AND kind = 'obligation'",
            )
            .bind(t.as_uuid())
            .bind(r.institution_id)
            .bind(today)
            .fetch_one(&mut *conn)
            .await
            .map_err(map_db_error)?;
            if compliant + non_compliant + overdue > 0 {
                g.computed_compliance = Some(compliance_status(compliant, non_compliant, overdue));
            }
            g.facts["obligations"] = serde_json::json!({"compliant": compliant,
                "nonCompliant": non_compliant, "overdueOpen": overdue,
                "status": g.computed_compliance});
        }
        _ => {}
    }
    Ok(g)
}

/// Submit an open record with its proposed outcome (`services.accreditation.manage`;
/// `ACCREDITATION_DECISION`): 422 with every rule the outcome breaks; auto-approval closes it at
/// once (`accreditation_record.decided`).
#[utoipa::path(post, path = "/api/v1/services/accreditation-records/{id}/submit", operation_id = "services_accreditation_record_submit", tag = "services-accreditation",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = AccreditationSubmit, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = AccreditationRecordView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn record_submit(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<AccreditationSubmit>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AccreditationManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let outcome = b.outcome.trim().to_owned();
    let rationale = optional_text("rationale", b.rationale, 4000)?;
    let grade = optional_text("grade", b.grade, 50)?;
    let (probe, items) = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        let r = load_record(&mut conn, t, id, false).await?;
        let items = load_items(&mut conn, t, id).await?;
        (r, items)
    };
    check_version(v, probe.version)?;
    if probe.status != RecordStatus::Open {
        return Err(KernelError::Conflict(format!(
            "accreditation record {} is {}; only an open record is submitted",
            probe.number, probe.status
        ))
        .into());
    }
    let inst = InstitutionId(probe.institution_id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let mut g = gather(&st, &scope, &probe).await?;
    let citations = ["regulatorySnapshotIds", "governanceEvidenceIds"]
        .iter()
        .map(|k| {
            probe
                .refs
                .get(*k)
                .and_then(serde_json::Value::as_array)
                .map_or(0, Vec::len)
        })
        .sum::<usize>()
        + usize::from(
            probe
                .refs
                .get("sourceUrl")
                .and_then(serde_json::Value::as_str)
                .is_some(),
        );
    let facts: Vec<ItemFact> = items
        .iter()
        .map(|i| ItemFact {
            kind: i.kind,
            status: i.status,
        })
        .collect();
    let problems = record_problems(&RecordFacts {
        kind: probe.kind,
        severity: probe.severity,
        outcome: &outcome,
        rationale: rationale.as_deref(),
        items: &facts,
        citations,
        child_findings: g.child_findings,
        grade: grade.as_deref(),
        measurement: g.measurement,
        coverage_missing: &g.coverage_missing,
        open_major_findings: g.open_major_findings,
        computed_compliance: g.computed_compliance,
        cycle_ready: g.cycle_ready,
        approved_evidence: g.approved_evidence,
    });
    if !problems.is_empty() {
        return Err(KernelError::Validation(
            problems
                .into_iter()
                .map(|p| FieldViolation::new("outcome", "decision_rule", p))
                .collect(),
        )
        .into());
    }
    if let Some(gr) = &grade {
        g.facts["grade"] = serde_json::json!(gr);
    }
    let req = request_approval(
        &st,
        &scope,
        process::ACCREDITATION_DECISION,
        business_key("accreditation_record", id, probe.version),
        BTreeMap::from([
            ("recordId".to_owned(), serde_json::json!(id)),
            ("kind".to_owned(), serde_json::json!(probe.kind)),
            ("outcome".to_owned(), serde_json::json!(outcome)),
        ]),
    )
    .await?;
    let pid = req.process_instance_id().to_owned();
    let (mut tx, r, scope) = begin(&st, ctx, id, v).await?;
    sqlx::query(
        "UPDATE services.accreditation_record SET outcome = $3, rationale = $4, facts = $5 \
         WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(t.as_uuid())
    .bind(&outcome)
    .bind(rationale.as_deref())
    .bind(&g.facts)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    move_record(
        &mut tx,
        &scope,
        &r,
        RecordStatus::PendingDecision,
        Some(AccreditationApprovalState::Pending),
        rationale.as_deref(),
        Some(&pid),
        None,
        "services.accreditation_record.submit",
        &a,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = req {
        let cur = load_record(&mut tx, t, id, true).await?;
        move_record(
            &mut tx,
            &scope,
            &cur,
            RecordStatus::Closed,
            Some(AccreditationApprovalState::Approved),
            Some("auto-approved"),
            Some(&pid),
            Some((
                ACCREDITATION_RECORD_DECIDED,
                ACCREDITATION_RECORD_DECIDED_SCHEMA,
            )),
            "services.accreditation_record.approve",
            &Actor::system(a.correlation_id.clone()),
        )
        .await?;
    }
    tx.commit().await.map_err(map_db_error)?;
    let d = detail(&st, t, id).await?;
    Ok(respond(StatusCode::OK, &d))
}

/// Decide a submitted record (`services.report.approve`, step-up): approve → closed with its
/// outcome; reject (reason required) → open, outcome cleared.
#[utoipa::path(post, path = "/api/v1/services/accreditation-records/{id}/decision", operation_id = "services_accreditation_record_decide", tag = "services-accreditation",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = ReportDecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = AccreditationRecordView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn record_decide(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ReportDecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ReportApprove)?;
    let v = expected!(headers);
    let reason = clean(b.reason.clone());
    let approve = b.decision == ReportDecision::Approve;
    if !approve && reason.is_none() {
        return Err(invalid("reason", "required", "a rejection needs a reason").into());
    }
    let a = actor(ctx);
    let (mut tx, r, scope) = begin(&st, ctx, id, v).await?;
    if r.status != RecordStatus::PendingDecision
        || r.approval_state != AccreditationApprovalState::Pending
    {
        return Err(KernelError::Conflict(format!(
            "accreditation record {} is not waiting for a decision",
            r.number
        ))
        .into());
    }
    let ev = Some((
        ACCREDITATION_RECORD_DECIDED,
        ACCREDITATION_RECORD_DECIDED_SCHEMA,
    ));
    if approve {
        move_record(
            &mut tx,
            &scope,
            &r,
            RecordStatus::Closed,
            Some(AccreditationApprovalState::Approved),
            reason.as_deref(),
            None,
            ev,
            "services.accreditation_record.approve",
            &a,
        )
        .await?;
    } else {
        sqlx::query("UPDATE services.accreditation_record SET outcome = NULL WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
        move_record(
            &mut tx,
            &scope,
            &r,
            RecordStatus::Open,
            Some(AccreditationApprovalState::Rejected),
            reason.as_deref(),
            None,
            ev,
            "services.accreditation_record.reject",
            &a,
        )
        .await?;
    }
    tx.commit().await.map_err(map_db_error)?;
    let d = detail(&st, ctx.scope.tenant_id, id).await?;
    Ok(respond(StatusCode::OK, &d))
}

/// Hand an approved self-study report off to the accrediting body (H-071;
/// `services.accreditation.manage`): records the portal reference once, moves the cycle to
/// `submitted` and emits `accreditation_record.handed_off`.
#[utoipa::path(post, path = "/api/v1/services/accreditation-records/{id}/handoff", operation_id = "services_accreditation_record_handoff", tag = "services-accreditation",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = AccreditationHandoff, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = AccreditationRecordView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn record_handoff(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<AccreditationHandoff>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AccreditationManage)?;
    let v = expected!(headers);
    let reference = required_text("reference", &b.reference, 200)?;
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let (mut tx, r, scope) = begin(&st, ctx, id, v).await?;
    if r.kind != RecordKind::SelfStudy
        || r.status != RecordStatus::Closed
        || r.handed_off_at.is_some()
    {
        return Err(KernelError::Conflict(format!(
            "accreditation record {} is not an approved self-study report awaiting hand-off",
            r.number
        ))
        .into());
    }
    let nv: i64 = sqlx::query_scalar(
        "UPDATE services.accreditation_record SET handed_off_at = now(), handoff_reference = $3, \
         version = version + 1, updated_at = now() WHERE id = $1 AND tenant_id = $2 RETURNING version",
    )
    .bind(id)
    .bind(t.as_uuid())
    .bind(&reference)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    transition(
        &mut tx,
        &scope,
        &Transition {
            aggregate_type: "accreditation_record",
            id,
            institution: InstitutionId(r.institution_id),
            from: Some("closed"),
            to: "handed_off",
            version: nv,
            reason: Some(&reference),
            process_instance_id: r.process_instance_id.as_deref(),
            number: Some(&r.number),
            subject_ref: None,
        },
        Some((SELF_STUDY_HANDED_OFF, SELF_STUDY_HANDED_OFF_SCHEMA)),
        "services.accreditation_record.handoff",
        &a,
    )
    .await?;
    if let Some(cid) = r.cycle_id {
        let c = load_cycle(&mut tx, t, cid, true).await?;
        if c.status == CycleStatus::Active {
            set_cycle_status(
                &mut tx,
                &scope,
                &c,
                CycleStatus::Submitted,
                Some(&reference),
                &a,
            )
            .await?;
        }
    }
    tx.commit().await.map_err(map_db_error)?;
    let d = detail(&st, t, id).await?;
    Ok(respond(StatusCode::OK, &d))
}

/// Add an item to a draft / open record (`services.accreditation.manage`; Idempotency-Key).
#[utoipa::path(post, path = "/api/v1/services/accreditation-records/{id}/items", operation_id = "services_accreditation_item_add", tag = "services-accreditation",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)),
    request_body = AccreditationItemCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = AccreditationRecordView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn item_add(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<AccreditationItemCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AccreditationManage)?;
    require_idempotency_key(&headers)?;
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let title = required_text("title", &b.title, 200)?;
    let due_on = b
        .due_on
        .as_deref()
        .map(|r| parse_day("dueOn", r))
        .transpose()?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let r = load_record(&mut tx, t, id, true).await?;
    let inst = InstitutionId(r.institution_id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    editable(&r)?;
    if !r.kind.item_kinds().contains(&b.kind) {
        return Err(invalid(
            "kind",
            "not_for_record",
            &format!("a {} does not take {} items", r.kind, b.kind),
        )
        .into());
    }
    let item: Uuid = sqlx::query_scalar(
        "INSERT INTO services.accreditation_item (tenant_id, institution_id, record_id, kind, \
         title, owner, due_on) VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(r.institution_id)
    .bind(id)
    .bind(b.kind.as_str())
    .bind(&title)
    .bind(clean(b.owner).as_deref())
    .bind(due_on)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    crate::shared::publish::audit(
        &mut tx,
        &scope,
        "services.accreditation_item.add",
        "accreditation_item",
        item.to_string(),
        serde_json::json!({"recordId": id, "kind": b.kind, "title": title}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let d = detail(&st, t, id).await?;
    Ok(respond(StatusCode::CREATED, &d))
}

async fn load_item_locked(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
) -> Result<AccreditationItemView, KernelError> {
    let sql = format!("{ITEM_SQL} WHERE id = $1 AND tenant_id = $2 FOR UPDATE");
    let row = sqlx::query(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "accreditation item",
        })?;
    item_from_row(&row).map_err(map_db_error)
}

/// Progress an item (`services.accreditation.manage`; If-Match = the item's version). Returns
/// the record.
#[utoipa::path(post, path = "/api/v1/services/accreditation-items/{id}/progress", operation_id = "services_accreditation_item_progress", tag = "services-accreditation",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = AccreditationItemProgress, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = AccreditationRecordView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn item_progress(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<AccreditationItemProgress>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AccreditationManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    if b.status == RecordItemStatus::Verified {
        return Err(invalid("status", "use_verify", "verify with POST …/verify").into());
    }
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let item = load_item_locked(&mut tx, t, id).await?;
    check_version(v, item.version)?;
    let r = load_record(&mut tx, t, item.record_id, true).await?;
    let inst = InstitutionId(r.institution_id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    editable(&r)?;
    if !item.status.can_transition(b.status) {
        return Err(KernelError::Conflict(format!(
            "item {} is {}; it cannot become {}",
            item.title, item.status, b.status
        ))
        .into());
    }
    let done = b.status == RecordItemStatus::Done;
    sqlx::query(
        "UPDATE services.accreditation_item SET status = $3, note = COALESCE($4, note), \
         evidence_ref = COALESCE($5, evidence_ref), \
         done_by = CASE WHEN $6 THEN $7 ELSE done_by END, \
         done_at = CASE WHEN $6 THEN now() ELSE done_at END, \
         version = version + 1, updated_at = now() WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(t.as_uuid())
    .bind(b.status.as_str())
    .bind(clean(b.note).as_deref())
    .bind(clean(b.evidence_ref).as_deref())
    .bind(done)
    .bind(a.id.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    crate::shared::publish::audit(
        &mut tx,
        &scope,
        "services.accreditation_item.progress",
        "accreditation_item",
        id.to_string(),
        serde_json::json!({"from": item.status, "to": b.status}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let d = detail(&st, t, item.record_id).await?;
    Ok(respond(StatusCode::OK, &d))
}

/// Verify a done item (`services.accreditation.manage`; evidence reference required). Returns
/// the record.
#[utoipa::path(post, path = "/api/v1/services/accreditation-items/{id}/verify", operation_id = "services_accreditation_item_verify", tag = "services-accreditation",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = AccreditationItemVerify, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = AccreditationRecordView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn item_verify(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<AccreditationItemVerify>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AccreditationManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let item = load_item_locked(&mut tx, t, id).await?;
    check_version(v, item.version)?;
    let r = load_record(&mut tx, t, item.record_id, true).await?;
    let inst = InstitutionId(r.institution_id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    editable(&r)?;
    if item.status != RecordItemStatus::Done {
        return Err(KernelError::Conflict(format!(
            "item {} is {}; only a done item is verified",
            item.title, item.status
        ))
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
        "UPDATE services.accreditation_item SET status = 'verified', evidence_ref = $3, \
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
    crate::shared::publish::audit(
        &mut tx,
        &scope,
        "services.accreditation_item.verify",
        "accreditation_item",
        id.to_string(),
        serde_json::json!({"recordId": item.record_id, "evidenceRef": evidence}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let d = detail(&st, t, item.record_id).await?;
    Ok(respond(StatusCode::OK, &d))
}

pub fn router(state: ServicesState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(record_create, record_list))
        .routes(routes!(record_get))
        .routes(routes!(record_start))
        .routes(routes!(record_cancel))
        .routes(routes!(record_submit))
        .routes(routes!(record_decide))
        .routes(routes!(record_handoff))
        .routes(routes!(item_add))
        .routes(routes!(item_progress))
        .routes(routes!(item_verify))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers() {
        assert_eq!(numbering::format_number(RECORD_NUMBER.1, 12), "ACR-000012");
        assert!(parse_day("dueOn", "2026-13-01").is_err());
    }
}
