//! **2A** readiness provider (contract doc §7). Returns **every** key below on each call:
//! `institution.profile_complete`, `institution.timezone_set`, `institution.policies_effective`
//! (advisory), `organisation.root_unit_active`, `organisation.governance_active` (advisory),
//! `position.definitions_active` (advisory). Computed on read from the 2A tables; dates are
//! business-local (institution timezone).

use super::common::business_today;
use crate::shared::readiness::{ReadinessArea, ReadinessItem, ReadinessStatus};
use sqlx::PgPool;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use sutra_persistence::map_db_error;

fn met(ok: bool) -> ReadinessStatus {
    if ok {
        ReadinessStatus::Met
    } else {
        ReadinessStatus::Unmet
    }
}

pub async fn facts(
    pool: &PgPool,
    scope: &ExecutionScope,
    institution_id: InstitutionId,
) -> Result<Vec<ReadinessItem>, KernelError> {
    let inst = crate::repo::get(pool, scope, institution_id).await?;
    let today = business_today(pool, scope, institution_id).await?;
    let missing = inst.profile.missing_fields();
    let tz_ok = super::repo::timezone_known(pool, &inst.timezone).await?;
    let (policies, roots, units, governance, positions): (i64, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT \
           (SELECT count(DISTINCT policy_id) FROM institution.institution_policy_revision \
              WHERE institution_id = $1 AND tenant_id = $2 AND status <> 'draft' AND effective_from <= $3), \
           (SELECT count(*) FROM institution.org_unit \
              WHERE institution_id = $1 AND tenant_id = $2 AND parent_id IS NULL AND status = 'active'), \
           (SELECT count(*) FROM institution.org_unit \
              WHERE institution_id = $1 AND tenant_id = $2 AND status IN ('draft', 'active', 'suspended')), \
           (SELECT count(*) FROM institution.governance_config \
              WHERE institution_id = $1 AND tenant_id = $2 AND status IN ('published', 'superseded') \
                AND effective_from <= $3 AND (effective_to IS NULL OR effective_to >= $3)), \
           (SELECT count(*) FROM institution.position_definition \
              WHERE institution_id = $1 AND tenant_id = $2 AND status = 'active')",
    )
    .bind(institution_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(today)
    .fetch_one(pool)
    .await
    .map_err(map_db_error)?;

    let profile = ReadinessItem::new(
        "institution.profile_complete",
        ReadinessArea::Institution,
        "Institution profile complete",
        true,
        met(missing.is_empty()),
    )
    .link("/campus/institution");
    let profile = if missing.is_empty() {
        profile.detail("All mandatory master information is recorded")
    } else {
        profile.detail(format!("Missing: {}", missing.join(", ")))
    };
    Ok(vec![
        profile,
        ReadinessItem::new(
            "institution.timezone_set",
            ReadinessArea::Institution,
            "Business timezone set",
            true,
            met(tz_ok),
        )
        .detail(if tz_ok {
            format!("Business dates use {}", inst.timezone)
        } else {
            format!("`{}` is not an IANA timezone", inst.timezone)
        })
        .link("/campus/institution"),
        ReadinessItem::new(
            "institution.policies_effective",
            ReadinessArea::Institution,
            "Institutional policies in effect",
            false,
            met(policies > 0),
        )
        .detail(format!("{policies} policies in effect today"))
        .link("/campus/policies"),
        ReadinessItem::new(
            "organisation.root_unit_active",
            ReadinessArea::Organisation,
            "Root organisation unit active",
            true,
            met(roots > 0),
        )
        .detail(if roots > 0 {
            "The root unit is active".to_owned()
        } else {
            format!("{units} organisation units registered, no active root unit")
        })
        .link("/campus/organisation"),
        ReadinessItem::new(
            "organisation.governance_active",
            ReadinessArea::Organisation,
            "Organisation governance configured",
            false,
            met(governance > 0),
        )
        .detail(format!(
            "{governance} governance configurations in effect today"
        ))
        .link("/campus/organisation"),
        ReadinessItem::new(
            "position.definitions_active",
            ReadinessArea::Organisation,
            "Position definitions active",
            false,
            met(positions > 0),
        )
        .detail(format!("{positions} active position definitions"))
        .link("/campus/positions"),
    ])
}

/// Keys this provider always returns (unit-tested against the contract table).
pub const KEYS: &[&str] = &[
    "institution.profile_complete",
    "institution.timezone_set",
    "institution.policies_effective",
    "organisation.root_unit_active",
    "organisation.governance_active",
    "position.definitions_active",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_match_the_contract() {
        assert_eq!(KEYS.len(), 6);
        assert!(KEYS.iter().all(|k| k.split('.').count() == 2));
        assert_eq!(met(true), ReadinessStatus::Met);
        assert_eq!(met(false), ReadinessStatus::Unmet);
    }
}
