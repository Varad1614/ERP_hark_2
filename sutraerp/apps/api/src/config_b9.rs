//! Batch 9 (production hardening) configuration: telemetry export, audit chaining, the Flowable
//! workflow adapter and OpenSearch projections (`docs/batch9-contracts.md` §1).
//!
//! Foundation-owned: every variable name and default here is contract. Slices read
//! `Config::hardening` and never parse these variables themselves; a slice that needs a new
//! setting appends it at the end of the matching struct with a `// 9X:` marker and a test.

use crate::config::{ConfigError, Environment, Key32, SecretString, Source};
use std::net::SocketAddr;
use std::time::Duration;

/// All Batch 9 settings (`Config::hardening`).
#[derive(Debug, Clone)]
pub struct HardeningConfig {
    pub telemetry: TelemetryConfig,
    pub audit_chain: AuditChainConfig,
    pub workflow: WorkflowConfig,
    pub search: SearchConfig,
    // INT: SEC-9D-003 — payment gateway selection + webhook secret.
    pub payments: PaymentsConfig,
}

/// INT (SEC-9D-003): `SUTRA_PAYMENT_GATEWAY`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaymentGatewayMode {
    /// No gateway: student online checkout and the gateway webhook answer 503 (feature off).
    /// Default in `production`.
    Disabled,
    /// `sutra_finance::MockPaymentGateway` (deterministic, captures without money). Default in
    /// `development` / `test`; refused in `production` with the public dev webhook secret.
    Mock,
}

/// INT (SEC-9D-003): online payments.
#[derive(Debug, Clone)]
pub struct PaymentsConfig {
    pub gateway: PaymentGatewayMode,
    /// `SUTRA_PAYMENT_WEBHOOK_SECRET` (OpenBao `kv/sutraerp/<env>/payments#webhook_secret`):
    /// HMAC-SHA256 key of the gateway webhook. Unset → the dev constant outside production.
    pub webhook_secret: Option<SecretString>,
}

/// INT (SEC-9D-003): the public development webhook secret (`sutra_finance::payments::gateway::
/// DEV_WEBHOOK_SECRET`); production refuses it.
pub const DEV_PAYMENT_WEBHOOK_SECRET: &str = "sutra-dev-gateway-secret";

/// `OTEL_EXPORTER_OTLP_PROTOCOL`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtlpProtocol {
    Grpc,
    HttpProtobuf,
}

/// OpenTelemetry export + Prometheus scrape endpoint (9A).
#[derive(Debug, Clone)]
pub struct TelemetryConfig {
    /// `OTEL_EXPORTER_OTLP_ENDPOINT`; `None` = no trace/log export (logs still go to stdout).
    pub otlp_endpoint: Option<String>,
    /// `OTEL_EXPORTER_OTLP_PROTOCOL` = `grpc` (default, port 4317) | `http/protobuf` (4318).
    pub otlp_protocol: OtlpProtocol,
    /// `OTEL_SERVICE_NAME` (default `sutra-api`).
    pub service_name: String,
    /// `OTEL_RESOURCE_ATTRIBUTES` (`k=v,k=v`, passed through verbatim).
    pub resource_attributes: Option<String>,
    /// `OTEL_TRACES_SAMPLER_ARG`: parent-based trace-id ratio 0.0 … 1.0 (default 1.0).
    pub sample_ratio: f64,
    /// `SUTRA_METRICS` (default true outside `test`): serve Prometheus text on `metrics_addr`.
    pub metrics_enabled: bool,
    /// `SUTRA_METRICS_ADDR` (default `0.0.0.0:9464`): separate listener, never the public port.
    pub metrics_addr: SocketAddr,
    // 9A: `SUTRA_GIT_SHA` (set by the image build, contract §3.3 `sutra_build_info{git_sha}`;
    // empty when unset).
    pub git_sha: String,
}

