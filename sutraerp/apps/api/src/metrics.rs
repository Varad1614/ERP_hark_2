//! Prometheus metrics (ADR-L5-0029, docs/batch9-contracts.md §3.3) — **owned by slice 9A**.
//!
//! * One process-wide registry ([`global`]) with the contract names (prefix `sutra_`).
//! * [`track_http`]: axum middleware for the HTTP RED metrics. Labels are the method, the
//!   **matched route template** (`unmatched` for the fallback) and the status class — never ids,
//!   tenants or raw paths. It also records `http.response.status_code` on the request span and
//!   writes one `request completed` log line inside it (health probes excepted).
//! * [`EventMetricsLayer`]: a `tracing` layer that turns the existing scheduler / outbox-relay
//!   events (`sutra::metrics` target, `sutra_persistence::jobs` skip / failure events) and the
//!   audit-chain verification failures into counters, so the shared crates need no metrics
//!   dependency.
//! * [`collect`]: periodic gauges (database pool, outbox backlog, unsealed audit rows).
//! * [`serve`]: the separate `/metrics` listener on `SUTRA_METRICS_ADDR` (never the public port).

use axum::extract::{MatchedPath, Request};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use prometheus_client::encoding::text::encode;
use prometheus_client::encoding::EncodeLabelSet;
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::gauge::Gauge;
use prometheus_client::metrics::histogram::Histogram;
use prometheus_client::registry::Registry;
use sqlx::PgPool;
use std::net::SocketAddr;
use std::sync::LazyLock;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

/// Histogram buckets of `sutra_http_request_duration_seconds` (5 ms … 10 s).
pub const DURATION_BUCKETS: [f64; 12] = [
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 7.5, 10.0,
];

/// OpenMetrics text exposition content type.
pub const CONTENT_TYPE: &str = "application/openmetrics-text; version=1.0.0; charset=utf-8";

