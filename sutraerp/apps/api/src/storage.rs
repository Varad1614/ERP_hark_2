//! Part 2 · S4-docs: document storage wiring of the composition root — the
//! [`ObjectStorePort`] over `sutra_infra::s3::S3Client`, its readiness check, the document vault
//! built from `Config::documents`, and the request-body limit that lets only the document upload
//! routes exceed `SUTRA_MAX_BODY_BYTES` (up to `SUTRA_DOCS_MAX_BYTES`) — and, Part 2 ·
//! S4-ui-import, the data import upload (`POST /api/v1/platform/imports`, up to the import
//! engine's 8 MiB file as base64 JSON).

use crate::config::Config;
use crate::config_docs::DocumentStorageMode;
use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use http::{header, Method, StatusCode};
use sqlx::PgPool;
use std::sync::Arc;
use std::time::Duration;
use sutra_contracts::platform::documents::{DocumentVault, ObjectStorePort};
use sutra_infra::s3::S3Client;
use sutra_kernel::error::KernelError;
use sutra_kernel::health::ReadinessCheck;
use sutra_platform::documents::{DisabledDocumentVault, PgDocumentVault, VaultSettings};

/// [`ObjectStorePort`] over the S3 REST client (`sutra-infra` may only depend on the kernel).
pub struct S3ObjectStore(pub Arc<S3Client>);

#[async_trait::async_trait]
impl ObjectStorePort for S3ObjectStore {
    fn backend(&self) -> &'static str {
        "s3"
    }

    async fn ready(&self) -> Result<(), KernelError> {
        self.0.health().await
    }

    async fn put(
        &self,
        key: &str,
        content_type: &str,
        body: Vec<u8>,
        sha256: [u8; 32],
    ) -> Result<(), KernelError> {
        self.0.put_object(key, content_type, body, &sha256).await
    }

    async fn get(&self, key: &str, max_bytes: u64) -> Result<Vec<u8>, KernelError> {
        self.0.get_object(key, max_bytes).await
    }

    async fn delete(&self, key: &str) -> Result<(), KernelError> {
        self.0.delete_object(key).await
    }

    fn presign_get(
        &self,
        key: &str,
        ttl: Duration,
        filename: &str,
        content_type: &str,
    ) -> Result<String, KernelError> {
        let disposition = format!("attachment; filename=\"{}\"", filename.replace('"', "_"));
        self.0
            .presign_get(key, ttl, Some(&disposition), Some(content_type))
    }
}

/// Non-critical readiness check `document-storage` (bucket answers and is private): documents
/// are one feature, the rest of the API keeps serving while storage is down.
pub struct ObjectStoreReadiness(pub Arc<dyn ObjectStorePort>);

#[async_trait::async_trait]
impl ReadinessCheck for ObjectStoreReadiness {
    fn name(&self) -> &'static str {
        "document-storage"
    }

    async fn check(&self) -> Result<(), String> {
        self.0.ready().await.map_err(|e| e.to_string())
    }

    fn critical(&self) -> bool {
        false
    }
}

/// The document service of this process, its object store (for the sweeper) and readiness.
pub struct DocumentStorage {
    pub vault: Arc<dyn DocumentVault>,
    pub store: Option<Arc<dyn ObjectStorePort>>,
    pub readiness: Option<Arc<dyn ReadinessCheck>>,
}

/// Builds the document service from `Config::documents` (`disabled` → every file endpoint 503).
pub fn build(cfg: &Config, pool: &PgPool) -> Result<DocumentStorage, String> {
    let d = &cfg.documents;
    if d.mode == DocumentStorageMode::Disabled {
        tracing::info!("documents: object storage disabled (file endpoints answer 503)");
        return Ok(DocumentStorage {
            vault: Arc::new(DisabledDocumentVault),
            store: None,
            readiness: None,
        });
    }
    let settings = d
        .s3_settings()
        .ok_or_else(|| "S3 document storage is incompletely configured".to_owned())?;
    tracing::info!(
        endpoint = %settings.endpoint,
        bucket = %settings.bucket,
        path_style = settings.path_style,
        sse = ?settings.sse,
        "documents: S3-compatible object storage"
    );
    let client = Arc::new(S3Client::new(settings)?);
    let store: Arc<dyn ObjectStorePort> = Arc::new(S3ObjectStore(client));
    let vault = PgDocumentVault::new(
        pool.clone(),
        store.clone(),
        VaultSettings {
            max_bytes: d.max_bytes,
            retention_days: d.retention_days,
            presign_ttl: (d.presign_ttl_secs > 0)
                .then_some(Duration::from_secs(d.presign_ttl_secs)),
        },
    );
    Ok(DocumentStorage {
        vault: Arc::new(vault),
        readiness: Some(Arc::new(ObjectStoreReadiness(store.clone()))),
        store: Some(store),
    })
}

