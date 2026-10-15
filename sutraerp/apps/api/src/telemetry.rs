//! Structured logging, distributed tracing and OTLP export (ADR-L5-0029, docs/batch9-contracts.md
//! §3.1 / §3.2) — **owned by slice 9A**.
//!
//! * JSON logs in production, pretty logs in development (`SUTRA_LOG_FORMAT`). Every JSON line
//!   written inside a span carries the W3C `trace_id` / `span_id` (hex) of that span.
//! * Every process installs an OpenTelemetry tracer provider (parent-based trace-id ratio sampler,
//!   `OTEL_TRACES_SAMPLER_ARG`) bridged into `tracing` by `tracing-opentelemetry`, so trace ids
//!   exist (and inbound `traceparent` headers are honoured) even without an exporter.
//! * With `OTEL_EXPORTER_OTLP_ENDPOINT` set, spans **and** log records are exported over OTLP
//!   (`grpc` via tonic, or `http/protobuf`) through batch processors; [`shutdown`] flushes them.
//! * HTTP server spans follow the OTel HTTP semantic conventions: name `"{METHOD} {route}"`
//!   (`otel.name`), `http.request.method`, `http.route` (matched template, never the raw path),
//!   `http.response.status_code` (recorded by [`crate::metrics::track_http`]), `url.scheme`,
//!   `sutra.request_id`. No user names, e-mails or raw paths are recorded.
//! * Events with target `sutra::metrics` and the scheduler's job events also feed the Prometheus
//!   counters ([`crate::metrics::EventMetricsLayer`]).

use crate::config::{Config, Environment, LogFormat};
use crate::config_b9::{OtlpProtocol, TelemetryConfig};
use axum::extract::MatchedPath;
use axum::http::{HeaderMap, Request};
use opentelemetry::propagation::{Extractor, TextMapPropagator};
use opentelemetry::trace::{TraceContextExt, TracerProvider as _};
use opentelemetry::KeyValue;
use opentelemetry_otlp::{Protocol, WithExportConfig};
use opentelemetry_sdk::logs::SdkLoggerProvider;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::{Sampler, SdkTracerProvider};
use opentelemetry_sdk::Resource;
use std::fmt;
use std::sync::OnceLock;
use std::time::Duration;
use sutra_web::request_id::RequestId;
use tracing::{Event, Span, Subscriber};
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::fmt::format::{FormatEvent, FormatFields, JsonFields, Writer};
use tracing_subscriber::fmt::FmtContext;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer, Registry};

/// Instrumentation scope of every span the API creates.
pub const TRACER_NAME: &str = "sutra-api";

/// Export timeout per OTLP batch.
const EXPORT_TIMEOUT: Duration = Duration::from_secs(10);

/// Providers kept for the shutdown flush.
struct Providers {
    tracer: SdkTracerProvider,
    logger: Option<SdkLoggerProvider>,
}

static PROVIDERS: OnceLock<Providers> = OnceLock::new();

type BoxedLayer = Box<dyn Layer<Registry> + Send + Sync + 'static>;

fn env_filter(cfg: &Config) -> EnvFilter {
    EnvFilter::try_new(&cfg.log_filter).unwrap_or_else(|_| EnvFilter::new("info"))
}

/// Log records exported over OTLP: everything at `info`+ except the exporter's own transport
/// crates (their events would otherwise feed back into the exporter).
fn otlp_log_filter() -> Targets {
    Targets::new()
        .with_default(LevelFilter::INFO)
        .with_target("h2", LevelFilter::OFF)
        .with_target("hyper", LevelFilter::OFF)
        .with_target("hyper_util", LevelFilter::OFF)
        .with_target("tonic", LevelFilter::OFF)
        .with_target("tower", LevelFilter::OFF)
        .with_target("reqwest", LevelFilter::OFF)
        .with_target("opentelemetry", LevelFilter::OFF)
        .with_target("opentelemetry_sdk", LevelFilter::OFF)
        .with_target("opentelemetry_otlp", LevelFilter::OFF)
}

/// `deployment.environment` value of the resource.
pub fn environment_name(env: Environment) -> &'static str {
    match env {
        Environment::Development => "development",
        Environment::Test => "test",
        Environment::Production => "production",
    }
}

/// `k=v,k=v` (`OTEL_RESOURCE_ATTRIBUTES`) → attributes; malformed pairs are skipped.
pub fn parse_resource_attributes(raw: &str) -> Vec<KeyValue> {
    raw.split(',')
        .filter_map(|pair| {
            let (k, v) = pair.split_once('=')?;
            let (k, v) = (k.trim(), v.trim());
            (!k.is_empty()).then(|| KeyValue::new(k.to_owned(), v.to_owned()))
        })
        .collect()
}