/// Every metric family the API exports (contract §3.3; the 9C families are registered by 9C).
pub const METRIC_NAMES: [&str; 14] = [
    "sutra_http_requests_total",
    "sutra_http_request_duration_seconds",
    "sutra_http_requests_in_flight",
    "sutra_db_pool_connections",
    "sutra_outbox_pending",
    "sutra_outbox_published_total",
    "sutra_outbox_failed_total",
    "sutra_job_runs_total",
    "sutra_audit_unsealed",
    "sutra_audit_chain_verify_failures_total",
    "sutra_build_info",
    // Process start (seconds since the epoch), for restarts on the dashboards.
    "sutra_process_start_time_seconds",
    // INT: 9C families bridged from the `metrics` facade (crate::metrics_bridge).
    "sutra_workflow_calls_total",
    "sutra_search_projection_lag_seconds",
];

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct HttpLabels {
    pub method: String,
    pub route: String,
    pub status: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct RouteLabels {
    pub method: String,
    pub route: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct StateLabels {
    pub state: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct JobLabels {
    pub job: String,
    pub outcome: String,
}

/// INT: `sutra_workflow_calls_total{op, outcome}` (9C, fed through [`crate::metrics_bridge`]).
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct WorkflowLabels {
    pub op: String,
    pub outcome: String,
}

/// INT: `sutra_search_projection_lag_seconds{projection}` (9C, fed through [`crate::metrics_bridge`]).
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct ProjectionLabels {
    pub projection: String,
}

/// INT: floating-point gauge (seconds).
pub type F64Gauge = Gauge<f64, std::sync::atomic::AtomicU64>;

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct BuildLabels {
    pub version: String,
    pub git_sha: String,
}

fn duration_histogram() -> Histogram {
    Histogram::new(DURATION_BUCKETS)
}

type HistogramFamily<L> = Family<L, Histogram, fn() -> Histogram>;

/// The API's metric families.
pub struct Metrics {
    registry: Registry,
    pub http_requests: Family<HttpLabels, Counter>,
    pub http_duration: HistogramFamily<RouteLabels>,
    pub http_in_flight: Gauge,
    pub db_pool: Family<StateLabels, Gauge>,
    pub outbox_pending: Gauge,
    pub outbox_published: Counter,
    pub outbox_failed: Counter,
    pub job_runs: Family<JobLabels, Counter>,
    pub audit_unsealed: Gauge,
    pub audit_verify_failures: Counter,
    pub build_info: Family<BuildLabels, Gauge>,
    pub process_start: Gauge,
    /// INT: 9C families recorded through the `metrics` facade bridge.
    pub workflow_calls: Family<WorkflowLabels, Counter>,
    pub search_lag: Family<ProjectionLabels, F64Gauge>,
}

impl Metrics {
    fn new() -> Self {
        let mut registry = Registry::default();
        let http_requests = Family::<HttpLabels, Counter>::default();
        registry.register(
            "sutra_http_requests",
            "HTTP requests by method, matched route and status class",
            http_requests.clone(),
        );
        let ctor: fn() -> Histogram = duration_histogram;
        let http_duration: HistogramFamily<RouteLabels> = Family::new_with_constructor(ctor);
        registry.register(
            "sutra_http_request_duration_seconds",
            "HTTP request latency in seconds by method and matched route",
            http_duration.clone(),
        );
        let http_in_flight = Gauge::default();
        registry.register(
            "sutra_http_requests_in_flight",
            "HTTP requests currently being served",
            http_in_flight.clone(),
        );
        let db_pool = Family::<StateLabels, Gauge>::default();
        registry.register(
            "sutra_db_pool_connections",
            "PostgreSQL pool connections by state (idle, used, max)",
            db_pool.clone(),
        );
        let outbox_pending = Gauge::default();
        registry.register(
            "sutra_outbox_pending",
            "Outbox rows not yet relayed (neither published nor dead-lettered)",
            outbox_pending.clone(),
        );
        let outbox_published = Counter::default();
        registry.register(
            "sutra_outbox_published",
            "Outbox events published by this process",
            outbox_published.clone(),
        );
        let outbox_failed = Counter::default();
        registry.register(
            "sutra_outbox_failed",
            "Outbox publish attempts that failed (retried or dead-lettered)",
            outbox_failed.clone(),
        );
        let job_runs = Family::<JobLabels, Counter>::default();
        registry.register(
            "sutra_job_runs",
            "Scheduled job runs by job and outcome (ok, error, skipped)",
            job_runs.clone(),
        );
        let audit_unsealed = Gauge::default();
        registry.register(
            "sutra_audit_unsealed",
            "Audit rows without an audit chain entry",
            audit_unsealed.clone(),
        );
        let audit_verify_failures = Counter::default();
        registry.register(
            "sutra_audit_chain_verify_failures",
            "Audit chain verifications that found a broken chain",
            audit_verify_failures.clone(),
        );
        let build_info = Family::<BuildLabels, Gauge>::default();
        registry.register(
            "sutra_build_info",
            "Build of the running API (value is always 1)",
            build_info.clone(),
        );
        let process_start = Gauge::default();
        registry.register(
            "sutra_process_start_time_seconds",
            "Start time of the process since the Unix epoch in seconds",
            process_start.clone(),
        );
        // INT: the 9C families (`metrics` facade → crate::metrics_bridge → this registry).
        let workflow_calls = Family::<WorkflowLabels, Counter>::default();
        registry.register(
            "sutra_workflow_calls",
            "Workflow engine (Flowable) calls by operation and outcome",
            workflow_calls.clone(),
        );
        for op in crate::metrics_bridge::WORKFLOW_OPS {
            for outcome in ["ok", "error"] {
                workflow_calls
                    .get_or_create(&WorkflowLabels {
                        op: op.to_owned(),
                        outcome: outcome.to_owned(),
                    })
                    .inc_by(0);
            }
        }
        let search_lag = Family::<ProjectionLabels, F64Gauge>::default();
        registry.register(
            "sutra_search_projection_lag_seconds",
            "Age in seconds of the oldest outbox event not yet projected, by projection",
            search_lag.clone(),
        );
        for projection in sutra_contracts::platform::search::PROJECTIONS {
            search_lag
                .get_or_create(&ProjectionLabels {
                    projection: (*projection).to_owned(),
                })
                .set(0.0);
        }
        let started = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| i64::try_from(d.as_secs()).unwrap_or(0))
            .unwrap_or(0);
        process_start.set(started);
        // Export the pool family before the first collector pass (OpenMetrics omits empty families).
        for state in ["idle", "used", "max"] {
            db_pool
                .get_or_create(&StateLabels {
                    state: state.to_owned(),
                })
                .set(0);
        }
        Self {
            registry,
            http_requests,
            http_duration,
            http_in_flight,
            db_pool,
            outbox_pending,
            outbox_published,
            outbox_failed,
            job_runs,
            audit_unsealed,
            audit_verify_failures,
            build_info,
            process_start,
            workflow_calls,
            search_lag,
        }
    }

    /// OpenMetrics text of every family.
    pub fn render(&self) -> String {
        let mut out = String::new();
        if encode(&mut out, &self.registry).is_err() {
            out.clear();
        }
        out
    }
}

static METRICS: LazyLock<Metrics> = LazyLock::new(Metrics::new);

/// The process-wide registry.
pub fn global() -> &'static Metrics {
    &METRICS
}

/// Sets `sutra_build_info{version, git_sha} 1` (git sha from `SUTRA_GIT_SHA` via the config).
pub fn set_build_info(git_sha: &str) {
    global()
        .build_info
        .get_or_create(&BuildLabels {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            git_sha: if git_sha.is_empty() {
                "unknown".to_owned()
            } else {
                git_sha.to_owned()
            },
        })
        .set(1);
}

/// Creates the zero-valued `sutra_job_runs_total{job, outcome}` series of a scheduled job, so the
/// family is exported (OpenMetrics omits families without series) before the first run.
pub fn prime_job(job: &str) {
    for outcome in ["ok", "error", "skipped"] {
        global()
            .job_runs
            .get_or_create(&JobLabels {
                job: job.to_owned(),
                outcome: outcome.to_owned(),
            })
            .inc_by(0);
    }
}

/// `2xx` … `5xx` (`1xx` for informational).
pub fn status_class(status: u16) -> &'static str {
    match status {
        100..=199 => "1xx",
        200..=299 => "2xx",
        300..=399 => "3xx",
        400..=499 => "4xx",
        _ => "5xx",
    }
}