/// Tamper-evident audit chain sealer + anchors (9A).
#[derive(Debug, Clone)]
pub struct AuditChainConfig {
    /// `SUTRA_AUDIT_CHAIN` (default true outside `test`).
    pub enabled: bool,
    /// `SUTRA_AUDIT_SEAL_SECS` (default 10): sealer interval.
    pub seal_interval: Duration,
    /// `SUTRA_AUDIT_SEAL_BATCH` (default 500): rows sealed per tenant per tick.
    pub seal_batch: u32,
    /// `SUTRA_AUDIT_ANCHOR_SECS` (default 3600; 0 disables anchoring).
    pub anchor_interval: Duration,
    /// `SUTRA_AUDIT_ANCHOR_KEY` (64 hex; OpenBao `kv/sutraerp/<env>/audit#anchor_key`).
    pub anchor_key: Option<Key32>,
    /// `SUTRA_AUDIT_ANCHOR_KEY_ID` (default `k1`): stored with each anchor for key rotation.
    pub anchor_key_id: String,
}

/// `SUTRA_WORKFLOW`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowMode {
    /// `NoopWorkflowAdapter` (approvals auto-complete; Batches 2–8 behaviour).
    Noop,
    /// `sutra_platform::workflow_flowable::FlowableWorkflowAdapter` (9C).
    Flowable,
}

/// Flowable REST adapter (9C).
#[derive(Debug, Clone)]
pub struct WorkflowConfig {
    pub mode: WorkflowMode,
    /// `SUTRA_FLOWABLE_URL` (default `http://localhost:8090/flowable-rest`).
    pub flowable_url: String,
    /// `SUTRA_FLOWABLE_USER` (default `rest-admin`).
    pub flowable_user: String,
    /// `SUTRA_FLOWABLE_PASSWORD` (OpenBao `kv/sutraerp/<env>/flowable#password`).
    pub flowable_password: Option<SecretString>,
    /// `SUTRA_FLOWABLE_TIMEOUT_MS` (default 5000).
    pub timeout: Duration,
    /// `SUTRA_FLOWABLE_DEPLOY_ON_START` (default true): deploy `deploy/flowable/processes/*.bpmn20.xml`.
    pub deploy_on_start: bool,
}

/// `SUTRA_SEARCH`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    Off,
    OpenSearch,
}

/// OpenSearch projections (9C).
#[derive(Debug, Clone)]
pub struct SearchConfig {
    pub mode: SearchMode,
    /// `SUTRA_OPENSEARCH_URL` (default `http://localhost:9200`).
    pub url: String,
    /// `SUTRA_OPENSEARCH_USER`.
    pub user: Option<String>,
    /// `SUTRA_OPENSEARCH_PASSWORD` (OpenBao `kv/sutraerp/<env>/opensearch#password`).
    pub password: Option<SecretString>,
    /// `SUTRA_OPENSEARCH_INDEX_PREFIX` (default `sutra-<env>`).
    pub index_prefix: String,
    /// `SUTRA_SEARCH_POLL_MS` (default 2000): projector poll interval over the outbox.
    pub poll_interval: Duration,
    /// `SUTRA_SEARCH_BATCH` (default 500).
    pub batch_size: u32,
}

fn parse<T: std::str::FromStr>(
    src: &dyn Source,
    key: &'static str,
    default: T,
) -> Result<T, ConfigError> {
    match src.get(key) {
        None => Ok(default),
        Some(v) => v
            .trim()
            .parse()
            .map_err(|_| ConfigError::Invalid { key, value: v }),
    }
}

fn string(src: &dyn Source, key: &'static str, default: &str) -> String {
    src.get(key).unwrap_or_else(|| default.to_owned())
}