fn hostname() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .filter(|h| !h.trim().is_empty())
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|h| h.trim().to_owned())
                .filter(|h| !h.is_empty())
        })
        .unwrap_or_else(|| "unknown".into())
}

/// Resource attributes of every exported span and log record (contract §3.1).
pub fn resource(cfg: &Config) -> Resource {
    use opentelemetry_semantic_conventions::resource::{
        DEPLOYMENT_ENVIRONMENT_NAME, SERVICE_INSTANCE_ID, SERVICE_VERSION,
    };
    let t = &cfg.hardening.telemetry;
    let env = environment_name(cfg.env);
    let mut attrs = t
        .resource_attributes
        .as_deref()
        .map(parse_resource_attributes)
        .unwrap_or_default();
    attrs.extend([
        KeyValue::new(SERVICE_VERSION, env!("CARGO_PKG_VERSION")),
        KeyValue::new("deployment.environment", env),
        KeyValue::new(DEPLOYMENT_ENVIRONMENT_NAME, env),
        KeyValue::new(SERVICE_INSTANCE_ID, hostname()),
    ]);
    Resource::builder_empty()
        .with_service_name(t.service_name.clone())
        .with_attributes(attrs)
        .build()
}

/// OTLP endpoint of one signal: gRPC takes the collector address as is, HTTP needs the
/// per-signal path (`/v1/traces`, `/v1/logs`).
pub fn signal_endpoint(t: &TelemetryConfig, endpoint: &str, signal: &str) -> String {
    match t.otlp_protocol {
        OtlpProtocol::Grpc => endpoint.to_owned(),
        OtlpProtocol::HttpProtobuf => {
            let base = endpoint.trim_end_matches('/');
            if base.ends_with(&format!("/v1/{signal}")) {
                base.to_owned()
            } else {
                format!("{base}/v1/{signal}")
            }
        }
    }
}

fn span_exporter(
    t: &TelemetryConfig,
    endpoint: &str,
) -> Result<opentelemetry_otlp::SpanExporter, opentelemetry_otlp::ExporterBuildError> {
    let ep = signal_endpoint(t, endpoint, "traces");
    match t.otlp_protocol {
        OtlpProtocol::Grpc => opentelemetry_otlp::SpanExporter::builder()
            .with_tonic()
            .with_protocol(Protocol::Grpc)
            .with_endpoint(ep)
            .with_timeout(EXPORT_TIMEOUT)
            .build(),
        OtlpProtocol::HttpProtobuf => opentelemetry_otlp::SpanExporter::builder()
            .with_http()
            .with_protocol(Protocol::HttpBinary)
            .with_endpoint(ep)
            .with_timeout(EXPORT_TIMEOUT)
            .build(),
    }
}

fn log_exporter(
    t: &TelemetryConfig,
    endpoint: &str,
) -> Result<opentelemetry_otlp::LogExporter, opentelemetry_otlp::ExporterBuildError> {
    let ep = signal_endpoint(t, endpoint, "logs");
    match t.otlp_protocol {
        OtlpProtocol::Grpc => opentelemetry_otlp::LogExporter::builder()
            .with_tonic()
            .with_protocol(Protocol::Grpc)
            .with_endpoint(ep)
            .with_timeout(EXPORT_TIMEOUT)
            .build(),
        OtlpProtocol::HttpProtobuf => opentelemetry_otlp::LogExporter::builder()
            .with_http()
            .with_protocol(Protocol::HttpBinary)
            .with_endpoint(ep)
            .with_timeout(EXPORT_TIMEOUT)
            .build(),
    }
}

/// JSON event formatter that adds the active OpenTelemetry `trace_id` / `span_id` (hex) to
/// every line written inside a span.
pub struct TraceJson<F>(pub F);

impl<S, N, F> FormatEvent<S, N> for TraceJson<F>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
    F: FormatEvent<S, N>,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let cx = opentelemetry::Context::current();
        let span = cx.span();
        let sc = span.span_context();
        if !sc.is_valid() {
            return self.0.format_event(ctx, writer, event);
        }
        let mut line = String::new();
        self.0.format_event(ctx, Writer::new(&mut line), event)?;
        writer.write_str(&with_trace_ids(
            &line,
            &sc.trace_id().to_string(),
            &sc.span_id().to_string(),
        ))
    }
}

/// Inserts `trace_id` / `span_id` into one rendered JSON log line (unchanged when the line is
/// not a JSON object).
pub fn with_trace_ids(line: &str, trace_id: &str, span_id: &str) -> String {
    let body = line.trim_end_matches('\n');
    match serde_json::from_str::<serde_json::Value>(body) {
        Ok(serde_json::Value::Object(mut map)) => {
            map.insert(
                "trace_id".into(),
                serde_json::Value::String(trace_id.into()),
            );
            map.insert("span_id".into(), serde_json::Value::String(span_id.into()));
            let mut out = serde_json::Value::Object(map).to_string();
            out.push('\n');
            out
        }
        _ => line.to_owned(),
    }
}

