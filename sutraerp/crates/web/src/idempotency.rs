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
//! 5xx outcomes release the key so the client may retry. A completed response larger than
//! [`MAX_BODY_BYTES`] is still delivered in full; the key then stores a small
//! `already-processed` record (same status) instead of the body, so a retry is never executed
//! twice (Part 2 · S5-capacity, DEF-P2-CAP-005: a committed 2,000-row marks entry used to be
//! answered 500 and its key released). Requests without the header pass
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

/// Largest response of a keyed mutating request this layer buffers before handing it to the
/// client (the handler has committed by then; see [`replay_marker`]).
pub const MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;

/// What a key stores when the committed response is too large to keep for replay: the same
/// status with a small problem body telling the client the request was already processed.
fn replay_marker(status: StatusCode, len: usize) -> StoredResponse {
    let body = serde_json::json!({
        "type": "urn:sutraerp:problem:already-processed",
        "title": "Already processed",
        "status": status.as_u16(),
        "detail": format!(
            "this request was already processed (status {}); its {len}-byte response is too \
             large to replay; read the resource again",
            status.as_u16()
        ),
        "errorCode": "already-processed",
    });
    StoredResponse {
        status: status.as_u16(),
        content_type: Some("application/problem+json".to_owned()),
        body: body.to_string().into_bytes(),
    }
}

/// Request extension: the transport body limit of the current route, set by the composition
/// root's body-limit middleware (which also enforces it). A route allowed a larger body than
/// [`MAX_BODY_BYTES`] (data import uploads, document files) is fingerprinted up to that limit
/// instead of being refused as too large (413) (Part 2 · S4-ui-import).
/// Only the SHA-256 fingerprint is stored, never the request body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RouteBodyLimit(pub usize);

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
    let limit = parts
        .extensions
        .get::<RouteBodyLimit>()
        .map_or(MAX_BODY_BYTES, |l| l.0.max(MAX_BODY_BYTES));
    let bytes = match to_bytes(body, limit).await {
        Ok(b) => b,
        // Above the route's limit: the same 413 the transport limit gives (not a 400).
        Err(e) if e.to_string().contains("length limit exceeded") => {
            let p = crate::Problem::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "payload-too-large",
                "Request body too large",
            );
            return Ok(p.into_response());
        }
        Err(_) => {
            return Err(KernelError::BadRequest("request body could not be read".into()).into());
        }
    };
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
    let res_bytes = match to_bytes(res_body, MAX_RESPONSE_BYTES).await {
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
    let stored = if res_bytes.len() > MAX_BODY_BYTES {
        replay_marker(status, res_bytes.len())
    } else {
        StoredResponse {
            status: status.as_u16(),
            content_type,
            body: res_bytes.to_vec(),
        }
    };
    store.complete(&scope_key, &key, stored).await?;
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
                        // The body's length, not an echo: a route may accept a body above
                        // MAX_BODY_BYTES, but its response must stay under the replay buffer.
                        (
                            StatusCode::CREATED,
                            format!("{{\"n\":{n},\"len\":{}}}", body.len()),
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
            mfa_level: sutra_kernel::auth::MfaLevel::None,
            amr: vec![],
            step_up_enforced: false,
            privileged_mfa_enforced: false,
        };
        assert_eq!(owner_key(Some(&ctx)), format!("{t}/dev|a"));
        assert_eq!(owner_key(None), "anonymous");
    }

    #[tokio::test]
    async fn fingerprints_bodies_up_to_the_route_limit() {
        let c = Arc::new(AtomicUsize::new(0));
        let a = app(c.clone());
        let big = "x".repeat(MAX_BODY_BYTES + 512 * 1024);
        // Without a route limit: refused above MAX_BODY_BYTES (413), handler not run.
        let r = a.clone().oneshot(req(Some("big-1"), &big)).await.unwrap();
        assert_eq!(r.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert!(text(r).await.contains("payload-too-large"));
        assert_eq!(c.load(Ordering::SeqCst), 0);
        // With the route's larger limit: processed once and replayed.
        let with_limit = |key: &str| {
            let mut r = req(Some(key), &big);
            r.extensions_mut().insert(RouteBodyLimit(4 * 1024 * 1024));
            r
        };
        let r1 = a.clone().oneshot(with_limit("big-2")).await.unwrap();
        assert_eq!(r1.status(), StatusCode::CREATED);
        assert!(text(r1).await.contains(&format!("\"len\":{}", big.len())));
        let r2 = a.clone().oneshot(with_limit("big-2")).await.unwrap();
        assert_eq!(r2.headers()["idempotency-replayed"], "true");
        assert_eq!(c.load(Ordering::SeqCst), 1);
        // A limit below the default never lowers it.
        let mut small = req(Some("small"), "x");
        small.extensions_mut().insert(RouteBodyLimit(1));
        let r = a.clone().oneshot(small).await.unwrap();
        assert_eq!(r.status(), StatusCode::CREATED);
    }

    /// DEF-P2-CAP-005: a committed response above the replay buffer reaches the client in full
    /// (not a 500), and a retry with the same key is answered from the key without running the
    /// handler again.
    #[tokio::test]
    async fn large_committed_response_is_delivered_and_never_re_executed() {
        let c = Arc::new(AtomicUsize::new(0));
        let counter = c.clone();
        let store: SharedIdempotencyStore = Arc::new(InMemoryIdempotencyStore::default());
        let big = MAX_BODY_BYTES * 2;
        let a = Router::new()
            .route(
                "/marks",
                post(move || {
                    let c = counter.clone();
                    async move {
                        c.fetch_add(1, Ordering::SeqCst);
                        (StatusCode::OK, "x".repeat(big))
                    }
                }),
            )
            .layer(axum::middleware::from_fn_with_state(store, idempotency));
        let call = |key: &str| {
            Request::builder()
                .method("POST")
                .uri("/marks")
                .header("idempotency-key", key)
                .body(Body::from("{}"))
                .unwrap()
        };
        let r1 = a.clone().oneshot(call("m1")).await.unwrap();
        assert_eq!(r1.status(), StatusCode::OK);
        assert_eq!(text(r1).await.len(), big, "the full response");
        let r2 = a.clone().oneshot(call("m1")).await.unwrap();
        assert_eq!(r2.status(), StatusCode::OK);
        assert_eq!(r2.headers()["idempotency-replayed"], "true");
        assert!(text(r2).await.contains("already-processed"));
        assert_eq!(c.load(Ordering::SeqCst), 1, "handler executed once");
    }

    #[tokio::test]
    async fn rejects_malformed_key() {
        let a = app(Arc::new(AtomicUsize::new(0)));
        let r = a.oneshot(req(Some("has space"), "x")).await.unwrap();
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    }
}
