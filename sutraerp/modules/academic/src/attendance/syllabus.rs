//! Syllabus coverage (L3-D-026): actual teaching progress per section → published record.
//! A published record is immutable; a later change is a new record superseding it.

use super::domain;
use crate::shared::faculty::require_teaching;
use crate::shared::guard::require_institution_writable;
use crate::shared::{AcademicState, Actor};
use crate::timetable::kit::{self, check_version, illegal, invalid, StatusEvent};
use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::PgConnection;
use sutra_contracts::academic::events::delivery::{
    SYLLABUS_COVERAGE_PUBLISHED, SYLLABUS_COVERAGE_PUBLISHED_SCHEMA,
};
use sutra_contracts::academic::ids::OfferingSectionId;
use sutra_contracts::academic::status::{SyllabusCoverageStatus, TopicCoverageStatus};
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::TenantId;
use sutra_persistence::map_db_error;
use uuid::Uuid;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SyllabusTopic {
    pub id: Uuid,
    pub position: i32,
    pub title: String,
    /// Curriculum learning-outcome reference (Domain B, plain text until outcomes are modelled).
    pub learning_outcome_ref: Option<String>,
    pub session_id: Option<Uuid>,
    pub status: TopicCoverageStatus,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SyllabusCoverage {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub offering_id: Uuid,
    pub section_id: Uuid,
    pub summary: Option<String>,
    pub status: SyllabusCoverageStatus,
    pub supersedes_id: Option<Uuid>,
    pub submitted_by: Option<String>,
    pub published_by: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub topics: Vec<SyllabusTopic>,
    /// Share of topics `completed` (0–100).
    #[serde(default)]
    pub completed_percent: f64,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct SyllabusCoverageList {
    pub items: Vec<SyllabusCoverage>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TopicInput {
    pub title: String,
    pub learning_outcome_ref: Option<String>,
    pub session_id: Option<Uuid>,
    pub status: TopicCoverageStatus,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewSyllabusCoverage {
    pub summary: Option<String>,
    /// Omitted: copies the topics of the latest published record (a correction is new history).
    pub topics: Option<Vec<TopicInput>>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReplaceTopics {
    pub summary: Option<String>,
    pub topics: Vec<TopicInput>,
}

const SC: &str = "syllabus_coverage";
const SC_ENTITY: &str = "syllabus coverage";

async fn with_topics<'e, E: sqlx::PgExecutor<'e>>(
    ex: E,
    tenant: TenantId,
    mut c: SyllabusCoverage,
) -> Result<SyllabusCoverage, KernelError> {
    c.topics = kit::list_by(
        ex,
        "syllabus_topic",
        tenant,
        "coverage_id",
        c.id,
        None,
        "t.position",
        1000,
    )
    .await?;
    let done = c
        .topics
        .iter()
        .filter(|t| t.status == TopicCoverageStatus::Completed)
        .count();
    c.completed_percent = if c.topics.is_empty() {
        0.0
    } else {
        (done as f64 * 10000.0 / c.topics.len() as f64).round() / 100.0
    };
    Ok(c)
}

pub async fn get(
    state: &AcademicState,
    tenant: TenantId,
    id: Uuid,
) -> Result<SyllabusCoverage, KernelError> {
    let c: SyllabusCoverage = kit::fetch(&state.pool, SC, SC_ENTITY, tenant, id).await?;
    with_topics(&state.pool, tenant, c).await
}

pub async fn list(
    state: &AcademicState,
    tenant: TenantId,
    section: Uuid,
) -> Result<Vec<SyllabusCoverage>, KernelError> {
    let rows: Vec<SyllabusCoverage> = kit::list_by(
        &state.pool,
        SC,
        tenant,
        "section_id",
        section,
        None,
        "t.created_at DESC",
        100,
    )
    .await?;
    let mut out = Vec::new();
    for c in rows {
        out.push(with_topics(&state.pool, tenant, c).await?);
    }
    Ok(out)
}

async fn write_topics(
    conn: &mut PgConnection,
    tenant: TenantId,
    coverage: Uuid,
    section: Uuid,
    topics: &[TopicInput],
) -> Result<(), KernelError> {
    for (i, t) in topics.iter().enumerate() {
        let title = kit::required_text(&format!("topics[{i}].title"), Some(&t.title))?;
        if let Some(sid) = t.session_id {
            let sec: Option<Uuid> = sqlx::query_scalar(
                "SELECT section_id FROM academic.teaching_session WHERE id = $1 AND tenant_id = $2",
            )
            .bind(sid)
            .bind(tenant.as_uuid())
            .fetch_optional(&mut *conn)
            .await
            .map_err(map_db_error)?;
            if sec != Some(section) {
                return Err(invalid(
                    &format!("topics[{i}].sessionId"),
                    "section",
                    "the session belongs to another section",
                ));
            }
        }
        kit::append(conn, "syllabus_topic", json!({
            "tenant_id": tenant.as_uuid(), "coverage_id": coverage, "position": i + 1, "title": title,
            "learning_outcome_ref": kit::opt_text(t.learning_outcome_ref.as_deref()), "session_id": t.session_id,
            "status": t.status.as_str(), "notes": kit::opt_text(t.notes.as_deref()),
        })).await?;
    }
    Ok(())
}

pub async fn create(
    state: &AcademicState,
    auth: &AuthContext,
    section: Uuid,
    cmd: NewSyllabusCoverage,
    actor: &Actor,
) -> Result<SyllabusCoverage, KernelError> {
    require_teaching(
        state,
        auth,
        OfferingSectionId(section),
        Permission::AttendanceManage,
    )
    .await?;
    let c = kit::deliverable_section(state, auth, section).await?;
    let tenant = auth.scope.tenant_id;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let latest: Option<serde_json::Value> = sqlx::query_scalar(
        "SELECT to_jsonb(t) FROM academic.syllabus_coverage t WHERE t.tenant_id = $1 AND t.section_id = $2 AND t.status = 'published' \
         ORDER BY t.published_at DESC LIMIT 1",
    )
    .bind(tenant.as_uuid())
    .bind(section)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let latest: Option<SyllabusCoverage> = latest.map(kit::decode).transpose()?;
    let cov: SyllabusCoverage = kit::insert(&mut tx, SC, json!({
        "tenant_id": tenant.as_uuid(), "institution_id": c.institution_uuid(), "offering_id": c.offering.id.as_uuid(),
        "section_id": section, "summary": kit::opt_text(cmd.summary.as_deref()), "supersedes_id": latest.as_ref().map(|l| l.id),
    }))
    .await
    .map_err(|e| match e {
        KernelError::Conflict(_) => KernelError::Conflict("the section already has a coverage record in progress".into()),
        other => other,
    })?;
    match (cmd.topics, latest) {
        (Some(topics), _) => write_topics(&mut tx, tenant, cov.id, section, &topics).await?,
        (None, Some(prev)) => {
            sqlx::query(
                "INSERT INTO academic.syllabus_topic (tenant_id, coverage_id, position, title, learning_outcome_ref, session_id, status, notes) \
                 SELECT tenant_id, $2, position, title, learning_outcome_ref, session_id, status, notes FROM academic.syllabus_topic WHERE coverage_id = $1",
            )
            .bind(prev.id)
            .bind(cov.id)
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
        }
        (None, None) => {}
    }
    kit::audit(
        &mut tx,
        &c.scope,
        SC,
        "create",
        cov.id,
        json!({"sectionId": section}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get(state, tenant, cov.id).await
}

struct Locked {
    cov: SyllabusCoverage,
    ctx: kit::SectionCtx,
}

async fn lock_cov(
    state: &AcademicState,
    conn: &mut PgConnection,
    auth: &AuthContext,
    id: Uuid,
    expected: Version,
    manage: Permission,
) -> Result<Locked, KernelError> {
    let tenant = auth.scope.tenant_id;
    let cur: SyllabusCoverage = kit::fetch(&state.pool, SC, SC_ENTITY, tenant, id).await?;
    require_teaching(state, auth, OfferingSectionId(cur.section_id), manage).await?;
    let ctx = kit::section_ctx(state, auth, cur.section_id).await?;
    require_institution_writable(state, &ctx.scope, ctx.section.institution_id).await?;
    let cov: SyllabusCoverage = kit::lock(conn, SC, SC_ENTITY, tenant, id).await?;
    check_version(cov.version, expected)?;
    Ok(Locked { cov, ctx })
}

pub async fn replace_topics(
    state: &AcademicState,
    auth: &AuthContext,
    id: Uuid,
    expected: Version,
    cmd: ReplaceTopics,
    actor: &Actor,
) -> Result<SyllabusCoverage, KernelError> {
    let tenant = auth.scope.tenant_id;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let Locked { cov, ctx } = lock_cov(
        state,
        &mut tx,
        auth,
        id,
        expected,
        Permission::AttendanceManage,
    )
    .await?;
    if cov.status == SyllabusCoverageStatus::Published {
        return Err(KernelError::Conflict(
            "a published coverage record is immutable; start a new record".into(),
        ));
    }
    sqlx::query("DELETE FROM academic.syllabus_topic WHERE coverage_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    write_topics(&mut tx, tenant, id, cov.section_id, &cmd.topics).await?;
    // editing a submitted record sends it back to draft
    kit::patch::<SyllabusCoverage>(&mut tx, SC, tenant, id, json!({"summary": kit::opt_text(cmd.summary.as_deref()).or(cov.summary), "status": "draft"})).await?;
    kit::audit(
        &mut tx,
        &ctx.scope,
        SC,
        "update_topics",
        id,
        json!({"topics": cmd.topics.len()}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get(state, tenant, id).await
}

pub async fn submit(
    state: &AcademicState,
    auth: &AuthContext,
    id: Uuid,
    expected: Version,
    actor: &Actor,
) -> Result<SyllabusCoverage, KernelError> {
    let tenant = auth.scope.tenant_id;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let Locked { cov, ctx } = lock_cov(
        state,
        &mut tx,
        auth,
        id,
        expected,
        Permission::AttendanceManage,
    )
    .await?;
    if !domain::coverage_can_transition(cov.status, SyllabusCoverageStatus::Submitted) {
        return Err(illegal(
            "syllabus coverage",
            cov.status,
            SyllabusCoverageStatus::Submitted,
        ));
    }
    let n: i64 =
        sqlx::query_scalar("SELECT count(*) FROM academic.syllabus_topic WHERE coverage_id = $1")
            .bind(id)
            .fetch_one(&mut *tx)
            .await
            .map_err(map_db_error)?;
    if n == 0 {
        return Err(KernelError::Conflict(
            "record at least one topic before submitting".into(),
        ));
    }
    kit::patch::<SyllabusCoverage>(
        &mut tx,
        SC,
        tenant,
        id,
        json!({"status": "submitted", "submitted_by": actor.id}),
    )
    .await?;
    kit::audit(&mut tx, &ctx.scope, SC, "submit", id, json!({}), actor).await?;
    tx.commit().await.map_err(map_db_error)?;
    get(state, tenant, id).await
}

/// Publication decision (`attendance.manage`, L4-D-026-BH-005): validates topic/session consistency.
pub async fn publish(
    state: &AcademicState,
    auth: &AuthContext,
    id: Uuid,
    expected: Version,
    actor: &Actor,
) -> Result<SyllabusCoverage, KernelError> {
    auth.require(Permission::AttendanceManage)?;
    let tenant = auth.scope.tenant_id;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let Locked { cov, ctx } = lock_cov(
        state,
        &mut tx,
        auth,
        id,
        expected,
        Permission::AttendanceManage,
    )
    .await?;
    if cov.status != SyllabusCoverageStatus::Submitted {
        return Err(illegal(
            "syllabus coverage",
            cov.status,
            SyllabusCoverageStatus::Published,
        ));
    }
    // topics marked started/completed against a session need that session to have been taught
    let bad: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM academic.syllabus_topic t JOIN academic.teaching_session s ON s.id = t.session_id \
         WHERE t.coverage_id = $1 AND t.status IN ('started', 'completed') AND s.status NOT IN ('started', 'in_progress', 'paused', 'completed', 'locked', 'archived')",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if bad > 0 {
        return Err(KernelError::Conflict(format!(
            "{bad} topic(s) claim coverage in a session that was not taught"
        )));
    }
    // a superseded record stays published as history (L4-D-026-BH-007)
    let ns: SyllabusCoverage = kit::patch(
        &mut tx,
        SC,
        tenant,
        id,
        json!({"status": "published", "published_by": actor.id, "published_at": Utc::now()}),
    )
    .await?;
    StatusEvent {
        aggregate_id: id,
        institution_id: ctx.institution_uuid(),
        offering_id: Some(ns.offering_id),
        section_id: Some(ns.section_id),
        from: Some(cov.status.as_str()),
        to: "published",
        version: ns.version,
        ..Default::default()
    }
    .emit(
        &mut tx,
        &ctx.scope,
        (
            SYLLABUS_COVERAGE_PUBLISHED,
            SYLLABUS_COVERAGE_PUBLISHED_SCHEMA,
        ),
        actor,
    )
    .await?;
    kit::audit(
        &mut tx,
        &ctx.scope,
        SC,
        "publish",
        id,
        json!({"supersedes": cov.supersedes_id}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get(state, tenant, id).await
}
