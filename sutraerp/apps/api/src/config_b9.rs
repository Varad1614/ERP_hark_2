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
    /// `development` / `test`; always refused in `production` (Part 1 · Stage 3, DEF-F-002).
    Mock,
    /// Part 2 S4-pay: Razorpay (`crate::payment_gateway::RazorpayGateway`). Needs
    /// [`RazorpayConfig`] and the webhook secret; production needs a live key (`rzp_live_…`) and an
    /// `https` base URL.
    Razorpay,
}

/// Part 2 S4-pay: Razorpay API settings (`SUTRA_RAZORPAY_*`).
#[derive(Debug, Clone)]
pub struct RazorpayConfig {
    /// `SUTRA_RAZORPAY_KEY_ID` (`rzp_test_…` / `rzp_live_…`; public, also used by Checkout).
    pub key_id: String,
    /// `SUTRA_RAZORPAY_KEY_SECRET` or the file named by `SUTRA_RAZORPAY_KEY_SECRET_FILE`
    /// (OpenBao `kv/sutraerp/<env>/payments#razorpay_key_secret`). Never logged.
    pub key_secret: SecretString,
    /// `SUTRA_RAZORPAY_BASE_URL` (default `https://api.razorpay.com`).
    pub base_url: String,
    /// `SUTRA_RAZORPAY_TIMEOUT_MS` (default 10000, 500 … 60000): per-request timeout.
    pub timeout: Duration,
    /// `SUTRA_RAZORPAY_MAX_RETRIES` (default 2, at most 5): extra attempts on timeouts, 429, 5xx.
    pub max_retries: u32,
}

/// Part 2 S4-pay: pending / failed gateway payment recovery job (`SUTRA_PAYMENT_RECOVERY_*`).
#[derive(Debug, Clone)]
pub struct PaymentRecoveryConfig {
    /// `SUTRA_PAYMENT_RECOVERY_INTERVAL_SECS` (default 300; 0 = off). Runs with the sweepers.
    pub interval: Duration,
    /// `SUTRA_PAYMENT_RECOVERY_MIN_AGE_SECS` (default 600): a payment is first polled this long
    /// after it was created (the payer may still be in checkout); later polls back off
    /// exponentially.
    pub min_age: Duration,
    /// `SUTRA_PAYMENT_RECOVERY_EXPIRE_AFTER_HOURS` (default 48): an open payment the gateway still
    /// reports as pending is expired after this; failed payments are re-checked for a late
    /// capture until then.
    pub expire_after: Duration,
    /// `SUTRA_PAYMENT_RECOVERY_BATCH` (default 50): payments polled per run.
    pub batch_size: u32,
}

/// INT (SEC-9D-003): online payments.
#[derive(Debug, Clone)]
pub struct PaymentsConfig {
    pub gateway: PaymentGatewayMode,
    /// `SUTRA_PAYMENT_WEBHOOK_SECRET` (OpenBao `kv/sutraerp/<env>/payments#webhook_secret`):
    /// HMAC-SHA256 key of the gateway webhook. Unset → the dev constant outside production.
    /// Part 2 S4-pay: may also come from the file named by `SUTRA_PAYMENT_WEBHOOK_SECRET_FILE`.
    pub webhook_secret: Option<SecretString>,
    /// Part 2 S4-pay: set when `gateway = Razorpay`.
    pub razorpay: Option<RazorpayConfig>,
    /// Part 2 S4-pay: recovery polling of open / failed gateway payments.
    pub recovery: PaymentRecoveryConfig,
}

/// Part 2 S4-pay: a secret from `key`, or from the file named by `file_key` (Kubernetes / OpenBao
/// agent secret files; trailing newline trimmed). The value never appears in errors.
fn secret(
    src: &dyn Source,
    key: &'static str,
    file_key: &'static str,
) -> Result<Option<SecretString>, ConfigError> {
    if let Some(v) = src.get(key) {
        return Ok(Some(SecretString(v)));
    }
    match src.get(file_key) {
        None => Ok(None),
        Some(path) => {
            let v = std::fs::read_to_string(path.trim()).map_err(|e| ConfigError::Invalid {
                key: file_key,
                value: format!("{path}: {}", e.kind()),
            })?;
            let v = v.trim_end_matches(['\r', '\n']).to_owned();
            Ok((!v.is_empty()).then_some(SecretString(v)))
        }
    }
}

