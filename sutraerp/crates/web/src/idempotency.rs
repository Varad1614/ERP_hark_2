//! `Idempotency-Key` middleware (ADR-L5-0034 §3.6).
//!
//! For mutating requests (POST/PUT/PATCH/DELETE) that carry `Idempotency-Key`:
//!
//! 1. the request is fingerprinted (method, path+query, content-type, body SHA-256);
//! 2. the key is *begun* in the owner-scoped [`IdempotencyStore`]; the owner is the authenticated
//!    principal (`<tenant>/<subject>` from the session, see [`crate::auth`]) so one tenant or user can
//!    never replay another's stored response. Anonymous requests share the `anonymous` owner;
//! 3. first execution runs the handler and stores the final 2xx/4xx response;
//! 4. a retry with the same fingerprint replays the stored response with
//!    `Idempotency-Replayed: true`; a different fingerprint is rejected with 422;
//!    a retry while the first attempt is still running gets 409.
//!
//! 5xx outcomes release the key so the client may retry. Requests without the header pass
//! through untouched; handlers that *require* a key can use [`require_idempotency_key`].

use crate::problem::ApiError;
use axum::body::{to_bytes, Body};
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use sutra_kernel::auth::AuthContext;
use sutra_kernel::error::KernelError;
use sutra_kernel::idempotency::{BeginOutcome, IdempotencyStore, StoredResponse};

pub static IDEMPOTENCY_KEY: HeaderName = HeaderName::from_static("idempotency-key");
pub static IDEMPOTENCY_REPLAYED: HeaderName = HeaderName::from_static("idempotency-replayed");

/// Maximum request/response body buffered for fingerprinting and replay.
pub const MAX_BODY_BYTES: usize = 1024 * 1024;

pub type SharedIdempotencyStore = Arc<dyn IdempotencyStore>;

fn is_mutating(m: &Method) -> bool {
    matches!(
        *m,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    )
}

fn valid_key(k: &str) -> bool {
    !k.is_empty() && k.len() <= 255 && k.bytes().all(|b| b.is_ascii_graphic())
}

/// Reads and validates the `Idempotency-Key` header.
pub fn read_key(headers: &HeaderMap) -> Result<Option<String>, KernelError> {
    match headers.get(&IDEMPOTENCY_KEY) {
        None => Ok(None),
        Some(v) => {
            let s = v
                .to_str()
                .map_err(|_| KernelError::BadRequest("Idempotency-Key must be ASCII".into()))?;
            if valid_key(s) {
                Ok(Some(s.to_owned()))
            } else {
                Err(KernelError::BadRequest(
                    "Idempotency-Key must be 1-255 visible ASCII characters".into(),
                ))
            }
        }
    }
}

/// Rejects a request that lacks `Idempotency-Key` (for retry-sensitive commands).
pub fn require_idempotency_key(headers: &HeaderMap) -> Result<String, KernelError> {
    read_key(headers)?
        .ok_or_else(|| KernelError::BadRequest("Idempotency-Key header is required".into()))
}