/// Decrements the in-flight gauge even when the request future is dropped.
struct InFlight;

impl InFlight {
    fn start() -> Self {
        global().http_in_flight.inc();
        Self
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        global().http_in_flight.dec();
    }
}

/// Records one finished request (also used by tests).
pub fn record_request(method: &str, route: &str, status: u16, elapsed: Duration) {
    let m = global();
    m.http_requests
        .get_or_create(&HttpLabels {
            method: method.to_owned(),
            route: route.to_owned(),
            status: status_class(status).to_owned(),
        })
        .inc();
    m.http_duration
        .get_or_create(&RouteLabels {
            method: method.to_owned(),
            route: route.to_owned(),
        })
        .observe(elapsed.as_secs_f64());
}

/// HTTP RED middleware (inside the trace layer, so it runs within the request span).
pub async fn track_http(req: Request, next: Next) -> Response {
    let method = crate::telemetry::method_name(req.method());
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map_or_else(|| "unmatched".to_owned(), |p| p.as_str().to_owned());
    let guard = InFlight::start();
    let started = Instant::now();
    let res = next.run(req).await;
    let elapsed = started.elapsed();
    drop(guard);
    let status = res.status().as_u16();
    record_request(method, &route, status, elapsed);
    let span = tracing::Span::current();
    span.record("http.response.status_code", status);
    if status >= 500 {
        span.record("otel.status_code", "ERROR");
    }
    if !route.starts_with("/health") {
        tracing::info!(
            target: "sutra::http",
            status = status,
            duration_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
            "request completed"
        );
    }
    res
}

/// Counters fed by events: scheduler job outcomes, outbox relay passes, audit verify failures.
#[derive(Clone, Copy, Debug, Default)]
pub struct EventMetricsLayer;

impl EventMetricsLayer {
    /// Only the events this layer reads (debug for the scheduler's skip events).
    pub fn filter() -> Targets {
        Targets::new()
            .with_target("sutra::metrics", LevelFilter::INFO)
            .with_target("sutra_persistence::jobs", LevelFilter::DEBUG)
    }
}

#[derive(Default)]
struct EventFields {
    message: String,
    job: Option<String>,
    relay_published: u64,
    relay_retried: u64,
    relay_dead_lettered: u64,
    audit_chain_verify_failed: bool,
}

