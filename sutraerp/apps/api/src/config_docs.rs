//! Part 2 · Stage 4.2 (S4-docs): document storage configuration.
//!
//! | Variable | Default | |
//! |----------|---------|-|
//! | `SUTRA_DOCUMENT_STORAGE` | `disabled` (dev / test) | `s3` = S3-compatible object storage (MinIO / AWS S3). **Required (`s3`) in production.** |
//! | `SUTRA_S3_ENDPOINT` | — | `http(s)://host[:port]` the API talks to; `https` required in production |
//! | `SUTRA_S3_PUBLIC_ENDPOINT` | endpoint | browser-facing endpoint used only in presigned links |
//! | `SUTRA_S3_REGION` | `us-east-1` | |
//! | `SUTRA_S3_BUCKET` | — | private bucket (anonymous access is refused at start of use) |
//! | `SUTRA_S3_ACCESS_KEY_ID` / `SUTRA_S3_SECRET_ACCESS_KEY` | — | OpenBao `kv/sutraerp/<env>/storage`; dev / CI secrets refused in production |
//! | `SUTRA_S3_PATH_STYLE` | `true` | `false` = virtual-hosted addressing (AWS) |
//! | `SUTRA_S3_SSE` | `none` (dev), `AES256` (production) | `none` \| `AES256` \| `aws:kms` |
//! | `SUTRA_S3_SSE_KMS_KEY_ID` | — | key of `aws:kms` |
//! | `SUTRA_S3_CREATE_BUCKET` | `false` | create the bucket when missing (dev / CI) |
//! | `SUTRA_S3_TIMEOUT_MS` | `15000` | per request |
//! | `SUTRA_DOCS_MAX_BYTES` | `10485760` | largest file, 1 KiB ..= 64 MiB (also the upload routes' body limit) |
//! | `SUTRA_DOCS_RETENTION_DAYS` | `2920` (8 years) | minimum retention from upload; ≥ 1 in production |
//! | `SUTRA_DOCS_PRESIGN_TTL_SECS` | `0` (off) | presigned download links, 1 ..= 300 s |
//! | `SUTRA_DOCS_SWEEP_SECS` | `3600` | retention sweeper interval (`0` = off) |

use crate::config::{ConfigError, Environment, SecretString, Source};
use std::time::Duration;

/// `SUTRA_DOCUMENT_STORAGE`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocumentStorageMode {
    /// No object storage: the file endpoints answer 503. Refused in production.
    Disabled,
    /// S3-compatible object storage (`sutra_infra::s3`).
    S3,
}

/// `SUTRA_S3_SSE`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SseMode {
    None,
    Aes256,
    Kms,
}

/// S3 connection settings.
#[derive(Clone, Debug)]
pub struct S3Config {
    pub endpoint: Option<String>,
    pub public_endpoint: Option<String>,
    pub region: String,
    pub bucket: Option<String>,
    pub access_key_id: Option<String>,
    pub secret_access_key: Option<SecretString>,
    pub path_style: bool,
    pub sse: SseMode,
    pub sse_kms_key_id: Option<String>,
    pub create_bucket: bool,
    pub timeout: Duration,
}

/// Everything about document storage (`Config::documents`).
#[derive(Clone, Debug)]
pub struct DocumentStorageConfig {
    pub mode: DocumentStorageMode,
    pub s3: S3Config,
    pub max_bytes: u64,
    pub retention_days: u32,
    pub presign_ttl_secs: u64,
    pub sweep_interval: Duration,
}

/// Secrets of the compose / CI MinIO and MinIO's factory default; refused in production.
pub const DEV_S3_SECRETS: [&str; 3] =
    ["minioadmin", "sutra-minio-dev-only", "sutraci-minio-secret"];

/// Largest `SUTRA_DOCS_MAX_BYTES`.
pub const MAX_DOCUMENT_BYTES: u64 = 64 * 1024 * 1024;
/// Longest presigned link lifetime the API issues.
pub const MAX_PRESIGN_TTL_SECS: u64 = 300;

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

fn invalid(key: &'static str, value: impl Into<String>) -> ConfigError {
    ConfigError::Invalid {
        key,
        value: value.into(),
    }
}

