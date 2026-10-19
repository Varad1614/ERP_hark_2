//! [`OperationalMetricsPort`] — the read port Domain H (dashboards, regulatory snapshots) uses to
//! obtain figures from every other domain without reading their schemas.
//!
//! Each owning domain implements it over its **own** schema (foundation adapters:
//! `sutra_student::PgStudentMetrics`, `sutra_people::PgPeopleMetrics`,
//! `sutra_finance::PgFinanceMetrics`, `sutra_campus::PgCampusMetrics`,
//! `sutra_resources::PgResourceMetrics`); the composition root hands the list to
//! `sutra_services::ServicesDeps::metrics`. Frozen shape: additive changes only. Values are
//! `rust_decimal` (counts are whole numbers, money two places) — never floats.

use rust_decimal::Decimal;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};

/// One figure of one domain, as of the call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetricValue {
    /// Owning domain key (`student`, `people`, `finance`, `campus`, `resources`).
    pub domain: String,
    /// Stable dotted key (`student.active_students`, `finance.receivables_outstanding` …):
    /// `^[a-z][a-z0-9_]*\.[a-z][a-z0-9_.]*$`, prefixed with the domain key.
    pub key: String,
    /// Human label (English).
    pub label: String,
    pub value: Decimal,
    /// `count`, `inr`, `percent`, `days`.
    pub unit: String,
}

impl MetricValue {
    pub fn new(domain: &str, key: &str, label: &str, value: Decimal, unit: &str) -> Self {
        Self {
            domain: domain.to_owned(),
            key: format!("{domain}.{key}"),
            label: label.to_owned(),
            value,
            unit: unit.to_owned(),
        }
    }

    /// A whole-number count (`unit = count`).
    pub fn count(domain: &str, key: &str, label: &str, n: i64) -> Self {
        Self::new(domain, key, label, Decimal::from(n), "count")
    }
}

/// Read port over the operational figures of one domain (tenant from `scope`).
#[async_trait::async_trait]
pub trait OperationalMetricsPort: Send + Sync + 'static {
    /// Owning domain key (`student`, `people`, …).
    fn domain(&self) -> &'static str;

    /// Every figure of this domain for one institution (unknown institution → zeros).
    async fn metrics(
        &self,
        scope: &ExecutionScope,
        institution_id: InstitutionId,
    ) -> Result<Vec<MetricValue>, KernelError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_prefixed_with_the_domain() {
        let m = MetricValue::new(
            "campus",
            "active_spaces",
            "Active spaces",
            Decimal::ONE,
            "count",
        );
        assert_eq!(m.key, "campus.active_spaces");
        assert_eq!(m.domain, "campus");
        let c = MetricValue::count("student", "active_students", "Active students", 3);
        assert_eq!((c.value, c.unit.as_str()), (Decimal::from(3), "count"));
    }
}
