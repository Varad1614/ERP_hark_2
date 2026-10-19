//! Dashboard and widget state machines (L2-H07; contract §4.2) and the pure rules: widget
//! bindings are validated against the live metric catalogue, `finance` dashboards (F-076) bind
//! only canonical Finance figures (`finance.*`), and a rendered widget shows the figure of its
//! metric key (decimal string, never floats).

use sutra_contracts::services::ports::MetricValue;

/// Declares a Domain H slice enum with `ALL`, `as_str`, `parse` and `snake_case` serde / OpenAPI names.
macro_rules! services_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident => $wire:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(
            Clone, Copy, Debug, PartialEq, Eq, Hash,
            serde::Serialize, serde::Deserialize, utoipa::ToSchema,
        )]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($variant),+ }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            /// Stored / wire name.
            pub fn as_str(&self) -> &'static str {
                match self { $($name::$variant => $wire),+ }
            }

            /// Strict parse of the stored name.
            pub fn parse(s: &str) -> Option<Self> {
                Self::ALL.iter().copied().find(|v| v.as_str() == s)
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}
pub(crate) use services_enum;

services_enum!(
    /// Governed dashboard (L3-H-048).
    DashboardStatus { Draft => "draft", InReview => "in_review", Published => "published", Retired => "retired" }
);
services_enum!(
    /// One immutable version of a dashboard's layout (L3-H-052).
    DashboardVersionStatus {
        Draft => "draft", InReview => "in_review", Approved => "approved",
        Published => "published", Superseded => "superseded", Rejected => "rejected",
    }
);
services_enum!(
    /// What a dashboard is for; `finance` binds only `finance.*` figures (F-076).
    DashboardCategory {
        Operational => "operational", Finance => "finance", Academic => "academic",
        Campus => "campus", People => "people", Regulatory => "regulatory",
    }
);
services_enum!(
    /// Widget definition (L3-H-054).
    WidgetStatus { Draft => "draft", Active => "active", Retired => "retired" }
);
services_enum!(
    /// How a widget presents its figure.
    WidgetVisualization { Stat => "stat", Gauge => "gauge", Table => "table" }
);
services_enum!(
    /// Dashboard assignment to a role (L3-H-050).
    DashboardAssignmentStatus { Active => "active", Revoked => "revoked" }
);

impl DashboardVersionStatus {
    /// A version still being prepared (at most one per dashboard).
    pub fn is_open(self) -> bool {
        matches!(self, Self::Draft | Self::InReview | Self::Approved)
    }

    pub fn can_transition(self, to: Self) -> bool {
        use DashboardVersionStatus::*;
        matches!(
            (self, to),
            (Draft, InReview)
                | (InReview, Approved)
                | (InReview, Rejected)
                | (Approved, Published)
                | (Published, Superseded)
                | (Superseded, Published)
        )
    }
}

impl WidgetStatus {
    pub fn can_transition(self, to: Self) -> bool {
        matches!(
            (self, to),
            (Self::Draft, Self::Active)
                | (Self::Active, Self::Retired)
                | (Self::Draft, Self::Retired)
        )
    }
}

/// Why a metric key cannot be bound (`None` = fine): unknown in the live catalogue, or outside
/// Finance for a `finance` dashboard.
pub fn binding_problem(
    key: &str,
    category: Option<DashboardCategory>,
    catalogue: &[MetricValue],
) -> Option<String> {
    if !catalogue.iter().any(|m| m.key == key) {
        return Some(format!("unknown metric `{key}`"));
    }
    if category == Some(DashboardCategory::Finance) && !key.starts_with("finance.") {
        return Some(format!(
            "`{key}` is not a canonical Finance figure; finance dashboards bind only finance.* metrics"
        ));
    }
    None
}

/// The figure of a metric key, if the catalogue has it.
pub fn figure<'a>(key: &str, catalogue: &'a [MetricValue]) -> Option<&'a MetricValue> {
    catalogue.iter().find(|m| m.key == key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;

    #[test]
    fn version_machine() {
        use DashboardVersionStatus as V;
        assert!(V::Draft.can_transition(V::InReview));
        assert!(V::Approved.can_transition(V::Published));
        assert!(V::Superseded.can_transition(V::Published), "rollback");
        assert!(!V::Draft.can_transition(V::Published), "review first");
        assert!(V::Approved.is_open() && !V::Published.is_open());
        assert!(WidgetStatus::Active.can_transition(WidgetStatus::Retired));
        assert!(!WidgetStatus::Retired.can_transition(WidgetStatus::Active));
    }

    #[test]
    fn bindings_follow_the_catalogue_and_finance_rule() {
        let cat = vec![
            MetricValue::new(
                "finance",
                "receivables_outstanding",
                "R",
                Decimal::ONE,
                "inr",
            ),
            MetricValue::count("campus", "active_spaces", "S", 2),
        ];
        assert!(binding_problem("campus.active_spaces", None, &cat).is_none());
        assert!(binding_problem("campus.nope", None, &cat).is_some());
        assert!(binding_problem(
            "campus.active_spaces",
            Some(DashboardCategory::Finance),
            &cat
        )
        .is_some());
        assert!(binding_problem(
            "finance.receivables_outstanding",
            Some(DashboardCategory::Finance),
            &cat
        )
        .is_none());
        assert_eq!(
            figure("campus.active_spaces", &cat).map(|m| m.value),
            Some(Decimal::from(2))
        );
    }
}