/// Installs the global subscriber, the W3C trace-context propagator and (with an endpoint) the
/// OTLP span + log exporters. Safe to call twice (the second call keeps the first subscriber).
pub fn init(cfg: &Config) {
    let t = &cfg.hardening.telemetry;
    let resource = resource(cfg);
    let mut tracer_builder = SdkTracerProvider::builder()
        .with_sampler(Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(
            t.sample_ratio,
        ))))
        .with_resource(resource.clone());
    let mut logger: Option<SdkLoggerProvider> = None;
    let mut export_errors: Vec<String> = vec![];
    if let Some(endpoint) = t.otlp_endpoint.as_deref().filter(|e| !e.trim().is_empty()) {
        match span_exporter(t, endpoint) {
            Ok(exporter) => tracer_builder = tracer_builder.with_batch_exporter(exporter),
            Err(e) => export_errors.push(format!("span exporter: {e}")),
        }
        match log_exporter(t, endpoint) {
            Ok(exporter) => {
                logger = Some(
                    SdkLoggerProvider::builder()
                        .with_resource(resource)
                        .with_batch_exporter(exporter)
                        .build(),
                )
            }
            Err(e) => export_errors.push(format!("log exporter: {e}")),
        }
    }
    let tracer_provider = tracer_builder.build();
    let tracer = tracer_provider.tracer(TRACER_NAME);
    opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());

    let mut layers: Vec<BoxedLayer> = Vec::with_capacity(4);
    layers.push(match cfg.log_format {
        LogFormat::Json => tracing_subscriber::fmt::layer()
            .fmt_fields(JsonFields::new())
            .event_format(TraceJson(
                tracing_subscriber::fmt::format()
                    .json()
                    .flatten_event(true)
                    .with_current_span(true)
                    .with_span_list(false),
            ))
            .with_filter(env_filter(cfg))
            .boxed(),
        LogFormat::Pretty => tracing_subscriber::fmt::layer()
            .compact()
            .with_filter(env_filter(cfg))
            .boxed(),
    });
    layers.push(
        tracing_opentelemetry::layer()
            .with_tracer(tracer)
            .with_filter(env_filter(cfg))
            .boxed(),
    );
    if let Some(lp) = &logger {
        layers.push(
            opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge::new(lp)
                .with_filter(otlp_log_filter())
                .with_filter(env_filter(cfg))
                .boxed(),
        );
    }
    layers.push(
        crate::metrics::EventMetricsLayer
            .with_filter(crate::metrics::EventMetricsLayer::filter())
            .boxed(),
    );
    if tracing_subscriber::registry()
        .with(layers)
        .try_init()
        .is_err()
    {
        // Already initialised (tests); keep the existing subscriber.
        return;
    }
    let exporting = logger.is_some();
    let _ = PROVIDERS.set(Providers {
        tracer: tracer_provider,
        logger,
    });
    for e in export_errors {
        tracing::error!(error = %e, "OTLP exporter could not be built; telemetry stays local");
    }
    if let Some(endpoint) = &t.otlp_endpoint {
        tracing::info!(
            %endpoint,
            protocol = ?t.otlp_protocol,
            sample_ratio = t.sample_ratio,
            logs = exporting,
            "OpenTelemetry OTLP export enabled"
        );
    }
}

/// Flushes and shuts down the OTLP exporters (blocking; call from `spawn_blocking`). Idempotent.
pub fn shutdown() {
    let Some(p) = PROVIDERS.get() else {
        return;
    };
    if let Err(e) = p.tracer.force_flush() {
        eprintln!("telemetry: span flush failed: {e}");
    }
    if let Some(l) = &p.logger {
        if let Err(e) = l.force_flush() {
            eprintln!("telemetry: log flush failed: {e}");
        }
        let _ = l.shutdown();
    }
    let _ = p.tracer.shutdown();
}

/// Waits for `cancel`, then flushes the exporters (spawned beside the workers by `serve`).
pub async fn flush_on_shutdown(cancel: tokio_util::sync::CancellationToken) {
    cancel.cancelled().await;
    if tokio::task::spawn_blocking(shutdown).await.is_err() {
        eprintln!("telemetry: shutdown task panicked");
    }
}

/// `http::HeaderMap` as an OpenTelemetry propagation carrier.
pub struct HeaderExtractor<'a>(pub &'a HeaderMap);

impl Extractor for HeaderExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|v| v.to_str().ok())
    }

    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(|k| k.as_str()).collect()
    }
}

