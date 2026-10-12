//! Governed publication engine of the 2B versioned definitions (academic structure, curriculum
//! version, school model), following the WorkflowPort pattern of contract §9:
//!
//! 1. validate the draft and its publication coherence (read only);
//! 2. `request_approval` before the transaction (business key `<entity>:<id>:v<version>`);
//! 3. one transaction: re-check version + coherence, `draft → in_review` with the process
//!    instance, and — when auto-approved — the publication itself (predecessor `superseded`,
//!    this version `published`, `*.published` event) under the system actor;
//! 4. `POST …/decision` completes an `in_review` definition (approve / reject / return).

use super::domain::{ensure, parse_status};
use super::support::{audit_transition, begin, expect_version, institution_of, Decision};
use super::{framework, school, structure};
use crate::shared::approval::{self, process, ApprovalOutcome};
use crate::shared::{Actor, InstitutionState};
use sqlx::{PgConnection, PgPool, Row};
use std::collections::BTreeMap;
use sutra_contracts::institution::status::PublicationStatus;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

/// The versioned definitions that go through a governed publication.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Structure,
    CurriculumVersion,
    SchoolModel,
}

impl Kind {
    pub(crate) fn table(self) -> &'static str {
        match self {
            Self::Structure => "academic_structure",
            Self::CurriculumVersion => "curriculum_version",
            Self::SchoolModel => "school_academic_model",
        }
    }

    /// Aggregate name used for `NotFound`, audit targets and business keys.
    pub(crate) fn entity(self) -> &'static str {
        match self {
            Self::Structure => "academic_structure",
            Self::CurriculumVersion => "curriculum_version",
            Self::SchoolModel => "school_model",
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Structure => "academic structure",
            Self::CurriculumVersion => "curriculum version",
            Self::SchoolModel => "school academic model",
        }
    }

    fn process(self) -> &'static str {
        match self {
            Self::Structure => process::ACADEMIC_STRUCTURE_PUBLICATION,
            Self::CurriculumVersion => process::CURRICULUM_PUBLICATION,
            Self::SchoolModel => process::SCHOOL_MODEL_PUBLICATION,
        }
    }

    fn action(self, verb: &str) -> String {
        format!("institution.{}.{verb}", self.entity())
    }
}

/// Status + version of a row of the session tenant (optionally `FOR UPDATE`).
pub(crate) async fn head(
    conn: &mut PgConnection,
    tenant: &ExecutionScope,
    table: &'static str,
    entity: &'static str,
    id: Uuid,
    lock: bool,
) -> Result<(PublicationStatus, i64), KernelError> {
    let row = sqlx::query(&format!(
        "SELECT status, version FROM institution.{table} WHERE id = $1 AND tenant_id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    ))
    .bind(id)
    .bind(tenant.tenant_id.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound { entity })?;
    let status: String = row.try_get("status").map_err(map_db_error)?;
    Ok((
        parse_status(&status).map_err(map_db_error)?,
        row.try_get("version").map_err(map_db_error)?,
    ))
}

/// Moves a row to `to` (version + 1), maintaining the bookkeeping columns. Returns the new
/// version.
pub(crate) async fn move_status(
    conn: &mut PgConnection,
    tenant: &ExecutionScope,
    table: &'static str,
    id: Uuid,
    to: PublicationStatus,
    reason: Option<&str>,
    process_instance_id: Option<&str>,
) -> Result<i64, KernelError> {
    sqlx::query_scalar(&format!(
        "UPDATE institution.{table} SET status = $1, status_reason = $2, \
         process_instance_id = COALESCE($3, process_instance_id), \
         published_at = CASE WHEN $1 = 'published' THEN now() ELSE published_at END, \
         retired_at = CASE WHEN $1 = 'retired' THEN now() ELSE retired_at END, \
         version = version + 1, updated_at = now() \
         WHERE id = $4 AND tenant_id = $5 RETURNING version"
    ))
    .bind(to.as_str())
    .bind(reason)
    .bind(process_instance_id)
    .bind(id)
    .bind(tenant.tenant_id.as_uuid())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)
}

/// Sets the currently published row of a lineage (`<lineage_col> = lineage`) to `superseded`
/// and returns its id. Runs before the successor is published (one published row per lineage).
pub(crate) async fn supersede_current(
    conn: &mut PgConnection,
    tenant: &ExecutionScope,
    table: &'static str,
    lineage_col: &'static str,
    lineage: Uuid,
    except: Uuid,
) -> Result<Option<Uuid>, KernelError> {
    sqlx::query_scalar(&format!(
        "UPDATE institution.{table} SET status = 'superseded', version = version + 1, \
         updated_at = now() WHERE tenant_id = $1 AND {lineage_col} = $2 AND status = 'published' \
         AND id <> $3 RETURNING id"
    ))
    .bind(tenant.tenant_id.as_uuid())
    .bind(lineage)
    .bind(except)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)
}