/// `rzp_test_…` / `rzp_live_…` followed by letters and digits.
fn is_razorpay_key_id(id: &str) -> bool {
    ["rzp_test_", "rzp_live_"].iter().any(|p| {
        id.strip_prefix(p)
            .is_some_and(|r| !r.is_empty() && r.chars().all(|c| c.is_ascii_alphanumeric()))
    })
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
    /// `SUTRA_METRICS_ADDR` (default `127.0.0.1:9464`): separate listener, never the public port.
    /// Audit A3 (M6): loopback by default because `/metrics` is unauthenticated; deployments
    /// whose Prometheus scrapes over the network set it explicitly (Helm: `0.0.0.0:<metricsPort>`
    /// behind the metrics NetworkPolicy; CI / on-prem monitoring: `0.0.0.0:9464`).
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
    /// `NoopWorkflowAdapter` (records a process id only; development / test — refused in
    /// production, Part 1 DEF-X-001).
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
    /// Part 1 (DEF-X-001): `SUTRA_APPROVAL_MODE` = `human` (default) | `auto`. One mode handed to
    /// every module state; `auto` is a development / test opt-in refused in production.
    pub approvals: sutra_kernel::approval::ApprovalMode,
    /// Part 1 (DEF-X-001): `SUTRA_FLOWABLE_AUTO_APPROVE` (default false). When false the Flowable
    /// adapter always starts processes with `autoApprove=false` (the BPMN waits at `decide`);
    /// refused in production.
    pub flowable_auto_approve: bool,
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
                SocketAddr::from(([127, 0, 0, 1], 9464)),
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
            approvals: match src.get("SUTRA_APPROVAL_MODE") {
                None => sutra_kernel::approval::ApprovalMode::default(),
                Some(v) => {
                    sutra_kernel::approval::ApprovalMode::parse(&v).ok_or(ConfigError::Invalid {
                        key: "SUTRA_APPROVAL_MODE",
                        value: v,
                    })?
                }
            },
            flowable_auto_approve: parse(src, "SUTRA_FLOWABLE_AUTO_APPROVE", false)?,
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
            Some("razorpay") => PaymentGatewayMode::Razorpay, // Part 2 S4-pay
            Some(v) => {
                return Err(ConfigError::Invalid {
                    key: "SUTRA_PAYMENT_GATEWAY",
                    value: v.into(),
                })
            }
        };
        // Part 2 S4-pay: Razorpay keys + recovery job.
        let razorpay = if payment_gateway == PaymentGatewayMode::Razorpay {
            let key_id = src
                .get("SUTRA_RAZORPAY_KEY_ID")
                .ok_or_else(|| ConfigError::Invalid {
                    key: "SUTRA_RAZORPAY_KEY_ID",
                    value: "required when SUTRA_PAYMENT_GATEWAY=razorpay".into(),
                })?
                .trim()
                .to_owned();
            if !is_razorpay_key_id(&key_id) {
                return Err(ConfigError::Invalid {
                    key: "SUTRA_RAZORPAY_KEY_ID",
                    value: format!("{key_id} (expected rzp_test_… or rzp_live_…)"),
                });
            }
            let key_secret = secret(
                src,
                "SUTRA_RAZORPAY_KEY_SECRET",
                "SUTRA_RAZORPAY_KEY_SECRET_FILE",
            )?
            .ok_or_else(|| ConfigError::Invalid {
                key: "SUTRA_RAZORPAY_KEY_SECRET",
                value: "required when SUTRA_PAYMENT_GATEWAY=razorpay".into(),
            })?;
            let base_url = string(src, "SUTRA_RAZORPAY_BASE_URL", "https://api.razorpay.com");
            if !(base_url.starts_with("https://") || base_url.starts_with("http://")) {
                return Err(ConfigError::Invalid {
                    key: "SUTRA_RAZORPAY_BASE_URL",
                    value: base_url,
                });
            }
            let timeout_ms: u64 = parse(src, "SUTRA_RAZORPAY_TIMEOUT_MS", 10_000)?;
            if !(500..=60_000).contains(&timeout_ms) {
                return Err(ConfigError::Invalid {
                    key: "SUTRA_RAZORPAY_TIMEOUT_MS",
                    value: timeout_ms.to_string(),
                });
            }
            let max_retries: u32 = parse(src, "SUTRA_RAZORPAY_MAX_RETRIES", 2)?;
            if max_retries > 5 {
                return Err(ConfigError::Invalid {
                    key: "SUTRA_RAZORPAY_MAX_RETRIES",
                    value: max_retries.to_string(),
                });
            }
            Some(RazorpayConfig {
                key_id,
                key_secret,
                base_url,
                timeout: Duration::from_millis(timeout_ms),
                max_retries,
            })
        } else {
            None
        };
        let recovery = PaymentRecoveryConfig {
            interval: Duration::from_secs(parse(src, "SUTRA_PAYMENT_RECOVERY_INTERVAL_SECS", 300)?),
            min_age: Duration::from_secs(parse(src, "SUTRA_PAYMENT_RECOVERY_MIN_AGE_SECS", 600)?),
            expire_after: Duration::from_secs(
                parse::<u64>(src, "SUTRA_PAYMENT_RECOVERY_EXPIRE_AFTER_HOURS", 48)?
                    .saturating_mul(3600),
            ),
            batch_size: parse(src, "SUTRA_PAYMENT_RECOVERY_BATCH", 50)?,
        };
        let payments = PaymentsConfig {
            gateway: payment_gateway,
            webhook_secret: secret(
                src,
                "SUTRA_PAYMENT_WEBHOOK_SECRET",
                "SUTRA_PAYMENT_WEBHOOK_SECRET_FILE",
            )?,
            razorpay,
            recovery,
        };
        // Part 2 S4-pay: Razorpay verifies every webhook with this secret — required in every
        // environment, never the public development constant.
        if payments.gateway == PaymentGatewayMode::Razorpay {
            match payments.webhook_secret.as_ref().map(|s| s.0.as_str()) {
                None => {
                    return Err(ConfigError::Invalid {
                        key: "SUTRA_PAYMENT_WEBHOOK_SECRET",
                        value: "required when SUTRA_PAYMENT_GATEWAY=razorpay".into(),
                    })
                }
                Some(s) if s == DEV_PAYMENT_WEBHOOK_SECRET => {
                    return Err(ConfigError::Forbidden(
                        "SUTRA_PAYMENT_WEBHOOK_SECRET must not be the development secret with \
                         SUTRA_PAYMENT_GATEWAY=razorpay",
                    ))
                }
                Some(_) => {}
            }
        }
        if env == Environment::Production {
            if let Some(r) = &payments.razorpay {
                if !r.key_id.starts_with("rzp_live_") {
                    return Err(ConfigError::Forbidden(
                        "SUTRA_RAZORPAY_KEY_ID must be a live key (rzp_live_…) in production: test \
                         mode captures without money",
                    ));
                }
                if !r.base_url.starts_with("https://") {
                    return Err(ConfigError::Forbidden(
                        "SUTRA_RAZORPAY_BASE_URL must use https in production",
                    ));
                }
            }
        }
        // Part 1 · Stage 3 (DEF-F-002): the mock "captures" without money — never in production,
        // whatever webhook secret accompanies it. Only `disabled` is valid until a real gateway
        // adapter exists (then this match grows its mode and the secret rules below apply).
        if env == Environment::Production && payments.gateway == PaymentGatewayMode::Mock {
            return Err(ConfigError::Forbidden(
                "SUTRA_PAYMENT_GATEWAY=mock is not allowed in production (the mock captures \
                 without money); use disabled until a real gateway adapter is configured",
            ));
        }
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
        // Audit A3 (M6): unauthenticated metrics stay on loopback unless configured otherwise.
        assert!(c.telemetry.metrics_addr.ip().is_loopback());
        assert!(c.audit_chain.enabled);
        assert!(c.audit_chain.anchor_key.is_none());
        assert_eq!(c.workflow.mode, WorkflowMode::Noop);
        // Part 1 (DEF-X-001): human decision by default, no Flowable auto-approval.
        assert_eq!(
            c.workflow.approvals,
            sutra_kernel::approval::ApprovalMode::AwaitDecision
        );
        assert!(!c.workflow.flowable_auto_approve);
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
        // Part 1 · Stage 3 (DEF-F-002): a strong secret no longer makes the mock acceptable.
        assert!(matches!(
            HardeningConfig::load(
                &HashMap::from([
                    ("SUTRA_PAYMENT_GATEWAY", "mock"),
                    (
                        "SUTRA_PAYMENT_WEBHOOK_SECRET",
                        "0123456789abcdef0123456789abcdef0123",
                    ),
                ]),
                Environment::Production,
            ),
            Err(ConfigError::Forbidden(_))
        ));
        let dev_mock = HardeningConfig::load(
            &HashMap::from([("SUTRA_PAYMENT_GATEWAY", "mock")]),
            Environment::Development,
        )
        .unwrap();
        assert_eq!(dev_mock.payments.gateway, PaymentGatewayMode::Mock);
        assert!(HardeningConfig::load(
            &HashMap::from([("SUTRA_PAYMENT_GATEWAY", "razorpay")]),
            Environment::Development
        )
        .is_err());
    }

    // Part 2 S4-pay: Razorpay keys, secret files, recovery defaults and production rules.
    #[test]
    fn razorpay_settings_and_production_rules() {
        const HOOK: &str = "whsec-0123456789abcdef0123456789abcdef";
        fn with(
            extra: &[(&'static str, &'static str)],
            drop: &[&str],
            env: Environment,
        ) -> Result<HardeningConfig, ConfigError> {
            let base = [
                ("SUTRA_PAYMENT_GATEWAY", "razorpay"),
                ("SUTRA_RAZORPAY_KEY_ID", "rzp_test_AbC123"),
                ("SUTRA_RAZORPAY_KEY_SECRET", "key-secret-value"),
                ("SUTRA_PAYMENT_WEBHOOK_SECRET", HOOK),
            ];
            let mut m: HashMap<&'static str, &'static str> = base
                .iter()
                .copied()
                .filter(|(k, _)| !drop.contains(k))
                .collect();
            m.extend(extra.iter().copied());
            HardeningConfig::load(&m, env)
        }
        let dev = with(&[], &[], Environment::Development).unwrap();
        assert_eq!(dev.payments.gateway, PaymentGatewayMode::Razorpay);
        let r = dev.payments.razorpay.as_ref().unwrap();
        assert_eq!(r.key_id, "rzp_test_AbC123");
        assert_eq!(r.key_secret.0, "key-secret-value");
        assert_eq!(r.base_url, "https://api.razorpay.com");
        assert_eq!(r.timeout, Duration::from_secs(10));
        assert_eq!(r.max_retries, 2);
        assert!(
            !format!("{r:?}").contains("key-secret-value"),
            "secret redacted"
        );
        assert_eq!(dev.payments.recovery.interval, Duration::from_secs(300));
        assert_eq!(dev.payments.recovery.min_age, Duration::from_secs(600));
        assert_eq!(
            dev.payments.recovery.expire_after,
            Duration::from_secs(48 * 3600)
        );
        // Missing / malformed keys and secrets are refused in every environment.
        for drop in [
            "SUTRA_RAZORPAY_KEY_ID",
            "SUTRA_RAZORPAY_KEY_SECRET",
            "SUTRA_PAYMENT_WEBHOOK_SECRET",
        ] {
            assert!(
                with(&[], &[drop], Environment::Development).is_err(),
                "{drop}"
            );
        }
        for (k, v) in [
            ("SUTRA_RAZORPAY_KEY_ID", "key_123"),
            ("SUTRA_RAZORPAY_KEY_ID", "rzp_test_"),
            ("SUTRA_RAZORPAY_BASE_URL", "ftp://x"),
            ("SUTRA_RAZORPAY_TIMEOUT_MS", "10"),
            ("SUTRA_RAZORPAY_MAX_RETRIES", "9"),
            ("SUTRA_PAYMENT_WEBHOOK_SECRET", DEV_PAYMENT_WEBHOOK_SECRET),
        ] {
            assert!(
                with(&[(k, v)], &[], Environment::Development).is_err(),
                "{k}={v}"
            );
        }
        // Secrets from files (trailing newline trimmed); an unreadable file is an error.
        // Per-test directory under the workspace's own (git-ignored) `target/`, not the shared,
        // world-writable system temp dir (Semgrep rust.lang.security.temp-dir).
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/sutra-test-tmp")
            .join(format!("cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("razorpay_key_secret");
        std::fs::write(&file, "from-file-secret\n").unwrap();
        let path: &'static str = Box::leak(file.to_string_lossy().into_owned().into_boxed_str());
        let f = with(
            &[("SUTRA_RAZORPAY_KEY_SECRET_FILE", path)],
            &["SUTRA_RAZORPAY_KEY_SECRET"],
            Environment::Development,
        )
        .unwrap();
        assert_eq!(
            f.payments.razorpay.unwrap().key_secret.0,
            "from-file-secret"
        );
        assert!(with(
            &[(
                "SUTRA_RAZORPAY_KEY_SECRET_FILE",
                "/nonexistent/sutra/secret"
            )],
            &["SUTRA_RAZORPAY_KEY_SECRET"],
            Environment::Development,
        )
        .is_err());
        // Production: live key + https only; test keys capture without money.
        assert!(matches!(
            with(&[], &[], Environment::Production),
            Err(ConfigError::Forbidden(_))
        ));
        let live = [("SUTRA_RAZORPAY_KEY_ID", "rzp_live_AbC123")];
        let prod = with(&live, &[], Environment::Production).unwrap();
        assert_eq!(prod.payments.gateway, PaymentGatewayMode::Razorpay);
        assert!(with(
            &[
                live[0],
                ("SUTRA_RAZORPAY_BASE_URL", "http://api.razorpay.com")
            ],
            &[],
            Environment::Production
        )
        .is_err());
        // The existing production webhook-secret rule (≥ 32 characters) still applies.
        assert!(with(
            &[live[0], ("SUTRA_PAYMENT_WEBHOOK_SECRET", "short-secret")],
            &[],
            Environment::Production
        )
        .is_err());
    }
}