fn fingerprint(method: &Method, uri: &str, content_type: Option<&str>, body: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(method.as_str().as_bytes());
    h.update([0]);
    h.update(uri.as_bytes());
    h.update([0]);
    h.update(content_type.unwrap_or("").as_bytes());
    h.update([0]);
    h.update(body);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Owner scope of an idempotency record: the authenticated principal within its tenant.
pub fn owner_key(ctx: Option<&AuthContext>) -> String {
    match ctx {
        Some(c) => format!("{}/{}", c.scope.tenant_id, c.subject),
        None => "anonymous".to_owned(),
    }
}

fn replay(stored: StoredResponse) -> Response {
    let status = StatusCode::from_u16(stored.status).unwrap_or(StatusCode::OK);
    let mut res = (status, stored.body).into_response();
    if let Some(ct) = stored
        .content_type
        .and_then(|c| HeaderValue::from_str(&c).ok())
    {
        res.headers_mut().insert(header::CONTENT_TYPE, ct);
    }
    res.headers_mut().insert(
        IDEMPOTENCY_REPLAYED.clone(),
        HeaderValue::from_static("true"),
    );
    res
}

pub async fn idempotency(
    State(store): State<SharedIdempotencyStore>,
    req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    if !is_mutating(req.method()) {
        return Ok(next.run(req).await);
    }
    let Some(key) = read_key(req.headers())? else {
        return Ok(next.run(req).await);
    };

    let (parts, body) = req.into_parts();
    let bytes = to_bytes(body, MAX_BODY_BYTES).await.map_err(|_| {
        KernelError::BadRequest("request body too large for idempotent processing".into())
    })?;
    let uri = parts
        .uri
        .path_and_query()
        .map(|p| p.as_str())
        .unwrap_or("/")
        .to_owned();
    let ct = parts
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok());
    let fp = fingerprint(&parts.method, &uri, ct, &bytes);
    let scope_key = owner_key(parts.extensions.get::<AuthContext>());

    match store.begin(&scope_key, &key, &fp).await? {
        BeginOutcome::Replay(stored) => return Ok(replay(stored)),
        BeginOutcome::InFlight => return Err(KernelError::IdempotencyInFlight.into()),
        BeginOutcome::Mismatch => return Err(KernelError::IdempotencyMismatch.into()),
        BeginOutcome::Started => {}
    }

    let res = next
        .run(Request::from_parts(parts, Body::from(bytes)))
        .await;
    let status = res.status();
    if status.is_server_error() {
        store.abandon(&scope_key, &key).await?;
        return Ok(res);
    }

    let (res_parts, res_body) = res.into_parts();
    let res_bytes = match to_bytes(res_body, MAX_BODY_BYTES).await {
        Ok(b) => b,
        Err(_) => {
            store.abandon(&scope_key, &key).await?;
            return Err(
                KernelError::internal_msg("response too large to store for idempotency").into(),
            );
        }
    };
    let content_type = res_parts
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    store
        .complete(
            &scope_key,
            &key,
            StoredResponse {
                status: status.as_u16(),
                content_type,
                body: res_bytes.to_vec(),
            },
        )
        .await?;
    Ok(Response::from_parts(res_parts, Body::from(res_bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{routing::post, Router};
    use http_body_util::BodyExt;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use sutra_kernel::idempotency::InMemoryIdempotencyStore;
    use tower::ServiceExt;

    fn app(counter: Arc<AtomicUsize>) -> Router {
        let store: SharedIdempotencyStore = Arc::new(InMemoryIdempotencyStore::default());
        Router::new()
            .route(
                "/things",
                post(move |body: String| {
                    let c = counter.clone();
                    async move {
                        let n = c.fetch_add(1, Ordering::SeqCst) + 1;
                        (
                            StatusCode::CREATED,
                            format!("{{\"n\":{n},\"echo\":{body:?}}}"),
                        )
                    }
                }),
            )
            .layer(axum::middleware::from_fn_with_state(store, idempotency))
    }

    fn req(key: Option<&str>, body: &str) -> Request {
        let mut b = Request::builder().method("POST").uri("/things");
        if let Some(k) = key {
            b = b.header("idempotency-key", k);
        }
        b.body(Body::from(body.to_owned())).unwrap()
    }

    async fn text(res: Response) -> String {
        String::from_utf8(res.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap()
    }

    #[tokio::test]
    async fn replays_same_request() {
        let c = Arc::new(AtomicUsize::new(0));
        let a = app(c.clone());
        let r1 = a.clone().oneshot(req(Some("k1"), "x")).await.unwrap();
        assert_eq!(r1.status(), StatusCode::CREATED);
        let b1 = text(r1).await;
        let r2 = a.clone().oneshot(req(Some("k1"), "x")).await.unwrap();
        assert_eq!(r2.status(), StatusCode::CREATED);
        assert_eq!(r2.headers()["idempotency-replayed"], "true");
        assert_eq!(text(r2).await, b1);
        assert_eq!(c.load(Ordering::SeqCst), 1, "handler executed once");
    }

    #[tokio::test]
    async fn rejects_key_reuse_with_different_body() {
        let a = app(Arc::new(AtomicUsize::new(0)));
        let _ = a.clone().oneshot(req(Some("k2"), "x")).await.unwrap();
        let r = a.clone().oneshot(req(Some("k2"), "y")).await.unwrap();
        assert_eq!(r.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(text(r).await.contains("idempotency-key-reused"));
    }

    #[tokio::test]
    async fn passes_through_without_key() {
        let c = Arc::new(AtomicUsize::new(0));
        let a = app(c.clone());
        let _ = a.clone().oneshot(req(None, "x")).await.unwrap();
        let _ = a.clone().oneshot(req(None, "x")).await.unwrap();
        assert_eq!(c.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn owner_key_is_tenant_and_subject() {
        use sutra_kernel::auth::Role;
        use sutra_kernel::scope::{ExecutionScope, TenantId};
        let t = TenantId::new();
        let ctx = AuthContext {
            session_id: uuid::Uuid::now_v7(),
            subject: "dev|a".into(),
            login_id: "a".into(),
            display_name: "A".into(),
            roles: vec![Role::Student],
            scope: ExecutionScope::tenant(t),
            csrf_token: String::new(),
            expires_at: chrono::Utc::now(),
            idle_expires_at: chrono::Utc::now(),
        };
        assert_eq!(owner_key(Some(&ctx)), format!("{t}/dev|a"));
        assert_eq!(owner_key(None), "anonymous");
    }

    #[tokio::test]
    async fn rejects_malformed_key() {
        let a = app(Arc::new(AtomicUsize::new(0)));
        let r = a.oneshot(req(Some("has space"), "x")).await.unwrap();
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    }
}