impl Visit for EventFields {
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "message" => self.message = value.to_owned(),
            "job" => self.job = Some(value.to_owned()),
            _ => {}
        }
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        match field.name() {
            "relay_published" => self.relay_published = value,
            "relay_retried" => self.relay_retried = value,
            "relay_dead_lettered" => self.relay_dead_lettered = value,
            _ => {}
        }
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.record_u64(field, u64::try_from(value).unwrap_or(0));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        if field.name() == "audit_chain_verify_failed" {
            self.audit_chain_verify_failed = value;
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        match field.name() {
            "message" => self.message = format!("{value:?}"),
            "job" => self.job = Some(format!("{value:?}").trim_matches('"').to_owned()),
            _ => {}
        }
    }
}

/// Applies one event to the counters.
fn apply(target: &str, f: &EventFields) {
    let m = global();
    let job_outcome = |outcome: &str| {
        if let Some(job) = &f.job {
            m.job_runs
                .get_or_create(&JobLabels {
                    job: job.clone(),
                    outcome: outcome.to_owned(),
                })
                .inc();
        }
    };
    match (target, f.message.as_str()) {
        ("sutra::metrics", "sweeper run") => job_outcome("ok"),
        ("sutra::metrics", "outbox relay pass") => {
            m.outbox_published.inc_by(f.relay_published);
            m.outbox_failed
                .inc_by(f.relay_retried.saturating_add(f.relay_dead_lettered));
        }
        ("sutra_persistence::jobs", msg) if msg.starts_with("sweeper skipped") => {
            job_outcome("skipped")
        }
        ("sutra_persistence::jobs", msg) if msg.starts_with("sweeper run failed") => {
            job_outcome("error")
        }
        _ => {}
    }
    if target == "sutra::metrics" && f.audit_chain_verify_failed {
        m.audit_verify_failures.inc();
    }
}

impl<S: Subscriber> Layer<S> for EventMetricsLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut fields = EventFields::default();
        event.record(&mut fields);
        apply(event.metadata().target(), &fields);
    }
}

/// Database / outbox / audit gauges, refreshed every `interval` until `cancel`.
pub async fn collect(pool: PgPool, interval: Duration, cancel: CancellationToken) {
    let m = global();
    loop {
        let max = pool.options().get_max_connections();
        let size = pool.size();
        let idle = u32::try_from(pool.num_idle()).unwrap_or(u32::MAX);
        for (state, v) in [
            ("idle", idle),
            ("used", size.saturating_sub(idle)),
            ("max", max),
        ] {
            m.db_pool
                .get_or_create(&StateLabels {
                    state: state.to_owned(),
                })
                .set(i64::from(v));
        }
        match sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM platform.outbox \
             WHERE published_at IS NULL AND dead_lettered_at IS NULL",
        )
        .fetch_one(&pool)
        .await
        {
            Ok(n) => {
                m.outbox_pending.set(n);
            }
            Err(e) => tracing::debug!(error = %e, "outbox backlog gauge not refreshed"),
        }
        match sutra_persistence::audit_chain::unsealed_total_estimate(&pool).await {
            Ok(n) => {
                m.audit_unsealed.set(n);
            }
            Err(e) => tracing::debug!(error = %e, "unsealed audit gauge not refreshed"),
        }
        tokio::select! {
            () = cancel.cancelled() => break,
            () = tokio::time::sleep(interval) => {}
        }
    }
}

async fn metrics_handler() -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, CONTENT_TYPE)],
        global().render(),
    )
        .into_response()
}

/// Router of the metrics listener (`GET /metrics` only).
pub fn router() -> Router {
    Router::new().route("/metrics", get(metrics_handler))
}