impl HardeningConfig {
    pub fn load(src: &dyn Source, env: Environment) -> Result<Self, ConfigError> {
        let not_test = env != Environment::Test;
        let env_name = match env {
            Environment::Development => "development",
            Environment::Test => "test",
            Environment::Production => "production",
        };
        let otlp_protocol = match src.get("OTEL_EXPORTER_OTLP_PROTOCOL").as_deref() {
            None | Some("grpc") => OtlpProtocol::Grpc,
            Some("http/protobuf") | Some("http") => OtlpProtocol::HttpProtobuf,
            Some(v) => {
                return Err(ConfigError::Invalid {
                    key: "OTEL_EXPORTER_OTLP_PROTOCOL",
                    value: v.into(),
                })
            }
        };
        let sample_ratio: f64 = parse(src, "OTEL_TRACES_SAMPLER_ARG", 1.0)?;
        if !(0.0..=1.0).contains(&sample_ratio) {
            return Err(ConfigError::Invalid {
                key: "OTEL_TRACES_SAMPLER_ARG",
                value: sample_ratio.to_string(),
            });
        }
        let telemetry = TelemetryConfig {
            otlp_endpoint: src.get("OTEL_EXPORTER_OTLP_ENDPOINT"),
            otlp_protocol,
            service_name: string(src, "OTEL_SERVICE_NAME", "sutra-api"),
            resource_attributes: src.get("OTEL_RESOURCE_ATTRIBUTES"),
            sample_ratio,
            metrics_enabled: parse(src, "SUTRA_METRICS", not_test)?,
            metrics_addr: parse(
                src,
                "SUTRA_METRICS_ADDR",
                SocketAddr::from(([0, 0, 0, 0], 9464)),
            )?,
            git_sha: string(src, "SUTRA_GIT_SHA", ""), // 9A
        };
        let audit_chain = AuditChainConfig {
            enabled: parse(src, "SUTRA_AUDIT_CHAIN", not_test)?,
            seal_interval: Duration::from_secs(parse(src, "SUTRA_AUDIT_SEAL_SECS", 10)?),
            seal_batch: parse(src, "SUTRA_AUDIT_SEAL_BATCH", 500)?,
            anchor_interval: Duration::from_secs(parse(src, "SUTRA_AUDIT_ANCHOR_SECS", 3600)?),
            anchor_key: src
                .get("SUTRA_AUDIT_ANCHOR_KEY")
                .map(|v| {
                    v.parse().map_err(|_| ConfigError::Invalid {
                        key: "SUTRA_AUDIT_ANCHOR_KEY",
                        value: "<64 hex characters expected>".into(),
                    })
                })
                .transpose()?,
            anchor_key_id: string(src, "SUTRA_AUDIT_ANCHOR_KEY_ID", "k1"),
        };
        let workflow_mode = match src
            .get("SUTRA_WORKFLOW")
            .as_deref()
            .map(str::to_lowercase)
            .as_deref()
        {
            None | Some("noop") => WorkflowMode::Noop,
            Some("flowable") => WorkflowMode::Flowable,
            Some(v) => {
                return Err(ConfigError::Invalid {
                    key: "SUTRA_WORKFLOW",
                    value: v.into(),
                })
            }
        };
        let workflow = WorkflowConfig {
            mode: workflow_mode,
            flowable_url: string(
                src,
                "SUTRA_FLOWABLE_URL",
                "http://localhost:8090/flowable-rest",
            ),
            flowable_user: string(src, "SUTRA_FLOWABLE_USER", "rest-admin"),
            flowable_password: src.get("SUTRA_FLOWABLE_PASSWORD").map(SecretString),
            timeout: Duration::from_millis(parse(src, "SUTRA_FLOWABLE_TIMEOUT_MS", 5000)?),
            deploy_on_start: parse(src, "SUTRA_FLOWABLE_DEPLOY_ON_START", true)?,
        };
        let search_mode = match src
            .get("SUTRA_SEARCH")
            .as_deref()
            .map(str::to_lowercase)
            .as_deref()
        {
            None | Some("off") => SearchMode::Off,
            Some("opensearch") => SearchMode::OpenSearch,
            Some(v) => {
                return Err(ConfigError::Invalid {
                    key: "SUTRA_SEARCH",
                    value: v.into(),
                })
            }
        };
        let search = SearchConfig {
            mode: search_mode,
            url: string(src, "SUTRA_OPENSEARCH_URL", "http://localhost:9200"),
            user: src.get("SUTRA_OPENSEARCH_USER"),
            password: src.get("SUTRA_OPENSEARCH_PASSWORD").map(SecretString),
            index_prefix: src
                .get("SUTRA_OPENSEARCH_INDEX_PREFIX")
                .unwrap_or_else(|| format!("sutra-{env_name}")),
            poll_interval: Duration::from_millis(parse(src, "SUTRA_SEARCH_POLL_MS", 2000)?),
            batch_size: parse(src, "SUTRA_SEARCH_BATCH", 500)?,
        };
        // INT: SEC-9D-003.
        let payment_gateway = match src
            .get("SUTRA_PAYMENT_GATEWAY")
            .as_deref()
            .map(str::to_lowercase)
            .as_deref()
        {
            None if env == Environment::Production => PaymentGatewayMode::Disabled,
            None => PaymentGatewayMode::Mock,
            Some("disabled") | Some("off") => PaymentGatewayMode::Disabled,
            Some("mock") => PaymentGatewayMode::Mock,
            Some(v) => {
                return Err(ConfigError::Invalid {
                    key: "SUTRA_PAYMENT_GATEWAY",
                    value: v.into(),
                })
            }
        };
        let payments = PaymentsConfig {
            gateway: payment_gateway,
            webhook_secret: src.get("SUTRA_PAYMENT_WEBHOOK_SECRET").map(SecretString),
        };
        if env == Environment::Production && payments.gateway != PaymentGatewayMode::Disabled {
            match payments.webhook_secret.as_ref().map(|s| s.0.as_str()) {
                None => {
                    return Err(ConfigError::RequiredInProduction(
                        "SUTRA_PAYMENT_WEBHOOK_SECRET",
                    ))
                }
                Some(s) if s == DEV_PAYMENT_WEBHOOK_SECRET || s.len() < 32 => {
                    return Err(ConfigError::Forbidden(
                        "SUTRA_PAYMENT_WEBHOOK_SECRET must not be the development secret and \
                         needs at least 32 characters in production",
                    ))
                }
                Some(_) => {}
            }
        }
        Ok(Self {
            telemetry,
            audit_chain,
            workflow,
            search,
            payments,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn defaults_are_off_or_local() {
        let c =
            HardeningConfig::load(&HashMap::<&str, &str>::new(), Environment::Development).unwrap();
        assert!(c.telemetry.otlp_endpoint.is_none());
        assert_eq!(c.telemetry.otlp_protocol, OtlpProtocol::Grpc);
        assert_eq!(c.telemetry.service_name, "sutra-api");
        assert!(c.telemetry.metrics_enabled);
        assert_eq!(c.telemetry.metrics_addr.port(), 9464);
        assert!(c.audit_chain.enabled);
        assert!(c.audit_chain.anchor_key.is_none());
        assert_eq!(c.workflow.mode, WorkflowMode::Noop);
        assert_eq!(c.search.mode, SearchMode::Off);
        assert_eq!(c.search.index_prefix, "sutra-development");
        let t = HardeningConfig::load(&HashMap::<&str, &str>::new(), Environment::Test).unwrap();
        assert!(!t.audit_chain.enabled);
        assert!(!t.telemetry.metrics_enabled);
    }

    #[test]
    fn parses_and_rejects() {
        let c = HardeningConfig::load(
            &HashMap::from([
                ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://otel:4317"),
                ("OTEL_EXPORTER_OTLP_PROTOCOL", "http/protobuf"),
                ("OTEL_TRACES_SAMPLER_ARG", "0.25"),
                ("SUTRA_WORKFLOW", "flowable"),
                ("SUTRA_SEARCH", "opensearch"),
                (
                    "SUTRA_AUDIT_ANCHOR_KEY",
                    "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff",
                ),
            ]),
            Environment::Production,
        )
        .unwrap();
        assert_eq!(c.telemetry.otlp_protocol, OtlpProtocol::HttpProtobuf);
        assert!((c.telemetry.sample_ratio - 0.25).abs() < f64::EPSILON);
        assert_eq!(c.workflow.mode, WorkflowMode::Flowable);
        assert_eq!(c.search.mode, SearchMode::OpenSearch);
        assert_eq!(c.search.index_prefix, "sutra-production");
        assert!(c.audit_chain.anchor_key.is_some());
        for (k, v) in [
            ("OTEL_TRACES_SAMPLER_ARG", "1.5"),
            ("OTEL_EXPORTER_OTLP_PROTOCOL", "thrift"),
            ("SUTRA_WORKFLOW", "camunda"),
            ("SUTRA_SEARCH", "solr"),
            ("SUTRA_AUDIT_ANCHOR_KEY", "short"),
        ] {
            assert!(
                HardeningConfig::load(&HashMap::from([(k, v)]), Environment::Development).is_err(),
                "{k}={v}"
            );
        }
    }

    // 9A: build identity for `sutra_build_info`.
    #[test]
    fn git_sha_is_passed_through() {
        let none =
            HardeningConfig::load(&HashMap::<&str, &str>::new(), Environment::Development).unwrap();
        assert_eq!(none.telemetry.git_sha, "");
        let set = HardeningConfig::load(
            &HashMap::from([("SUTRA_GIT_SHA", "0123456789ab")]),
            Environment::Production,
        )
        .unwrap();
        assert_eq!(set.telemetry.git_sha, "0123456789ab");
    }

    // INT: SEC-9D-003 — mock gateway off in production, dev secret refused.
    #[test]
    fn payment_gateway_defaults_and_production_rules() {
        let none = HashMap::<&str, &str>::new();
        let dev = HardeningConfig::load(&none, Environment::Development).unwrap();
        assert_eq!(dev.payments.gateway, PaymentGatewayMode::Mock);
        let test = HardeningConfig::load(&none, Environment::Test).unwrap();
        assert_eq!(test.payments.gateway, PaymentGatewayMode::Mock);
        let prod = HardeningConfig::load(&none, Environment::Production).unwrap();
        assert_eq!(prod.payments.gateway, PaymentGatewayMode::Disabled);
        let off = HardeningConfig::load(
            &HashMap::from([("SUTRA_PAYMENT_GATEWAY", "disabled")]),
            Environment::Development,
        )
        .unwrap();
        assert_eq!(off.payments.gateway, PaymentGatewayMode::Disabled);
        // Production + mock: secret required, the dev constant and short secrets refused.
        assert!(HardeningConfig::load(
            &HashMap::from([("SUTRA_PAYMENT_GATEWAY", "mock")]),
            Environment::Production
        )
        .is_err());
        assert!(HardeningConfig::load(
            &HashMap::from([
                ("SUTRA_PAYMENT_GATEWAY", "mock"),
                ("SUTRA_PAYMENT_WEBHOOK_SECRET", DEV_PAYMENT_WEBHOOK_SECRET),
            ]),
            Environment::Production
        )
        .is_err());
        assert!(HardeningConfig::load(
            &HashMap::from([
                ("SUTRA_PAYMENT_GATEWAY", "mock"),
                ("SUTRA_PAYMENT_WEBHOOK_SECRET", "short"),
            ]),
            Environment::Production
        )
        .is_err());
        let ok = HardeningConfig::load(
            &HashMap::from([
                ("SUTRA_PAYMENT_GATEWAY", "mock"),
                (
                    "SUTRA_PAYMENT_WEBHOOK_SECRET",
                    "0123456789abcdef0123456789abcdef0123",
                ),
            ]),
            Environment::Production,
        )
        .unwrap();
        assert_eq!(ok.payments.gateway, PaymentGatewayMode::Mock);
        assert!(HardeningConfig::load(
            &HashMap::from([("SUTRA_PAYMENT_GATEWAY", "razorpay")]),
            Environment::Development
        )
        .is_err());
    }
}
