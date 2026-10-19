//! Institution **activation readiness** (contract doc §7).
//!
//! Readiness is *computed on read* from the owning tables — L2-B01 states activation creates no
//! authoritative readiness state, so nothing here is persisted. Each slice owns one provider that
//! returns **all of its keys every time** (status `met`, `unmet` or `not_applicable`):
//!
//! | Provider (slice-owned file) | Keys |
//! |------------------------------|------|
//! | `crate::lifecycle::readiness::facts` (2A) | `institution.profile_complete`, `institution.timezone_set`, `institution.policies_effective`, `organisation.root_unit_active`, `organisation.governance_active`, `position.definitions_active` |
//! | `crate::campus::readiness::facts` (2B) | `campus.active_campus`, `curriculum.academic_structure_published`, `curriculum.programme_published`, `curriculum.curriculum_published`, `curriculum.school_model_published` |
//! | `crate::calendar::readiness::facts` (2C) | `calendar.version_published`, `calendar.academic_year_open`, `calendar.assignment_effective` |
//!
//! [`evaluate`] (foundation-owned) concatenates them; 2C serves it over HTTP and 2A's activation
//! submission refuses with 409 when [`ReadinessReport::ready`] is false.

use crate::shared::guard;
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use sutra_contracts::institution::status::InstitutionStatus;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};

/// Outcome of one readiness check.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessStatus {
    Met,
    Unmet,
    /// Does not apply to this institution (e.g. school model for a university).
    NotApplicable,
}

/// Area a check belongs to (drives grouping in the web checklist).
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessArea {
    Institution,
    Organisation,
    Campus,
    Curriculum,
    Calendar,
}

/// One checklist line.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReadinessItem {
    /// Stable key from the table above, e.g. `campus.active_campus`.
    pub key: String,
    pub area: ReadinessArea,
    /// Short human label, e.g. "At least one active campus".
    pub label: String,
    /// Required for activation (`false` = advisory).
    pub required: bool,
    pub status: ReadinessStatus,
    /// Why it is unmet / what was found, e.g. "2 campuses registered, none active".
    pub detail: Option<String>,
    /// Web route that fixes it, e.g. `/campus/campuses`.
    pub link: Option<String>,
}

impl ReadinessItem {
    pub fn new(
        key: &str,
        area: ReadinessArea,
        label: &str,
        required: bool,
        status: ReadinessStatus,
    ) -> Self {
        Self {
            key: key.to_owned(),
            area,
            label: label.to_owned(),
            required,
            status,
            detail: None,
            link: None,
        }
    }

    pub fn detail(mut self, d: impl Into<String>) -> Self {
        self.detail = Some(d.into());
        self
    }

    pub fn link(mut self, l: &str) -> Self {
        self.link = Some(l.to_owned());
        self
    }

    /// Blocks activation.
    pub fn blocking(&self) -> bool {
        self.required && self.status == ReadinessStatus::Unmet
    }
}

/// The aggregated checklist.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReadinessReport {
    pub institution_id: InstitutionId,
    pub institution_status: InstitutionStatus,
    /// No required item is unmet.
    pub ready: bool,
    pub items: Vec<ReadinessItem>,
    pub evaluated_at: DateTime<Utc>,
}

impl ReadinessReport {
    pub fn from_items(
        institution_id: InstitutionId,
        institution_status: InstitutionStatus,
        items: Vec<ReadinessItem>,
    ) -> Self {
        let ready = !items.iter().any(ReadinessItem::blocking);
        Self {
            institution_id,
            institution_status,
            ready,
            items,
            evaluated_at: Utc::now(),
        }
    }

    /// Keys of the blocking items (for the 409 detail of an activation submission).
    pub fn blocking_keys(&self) -> Vec<&str> {
        self.items
            .iter()
            .filter(|i| i.blocking())
            .map(|i| i.key.as_str())
            .collect()
    }
}

/// Evaluates the full checklist for one institution of the scope's tenant (404 when missing).
pub async fn evaluate(
    pool: &PgPool,
    scope: &ExecutionScope,
    institution_id: InstitutionId,
) -> Result<ReadinessReport, KernelError> {
    let status = guard::institution_status(pool, scope, institution_id).await?;
    let mut items = crate::lifecycle::readiness::facts(pool, scope, institution_id).await?;
    items.extend(crate::campus::readiness::facts(pool, scope, institution_id).await?);
    items.extend(crate::calendar::readiness::facts(pool, scope, institution_id).await?);
    Ok(ReadinessReport::from_items(institution_id, status, items))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_required_unmet_items_block() {
        let items = vec![
            ReadinessItem::new("a", ReadinessArea::Campus, "A", true, ReadinessStatus::Met),
            ReadinessItem::new(
                "b",
                ReadinessArea::Calendar,
                "B",
                false,
                ReadinessStatus::Unmet,
            ),
            ReadinessItem::new(
                "c",
                ReadinessArea::Curriculum,
                "C",
                true,
                ReadinessStatus::NotApplicable,
            ),
        ];
        let r = ReadinessReport::from_items(
            InstitutionId::new(),
            InstitutionStatus::Draft,
            items.clone(),
        );
        assert!(r.ready);
        let mut blocked = items;
        blocked.push(ReadinessItem::new(
            "d",
            ReadinessArea::Institution,
            "D",
            true,
            ReadinessStatus::Unmet,
        ));
        let r =
            ReadinessReport::from_items(InstitutionId::new(), InstitutionStatus::Draft, blocked);
        assert!(!r.ready);
        assert_eq!(r.blocking_keys(), vec!["d"]);
        assert_eq!(
            serde_json::to_value(ReadinessStatus::NotApplicable).unwrap_or_default(),
            "not_applicable"
        );
    }
}