impl DocumentStorageConfig {
    pub fn load(src: &dyn Source, env: Environment) -> Result<Self, ConfigError> {
        let mode = match src
            .get("SUTRA_DOCUMENT_STORAGE")
            .map(|v| v.trim().to_ascii_lowercase())
            .as_deref()
        {
            None | Some("disabled") | Some("off") => DocumentStorageMode::Disabled,
            Some("s3") | Some("minio") => DocumentStorageMode::S3,
            Some(v) => return Err(invalid("SUTRA_DOCUMENT_STORAGE", v)),
        };
        let sse = match src
            .get("SUTRA_S3_SSE")
            .map(|v| v.trim().to_ascii_lowercase())
            .as_deref()
        {
            None if env == Environment::Production => SseMode::Aes256,
            None | Some("none") | Some("off") => SseMode::None,
            Some("aes256") | Some("sse-s3") => SseMode::Aes256,
            Some("aws:kms") | Some("kms") | Some("sse-kms") => SseMode::Kms,
            Some(v) => return Err(invalid("SUTRA_S3_SSE", v)),
        };
        let s3 = S3Config {
            endpoint: src.get("SUTRA_S3_ENDPOINT"),
            public_endpoint: src.get("SUTRA_S3_PUBLIC_ENDPOINT"),
            region: src
                .get("SUTRA_S3_REGION")
                .unwrap_or_else(|| "us-east-1".to_owned()),
            bucket: src.get("SUTRA_S3_BUCKET"),
            access_key_id: src.get("SUTRA_S3_ACCESS_KEY_ID"),
            secret_access_key: src.get("SUTRA_S3_SECRET_ACCESS_KEY").map(SecretString),
            path_style: parse(src, "SUTRA_S3_PATH_STYLE", true)?,
            sse,
            sse_kms_key_id: src.get("SUTRA_S3_SSE_KMS_KEY_ID"),
            create_bucket: parse(src, "SUTRA_S3_CREATE_BUCKET", false)?,
            timeout: Duration::from_millis(parse(src, "SUTRA_S3_TIMEOUT_MS", 15_000u64)?),
        };
        let cfg = Self {
            mode,
            s3,
            max_bytes: parse(src, "SUTRA_DOCS_MAX_BYTES", 10 * 1024 * 1024u64)?,
            retention_days: parse(src, "SUTRA_DOCS_RETENTION_DAYS", 2_920u32)?,
            presign_ttl_secs: parse(src, "SUTRA_DOCS_PRESIGN_TTL_SECS", 0u64)?,
            sweep_interval: Duration::from_secs(parse(src, "SUTRA_DOCS_SWEEP_SECS", 3_600u64)?),
        };
        cfg.validate()?;
        Ok(cfg)
    }

    /// Rules of every environment.
    fn validate(&self) -> Result<(), ConfigError> {
        if !(1024..=MAX_DOCUMENT_BYTES).contains(&self.max_bytes) {
            return Err(invalid(
                "SUTRA_DOCS_MAX_BYTES",
                format!("{} (must be 1 KiB ..= 64 MiB)", self.max_bytes),
            ));
        }
        if self.retention_days > 36_500 {
            return Err(invalid(
                "SUTRA_DOCS_RETENTION_DAYS",
                format!("{} (at most 100 years)", self.retention_days),
            ));
        }
        if self.presign_ttl_secs > MAX_PRESIGN_TTL_SECS {
            return Err(invalid(
                "SUTRA_DOCS_PRESIGN_TTL_SECS",
                format!("{} (0 = off, at most 300)", self.presign_ttl_secs),
            ));
        }
        if self.s3.timeout.is_zero() {
            return Err(invalid("SUTRA_S3_TIMEOUT_MS", "0"));
        }
        if self.mode == DocumentStorageMode::S3 {
            let endpoint = self
                .s3
                .endpoint
                .as_deref()
                .ok_or_else(|| invalid("SUTRA_S3_ENDPOINT", "<missing> (required with s3)"))?;
            for (key, url) in [
                ("SUTRA_S3_ENDPOINT", Some(endpoint)),
                (
                    "SUTRA_S3_PUBLIC_ENDPOINT",
                    self.s3.public_endpoint.as_deref(),
                ),
            ] {
                if let Some(u) = url {
                    if !(u.starts_with("http://") || u.starts_with("https://")) {
                        return Err(invalid(key, u));
                    }
                }
            }
            if self.s3.bucket.is_none() {
                return Err(invalid("SUTRA_S3_BUCKET", "<missing> (required with s3)"));
            }
            if self.s3.access_key_id.is_none() || self.s3.secret_access_key.is_none() {
                return Err(invalid(
                    "SUTRA_S3_SECRET_ACCESS_KEY",
                    "<missing> (SUTRA_S3_ACCESS_KEY_ID and SUTRA_S3_SECRET_ACCESS_KEY are required with s3)",
                ));
            }
        }
        Ok(())
    }

