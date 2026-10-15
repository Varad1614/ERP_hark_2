//! INT (Batch 9 integration): bridge from the `metrics` 0.24 facade (used by 9C in
//! `sutra-platform`: `sutra_workflow_calls_total`, `sutra_search_projection_lag_seconds`) into the
//! single prometheus-client registry of 9A ([`crate::metrics`]), so one `/metrics` listener
//! exports every contract family (docs/batch9-contracts.md §3.3).
//!
//! The recorder maps the two known 9C keys onto registered families; any other facade metric is
//! dropped (a no-op handle), so a stray `metrics::*!` call can never add an unreviewed family or
//! a high-cardinality label.

use crate::metrics::{global, ProjectionLabels, WorkflowLabels};
use metrics::{
    Counter, CounterFn, Gauge, GaugeFn, Histogram, Key, KeyName, Metadata, Recorder, SharedString,
    Unit,
};
use std::sync::Arc;

/// Operations the Flowable adapter records (`op` label), primed at zero.
pub const WORKFLOW_OPS: [&str; 5] = [
    "deploy",
    "start_process",
    "complete_task",
    "decide",
    "signal",
];

const WORKFLOW_CALLS: &str = "sutra_workflow_calls_total";
const SEARCH_LAG: &str = "sutra_search_projection_lag_seconds";

fn label<'a>(key: &'a Key, name: &str) -> Option<&'a str> {
    key.labels().find(|l| l.key() == name).map(|l| l.value())
}

struct PromCounter(prometheus_client::metrics::counter::Counter);

impl CounterFn for PromCounter {
    fn increment(&self, value: u64) {
        self.0.inc_by(value);
    }

    fn absolute(&self, value: u64) {
        let current = self.0.get();
        if value > current {
            self.0.inc_by(value - current);
        }
    }
}

struct PromGauge(crate::metrics::F64Gauge);

impl GaugeFn for PromGauge {
    fn increment(&self, value: f64) {
        self.0.inc_by(value);
    }

    fn decrement(&self, value: f64) {
        self.0.dec_by(value);
    }

    fn set(&self, value: f64) {
        self.0.set(value);
    }
}

/// `metrics::Recorder` writing into the 9A registry.
#[derive(Debug, Default, Clone, Copy)]
pub struct PrometheusBridge;

impl Recorder for PrometheusBridge {
    fn describe_counter(&self, _key: KeyName, _unit: Option<Unit>, _description: SharedString) {}

    fn describe_gauge(&self, _key: KeyName, _unit: Option<Unit>, _description: SharedString) {}

    fn describe_histogram(&self, _key: KeyName, _unit: Option<Unit>, _description: SharedString) {}

    fn register_counter(&self, key: &Key, _metadata: &Metadata<'_>) -> Counter {
        if key.name() != WORKFLOW_CALLS {
            return Counter::noop();
        }
        let labels = WorkflowLabels {
            op: label(key, "op").unwrap_or("unknown").to_owned(),
            outcome: label(key, "outcome").unwrap_or("unknown").to_owned(),
        };
        let c = global().workflow_calls.get_or_create(&labels).clone();
        Counter::from_arc(Arc::new(PromCounter(c)))
    }

    fn register_gauge(&self, key: &Key, _metadata: &Metadata<'_>) -> Gauge {
        if key.name() != SEARCH_LAG {
            return Gauge::noop();
        }
        let labels = ProjectionLabels {
            projection: label(key, "projection").unwrap_or("unknown").to_owned(),
        };
        let g = global().search_lag.get_or_create(&labels).clone();
        Gauge::from_arc(Arc::new(PromGauge(g)))
    }

    fn register_histogram(&self, _key: &Key, _metadata: &Metadata<'_>) -> Histogram {
        Histogram::noop()
    }
}

/// Installs [`PrometheusBridge`] as the global `metrics` recorder (idempotent: a second call,
/// e.g. from a second app built in the same test process, is ignored).
pub fn install() {
    let _ = metrics::set_global_recorder(PrometheusBridge);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn facade_calls_land_in_the_prometheus_registry() {
        let bridge = PrometheusBridge;
        metrics::with_local_recorder(&bridge, || {
            metrics::counter!(WORKFLOW_CALLS, "op" => "start_process", "outcome" => "ok")
                .increment(3);
            metrics::gauge!(SEARCH_LAG, "projection" => "students").set(1.5);
            // Unknown facade metrics are dropped, never exported.
            metrics::counter!("sutra_unreviewed_total").increment(1);
        });
        let text = global().render();
        assert!(
            text.contains("sutra_workflow_calls_total{op=\"start_process\",outcome=\"ok\"} 3"),
            "{text}"
        );
        assert!(
            text.contains("sutra_search_projection_lag_seconds{projection=\"students\"} 1.5"),
            "{text}"
        );
        assert!(!text.contains("sutra_unreviewed"));
        // Both families are exported before any call (primed series).
        assert!(text.contains("# HELP sutra_workflow_calls "));
        assert!(text.contains("# HELP sutra_search_projection_lag_seconds "));
    }
}