/// Request-body limits: `default` (`SUTRA_MAX_BODY_BYTES`) for every route, `upload` for the
/// document file uploads (`PUT /api/v1/…/file`), `import` for the data import upload
/// (`POST /api/v1/platform/imports`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BodyLimits {
    pub default: usize,
    pub upload: usize,
    pub import: usize,
}

impl BodyLimits {
    pub fn from_config(cfg: &Config) -> Self {
        let docs = usize::try_from(cfg.documents.max_bytes).unwrap_or(usize::MAX);
        Self {
            default: cfg.http.max_body_bytes,
            // The handler enforces SUTRA_DOCS_MAX_BYTES exactly (422); the transport only needs
            // to let a body of that size through.
            upload: docs.max(cfg.http.max_body_bytes).saturating_add(1024),
            // Part 2 · S4-ui-import: the engine refuses files above 8 MiB (422); the base64 JSON
            // body of such a file must get through the transport first.
            import: sutra_platform::dataimport::http::UPLOAD_BODY_BYTES
                .max(cfg.http.max_body_bytes),
        }
    }

    pub fn for_request(&self, method: &Method, path: &str) -> usize {
        if is_document_upload(method, path) {
            self.upload
        } else if is_import_upload(method, path) {
            self.import
        } else {
            self.default
        }
    }
}

/// `POST /api/v1/platform/imports` — the data import upload (base64 file in a JSON body).
pub fn is_import_upload(method: &Method, path: &str) -> bool {
    method == Method::POST && path.trim_end_matches('/') == "/api/v1/platform/imports"
}

/// `PUT /api/v1/…/file` — the raw-file upload routes of the document service.
pub fn is_document_upload(method: &Method, path: &str) -> bool {
    method == Method::PUT && path.starts_with("/api/v1/") && path.ends_with("/file")
}

/// Middleware replacing a single global body limit: 413 when `Content-Length` is above the
/// route's limit, otherwise the body is wrapped so reading past it fails (also 413 for JSON).
pub async fn body_limit(State(limits): State<BodyLimits>, req: Request, next: Next) -> Response {
    let limit = limits.for_request(req.method(), req.uri().path());
    let declared = req
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok());
    if declared.is_some_and(|n| n > u64::try_from(limit).unwrap_or(u64::MAX)) {
        return sutra_web::Problem::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload-too-large",
            "Request body too large",
        )
        .into_response();
    }
    let (mut parts, body) = req.into_parts();
    // Idempotent requests are fingerprinted up to the route's limit (not the 1 MiB default).
    parts
        .extensions
        .insert(sutra_web::idempotency::RouteBodyLimit(limit));
    let body = axum::body::Body::new(http_body_util::Limited::new(body, limit));
    next.run(Request::from_parts(parts, body)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_document_uploads_get_the_upload_limit() {
        let l = BodyLimits {
            default: 2 * 1024 * 1024,
            upload: 10 * 1024 * 1024 + 1024,
            import: 11 * 1024 * 1024,
        };
        let p = "/api/v1/student/admissions/a/documents/b/file";
        assert_eq!(l.for_request(&Method::PUT, p), l.upload);
        assert_eq!(l.for_request(&Method::POST, p), l.default);
        assert_eq!(l.for_request(&Method::GET, p), l.default);
        assert_eq!(
            l.for_request(
                &Method::PUT,
                "/api/v1/student/admissions/a/documents/b/file/link"
            ),
            l.default
        );
        assert_eq!(l.for_request(&Method::PUT, "/file"), l.default);
        assert_eq!(
            l.for_request(&Method::POST, "/api/v1/auth/login"),
            l.default
        );
    }

    #[test]
    fn only_the_import_upload_gets_the_import_limit() {
        let l = BodyLimits {
            default: 2 * 1024 * 1024,
            upload: 10 * 1024 * 1024 + 1024,
            import: sutra_platform::dataimport::http::UPLOAD_BODY_BYTES,
        };
        let p = "/api/v1/platform/imports";
        assert_eq!(l.for_request(&Method::POST, p), l.import);
        assert_eq!(
            l.for_request(&Method::POST, "/api/v1/platform/imports/"),
            l.import
        );
        assert_eq!(l.for_request(&Method::GET, p), l.default);
        for other in [
            "/api/v1/platform/imports/mapping-profiles",
            "/api/v1/platform/imports/0192f5a0-7c3e-7000-8000-000000000001/commit",
            "/api/v1/platform/importsX",
        ] {
            assert_eq!(l.for_request(&Method::POST, other), l.default, "{other}");
        }
        // The base64 JSON of the engine's largest file (8 MiB) fits; that of a 9 MiB file does not.
        let json_of = |file: usize| file.div_ceil(3) * 4 + 1024;
        let limit = sutra_platform::dataimport::http::UPLOAD_BODY_BYTES;
        assert!(limit > json_of(8 * 1024 * 1024), "{limit}");
        assert!(limit < json_of(9 * 1024 * 1024), "{limit}");
    }
}