async fn validate(
    kind: Kind,
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    id: Uuid,
) -> Result<(), KernelError> {
    match kind {
        Kind::Structure => structure::validate_publication(conn, scope, id).await,
        Kind::CurriculumVersion => framework::validate_publication(conn, scope, id).await,
        Kind::SchoolModel => school::validate_publication(conn, scope, id).await,
    }
}

async fn finalize(
    kind: Kind,
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    id: Uuid,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<(), KernelError> {
    match kind {
        Kind::Structure => structure::finalize_publication(conn, scope, id, reason, actor).await,
        Kind::CurriculumVersion => {
            framework::finalize_publication(conn, scope, id, reason, actor).await
        }
        Kind::SchoolModel => school::finalize_publication(conn, scope, id, reason, actor).await,
    }
}

async fn institution_scope_of(
    pool: &PgPool,
    tenant: &ExecutionScope,
    kind: Kind,
    id: Uuid,
) -> Result<(ExecutionScope, InstitutionId), KernelError> {
    let inst = institution_of(pool, tenant, kind.table(), kind.entity(), id).await?;
    Ok((ExecutionScope::institution(tenant.tenant_id, inst), inst))
}

/// `POST …/publish`: submit for publication (and publish at once under auto-approval).
pub async fn submit(
    st: &InstitutionState,
    tenant: &ExecutionScope,
    kind: Kind,
    id: Uuid,
    expected: Version,
    actor: &Actor,
) -> Result<(), KernelError> {
    let (scope, inst) = institution_scope_of(&st.pool, tenant, kind, id).await?;
    {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        let (status, version) =
            head(&mut conn, &scope, kind.table(), kind.entity(), id, false).await?;
        expect_version(expected, version)?;
        ensure(kind.label(), status, PublicationStatus::InReview)?;
        validate(kind, &mut conn, &scope, id).await?;
    }
    let outcome = approval::request_approval(
        st,
        &scope,
        kind.process(),
        approval::business_key(kind.entity(), id, expected.0),
        BTreeMap::from([("kind".to_owned(), serde_json::json!(kind.entity()))]),
    )
    .await?;
    let pid = outcome.process_instance_id().to_owned();
    let mut tx = begin(&st.pool, &scope, inst).await?;
    let (status, version) = head(&mut tx, &scope, kind.table(), kind.entity(), id, true).await?;
    expect_version(expected, version)?;
    ensure(kind.label(), status, PublicationStatus::InReview)?;
    validate(kind, &mut tx, &scope, id).await?;
    let v = move_status(
        &mut tx,
        &scope,
        kind.table(),
        id,
        PublicationStatus::InReview,
        None,
        Some(&pid),
    )
    .await?;
    audit_transition(
        &mut tx,
        &scope,
        &kind.action("submit"),
        kind.entity(),
        id,
        status.as_str(),
        PublicationStatus::InReview.as_str(),
        v,
        None,
        Some(&pid),
        actor,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        let system = Actor::system(actor.correlation_id.clone());
        finalize(kind, &mut tx, &scope, id, None, &system).await?;
    }
    tx.commit().await.map_err(map_db_error)?;
    Ok(())
}

/// `POST …/decision` on an `in_review` definition.
#[allow(clippy::too_many_arguments)]
pub async fn decide(
    pool: &PgPool,
    tenant: &ExecutionScope,
    kind: Kind,
    id: Uuid,
    expected: Version,
    decision: Decision,
    reason: Option<String>,
    actor: &Actor,
) -> Result<(), KernelError> {
    let (scope, inst) = institution_scope_of(pool, tenant, kind, id).await?;
    let mut tx = begin(pool, &scope, inst).await?;
    let (status, version) = head(&mut tx, &scope, kind.table(), kind.entity(), id, true).await?;
    expect_version(expected, version)?;
    if status != PublicationStatus::InReview {
        return Err(KernelError::Conflict(format!(
            "{} is {status}; only an in_review definition can be decided",
            kind.label()
        )));
    }
    match decision {
        Decision::Approve => {
            validate(kind, &mut tx, &scope, id).await?;
            finalize(kind, &mut tx, &scope, id, reason.as_deref(), actor).await?;
        }
        Decision::Reject | Decision::Return => {
            ensure(kind.label(), status, PublicationStatus::Draft)?;
            let v = move_status(
                &mut tx,
                &scope,
                kind.table(),
                id,
                PublicationStatus::Draft,
                reason.as_deref(),
                None,
            )
            .await?;
            let verb = if decision == Decision::Reject {
                "reject"
            } else {
                "return"
            };
            audit_transition(
                &mut tx,
                &scope,
                &kind.action(verb),
                kind.entity(),
                id,
                status.as_str(),
                PublicationStatus::Draft.as_str(),
                v,
                reason.as_deref(),
                None,
                actor,
            )
            .await?;
        }
    }
    tx.commit().await.map_err(map_db_error)?;
    Ok(())
}
