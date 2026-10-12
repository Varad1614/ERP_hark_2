//! PostgreSQL repository for the reference aggregate. Only this module touches the
//! `institution` schema (ADR-L5-0036 §3.2).

use crate::domain::{Institution, RegisterInstitution};
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use sutra_contracts::institution::events::{
    InstitutionRegisteredV1, InstitutionRenamedV1, INSTITUTION_REGISTERED,
    INSTITUTION_REGISTERED_SCHEMA, INSTITUTION_RENAMED, INSTITUTION_RENAMED_SCHEMA,
};
use sutra_contracts::institution::status::InstitutionStatus;
use sutra_contracts::institution::InstitutionDirectory;
use sutra_kernel::audit::{ActorKind, AuditEntry, AuditOutcome};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::events::{EventEnvelope, NewEvent};
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::{audit, map_db_error, outbox};

use sutra_contracts::institution::events::SOURCE;
/// Columns of one institution row (shared with the 2A lifecycle repository).
pub(crate) const COLUMNS: &str = "id, tenant_id, code, name, timezone, status, status_changed_at, \
    short_name, institution_type, affiliated_university, regulatory_bodies, registration_number, \
    address, contact_phone, official_email, website, academic_pattern, working_days, \
    default_language, administrator_email, profile_revision, version, created_at, updated_at";

pub(crate) fn from_row(r: &sqlx::postgres::PgRow) -> Result<Institution, sqlx::Error> {
    let status: String = r.try_get("status")?;
    Ok(Institution {
        id: InstitutionId(r.try_get("id")?),
        tenant_id: TenantId(r.try_get("tenant_id")?),
        code: r.try_get("code")?,
        name: r.try_get("name")?,
        timezone: r.try_get("timezone")?,
        status: InstitutionStatus::parse(&status).ok_or_else(|| {
            sqlx::Error::Decode(format!("unknown institution status `{status}`").into())
        })?,
        status_changed_at: r.try_get("status_changed_at")?,
        profile: crate::lifecycle::domain::InstitutionProfile::from_row(r)?,
        version: Version(r.try_get("version")?),
        created_at: r.try_get::<DateTime<Utc>, _>("created_at")?,
        updated_at: r.try_get::<DateTime<Utc>, _>("updated_at")?,
    })
}

/// Context of the caller performing a mutation (shared by every Domain B slice).
pub use crate::shared::Actor;

pub async fn register(
    pool: &PgPool,
    scope: &ExecutionScope,
    cmd: RegisterInstitution,
    actor: &Actor,
) -> Result<Institution, KernelError> {
    cmd.validate()?;
    if let Some(tz) = cmd.timezone.as_deref() {
        crate::lifecycle::repo::require_known_timezone(pool, tz).await?;
    }
    let id = InstitutionId::new();
    let mut tx = pool.begin().await.map_err(map_db_error)?;
    let row = sqlx::query(&format!(
        "INSERT INTO institution.institution (id, tenant_id, code, name, timezone) VALUES ($1, $2, $3, $4, $5) RETURNING {COLUMNS}"
    ))
    .bind(id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(cmd.code.trim())
    .bind(cmd.name.trim())
    .bind(cmd.timezone.as_deref().unwrap_or("Asia/Kolkata"))
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let inst = from_row(&row).map_err(map_db_error)?;

    let event_scope = ExecutionScope::institution(scope.tenant_id, inst.id);
    let env = EventEnvelope::new(
        &event_scope,
        NewEvent {
            source: SOURCE,
            event_type: INSTITUTION_REGISTERED,
            dataschema: INSTITUTION_REGISTERED_SCHEMA,
            subject: Some(inst.id.to_string()),
            data: serde_json::to_value(InstitutionRegisteredV1 {
                institution_id: inst.id.as_uuid(),
                code: inst.code.clone(),
                name: inst.name.clone(),
            })
            .map_err(KernelError::internal)?,
        },
    )
    .with_correlation(actor.correlation_id.clone())
    .with_actor(actor.id.clone());
    outbox::enqueue(&mut *tx, &env)
        .await
        .map_err(map_db_error)?;
    audit::insert(
        &mut *tx,
        &AuditEntry::new(
            event_scope,
            "institution.institution.register",
            AuditOutcome::Succeeded,
        )
        .actor(ActorKind::User, actor.id.clone())
        .target("institution", inst.id.to_string())
        .correlation(actor.correlation_id.clone()),
    )
    .await
    .map_err(map_db_error)?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(inst)
}

pub async fn get(
    pool: &PgPool,
    scope: &ExecutionScope,
    id: InstitutionId,
) -> Result<Institution, KernelError> {
    let row = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM institution.institution WHERE id = $1 AND tenant_id = $2"
    ))
    .bind(id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .fetch_optional(pool)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "institution",
    })?;
    from_row(&row).map_err(map_db_error)
}