    /// Settings of the S3 adapter (`None` unless `s3`).
    pub fn s3_settings(&self) -> Option<sutra_infra::s3::S3Settings> {
        if self.mode != DocumentStorageMode::S3 {
            return None;
        }
        let s = &self.s3;
        Some(sutra_infra::s3::S3Settings {
            endpoint: s.endpoint.clone()?,
            public_endpoint: s.public_endpoint.clone(),
            region: s.region.clone(),
            bucket: s.bucket.clone()?,
            access_key_id: s.access_key_id.clone()?,
            secret_access_key: s.secret_access_key.as_ref()?.0.clone(),
            path_style: s.path_style,
            sse: match s.sse {
                SseMode::None => sutra_infra::s3::ServerSideEncryption::None,
                SseMode::Aes256 => sutra_infra::s3::ServerSideEncryption::Aes256,
                SseMode::Kms => sutra_infra::s3::ServerSideEncryption::Kms,
            },
            sse_kms_key_id: s.sse_kms_key_id.clone(),
            create_bucket: s.create_bucket,
            timeout: s.timeout,
        })
    }
}

/// Production start-up gate (called last by `Config::validate`): documents need real,
/// encrypted-in-transit object storage with non-development credentials.
pub fn validate_production(d: &DocumentStorageConfig) -> Result<(), ConfigError> {
    if d.mode == DocumentStorageMode::Disabled {
        return Err(ConfigError::RequiredInProduction(
            "SUTRA_DOCUMENT_STORAGE=s3 (with SUTRA_S3_ENDPOINT, SUTRA_S3_BUCKET, \
             SUTRA_S3_ACCESS_KEY_ID, SUTRA_S3_SECRET_ACCESS_KEY)",
        ));
    }
    if !d
        .s3
        .endpoint
        .as_deref()
        .is_some_and(|e| e.starts_with("https://"))
    {
        return Err(ConfigError::Forbidden(
            "SUTRA_S3_ENDPOINT must use https:// in production",
        ));
    }
    if d.s3
        .public_endpoint
        .as_deref()
        .is_some_and(|e| !e.starts_with("https://"))
    {
        return Err(ConfigError::Forbidden(
            "SUTRA_S3_PUBLIC_ENDPOINT must use https:// in production",
        ));
    }
    let secret =
        d.s3.secret_access_key
            .as_ref()
            .map(|s| s.0.as_str())
            .unwrap_or("");
    if secret.len() < 16 || DEV_S3_SECRETS.contains(&secret) {
        return Err(ConfigError::Forbidden(
            "SUTRA_S3_SECRET_ACCESS_KEY is a development / weak secret (16+ characters required) \
             and is not allowed in production",
        ));
    }
    if d.retention_days == 0 {
        return Err(ConfigError::Forbidden(
            "SUTRA_DOCS_RETENTION_DAYS=0 is not allowed in production",
        ));
    }
    if d.s3.create_bucket {
        return Err(ConfigError::Forbidden(
            "SUTRA_S3_CREATE_BUCKET=true is not allowed in production (provision the private \
             bucket with its retention / encryption policy first)",
        ));
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A valid production document storage environment (merged into `config_http::tests::prod`).
    pub(crate) const PROD: [(&str, &str); 5] = [
        ("SUTRA_DOCUMENT_STORAGE", "s3"),
        ("SUTRA_S3_ENDPOINT", "https://objects.example.edu"),
        ("SUTRA_S3_BUCKET", "sutraerp-documents"),
        ("SUTRA_S3_ACCESS_KEY_ID", "sutraerp-api"),
        (
            "SUTRA_S3_SECRET_ACCESS_KEY",
            "unit-test-only-object-storage-secret",
        ),
    ];

    fn load(
        env: Environment,
        pairs: &[(&'static str, &'static str)],
    ) -> Result<DocumentStorageConfig, ConfigError> {
        let m: HashMap<&'static str, &'static str> = pairs.iter().copied().collect();
        DocumentStorageConfig::load(&m, env)
    }

    #[test]
    fn defaults_are_disabled_outside_production() {
        let d = load(Environment::Development, &[]).unwrap();
        assert_eq!(d.mode, DocumentStorageMode::Disabled);
        assert_eq!(d.max_bytes, 10 * 1024 * 1024);
        assert_eq!(d.retention_days, 2_920);
        assert_eq!(d.presign_ttl_secs, 0);
        assert_eq!(d.s3.sse, SseMode::None);
        assert!(d.s3.path_style);
        assert!(d.s3_settings().is_none());
        let p = load(Environment::Production, &[]).unwrap();
        assert_eq!(p.s3.sse, SseMode::Aes256, "SSE on by default in production");
        assert!(matches!(
            validate_production(&p),
            Err(ConfigError::RequiredInProduction(_))
        ));
    }

    #[test]
    fn s3_needs_endpoint_bucket_and_keys() {
        assert!(load(
            Environment::Development,
            &[("SUTRA_DOCUMENT_STORAGE", "s3")]
        )
        .is_err());
        let ok = load(
            Environment::Development,
            &[
                ("SUTRA_DOCUMENT_STORAGE", "minio"),
                ("SUTRA_S3_ENDPOINT", "http://minio:9000"),
                ("SUTRA_S3_BUCKET", "sutraerp-documents"),
                ("SUTRA_S3_ACCESS_KEY_ID", "sutra"),
                ("SUTRA_S3_SECRET_ACCESS_KEY", "sutra-minio-dev-only"),
                ("SUTRA_S3_CREATE_BUCKET", "true"),
                ("SUTRA_DOCS_PRESIGN_TTL_SECS", "60"),
            ],
        )
        .unwrap();
        let s = ok.s3_settings().unwrap();
        assert_eq!(s.endpoint, "http://minio:9000");
        assert!(s.create_bucket && s.path_style);
        assert!(
            !format!("{ok:?}").contains("sutra-minio-dev-only"),
            "secret redacted"
        );
        for bad in [
            ("SUTRA_DOCUMENT_STORAGE", "ftp"),
            ("SUTRA_S3_SSE", "rot13"),
            ("SUTRA_DOCS_MAX_BYTES", "10"),
            ("SUTRA_DOCS_MAX_BYTES", "999999999999"),
            ("SUTRA_DOCS_PRESIGN_TTL_SECS", "3600"),
            ("SUTRA_DOCS_RETENTION_DAYS", "99999"),
            ("SUTRA_S3_TIMEOUT_MS", "0"),
            ("SUTRA_S3_PATH_STYLE", "maybe"),
        ] {
            assert!(load(Environment::Development, &[bad]).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn production_refuses_weak_or_plain_storage() {
        let p = load(Environment::Production, &PROD).unwrap();
        assert!(validate_production(&p).is_ok());
        assert_eq!(p.s3.sse, SseMode::Aes256);
        let with = |extra: (&'static str, &'static str)| {
            let mut v = PROD.to_vec();
            v.push(extra);
            validate_production(&load(Environment::Production, &v).unwrap())
        };
        for bad in [
            ("SUTRA_DOCUMENT_STORAGE", "disabled"),
            ("SUTRA_S3_ENDPOINT", "http://objects.example.edu"),
            ("SUTRA_S3_PUBLIC_ENDPOINT", "http://files.example.edu"),
            ("SUTRA_S3_SECRET_ACCESS_KEY", "sutraci-minio-secret"),
            ("SUTRA_S3_SECRET_ACCESS_KEY", "short"),
            ("SUTRA_DOCS_RETENTION_DAYS", "0"),
            ("SUTRA_S3_CREATE_BUCKET", "true"),
        ] {
            assert!(with(bad).is_err(), "{bad:?} must be refused in production");
        }
        assert!(
            with(("SUTRA_S3_SSE", "none")).is_ok(),
            "MinIO without KMS may opt out"
        );
    }
}