/// Remote parent context of an inbound request (W3C `traceparent` / `tracestate`).
pub fn extract_parent(headers: &HeaderMap) -> opentelemetry::Context {
    TraceContextPropagator::new().extract(&HeaderExtractor(headers))
}

/// Known HTTP methods keep their name; anything else is `_OTHER` (OTel semconv).
pub fn method_name(m: &axum::http::Method) -> &'static str {
    match m.as_str() {
        "GET" => "GET",
        "HEAD" => "HEAD",
        "POST" => "POST",
        "PUT" => "PUT",
        "PATCH" => "PATCH",
        "DELETE" => "DELETE",
        "OPTIONS" => "OPTIONS",
        "CONNECT" => "CONNECT",
        "TRACE" => "TRACE",
        _ => "_OTHER",
    }
}

/// Span factory for `tower_http::trace::TraceLayer`: one server span per request named after
/// the matched route, parented on the inbound `traceparent`.
pub fn make_span<B>(req: &Request<B>) -> Span {
    let request_id = req
        .extensions()
        .get::<RequestId>()
        .map(|r| r.0.as_str())
        .unwrap_or("-");
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(MatchedPath::as_str);
    let method = method_name(req.method());
    let name = match route {
        Some(r) => format!("{method} {r}"),
        None => method.to_owned(),
    };
    let span = tracing::info_span!(
        "http.request",
        otel.name = %name,
        otel.kind = "server",
        otel.status_code = tracing::field::Empty,
        http.request.method = method,
        http.route = route.unwrap_or("-"),
        http.response.status_code = tracing::field::Empty,
        url.scheme = "http",
        sutra.request_id = %request_id,
        sutra.tenant_id = tracing::field::Empty,
    );
    let parent = extract_parent(req.headers());
    if parent.span().span_context().is_remote() {
        let _ = span.set_parent(parent);
    }
    span
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn resource_attributes_parse_and_skip_garbage() {
        let kv = parse_resource_attributes("team=erp, region = in-south ,broken,=x");
        let pairs: Vec<(String, String)> = kv
            .iter()
            .map(|k| (k.key.as_str().to_owned(), k.value.as_str().into_owned()))
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("team".to_owned(), "erp".to_owned()),
                ("region".to_owned(), "in-south".to_owned())
            ]
        );
    }

    #[test]
    fn http_endpoints_get_the_signal_path_grpc_stays_as_is() {
        let mut cfg = Config::load(&HashMap::from([("SUTRA_ENV", "test")]))
            .unwrap()
            .hardening
            .telemetry;
        assert_eq!(
            signal_endpoint(&cfg, "http://c:4317", "traces"),
            "http://c:4317"
        );
        cfg.otlp_protocol = OtlpProtocol::HttpProtobuf;
        assert_eq!(
            signal_endpoint(&cfg, "http://c:4318/", "logs"),
            "http://c:4318/v1/logs"
        );
        assert_eq!(
            signal_endpoint(&cfg, "http://c:4318/v1/traces", "traces"),
            "http://c:4318/v1/traces"
        );
    }

    #[test]
    fn trace_ids_are_added_to_json_lines_only() {
        let out = with_trace_ids(
            "{\"message\":\"hi\",\"level\":\"INFO\"}\n",
            "0af7651916cd43dd8448eb211c80319c",
            "b7ad6b7169203331",
        );
        let v: serde_json::Value = serde_json::from_str(out.trim_end()).unwrap();
        assert_eq!(v["trace_id"], "0af7651916cd43dd8448eb211c80319c");
        assert_eq!(v["span_id"], "b7ad6b7169203331");
        assert_eq!(v["message"], "hi");
        assert!(out.ends_with('\n'));
        assert_eq!(with_trace_ids("plain text\n", "a", "b"), "plain text\n");
    }

    #[test]
    fn inbound_traceparent_is_extracted() {
        let mut h = HeaderMap::new();
        h.insert(
            "traceparent",
            "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01"
                .parse()
                .unwrap(),
        );
        let cx = extract_parent(&h);
        let span = cx.span();
        let sc = span.span_context();
        assert!(sc.is_valid() && sc.is_remote());
        assert_eq!(
            sc.trace_id().to_string(),
            "0af7651916cd43dd8448eb211c80319c"
        );
        let none = extract_parent(&HeaderMap::new());
        assert!(!none.span().span_context().is_valid());
    }

    #[test]
    fn methods_are_normalised() {
        assert_eq!(method_name(&axum::http::Method::PATCH), "PATCH");
        let odd = axum::http::Method::from_bytes(b"PURGE").unwrap();
        assert_eq!(method_name(&odd), "_OTHER");
    }
}
