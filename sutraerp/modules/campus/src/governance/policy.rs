//! Effective governance policy (G-014): the defaults overlaid with every effective configuration
//! version of the institution.

use super::domain::{apply_config, ConfigKey, GovernancePolicy};
use sqlx::{PgPool, Row};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;

/// The policy the closure rules use right now.
pub async fn effective_policy(
    pool: &PgPool,
    tenant: TenantId,
    institution: InstitutionId,
) -> Result<GovernancePolicy, KernelError> {
    let rows = sqlx::query(
        "SELECT key, settings FROM campus.governance_config_version WHERE tenant_id = $1 \
         AND institution_id = $2 AND status = 'effective'",
    )
    .bind(tenant.as_uuid())
    .bind(institution.as_uuid())
    .fetch_all(pool)
    .await
    .map_err(map_db_error)?;
    let mut p = GovernancePolicy::default();
    for r in &rows {
        let key: String = r.try_get("key").map_err(map_db_error)?;
        let settings: serde_json::Value = r.try_get("settings").map_err(map_db_error)?;
        if let Some(k) = ConfigKey::parse(&key) {
            apply_config(&mut p, k, &settings);
        }
    }
    Ok(p)
}