/// Serves `/metrics` on `addr` until `cancel`.
pub async fn serve(addr: SocketAddr, cancel: CancellationToken) {
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(%addr, error = %e, "metrics listener could not bind; /metrics disabled");
            return;
        }
    };
    tracing::info!(%addr, "Prometheus metrics listening on /metrics");
    let shutdown = async move { cancel.cancelled().await };
    if let Err(e) = axum::serve(listener, router())
        .with_graceful_shutdown(shutdown)
        .await
    {
        tracing::warn!(error = %e, "metrics listener stopped");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn help_names(text: &str) -> Vec<String> {
        text.lines()
            .filter_map(|l| l.strip_prefix("# HELP "))
            .filter_map(|l| l.split_whitespace().next())
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn every_contract_family_is_exported() {
        set_build_info("abc123");
        prime_job("metrics-prime-test");
        record_request(
            "GET",
            "/api/v1/notices/{id}",
            200,
            Duration::from_millis(12),
        );
        let text = global().render();
        let help = help_names(&text);
        for name in METRIC_NAMES {
            let base = name.strip_suffix("_total").unwrap_or(name);
            assert!(help.iter().any(|h| h == base), "{name} missing in\n{text}");
        }
        assert!(text.contains("sutra_build_info{version=\""));
        assert!(text.contains("git_sha=\"abc123\""));
        assert!(text.contains(
            "sutra_http_requests_total{method=\"GET\",route=\"/api/v1/notices/{id}\",status=\"2xx\"}"
        ));
        assert!(text.contains("sutra_http_request_duration_seconds_bucket{"));
    }

    #[test]
    fn labels_never_carry_ids() {
        let label_names = |text: &str, metric: &str| -> Vec<String> {
            text.lines()
                .filter(|l| l.starts_with(metric) && l.contains('{'))
                .flat_map(|l| {
                    let inner = &l[l.find('{').unwrap() + 1..l.rfind('}').unwrap()];
                    inner
                        .split(',')
                        .filter_map(|kv| kv.split_once('=').map(|(k, _)| k.to_owned()))
                        .collect::<Vec<_>>()
                })
                .collect()
        };
        record_request("POST", "unmatched", 404, Duration::from_millis(1));
        let text = global().render();
        let names = label_names(&text, "sutra_http_requests_total");
        assert!(!names.is_empty());
        for l in names {
            assert!(["method", "route", "status"].contains(&l.as_str()), "{l}");
        }
    }

    #[test]
    fn status_classes() {
        assert_eq!(status_class(101), "1xx");
        assert_eq!(status_class(204), "2xx");
        assert_eq!(status_class(304), "3xx");
        assert_eq!(status_class(429), "4xx");
        assert_eq!(status_class(503), "5xx");
    }

    #[test]
    fn events_feed_job_outbox_and_audit_counters() {
        let job = |name: &str, outcome: &str| {
            global()
                .job_runs
                .get_or_create(&JobLabels {
                    job: name.to_owned(),
                    outcome: outcome.to_owned(),
                })
                .get()
        };
        let ev = |message: &str| EventFields {
            message: message.to_owned(),
            job: Some("metrics-test-job".to_owned()),
            ..EventFields::default()
        };
        apply("sutra::metrics", &ev("sweeper run"));
        apply(
            "sutra_persistence::jobs",
            &ev("sweeper skipped: another replica holds the lock"),
        );
        apply("sutra_persistence::jobs", &ev("sweeper run failed"));
        assert_eq!(job("metrics-test-job", "ok"), 1);
        assert_eq!(job("metrics-test-job", "skipped"), 1);
        assert_eq!(job("metrics-test-job", "error"), 1);

        let published = global().outbox_published.get();
        let failed = global().outbox_failed.get();
        apply(
            "sutra::metrics",
            &EventFields {
                message: "outbox relay pass".into(),
                relay_published: 3,
                relay_retried: 1,
                relay_dead_lettered: 1,
                ..EventFields::default()
            },
        );
        assert!(global().outbox_published.get() >= published + 3);
        assert!(global().outbox_failed.get() >= failed + 2);

        let broken = global().audit_verify_failures.get();
        apply(
            "sutra::metrics",
            &EventFields {
                message: "audit chain verification failed".into(),
                audit_chain_verify_failed: true,
                ..EventFields::default()
            },
        );
        assert!(global().audit_verify_failures.get() > broken);
    }

    #[tokio::test]
    async fn metrics_router_serves_openmetrics_text() {
        use http_body_util::BodyExt;
        use tower::ServiceExt;
        let res = router()
            .oneshot(
                axum::http::Request::get("/metrics")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.headers()[header::CONTENT_TYPE], CONTENT_TYPE);
        let body = res.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(text.contains("# TYPE sutra_http_requests_in_flight gauge"));
        assert!(text.trim_end().ends_with("# EOF"));
    }
}