/// Institutions of the scope's tenant, optionally filtered by status, ordered by code.
pub async fn list(
    pool: &PgPool,
    scope: &ExecutionScope,
    status: Option<InstitutionStatus>,
) -> Result<Vec<Institution>, KernelError> {
    let rows = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM institution.institution WHERE tenant_id = $1 \
         AND ($2::text IS NULL OR status = $2) ORDER BY code"
    ))
    .bind(scope.tenant_id.as_uuid())
    .bind(status.map(|s| s.as_str()))
    .fetch_all(pool)
    .await
    .map_err(map_db_error)?;
    rows.iter()
        .map(|r| from_row(r).map_err(map_db_error))
        .collect()
}

pub async fn rename(
    pool: &PgPool,
    scope: &ExecutionScope,
    id: InstitutionId,
    expected: Version,
    name: &str,
    actor: &Actor,
) -> Result<Institution, KernelError> {
    let mut tx = pool.begin().await.map_err(map_db_error)?;
    let row = sqlx::query(&format!(
        "UPDATE institution.institution SET name = $1, version = version + 1, updated_at = now() \
         WHERE id = $2 AND tenant_id = $3 AND version = $4 \
           AND status NOT IN ('pending_approval', 'suspended', 'archived') RETURNING {COLUMNS}"
    ))
    .bind(name.trim())
    .bind(id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(expected.0)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let Some(row) = row else {
        // Distinguish "gone" from "changed underneath you".
        let current: Option<(i64, String)> = sqlx::query_as(
            "SELECT version, status FROM institution.institution WHERE id = $1 AND tenant_id = $2",
        )
        .bind(id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_db_error)?;
        return Err(match current {
            None => KernelError::NotFound {
                entity: "institution",
            },
            Some((actual, _)) if actual != expected.0 => KernelError::VersionConflict {
                expected: expected.0,
                actual,
            },
            // Locked during activation review; read-only while suspended or archived (L2-B01 §6).
            Some((_, status)) => KernelError::Conflict(format!(
                "institution is {status}; its identity cannot be changed now"
            )),
        });
    };
    let inst = from_row(&row).map_err(map_db_error)?;
    let event_scope = ExecutionScope::institution(scope.tenant_id, inst.id);
    let env = EventEnvelope::new(
        &event_scope,
        NewEvent {
            source: SOURCE,
            event_type: INSTITUTION_RENAMED,
            dataschema: INSTITUTION_RENAMED_SCHEMA,
            subject: Some(inst.id.to_string()),
            data: serde_json::to_value(InstitutionRenamedV1 {
                institution_id: inst.id.as_uuid(),
                name: inst.name.clone(),
                version: inst.version.0,
            })
            .map_err(KernelError::internal)?,
        },
    )
    .with_correlation(actor.correlation_id.clone())
    .with_actor(actor.id.clone());
    outbox::enqueue(&mut *tx, &env)
        .await
        .map_err(map_db_error)?;
    audit::insert(
        &mut *tx,
        &AuditEntry::new(
            event_scope,
            "institution.institution.rename",
            AuditOutcome::Succeeded,
        )
        .actor(ActorKind::User, actor.id.clone())
        .target("institution", inst.id.to_string())
        .correlation(actor.correlation_id.clone())
        .details(serde_json::json!({ "fromVersion": expected.0 })),
    )
    .await
    .map_err(map_db_error)?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(inst)
}

/// Contract implementation exposed to other modules via the composition root.
#[derive(Clone)]
pub struct PgInstitutionDirectory {
    pool: PgPool,
}

impl PgInstitutionDirectory {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl InstitutionDirectory for PgInstitutionDirectory {
    async fn exists(&self, scope: &ExecutionScope, id: InstitutionId) -> Result<bool, KernelError> {
        match get(&self.pool, scope, id).await {
            Ok(_) => Ok(true),
            Err(KernelError::NotFound { .. }) => Ok(false),
            Err(e) => Err(e),
        }
    }
}
