//! PostgreSQL repository for the reference aggregate. Only this module touches the
//! `institution` schema (ADR-L5-0036 §3.2).

use crate::domain::{Institution, RegisterInstitution};
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use sutra_contracts::institution::events::{
    InstitutionRegisteredV1, InstitutionRenamedV1, INSTITUTION_REGISTERED,
    INSTITUTION_REGISTERED_SCHEMA, INSTITUTION_RENAMED, INSTITUTION_RENAMED_SCHEMA,
};
use sutra_contracts::institution::InstitutionDirectory;
use sutra_kernel::audit::{ActorKind, AuditEntry, AuditOutcome};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::events::{EventEnvelope, NewEvent};
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::{audit, map_db_error, outbox};

use sutra_contracts::institution::events::SOURCE;
const COLUMNS: &str = "id, tenant_id, code, name, timezone, version, created_at, updated_at";

fn from_row(r: &sqlx::postgres::PgRow) -> Result<Institution, sqlx::Error> {
    Ok(Institution {
        id: InstitutionId(r.try_get("id")?),
        tenant_id: TenantId(r.try_get("tenant_id")?),
        code: r.try_get("code")?,
        name: r.try_get("name")?,
        timezone: r.try_get("timezone")?,
        version: Version(r.try_get("version")?),
        created_at: r.try_get::<DateTime<Utc>, _>("created_at")?,
        updated_at: r.try_get::<DateTime<Utc>, _>("updated_at")?,
    })
}

/// Context of the caller performing a mutation.
#[derive(Debug, Clone, Default)]
pub struct Actor {
    pub id: Option<String>,
    pub correlation_id: Option<String>,
}

pub async fn register(
    pool: &PgPool,
    scope: &ExecutionScope,
    cmd: RegisterInstitution,
    actor: &Actor,
) -> Result<Institution, KernelError> {
    cmd.validate()?;
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
         WHERE id = $2 AND tenant_id = $3 AND version = $4 RETURNING {COLUMNS}"
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
        let current: Option<i64> = sqlx::query_scalar(
            "SELECT version FROM institution.institution WHERE id = $1 AND tenant_id = $2",
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
            Some(actual) => KernelError::VersionConflict {
                expected: expected.0,
                actual,
            },
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
